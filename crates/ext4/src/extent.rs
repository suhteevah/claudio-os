//! ext4 extent tree parsing and traversal.
//!
//! ext4 replaces the legacy block map (12 direct + 3 indirect pointers) with an extent
//! tree stored in the inode's `i_block` area. This allows efficient mapping of large
//! contiguous regions of logical blocks to physical blocks.
//!
//! The extent tree is a B-tree:
//! - The root node is stored in the inode's i_block (60 bytes).
//! - Internal nodes contain `ExtentIndex` entries pointing to child nodes.
//! - Leaf nodes contain `ExtentLeaf` entries mapping logical to physical blocks.
//! - Every node starts with an `ExtentHeader`.
//!
//! Reference: <https://ext4.wiki.kernel.org/index.php/Ext4_Disk_Layout#Extent_Tree>

use alloc::vec::Vec;
use core::fmt;

/// Magic number for the extent header: 0xF30A.
pub const EXT4_EXTENT_MAGIC: u16 = 0xF30A;

/// Size of the extent header in bytes.
pub const EXTENT_HEADER_SIZE: usize = 12;

/// Size of each extent index entry in bytes.
pub const EXTENT_INDEX_SIZE: usize = 12;

/// Size of each extent leaf entry in bytes.
pub const EXTENT_LEAF_SIZE: usize = 12;

/// Maximum number of extents that fit in the inode's i_block root node.
/// (60 bytes - 12 header) / 12 per entry = 4 entries.
pub const ROOT_MAX_ENTRIES: u16 = 4;

/// Extent tree header. Present at the start of every extent tree node.
#[derive(Clone, Copy)]
pub struct ExtentHeader {
    /// Magic number, must be EXT4_EXTENT_MAGIC (0xF30A).
    pub magic: u16,
    /// Number of valid entries following the header.
    pub entries: u16,
    /// Maximum number of entries that can be stored in this node.
    pub max: u16,
    /// Depth of this node in the tree. 0 = leaf node, >0 = internal node.
    pub depth: u16,
    /// Generation of the tree (used for caching, can be 0).
    pub generation: u32,
}

impl ExtentHeader {
    /// Parse an extent header from the first 12 bytes of a buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < EXTENT_HEADER_SIZE {
            log::error!("[ext4::extent] header buffer too small: {} bytes", buf.len());
            return None;
        }

        let magic = u16::from_le_bytes([buf[0], buf[1]]);
        if magic != EXT4_EXTENT_MAGIC {
            log::error!("[ext4::extent] invalid extent magic: 0x{:04X} (expected 0x{:04X})", magic, EXT4_EXTENT_MAGIC);
            return None;
        }

        let hdr = ExtentHeader {
            magic,
            entries: u16::from_le_bytes([buf[2], buf[3]]),
            max:     u16::from_le_bytes([buf[4], buf[5]]),
            depth:   u16::from_le_bytes([buf[6], buf[7]]),
            generation: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
        };

        log::trace!("[ext4::extent] header: entries={}, max={}, depth={}, gen={}",
            hdr.entries, hdr.max, hdr.depth, hdr.generation);

        Some(hdr)
    }

    /// Serialize the extent header to a 12-byte buffer.
    pub fn to_bytes(&self) -> [u8; EXTENT_HEADER_SIZE] {
        let mut buf = [0u8; EXTENT_HEADER_SIZE];
        buf[0..2].copy_from_slice(&self.magic.to_le_bytes());
        buf[2..4].copy_from_slice(&self.entries.to_le_bytes());
        buf[4..6].copy_from_slice(&self.max.to_le_bytes());
        buf[6..8].copy_from_slice(&self.depth.to_le_bytes());
        buf[8..12].copy_from_slice(&self.generation.to_le_bytes());
        buf
    }

    /// Whether this node is a leaf node (depth == 0).
    #[inline]
    pub fn is_leaf(&self) -> bool {
        self.depth == 0
    }
}

impl fmt::Debug for ExtentHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtentHeader")
            .field("magic", &format_args!("0x{:04X}", self.magic))
            .field("entries", &self.entries)
            .field("max", &self.max)
            .field("depth", &self.depth)
            .finish()
    }
}

/// Extent index entry (internal node). Points to a child node stored in a data block.
#[derive(Clone, Copy)]
pub struct ExtentIndex {
    /// Logical block number that this subtree covers (first logical block).
    pub block: u32,
    /// Physical block number of the child node (low 32 bits).
    pub leaf_lo: u32,
    /// Physical block number of the child node (high 16 bits).
    pub leaf_hi: u16,
    /// Padding (unused).
    pub padding: u16,
}

impl ExtentIndex {
    /// Parse an extent index entry from a 12-byte buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < EXTENT_INDEX_SIZE {
            log::error!("[ext4::extent] index buffer too small: {} bytes", buf.len());
            return None;
        }

        let idx = ExtentIndex {
            block:   u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            leaf_lo: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            leaf_hi: u16::from_le_bytes([buf[8], buf[9]]),
            padding: u16::from_le_bytes([buf[10], buf[11]]),
        };

        log::trace!("[ext4::extent] index: logical_block={}, child_phys={}", idx.block, idx.physical_block());
        Some(idx)
    }

    /// Serialize the extent index to a 12-byte buffer.
    pub fn to_bytes(&self) -> [u8; EXTENT_INDEX_SIZE] {
        let mut buf = [0u8; EXTENT_INDEX_SIZE];
        buf[0..4].copy_from_slice(&self.block.to_le_bytes());
        buf[4..8].copy_from_slice(&self.leaf_lo.to_le_bytes());
        buf[8..10].copy_from_slice(&self.leaf_hi.to_le_bytes());
        buf[10..12].copy_from_slice(&self.padding.to_le_bytes());
        buf
    }

    /// Full 48-bit physical block number of the child node.
    #[inline]
    pub fn physical_block(&self) -> u64 {
        self.leaf_lo as u64 | ((self.leaf_hi as u64) << 32)
    }
}

impl fmt::Debug for ExtentIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtentIndex")
            .field("logical_block", &self.block)
            .field("physical_block", &self.physical_block())
            .finish()
    }
}

/// Extent leaf entry. Maps a range of logical blocks to a contiguous range of physical blocks.
#[derive(Clone, Copy)]
pub struct ExtentLeaf {
    /// First logical block number that this extent covers.
    pub block: u32,
    /// Number of blocks covered by this extent.
    /// If the high bit (0x8000) is set, the extent is uninitialized (pre-allocated but unwritten).
    pub len: u16,
    /// Starting physical block number (high 16 bits).
    pub start_hi: u16,
    /// Starting physical block number (low 32 bits).
    pub start_lo: u32,
}

impl ExtentLeaf {
    /// Parse an extent leaf entry from a 12-byte buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < EXTENT_LEAF_SIZE {
            log::error!("[ext4::extent] leaf buffer too small: {} bytes", buf.len());
            return None;
        }

        let leaf = ExtentLeaf {
            block:    u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            len:      u16::from_le_bytes([buf[4], buf[5]]),
            start_hi: u16::from_le_bytes([buf[6], buf[7]]),
            start_lo: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
        };

        log::trace!("[ext4::extent] leaf: logical={}, len={}, phys_start={}, uninit={}",
            leaf.block, leaf.block_count(), leaf.physical_start(), leaf.is_uninitialized());
        Some(leaf)
    }

    /// Serialize the extent leaf to a 12-byte buffer.
    pub fn to_bytes(&self) -> [u8; EXTENT_LEAF_SIZE] {
        let mut buf = [0u8; EXTENT_LEAF_SIZE];
        buf[0..4].copy_from_slice(&self.block.to_le_bytes());
        buf[4..6].copy_from_slice(&self.len.to_le_bytes());
        buf[6..8].copy_from_slice(&self.start_hi.to_le_bytes());
        buf[8..12].copy_from_slice(&self.start_lo.to_le_bytes());
        buf
    }

    /// Full 48-bit physical start block.
    #[inline]
    pub fn physical_start(&self) -> u64 {
        self.start_lo as u64 | ((self.start_hi as u64) << 32)
    }

    /// Number of blocks in this extent (masking out the uninitialized flag).
    #[inline]
    pub fn block_count(&self) -> u32 {
        (self.len & 0x7FFF) as u32
    }

    /// Whether this extent is uninitialized (pre-allocated, not yet written).
    #[inline]
    pub fn is_uninitialized(&self) -> bool {
        self.len & 0x8000 != 0
    }

    /// Check if a logical block number falls within this extent.
    #[inline]
    pub fn contains_block(&self, logical_block: u32) -> bool {
        logical_block >= self.block && logical_block < self.block + self.block_count()
    }

    /// Map a logical block number to its physical block number using this extent.
    /// Returns `None` if the logical block is not within this extent.
    pub fn map_block(&self, logical_block: u32) -> Option<u64> {
        if self.contains_block(logical_block) {
            let offset = (logical_block - self.block) as u64;
            Some(self.physical_start() + offset)
        } else {
            None
        }
    }
}

impl fmt::Debug for ExtentLeaf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtentLeaf")
            .field("logical_start", &self.block)
            .field("len", &self.block_count())
            .field("physical_start", &self.physical_start())
            .field("uninitialized", &self.is_uninitialized())
            .finish()
    }
}

/// Parse all extent leaf entries from an extent tree node buffer (after the header).
///
/// This only reads the direct leaves in one node. For a full traversal across
/// internal nodes, use `resolve_extent_tree`.
pub fn parse_leaves(buf: &[u8]) -> Vec<ExtentLeaf> {
    let header = match ExtentHeader::from_bytes(buf) {
        Some(h) if h.is_leaf() => h,
        Some(h) => {
            log::warn!("[ext4::extent] parse_leaves called on non-leaf node (depth={})", h.depth);
            return Vec::new();
        }
        None => return Vec::new(),
    };

    let mut leaves = Vec::with_capacity(header.entries as usize);
    for i in 0..header.entries as usize {
        let offset = EXTENT_HEADER_SIZE + i * EXTENT_LEAF_SIZE;
        if offset + EXTENT_LEAF_SIZE > buf.len() {
            log::warn!("[ext4::extent] truncated leaf at index {}", i);
            break;
        }
        if let Some(leaf) = ExtentLeaf::from_bytes(&buf[offset..]) {
            leaves.push(leaf);
        }
    }

    log::debug!("[ext4::extent] parsed {} leaves from node", leaves.len());
    leaves
}

/// Parse all extent index entries from an internal node buffer (after the header).
pub fn parse_indices(buf: &[u8]) -> Vec<ExtentIndex> {
    let header = match ExtentHeader::from_bytes(buf) {
        Some(h) if !h.is_leaf() => h,
        Some(h) => {
            log::warn!("[ext4::extent] parse_indices called on leaf node (depth={})", h.depth);
            return Vec::new();
        }
        None => return Vec::new(),
    };

    let mut indices = Vec::with_capacity(header.entries as usize);
    for i in 0..header.entries as usize {
        let offset = EXTENT_HEADER_SIZE + i * EXTENT_INDEX_SIZE;
        if offset + EXTENT_INDEX_SIZE > buf.len() {
            log::warn!("[ext4::extent] truncated index at entry {}", i);
            break;
        }
        if let Some(idx) = ExtentIndex::from_bytes(&buf[offset..]) {
            indices.push(idx);
        }
    }

    log::debug!("[ext4::extent] parsed {} indices from node", indices.len());
    indices
}

/// Find the extent index whose subtree contains the given logical block.
///
/// Uses binary search over the sorted index entries. Returns the index entry
/// whose `block` is <= `logical_block` (the largest such entry).
pub fn find_index_for_block(indices: &[ExtentIndex], logical_block: u32) -> Option<&ExtentIndex> {
    if indices.is_empty() {
        return None;
    }

    // Binary search: find the last index where block <= logical_block
    let mut lo = 0usize;
    let mut hi = indices.len();

    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if indices[mid].block <= logical_block {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }

    if lo == 0 {
        log::trace!("[ext4::extent] no index covers logical block {}", logical_block);
        None
    } else {
        let idx = &indices[lo - 1];
        log::trace!("[ext4::extent] found index for block {}: {:?}", logical_block, idx);
        Some(idx)
    }
}

/// Find the leaf extent that covers the given logical block.
///
/// Searches linearly through the leaf entries. For large files with many extents,
/// a binary search could be used instead.
pub fn find_leaf_for_block(leaves: &[ExtentLeaf], logical_block: u32) -> Option<&ExtentLeaf> {
    for leaf in leaves {
        if leaf.contains_block(logical_block) {
            log::trace!("[ext4::extent] found leaf for block {}: {:?}", logical_block, leaf);
            return Some(leaf);
        }
    }
    log::trace!("[ext4::extent] no leaf covers logical block {}", logical_block);
    None
}
