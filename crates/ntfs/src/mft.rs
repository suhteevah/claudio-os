//! Master File Table (MFT) entry parsing and writing.
//!
//! The MFT is the central data structure of NTFS. Every file, directory, and
//! metadata object on the volume has at least one MFT entry (also called a
//! file record segment). Each entry is typically 1024 bytes and begins with
//! the magic signature "FILE".
//!
//! The first 16 MFT entries are reserved for system metadata files:
//! - Entry 0:  $MFT       — The MFT itself
//! - Entry 1:  $MFTMirr   — Mirror of the first 4 MFT entries
//! - Entry 2:  $LogFile    — NTFS journal / transaction log
//! - Entry 3:  $Volume     — Volume metadata (name, version, flags)
//! - Entry 4:  $AttrDef    — Attribute definitions
//! - Entry 5:  $Root (.)   — Root directory
//! - Entry 6:  $Bitmap     — Volume cluster allocation bitmap
//! - Entry 7:  $Boot       — Boot sector and bootstrap code
//! - Entry 8:  $BadClus    — Bad cluster list
//! - Entry 9:  $Secure     — Security descriptors
//! - Entry 10: $UpCase     — Uppercase mapping table
//! - Entry 11: $Extend     — Directory for extended metadata ($ObjId, $Quota, $Reparse, $UsnJrnl)
//! - Entries 12-15: Reserved for future use
//!
//! Reference: <https://flatcap.github.io/linux-ntfs/ntfs/concepts/file_record.html>

use alloc::vec::Vec;
use core::fmt;

use crate::attribute::{AttributeHeader, AttributeType};

/// MFT entry magic signature: "FILE" in ASCII.
pub const MFT_ENTRY_MAGIC: &[u8; 4] = b"FILE";

/// MFT entry magic as a u32 (little-endian "FILE").
pub const MFT_ENTRY_MAGIC_U32: u32 = 0x454C4946;

// --- Well-known MFT entry numbers ---

/// $MFT — The Master File Table itself.
pub const MFT_ENTRY_MFT: u64 = 0;
/// $MFTMirr — Mirror of first 4 MFT entries.
pub const MFT_ENTRY_MFTMIRR: u64 = 1;
/// $LogFile — NTFS transaction log.
pub const MFT_ENTRY_LOGFILE: u64 = 2;
/// $Volume — Volume metadata (name, version, flags).
pub const MFT_ENTRY_VOLUME: u64 = 3;
/// $AttrDef — Attribute definition table.
pub const MFT_ENTRY_ATTRDEF: u64 = 4;
/// $Root — Root directory (\ or /).
pub const MFT_ENTRY_ROOT: u64 = 5;
/// $Bitmap — Volume cluster allocation bitmap.
pub const MFT_ENTRY_BITMAP: u64 = 6;
/// $Boot — Boot sector and bootstrap code.
pub const MFT_ENTRY_BOOT: u64 = 7;
/// $BadClus — Bad cluster tracking.
pub const MFT_ENTRY_BADCLUS: u64 = 8;
/// $Secure — Shared security descriptors.
pub const MFT_ENTRY_SECURE: u64 = 9;
/// $UpCase — Unicode uppercase mapping table.
pub const MFT_ENTRY_UPCASE: u64 = 10;
/// $Extend — Extended system metadata directory.
pub const MFT_ENTRY_EXTEND: u64 = 11;
/// First MFT entry available for user files.
pub const MFT_ENTRY_FIRST_USER: u64 = 16;

// --- MFT entry flags ---

/// Entry is in use (allocated).
pub const MFT_ENTRY_FLAG_IN_USE: u16 = 0x0001;
/// Entry is a directory.
pub const MFT_ENTRY_FLAG_DIRECTORY: u16 = 0x0002;
/// Entry is in the $Extend directory.
pub const MFT_ENTRY_FLAG_IN_EXTEND: u16 = 0x0004;
/// Entry has a view index (used for $Secure, $ObjId, etc.).
pub const MFT_ENTRY_FLAG_VIEW_INDEX: u16 = 0x0008;

/// Default MFT record size (1024 bytes).
pub const MFT_RECORD_DEFAULT_SIZE: usize = 1024;

/// Offset of the end-of-attributes marker.
pub const ATTR_END_MARKER: u32 = 0xFFFFFFFF;

/// Parsed MFT entry header (first 42 bytes of the file record).
#[derive(Clone)]
pub struct MftEntryHeader {
    /// Magic signature, must be "FILE" (0x454C4946).
    pub magic: u32,
    /// Offset to the update sequence array (USA) from start of entry.
    pub usa_offset: u16,
    /// Size of the update sequence array in u16 words (including the check value).
    pub usa_count: u16,
    /// $LogFile sequence number (LSN) for this entry.
    pub logfile_seq_number: u64,
    /// Sequence number: incremented each time this entry is reused.
    pub sequence_number: u16,
    /// Hard link count (number of directory entries pointing here).
    pub hard_link_count: u16,
    /// Byte offset to the first attribute from start of entry.
    pub first_attribute_offset: u16,
    /// Flags (MFT_ENTRY_FLAG_IN_USE, MFT_ENTRY_FLAG_DIRECTORY, etc.).
    pub flags: u16,
    /// Number of bytes actually used in this MFT entry.
    pub used_size: u32,
    /// Total allocated size of this MFT entry (typically 1024).
    pub allocated_size: u32,
    /// Base MFT record reference (0 if this is the base record).
    /// For extension records, this points to the base record.
    pub base_record: u64,
    /// Next attribute ID to assign.
    pub next_attribute_id: u16,
}

impl MftEntryHeader {
    /// Parse the MFT entry header from the first 42+ bytes of a file record.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 42 {
            log::error!("[ntfs::mft] header buffer too small: {} bytes (need >= 42)", buf.len());
            return None;
        }

        let magic = read_u32(buf, 0x00);
        if magic != MFT_ENTRY_MAGIC_U32 {
            log::error!("[ntfs::mft] invalid magic: 0x{:08X} (expected 0x{:08X} 'FILE')",
                magic, MFT_ENTRY_MAGIC_U32);
            return None;
        }

        let header = MftEntryHeader {
            magic,
            usa_offset:            read_u16(buf, 0x04),
            usa_count:             read_u16(buf, 0x06),
            logfile_seq_number:    read_u64(buf, 0x08),
            sequence_number:       read_u16(buf, 0x10),
            hard_link_count:       read_u16(buf, 0x12),
            first_attribute_offset: read_u16(buf, 0x14),
            flags:                 read_u16(buf, 0x16),
            used_size:             read_u32(buf, 0x18),
            allocated_size:        read_u32(buf, 0x1C),
            base_record:           read_u64(buf, 0x20),
            next_attribute_id:     read_u16(buf, 0x28),
        };

        log::debug!("[ntfs::mft] header: magic=FILE, seq={}, links={}, flags=0x{:04X}, \
            used={}, allocated={}, first_attr_off=0x{:04X}",
            header.sequence_number, header.hard_link_count, header.flags,
            header.used_size, header.allocated_size, header.first_attribute_offset);
        log::trace!("[ntfs::mft] usa_offset=0x{:04X}, usa_count={}, lsn={}, base_record=0x{:016X}",
            header.usa_offset, header.usa_count,
            header.logfile_seq_number, header.base_record);

        Some(header)
    }

    /// Whether this entry is in use (allocated).
    #[inline]
    pub fn is_in_use(&self) -> bool {
        self.flags & MFT_ENTRY_FLAG_IN_USE != 0
    }

    /// Whether this entry represents a directory.
    #[inline]
    pub fn is_directory(&self) -> bool {
        self.flags & MFT_ENTRY_FLAG_DIRECTORY != 0
    }

    /// Whether this is a base record (not an extension record).
    #[inline]
    pub fn is_base_record(&self) -> bool {
        self.base_record == 0
    }
}

impl fmt::Debug for MftEntryHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MftEntryHeader")
            .field("magic", &"FILE")
            .field("sequence_number", &self.sequence_number)
            .field("hard_link_count", &self.hard_link_count)
            .field("flags", &format_args!("0x{:04X}", self.flags))
            .field("in_use", &self.is_in_use())
            .field("is_directory", &self.is_directory())
            .field("used_size", &self.used_size)
            .field("allocated_size", &self.allocated_size)
            .finish()
    }
}

/// A complete parsed MFT entry with fixup applied and attributes extracted.
#[derive(Clone)]
pub struct MftEntry {
    /// The entry header.
    pub header: MftEntryHeader,
    /// The raw bytes of the entry after fixup array has been applied.
    pub data: Vec<u8>,
}

impl MftEntry {
    /// Parse a complete MFT entry from raw bytes read from disk.
    ///
    /// This applies the Update Sequence Array (fixup) before parsing.
    /// `record_size` should come from the boot sector (typically 1024).
    pub fn from_bytes(raw: &[u8], record_size: usize) -> Option<Self> {
        if raw.len() < record_size {
            log::error!("[ntfs::mft] entry buffer too small: {} bytes (need >= {})",
                raw.len(), record_size);
            return None;
        }

        // Parse header first to get USA info
        let header = MftEntryHeader::from_bytes(raw)?;

        // Copy the data so we can apply fixups
        let mut data = alloc::vec![0u8; record_size];
        data[..record_size].copy_from_slice(&raw[..record_size]);

        // Apply fixup array (Update Sequence Array)
        let usa_offset = header.usa_offset as usize;
        let usa_count = header.usa_count as usize;

        if usa_count < 1 {
            log::error!("[ntfs::mft] invalid USA count: {}", usa_count);
            return None;
        }

        if usa_offset + usa_count * 2 > record_size {
            log::error!("[ntfs::mft] USA extends beyond record: offset={}, count={}, record_size={}",
                usa_offset, usa_count, record_size);
            return None;
        }

        // The first u16 is the check value
        let check_value = read_u16(&data, usa_offset);
        log::trace!("[ntfs::mft] applying fixup: usa_offset=0x{:04X}, usa_count={}, check=0x{:04X}",
            usa_offset, usa_count, check_value);

        // Apply fixups: for each sector, verify the last 2 bytes match check_value,
        // then replace them with the stored original bytes
        let sector_size = 512usize; // NTFS always uses 512-byte fixup sectors
        for i in 1..usa_count {
            let sector_end = i * sector_size;
            if sector_end > record_size {
                break;
            }
            let fixup_pos = sector_end - 2;
            let stored_value = read_u16(&data, fixup_pos);

            if stored_value != check_value {
                log::error!("[ntfs::mft] fixup mismatch at sector {}: found 0x{:04X}, expected 0x{:04X}",
                    i, stored_value, check_value);
                return None;
            }

            // Replace with the real bytes from the USA
            let original = read_u16(&data, usa_offset + i * 2);
            write_u16(&mut data, fixup_pos, original);
            log::trace!("[ntfs::mft] fixup sector {}: replaced 0x{:04X} with 0x{:04X} at offset 0x{:04X}",
                i, check_value, original, fixup_pos);
        }

        Some(MftEntry { header, data })
    }

    /// Serialize this MFT entry back to raw bytes for writing to disk.
    ///
    /// This re-applies the fixup array (Update Sequence Array) so the
    /// on-disk format has the check values at sector boundaries.
    pub fn to_bytes(&self) -> Vec<u8> {
        let record_size = self.data.len();
        let mut buf = self.data.clone();

        let usa_offset = self.header.usa_offset as usize;
        let usa_count = self.header.usa_count as usize;

        if usa_count < 1 || usa_offset + usa_count * 2 > record_size {
            log::warn!("[ntfs::mft] cannot apply fixup for serialization, returning raw data");
            return buf;
        }

        // Generate a check value (use existing or increment)
        let check_value = read_u16(&buf, usa_offset);
        let sector_size = 512usize;

        for i in 1..usa_count {
            let sector_end = i * sector_size;
            if sector_end > record_size {
                break;
            }
            let fixup_pos = sector_end - 2;

            // Store the real bytes in the USA
            let original = read_u16(&buf, fixup_pos);
            write_u16(&mut buf, usa_offset + i * 2, original);

            // Write the check value at the sector boundary
            write_u16(&mut buf, fixup_pos, check_value);
            log::trace!("[ntfs::mft] serialize fixup sector {}: stored 0x{:04X}, wrote check 0x{:04X}",
                i, original, check_value);
        }

        log::trace!("[ntfs::mft] serialized {} byte MFT entry", buf.len());
        buf
    }

    /// Iterate over all attributes in this MFT entry.
    ///
    /// Returns an iterator that yields `AttributeHeader` for each attribute.
    /// The iterator stops when it encounters the end marker (0xFFFFFFFF) or
    /// runs past the used_size.
    pub fn attributes(&self) -> MftAttributeIter<'_> {
        MftAttributeIter {
            data: &self.data,
            offset: self.header.first_attribute_offset as usize,
            used_size: self.header.used_size as usize,
        }
    }

    /// Find the first attribute of a given type in this MFT entry.
    pub fn find_attribute(&self, attr_type: AttributeType) -> Option<(AttributeHeader, usize)> {
        log::trace!("[ntfs::mft] searching for attribute type 0x{:08X}", attr_type as u32);
        for (hdr, offset) in self.attributes() {
            if hdr.attr_type == attr_type {
                log::trace!("[ntfs::mft] found attribute type 0x{:08X} at offset 0x{:04X}",
                    attr_type as u32, offset);
                return Some((hdr, offset));
            }
        }
        log::trace!("[ntfs::mft] attribute type 0x{:08X} not found", attr_type as u32);
        None
    }

    /// Find all attributes of a given type in this MFT entry.
    pub fn find_all_attributes(&self, attr_type: AttributeType) -> Vec<(AttributeHeader, usize)> {
        let mut results = Vec::new();
        for (hdr, offset) in self.attributes() {
            if hdr.attr_type == attr_type {
                results.push((hdr, offset));
            }
        }
        log::trace!("[ntfs::mft] found {} attributes of type 0x{:08X}",
            results.len(), attr_type as u32);
        results
    }

    /// Get the resident attribute data for an attribute at the given offset.
    ///
    /// Returns `None` if the attribute is non-resident or the offset is invalid.
    pub fn resident_data(&self, attr_offset: usize) -> Option<&[u8]> {
        let hdr = AttributeHeader::from_bytes(&self.data[attr_offset..])?;
        if hdr.non_resident {
            log::trace!("[ntfs::mft] attribute at 0x{:04X} is non-resident", attr_offset);
            return None;
        }
        let res = crate::attribute::ResidentHeader::from_bytes(
            &self.data[attr_offset + AttributeHeader::HEADER_SIZE..]
        )?;
        let data_start = attr_offset + res.value_offset as usize;
        let data_end = data_start + res.value_length as usize;
        if data_end > self.data.len() {
            log::error!("[ntfs::mft] resident data extends beyond record: {}..{} > {}",
                data_start, data_end, self.data.len());
            return None;
        }
        log::trace!("[ntfs::mft] resident data at 0x{:04X}..0x{:04X} ({} bytes)",
            data_start, data_end, res.value_length);
        Some(&self.data[data_start..data_end])
    }

    /// Get the non-resident attribute header for data run decoding.
    pub fn non_resident_header(&self, attr_offset: usize) -> Option<crate::attribute::NonResidentHeader> {
        let hdr = AttributeHeader::from_bytes(&self.data[attr_offset..])?;
        if !hdr.non_resident {
            log::trace!("[ntfs::mft] attribute at 0x{:04X} is resident", attr_offset);
            return None;
        }
        crate::attribute::NonResidentHeader::from_bytes(
            &self.data[attr_offset + AttributeHeader::HEADER_SIZE..]
        )
    }

    /// Get the raw data runs bytes for a non-resident attribute.
    pub fn data_run_bytes(&self, attr_offset: usize) -> Option<&[u8]> {
        let hdr = AttributeHeader::from_bytes(&self.data[attr_offset..])?;
        if !hdr.non_resident {
            return None;
        }
        let nr = crate::attribute::NonResidentHeader::from_bytes(
            &self.data[attr_offset + AttributeHeader::HEADER_SIZE..]
        )?;
        let runs_start = attr_offset + nr.mapping_pairs_offset as usize;
        let runs_end = attr_offset + hdr.length as usize;
        if runs_end > self.data.len() {
            log::error!("[ntfs::mft] data runs extend beyond record");
            return None;
        }
        log::trace!("[ntfs::mft] data runs at 0x{:04X}..0x{:04X}", runs_start, runs_end);
        Some(&self.data[runs_start..runs_end])
    }
}

impl fmt::Debug for MftEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MftEntry")
            .field("header", &self.header)
            .field("data_len", &self.data.len())
            .finish()
    }
}

/// Iterator over attributes within an MFT entry.
pub struct MftAttributeIter<'a> {
    data: &'a [u8],
    offset: usize,
    used_size: usize,
}

impl<'a> Iterator for MftAttributeIter<'a> {
    type Item = (AttributeHeader, usize);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.offset + 4 > self.used_size || self.offset + 4 > self.data.len() {
                return None;
            }

            // Check for end marker
            let type_val = read_u32(self.data, self.offset);
            if type_val == ATTR_END_MARKER {
                log::trace!("[ntfs::mft] attribute end marker at offset 0x{:04X}", self.offset);
                return None;
            }

            let hdr = AttributeHeader::from_bytes(&self.data[self.offset..])?;
            if hdr.length == 0 {
                log::error!("[ntfs::mft] zero-length attribute at offset 0x{:04X}", self.offset);
                return None;
            }

            let current_offset = self.offset;
            self.offset += hdr.length as usize;

            log::trace!("[ntfs::mft] attribute at 0x{:04X}: type=0x{:08X}, len={}, non_resident={}",
                current_offset, hdr.attr_type as u32, hdr.length, hdr.non_resident);

            return Some((hdr, current_offset));
        }
    }
}

/// Extract the MFT entry number from an MFT reference (lower 48 bits).
#[inline]
pub fn mft_reference_number(reference: u64) -> u64 {
    reference & 0x0000_FFFF_FFFF_FFFF
}

/// Extract the sequence number from an MFT reference (upper 16 bits).
#[inline]
pub fn mft_reference_sequence(reference: u64) -> u16 {
    (reference >> 48) as u16
}

/// Build an MFT reference from an entry number and sequence number.
#[inline]
pub fn make_mft_reference(entry_number: u64, sequence: u16) -> u64 {
    (entry_number & 0x0000_FFFF_FFFF_FFFF) | ((sequence as u64) << 48)
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

#[inline]
fn write_u16(buf: &mut [u8], offset: usize, val: u16) {
    buf[offset..offset + 2].copy_from_slice(&val.to_le_bytes());
}
