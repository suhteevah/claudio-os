//! btrfs file extent data items.
//!
//! Each file's data is described by EXTENT_DATA items (type 108) in the filesystem tree.
//! The key is (inode, EXTENT_DATA, file_offset).
//!
//! Three extent types exist:
//! - **Inline**: small files stored directly in the B-tree leaf item.
//! - **Regular**: data stored in a separate extent on disk.
//! - **Prealloc**: space reserved but not yet written (fallocate).
//!
//! Reference: <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html#file-extent-item>

use alloc::vec::Vec;
use core::fmt;

/// Size of the file extent item header on disk (21 bytes, before inline data or extent ref).
pub const BTRFS_FILE_EXTENT_HEADER_SIZE: usize = 21;

/// Size of the regular/prealloc extent reference fields (32 bytes after header).
pub const BTRFS_FILE_EXTENT_REF_SIZE: usize = 32;

/// Total size of a non-inline file extent item (header + ref = 53 bytes).
pub const BTRFS_FILE_EXTENT_ITEM_SIZE: usize = BTRFS_FILE_EXTENT_HEADER_SIZE + BTRFS_FILE_EXTENT_REF_SIZE;

/// Extent type: inline data (stored in the B-tree leaf).
pub const BTRFS_FILE_EXTENT_INLINE: u8 = 0;

/// Extent type: regular data extent (stored in a separate on-disk extent).
pub const BTRFS_FILE_EXTENT_REG: u8 = 1;

/// Extent type: preallocated extent (fallocate, not yet written).
pub const BTRFS_FILE_EXTENT_PREALLOC: u8 = 2;

/// Compression type: no compression.
pub const BTRFS_COMPRESS_NONE: u8 = 0;
/// Compression type: zlib.
pub const BTRFS_COMPRESS_ZLIB: u8 = 1;
/// Compression type: LZO.
pub const BTRFS_COMPRESS_LZO: u8 = 2;
/// Compression type: ZSTD.
pub const BTRFS_COMPRESS_ZSTD: u8 = 3;

/// Encryption type: none.
pub const BTRFS_ENCRYPTION_NONE: u8 = 0;

/// Other encoding: none.
pub const BTRFS_ENCODING_NONE: u16 = 0;

/// Parsed btrfs file extent item.
#[derive(Clone)]
pub struct FileExtentItem {
    /// Generation when this extent was created.
    pub generation: u64,
    /// Size of the decoded (uncompressed) data in bytes.
    /// For inline extents, this is the size of the inline data.
    /// For regular/prealloc, this is `num_bytes`.
    pub ram_bytes: u64,
    /// Compression type (BTRFS_COMPRESS_*).
    pub compression: u8,
    /// Encryption type (always 0 currently).
    pub encryption: u8,
    /// Other encoding (always 0 currently).
    pub other_encoding: u16,
    /// Extent type (INLINE, REG, or PREALLOC).
    pub extent_type: u8,

    // --- Fields only valid for REG and PREALLOC extents ---
    /// Logical byte number of the extent on disk (0 for holes/sparse).
    pub disk_bytenr: u64,
    /// Number of bytes on disk (may differ from ram_bytes if compressed).
    pub disk_num_bytes: u64,
    /// Offset within the extent where the file's data starts.
    /// (For partial extent references when extents are shared via reflinks.)
    pub offset: u64,
    /// Number of bytes of file data in this reference.
    pub num_bytes: u64,

    // --- Field only valid for INLINE extents ---
    /// The inline data (stored directly in the B-tree leaf item).
    pub inline_data: Vec<u8>,
}

impl FileExtentItem {
    /// Parse a file extent item from raw item data.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < BTRFS_FILE_EXTENT_HEADER_SIZE {
            log::error!("[btrfs::extent] buffer too small: {} bytes (need >= {})",
                buf.len(), BTRFS_FILE_EXTENT_HEADER_SIZE);
            return None;
        }

        let generation = read_u64(buf, 0);
        let ram_bytes = read_u64(buf, 8);
        let compression = buf[16];
        let encryption = buf[17];
        let other_encoding = read_u16(buf, 18);
        let extent_type = buf[20];

        let (disk_bytenr, disk_num_bytes, offset, num_bytes, inline_data) = match extent_type {
            BTRFS_FILE_EXTENT_INLINE => {
                // Inline data follows the header
                let data_len = buf.len() - BTRFS_FILE_EXTENT_HEADER_SIZE;
                let inline_data = buf[BTRFS_FILE_EXTENT_HEADER_SIZE..].to_vec();
                log::trace!("[btrfs::extent] inline extent: {} bytes of data, compression={}",
                    data_len, compression);
                (0, 0, 0, 0, inline_data)
            }
            BTRFS_FILE_EXTENT_REG | BTRFS_FILE_EXTENT_PREALLOC => {
                if buf.len() < BTRFS_FILE_EXTENT_ITEM_SIZE {
                    log::error!("[btrfs::extent] buffer too small for regular extent: {} bytes (need >= {})",
                        buf.len(), BTRFS_FILE_EXTENT_ITEM_SIZE);
                    return None;
                }
                let disk_bytenr = read_u64(buf, 21);
                let disk_num_bytes = read_u64(buf, 29);
                let offset = read_u64(buf, 37);
                let num_bytes = read_u64(buf, 45);

                let type_str = if extent_type == BTRFS_FILE_EXTENT_REG { "regular" } else { "prealloc" };
                log::trace!("[btrfs::extent] {} extent: disk_bytenr=0x{:X}, disk_num_bytes={}, offset={}, num_bytes={}",
                    type_str, disk_bytenr, disk_num_bytes, offset, num_bytes);

                (disk_bytenr, disk_num_bytes, offset, num_bytes, Vec::new())
            }
            _ => {
                log::error!("[btrfs::extent] unknown extent type: {}", extent_type);
                return None;
            }
        };

        Some(FileExtentItem {
            generation,
            ram_bytes,
            compression,
            encryption,
            other_encoding,
            extent_type,
            disk_bytenr,
            disk_num_bytes,
            offset,
            num_bytes,
            inline_data,
        })
    }

    /// Serialize this file extent item to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self.extent_type {
            BTRFS_FILE_EXTENT_INLINE => {
                let total = BTRFS_FILE_EXTENT_HEADER_SIZE + self.inline_data.len();
                let mut buf = alloc::vec![0u8; total];
                write_u64(&mut buf, 0, self.generation);
                write_u64(&mut buf, 8, self.ram_bytes);
                buf[16] = self.compression;
                buf[17] = self.encryption;
                write_u16(&mut buf, 18, self.other_encoding);
                buf[20] = self.extent_type;
                buf[BTRFS_FILE_EXTENT_HEADER_SIZE..].copy_from_slice(&self.inline_data);
                log::trace!("[btrfs::extent] serialized inline extent: {} bytes", buf.len());
                buf
            }
            _ => {
                let mut buf = alloc::vec![0u8; BTRFS_FILE_EXTENT_ITEM_SIZE];
                write_u64(&mut buf, 0, self.generation);
                write_u64(&mut buf, 8, self.ram_bytes);
                buf[16] = self.compression;
                buf[17] = self.encryption;
                write_u16(&mut buf, 18, self.other_encoding);
                buf[20] = self.extent_type;
                write_u64(&mut buf, 21, self.disk_bytenr);
                write_u64(&mut buf, 29, self.disk_num_bytes);
                write_u64(&mut buf, 37, self.offset);
                write_u64(&mut buf, 45, self.num_bytes);
                log::trace!("[btrfs::extent] serialized regular extent: {} bytes", buf.len());
                buf
            }
        }
    }

    /// Whether this is an inline extent.
    #[inline]
    pub fn is_inline(&self) -> bool {
        self.extent_type == BTRFS_FILE_EXTENT_INLINE
    }

    /// Whether this is a regular (non-inline, non-prealloc) extent.
    #[inline]
    pub fn is_regular(&self) -> bool {
        self.extent_type == BTRFS_FILE_EXTENT_REG
    }

    /// Whether this is a preallocated extent.
    #[inline]
    pub fn is_prealloc(&self) -> bool {
        self.extent_type == BTRFS_FILE_EXTENT_PREALLOC
    }

    /// Whether this extent represents a hole (regular extent with disk_bytenr == 0).
    #[inline]
    pub fn is_hole(&self) -> bool {
        self.is_regular() && self.disk_bytenr == 0
    }

    /// Whether this extent is compressed.
    #[inline]
    pub fn is_compressed(&self) -> bool {
        self.compression != BTRFS_COMPRESS_NONE
    }

    /// Create a new inline extent item.
    pub fn new_inline(data: &[u8], generation: u64) -> Self {
        log::debug!("[btrfs::extent] creating inline extent: {} bytes, gen={}", data.len(), generation);
        FileExtentItem {
            generation,
            ram_bytes: data.len() as u64,
            compression: BTRFS_COMPRESS_NONE,
            encryption: BTRFS_ENCRYPTION_NONE,
            other_encoding: BTRFS_ENCODING_NONE,
            extent_type: BTRFS_FILE_EXTENT_INLINE,
            disk_bytenr: 0,
            disk_num_bytes: 0,
            offset: 0,
            num_bytes: 0,
            inline_data: data.to_vec(),
        }
    }

    /// Create a new regular extent item.
    pub fn new_regular(
        disk_bytenr: u64,
        disk_num_bytes: u64,
        offset: u64,
        num_bytes: u64,
        generation: u64,
    ) -> Self {
        log::debug!("[btrfs::extent] creating regular extent: disk=0x{:X}+{}, file_off={}, len={}, gen={}",
            disk_bytenr, disk_num_bytes, offset, num_bytes, generation);
        FileExtentItem {
            generation,
            ram_bytes: num_bytes,
            compression: BTRFS_COMPRESS_NONE,
            encryption: BTRFS_ENCRYPTION_NONE,
            other_encoding: BTRFS_ENCODING_NONE,
            extent_type: BTRFS_FILE_EXTENT_REG,
            disk_bytenr,
            disk_num_bytes,
            offset,
            num_bytes,
            inline_data: Vec::new(),
        }
    }
}

impl fmt::Debug for FileExtentItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let type_str = match self.extent_type {
            BTRFS_FILE_EXTENT_INLINE => "inline",
            BTRFS_FILE_EXTENT_REG => "regular",
            BTRFS_FILE_EXTENT_PREALLOC => "prealloc",
            _ => "unknown",
        };
        let comp_str = match self.compression {
            BTRFS_COMPRESS_NONE => "none",
            BTRFS_COMPRESS_ZLIB => "zlib",
            BTRFS_COMPRESS_LZO => "lzo",
            BTRFS_COMPRESS_ZSTD => "zstd",
            _ => "unknown",
        };
        let mut d = f.debug_struct("FileExtentItem");
        d.field("type", &type_str)
            .field("compression", &comp_str)
            .field("generation", &self.generation)
            .field("ram_bytes", &self.ram_bytes);
        if !self.is_inline() {
            d.field("disk_bytenr", &format_args!("0x{:X}", self.disk_bytenr))
                .field("disk_num_bytes", &self.disk_num_bytes)
                .field("offset", &self.offset)
                .field("num_bytes", &self.num_bytes);
        } else {
            d.field("inline_len", &self.inline_data.len());
        }
        d.finish()
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
