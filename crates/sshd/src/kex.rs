//! SSH Key Exchange (RFC 4253 §7-8).
//!
//! Implements hybrid post-quantum key exchange:
//! - `mlkem768x25519-sha256@openssh.com` — ML-KEM-768 + X25519, SHA-256 exchange hash
//! - `curve25519-sha256` — Classical X25519-only fallback
//!
//! The hybrid PQ KEX follows the draft-kampanakis-curdle-ssh-pq-ke pattern:
//! 1. Server generates ML-KEM-768 keypair + X25519 keypair
//! 2. Server sends both public keys concatenated in SSH_MSG_KEX_ECDH_REPLY
//! 3. Client encapsulates ML-KEM + performs X25519 DH
//! 4. Client sends ciphertext + X25519 public key in SSH_MSG_KEX_ECDH_INIT
//! 5. Server decapsulates ML-KEM + completes X25519
//! 6. Shared secret = SHA-256(mlkem_shared || x25519_shared)
//!
//! After key exchange, derives encryption keys per RFC 4253 §7.2:
//! - IV, encryption key, integrity key using HASH(K || H || char || session_id)

use alloc::string::String;
use alloc::vec::Vec;

use sha2::{Sha256, Digest};

use ml_kem::{KemCore, MlKem768, EncodedSizeUser as MlKemEncodedSizeUser};
use ml_kem::kem::Decapsulate;
use x25519_dalek::PublicKey as X25519PublicKey;

use crate::transport::{KexInit, TransportError};
use crate::wire::*;

// ---------------------------------------------------------------------------
// Negotiated algorithms
// ---------------------------------------------------------------------------

/// The result of algorithm negotiation from two KEXINIT messages.
#[derive(Debug, Clone)]
pub struct NegotiatedAlgorithms {
    pub kex: String,
    pub host_key: String,
    pub encryption_c2s: String,
    pub encryption_s2c: String,
    pub mac_c2s: String,
    pub mac_s2c: String,
    pub compression_c2s: String,
    pub compression_s2c: String,
}

/// Negotiate algorithms: for each category, pick the first algorithm in the
/// server's list that also appears in the client's list (RFC 4253 §7.1).
pub fn negotiate(
    client: &KexInit,
    server_kex: &[&str],
    server_hostkey: &[&str],
    server_enc: &[&str],
    server_mac: &[&str],
    server_comp: &[&str],
) -> Result<NegotiatedAlgorithms, KexError> {
    log::debug!("kex: negotiating algorithms");

    let kex = pick_algorithm(server_kex, &client.kex_algorithms, "kex")?;
    let host_key = pick_algorithm(
        server_hostkey,
        &client.server_host_key_algorithms,
        "host_key",
    )?;
    let encryption_c2s = pick_algorithm(
        server_enc,
        &client.encryption_algorithms_c2s,
        "encryption_c2s",
    )?;
    let encryption_s2c = pick_algorithm(
        server_enc,
        &client.encryption_algorithms_s2c,
        "encryption_s2c",
    )?;
    let mac_c2s = pick_algorithm(server_mac, &client.mac_algorithms_c2s, "mac_c2s")?;
    let mac_s2c = pick_algorithm(server_mac, &client.mac_algorithms_s2c, "mac_s2c")?;
    let compression_c2s = pick_algorithm(
        server_comp,
        &client.compression_algorithms_c2s,
        "compression_c2s",
    )?;
    let compression_s2c = pick_algorithm(
        server_comp,
        &client.compression_algorithms_s2c,
        "compression_s2c",
    )?;

    log::info!(
        "kex: negotiated — kex={}, hostkey={}, enc_c2s={}, enc_s2c={}",
        kex,
        host_key,
        encryption_c2s,
        encryption_s2c,
    );

    Ok(NegotiatedAlgorithms {
        kex,
        host_key,
        encryption_c2s,
        encryption_s2c,
        mac_c2s,
        mac_s2c,
        compression_c2s,
        compression_s2c,
    })
}

/// Pick the first server algorithm that appears in the client's list.
fn pick_algorithm(
    server_list: &[&str],
    client_list: &[String],
    category: &str,
) -> Result<String, KexError> {
    for server_alg in server_list {
        for client_alg in client_list {
            if *server_alg == client_alg.as_str() {
                log::debug!("kex: {} negotiated: {}", category, server_alg);
                return Ok(String::from(*server_alg));
            }
        }
    }
    log::error!(
        "kex: no common algorithm for {} — server={:?}, client={:?}",
        category,
        server_list,
        client_list,
    );
    Err(KexError::NoCommonAlgorithm(String::from(category)))
}

// ---------------------------------------------------------------------------
// Key exchange state
// ---------------------------------------------------------------------------

/// Key exchange state machine.
#[derive(Debug)]
pub enum KexState {
    /// Waiting for client KEXINIT.
    WaitingForClientKexInit,
    /// KEXINIT exchanged, waiting for client's KEX_ECDH_INIT.
    WaitingForClientKexDhInit {
        algorithms: NegotiatedAlgorithms,
        server_kexinit_payload: Vec<u8>,
        client_kexinit_payload: Vec<u8>,
    },
    /// Key exchange complete, waiting for NEWKEYS.
    WaitingForNewKeys {
        session_id: Vec<u8>,
        exchange_hash: Vec<u8>,
        shared_secret: Vec<u8>,
        algorithms: NegotiatedAlgorithms,
    },
    /// Key exchange done, keys active.
    Complete {
        session_id: Vec<u8>,
    },
}

// ---------------------------------------------------------------------------
// Exchange hash computation
// ---------------------------------------------------------------------------

/// Compute the exchange hash H for the key exchange.
///
/// Per RFC 4253 §8 and the hybrid PQ KEX draft:
/// ```text
/// H = HASH(V_C || V_S || I_C || I_S || K_S || e || f || K)
/// ```
/// Where:
/// - V_C = client version string (without CR LF)
/// - V_S = server version string (without CR LF)
/// - I_C = client SSH_MSG_KEXINIT payload
/// - I_S = server SSH_MSG_KEXINIT payload
/// - K_S = server public host key blob
/// - e   = client's ephemeral public value (X25519 pubkey, or hybrid blob)
/// - f   = server's ephemeral public value (X25519 pubkey, or hybrid blob)
/// - K   = shared secret (mpint)
pub fn compute_exchange_hash(
    client_version: &str,
    server_version: &str,
    client_kexinit: &[u8],
    server_kexinit: &[u8],
    host_key_blob: &[u8],
    client_ephemeral: &[u8],
    server_ephemeral: &[u8],
    shared_secret: &[u8],
) -> Vec<u8> {
    log::debug!(
        "kex: computing exchange hash — V_C={}, V_S={}, I_C={} bytes, I_S={} bytes, K_S={} bytes",
        client_version,
        server_version,
        client_kexinit.len(),
        server_kexinit.len(),
        host_key_blob.len(),
    );

    let mut w = SshWriter::with_capacity(1024);

    // V_C: client version string (as SSH string)
    w.write_string_utf8(client_version);
    // V_S: server version string (as SSH string)
    w.write_string_utf8(server_version);
    // I_C: client KEXINIT payload (as SSH string)
    w.write_string(client_kexinit);
    // I_S: server KEXINIT payload (as SSH string)
    w.write_string(server_kexinit);
    // K_S: host key blob (as SSH string)
    w.write_string(host_key_blob);
    // e: client ephemeral public value (as SSH string)
    w.write_string(client_ephemeral);
    // f: server ephemeral public value (as SSH string)
    w.write_string(server_ephemeral);
    // K: shared secret (as mpint)
    w.write_mpint(shared_secret);

    let hash_input = w.into_bytes();

    let mut hasher = Sha256::new();
    hasher.update(&hash_input);
    let result = hasher.finalize();

    log::debug!(
        "kex: exchange hash H = {:02x}{:02x}{:02x}{:02x}...",
        result[0],
        result[1],
        result[2],
        result[3],
    );

    Vec::from(result.as_slice())
}

// ---------------------------------------------------------------------------
// Key derivation (RFC 4253 §7.2)
// ---------------------------------------------------------------------------

/// Derived key material from the key exchange.
#[derive(Debug)]
pub struct DerivedKeys {
    /// IV client-to-server (char 'A').
    pub iv_c2s: Vec<u8>,
    /// IV server-to-client (char 'B').
    pub iv_s2c: Vec<u8>,
    /// Encryption key client-to-server (char 'C').
    pub enc_key_c2s: Vec<u8>,
    /// Encryption key server-to-client (char 'D').
    pub enc_key_s2c: Vec<u8>,
    /// Integrity key client-to-server (char 'E').
    pub integrity_key_c2s: Vec<u8>,
    /// Integrity key server-to-client (char 'F').
    pub integrity_key_s2c: Vec<u8>,
}

/// Derive all encryption keys from the shared secret K, exchange hash H,
/// and session ID.
///
/// Each key = HASH(K || H || <letter> || session_id), extended if needed
/// by appending HASH(K || H || <existing key bytes>).
pub fn derive_keys(
    shared_secret: &[u8],
    exchange_hash: &[u8],
    session_id: &[u8],
    iv_len: usize,
    enc_key_len: usize,
    integrity_key_len: usize,
) -> DerivedKeys {
    log::debug!(
        "kex: deriving keys — iv_len={}, enc_key_len={}, integrity_key_len={}",
        iv_len,
        enc_key_len,
        integrity_key_len,
    );

    let iv_c2s = derive_key(shared_secret, exchange_hash, b'A', session_id, iv_len);
    let iv_s2c = derive_key(shared_secret, exchange_hash, b'B', session_id, iv_len);
    let enc_key_c2s = derive_key(shared_secret, exchange_hash, b'C', session_id, enc_key_len);
    let enc_key_s2c = derive_key(shared_secret, exchange_hash, b'D', session_id, enc_key_len);
    let integrity_key_c2s =
        derive_key(shared_secret, exchange_hash, b'E', session_id, integrity_key_len);
    let integrity_key_s2c =
        derive_key(shared_secret, exchange_hash, b'F', session_id, integrity_key_len);

    log::debug!("kex: all keys derived successfully");

    DerivedKeys {
        iv_c2s,
        iv_s2c,
        enc_key_c2s,
        enc_key_s2c,
        integrity_key_c2s,
        integrity_key_s2c,
    }
}

/// Derive a single key: HASH(K || H || X || session_id).
/// If the key needs to be longer than one hash output, extend by
/// appending HASH(K || H || K1) repeatedly.
fn derive_key(
    shared_secret: &[u8],
    exchange_hash: &[u8],
    letter: u8,
    session_id: &[u8],
    needed_len: usize,
) -> Vec<u8> {
    // Build the mpint encoding of K for hashing
    let mut k_mpint = SshWriter::new();
    k_mpint.write_mpint(shared_secret);
    let k_bytes = k_mpint.into_bytes();

    // First round: HASH(K || H || letter || session_id)
    let mut hasher = Sha256::new();
    hasher.update(&k_bytes);
    hasher.update(exchange_hash);
    hasher.update(&[letter]);
    hasher.update(session_id);
    let first_hash = hasher.finalize();

    let mut key = Vec::from(first_hash.as_slice());

    // Extend if needed: HASH(K || H || K1 || K2 || ...)
    while key.len() < needed_len {
        let mut hasher = Sha256::new();
        hasher.update(&k_bytes);
        hasher.update(exchange_hash);
        hasher.update(&key);
        let next = hasher.finalize();
        key.extend_from_slice(next.as_slice());
    }

    key.truncate(needed_len);

    log::trace!(
        "kex: derived key '{}' = {:02x}{:02x}{:02x}{:02x}... ({} bytes)",
        letter as char,
        key[0],
        key.get(1).copied().unwrap_or(0),
        key.get(2).copied().unwrap_or(0),
        key.get(3).copied().unwrap_or(0),
        key.len(),
    );

    key
}

// ---------------------------------------------------------------------------
// Hybrid PQ KEX: mlkem768x25519-sha256@openssh.com
// ---------------------------------------------------------------------------

/// Server-side ephemeral keys for hybrid PQ KEX.
pub struct HybridKexServerState {
    /// ML-KEM-768 decapsulation key (serialized).
    mlkem_dk_bytes: Vec<u8>,
    /// ML-KEM-768 encapsulation key (serialized, 1184 bytes for ML-KEM-768).
    mlkem_ek_bytes: Vec<u8>,
    /// X25519 secret key (32 bytes).
    x25519_secret: [u8; 32],
    /// X25519 public key (32 bytes).
    x25519_public: [u8; 32],
}

impl HybridKexServerState {
    /// Generate server ephemeral keys for the hybrid PQ KEX.
    ///
    /// Generates:
    /// - ML-KEM-768 keypair (encapsulation key + decapsulation key)
    /// - X25519 keypair
    pub fn generate(rng: &mut dyn FnMut(&mut [u8])) -> Self {
        log::info!("kex: generating hybrid PQ ephemeral keys (ML-KEM-768 + X25519)");

        // Generate ML-KEM-768 keypair using real ml-kem crate
        let mut rng_wrapper = FnRng(rng);
        let (dk, ek) = MlKem768::generate(&mut rng_wrapper);
        let rng = rng_wrapper.0; // recover borrow

        // Serialize keys to bytes for storage
        let mlkem_dk_bytes = Vec::from(dk.as_bytes().as_slice());
        let mlkem_ek_bytes = Vec::from(ek.as_bytes().as_slice());

        log::debug!(
            "kex: ML-KEM-768 keypair generated — ek={} bytes, dk={} bytes",
            mlkem_ek_bytes.len(),
            mlkem_dk_bytes.len(),
        );

        // Generate X25519 keypair using real x25519-dalek
        let x25519_secret_key = x25519_dalek::StaticSecret::random_from_rng(FnRng(rng));
        let x25519_public_key = X25519PublicKey::from(&x25519_secret_key);
        let x25519_secret = x25519_secret_key.to_bytes();
        let x25519_public = x25519_public_key.to_bytes();

        log::debug!("kex: X25519 keypair generated");

        Self {
            mlkem_dk_bytes,
            mlkem_ek_bytes,
            x25519_secret,
            x25519_public,
        }
    }

    /// Build the server's ephemeral public value for SSH_MSG_KEX_ECDH_REPLY.
    ///
    /// For the hybrid PQ KEX, this is:
    /// `mlkem_ek (1184 bytes) || x25519_public (32 bytes)`
    pub fn server_ephemeral_public(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.mlkem_ek_bytes.len() + self.x25519_public.len());
        out.extend_from_slice(&self.mlkem_ek_bytes);
        out.extend_from_slice(&self.x25519_public);
        log::debug!(
            "kex: server ephemeral public = {} bytes (mlkem_ek={} + x25519={})",
            out.len(),
            self.mlkem_ek_bytes.len(),
            self.x25519_public.len(),
        );
        out
    }

    /// Process the client's KEX_ECDH_INIT and compute the shared secret.
    ///
    /// Client sends: `mlkem_ciphertext (1088 bytes for ML-KEM-768) || x25519_public (32 bytes)`
    ///
    /// Server:
    /// 1. Decapsulates ML-KEM ciphertext -> mlkem_shared_secret (32 bytes)
    /// 2. Performs X25519 DH with client's X25519 public key -> x25519_shared (32 bytes)
    /// 3. shared_secret = SHA-256(mlkem_shared || x25519_shared)
    pub fn compute_shared_secret(
        &self,
        client_ephemeral: &[u8],
    ) -> Result<Vec<u8>, KexError> {
        log::info!("kex: computing hybrid shared secret from client ephemeral ({} bytes)", client_ephemeral.len());

        // ML-KEM-768 ciphertext is 1088 bytes, X25519 public key is 32 bytes
        const MLKEM768_CT_SIZE: usize = 1088;
        const X25519_PK_SIZE: usize = 32;

        if client_ephemeral.len() < MLKEM768_CT_SIZE + X25519_PK_SIZE {
            log::error!(
                "kex: client ephemeral too short: {} bytes, expected >= {}",
                client_ephemeral.len(),
                MLKEM768_CT_SIZE + X25519_PK_SIZE,
            );
            return Err(KexError::InvalidEphemeralKey);
        }

        let mlkem_ct = &client_ephemeral[..MLKEM768_CT_SIZE];
        let x25519_client_pk = &client_ephemeral[MLKEM768_CT_SIZE..MLKEM768_CT_SIZE + X25519_PK_SIZE];

        log::debug!(
            "kex: client sent mlkem_ct={} bytes, x25519_pk={} bytes",
            mlkem_ct.len(),
            x25519_client_pk.len(),
        );

        // Decapsulate ML-KEM-768 using real ml-kem crate
        let dk_encoded: &ml_kem::Encoded::<<MlKem768 as KemCore>::DecapsulationKey> =
            self.mlkem_dk_bytes.as_slice().try_into()
                .map_err(|_| {
                    log::error!("kex: ML-KEM-768 decapsulation key wrong size");
                    KexError::MlKemDecapsulationFailed
                })?;
        let dk = <MlKem768 as KemCore>::DecapsulationKey::from_bytes(dk_encoded);
        let ct: &ml_kem::Ciphertext::<MlKem768> = mlkem_ct.try_into()
            .map_err(|_| {
                log::error!("kex: ML-KEM-768 ciphertext wrong size");
                KexError::MlKemDecapsulationFailed
            })?;
        let mlkem_shared = dk.decapsulate(ct)
            .map_err(|_| {
                log::error!("kex: ML-KEM-768 decapsulation failed");
                KexError::MlKemDecapsulationFailed
            })?;
        log::debug!("kex: ML-KEM-768 shared secret derived ({} bytes)", mlkem_shared.len());

        // X25519 DH using real x25519-dalek
        let x25519_secret = x25519_dalek::StaticSecret::from(self.x25519_secret);
        let mut client_pk_bytes = [0u8; 32];
        client_pk_bytes.copy_from_slice(x25519_client_pk);
        let their_public = X25519PublicKey::from(client_pk_bytes);
        let x25519_shared = x25519_secret.diffie_hellman(&their_public);

        // Check for all-zero output (low-order point attack)
        if x25519_shared.as_bytes().iter().all(|&b| b == 0) {
            log::error!("kex: X25519 DH produced all-zero output — invalid client public key");
            return Err(KexError::X25519ZeroOutput);
        }
        log::debug!("kex: X25519 shared secret derived");

        // Combine: shared_secret = SHA-256(mlkem_shared || x25519_shared)
        let mut hasher = Sha256::new();
        hasher.update(mlkem_shared.as_slice());
        hasher.update(x25519_shared.as_bytes());
        let combined = hasher.finalize();

        log::info!(
            "kex: hybrid shared secret computed = {:02x}{:02x}{:02x}{:02x}...",
            combined[0],
            combined[1],
            combined[2],
            combined[3],
        );

        Ok(Vec::from(combined.as_slice()))
    }
}

// ---------------------------------------------------------------------------
// Classical fallback: curve25519-sha256
// ---------------------------------------------------------------------------

/// Server-side state for classical curve25519-sha256 KEX.
pub struct ClassicalKexServerState {
    /// X25519 secret key.
    x25519_secret: [u8; 32],
    /// X25519 public key.
    x25519_public: [u8; 32],
}

impl ClassicalKexServerState {
    /// Generate X25519 keypair for classical KEX.
    pub fn generate(rng: &mut dyn FnMut(&mut [u8])) -> Self {
        log::info!("kex: generating classical X25519 ephemeral key");

        // Generate X25519 keypair using real x25519-dalek
        let secret = x25519_dalek::StaticSecret::random_from_rng(FnRng(rng));
        let public = X25519PublicKey::from(&secret);
        let x25519_secret = secret.to_bytes();
        let x25519_public = public.to_bytes();

        log::debug!("kex: X25519 keypair generated for classical KEX");

        Self {
            x25519_secret,
            x25519_public,
        }
    }

    /// Get the server's ephemeral public key.
    pub fn server_ephemeral_public(&self) -> &[u8; 32] {
        &self.x25519_public
    }

    /// Compute shared secret from client's X25519 public key.
    pub fn compute_shared_secret(
        &self,
        client_public: &[u8],
    ) -> Result<Vec<u8>, KexError> {
        if client_public.len() != 32 {
            log::error!("kex: invalid X25519 public key length: {}", client_public.len());
            return Err(KexError::InvalidEphemeralKey);
        }

        log::debug!("kex: computing classical X25519 shared secret");

        // Real X25519 DH via x25519-dalek
        let secret = x25519_dalek::StaticSecret::from(self.x25519_secret);
        let mut pk_bytes = [0u8; 32];
        pk_bytes.copy_from_slice(client_public);
        let their_public = X25519PublicKey::from(pk_bytes);
        let shared = secret.diffie_hellman(&their_public);

        // Check for all-zero output (low-order point attack)
        if shared.as_bytes().iter().all(|&b| b == 0) {
            log::error!("kex: X25519 DH produced all-zero output — invalid client public key");
            return Err(KexError::X25519ZeroOutput);
        }

        log::info!("kex: classical shared secret computed");

        Ok(Vec::from(shared.as_bytes().as_slice()))
    }
}

// ---------------------------------------------------------------------------
// RNG wrapper: adapts `&mut dyn FnMut(&mut [u8])` to `CryptoRng + RngCore`
// ---------------------------------------------------------------------------

/// Wraps a `&mut dyn FnMut(&mut [u8])` to implement `RngCore + CryptoRng`
/// so it can be passed to crypto crate APIs that require those traits.
pub(crate) struct FnRng<'a>(pub(crate) &'a mut dyn FnMut(&mut [u8]));

impl<'a> rand_core::RngCore for FnRng<'a> {
    fn next_u32(&mut self) -> u32 {
        let mut buf = [0u8; 4];
        (self.0)(&mut buf);
        u32::from_le_bytes(buf)
    }

    fn next_u64(&mut self) -> u64 {
        let mut buf = [0u8; 8];
        (self.0)(&mut buf);
        u64::from_le_bytes(buf)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        (self.0)(dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        (self.0)(dest);
        Ok(())
    }
}

impl<'a> rand_core::CryptoRng for FnRng<'a> {}

// ---------------------------------------------------------------------------
// KEX message builders
// ---------------------------------------------------------------------------

/// Build SSH_MSG_KEX_ECDH_REPLY (message type 31).
///
/// ```text
/// byte      SSH_MSG_KEX_ECDH_REPLY (31)
/// string    server public host key (K_S)
/// string    server ephemeral public value (f)
/// string    signature of H
/// ```
pub fn build_kex_ecdh_reply(
    host_key_blob: &[u8],
    server_ephemeral: &[u8],
    signature: &[u8],
) -> Vec<u8> {
    log::debug!(
        "kex: building KEX_ECDH_REPLY — host_key={} bytes, ephemeral={} bytes, sig={} bytes",
        host_key_blob.len(),
        server_ephemeral.len(),
        signature.len(),
    );

    let mut w = SshWriter::new();
    w.write_byte(SSH_MSG_KEX_ECDH_REPLY);
    w.write_string(host_key_blob);
    w.write_string(server_ephemeral);
    w.write_string(signature);
    w.into_bytes()
}

/// Parse SSH_MSG_KEX_ECDH_INIT (message type 30) from client.
///
/// ```text
/// byte      SSH_MSG_KEX_ECDH_INIT (30)
/// string    client ephemeral public value (e)
/// ```
pub fn parse_kex_ecdh_init(payload: &[u8]) -> Result<Vec<u8>, KexError> {
    let mut r = SshReader::new(payload);
    let msg_type = r.read_byte().map_err(|_| KexError::MalformedMessage)?;
    if msg_type != SSH_MSG_KEX_ECDH_INIT {
        log::error!("kex: expected KEX_ECDH_INIT (30), got {}", msg_type);
        return Err(KexError::UnexpectedMessage(msg_type));
    }

    let client_ephemeral = r.read_string_raw().map_err(|_| KexError::MalformedMessage)?;

    log::debug!(
        "kex: parsed KEX_ECDH_INIT — client ephemeral {} bytes",
        client_ephemeral.len(),
    );

    Ok(Vec::from(client_ephemeral))
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum KexError {
    /// No common algorithm found for a category.
    NoCommonAlgorithm(String),
    /// Invalid ephemeral key from client.
    InvalidEphemeralKey,
    /// KEX message is malformed.
    MalformedMessage,
    /// Unexpected message type during KEX.
    UnexpectedMessage(u8),
    /// ML-KEM decapsulation failed.
    MlKemDecapsulationFailed,
    /// X25519 DH resulted in all-zero output (invalid public key).
    X25519ZeroOutput,
    /// Transport-layer error.
    Transport(TransportError),
}

impl From<TransportError> for KexError {
    fn from(e: TransportError) -> Self {
        Self::Transport(e)
    }
}

impl core::fmt::Display for KexError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoCommonAlgorithm(cat) => write!(f, "no common {} algorithm", cat),
            Self::InvalidEphemeralKey => write!(f, "invalid ephemeral key"),
            Self::MalformedMessage => write!(f, "malformed KEX message"),
            Self::UnexpectedMessage(t) => write!(f, "unexpected message type {} during KEX", t),
            Self::MlKemDecapsulationFailed => write!(f, "ML-KEM decapsulation failed"),
            Self::X25519ZeroOutput => write!(f, "X25519 produced all-zero output"),
            Self::Transport(e) => write!(f, "transport error: {}", e),
        }
    }
}
