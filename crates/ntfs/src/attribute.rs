//! NTFS attribute parsing.
//!
//! Every piece of data in NTFS is stored as an attribute within an MFT entry.
//! Attributes can be resident (data stored inline in the MFT entry) or
//! non-resident (data stored in clusters on disk, referenced via data runs).
//!
//! Each attribute has a type (e.g., $STANDARD_INFORMATION, $FILE_NAME, $DATA)
//! and optional name (for named data streams like "file.txt:stream_name").
//!
//! Reference: <https://flatcap.github.io/linux-ntfs/ntfs/concepts/attribute_header.html>

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

// --- Attribute type constants ---

/// Attribute type enumeration.
/// These are the standard attribute types defined by NTFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AttributeType {
    /// $STANDARD_INFORMATION — timestamps, permissions, security ID, quota, USN.
    StandardInformation = 0x10,
    /// $ATTRIBUTE_LIST — list of attributes when they span multiple MFT entries.
    AttributeList = 0x20,
    /// $FILE_NAME — filename (Unicode), parent directory reference, timestamps.
    FileName = 0x30,
    /// $OBJECT_ID — 16-byte object identifier (GUID).
    ObjectId = 0x40,
    /// $SECURITY_DESCRIPTOR — NTFS security descriptor (ACL).
    SecurityDescriptor = 0x50,
    /// $VOLUME_NAME — volume label (Unicode string).
    VolumeName = 0x60,
    /// $VOLUME_INFORMATION — volume version and flags.
    VolumeInformation = 0x70,
    /// $DATA — file data (unnamed = default stream, named = alternate data stream).
    Data = 0x80,
    /// $INDEX_ROOT — root node of a B+ tree index (always resident).
    IndexRoot = 0x90,
    /// $INDEX_ALLOCATION — non-resident B+ tree index nodes.
    IndexAllocation = 0xA0,
    /// $BITMAP — bitmap for MFT entries or index allocation.
    Bitmap = 0xB0,
    /// $REPARSE_POINT — reparse point data (symlinks, mount points, etc.).
    ReparsePoint = 0xC0,
    /// $EA_INFORMATION — extended attribute information.
    EaInformation = 0xD0,
    /// $EA — extended attributes.
    Ea = 0xE0,
    /// End-of-attributes marker (not a real attribute).
    End = 0xFFFFFFFF,
}

impl AttributeType {
    /// Convert a raw u32 to an AttributeType.
    pub fn from_u32(val: u32) -> Option<Self> {
        match val {
            0x10 => Some(Self::StandardInformation),
            0x20 => Some(Self::AttributeList),
            0x30 => Some(Self::FileName),
            0x40 => Some(Self::ObjectId),
            0x50 => Some(Self::SecurityDescriptor),
            0x60 => Some(Self::VolumeName),
            0x70 => Some(Self::VolumeInformation),
            0x80 => Some(Self::Data),
            0x90 => Some(Self::IndexRoot),
            0xA0 => Some(Self::IndexAllocation),
            0xB0 => Some(Self::Bitmap),
            0xC0 => Some(Self::ReparsePoint),
            0xD0 => Some(Self::EaInformation),
            0xE0 => Some(Self::Ea),
            0xFFFFFFFF => Some(Self::End),
            _ => None,
        }
    }

    /// Human-readable name for the attribute type.
    pub fn name(&self) -> &'static str {
        match self {
            Self::StandardInformation => "$STANDARD_INFORMATION",
            Self::AttributeList => "$ATTRIBUTE_LIST",
            Self::FileName => "$FILE_NAME",
            Self::ObjectId => "$OBJECT_ID",
            Self::SecurityDescriptor => "$SECURITY_DESCRIPTOR",
            Self::VolumeName => "$VOLUME_NAME",
            Self::VolumeInformation => "$VOLUME_INFORMATION",
            Self::Data => "$DATA",
            Self::IndexRoot => "$INDEX_ROOT",
            Self::IndexAllocation => "$INDEX_ALLOCATION",
            Self::Bitmap => "$BITMAP",
            Self::ReparsePoint => "$REPARSE_POINT",
            Self::EaInformation => "$EA_INFORMATION",
            Self::Ea => "$EA",
            Self::End => "$END",
        }
    }
}

// --- Attribute flags ---

/// Attribute is compressed (LZNT1).
pub const ATTR_FLAG_COMPRESSED: u16 = 0x0001;
/// Attribute is encrypted (EFS).
pub const ATTR_FLAG_ENCRYPTED: u16 = 0x4000;
/// Attribute is sparse.
pub const ATTR_FLAG_SPARSE: u16 = 0x8000;

/// Parsed attribute header (common to both resident and non-resident attributes).
///
/// This is the first 16 bytes of every attribute. After this, either a
/// `ResidentHeader` or `NonResidentHeader` follows depending on `non_resident`.
#[derive(Clone)]
pub struct AttributeHeader {
    /// Attribute type (e.g., 0x80 for $DATA).
    pub attr_type: AttributeType,
    /// Total length of this attribute in bytes (header + data/runs).
    pub length: u32,
    /// Whether this attribute is non-resident (data stored in clusters).
    pub non_resident: bool,
    /// Length of the attribute name in UTF-16 characters (0 if unnamed).
    pub name_length: u8,
    /// Byte offset to the attribute name from start of attribute.
    pub name_offset: u16,
    /// Attribute flags (compressed, encrypted, sparse).
    pub flags: u16,
    /// Attribute instance number (unique within this MFT entry).
    pub instance: u16,
}

impl AttributeHeader {
    /// Size of the common attribute header (16 bytes).
    pub const HEADER_SIZE: usize = 16;

    /// Parse the common attribute header from the start of an attribute.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::HEADER_SIZE {
            log::error!("[ntfs::attribute] header buffer too small: {} bytes (need >= {})",
                buf.len(), Self::HEADER_SIZE);
            return None;
        }

        let type_val = read_u32(buf, 0x00);
        let attr_type = AttributeType::from_u32(type_val)?;

        if attr_type == AttributeType::End {
            return None;
        }

        let hdr = AttributeHeader {
            attr_type,
            length:      read_u32(buf, 0x04),
            non_resident: buf[0x08] != 0,
            name_length: buf[0x09],
            name_offset: read_u16(buf, 0x0A),
            flags:       read_u16(buf, 0x0C),
            instance:    read_u16(buf, 0x0E),
        };

        log::trace!("[ntfs::attribute] header: type={} (0x{:08X}), len={}, non_resident={}, \
            name_len={}, flags=0x{:04X}, instance={}",
            hdr.attr_type.name(), type_val, hdr.length, hdr.non_resident,
            hdr.name_length, hdr.flags, hdr.instance);

        Some(hdr)
    }

    /// Get the attribute name (if present) from the MFT entry data.
    ///
    /// `attr_base` is the start of this attribute within the entry data.
    pub fn name_from_data(&self, data: &[u8], attr_base: usize) -> Option<String> {
        if self.name_length == 0 {
            return None;
        }
        let name_start = attr_base + self.name_offset as usize;
        let name_bytes = self.name_length as usize * 2; // UTF-16LE
        if name_start + name_bytes > data.len() {
            log::error!("[ntfs::attribute] name extends beyond data");
            return None;
        }
        let utf16: Vec<u16> = (0..self.name_length as usize)
            .map(|i| {
                let off = name_start + i * 2;
                u16::from_le_bytes([data[off], data[off + 1]])
            })
            .collect();
        let name = String::from_utf16_lossy(&utf16);
        log::trace!("[ntfs::attribute] attribute name: '{}'", name);
        Some(name)
    }

    /// Whether this attribute is compressed.
    #[inline]
    pub fn is_compressed(&self) -> bool {
        self.flags & ATTR_FLAG_COMPRESSED != 0
    }

    /// Whether this attribute is encrypted.
    #[inline]
    pub fn is_encrypted(&self) -> bool {
        self.flags & ATTR_FLAG_ENCRYPTED != 0
    }

    /// Whether this attribute is sparse.
    #[inline]
    pub fn is_sparse(&self) -> bool {
        self.flags & ATTR_FLAG_SPARSE != 0
    }
}

impl fmt::Debug for AttributeHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttributeHeader")
            .field("type", &self.attr_type.name())
            .field("length", &self.length)
            .field("non_resident", &self.non_resident)
            .field("name_length", &self.name_length)
            .field("flags", &format_args!("0x{:04X}", self.flags))
            .field("instance", &self.instance)
            .finish()
    }
}

/// Resident attribute header (follows the common AttributeHeader).
///
/// For resident attributes, the data is stored directly in the MFT entry.
#[derive(Clone, Debug)]
pub struct ResidentHeader {
    /// Length of the attribute value in bytes.
    pub value_length: u32,
    /// Byte offset to the attribute value from start of the attribute.
    pub value_offset: u16,
    /// Indexed flag (1 if this attribute is indexed).
    pub indexed_flag: u8,
}

impl ResidentHeader {
    /// Size of the resident-specific header (8 bytes, at offset 16 from attribute start).
    pub const SIZE: usize = 8;

    /// Parse the resident header from bytes starting at offset 16 of the attribute.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            log::error!("[ntfs::attribute] resident header too small: {} bytes", buf.len());
            return None;
        }

        let hdr = ResidentHeader {
            value_length: read_u32(buf, 0x00),
            value_offset: read_u16(buf, 0x04),
            indexed_flag: buf[0x06],
        };

        log::trace!("[ntfs::attribute] resident: value_length={}, value_offset=0x{:04X}, indexed={}",
            hdr.value_length, hdr.value_offset, hdr.indexed_flag);

        Some(hdr)
    }
}

/// Non-resident attribute header (follows the common AttributeHeader).
///
/// For non-resident attributes, the data is stored in clusters on disk,
/// referenced by data runs (mapping pairs).
#[derive(Clone, Debug)]
pub struct NonResidentHeader {
    /// Lowest Virtual Cluster Number (VCN) covered by this attribute record.
    pub lowest_vcn: u64,
    /// Highest VCN covered by this attribute record.
    pub highest_vcn: u64,
    /// Byte offset to the mapping pairs (data runs) from start of the attribute.
    pub mapping_pairs_offset: u16,
    /// Compression unit size (log2, 0 = not compressed).
    pub compression_unit: u16,
    /// Allocated size on disk in bytes (multiple of cluster size).
    pub allocated_size: u64,
    /// Actual data size in bytes.
    pub data_size: u64,
    /// Initialized data size in bytes (rest is zero-filled).
    pub initialized_size: u64,
    /// Compressed size (only if compressed, after allocated_size fields).
    pub compressed_size: Option<u64>,
}

impl NonResidentHeader {
    /// Minimum size of the non-resident header (48 bytes from offset 16).
    pub const MIN_SIZE: usize = 48;

    /// Parse the non-resident header from bytes starting at offset 16 of the attribute.
    ///
    /// Layout (offsets from start of NR-specific area, i.e., byte 0x10 from attribute start):
    /// - 0x00: lowest_vcn (8 bytes)
    /// - 0x08: highest_vcn (8 bytes)
    /// - 0x10: mapping_pairs_offset (2 bytes) -- relative to attribute start
    /// - 0x12: compression_unit (2 bytes)
    /// - 0x14: padding (4 bytes)
    /// - 0x18: allocated_size (8 bytes)
    /// - 0x20: data_size (8 bytes)
    /// - 0x28: initialized_size (8 bytes)
    /// - 0x30: compressed_size (8 bytes, optional, only if compressed)
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::MIN_SIZE {
            log::error!("[ntfs::attribute] non-resident header too small: {} bytes (need >= {})",
                buf.len(), Self::MIN_SIZE);
            return None;
        }

        let compression_unit = read_u16(buf, 0x12);

        let hdr = NonResidentHeader {
            lowest_vcn:           read_u64(buf, 0x00),
            highest_vcn:          read_u64(buf, 0x08),
            mapping_pairs_offset: read_u16(buf, 0x10),
            compression_unit,
            allocated_size:       read_u64(buf, 0x18),
            data_size:            read_u64(buf, 0x20),
            initialized_size:     read_u64(buf, 0x28),
            compressed_size: if compression_unit > 0 && buf.len() >= 0x38 {
                Some(read_u64(buf, 0x30))
            } else {
                None
            },
        };

        log::trace!("[ntfs::attribute] non-resident: vcn={}..{}, mapping_offset=0x{:04X}, \
            alloc={}, data={}, init={}",
            hdr.lowest_vcn, hdr.highest_vcn, hdr.mapping_pairs_offset,
            hdr.allocated_size, hdr.data_size, hdr.initialized_size);
        if let Some(comp) = hdr.compressed_size {
            log::trace!("[ntfs::attribute] compressed_size={}, compression_unit={}",
                comp, hdr.compression_unit);
        }

        Some(hdr)
    }

    /// Number of clusters this attribute spans.
    #[inline]
    pub fn cluster_count(&self) -> u64 {
        self.highest_vcn - self.lowest_vcn + 1
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
fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3],
        buf[offset + 4], buf[offset + 5], buf[offset + 6], buf[offset + 7],
    ])
}
