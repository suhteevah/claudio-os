//! btrfs directory entry (dir_item) parsing and operations.
//!
//! Directories in btrfs store entries in two forms:
//! - **DIR_ITEM** (type 84): keyed by (parent_inode, DIR_ITEM, crc32c(name)).
//!   Used for O(1) name lookup.
//! - **DIR_INDEX** (type 96): keyed by (parent_inode, DIR_INDEX, sequence_number).
//!   Used for ordered directory iteration (readdir).
//!
//! Both types have the same on-disk format (btrfs_dir_item).
//! Multiple entries can share the same key if there are hash collisions (DIR_ITEM)
//! or if they happen to share a sequence number (shouldn't happen for DIR_INDEX).
//!
//! Reference: <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html#dir-item>

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::crc32c;
use crate::key::BtrfsKey;

/// Minimum size of a dir_item on disk (30 bytes + 0 name + 0 data).
pub const BTRFS_DIR_ITEM_HEADER_SIZE: usize = 30;

/// btrfs directory entry type constants (stored in `dir_type` field).
pub mod dir_type {
    /// Unknown type.
    pub const UNKNOWN: u8 = 0;
    /// Regular file.
    pub const REG_FILE: u8 = 1;
    /// Directory.
    pub const DIR: u8 = 2;
    /// Character device.
    pub const CHRDEV: u8 = 3;
    /// Block device.
    pub const BLKDEV: u8 = 4;
    /// FIFO.
    pub const FIFO: u8 = 5;
    /// Socket.
    pub const SOCK: u8 = 6;
    /// Symbolic link.
    pub const SYMLINK: u8 = 7;
    /// Extended attribute.
    pub const XATTR: u8 = 8;
}

/// A parsed btrfs directory item.
#[derive(Clone)]
pub struct DirItem {
    /// Key of the child inode (objectid=inode_number, type=INODE_ITEM, offset=0).
    pub location: BtrfsKey,
    /// Transaction id when this entry was created.
    pub transid: u64,
    /// Length of the embedded data (usually 0, nonzero for xattr items).
    pub data_len: u16,
    /// Length of the filename.
    pub name_len: u16,
    /// File type (dir_type::REG_FILE, dir_type::DIR, etc.).
    pub dir_type: u8,
    /// The filename bytes.
    pub name: Vec<u8>,
    /// Embedded data (for xattr items).
    pub data: Vec<u8>,
}

impl DirItem {
    /// Parse a single dir_item from a buffer.
    ///
    /// Returns the parsed item and the number of bytes consumed, or None on error.
    /// Multiple dir_items can be concatenated in the same leaf item data (hash collisions).
    pub fn from_bytes(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.len() < BTRFS_DIR_ITEM_HEADER_SIZE {
            log::error!("[btrfs::dir] buffer too small for dir_item: {} bytes (need >= {})",
                buf.len(), BTRFS_DIR_ITEM_HEADER_SIZE);
            return None;
        }

        let location = BtrfsKey::from_bytes(&buf[0..17])?;
        let transid = read_u64(buf, 17);
        let data_len = read_u16(buf, 25);
        let name_len = read_u16(buf, 27);
        let dir_type = buf[29];

        let total_size = BTRFS_DIR_ITEM_HEADER_SIZE + name_len as usize + data_len as usize;
        if buf.len() < total_size {
            log::error!("[btrfs::dir] buffer too small for dir_item payload: need {} bytes, have {}",
                total_size, buf.len());
            return None;
        }

        let name_start = BTRFS_DIR_ITEM_HEADER_SIZE;
        let name_end = name_start + name_len as usize;
        let name = buf[name_start..name_end].to_vec();

        let data_start = name_end;
        let data_end = data_start + data_len as usize;
        let data = buf[data_start..data_end].to_vec();

        log::trace!("[btrfs::dir] parsed dir_item: name={:?}, type={}, location=({},{},{}), transid={}",
            core::str::from_utf8(&name).unwrap_or("<invalid>"),
            dir_type, location.objectid, location.key_type, location.offset, transid);

        Some((
            DirItem {
                location,
                transid,
                data_len,
                name_len,
                dir_type,
                name,
                data,
            },
            total_size,
        ))
    }

    /// Parse all dir_items from a single leaf item's data.
    ///
    /// Multiple dir_items can be packed together when there are hash collisions.
    pub fn parse_all(buf: &[u8]) -> Vec<DirItem> {
        let mut items = Vec::new();
        let mut offset = 0;

        while offset < buf.len() {
            match DirItem::from_bytes(&buf[offset..]) {
                Some((item, consumed)) => {
                    items.push(item);
                    offset += consumed;
                }
                None => {
                    log::trace!("[btrfs::dir] stopping dir_item parsing at offset {}", offset);
                    break;
                }
            }
        }

        log::debug!("[btrfs::dir] parsed {} dir_items from {} bytes", items.len(), buf.len());
        items
    }

    /// Serialize this dir_item to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let total_size = BTRFS_DIR_ITEM_HEADER_SIZE + self.name_len as usize + self.data_len as usize;
        let mut buf = alloc::vec![0u8; total_size];

        let key_bytes = self.location.to_bytes();
        buf[0..17].copy_from_slice(&key_bytes);
        write_u64(&mut buf, 17, self.transid);
        write_u16(&mut buf, 25, self.data_len);
        write_u16(&mut buf, 27, self.name_len);
        buf[29] = self.dir_type;

        let name_start = BTRFS_DIR_ITEM_HEADER_SIZE;
        buf[name_start..name_start + self.name_len as usize]
            .copy_from_slice(&self.name[..self.name_len as usize]);

        if self.data_len > 0 {
            let data_start = name_start + self.name_len as usize;
            buf[data_start..data_start + self.data_len as usize]
                .copy_from_slice(&self.data[..self.data_len as usize]);
        }

        log::trace!("[btrfs::dir] serialized dir_item: {} bytes", buf.len());
        buf
    }

    /// Get the filename as a UTF-8 string.
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("<invalid>")
    }

    /// Create a new directory entry for a file.
    pub fn new(name: &[u8], inode: u64, dir_type: u8, transid: u64) -> Self {
        log::debug!("[btrfs::dir] creating dir_item: name={:?}, inode={}, type={}, transid={}",
            core::str::from_utf8(name).unwrap_or("<invalid>"), inode, dir_type, transid);
        DirItem {
            location: BtrfsKey::new(inode, crate::key::KeyType::InodeItem as u8, 0),
            transid,
            data_len: 0,
            name_len: name.len() as u16,
            dir_type,
            name: name.to_vec(),
            data: Vec::new(),
        }
    }

    /// Whether this is a "." (current directory) entry.
    #[inline]
    pub fn is_dot(&self) -> bool {
        self.name_len == 1 && self.name.first() == Some(&b'.')
    }

    /// Whether this is a ".." (parent directory) entry.
    #[inline]
    pub fn is_dotdot(&self) -> bool {
        self.name_len == 2 && self.name.starts_with(b"..")
    }
}

impl fmt::Debug for DirItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let type_str = match self.dir_type {
            dir_type::REG_FILE => "file",
            dir_type::DIR => "dir",
            dir_type::SYMLINK => "symlink",
            dir_type::CHRDEV => "chrdev",
            dir_type::BLKDEV => "blkdev",
            dir_type::FIFO => "fifo",
            dir_type::SOCK => "sock",
            dir_type::XATTR => "xattr",
            _ => "unknown",
        };
        f.debug_struct("DirItem")
            .field("name", &self.name_str())
            .field("inode", &self.location.objectid)
            .field("type", &type_str)
            .field("transid", &self.transid)
            .finish()
    }
}

/// Compute the DIR_ITEM key for a directory entry lookup.
///
/// The key is (parent_inode, DIR_ITEM, crc32c(name)).
pub fn dir_item_key(parent_inode: u64, name: &[u8]) -> BtrfsKey {
    let hash = crc32c::btrfs_name_hash(name);
    let key = BtrfsKey::new(parent_inode, crate::key::KeyType::DirItem as u8, hash as u64);
    log::trace!("[btrfs::dir] dir_item_key: parent={}, name={:?}, hash=0x{:08X}",
        parent_inode, core::str::from_utf8(name).unwrap_or("<invalid>"), hash);
    key
}

/// Compute the DIR_INDEX key for a directory entry.
///
/// The key is (parent_inode, DIR_INDEX, sequence_number).
pub fn dir_index_key(parent_inode: u64, index: u64) -> BtrfsKey {
    BtrfsKey::new(parent_inode, crate::key::KeyType::DirIndex as u8, index)
}

/// Look up a name in a list of dir_items (parsed from a single leaf item).
///
/// Handles hash collisions by comparing names.
pub fn find_by_name<'a>(items: &'a [DirItem], name: &[u8]) -> Option<&'a DirItem> {
    for item in items {
        if item.name_len as usize == name.len() && &item.name[..item.name_len as usize] == name {
            log::debug!("[btrfs::dir] found dir_item for {:?} -> inode {}",
                core::str::from_utf8(name).unwrap_or("<invalid>"), item.location.objectid);
            return Some(item);
        }
    }
    log::trace!("[btrfs::dir] name {:?} not found in {} dir_items",
        core::str::from_utf8(name).unwrap_or("<invalid>"), items.len());
    None
}

/// A high-level directory entry returned by list_dir operations.
#[derive(Clone, Debug)]
pub struct DirEntry {
    /// Filename.
    pub name: String,
    /// Inode number.
    pub inode: u64,
    /// File type (dir_type constant).
    pub file_type: u8,
}

impl DirEntry {
    /// Human-readable file type string.
    pub fn type_str(&self) -> &'static str {
        match self.file_type {
            dir_type::REG_FILE => "file",
            dir_type::DIR => "dir",
            dir_type::SYMLINK => "symlink",
            dir_type::CHRDEV => "chrdev",
            dir_type::BLKDEV => "blkdev",
            dir_type::FIFO => "fifo",
            dir_type::SOCK => "sock",
            _ => "unknown",
        }
    }
}

// --- Little-endian byte helpers ---

#[inline]
fn read_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
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
fn write_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_le_bytes());
}
