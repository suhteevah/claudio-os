//! # claudio-btrfs
//!
//! A `no_std` btrfs filesystem implementation for ClaudioOS.
//!
//! This crate provides read and write access to btrfs filesystems, including:
//! - Superblock parsing and validation (at offset 0x10000)
//! - B-tree traversal and modification (search, insert, split)
//! - Inode item reading and writing
//! - Directory entry parsing, creation, and lookup (crc32c name hashing)
//! - File extent data (inline, regular, prealloc)
//! - Chunk/device mapping (logical to physical address translation)
//! - CRC32C checksums (superblock, metadata, directory name hashing)
//! - High-level file read/write/create/mkdir API
//!
//! ## Usage
//!
//! ```rust,no_run
//! use claudio_btrfs::{BtrFs, BlockDevice};
//!
//! // Implement BlockDevice for your storage backend
//! // Then mount the filesystem:
//! let fs = BtrFs::mount(device).expect("failed to mount btrfs");
//! let data = fs.read_file(b"/etc/hostname").expect("read failed");
//! ```

#![no_std]

extern crate alloc;

pub mod chunk;
pub mod crc32c;
pub mod dir;
pub mod extent;
pub mod inode;
pub mod item;
pub mod key;
pub mod readwrite;
pub mod superblock;
pub mod tree;

pub use readwrite::{BlockDevice, BtrFs, BtrfsError};
pub use superblock::Superblock;
pub use key::{BtrfsKey, KeyType};
pub use item::{BtrfsHeader, BtrfsLeaf, BtrfsNode, BtrfsKeyPtr};
pub use inode::InodeItem;
pub use dir::DirItem;
pub use extent::FileExtentItem;
pub use chunk::{ChunkItem, Stripe};
