//! $UpCase table for NTFS case-insensitive filename comparison.
//!
//! NTFS stores a Unicode uppercase mapping table in the system file $UpCase
//! (MFT entry 10). This table maps every UTF-16 code point to its uppercase
//! equivalent and is used for case-insensitive filename comparisons.
//!
//! The table is exactly 65536 entries (128 KiB) — one u16 per UTF-16 code unit.
//!
//! Reference: <https://flatcap.github.io/linux-ntfs/ntfs/files/upcase.html>

use alloc::vec::Vec;
use core::cmp::Ordering;

/// Size of the $UpCase table in UTF-16 entries.
pub const UPCASE_TABLE_ENTRIES: usize = 65536;

/// Size of the $UpCase table in bytes (128 KiB).
pub const UPCASE_TABLE_SIZE: usize = UPCASE_TABLE_ENTRIES * 2;

/// The $UpCase uppercase mapping table.
///
/// Used for case-insensitive filename comparisons per the NTFS spec.
#[derive(Clone)]
pub struct UpCaseTable {
    /// 65536-entry table: table[c] = uppercase(c) for each UTF-16 code unit.
    pub table: Vec<u16>,
}

impl UpCaseTable {
    /// Load the $UpCase table from raw bytes (128 KiB, little-endian u16 array).
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < UPCASE_TABLE_SIZE {
            log::error!("[ntfs::upcase] buffer too small: {} bytes (need >= {})",
                buf.len(), UPCASE_TABLE_SIZE);
            return None;
        }

        let mut table = Vec::with_capacity(UPCASE_TABLE_ENTRIES);
        for i in 0..UPCASE_TABLE_ENTRIES {
            let off = i * 2;
            let val = u16::from_le_bytes([buf[off], buf[off + 1]]);
            table.push(val);
        }

        log::info!("[ntfs::upcase] loaded $UpCase table ({} entries, {} bytes)",
            UPCASE_TABLE_ENTRIES, UPCASE_TABLE_SIZE);

        // Sanity check: 'a' should map to 'A'
        if table[b'a' as usize] != b'A' as u16 {
            log::warn!("[ntfs::upcase] unexpected mapping: 'a' -> 0x{:04X} (expected 0x{:04X})",
                table[b'a' as usize], b'A' as u16);
        }

        Some(UpCaseTable { table })
    }

    /// Generate a default (ASCII-only) uppercase table.
    ///
    /// This is a fallback when the $UpCase file cannot be read.
    /// It correctly handles ASCII but does NOT handle accented characters
    /// or other Unicode uppercase rules.
    pub fn default_ascii() -> Self {
        log::warn!("[ntfs::upcase] generating fallback ASCII-only $UpCase table");
        let mut table = Vec::with_capacity(UPCASE_TABLE_ENTRIES);
        for i in 0..UPCASE_TABLE_ENTRIES {
            let c = i as u16;
            let upper = if c >= b'a' as u16 && c <= b'z' as u16 {
                c - 32
            } else {
                c
            };
            table.push(upper);
        }
        UpCaseTable { table }
    }

    /// Convert a UTF-16 code unit to uppercase.
    #[inline]
    pub fn to_upper(&self, c: u16) -> u16 {
        self.table[c as usize]
    }

    /// Compare two UTF-16LE filenames case-insensitively using this table.
    ///
    /// Returns `Ordering::Equal`, `Less`, or `Greater` per NTFS collation rules.
    pub fn compare_names(&self, a: &[u16], b: &[u16]) -> Ordering {
        let len = a.len().min(b.len());
        for i in 0..len {
            let ua = self.to_upper(a[i]);
            let ub = self.to_upper(b[i]);
            match ua.cmp(&ub) {
                Ordering::Equal => continue,
                other => {
                    log::trace!("[ntfs::upcase] compare: mismatch at pos {}: 0x{:04X} vs 0x{:04X}",
                        i, ua, ub);
                    return other;
                }
            }
        }
        a.len().cmp(&b.len())
    }

    /// Check if two UTF-16LE filenames are equal (case-insensitive).
    pub fn names_equal(&self, a: &[u16], b: &[u16]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        for i in 0..a.len() {
            if self.to_upper(a[i]) != self.to_upper(b[i]) {
                return false;
            }
        }
        true
    }

    /// Convert a UTF-16LE string to uppercase using this table.
    pub fn to_uppercase(&self, name: &[u16]) -> Vec<u16> {
        name.iter().map(|&c| self.to_upper(c)).collect()
    }

    /// Serialize the table back to bytes (128 KiB, little-endian).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(UPCASE_TABLE_SIZE);
        for &val in &self.table {
            buf.extend_from_slice(&val.to_le_bytes());
        }
        log::trace!("[ntfs::upcase] serialized {} bytes", buf.len());
        buf
    }
}

impl core::fmt::Debug for UpCaseTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UpCaseTable")
            .field("entries", &self.table.len())
            .field("size_bytes", &(self.table.len() * 2))
            .finish()
    }
}
