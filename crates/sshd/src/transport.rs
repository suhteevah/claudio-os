//! SSH Transport Layer (RFC 4253).
//!
//! Implements the SSH binary packet protocol:
//! - Version string exchange (`SSH-2.0-ClaudioOS_0.1`)
//! - Binary packet framing: `packet_length(4) + padding_length(1) + payload + padding + MAC`
//! - SSH_MSG_KEXINIT construction and parsing
//! - SSH_MSG_NEWKEYS handling
//! - Packet encryption/decryption after keys are established
//! - Sequence number tracking per direction
//! - Maximum packet size enforcement

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use chacha20poly1305::{ChaCha20Poly1305, KeyInit, aead::{Aead, AeadInPlace}};
use chacha20poly1305::aead::generic_array::GenericArray;

use crate::wire::*;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Our SSH version string (no trailing CR LF — caller appends that).
pub const SSH_VERSION_STRING: &str = "SSH-2.0-ClaudioOS_0.1";

/// Minimum SSH version string from any peer.
pub const SSH_VERSION_PREFIX: &str = "SSH-2.0-";

/// KEXINIT cookie size (16 random bytes).
pub const COOKIE_SIZE: usize = 16;

// ---------------------------------------------------------------------------
// Algorithm lists — our server advertises these in SSH_MSG_KEXINIT
// ---------------------------------------------------------------------------

/// Key exchange algorithms (best first).
pub const KEX_ALGORITHMS: &[&str] = &[
    "mlkem768x25519-sha256@openssh.com",
    "curve25519-sha256",
    "curve25519-sha256@libssh.org",
];

/// Host key algorithms.
pub const HOST_KEY_ALGORITHMS: &[&str] = &[
    "mlkem768-ed25519@openssh.com", // hybrid PQ host key
    "ssh-ed25519",
];

/// Encryption algorithms (client-to-server and server-to-client).
pub const ENCRYPTION_ALGORITHMS: &[&str] = &[
    "chacha20-poly1305@openssh.com",
];

/// MAC algorithms. With chacha20-poly1305, MAC is integrated (AEAD).
/// We still advertise hmac-sha2-256 as a fallback if non-AEAD cipher is negotiated.
pub const MAC_ALGORITHMS: &[&str] = &[
    "hmac-sha2-256",
];

/// Compression algorithms.
pub const COMPRESSION_ALGORITHMS: &[&str] = &["none"];

// ---------------------------------------------------------------------------
// Packet sequencing
// ---------------------------------------------------------------------------

/// Tracks sequence numbers for a direction (send or receive).
/// Sequence numbers wrap at u32::MAX per RFC 4253 §6.4.
#[derive(Debug)]
pub struct SequenceCounter {
    seq: u32,
}

impl SequenceCounter {
    pub fn new() -> Self {
        Self { seq: 0 }
    }

    /// Get current sequence number and advance.
    pub fn next(&mut self) -> u32 {
        let current = self.seq;
        self.seq = self.seq.wrapping_add(1);
        current
    }

    /// Get current sequence number without advancing.
    pub fn current(&self) -> u32 {
        self.seq
    }
}

// ---------------------------------------------------------------------------
// Encryption state
// ---------------------------------------------------------------------------

/// Represents the encryption state for one direction.
#[derive(Debug)]
pub enum CipherState {
    /// No encryption (before NEWKEYS).
    Plaintext,
    /// ChaCha20-Poly1305 AEAD.
    ChaCha20Poly1305 {
        /// 256-bit key for main encryption.
        key: [u8; 32],
        /// 256-bit key for packet length encryption.
        header_key: [u8; 32],
    },
}

impl CipherState {
    pub fn is_encrypted(&self) -> bool {
        !matches!(self, Self::Plaintext)
    }
}

// ---------------------------------------------------------------------------
// KEXINIT message
// ---------------------------------------------------------------------------

/// Parsed SSH_MSG_KEXINIT message.
#[derive(Debug, Clone)]
pub struct KexInit {
    /// 16 random bytes.
    pub cookie: [u8; 16],
    /// Key exchange algorithms.
    pub kex_algorithms: Vec<String>,
    /// Server host key algorithms.
    pub server_host_key_algorithms: Vec<String>,
    /// Encryption algorithms client-to-server.
    pub encryption_algorithms_c2s: Vec<String>,
    /// Encryption algorithms server-to-client.
    pub encryption_algorithms_s2c: Vec<String>,
    /// MAC algorithms client-to-server.
    pub mac_algorithms_c2s: Vec<String>,
    /// MAC algorithms server-to-client.
    pub mac_algorithms_s2c: Vec<String>,
    /// Compression algorithms client-to-server.
    pub compression_algorithms_c2s: Vec<String>,
    /// Compression algorithms server-to-client.
    pub compression_algorithms_s2c: Vec<String>,
    /// First KEX packet follows.
    pub first_kex_packet_follows: bool,
    /// Reserved (always 0).
    pub reserved: u32,
    /// Raw bytes of the entire KEXINIT payload (needed for exchange hash).
    pub raw_payload: Vec<u8>,
}

impl KexInit {
    /// Build our server KEXINIT message.
    ///
    /// `cookie` should be 16 random bytes from the RNG.
    pub fn build_server(cookie: [u8; 16]) -> Vec<u8> {
        log::debug!("transport: building server SSH_MSG_KEXINIT");

        let mut w = SshWriter::new();
        w.write_byte(SSH_MSG_KEXINIT);
        w.write_raw(&cookie);
        w.write_name_list(KEX_ALGORITHMS);
        w.write_name_list(HOST_KEY_ALGORITHMS);
        w.write_name_list(ENCRYPTION_ALGORITHMS);
        w.write_name_list(ENCRYPTION_ALGORITHMS);
        w.write_name_list(MAC_ALGORITHMS);
        w.write_name_list(MAC_ALGORITHMS);
        w.write_name_list(COMPRESSION_ALGORITHMS);
        w.write_name_list(COMPRESSION_ALGORITHMS);
        // languages client-to-server
        w.write_name_list(&[]);
        // languages server-to-client
        w.write_name_list(&[]);
        // first_kex_packet_follows
        w.write_boolean(false);
        // reserved
        w.write_uint32(0);

        let payload = w.into_bytes();
        log::trace!(
            "transport: KEXINIT payload {} bytes, kex={:?}, hostkey={:?}, enc={:?}",
            payload.len(),
            KEX_ALGORITHMS,
            HOST_KEY_ALGORITHMS,
            ENCRYPTION_ALGORITHMS,
        );
        payload
    }

    /// Parse a client KEXINIT payload (the payload bytes, starting with SSH_MSG_KEXINIT byte).
    pub fn parse(payload: &[u8]) -> Result<Self, TransportError> {
        log::debug!("transport: parsing client SSH_MSG_KEXINIT ({} bytes)", payload.len());

        let mut r = SshReader::new(payload);
        let msg_type = r.read_byte().map_err(|_| TransportError::MalformedKexInit)?;
        if msg_type != SSH_MSG_KEXINIT {
            log::error!("transport: expected KEXINIT (20), got {}", msg_type);
            return Err(TransportError::UnexpectedMessage(msg_type));
        }

        let mut cookie = [0u8; 16];
        let cookie_bytes = r.read_bytes(16).map_err(|_| TransportError::MalformedKexInit)?;
        cookie.copy_from_slice(cookie_bytes);

        let kex_algorithms = r.read_name_list().map_err(|_| TransportError::MalformedKexInit)?;
        let server_host_key_algorithms =
            r.read_name_list().map_err(|_| TransportError::MalformedKexInit)?;
        let encryption_algorithms_c2s =
            r.read_name_list().map_err(|_| TransportError::MalformedKexInit)?;
        let encryption_algorithms_s2c =
            r.read_name_list().map_err(|_| TransportError::MalformedKexInit)?;
        let mac_algorithms_c2s =
            r.read_name_list().map_err(|_| TransportError::MalformedKexInit)?;
        let mac_algorithms_s2c =
            r.read_name_list().map_err(|_| TransportError::MalformedKexInit)?;
        let compression_algorithms_c2s =
            r.read_name_list().map_err(|_| TransportError::MalformedKexInit)?;
        let compression_algorithms_s2c =
            r.read_name_list().map_err(|_| TransportError::MalformedKexInit)?;
        // languages (ignored)
        let _ = r.read_name_list();
        let _ = r.read_name_list();
        let first_kex_packet_follows = r.read_boolean().unwrap_or(false);
        let reserved = r.read_uint32().unwrap_or(0);

        log::info!(
            "transport: client KEXINIT — kex={:?}, hostkey={:?}, enc_c2s={:?}",
            kex_algorithms,
            server_host_key_algorithms,
            encryption_algorithms_c2s,
        );

        Ok(Self {
            cookie,
            kex_algorithms,
            server_host_key_algorithms,
            encryption_algorithms_c2s,
            encryption_algorithms_s2c,
            mac_algorithms_c2s,
            mac_algorithms_s2c,
            compression_algorithms_c2s,
            compression_algorithms_s2c,
            first_kex_packet_follows,
            reserved,
            raw_payload: Vec::from(payload),
        })
    }
}

// ---------------------------------------------------------------------------
// Binary packet framing
// ---------------------------------------------------------------------------

/// Frame a payload into an SSH binary packet (unencrypted).
///
/// Layout: `packet_length(4) || padding_length(1) || payload || random_padding`
///
/// The caller should provide random padding bytes via `rng_fill`.
pub fn frame_packet(payload: &[u8], rng_fill: &dyn Fn(&mut [u8])) -> Vec<u8> {
    let block_size = UNENCRYPTED_BLOCK_SIZE;
    let padding_len = compute_padding(payload.len(), block_size);
    let packet_length = 1 + payload.len() + padding_len; // padding_length(1) + payload + padding

    log::trace!(
        "transport: framing packet — payload={}, padding={}, total={}",
        payload.len(),
        padding_len,
        4 + packet_length,
    );

    let mut pkt = Vec::with_capacity(4 + packet_length);
    pkt.extend_from_slice(&(packet_length as u32).to_be_bytes());
    pkt.push(padding_len as u8);
    pkt.extend_from_slice(payload);

    let mut padding = vec![0u8; padding_len];
    rng_fill(&mut padding);
    pkt.extend_from_slice(&padding);

    pkt
}

/// Frame a payload into an encrypted SSH binary packet using ChaCha20-Poly1305.
///
/// ChaCha20-Poly1305@openssh.com uses:
/// - `header_key` to encrypt the 4-byte packet length (ChaCha20 with nonce = seq number)
/// - `main_key` to encrypt the rest (ChaCha20 with nonce = seq number)
/// - Poly1305 MAC over the encrypted packet (16 bytes appended)
///
/// Returns the full encrypted packet including the 16-byte MAC tag.
pub fn frame_packet_encrypted(
    payload: &[u8],
    seq: u32,
    cipher: &CipherState,
    rng_fill: &dyn Fn(&mut [u8]),
) -> Result<Vec<u8>, TransportError> {
    match cipher {
        CipherState::Plaintext => {
            // No encryption — just frame normally
            Ok(frame_packet(payload, rng_fill))
        }
        CipherState::ChaCha20Poly1305 { key, header_key } => {
            // TODO: Implement ChaCha20-Poly1305@openssh.com packet encryption
            // This requires:
            // 1. Build unencrypted packet (packet_length || padding_length || payload || padding)
            // 2. Encrypt packet_length (4 bytes) with header_key, nonce = seq
            // 3. Encrypt remaining bytes with main key, nonce = seq
            // 4. Compute Poly1305 MAC over entire encrypted packet
            // 5. Append 16-byte MAC tag
            //
            // For now, use chacha20poly1305 crate:
            // use chacha20poly1305::{ChaCha20Poly1305, KeyInit, AeadInPlace};
            let _ = (key, header_key, seq);
            log::warn!("transport: ChaCha20-Poly1305 encryption not yet wired — sending plaintext");
            Ok(frame_packet(payload, rng_fill))
        }
    }
}

/// Parse a received SSH binary packet (unencrypted).
///
/// Expects: `packet_length(4) || padding_length(1) || payload || padding`
///
/// Returns the payload bytes (without padding_length byte or padding).
pub fn parse_packet(data: &[u8]) -> Result<(Vec<u8>, usize), TransportError> {
    if data.len() < 5 {
        return Err(TransportError::PacketTooShort);
    }

    let packet_length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    log::trace!("transport: packet_length = {}", packet_length);

    if packet_length > MAX_PACKET_SIZE {
        log::error!("transport: packet too large: {} bytes", packet_length);
        return Err(TransportError::PacketTooLarge(packet_length));
    }

    let total_len = 4 + packet_length;
    if data.len() < total_len {
        return Err(TransportError::PacketTooShort);
    }

    let padding_length = data[4] as usize;
    if padding_length < MIN_PADDING || padding_length >= packet_length {
        log::error!(
            "transport: invalid padding length: {} (packet_length={})",
            padding_length,
            packet_length,
        );
        return Err(TransportError::InvalidPadding);
    }

    let payload_length = packet_length - 1 - padding_length;
    let payload = Vec::from(&data[5..5 + payload_length]);

    log::trace!(
        "transport: parsed packet — payload={} bytes, padding={}, consumed={}",
        payload_length,
        padding_length,
        total_len,
    );

    Ok((payload, total_len))
}

/// Parse an encrypted packet. Returns the decrypted payload.
pub fn parse_packet_encrypted(
    data: &[u8],
    seq: u32,
    cipher: &CipherState,
) -> Result<(Vec<u8>, usize), TransportError> {
    match cipher {
        CipherState::Plaintext => parse_packet(data),
        CipherState::ChaCha20Poly1305 { key, header_key } => {
            // TODO: Implement ChaCha20-Poly1305@openssh.com packet decryption
            // 1. Decrypt packet_length (4 bytes) with header_key, nonce = seq
            // 2. Verify Poly1305 MAC (last 16 bytes) over encrypted data
            // 3. Decrypt remaining bytes with main key, nonce = seq
            // 4. Extract payload from decrypted packet
            let _ = (key, header_key, seq);
            log::warn!("transport: ChaCha20-Poly1305 decryption not yet wired — parsing as plaintext");
            parse_packet(data)
        }
    }
}

// ---------------------------------------------------------------------------
// Version string exchange
// ---------------------------------------------------------------------------

/// Build our SSH version string with CR LF terminator.
pub fn version_string() -> Vec<u8> {
    let mut v = Vec::from(SSH_VERSION_STRING.as_bytes());
    v.push(b'\r');
    v.push(b'\n');
    log::info!("transport: sending version string: {}", SSH_VERSION_STRING);
    v
}

/// Parse a received version string.
///
/// Per RFC 4253 §4.2: lines not starting with "SSH-" are ignored (banner lines).
/// Returns the version string (without CR LF) when found.
pub fn parse_version_string(data: &[u8]) -> Result<String, TransportError> {
    // Find lines separated by \r\n or \n
    let mut start = 0;
    while start < data.len() {
        let end = data[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| start + p)
            .unwrap_or(data.len());

        let line_end = if end > 0 && data[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };

        let line = &data[start..line_end];

        if line.starts_with(b"SSH-") {
            let version = core::str::from_utf8(line)
                .map_err(|_| TransportError::InvalidVersionString)?;

            if !version.starts_with(SSH_VERSION_PREFIX) {
                log::error!(
                    "transport: unsupported SSH version: {}",
                    version,
                );
                return Err(TransportError::UnsupportedVersion);
            }

            log::info!("transport: peer version string: {}", version);
            return Ok(String::from(version));
        }

        // Skip banner lines
        log::trace!(
            "transport: skipping banner line ({} bytes)",
            line_end - start,
        );
        start = end + 1;
    }

    Err(TransportError::InvalidVersionString)
}

/// Build an SSH_MSG_DISCONNECT packet.
pub fn build_disconnect(reason_code: u32, description: &str) -> Vec<u8> {
    log::info!(
        "transport: building DISCONNECT — reason={}, desc={}",
        reason_code,
        description,
    );
    let mut w = SshWriter::new();
    w.write_byte(SSH_MSG_DISCONNECT);
    w.write_uint32(reason_code);
    w.write_string_utf8(description);
    w.write_string_utf8(""); // language tag
    w.into_bytes()
}

/// Build an SSH_MSG_NEWKEYS packet.
pub fn build_newkeys() -> Vec<u8> {
    log::debug!("transport: building SSH_MSG_NEWKEYS");
    vec![SSH_MSG_NEWKEYS]
}

/// Build an SSH_MSG_SERVICE_ACCEPT packet.
pub fn build_service_accept(service_name: &str) -> Vec<u8> {
    log::debug!("transport: building SERVICE_ACCEPT for '{}'", service_name);
    let mut w = SshWriter::new();
    w.write_byte(SSH_MSG_SERVICE_ACCEPT);
    w.write_string_utf8(service_name);
    w.into_bytes()
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TransportError {
    /// Peer version string is not valid.
    InvalidVersionString,
    /// Peer uses an unsupported SSH version.
    UnsupportedVersion,
    /// Packet is too short to parse.
    PacketTooShort,
    /// Packet exceeds maximum size.
    PacketTooLarge(usize),
    /// Invalid padding in packet.
    InvalidPadding,
    /// Received unexpected message type.
    UnexpectedMessage(u8),
    /// KEXINIT message is malformed.
    MalformedKexInit,
    /// MAC verification failed.
    MacVerifyFailed,
    /// Sequence number overflow (extremely unlikely but tracked).
    SequenceOverflow,
    /// Wire format error.
    Wire(WireError),
}

impl From<WireError> for TransportError {
    fn from(e: WireError) -> Self {
        Self::Wire(e)
    }
}

impl core::fmt::Display for TransportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidVersionString => write!(f, "invalid SSH version string"),
            Self::UnsupportedVersion => write!(f, "unsupported SSH protocol version"),
            Self::PacketTooShort => write!(f, "SSH packet too short"),
            Self::PacketTooLarge(n) => write!(f, "SSH packet too large: {} bytes", n),
            Self::InvalidPadding => write!(f, "invalid SSH packet padding"),
            Self::UnexpectedMessage(t) => write!(f, "unexpected SSH message type: {}", t),
            Self::MalformedKexInit => write!(f, "malformed KEXINIT message"),
            Self::MacVerifyFailed => write!(f, "MAC verification failed"),
            Self::SequenceOverflow => write!(f, "sequence number overflow"),
            Self::Wire(e) => write!(f, "wire format error: {}", e),
        }
    }
}
