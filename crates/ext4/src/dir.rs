//! ext4 directory entry parsing and creation.
//!
//! Directories in ext4 are stored as a series of variable-length entries in the
//! directory's data blocks. Each entry contains an inode number, entry length,
//! name length, file type, and the filename.
//!
//! ext4 supports two directory formats:
//! - Linear: entries are stored sequentially, scanned linearly for lookup.
//! - HTree (indexed): a hash tree for O(1) lookup in large directories.
//!   This module implements linear directory operations; htree can be added later.
//!
//! Reference: <https://ext4.wiki.kernel.org/index.php/Ext4_Disk_Layout#Directory_Entries>

use alloc::vec::Vec;
use core::fmt;

/// Minimum directory entry size (inode + rec_len + name_len + file_type = 8 bytes).
pub const DIR_ENTRY_MIN_SIZE: usize = 8;

/// Maximum filename length in a directory entry.
pub const EXT4_NAME_LEN: usize = 255;

// --- File type constants for directory entries (when INCOMPAT_FILETYPE is set) ---

/// Unknown file type.
pub const FT_UNKNOWN: u8 = 0;
/// Regular file.
pub const FT_REG_FILE: u8 = 1;
/// Directory.
pub const FT_DIR: u8 = 2;
/// Character device.
pub const FT_CHRDEV: u8 = 3;
/// Block device.
pub const FT_BLKDEV: u8 = 4;
/// FIFO (named pipe).
pub const FT_FIFO: u8 = 5;
/// Socket.
pub const FT_SOCK: u8 = 6;
/// Symbolic link.
pub const FT_SYMLINK: u8 = 7;

/// A parsed directory entry.
#[derive(Clone)]
pub struct DirEntry {
    /// Inode number this entry points to. 0 means the entry is unused/deleted.
    pub inode: u32,
    /// Total size of this entry in bytes (includes padding for alignment).
    /// The next entry starts at the current offset + rec_len.
    pub rec_len: u16,
    /// Length of the filename in bytes.
    pub name_len: u8,
    /// File type (FT_REG_FILE, FT_DIR, etc.). Only valid if INCOMPAT_FILETYPE is set.
    pub file_type: u8,
    /// The filename (not null-terminated, length is `name_len`).
    pub name: Vec<u8>,
}

impl DirEntry {
    /// Parse a single directory entry from a buffer at the given offset.
    ///
    /// Returns `None` if the buffer is too small or the entry is invalid.
    /// On success, returns the parsed entry. Use `rec_len` to advance to the next entry.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < DIR_ENTRY_MIN_SIZE {
            log::trace!("[ext4::dir] buffer too small for dir entry: {} bytes", buf.len());
            return None;
        }

        let inode = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let rec_len = u16::from_le_bytes([buf[4], buf[5]]);
        let name_len = buf[6];
        let file_type = buf[7];

        if rec_len < DIR_ENTRY_MIN_SIZE as u16 {
            log::error!("[ext4::dir] invalid rec_len={} (minimum is {})", rec_len, DIR_ENTRY_MIN_SIZE);
            return None;
        }

        if (rec_len as usize) > buf.len() {
            log::error!("[ext4::dir] rec_len={} exceeds buffer size {}", rec_len, buf.len());
            return None;
        }

        let name_end = DIR_ENTRY_MIN_SIZE + name_len as usize;
        if name_end > buf.len() {
            log::error!("[ext4::dir] name extends beyond buffer: name_len={}, avail={}", name_len, buf.len() - DIR_ENTRY_MIN_SIZE);
            return None;
        }

        let name = buf[DIR_ENTRY_MIN_SIZE..name_end].to_vec();

        log::trace!("[ext4::dir] entry: inode={}, rec_len={}, type={}, name={:?}",
            inode, rec_len, file_type, core::str::from_utf8(&name).unwrap_or("<invalid>"));

        Some(DirEntry {
            inode,
            rec_len,
            name_len,
            file_type,
            name,
        })
    }

    /// Serialize this directory entry into bytes.
    ///
    /// The output will be exactly `rec_len` bytes, zero-padded after the name.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = alloc::vec![0u8; self.rec_len as usize];
        buf[0..4].copy_from_slice(&self.inode.to_le_bytes());
        buf[4..6].copy_from_slice(&self.rec_len.to_le_bytes());
        buf[6] = self.name_len;
        buf[7] = self.file_type;
        let name_end = DIR_ENTRY_MIN_SIZE + self.name_len as usize;
        if name_end <= buf.len() {
            buf[DIR_ENTRY_MIN_SIZE..name_end].copy_from_slice(&self.name[..self.name_len as usize]);
        }
        log::trace!("[ext4::dir] serialized entry: inode={}, rec_len={}, name_len={}", self.inode, self.rec_len, self.name_len);
        buf
    }

    /// Get the filename as a UTF-8 string (lossy conversion).
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("<invalid>")
    }

    /// Calculate the actual size needed for this entry (rounded up to 4-byte boundary).
    pub fn actual_size(&self) -> u16 {
        let base = DIR_ENTRY_MIN_SIZE as u16 + self.name_len as u16;
        // Round up to 4-byte alignment
        (base + 3) & !3
    }

    /// Create a new directory entry.
    ///
    /// `rec_len` should be set to the actual space available in the directory block.
    /// For the last entry in a block, this extends to the end of the block.
    pub fn new(inode: u32, name: &[u8], file_type: u8, rec_len: u16) -> Self {
        log::debug!("[ext4::dir] creating entry: inode={}, name={:?}, type={}, rec_len={}",
            inode, core::str::from_utf8(name).unwrap_or("<invalid>"), file_type, rec_len);
        DirEntry {
            inode,
            rec_len,
            name_len: name.len() as u8,
            file_type,
            name: name.to_vec(),
        }
    }

    /// Whether this is the "." (current directory) entry.
    #[inline]
    pub fn is_dot(&self) -> bool {
        self.name_len == 1 && self.name.first() == Some(&b'.')
    }

    /// Whether this is the ".." (parent directory) entry.
    #[inline]
    pub fn is_dotdot(&self) -> bool {
        self.name_len == 2 && self.name.starts_with(b"..")
    }

    /// Whether this entry has been deleted (inode == 0).
    #[inline]
    pub fn is_deleted(&self) -> bool {
        self.inode == 0
    }
}

impl fmt::Debug for DirEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let type_str = match self.file_type {
            FT_REG_FILE => "file",
            FT_DIR => "dir",
            FT_SYMLINK => "symlink",
            FT_CHRDEV => "chrdev",
            FT_BLKDEV => "blkdev",
            FT_FIFO => "fifo",
            FT_SOCK => "sock",
            _ => "unknown",
        };
        f.debug_struct("DirEntry")
            .field("inode", &self.inode)
            .field("name", &self.name_str())
            .field("type", &type_str)
            .field("rec_len", &self.rec_len)
            .finish()
    }
}

/// Iterator over directory entries in a raw directory data block.
///
/// Yields each `DirEntry` in sequence until the block is exhausted.
pub struct DirEntryIter<'a> {
    buf: &'a [u8],
    offset: usize,
}

impl<'a> DirEntryIter<'a> {
    /// Create a new iterator over directory entries in the given block data.
    pub fn new(buf: &'a [u8]) -> Self {
        log::trace!("[ext4::dir] starting directory iteration over {} bytes", buf.len());
        DirEntryIter { buf, offset: 0 }
    }
}

impl<'a> Iterator for DirEntryIter<'a> {
    type Item = (usize, DirEntry);

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.buf.len() {
            return None;
        }

        let remaining = &self.buf[self.offset..];
        if remaining.len() < DIR_ENTRY_MIN_SIZE {
            return None;
        }

        let entry = DirEntry::from_bytes(remaining)?;
        if entry.rec_len == 0 {
            log::error!("[ext4::dir] zero rec_len at offset {}, stopping iteration", self.offset);
            return None;
        }

        let entry_offset = self.offset;
        self.offset += entry.rec_len as usize;

        Some((entry_offset, entry))
    }
}

/// Look up a filename in a directory block.
///
/// Returns the `DirEntry` if found, along with its byte offset in the block.
pub fn lookup_in_block(block_data: &[u8], name: &[u8]) -> Option<(usize, DirEntry)> {
    log::debug!("[ext4::dir] looking up {:?} in directory block ({} bytes)",
        core::str::from_utf8(name).unwrap_or("<invalid>"), block_data.len());

    for (offset, entry) in DirEntryIter::new(block_data) {
        if entry.inode != 0
            && entry.name_len as usize == name.len()
            && &entry.name[..entry.name_len as usize] == name
        {
            log::debug!("[ext4::dir] found {:?} -> inode {}", entry.name_str(), entry.inode);
            return Some((offset, entry));
        }
    }

    log::trace!("[ext4::dir] {:?} not found in block", core::str::from_utf8(name).unwrap_or("<invalid>"));
    None
}

/// Find space for a new directory entry in a block.
///
/// Scans entries looking for either a deleted entry or trailing space in the last
/// entry's rec_len that can accommodate `needed_len` bytes.
///
/// Returns `Some((offset, available_rec_len))` if space was found.
pub fn find_space_in_block(block_data: &[u8], needed_len: u16) -> Option<(usize, u16)> {
    log::trace!("[ext4::dir] searching for {} bytes of space in block ({} bytes)",
        needed_len, block_data.len());

    let mut offset = 0usize;

    while offset < block_data.len() {
        let remaining = &block_data[offset..];
        if remaining.len() < DIR_ENTRY_MIN_SIZE {
            break;
        }

        let entry = match DirEntry::from_bytes(remaining) {
            Some(e) => e,
            None => break,
        };

        if entry.rec_len == 0 {
            break;
        }

        // Check if this is a deleted entry with enough space
        if entry.inode == 0 && entry.rec_len >= needed_len {
            log::debug!("[ext4::dir] found deleted entry at offset {} with rec_len={}", offset, entry.rec_len);
            return Some((offset, entry.rec_len));
        }

        // Check if the entry has trailing slack space
        let actual = entry.actual_size();
        let slack = entry.rec_len - actual;
        if slack >= needed_len {
            log::debug!("[ext4::dir] found {} bytes slack at offset {} (actual={}, rec_len={})",
                slack, offset, actual, entry.rec_len);
            return Some((offset + actual as usize, slack));
        }

        offset += entry.rec_len as usize;
    }

    log::trace!("[ext4::dir] no space found in block");
    None
}

/// Create the initial directory entries for a new directory (. and ..).
///
/// Returns the serialized block data containing just the two entries,
/// with ".." consuming the rest of the block via its rec_len.
pub fn create_dot_entries(self_inode: u32, parent_inode: u32, block_size: u32) -> Vec<u8> {
    log::debug!("[ext4::dir] creating . and .. entries: self={}, parent={}, block_size={}",
        self_inode, parent_inode, block_size);

    let dot_actual_size: u16 = 12; // 8 + 1 name + 3 padding
    let dotdot_rec_len = block_size as u16 - dot_actual_size;

    let dot = DirEntry::new(self_inode, b".", FT_DIR, dot_actual_size);
    let dotdot = DirEntry::new(parent_inode, b"..", FT_DIR, dotdot_rec_len);

    let mut block = alloc::vec![0u8; block_size as usize];
    let dot_bytes = dot.to_bytes();
    block[..dot_bytes.len()].copy_from_slice(&dot_bytes);
    let dotdot_bytes = dotdot.to_bytes();
    block[dot_actual_size as usize..dot_actual_size as usize + dotdot_bytes.len()]
        .copy_from_slice(&dotdot_bytes);

    log::trace!("[ext4::dir] created dot entries block ({} bytes)", block.len());
    block
}
