//! ext4 superblock parsing.
//!
//! The superblock is located at byte offset 1024 from the start of the partition
//! and contains all metadata about the filesystem geometry, feature flags, and state.
//!
//! Reference: <https://ext4.wiki.kernel.org/index.php/Ext4_Disk_Layout#The_Super_Block>

use alloc::vec::Vec;
use core::fmt;

/// ext4 magic number, always 0xEF53 at offset 0x38 in the superblock.
pub const EXT4_SUPER_MAGIC: u16 = 0xEF53;

/// Byte offset of the superblock from the start of the partition.
pub const SUPERBLOCK_OFFSET: u64 = 1024;

/// Size of the base superblock structure (the original ext2 portion).
pub const SUPERBLOCK_BASE_SIZE: usize = 264;

/// Size of the full ext4 superblock (1024 bytes).
pub const SUPERBLOCK_SIZE: usize = 1024;

// --- Feature flag constants ---

/// Compatible feature: directory preallocation.
pub const COMPAT_DIR_PREALLOC: u32 = 0x0001;
/// Compatible feature: imagic inodes (AFS).
pub const COMPAT_IMAGIC_INODES: u32 = 0x0002;
/// Compatible feature: has a journal (ext3/ext4).
pub const COMPAT_HAS_JOURNAL: u32 = 0x0004;
/// Compatible feature: extended attributes.
pub const COMPAT_EXT_ATTR: u32 = 0x0008;
/// Compatible feature: filesystem can resize itself for larger partitions.
pub const COMPAT_RESIZE_INODE: u32 = 0x0010;
/// Compatible feature: directory indexing (htree).
pub const COMPAT_DIR_INDEX: u32 = 0x0020;
/// Compatible feature: sparse superblock v2.
pub const COMPAT_SPARSE_SUPER2: u32 = 0x0200;

/// Incompatible feature: compression.
pub const INCOMPAT_COMPRESSION: u32 = 0x0001;
/// Incompatible feature: directory entries record the file type.
pub const INCOMPAT_FILETYPE: u32 = 0x0002;
/// Incompatible feature: filesystem needs recovery (journal replay).
pub const INCOMPAT_RECOVER: u32 = 0x0004;
/// Incompatible feature: filesystem has a separate journal device.
pub const INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
/// Incompatible feature: meta block groups.
pub const INCOMPAT_META_BG: u32 = 0x0010;
/// Incompatible feature: files use extents (critical for ext4).
pub const INCOMPAT_EXTENTS: u32 = 0x0040;
/// Incompatible feature: filesystem uses 64-bit block numbers.
pub const INCOMPAT_64BIT: u32 = 0x0080;
/// Incompatible feature: multiple mount protection.
pub const INCOMPAT_MMP: u32 = 0x0100;
/// Incompatible feature: flexible block groups.
pub const INCOMPAT_FLEX_BG: u32 = 0x0200;
/// Incompatible feature: large extended attribute values in inodes.
pub const INCOMPAT_EA_INODE: u32 = 0x0400;
/// Incompatible feature: data in directory entry.
pub const INCOMPAT_DIRDATA: u32 = 0x1000;
/// Incompatible feature: metadata checksum seed in superblock.
pub const INCOMPAT_CSUM_SEED: u32 = 0x2000;
/// Incompatible feature: large directory (> 2GB or 3-level htree).
pub const INCOMPAT_LARGEDIR: u32 = 0x4000;
/// Incompatible feature: data in inode.
pub const INCOMPAT_INLINE_DATA: u32 = 0x8000;
/// Incompatible feature: encrypted inodes.
pub const INCOMPAT_ENCRYPT: u32 = 0x10000;

/// Read-only compatible feature: sparse superblocks.
pub const RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
/// Read-only compatible feature: large files (> 2GB).
pub const RO_COMPAT_LARGE_FILE: u32 = 0x0002;
/// Read-only compatible feature: btree directories (unused).
pub const RO_COMPAT_BTREE_DIR: u32 = 0x0004;
/// Read-only compatible feature: huge files.
pub const RO_COMPAT_HUGE_FILE: u32 = 0x0008;
/// Read-only compatible feature: group descriptor checksums.
pub const RO_COMPAT_GDT_CSUM: u32 = 0x0010;
/// Read-only compatible feature: large subdirectory count.
pub const RO_COMPAT_DIR_NLINK: u32 = 0x0020;
/// Read-only compatible feature: large inodes (> 128 bytes).
pub const RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;
/// Read-only compatible feature: metadata checksums.
pub const RO_COMPAT_METADATA_CSUM: u32 = 0x0400;

/// Filesystem state: cleanly unmounted.
pub const STATE_VALID: u16 = 0x0001;
/// Filesystem state: errors detected.
pub const STATE_ERROR: u16 = 0x0002;
/// Filesystem state: orphans being recovered.
pub const STATE_ORPHAN: u16 = 0x0004;

/// Parsed ext4 superblock.
///
/// Contains all fields needed for filesystem operation. Fields are stored
/// in native endian (converted from little-endian on disk).
#[derive(Clone)]
pub struct Superblock {
    // --- Geometry ---
    /// Total number of inodes in the filesystem.
    pub inodes_count: u32,
    /// Total number of blocks (low 32 bits).
    pub blocks_count_lo: u32,
    /// Number of blocks reserved for the superuser.
    pub reserved_blocks_count_lo: u32,
    /// Number of free blocks (low 32 bits).
    pub free_blocks_count_lo: u32,
    /// Number of free inodes.
    pub free_inodes_count: u32,
    /// First data block (0 for 4K blocks, 1 for 1K blocks).
    pub first_data_block: u32,
    /// Block size = 1024 << log_block_size.
    pub log_block_size: u32,
    /// Cluster size = 1024 << log_cluster_size (bigalloc).
    pub log_cluster_size: u32,
    /// Number of blocks per block group.
    pub blocks_per_group: u32,
    /// Number of clusters per block group (bigalloc).
    pub clusters_per_group: u32,
    /// Number of inodes per block group.
    pub inodes_per_group: u32,

    // --- Timestamps ---
    /// Last mount time (UNIX timestamp).
    pub mount_time: u32,
    /// Last write time (UNIX timestamp).
    pub write_time: u32,
    /// Mount count since last fsck.
    pub mount_count: u16,
    /// Maximum mount count before fsck required.
    pub max_mount_count: u16,

    // --- Identification ---
    /// Magic number, must be EXT4_SUPER_MAGIC (0xEF53).
    pub magic: u16,
    /// Filesystem state (STATE_VALID, STATE_ERROR, etc.).
    pub state: u16,
    /// Behavior on errors (1=continue, 2=remount ro, 3=panic).
    pub errors: u16,
    /// Minor revision level.
    pub minor_rev_level: u16,

    // --- Consistency ---
    /// Time of last fsck (UNIX timestamp).
    pub last_check: u32,
    /// Maximum time between fscks.
    pub check_interval: u32,
    /// OS that created the filesystem (0=Linux).
    pub creator_os: u32,
    /// Revision level (0=original, 1=v2 with dynamic inode sizes).
    pub rev_level: u32,
    /// Default UID for reserved blocks.
    pub def_resuid: u16,
    /// Default GID for reserved blocks.
    pub def_resgid: u16,

    // --- ext4-specific (rev_level >= 1) ---
    /// First non-reserved inode.
    pub first_ino: u32,
    /// Size of each inode in bytes (128 for original, typically 256 for ext4).
    pub inode_size: u16,
    /// Block group number containing this superblock (for backups).
    pub block_group_nr: u16,
    /// Compatible feature flags.
    pub feature_compat: u32,
    /// Incompatible feature flags.
    pub feature_incompat: u32,
    /// Read-only compatible feature flags.
    pub feature_ro_compat: u32,
    /// 128-bit UUID for the filesystem.
    pub uuid: [u8; 16],
    /// Volume label (null-terminated UTF-8).
    pub volume_name: [u8; 16],
    /// Directory where last mounted (null-terminated).
    pub last_mounted: [u8; 64],

    // --- 64-bit support ---
    /// Block group descriptor size (32 or 64 bytes).
    pub desc_size: u16,
    /// Total blocks count (high 32 bits, if INCOMPAT_64BIT).
    pub blocks_count_hi: u32,
    /// Free blocks count (high 32 bits).
    pub free_blocks_count_hi: u32,
}

impl Superblock {
    /// Parse a superblock from a 1024-byte buffer read from disk.
    ///
    /// The buffer must contain exactly the bytes at partition offset 1024..2048.
    /// Returns `None` if the magic number is invalid.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < SUPERBLOCK_BASE_SIZE {
            log::error!("[ext4::superblock] buffer too small: {} bytes (need >= {})", buf.len(), SUPERBLOCK_BASE_SIZE);
            return None;
        }

        let magic = u16::from_le_bytes([buf[0x38], buf[0x39]]);
        if magic != EXT4_SUPER_MAGIC {
            log::error!("[ext4::superblock] invalid magic: 0x{:04X} (expected 0x{:04X})", magic, EXT4_SUPER_MAGIC);
            return None;
        }

        let sb = Superblock {
            inodes_count:            read_u32(buf, 0x00),
            blocks_count_lo:         read_u32(buf, 0x04),
            reserved_blocks_count_lo: read_u32(buf, 0x08),
            free_blocks_count_lo:    read_u32(buf, 0x0C),
            free_inodes_count:       read_u32(buf, 0x10),
            first_data_block:        read_u32(buf, 0x14),
            log_block_size:          read_u32(buf, 0x18),
            log_cluster_size:        read_u32(buf, 0x1C),
            blocks_per_group:        read_u32(buf, 0x20),
            clusters_per_group:      read_u32(buf, 0x24),
            inodes_per_group:        read_u32(buf, 0x28),
            mount_time:              read_u32(buf, 0x2C),
            write_time:              read_u32(buf, 0x30),
            mount_count:             read_u16(buf, 0x34),
            max_mount_count:         read_u16(buf, 0x36),
            magic,
            state:                   read_u16(buf, 0x3A),
            errors:                  read_u16(buf, 0x3C),
            minor_rev_level:         read_u16(buf, 0x3E),
            last_check:              read_u32(buf, 0x40),
            check_interval:          read_u32(buf, 0x44),
            creator_os:              read_u32(buf, 0x48),
            rev_level:               read_u32(buf, 0x4C),
            def_resuid:              read_u16(buf, 0x50),
            def_resgid:              read_u16(buf, 0x52),
            first_ino:               read_u32(buf, 0x54),
            inode_size:              read_u16(buf, 0x58),
            block_group_nr:          read_u16(buf, 0x5A),
            feature_compat:          read_u32(buf, 0x5C),
            feature_incompat:        read_u32(buf, 0x60),
            feature_ro_compat:       read_u32(buf, 0x64),
            uuid:                    read_uuid(buf, 0x68),
            volume_name:             read_16bytes(buf, 0x78),
            last_mounted:            read_64bytes(buf, 0x88),
            desc_size: if buf.len() >= 0xFE + 2 {
                read_u16(buf, 0xFE)
            } else {
                32 // default for 32-bit block group descriptors
            },
            blocks_count_hi: if buf.len() >= 0x150 + 4 {
                read_u32(buf, 0x150)
            } else {
                0
            },
            free_blocks_count_hi: if buf.len() >= 0x158 + 4 {
                read_u32(buf, 0x158)
            } else {
                0
            },
        };

        log::info!("[ext4::superblock] parsed: magic=0x{:04X}, inodes={}, blocks={}, block_size={}, inode_size={}",
            sb.magic, sb.inodes_count, sb.total_blocks(), sb.block_size(), sb.inode_size);
        log::debug!("[ext4::superblock] features: compat=0x{:08X}, incompat=0x{:08X}, ro_compat=0x{:08X}",
            sb.feature_compat, sb.feature_incompat, sb.feature_ro_compat);
        log::debug!("[ext4::superblock] blocks_per_group={}, inodes_per_group={}, first_data_block={}",
            sb.blocks_per_group, sb.inodes_per_group, sb.first_data_block);
        log::debug!("[ext4::superblock] state=0x{:04X}, rev_level={}, desc_size={}",
            sb.state, sb.rev_level, sb.desc_size);

        Some(sb)
    }

    /// Serialize the superblock back into a 1024-byte buffer for writing to disk.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = alloc::vec![0u8; SUPERBLOCK_SIZE];

        write_u32(&mut buf, 0x00, self.inodes_count);
        write_u32(&mut buf, 0x04, self.blocks_count_lo);
        write_u32(&mut buf, 0x08, self.reserved_blocks_count_lo);
        write_u32(&mut buf, 0x0C, self.free_blocks_count_lo);
        write_u32(&mut buf, 0x10, self.free_inodes_count);
        write_u32(&mut buf, 0x14, self.first_data_block);
        write_u32(&mut buf, 0x18, self.log_block_size);
        write_u32(&mut buf, 0x1C, self.log_cluster_size);
        write_u32(&mut buf, 0x20, self.blocks_per_group);
        write_u32(&mut buf, 0x24, self.clusters_per_group);
        write_u32(&mut buf, 0x28, self.inodes_per_group);
        write_u32(&mut buf, 0x2C, self.mount_time);
        write_u32(&mut buf, 0x30, self.write_time);
        write_u16(&mut buf, 0x34, self.mount_count);
        write_u16(&mut buf, 0x36, self.max_mount_count);
        write_u16(&mut buf, 0x38, self.magic);
        write_u16(&mut buf, 0x3A, self.state);
        write_u16(&mut buf, 0x3C, self.errors);
        write_u16(&mut buf, 0x3E, self.minor_rev_level);
        write_u32(&mut buf, 0x40, self.last_check);
        write_u32(&mut buf, 0x44, self.check_interval);
        write_u32(&mut buf, 0x48, self.creator_os);
        write_u32(&mut buf, 0x4C, self.rev_level);
        write_u16(&mut buf, 0x50, self.def_resuid);
        write_u16(&mut buf, 0x52, self.def_resgid);
        write_u32(&mut buf, 0x54, self.first_ino);
        write_u16(&mut buf, 0x58, self.inode_size);
        write_u16(&mut buf, 0x5A, self.block_group_nr);
        write_u32(&mut buf, 0x5C, self.feature_compat);
        write_u32(&mut buf, 0x60, self.feature_incompat);
        write_u32(&mut buf, 0x64, self.feature_ro_compat);
        buf[0x68..0x78].copy_from_slice(&self.uuid);
        buf[0x78..0x88].copy_from_slice(&self.volume_name);
        buf[0x88..0xC8].copy_from_slice(&self.last_mounted);
        write_u16(&mut buf, 0xFE, self.desc_size);
        write_u32(&mut buf, 0x150, self.blocks_count_hi);
        write_u32(&mut buf, 0x158, self.free_blocks_count_hi);

        log::trace!("[ext4::superblock] serialized {} bytes to disk format", buf.len());
        buf
    }

    /// Computed block size in bytes: `1024 << log_block_size`.
    #[inline]
    pub fn block_size(&self) -> u64 {
        1024u64 << self.log_block_size
    }

    /// Total number of blocks, combining low and high 32 bits.
    #[inline]
    pub fn total_blocks(&self) -> u64 {
        self.blocks_count_lo as u64 | ((self.blocks_count_hi as u64) << 32)
    }

    /// Total number of free blocks, combining low and high 32 bits.
    #[inline]
    pub fn free_blocks(&self) -> u64 {
        self.free_blocks_count_lo as u64 | ((self.free_blocks_count_hi as u64) << 32)
    }

    /// Number of block groups in the filesystem.
    #[inline]
    pub fn block_group_count(&self) -> u32 {
        // Round up: (total_blocks - first_data_block + blocks_per_group - 1) / blocks_per_group
        let total = self.total_blocks() - self.first_data_block as u64;
        ((total + self.blocks_per_group as u64 - 1) / self.blocks_per_group as u64) as u32
    }

    /// Size of each block group descriptor in bytes.
    /// 32 for standard, 64 for 64-bit mode.
    #[inline]
    pub fn group_desc_size(&self) -> u32 {
        if self.feature_incompat & INCOMPAT_64BIT != 0 && self.desc_size >= 64 {
            self.desc_size as u32
        } else {
            32
        }
    }

    /// Whether the filesystem uses extents (INCOMPAT_EXTENTS flag).
    #[inline]
    pub fn has_extents(&self) -> bool {
        self.feature_incompat & INCOMPAT_EXTENTS != 0
    }

    /// Whether the filesystem has a journal (COMPAT_HAS_JOURNAL flag).
    #[inline]
    pub fn has_journal(&self) -> bool {
        self.feature_compat & COMPAT_HAS_JOURNAL != 0
    }

    /// Whether the filesystem uses 64-bit block numbers.
    #[inline]
    pub fn is_64bit(&self) -> bool {
        self.feature_incompat & INCOMPAT_64BIT != 0
    }

    /// Whether directory entries contain file type information.
    #[inline]
    pub fn has_filetype(&self) -> bool {
        self.feature_incompat & INCOMPAT_FILETYPE != 0
    }

    /// Whether the filesystem uses flexible block groups.
    #[inline]
    pub fn has_flex_bg(&self) -> bool {
        self.feature_incompat & INCOMPAT_FLEX_BG != 0
    }

    /// Get the volume name as a string (trimming null bytes).
    pub fn volume_name_str(&self) -> &str {
        let end = self.volume_name.iter().position(|&b| b == 0).unwrap_or(16);
        core::str::from_utf8(&self.volume_name[..end]).unwrap_or("<invalid>")
    }
}

impl fmt::Debug for Superblock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Superblock")
            .field("magic", &format_args!("0x{:04X}", self.magic))
            .field("block_size", &self.block_size())
            .field("inode_size", &self.inode_size)
            .field("total_blocks", &self.total_blocks())
            .field("inodes_count", &self.inodes_count)
            .field("free_blocks", &self.free_blocks())
            .field("free_inodes", &self.free_inodes_count)
            .field("block_group_count", &self.block_group_count())
            .field("has_extents", &self.has_extents())
            .field("has_journal", &self.has_journal())
            .field("is_64bit", &self.is_64bit())
            .field("volume", &self.volume_name_str())
            .finish()
    }
}

// --- Little-endian byte reading helpers ---

#[inline]
fn read_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

#[inline]
fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3]])
}

#[inline]
fn read_uuid(buf: &[u8], offset: usize) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&buf[offset..offset + 16]);
    uuid
}

#[inline]
fn read_16bytes(buf: &[u8], offset: usize) -> [u8; 16] {
    let mut out = [0u8; 16];
    out.copy_from_slice(&buf[offset..offset + 16]);
    out
}

#[inline]
fn read_64bytes(buf: &[u8], offset: usize) -> [u8; 64] {
    let mut out = [0u8; 64];
    out.copy_from_slice(&buf[offset..offset + 64]);
    out
}

#[inline]
fn write_u16(buf: &mut [u8], offset: usize, val: u16) {
    buf[offset..offset + 2].copy_from_slice(&val.to_le_bytes());
}

#[inline]
fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}
