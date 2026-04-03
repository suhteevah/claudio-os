//! SCSI command construction and response parsing.
//!
//! Implements the SCSI commands used by USB mass storage devices:
//! - **INQUIRY** (0x12): Device identification
//! - **READ_CAPACITY_10** (0x25): Disk geometry (last LBA + block size)
//! - **READ_10** (0x28): Read sectors
//! - **WRITE_10** (0x2A): Write sectors
//! - **TEST_UNIT_READY** (0x00): Check device readiness
//! - **REQUEST_SENSE** (0x03): Error details after failed command
//! - **MODE_SENSE** (0x1A): Device parameters and mode pages

use alloc::string::String;

/// SCSI operation codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScsiOpcode {
    TestUnitReady = 0x00,
    RequestSense = 0x03,
    Inquiry = 0x12,
    ModeSense6 = 0x1A,
    ReadCapacity10 = 0x25,
    Read10 = 0x28,
    Write10 = 0x2A,
}

/// High-level SCSI command abstraction.
#[derive(Debug, Clone)]
pub enum ScsiCommand {
    /// TEST UNIT READY — 6-byte CDB, no data transfer.
    TestUnitReady,

    /// REQUEST SENSE — 6-byte CDB, returns sense data (up to `alloc_len` bytes).
    RequestSense { alloc_len: u8 },

    /// INQUIRY — 6-byte CDB, returns device identification (up to `alloc_len` bytes).
    Inquiry { alloc_len: u8 },

    /// MODE SENSE(6) — 6-byte CDB, returns mode page data.
    ModeSense { page_code: u8, alloc_len: u8 },

    /// READ CAPACITY(10) — 10-byte CDB, returns 8-byte capacity descriptor.
    ReadCapacity10,

    /// READ(10) — 10-byte CDB, reads `block_count` blocks starting at `lba`.
    Read10 { lba: u32, block_count: u16 },

    /// WRITE(10) — 10-byte CDB, writes `block_count` blocks starting at `lba`.
    Write10 { lba: u32, block_count: u16 },
}

impl ScsiCommand {
    /// Build the Command Descriptor Block (CDB) bytes for this command.
    pub fn to_cdb(&self) -> ([u8; 16], u8) {
        let mut cdb = [0u8; 16];

        let len = match self {
            Self::TestUnitReady => {
                cdb[0] = ScsiOpcode::TestUnitReady as u8;
                log::trace!("SCSI: TEST UNIT READY CDB");
                6
            }

            Self::RequestSense { alloc_len } => {
                cdb[0] = ScsiOpcode::RequestSense as u8;
                cdb[4] = *alloc_len;
                log::trace!("SCSI: REQUEST SENSE CDB, alloc_len={}", alloc_len);
                6
            }

            Self::Inquiry { alloc_len } => {
                cdb[0] = ScsiOpcode::Inquiry as u8;
                cdb[4] = *alloc_len;
                log::trace!("SCSI: INQUIRY CDB, alloc_len={}", alloc_len);
                6
            }

            Self::ModeSense { page_code, alloc_len } => {
                cdb[0] = ScsiOpcode::ModeSense6 as u8;
                cdb[2] = *page_code & 0x3F; // page code in bits 5:0
                cdb[4] = *alloc_len;
                log::trace!("SCSI: MODE SENSE(6) CDB, page={:#04x}, alloc_len={}", page_code, alloc_len);
                6
            }

            Self::ReadCapacity10 => {
                cdb[0] = ScsiOpcode::ReadCapacity10 as u8;
                log::trace!("SCSI: READ CAPACITY(10) CDB");
                10
            }

            Self::Read10 { lba, block_count } => {
                cdb[0] = ScsiOpcode::Read10 as u8;
                cdb[2..6].copy_from_slice(&lba.to_be_bytes());
                cdb[7..9].copy_from_slice(&block_count.to_be_bytes());
                log::trace!("SCSI: READ(10) CDB, lba={}, count={}", lba, block_count);
                10
            }

            Self::Write10 { lba, block_count } => {
                cdb[0] = ScsiOpcode::Write10 as u8;
                cdb[2..6].copy_from_slice(&lba.to_be_bytes());
                cdb[7..9].copy_from_slice(&block_count.to_be_bytes());
                log::trace!("SCSI: WRITE(10) CDB, lba={}, count={}", lba, block_count);
                10
            }
        };

        (cdb, len)
    }

    /// Returns the expected data transfer length in bytes for this command.
    ///
    /// For READ_10/WRITE_10, `block_size` must be provided.
    pub fn data_transfer_length(&self, block_size: u32) -> u32 {
        match self {
            Self::TestUnitReady => 0,
            Self::RequestSense { alloc_len } => *alloc_len as u32,
            Self::Inquiry { alloc_len } => *alloc_len as u32,
            Self::ModeSense { alloc_len, .. } => *alloc_len as u32,
            Self::ReadCapacity10 => 8,
            Self::Read10 { block_count, .. } => (*block_count as u32) * block_size,
            Self::Write10 { block_count, .. } => (*block_count as u32) * block_size,
        }
    }

    /// Returns `true` if this is a device-to-host (IN) data transfer.
    pub fn is_data_in(&self) -> bool {
        match self {
            Self::TestUnitReady => false,
            Self::RequestSense { .. } => true,
            Self::Inquiry { .. } => true,
            Self::ModeSense { .. } => true,
            Self::ReadCapacity10 => true,
            Self::Read10 { .. } => true,
            Self::Write10 { .. } => false,
        }
    }
}

// ── INQUIRY response ────────────────────────────────────────────────────────

/// Standard INQUIRY response data (parsed from the first 36 bytes).
#[derive(Debug, Clone)]
pub struct InquiryData {
    /// Peripheral device type (bits 4:0 of byte 0).
    /// 0x00 = Direct access block device (disk).
    pub peripheral_device_type: u8,
    /// Removable Media Bit (bit 7 of byte 1).
    pub removable: bool,
    /// T10 vendor identification (8 bytes, ASCII, space-padded).
    pub vendor: String,
    /// Product identification (16 bytes, ASCII, space-padded).
    pub product: String,
    /// Product revision level (4 bytes, ASCII).
    pub revision: String,
}

impl InquiryData {
    /// Parse from the raw INQUIRY response buffer (at least 36 bytes).
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 36 {
            log::error!("SCSI: INQUIRY response too short: {} bytes (need 36)", buf.len());
            return None;
        }

        let peripheral_device_type = buf[0] & 0x1F;
        let removable = (buf[1] & 0x80) != 0;

        let vendor = core::str::from_utf8(&buf[8..16])
            .unwrap_or("<invalid>")
            .trim()
            .into();
        let product = core::str::from_utf8(&buf[16..32])
            .unwrap_or("<invalid>")
            .trim()
            .into();
        let revision = core::str::from_utf8(&buf[32..36])
            .unwrap_or("<invalid>")
            .trim()
            .into();

        log::info!(
            "SCSI: INQUIRY — type={:#04x} removable={} vendor=\"{}\" product=\"{}\" rev=\"{}\"",
            peripheral_device_type, removable, vendor, product, revision,
        );

        Some(Self {
            peripheral_device_type,
            removable,
            vendor,
            product,
            revision,
        })
    }
}

// ── READ CAPACITY(10) response ──────────────────────────────────────────────

/// READ CAPACITY(10) response: last LBA and block size.
#[derive(Debug, Clone, Copy)]
pub struct ReadCapacity10Data {
    /// Last logical block address (0-based). Total blocks = last_lba + 1.
    pub last_lba: u32,
    /// Block (sector) size in bytes (typically 512 or 4096).
    pub block_size: u32,
}

impl ReadCapacity10Data {
    /// Parse from the 8-byte READ CAPACITY(10) response.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 8 {
            log::error!("SCSI: READ CAPACITY(10) response too short: {} bytes (need 8)", buf.len());
            return None;
        }

        let last_lba = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let block_size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);

        let total_blocks = last_lba as u64 + 1;
        let total_bytes = total_blocks * block_size as u64;
        let total_mib = total_bytes / (1024 * 1024);

        log::info!(
            "SCSI: READ CAPACITY(10) — last_lba={} block_size={} total={} MiB",
            last_lba, block_size, total_mib,
        );

        Some(Self { last_lba, block_size })
    }

    /// Total number of logical blocks on the device.
    pub fn total_blocks(&self) -> u64 {
        self.last_lba as u64 + 1
    }

    /// Total device capacity in bytes.
    pub fn total_bytes(&self) -> u64 {
        self.total_blocks() * self.block_size as u64
    }
}

// ── REQUEST SENSE response ──────────────────────────────────────────────────

/// Parsed REQUEST SENSE response data.
#[derive(Debug, Clone, Copy)]
pub struct SenseData {
    /// Sense key (bits 3:0 of byte 2).
    pub sense_key: u8,
    /// Additional Sense Code (byte 12).
    pub asc: u8,
    /// Additional Sense Code Qualifier (byte 13).
    pub ascq: u8,
}

impl SenseData {
    /// Parse from the raw REQUEST SENSE response (at least 14 bytes for full info).
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 14 {
            log::warn!("SCSI: REQUEST SENSE response short: {} bytes", buf.len());
            if buf.len() >= 3 {
                // Minimal parse: at least sense key
                return Some(Self {
                    sense_key: buf[2] & 0x0F,
                    asc: 0,
                    ascq: 0,
                });
            }
            return None;
        }

        let sense_key = buf[2] & 0x0F;
        let asc = buf[12];
        let ascq = buf[13];

        log::info!(
            "SCSI: REQUEST SENSE — key={:#04x} ASC={:#04x} ASCQ={:#04x} ({})",
            sense_key, asc, ascq, sense_key_name(sense_key),
        );

        Some(Self { sense_key, asc, ascq })
    }
}

/// Human-readable sense key names.
fn sense_key_name(key: u8) -> &'static str {
    match key {
        0x00 => "NO SENSE",
        0x01 => "RECOVERED ERROR",
        0x02 => "NOT READY",
        0x03 => "MEDIUM ERROR",
        0x04 => "HARDWARE ERROR",
        0x05 => "ILLEGAL REQUEST",
        0x06 => "UNIT ATTENTION",
        0x07 => "DATA PROTECT",
        0x08 => "BLANK CHECK",
        0x0B => "ABORTED COMMAND",
        0x0E => "MISCOMPARE",
        _ => "UNKNOWN",
    }
}

// ── MODE SENSE(6) response ──────────────────────────────────────────────────

/// Parsed MODE SENSE(6) response header.
#[derive(Debug, Clone, Copy)]
pub struct ModeSenseHeader {
    /// Mode data length (byte 0).
    pub data_length: u8,
    /// Medium type (byte 1).
    pub medium_type: u8,
    /// Device-specific parameter (byte 2).
    /// Bit 7 = write-protect.
    pub device_specific: u8,
    /// Block descriptor length (byte 3).
    pub block_descriptor_length: u8,
}

impl ModeSenseHeader {
    /// Parse from the raw MODE SENSE(6) response.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 4 {
            log::error!("SCSI: MODE SENSE(6) response too short: {} bytes", buf.len());
            return None;
        }

        let header = Self {
            data_length: buf[0],
            medium_type: buf[1],
            device_specific: buf[2],
            block_descriptor_length: buf[3],
        };

        let write_protected = (header.device_specific & 0x80) != 0;
        log::info!(
            "SCSI: MODE SENSE — medium_type={:#04x} write_protect={} blk_desc_len={}",
            header.medium_type, write_protected, header.block_descriptor_length,
        );

        Some(header)
    }

    /// Returns `true` if the device is write-protected.
    pub fn is_write_protected(&self) -> bool {
        (self.device_specific & 0x80) != 0
    }
}
