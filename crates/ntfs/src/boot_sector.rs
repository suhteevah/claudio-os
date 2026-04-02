//! NTFS boot sector / BIOS Parameter Block (BPB) parsing.
//!
//! The boot sector occupies the first 512 bytes of an NTFS volume and contains
//! all geometry information needed to locate the MFT and interpret the volume.
//!
//! Reference: <https://docs.microsoft.com/en-us/windows/win32/fileio/master-file-table>

use alloc::vec::Vec;
use core::fmt;

/// The OEM ID that identifies an NTFS volume, always "NTFS    " (8 bytes, space-padded).
pub const NTFS_OEM_ID: &[u8; 8] = b"NTFS    ";

/// Boot sector signature at bytes 510-511 (0x55, 0xAA).
pub const BOOT_SECTOR_SIGNATURE: u16 = 0xAA55;

/// Size of the NTFS boot sector on disk (512 bytes).
pub const BOOT_SECTOR_SIZE: usize = 512;

/// Parsed NTFS boot sector / BPB.
///
/// Contains all fields needed to locate the MFT and compute cluster sizes.
/// Fields are stored in native endian (converted from little-endian on disk).
#[derive(Clone)]
pub struct BootSector {
    /// Jump instruction (3 bytes, typically 0xEB 0x52 0x90).
    pub jump: [u8; 3],
    /// OEM ID, must be "NTFS    ".
    pub oem_id: [u8; 8],
    /// Bytes per sector (typically 512).
    pub bytes_per_sector: u16,
    /// Sectors per cluster (power of 2: 1, 2, 4, 8, 16, 32, 64, 128).
    pub sectors_per_cluster: u8,
    /// Reserved sectors (always 0 for NTFS).
    pub reserved_sectors: u16,
    /// Always 0 for NTFS (no FATs).
    pub unused_fats: u8,
    /// Always 0 for NTFS.
    pub unused_root_entries: u16,
    /// Always 0 for NTFS (use total_sectors_64 instead).
    pub unused_total_sectors_16: u16,
    /// Media descriptor (0xF8 for hard disks).
    pub media_descriptor: u8,
    /// Always 0 for NTFS.
    pub unused_sectors_per_fat: u16,
    /// Sectors per track (CHS geometry, used by BIOS).
    pub sectors_per_track: u16,
    /// Number of heads (CHS geometry, used by BIOS).
    pub number_of_heads: u16,
    /// Hidden sectors (partition offset from start of disk).
    pub hidden_sectors: u32,
    /// Always 0 for NTFS.
    pub unused_total_sectors_32: u32,

    // --- NTFS-specific extended BPB ---

    /// Always 0x80 0x00 0x80 0x00 for NTFS.
    pub bpb_reserved: u32,
    /// Total number of sectors on the volume (64-bit).
    pub total_sectors: u64,
    /// Logical cluster number (LCN) of the $MFT.
    pub mft_cluster: u64,
    /// Logical cluster number (LCN) of the $MFTMirr.
    pub mft_mirror_cluster: u64,
    /// Clusters per MFT record.
    /// If negative, the MFT record size is 2^|value| bytes.
    /// If positive, it is the number of clusters per record.
    pub clusters_per_mft_record: i8,
    /// Clusters per index block.
    /// If negative, the index block size is 2^|value| bytes.
    /// If positive, it is the number of clusters per index block.
    pub clusters_per_index_block: i8,
    /// Volume serial number (64-bit).
    pub serial_number: u64,
    /// Checksum (unused by Windows).
    pub checksum: u32,
    /// Boot sector signature (0xAA55 at offset 510-511).
    pub signature: u16,
}

impl BootSector {
    /// Parse an NTFS boot sector from a 512-byte buffer.
    ///
    /// The buffer must contain the first sector of the NTFS volume.
    /// Returns `None` if the OEM ID is not "NTFS    " or the signature is invalid.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < BOOT_SECTOR_SIZE {
            log::error!("[ntfs::boot_sector] buffer too small: {} bytes (need >= {})",
                buf.len(), BOOT_SECTOR_SIZE);
            return None;
        }

        // Validate OEM ID
        let oem_id: [u8; 8] = [
            buf[3], buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10],
        ];
        if &oem_id != NTFS_OEM_ID {
            log::error!("[ntfs::boot_sector] invalid OEM ID: {:?} (expected {:?})",
                &oem_id, NTFS_OEM_ID);
            return None;
        }

        let signature = read_u16(buf, 510);
        if signature != BOOT_SECTOR_SIGNATURE {
            log::error!("[ntfs::boot_sector] invalid boot signature: 0x{:04X} (expected 0x{:04X})",
                signature, BOOT_SECTOR_SIGNATURE);
            return None;
        }

        let mut jump = [0u8; 3];
        jump.copy_from_slice(&buf[0..3]);

        let bs = BootSector {
            jump,
            oem_id,
            bytes_per_sector:        read_u16(buf, 0x0B),
            sectors_per_cluster:     buf[0x0D],
            reserved_sectors:        read_u16(buf, 0x0E),
            unused_fats:             buf[0x10],
            unused_root_entries:     read_u16(buf, 0x11),
            unused_total_sectors_16: read_u16(buf, 0x13),
            media_descriptor:        buf[0x15],
            unused_sectors_per_fat:  read_u16(buf, 0x16),
            sectors_per_track:       read_u16(buf, 0x18),
            number_of_heads:         read_u16(buf, 0x1A),
            hidden_sectors:          read_u32(buf, 0x1C),
            unused_total_sectors_32: read_u32(buf, 0x20),
            bpb_reserved:            read_u32(buf, 0x24),
            total_sectors:           read_u64(buf, 0x28),
            mft_cluster:             read_u64(buf, 0x30),
            mft_mirror_cluster:      read_u64(buf, 0x38),
            clusters_per_mft_record: buf[0x40] as i8,
            clusters_per_index_block: buf[0x44] as i8,
            serial_number:           read_u64(buf, 0x48),
            checksum:                read_u32(buf, 0x50),
            signature,
        };

        log::info!("[ntfs::boot_sector] parsed: bytes_per_sector={}, sectors_per_cluster={}, \
            total_sectors={}, mft_cluster={}, mft_mirror_cluster={}",
            bs.bytes_per_sector, bs.sectors_per_cluster,
            bs.total_sectors, bs.mft_cluster, bs.mft_mirror_cluster);
        log::debug!("[ntfs::boot_sector] clusters_per_mft_record={}, clusters_per_index_block={}, \
            serial=0x{:016X}, media=0x{:02X}",
            bs.clusters_per_mft_record, bs.clusters_per_index_block,
            bs.serial_number, bs.media_descriptor);
        log::debug!("[ntfs::boot_sector] mft_record_size={} bytes, index_block_size={} bytes",
            bs.mft_record_size(), bs.index_block_size());

        Some(bs)
    }

    /// Serialize the boot sector to a 512-byte buffer for writing to disk.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = alloc::vec![0u8; BOOT_SECTOR_SIZE];

        buf[0..3].copy_from_slice(&self.jump);
        buf[3..11].copy_from_slice(&self.oem_id);
        write_u16(&mut buf, 0x0B, self.bytes_per_sector);
        buf[0x0D] = self.sectors_per_cluster;
        write_u16(&mut buf, 0x0E, self.reserved_sectors);
        buf[0x10] = self.unused_fats;
        write_u16(&mut buf, 0x11, self.unused_root_entries);
        write_u16(&mut buf, 0x13, self.unused_total_sectors_16);
        buf[0x15] = self.media_descriptor;
        write_u16(&mut buf, 0x16, self.unused_sectors_per_fat);
        write_u16(&mut buf, 0x18, self.sectors_per_track);
        write_u16(&mut buf, 0x1A, self.number_of_heads);
        write_u32(&mut buf, 0x1C, self.hidden_sectors);
        write_u32(&mut buf, 0x20, self.unused_total_sectors_32);
        write_u32(&mut buf, 0x24, self.bpb_reserved);
        write_u64(&mut buf, 0x28, self.total_sectors);
        write_u64(&mut buf, 0x30, self.mft_cluster);
        write_u64(&mut buf, 0x38, self.mft_mirror_cluster);
        buf[0x40] = self.clusters_per_mft_record as u8;
        buf[0x44] = self.clusters_per_index_block as u8;
        write_u64(&mut buf, 0x48, self.serial_number);
        write_u32(&mut buf, 0x50, self.checksum);
        write_u16(&mut buf, 510, self.signature);

        log::trace!("[ntfs::boot_sector] serialized {} bytes to disk format", buf.len());
        buf
    }

    /// Bytes per cluster (bytes_per_sector * sectors_per_cluster).
    #[inline]
    pub fn cluster_size(&self) -> u64 {
        self.bytes_per_sector as u64 * self.sectors_per_cluster as u64
    }

    /// Size of a single MFT record in bytes.
    ///
    /// If `clusters_per_mft_record` is negative, the size is `2^|value|`.
    /// If positive, it is `clusters_per_mft_record * cluster_size`.
    #[inline]
    pub fn mft_record_size(&self) -> u64 {
        if self.clusters_per_mft_record < 0 {
            1u64 << (-self.clusters_per_mft_record as u64)
        } else {
            self.clusters_per_mft_record as u64 * self.cluster_size()
        }
    }

    /// Size of an index block (INDX) in bytes.
    ///
    /// If `clusters_per_index_block` is negative, the size is `2^|value|`.
    /// If positive, it is `clusters_per_index_block * cluster_size`.
    #[inline]
    pub fn index_block_size(&self) -> u64 {
        if self.clusters_per_index_block < 0 {
            1u64 << (-self.clusters_per_index_block as u64)
        } else {
            self.clusters_per_index_block as u64 * self.cluster_size()
        }
    }

    /// Byte offset of the MFT on the volume.
    #[inline]
    pub fn mft_byte_offset(&self) -> u64 {
        self.mft_cluster * self.cluster_size()
    }

    /// Byte offset of the MFT mirror on the volume.
    #[inline]
    pub fn mft_mirror_byte_offset(&self) -> u64 {
        self.mft_mirror_cluster * self.cluster_size()
    }

    /// Total volume size in bytes.
    #[inline]
    pub fn volume_size(&self) -> u64 {
        self.total_sectors * self.bytes_per_sector as u64
    }
}

impl fmt::Debug for BootSector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BootSector")
            .field("bytes_per_sector", &self.bytes_per_sector)
            .field("sectors_per_cluster", &self.sectors_per_cluster)
            .field("cluster_size", &self.cluster_size())
            .field("total_sectors", &self.total_sectors)
            .field("volume_size", &self.volume_size())
            .field("mft_cluster", &self.mft_cluster)
            .field("mft_mirror_cluster", &self.mft_mirror_cluster)
            .field("mft_record_size", &self.mft_record_size())
            .field("index_block_size", &self.index_block_size())
            .field("serial", &format_args!("0x{:016X}", self.serial_number))
            .finish()
    }
}

// --- Little-endian byte reading/writing helpers ---

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
