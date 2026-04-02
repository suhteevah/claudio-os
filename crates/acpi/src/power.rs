//! Power management operations using ACPI.
//!
//! Provides ACPI enable, sleep states (S1-S5), shutdown, reboot, and
//! PM timer microsecond delays.

use crate::fadt::{Fadt, GenericAddressStructure};
use crate::sdt::SdtHeader;
use crate::AcpiError;

/// PM timer frequency: 3.579545 MHz (defined by ACPI spec).
const PM_TIMER_FREQUENCY: u32 = 3_579_545;

/// SLP_EN bit in PM1 control register (bit 13).
const SLP_EN: u16 = 1 << 13;

/// Power manager that holds references to the FADT for register access.
#[derive(Debug)]
pub struct PowerManager {
    /// The parsed FADT.
    pub fadt: Fadt,
    /// SLP_TYPa for S5 (shutdown), if found.
    pub s5_slp_typ_a: Option<u8>,
    /// SLP_TYPb for S5 (shutdown), if found.
    pub s5_slp_typ_b: Option<u8>,
}

impl PowerManager {
    /// Create a new power manager from a parsed FADT.
    pub fn new(fadt: Fadt) -> Self {
        log::info!("power: initializing power manager");
        log::debug!(
            "power: SMI_CMD={:#X} ACPI_ENABLE={:#X} ACPI_DISABLE={:#X}",
            fadt.smi_command_port,
            fadt.acpi_enable,
            fadt.acpi_disable,
        );
        log::debug!(
            "power: PM1a_CNT={:#X} PM1b_CNT={:#X} PM_TMR={:#X}",
            fadt.pm1a_control_block,
            fadt.pm1b_control_block,
            fadt.pm_timer_block,
        );

        PowerManager {
            fadt,
            s5_slp_typ_a: None,
            s5_slp_typ_b: None,
        }
    }

    /// Enable ACPI mode by writing ACPI_ENABLE to the SMI command port.
    ///
    /// # Safety
    ///
    /// Performs I/O port writes that affect hardware state.
    pub unsafe fn enable_acpi(&self) -> Result<(), AcpiError> {
        let smi_cmd = self.fadt.smi_command_port;
        let acpi_enable = self.fadt.acpi_enable;

        if smi_cmd == 0 {
            log::info!("power: SMI_CMD is 0, ACPI may already be enabled (hardware-reduced)");
            return Ok(());
        }

        if acpi_enable == 0 {
            log::info!("power: ACPI_ENABLE is 0, ACPI is already in ACPI mode");
            return Ok(());
        }

        log::info!(
            "power: enabling ACPI mode: writing {:#X} to SMI_CMD port {:#X}",
            acpi_enable,
            smi_cmd,
        );

        unsafe {
            outb(smi_cmd as u16, acpi_enable);
        }

        // Poll PM1a control register to verify ACPI mode is enabled
        // SCI_EN is bit 0 of PM1_CNT
        let pm1a_cnt = self.fadt.pm1a_cnt_port();
        log::debug!("power: polling PM1a_CNT ({:#X}) for SCI_EN bit", pm1a_cnt);

        for attempt in 0..1000 {
            let val = unsafe { inw(pm1a_cnt) };
            if val & 1 != 0 {
                log::info!("power: ACPI mode enabled (SCI_EN set after {} polls)", attempt + 1);
                return Ok(());
            }
        }

        log::warn!("power: SCI_EN not set after 1000 polls, ACPI enable may have failed");
        Ok(())
    }

    /// Disable ACPI mode by writing ACPI_DISABLE to the SMI command port.
    ///
    /// # Safety
    ///
    /// Performs I/O port writes that affect hardware state.
    pub unsafe fn disable_acpi(&self) -> Result<(), AcpiError> {
        let smi_cmd = self.fadt.smi_command_port;
        let acpi_disable = self.fadt.acpi_disable;

        if smi_cmd == 0 || acpi_disable == 0 {
            log::warn!("power: cannot disable ACPI (SMI_CMD={:#X}, ACPI_DISABLE={:#X})",
                smi_cmd, acpi_disable);
            return Ok(());
        }

        log::info!(
            "power: disabling ACPI mode: writing {:#X} to SMI_CMD port {:#X}",
            acpi_disable,
            smi_cmd,
        );

        unsafe {
            outb(smi_cmd as u16, acpi_disable);
        }

        Ok(())
    }

    /// Parse the DSDT to find the \_S5 object for shutdown sleep type values.
    ///
    /// The \_S5 object in AML typically looks like:
    /// `08 5F 53 35 5F 12 04 01 0A xx` where xx is SLP_TYPa.
    ///
    /// # Safety
    ///
    /// The DSDT physical address must be mapped and readable.
    pub unsafe fn parse_s5_from_dsdt(&mut self, dsdt_addr: u64) -> Result<(), AcpiError> {
        log::info!("power: searching for \\_S5 object in DSDT at {:#X}", dsdt_addr);

        let header = unsafe { SdtHeader::from_address(dsdt_addr)? };

        if &header.signature != b"DSDT" {
            log::error!(
                "power: expected 'DSDT' signature, got '{}'",
                header.signature_str()
            );
            return Err(AcpiError::SignatureMismatch);
        }

        let table_len = header.length as usize;
        let data = unsafe {
            core::slice::from_raw_parts(dsdt_addr as *const u8, table_len)
        };

        // Search for "_S5_" in the AML bytecode
        // The pattern is: NameOp(0x08) '_' 'S' '5' '_' PackageOp(0x12) PkgLength NumElements BytePrefix(0x0A) Value
        let s5_name = b"_S5_";

        for i in 0..data.len().saturating_sub(10) {
            if &data[i..i + 4] == s5_name {
                log::debug!("power: found '_S5_' name at DSDT offset {}", i);

                // Look for the package data after the name
                // Skip to the package contents
                let mut j = i + 4;

                // Check for PackageOp (0x12)
                if j < data.len() && data[j] == 0x12 {
                    j += 1;
                    // Skip PkgLength (1-4 bytes, simplified: handle 1-byte length)
                    if j < data.len() {
                        let pkg_lead = data[j];
                        if pkg_lead & 0xC0 == 0 {
                            // 1-byte PkgLength
                            j += 1;
                        } else {
                            // Multi-byte PkgLength
                            let extra_bytes = (pkg_lead >> 6) as usize;
                            j += 1 + extra_bytes;
                        }
                    }

                    // Skip NumElements
                    if j < data.len() {
                        j += 1;
                    }

                    // Read SLP_TYPa value
                    if j < data.len() {
                        let slp_typ_a = if data[j] == 0x0A {
                            // BytePrefix
                            j += 1;
                            if j < data.len() { data[j] } else { 0 }
                        } else {
                            data[j]
                        };

                        j += 1;

                        // Read SLP_TYPb value (may or may not be present)
                        let slp_typ_b = if j < data.len() {
                            if data[j] == 0x0A {
                                j += 1;
                                if j < data.len() { data[j] } else { slp_typ_a }
                            } else {
                                data[j]
                            }
                        } else {
                            slp_typ_a
                        };

                        log::info!(
                            "power: S5 sleep types found: SLP_TYPa={} SLP_TYPb={}",
                            slp_typ_a,
                            slp_typ_b,
                        );

                        self.s5_slp_typ_a = Some(slp_typ_a);
                        self.s5_slp_typ_b = Some(slp_typ_b);
                        return Ok(());
                    }
                }
            }
        }

        log::error!("power: \\_S5 object not found in DSDT ({} bytes searched)", table_len);
        Err(AcpiError::S5NotFound)
    }

    /// Perform ACPI shutdown (enter S5 sleep state).
    ///
    /// This writes SLP_TYPa | SLP_EN to PM1a_CNT (and PM1b_CNT if present).
    /// The machine should power off after this call.
    ///
    /// # Safety
    ///
    /// This will power off the machine. Ensure all state is saved.
    pub unsafe fn shutdown(&self) -> Result<core::convert::Infallible, AcpiError> {
        let slp_typ_a = self.s5_slp_typ_a.ok_or_else(|| {
            log::error!("power: cannot shutdown, S5 SLP_TYPa not found (call parse_s5_from_dsdt first)");
            AcpiError::S5NotFound
        })?;

        let slp_typ_b = self.s5_slp_typ_b.unwrap_or(slp_typ_a);

        let pm1a_cnt = self.fadt.pm1a_cnt_port();
        let pm1b_cnt = self.fadt.pm1b_cnt_port();

        // SLP_TYP goes in bits 10-12 of PM1_CNT, SLP_EN is bit 13
        let val_a: u16 = ((slp_typ_a as u16) << 10) | SLP_EN;
        let val_b: u16 = ((slp_typ_b as u16) << 10) | SLP_EN;

        log::info!("power: SHUTDOWN: writing PM1a_CNT={:#X} <- {:#X}", pm1a_cnt, val_a);

        unsafe {
            outw(pm1a_cnt, val_a);
        }

        if pm1b_cnt != 0 {
            log::info!("power: SHUTDOWN: writing PM1b_CNT={:#X} <- {:#X}", pm1b_cnt, val_b);
            unsafe {
                outw(pm1b_cnt, val_b);
            }
        }

        log::error!("power: SHUTDOWN FAILED — machine is still running!");

        // If we get here, shutdown failed. Halt.
        loop {
            unsafe { core::arch::asm!("hlt"); }
        }
    }

    /// Perform ACPI reboot via the reset register.
    ///
    /// # Safety
    ///
    /// This will reboot the machine.
    pub unsafe fn reboot(&self) -> Result<core::convert::Infallible, AcpiError> {
        if !self.fadt.reset_supported() {
            log::error!("power: reset register not supported (FADT flags bit 10 not set)");
            return Err(AcpiError::IoError);
        }

        let reg = &self.fadt.reset_register;
        let value = self.fadt.reset_value;

        if !reg.is_valid() {
            log::error!("power: reset register address is invalid");
            return Err(AcpiError::InvalidPointer);
        }

        let reg_addr = reg.address;
        log::info!(
            "power: REBOOT: space={} addr={:#X} value={:#X}",
            reg.address_space,
            reg_addr,
            value,
        );

        unsafe {
            write_gas(reg, value);
        }

        log::error!("power: REBOOT FAILED — machine is still running!");

        loop {
            unsafe { core::arch::asm!("hlt"); }
        }
    }

    /// Read the PM timer value.
    ///
    /// The PM timer ticks at 3.579545 MHz. It may be 24-bit or 32-bit
    /// depending on the FADT flags.
    ///
    /// # Safety
    ///
    /// Performs I/O port reads.
    pub unsafe fn pm_timer_read(&self) -> u32 {
        let port = self.fadt.pm_timer_port();
        let val = unsafe { inl(port) };

        if !self.fadt.pm_timer_is_32bit() {
            // Mask to 24 bits
            val & 0x00FF_FFFF
        } else {
            val
        }
    }

    /// Spin-wait for a number of microseconds using the PM timer.
    ///
    /// # Safety
    ///
    /// Performs I/O port reads.
    pub unsafe fn pm_timer_delay_us(&self, microseconds: u32) {
        let ticks_needed = (microseconds as u64 * PM_TIMER_FREQUENCY as u64) / 1_000_000;
        let mask: u32 = if self.fadt.pm_timer_is_32bit() {
            0xFFFF_FFFF
        } else {
            0x00FF_FFFF
        };

        log::trace!(
            "power: PM timer delay: {}us = {} ticks (mask={:#X})",
            microseconds,
            ticks_needed,
            mask,
        );

        let start = unsafe { self.pm_timer_read() };
        let mut elapsed: u64 = 0;
        let mut last = start;

        while elapsed < ticks_needed {
            let current = unsafe { self.pm_timer_read() };
            let delta = if current >= last {
                current - last
            } else {
                // Timer wrapped
                (mask - last) + current + 1
            };
            elapsed += delta as u64;
            last = current;
        }

        log::trace!("power: PM timer delay complete ({} ticks elapsed)", elapsed);
    }

    /// Enter a sleep state (S1-S4). For S5 (shutdown), use `shutdown()` instead.
    ///
    /// # Safety
    ///
    /// Enters a hardware sleep state. The system may lose state depending on the
    /// sleep level.
    pub unsafe fn enter_sleep_state(
        &self,
        slp_typ_a: u8,
        slp_typ_b: u8,
    ) -> Result<(), AcpiError> {
        let pm1a_cnt = self.fadt.pm1a_cnt_port();
        let pm1b_cnt = self.fadt.pm1b_cnt_port();

        let val_a: u16 = ((slp_typ_a as u16) << 10) | SLP_EN;
        let val_b: u16 = ((slp_typ_b as u16) << 10) | SLP_EN;

        log::info!(
            "power: entering sleep state: SLP_TYPa={} SLP_TYPb={} PM1a_CNT={:#X}",
            slp_typ_a,
            slp_typ_b,
            pm1a_cnt,
        );

        unsafe {
            outw(pm1a_cnt, val_a);
        }

        if pm1b_cnt != 0 {
            log::debug!("power: writing PM1b_CNT={:#X} <- {:#X}", pm1b_cnt, val_b);
            unsafe {
                outw(pm1b_cnt, val_b);
            }
        }

        log::info!("power: sleep state entered, woke up successfully");
        Ok(())
    }
}

// --- Low-level I/O port access ---

/// Write a byte to an I/O port.
///
/// # Safety
///
/// Direct hardware I/O.
#[inline]
unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Write a word to an I/O port.
///
/// # Safety
///
/// Direct hardware I/O.
#[inline]
unsafe fn outw(port: u16, value: u16) {
    unsafe {
        core::arch::asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Read a word from an I/O port.
///
/// # Safety
///
/// Direct hardware I/O.
#[inline]
unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!(
            "in ax, dx",
            in("dx") port,
            out("ax") value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

/// Read a dword from an I/O port.
///
/// # Safety
///
/// Direct hardware I/O.
#[inline]
unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!(
            "in eax, dx",
            in("dx") port,
            out("eax") value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

/// Write a value to a Generic Address Structure location.
///
/// # Safety
///
/// Performs I/O port writes or MMIO writes depending on the GAS address space.
unsafe fn write_gas(gas: &GenericAddressStructure, value: u8) {
    match gas.address_space {
        0 => {
            // System Memory
            let gas_addr = gas.address;
            log::trace!("power: MMIO write {:#X} <- {:#X}", gas_addr, value);
            unsafe {
                core::ptr::write_volatile(gas.address as *mut u8, value);
            }
        }
        1 => {
            // System I/O
            let gas_addr = gas.address;
            log::trace!("power: I/O write port {:#X} <- {:#X}", gas_addr, value);
            unsafe {
                outb(gas.address as u16, value);
            }
        }
        2 => {
            // PCI Configuration Space
            log::warn!("power: PCI config space GAS write not implemented");
        }
        _ => {
            log::error!(
                "power: unsupported GAS address space {} for write",
                gas.address_space,
            );
        }
    }
}
