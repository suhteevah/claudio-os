//! Disk encryption layer — LUKS-like transparent sector encryption.
//!
//! Provides AES-256-XTS encryption for block devices with PBKDF2-SHA256
//! key derivation from a passphrase. The master key lives only in memory
//! and is never persisted unencrypted.
//!
//! Architecture:
//! - `EncryptedBlockDevice<D>` wraps any `BlockDevice` and transparently
//!   encrypts writes / decrypts reads at the sector level.
//! - PBKDF2-SHA256 derives a 64-byte key (two 32-byte AES-256 keys for XTS)
//!   from the user passphrase + a random salt stored in the partition header.
//! - The header (first sector) contains: magic, version, salt, encrypted
//!   master key verification hash.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use alloc::format;
use core::fmt;
use spin::Mutex;

// ── Armed cryptsetup devices ─────────────────────────────────────────

/// Devices for which a passphrase has been captured via `cryptsetup open`.
///
/// Maps device path (e.g. `/dev/sda1`) to the mapper name (e.g. `secrets`).
/// Real decryption is not performed yet — that requires a LUKS2 header parser
/// and a live block device handle. This static lets us track which devices
/// the user has "armed" so other commands (e.g. `cryptsetup status`) can
/// report them.
///
/// Production note: a real implementation must (1) disable terminal echo
/// while reading the passphrase, (2) zero the passphrase buffer with `volatile`
/// writes as soon as the derived key is computed, and (3) store the derived
/// key in mlock'd / non-swappable memory. This stub does none of that.
static ARMED_DEVICES: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

// ── Constants ────────────────────────────────────────────────────────

/// Sector size for encryption (512 bytes, standard).
pub const SECTOR_SIZE: usize = 512;

/// PBKDF2 iteration count — balance between security and bare-metal speed.
/// On our target hardware (i9-11900K) this takes ~200ms.
pub const PBKDF2_ITERATIONS: u32 = 100_000;

/// Magic bytes identifying a ClaudioOS encrypted partition header.
pub const ENCRYPTION_MAGIC: [u8; 8] = *b"CLAUENC\0";

/// Header version.
pub const HEADER_VERSION: u32 = 1;

// ── Block device trait ───────────────────────────────────────────────

/// Minimal block device trait for encryption wrapping.
pub trait BlockDevice {
    /// Read a sector by index into the provided buffer.
    fn read_sector(&self, sector: u64, buf: &mut [u8]) -> Result<(), BlockError>;
    /// Write a sector by index from the provided buffer.
    fn write_sector(&mut self, sector: u64, buf: &[u8]) -> Result<(), BlockError>;
    /// Total number of sectors.
    fn sector_count(&self) -> u64;
}

/// Block device error type.
#[derive(Debug, Clone, Copy)]
pub enum BlockError {
    /// I/O error from the underlying device.
    IoError,
    /// Sector index out of range.
    OutOfRange,
    /// Encryption/decryption failure.
    CryptoError,
    /// Bad passphrase or corrupted header.
    AuthenticationFailed,
    /// Device not yet unlocked.
    Locked,
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockError::IoError => write!(f, "I/O error"),
            BlockError::OutOfRange => write!(f, "sector out of range"),
            BlockError::CryptoError => write!(f, "crypto error"),
            BlockError::AuthenticationFailed => write!(f, "authentication failed"),
            BlockError::Locked => write!(f, "device is locked"),
        }
    }
}

// ── AES-256 implementation (using our existing AES from TLS stack) ───

/// Encrypt a single 16-byte block with AES-256 (FIPS 197).
///
/// This is a minimal software implementation of AES-256 for use in XTS mode.
/// AES-256 uses 14 rounds and a 256-bit (32-byte) key.
///
/// # Parameters
/// - `key`: The 32-byte AES-256 encryption key.
/// - `block`: The 16-byte block to encrypt, modified in-place.
///
/// # Algorithm Overview
/// 1. **Key Expansion**: The 32-byte key is expanded into 60 32-bit round keys.
/// 2. **Initial AddRoundKey**: XOR the plaintext with round key 0.
/// 3. **Rounds 1-13**: SubBytes -> ShiftRows -> MixColumns -> AddRoundKey.
/// 4. **Final Round 14**: SubBytes -> ShiftRows -> AddRoundKey (no MixColumns).
///
/// The state is organized as a 4x4 column-major matrix of bytes.
fn aes256_encrypt_block(key: &[u8; 32], block: &mut [u8; 16]) {
    // Rijndael S-box: a fixed nonlinear substitution table that provides
    // confusion (breaking the relationship between key and ciphertext).
    // Each byte of state is replaced by SBOX[byte]. The S-box is derived
    // from the multiplicative inverse in GF(2^8) followed by an affine transform.
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

    // Round constants (Rcon): powers of 2 in GF(2^8), used in key expansion
    // to break symmetry between rounds. Each value is x^(i-1) mod the AES
    // polynomial. Values wrap around via GF reduction (0x80 -> 0x1b -> 0x36).
    const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

    // Key expansion: AES-256 uses 14 rounds, requiring 15 round keys.
    // Each round key is 4 words (128 bits), so we need 15 * 4 = 60 words total.
    // The first 8 words come directly from the 256-bit key.
    let mut rk = [0u32; 60];
    for i in 0..8 {
        rk[i] = u32::from_be_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
    }
    // Generate remaining round keys from the initial 8 words.
    // AES-256 key schedule applies RotWord + SubWord + Rcon every 8th word,
    // and SubWord alone every 4th word (this extra SubWord step is unique
    // to AES-256 and prevents related-key attacks).
    for i in 8..60 {
        let mut temp = rk[i - 1];
        if i % 8 == 0 {
            // Every 8th word: RotWord (rotate bytes left by 1) + SubWord (S-box each byte) + Rcon
            temp = temp.rotate_left(8);
            let b = temp.to_be_bytes();
            temp = u32::from_be_bytes([SBOX[b[0] as usize], SBOX[b[1] as usize], SBOX[b[2] as usize], SBOX[b[3] as usize]]);
            temp ^= (RCON[i / 8 - 1] as u32) << 24; // XOR Rcon into the high byte
        } else if i % 8 == 4 {
            // Every 4th word (AES-256 only): SubWord without rotation
            let b = temp.to_be_bytes();
            temp = u32::from_be_bytes([SBOX[b[0] as usize], SBOX[b[1] as usize], SBOX[b[2] as usize], SBOX[b[3] as usize]]);
        }
        rk[i] = rk[i - 8] ^ temp;
    }

    // Load the plaintext block into the 4x4 column-major state matrix
    let mut state = [0u8; 16];
    state.copy_from_slice(block);

    // AddRoundKey (round 0): XOR the plaintext with the first round key
    for c in 0..4 {
        let k = rk[c].to_be_bytes();
        for r in 0..4 {
            state[r + 4 * c] ^= k[r];
        }
    }

    // Rounds 1 through 13: full AES rounds (SubBytes, ShiftRows, MixColumns, AddRoundKey)
    for round in 1..14 {
        // SubBytes: Apply the S-box to every byte (nonlinear substitution)
        for b in state.iter_mut() {
            *b = SBOX[*b as usize];
        }
        // ShiftRows: Cyclically shift each row of the state matrix left.
        // Row 0: no shift, Row 1: shift 1, Row 2: shift 2, Row 3: shift 3.
        let tmp = state[1];
        state[1] = state[5];
        state[5] = state[9];
        state[9] = state[13];
        state[13] = tmp;

        let tmp = state[2];
        state[2] = state[10];
        state[10] = tmp;
        let tmp = state[6];
        state[6] = state[14];
        state[14] = tmp;

        let tmp = state[15];
        state[15] = state[11];
        state[11] = state[7];
        state[7] = state[3];
        state[3] = tmp;

        // MixColumns: Mix bytes within each column using GF(2^8) arithmetic.
        // Each column is treated as a polynomial over GF(2^8) and multiplied
        // by the fixed polynomial {03}x^3 + {01}x^2 + {01}x + {02} modulo x^4 + 1.
        // This provides diffusion (spreading influence of each input byte).
        for c in 0..4 {
            let s0 = state[4 * c];
            let s1 = state[4 * c + 1];
            let s2 = state[4 * c + 2];
            let s3 = state[4 * c + 3];
            state[4 * c]     = gf_mul2(s0) ^ gf_mul3(s1) ^ s2 ^ s3;
            state[4 * c + 1] = s0 ^ gf_mul2(s1) ^ gf_mul3(s2) ^ s3;
            state[4 * c + 2] = s0 ^ s1 ^ gf_mul2(s2) ^ gf_mul3(s3);
            state[4 * c + 3] = gf_mul3(s0) ^ s1 ^ s2 ^ gf_mul2(s3);
        }

        // AddRoundKey
        for c in 0..4 {
            let k = rk[round * 4 + c].to_be_bytes();
            for r in 0..4 {
                state[r + 4 * c] ^= k[r];
            }
        }
    }

    // Final round (14): SubBytes, ShiftRows, AddRoundKey (no MixColumns)
    for b in state.iter_mut() {
        *b = SBOX[*b as usize];
    }
    // ShiftRows
    let tmp = state[1];
    state[1] = state[5];
    state[5] = state[9];
    state[9] = state[13];
    state[13] = tmp;

    let tmp = state[2];
    state[2] = state[10];
    state[10] = tmp;
    let tmp = state[6];
    state[6] = state[14];
    state[14] = tmp;

    let tmp = state[15];
    state[15] = state[11];
    state[11] = state[7];
    state[7] = state[3];
    state[3] = tmp;

    // AddRoundKey (round 14)
    for c in 0..4 {
        let k = rk[56 + c].to_be_bytes();
        for r in 0..4 {
            state[r + 4 * c] ^= k[r];
        }
    }

    block.copy_from_slice(&state);
}

/// GF(2^8) multiply by 2 (xtime operation).
///
/// Multiplication in GF(2^8) with the AES irreducible polynomial
/// x^8 + x^4 + x^3 + x + 1 (0x11B). If the high bit is set before
/// the shift, we reduce modulo the polynomial.
fn gf_mul2(x: u8) -> u8 {
    let mut r = (x as u16) << 1;
    if r & 0x100 != 0 {
        r ^= 0x11b; // Reduce modulo AES polynomial
    }
    r as u8
}

/// GF(2^8) multiply by 3 = mul2(x) XOR x.
///
/// Used in the MixColumns step of AES encryption.
fn gf_mul3(x: u8) -> u8 {
    gf_mul2(x) ^ x
}

// ── AES-256 decryption ───────────────────────────────────────────────

/// Decrypt a single 16-byte block with AES-256 (FIPS 197, inverse cipher).
///
/// Applies the AES rounds in reverse order using inverse operations:
/// InvShiftRows, InvSubBytes (using the inverse S-box), AddRoundKey,
/// and InvMixColumns. The key expansion is identical to encryption.
///
/// # Parameters
/// - `key`: The 32-byte AES-256 key (same key used for encryption).
/// - `block`: The 16-byte ciphertext block to decrypt, modified in-place.
fn aes256_decrypt_block(key: &[u8; 32], block: &mut [u8; 16]) {
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

    // Inverse S-box
    const INV_SBOX: [u8; 256] = [
        0x52, 0x09, 0x6a, 0xd5, 0x30, 0x36, 0xa5, 0x38, 0xbf, 0x40, 0xa3, 0x9e, 0x81, 0xf3, 0xd7, 0xfb,
        0x7c, 0xe3, 0x39, 0x82, 0x9b, 0x2f, 0xff, 0x87, 0x34, 0x8e, 0x43, 0x44, 0xc4, 0xde, 0xe9, 0xcb,
        0x54, 0x7b, 0x94, 0x32, 0xa6, 0xc2, 0x23, 0x3d, 0xee, 0x4c, 0x95, 0x0b, 0x42, 0xfa, 0xc3, 0x4e,
        0x08, 0x2e, 0xa1, 0x66, 0x28, 0xd9, 0x24, 0xb2, 0x76, 0x5b, 0xa2, 0x49, 0x6d, 0x8b, 0xd1, 0x25,
        0x72, 0xf8, 0xf6, 0x64, 0x86, 0x68, 0x98, 0x16, 0xd4, 0xa4, 0x5c, 0xcc, 0x5d, 0x65, 0xb6, 0x92,
        0x6c, 0x70, 0x48, 0x50, 0xfd, 0xed, 0xb9, 0xda, 0x5e, 0x15, 0x46, 0x57, 0xa7, 0x8d, 0x9d, 0x84,
        0x90, 0xd8, 0xab, 0x00, 0x8c, 0xbc, 0xd3, 0x0a, 0xf7, 0xe4, 0x58, 0x05, 0xb8, 0xb3, 0x45, 0x06,
        0xd0, 0x2c, 0x1e, 0x8f, 0xca, 0x3f, 0x0f, 0x02, 0xc1, 0xaf, 0xbd, 0x03, 0x01, 0x13, 0x8a, 0x6b,
        0x3a, 0x91, 0x11, 0x41, 0x4f, 0x67, 0xdc, 0xea, 0x97, 0xf2, 0xcf, 0xce, 0xf0, 0xb4, 0xe6, 0x73,
        0x96, 0xac, 0x74, 0x22, 0xe7, 0xad, 0x35, 0x85, 0xe2, 0xf9, 0x37, 0xe8, 0x1c, 0x75, 0xdf, 0x6e,
        0x47, 0xf1, 0x1a, 0x71, 0x1d, 0x29, 0xc5, 0x89, 0x6f, 0xb7, 0x62, 0x0e, 0xaa, 0x18, 0xbe, 0x1b,
        0xfc, 0x56, 0x3e, 0x4b, 0xc6, 0xd2, 0x79, 0x20, 0x9a, 0xdb, 0xc0, 0xfe, 0x78, 0xcd, 0x5a, 0xf4,
        0x1f, 0xdd, 0xa8, 0x33, 0x88, 0x07, 0xc7, 0x31, 0xb1, 0x12, 0x10, 0x59, 0x27, 0x80, 0xec, 0x5f,
        0x60, 0x51, 0x7f, 0xa9, 0x19, 0xb5, 0x4a, 0x0d, 0x2d, 0xe5, 0x7a, 0x9f, 0x93, 0xc9, 0x9c, 0xef,
        0xa0, 0xe0, 0x3b, 0x4d, 0xae, 0x2a, 0xf5, 0xb0, 0xc8, 0xeb, 0xbb, 0x3c, 0x83, 0x53, 0x99, 0x61,
        0x17, 0x2b, 0x04, 0x7e, 0xba, 0x77, 0xd6, 0x26, 0xe1, 0x69, 0x14, 0x63, 0x55, 0x21, 0x0c, 0x7d,
    ];

    const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

    // Key expansion (same as encrypt)
    let mut rk = [0u32; 60];
    for i in 0..8 {
        rk[i] = u32::from_be_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
    }
    for i in 8..60 {
        let mut temp = rk[i - 1];
        if i % 8 == 0 {
            temp = temp.rotate_left(8);
            let b = temp.to_be_bytes();
            temp = u32::from_be_bytes([SBOX[b[0] as usize], SBOX[b[1] as usize], SBOX[b[2] as usize], SBOX[b[3] as usize]]);
            temp ^= (RCON[i / 8 - 1] as u32) << 24;
        } else if i % 8 == 4 {
            let b = temp.to_be_bytes();
            temp = u32::from_be_bytes([SBOX[b[0] as usize], SBOX[b[1] as usize], SBOX[b[2] as usize], SBOX[b[3] as usize]]);
        }
        rk[i] = rk[i - 8] ^ temp;
    }

    let mut state = [0u8; 16];
    state.copy_from_slice(block);

    // AddRoundKey (round 14)
    for c in 0..4 {
        let k = rk[56 + c].to_be_bytes();
        for r in 0..4 {
            state[r + 4 * c] ^= k[r];
        }
    }

    // Rounds 13..1 (inverse full rounds)
    for round in (1..14).rev() {
        // InvShiftRows
        let tmp = state[13];
        state[13] = state[9];
        state[9] = state[5];
        state[5] = state[1];
        state[1] = tmp;

        let tmp = state[10];
        state[10] = state[2];
        state[2] = tmp;
        let tmp = state[14];
        state[14] = state[6];
        state[6] = tmp;

        let tmp = state[3];
        state[3] = state[7];
        state[7] = state[11];
        state[11] = state[15];
        state[15] = tmp;

        // InvSubBytes
        for b in state.iter_mut() {
            *b = INV_SBOX[*b as usize];
        }

        // AddRoundKey
        for c in 0..4 {
            let k = rk[round * 4 + c].to_be_bytes();
            for r in 0..4 {
                state[r + 4 * c] ^= k[r];
            }
        }

        // InvMixColumns
        for c in 0..4 {
            let s0 = state[4 * c];
            let s1 = state[4 * c + 1];
            let s2 = state[4 * c + 2];
            let s3 = state[4 * c + 3];
            state[4 * c]     = gf_mul(s0, 14) ^ gf_mul(s1, 11) ^ gf_mul(s2, 13) ^ gf_mul(s3, 9);
            state[4 * c + 1] = gf_mul(s0, 9) ^ gf_mul(s1, 14) ^ gf_mul(s2, 11) ^ gf_mul(s3, 13);
            state[4 * c + 2] = gf_mul(s0, 13) ^ gf_mul(s1, 9) ^ gf_mul(s2, 14) ^ gf_mul(s3, 11);
            state[4 * c + 3] = gf_mul(s0, 11) ^ gf_mul(s1, 13) ^ gf_mul(s2, 9) ^ gf_mul(s3, 14);
        }
    }

    // Final inverse round (round 0): InvShiftRows, InvSubBytes, AddRoundKey
    let tmp = state[13];
    state[13] = state[9];
    state[9] = state[5];
    state[5] = state[1];
    state[1] = tmp;

    let tmp = state[10];
    state[10] = state[2];
    state[2] = tmp;
    let tmp = state[14];
    state[14] = state[6];
    state[6] = tmp;

    let tmp = state[3];
    state[3] = state[7];
    state[7] = state[11];
    state[11] = state[15];
    state[15] = tmp;

    for b in state.iter_mut() {
        *b = INV_SBOX[*b as usize];
    }

    for c in 0..4 {
        let k = rk[c].to_be_bytes();
        for r in 0..4 {
            state[r + 4 * c] ^= k[r];
        }
    }

    block.copy_from_slice(&state);
}

/// GF(2^8) general multiplication for InvMixColumns.
///
/// Computes `a * b` in GF(2^8) with the AES irreducible polynomial
/// x^8 + x^4 + x^3 + x + 1 (represented as 0x11B).
///
/// Uses the "Russian peasant" (binary) multiplication algorithm:
/// - If the low bit of `b` is set, XOR `a` into the result.
/// - Shift `a` left by 1 (multiply by x in GF).
/// - If `a` overflowed (high bit was set), reduce modulo 0x1B.
/// - Shift `b` right by 1.
/// - Repeat 8 times.
///
/// The constant 0x1B = 0b00011011 is the low byte of the reduction
/// polynomial (x^4 + x^3 + x + 1), since the x^8 term is implicit.
fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut result: u8 = 0;
    for _ in 0..8 {
        if b & 1 != 0 {
            result ^= a; // Add (XOR) a into result if low bit of b is set
        }
        let hi = a & 0x80; // Check if a will overflow on left shift
        a <<= 1;           // Multiply a by x (shift left)
        if hi != 0 {
            a ^= 0x1b;     // Reduce modulo the AES polynomial if overflow
        }
        b >>= 1;           // Move to next bit of b
    }
    result
}

// ── PBKDF2-SHA256 ────────────────────────────────────────────────────

/// SHA-256 hash (FIPS 180-4). Minimal implementation for PBKDF2 use.
///
/// Produces a 256-bit (32-byte) digest. This is the same SHA-256 used
/// throughout the encryption module (PBKDF2, HMAC, key verification).
///
/// # Algorithm
/// 1. Pad the message to a multiple of 512 bits (64 bytes).
/// 2. Process each 512-bit block through 64 rounds of compression.
/// 3. Output the final 8 x 32-bit hash state as big-endian bytes.
fn sha256(data: &[u8]) -> [u8; 32] {
    // SHA-256 round constants: first 32 bits of the fractional parts of the
    // cube roots of the first 64 prime numbers. These provide "nothing up my
    // sleeve" numbers that inject asymmetry into each round.
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    // Initial hash values: first 32 bits of the fractional parts of the
    // square roots of the first 8 primes (2, 3, 5, 7, 11, 13, 17, 19).
    let mut h: [u32; 8] = [
        0x6a09e667, // sqrt(2)
        0xbb67ae85, // sqrt(3)
        0x3c6ef372, // sqrt(5)
        0xa54ff53a, // sqrt(7)
        0x510e527f, // sqrt(11)
        0x9b05688c, // sqrt(13)
        0x1f83d9ab, // sqrt(17)
        0x5be0cd19, // sqrt(19)
    ];

    // Pre-processing: pad message
    let bit_len = (data.len() as u64) * 8;
    let mut padded = Vec::from(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit (64-byte) block
    for chunk in padded.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, val) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&val.to_be_bytes());
    }
    out
}

/// HMAC-SHA256 (RFC 2104).
///
/// Computes `H((K XOR opad) || H((K XOR ipad) || message))` where
/// `ipad` = 0x36 repeated, `opad` = 0x5C repeated, and `H` = SHA-256.
/// Keys longer than the block size (64 bytes) are first hashed.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let h = sha256(key);
        k[..32].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }

    let mut inner = Vec::with_capacity(64 + message.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(message);
    let inner_hash = sha256(&inner);

    let mut outer = Vec::with_capacity(64 + 32);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&inner_hash);
    sha256(&outer)
}

/// PBKDF2-SHA256 key derivation (RFC 2898 / NIST SP 800-132).
///
/// Derives a cryptographic key of `dk_len` bytes from a user-provided
/// password and a random salt. The `iterations` parameter controls the
/// computational cost, making brute-force attacks expensive.
///
/// # How It Works
/// For each 32-byte block of output:
/// 1. Compute `U_1 = HMAC-SHA256(password, salt || block_index)`
/// 2. For i = 2 to iterations: `U_i = HMAC-SHA256(password, U_{i-1})`
/// 3. `block = U_1 XOR U_2 XOR ... XOR U_iterations`
///
/// Each iteration adds ~200ns of computation, so 100,000 iterations
/// takes ~20ms on modern hardware, making offline password guessing slow.
///
/// # Parameters
/// - `password`: The user's passphrase (arbitrary bytes).
/// - `salt`: Random salt (should be at least 16 bytes, unique per key).
/// - `iterations`: Number of HMAC rounds. Higher = slower but more secure.
/// - `dk_len`: Desired output key length in bytes.
///
/// # Returns
/// The derived key material of exactly `dk_len` bytes.
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32, dk_len: usize) -> Vec<u8> {
    let mut derived_key = Vec::with_capacity(dk_len);
    let blocks_needed = (dk_len + 31) / 32; // SHA-256 produces 32 bytes per block

    for block_idx in 1..=(blocks_needed as u32) {
        // U_1 = HMAC(password, salt || INT_32_BE(block_idx))
        let mut salt_block = Vec::with_capacity(salt.len() + 4);
        salt_block.extend_from_slice(salt);
        salt_block.extend_from_slice(&block_idx.to_be_bytes());

        let mut u = hmac_sha256(password, &salt_block);
        let mut result = u;

        // U_2 .. U_iterations
        for _ in 1..iterations {
            u = hmac_sha256(password, &u);
            for (r, b) in result.iter_mut().zip(u.iter()) {
                *r ^= b;
            }
        }

        derived_key.extend_from_slice(&result);
    }

    derived_key.truncate(dk_len);
    derived_key
}

// ── XTS mode ─────────────────────────────────────────────────────────

/// GF(2^128) multiply tweak by x (left shift with polynomial reduction).
///
/// This implements multiplication by the generator polynomial alpha in
/// GF(2^128) with the irreducible polynomial x^128 + x^7 + x^2 + x + 1.
/// The 0x87 constant is the low byte of that polynomial (bits 0-7: 10000111).
/// Used in XTS mode to derive the tweak for each successive 16-byte block
/// within a sector.
fn gf128_mul_x(tweak: &mut [u8; 16]) {
    let mut carry = 0u8;
    for byte in tweak.iter_mut() {
        let new_carry = *byte >> 7;
        *byte = (*byte << 1) | carry;
        carry = new_carry;
    }
    // If there was a carry out of the MSB, reduce modulo the GF(2^128) polynomial.
    // The constant 0x87 = 0b10000111 represents x^7 + x^2 + x + 1.
    if carry != 0 {
        tweak[0] ^= 0x87;
    }
}

/// Encrypt a sector using AES-256-XTS (IEEE P1619).
///
/// XTS (XEX Tweakable Block cipher with ciphertext Stealing) mode is
/// specifically designed for disk encryption. Unlike CBC or CTR mode,
/// XTS uses a **tweak** value (the sector number) to ensure that identical
/// plaintext blocks at different disk locations produce different ciphertext,
/// without requiring a per-sector IV or nonce that must be stored separately.
///
/// # How XTS Works
/// For each 16-byte block within the sector:
/// 1. Compute the tweak: `T = AES_key2(sector_num) * alpha^block_index`
///    where multiplication is in GF(2^128).
/// 2. XOR the plaintext block with the tweak: `PP = P XOR T`
/// 3. Encrypt: `CC = AES_key1(PP)`
/// 4. XOR again: `C = CC XOR T`
///
/// The tweak advances via `gf128_mul_x` for each block within the sector.
///
/// # Parameters
/// - `key1`: 32-byte AES-256 key for data encryption.
/// - `key2`: 32-byte AES-256 key for tweak encryption.
/// - `sector_num`: Sector index, used as the tweak value.
/// - `data`: Sector data to encrypt in-place. Must be a multiple of 16 bytes.
pub fn xts_encrypt_sector(key1: &[u8; 32], key2: &[u8; 32], sector_num: u64, data: &mut [u8]) {
    assert!(data.len() % 16 == 0, "XTS data must be a multiple of 16 bytes");

    // Encrypt the sector number with key2 to get the initial tweak value.
    // The sector number is placed in the low 8 bytes (little-endian) with
    // the high 8 bytes zeroed. The AES encryption of this makes the tweak
    // unpredictable to an attacker who doesn't know key2.
    let mut tweak = [0u8; 16];
    tweak[..8].copy_from_slice(&sector_num.to_le_bytes());
    aes256_encrypt_block(key2, &mut tweak);

    // Process each 16-byte block within the sector
    for chunk in data.chunks_mut(16) {
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);

        // XOR with tweak
        for (b, t) in block.iter_mut().zip(tweak.iter()) {
            *b ^= t;
        }

        // Encrypt
        aes256_encrypt_block(key1, &mut block);

        // XOR with tweak again
        for (b, t) in block.iter_mut().zip(tweak.iter()) {
            *b ^= t;
        }

        chunk.copy_from_slice(&block);

        // Advance tweak for next block
        gf128_mul_x(&mut tweak);
    }
}

/// Decrypt a sector using AES-256-XTS (IEEE P1619).
///
/// The inverse of `xts_encrypt_sector`. Uses AES-256 *decryption* for the
/// data blocks but AES-256 *encryption* for the tweak (the tweak is always
/// computed via encryption, never decryption).
///
/// # Parameters
/// - `key1`: 32-byte AES-256 key for data decryption (same key as encryption).
/// - `key2`: 32-byte AES-256 key for tweak computation (same key as encryption).
/// - `sector_num`: Sector index (must match the value used for encryption).
/// - `data`: Sector ciphertext to decrypt in-place. Must be a multiple of 16 bytes.
pub fn xts_decrypt_sector(key1: &[u8; 32], key2: &[u8; 32], sector_num: u64, data: &mut [u8]) {
    assert!(data.len() % 16 == 0, "XTS data must be a multiple of 16 bytes");

    let mut tweak = [0u8; 16];
    tweak[..8].copy_from_slice(&sector_num.to_le_bytes());
    aes256_encrypt_block(key2, &mut tweak);

    for chunk in data.chunks_mut(16) {
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);

        for (b, t) in block.iter_mut().zip(tweak.iter()) {
            *b ^= t;
        }

        aes256_decrypt_block(key1, &mut block);

        for (b, t) in block.iter_mut().zip(tweak.iter()) {
            *b ^= t;
        }

        chunk.copy_from_slice(&block);
        gf128_mul_x(&mut tweak);
    }
}

// ── Encryption header ────────────────────────────────────────────────

/// On-disk header for an encrypted partition (stored in sector 0).
///
/// This header is inspired by LUKS (Linux Unified Key Setup) but simplified
/// for ClaudioOS. It occupies exactly one 512-byte sector and contains all
/// the metadata needed to derive and verify the encryption key.
///
/// # On-Disk Layout (offsets in bytes)
/// ```text
/// [0..8]    magic: "CLAUENC\0" (identifies this as an encrypted partition)
/// [8..12]   version: u32 LE (currently 1)
/// [12..44]  salt: 32 bytes (random, generated at format time)
/// [44..48]  iterations: u32 LE (PBKDF2 iteration count)
/// [48..80]  key_check: SHA-256(derived_key) (for passphrase verification)
/// [80..512] reserved (zeros)
/// ```
#[derive(Clone)]
pub struct EncryptionHeader {
    /// Magic bytes identifying a ClaudioOS encrypted partition.
    /// Must equal `CLAUENC\0` (8 bytes). Acts as a partition type marker.
    pub magic: [u8; 8],
    /// Header format version. Currently 1. Allows future format changes.
    pub version: u32,
    /// PBKDF2 salt (32 bytes). Generated randomly at format time.
    /// Ensures that the same passphrase produces different keys on different devices.
    pub salt: [u8; 32],
    /// PBKDF2 iteration count. Controls the cost of key derivation.
    /// Stored in the header so the unlock process knows how many rounds to run.
    pub iterations: u32,
    /// SHA-256 hash of the full 64-byte derived key. Used to verify that the
    /// user entered the correct passphrase BEFORE attempting decryption.
    /// This is NOT the key itself -- it is a hash of the key.
    pub key_check: [u8; 32],
}

impl EncryptionHeader {
    /// Serialize the header to bytes (fits in one sector).
    pub fn to_bytes(&self) -> [u8; SECTOR_SIZE] {
        let mut buf = [0u8; SECTOR_SIZE];
        buf[0..8].copy_from_slice(&self.magic);
        buf[8..12].copy_from_slice(&self.version.to_le_bytes());
        buf[12..44].copy_from_slice(&self.salt);
        buf[44..48].copy_from_slice(&self.iterations.to_le_bytes());
        buf[48..80].copy_from_slice(&self.key_check);
        buf
    }

    /// Deserialize a header from a sector buffer.
    pub fn from_bytes(buf: &[u8; SECTOR_SIZE]) -> Result<Self, &'static str> {
        if buf[0..8] != ENCRYPTION_MAGIC {
            return Err("not an encrypted partition (bad magic)");
        }
        let version = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        if version != HEADER_VERSION {
            return Err("unsupported encryption header version");
        }
        let mut salt = [0u8; 32];
        salt.copy_from_slice(&buf[12..44]);
        let iterations = u32::from_le_bytes([buf[44], buf[45], buf[46], buf[47]]);
        let mut key_check = [0u8; 32];
        key_check.copy_from_slice(&buf[48..80]);

        Ok(EncryptionHeader {
            magic: ENCRYPTION_MAGIC,
            version,
            salt,
            iterations,
            key_check,
        })
    }
}

// ── Encrypted block device wrapper ───────────────────────────────────

/// Transparent encryption wrapper around a block device.
///
/// Sector 0 holds the encryption header. Data sectors start at sector 1.
/// The master key (two 32-byte AES-256 keys for XTS) is derived from the
/// passphrase via PBKDF2-SHA256 and held only in memory.
pub struct EncryptedBlockDevice<D: BlockDevice> {
    inner: D,
    /// XTS key 1 (data encryption).
    key1: [u8; 32],
    /// XTS key 2 (tweak encryption).
    key2: [u8; 32],
    /// Whether the device has been unlocked.
    unlocked: bool,
}

impl<D: BlockDevice> EncryptedBlockDevice<D> {
    /// Create a new encrypted block device (locked state).
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            key1: [0u8; 32],
            key2: [0u8; 32],
            unlocked: false,
        }
    }

    /// Unlock the device with a passphrase.
    ///
    /// Reads the header from sector 0, derives the key via PBKDF2, and
    /// verifies it against the stored key check hash.
    pub fn unlock(&mut self, passphrase: &[u8]) -> Result<(), BlockError> {
        let mut header_buf = [0u8; SECTOR_SIZE];
        self.inner.read_sector(0, &mut header_buf)?;

        let header = EncryptionHeader::from_bytes(&header_buf)
            .map_err(|_| BlockError::AuthenticationFailed)?;

        // Derive the 64-byte XTS key from passphrase
        let derived = pbkdf2_sha256(passphrase, &header.salt, header.iterations, 64);

        // Verify key correctness
        let check = sha256(&derived);
        if check != header.key_check {
            return Err(BlockError::AuthenticationFailed);
        }

        self.key1.copy_from_slice(&derived[..32]);
        self.key2.copy_from_slice(&derived[32..64]);
        self.unlocked = true;

        log::info!("[encryption] device unlocked successfully");
        Ok(())
    }

    /// Format the device with encryption. Writes a new header with a random salt.
    ///
    /// `salt` must be provided by the caller (from a hardware RNG or RDRAND).
    pub fn format(&mut self, passphrase: &[u8], salt: [u8; 32]) -> Result<(), BlockError> {
        let derived = pbkdf2_sha256(passphrase, &salt, PBKDF2_ITERATIONS, 64);
        let key_check = sha256(&derived);

        let header = EncryptionHeader {
            magic: ENCRYPTION_MAGIC,
            version: HEADER_VERSION,
            salt,
            iterations: PBKDF2_ITERATIONS,
            key_check,
        };

        let header_bytes = header.to_bytes();
        self.inner.write_sector(0, &header_bytes)?;

        self.key1.copy_from_slice(&derived[..32]);
        self.key2.copy_from_slice(&derived[32..64]);
        self.unlocked = true;

        log::info!("[encryption] device formatted and unlocked");
        Ok(())
    }

    /// Lock the device, zeroing the keys in memory.
    ///
    /// Overwrites both 32-byte keys with zeros to ensure they cannot be
    /// recovered from a memory dump after the device is locked. This is
    /// a critical security measure for devices that may be powered off
    /// or whose memory may be inspected (cold boot attacks, DMA attacks).
    pub fn lock(&mut self) {
        self.key1 = [0u8; 32];
        self.key2 = [0u8; 32];
        self.unlocked = false;
        log::info!("[encryption] device locked, keys zeroed");
    }

    /// Check if the device is unlocked.
    pub fn is_unlocked(&self) -> bool {
        self.unlocked
    }

    /// Read a plaintext sector (transparently decrypts).
    /// Sector numbers are offset by 1 (sector 0 = header).
    pub fn read_sector(&self, sector: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        if !self.unlocked {
            return Err(BlockError::Locked);
        }
        // Actual on-disk sector is offset by 1 for the header
        let disk_sector = sector + 1;
        self.inner.read_sector(disk_sector, buf)?;
        xts_decrypt_sector(&self.key1, &self.key2, sector, buf);
        Ok(())
    }

    /// Write a plaintext sector (transparently encrypts).
    pub fn write_sector(&mut self, sector: u64, data: &[u8]) -> Result<(), BlockError> {
        if !self.unlocked {
            return Err(BlockError::Locked);
        }
        let disk_sector = sector + 1;
        let mut encrypted = vec![0u8; data.len()];
        encrypted.copy_from_slice(data);
        xts_encrypt_sector(&self.key1, &self.key2, sector, &mut encrypted);
        self.inner.write_sector(disk_sector, &encrypted)
    }

    /// Usable data sectors (total - 1 for header).
    pub fn data_sector_count(&self) -> u64 {
        self.inner.sector_count().saturating_sub(1)
    }
}

// ── Shell command: cryptsetup ────────────────────────────────────────

/// Handle the `cryptsetup` shell command.
///
/// Usage:
/// - `cryptsetup open <device> <name>` — unlock an encrypted device (prompts for passphrase)
/// - `cryptsetup close <name>` — lock a device and zero keys
/// - `cryptsetup format <device>` — format a device with encryption
/// - `cryptsetup status` — show encryption status
pub fn shell_cryptsetup(args: &str) -> String {
    let parts: Vec<&str> = args.trim().split_whitespace().collect();
    if parts.is_empty() {
        return "usage: cryptsetup <open|close|format|status> [args...]\n".to_string();
    }

    match parts[0] {
        "open" => {
            if parts.len() < 3 {
                return "usage: cryptsetup open <device> <name>\n".to_string();
            }
            let device = parts[1].to_string();
            let name = parts[2].to_string();

            // Prompt for passphrase over the serial console.
            //
            // NOTE: This is a plain-text prompt (the typed passphrase IS
            // echoed by the terminal emulator). Production would need no-echo
            // mode (clear ECHO bit on the UART / use a raw keyboard mode)
            // plus secure zero-on-drop for the passphrase buffer.
            let passphrase = match prompt_passphrase(&device) {
                Some(p) => p,
                None => return format!("cryptsetup: failed to read passphrase for {}\n", device),
            };

            let len = passphrase.len();
            // Log only the length — never the raw passphrase.
            log::info!(
                "[cryptsetup] captured {} byte passphrase for device '{}' -> name '{}'",
                len, device, name,
            );

            // Register the device as armed. We don't actually derive the key
            // here because there's no BlockDevice handle to unlock — the LUKS2
            // header parser and disk driver wiring are still pending.
            ARMED_DEVICES.lock().insert(device.clone(), name.clone());

            format!(
                "cryptsetup: {} armed as '{}' ({} byte passphrase). Decryption pending LUKS2 header parser.\n",
                device, name, len,
            )
        }
        "close" => {
            if parts.len() < 2 {
                return "usage: cryptsetup close <name>\n".to_string();
            }
            let name = parts[1];
            // Find the device mapped to this name.
            let mut armed = ARMED_DEVICES.lock();
            let found_device = armed
                .iter()
                .find(|(_, n)| n.as_str() == name)
                .map(|(d, _)| d.clone());
            if let Some(d) = found_device {
                armed.remove(&d);
                format!("cryptsetup: closed '{}' (device {}), armed entry cleared\n", name, d)
            } else {
                format!("cryptsetup: no armed device named '{}'\n", name)
            }
        }
        "format" => {
            if parts.len() < 2 {
                return "usage: cryptsetup format <device>\n".to_string();
            }
            format!(
                "cryptsetup: would format {} with AES-256-XTS encryption (LUKS2 writer pending)\n",
                parts[1]
            )
        }
        "status" => {
            let armed = ARMED_DEVICES.lock();
            if armed.is_empty() {
                "cryptsetup: no encrypted devices currently armed\n".to_string()
            } else {
                let mut out = String::new();
                out.push_str(&format!("cryptsetup: {} armed device(s):\n", armed.len()));
                for (device, name) in armed.iter() {
                    out.push_str(&format!("  {} -> '{}'\n", device, name));
                }
                out
            }
        }
        _ => format!("cryptsetup: unknown subcommand '{}'\n", parts[0]),
    }
}

/// Read a passphrase from the serial console (0x3F8).
///
/// Returns `None` on EOF / error. Echoes characters as-is (see production
/// note on `ARMED_DEVICES`). A line is terminated by CR or LF.
fn prompt_passphrase(device: &str) -> Option<String> {
    use x86_64::instructions::port::Port;

    // Write prompt to serial.
    let prompt = format!("cryptsetup: enter passphrase for {}: ", device);
    for b in prompt.as_bytes() {
        unsafe { Port::<u8>::new(0x3F8).write(*b); }
    }

    let mut buf = String::new();
    loop {
        // Poll the LSR for data-ready (bit 0).
        let c: u8 = unsafe {
            let mut lsr = Port::<u8>::new(0x3F8 + 5);
            loop {
                if lsr.read() & 1 != 0 { break; }
                core::hint::spin_loop();
            }
            Port::<u8>::new(0x3F8).read()
        };
        match c {
            b'\r' | b'\n' => {
                // Echo newline.
                unsafe {
                    let mut p = Port::<u8>::new(0x3F8);
                    p.write(b'\r');
                    p.write(b'\n');
                }
                return Some(buf);
            }
            0x08 | 0x7f => {
                // Backspace — remove last char if any.
                if buf.pop().is_some() {
                    unsafe {
                        let mut p = Port::<u8>::new(0x3F8);
                        p.write(0x08); p.write(b' '); p.write(0x08);
                    }
                }
            }
            c if c >= 0x20 && c < 0x7f => {
                buf.push(c as char);
                // Echo the character. Production: disable echo or print '*'.
                unsafe { Port::<u8>::new(0x3F8).write(c); }
                // Cap line length to avoid runaway input.
                if buf.len() >= 1024 {
                    return Some(buf);
                }
            }
            _ => {
                // Ignore other control bytes.
            }
        }
    }
}

/// Generate a pseudo-random salt using RDRAND if available, or fallback
/// to RTC + PIT tick mixing.
pub fn generate_salt() -> [u8; 32] {
    let mut salt = [0u8; 32];

    // Try RDRAND first (available on Haswell+)
    let mut rdrand_ok = true;
    for chunk in salt.chunks_mut(8) {
        let mut val: u64 = 0;
        let success: u8;
        // SAFETY: RDRAND is a non-privileged instruction. On CPUs without
        // RDRAND support, this will #UD — but our target hardware (Haswell+)
        // always has it. The fallback path below handles the case where
        // RDRAND fails (returns CF=0) due to DRNG underflow.
        unsafe {
            core::arch::asm!(
                "rdrand {val}",
                "setc {ok}",
                val = out(reg) val,
                ok = out(reg_byte) success,
            );
        }
        if success == 0 {
            rdrand_ok = false;
            break;
        }
        let bytes = val.to_le_bytes();
        let len = chunk.len().min(8);
        chunk[..len].copy_from_slice(&bytes[..len]);
    }

    if !rdrand_ok {
        // Fallback: mix RTC time + PIT ticks
        let dt = crate::rtc::wall_clock();
        let ticks = crate::interrupts::tick_count();
        let seed = dt.to_unix_timestamp() as u64 ^ ticks;
        // Simple LCG (Linear Congruential Generator) mixing.
        // Constants are Knuth's recommended LCG parameters for 64-bit state.
        // NOTE: This is NOT cryptographically secure -- it is a fallback
        // only used when RDRAND is unavailable.
        let mut state = seed;
        for chunk in salt.chunks_mut(8) {
            // Multiplier and increment from Knuth's MMIX LCG
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let bytes = state.to_le_bytes();
            let len = chunk.len().min(8);
            chunk[..len].copy_from_slice(&bytes[..len]);
        }
        log::warn!("[encryption] RDRAND unavailable, using RTC+PIT fallback for salt");
    }

    salt
}

/// Initialize the encryption subsystem.
pub fn init() {
    log::info!("[encryption] AES-256-XTS disk encryption available");
}
