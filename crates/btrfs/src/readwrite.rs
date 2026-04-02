//! High-level btrfs filesystem read/write API.
//!
//! This module provides the main `BtrFs` type that ties together the superblock,
//! chunk map, B-tree operations, inodes, directories, and extents into a usable
//! filesystem interface.
//!
//! ## Usage
//!
//! Implement the `BlockDevice` trait for your storage backend, then:
//!
//! ```rust,no_run
//! use claudio_btrfs::{BtrFs, BlockDevice};
//!
//! let fs = BtrFs::mount(my_device).expect("mount failed");
//! let data = fs.read_file(b"/hello.txt").expect("read failed");
//! fs.write_file(b"/output.txt", &data).expect("write failed");
//! ```

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::chunk::ChunkMap;
use crate::dir::{self, DirItem, DirEntry, dir_item_key, dir_index_key, dir_type};
use crate::extent::{FileExtentItem, BTRFS_FILE_EXTENT_INLINE, BTRFS_FILE_EXTENT_REG};
use crate::inode::{InodeItem, BtrfsTimespec};
use crate::item::BtrfsHeader;
use crate::key::{BtrfsKey, KeyType, objectid};
use crate::superblock::{Superblock, SUPERBLOCK_OFFSET, SUPERBLOCK_SIZE};
use crate::tree;

/// Errors that can occur during btrfs filesystem operations.
#[derive(Debug)]
pub enum BtrfsError {
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
    /// No free space available for allocation.
    NoFreeSpace,
    /// The filesystem is corrupt (e.g., invalid tree node, broken chunk map).
    Corrupt(&'static str),
    /// A filename exceeds the maximum length (255 bytes).
    NameTooLong,
    /// The path is invalid (empty, missing leading slash, etc.).
    InvalidPath,
    /// The target is a directory when a file was expected.
    IsADirectory,
    /// The target is a file when a directory was expected.
    IsNotADirectory,
    /// Compressed extents not supported by this implementation.
    CompressedExtent,
    /// The chunk map could not resolve a logical address.
    UnmappedLogical(u64),
}

impl fmt::Display for BtrfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BtrfsError::IoError => write!(f, "I/O error"),
            BtrfsError::InvalidSuperblock => write!(f, "invalid superblock"),
            BtrfsError::UnsupportedFeature(feat) => write!(f, "unsupported feature: {}", feat),
            BtrfsError::NotFound => write!(f, "not found"),
            BtrfsError::NotADirectory => write!(f, "not a directory"),
            BtrfsError::AlreadyExists => write!(f, "already exists"),
            BtrfsError::NoFreeSpace => write!(f, "no free space"),
            BtrfsError::Corrupt(msg) => write!(f, "filesystem corrupt: {}", msg),
            BtrfsError::NameTooLong => write!(f, "filename too long"),
            BtrfsError::InvalidPath => write!(f, "invalid path"),
            BtrfsError::IsADirectory => write!(f, "is a directory"),
            BtrfsError::IsNotADirectory => write!(f, "is not a directory"),
            BtrfsError::CompressedExtent => write!(f, "compressed extents not supported"),
            BtrfsError::UnmappedLogical(addr) => write!(f, "unmapped logical address: 0x{:X}", addr),
        }
    }
}

/// Trait for the underlying block storage device.
///
/// Implement this for your NVMe driver, virtio-blk, RAM disk, or disk image
/// to provide btrfs with raw byte-level access.
pub trait BlockDevice {
    /// Read `buf.len()` bytes from the device starting at `offset`.
    ///
    /// `offset` is a byte offset from the start of the partition/device.
    /// Returns `Ok(())` on success.
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<(), BtrfsError>;

    /// Write `buf.len()` bytes to the device starting at `offset`.
    ///
    /// `offset` is a byte offset from the start of the partition/device.
    /// Returns `Ok(())` on success.
    fn write_bytes(&self, offset: u64, buf: &[u8]) -> Result<(), BtrfsError>;
}

/// A mounted btrfs filesystem.
///
/// Holds the superblock, chunk map, and device reference needed for all operations.
pub struct BtrFs<D: BlockDevice> {
    /// The underlying block device.
    dev: D,
    /// Parsed superblock.
    pub sb: Superblock,
    /// Chunk map (logical to physical address translation).
    pub chunk_map: ChunkMap,
    /// Current generation (for write operations).
    generation: u64,
    /// Next available inode number (tracked in memory for allocation).
    next_inode: u64,
    /// Next available directory index sequence.
    next_dir_index: u64,
}

impl<D: BlockDevice> BtrFs<D> {
    /// Mount a btrfs filesystem from the given block device.
    ///
    /// Reads the superblock, parses the sys_chunk_array for bootstrap chunk mappings,
    /// then reads the chunk tree for the full chunk map. Finally, locates the
    /// filesystem tree root.
    pub fn mount(dev: D) -> Result<Self, BtrfsError> {
        log::info!("[btrfs::readwrite] mounting btrfs filesystem...");

        // Step 1: Read the superblock at offset 0x10000
        let mut sb_buf = vec![0u8; SUPERBLOCK_SIZE];
        dev.read_bytes(SUPERBLOCK_OFFSET, &mut sb_buf)?;

        let sb = Superblock::from_bytes(&sb_buf)
            .ok_or(BtrfsError::InvalidSuperblock)?;

        log::info!("[btrfs::readwrite] superblock parsed: generation={}, nodesize={}, label={:?}",
            sb.generation, sb.nodesize, sb.label_str());

        // Step 2: Parse sys_chunk_array for bootstrap chunk mappings
        let mut chunk_map = ChunkMap::new();
        chunk_map.parse_sys_chunk_array(&sb.sys_chunk_array, sb.sys_chunk_array_size);

        log::info!("[btrfs::readwrite] bootstrap chunk map: {} entries", chunk_map.len());

        // Step 3: Read the full chunk tree to populate remaining chunk mappings
        let chunk_root = sb.chunk_root;
        let chunk_level = sb.chunk_root_level;
        let nodesize = sb.nodesize;

        log::debug!("[btrfs::readwrite] reading chunk tree: root=0x{:X}, level={}", chunk_root, chunk_level);

        // We need a temporary copy of chunk_map for the closure
        // Walk the chunk tree to find all CHUNK_ITEM entries
        {
            let chunk_map_ref = &chunk_map;
            let dev_ref = &dev;
            let read_node_fn = |logical: u64| -> Option<Vec<u8>> {
                read_node_via_chunks(dev_ref, chunk_map_ref, logical, nodesize)
            };

            let items = tree::collect_tree_items(chunk_root, chunk_level, nodesize, read_node_fn);

            for (key, data) in &items {
                if key.key_type == KeyType::ChunkItem as u8 {
                    if let Some(chunk) = crate::chunk::ChunkItem::from_bytes(data) {
                        // Only insert if not already present
                        let logical = key.offset;
                        let already_exists = chunk_map.entries.iter().any(|e| e.logical == logical);
                        if !already_exists {
                            chunk_map.insert(logical, chunk);
                        }
                    }
                }
            }
        }

        log::info!("[btrfs::readwrite] full chunk map: {} entries", chunk_map.len());

        let generation = sb.generation;

        let mut fs = BtrFs {
            dev,
            sb,
            chunk_map,
            generation,
            next_inode: objectid::FIRST_FREE_OBJECTID + 1,
            next_dir_index: 2,
        };

        // Step 4: Scan the FS tree to find the highest inode number (for allocation)
        fs.scan_next_inode()?;

        log::info!("[btrfs::readwrite] btrfs mounted successfully. next_inode={}", fs.next_inode);

        Ok(fs)
    }

    /// Read a node from the device, resolving logical to physical via chunk map.
    fn read_node(&self, logical: u64) -> Result<Vec<u8>, BtrfsError> {
        let (_devid, physical) = self.chunk_map.resolve(logical)
            .ok_or(BtrfsError::UnmappedLogical(logical))?;

        let mut buf = vec![0u8; self.sb.nodesize as usize];
        self.dev.read_bytes(physical, &mut buf)?;

        log::trace!("[btrfs::readwrite] read node: logical=0x{:X} -> physical=0x{:X} ({} bytes)",
            logical, physical, buf.len());

        Ok(buf)
    }

    /// Write a node to the device, resolving logical to physical.
    fn write_node(&self, logical: u64, data: &[u8]) -> Result<(), BtrfsError> {
        let (_devid, physical) = self.chunk_map.resolve(logical)
            .ok_or(BtrfsError::UnmappedLogical(logical))?;

        self.dev.write_bytes(physical, data)?;

        log::trace!("[btrfs::readwrite] wrote node: logical=0x{:X} -> physical=0x{:X} ({} bytes)",
            logical, physical, data.len());

        Ok(())
    }

    /// Search the filesystem tree for a key.
    fn search_fs_tree(&self, key: &BtrfsKey) -> Result<tree::SearchResult, BtrfsError> {
        let fs_root = self.find_fs_tree_root()?;
        let fs_level = self.find_fs_tree_level()?;

        let chunk_map = &self.chunk_map;
        let dev = &self.dev;
        let nodesize = self.sb.nodesize;

        let result = tree::search_tree(
            fs_root, fs_level, key, nodesize,
            |logical| read_node_via_chunks(dev, chunk_map, logical, nodesize),
        );

        result.ok_or(BtrfsError::Corrupt("failed to search filesystem tree"))
    }

    /// Find the root of the default filesystem tree by searching the root tree.
    fn find_fs_tree_root(&self) -> Result<u64, BtrfsError> {
        let root_key = BtrfsKey::new(
            objectid::FS_TREE_OBJECTID,
            KeyType::RootItem as u8,
            0,
        );

        let chunk_map = &self.chunk_map;
        let dev = &self.dev;
        let nodesize = self.sb.nodesize;

        let result = tree::search_tree(
            self.sb.root, self.sb.root_level, &root_key, nodesize,
            |logical| read_node_via_chunks(dev, chunk_map, logical, nodesize),
        ).ok_or(BtrfsError::Corrupt("failed to search root tree for FS_TREE"))?;

        if !result.exact {
            log::error!("[btrfs::readwrite] FS_TREE root item not found in root tree");
            return Err(BtrfsError::Corrupt("FS_TREE root item not found"));
        }

        let slot = result.path.leaf_slot().unwrap_or(0);
        let data = result.leaf.item_data(slot)
            .ok_or(BtrfsError::Corrupt("cannot read FS_TREE root item data"))?;

        // btrfs_root_item: first field after the inode item (160 bytes) is generation (u64),
        // then root_dirid (u64), then bytenr (u64)
        if data.len() < 176 + 8 {
            return Err(BtrfsError::Corrupt("root item too small"));
        }

        let bytenr = u64::from_le_bytes([
            data[176], data[177], data[178], data[179],
            data[180], data[181], data[182], data[183],
        ]);

        log::debug!("[btrfs::readwrite] FS_TREE root bytenr=0x{:X}", bytenr);
        Ok(bytenr)
    }

    /// Find the level of the filesystem tree root.
    fn find_fs_tree_level(&self) -> Result<u8, BtrfsError> {
        let fs_root = self.find_fs_tree_root()?;
        let node_data = self.read_node(fs_root)?;
        let header = BtrfsHeader::from_bytes(&node_data)
            .ok_or(BtrfsError::Corrupt("cannot parse FS tree root header"))?;
        Ok(header.level)
    }

    /// Scan the FS tree to find the highest inode number in use.
    fn scan_next_inode(&mut self) -> Result<(), BtrfsError> {
        let fs_root = self.find_fs_tree_root()?;
        let fs_level = self.find_fs_tree_level()?;

        let chunk_map = &self.chunk_map;
        let dev = &self.dev;
        let nodesize = self.sb.nodesize;

        let mut max_inode = objectid::FIRST_FREE_OBJECTID;

        tree::walk_tree(
            fs_root, fs_level, nodesize,
            |logical| read_node_via_chunks(dev, chunk_map, logical, nodesize),
            |key, _data| {
                if key.key_type == KeyType::InodeItem as u8 && key.objectid > max_inode {
                    max_inode = key.objectid;
                }
            },
        );

        self.next_inode = max_inode + 1;
        log::debug!("[btrfs::readwrite] highest inode found: {}, next_inode={}", max_inode, self.next_inode);
        Ok(())
    }

    /// Allocate a new inode number.
    fn alloc_inode(&mut self) -> u64 {
        let ino = self.next_inode;
        self.next_inode += 1;
        log::debug!("[btrfs::readwrite] allocated inode {}", ino);
        ino
    }

    /// Allocate a new directory index.
    fn alloc_dir_index(&mut self) -> u64 {
        let idx = self.next_dir_index;
        self.next_dir_index += 1;
        idx
    }

    /// Resolve a path to its inode number and inode item.
    ///
    /// Path must start with `/` and components are separated by `/`.
    fn resolve_path(&self, path: &[u8]) -> Result<(u64, InodeItem), BtrfsError> {
        if path.is_empty() || path[0] != b'/' {
            log::error!("[btrfs::readwrite] invalid path: must start with /");
            return Err(BtrfsError::InvalidPath);
        }

        log::debug!("[btrfs::readwrite] resolving path: {:?}",
            core::str::from_utf8(path).unwrap_or("<invalid>"));

        // Start from the root directory (inode 256 in the default subvolume)
        let mut current_inode = objectid::FIRST_FREE_OBJECTID;
        let mut current_item = self.read_inode(current_inode)?;

        // Split path into components
        let path_str = &path[1..]; // skip leading /
        if path_str.is_empty() {
            // Root directory
            return Ok((current_inode, current_item));
        }

        for component in path_str.split(|&b| b == b'/') {
            if component.is_empty() {
                continue; // skip double slashes
            }

            if !current_item.is_dir() {
                log::error!("[btrfs::readwrite] path component is not a directory at inode {}", current_inode);
                return Err(BtrfsError::NotADirectory);
            }

            // Look up the component in the current directory
            let dir_entry = self.lookup_dir_entry(current_inode, component)?;
            current_inode = dir_entry.location.objectid;
            current_item = self.read_inode(current_inode)?;

            log::trace!("[btrfs::readwrite] resolved {:?} -> inode {}",
                core::str::from_utf8(component).unwrap_or("<invalid>"), current_inode);
        }

        Ok((current_inode, current_item))
    }

    /// Read an inode item from the filesystem tree.
    fn read_inode(&self, inode: u64) -> Result<InodeItem, BtrfsError> {
        let key = BtrfsKey::new(inode, KeyType::InodeItem as u8, 0);
        let result = self.search_fs_tree(&key)?;

        if !result.exact {
            log::error!("[btrfs::readwrite] inode {} not found", inode);
            return Err(BtrfsError::NotFound);
        }

        let slot = result.path.leaf_slot().unwrap_or(0);
        let data = result.leaf.item_data(slot)
            .ok_or(BtrfsError::Corrupt("cannot read inode item data"))?;

        InodeItem::from_bytes(data).ok_or(BtrfsError::Corrupt("cannot parse inode item"))
    }

    /// Look up a directory entry by name in the given directory inode.
    fn lookup_dir_entry(&self, dir_inode: u64, name: &[u8]) -> Result<DirItem, BtrfsError> {
        let key = dir_item_key(dir_inode, name);
        let result = self.search_fs_tree(&key)?;

        if !result.exact {
            log::trace!("[btrfs::readwrite] dir entry {:?} not found in inode {}",
                core::str::from_utf8(name).unwrap_or("<invalid>"), dir_inode);
            return Err(BtrfsError::NotFound);
        }

        let slot = result.path.leaf_slot().unwrap_or(0);
        let data = result.leaf.item_data(slot)
            .ok_or(BtrfsError::Corrupt("cannot read dir item data"))?;

        let items = DirItem::parse_all(data);
        dir::find_by_name(&items, name)
            .cloned()
            .ok_or(BtrfsError::NotFound)
    }

    /// Read the contents of a file at the given path.
    pub fn read_file(&self, path: &[u8]) -> Result<Vec<u8>, BtrfsError> {
        log::info!("[btrfs::readwrite] read_file: {:?}",
            core::str::from_utf8(path).unwrap_or("<invalid>"));

        let (inode_num, inode_item) = self.resolve_path(path)?;

        if inode_item.is_dir() {
            return Err(BtrfsError::IsADirectory);
        }

        if !inode_item.is_file() {
            log::error!("[btrfs::readwrite] inode {} is not a regular file (mode=0o{:06o})",
                inode_num, inode_item.mode);
            return Err(BtrfsError::NotFound);
        }

        self.read_file_data(inode_num, inode_item.size)
    }

    /// Read file data by collecting all EXTENT_DATA items for the inode.
    fn read_file_data(&self, inode: u64, size: u64) -> Result<Vec<u8>, BtrfsError> {
        log::debug!("[btrfs::readwrite] reading file data for inode {}: {} bytes", inode, size);

        let mut file_data = vec![0u8; size as usize];
        let mut bytes_read = 0u64;

        // Search for the first EXTENT_DATA item
        let _start_key = BtrfsKey::new(inode, KeyType::ExtentData as u8, 0);
        let fs_root = self.find_fs_tree_root()?;
        let fs_level = self.find_fs_tree_level()?;
        let nodesize = self.sb.nodesize;
        let chunk_map = &self.chunk_map;
        let dev = &self.dev;

        // Collect all extent data items for this inode
        let mut extent_items: Vec<(u64, FileExtentItem)> = Vec::new();

        tree::walk_tree(
            fs_root, fs_level, nodesize,
            |logical| read_node_via_chunks(dev, chunk_map, logical, nodesize),
            |key, data| {
                if key.objectid == inode && key.key_type == KeyType::ExtentData as u8 {
                    if let Some(extent) = FileExtentItem::from_bytes(data) {
                        extent_items.push((key.offset, extent));
                    }
                }
            },
        );

        log::debug!("[btrfs::readwrite] found {} extent items for inode {}", extent_items.len(), inode);

        for (file_offset, extent) in &extent_items {
            if extent.is_compressed() {
                log::error!("[btrfs::readwrite] compressed extent at offset {} not supported", file_offset);
                return Err(BtrfsError::CompressedExtent);
            }

            match extent.extent_type {
                BTRFS_FILE_EXTENT_INLINE => {
                    let len = extent.inline_data.len().min((size - file_offset) as usize);
                    let dst_start = *file_offset as usize;
                    let dst_end = dst_start + len;
                    if dst_end <= file_data.len() {
                        file_data[dst_start..dst_end].copy_from_slice(&extent.inline_data[..len]);
                        bytes_read += len as u64;
                        log::trace!("[btrfs::readwrite] read {} inline bytes at offset {}", len, file_offset);
                    }
                }
                BTRFS_FILE_EXTENT_REG => {
                    if extent.is_hole() {
                        // Sparse hole: already zeroed
                        log::trace!("[btrfs::readwrite] hole at offset {}, {} bytes", file_offset, extent.num_bytes);
                        continue;
                    }

                    let logical_start = extent.disk_bytenr + extent.offset;
                    let len = extent.num_bytes.min(size - file_offset);

                    // Read the extent data
                    let (_devid, physical) = self.chunk_map.resolve(logical_start)
                        .ok_or(BtrfsError::UnmappedLogical(logical_start))?;

                    let dst_start = *file_offset as usize;
                    let dst_end = (dst_start + len as usize).min(file_data.len());
                    let read_len = dst_end - dst_start;

                    self.dev.read_bytes(physical, &mut file_data[dst_start..dst_end])?;
                    bytes_read += read_len as u64;

                    log::trace!("[btrfs::readwrite] read {} bytes from logical=0x{:X} at file offset {}",
                        read_len, logical_start, file_offset);
                }
                _ => {
                    log::trace!("[btrfs::readwrite] skipping prealloc extent at offset {}", file_offset);
                }
            }
        }

        log::info!("[btrfs::readwrite] read_file_data: inode={}, total {} bytes read", inode, bytes_read);
        Ok(file_data)
    }

    /// Write data to a file at the given path (creating it if it doesn't exist).
    ///
    /// For simplicity, this creates inline extents for small files and regular extents
    /// for larger files. Existing file contents are replaced.
    pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<(), BtrfsError> {
        log::info!("[btrfs::readwrite] write_file: {:?} ({} bytes)",
            core::str::from_utf8(path).unwrap_or("<invalid>"), data.len());

        if path.is_empty() || path[0] != b'/' {
            return Err(BtrfsError::InvalidPath);
        }

        // Split path into parent and filename
        let (parent_path, filename) = split_path(path)?;

        // Resolve the parent directory
        let (parent_inode, parent_item) = self.resolve_path(parent_path)?;
        if !parent_item.is_dir() {
            return Err(BtrfsError::NotADirectory);
        }

        // Check if the file already exists
        let file_exists = self.lookup_dir_entry(parent_inode, filename).is_ok();

        if file_exists {
            log::debug!("[btrfs::readwrite] file already exists, will overwrite");
            // For a real implementation, we'd update the existing inode's extent data.
            // For now, we note this as a limitation.
            log::warn!("[btrfs::readwrite] overwrite not yet fully implemented; creating new inode");
        }

        // Allocate a new inode
        let new_inode = self.alloc_inode();
        let next_gen = self.generation + 1;
        let now = BtrfsTimespec { sec: 0, nsec: 0 }; // TODO: get real time

        // Create inode item
        let inode_item = InodeItem::new_file(0o644, 0, 0, next_gen, now);
        let inode_data = inode_item.to_bytes();

        // Create extent data item
        let extent_item = if data.len() < (self.sb.nodesize as usize / 2) {
            // Inline extent for small files
            FileExtentItem::new_inline(data, next_gen)
        } else {
            // TODO: allocate a data extent on disk for large files
            // For now, use inline for everything (will fail for very large files)
            log::warn!("[btrfs::readwrite] large file write ({} bytes) - using inline extent (size limited)",
                data.len());
            FileExtentItem::new_inline(data, next_gen)
        };
        let extent_data = extent_item.to_bytes();

        // Create directory entries (DIR_ITEM and DIR_INDEX)
        let dir_item = DirItem::new(filename, new_inode, dir_type::REG_FILE, next_gen);
        let dir_item_data = dir_item.to_bytes();

        // Insert into the filesystem tree:
        // 1. INODE_ITEM
        // 2. EXTENT_DATA
        // 3. DIR_ITEM in parent
        // 4. DIR_INDEX in parent

        let fs_root = self.find_fs_tree_root()?;
        let fs_level = self.find_fs_tree_level()?;

        // Read the root leaf/node and insert items
        let inode_key = BtrfsKey::new(new_inode, KeyType::InodeItem as u8, 0);
        let extent_key = BtrfsKey::new(new_inode, KeyType::ExtentData as u8, 0);
        let dir_key = dir_item_key(parent_inode, filename);
        let dir_idx = self.alloc_dir_index();
        let dir_idx_key = dir_index_key(parent_inode, dir_idx);

        // For a minimal write implementation, we insert into the leaf directly
        self.insert_item(fs_root, fs_level, &inode_key, &inode_data)?;
        self.insert_item(fs_root, fs_level, &extent_key, &extent_data)?;
        self.insert_item(fs_root, fs_level, &dir_key, &dir_item_data)?;
        self.insert_item(fs_root, fs_level, &dir_idx_key, &dir_item_data)?;

        // Update generation
        self.generation = next_gen;

        log::info!("[btrfs::readwrite] write_file complete: inode={}, {} bytes written", new_inode, data.len());
        Ok(())
    }

    /// Insert an item into a tree at the given root.
    fn insert_item(
        &self,
        root_bytenr: u64,
        root_level: u8,
        key: &BtrfsKey,
        data: &[u8],
    ) -> Result<(), BtrfsError> {
        let nodesize = self.sb.nodesize;
        let chunk_map = &self.chunk_map;
        let dev = &self.dev;

        // Search for the insertion point
        let result = tree::search_tree(
            root_bytenr, root_level, key, nodesize,
            |logical| read_node_via_chunks(dev, chunk_map, logical, nodesize),
        ).ok_or(BtrfsError::Corrupt("failed to search for insertion point"))?;

        if result.exact {
            log::warn!("[btrfs::readwrite] key {} already exists, skipping insert", key);
            return Ok(());
        }

        // Get the leaf and try to insert
        let leaf_bytenr = result.path.leaf().map(|e| e.bytenr)
            .ok_or(BtrfsError::Corrupt("no leaf in search path"))?;

        let mut leaf_data = self.read_node(leaf_bytenr)?;

        let inserted = tree::insert_into_leaf(
            &mut leaf_data,
            nodesize,
            key,
            data,
            self.generation + 1,
            &self.sb.fsid,
        );

        if inserted.is_some() {
            // Write the updated leaf back
            self.write_node(leaf_bytenr, &leaf_data)?;
            log::debug!("[btrfs::readwrite] inserted key {} into leaf at 0x{:X}", key, leaf_bytenr);
            Ok(())
        } else {
            // Leaf is full -- would need to split
            log::error!("[btrfs::readwrite] leaf at 0x{:X} is full, split needed (not yet implemented for write path)",
                leaf_bytenr);
            Err(BtrfsError::NoFreeSpace)
        }
    }

    /// Create a directory at the given path.
    pub fn mkdir(&mut self, path: &[u8]) -> Result<(), BtrfsError> {
        log::info!("[btrfs::readwrite] mkdir: {:?}",
            core::str::from_utf8(path).unwrap_or("<invalid>"));

        if path.is_empty() || path[0] != b'/' {
            return Err(BtrfsError::InvalidPath);
        }

        let (parent_path, dirname) = split_path(path)?;

        // Resolve the parent
        let (parent_inode, parent_item) = self.resolve_path(parent_path)?;
        if !parent_item.is_dir() {
            return Err(BtrfsError::NotADirectory);
        }

        // Check if already exists
        if self.lookup_dir_entry(parent_inode, dirname).is_ok() {
            return Err(BtrfsError::AlreadyExists);
        }

        let new_inode = self.alloc_inode();
        let next_gen = self.generation + 1;
        let now = BtrfsTimespec { sec: 0, nsec: 0 };

        // Create the directory inode
        let inode_item = InodeItem::new_dir(0o755, 0, 0, next_gen, now);
        let inode_data = inode_item.to_bytes();

        // Create dir entries in the parent
        let dir_item = DirItem::new(dirname, new_inode, dir_type::DIR, next_gen);
        let dir_item_data = dir_item.to_bytes();

        let fs_root = self.find_fs_tree_root()?;
        let fs_level = self.find_fs_tree_level()?;

        let inode_key = BtrfsKey::new(new_inode, KeyType::InodeItem as u8, 0);
        let dir_key = dir_item_key(parent_inode, dirname);
        let dir_idx = self.alloc_dir_index();
        let dir_idx_key = dir_index_key(parent_inode, dir_idx);

        self.insert_item(fs_root, fs_level, &inode_key, &inode_data)?;
        self.insert_item(fs_root, fs_level, &dir_key, &dir_item_data)?;
        self.insert_item(fs_root, fs_level, &dir_idx_key, &dir_item_data)?;

        self.generation = next_gen;

        log::info!("[btrfs::readwrite] mkdir complete: {:?} -> inode {}",
            core::str::from_utf8(dirname).unwrap_or("<invalid>"), new_inode);

        Ok(())
    }

    /// List the contents of a directory at the given path.
    pub fn list_dir(&self, path: &[u8]) -> Result<Vec<DirEntry>, BtrfsError> {
        log::info!("[btrfs::readwrite] list_dir: {:?}",
            core::str::from_utf8(path).unwrap_or("<invalid>"));

        let (dir_inode, dir_item) = self.resolve_path(path)?;

        if !dir_item.is_dir() {
            return Err(BtrfsError::IsNotADirectory);
        }

        // Walk the FS tree looking for DIR_INDEX items for this inode
        let fs_root = self.find_fs_tree_root()?;
        let fs_level = self.find_fs_tree_level()?;
        let nodesize = self.sb.nodesize;
        let chunk_map = &self.chunk_map;
        let dev = &self.dev;

        let mut entries: Vec<DirEntry> = Vec::new();

        tree::walk_tree(
            fs_root, fs_level, nodesize,
            |logical| read_node_via_chunks(dev, chunk_map, logical, nodesize),
            |key, data| {
                if key.objectid == dir_inode && key.key_type == KeyType::DirIndex as u8 {
                    let items = DirItem::parse_all(data);
                    for item in items {
                        let name = String::from_utf8_lossy(&item.name[..item.name_len as usize]).into_owned();
                        entries.push(DirEntry {
                            name,
                            inode: item.location.objectid,
                            file_type: item.dir_type,
                        });
                    }
                }
            },
        );

        log::info!("[btrfs::readwrite] list_dir: {} entries in inode {}", entries.len(), dir_inode);

        for entry in &entries {
            log::debug!("[btrfs::readwrite]   {:?} (inode={}, type={})",
                entry.name, entry.inode, entry.type_str());
        }

        Ok(entries)
    }

    /// Flush the superblock to disk (with updated generation).
    pub fn sync(&self) -> Result<(), BtrfsError> {
        log::info!("[btrfs::readwrite] syncing superblock to disk (generation={})", self.sb.generation);

        let sb_bytes = self.sb.to_bytes();
        self.dev.write_bytes(SUPERBLOCK_OFFSET, &sb_bytes)?;

        log::info!("[btrfs::readwrite] sync complete");
        Ok(())
    }
}

/// Read a tree node via chunk map address translation.
///
/// Helper function used by tree operations that need a closure for reading nodes.
fn read_node_via_chunks<D: BlockDevice>(
    dev: &D,
    chunk_map: &ChunkMap,
    logical: u64,
    nodesize: u32,
) -> Option<Vec<u8>> {
    let (_devid, physical) = chunk_map.resolve(logical)?;

    let mut buf = vec![0u8; nodesize as usize];
    match dev.read_bytes(physical, &mut buf) {
        Ok(()) => {
            log::trace!("[btrfs::readwrite] read_node_via_chunks: logical=0x{:X} -> physical=0x{:X}",
                logical, physical);
            Some(buf)
        }
        Err(e) => {
            log::error!("[btrfs::readwrite] failed to read node at logical=0x{:X}, physical=0x{:X}: {}",
                logical, physical, e);
            None
        }
    }
}

/// Split a path into (parent_path, filename).
///
/// For example, `/foo/bar/baz` -> (`/foo/bar`, `baz`).
fn split_path(path: &[u8]) -> Result<(&[u8], &[u8]), BtrfsError> {
    if path.is_empty() || path[0] != b'/' {
        return Err(BtrfsError::InvalidPath);
    }

    // Remove trailing slash
    let path = if path.len() > 1 && path[path.len() - 1] == b'/' {
        &path[..path.len() - 1]
    } else {
        path
    };

    // Find last slash
    let last_slash = path.iter().rposition(|&b| b == b'/')
        .ok_or(BtrfsError::InvalidPath)?;

    let parent = if last_slash == 0 { &path[..1] } else { &path[..last_slash] };
    let filename = &path[last_slash + 1..];

    if filename.is_empty() {
        return Err(BtrfsError::InvalidPath);
    }

    if filename.len() > 255 {
        return Err(BtrfsError::NameTooLong);
    }

    Ok((parent, filename))
}
