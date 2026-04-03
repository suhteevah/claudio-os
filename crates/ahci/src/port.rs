//! AHCI port initialization, type detection, and command engine management.
//!
//! Each AHCI port connects to one SATA device (drive, optical, port multiplier,
//! or enclosure bridge). This module handles bringing a port online, detecting
//! what is attached, and allocating the DMA structures the HBA needs.

use alloc::alloc::{alloc_zeroed, Layout};

use crate::hba::*;

/// The type of device attached to an AHCI port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDeviceType {
    /// SATA hard drive or SSD.
    Ata,
    /// SATAPI device (CD/DVD/Blu-ray).
    Atapi,
    /// Enclosure management bridge.
    Semb,
    /// Port multiplier.
    PortMultiplier,
    /// No device detected, or unknown.
    None,
}

/// Detect the type of device attached to a port by reading SStatus and signature.
///
/// A device is considered present only if the PHY reports DET=3 (device present
/// and communication established) and IPM=1 (interface active).
pub fn detect_port_type(hba: &HbaRegs, port: u32) -> PortDeviceType {
    let ssts = hba.port_read_ssts(port);
    let det = ssts & SSTS_DET_MASK;
    let ipm = ssts & SSTS_IPM_MASK;

    if det != SSTS_DET_PRESENT {
        log::trace!("[ahci] port {}: DET={:#x}, no device present", port, det);
        return PortDeviceType::None;
    }
    if ipm != SSTS_IPM_ACTIVE {
        log::trace!("[ahci] port {}: IPM={:#x}, interface not active", port, ipm);
        return PortDeviceType::None;
    }

    let sig = hba.port_read_sig(port);
    let dev_type = match sig {
        SATA_SIG_ATA => PortDeviceType::Ata,
        SATA_SIG_ATAPI => PortDeviceType::Atapi,
        SATA_SIG_SEMB => PortDeviceType::Semb,
        SATA_SIG_PM => PortDeviceType::PortMultiplier,
        _ => {
            log::warn!(
                "[ahci] port {}: unknown signature {:#010x}, treating as ATA",
                port, sig
            );
            // Many drives report a non-standard signature before IDENTIFY.
            // Treat as ATA and let IDENTIFY confirm.
            PortDeviceType::Ata
        }
    };

    log::info!(
        "[ahci] port {}: detected {:?} (SIG={:#010x}, SSTS={:#010x})",
        port, dev_type, sig, ssts
    );
    dev_type
}

/// Stop the command engine on a port.
///
/// Clears the ST (Start) bit, then waits for CR (Command List Running) to clear.
/// Also clears FRE and waits for FR to clear.
///
/// Returns `true` if the engine stopped within the timeout, `false` on timeout.
pub fn stop_cmd_engine(hba: &HbaRegs, port: u32) -> bool {
    log::debug!("[ahci] port {}: stopping command engine", port);

    let mut cmd = hba.port_read_cmd(port);

    // Clear ST (Start) first
    cmd &= !PORT_CMD_ST;
    hba.port_write_cmd(port, cmd);

    // Clear FRE (FIS Receive Enable)
    cmd &= !PORT_CMD_FRE;
    hba.port_write_cmd(port, cmd);

    // Wait for CR and FR to clear (up to 500ms, spec says 500ms max)
    let mut timeout = 500_000u32; // ~500ms at 1us per iteration (approximate)
    loop {
        let cmd = hba.port_read_cmd(port);
        if cmd & PORT_CMD_CR == 0 && cmd & PORT_CMD_FR == 0 {
            log::debug!("[ahci] port {}: command engine stopped", port);
            return true;
        }
        if timeout == 0 {
            log::error!(
                "[ahci] port {}: timeout waiting for command engine to stop (CMD={:#010x})",
                port,
                cmd
            );
            return false;
        }
        timeout -= 1;
        // Spin — bare-metal, no sleep primitive available at this layer.
        core::hint::spin_loop();
    }
}

/// Start the command engine on a port.
///
/// Enables FRE first (required), then sets ST. The HBA will begin processing
/// the command list.
pub fn start_cmd_engine(hba: &HbaRegs, port: u32) {
    log::debug!("[ahci] port {}: starting command engine", port);

    // Wait until CR is clear before starting
    wait_port_idle(hba, port);

    let mut cmd = hba.port_read_cmd(port);

    // Set FRE first — ST must not be set without FRE.
    cmd |= PORT_CMD_FRE;
    hba.port_write_cmd(port, cmd);

    // Now set ST to begin command processing.
    cmd |= PORT_CMD_ST;
    hba.port_write_cmd(port, cmd);

    log::debug!(
        "[ahci] port {}: command engine started (CMD={:#010x})",
        port,
        hba.port_read_cmd(port)
    );
}

/// Wait for the port to become idle (CR and FR both clear).
///
/// Spins with a generous timeout. Panics on timeout — if the HBA is
/// unresponsive, we cannot safely continue.
pub fn wait_port_idle(hba: &HbaRegs, port: u32) {
    let mut timeout = 1_000_000u32;
    loop {
        let cmd = hba.port_read_cmd(port);
        if cmd & (PORT_CMD_CR | PORT_CMD_FR) == 0 {
            return;
        }
        if timeout == 0 {
            log::error!(
                "[ahci] port {}: FATAL — port not idle after timeout (CMD={:#010x})",
                port,
                cmd
            );
            return;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
}

/// Allocate and configure the Command List and FIS Receive area for a port.
///
/// The Command List must be 1024-byte aligned and holds 32 command headers
/// (32 bytes each = 1024 bytes total).
///
/// The FIS Receive area must be 256-byte aligned and is 256 bytes.
///
/// Returns `(cmd_list_addr, fis_addr)` as physical addresses.
pub fn allocate_port_memory(hba: &HbaRegs, port: u32) -> Option<(u64, u64)> {
    log::debug!(
        "[ahci] port {}: allocating command list and FIS receive area",
        port
    );

    // Command list: 32 headers * 32 bytes = 1024 bytes, 1024-byte aligned.
    let cmd_list_layout = match Layout::from_size_align(1024, 1024) {
        Ok(l) => l,
        Err(_) => {
            log::error!("[ahci] port {}: invalid command list layout", port);
            return None;
        }
    };
    let cmd_list_ptr = unsafe { alloc_zeroed(cmd_list_layout) };
    if cmd_list_ptr.is_null() {
        log::error!("[ahci] port {}: failed to allocate command list", port);
        return None;
    }
    let cmd_list_addr = cmd_list_ptr as u64;

    // FIS receive area: 256 bytes, 256-byte aligned.
    let fis_layout = match Layout::from_size_align(256, 256) {
        Ok(l) => l,
        Err(_) => {
            log::error!("[ahci] port {}: invalid FIS layout", port);
            return None;
        }
    };
    let fis_ptr = unsafe { alloc_zeroed(fis_layout) };
    if fis_ptr.is_null() {
        log::error!("[ahci] port {}: failed to allocate FIS receive area", port);
        return None;
    }
    let fis_addr = fis_ptr as u64;

    // Program the port registers with these addresses.
    hba.port_write_clb(port, cmd_list_addr as u32);
    hba.port_write_clbu(port, (cmd_list_addr >> 32) as u32);
    hba.port_write_fb(port, fis_addr as u32);
    hba.port_write_fbu(port, (fis_addr >> 32) as u32);

    log::info!(
        "[ahci] port {}: CLB={:#x}, FB={:#x}",
        port, cmd_list_addr, fis_addr
    );

    Some((cmd_list_addr, fis_addr))
}

/// Perform a COMRESET on a port via the SControl register.
///
/// This resets the SATA PHY and re-establishes communication with the device.
/// After reset, the port signature is updated.
///
/// Returns `true` if a device was detected after reset.
pub fn port_reset(hba: &HbaRegs, port: u32) -> bool {
    log::info!("[ahci] port {}: initiating COMRESET", port);

    // Stop the command engine before reset.
    stop_cmd_engine(hba, port);

    // Set DET to 1 (COMRESET) in SControl.
    let mut sctl = hba.port_read_sctl(port);
    sctl = (sctl & !SCTL_DET_MASK) | SCTL_DET_COMRESET;
    hba.port_write_sctl(port, sctl);

    // Spec requires at least 1ms for COMRESET to be asserted.
    // Spin for approximately 2ms (very conservative).
    for _ in 0..200_000u32 {
        core::hint::spin_loop();
    }

    // Clear DET back to 0 to allow normal operation.
    sctl = hba.port_read_sctl(port);
    sctl &= !SCTL_DET_MASK;
    hba.port_write_sctl(port, sctl);

    // Wait for PHY communication to be re-established (DET = 3).
    let mut timeout = 1_000_000u32;
    loop {
        let ssts = hba.port_read_ssts(port);
        let det = ssts & SSTS_DET_MASK;
        if det == SSTS_DET_PRESENT {
            log::info!("[ahci] port {}: device detected after COMRESET", port);
            break;
        }
        if timeout == 0 {
            log::warn!(
                "[ahci] port {}: no device detected after COMRESET (SSTS={:#010x})",
                port, ssts
            );
            return false;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }

    // Clear SERR (all error bits) after reset.
    hba.port_write_serr(port, 0xFFFF_FFFF);

    // Clear port interrupt status.
    hba.port_write_is(port, 0xFFFF_FFFF);

    // Wait for BSY to clear in Task File Data — device is ready.
    let mut timeout = 1_000_000u32;
    loop {
        let tfd = hba.port_read_tfd(port);
        if tfd & (TFD_STS_BSY | TFD_STS_DRQ) == 0 {
            log::debug!("[ahci] port {}: device ready (TFD={:#010x})", port, tfd);
            break;
        }
        if timeout == 0 {
            log::warn!(
                "[ahci] port {}: device busy after reset (TFD={:#010x})",
                port, tfd
            );
            // Not fatal — some devices take a while. Caller can retry.
            break;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }

    true
}

/// Initialize a port: stop engine, allocate DMA memory, reset, start engine.
///
/// Returns `(cmd_list_addr, fis_addr)` if successful, or `None` if no device.
pub fn init_port(hba: &HbaRegs, port: u32) -> Option<(u64, u64)> {
    log::info!("[ahci] port {}: initializing", port);

    // Stop any existing command processing.
    stop_cmd_engine(hba, port);

    // Allocate DMA buffers.
    let (clb, fb) = allocate_port_memory(hba, port)?;

    // Clear pending interrupts and errors.
    hba.port_write_serr(port, 0xFFFF_FFFF);
    hba.port_write_is(port, 0xFFFF_FFFF);

    // Start the command engine.
    start_cmd_engine(hba, port);

    // Check if a device is actually present.
    let dev_type = detect_port_type(hba, port);
    if dev_type == PortDeviceType::None {
        log::info!("[ahci] port {}: no device, skipping", port);
        // Engine started but no device — harmless.
        return None;
    }

    log::info!(
        "[ahci] port {}: initialized successfully, device type = {:?}",
        port, dev_type
    );
    Some((clb, fb))
}
