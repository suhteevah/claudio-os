//! btrfs superblock parsing.
//!
//! The primary superblock is located at byte offset 0x10000 (64 KiB) from the start
//! of the device. Mirror copies exist at 64 MiB, 256 GiB, and 1 PiB.
//!
//! The first 32 bytes of the superblock are the checksum of the remaining bytes.
//! btrfs uses CRC32C by default (csum_type == 0).
//!
//! Reference: <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html#superblock>

use alloc::vec::Vec;
use core::fmt;

/// btrfs magic number: `_BHRfS_M` in ASCII.
pub const BTRFS_MAGIC: u64 = 0x4D5F53665248425F;

/// Byte offset of the primary superblock from the start of the device.
pub const SUPERBLOCK_OFFSET: u64 = 0x10000;

/// Offsets of all superblock copies (primary + mirrors).
pub const SUPERBLOCK_OFFSETS: [u64; 4] = [
    0x10000,           // 64 KiB
    0x4000000,         // 64 MiB
    0x4000000000,      // 256 GiB
    0x4000000000000,   // 1 PiB
];

/// Size of the superblock on disk (4096 bytes, but only ~2048 used).
pub const SUPERBLOCK_SIZE: usize = 4096;

/// Offset of the sys_chunk_array within the superblock.
pub const SYS_CHUNK_ARRAY_OFFSET: usize = 0x32B;

/// Maximum size of the sys_chunk_array.
pub const BTRFS_SYSTEM_CHUNK_ARRAY_SIZE: usize = 2048;

/// Checksum type: CRC32C.
pub const BTRFS_CSUM_TYPE_CRC32C: u16 = 0;
/// Checksum type: xxhash64.
pub const BTRFS_CSUM_TYPE_XXHASH: u16 = 1;
/// Checksum type: SHA256.
pub const BTRFS_CSUM_TYPE_SHA256: u16 = 2;
/// Checksum type: BLAKE2b.
pub const BTRFS_CSUM_TYPE_BLAKE2: u16 = 3;

/// Size of the checksum field (32 bytes; only first 4 used for CRC32C).
pub const BTRFS_CSUM_SIZE: usize = 32;

/// Number of bytes in a UUID.
pub const BTRFS_UUID_SIZE: usize = 16;

/// btrfs leaf/node size default (16 KiB).
pub const BTRFS_DEFAULT_NODESIZE: u32 = 16384;

/// btrfs default sector size (4 KiB).
pub const BTRFS_DEFAULT_SECTORSIZE: u32 = 4096;

/// Incompat feature flags.
pub mod incompat {
    /// Mixed block groups (data + metadata in same block group).
    pub const MIXED_BACKREF: u64 = 1 << 0;
    /// Default subvolume.
    pub const DEFAULT_SUBVOL: u64 = 1 << 1;
    /// Mixed data+metadata block groups.
    pub const MIXED_GROUPS: u64 = 1 << 2;
    /// Compression (zlib/lzo/zstd).
    pub const COMPRESS_LZO: u64 = 1 << 3;
    pub const COMPRESS_ZSTD: u64 = 1 << 4;
    /// Big metadata (metadata blocks > 4K).
    pub const BIG_METADATA: u64 = 1 << 5;
    /// Extended inode refs.
    pub const EXTENDED_IREF: u64 = 1 << 6;
    /// RAID56 support.
    pub const RAID56: u64 = 1 << 7;
    /// Skinny metadata (METADATA_ITEM instead of EXTENT_ITEM for tree blocks).
    pub const SKINNY_METADATA: u64 = 1 << 8;
    /// No-holes feature (don't store explicit hole extents).
    pub const NO_HOLES: u64 = 1 << 9;
    /// Metadata UUID (use metadata_uuid instead of fsid).
    pub const METADATA_UUID: u64 = 1 << 10;
    /// RAID1C3/RAID1C4 support.
    pub const RAID1C34: u64 = 1 << 11;
    /// Zoned block device support.
    pub const ZONED: u64 = 1 << 12;
    /// Extent tree v2.
    pub const EXTENT_TREE_V2: u64 = 1 << 13;
}

/// Compat RO flags.
pub mod compat_ro {
    /// Free space tree.
    pub const FREE_SPACE_TREE: u64 = 1 << 0;
    /// Free space tree valid.
    pub const FREE_SPACE_TREE_VALID: u64 = 1 << 1;
    /// Verity support.
    pub const VERITY: u64 = 1 << 2;
    /// Block group tree.
    pub const BLOCK_GROUP_TREE: u64 = 1 << 3;
}

/// Parsed btrfs superblock.
///
/// Contains all fields needed for filesystem operation. Fields are stored
/// in native endian (converted from little-endian on disk).
#[derive(Clone)]
pub struct Superblock {
    /// Checksum of everything past the csum field (first 32 bytes).
    pub csum: [u8; BTRFS_CSUM_SIZE],
    /// Filesystem UUID (16 bytes).
    pub fsid: [u8; BTRFS_UUID_SIZE],
    /// Physical address of this block (should match where we read it from).
    pub bytenr: u64,
    /// Flags (currently unused, must be 0).
    pub flags: u64,
    /// Magic number, must be BTRFS_MAGIC.
    pub magic: u64,
    /// Filesystem generation (transaction id of last commit).
    pub generation: u64,
    /// Logical address of the root tree root.
    pub root: u64,
    /// Logical address of the chunk tree root.
    pub chunk_root: u64,
    /// Logical address of the log tree root (0 if none).
    pub log_root: u64,
    /// Log root transaction id.
    pub log_root_transid: u64,
    /// Total bytes in the filesystem.
    pub total_bytes: u64,
    /// Bytes used by data and metadata.
    pub bytes_used: u64,
    /// Objectid of the root directory (usually 6).
    pub root_dir_objectid: u64,
    /// Number of physical devices.
    pub num_devices: u64,
    /// Sector size in bytes (usually 4096).
    pub sectorsize: u32,
    /// Node size in bytes (usually 16384).
    pub nodesize: u32,
    /// Leaf size in bytes (deprecated, same as nodesize since Linux 3.4).
    pub leafsize: u32,
    /// Stripe size in bytes (usually 65536).
    pub stripesize: u32,
    /// Size of the sys_chunk_array in bytes.
    pub sys_chunk_array_size: u32,
    /// Chunk root generation.
    pub chunk_root_generation: u64,
    /// Compatible feature flags.
    pub compat_flags: u64,
    /// Compatible read-only feature flags.
    pub compat_ro_flags: u64,
    /// Incompatible feature flags.
    pub incompat_flags: u64,
    /// Checksum type (0 = CRC32C).
    pub csum_type: u16,
    /// Root level (depth of the root tree).
    pub root_level: u8,
    /// Chunk root level.
    pub chunk_root_level: u8,
    /// Log root level.
    pub log_root_level: u8,
    /// Device item for this device.
    pub dev_item_devid: u64,
    pub dev_item_total_bytes: u64,
    pub dev_item_bytes_used: u64,
    pub dev_item_io_align: u32,
    pub dev_item_io_width: u32,
    pub dev_item_sector_size: u32,
    pub dev_item_type: u64,
    pub dev_item_generation: u64,
    pub dev_item_start_offset: u64,
    pub dev_item_dev_group: u32,
    pub dev_item_seek_speed: u8,
    pub dev_item_bandwidth: u8,
    pub dev_item_uuid: [u8; BTRFS_UUID_SIZE],
    pub dev_item_fsid: [u8; BTRFS_UUID_SIZE],
    /// Label (up to 256 bytes, null-terminated).
    pub label: [u8; 256],
    /// Cache generation.
    pub cache_generation: u64,
    /// UUID tree generation.
    pub uuid_tree_generation: u64,
    /// Metadata UUID (if INCOMPAT_METADATA_UUID is set).
    pub metadata_uuid: [u8; BTRFS_UUID_SIZE],
    /// Raw sys_chunk_array (embedded chunk mappings needed to bootstrap).
    pub sys_chunk_array: [u8; BTRFS_SYSTEM_CHUNK_ARRAY_SIZE],
}

impl Superblock {
    /// Parse a superblock from a 4096-byte buffer read from disk.
    ///
    /// The buffer must contain the bytes at device offset 0x10000.
    /// Returns `None` if the magic number is invalid.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < SUPERBLOCK_SIZE {
            log::error!("[btrfs::superblock] buffer too small: {} bytes (need >= {})", buf.len(), SUPERBLOCK_SIZE);
            return None;
        }

        let magic = read_u64(buf, 0x40);
        if magic != BTRFS_MAGIC {
            log::error!("[btrfs::superblock] invalid magic: 0x{:016X} (expected 0x{:016X})", magic, BTRFS_MAGIC);
            return None;
        }

        let mut csum = [0u8; BTRFS_CSUM_SIZE];
        csum.copy_from_slice(&buf[0x00..0x20]);

        let mut fsid = [0u8; BTRFS_UUID_SIZE];
        fsid.copy_from_slice(&buf[0x20..0x30]);

        let sys_chunk_array_size = read_u32(buf, 0xC4);
        let mut sys_chunk_array = [0u8; BTRFS_SYSTEM_CHUNK_ARRAY_SIZE];
        let sca_end = (SYS_CHUNK_ARRAY_OFFSET + sys_chunk_array_size as usize).min(buf.len());
        let sca_len = sca_end.saturating_sub(SYS_CHUNK_ARRAY_OFFSET);
        sys_chunk_array[..sca_len].copy_from_slice(&buf[SYS_CHUNK_ARRAY_OFFSET..sca_end]);

        let mut dev_uuid = [0u8; BTRFS_UUID_SIZE];
        dev_uuid.copy_from_slice(&buf[0x12B..0x13B]);
        let mut dev_fsid = [0u8; BTRFS_UUID_SIZE];
        dev_fsid.copy_from_slice(&buf[0x13B..0x14B]);

        let mut label = [0u8; 256];
        label.copy_from_slice(&buf[0x12B + 0x20..0x12B + 0x20 + 256]);

        let mut metadata_uuid = [0u8; BTRFS_UUID_SIZE];
        if buf.len() >= 0x3FB + BTRFS_UUID_SIZE {
            metadata_uuid.copy_from_slice(&buf[0x3FB..0x3FB + BTRFS_UUID_SIZE]);
        }

        let sb = Superblock {
            csum,
            fsid,
            bytenr:                   read_u64(buf, 0x30),
            flags:                    read_u64(buf, 0x38),
            magic,
            generation:               read_u64(buf, 0x48),
            root:                     read_u64(buf, 0x50),
            chunk_root:               read_u64(buf, 0x58),
            log_root:                 read_u64(buf, 0x60),
            log_root_transid:         read_u64(buf, 0x68),
            total_bytes:              read_u64(buf, 0x70),
            bytes_used:               read_u64(buf, 0x78),
            root_dir_objectid:        read_u64(buf, 0x80),
            num_devices:              read_u64(buf, 0x88),
            sectorsize:               read_u32(buf, 0x90),
            nodesize:                 read_u32(buf, 0x94),
            leafsize:                 read_u32(buf, 0x98),
            stripesize:               read_u32(buf, 0x9C),
            sys_chunk_array_size,
            chunk_root_generation:    read_u64(buf, 0xA0),
            compat_flags:             read_u64(buf, 0xA8),
            compat_ro_flags:          read_u64(buf, 0xB0),
            incompat_flags:           read_u64(buf, 0xB8),
            csum_type:                read_u16(buf, 0xC0),
            root_level:               buf[0xC2],
            chunk_root_level:         buf[0xC3],
            log_root_level:           buf[0xC4 + 4], // at 0xC8 after sys_chunk_array_size
            dev_item_devid:           read_u64(buf, 0xC9),
            dev_item_total_bytes:     read_u64(buf, 0xD1),
            dev_item_bytes_used:      read_u64(buf, 0xD9),
            dev_item_io_align:        read_u32(buf, 0xE1),
            dev_item_io_width:        read_u32(buf, 0xE5),
            dev_item_sector_size:     read_u32(buf, 0xE9),
            dev_item_type:            read_u64(buf, 0xED),
            dev_item_generation:      read_u64(buf, 0xF5),
            dev_item_start_offset:    read_u64(buf, 0xFD),
            dev_item_dev_group:       read_u32(buf, 0x105),
            dev_item_seek_speed:      buf[0x109],
            dev_item_bandwidth:       buf[0x10A],
            dev_item_uuid:            dev_uuid,
            dev_item_fsid:            dev_fsid,
            label,
            cache_generation:         read_u64(buf, 0x24B),
            uuid_tree_generation:     read_u64(buf, 0x253),
            metadata_uuid,
            sys_chunk_array,
        };

        // Verify the checksum
        if sb.csum_type == BTRFS_CSUM_TYPE_CRC32C {
            let computed = crate::crc32c::btrfs_csum(&buf[0x20..SUPERBLOCK_SIZE]);
            let stored = u32::from_le_bytes([sb.csum[0], sb.csum[1], sb.csum[2], sb.csum[3]]);
            if computed != stored {
                log::warn!("[btrfs::superblock] CRC32C mismatch: computed=0x{:08X}, stored=0x{:08X}", computed, stored);
            } else {
                log::debug!("[btrfs::superblock] CRC32C checksum verified: 0x{:08X}", computed);
            }
        }

        log::info!("[btrfs::superblock] parsed: magic=0x{:016X}, generation={}, nodesize={}, sectorsize={}",
            sb.magic, sb.generation, sb.nodesize, sb.sectorsize);
        log::info!("[btrfs::superblock] total_bytes={}, bytes_used={}, num_devices={}",
            sb.total_bytes, sb.bytes_used, sb.num_devices);
        log::debug!("[btrfs::superblock] root=0x{:X}, chunk_root=0x{:X}, log_root=0x{:X}",
            sb.root, sb.chunk_root, sb.log_root);
        log::debug!("[btrfs::superblock] root_level={}, chunk_root_level={}, log_root_level={}",
            sb.root_level, sb.chunk_root_level, sb.log_root_level);
        log::debug!("[btrfs::superblock] incompat_flags=0x{:016X}, compat_ro_flags=0x{:016X}, csum_type={}",
            sb.incompat_flags, sb.compat_ro_flags, sb.csum_type);
        log::debug!("[btrfs::superblock] sys_chunk_array_size={}", sb.sys_chunk_array_size);

        Some(sb)
    }

    /// Serialize the superblock back into a 4096-byte buffer for writing to disk.
    ///
    /// The checksum is recomputed automatically if csum_type is CRC32C.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = alloc::vec![0u8; SUPERBLOCK_SIZE];

        // Skip csum[0..0x20] for now, write it after computing
        buf[0x20..0x30].copy_from_slice(&self.fsid);
        write_u64(&mut buf, 0x30, self.bytenr);
        write_u64(&mut buf, 0x38, self.flags);
        write_u64(&mut buf, 0x40, self.magic);
        write_u64(&mut buf, 0x48, self.generation);
        write_u64(&mut buf, 0x50, self.root);
        write_u64(&mut buf, 0x58, self.chunk_root);
        write_u64(&mut buf, 0x60, self.log_root);
        write_u64(&mut buf, 0x68, self.log_root_transid);
        write_u64(&mut buf, 0x70, self.total_bytes);
        write_u64(&mut buf, 0x78, self.bytes_used);
        write_u64(&mut buf, 0x80, self.root_dir_objectid);
        write_u64(&mut buf, 0x88, self.num_devices);
        write_u32(&mut buf, 0x90, self.sectorsize);
        write_u32(&mut buf, 0x94, self.nodesize);
        write_u32(&mut buf, 0x98, self.leafsize);
        write_u32(&mut buf, 0x9C, self.stripesize);
        write_u32(&mut buf, 0xC4, self.sys_chunk_array_size);
        write_u64(&mut buf, 0xA0, self.chunk_root_generation);
        write_u64(&mut buf, 0xA8, self.compat_flags);
        write_u64(&mut buf, 0xB0, self.compat_ro_flags);
        write_u64(&mut buf, 0xB8, self.incompat_flags);
        write_u16(&mut buf, 0xC0, self.csum_type);
        buf[0xC2] = self.root_level;
        buf[0xC3] = self.chunk_root_level;

        // dev_item
        write_u64(&mut buf, 0xC9, self.dev_item_devid);
        write_u64(&mut buf, 0xD1, self.dev_item_total_bytes);
        write_u64(&mut buf, 0xD9, self.dev_item_bytes_used);
        write_u32(&mut buf, 0xE1, self.dev_item_io_align);
        write_u32(&mut buf, 0xE5, self.dev_item_io_width);
        write_u32(&mut buf, 0xE9, self.dev_item_sector_size);
        write_u64(&mut buf, 0xED, self.dev_item_type);
        write_u64(&mut buf, 0xF5, self.dev_item_generation);
        write_u64(&mut buf, 0xFD, self.dev_item_start_offset);
        write_u32(&mut buf, 0x105, self.dev_item_dev_group);
        buf[0x109] = self.dev_item_seek_speed;
        buf[0x10A] = self.dev_item_bandwidth;
        buf[0x12B..0x13B].copy_from_slice(&self.dev_item_uuid);
        buf[0x13B..0x14B].copy_from_slice(&self.dev_item_fsid);

        // label
        buf[0x14B..0x24B].copy_from_slice(&self.label);

        // sys_chunk_array
        let sca_len = (self.sys_chunk_array_size as usize).min(BTRFS_SYSTEM_CHUNK_ARRAY_SIZE);
        buf[SYS_CHUNK_ARRAY_OFFSET..SYS_CHUNK_ARRAY_OFFSET + sca_len]
            .copy_from_slice(&self.sys_chunk_array[..sca_len]);

        // Recompute checksum over bytes [0x20..SUPERBLOCK_SIZE]
        if self.csum_type == BTRFS_CSUM_TYPE_CRC32C {
            let computed = crate::crc32c::btrfs_csum(&buf[0x20..SUPERBLOCK_SIZE]);
            buf[0..4].copy_from_slice(&computed.to_le_bytes());
            // Zero the rest of the csum field
            for b in &mut buf[4..BTRFS_CSUM_SIZE] {
                *b = 0;
            }
            log::trace!("[btrfs::superblock] recomputed CRC32C: 0x{:08X}", computed);
        } else {
            buf[0..BTRFS_CSUM_SIZE].copy_from_slice(&self.csum);
        }

        log::trace!("[btrfs::superblock] serialized {} bytes to disk format", buf.len());
        buf
    }

    /// Get the filesystem label as a string (trimming null bytes).
    pub fn label_str(&self) -> &str {
        let end = self.label.iter().position(|&b| b == 0).unwrap_or(256);
        core::str::from_utf8(&self.label[..end]).unwrap_or("<invalid>")
    }

    /// Whether the filesystem uses the metadata UUID instead of fsid.
    #[inline]
    pub fn has_metadata_uuid(&self) -> bool {
        self.incompat_flags & incompat::METADATA_UUID != 0
    }

    /// Get the effective metadata UUID (fsid or metadata_uuid depending on flags).
    pub fn effective_metadata_uuid(&self) -> &[u8; BTRFS_UUID_SIZE] {
        if self.has_metadata_uuid() {
            &self.metadata_uuid
        } else {
            &self.fsid
        }
    }

    /// Whether the no-holes feature is enabled.
    #[inline]
    pub fn has_no_holes(&self) -> bool {
        self.incompat_flags & incompat::NO_HOLES != 0
    }

    /// Whether skinny metadata is enabled.
    #[inline]
    pub fn has_skinny_metadata(&self) -> bool {
        self.incompat_flags & incompat::SKINNY_METADATA != 0
    }
}

impl fmt::Debug for Superblock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Superblock")
            .field("magic", &format_args!("0x{:016X}", self.magic))
            .field("generation", &self.generation)
            .field("nodesize", &self.nodesize)
            .field("sectorsize", &self.sectorsize)
            .field("total_bytes", &self.total_bytes)
            .field("bytes_used", &self.bytes_used)
            .field("num_devices", &self.num_devices)
            .field("root", &format_args!("0x{:X}", self.root))
            .field("chunk_root", &format_args!("0x{:X}", self.chunk_root))
            .field("label", &self.label_str())
            .field("csum_type", &self.csum_type)
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
fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

#[inline]
fn write_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_le_bytes());
}
