//! # claudio-vfs
//!
//! A `no_std` Virtual Filesystem layer for ClaudioOS.
//!
//! This crate unifies multiple filesystem implementations (ext4, btrfs, NTFS, FAT32)
//! and storage drivers (AHCI, NVMe, VirtIO-blk) behind a single API. It provides:
//!
//! - **Path handling**: Owned heap-allocated paths with normalization, component
//!   iteration, join, parent, extension, and case-sensitivity options.
//! - **Mount table**: Longest-prefix-match mount point resolution with read-only
//!   flag support and ordered mount listing.
//! - **File operations**: POSIX-like open/read/write/seek/close/stat with
//!   file descriptors, open flags, and seek whence.
//! - **Directory operations**: readdir iterator, mkdir, rmdir, DirEntry.
//! - **Filesystem trait**: A trait that ext4/btrfs/NTFS/FAT32 crates implement.
//! - **Block device abstraction**: Trait for storage backends, plus GPT and MBR
//!   partition table parsing with filesystem type auto-detection.
//! - **Top-level VFS**: Ties everything together with a global init, working
//!   directory tracking, and full POSIX-like API (open, read, write, close,
//!   stat, mkdir, readdir, rm, cp, mv).
//!
//! ## Usage
//!
//! ```rust,no_run
//! use claudio_vfs::{Vfs, VfsError};
//!
//! let mut vfs = Vfs::new();
//! // Mount a filesystem at a path:
//! // vfs.mount("/data", my_ext4_fs, MountOptions::default());
//! // Then use POSIX-like operations:
//! // let fd = vfs.open("/data/hello.txt", OpenFlags::READ)?;
//! // let n = vfs.read(fd, &mut buf)?;
//! // vfs.close(fd)?;
//! ```

#![no_std]

extern crate alloc;

pub mod path;
pub mod mount;
pub mod file;
pub mod dir;
pub mod fs_trait;
pub mod device;
pub mod vfs;
pub mod adapters;
pub mod memfs;

pub use path::Path;
pub use mount::{MountPoint, MountTable, MountOptions};
pub use file::{FileDescriptor, OpenFlags, SeekWhence, FileInfo, FileType};
pub use dir::DirEntry;
pub use fs_trait::{Filesystem, FsType};
pub use device::{BlockDevice, Partition, PartitionScheme, PartitionEntry};
pub use vfs::{Vfs, VfsError};
pub use memfs::MemFs;
