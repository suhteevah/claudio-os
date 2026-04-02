//! # claudio-ntfs
//!
//! A `no_std` NTFS filesystem implementation for ClaudioOS.
//!
//! This crate provides read and write access to NTFS filesystems, including:
//! - Boot sector / BPB parsing and validation
//! - Master File Table (MFT) entry parsing with fixup array support
//! - Attribute parsing (resident and non-resident) with data run decoding
//! - $FILE_NAME attribute parsing (UTF-16LE filenames, timestamps)
//! - B+ tree index traversal for directory lookups
//! - $UpCase table for case-insensitive filename comparison
//! - High-level read/write/mkdir/list_dir API
//!
//! ## Usage
//!
//! ```rust,no_run
//! use claudio_ntfs::{NtfsFs, BlockDevice};
//!
//! // Implement BlockDevice for your storage backend
//! // Then mount the filesystem:
//! let fs = NtfsFs::mount(device).expect("failed to mount NTFS");
//! let data = fs.read_file(b"/Windows/System32/config/SAM").expect("read failed");
//! ```

#![no_std]

extern crate alloc;

pub mod attribute;
pub mod boot_sector;
pub mod data_runs;
pub mod filename;
pub mod index;
pub mod mft;
pub mod readwrite;
pub mod upcase;

pub use readwrite::{BlockDevice, NtfsFs, NtfsError};
pub use boot_sector::BootSector;
pub use mft::{MftEntry, MftEntryHeader, MFT_ENTRY_FLAG_IN_USE, MFT_ENTRY_FLAG_DIRECTORY};
pub use attribute::{AttributeHeader, ResidentHeader, NonResidentHeader, AttributeType};
pub use filename::{FileNameAttr, FileNamespace};
pub use index::{IndexRoot, IndexEntry, IndexNodeHeader};
pub use data_runs::{DataRun, decode_data_runs};
pub use upcase::UpCaseTable;
