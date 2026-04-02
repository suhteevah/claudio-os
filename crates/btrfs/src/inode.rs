//! btrfs inode item parsing.
//!
//! Each file, directory, symlink, etc. in btrfs has an INODE_ITEM stored in the
//! filesystem tree. The inode item contains metadata like size, timestamps, mode,
//! and link count.
//!
//! The INODE_ITEM key is: (inode_number, INODE_ITEM, 0).
//!
//! Timestamps in btrfs are stored as a `btrfs_timespec`: (seconds: i64, nsec: u32).
//!
//! Reference: <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html#inode-item>

use alloc::vec::Vec;
use core::fmt;

/// Size of a btrfs inode item on disk (160 bytes).
pub const BTRFS_INODE_ITEM_SIZE: usize = 160;

/// Size of a btrfs timespec on disk (12 bytes: i64 sec + u32 nsec).
pub const BTRFS_TIMESPEC_SIZE: usize = 12;

// --- File mode constants (same as Linux stat.h, used in btrfs mode field) ---

/// Socket.
pub const S_IFSOCK: u32 = 0o140000;
/// Symbolic link.
pub const S_IFLNK: u32 = 0o120000;
/// Regular file.
pub const S_IFREG: u32 = 0o100000;
/// Block device.
pub const S_IFBLK: u32 = 0o060000;
/// Directory.
pub const S_IFDIR: u32 = 0o040000;
/// Character device.
pub const S_IFCHR: u32 = 0o020000;
/// FIFO (named pipe).
pub const S_IFIFO: u32 = 0o010000;
/// File type mask.
pub const S_IFMT: u32 = 0o170000;

/// btrfs timespec (seconds + nanoseconds).
#[derive(Clone, Copy, Debug, Default)]
pub struct BtrfsTimespec {
    /// Seconds since epoch (signed, can be negative for pre-1970).
    pub sec: i64,
    /// Nanoseconds (0..999_999_999).
    pub nsec: u32,
}

impl BtrfsTimespec {
    /// Parse a timespec from a 12-byte buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < BTRFS_TIMESPEC_SIZE {
            return None;
        }
        Some(BtrfsTimespec {
            sec: read_i64(buf, 0),
            nsec: read_u32(buf, 8),
        })
    }

    /// Serialize to a 12-byte buffer.
    pub fn to_bytes(&self) -> [u8; BTRFS_TIMESPEC_SIZE] {
        let mut buf = [0u8; BTRFS_TIMESPEC_SIZE];
        buf[0..8].copy_from_slice(&self.sec.to_le_bytes());
        buf[8..12].copy_from_slice(&self.nsec.to_le_bytes());
        buf
    }
}

/// Parsed btrfs inode item.
///
/// Contains all metadata for a file/directory/symlink in the filesystem.
#[derive(Clone)]
pub struct InodeItem {
    /// Generation when this inode was created.
    pub generation: u64,
    /// Transaction id of last modification.
    pub transid: u64,
    /// File size in bytes (for regular files).
    pub size: u64,
    /// Number of bytes used on disk (including metadata overhead).
    pub nbytes: u64,
    /// Block group hint for allocations.
    pub block_group: u64,
    /// Number of hard links.
    pub nlink: u32,
    /// Owner user ID.
    pub uid: u32,
    /// Owner group ID.
    pub gid: u32,
    /// File mode (type + permissions, same as stat.st_mode).
    pub mode: u32,
    /// Device number (for device nodes).
    pub rdev: u64,
    /// Inode flags (NODATASUM, NODATACOW, READONLY, NOCOMPRESS, PREALLOC, etc.).
    pub flags: u64,
    /// Sequence number for NFS (incremented on each change).
    pub sequence: u64,
    /// Access time.
    pub atime: BtrfsTimespec,
    /// Data modification time.
    pub ctime: BtrfsTimespec,
    /// Metadata change time.
    pub mtime: BtrfsTimespec,
    /// Creation (birth) time.
    pub otime: BtrfsTimespec,
}

/// Inode flags.
pub mod inode_flags {
    /// Do not compute checksums for this inode's data.
    pub const NODATASUM: u64 = 1 << 0;
    /// Do not COW data extents (for database files, etc.).
    pub const NODATACOW: u64 = 1 << 1;
    /// Read-only inode.
    pub const READONLY: u64 = 1 << 2;
    /// Do not compress data.
    pub const NOCOMPRESS: u64 = 1 << 3;
    /// Preallocated extents (fallocate).
    pub const PREALLOC: u64 = 1 << 4;
    /// Do not auto-defragment.
    pub const NODEFRAG: u64 = 1 << 5;
    /// Inode has been relocated.
    pub const RELOCATED: u64 = 1 << 6;
    /// Compression is enabled.
    pub const COMPRESS: u64 = 1 << 7;
}

impl InodeItem {
    /// Parse an inode item from the raw item data (160 bytes).
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < BTRFS_INODE_ITEM_SIZE {
            log::error!("[btrfs::inode] buffer too small: {} bytes (need >= {})", buf.len(), BTRFS_INODE_ITEM_SIZE);
            return None;
        }

        let inode = InodeItem {
            generation:  read_u64(buf, 0),
            transid:     read_u64(buf, 8),
            size:        read_u64(buf, 16),
            nbytes:      read_u64(buf, 24),
            block_group: read_u64(buf, 32),
            nlink:       read_u32(buf, 40),
            uid:         read_u32(buf, 44),
            gid:         read_u32(buf, 48),
            mode:        read_u32(buf, 52),
            rdev:        read_u64(buf, 56),
            flags:       read_u64(buf, 64),
            sequence:    read_u64(buf, 72),
            // 4 reserved u64s at offsets 80..112 (32 bytes reserved)
            atime:       BtrfsTimespec::from_bytes(&buf[112..124]).unwrap_or_default(),
            ctime:       BtrfsTimespec::from_bytes(&buf[124..136]).unwrap_or_default(),
            mtime:       BtrfsTimespec::from_bytes(&buf[136..148]).unwrap_or_default(),
            otime:       BtrfsTimespec::from_bytes(&buf[148..160]).unwrap_or_default(),
        };

        log::trace!("[btrfs::inode] parsed: mode=0o{:06o}, size={}, nlink={}, uid={}, gid={}, gen={}",
            inode.mode, inode.size, inode.nlink, inode.uid, inode.gid, inode.generation);
        log::trace!("[btrfs::inode] flags=0x{:016X}, nbytes={}, rdev={}", inode.flags, inode.nbytes, inode.rdev);

        Some(inode)
    }

    /// Serialize this inode item to a 160-byte buffer.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = alloc::vec![0u8; BTRFS_INODE_ITEM_SIZE];

        write_u64(&mut buf, 0, self.generation);
        write_u64(&mut buf, 8, self.transid);
        write_u64(&mut buf, 16, self.size);
        write_u64(&mut buf, 24, self.nbytes);
        write_u64(&mut buf, 32, self.block_group);
        write_u32(&mut buf, 40, self.nlink);
        write_u32(&mut buf, 44, self.uid);
        write_u32(&mut buf, 48, self.gid);
        write_u32(&mut buf, 52, self.mode);
        write_u64(&mut buf, 56, self.rdev);
        write_u64(&mut buf, 64, self.flags);
        write_u64(&mut buf, 72, self.sequence);
        // 32 bytes of reserved zeros at 80..112
        buf[112..124].copy_from_slice(&self.atime.to_bytes());
        buf[124..136].copy_from_slice(&self.ctime.to_bytes());
        buf[136..148].copy_from_slice(&self.mtime.to_bytes());
        buf[148..160].copy_from_slice(&self.otime.to_bytes());

        log::trace!("[btrfs::inode] serialized {} bytes", buf.len());
        buf
    }

    /// File type from the mode field.
    #[inline]
    pub fn file_type(&self) -> u32 {
        self.mode & S_IFMT
    }

    /// Whether this is a regular file.
    #[inline]
    pub fn is_file(&self) -> bool {
        self.file_type() == S_IFREG
    }

    /// Whether this is a directory.
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.file_type() == S_IFDIR
    }

    /// Whether this is a symbolic link.
    #[inline]
    pub fn is_symlink(&self) -> bool {
        self.file_type() == S_IFLNK
    }

    /// Permission bits (lower 12 bits of mode).
    #[inline]
    pub fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }

    /// Create a new inode item for a regular file.
    pub fn new_file(mode_perms: u32, uid: u32, gid: u32, generation: u64, now: BtrfsTimespec) -> Self {
        log::debug!("[btrfs::inode] creating new file inode: mode=0o{:04o}, uid={}, gid={}, gen={}",
            mode_perms, uid, gid, generation);
        InodeItem {
            generation,
            transid: generation,
            size: 0,
            nbytes: 0,
            block_group: 0,
            nlink: 1,
            uid,
            gid,
            mode: S_IFREG | (mode_perms & 0o7777),
            rdev: 0,
            flags: 0,
            sequence: 0,
            atime: now,
            ctime: now,
            mtime: now,
            otime: now,
        }
    }

    /// Create a new inode item for a directory.
    pub fn new_dir(mode_perms: u32, uid: u32, gid: u32, generation: u64, now: BtrfsTimespec) -> Self {
        log::debug!("[btrfs::inode] creating new directory inode: mode=0o{:04o}, uid={}, gid={}, gen={}",
            mode_perms, uid, gid, generation);
        InodeItem {
            generation,
            transid: generation,
            size: 0,
            nbytes: 0,
            block_group: 0,
            nlink: 1,
            uid,
            gid,
            mode: S_IFDIR | (mode_perms & 0o7777),
            rdev: 0,
            flags: 0,
            sequence: 0,
            atime: now,
            ctime: now,
            mtime: now,
            otime: now,
        }
    }
}

impl fmt::Debug for InodeItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let type_str = match self.file_type() {
            S_IFREG => "file",
            S_IFDIR => "dir",
            S_IFLNK => "symlink",
            S_IFBLK => "block",
            S_IFCHR => "char",
            S_IFIFO => "fifo",
            S_IFSOCK => "socket",
            _ => "unknown",
        };
        f.debug_struct("InodeItem")
            .field("type", &type_str)
            .field("mode", &format_args!("0o{:06o}", self.mode))
            .field("size", &self.size)
            .field("nlink", &self.nlink)
            .field("uid", &self.uid)
            .field("gid", &self.gid)
            .field("generation", &self.generation)
            .field("flags", &format_args!("0x{:016X}", self.flags))
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
fn read_i64(buf: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes([
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
