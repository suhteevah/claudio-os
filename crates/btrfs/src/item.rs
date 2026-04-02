//! btrfs tree node structures (headers, leaves, internal nodes).
//!
//! Every tree node (both leaf and internal) starts with a `btrfs_header` (101 bytes).
//!
//! - **Leaf nodes** (level == 0): header followed by an array of `btrfs_item` descriptors,
//!   with item data packed from the end of the node backwards.
//! - **Internal nodes** (level > 0): header followed by an array of `btrfs_key_ptr` entries,
//!   each pointing to a child node.
//!
//! Reference: <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html#header>

use alloc::vec::Vec;
use core::fmt;

use crate::crc32c;
use crate::key::{BtrfsKey, BTRFS_KEY_SIZE};
use crate::superblock::BTRFS_CSUM_SIZE;

/// Size of the btrfs_header on disk (101 bytes).
pub const BTRFS_HEADER_SIZE: usize = 101;

/// Size of a btrfs_item descriptor on disk (25 bytes: key + offset + size).
pub const BTRFS_ITEM_SIZE: usize = 25;

/// Size of a btrfs_key_ptr on disk (33 bytes: key + blockptr + generation).
pub const BTRFS_KEY_PTR_SIZE: usize = 33;

/// btrfs tree node header. Present at the start of every tree node (leaf and internal).
#[derive(Clone)]
pub struct BtrfsHeader {
    /// Checksum of everything after the csum field.
    pub csum: [u8; BTRFS_CSUM_SIZE],
    /// Filesystem UUID (must match superblock fsid).
    pub fsid: [u8; 16],
    /// Logical byte number of this node (its address in the tree).
    pub bytenr: u64,
    /// Flags (currently: bit 0 = WRITTEN, meaning this node has been flushed).
    pub flags: u64,
    /// UUID of the chunk tree that owns this node.
    pub chunk_tree_uuid: [u8; 16],
    /// Generation (transaction id) when this node was last written.
    pub generation: u64,
    /// The objectid of the tree this node belongs to (e.g., FS_TREE=5).
    pub owner: u64,
    /// Number of items (leaf) or key_ptrs (internal) in this node.
    pub nritems: u32,
    /// Level of this node in the tree. 0 = leaf, >0 = internal.
    pub level: u8,
}

impl BtrfsHeader {
    /// Parse a header from the first 101 bytes of a node buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < BTRFS_HEADER_SIZE {
            log::error!("[btrfs::item] header buffer too small: {} bytes (need >= {})", buf.len(), BTRFS_HEADER_SIZE);
            return None;
        }

        let mut csum = [0u8; BTRFS_CSUM_SIZE];
        csum.copy_from_slice(&buf[0..BTRFS_CSUM_SIZE]);

        let mut fsid = [0u8; 16];
        fsid.copy_from_slice(&buf[0x20..0x30]);

        let mut chunk_tree_uuid = [0u8; 16];
        chunk_tree_uuid.copy_from_slice(&buf[0x40..0x50]);

        let hdr = BtrfsHeader {
            csum,
            fsid,
            bytenr:          read_u64(buf, 0x30),
            flags:           read_u64(buf, 0x38),
            chunk_tree_uuid,
            generation:      read_u64(buf, 0x50),
            owner:           read_u64(buf, 0x58),
            nritems:         read_u32(buf, 0x60),
            level:           buf[0x64],
        };

        log::trace!("[btrfs::item] header: bytenr=0x{:X}, gen={}, owner={}, nritems={}, level={}",
            hdr.bytenr, hdr.generation, hdr.owner, hdr.nritems, hdr.level);

        Some(hdr)
    }

    /// Serialize this header to bytes.
    pub fn to_bytes(&self) -> [u8; BTRFS_HEADER_SIZE] {
        let mut buf = [0u8; BTRFS_HEADER_SIZE];
        buf[0..BTRFS_CSUM_SIZE].copy_from_slice(&self.csum);
        buf[0x20..0x30].copy_from_slice(&self.fsid);
        write_u64(&mut buf, 0x30, self.bytenr);
        write_u64(&mut buf, 0x38, self.flags);
        buf[0x40..0x50].copy_from_slice(&self.chunk_tree_uuid);
        write_u64(&mut buf, 0x50, self.generation);
        write_u64(&mut buf, 0x58, self.owner);
        write_u32(&mut buf, 0x60, self.nritems);
        buf[0x64] = self.level;
        buf
    }

    /// Whether this is a leaf node (level == 0).
    #[inline]
    pub fn is_leaf(&self) -> bool {
        self.level == 0
    }

    /// Verify the checksum of a full node buffer (using CRC32C).
    pub fn verify_csum(&self, full_node: &[u8]) -> bool {
        let computed = crc32c::btrfs_csum(&full_node[BTRFS_CSUM_SIZE..]);
        let stored = u32::from_le_bytes([self.csum[0], self.csum[1], self.csum[2], self.csum[3]]);
        if computed != stored {
            log::warn!("[btrfs::item] header csum mismatch at bytenr=0x{:X}: computed=0x{:08X}, stored=0x{:08X}",
                self.bytenr, computed, stored);
            false
        } else {
            log::trace!("[btrfs::item] header csum verified at bytenr=0x{:X}: 0x{:08X}", self.bytenr, computed);
            true
        }
    }

    /// Recompute and store the CRC32C checksum for a full node buffer.
    pub fn update_csum(node_buf: &mut [u8]) {
        let computed = crc32c::btrfs_csum(&node_buf[BTRFS_CSUM_SIZE..]);
        node_buf[0..4].copy_from_slice(&computed.to_le_bytes());
        for b in &mut node_buf[4..BTRFS_CSUM_SIZE] {
            *b = 0;
        }
        log::trace!("[btrfs::item] updated node csum: 0x{:08X}", computed);
    }
}

impl fmt::Debug for BtrfsHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BtrfsHeader")
            .field("bytenr", &format_args!("0x{:X}", self.bytenr))
            .field("generation", &self.generation)
            .field("owner", &self.owner)
            .field("nritems", &self.nritems)
            .field("level", &self.level)
            .finish()
    }
}

/// A btrfs item descriptor in a leaf node.
///
/// Items are stored right after the header in a leaf. The actual item data is stored
/// at the end of the node, growing backwards. Each item descriptor says where its
/// data is and how large it is (offset + size relative to the start of the node).
#[derive(Clone)]
pub struct BtrfsItemDesc {
    /// The key identifying this item.
    pub key: BtrfsKey,
    /// Byte offset of the item data from the start of the leaf node.
    pub offset: u32,
    /// Size of the item data in bytes.
    pub size: u32,
}

impl BtrfsItemDesc {
    /// Parse an item descriptor from a 25-byte buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < BTRFS_ITEM_SIZE {
            log::error!("[btrfs::item] item desc buffer too small: {} bytes (need >= {})", buf.len(), BTRFS_ITEM_SIZE);
            return None;
        }

        let key = BtrfsKey::from_bytes(&buf[0..BTRFS_KEY_SIZE])?;
        let offset = read_u32(buf, BTRFS_KEY_SIZE);
        let size = read_u32(buf, BTRFS_KEY_SIZE + 4);

        log::trace!("[btrfs::item] item desc: key={}, offset={}, size={}", key, offset, size);

        Some(BtrfsItemDesc { key, offset, size })
    }

    /// Serialize this item descriptor to a 25-byte buffer.
    pub fn to_bytes(&self) -> [u8; BTRFS_ITEM_SIZE] {
        let mut buf = [0u8; BTRFS_ITEM_SIZE];
        let key_bytes = self.key.to_bytes();
        buf[0..BTRFS_KEY_SIZE].copy_from_slice(&key_bytes);
        write_u32(&mut buf, BTRFS_KEY_SIZE, self.offset);
        write_u32(&mut buf, BTRFS_KEY_SIZE + 4, self.size);
        buf
    }
}

impl fmt::Debug for BtrfsItemDesc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BtrfsItemDesc")
            .field("key", &self.key)
            .field("offset", &self.offset)
            .field("size", &self.size)
            .finish()
    }
}

/// A btrfs key_ptr in an internal (non-leaf) node.
///
/// Each key_ptr contains a key (the leftmost key in the child subtree) and the
/// logical block pointer + generation of the child node.
#[derive(Clone)]
pub struct BtrfsKeyPtr {
    /// The key (leftmost key of the child subtree).
    pub key: BtrfsKey,
    /// Logical byte address of the child node.
    pub blockptr: u64,
    /// Generation of the child node.
    pub generation: u64,
}

impl BtrfsKeyPtr {
    /// Parse a key_ptr from a 33-byte buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < BTRFS_KEY_PTR_SIZE {
            log::error!("[btrfs::item] key_ptr buffer too small: {} bytes (need >= {})", buf.len(), BTRFS_KEY_PTR_SIZE);
            return None;
        }

        let key = BtrfsKey::from_bytes(&buf[0..BTRFS_KEY_SIZE])?;
        let blockptr = read_u64(buf, BTRFS_KEY_SIZE);
        let generation = read_u64(buf, BTRFS_KEY_SIZE + 8);

        log::trace!("[btrfs::item] key_ptr: key={}, blockptr=0x{:X}, gen={}",
            key, blockptr, generation);

        Some(BtrfsKeyPtr { key, blockptr, generation })
    }

    /// Serialize this key_ptr to a 33-byte buffer.
    pub fn to_bytes(&self) -> [u8; BTRFS_KEY_PTR_SIZE] {
        let mut buf = [0u8; BTRFS_KEY_PTR_SIZE];
        let key_bytes = self.key.to_bytes();
        buf[0..BTRFS_KEY_SIZE].copy_from_slice(&key_bytes);
        write_u64(&mut buf, BTRFS_KEY_SIZE, self.blockptr);
        write_u64(&mut buf, BTRFS_KEY_SIZE + 8, self.generation);
        buf
    }
}

impl fmt::Debug for BtrfsKeyPtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BtrfsKeyPtr")
            .field("key", &self.key)
            .field("blockptr", &format_args!("0x{:X}", self.blockptr))
            .field("generation", &self.generation)
            .finish()
    }
}

/// A parsed btrfs leaf node (level == 0).
///
/// Contains the header, item descriptors, and the raw node data for extracting
/// item payloads.
#[derive(Clone)]
pub struct BtrfsLeaf {
    /// The node header.
    pub header: BtrfsHeader,
    /// Item descriptors (sorted by key).
    pub items: Vec<BtrfsItemDesc>,
    /// Raw node data (needed to extract item payloads by offset/size).
    pub data: Vec<u8>,
}

impl BtrfsLeaf {
    /// Parse a leaf node from a full node buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        let header = BtrfsHeader::from_bytes(buf)?;

        if !header.is_leaf() {
            log::error!("[btrfs::item] expected leaf (level=0), got level={}", header.level);
            return None;
        }

        let mut items = Vec::with_capacity(header.nritems as usize);
        for i in 0..header.nritems as usize {
            let item_off = BTRFS_HEADER_SIZE + i * BTRFS_ITEM_SIZE;
            if item_off + BTRFS_ITEM_SIZE > buf.len() {
                log::warn!("[btrfs::item] truncated leaf at item {}", i);
                break;
            }
            if let Some(item) = BtrfsItemDesc::from_bytes(&buf[item_off..]) {
                items.push(item);
            }
        }

        log::debug!("[btrfs::item] parsed leaf: bytenr=0x{:X}, nritems={}, parsed={}",
            header.bytenr, header.nritems, items.len());

        Some(BtrfsLeaf {
            header,
            items,
            data: buf.to_vec(),
        })
    }

    /// Get the raw data for item at index `i`.
    pub fn item_data(&self, i: usize) -> Option<&[u8]> {
        let item = self.items.get(i)?;
        let start = item.offset as usize;
        let end = start + item.size as usize;
        if end > self.data.len() {
            log::error!("[btrfs::item] item {} data out of bounds: offset={}, size={}, node_len={}",
                i, item.offset, item.size, self.data.len());
            return None;
        }
        Some(&self.data[start..end])
    }

    /// Find the first item whose key matches (or is the first key >= the search key).
    ///
    /// Returns the index of the matching item, or `None` if no match.
    pub fn find_key(&self, search: &BtrfsKey) -> Option<usize> {
        // Binary search for the first item >= search key
        let mut lo = 0usize;
        let mut hi = self.items.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.items[mid].key < *search {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < self.items.len() {
            log::trace!("[btrfs::item] find_key: search={}, found index {} key={}",
                search, lo, self.items[lo].key);
            Some(lo)
        } else {
            log::trace!("[btrfs::item] find_key: search={}, not found (past end)", search);
            None
        }
    }

    /// Find an item with an exact key match.
    pub fn find_exact(&self, search: &BtrfsKey) -> Option<usize> {
        let idx = self.find_key(search)?;
        if self.items[idx].key == *search {
            Some(idx)
        } else {
            None
        }
    }
}

impl fmt::Debug for BtrfsLeaf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BtrfsLeaf")
            .field("header", &self.header)
            .field("items_count", &self.items.len())
            .finish()
    }
}

/// A parsed btrfs internal node (level > 0).
#[derive(Clone)]
pub struct BtrfsNode {
    /// The node header.
    pub header: BtrfsHeader,
    /// Key pointer entries (sorted by key).
    pub ptrs: Vec<BtrfsKeyPtr>,
}

impl BtrfsNode {
    /// Parse an internal node from a full node buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        let header = BtrfsHeader::from_bytes(buf)?;

        if header.is_leaf() {
            log::error!("[btrfs::item] expected internal node (level>0), got level=0");
            return None;
        }

        let mut ptrs = Vec::with_capacity(header.nritems as usize);
        for i in 0..header.nritems as usize {
            let ptr_off = BTRFS_HEADER_SIZE + i * BTRFS_KEY_PTR_SIZE;
            if ptr_off + BTRFS_KEY_PTR_SIZE > buf.len() {
                log::warn!("[btrfs::item] truncated internal node at ptr {}", i);
                break;
            }
            if let Some(ptr) = BtrfsKeyPtr::from_bytes(&buf[ptr_off..]) {
                ptrs.push(ptr);
            }
        }

        log::debug!("[btrfs::item] parsed internal node: bytenr=0x{:X}, level={}, nritems={}, parsed={}",
            header.bytenr, header.level, header.nritems, ptrs.len());

        Some(BtrfsNode { header, ptrs })
    }

    /// Find the child pointer that should contain the given key.
    ///
    /// Returns the index of the last key_ptr whose key is <= search_key.
    pub fn find_child(&self, search: &BtrfsKey) -> Option<usize> {
        if self.ptrs.is_empty() {
            return None;
        }

        // Find the last ptr where key <= search
        let mut lo = 0usize;
        let mut hi = self.ptrs.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.ptrs[mid].key <= *search {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        let idx = if lo == 0 { 0 } else { lo - 1 };
        log::trace!("[btrfs::item] find_child: search={}, selected ptr index {} -> blockptr=0x{:X}",
            search, idx, self.ptrs[idx].blockptr);
        Some(idx)
    }
}

impl fmt::Debug for BtrfsNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BtrfsNode")
            .field("header", &self.header)
            .field("ptrs_count", &self.ptrs.len())
            .finish()
    }
}

// --- Little-endian byte helpers ---

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
fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

#[inline]
fn write_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_le_bytes());
}
