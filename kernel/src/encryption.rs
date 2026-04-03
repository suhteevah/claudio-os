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

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use alloc::format;
use core::fmt;

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

/// AES-256 block encryption (single 16-byte block).
/// This is a minimal software AES-256 for use in XTS mode.
fn aes256_encrypt_block(key: &[u8; 32], block: &mut [u8; 16]) {
    // Rijndael S-box
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

    // Rcon for key expansion
    const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

    // Key expansion: AES-256 uses 14 rounds = 15 round keys = 60 u32 words
    let mut rk = [0u32; 60];
    for i in 0..8 {
        rk[i] = u32::from_be_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
    }
    for i in 8..60 {
        let mut temp = rk[i - 1];
        if i % 8 == 0 {
            // RotWord + SubWord + Rcon
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

    // State as 4x4 column-major
    let mut state = [0u8; 16];
    state.copy_from_slice(block);

    // AddRoundKey (round 0)
    for c in 0..4 {
        let k = rk[c].to_be_bytes();
        for r in 0..4 {
            state[r + 4 * c] ^= k[r];
        }
    }

    // Rounds 1..13 (full rounds)
    for round in 1..14 {
        // SubBytes
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

        // MixColumns
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

/// GF(2^8) multiply by 2.
fn gf_mul2(x: u8) -> u8 {
    let mut r = (x as u16) << 1;
    if r & 0x100 != 0 {
        r ^= 0x11b;
    }
    r as u8
}

/// GF(2^8) multiply by 3 = mul2(x) ^ x.
fn gf_mul3(x: u8) -> u8 {
    gf_mul2(x) ^ x
}

// ── AES-256 decryption ───────────────────────────────────────────────

/// AES-256 block decryption (single 16-byte block).
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

/// GF(2^8) multiplication for InvMixColumns.
fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut result: u8 = 0;
    for _ in 0..8 {
        if b & 1 != 0 {
            result ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    result
}

// ── PBKDF2-SHA256 ────────────────────────────────────────────────────

/// SHA-256 hash (minimal implementation for PBKDF2).
fn sha256(data: &[u8]) -> [u8; 32] {
    // SHA-256 constants
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

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
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

/// HMAC-SHA256.
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

/// PBKDF2-SHA256 key derivation.
///
/// Derives `dk_len` bytes from `password` and `salt` using `iterations` rounds.
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
fn gf128_mul_x(tweak: &mut [u8; 16]) {
    let mut carry = 0u8;
    for byte in tweak.iter_mut() {
        let new_carry = *byte >> 7;
        *byte = (*byte << 1) | carry;
        carry = new_carry;
    }
    // If there was a carry out of the MSB, XOR with the GF(2^128) polynomial
    if carry != 0 {
        tweak[0] ^= 0x87;
    }
}

/// Encrypt a sector using AES-256-XTS.
///
/// `key1` is used for the data encryption, `key2` for the tweak encryption.
/// `sector_num` is used as the tweak value.
pub fn xts_encrypt_sector(key1: &[u8; 32], key2: &[u8; 32], sector_num: u64, data: &mut [u8]) {
    assert!(data.len() % 16 == 0, "XTS data must be a multiple of 16 bytes");

    // Encrypt the sector number to get the initial tweak
    let mut tweak = [0u8; 16];
    tweak[..8].copy_from_slice(&sector_num.to_le_bytes());
    aes256_encrypt_block(key2, &mut tweak);

    // Process each 16-byte block
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

/// Decrypt a sector using AES-256-XTS.
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
#[derive(Clone)]
pub struct EncryptionHeader {
    /// Magic bytes: CLAUENC\0
    pub magic: [u8; 8],
    /// Header version.
    pub version: u32,
    /// PBKDF2 salt (32 bytes).
    pub salt: [u8; 32],
    /// PBKDF2 iteration count.
    pub iterations: u32,
    /// SHA-256 hash of the derived key — used to verify correct passphrase.
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

    /// Lock the device — zeroes out keys in memory.
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
            format!(
                "cryptsetup: would unlock {} as '{}' (passphrase prompt not yet wired)\n",
                parts[1], parts[2]
            )
        }
        "close" => {
            if parts.len() < 2 {
                return "usage: cryptsetup close <name>\n".to_string();
            }
            format!("cryptsetup: would lock '{}' and zero keys\n", parts[1])
        }
        "format" => {
            if parts.len() < 2 {
                return "usage: cryptsetup format <device>\n".to_string();
            }
            format!(
                "cryptsetup: would format {} with AES-256-XTS encryption\n",
                parts[1]
            )
        }
        "status" => "cryptsetup: no encrypted devices currently open\n".to_string(),
        _ => format!("cryptsetup: unknown subcommand '{}'\n", parts[0]),
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
        // Simple PRNG mixing
        let mut state = seed;
        for chunk in salt.chunks_mut(8) {
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
