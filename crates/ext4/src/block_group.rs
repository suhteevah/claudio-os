//! Block group descriptor table parsing and management.
//!
//! ext4 divides the filesystem into block groups, each described by a block group
//! descriptor. The descriptor table follows the superblock (or its backup copies)
//! and contains one entry per block group.
//!
//! In 32-bit mode, each descriptor is 32 bytes. In 64-bit mode (INCOMPAT_64BIT),
//! each descriptor is `desc_size` bytes (typically 64).
//!
//! Reference: <https://ext4.wiki.kernel.org/index.php/Ext4_Disk_Layout#Block_Group_Descriptors>

use alloc::vec::Vec;
use core::fmt;

/// Size of a standard 32-bit block group descriptor.
pub const BGD_SIZE_32: usize = 32;

/// Size of a 64-bit block group descriptor.
pub const BGD_SIZE_64: usize = 64;

/// A single block group descriptor.
///
/// Tracks the location of bitmaps, inode table, and free counts for one block group.
#[derive(Clone)]
pub struct BlockGroupDesc {
    /// Block number of the block bitmap (low 32 bits).
    pub block_bitmap_lo: u32,
    /// Block number of the inode bitmap (low 32 bits).
    pub inode_bitmap_lo: u32,
    /// Block number of the first block of the inode table (low 32 bits).
    pub inode_table_lo: u32,
    /// Number of free blocks in this group (low 16 bits).
    pub free_blocks_count_lo: u16,
    /// Number of free inodes in this group (low 16 bits).
    pub free_inodes_count_lo: u16,
    /// Number of directories in this group (low 16 bits).
    pub used_dirs_count_lo: u16,
    /// Block group flags.
    pub flags: u16,
    /// Location of snapshot exclusion bitmap (low 32 bits).
    pub exclude_bitmap_lo: u32,
    /// Block bitmap checksum (low 16 bits).
    pub block_bitmap_csum_lo: u16,
    /// Inode bitmap checksum (low 16 bits).
    pub inode_bitmap_csum_lo: u16,
    /// Number of unused inodes in this group (low 16 bits).
    pub itable_unused_lo: u16,
    /// Group descriptor checksum.
    pub checksum: u16,

    // --- 64-bit extension fields (only valid if desc_size >= 64) ---
    /// Block bitmap block (high 32 bits).
    pub block_bitmap_hi: u32,
    /// Inode bitmap block (high 32 bits).
    pub inode_bitmap_hi: u32,
    /// Inode table block (high 32 bits).
    pub inode_table_hi: u32,
    /// Free blocks count (high 16 bits).
    pub free_blocks_count_hi: u16,
    /// Free inodes count (high 16 bits).
    pub free_inodes_count_hi: u16,
    /// Used directories count (high 16 bits).
    pub used_dirs_count_hi: u16,
    /// Unused inodes count (high 16 bits).
    pub itable_unused_hi: u16,
    /// Exclude bitmap block (high 32 bits).
    pub exclude_bitmap_hi: u32,
    /// Block bitmap checksum (high 16 bits).
    pub block_bitmap_csum_hi: u16,
    /// Inode bitmap checksum (high 16 bits).
    pub inode_bitmap_csum_hi: u16,
}

impl BlockGroupDesc {
    /// Parse a block group descriptor from a byte buffer.
    ///
    /// `desc_size` determines whether 64-bit extension fields are read.
    /// For 32-bit mode, pass 32. For 64-bit mode, pass the superblock's `desc_size`.
    pub fn from_bytes(buf: &[u8], desc_size: usize) -> Option<Self> {
        if buf.len() < BGD_SIZE_32 {
            log::error!("[ext4::block_group] buffer too small: {} bytes (need >= {})", buf.len(), BGD_SIZE_32);
            return None;
        }

        let bgd = BlockGroupDesc {
            block_bitmap_lo:       read_u32(buf, 0x00),
            inode_bitmap_lo:       read_u32(buf, 0x04),
            inode_table_lo:        read_u32(buf, 0x08),
            free_blocks_count_lo:  read_u16(buf, 0x0C),
            free_inodes_count_lo:  read_u16(buf, 0x0E),
            used_dirs_count_lo:    read_u16(buf, 0x10),
            flags:                 read_u16(buf, 0x12),
            exclude_bitmap_lo:     read_u32(buf, 0x14),
            block_bitmap_csum_lo:  read_u16(buf, 0x18),
            inode_bitmap_csum_lo:  read_u16(buf, 0x1A),
            itable_unused_lo:      read_u16(buf, 0x1C),
            checksum:              read_u16(buf, 0x1E),

            // 64-bit fields
            block_bitmap_hi:       if desc_size >= BGD_SIZE_64 && buf.len() >= BGD_SIZE_64 { read_u32(buf, 0x20) } else { 0 },
            inode_bitmap_hi:       if desc_size >= BGD_SIZE_64 && buf.len() >= BGD_SIZE_64 { read_u32(buf, 0x24) } else { 0 },
            inode_table_hi:        if desc_size >= BGD_SIZE_64 && buf.len() >= BGD_SIZE_64 { read_u32(buf, 0x28) } else { 0 },
            free_blocks_count_hi:  if desc_size >= BGD_SIZE_64 && buf.len() >= BGD_SIZE_64 { read_u16(buf, 0x2C) } else { 0 },
            free_inodes_count_hi:  if desc_size >= BGD_SIZE_64 && buf.len() >= BGD_SIZE_64 { read_u16(buf, 0x2E) } else { 0 },
            used_dirs_count_hi:    if desc_size >= BGD_SIZE_64 && buf.len() >= BGD_SIZE_64 { read_u16(buf, 0x30) } else { 0 },
            itable_unused_hi:      if desc_size >= BGD_SIZE_64 && buf.len() >= BGD_SIZE_64 { read_u16(buf, 0x32) } else { 0 },
            exclude_bitmap_hi:     if desc_size >= BGD_SIZE_64 && buf.len() >= BGD_SIZE_64 { read_u32(buf, 0x34) } else { 0 },
            block_bitmap_csum_hi:  if desc_size >= BGD_SIZE_64 && buf.len() >= BGD_SIZE_64 { read_u16(buf, 0x38) } else { 0 },
            inode_bitmap_csum_hi:  if desc_size >= BGD_SIZE_64 && buf.len() >= BGD_SIZE_64 { read_u16(buf, 0x3A) } else { 0 },
        };

        log::trace!("[ext4::block_group] parsed: block_bitmap={}, inode_bitmap={}, inode_table={}, free_blocks={}, free_inodes={}",
            bgd.block_bitmap(), bgd.inode_bitmap(), bgd.inode_table(),
            bgd.free_blocks_count(), bgd.free_inodes_count());

        Some(bgd)
    }

    /// Serialize this block group descriptor into bytes.
    ///
    /// `desc_size` controls the output length (32 or 64).
    pub fn to_bytes(&self, desc_size: usize) -> Vec<u8> {
        let size = if desc_size >= BGD_SIZE_64 { BGD_SIZE_64 } else { BGD_SIZE_32 };
        let mut buf = alloc::vec![0u8; size];

        write_u32(&mut buf, 0x00, self.block_bitmap_lo);
        write_u32(&mut buf, 0x04, self.inode_bitmap_lo);
        write_u32(&mut buf, 0x08, self.inode_table_lo);
        write_u16(&mut buf, 0x0C, self.free_blocks_count_lo);
        write_u16(&mut buf, 0x0E, self.free_inodes_count_lo);
        write_u16(&mut buf, 0x10, self.used_dirs_count_lo);
        write_u16(&mut buf, 0x12, self.flags);
        write_u32(&mut buf, 0x14, self.exclude_bitmap_lo);
        write_u16(&mut buf, 0x18, self.block_bitmap_csum_lo);
        write_u16(&mut buf, 0x1A, self.inode_bitmap_csum_lo);
        write_u16(&mut buf, 0x1C, self.itable_unused_lo);
        write_u16(&mut buf, 0x1E, self.checksum);

        if size >= BGD_SIZE_64 {
            write_u32(&mut buf, 0x20, self.block_bitmap_hi);
            write_u32(&mut buf, 0x24, self.inode_bitmap_hi);
            write_u32(&mut buf, 0x28, self.inode_table_hi);
            write_u16(&mut buf, 0x2C, self.free_blocks_count_hi);
            write_u16(&mut buf, 0x2E, self.free_inodes_count_hi);
            write_u16(&mut buf, 0x30, self.used_dirs_count_hi);
            write_u16(&mut buf, 0x32, self.itable_unused_hi);
            write_u32(&mut buf, 0x34, self.exclude_bitmap_hi);
            write_u16(&mut buf, 0x38, self.block_bitmap_csum_hi);
            write_u16(&mut buf, 0x3A, self.inode_bitmap_csum_hi);
        }

        log::trace!("[ext4::block_group] serialized {} bytes", buf.len());
        buf
    }

    /// Full 64-bit block bitmap block number.
    #[inline]
    pub fn block_bitmap(&self) -> u64 {
        self.block_bitmap_lo as u64 | ((self.block_bitmap_hi as u64) << 32)
    }

    /// Full 64-bit inode bitmap block number.
    #[inline]
    pub fn inode_bitmap(&self) -> u64 {
        self.inode_bitmap_lo as u64 | ((self.inode_bitmap_hi as u64) << 32)
    }

    /// Full 64-bit inode table start block number.
    #[inline]
    pub fn inode_table(&self) -> u64 {
        self.inode_table_lo as u64 | ((self.inode_table_hi as u64) << 32)
    }

    /// Total free blocks count (combining lo and hi).
    #[inline]
    pub fn free_blocks_count(&self) -> u32 {
        self.free_blocks_count_lo as u32 | ((self.free_blocks_count_hi as u32) << 16)
    }

    /// Total free inodes count (combining lo and hi).
    #[inline]
    pub fn free_inodes_count(&self) -> u32 {
        self.free_inodes_count_lo as u32 | ((self.free_inodes_count_hi as u32) << 16)
    }

    /// Total used directories count (combining lo and hi).
    #[inline]
    pub fn used_dirs_count(&self) -> u32 {
        self.used_dirs_count_lo as u32 | ((self.used_dirs_count_hi as u32) << 16)
    }

    /// Set the free blocks count (splits into lo and hi).
    pub fn set_free_blocks_count(&mut self, count: u32) {
        self.free_blocks_count_lo = count as u16;
        self.free_blocks_count_hi = (count >> 16) as u16;
        log::trace!("[ext4::block_group] set free_blocks_count={}", count);
    }

    /// Set the free inodes count (splits into lo and hi).
    pub fn set_free_inodes_count(&mut self, count: u32) {
        self.free_inodes_count_lo = count as u16;
        self.free_inodes_count_hi = (count >> 16) as u16;
        log::trace!("[ext4::block_group] set free_inodes_count={}", count);
    }
}

impl fmt::Debug for BlockGroupDesc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockGroupDesc")
            .field("block_bitmap", &self.block_bitmap())
            .field("inode_bitmap", &self.inode_bitmap())
            .field("inode_table", &self.inode_table())
            .field("free_blocks", &self.free_blocks_count())
            .field("free_inodes", &self.free_inodes_count())
            .field("used_dirs", &self.used_dirs_count())
            .finish()
    }
}

/// Parse the entire block group descriptor table from a buffer.
///
/// The buffer should contain the raw bytes starting at the block group descriptor
/// table offset (the block following the superblock, typically block 1 for 4K blocks
/// or block 2 for 1K blocks).
///
/// Returns a Vec of all block group descriptors.
pub fn parse_block_group_table(buf: &[u8], count: u32, desc_size: usize) -> Vec<BlockGroupDesc> {
    log::info!("[ext4::block_group] parsing {} block group descriptors (desc_size={})", count, desc_size);
    let mut groups = Vec::with_capacity(count as usize);

    for i in 0..count as usize {
        let offset = i * desc_size;
        if offset + desc_size > buf.len() {
            log::warn!("[ext4::block_group] truncated at group {} (offset {} + {} > {})",
                i, offset, desc_size, buf.len());
            break;
        }
        match BlockGroupDesc::from_bytes(&buf[offset..], desc_size) {
            Some(bgd) => {
                log::trace!("[ext4::block_group] group {}: {:?}", i, bgd);
                groups.push(bgd);
            }
            None => {
                log::error!("[ext4::block_group] failed to parse group {}", i);
                break;
            }
        }
    }

    log::info!("[ext4::block_group] parsed {} block group descriptors", groups.len());
    groups
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
