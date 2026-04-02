//! High-level ext4 filesystem read/write API.
//!
//! This module provides the main `Ext4Fs` type that ties together superblock,
//! block groups, inodes, directories, extents, and bitmaps into a usable
//! filesystem interface.
//!
//! ## Usage
//!
//! Implement the `BlockDevice` trait for your storage backend, then:
//!
//! ```rust,no_run
//! use claudio_ext4::{Ext4Fs, BlockDevice};
//!
//! let fs = Ext4Fs::mount(my_device).expect("mount failed");
//! let data = fs.read_file(b"/hello.txt").expect("read failed");
//! fs.write_file(b"/output.txt", &data).expect("write failed");
//! ```

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::bitmap::BitmapAllocator;
use crate::block_group::{self, BlockGroupDesc};
use crate::dir::{self, DirEntry, DirEntryIter, FT_DIR, FT_REG_FILE};
use crate::extent::{self, ExtentHeader, ExtentLeaf, EXTENT_HEADER_SIZE, EXTENT_LEAF_SIZE};
use crate::inode::{self, Inode, ROOT_INODE};
use crate::superblock::{Superblock, SUPERBLOCK_OFFSET, SUPERBLOCK_SIZE};

/// Errors that can occur during ext4 filesystem operations.
#[derive(Debug)]
pub enum Ext4Error {
    /// The device returned an I/O error.
    IoError,
    /// The superblock magic number is invalid or the superblock is corrupt.
    InvalidSuperblock,
    /// An unsupported feature flag was encountered.
    UnsupportedFeature(&'static str),
    /// The requested path was not found.
    NotFound,
    /// A path component is not a directory.
    NotADirectory,
    /// The target path already exists.
    AlreadyExists,
    /// No free blocks available for allocation.
    NoFreeBlocks,
    /// No free inodes available for allocation.
    NoFreeInodes,
    /// The filesystem is corrupt (e.g., invalid extent tree).
    Corrupt(&'static str),
    /// A filename exceeds the maximum length (255 bytes).
    NameTooLong,
    /// The path is invalid (empty, missing leading slash, etc.).
    InvalidPath,
    /// The target is a directory when a file was expected.
    IsADirectory,
    /// The target is a file when a directory was expected.
    IsNotADirectory,
    /// Directory is not empty (for rmdir).
    DirectoryNotEmpty,
}

impl fmt::Display for Ext4Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ext4Error::IoError => write!(f, "I/O error"),
            Ext4Error::InvalidSuperblock => write!(f, "invalid superblock"),
            Ext4Error::UnsupportedFeature(feat) => write!(f, "unsupported feature: {}", feat),
            Ext4Error::NotFound => write!(f, "not found"),
            Ext4Error::NotADirectory => write!(f, "not a directory"),
            Ext4Error::AlreadyExists => write!(f, "already exists"),
            Ext4Error::NoFreeBlocks => write!(f, "no free blocks"),
            Ext4Error::NoFreeInodes => write!(f, "no free inodes"),
            Ext4Error::Corrupt(msg) => write!(f, "filesystem corrupt: {}", msg),
            Ext4Error::NameTooLong => write!(f, "filename too long"),
            Ext4Error::InvalidPath => write!(f, "invalid path"),
            Ext4Error::IsADirectory => write!(f, "is a directory"),
            Ext4Error::IsNotADirectory => write!(f, "is not a directory"),
            Ext4Error::DirectoryNotEmpty => write!(f, "directory not empty"),
        }
    }
}

/// Trait for the underlying block storage device.
///
/// Implement this for your NVMe driver, virtio-blk, RAM disk, or disk image
/// to provide ext4 with raw block access.
pub trait BlockDevice {
    /// Read `buf.len()` bytes from the device starting at `offset`.
    ///
    /// `offset` is a byte offset from the start of the partition.
    /// Returns `Ok(())` on success.
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<(), Ext4Error>;

    /// Write `buf.len()` bytes to the device starting at `offset`.
    ///
    /// `offset` is a byte offset from the start of the partition.
    /// Returns `Ok(())` on success.
    fn write_bytes(&self, offset: u64, buf: &[u8]) -> Result<(), Ext4Error>;

    /// Flush any cached writes to the underlying storage.
    ///
    /// Called after metadata updates (superblock, bitmaps, etc.) to ensure durability.
    fn flush(&self) -> Result<(), Ext4Error> {
        Ok(())
    }
}

/// Main ext4 filesystem handle.
///
/// Holds the parsed superblock, block group descriptors, and a reference to the
/// block device. All operations go through this struct.
pub struct Ext4Fs<D: BlockDevice> {
    /// The underlying block device.
    pub device: D,
    /// The parsed superblock.
    pub sb: Superblock,
    /// Block group descriptors, one per block group.
    pub groups: Vec<BlockGroupDesc>,
}

impl<D: BlockDevice> Ext4Fs<D> {
    /// Mount an ext4 filesystem from the given block device.
    ///
    /// Reads and validates the superblock, then loads all block group descriptors.
    pub fn mount(device: D) -> Result<Self, Ext4Error> {
        log::info!("[ext4::mount] mounting ext4 filesystem...");

        // Read superblock
        let mut sb_buf = vec![0u8; SUPERBLOCK_SIZE];
        device.read_bytes(SUPERBLOCK_OFFSET, &mut sb_buf)?;

        let sb = Superblock::from_bytes(&sb_buf).ok_or_else(|| {
            log::error!("[ext4::mount] failed to parse superblock");
            Ext4Error::InvalidSuperblock
        })?;

        log::info!("[ext4::mount] superblock valid: {} blocks, {} inodes, block_size={}, volume={:?}",
            sb.total_blocks(), sb.inodes_count, sb.block_size(), sb.volume_name_str());

        // Read block group descriptor table
        let bg_count = sb.block_group_count();
        let desc_size = sb.group_desc_size() as usize;
        let gdt_block = if sb.block_size() == 1024 { 2 } else { 1 };
        let gdt_offset = gdt_block as u64 * sb.block_size();
        let gdt_len = bg_count as usize * desc_size;

        log::debug!("[ext4::mount] reading {} block group descriptors from offset {} ({} bytes)",
            bg_count, gdt_offset, gdt_len);

        let mut gdt_buf = vec![0u8; gdt_len];
        device.read_bytes(gdt_offset, &mut gdt_buf)?;

        let groups = block_group::parse_block_group_table(&gdt_buf, bg_count, desc_size);
        if groups.len() != bg_count as usize {
            log::error!("[ext4::mount] expected {} block groups, parsed {}", bg_count, groups.len());
            return Err(Ext4Error::Corrupt("incomplete block group descriptor table"));
        }

        log::info!("[ext4::mount] mounted successfully: {} block groups", groups.len());

        Ok(Ext4Fs { device, sb, groups })
    }

    /// Read a block from the device.
    ///
    /// `block_num` is the absolute block number. Returns a Vec of `block_size` bytes.
    pub fn read_block(&self, block_num: u64) -> Result<Vec<u8>, Ext4Error> {
        let offset = block_num * self.sb.block_size();
        let mut buf = vec![0u8; self.sb.block_size() as usize];
        log::trace!("[ext4::io] reading block {} (offset={})", block_num, offset);
        self.device.read_bytes(offset, &mut buf)?;
        Ok(buf)
    }

    /// Write a block to the device.
    pub fn write_block(&self, block_num: u64, data: &[u8]) -> Result<(), Ext4Error> {
        let offset = block_num * self.sb.block_size();
        log::trace!("[ext4::io] writing block {} (offset={}, {} bytes)", block_num, offset, data.len());
        self.device.write_bytes(offset, data)?;
        Ok(())
    }

    /// Read an inode by its inode number (1-based).
    pub fn read_inode(&self, ino: u32) -> Result<Inode, Ext4Error> {
        if ino == 0 {
            log::error!("[ext4::inode] cannot read inode 0 (does not exist)");
            return Err(Ext4Error::NotFound);
        }

        let group = ((ino - 1) / self.sb.inodes_per_group) as usize;
        let index = ((ino - 1) % self.sb.inodes_per_group) as usize;

        if group >= self.groups.len() {
            log::error!("[ext4::inode] inode {} maps to group {} but only {} groups exist",
                ino, group, self.groups.len());
            return Err(Ext4Error::Corrupt("inode group out of range"));
        }

        let inode_table_block = self.groups[group].inode_table();
        let inode_size = self.sb.inode_size as u64;
        let offset = inode_table_block * self.sb.block_size() + index as u64 * inode_size;

        log::trace!("[ext4::inode] reading inode {}: group={}, index={}, offset={}",
            ino, group, index, offset);

        let mut buf = vec![0u8; inode_size as usize];
        self.device.read_bytes(offset, &mut buf)?;

        Inode::from_bytes(&buf, self.sb.inode_size as usize).ok_or_else(|| {
            log::error!("[ext4::inode] failed to parse inode {}", ino);
            Ext4Error::Corrupt("invalid inode data")
        })
    }

    /// Write an inode back to disk by its inode number (1-based).
    pub fn write_inode(&self, ino: u32, inode: &Inode) -> Result<(), Ext4Error> {
        if ino == 0 {
            return Err(Ext4Error::NotFound);
        }

        let group = ((ino - 1) / self.sb.inodes_per_group) as usize;
        let index = ((ino - 1) % self.sb.inodes_per_group) as usize;

        if group >= self.groups.len() {
            return Err(Ext4Error::Corrupt("inode group out of range"));
        }

        let inode_table_block = self.groups[group].inode_table();
        let inode_size = self.sb.inode_size as u64;
        let offset = inode_table_block * self.sb.block_size() + index as u64 * inode_size;

        log::trace!("[ext4::inode] writing inode {}: group={}, index={}, offset={}",
            ino, group, index, offset);

        let buf = inode.to_bytes(self.sb.inode_size as usize);
        self.device.write_bytes(offset, &buf)?;
        Ok(())
    }

    /// Resolve all data blocks for an inode using its extent tree.
    ///
    /// Returns a list of (logical_block, physical_block) mappings covering
    /// all data blocks of the file.
    pub fn resolve_extents(&self, inode: &Inode) -> Result<Vec<ExtentLeaf>, Ext4Error> {
        if !inode.uses_extents() {
            log::error!("[ext4::extent] inode does not use extents (legacy block map not yet supported)");
            return Err(Ext4Error::UnsupportedFeature("legacy block map"));
        }

        let header = inode.extent_header().ok_or_else(|| {
            log::error!("[ext4::extent] failed to parse extent header from inode");
            Ext4Error::Corrupt("invalid extent header")
        })?;

        log::debug!("[ext4::extent] resolving extent tree: depth={}, entries={}",
            header.depth, header.entries);

        if header.is_leaf() {
            // Leaf node directly in the inode
            let leaves = extent::parse_leaves(&inode.i_block);
            log::debug!("[ext4::extent] resolved {} leaf extents from inode root", leaves.len());
            return Ok(leaves);
        }

        // Internal node: need to traverse the tree
        self.resolve_extent_tree_recursive(&inode.i_block, header.depth)
    }

    /// Recursively traverse the extent tree, reading child nodes from disk.
    fn resolve_extent_tree_recursive(&self, node_buf: &[u8], depth: u16) -> Result<Vec<ExtentLeaf>, Ext4Error> {
        if depth == 0 {
            return Ok(extent::parse_leaves(node_buf));
        }

        let indices = extent::parse_indices(node_buf);
        let mut all_leaves = Vec::new();

        for idx in &indices {
            let child_block = idx.physical_block();
            log::trace!("[ext4::extent] traversing index -> physical block {}", child_block);
            let child_data = self.read_block(child_block)?;
            let child_leaves = self.resolve_extent_tree_recursive(&child_data, depth - 1)?;
            all_leaves.extend(child_leaves);
        }

        log::debug!("[ext4::extent] resolved {} leaves from depth-{} subtree", all_leaves.len(), depth);
        Ok(all_leaves)
    }

    /// Read all data blocks of an inode into a contiguous Vec.
    ///
    /// The returned Vec is truncated to the inode's actual file size.
    pub fn read_inode_data(&self, inode: &Inode) -> Result<Vec<u8>, Ext4Error> {
        let file_size = inode.size();
        if file_size == 0 {
            log::trace!("[ext4::read] inode has zero size");
            return Ok(Vec::new());
        }

        let extents = self.resolve_extents(inode)?;
        let block_size = self.sb.block_size();
        let total_blocks = ((file_size + block_size - 1) / block_size) as u32;

        log::debug!("[ext4::read] reading {} bytes ({} blocks, {} extents)",
            file_size, total_blocks, extents.len());

        let mut data = Vec::with_capacity(file_size as usize);

        for logical_block in 0..total_blocks {
            let phys = extent::find_leaf_for_block(&extents, logical_block)
                .and_then(|leaf| leaf.map_block(logical_block))
                .ok_or_else(|| {
                    log::error!("[ext4::read] no extent mapping for logical block {}", logical_block);
                    Ext4Error::Corrupt("missing extent for logical block")
                })?;

            let block_data = self.read_block(phys)?;
            data.extend_from_slice(&block_data);
        }

        // Truncate to actual file size
        data.truncate(file_size as usize);
        log::debug!("[ext4::read] read {} bytes of inode data", data.len());
        Ok(data)
    }

    /// Look up a path component by component, starting from the root inode.
    ///
    /// Returns the inode number and parsed Inode for the target.
    /// Path must start with '/'.
    pub fn lookup_path(&self, path: &[u8]) -> Result<(u32, Inode), Ext4Error> {
        if path.is_empty() || path[0] != b'/' {
            log::error!("[ext4::lookup] invalid path (must start with '/'): {:?}",
                core::str::from_utf8(path).unwrap_or("<invalid>"));
            return Err(Ext4Error::InvalidPath);
        }

        log::debug!("[ext4::lookup] resolving path: {:?}",
            core::str::from_utf8(path).unwrap_or("<invalid>"));

        let mut current_ino = ROOT_INODE;
        let mut current_inode = self.read_inode(ROOT_INODE)?;

        // Split path and iterate components (skip leading '/' and empty segments)
        let components: Vec<&[u8]> = path[1..]
            .split(|&b| b == b'/')
            .filter(|c| !c.is_empty())
            .collect();

        if components.is_empty() {
            // Root directory
            log::debug!("[ext4::lookup] resolved to root inode {}", ROOT_INODE);
            return Ok((ROOT_INODE, current_inode));
        }

        for component in components.iter() {
            if !current_inode.is_dir() {
                log::error!("[ext4::lookup] inode {} is not a directory at component {:?}",
                    current_ino, core::str::from_utf8(component).unwrap_or("<invalid>"));
                return Err(Ext4Error::NotADirectory);
            }

            log::trace!("[ext4::lookup] searching directory inode {} for {:?}",
                current_ino, core::str::from_utf8(component).unwrap_or("<invalid>"));

            let (found_ino, found_inode) = self.lookup_in_dir(&current_inode, component)?;
            current_ino = found_ino;
            current_inode = found_inode;

            log::trace!("[ext4::lookup] component {:?} -> inode {}",
                core::str::from_utf8(component).unwrap_or("<invalid>"), current_ino);
        }

        log::debug!("[ext4::lookup] resolved path -> inode {}", current_ino);
        Ok((current_ino, current_inode))
    }

    /// Search a directory inode for a name.
    ///
    /// Returns the inode number and parsed Inode of the found entry.
    fn lookup_in_dir(&self, dir_inode: &Inode, name: &[u8]) -> Result<(u32, Inode), Ext4Error> {
        let extents = self.resolve_extents(dir_inode)?;

        for ext in &extents {
            for blk_offset in 0..ext.block_count() {
                let phys = ext.physical_start() + blk_offset as u64;
                let block_data = self.read_block(phys)?;

                if let Some((_offset, entry)) = dir::lookup_in_block(&block_data, name) {
                    let ino = entry.inode;
                    let inode = self.read_inode(ino)?;
                    return Ok((ino, inode));
                }
            }
        }

        log::trace!("[ext4::lookup] {:?} not found in directory",
            core::str::from_utf8(name).unwrap_or("<invalid>"));
        Err(Ext4Error::NotFound)
    }

    /// Read a file by its absolute path.
    ///
    /// Returns the file contents as a Vec<u8>.
    pub fn read_file(&self, path: &[u8]) -> Result<Vec<u8>, Ext4Error> {
        log::info!("[ext4::read_file] reading {:?}",
            core::str::from_utf8(path).unwrap_or("<invalid>"));

        let (_ino, inode) = self.lookup_path(path)?;

        if inode.is_dir() {
            log::error!("[ext4::read_file] path is a directory, not a file");
            return Err(Ext4Error::IsADirectory);
        }

        let data = self.read_inode_data(&inode)?;
        log::info!("[ext4::read_file] read {} bytes from {:?}",
            data.len(), core::str::from_utf8(path).unwrap_or("<invalid>"));
        Ok(data)
    }

    /// List directory entries at the given path.
    pub fn list_dir(&self, path: &[u8]) -> Result<Vec<DirEntry>, Ext4Error> {
        log::info!("[ext4::list_dir] listing {:?}",
            core::str::from_utf8(path).unwrap_or("<invalid>"));

        let (_ino, inode) = self.lookup_path(path)?;

        if !inode.is_dir() {
            return Err(Ext4Error::IsNotADirectory);
        }

        let extents = self.resolve_extents(&inode)?;
        let mut entries = Vec::new();

        for ext in &extents {
            for blk_offset in 0..ext.block_count() {
                let phys = ext.physical_start() + blk_offset as u64;
                let block_data = self.read_block(phys)?;

                for (_offset, entry) in DirEntryIter::new(&block_data) {
                    if !entry.is_deleted() {
                        entries.push(entry);
                    }
                }
            }
        }

        log::info!("[ext4::list_dir] found {} entries", entries.len());
        Ok(entries)
    }

    // --- Write operations ---

    /// Read the block bitmap for a block group.
    fn read_block_bitmap(&self, group: usize) -> Result<Vec<u8>, Ext4Error> {
        let bitmap_block = self.groups[group].block_bitmap();
        log::trace!("[ext4::bitmap] reading block bitmap for group {} (block {})", group, bitmap_block);
        self.read_block(bitmap_block)
    }

    /// Write the block bitmap for a block group.
    fn write_block_bitmap(&self, group: usize, bitmap: &[u8]) -> Result<(), Ext4Error> {
        let bitmap_block = self.groups[group].block_bitmap();
        log::trace!("[ext4::bitmap] writing block bitmap for group {} (block {})", group, bitmap_block);
        self.write_block(bitmap_block, bitmap)
    }

    /// Read the inode bitmap for a block group.
    fn read_inode_bitmap(&self, group: usize) -> Result<Vec<u8>, Ext4Error> {
        let bitmap_block = self.groups[group].inode_bitmap();
        log::trace!("[ext4::bitmap] reading inode bitmap for group {} (block {})", group, bitmap_block);
        self.read_block(bitmap_block)
    }

    /// Write the inode bitmap for a block group.
    fn write_inode_bitmap(&self, group: usize, bitmap: &[u8]) -> Result<(), Ext4Error> {
        let bitmap_block = self.groups[group].inode_bitmap();
        log::trace!("[ext4::bitmap] writing inode bitmap for group {} (block {})", group, bitmap_block);
        self.write_block(bitmap_block, bitmap)
    }

    /// Allocate a single block from any block group.
    ///
    /// Returns the absolute block number. Prefers the given `preferred_group` if it
    /// has free blocks.
    pub fn allocate_block(&mut self, preferred_group: usize) -> Result<u64, Ext4Error> {
        log::debug!("[ext4::alloc] allocating block (preferred group={})", preferred_group);

        let num_groups = self.groups.len();
        for offset in 0..num_groups {
            let group = (preferred_group + offset) % num_groups;

            if self.groups[group].free_blocks_count() == 0 {
                continue;
            }

            let mut bitmap = self.read_block_bitmap(group)?;
            let total_bits = self.sb.blocks_per_group;

            if let Some(bit) = BitmapAllocator::allocate_one(&mut bitmap, 0, total_bits) {
                self.write_block_bitmap(group, &bitmap)?;

                // Update block group free count
                let new_count = self.groups[group].free_blocks_count().saturating_sub(1);
                self.groups[group].set_free_blocks_count(new_count);

                // Update superblock free count
                let sb_free = self.sb.free_blocks().saturating_sub(1);
                self.sb.free_blocks_count_lo = sb_free as u32;
                self.sb.free_blocks_count_hi = (sb_free >> 32) as u32;

                let abs_block = group as u64 * self.sb.blocks_per_group as u64
                    + self.sb.first_data_block as u64
                    + bit as u64;

                log::info!("[ext4::alloc] allocated block {} (group={}, bit={})", abs_block, group, bit);
                return Ok(abs_block);
            }
        }

        log::error!("[ext4::alloc] no free blocks in any group");
        Err(Ext4Error::NoFreeBlocks)
    }

    /// Allocate a new inode from any block group.
    ///
    /// Returns the new inode number (1-based).
    pub fn allocate_inode(&mut self, preferred_group: usize) -> Result<u32, Ext4Error> {
        log::debug!("[ext4::alloc] allocating inode (preferred group={})", preferred_group);

        let num_groups = self.groups.len();
        for offset in 0..num_groups {
            let group = (preferred_group + offset) % num_groups;

            if self.groups[group].free_inodes_count() == 0 {
                continue;
            }

            let mut bitmap = self.read_inode_bitmap(group)?;
            let total_bits = self.sb.inodes_per_group;

            if let Some(bit) = BitmapAllocator::allocate_one(&mut bitmap, 0, total_bits) {
                self.write_inode_bitmap(group, &bitmap)?;

                // Update block group free count
                let new_count = self.groups[group].free_inodes_count().saturating_sub(1);
                self.groups[group].set_free_inodes_count(new_count);

                // Update superblock free count
                self.sb.free_inodes_count = self.sb.free_inodes_count.saturating_sub(1);

                let ino = group as u32 * self.sb.inodes_per_group + bit + 1;

                log::info!("[ext4::alloc] allocated inode {} (group={}, bit={})", ino, group, bit);
                return Ok(ino);
            }
        }

        log::error!("[ext4::alloc] no free inodes in any group");
        Err(Ext4Error::NoFreeInodes)
    }

    /// Add a directory entry to a directory inode.
    ///
    /// Searches existing blocks for free space; allocates a new block if needed.
    fn add_dir_entry(&mut self, dir_ino: u32, dir_inode: &mut Inode, entry: &DirEntry) -> Result<(), Ext4Error> {
        let needed = entry.actual_size();
        log::debug!("[ext4::dir] adding entry {:?} (inode={}) to dir inode {}, needs {} bytes",
            entry.name_str(), entry.inode, dir_ino, needed);

        let extents = self.resolve_extents(dir_inode)?;

        // Try to find space in existing blocks
        for ext in &extents {
            for blk_offset in 0..ext.block_count() {
                let phys = ext.physical_start() + blk_offset as u64;
                let block_data = self.read_block(phys)?;

                if let Some((insert_offset, available)) = dir::find_space_in_block(&block_data, needed) {
                    // Found space: insert the entry
                    let mut modified_block = block_data;

                    // If inserting at a split point (after an existing entry), update the
                    // previous entry's rec_len to its actual size first
                    if insert_offset > 0 {
                            // The previous entry's rec_len was already accounted for by
                        // find_space_in_block, which returns the offset after the
                        // previous entry's actual data.
                    }

                    let mut new_entry = entry.clone();
                    new_entry.rec_len = available;
                    let entry_bytes = new_entry.to_bytes();
                    let end = insert_offset + entry_bytes.len();
                    if end <= modified_block.len() {
                        modified_block[insert_offset..end].copy_from_slice(&entry_bytes);
                        self.write_block(phys, &modified_block)?;
                        log::info!("[ext4::dir] inserted entry at offset {} in block {}", insert_offset, phys);
                        return Ok(());
                    }
                }
            }
        }

        // No space in existing blocks: allocate a new block
        log::debug!("[ext4::dir] no space in existing blocks, allocating new block for directory");
        let dir_group = ((dir_ino - 1) / self.sb.inodes_per_group) as usize;
        let new_block = self.allocate_block(dir_group)?;

        // Create new block with just this entry, rec_len covering the whole block
        let block_size = self.sb.block_size() as u16;
        let mut new_entry = entry.clone();
        new_entry.rec_len = block_size;
        let entry_bytes = new_entry.to_bytes();

        let mut block_buf = vec![0u8; self.sb.block_size() as usize];
        block_buf[..entry_bytes.len()].copy_from_slice(&entry_bytes);
        self.write_block(new_block, &block_buf)?;

        // Update the directory inode's extent tree to include the new block
        self.append_extent(dir_ino, dir_inode, new_block)?;

        // Update directory size
        let new_size = dir_inode.size() + self.sb.block_size();
        dir_inode.set_size(new_size);
        self.write_inode(dir_ino, dir_inode)?;

        log::info!("[ext4::dir] added entry in new block {}", new_block);
        Ok(())
    }

    /// Append a new physical block to an inode's extent tree.
    ///
    /// This is a simplified implementation that adds a new single-block extent.
    /// A production implementation would merge with adjacent extents.
    fn append_extent(&self, ino: u32, inode: &mut Inode, phys_block: u64) -> Result<(), Ext4Error> {
        let header = inode.extent_header().ok_or(Ext4Error::Corrupt("no extent header"))?;

        if !header.is_leaf() {
            log::error!("[ext4::extent] cannot append to non-leaf root (multi-level trees not yet supported for append)");
            return Err(Ext4Error::UnsupportedFeature("multi-level extent tree append"));
        }

        if header.entries >= header.max {
            log::error!("[ext4::extent] root extent node is full ({}/{}), tree splitting not yet implemented",
                header.entries, header.max);
            return Err(Ext4Error::UnsupportedFeature("extent tree splitting"));
        }

        // Calculate the next logical block number
        let leaves = extent::parse_leaves(&inode.i_block);
        let next_logical = leaves.iter()
            .map(|l| l.block + l.block_count())
            .max()
            .unwrap_or(0);

        // Create new leaf extent
        let new_leaf = ExtentLeaf {
            block: next_logical,
            len: 1,
            start_hi: (phys_block >> 32) as u16,
            start_lo: phys_block as u32,
        };
        let leaf_bytes = new_leaf.to_bytes();

        // Write the new leaf after existing entries
        let entry_offset = EXTENT_HEADER_SIZE + header.entries as usize * EXTENT_LEAF_SIZE;
        inode.i_block[entry_offset..entry_offset + EXTENT_LEAF_SIZE].copy_from_slice(&leaf_bytes);

        // Update header entries count
        let new_entries = header.entries + 1;
        inode.i_block[2] = new_entries as u8;
        inode.i_block[3] = (new_entries >> 8) as u8;

        self.write_inode(ino, inode)?;
        log::debug!("[ext4::extent] appended extent: logical={}, phys={}", next_logical, phys_block);
        Ok(())
    }

    /// Write a file at the given absolute path.
    ///
    /// If the file exists, its contents are replaced. If it does not exist, it is created.
    /// Parent directories must already exist.
    pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<(), Ext4Error> {
        log::info!("[ext4::write_file] writing {} bytes to {:?}",
            data.len(), core::str::from_utf8(path).unwrap_or("<invalid>"));

        // Split into parent path and filename
        let (parent_path, filename) = split_path(path)?;

        // Resolve parent directory
        let (parent_ino, mut parent_inode) = self.lookup_path(parent_path)?;
        if !parent_inode.is_dir() {
            return Err(Ext4Error::NotADirectory);
        }

        // Check if file already exists
        let existing = self.lookup_in_dir(&parent_inode, filename);
        let (file_ino, mut file_inode) = match existing {
            Ok((ino, inode)) => {
                if inode.is_dir() {
                    return Err(Ext4Error::IsADirectory);
                }
                log::debug!("[ext4::write_file] overwriting existing file at inode {}", ino);
                // TODO: free old blocks before rewriting
                (ino, inode)
            }
            Err(Ext4Error::NotFound) => {
                // Create new inode
                let group = ((parent_ino - 1) / self.sb.inodes_per_group) as usize;
                let new_ino = self.allocate_inode(group)?;
                let new_inode = Inode::new_file(0o644, 0, 0, 0);
                self.write_inode(new_ino, &new_inode)?;

                // Add directory entry
                let entry = DirEntry::new(new_ino, filename, FT_REG_FILE, 0);
                self.add_dir_entry(parent_ino, &mut parent_inode, &entry)?;

                log::info!("[ext4::write_file] created new file inode {}", new_ino);
                (new_ino, new_inode)
            }
            Err(e) => return Err(e),
        };

        // Allocate blocks and write data
        let block_size = self.sb.block_size() as usize;
        let blocks_needed = (data.len() + block_size - 1) / block_size;
        let group = ((file_ino - 1) / self.sb.inodes_per_group) as usize;

        // Reset the inode's extent tree for fresh write
        file_inode.flags |= inode::EXT4_EXTENTS_FL;
        file_inode.i_block = [0u8; inode::I_BLOCK_SIZE];
        let header = ExtentHeader {
            magic: extent::EXT4_EXTENT_MAGIC,
            entries: 0,
            max: extent::ROOT_MAX_ENTRIES,
            depth: 0,
            generation: 0,
        };
        let hdr_bytes = header.to_bytes();
        file_inode.i_block[..EXTENT_HEADER_SIZE].copy_from_slice(&hdr_bytes);

        log::debug!("[ext4::write_file] writing {} blocks of data", blocks_needed);

        for i in 0..blocks_needed {
            let phys_block = self.allocate_block(group)?;
            let start = i * block_size;
            let end = core::cmp::min(start + block_size, data.len());

            let mut block_buf = vec![0u8; block_size];
            block_buf[..end - start].copy_from_slice(&data[start..end]);
            self.write_block(phys_block, &block_buf)?;

            self.append_extent(file_ino, &mut file_inode, phys_block)?;
        }

        // Update file size and write inode
        file_inode.set_size(data.len() as u64);
        file_inode.blocks_lo = (blocks_needed as u32) * (self.sb.block_size() as u32 / 512);
        self.write_inode(file_ino, &file_inode)?;

        // Flush to ensure durability
        self.device.flush()?;

        log::info!("[ext4::write_file] wrote {} bytes to inode {} ({} blocks)",
            data.len(), file_ino, blocks_needed);
        Ok(())
    }

    /// Create a new directory at the given absolute path.
    ///
    /// Parent directories must already exist. The new directory is created with
    /// "." and ".." entries.
    pub fn mkdir(&mut self, path: &[u8]) -> Result<u32, Ext4Error> {
        log::info!("[ext4::mkdir] creating directory {:?}",
            core::str::from_utf8(path).unwrap_or("<invalid>"));

        let (parent_path, dirname) = split_path(path)?;

        // Resolve parent
        let (parent_ino, mut parent_inode) = self.lookup_path(parent_path)?;
        if !parent_inode.is_dir() {
            return Err(Ext4Error::NotADirectory);
        }

        // Check for existing entry
        if self.lookup_in_dir(&parent_inode, dirname).is_ok() {
            return Err(Ext4Error::AlreadyExists);
        }

        // Allocate inode
        let group = ((parent_ino - 1) / self.sb.inodes_per_group) as usize;
        let new_ino = self.allocate_inode(group)?;

        // Create directory inode
        let mut new_inode = Inode::new_dir(0o755, 0, 0, 0);

        // Allocate a block for . and .. entries
        let data_block = self.allocate_block(group)?;
        let dot_data = dir::create_dot_entries(new_ino, parent_ino, self.sb.block_size() as u32);
        self.write_block(data_block, &dot_data)?;

        // Set up the extent tree with one extent pointing to the dot-entries block
        self.append_extent(new_ino, &mut new_inode, data_block)?;
        new_inode.set_size(self.sb.block_size());
        new_inode.blocks_lo = (self.sb.block_size() / 512) as u32;
        self.write_inode(new_ino, &new_inode)?;

        // Add entry in parent directory
        let entry = DirEntry::new(new_ino, dirname, FT_DIR, 0);
        self.add_dir_entry(parent_ino, &mut parent_inode, &entry)?;

        // Increment parent link count (for the ".." entry pointing back)
        parent_inode.links_count = parent_inode.links_count.saturating_add(1);
        self.write_inode(parent_ino, &parent_inode)?;

        // Update block group used_dirs count
        if group < self.groups.len() {
            let old = self.groups[group].used_dirs_count();
            self.groups[group].used_dirs_count_lo = (old + 1) as u16;
            self.groups[group].used_dirs_count_hi = ((old + 1) >> 16) as u16;
        }

        self.device.flush()?;

        log::info!("[ext4::mkdir] created directory inode {} in parent {}", new_ino, parent_ino);
        Ok(new_ino)
    }

    /// Write the superblock and block group descriptors back to disk.
    ///
    /// Call this after any metadata-modifying operation to persist the state.
    pub fn sync_metadata(&self) -> Result<(), Ext4Error> {
        log::info!("[ext4::sync] writing superblock and block group descriptors to disk");

        // Write superblock
        let sb_bytes = self.sb.to_bytes();
        self.device.write_bytes(SUPERBLOCK_OFFSET, &sb_bytes)?;

        // Write block group descriptor table
        let desc_size = self.sb.group_desc_size() as usize;
        let gdt_block = if self.sb.block_size() == 1024 { 2 } else { 1 };
        let gdt_offset = gdt_block as u64 * self.sb.block_size();

        for (i, group) in self.groups.iter().enumerate() {
            let offset = gdt_offset + (i * desc_size) as u64;
            let bytes = group.to_bytes(desc_size);
            self.device.write_bytes(offset, &bytes)?;
        }

        self.device.flush()?;
        log::info!("[ext4::sync] metadata sync complete");
        Ok(())
    }
}

/// Split an absolute path into (parent_directory, filename).
///
/// e.g., b"/foo/bar/baz.txt" -> (b"/foo/bar", b"baz.txt")
/// e.g., b"/file.txt" -> (b"/", b"file.txt")
fn split_path(path: &[u8]) -> Result<(&[u8], &[u8]), Ext4Error> {
    if path.is_empty() || path[0] != b'/' {
        return Err(Ext4Error::InvalidPath);
    }

    // Find last '/'
    let last_slash = path.iter().rposition(|&b| b == b'/').ok_or(Ext4Error::InvalidPath)?;
    let filename = &path[last_slash + 1..];
    if filename.is_empty() {
        return Err(Ext4Error::InvalidPath);
    }
    if filename.len() > dir::EXT4_NAME_LEN {
        return Err(Ext4Error::NameTooLong);
    }

    let parent = if last_slash == 0 { &path[..1] } else { &path[..last_slash] };

    log::trace!("[ext4::path] split: parent={:?}, name={:?}",
        core::str::from_utf8(parent).unwrap_or("<invalid>"),
        core::str::from_utf8(filename).unwrap_or("<invalid>"));

    Ok((parent, filename))
}
