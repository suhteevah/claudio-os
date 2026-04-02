//! btrfs B-tree operations: traversal, search, insert, and split.
//!
//! btrfs stores all metadata in B-trees (technically B+ trees for leaves).
//! Each tree is identified by its root logical address (from the root tree or superblock).
//!
//! Tree operations require resolving logical addresses to physical via the chunk map.
//! The caller provides a closure/trait for reading nodes and resolving addresses.
//!
//! Reference: <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html#b-trees>

use alloc::vec::Vec;

use crate::item::{
    BtrfsHeader, BtrfsItemDesc, BtrfsLeaf, BtrfsNode,
    BTRFS_HEADER_SIZE, BTRFS_ITEM_SIZE,
};
use crate::key::BtrfsKey;

/// A path element: records which node and which slot we descended through.
#[derive(Clone, Debug)]
pub struct PathElement {
    /// Logical byte address of this node.
    pub bytenr: u64,
    /// The slot index chosen in this node (for internal: key_ptr index, for leaf: item index).
    pub slot: usize,
    /// Level of this node.
    pub level: u8,
}

/// A path from the root to a leaf through the B-tree.
///
/// The last element is always a leaf. Elements are ordered root-first.
#[derive(Clone, Debug)]
pub struct TreePath {
    /// Path elements from root to leaf.
    pub elements: Vec<PathElement>,
}

impl TreePath {
    /// Create a new empty path.
    pub fn new() -> Self {
        TreePath { elements: Vec::new() }
    }

    /// Get the leaf (last element) of the path.
    pub fn leaf(&self) -> Option<&PathElement> {
        self.elements.last()
    }

    /// Get the slot in the leaf.
    pub fn leaf_slot(&self) -> Option<usize> {
        self.leaf().map(|e| e.slot)
    }
}

/// Result of a tree search.
#[derive(Debug)]
pub struct SearchResult {
    /// The path through the tree to the found (or insertion point) item.
    pub path: TreePath,
    /// Whether an exact match was found.
    pub exact: bool,
    /// The leaf node containing the result.
    pub leaf: BtrfsLeaf,
}

/// Search a B-tree for a key, reading nodes via the provided callback.
///
/// `read_node` reads a full node (nodesize bytes) at the given logical address.
/// The callback is responsible for logical-to-physical translation via the chunk map.
///
/// Returns the leaf node and the path taken, along with whether an exact match was found.
pub fn search_tree<F>(
    root_bytenr: u64,
    root_level: u8,
    key: &BtrfsKey,
    nodesize: u32,
    mut read_node: F,
) -> Option<SearchResult>
where
    F: FnMut(u64) -> Option<Vec<u8>>,
{
    log::debug!("[btrfs::tree] searching tree: root=0x{:X}, level={}, key={}",
        root_bytenr, root_level, key);

    let mut path = TreePath::new();
    let mut current_bytenr = root_bytenr;
    let mut current_level = root_level;

    // Traverse internal nodes
    while current_level > 0 {
        log::trace!("[btrfs::tree] reading internal node at 0x{:X} (level={})", current_bytenr, current_level);

        let node_data = read_node(current_bytenr)?;
        if node_data.len() < nodesize as usize {
            log::error!("[btrfs::tree] short read at 0x{:X}: got {} bytes, expected {}",
                current_bytenr, node_data.len(), nodesize);
            return None;
        }

        let node = BtrfsNode::from_bytes(&node_data)?;

        if node.header.level != current_level {
            log::error!("[btrfs::tree] level mismatch at 0x{:X}: header says {}, expected {}",
                current_bytenr, node.header.level, current_level);
            return None;
        }

        let slot = node.find_child(key).unwrap_or(0);

        path.elements.push(PathElement {
            bytenr: current_bytenr,
            slot,
            level: current_level,
        });

        current_bytenr = node.ptrs[slot].blockptr;
        current_level -= 1;

        log::trace!("[btrfs::tree] descending to child at 0x{:X} via slot {}", current_bytenr, slot);
    }

    // Read the leaf node
    log::trace!("[btrfs::tree] reading leaf node at 0x{:X}", current_bytenr);
    let leaf_data = read_node(current_bytenr)?;
    if leaf_data.len() < nodesize as usize {
        log::error!("[btrfs::tree] short leaf read at 0x{:X}: got {} bytes", current_bytenr, leaf_data.len());
        return None;
    }

    let leaf = BtrfsLeaf::from_bytes(&leaf_data)?;

    // Find the key in the leaf
    let (slot, exact) = match leaf.find_exact(key) {
        Some(idx) => {
            log::debug!("[btrfs::tree] exact match at leaf slot {}", idx);
            (idx, true)
        }
        None => {
            let slot = leaf.find_key(key).unwrap_or(leaf.items.len());
            log::debug!("[btrfs::tree] no exact match, insertion point at slot {}", slot);
            (slot, false)
        }
    };

    path.elements.push(PathElement {
        bytenr: current_bytenr,
        slot,
        level: 0,
    });

    Some(SearchResult { path, exact, leaf })
}

/// Walk all items in a tree in key order, calling the visitor for each item.
///
/// This performs a depth-first traversal of the tree, visiting every leaf item.
/// Useful for listing all entries in a subvolume tree, etc.
pub fn walk_tree<F, V>(
    root_bytenr: u64,
    root_level: u8,
    nodesize: u32,
    mut read_node: F,
    mut visitor: V,
) where
    F: FnMut(u64) -> Option<Vec<u8>>,
    V: FnMut(&BtrfsKey, &[u8]),
{
    log::debug!("[btrfs::tree] walking tree: root=0x{:X}, level={}", root_bytenr, root_level);
    walk_tree_recurse(root_bytenr, root_level, nodesize, &mut read_node, &mut visitor);
}

fn walk_tree_recurse<F, V>(
    bytenr: u64,
    level: u8,
    nodesize: u32,
    read_node: &mut F,
    visitor: &mut V,
) where
    F: FnMut(u64) -> Option<Vec<u8>>,
    V: FnMut(&BtrfsKey, &[u8]),
{
    let node_data = match read_node(bytenr) {
        Some(d) => d,
        None => {
            log::error!("[btrfs::tree] failed to read node at 0x{:X} during walk", bytenr);
            return;
        }
    };

    if level == 0 {
        // Leaf node: visit all items
        if let Some(leaf) = BtrfsLeaf::from_bytes(&node_data) {
            log::trace!("[btrfs::tree] walking leaf at 0x{:X} with {} items", bytenr, leaf.items.len());
            for (i, item) in leaf.items.iter().enumerate() {
                if let Some(data) = leaf.item_data(i) {
                    visitor(&item.key, data);
                }
            }
        }
    } else {
        // Internal node: recurse into children
        if let Some(node) = BtrfsNode::from_bytes(&node_data) {
            log::trace!("[btrfs::tree] walking internal node at 0x{:X} with {} children", bytenr, node.ptrs.len());
            for ptr in &node.ptrs {
                walk_tree_recurse(ptr.blockptr, level - 1, nodesize, read_node, visitor);
            }
        }
    }
}

/// Iterator that yields (key, data) pairs from a tree in sorted order.
///
/// This collects items from the tree walk. For very large trees, consider
/// using `walk_tree` directly with a streaming visitor instead.
pub fn collect_tree_items<F>(
    root_bytenr: u64,
    root_level: u8,
    nodesize: u32,
    read_node: F,
) -> Vec<(BtrfsKey, Vec<u8>)>
where
    F: FnMut(u64) -> Option<Vec<u8>>,
{
    let mut items = Vec::new();
    walk_tree(root_bytenr, root_level, nodesize, read_node, |key, data| {
        items.push((*key, data.to_vec()));
    });
    log::debug!("[btrfs::tree] collected {} items from tree at root=0x{:X}", items.len(), root_bytenr);
    items
}

/// Insert an item into a leaf node buffer.
///
/// This is a low-level operation that modifies the raw node buffer in-place.
/// If the leaf is full, returns `None` (caller must split the leaf first).
///
/// On success, returns the modified leaf buffer.
pub fn insert_into_leaf(
    leaf_buf: &mut Vec<u8>,
    nodesize: u32,
    key: &BtrfsKey,
    data: &[u8],
    generation: u64,
    _fsid: &[u8; 16],
) -> Option<()> {
    let header = BtrfsHeader::from_bytes(leaf_buf)?;
    let nritems = header.nritems as usize;

    log::debug!("[btrfs::tree] inserting key={} ({} bytes data) into leaf at 0x{:X} (nritems={})",
        key, data.len(), header.bytenr, nritems);

    // Find insertion point
    let mut insert_idx = nritems;
    for i in 0..nritems {
        let item_off = BTRFS_HEADER_SIZE + i * BTRFS_ITEM_SIZE;
        if let Some(item) = BtrfsItemDesc::from_bytes(&leaf_buf[item_off..]) {
            if item.key >= *key {
                if item.key == *key {
                    log::warn!("[btrfs::tree] duplicate key {} in leaf", key);
                    return None;
                }
                insert_idx = i;
                break;
            }
        }
    }

    // Calculate space needed
    let items_end = BTRFS_HEADER_SIZE + (nritems + 1) * BTRFS_ITEM_SIZE;
    let data_size = data.len() as u32;

    // Find the lowest data offset (data grows down from end of node)
    let mut lowest_data_offset = nodesize;
    for i in 0..nritems {
        let item_off = BTRFS_HEADER_SIZE + i * BTRFS_ITEM_SIZE;
        if let Some(item) = BtrfsItemDesc::from_bytes(&leaf_buf[item_off..]) {
            if item.offset < lowest_data_offset {
                lowest_data_offset = item.offset;
            }
        }
    }

    let new_data_offset = lowest_data_offset - data_size;

    // Check if there's enough space
    if items_end as u32 > new_data_offset {
        log::warn!("[btrfs::tree] leaf full: items would end at {}, data starts at {}",
            items_end, new_data_offset);
        return None;
    }

    // Shift existing item descriptors after the insertion point to the right
    if insert_idx < nritems {
        let src_start = BTRFS_HEADER_SIZE + insert_idx * BTRFS_ITEM_SIZE;
        let src_end = BTRFS_HEADER_SIZE + nritems * BTRFS_ITEM_SIZE;
        let shift_len = src_end - src_start;
        // Copy backwards to avoid overlap issues
        for i in (0..shift_len).rev() {
            leaf_buf[src_start + BTRFS_ITEM_SIZE + i] = leaf_buf[src_start + i];
        }
        log::trace!("[btrfs::tree] shifted {} item descriptors right from slot {}", nritems - insert_idx, insert_idx);
    }

    // Write the new item descriptor
    let new_item = BtrfsItemDesc {
        key: *key,
        offset: new_data_offset,
        size: data_size,
    };
    let item_bytes = new_item.to_bytes();
    let item_off = BTRFS_HEADER_SIZE + insert_idx * BTRFS_ITEM_SIZE;
    leaf_buf[item_off..item_off + BTRFS_ITEM_SIZE].copy_from_slice(&item_bytes);

    // Write the item data
    leaf_buf[new_data_offset as usize..new_data_offset as usize + data.len()]
        .copy_from_slice(data);

    // Update header: increment nritems, update generation
    let new_nritems = (nritems + 1) as u32;
    write_u32(leaf_buf, 0x60, new_nritems);
    write_u64(leaf_buf, 0x50, generation);

    // Update checksum
    BtrfsHeader::update_csum(leaf_buf);

    log::info!("[btrfs::tree] inserted key={} at slot {} (data_offset={}, data_size={}), nritems now {}",
        key, insert_idx, new_data_offset, data_size, new_nritems);

    Some(())
}

/// Split a full leaf node into two halves.
///
/// Returns (left_buf, right_buf, split_key) where split_key is the first key
/// in the right leaf. The caller is responsible for:
/// 1. Writing both new node buffers to disk
/// 2. Updating the parent internal node with the new key_ptr
///
/// `right_bytenr` is the logical address allocated for the right (new) leaf.
pub fn split_leaf(
    leaf_buf: &[u8],
    nodesize: u32,
    right_bytenr: u64,
    generation: u64,
) -> Option<(Vec<u8>, Vec<u8>, BtrfsKey)> {
    let leaf = BtrfsLeaf::from_bytes(leaf_buf)?;
    let nritems = leaf.items.len();

    if nritems < 2 {
        log::error!("[btrfs::tree] cannot split leaf with {} items", nritems);
        return None;
    }

    let split_point = nritems / 2;
    let split_key = leaf.items[split_point].key;

    log::info!("[btrfs::tree] splitting leaf at 0x{:X}: {} items, split at index {} (key={})",
        leaf.header.bytenr, nritems, split_point, split_key);

    // Build left leaf (items 0..split_point)
    let mut left_buf = alloc::vec![0u8; nodesize as usize];
    let left_items = &leaf.items[..split_point];
    build_leaf_buf(&mut left_buf, &leaf.header, left_items, &leaf.data, generation, leaf.header.bytenr)?;

    // Build right leaf (items split_point..nritems)
    let mut right_buf = alloc::vec![0u8; nodesize as usize];
    let right_items = &leaf.items[split_point..];
    let mut right_header = leaf.header.clone();
    right_header.bytenr = right_bytenr;
    build_leaf_buf(&mut right_buf, &right_header, right_items, &leaf.data, generation, right_bytenr)?;

    log::debug!("[btrfs::tree] split complete: left has {} items, right has {} items",
        split_point, nritems - split_point);

    Some((left_buf, right_buf, split_key))
}

/// Helper: build a leaf node buffer from a set of items and their data.
fn build_leaf_buf(
    buf: &mut [u8],
    template_header: &BtrfsHeader,
    items: &[BtrfsItemDesc],
    src_data: &[u8],
    generation: u64,
    bytenr: u64,
) -> Option<()> {
    let nodesize = buf.len() as u32;

    // Write header
    let mut header_bytes = template_header.to_bytes();
    // Update header fields
    write_u32(&mut header_bytes, 0x60, items.len() as u32);
    write_u64(&mut header_bytes, 0x50, generation);
    write_u64(&mut header_bytes, 0x30, bytenr);
    buf[..BTRFS_HEADER_SIZE].copy_from_slice(&header_bytes);

    // Pack items: data grows down from end of node
    let mut data_end = nodesize;
    for (i, item) in items.iter().enumerate() {
        let item_data = &src_data[item.offset as usize..(item.offset + item.size) as usize];
        let new_offset = data_end - item.size;
        data_end = new_offset;

        // Write item data at new offset
        buf[new_offset as usize..(new_offset + item.size) as usize].copy_from_slice(item_data);

        // Write item descriptor
        let new_desc = BtrfsItemDesc {
            key: item.key,
            offset: new_offset,
            size: item.size,
        };
        let desc_bytes = new_desc.to_bytes();
        let desc_off = BTRFS_HEADER_SIZE + i * BTRFS_ITEM_SIZE;
        buf[desc_off..desc_off + BTRFS_ITEM_SIZE].copy_from_slice(&desc_bytes);
    }

    // Update checksum
    BtrfsHeader::update_csum(buf);

    Some(())
}

// --- Byte helpers ---

#[inline]
fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

#[inline]
fn write_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_le_bytes());
}
