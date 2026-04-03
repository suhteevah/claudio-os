//! Bulk-Only Transport (BOT) protocol implementation.
//!
//! Implements the USB Mass Storage Bulk-Only Transport specification:
//! - **CBW** (Command Block Wrapper): 31-byte packet wrapping a SCSI CDB
//! - **CSW** (Command Status Wrapper): 13-byte status response
//! - **Reset Recovery**: Bulk-Only Mass Storage Reset + clear HALT on endpoints

use core::sync::atomic::{AtomicU32, Ordering};

/// CBW signature: "USBC" = 0x43425355.
pub const CBW_SIGNATURE: u32 = 0x4342_5355;

/// CSW signature: "USBS" = 0x53425355.
pub const CSW_SIGNATURE: u32 = 0x5342_5355;

/// CBW size in bytes.
pub const CBW_SIZE: usize = 31;

/// CSW size in bytes.
pub const CSW_SIZE: usize = 13;

/// Maximum SCSI CDB length embedded in a CBW.
pub const MAX_CDB_LENGTH: usize = 16;

/// Global tag counter for CBW/CSW matching.
static TAG_COUNTER: AtomicU32 = AtomicU32::new(1);

/// Errors from Bulk-Only Transport operations.
#[derive(Debug)]
pub enum BotError {
    /// CBW could not be sent to the bulk-out endpoint.
    CbwTransferFailed,
    /// CSW could not be received from the bulk-in endpoint.
    CswTransferFailed,
    /// CSW signature mismatch (not "USBS").
    CswSignatureInvalid(u32),
    /// CSW tag does not match the CBW tag we sent.
    CswTagMismatch { expected: u32, got: u32 },
    /// CSW reports a command failure (bCSWStatus = 0x01).
    CommandFailed,
    /// CSW reports a phase error (bCSWStatus = 0x02), needs reset recovery.
    PhaseError,
    /// Data transfer on bulk-in/out endpoint failed.
    DataTransferFailed,
    /// Bulk-Only Mass Storage Reset request failed.
    ResetFailed,
    /// Clear Feature HALT on endpoint failed.
    ClearHaltFailed,
    /// The device returned an unexpected CSW status byte.
    UnknownStatus(u8),
    /// Transfer timed out waiting for the device.
    Timeout,
}

impl core::fmt::Display for BotError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::CbwTransferFailed => write!(f, "CBW transfer failed"),
            Self::CswTransferFailed => write!(f, "CSW transfer failed"),
            Self::CswSignatureInvalid(sig) => write!(f, "CSW invalid signature: {:#010x}", sig),
            Self::CswTagMismatch { expected, got } => {
                write!(f, "CSW tag mismatch: expected {:#010x}, got {:#010x}", expected, got)
            }
            Self::CommandFailed => write!(f, "SCSI command failed (bCSWStatus=1)"),
            Self::PhaseError => write!(f, "BOT phase error (bCSWStatus=2), reset required"),
            Self::DataTransferFailed => write!(f, "bulk data transfer failed"),
            Self::ResetFailed => write!(f, "Bulk-Only Mass Storage Reset failed"),
            Self::ClearHaltFailed => write!(f, "Clear Feature HALT failed"),
            Self::UnknownStatus(s) => write!(f, "unknown CSW status: {:#04x}", s),
            Self::Timeout => write!(f, "BOT transfer timed out"),
        }
    }
}

/// CSW status codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CswStatus {
    /// Command passed ("good status").
    Passed = 0x00,
    /// Command failed.
    Failed = 0x01,
    /// Phase error — requires reset recovery.
    PhaseError = 0x02,
}

impl CswStatus {
    /// Parse a raw status byte.
    pub fn from_byte(b: u8) -> Result<Self, BotError> {
        match b {
            0x00 => Ok(Self::Passed),
            0x01 => Ok(Self::Failed),
            0x02 => Ok(Self::PhaseError),
            other => Err(BotError::UnknownStatus(other)),
        }
    }
}

// ── Command Block Wrapper (CBW) ─────────────────────────────────────────────

/// A 31-byte Command Block Wrapper as defined by the BOT specification.
///
/// Layout (little-endian):
/// ```text
/// Offset  Field                 Size
/// 0       dCBWSignature         4    (0x43425355)
/// 4       dCBWTag               4    (matches CSW)
/// 8       dCBWDataTransferLen   4    (bytes expected)
/// 12      bmCBWFlags            1    (bit 7: 0=out, 1=in)
/// 13      bCBWLUN               1    (bits 3:0)
/// 14      bCBWCBLength          1    (1..16)
/// 15      CBWCB                 16   (the SCSI CDB, zero-padded)
/// ```
#[derive(Debug, Clone)]
pub struct CommandBlockWrapper {
    /// Unique tag to match with the corresponding CSW.
    pub tag: u32,
    /// Number of bytes the host expects to transfer in the data phase.
    pub data_transfer_length: u32,
    /// Direction flag: `true` = device-to-host (IN), `false` = host-to-device (OUT).
    pub direction_in: bool,
    /// Logical Unit Number (typically 0).
    pub lun: u8,
    /// The SCSI Command Descriptor Block (1..16 bytes, zero-padded to 16).
    pub cdb: [u8; MAX_CDB_LENGTH],
    /// Actual length of the CDB (1..16).
    pub cdb_length: u8,
}

impl CommandBlockWrapper {
    /// Create a new CBW wrapping the given SCSI CDB.
    ///
    /// Automatically assigns a unique tag from the global counter.
    pub fn new(cdb: &[u8], data_transfer_length: u32, direction_in: bool, lun: u8) -> Self {
        let tag = TAG_COUNTER.fetch_add(1, Ordering::Relaxed);
        let cdb_length = cdb.len().min(MAX_CDB_LENGTH) as u8;

        let mut cdb_padded = [0u8; MAX_CDB_LENGTH];
        let copy_len = cdb.len().min(MAX_CDB_LENGTH);
        cdb_padded[..copy_len].copy_from_slice(&cdb[..copy_len]);

        log::trace!(
            "BOT: CBW tag={:#010x} xfer_len={} dir={} lun={} cdb_len={} cdb={:02x?}",
            tag, data_transfer_length,
            if direction_in { "IN" } else { "OUT" },
            lun, cdb_length, &cdb_padded[..copy_len],
        );

        Self {
            tag,
            data_transfer_length,
            direction_in,
            lun,
            cdb: cdb_padded,
            cdb_length,
        }
    }

    /// Serialize to a 31-byte little-endian buffer.
    pub fn to_bytes(&self) -> [u8; CBW_SIZE] {
        let mut buf = [0u8; CBW_SIZE];

        // dCBWSignature
        buf[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        // dCBWTag
        buf[4..8].copy_from_slice(&self.tag.to_le_bytes());
        // dCBWDataTransferLength
        buf[8..12].copy_from_slice(&self.data_transfer_length.to_le_bytes());
        // bmCBWFlags
        buf[12] = if self.direction_in { 0x80 } else { 0x00 };
        // bCBWLUN
        buf[13] = self.lun & 0x0F;
        // bCBWCBLength
        buf[14] = self.cdb_length;
        // CBWCB (16 bytes, zero-padded)
        buf[15..31].copy_from_slice(&self.cdb);

        buf
    }
}

// ── Command Status Wrapper (CSW) ────────────────────────────────────────────

/// A 13-byte Command Status Wrapper as defined by the BOT specification.
///
/// Layout (little-endian):
/// ```text
/// Offset  Field                 Size
/// 0       dCSWSignature         4    (0x53425355)
/// 4       dCSWTag               4    (matches CBW)
/// 8       dCSWDataResidue       4    (bytes NOT transferred)
/// 12      bCSWStatus            1    (0=pass, 1=fail, 2=phase error)
/// ```
#[derive(Debug, Clone)]
pub struct CommandStatusWrapper {
    /// Tag that must match the CBW this status corresponds to.
    pub tag: u32,
    /// Number of bytes from `dCBWDataTransferLength` NOT processed.
    pub data_residue: u32,
    /// Status of the command execution.
    pub status: CswStatus,
}

impl CommandStatusWrapper {
    /// Parse a 13-byte CSW from a bulk-in transfer.
    ///
    /// Validates the signature and parses the status byte.
    pub fn from_bytes(buf: &[u8; CSW_SIZE]) -> Result<Self, BotError> {
        let signature = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if signature != CSW_SIGNATURE {
            log::error!("BOT: CSW signature invalid: {:#010x} (expected {:#010x})", signature, CSW_SIGNATURE);
            return Err(BotError::CswSignatureInvalid(signature));
        }

        let tag = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let data_residue = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let status = CswStatus::from_byte(buf[12])?;

        log::trace!(
            "BOT: CSW tag={:#010x} residue={} status={:?}",
            tag, data_residue, status,
        );

        Ok(Self { tag, data_residue, status })
    }

    /// Validate this CSW against the expected CBW tag and check status.
    pub fn validate(&self, expected_tag: u32) -> Result<(), BotError> {
        if self.tag != expected_tag {
            log::error!(
                "BOT: CSW tag mismatch: expected {:#010x}, got {:#010x}",
                expected_tag, self.tag,
            );
            return Err(BotError::CswTagMismatch {
                expected: expected_tag,
                got: self.tag,
            });
        }

        match self.status {
            CswStatus::Passed => Ok(()),
            CswStatus::Failed => {
                log::warn!("BOT: command failed (bCSWStatus=1), residue={}", self.data_residue);
                Err(BotError::CommandFailed)
            }
            CswStatus::PhaseError => {
                log::error!("BOT: phase error — reset recovery required");
                Err(BotError::PhaseError)
            }
        }
    }
}

// ── Bulk-Only Transport engine ──────────────────────────────────────────────

/// Trait abstracting the raw USB bulk endpoint I/O.
///
/// This is implemented by the xHCI driver to provide bulk-in/out transfers
/// and the class-specific control requests needed for BOT reset recovery.
pub trait BulkTransport {
    /// Send `data` on the bulk-out endpoint.
    fn bulk_out(&self, data: &[u8]) -> Result<usize, BotError>;

    /// Receive up to `buf.len()` bytes from the bulk-in endpoint.
    fn bulk_in(&self, buf: &mut [u8]) -> Result<usize, BotError>;

    /// Issue a Bulk-Only Mass Storage Reset (class-specific control request).
    ///
    /// bmRequestType = 0x21 (class, interface, host-to-device)
    /// bRequest = 0xFF
    /// wValue = 0, wIndex = interface number, wLength = 0
    fn mass_storage_reset(&self) -> Result<(), BotError>;

    /// Clear a HALT condition on the given endpoint address.
    ///
    /// Uses CLEAR_FEATURE(ENDPOINT_HALT) standard request.
    fn clear_halt(&self, endpoint: u8) -> Result<(), BotError>;

    /// Get the bulk-in endpoint address.
    fn bulk_in_endpoint(&self) -> u8;

    /// Get the bulk-out endpoint address.
    fn bulk_out_endpoint(&self) -> u8;
}

/// Execute a full BOT command-transport-status cycle.
///
/// 1. Send the CBW on the bulk-out endpoint
/// 2. Transfer data (if any) on the appropriate bulk endpoint
/// 3. Receive the CSW on the bulk-in endpoint
/// 4. Validate the CSW
///
/// On phase error, performs reset recovery automatically.
pub fn execute_command(
    transport: &dyn BulkTransport,
    cbw: &CommandBlockWrapper,
    data_buf: Option<&mut [u8]>,
) -> Result<u32, BotError> {
    // Step 1: Send CBW
    let cbw_bytes = cbw.to_bytes();
    log::debug!("BOT: sending CBW, tag={:#010x}, xfer_len={}", cbw.tag, cbw.data_transfer_length);

    let sent = transport.bulk_out(&cbw_bytes)?;
    if sent != CBW_SIZE {
        log::error!("BOT: CBW short write: {} of {} bytes", sent, CBW_SIZE);
        return Err(BotError::CbwTransferFailed);
    }

    // Step 2: Data phase (if any)
    if cbw.data_transfer_length > 0 {
        if let Some(buf) = data_buf {
            if cbw.direction_in {
                log::trace!("BOT: data-in phase, expecting {} bytes", cbw.data_transfer_length);
                let mut offset = 0;
                while offset < buf.len() && offset < cbw.data_transfer_length as usize {
                    let n = transport.bulk_in(&mut buf[offset..])?;
                    if n == 0 {
                        log::warn!("BOT: bulk-in returned 0 bytes at offset {}", offset);
                        break;
                    }
                    offset += n;
                }
                log::trace!("BOT: data-in complete, got {} bytes", offset);
            } else {
                log::trace!("BOT: data-out phase, sending {} bytes", buf.len());
                let mut offset = 0;
                while offset < buf.len() {
                    let n = transport.bulk_out(&buf[offset..])?;
                    if n == 0 {
                        log::warn!("BOT: bulk-out returned 0 bytes at offset {}", offset);
                        break;
                    }
                    offset += n;
                }
                log::trace!("BOT: data-out complete, sent {} bytes", offset);
            }
        }
    }

    // Step 3: Receive CSW
    let mut csw_buf = [0u8; CSW_SIZE];
    let received = transport.bulk_in(&mut csw_buf)?;
    if received != CSW_SIZE {
        log::error!("BOT: CSW short read: {} of {} bytes", received, CSW_SIZE);
        return Err(BotError::CswTransferFailed);
    }

    let csw = CommandStatusWrapper::from_bytes(&csw_buf)?;

    // Step 4: Validate
    match csw.validate(cbw.tag) {
        Ok(()) => {
            log::debug!("BOT: command complete, residue={}", csw.data_residue);
            Ok(csw.data_residue)
        }
        Err(BotError::PhaseError) => {
            log::error!("BOT: phase error detected, performing reset recovery");
            reset_recovery(transport)?;
            Err(BotError::PhaseError)
        }
        Err(e) => Err(e),
    }
}

/// Execute a BOT command that sends data to the device (host-to-device).
///
/// This is a convenience wrapper around `execute_command` for write operations.
pub fn execute_command_out(
    transport: &dyn BulkTransport,
    cbw: &CommandBlockWrapper,
    data: &[u8],
) -> Result<u32, BotError> {
    // Step 1: Send CBW
    let cbw_bytes = cbw.to_bytes();
    log::debug!("BOT: sending CBW (out), tag={:#010x}, xfer_len={}", cbw.tag, cbw.data_transfer_length);

    let sent = transport.bulk_out(&cbw_bytes)?;
    if sent != CBW_SIZE {
        log::error!("BOT: CBW short write: {} of {} bytes", sent, CBW_SIZE);
        return Err(BotError::CbwTransferFailed);
    }

    // Step 2: Data-out phase
    if !data.is_empty() {
        log::trace!("BOT: data-out phase, sending {} bytes", data.len());
        let mut offset = 0;
        while offset < data.len() {
            let n = transport.bulk_out(&data[offset..])?;
            if n == 0 {
                log::warn!("BOT: bulk-out returned 0 bytes at offset {}", offset);
                break;
            }
            offset += n;
        }
        log::trace!("BOT: data-out complete, sent {} bytes", offset);
    }

    // Step 3: Receive CSW
    let mut csw_buf = [0u8; CSW_SIZE];
    let received = transport.bulk_in(&mut csw_buf)?;
    if received != CSW_SIZE {
        log::error!("BOT: CSW short read: {} of {} bytes", received, CSW_SIZE);
        return Err(BotError::CswTransferFailed);
    }

    let csw = CommandStatusWrapper::from_bytes(&csw_buf)?;

    match csw.validate(cbw.tag) {
        Ok(()) => {
            log::debug!("BOT: out-command complete, residue={}", csw.data_residue);
            Ok(csw.data_residue)
        }
        Err(BotError::PhaseError) => {
            log::error!("BOT: phase error on out-command, performing reset recovery");
            reset_recovery(transport)?;
            Err(BotError::PhaseError)
        }
        Err(e) => Err(e),
    }
}

/// Perform Bulk-Only Mass Storage Reset Recovery.
///
/// Per the BOT spec (section 5.3.4):
/// 1. Send Bulk-Only Mass Storage Reset (class request 0xFF)
/// 2. Clear HALT on the bulk-in endpoint
/// 3. Clear HALT on the bulk-out endpoint
pub fn reset_recovery(transport: &dyn BulkTransport) -> Result<(), BotError> {
    log::warn!("BOT: performing reset recovery sequence");

    log::debug!("BOT: step 1/3 — Bulk-Only Mass Storage Reset");
    transport.mass_storage_reset()?;

    log::debug!("BOT: step 2/3 — Clear HALT on bulk-in endpoint {:#04x}", transport.bulk_in_endpoint());
    transport.clear_halt(transport.bulk_in_endpoint())?;

    log::debug!("BOT: step 3/3 — Clear HALT on bulk-out endpoint {:#04x}", transport.bulk_out_endpoint());
    transport.clear_halt(transport.bulk_out_endpoint())?;

    log::info!("BOT: reset recovery complete");
    Ok(())
}
