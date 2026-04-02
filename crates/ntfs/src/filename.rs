//! $FILE_NAME attribute (type 0x30) parsing.
//!
//! Every file and directory in NTFS has at least one $FILE_NAME attribute.
//! It contains the filename (UTF-16LE), parent directory reference, timestamps
//! (duplicated from $STANDARD_INFORMATION for directory listings), file sizes,
//! and the filename namespace (POSIX, Win32, DOS, or Win32+DOS).
//!
//! Reference: <https://flatcap.github.io/linux-ntfs/ntfs/attributes/file_name.html>

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// Filename namespace values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FileNamespace {
    /// POSIX: case-sensitive, allows almost all Unicode characters.
    Posix = 0x00,
    /// Win32: case-insensitive, standard Windows filename rules.
    Win32 = 0x01,
    /// DOS: 8.3 short filename, uppercase only.
    Dos = 0x02,
    /// Win32 and DOS: the filename is valid for both namespaces.
    /// This is the most common case for files with short names.
    Win32AndDos = 0x03,
}

impl FileNamespace {
    /// Convert from raw byte value.
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0x00 => Some(Self::Posix),
            0x01 => Some(Self::Win32),
            0x02 => Some(Self::Dos),
            0x03 => Some(Self::Win32AndDos),
            _ => None,
        }
    }

    /// Human-readable name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Posix => "POSIX",
            Self::Win32 => "Win32",
            Self::Dos => "DOS",
            Self::Win32AndDos => "Win32+DOS",
        }
    }
}

// --- File attribute flags (same as Win32 FILE_ATTRIBUTE_*) ---

/// File is read-only.
pub const FILE_ATTR_READONLY: u32 = 0x00000001;
/// File is hidden.
pub const FILE_ATTR_HIDDEN: u32 = 0x00000002;
/// File is a system file.
pub const FILE_ATTR_SYSTEM: u32 = 0x00000004;
/// Entry is a directory.
pub const FILE_ATTR_DIRECTORY: u32 = 0x00000010;
/// File is an archive (needs backup).
pub const FILE_ATTR_ARCHIVE: u32 = 0x00000020;
/// File is a device (unused in NTFS).
pub const FILE_ATTR_DEVICE: u32 = 0x00000040;
/// File has no special attributes.
pub const FILE_ATTR_NORMAL: u32 = 0x00000080;
/// File is a temporary file.
pub const FILE_ATTR_TEMPORARY: u32 = 0x00000100;
/// File is a sparse file.
pub const FILE_ATTR_SPARSE: u32 = 0x00000200;
/// File is a reparse point.
pub const FILE_ATTR_REPARSE_POINT: u32 = 0x00000400;
/// File is compressed.
pub const FILE_ATTR_COMPRESSED: u32 = 0x00000800;
/// File is offline (data moved to remote storage).
pub const FILE_ATTR_OFFLINE: u32 = 0x00001000;
/// File is not indexed by the content indexing service.
pub const FILE_ATTR_NOT_CONTENT_INDEXED: u32 = 0x00002000;
/// File is encrypted.
pub const FILE_ATTR_ENCRYPTED: u32 = 0x00004000;

/// Minimum size of a $FILE_NAME attribute (66 bytes + at least 2 bytes for 1-char name).
pub const FILE_NAME_MIN_SIZE: usize = 66;

/// Parsed $FILE_NAME attribute.
#[derive(Clone)]
pub struct FileNameAttr {
    /// MFT reference to the parent directory.
    pub parent_reference: u64,
    /// File creation time (Windows FILETIME: 100ns intervals since 1601-01-01).
    pub creation_time: u64,
    /// File modification time (FILETIME).
    pub modification_time: u64,
    /// MFT record modification time (FILETIME).
    pub mft_modification_time: u64,
    /// File access time (FILETIME).
    pub access_time: u64,
    /// Allocated size of the file on disk (for directory listings).
    pub allocated_size: u64,
    /// Real (logical) file size (for directory listings).
    pub real_size: u64,
    /// File attribute flags (FILE_ATTR_*).
    pub flags: u32,
    /// Extended attributes / reparse point tag.
    pub ea_reparse: u32,
    /// Filename length in UTF-16 characters.
    pub name_length: u8,
    /// Filename namespace (POSIX, Win32, DOS, Win32+DOS).
    pub namespace: FileNamespace,
    /// The filename as a Rust String (decoded from UTF-16LE).
    pub name: String,
    /// The raw UTF-16LE filename bytes.
    pub name_utf16: Vec<u16>,
}

impl FileNameAttr {
    /// Parse a $FILE_NAME attribute from its value bytes.
    ///
    /// The input should be the attribute value (not including the attribute header).
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < FILE_NAME_MIN_SIZE {
            log::error!("[ntfs::filename] buffer too small: {} bytes (need >= {})",
                buf.len(), FILE_NAME_MIN_SIZE);
            return None;
        }

        let name_length = buf[0x40];
        let namespace_byte = buf[0x41];
        let namespace = FileNamespace::from_u8(namespace_byte).unwrap_or_else(|| {
            log::warn!("[ntfs::filename] unknown namespace: 0x{:02X}, defaulting to POSIX",
                namespace_byte);
            FileNamespace::Posix
        });

        let name_bytes = name_length as usize * 2;
        let total_needed = 0x42 + name_bytes;
        if buf.len() < total_needed {
            log::error!("[ntfs::filename] buffer too small for name: {} bytes (need >= {})",
                buf.len(), total_needed);
            return None;
        }

        // Decode UTF-16LE filename
        let name_utf16: Vec<u16> = (0..name_length as usize)
            .map(|i| {
                let off = 0x42 + i * 2;
                u16::from_le_bytes([buf[off], buf[off + 1]])
            })
            .collect();
        let name = String::from_utf16_lossy(&name_utf16);

        let attr = FileNameAttr {
            parent_reference:     read_u64(buf, 0x00),
            creation_time:        read_u64(buf, 0x08),
            modification_time:    read_u64(buf, 0x10),
            mft_modification_time: read_u64(buf, 0x18),
            access_time:          read_u64(buf, 0x20),
            allocated_size:       read_u64(buf, 0x28),
            real_size:            read_u64(buf, 0x30),
            flags:                read_u32(buf, 0x38),
            ea_reparse:           read_u32(buf, 0x3C),
            name_length,
            namespace,
            name: name.clone(),
            name_utf16,
        };

        log::debug!("[ntfs::filename] parsed: name='{}', namespace={}, parent=0x{:012X}, \
            flags=0x{:08X}, size={}",
            name, namespace.name(),
            attr.parent_reference & 0x0000_FFFF_FFFF_FFFF,
            attr.flags, attr.real_size);
        log::trace!("[ntfs::filename] times: created={}, modified={}, mft_modified={}, accessed={}",
            attr.creation_time, attr.modification_time,
            attr.mft_modification_time, attr.access_time);

        Some(attr)
    }

    /// Serialize this $FILE_NAME attribute value for writing.
    pub fn to_bytes(&self) -> Vec<u8> {
        let name_bytes = self.name_length as usize * 2;
        let total = 0x42 + name_bytes;
        let mut buf = alloc::vec![0u8; total];

        write_u64(&mut buf, 0x00, self.parent_reference);
        write_u64(&mut buf, 0x08, self.creation_time);
        write_u64(&mut buf, 0x10, self.modification_time);
        write_u64(&mut buf, 0x18, self.mft_modification_time);
        write_u64(&mut buf, 0x20, self.access_time);
        write_u64(&mut buf, 0x28, self.allocated_size);
        write_u64(&mut buf, 0x30, self.real_size);
        write_u32(&mut buf, 0x38, self.flags);
        write_u32(&mut buf, 0x3C, self.ea_reparse);
        buf[0x40] = self.name_length;
        buf[0x41] = self.namespace as u8;

        for (i, &ch) in self.name_utf16.iter().enumerate() {
            let off = 0x42 + i * 2;
            buf[off..off + 2].copy_from_slice(&ch.to_le_bytes());
        }

        log::trace!("[ntfs::filename] serialized {} bytes for '{}'", buf.len(), self.name);
        buf
    }

    /// Whether this file is a directory.
    #[inline]
    pub fn is_directory(&self) -> bool {
        self.flags & FILE_ATTR_DIRECTORY != 0
    }

    /// Whether this file is hidden.
    #[inline]
    pub fn is_hidden(&self) -> bool {
        self.flags & FILE_ATTR_HIDDEN != 0
    }

    /// Whether this file is a system file.
    #[inline]
    pub fn is_system(&self) -> bool {
        self.flags & FILE_ATTR_SYSTEM != 0
    }

    /// Extract the parent directory MFT entry number (lower 48 bits of reference).
    #[inline]
    pub fn parent_entry_number(&self) -> u64 {
        self.parent_reference & 0x0000_FFFF_FFFF_FFFF
    }

    /// Convert a Windows FILETIME to a Unix timestamp (seconds since 1970-01-01).
    ///
    /// FILETIME is 100-nanosecond intervals since 1601-01-01 00:00:00 UTC.
    /// The offset between the two epochs is 11644473600 seconds.
    pub fn filetime_to_unix(filetime: u64) -> i64 {
        const EPOCH_DIFF: i64 = 11_644_473_600;
        (filetime as i64 / 10_000_000) - EPOCH_DIFF
    }

    /// Convert a Unix timestamp to a Windows FILETIME.
    pub fn unix_to_filetime(unix_secs: i64) -> u64 {
        const EPOCH_DIFF: i64 = 11_644_473_600;
        ((unix_secs + EPOCH_DIFF) * 10_000_000) as u64
    }
}

impl fmt::Debug for FileNameAttr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileNameAttr")
            .field("name", &self.name)
            .field("namespace", &self.namespace.name())
            .field("parent_entry", &self.parent_entry_number())
            .field("flags", &format_args!("0x{:08X}", self.flags))
            .field("real_size", &self.real_size)
            .field("is_directory", &self.is_directory())
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
fn write_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

#[inline]
fn write_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_le_bytes());
}
