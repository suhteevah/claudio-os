//! USB Mass Storage device driver.
//!
//! `UsbStorageDevice` wraps a BOT transport and provides sector-level I/O
//! plus a `claudio_vfs::BlockDevice` implementation for VFS integration.

use alloc::vec;

use spin::Mutex;

use claudio_vfs::device::{BlockDevice, DeviceError};

use crate::bot::{self, BotError, BulkTransport, CommandBlockWrapper};
use crate::scsi::{
    ScsiCommand, InquiryData, ReadCapacity10Data, SenseData, ModeSenseHeader,
};

// ── USB class/subclass/protocol constants ───────────────────────────────────

/// USB interface class: Mass Storage.
pub const USB_CLASS_MASS_STORAGE: u8 = 0x08;

/// USB interface subclass: SCSI Transparent Command Set.
pub const USB_SUBCLASS_SCSI: u8 = 0x06;

/// USB interface protocol: Bulk-Only Transport.
pub const USB_PROTOCOL_BOT: u8 = 0x50;

/// Maximum number of TEST UNIT READY retries during initialization.
const MAX_TUR_RETRIES: usize = 10;

/// Maximum sectors per single READ_10 / WRITE_10 (limited by u16 block count).
const MAX_SECTORS_PER_TRANSFER: u16 = 128;

// ── Detection ───────────────────────────────────────────────────────────────

/// USB interface descriptor fields relevant to mass storage detection.
#[derive(Debug, Clone, Copy)]
pub struct UsbInterfaceDescriptor {
    pub interface_class: u8,
    pub interface_subclass: u8,
    pub interface_protocol: u8,
}

/// Check if a USB interface descriptor matches the mass storage BOT profile.
pub fn is_mass_storage_bot(desc: &UsbInterfaceDescriptor) -> bool {
    let matched = desc.interface_class == USB_CLASS_MASS_STORAGE
        && desc.interface_subclass == USB_SUBCLASS_SCSI
        && desc.interface_protocol == USB_PROTOCOL_BOT;

    if matched {
        log::info!(
            "USB-STORAGE: detected Mass Storage device (class={:#04x} sub={:#04x} proto={:#04x})",
            desc.interface_class, desc.interface_subclass, desc.interface_protocol,
        );
    } else {
        log::trace!(
            "USB-STORAGE: interface not mass storage (class={:#04x} sub={:#04x} proto={:#04x})",
            desc.interface_class, desc.interface_subclass, desc.interface_protocol,
        );
    }

    matched
}

// ── USB Storage Device ──────────────────────────────────────────────────────

/// A USB Mass Storage device accessed via Bulk-Only Transport.
///
/// Wraps a `BulkTransport` implementation (provided by xHCI) and exposes
/// sector-level read/write plus the VFS `BlockDevice` trait.
pub struct UsbStorageDevice<T: BulkTransport> {
    /// The underlying BOT transport (bulk endpoints + control).
    transport: Mutex<T>,
    /// SCSI INQUIRY data (vendor, product, etc.).
    inquiry: Option<InquiryData>,
    /// Disk geometry from READ CAPACITY(10).
    capacity: Option<ReadCapacity10Data>,
    /// Whether the device is write-protected (from MODE SENSE).
    write_protected: bool,
    /// Logical Unit Number (typically 0 for single-LUN devices).
    lun: u8,
}

impl<T: BulkTransport> UsbStorageDevice<T> {
    /// Create a new USB storage device wrapper.
    ///
    /// Does NOT perform initialization — call `init()` after construction.
    pub fn new(transport: T, lun: u8) -> Self {
        log::info!("USB-STORAGE: created device wrapper, lun={}", lun);
        Self {
            transport: Mutex::new(transport),
            inquiry: None,
            capacity: None,
            write_protected: false,
            lun,
        }
    }

    /// Initialize the device: INQUIRY, TEST UNIT READY, READ CAPACITY, MODE SENSE.
    ///
    /// This must be called before any read/write operations.
    pub fn init(&mut self) -> Result<(), BotError> {
        log::info!("USB-STORAGE: initializing device (lun={})", self.lun);

        // Step 1: INQUIRY
        self.do_inquiry()?;

        // Step 2: TEST UNIT READY (with retries — device may need spin-up time)
        self.wait_unit_ready()?;

        // Step 3: READ CAPACITY(10) — get geometry
        self.do_read_capacity()?;

        // Step 4: MODE SENSE — check write-protect
        self.do_mode_sense()?;

        if let Some(ref cap) = self.capacity {
            let total_mib = cap.total_bytes() / (1024 * 1024);
            log::info!(
                "USB-STORAGE: device ready — {} MiB, block_size={}, write_protect={}",
                total_mib, cap.block_size, self.write_protected,
            );
        }

        Ok(())
    }

    /// Execute an INQUIRY command and store the result.
    fn do_inquiry(&mut self) -> Result<(), BotError> {
        let cmd = ScsiCommand::Inquiry { alloc_len: 36 };
        let (cdb, cdb_len) = cmd.to_cdb();

        let cbw = CommandBlockWrapper::new(
            &cdb[..cdb_len as usize],
            cmd.data_transfer_length(0),
            cmd.is_data_in(),
            self.lun,
        );

        let mut buf = [0u8; 36];
        let transport = self.transport.lock();
        bot::execute_command(&*transport, &cbw, Some(&mut buf))?;
        drop(transport);

        self.inquiry = InquiryData::from_bytes(&buf);
        if self.inquiry.is_none() {
            log::warn!("USB-STORAGE: INQUIRY parse failed, continuing anyway");
        }

        Ok(())
    }

    /// Poll TEST UNIT READY until the device is ready or we exhaust retries.
    fn wait_unit_ready(&self) -> Result<(), BotError> {
        for attempt in 0..MAX_TUR_RETRIES {
            log::debug!("USB-STORAGE: TEST UNIT READY attempt {}/{}", attempt + 1, MAX_TUR_RETRIES);

            let cmd = ScsiCommand::TestUnitReady;
            let (cdb, cdb_len) = cmd.to_cdb();
            let cbw = CommandBlockWrapper::new(
                &cdb[..cdb_len as usize],
                0,
                false,
                self.lun,
            );

            let transport = self.transport.lock();
            match bot::execute_command(&*transport, &cbw, None) {
                Ok(_) => {
                    log::info!("USB-STORAGE: device is ready");
                    return Ok(());
                }
                Err(BotError::CommandFailed) => {
                    drop(transport);
                    log::debug!("USB-STORAGE: not ready yet, requesting sense data");
                    // REQUEST SENSE to clear the check condition
                    let _ = self.do_request_sense();
                    // Spin-wait briefly (no timer available in no_std)
                    for _ in 0..100_000 {
                        core::hint::spin_loop();
                    }
                }
                Err(e) => return Err(e),
            }
        }

        log::error!("USB-STORAGE: device not ready after {} attempts", MAX_TUR_RETRIES);
        Err(BotError::Timeout)
    }

    /// Execute a REQUEST SENSE command and return the parsed sense data.
    fn do_request_sense(&self) -> Result<SenseData, BotError> {
        let cmd = ScsiCommand::RequestSense { alloc_len: 18 };
        let (cdb, cdb_len) = cmd.to_cdb();

        let cbw = CommandBlockWrapper::new(
            &cdb[..cdb_len as usize],
            cmd.data_transfer_length(0),
            cmd.is_data_in(),
            self.lun,
        );

        let mut buf = [0u8; 18];
        let transport = self.transport.lock();
        bot::execute_command(&*transport, &cbw, Some(&mut buf))?;
        drop(transport);

        SenseData::from_bytes(&buf).ok_or(BotError::CommandFailed)
    }

    /// Execute READ CAPACITY(10) and store the geometry.
    fn do_read_capacity(&mut self) -> Result<(), BotError> {
        let cmd = ScsiCommand::ReadCapacity10;
        let (cdb, cdb_len) = cmd.to_cdb();

        let cbw = CommandBlockWrapper::new(
            &cdb[..cdb_len as usize],
            cmd.data_transfer_length(0),
            cmd.is_data_in(),
            self.lun,
        );

        let mut buf = [0u8; 8];
        let transport = self.transport.lock();
        bot::execute_command(&*transport, &cbw, Some(&mut buf))?;
        drop(transport);

        self.capacity = ReadCapacity10Data::from_bytes(&buf);
        if self.capacity.is_none() {
            log::error!("USB-STORAGE: READ CAPACITY(10) parse failed");
            return Err(BotError::CommandFailed);
        }

        Ok(())
    }

    /// Execute MODE SENSE(6) to check write-protect status.
    fn do_mode_sense(&mut self) -> Result<(), BotError> {
        let cmd = ScsiCommand::ModeSense { page_code: 0x3F, alloc_len: 192 };
        let (cdb, cdb_len) = cmd.to_cdb();

        let cbw = CommandBlockWrapper::new(
            &cdb[..cdb_len as usize],
            cmd.data_transfer_length(0),
            cmd.is_data_in(),
            self.lun,
        );

        let mut buf = [0u8; 192];
        let transport = self.transport.lock();
        let result = bot::execute_command(&*transport, &cbw, Some(&mut buf));
        drop(transport);

        match result {
            Ok(_) => {
                if let Some(header) = ModeSenseHeader::from_bytes(&buf) {
                    self.write_protected = header.is_write_protected();
                }
            }
            Err(BotError::CommandFailed) => {
                // Some devices don't support MODE SENSE — that's fine
                log::warn!("USB-STORAGE: MODE SENSE not supported, assuming read-write");
                self.write_protected = false;
            }
            Err(e) => return Err(e),
        }

        Ok(())
    }

    /// Read `count` sectors starting at `lba` into `buf`.
    ///
    /// `buf` must be at least `count * block_size` bytes.
    pub fn read_sectors(&self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), BotError> {
        let block_size = self.block_size_or_default();
        let expected = count as usize * block_size as usize;

        if buf.len() < expected {
            log::error!(
                "USB-STORAGE: read_sectors buffer too small: {} < {} (lba={} count={})",
                buf.len(), expected, lba, count,
            );
            return Err(BotError::DataTransferFailed);
        }

        log::debug!("USB-STORAGE: read_sectors lba={} count={} ({} bytes)", lba, count, expected);

        // Split into chunks of MAX_SECTORS_PER_TRANSFER
        let mut remaining = count;
        let mut current_lba = lba;
        let mut offset = 0usize;

        while remaining > 0 {
            let chunk = remaining.min(MAX_SECTORS_PER_TRANSFER);
            let chunk_bytes = chunk as usize * block_size as usize;

            let cmd = ScsiCommand::Read10 { lba: current_lba, block_count: chunk };
            let (cdb, cdb_len) = cmd.to_cdb();

            let cbw = CommandBlockWrapper::new(
                &cdb[..cdb_len as usize],
                cmd.data_transfer_length(block_size),
                true,
                self.lun,
            );

            let transport = self.transport.lock();
            bot::execute_command(&*transport, &cbw, Some(&mut buf[offset..offset + chunk_bytes]))?;
            drop(transport);

            current_lba += chunk as u32;
            remaining -= chunk;
            offset += chunk_bytes;
        }

        log::debug!("USB-STORAGE: read_sectors complete, {} bytes transferred", offset);
        Ok(())
    }

    /// Write `count` sectors starting at `lba` from `data`.
    ///
    /// `data` must be at least `count * block_size` bytes.
    pub fn write_sectors(&self, lba: u32, count: u16, data: &[u8]) -> Result<(), BotError> {
        if self.write_protected {
            log::error!("USB-STORAGE: write refused — device is write-protected");
            return Err(BotError::CommandFailed);
        }

        let block_size = self.block_size_or_default();
        let expected = count as usize * block_size as usize;

        if data.len() < expected {
            log::error!(
                "USB-STORAGE: write_sectors data too small: {} < {} (lba={} count={})",
                data.len(), expected, lba, count,
            );
            return Err(BotError::DataTransferFailed);
        }

        log::debug!("USB-STORAGE: write_sectors lba={} count={} ({} bytes)", lba, count, expected);

        let mut remaining = count;
        let mut current_lba = lba;
        let mut offset = 0usize;

        while remaining > 0 {
            let chunk = remaining.min(MAX_SECTORS_PER_TRANSFER);
            let chunk_bytes = chunk as usize * block_size as usize;

            let cmd = ScsiCommand::Write10 { lba: current_lba, block_count: chunk };
            let (cdb, cdb_len) = cmd.to_cdb();

            let cbw = CommandBlockWrapper::new(
                &cdb[..cdb_len as usize],
                cmd.data_transfer_length(block_size),
                false,
                self.lun,
            );

            let transport = self.transport.lock();
            bot::execute_command_out(&*transport, &cbw, &data[offset..offset + chunk_bytes])?;
            drop(transport);

            current_lba += chunk as u32;
            remaining -= chunk;
            offset += chunk_bytes;
        }

        log::debug!("USB-STORAGE: write_sectors complete, {} bytes transferred", offset);
        Ok(())
    }

    /// Return the disk capacity info, if available.
    pub fn capacity(&self) -> Option<&ReadCapacity10Data> {
        self.capacity.as_ref()
    }

    /// Return the INQUIRY data, if available.
    pub fn inquiry(&self) -> Option<&InquiryData> {
        self.inquiry.as_ref()
    }

    /// Return whether the device is write-protected.
    pub fn is_write_protected(&self) -> bool {
        self.write_protected
    }

    /// Get block size, defaulting to 512 if capacity hasn't been read yet.
    fn block_size_or_default(&self) -> u32 {
        self.capacity.as_ref().map(|c| c.block_size).unwrap_or(512)
    }
}

// ── BlockDevice trait implementation ────────────────────────────────────────

impl<T: BulkTransport + Send + Sync> BlockDevice for UsbStorageDevice<T> {
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        let block_size = self.block_size_or_default();
        let bs = block_size as u64;

        // Calculate the starting LBA and any misalignment
        let start_lba = offset / bs;
        let start_offset = (offset % bs) as usize;

        // Calculate total sectors needed
        let end = offset + buf.len() as u64;
        let end_lba = (end + bs - 1) / bs; // round up
        let sector_count = (end_lba - start_lba) as u16;

        if start_lba > u32::MAX as u64 {
            log::error!("USB-STORAGE: LBA overflow: {}", start_lba);
            return Err(DeviceError::OutOfBounds);
        }

        log::trace!(
            "USB-STORAGE: BlockDevice::read_bytes offset={} len={} -> lba={} count={}",
            offset, buf.len(), start_lba, sector_count,
        );

        // If aligned and exact, read directly
        if start_offset == 0 && buf.len() == sector_count as usize * block_size as usize {
            self.read_sectors(start_lba as u32, sector_count, buf)
                .map_err(|e| {
                    log::error!("USB-STORAGE: read_bytes failed: {}", e);
                    DeviceError::IoError
                })?;
            return Ok(buf.len());
        }

        // Unaligned: read into a temporary buffer
        let tmp_size = sector_count as usize * block_size as usize;
        let mut tmp = vec![0u8; tmp_size];
        self.read_sectors(start_lba as u32, sector_count, &mut tmp)
            .map_err(|e| {
                log::error!("USB-STORAGE: read_bytes (unaligned) failed: {}", e);
                DeviceError::IoError
            })?;

        let copy_len = buf.len().min(tmp_size - start_offset);
        buf[..copy_len].copy_from_slice(&tmp[start_offset..start_offset + copy_len]);

        Ok(copy_len)
    }

    fn write_bytes(&self, offset: u64, data: &[u8]) -> Result<usize, DeviceError> {
        if self.write_protected {
            log::error!("USB-STORAGE: BlockDevice::write_bytes refused — write-protected");
            return Err(DeviceError::IoError);
        }

        let block_size = self.block_size_or_default();
        let bs = block_size as u64;

        let start_lba = offset / bs;
        let start_offset = (offset % bs) as usize;

        let end = offset + data.len() as u64;
        let end_lba = (end + bs - 1) / bs;
        let sector_count = (end_lba - start_lba) as u16;

        if start_lba > u32::MAX as u64 {
            log::error!("USB-STORAGE: LBA overflow: {}", start_lba);
            return Err(DeviceError::OutOfBounds);
        }

        log::trace!(
            "USB-STORAGE: BlockDevice::write_bytes offset={} len={} -> lba={} count={}",
            offset, data.len(), start_lba, sector_count,
        );

        // If aligned and exact, write directly
        if start_offset == 0 && data.len() == sector_count as usize * block_size as usize {
            self.write_sectors(start_lba as u32, sector_count, data)
                .map_err(|e| {
                    log::error!("USB-STORAGE: write_bytes failed: {}", e);
                    DeviceError::IoError
                })?;
            return Ok(data.len());
        }

        // Unaligned: read-modify-write
        let tmp_size = sector_count as usize * block_size as usize;
        let mut tmp = vec![0u8; tmp_size];

        // Read existing data
        self.read_sectors(start_lba as u32, sector_count, &mut tmp)
            .map_err(|e| {
                log::error!("USB-STORAGE: write_bytes read-modify-write read failed: {}", e);
                DeviceError::IoError
            })?;

        // Modify
        let copy_len = data.len().min(tmp_size - start_offset);
        tmp[start_offset..start_offset + copy_len].copy_from_slice(&data[..copy_len]);

        // Write back
        self.write_sectors(start_lba as u32, sector_count, &tmp)
            .map_err(|e| {
                log::error!("USB-STORAGE: write_bytes read-modify-write write failed: {}", e);
                DeviceError::IoError
            })?;

        Ok(copy_len)
    }

    fn flush(&self) -> Result<(), DeviceError> {
        // USB Mass Storage BOT has no explicit flush/sync command in the basic set.
        // SYNCHRONIZE_CACHE could be sent, but most USB devices handle this internally.
        log::trace!("USB-STORAGE: flush (no-op for BOT)");
        Ok(())
    }

    fn sector_size(&self) -> u32 {
        self.block_size_or_default()
    }

    fn total_size(&self) -> u64 {
        self.capacity
            .as_ref()
            .map(|c| c.total_bytes())
            .unwrap_or(0)
    }
}
