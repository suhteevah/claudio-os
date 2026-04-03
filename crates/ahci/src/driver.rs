//! High-level AHCI driver API.
//!
//! Provides `AhciController` (discovers and manages all AHCI ports) and
//! `AhciDisk` (per-drive read/write interface that implements a BlockDevice-
//! compatible API matching the pattern used by ClaudioOS filesystem crates).

use alloc::alloc::{alloc_zeroed, Layout};
use alloc::vec::Vec;
use core::ptr;

use crate::command::{self, CommandHeader};
use crate::hba::{HbaRegs, *};
use crate::identify::IdentifyData;
use crate::port::{self, PortDeviceType};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during AHCI operations.
#[derive(Debug)]
pub enum AhciError {
    /// The device returned an I/O error (task file error, interface error, etc.).
    IoError,
    /// A command timed out waiting for completion.
    Timeout,
    /// No device is attached to the requested port.
    NoDevice,
    /// The requested LBA or sector count is out of range.
    OutOfRange,
    /// The buffer size does not match the requested transfer size.
    BufferSizeMismatch,
    /// The controller failed to initialize.
    InitFailed,
}

// ---------------------------------------------------------------------------
// AhciController
// ---------------------------------------------------------------------------

/// Maximum number of AHCI ports.
const MAX_PORTS: usize = 32;

/// Represents a detected SATA drive.
#[derive(Debug)]
pub struct AhciDisk {
    /// Which AHCI port this drive is on (0..31).
    pub port: u32,
    /// Parsed IDENTIFY data.
    pub identify: IdentifyData,
    /// Total sector count.
    pub sector_count: u64,
    /// Logical sector size in bytes (typically 512).
    pub sector_size: u32,
    /// Physical address of this port's Command List.
    cmd_list_addr: u64,
    /// Physical address of this port's FIS Receive area.
    _fis_addr: u64,
}

/// Top-level AHCI controller.
///
/// Manages the HBA, detects all attached drives, and provides access to them.
pub struct AhciController {
    /// Memory-mapped HBA registers.
    hba: HbaRegs,
    /// Bitmask of ports that are implemented (from PI register).
    pub ports_implemented: u32,
    /// Number of command slots per port.
    pub num_cmd_slots: u32,
    /// Detected drives (one per active ATA port).
    pub disks: Vec<AhciDisk>,
}

impl AhciController {
    /// Initialize the AHCI controller from the PCI BAR5 address.
    ///
    /// This performs the full initialization sequence:
    /// 1. Enable AHCI mode (GHC.AE)
    /// 2. Determine implemented ports
    /// 3. Initialize each implemented port
    /// 4. Detect and IDENTIFY attached drives
    ///
    /// # Safety
    ///
    /// `pci_bar5_addr` must be the physical address of the AHCI ABAR, and
    /// that region must be identity-mapped in the page tables.
    pub unsafe fn init(pci_bar5_addr: u64) -> Result<Self, AhciError> {
        log::info!("[ahci] initializing AHCI controller at ABAR={:#x}", pci_bar5_addr);

        // SAFETY: Caller guarantees the address is valid identity-mapped MMIO.
        let hba = unsafe { HbaRegs::from_base_addr(pci_bar5_addr) };

        // Log version and capabilities.
        let (ver_major, ver_minor) = hba.version();
        log::info!("[ahci] AHCI version {}.{}", ver_major, ver_minor);

        let cap = hba.read_cap();
        let num_ports = (cap & CAP_NP_MASK) + 1;
        let num_cmd_slots = ((cap & CAP_NCS_MASK) >> CAP_NCS_SHIFT) + 1;
        let s64a = cap & CAP_S64A != 0;
        log::info!(
            "[ahci] CAP: {} ports, {} command slots, 64-bit={}",
            num_ports, num_cmd_slots, s64a
        );

        // Enable AHCI mode (set GHC.AE). This may already be set.
        let ghc = hba.read_ghc();
        if ghc & GHC_AE == 0 {
            log::info!("[ahci] enabling AHCI mode (GHC.AE)");
            hba.write_ghc(ghc | GHC_AE);
        } else {
            log::debug!("[ahci] AHCI mode already enabled");
        }

        // Perform HBA reset to get a clean state.
        log::info!("[ahci] performing HBA reset (GHC.HR)");
        hba.write_ghc(hba.read_ghc() | GHC_HR);

        // Wait for HR to self-clear (spec says max 1 second).
        let mut timeout = 10_000_000u32;
        loop {
            if hba.read_ghc() & GHC_HR == 0 {
                log::info!("[ahci] HBA reset complete");
                break;
            }
            if timeout == 0 {
                log::error!("[ahci] HBA reset timeout — controller unresponsive");
                return Err(AhciError::InitFailed);
            }
            timeout -= 1;
            core::hint::spin_loop();
        }

        // Re-enable AHCI mode after reset (reset clears GHC.AE).
        hba.write_ghc(hba.read_ghc() | GHC_AE);
        log::debug!("[ahci] re-enabled AHCI mode after reset");

        // Read Ports Implemented.
        let pi = hba.read_pi();
        log::info!("[ahci] Ports Implemented (PI) = {:#010x}", pi);

        // Enable global interrupts.
        hba.write_ghc(hba.read_ghc() | GHC_IE);
        log::debug!("[ahci] global interrupts enabled");

        let mut controller = Self {
            hba,
            ports_implemented: pi,
            num_cmd_slots,
            disks: Vec::new(),
        };

        // Initialize each implemented port and detect drives.
        for port_num in 0..MAX_PORTS as u32 {
            if pi & (1 << port_num) == 0 {
                continue;
            }

            log::info!("[ahci] ---- port {} ----", port_num);

            // Detect device type before full init.
            let dev_type = port::detect_port_type(&controller.hba, port_num);
            if dev_type == PortDeviceType::None {
                log::debug!("[ahci] port {}: no device, skipping", port_num);
                continue;
            }

            if dev_type != PortDeviceType::Ata {
                log::info!(
                    "[ahci] port {}: device type {:?} not supported (ATA only), skipping",
                    port_num, dev_type
                );
                continue;
            }

            // Initialize the port (allocate DMA buffers, start engine).
            let Some((clb, fb)) = port::init_port(&controller.hba, port_num) else {
                log::warn!("[ahci] port {}: init_port failed", port_num);
                continue;
            };

            // Issue IDENTIFY DEVICE to learn about the drive.
            match controller.identify_drive(port_num, clb, fb) {
                Ok(disk) => {
                    log::info!(
                        "[ahci] port {}: drive detected: \"{}\" ({} sectors, {} bytes/sector, {} GiB)",
                        port_num,
                        disk.identify.model,
                        disk.sector_count,
                        disk.sector_size,
                        disk.identify.capacity_bytes() / (1024 * 1024 * 1024),
                    );
                    controller.disks.push(disk);
                }
                Err(e) => {
                    log::error!("[ahci] port {}: IDENTIFY failed: {:?}", port_num, e);
                }
            }
        }

        log::info!(
            "[ahci] initialization complete: {} drive(s) detected",
            controller.disks.len()
        );

        Ok(controller)
    }

    /// Issue an IDENTIFY DEVICE command to a port and parse the result.
    fn identify_drive(
        &self,
        port_num: u32,
        cmd_list_addr: u64,
        fis_addr: u64,
    ) -> Result<AhciDisk, AhciError> {
        log::debug!("[ahci] port {}: issuing IDENTIFY DEVICE", port_num);

        // Allocate a 512-byte buffer for IDENTIFY data (word-aligned).
        let id_layout = match Layout::from_size_align(512, 16) {
            Ok(l) => l,
            Err(_) => {
                log::error!("[ahci] port {}: invalid IDENTIFY layout", port_num);
                return Err(AhciError::InitFailed);
            }
        };
        let id_buf = unsafe { alloc_zeroed(id_layout) };
        if id_buf.is_null() {
            log::error!("[ahci] port {}: failed to allocate IDENTIFY buffer", port_num);
            return Err(AhciError::InitFailed);
        }
        let id_buf_addr = id_buf as u64;

        // Build the IDENTIFY command.
        let (cmd_hdr, _ct_addr) = command::build_identify(id_buf_addr);

        // Write the command header to slot 0 in the Command List.
        unsafe {
            ptr::copy_nonoverlapping(
                &cmd_hdr as *const CommandHeader as *const u8,
                cmd_list_addr as *mut u8,
                core::mem::size_of::<CommandHeader>(),
            );
        }

        // Issue the command by setting bit 0 in CI (Command Issue).
        self.issue_command_and_wait(port_num, 0)?;

        // Parse the IDENTIFY response.
        let id_bytes: &[u8; 512] = unsafe { &*(id_buf as *const [u8; 512]) };
        let identify = IdentifyData::parse(id_bytes);

        let sector_count = identify.total_sectors();
        let sector_size = identify.logical_sector_size;

        Ok(AhciDisk {
            port: port_num,
            identify,
            sector_count,
            sector_size,
            cmd_list_addr,
            _fis_addr: fis_addr,
        })
    }

    /// Issue a command in the given slot and poll until completion.
    ///
    /// Writes the CI bit and spins until the HBA clears it (command complete)
    /// or a task file error occurs.
    fn issue_command_and_wait(&self, port_num: u32, slot: u32) -> Result<(), AhciError> {
        log::trace!("[ahci] port {}: issuing command slot {}", port_num, slot);

        // Clear any pending interrupt status.
        self.hba.port_write_is(port_num, 0xFFFF_FFFF);

        // Issue the command.
        self.hba.port_write_ci(port_num, 1 << slot);

        // Poll for completion.
        let mut timeout = 50_000_000u32; // ~several seconds
        loop {
            let ci = self.hba.port_read_ci(port_num);
            if ci & (1 << slot) == 0 {
                // Command completed.
                log::trace!("[ahci] port {}: command slot {} completed", port_num, slot);
                break;
            }

            // Check for errors.
            let is = self.hba.port_read_is(port_num);
            if is & (PORT_IS_TFES | PORT_IS_HBFS | PORT_IS_HBDS | PORT_IS_IFS) != 0 {
                let tfd = self.hba.port_read_tfd(port_num);
                let serr = self.hba.port_read_serr(port_num);
                log::error!(
                    "[ahci] port {}: command error — IS={:#010x} TFD={:#010x} SERR={:#010x}",
                    port_num, is, tfd, serr
                );
                // Clear errors.
                self.hba.port_write_is(port_num, is);
                self.hba.port_write_serr(port_num, serr);
                return Err(AhciError::IoError);
            }

            if timeout == 0 {
                log::error!(
                    "[ahci] port {}: command timeout (CI={:#010x})",
                    port_num,
                    ci
                );
                return Err(AhciError::Timeout);
            }
            timeout -= 1;
            core::hint::spin_loop();
        }

        Ok(())
    }

    /// Get an immutable reference to the list of detected disks.
    pub fn disks(&self) -> &[AhciDisk] {
        &self.disks
    }

    /// Get a mutable reference to a disk by index.
    pub fn disk_mut(&mut self, index: usize) -> Option<&mut AhciDisk> {
        self.disks.get_mut(index)
    }
}

// ---------------------------------------------------------------------------
// AhciDisk: per-drive I/O
// ---------------------------------------------------------------------------

impl AhciDisk {
    /// Read `count` sectors starting at `lba` into `buf`.
    ///
    /// `buf` must be at least `count * sector_size` bytes. The buffer address
    /// is used directly for DMA, so it must be in identity-mapped physical memory
    /// (which is the case for all heap allocations in ClaudioOS).
    pub fn read_sectors(&self, hba: &HbaRegs, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), AhciError> {
        let expected_len = count as usize * self.sector_size as usize;
        if buf.len() < expected_len {
            log::error!(
                "[ahci] port {}: read_sectors buffer too small ({} < {})",
                self.port, buf.len(), expected_len
            );
            return Err(AhciError::BufferSizeMismatch);
        }

        if lba + count as u64 > self.sector_count {
            log::error!(
                "[ahci] port {}: read_sectors out of range: LBA {} + count {} > total {}",
                self.port, lba, count, self.sector_count
            );
            return Err(AhciError::OutOfRange);
        }

        log::debug!(
            "[ahci] port {}: read_sectors LBA={} count={}",
            self.port, lba, count
        );

        let buf_addr = buf.as_mut_ptr() as u64;

        // Build the READ DMA EXT command.
        let (cmd_hdr, _ct_addr) = command::build_read_dma_ext(lba, count, buf_addr, self.sector_size);

        // Write command header to slot 0.
        unsafe {
            ptr::copy_nonoverlapping(
                &cmd_hdr as *const CommandHeader as *const u8,
                self.cmd_list_addr as *mut u8,
                core::mem::size_of::<CommandHeader>(),
            );
        }

        // Issue and wait.
        // We construct a temporary HBA handle for command issuance.
        issue_and_wait(hba, self.port, 0)?;

        log::trace!(
            "[ahci] port {}: read_sectors complete (LBA={}, count={})",
            self.port, lba, count
        );
        Ok(())
    }

    /// Write `count` sectors starting at `lba` from `buf`.
    ///
    /// `buf` must be at least `count * sector_size` bytes.
    pub fn write_sectors(&self, hba: &HbaRegs, lba: u64, count: u16, buf: &[u8]) -> Result<(), AhciError> {
        let expected_len = count as usize * self.sector_size as usize;
        if buf.len() < expected_len {
            log::error!(
                "[ahci] port {}: write_sectors buffer too small ({} < {})",
                self.port, buf.len(), expected_len
            );
            return Err(AhciError::BufferSizeMismatch);
        }

        if lba + count as u64 > self.sector_count {
            log::error!(
                "[ahci] port {}: write_sectors out of range: LBA {} + count {} > total {}",
                self.port, lba, count, self.sector_count
            );
            return Err(AhciError::OutOfRange);
        }

        log::debug!(
            "[ahci] port {}: write_sectors LBA={} count={}",
            self.port, lba, count
        );

        let buf_addr = buf.as_ptr() as u64;

        // Build the WRITE DMA EXT command.
        let (cmd_hdr, _ct_addr) = command::build_write_dma_ext(lba, count, buf_addr, self.sector_size);

        // Write command header to slot 0.
        unsafe {
            ptr::copy_nonoverlapping(
                &cmd_hdr as *const CommandHeader as *const u8,
                self.cmd_list_addr as *mut u8,
                core::mem::size_of::<CommandHeader>(),
            );
        }

        issue_and_wait(hba, self.port, 0)?;

        log::trace!(
            "[ahci] port {}: write_sectors complete (LBA={}, count={})",
            self.port, lba, count
        );
        Ok(())
    }

    /// Flush the drive's volatile write cache.
    pub fn flush(&self, hba: &HbaRegs) -> Result<(), AhciError> {
        log::debug!("[ahci] port {}: flushing write cache", self.port);

        let (cmd_hdr, _ct_addr) = command::build_flush_cache();

        unsafe {
            ptr::copy_nonoverlapping(
                &cmd_hdr as *const CommandHeader as *const u8,
                self.cmd_list_addr as *mut u8,
                core::mem::size_of::<CommandHeader>(),
            );
        }

        issue_and_wait(hba, self.port, 0)?;

        log::debug!("[ahci] port {}: flush complete", self.port);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // BlockDevice-compatible interface
    // -----------------------------------------------------------------------

    /// Read `buf.len()` bytes from the device starting at byte `offset`.
    ///
    /// This matches the `BlockDevice` trait pattern used by `claudio-ext4`.
    /// Handles unaligned reads by reading full sectors and copying the
    /// relevant portion.
    pub fn read_bytes(&self, hba: &HbaRegs, offset: u64, buf: &mut [u8]) -> Result<(), AhciError> {
        if buf.is_empty() {
            return Ok(());
        }

        let ss = self.sector_size as u64;
        let start_lba = offset / ss;
        let end_byte = offset + buf.len() as u64;
        let end_lba = (end_byte + ss - 1) / ss;
        let total_sectors = end_lba - start_lba;

        log::trace!(
            "[ahci] port {}: read_bytes offset={} len={} -> LBA {}..{} ({} sectors)",
            self.port, offset, buf.len(), start_lba, end_lba, total_sectors
        );

        // Allocate a sector-aligned temporary buffer.
        let tmp_len = total_sectors as usize * self.sector_size as usize;
        let tmp_layout = match Layout::from_size_align(tmp_len, 16) {
            Ok(l) => l,
            Err(_) => {
                log::error!("[ahci] port {}: invalid read_bytes tmp layout", self.port);
                return Err(AhciError::IoError);
            }
        };
        let tmp_ptr = unsafe { alloc_zeroed(tmp_layout) };
        if tmp_ptr.is_null() {
            log::error!("[ahci] port {}: read_bytes alloc failed", self.port);
            return Err(AhciError::IoError);
        }

        let tmp_buf = unsafe { core::slice::from_raw_parts_mut(tmp_ptr, tmp_len) };

        // Read in chunks of up to 65535 sectors (max for 16-bit count, but
        // practically limited to ~128 for most controllers).
        let max_sectors_per_cmd: u64 = 128;
        let mut sectors_done: u64 = 0;
        while sectors_done < total_sectors {
            let remaining = total_sectors - sectors_done;
            let chunk = core::cmp::min(remaining, max_sectors_per_cmd) as u16;
            let chunk_offset = sectors_done as usize * self.sector_size as usize;
            self.read_sectors(
                hba,
                start_lba + sectors_done,
                chunk,
                &mut tmp_buf[chunk_offset..chunk_offset + chunk as usize * self.sector_size as usize],
            )?;
            sectors_done += chunk as u64;
        }

        // Copy the requested byte range from the temporary buffer.
        let byte_offset_in_tmp = (offset - start_lba * ss) as usize;
        buf.copy_from_slice(&tmp_buf[byte_offset_in_tmp..byte_offset_in_tmp + buf.len()]);

        // Free temporary buffer.
        unsafe { alloc::alloc::dealloc(tmp_ptr, tmp_layout) };

        Ok(())
    }

    /// Write `buf.len()` bytes to the device starting at byte `offset`.
    ///
    /// Handles unaligned writes via read-modify-write for partial sectors.
    pub fn write_bytes(&self, hba: &HbaRegs, offset: u64, buf: &[u8]) -> Result<(), AhciError> {
        if buf.is_empty() {
            return Ok(());
        }

        let ss = self.sector_size as u64;
        let start_lba = offset / ss;
        let end_byte = offset + buf.len() as u64;
        let end_lba = (end_byte + ss - 1) / ss;
        let total_sectors = end_lba - start_lba;

        log::trace!(
            "[ahci] port {}: write_bytes offset={} len={} -> LBA {}..{} ({} sectors)",
            self.port, offset, buf.len(), start_lba, end_lba, total_sectors
        );

        // Allocate a sector-aligned temporary buffer.
        let tmp_len = total_sectors as usize * self.sector_size as usize;
        let tmp_layout = match Layout::from_size_align(tmp_len, 16) {
            Ok(l) => l,
            Err(_) => {
                log::error!("[ahci] port {}: invalid write_bytes tmp layout", self.port);
                return Err(AhciError::IoError);
            }
        };
        let tmp_ptr = unsafe { alloc_zeroed(tmp_layout) };
        if tmp_ptr.is_null() {
            log::error!("[ahci] port {}: write_bytes alloc failed", self.port);
            return Err(AhciError::IoError);
        }

        let tmp_buf = unsafe { core::slice::from_raw_parts_mut(tmp_ptr, tmp_len) };

        let byte_offset_in_tmp = (offset - start_lba * ss) as usize;
        let aligned_start = byte_offset_in_tmp == 0;
        let aligned_end = (end_byte % ss) == 0;

        // If the write is not sector-aligned, read the first/last sectors first.
        if !aligned_start {
            log::trace!(
                "[ahci] port {}: read-modify-write for first partial sector",
                self.port
            );
            self.read_sectors(hba, start_lba, 1, &mut tmp_buf[..self.sector_size as usize])?;
        }
        if !aligned_end && total_sectors > 1 {
            let last_offset = (total_sectors - 1) as usize * self.sector_size as usize;
            log::trace!(
                "[ahci] port {}: read-modify-write for last partial sector",
                self.port
            );
            self.read_sectors(
                hba,
                end_lba - 1,
                1,
                &mut tmp_buf[last_offset..last_offset + self.sector_size as usize],
            )?;
        }

        // Copy user data into the temporary buffer.
        tmp_buf[byte_offset_in_tmp..byte_offset_in_tmp + buf.len()].copy_from_slice(buf);

        // Write all sectors.
        let max_sectors_per_cmd: u64 = 128;
        let mut sectors_done: u64 = 0;
        while sectors_done < total_sectors {
            let remaining = total_sectors - sectors_done;
            let chunk = core::cmp::min(remaining, max_sectors_per_cmd) as u16;
            let chunk_offset = sectors_done as usize * self.sector_size as usize;
            self.write_sectors(
                hba,
                start_lba + sectors_done,
                chunk,
                &tmp_buf[chunk_offset..chunk_offset + chunk as usize * self.sector_size as usize],
            )?;
            sectors_done += chunk as u64;
        }

        // Free temporary buffer.
        unsafe { alloc::alloc::dealloc(tmp_ptr, tmp_layout) };

        Ok(())
    }

    /// Flush cached writes to storage.
    ///
    /// Matches the `BlockDevice::flush` pattern.
    pub fn flush_bytes(&self, hba: &HbaRegs) -> Result<(), AhciError> {
        self.flush(hba)
    }
}

// ---------------------------------------------------------------------------
// Standalone command issue + poll (avoids borrow issues with &self + &hba)
// ---------------------------------------------------------------------------

/// Issue a command on a port and poll for completion.
fn issue_and_wait(hba: &HbaRegs, port: u32, slot: u32) -> Result<(), AhciError> {
    log::trace!("[ahci] port {}: issuing command slot {}", port, slot);

    // Clear pending interrupt status.
    hba.port_write_is(port, 0xFFFF_FFFF);

    // Issue the command.
    hba.port_write_ci(port, 1 << slot);

    // Poll for completion.
    let mut timeout = 50_000_000u32;
    loop {
        let ci = hba.port_read_ci(port);
        if ci & (1 << slot) == 0 {
            log::trace!("[ahci] port {}: slot {} completed", port, slot);
            return Ok(());
        }

        // Check for errors.
        let is = hba.port_read_is(port);
        if is & (PORT_IS_TFES | PORT_IS_HBFS | PORT_IS_HBDS | PORT_IS_IFS) != 0 {
            let tfd = hba.port_read_tfd(port);
            let serr = hba.port_read_serr(port);
            log::error!(
                "[ahci] port {}: command error — IS={:#010x} TFD={:#010x} SERR={:#010x}",
                port, is, tfd, serr
            );
            hba.port_write_is(port, is);
            hba.port_write_serr(port, serr);
            return Err(AhciError::IoError);
        }

        if timeout == 0 {
            log::error!("[ahci] port {}: command timeout (CI={:#010x})", port, ci);
            return Err(AhciError::Timeout);
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
}
