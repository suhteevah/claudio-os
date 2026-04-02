//! ext4 inode structure parsing and writing.
//!
//! Each file, directory, symlink, device node, etc. in ext4 is represented by an inode.
//! The inode contains metadata (permissions, timestamps, size) and either direct/indirect
//! block pointers (legacy) or an extent tree (modern ext4).
//!
//! On-disk, the inode is stored in the inode table within each block group.
//! The inode number is 1-based; inode 0 does not exist.
//!
//! Reference: <https://ext4.wiki.kernel.org/index.php/Ext4_Disk_Layout#Inode_Table>

use alloc::vec::Vec;
use core::fmt;

use crate::extent::ExtentHeader;

/// Inode number for the root directory, always 2.
pub const ROOT_INODE: u32 = 2;

/// Size of the original ext2 inode (128 bytes).
pub const INODE_BASE_SIZE: usize = 128;

/// Default ext4 inode size (256 bytes, with extra space for extended attributes).
pub const INODE_DEFAULT_SIZE: usize = 256;

// --- File mode constants (from Linux stat.h) ---

/// Socket.
pub const S_IFSOCK: u16 = 0xC000;
/// Symbolic link.
pub const S_IFLNK: u16 = 0xA000;
/// Regular file.
pub const S_IFREG: u16 = 0x8000;
/// Block device.
pub const S_IFBLK: u16 = 0x6000;
/// Directory.
pub const S_IFDIR: u16 = 0x4000;
/// Character device.
pub const S_IFCHR: u16 = 0x2000;
/// FIFO (named pipe).
pub const S_IFIFO: u16 = 0x1000;
/// File type mask.
pub const S_IFMT: u16 = 0xF000;

// --- Inode flags ---

/// Secure deletion (unused).
pub const EXT4_SECRM_FL: u32 = 0x00000001;
/// Undelete (unused).
pub const EXT4_UNRM_FL: u32 = 0x00000002;
/// Compress file.
pub const EXT4_COMPR_FL: u32 = 0x00000004;
/// Synchronous updates.
pub const EXT4_SYNC_FL: u32 = 0x00000008;
/// Immutable file.
pub const EXT4_IMMUTABLE_FL: u32 = 0x00000010;
/// Append only.
pub const EXT4_APPEND_FL: u32 = 0x00000020;
/// Do not dump.
pub const EXT4_NODUMP_FL: u32 = 0x00000040;
/// Do not update atime.
pub const EXT4_NOATIME_FL: u32 = 0x00000080;
/// Inode uses extents.
pub const EXT4_EXTENTS_FL: u32 = 0x00080000;
/// Inode stores large extended attribute values in its data blocks.
pub const EXT4_EA_INODE_FL: u32 = 0x00200000;
/// Inode has inline data.
pub const EXT4_INLINE_DATA_FL: u32 = 0x10000000;

/// Number of direct block pointers in legacy (non-extent) mode.
pub const DIRECT_BLOCKS: usize = 12;
/// Index of the indirect block pointer.
pub const INDIRECT_BLOCK: usize = 12;
/// Index of the double-indirect block pointer.
pub const DOUBLE_INDIRECT_BLOCK: usize = 13;
/// Index of the triple-indirect block pointer.
pub const TRIPLE_INDIRECT_BLOCK: usize = 14;
/// Total number of block pointer slots in the inode.
pub const BLOCK_POINTERS: usize = 15;

/// Size of the i_block area in the inode (60 bytes = 15 u32s or extent tree root).
pub const I_BLOCK_SIZE: usize = 60;

/// Parsed ext4 inode.
#[derive(Clone)]
pub struct Inode {
    /// File mode: type (upper 4 bits) + permissions (lower 12 bits).
    pub mode: u16,
    /// Owner UID (low 16 bits).
    pub uid: u16,
    /// File size in bytes (low 32 bits).
    pub size_lo: u32,
    /// Last access time (UNIX timestamp).
    pub atime: u32,
    /// Inode change time (UNIX timestamp).
    pub ctime: u32,
    /// Last modification time (UNIX timestamp).
    pub mtime: u32,
    /// Deletion time (UNIX timestamp, 0 if not deleted).
    pub dtime: u32,
    /// Group GID (low 16 bits).
    pub gid: u16,
    /// Hard link count.
    pub links_count: u16,
    /// Number of 512-byte blocks allocated (low 32 bits).
    pub blocks_lo: u32,
    /// Inode flags (EXT4_EXTENTS_FL, etc.).
    pub flags: u32,
    /// OS-specific value 1.
    pub osd1: u32,
    /// Block pointers or extent tree root (60 bytes).
    /// In extent mode, this contains an ExtentHeader followed by extent entries.
    /// In legacy mode, this contains 15 u32 block pointers.
    pub i_block: [u8; I_BLOCK_SIZE],
    /// File version (for NFS).
    pub generation: u32,
    /// File ACL (low 32 bits).
    pub file_acl_lo: u32,
    /// File size in bytes (high 32 bits) / directory ACL.
    pub size_hi: u32,
    /// Fragment address (obsolete).
    pub obso_faddr: u32,

    // --- Extended fields (inode_size > 128) ---
    /// Number of 512-byte blocks allocated (high 16 bits).
    pub blocks_hi: u16,
    /// File ACL (high 16 bits).
    pub file_acl_hi: u16,
    /// Owner UID (high 16 bits).
    pub uid_hi: u16,
    /// Group GID (high 16 bits).
    pub gid_hi: u16,
    /// Inode checksum (low 16 bits).
    pub checksum_lo: u16,
    /// Extra inode size (bytes of valid fields beyond 128).
    pub extra_isize: u16,
    /// Inode checksum (high 16 bits).
    pub checksum_hi: u16,
    /// Extra change time (nanoseconds + epoch bits).
    pub ctime_extra: u32,
    /// Extra modification time.
    pub mtime_extra: u32,
    /// Extra access time.
    pub atime_extra: u32,
    /// File creation time (UNIX timestamp).
    pub crtime: u32,
    /// File creation time (extra nanoseconds + epoch).
    pub crtime_extra: u32,
    /// Version (high 32 bits).
    pub version_hi: u32,
}

impl Inode {
    /// Parse an inode from raw bytes read from the inode table.
    ///
    /// `inode_size` should come from the superblock (typically 256).
    /// The buffer must be at least `INODE_BASE_SIZE` (128) bytes.
    pub fn from_bytes(buf: &[u8], inode_size: usize) -> Option<Self> {
        if buf.len() < INODE_BASE_SIZE {
            log::error!("[ext4::inode] buffer too small: {} bytes (need >= {})", buf.len(), INODE_BASE_SIZE);
            return None;
        }

        let mut i_block = [0u8; I_BLOCK_SIZE];
        i_block.copy_from_slice(&buf[0x28..0x28 + I_BLOCK_SIZE]);

        let has_extra = inode_size > INODE_BASE_SIZE && buf.len() >= INODE_DEFAULT_SIZE;

        let inode = Inode {
            mode:         read_u16(buf, 0x00),
            uid:          read_u16(buf, 0x02),
            size_lo:      read_u32(buf, 0x04),
            atime:        read_u32(buf, 0x08),
            ctime:        read_u32(buf, 0x0C),
            mtime:        read_u32(buf, 0x10),
            dtime:        read_u32(buf, 0x14),
            gid:          read_u16(buf, 0x18),
            links_count:  read_u16(buf, 0x1A),
            blocks_lo:    read_u32(buf, 0x1C),
            flags:        read_u32(buf, 0x20),
            osd1:         read_u32(buf, 0x24),
            i_block,
            generation:   read_u32(buf, 0x64),
            file_acl_lo:  read_u32(buf, 0x68),
            size_hi:      read_u32(buf, 0x6C),
            obso_faddr:   read_u32(buf, 0x70),

            // OS-specific block 2 (bytes 0x74..0x80)
            blocks_hi:     if has_extra { read_u16(buf, 0x74) } else { 0 },
            file_acl_hi:   if has_extra { read_u16(buf, 0x76) } else { 0 },
            uid_hi:        if has_extra { read_u16(buf, 0x78) } else { 0 },
            gid_hi:        if has_extra { read_u16(buf, 0x7A) } else { 0 },
            checksum_lo:   if has_extra { read_u16(buf, 0x7C) } else { 0 },
            extra_isize:   if has_extra && buf.len() >= 0x82 { read_u16(buf, 0x80) } else { 0 },
            checksum_hi:   if has_extra && buf.len() >= 0x84 { read_u16(buf, 0x82) } else { 0 },
            ctime_extra:   if has_extra && buf.len() >= 0x88 { read_u32(buf, 0x84) } else { 0 },
            mtime_extra:   if has_extra && buf.len() >= 0x8C { read_u32(buf, 0x88) } else { 0 },
            atime_extra:   if has_extra && buf.len() >= 0x90 { read_u32(buf, 0x8C) } else { 0 },
            crtime:        if has_extra && buf.len() >= 0x94 { read_u32(buf, 0x90) } else { 0 },
            crtime_extra:  if has_extra && buf.len() >= 0x98 { read_u32(buf, 0x94) } else { 0 },
            version_hi:    if has_extra && buf.len() >= 0x9C { read_u32(buf, 0x98) } else { 0 },
        };

        log::trace!("[ext4::inode] parsed: mode=0o{:06o}, size={}, links={}, flags=0x{:08X}",
            inode.mode, inode.size(), inode.links_count, inode.flags);

        Some(inode)
    }

    /// Serialize this inode into bytes for writing to the inode table.
    pub fn to_bytes(&self, inode_size: usize) -> Vec<u8> {
        let size = inode_size.max(INODE_BASE_SIZE);
        let mut buf = alloc::vec![0u8; size];

        write_u16(&mut buf, 0x00, self.mode);
        write_u16(&mut buf, 0x02, self.uid);
        write_u32(&mut buf, 0x04, self.size_lo);
        write_u32(&mut buf, 0x08, self.atime);
        write_u32(&mut buf, 0x0C, self.ctime);
        write_u32(&mut buf, 0x10, self.mtime);
        write_u32(&mut buf, 0x14, self.dtime);
        write_u16(&mut buf, 0x18, self.gid);
        write_u16(&mut buf, 0x1A, self.links_count);
        write_u32(&mut buf, 0x1C, self.blocks_lo);
        write_u32(&mut buf, 0x20, self.flags);
        write_u32(&mut buf, 0x24, self.osd1);
        buf[0x28..0x28 + I_BLOCK_SIZE].copy_from_slice(&self.i_block);
        write_u32(&mut buf, 0x64, self.generation);
        write_u32(&mut buf, 0x68, self.file_acl_lo);
        write_u32(&mut buf, 0x6C, self.size_hi);
        write_u32(&mut buf, 0x70, self.obso_faddr);

        if size >= INODE_DEFAULT_SIZE {
            write_u16(&mut buf, 0x74, self.blocks_hi);
            write_u16(&mut buf, 0x76, self.file_acl_hi);
            write_u16(&mut buf, 0x78, self.uid_hi);
            write_u16(&mut buf, 0x7A, self.gid_hi);
            write_u16(&mut buf, 0x7C, self.checksum_lo);
            write_u16(&mut buf, 0x80, self.extra_isize);
            write_u16(&mut buf, 0x82, self.checksum_hi);
            write_u32(&mut buf, 0x84, self.ctime_extra);
            write_u32(&mut buf, 0x88, self.mtime_extra);
            write_u32(&mut buf, 0x8C, self.atime_extra);
            write_u32(&mut buf, 0x90, self.crtime);
            write_u32(&mut buf, 0x94, self.crtime_extra);
            write_u32(&mut buf, 0x98, self.version_hi);
        }

        log::trace!("[ext4::inode] serialized {} bytes", buf.len());
        buf
    }

    /// Full 64-bit file size.
    #[inline]
    pub fn size(&self) -> u64 {
        self.size_lo as u64 | ((self.size_hi as u64) << 32)
    }

    /// Set the file size (splits into lo/hi).
    pub fn set_size(&mut self, size: u64) {
        self.size_lo = size as u32;
        self.size_hi = (size >> 32) as u32;
        log::trace!("[ext4::inode] set size={}", size);
    }

    /// Full 32-bit UID (combining lo and hi).
    #[inline]
    pub fn uid_full(&self) -> u32 {
        self.uid as u32 | ((self.uid_hi as u32) << 16)
    }

    /// Full 32-bit GID (combining lo and hi).
    #[inline]
    pub fn gid_full(&self) -> u32 {
        self.gid as u32 | ((self.gid_hi as u32) << 16)
    }

    /// Whether this inode uses the extent tree (EXT4_EXTENTS_FL).
    #[inline]
    pub fn uses_extents(&self) -> bool {
        self.flags & EXT4_EXTENTS_FL != 0
    }

    /// File type extracted from the mode field.
    #[inline]
    pub fn file_type(&self) -> u16 {
        self.mode & S_IFMT
    }

    /// Whether this inode is a regular file.
    #[inline]
    pub fn is_file(&self) -> bool {
        self.file_type() == S_IFREG
    }

    /// Whether this inode is a directory.
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.file_type() == S_IFDIR
    }

    /// Whether this inode is a symbolic link.
    #[inline]
    pub fn is_symlink(&self) -> bool {
        self.file_type() == S_IFLNK
    }

    /// Permission bits (lower 12 bits of mode).
    #[inline]
    pub fn permissions(&self) -> u16 {
        self.mode & 0x0FFF
    }

    /// Get the legacy direct block pointer at the given index (0..11).
    /// Only valid when `uses_extents()` is false.
    pub fn direct_block(&self, index: usize) -> u32 {
        if index >= DIRECT_BLOCKS {
            return 0;
        }
        let off = index * 4;
        u32::from_le_bytes([
            self.i_block[off],
            self.i_block[off + 1],
            self.i_block[off + 2],
            self.i_block[off + 3],
        ])
    }

    /// Get the indirect block pointer. Only valid when `uses_extents()` is false.
    pub fn indirect_block(&self) -> u32 {
        let off = INDIRECT_BLOCK * 4;
        u32::from_le_bytes([
            self.i_block[off],
            self.i_block[off + 1],
            self.i_block[off + 2],
            self.i_block[off + 3],
        ])
    }

    /// Get the double-indirect block pointer.
    pub fn double_indirect_block(&self) -> u32 {
        let off = DOUBLE_INDIRECT_BLOCK * 4;
        u32::from_le_bytes([
            self.i_block[off],
            self.i_block[off + 1],
            self.i_block[off + 2],
            self.i_block[off + 3],
        ])
    }

    /// Get the triple-indirect block pointer.
    pub fn triple_indirect_block(&self) -> u32 {
        let off = TRIPLE_INDIRECT_BLOCK * 4;
        u32::from_le_bytes([
            self.i_block[off],
            self.i_block[off + 1],
            self.i_block[off + 2],
            self.i_block[off + 3],
        ])
    }

    /// Parse the extent tree header from the i_block area.
    /// Returns `None` if this inode does not use extents or the header is invalid.
    pub fn extent_header(&self) -> Option<ExtentHeader> {
        if !self.uses_extents() {
            return None;
        }
        ExtentHeader::from_bytes(&self.i_block)
    }

    /// Create a new empty inode with sensible defaults for a regular file.
    pub fn new_file(mode_perms: u16, uid: u32, gid: u32, now: u32) -> Self {
        log::debug!("[ext4::inode] creating new file inode: mode=0o{:04o}, uid={}, gid={}", mode_perms, uid, gid);
        let mut inode = Self::zeroed();
        inode.mode = S_IFREG | (mode_perms & 0x0FFF);
        inode.uid = uid as u16;
        inode.uid_hi = (uid >> 16) as u16;
        inode.gid = gid as u16;
        inode.gid_hi = (gid >> 16) as u16;
        inode.atime = now;
        inode.ctime = now;
        inode.mtime = now;
        inode.crtime = now;
        inode.links_count = 1;
        // Set extents flag and initialize empty extent tree
        inode.flags = EXT4_EXTENTS_FL;
        inode.init_extent_tree();
        inode
    }

    /// Create a new empty inode for a directory.
    pub fn new_dir(mode_perms: u16, uid: u32, gid: u32, now: u32) -> Self {
        log::debug!("[ext4::inode] creating new directory inode: mode=0o{:04o}, uid={}, gid={}", mode_perms, uid, gid);
        let mut inode = Self::zeroed();
        inode.mode = S_IFDIR | (mode_perms & 0x0FFF);
        inode.uid = uid as u16;
        inode.uid_hi = (uid >> 16) as u16;
        inode.gid = gid as u16;
        inode.gid_hi = (gid >> 16) as u16;
        inode.atime = now;
        inode.ctime = now;
        inode.mtime = now;
        inode.crtime = now;
        inode.links_count = 2; // . and parent's link
        inode.flags = EXT4_EXTENTS_FL;
        inode.init_extent_tree();
        inode
    }

    /// Initialize an empty extent tree in the i_block area.
    fn init_extent_tree(&mut self) {
        // Write extent header: magic=0xF30A, entries=0, max=4, depth=0, generation=0
        let header = ExtentHeader {
            magic: crate::extent::EXT4_EXTENT_MAGIC,
            entries: 0,
            max: 4, // root node can hold 4 extents (60 bytes - 12 header = 48 / 12 per extent)
            depth: 0,
            generation: 0,
        };
        let hdr_bytes = header.to_bytes();
        self.i_block[..12].copy_from_slice(&hdr_bytes);
        log::trace!("[ext4::inode] initialized empty extent tree in i_block");
    }

    /// Create a zeroed inode.
    fn zeroed() -> Self {
        Inode {
            mode: 0, uid: 0, size_lo: 0, atime: 0, ctime: 0, mtime: 0, dtime: 0,
            gid: 0, links_count: 0, blocks_lo: 0, flags: 0, osd1: 0,
            i_block: [0; I_BLOCK_SIZE], generation: 0, file_acl_lo: 0, size_hi: 0,
            obso_faddr: 0, blocks_hi: 0, file_acl_hi: 0, uid_hi: 0, gid_hi: 0,
            checksum_lo: 0, extra_isize: 32, checksum_hi: 0,
            ctime_extra: 0, mtime_extra: 0, atime_extra: 0,
            crtime: 0, crtime_extra: 0, version_hi: 0,
        }
    }
}

impl fmt::Debug for Inode {
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
        f.debug_struct("Inode")
            .field("type", &type_str)
            .field("mode", &format_args!("0o{:06o}", self.mode))
            .field("size", &self.size())
            .field("links", &self.links_count)
            .field("uid", &self.uid_full())
            .field("gid", &self.gid_full())
            .field("flags", &format_args!("0x{:08X}", self.flags))
            .field("extents", &self.uses_extents())
            .finish()
    }
}

// --- Little-endian byte helpers ---

#[inline]
fn read_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

#[inline]
fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3]])
}

#[inline]
fn write_u16(buf: &mut [u8], offset: usize, val: u16) {
    buf[offset..offset + 2].copy_from_slice(&val.to_le_bytes());
}

#[inline]
fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}
