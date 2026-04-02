//! NTFS B+ tree index structures for directory lookups.
//!
//! NTFS directories use B+ tree indexes to store sorted filename entries.
//! The index consists of:
//! - `$INDEX_ROOT` attribute (type 0x90): always resident, contains the root node
//! - `$INDEX_ALLOCATION` attribute (type 0xA0): non-resident, contains overflow nodes
//! - `$BITMAP` attribute (type 0xB0): tracks which index allocation blocks are in use
//!
//! Each index node (both root and allocation) contains a header followed by
//! index entries. Each entry contains a key (the $FILE_NAME attribute value),
//! the MFT reference for the file, and optionally a pointer to a sub-node.
//!
//! The last entry in each node has the LAST_ENTRY flag set and contains no key.
//!
//! Reference: <https://flatcap.github.io/linux-ntfs/ntfs/concepts/index_record.html>

use alloc::vec::Vec;
use core::fmt;

use crate::filename::FileNameAttr;
use crate::mft;

/// Magic signature for INDX records: "INDX" in ASCII.
pub const INDX_MAGIC: &[u8; 4] = b"INDX";
/// INDX magic as u32 (little-endian).
pub const INDX_MAGIC_U32: u32 = 0x58444E49;

// --- Index entry flags ---

/// This entry has a sub-node (VCN pointer at the end of the entry).
pub const INDEX_ENTRY_FLAG_HAS_SUB_NODE: u16 = 0x0001;
/// This is the last entry in the node (sentinel, no key data).
pub const INDEX_ENTRY_FLAG_LAST_ENTRY: u16 = 0x0002;

// --- Index header flags ---

/// The index is large (uses $INDEX_ALLOCATION for overflow nodes).
pub const INDEX_HEADER_FLAG_LARGE_INDEX: u8 = 0x01;

/// Parsed $INDEX_ROOT attribute.
///
/// The $INDEX_ROOT is always resident and contains the root of the B+ tree.
/// It begins with metadata about the index type, followed by the index header
/// and the root node's entries.
#[derive(Clone, Debug)]
pub struct IndexRoot {
    /// Attribute type being indexed (0x30 = $FILE_NAME for directories).
    pub indexed_attribute_type: u32,
    /// Collation rule (0x01 = FILENAME for directories).
    pub collation_rule: u32,
    /// Size of each index allocation block in bytes (from boot sector).
    pub index_block_size: u32,
    /// Clusters per index block.
    pub clusters_per_index_block: u8,
    /// The index header (offsets are relative to the start of the index header).
    pub index_header: IndexHeader,
}

impl IndexRoot {
    /// Parse an $INDEX_ROOT from its attribute value bytes.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 16 {
            log::error!("[ntfs::index] INDEX_ROOT buffer too small: {} bytes (need >= 16)",
                buf.len());
            return None;
        }

        let indexed_attribute_type = read_u32(buf, 0x00);
        let collation_rule = read_u32(buf, 0x04);
        let index_block_size = read_u32(buf, 0x08);
        let clusters_per_index_block = buf[0x0C];

        log::debug!("[ntfs::index] INDEX_ROOT: attr_type=0x{:08X}, collation=0x{:08X}, \
            block_size={}, clusters_per_block={}",
            indexed_attribute_type, collation_rule,
            index_block_size, clusters_per_index_block);

        // Index header starts at offset 0x10
        let index_header = IndexHeader::from_bytes(&buf[0x10..])?;

        Some(IndexRoot {
            indexed_attribute_type,
            collation_rule,
            index_block_size,
            clusters_per_index_block,
            index_header,
        })
    }

    /// Whether this index has overflow nodes in $INDEX_ALLOCATION.
    #[inline]
    pub fn has_large_index(&self) -> bool {
        self.index_header.flags & INDEX_HEADER_FLAG_LARGE_INDEX != 0
    }

    /// Get the raw entries area from the INDEX_ROOT value bytes.
    /// `buf` should be the full INDEX_ROOT attribute value.
    pub fn entries_data<'a>(&self, buf: &'a [u8]) -> Option<&'a [u8]> {
        let header_offset = 0x10usize;
        let entries_start = header_offset + self.index_header.entries_offset as usize;
        let entries_end = header_offset + self.index_header.total_size as usize;
        if entries_end > buf.len() {
            log::error!("[ntfs::index] entries extend beyond buffer: {}..{} > {}",
                entries_start, entries_end, buf.len());
            return None;
        }
        Some(&buf[entries_start..entries_end])
    }
}

/// Index header (shared between INDEX_ROOT and INDX allocation blocks).
#[derive(Clone, Debug)]
pub struct IndexHeader {
    /// Byte offset from the start of this header to the first index entry.
    pub entries_offset: u32,
    /// Total size of the index entries area in bytes (including the header).
    pub total_size: u32,
    /// Allocated size of the index entries area in bytes.
    pub allocated_size: u32,
    /// Flags (INDEX_HEADER_FLAG_LARGE_INDEX).
    pub flags: u8,
}

impl IndexHeader {
    /// Size of the index header (16 bytes).
    pub const SIZE: usize = 16;

    /// Parse an index header.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            log::error!("[ntfs::index] index header too small: {} bytes", buf.len());
            return None;
        }

        let hdr = IndexHeader {
            entries_offset: read_u32(buf, 0x00),
            total_size:     read_u32(buf, 0x04),
            allocated_size: read_u32(buf, 0x08),
            flags:          buf[0x0C],
        };

        log::trace!("[ntfs::index] index_header: entries_off=0x{:04X}, total={}, alloc={}, flags=0x{:02X}",
            hdr.entries_offset, hdr.total_size, hdr.allocated_size, hdr.flags);

        Some(hdr)
    }
}

/// Parsed INDX allocation block header.
///
/// Each non-root node in the B+ tree is stored in an index allocation block
/// (also called an index record or INDX buffer). The block starts with a
/// standard NTFS record header with fixup support.
#[derive(Clone, Debug)]
pub struct IndexNodeHeader {
    /// Magic "INDX".
    pub magic: u32,
    /// Offset to the update sequence array.
    pub usa_offset: u16,
    /// Size of the update sequence array in u16 words.
    pub usa_count: u16,
    /// $LogFile sequence number.
    pub logfile_seq_number: u64,
    /// VCN of this index block in the $INDEX_ALLOCATION data.
    pub vcn: u64,
    /// The index header for this node.
    pub index_header: IndexHeader,
}

impl IndexNodeHeader {
    /// Minimum size of the INDX header (fixed portion before index entries).
    pub const MIN_SIZE: usize = 40;

    /// Parse an INDX block header from raw bytes (after fixup has been applied).
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::MIN_SIZE {
            log::error!("[ntfs::index] INDX buffer too small: {} bytes (need >= {})",
                buf.len(), Self::MIN_SIZE);
            return None;
        }

        let magic = read_u32(buf, 0x00);
        if magic != INDX_MAGIC_U32 {
            log::error!("[ntfs::index] invalid INDX magic: 0x{:08X} (expected 0x{:08X})",
                magic, INDX_MAGIC_U32);
            return None;
        }

        let index_header = IndexHeader::from_bytes(&buf[0x18..])?;

        let hdr = IndexNodeHeader {
            magic,
            usa_offset:         read_u16(buf, 0x04),
            usa_count:          read_u16(buf, 0x06),
            logfile_seq_number: read_u64(buf, 0x08),
            vcn:                read_u64(buf, 0x10),
            index_header,
        };

        log::debug!("[ntfs::index] INDX node: vcn={}, lsn={}, usa_off=0x{:04X}, usa_count={}",
            hdr.vcn, hdr.logfile_seq_number, hdr.usa_offset, hdr.usa_count);

        Some(hdr)
    }

    /// Apply fixup array to a raw INDX buffer (same algorithm as MFT entries).
    pub fn apply_fixup(buf: &mut [u8]) -> bool {
        if buf.len() < Self::MIN_SIZE {
            return false;
        }

        let usa_offset = read_u16(buf, 0x04) as usize;
        let usa_count = read_u16(buf, 0x06) as usize;

        if usa_count < 1 || usa_offset + usa_count * 2 > buf.len() {
            log::error!("[ntfs::index] invalid INDX fixup: usa_offset={}, usa_count={}, len={}",
                usa_offset, usa_count, buf.len());
            return false;
        }

        let check_value = read_u16(buf, usa_offset);
        let sector_size = 512usize;

        for i in 1..usa_count {
            let sector_end = i * sector_size;
            if sector_end > buf.len() {
                break;
            }
            let fixup_pos = sector_end - 2;
            let stored = read_u16(buf, fixup_pos);

            if stored != check_value {
                log::error!("[ntfs::index] INDX fixup mismatch at sector {}: 0x{:04X} != 0x{:04X}",
                    i, stored, check_value);
                return false;
            }

            let original = read_u16(buf, usa_offset + i * 2);
            write_u16(buf, fixup_pos, original);
            log::trace!("[ntfs::index] INDX fixup sector {}: 0x{:04X} -> 0x{:04X}", i, check_value, original);
        }

        true
    }

    /// Get the entries data area from a fully parsed INDX block.
    /// `buf` should be the full INDX block buffer (after fixup).
    pub fn entries_data<'a>(&self, buf: &'a [u8]) -> Option<&'a [u8]> {
        let header_base = 0x18usize; // index header starts at offset 0x18
        let entries_start = header_base + self.index_header.entries_offset as usize;
        let entries_end = header_base + self.index_header.total_size as usize;
        if entries_end > buf.len() {
            log::error!("[ntfs::index] INDX entries extend beyond buffer");
            return None;
        }
        Some(&buf[entries_start..entries_end])
    }
}

/// A single index entry within a B+ tree node.
#[derive(Clone)]
pub struct IndexEntry {
    /// MFT reference to the file this entry points to.
    pub mft_reference: u64,
    /// Total size of this index entry in bytes.
    pub entry_length: u16,
    /// Size of the content (key) in bytes.
    pub content_length: u16,
    /// Entry flags (HAS_SUB_NODE, LAST_ENTRY).
    pub flags: u16,
    /// The parsed $FILE_NAME key (None for the last/sentinel entry).
    pub filename: Option<FileNameAttr>,
    /// VCN of the sub-node (only if HAS_SUB_NODE flag is set).
    pub sub_node_vcn: Option<u64>,
}

impl IndexEntry {
    /// Minimum size of an index entry header (16 bytes).
    pub const MIN_SIZE: usize = 16;

    /// Parse a single index entry from bytes.
    ///
    /// Returns `(entry, bytes_consumed)` or `None` if parsing fails.
    pub fn from_bytes(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.len() < Self::MIN_SIZE {
            log::error!("[ntfs::index] index entry too small: {} bytes", buf.len());
            return None;
        }

        let mft_reference = read_u64(buf, 0x00);
        let entry_length = read_u16(buf, 0x08);
        let content_length = read_u16(buf, 0x0A);
        let flags = read_u16(buf, 0x0C);

        if entry_length < Self::MIN_SIZE as u16 {
            log::error!("[ntfs::index] invalid entry_length: {}", entry_length);
            return None;
        }
        if entry_length as usize > buf.len() {
            log::error!("[ntfs::index] entry extends beyond buffer: {} > {}", entry_length, buf.len());
            return None;
        }

        let is_last = flags & INDEX_ENTRY_FLAG_LAST_ENTRY != 0;
        let has_sub_node = flags & INDEX_ENTRY_FLAG_HAS_SUB_NODE != 0;

        // Parse the filename key (if this is not the last entry and has content)
        let filename = if !is_last && content_length > 0 {
            let key_start = 0x10; // key data starts at offset 16
            if key_start + content_length as usize <= buf.len() {
                let fn_attr = FileNameAttr::from_bytes(&buf[key_start..key_start + content_length as usize]);
                if fn_attr.is_none() {
                    log::warn!("[ntfs::index] failed to parse FILE_NAME in index entry");
                }
                fn_attr
            } else {
                log::warn!("[ntfs::index] FILE_NAME content extends beyond entry");
                None
            }
        } else {
            None
        };

        // Read sub-node VCN (last 8 bytes of the entry if HAS_SUB_NODE)
        let sub_node_vcn = if has_sub_node {
            let vcn_offset = entry_length as usize - 8;
            if vcn_offset >= Self::MIN_SIZE {
                let vcn = read_u64(buf, vcn_offset);
                log::trace!("[ntfs::index] entry has sub-node at VCN {}", vcn);
                Some(vcn)
            } else {
                log::warn!("[ntfs::index] HAS_SUB_NODE flag set but entry too small for VCN");
                None
            }
        } else {
            None
        };

        let entry = IndexEntry {
            mft_reference,
            entry_length,
            content_length,
            flags,
            filename,
            sub_node_vcn,
        };

        if let Some(ref fn_attr) = entry.filename {
            log::trace!("[ntfs::index] index entry: '{}' -> MFT #{}, flags=0x{:04X}",
                fn_attr.name, mft::mft_reference_number(mft_reference), flags);
        } else if is_last {
            log::trace!("[ntfs::index] last (sentinel) index entry, flags=0x{:04X}", flags);
        }

        Some((entry, entry_length as usize))
    }

    /// Whether this is the last (sentinel) entry in the node.
    #[inline]
    pub fn is_last(&self) -> bool {
        self.flags & INDEX_ENTRY_FLAG_LAST_ENTRY != 0
    }

    /// Whether this entry has a sub-node pointer.
    #[inline]
    pub fn has_sub_node(&self) -> bool {
        self.flags & INDEX_ENTRY_FLAG_HAS_SUB_NODE != 0
    }

    /// Get the MFT entry number from the reference.
    #[inline]
    pub fn entry_number(&self) -> u64 {
        mft::mft_reference_number(self.mft_reference)
    }
}

impl fmt::Debug for IndexEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("IndexEntry");
        s.field("mft_entry", &self.entry_number());
        if let Some(ref fn_attr) = self.filename {
            s.field("name", &fn_attr.name);
        }
        s.field("flags", &format_args!("0x{:04X}", self.flags));
        s.field("is_last", &self.is_last());
        s.field("has_sub_node", &self.has_sub_node());
        if let Some(vcn) = self.sub_node_vcn {
            s.field("sub_node_vcn", &vcn);
        }
        s.finish()
    }
}

/// Parse all index entries from a contiguous entries buffer.
///
/// Walks the entry list until the LAST_ENTRY sentinel is found.
pub fn parse_index_entries(buf: &[u8]) -> Vec<IndexEntry> {
    let mut entries = Vec::new();
    let mut pos = 0;

    loop {
        if pos + IndexEntry::MIN_SIZE > buf.len() {
            break;
        }

        match IndexEntry::from_bytes(&buf[pos..]) {
            Some((entry, consumed)) => {
                let is_last = entry.is_last();
                entries.push(entry);
                if is_last {
                    break;
                }
                pos += consumed;
            }
            None => {
                log::error!("[ntfs::index] failed to parse index entry at offset 0x{:04X}", pos);
                break;
            }
        }
    }

    log::debug!("[ntfs::index] parsed {} index entries", entries.len());
    entries
}

/// Search for a filename in a list of index entries (case-insensitive).
///
/// Returns the matching entry if found. This is a linear scan suitable for
/// small directories; for large directories, use the B+ tree traversal.
pub fn find_entry_by_name<'a>(entries: &'a [IndexEntry], name: &str) -> Option<&'a IndexEntry> {
    let name_lower: Vec<u16> = name.encode_utf16()
        .map(|c| if c >= b'A' as u16 && c <= b'Z' as u16 { c + 32 } else { c })
        .collect();

    for entry in entries {
        if entry.is_last() {
            continue;
        }
        if let Some(ref fn_attr) = entry.filename {
            let entry_lower: Vec<u16> = fn_attr.name_utf16.iter()
                .map(|&c| if c >= b'A' as u16 && c <= b'Z' as u16 { c + 32 } else { c })
                .collect();
            if entry_lower == name_lower {
                log::trace!("[ntfs::index] found entry '{}' matching '{}'", fn_attr.name, name);
                return Some(entry);
            }
        }
    }

    log::trace!("[ntfs::index] no entry found matching '{}'", name);
    None
}

// --- Little-endian byte helpers ---

#[inline]
fn read_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

#[inline]
fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3]])
}

#[inline]
fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3],
        buf[offset + 4], buf[offset + 5], buf[offset + 6], buf[offset + 7],
    ])
}

#[inline]
fn write_u16(buf: &mut [u8], offset: usize, val: u16) {
    buf[offset..offset + 2].copy_from_slice(&val.to_le_bytes());
}
