//! CRC32C (Castagnoli) implementation for btrfs.
//!
//! btrfs uses CRC32C for:
//! - Superblock checksums (first 32 bytes of superblock are the checksum)
//! - Metadata block checksums (tree node headers)
//! - Directory item name hashing (crc32c of filename)
//!
//! This is a table-based software implementation using the CRC32C polynomial
//! 0x1EDC6F41 (Castagnoli), which is different from the standard CRC32 (Ethernet).
//!
//! Reference: <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html>

/// CRC32C polynomial (Castagnoli) in reversed bit order.
const CRC32C_POLY: u32 = 0x82F6_3B78;

/// Precomputed CRC32C lookup table (256 entries).
const CRC32C_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut crc = i;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ CRC32C_POLY;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
};

/// Compute CRC32C of the given data, starting from an initial CRC value.
///
/// For a fresh checksum, pass `initial = 0xFFFF_FFFF` and XOR the result
/// with `0xFFFF_FFFF` afterwards (or just use [`crc32c`]).
#[inline]
pub fn crc32c_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        let index = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = CRC32C_TABLE[index] ^ (crc >> 8);
    }
    crc
}

/// Compute the CRC32C checksum of the given data.
///
/// This is the standard form: init with all-ones, XOR at end.
pub fn crc32c(data: &[u8]) -> u32 {
    let crc = crc32c_update(0xFFFF_FFFF, data);
    crc ^ 0xFFFF_FFFF
}

/// Compute the btrfs name hash for directory items.
///
/// btrfs uses CRC32C but with an initial seed of `0xFFFF_FFFF` and does NOT
/// final-XOR. This matches the `~crc32c(~0, name)` convention used in the kernel.
///
/// Actually, btrfs uses `btrfs_name_hash` = `crc32c(0xFFFFFFFE, name)` with
/// no final XOR? No -- the kernel `btrfs_name_hash` is simply:
/// `crc32c(~1, name, len)` which is `crc32c_le(~1, name, len)`.
///
/// In practice the kernel code does: `crc = btrfs_crc32c(~1, name, len)` which
/// is `crc32c_le` with init=0xFFFFFFFE. The result is NOT inverted.
pub fn btrfs_name_hash(name: &[u8]) -> u32 {
    // btrfs_name_hash = crc32c with initial value ~1 = 0xFFFFFFFE, no final XOR
    crc32c_update(0xFFFF_FFFE, name)
}

/// Compute the btrfs CRC32C checksum for on-disk metadata.
///
/// btrfs stores the checksum as the raw CRC32C (init=0xFFFFFFFF, final XOR with 0xFFFFFFFF)
/// in the first bytes of each metadata block (superblock, tree nodes).
#[inline]
pub fn btrfs_csum(data: &[u8]) -> u32 {
    crc32c(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32c_empty() {
        assert_eq!(crc32c(b""), 0x0000_0000);
    }

    #[test]
    fn test_crc32c_known_vectors() {
        // Known CRC32C test vector: "123456789" -> 0xE3069283
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn test_name_hash_deterministic() {
        let h1 = btrfs_name_hash(b"hello");
        let h2 = btrfs_name_hash(b"hello");
        assert_eq!(h1, h2);
        // Different names should (almost certainly) differ
        let h3 = btrfs_name_hash(b"world");
        assert_ne!(h1, h3);
    }
}
