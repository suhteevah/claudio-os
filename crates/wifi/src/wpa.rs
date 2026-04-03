//! WPA2-PSK authentication: PBKDF2-SHA1 key derivation, 4-way handshake,
//! and AES-CCMP encryption/decryption.
//!
//! # WPA2-PSK flow
//!
//! 1. **PSK derivation**: PBKDF2-HMAC-SHA1(passphrase, SSID, 4096, 32) -> PSK (= PMK).
//! 2. **4-way handshake** (EAPOL-Key frames):
//!    - AP -> STA: ANonce (message 1)
//!    - STA -> AP: SNonce + MIC (message 2)
//!    - AP -> STA: GTK + MIC (message 3)
//!    - STA -> AP: ACK + MIC (message 4)
//! 3. **PTK derivation**: PRF-384(PMK, "Pairwise key expansion", min(AA,SPA) || max(AA,SPA) || min(ANonce,SNonce) || max(ANonce,SNonce))
//!    - PTK = KCK (16B) || KEK (16B) || TK (16B)
//! 4. **AES-CCMP** encryption using the TK for unicast data frames.

use alloc::vec::Vec;

// -------------------------------------------------------------------
// HMAC-SHA1
// -------------------------------------------------------------------

/// SHA-1 block size.
const SHA1_BLOCK_SIZE: usize = 64;
/// SHA-1 digest size.
const SHA1_DIGEST_SIZE: usize = 20;

/// Minimal SHA-1 implementation for PBKDF2 and HMAC.
///
/// This is intentionally simple — WiFi auth is not performance-critical and
/// this avoids pulling in a crypto crate dependency.
struct Sha1 {
    state: [u32; 5],
    buffer: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha1 {
    fn new() -> Self {
        Self {
            state: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0],
            buffer: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        let mut offset = 0;
        self.total_len += data.len() as u64;

        // Fill existing buffer
        if self.buf_len > 0 {
            let space = 64 - self.buf_len;
            let copy_len = data.len().min(space);
            self.buffer[self.buf_len..self.buf_len + copy_len].copy_from_slice(&data[..copy_len]);
            self.buf_len += copy_len;
            offset += copy_len;

            if self.buf_len == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buf_len = 0;
            }
        }

        // Process full blocks
        while offset + 64 <= data.len() {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[offset..offset + 64]);
            self.compress(&block);
            offset += 64;
        }

        // Buffer remainder
        if offset < data.len() {
            let remaining = data.len() - offset;
            self.buffer[..remaining].copy_from_slice(&data[offset..]);
            self.buf_len = remaining;
        }
    }

    fn finalize(mut self) -> [u8; SHA1_DIGEST_SIZE] {
        let bit_len = self.total_len * 8;

        // Padding
        let mut pad = [0u8; 72]; // worst case: 64 + 8
        pad[0] = 0x80;
        let pad_len = if self.buf_len < 56 {
            56 - self.buf_len
        } else {
            120 - self.buf_len
        };
        self.update(&pad[..pad_len]);
        self.update(&bit_len.to_be_bytes());

        let mut digest = [0u8; SHA1_DIGEST_SIZE];
        for (i, &word) in self.state.iter().enumerate() {
            digest[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        digest
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = self.state;

        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };

            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
    }
}

/// Compute HMAC-SHA1(key, data).
fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; SHA1_DIGEST_SIZE] {
    let mut padded_key = [0u8; SHA1_BLOCK_SIZE];
    if key.len() > SHA1_BLOCK_SIZE {
        let mut h = Sha1::new();
        h.update(key);
        let digest = h.finalize();
        padded_key[..SHA1_DIGEST_SIZE].copy_from_slice(&digest);
    } else {
        padded_key[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; SHA1_BLOCK_SIZE];
    let mut opad = [0x5Cu8; SHA1_BLOCK_SIZE];
    for i in 0..SHA1_BLOCK_SIZE {
        ipad[i] ^= padded_key[i];
        opad[i] ^= padded_key[i];
    }

    // Inner hash
    let mut inner = Sha1::new();
    inner.update(&ipad);
    inner.update(data);
    let inner_digest = inner.finalize();

    // Outer hash
    let mut outer = Sha1::new();
    outer.update(&opad);
    outer.update(&inner_digest);
    outer.finalize()
}

// -------------------------------------------------------------------
// PBKDF2-HMAC-SHA1 (for PSK derivation)
// -------------------------------------------------------------------

/// Derive a WPA2 PSK from a passphrase and SSID using PBKDF2-HMAC-SHA1.
///
/// WPA2-PSK uses 4096 iterations and produces a 32-byte key (256 bits).
pub fn pbkdf2_sha1(passphrase: &[u8], ssid: &[u8], iterations: u32, key_len: usize) -> Vec<u8> {
    log::debug!(
        "wifi::wpa: PBKDF2-SHA1 — {} iterations, {} byte key",
        iterations, key_len
    );

    let mut output = Vec::with_capacity(key_len);
    let mut block = 1u32;

    while output.len() < key_len {
        // U_1 = HMAC-SHA1(passphrase, ssid || INT(block))
        let mut salt_block = Vec::with_capacity(ssid.len() + 4);
        salt_block.extend_from_slice(ssid);
        salt_block.extend_from_slice(&block.to_be_bytes());

        let mut u = hmac_sha1(passphrase, &salt_block);
        let mut result = u;

        // U_2 ... U_iterations
        for _ in 1..iterations {
            u = hmac_sha1(passphrase, &u);
            for j in 0..SHA1_DIGEST_SIZE {
                result[j] ^= u[j];
            }
        }

        let remaining = key_len - output.len();
        let copy_len = remaining.min(SHA1_DIGEST_SIZE);
        output.extend_from_slice(&result[..copy_len]);
        block += 1;
    }

    output
}

/// Derive a WPA2 PSK (Pairwise Master Key) from passphrase and SSID.
///
/// This is the standard WPA2-PSK derivation: PBKDF2-HMAC-SHA1 with 4096
/// iterations producing a 256-bit (32-byte) PMK.
pub fn derive_psk(passphrase: &str, ssid: &str) -> [u8; 32] {
    log::info!("wifi::wpa: deriving PSK for SSID=\"{}\"", ssid);
    let key = pbkdf2_sha1(passphrase.as_bytes(), ssid.as_bytes(), 4096, 32);
    let mut psk = [0u8; 32];
    psk.copy_from_slice(&key);
    psk
}

// -------------------------------------------------------------------
// PRF-X (pseudo-random function for PTK derivation)
// -------------------------------------------------------------------

/// PRF-384 used for PTK derivation.
///
/// PRF-X(K, A, B) = HMAC-SHA1(K, A || 0x00 || B || counter) for each block.
fn prf(key: &[u8], label: &[u8], data: &[u8], output_len: usize) -> Vec<u8> {
    let mut result = Vec::with_capacity(output_len);
    let mut counter = 0u8;

    while result.len() < output_len {
        let mut input = Vec::with_capacity(label.len() + 1 + data.len() + 1);
        input.extend_from_slice(label);
        input.push(0x00);
        input.extend_from_slice(data);
        input.push(counter);

        let hmac = hmac_sha1(key, &input);
        let remaining = output_len - result.len();
        let copy_len = remaining.min(SHA1_DIGEST_SIZE);
        result.extend_from_slice(&hmac[..copy_len]);
        counter += 1;
    }

    result
}

// -------------------------------------------------------------------
// PTK derivation
// -------------------------------------------------------------------

/// Pairwise Transient Key derived from the 4-way handshake.
#[derive(Debug, Clone)]
pub struct Ptk {
    /// Key Confirmation Key (16 bytes) — used for MIC in EAPOL frames.
    pub kck: [u8; 16],
    /// Key Encryption Key (16 bytes) — used to decrypt GTK in message 3.
    pub kek: [u8; 16],
    /// Temporal Key (16 bytes) — used for AES-CCMP data encryption.
    pub tk: [u8; 16],
}

/// Derive the PTK from the PMK and nonce values.
///
/// - `pmk`: Pairwise Master Key (= PSK for WPA2-Personal).
/// - `aa`: Authenticator Address (AP's MAC address).
/// - `spa`: Supplicant Address (our MAC address).
/// - `anonce`: Authenticator nonce (from 4-way handshake message 1).
/// - `snonce`: Supplicant nonce (generated by us for message 2).
pub fn derive_ptk(
    pmk: &[u8; 32],
    aa: &[u8; 6],
    spa: &[u8; 6],
    anonce: &[u8; 32],
    snonce: &[u8; 32],
) -> Ptk {
    log::info!("wifi::wpa: deriving PTK");

    // B = min(AA,SPA) || max(AA,SPA) || min(ANonce,SNonce) || max(ANonce,SNonce)
    let mut b = Vec::with_capacity(76);
    if aa < spa {
        b.extend_from_slice(aa);
        b.extend_from_slice(spa);
    } else {
        b.extend_from_slice(spa);
        b.extend_from_slice(aa);
    }
    if anonce < snonce {
        b.extend_from_slice(anonce);
        b.extend_from_slice(snonce);
    } else {
        b.extend_from_slice(snonce);
        b.extend_from_slice(anonce);
    }

    let ptk_data = prf(pmk, b"Pairwise key expansion", &b, 48);

    let mut kck = [0u8; 16];
    let mut kek = [0u8; 16];
    let mut tk = [0u8; 16];
    kck.copy_from_slice(&ptk_data[0..16]);
    kek.copy_from_slice(&ptk_data[16..32]);
    tk.copy_from_slice(&ptk_data[32..48]);

    log::debug!("wifi::wpa: PTK derived — KCK/KEK/TK ready");

    Ptk { kck, kek, tk }
}

// -------------------------------------------------------------------
// EAPOL-Key frame parsing and construction
// -------------------------------------------------------------------

/// EAPOL-Key frame type byte.
const EAPOL_KEY_TYPE: u8 = 0x03;
/// IEEE 802.1X-2004 version.
const EAPOL_VERSION: u8 = 0x02;

/// Key info flags.
const KEY_INFO_TYPE_HMAC_SHA1: u16 = 0x0002; // HMAC-SHA1 MIC + AES key wrap
const KEY_INFO_PAIRWISE: u16 = 0x0008;
const KEY_INFO_MIC: u16 = 0x0100;
const KEY_INFO_SECURE: u16 = 0x0200;
const KEY_INFO_ACK: u16 = 0x0080;

/// Parsed EAPOL-Key frame from the 4-way handshake.
#[derive(Debug, Clone)]
pub struct EapolKeyFrame {
    /// Key info field.
    pub key_info: u16,
    /// Key length (16 for CCMP).
    pub key_length: u16,
    /// Replay counter (monotonically increasing).
    pub replay_counter: u64,
    /// Nonce (32 bytes).
    pub nonce: [u8; 32],
    /// Key IV (16 bytes).
    pub key_iv: [u8; 16],
    /// MIC (16 bytes).
    pub mic: [u8; 16],
    /// Key data (encrypted GTK in message 3).
    pub key_data: Vec<u8>,
}

impl EapolKeyFrame {
    /// Parse an EAPOL-Key frame from raw bytes.
    ///
    /// `data` should start at the EAPOL header (version byte).
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 99 {
            log::warn!("wifi::wpa: EAPOL frame too short ({} bytes)", data.len());
            return None;
        }

        // EAPOL header: version(1) + type(1) + body_length(2)
        let _version = data[0];
        let pkt_type = data[1];
        if pkt_type != EAPOL_KEY_TYPE {
            log::warn!("wifi::wpa: not an EAPOL-Key frame (type=0x{:02X})", pkt_type);
            return None;
        }

        // Skip descriptor type byte (data[4])
        let key_info = u16::from_be_bytes([data[5], data[6]]);
        let key_length = u16::from_be_bytes([data[7], data[8]]);
        let replay_counter = u64::from_be_bytes([
            data[9], data[10], data[11], data[12],
            data[13], data[14], data[15], data[16],
        ]);

        let mut nonce = [0u8; 32];
        nonce.copy_from_slice(&data[17..49]);

        let mut key_iv = [0u8; 16];
        key_iv.copy_from_slice(&data[49..65]);

        // RSC (8 bytes) at 65..73, ID (8 bytes) at 73..81 — skip

        let mut mic = [0u8; 16];
        mic.copy_from_slice(&data[81..97]);

        let key_data_len = u16::from_be_bytes([data[97], data[98]]) as usize;
        let key_data = if data.len() >= 99 + key_data_len {
            data[99..99 + key_data_len].to_vec()
        } else {
            Vec::new()
        };

        log::trace!(
            "wifi::wpa: EAPOL-Key info=0x{:04X} key_len={} nonce[0..4]={:02X}{:02X}{:02X}{:02X}",
            key_info, key_length, nonce[0], nonce[1], nonce[2], nonce[3]
        );

        Some(Self {
            key_info,
            key_length,
            replay_counter,
            nonce,
            key_iv,
            mic,
            key_data,
        })
    }

    /// Determine which message of the 4-way handshake this is.
    pub fn message_number(&self) -> u8 {
        let has_ack = self.key_info & KEY_INFO_ACK != 0;
        let has_mic = self.key_info & KEY_INFO_MIC != 0;
        let has_secure = self.key_info & KEY_INFO_SECURE != 0;

        match (has_ack, has_mic, has_secure) {
            (true, false, false) => 1, // AP -> STA: ANonce
            (false, true, false) => 2, // STA -> AP: SNonce + MIC
            (true, true, true) => 3,   // AP -> STA: GTK + MIC (install)
            (false, true, true) => 4,  // STA -> AP: ACK
            _ => 0,                    // Unknown
        }
    }
}

/// Build EAPOL-Key message 2 (SNonce + MIC).
pub fn build_eapol_msg2(
    snonce: &[u8; 32],
    replay_counter: u64,
    kck: &[u8; 16],
    rsn_ie: &[u8],
) -> Vec<u8> {
    log::info!("wifi::wpa: building EAPOL-Key message 2");

    let key_data_len = rsn_ie.len();
    let body_len = 95 + key_data_len;

    let mut frame = Vec::with_capacity(4 + body_len);

    // EAPOL header
    frame.push(EAPOL_VERSION);
    frame.push(EAPOL_KEY_TYPE);
    frame.extend_from_slice(&(body_len as u16).to_be_bytes());

    // Descriptor type: EAPOL-Key
    frame.push(0x02); // IEEE 802.11 key descriptor

    // Key info: pairwise + HMAC-SHA1/AES + MIC
    let key_info: u16 = KEY_INFO_TYPE_HMAC_SHA1 | KEY_INFO_PAIRWISE | KEY_INFO_MIC;
    frame.extend_from_slice(&key_info.to_be_bytes());

    // Key length
    frame.extend_from_slice(&16u16.to_be_bytes());

    // Replay counter
    frame.extend_from_slice(&replay_counter.to_be_bytes());

    // SNonce
    frame.extend_from_slice(snonce);

    // Key IV (zeros)
    frame.extend_from_slice(&[0u8; 16]);

    // RSC (zeros)
    frame.extend_from_slice(&[0u8; 8]);

    // ID (zeros)
    frame.extend_from_slice(&[0u8; 8]);

    // MIC placeholder (16 bytes of zeros — computed below)
    let mic_offset = frame.len();
    frame.extend_from_slice(&[0u8; 16]);

    // Key data length + key data (RSN IE)
    frame.extend_from_slice(&(key_data_len as u16).to_be_bytes());
    frame.extend_from_slice(rsn_ie);

    // Compute MIC over the entire EAPOL frame (with MIC field zeroed)
    let mic = hmac_sha1(kck, &frame);
    frame[mic_offset..mic_offset + 16].copy_from_slice(&mic[..16]);

    log::debug!("wifi::wpa: EAPOL message 2 built ({} bytes)", frame.len());
    frame
}

/// Build EAPOL-Key message 4 (ACK).
pub fn build_eapol_msg4(
    replay_counter: u64,
    kck: &[u8; 16],
) -> Vec<u8> {
    log::info!("wifi::wpa: building EAPOL-Key message 4 (ACK)");

    let body_len = 95;
    let mut frame = Vec::with_capacity(4 + body_len);

    // EAPOL header
    frame.push(EAPOL_VERSION);
    frame.push(EAPOL_KEY_TYPE);
    frame.extend_from_slice(&(body_len as u16).to_be_bytes());

    // Descriptor type
    frame.push(0x02);

    // Key info: pairwise + HMAC-SHA1/AES + MIC + Secure
    let key_info: u16 = KEY_INFO_TYPE_HMAC_SHA1 | KEY_INFO_PAIRWISE | KEY_INFO_MIC | KEY_INFO_SECURE;
    frame.extend_from_slice(&key_info.to_be_bytes());

    // Key length
    frame.extend_from_slice(&16u16.to_be_bytes());

    // Replay counter
    frame.extend_from_slice(&replay_counter.to_be_bytes());

    // Nonce (zeros for message 4)
    frame.extend_from_slice(&[0u8; 32]);

    // Key IV
    frame.extend_from_slice(&[0u8; 16]);

    // RSC
    frame.extend_from_slice(&[0u8; 8]);

    // ID
    frame.extend_from_slice(&[0u8; 8]);

    // MIC placeholder
    let mic_offset = frame.len();
    frame.extend_from_slice(&[0u8; 16]);

    // Key data length (0)
    frame.extend_from_slice(&0u16.to_be_bytes());

    // Compute MIC
    let mic = hmac_sha1(kck, &frame);
    frame[mic_offset..mic_offset + 16].copy_from_slice(&mic[..16]);

    log::debug!("wifi::wpa: EAPOL message 4 built ({} bytes)", frame.len());
    frame
}

// -------------------------------------------------------------------
// AES-CCMP (128-bit) — 802.11i data encryption
// -------------------------------------------------------------------

/// AES-128 block size.
const AES_BLOCK_SIZE: usize = 16;

/// AES-128 key schedule (expanded key).
pub struct Aes128 {
    round_keys: [[u8; 16]; 11],
}

impl Aes128 {
    /// Create a new AES-128 cipher from a 16-byte key.
    pub fn new(key: &[u8; 16]) -> Self {
        let round_keys = aes_key_expand(key);
        Self { round_keys }
    }

    /// Encrypt a single 16-byte block in place.
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        aes_encrypt_block(block, &self.round_keys);
    }
}

/// CCMP nonce for a given frame.
pub struct CcmpNonce {
    /// Priority (QoS TID, or 0).
    pub priority: u8,
    /// Source MAC address.
    pub addr: [u8; 6],
    /// Packet number (48-bit, little-endian in CCMP header).
    pub pn: u64,
}

impl CcmpNonce {
    /// Serialize to the 13-byte nonce used by AES-CCM.
    pub fn to_bytes(&self) -> [u8; 13] {
        let mut nonce = [0u8; 13];
        nonce[0] = self.priority;
        nonce[1..7].copy_from_slice(&self.addr);
        // PN bytes in big-endian order within the nonce
        let pn_bytes = self.pn.to_be_bytes();
        nonce[7..13].copy_from_slice(&pn_bytes[2..8]);
        nonce
    }
}

/// Encrypt a plaintext payload using AES-128-CCMP.
///
/// Returns the ciphertext with an 8-byte MIC appended.
///
/// `aad` is the Additional Authenticated Data (constructed from the 802.11 header).
pub fn ccmp_encrypt(
    tk: &[u8; 16],
    nonce: &CcmpNonce,
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    let cipher = Aes128::new(tk);
    let nonce_bytes = nonce.to_bytes();

    // CCM: compute MIC (CBC-MAC), then CTR-encrypt
    let mic = ccm_cbc_mac(&cipher, &nonce_bytes, aad, plaintext, 8);

    let mut ciphertext = ccm_ctr_encrypt(&cipher, &nonce_bytes, plaintext);

    // Encrypt the MIC with counter 0
    let mut ctr0 = ccm_ctr_block(&nonce_bytes, 0);
    cipher.encrypt_block(&mut ctr0);
    let mut encrypted_mic = [0u8; 8];
    for i in 0..8 {
        encrypted_mic[i] = mic[i] ^ ctr0[i];
    }
    ciphertext.extend_from_slice(&encrypted_mic);

    ciphertext
}

/// Decrypt a CCMP ciphertext (with 8-byte MIC at the end).
///
/// Returns `None` if the MIC verification fails.
pub fn ccmp_decrypt(
    tk: &[u8; 16],
    nonce: &CcmpNonce,
    aad: &[u8],
    ciphertext_with_mic: &[u8],
) -> Option<Vec<u8>> {
    if ciphertext_with_mic.len() < 8 {
        return None;
    }

    let cipher = Aes128::new(tk);
    let nonce_bytes = nonce.to_bytes();

    let ciphertext = &ciphertext_with_mic[..ciphertext_with_mic.len() - 8];
    let received_mic = &ciphertext_with_mic[ciphertext_with_mic.len() - 8..];

    // CTR-decrypt the data
    let plaintext = ccm_ctr_encrypt(&cipher, &nonce_bytes, ciphertext); // CTR is symmetric

    // Decrypt the MIC
    let mut ctr0 = ccm_ctr_block(&nonce_bytes, 0);
    cipher.encrypt_block(&mut ctr0);
    let mut decrypted_mic = [0u8; 8];
    for i in 0..8 {
        decrypted_mic[i] = received_mic[i] ^ ctr0[i];
    }

    // Compute expected MIC
    let expected_mic = ccm_cbc_mac(&cipher, &nonce_bytes, aad, &plaintext, 8);

    // Constant-time comparison
    let mut diff = 0u8;
    for i in 0..8 {
        diff |= decrypted_mic[i] ^ expected_mic[i];
    }

    if diff != 0 {
        log::warn!("wifi::wpa: CCMP MIC verification failed");
        return None;
    }

    Some(plaintext)
}

// -------------------------------------------------------------------
// CCM internal helpers
// -------------------------------------------------------------------

fn ccm_ctr_block(nonce: &[u8; 13], counter: u16) -> [u8; 16] {
    let mut block = [0u8; 16];
    block[0] = 0x01; // Flags: L=2 (counter is 2 bytes)
    block[1..14].copy_from_slice(nonce);
    block[14] = (counter >> 8) as u8;
    block[15] = counter as u8;
    block
}

fn ccm_ctr_encrypt(cipher: &Aes128, nonce: &[u8; 13], data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len());
    let mut counter = 1u16;

    for chunk in data.chunks(AES_BLOCK_SIZE) {
        let mut keystream = ccm_ctr_block(nonce, counter);
        cipher.encrypt_block(&mut keystream);

        for (i, &byte) in chunk.iter().enumerate() {
            output.push(byte ^ keystream[i]);
        }
        counter += 1;
    }

    output
}

fn ccm_cbc_mac(
    cipher: &Aes128,
    nonce: &[u8; 13],
    aad: &[u8],
    plaintext: &[u8],
    mic_len: usize,
) -> Vec<u8> {
    // B_0: flags + nonce + message length
    let mut b0 = [0u8; 16];
    let flags = 0x40 // AAD present
        | ((((mic_len - 2) / 2) as u8) << 3) // M field
        | 0x01; // L field (counter bytes - 1 = 1)
    b0[0] = flags;
    b0[1..14].copy_from_slice(nonce);
    let data_len = plaintext.len() as u16;
    b0[14] = (data_len >> 8) as u8;
    b0[15] = data_len as u8;

    let mut tag = b0;
    cipher.encrypt_block(&mut tag);

    // AAD: prepend 2-byte length, then XOR in blocks
    if !aad.is_empty() {
        let aad_len = aad.len() as u16;
        let mut aad_buf = Vec::with_capacity(2 + aad.len());
        aad_buf.extend_from_slice(&aad_len.to_be_bytes());
        aad_buf.extend_from_slice(aad);
        // Pad to block boundary
        while aad_buf.len() % AES_BLOCK_SIZE != 0 {
            aad_buf.push(0);
        }
        for chunk in aad_buf.chunks(AES_BLOCK_SIZE) {
            for i in 0..AES_BLOCK_SIZE {
                tag[i] ^= chunk[i];
            }
            cipher.encrypt_block(&mut tag);
        }
    }

    // Plaintext blocks
    let mut padded = plaintext.to_vec();
    while padded.len() % AES_BLOCK_SIZE != 0 {
        padded.push(0);
    }
    for chunk in padded.chunks(AES_BLOCK_SIZE) {
        for i in 0..AES_BLOCK_SIZE {
            tag[i] ^= chunk[i];
        }
        cipher.encrypt_block(&mut tag);
    }

    tag[..mic_len].to_vec()
}

// -------------------------------------------------------------------
// AES-128 core (minimal, no_std)
// -------------------------------------------------------------------

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

fn aes_key_expand(key: &[u8; 16]) -> [[u8; 16]; 11] {
    let mut round_keys = [[0u8; 16]; 11];
    round_keys[0] = *key;

    for i in 1..11 {
        let prev = round_keys[i - 1];
        let mut next = [0u8; 16];

        // RotWord + SubWord + Rcon
        let t = [
            SBOX[prev[13] as usize] ^ RCON[i - 1],
            SBOX[prev[14] as usize],
            SBOX[prev[15] as usize],
            SBOX[prev[12] as usize],
        ];

        for j in 0..4 {
            next[j] = prev[j] ^ t[j];
        }
        for j in 4..16 {
            next[j] = prev[j] ^ next[j - 4];
        }

        round_keys[i] = next;
    }

    round_keys
}

fn gf_mul2(x: u8) -> u8 {
    if x & 0x80 != 0 {
        (x << 1) ^ 0x1b
    } else {
        x << 1
    }
}

fn aes_encrypt_block(block: &mut [u8; 16], round_keys: &[[u8; 16]; 11]) {
    // AddRoundKey (initial)
    for i in 0..16 {
        block[i] ^= round_keys[0][i];
    }

    for round in 1..10 {
        // SubBytes
        for b in block.iter_mut() {
            *b = SBOX[*b as usize];
        }

        // ShiftRows
        let tmp = block[1];
        block[1] = block[5];
        block[5] = block[9];
        block[9] = block[13];
        block[13] = tmp;

        let tmp = block[2];
        block[2] = block[10];
        block[10] = tmp;
        let tmp = block[6];
        block[6] = block[14];
        block[14] = tmp;

        let tmp = block[15];
        block[15] = block[11];
        block[11] = block[7];
        block[7] = block[3];
        block[3] = tmp;

        // MixColumns
        for col in 0..4 {
            let i = col * 4;
            let a = block[i];
            let b = block[i + 1];
            let c = block[i + 2];
            let d = block[i + 3];
            let sum = a ^ b ^ c ^ d;
            block[i] = a ^ sum ^ gf_mul2(a ^ b);
            block[i + 1] = b ^ sum ^ gf_mul2(b ^ c);
            block[i + 2] = c ^ sum ^ gf_mul2(c ^ d);
            block[i + 3] = d ^ sum ^ gf_mul2(d ^ a);
        }

        // AddRoundKey
        for i in 0..16 {
            block[i] ^= round_keys[round][i];
        }
    }

    // Final round (no MixColumns)
    for b in block.iter_mut() {
        *b = SBOX[*b as usize];
    }

    // ShiftRows
    let tmp = block[1];
    block[1] = block[5];
    block[5] = block[9];
    block[9] = block[13];
    block[13] = tmp;

    let tmp = block[2];
    block[2] = block[10];
    block[10] = tmp;
    let tmp = block[6];
    block[6] = block[14];
    block[14] = tmp;

    let tmp = block[15];
    block[15] = block[11];
    block[11] = block[7];
    block[7] = block[3];
    block[3] = tmp;

    // AddRoundKey
    for i in 0..16 {
        block[i] ^= round_keys[10][i];
    }
}
