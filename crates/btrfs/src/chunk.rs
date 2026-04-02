//! btrfs chunk and device mapping (logical to physical address translation).
//!
//! btrfs uses a logical address space for all metadata and data. Chunks map ranges
//! of logical addresses to physical locations on one or more devices.
//!
//! The chunk tree is a regular B-tree keyed by (FIRST_CHUNK_TREE_OBJECTID, CHUNK_ITEM, logical_offset).
//! At mount time, the sys_chunk_array in the superblock provides the bootstrap chunk
//! mappings needed to read the chunk tree itself.
//!
//! Each chunk item describes a contiguous range of logical addresses and contains
//! one or more stripes indicating where the data lives on physical devices.
//!
//! Reference: <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html#chunk-item>

use alloc::vec::Vec;
use core::fmt;

use crate::key::{BtrfsKey, BTRFS_KEY_SIZE};
use crate::superblock::BTRFS_UUID_SIZE;

/// Size of a chunk item header on disk (48 bytes, before stripes).
pub const BTRFS_CHUNK_ITEM_HEADER_SIZE: usize = 48;

/// Size of a single stripe on disk (32 bytes).
pub const BTRFS_STRIPE_SIZE: usize = 32;

/// Chunk type flags.
pub mod chunk_type {
    /// Data chunk.
    pub const DATA: u64 = 1 << 0;
    /// System chunk (chunk tree, superblock mirrors).
    pub const SYSTEM: u64 = 1 << 1;
    /// Metadata chunk.
    pub const METADATA: u64 = 1 << 2;
    /// RAID0 (striped).
    pub const RAID0: u64 = 1 << 3;
    /// RAID1 (mirrored).
    pub const RAID1: u64 = 1 << 4;
    /// Duplicate (same device, two copies).
    pub const DUP: u64 = 1 << 5;
    /// RAID10 (striped mirrors).
    pub const RAID10: u64 = 1 << 6;
    /// RAID5.
    pub const RAID5: u64 = 1 << 7;
    /// RAID6.
    pub const RAID6: u64 = 1 << 8;
    /// RAID1C3 (3 copies).
    pub const RAID1C3: u64 = 1 << 9;
    /// RAID1C4 (4 copies).
    pub const RAID1C4: u64 = 1 << 10;
}

/// A single stripe within a chunk item.
#[derive(Clone)]
pub struct Stripe {
    /// Device ID this stripe lives on.
    pub devid: u64,
    /// Physical byte offset on the device.
    pub offset: u64,
    /// UUID of the device.
    pub dev_uuid: [u8; BTRFS_UUID_SIZE],
}

impl Stripe {
    /// Parse a stripe from a 32-byte buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < BTRFS_STRIPE_SIZE {
            log::error!("[btrfs::chunk] stripe buffer too small: {} bytes (need >= {})", buf.len(), BTRFS_STRIPE_SIZE);
            return None;
        }

        let mut dev_uuid = [0u8; BTRFS_UUID_SIZE];
        dev_uuid.copy_from_slice(&buf[16..32]);

        let stripe = Stripe {
            devid: read_u64(buf, 0),
            offset: read_u64(buf, 8),
            dev_uuid,
        };

        log::trace!("[btrfs::chunk] stripe: devid={}, offset=0x{:X}", stripe.devid, stripe.offset);
        Some(stripe)
    }

    /// Serialize this stripe to a 32-byte buffer.
    pub fn to_bytes(&self) -> [u8; BTRFS_STRIPE_SIZE] {
        let mut buf = [0u8; BTRFS_STRIPE_SIZE];
        write_u64(&mut buf, 0, self.devid);
        write_u64(&mut buf, 8, self.offset);
        buf[16..32].copy_from_slice(&self.dev_uuid);
        buf
    }
}

impl fmt::Debug for Stripe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stripe")
            .field("devid", &self.devid)
            .field("offset", &format_args!("0x{:X}", self.offset))
            .finish()
    }
}

/// A parsed btrfs chunk item.
#[derive(Clone)]
pub struct ChunkItem {
    /// Size of the chunk in bytes.
    pub length: u64,
    /// Owner tree objectid (usually EXTENT_TREE_OBJECTID).
    pub owner: u64,
    /// Stripe length for RAID (usually 64 KiB).
    pub stripe_len: u64,
    /// Chunk type flags (DATA, METADATA, SYSTEM, RAID*, DUP).
    pub chunk_type: u64,
    /// Optimal I/O alignment.
    pub io_align: u32,
    /// Optimal I/O width.
    pub io_width: u32,
    /// Minimum I/O size (sector size).
    pub sector_size: u32,
    /// Number of stripes.
    pub num_stripes: u16,
    /// Minimum stripes for RAID (sub_stripes for RAID10).
    pub sub_stripes: u16,
    /// Stripe definitions.
    pub stripes: Vec<Stripe>,
}

impl ChunkItem {
    /// Parse a chunk item from raw bytes (the item data from a leaf, without the key).
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < BTRFS_CHUNK_ITEM_HEADER_SIZE {
            log::error!("[btrfs::chunk] chunk item buffer too small: {} bytes (need >= {})",
                buf.len(), BTRFS_CHUNK_ITEM_HEADER_SIZE);
            return None;
        }

        let length = read_u64(buf, 0);
        let owner = read_u64(buf, 8);
        let stripe_len = read_u64(buf, 16);
        let chunk_type = read_u64(buf, 24);
        let io_align = read_u32(buf, 32);
        let io_width = read_u32(buf, 36);
        let sector_size = read_u32(buf, 40);
        let num_stripes = read_u16(buf, 44);
        let sub_stripes = read_u16(buf, 46);

        let expected_size = BTRFS_CHUNK_ITEM_HEADER_SIZE + num_stripes as usize * BTRFS_STRIPE_SIZE;
        if buf.len() < expected_size {
            log::error!("[btrfs::chunk] chunk item too small for {} stripes: need {} bytes, have {}",
                num_stripes, expected_size, buf.len());
            return None;
        }

        let mut stripes = Vec::with_capacity(num_stripes as usize);
        for i in 0..num_stripes as usize {
            let stripe_off = BTRFS_CHUNK_ITEM_HEADER_SIZE + i * BTRFS_STRIPE_SIZE;
            if let Some(stripe) = Stripe::from_bytes(&buf[stripe_off..]) {
                stripes.push(stripe);
            }
        }

        log::debug!("[btrfs::chunk] parsed chunk: length={}, type=0x{:X}, num_stripes={}, stripe_len={}",
            length, chunk_type, num_stripes, stripe_len);

        Some(ChunkItem {
            length,
            owner,
            stripe_len,
            chunk_type,
            io_align,
            io_width,
            sector_size,
            num_stripes,
            sub_stripes,
            stripes,
        })
    }

    /// Serialize this chunk item to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let total = BTRFS_CHUNK_ITEM_HEADER_SIZE + self.stripes.len() * BTRFS_STRIPE_SIZE;
        let mut buf = alloc::vec![0u8; total];

        write_u64(&mut buf, 0, self.length);
        write_u64(&mut buf, 8, self.owner);
        write_u64(&mut buf, 16, self.stripe_len);
        write_u64(&mut buf, 24, self.chunk_type);
        write_u32(&mut buf, 32, self.io_align);
        write_u32(&mut buf, 36, self.io_width);
        write_u32(&mut buf, 40, self.sector_size);
        write_u16(&mut buf, 44, self.num_stripes);
        write_u16(&mut buf, 46, self.sub_stripes);

        for (i, stripe) in self.stripes.iter().enumerate() {
            let off = BTRFS_CHUNK_ITEM_HEADER_SIZE + i * BTRFS_STRIPE_SIZE;
            buf[off..off + BTRFS_STRIPE_SIZE].copy_from_slice(&stripe.to_bytes());
        }

        log::trace!("[btrfs::chunk] serialized chunk item: {} bytes", buf.len());
        buf
    }

    /// Whether this is a data chunk.
    #[inline]
    pub fn is_data(&self) -> bool {
        self.chunk_type & chunk_type::DATA != 0
    }

    /// Whether this is a metadata chunk.
    #[inline]
    pub fn is_metadata(&self) -> bool {
        self.chunk_type & chunk_type::METADATA != 0
    }

    /// Whether this is a system chunk.
    #[inline]
    pub fn is_system(&self) -> bool {
        self.chunk_type & chunk_type::SYSTEM != 0
    }

    /// Get total size on disk including all stripes.
    pub fn total_size(&self) -> usize {
        BTRFS_CHUNK_ITEM_HEADER_SIZE + self.stripes.len() * BTRFS_STRIPE_SIZE
    }
}

impl fmt::Debug for ChunkItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut types = alloc::string::String::new();
        if self.is_data() { types.push_str("DATA "); }
        if self.is_metadata() { types.push_str("METADATA "); }
        if self.is_system() { types.push_str("SYSTEM "); }
        f.debug_struct("ChunkItem")
            .field("length", &self.length)
            .field("type", &format_args!("0x{:X} [{}]", self.chunk_type, types.trim()))
            .field("num_stripes", &self.num_stripes)
            .field("stripe_len", &self.stripe_len)
            .field("stripes", &self.stripes)
            .finish()
    }
}

/// A chunk map entry: maps a range of logical addresses to a chunk item.
#[derive(Clone, Debug)]
pub struct ChunkMapEntry {
    /// Start of the logical address range.
    pub logical: u64,
    /// Chunk item describing the physical mapping.
    pub chunk: ChunkItem,
}

/// The chunk map: a sorted list of logical->physical mappings.
///
/// Built at mount time from the sys_chunk_array (bootstrap) and the chunk tree.
#[derive(Clone)]
pub struct ChunkMap {
    /// Sorted entries (by logical address).
    pub entries: Vec<ChunkMapEntry>,
}

impl ChunkMap {
    /// Create an empty chunk map.
    pub fn new() -> Self {
        ChunkMap { entries: Vec::new() }
    }

    /// Add a chunk mapping.
    pub fn insert(&mut self, logical: u64, chunk: ChunkItem) {
        log::debug!("[btrfs::chunk] adding chunk map entry: logical=0x{:X}, length={}, type=0x{:X}",
            logical, chunk.length, chunk.chunk_type);

        // Insert in sorted order
        let pos = self.entries.iter().position(|e| e.logical > logical);
        let entry = ChunkMapEntry { logical, chunk };
        match pos {
            Some(i) => self.entries.insert(i, entry),
            None => self.entries.push(entry),
        }
    }

    /// Resolve a logical address to a physical (device, offset) pair.
    ///
    /// Returns the physical byte offset on device stripe[0]. For RAID configurations,
    /// the caller may need to consider additional stripes.
    pub fn resolve(&self, logical: u64) -> Option<(u64, u64)> {
        // Binary search for the chunk containing this logical address
        let mut lo = 0usize;
        let mut hi = self.entries.len();

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.entries[mid].logical <= logical {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        if lo == 0 {
            log::error!("[btrfs::chunk] no chunk mapping for logical address 0x{:X}", logical);
            return None;
        }

        let entry = &self.entries[lo - 1];
        let chunk_end = entry.logical + entry.chunk.length;
        if logical >= chunk_end {
            log::error!("[btrfs::chunk] logical address 0x{:X} past end of chunk (0x{:X}..0x{:X})",
                logical, entry.logical, chunk_end);
            return None;
        }

        let offset_in_chunk = logical - entry.logical;

        // For single/DUP/RAID1, use the first stripe directly
        if let Some(stripe) = entry.chunk.stripes.first() {
            let physical = stripe.offset + offset_in_chunk;
            log::trace!("[btrfs::chunk] resolved logical 0x{:X} -> devid={}, physical=0x{:X} (chunk at 0x{:X})",
                logical, stripe.devid, physical, entry.logical);
            return Some((stripe.devid, physical));
        }

        log::error!("[btrfs::chunk] chunk at logical 0x{:X} has no stripes", entry.logical);
        None
    }

    /// Parse the sys_chunk_array from the superblock to bootstrap chunk mappings.
    ///
    /// The sys_chunk_array contains a sequence of (key, chunk_item) pairs.
    pub fn parse_sys_chunk_array(&mut self, array: &[u8], array_size: u32) {
        log::info!("[btrfs::chunk] parsing sys_chunk_array: {} bytes", array_size);

        let mut offset = 0usize;
        let end = array_size as usize;

        while offset < end {
            // Parse key
            if offset + BTRFS_KEY_SIZE > end {
                log::trace!("[btrfs::chunk] sys_chunk_array: not enough bytes for key at offset {}", offset);
                break;
            }

            let key = match BtrfsKey::from_bytes(&array[offset..]) {
                Some(k) => k,
                None => {
                    log::error!("[btrfs::chunk] sys_chunk_array: failed to parse key at offset {}", offset);
                    break;
                }
            };
            offset += BTRFS_KEY_SIZE;

            // Parse chunk item
            if offset + BTRFS_CHUNK_ITEM_HEADER_SIZE > end {
                log::error!("[btrfs::chunk] sys_chunk_array: not enough bytes for chunk header at offset {}", offset);
                break;
            }

            let chunk = match ChunkItem::from_bytes(&array[offset..end]) {
                Some(c) => c,
                None => {
                    log::error!("[btrfs::chunk] sys_chunk_array: failed to parse chunk at offset {}", offset);
                    break;
                }
            };

            let chunk_total = chunk.total_size();
            offset += chunk_total;

            let logical = key.offset;
            log::debug!("[btrfs::chunk] sys_chunk_array: chunk at logical=0x{:X}, length={}, type=0x{:X}",
                logical, chunk.length, chunk.chunk_type);

            self.insert(logical, chunk);
        }

        log::info!("[btrfs::chunk] sys_chunk_array parsed: {} chunk mappings", self.entries.len());
    }

    /// Number of chunk entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the chunk map is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl fmt::Debug for ChunkMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChunkMap")
            .field("entries", &self.entries.len())
            .finish()
    }
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

#[inline]
fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

#[inline]
fn write_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_le_bytes());
}
