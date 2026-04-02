//! FADT (Fixed ACPI Description Table) parsing.
//!
//! The FADT (signature "FACP") contains fixed hardware register addresses and
//! values needed for power management, sleep states, and system control.

use crate::sdt::{SdtHeader, SDT_HEADER_SIZE};
use crate::AcpiError;

/// Generic Address Structure (GAS) — 12 bytes.
/// Used by ACPI 2.0+ to describe register locations.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct GenericAddressStructure {
    /// Address space ID (0=System Memory, 1=System I/O, 2=PCI Config, etc.).
    pub address_space: u8,
    /// Register bit width.
    pub bit_width: u8,
    /// Register bit offset.
    pub bit_offset: u8,
    /// Access size (0=undefined, 1=byte, 2=word, 3=dword, 4=qword).
    pub access_size: u8,
    /// 64-bit address.
    pub address: u64,
}

impl GenericAddressStructure {
    /// Returns true if this GAS is a System I/O address.
    pub fn is_io(&self) -> bool {
        self.address_space == 1
    }

    /// Returns true if this GAS is a System Memory address.
    pub fn is_mmio(&self) -> bool {
        self.address_space == 0
    }

    /// Returns true if the address is valid (non-zero).
    pub fn is_valid(&self) -> bool {
        self.address != 0
    }
}

/// Parsed FADT (Fixed ACPI Description Table).
///
/// Contains the full FADT fields up to ACPI 6.x. Fields beyond the table's
/// actual length are zeroed.
#[derive(Debug, Clone, Copy)]
pub struct Fadt {
    /// Physical address of the FADT.
    pub address: u64,
    /// SDT header.
    pub header: SdtHeader,

    // --- ACPI 1.0 fields ---

    /// Physical address of the FACS (Firmware ACPI Control Structure).
    pub firmware_ctrl: u32,
    /// Physical address of the DSDT.
    pub dsdt: u32,
    /// Reserved in ACPI 2.0+ (was INT_MODEL in ACPI 1.0).
    pub reserved_int_model: u8,
    /// Preferred power management profile (0=Unspecified, 1=Desktop, 2=Mobile, etc.).
    pub preferred_pm_profile: u8,
    /// SCI interrupt number (IRQ for ACPI events).
    pub sci_interrupt: u16,
    /// SMI command port (I/O port for SMI generation).
    pub smi_command_port: u32,
    /// Value to write to SMI_CMD to enable ACPI.
    pub acpi_enable: u8,
    /// Value to write to SMI_CMD to disable ACPI.
    pub acpi_disable: u8,
    /// Value to write to SMI_CMD to enter S4BIOS state.
    pub s4bios_req: u8,
    /// Value to write to SMI_CMD for P-state control.
    pub pstate_control: u8,
    /// PM1a event block I/O port.
    pub pm1a_event_block: u32,
    /// PM1b event block I/O port (0 if not supported).
    pub pm1b_event_block: u32,
    /// PM1a control block I/O port.
    pub pm1a_control_block: u32,
    /// PM1b control block I/O port (0 if not supported).
    pub pm1b_control_block: u32,
    /// PM2 control block I/O port.
    pub pm2_control_block: u32,
    /// PM timer block I/O port.
    pub pm_timer_block: u32,
    /// GPE0 block I/O port.
    pub gpe0_block: u32,
    /// GPE1 block I/O port.
    pub gpe1_block: u32,
    /// PM1 event register length (bytes).
    pub pm1_event_length: u8,
    /// PM1 control register length (bytes).
    pub pm1_control_length: u8,
    /// PM2 control register length (bytes).
    pub pm2_control_length: u8,
    /// PM timer register length (bytes).
    pub pm_timer_length: u8,
    /// GPE0 block length (bytes).
    pub gpe0_length: u8,
    /// GPE1 block length (bytes).
    pub gpe1_length: u8,
    /// GPE1 base offset.
    pub gpe1_base: u8,
    /// C-state control support.
    pub cstate_control: u8,
    /// Worst-case C2 latency (microseconds).
    pub worst_c2_latency: u16,
    /// Worst-case C3 latency (microseconds).
    pub worst_c3_latency: u16,
    /// Flush size for WBINVD.
    pub flush_size: u16,
    /// Flush stride for WBINVD.
    pub flush_stride: u16,
    /// Duty cycle offset in P_CNT register.
    pub duty_offset: u8,
    /// Duty cycle width in bits.
    pub duty_width: u8,
    /// RTC CMOS day alarm index.
    pub day_alarm: u8,
    /// RTC CMOS month alarm index.
    pub month_alarm: u8,
    /// RTC CMOS century index.
    pub century: u8,
    /// IA-PC boot architecture flags (ACPI 2.0+).
    pub ia_pc_boot_arch: u16,
    /// Reserved.
    pub reserved2: u8,
    /// Fixed feature flags.
    pub flags: u32,

    // --- ACPI 2.0+ fields ---

    /// Reset register (GAS).
    pub reset_register: GenericAddressStructure,
    /// Value to write to reset register to perform reset.
    pub reset_value: u8,
    /// ARM boot architecture flags (ACPI 5.1+).
    pub arm_boot_arch: u16,
    /// FADT minor version.
    pub fadt_minor_version: u8,

    /// 64-bit FACS address (overrides firmware_ctrl if non-zero).
    pub x_firmware_ctrl: u64,
    /// 64-bit DSDT address (overrides dsdt if non-zero).
    pub x_dsdt: u64,

    /// Extended PM1a event block (GAS).
    pub x_pm1a_event_block: GenericAddressStructure,
    /// Extended PM1b event block (GAS).
    pub x_pm1b_event_block: GenericAddressStructure,
    /// Extended PM1a control block (GAS).
    pub x_pm1a_control_block: GenericAddressStructure,
    /// Extended PM1b control block (GAS).
    pub x_pm1b_control_block: GenericAddressStructure,
    /// Extended PM2 control block (GAS).
    pub x_pm2_control_block: GenericAddressStructure,
    /// Extended PM timer block (GAS).
    pub x_pm_timer_block: GenericAddressStructure,
    /// Extended GPE0 block (GAS).
    pub x_gpe0_block: GenericAddressStructure,
    /// Extended GPE1 block (GAS).
    pub x_gpe1_block: GenericAddressStructure,
}

/// Helper to read a GAS from a byte buffer at the given offset.
fn read_gas(buf: &[u8], offset: usize) -> GenericAddressStructure {
    if offset + 12 > buf.len() {
        return GenericAddressStructure {
            address_space: 0,
            bit_width: 0,
            bit_offset: 0,
            access_size: 0,
            address: 0,
        };
    }
    GenericAddressStructure {
        address_space: buf[offset],
        bit_width: buf[offset + 1],
        bit_offset: buf[offset + 2],
        access_size: buf[offset + 3],
        address: u64::from_le_bytes([
            buf[offset + 4],
            buf[offset + 5],
            buf[offset + 6],
            buf[offset + 7],
            buf[offset + 8],
            buf[offset + 9],
            buf[offset + 10],
            buf[offset + 11],
        ]),
    }
}

/// Helper to read a u8 from the byte buffer, returning 0 if out of bounds.
fn read_u8(buf: &[u8], offset: usize) -> u8 {
    buf.get(offset).copied().unwrap_or(0)
}

/// Helper to read a u16 LE from the byte buffer, returning 0 if out of bounds.
fn read_u16(buf: &[u8], offset: usize) -> u16 {
    if offset + 2 > buf.len() {
        return 0;
    }
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

/// Helper to read a u32 LE from the byte buffer, returning 0 if out of bounds.
fn read_u32(buf: &[u8], offset: usize) -> u32 {
    if offset + 4 > buf.len() {
        return 0;
    }
    u32::from_le_bytes([buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3]])
}

/// Helper to read a u64 LE from the byte buffer, returning 0 if out of bounds.
fn read_u64(buf: &[u8], offset: usize) -> u64 {
    if offset + 8 > buf.len() {
        return 0;
    }
    u64::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
        buf[offset + 4],
        buf[offset + 5],
        buf[offset + 6],
        buf[offset + 7],
    ])
}

impl Fadt {
    /// Parse the FADT from its physical address.
    ///
    /// # Safety
    ///
    /// `phys_addr` must point to a valid, mapped FADT (signature "FACP").
    pub unsafe fn from_address(phys_addr: u64) -> Result<Self, AcpiError> {
        log::info!("fadt: parsing at {:#X}", phys_addr);

        let header = unsafe { SdtHeader::from_address(phys_addr)? };

        if &header.signature != b"FACP" {
            log::error!(
                "fadt: expected 'FACP' signature, got '{}'",
                header.signature_str()
            );
            return Err(AcpiError::SignatureMismatch);
        }

        unsafe { header.validate_checksum(phys_addr)?; }

        let table_len = header.length as usize;
        let body_len = table_len.saturating_sub(SDT_HEADER_SIZE);
        let body_base = phys_addr + SDT_HEADER_SIZE as u64;

        // Read the entire table body into a buffer for safe field access
        let buf = unsafe {
            core::slice::from_raw_parts(body_base as *const u8, body_len)
        };

        // ACPI 1.0 fields (offsets relative to body start, i.e., after SDT header)
        let firmware_ctrl = read_u32(buf, 0);
        let dsdt = read_u32(buf, 4);
        let reserved_int_model = read_u8(buf, 8);
        let preferred_pm_profile = read_u8(buf, 9);
        let sci_interrupt = read_u16(buf, 10);
        let smi_command_port = read_u32(buf, 12);
        let acpi_enable = read_u8(buf, 16);
        let acpi_disable = read_u8(buf, 17);
        let s4bios_req = read_u8(buf, 18);
        let pstate_control = read_u8(buf, 19);
        let pm1a_event_block = read_u32(buf, 20);
        let pm1b_event_block = read_u32(buf, 24);
        let pm1a_control_block = read_u32(buf, 28);
        let pm1b_control_block = read_u32(buf, 32);
        let pm2_control_block = read_u32(buf, 36);
        let pm_timer_block = read_u32(buf, 40);
        let gpe0_block = read_u32(buf, 44);
        let gpe1_block = read_u32(buf, 48);
        let pm1_event_length = read_u8(buf, 52);
        let pm1_control_length = read_u8(buf, 53);
        let pm2_control_length = read_u8(buf, 54);
        let pm_timer_length = read_u8(buf, 55);
        let gpe0_length = read_u8(buf, 56);
        let gpe1_length = read_u8(buf, 57);
        let gpe1_base = read_u8(buf, 58);
        let cstate_control = read_u8(buf, 59);
        let worst_c2_latency = read_u16(buf, 60);
        let worst_c3_latency = read_u16(buf, 62);
        let flush_size = read_u16(buf, 64);
        let flush_stride = read_u16(buf, 66);
        let duty_offset = read_u8(buf, 68);
        let duty_width = read_u8(buf, 69);
        let day_alarm = read_u8(buf, 70);
        let month_alarm = read_u8(buf, 71);
        let century = read_u8(buf, 72);
        let ia_pc_boot_arch = read_u16(buf, 73);
        let reserved2 = read_u8(buf, 75);
        let flags = read_u32(buf, 76);

        // ACPI 2.0+ fields
        let reset_register = read_gas(buf, 80);
        let reset_value = read_u8(buf, 92);
        let arm_boot_arch = read_u16(buf, 93);
        let fadt_minor_version = read_u8(buf, 95);

        let x_firmware_ctrl = read_u64(buf, 96);
        let x_dsdt = read_u64(buf, 104);

        let x_pm1a_event_block = read_gas(buf, 112);
        let x_pm1b_event_block = read_gas(buf, 124);
        let x_pm1a_control_block = read_gas(buf, 136);
        let x_pm1b_control_block = read_gas(buf, 148);
        let x_pm2_control_block = read_gas(buf, 160);
        let x_pm_timer_block = read_gas(buf, 172);
        let x_gpe0_block = read_gas(buf, 184);
        let x_gpe1_block = read_gas(buf, 196);

        log::info!(
            "fadt: SCI_INT={} SMI_CMD={:#X} ACPI_ENABLE={:#X} ACPI_DISABLE={:#X}",
            sci_interrupt,
            smi_command_port,
            acpi_enable,
            acpi_disable,
        );
        log::info!(
            "fadt: PM1a_EVT={:#X} PM1a_CNT={:#X} PM_TMR={:#X}",
            pm1a_event_block,
            pm1a_control_block,
            pm_timer_block,
        );
        log::info!(
            "fadt: DSDT={:#X} X_DSDT={:#X} flags={:#X}",
            dsdt,
            x_dsdt,
            flags,
        );
        let reset_reg_addr = reset_register.address;
        log::debug!(
            "fadt: PM profile={} century_idx={} reset_reg={:#X} reset_val={:#X}",
            preferred_pm_profile,
            century,
            reset_reg_addr,
            reset_value,
        );

        if reset_register.is_valid() {
            let reset_reg_addr2 = reset_register.address;
            log::info!(
                "fadt: reset register: space={} addr={:#X} value={:#X}",
                reset_register.address_space,
                reset_reg_addr2,
                reset_value,
            );
        }

        Ok(Fadt {
            address: phys_addr,
            header,
            firmware_ctrl,
            dsdt,
            reserved_int_model,
            preferred_pm_profile,
            sci_interrupt,
            smi_command_port,
            acpi_enable,
            acpi_disable,
            s4bios_req,
            pstate_control,
            pm1a_event_block,
            pm1b_event_block,
            pm1a_control_block,
            pm1b_control_block,
            pm2_control_block,
            pm_timer_block,
            gpe0_block,
            gpe1_block,
            pm1_event_length,
            pm1_control_length,
            pm2_control_length,
            pm_timer_length,
            gpe0_length,
            gpe1_length,
            gpe1_base,
            cstate_control,
            worst_c2_latency,
            worst_c3_latency,
            flush_size,
            flush_stride,
            duty_offset,
            duty_width,
            day_alarm,
            month_alarm,
            century,
            ia_pc_boot_arch,
            reserved2,
            flags,
            reset_register,
            reset_value,
            arm_boot_arch,
            fadt_minor_version,
            x_firmware_ctrl,
            x_dsdt,
            x_pm1a_event_block,
            x_pm1b_event_block,
            x_pm1a_control_block,
            x_pm1b_control_block,
            x_pm2_control_block,
            x_pm_timer_block,
            x_gpe0_block,
            x_gpe1_block,
        })
    }

    /// Get the DSDT physical address, preferring X_DSDT (64-bit) over DSDT (32-bit).
    pub fn dsdt_address(&self) -> Option<u64> {
        if self.x_dsdt != 0 {
            Some(self.x_dsdt)
        } else if self.dsdt != 0 {
            Some(self.dsdt as u64)
        } else {
            None
        }
    }

    /// Get the PM1a control block I/O port, preferring the extended version.
    pub fn pm1a_cnt_port(&self) -> u16 {
        if self.x_pm1a_control_block.is_valid() && self.x_pm1a_control_block.is_io() {
            self.x_pm1a_control_block.address as u16
        } else {
            self.pm1a_control_block as u16
        }
    }

    /// Get the PM1b control block I/O port (may be 0 if not present).
    pub fn pm1b_cnt_port(&self) -> u16 {
        if self.x_pm1b_control_block.is_valid() && self.x_pm1b_control_block.is_io() {
            self.x_pm1b_control_block.address as u16
        } else {
            self.pm1b_control_block as u16
        }
    }

    /// Get the PM timer I/O port.
    pub fn pm_timer_port(&self) -> u16 {
        if self.x_pm_timer_block.is_valid() && self.x_pm_timer_block.is_io() {
            self.x_pm_timer_block.address as u16
        } else {
            self.pm_timer_block as u16
        }
    }

    /// Check if the PM timer is 32-bit (bit 8 of flags).
    pub fn pm_timer_is_32bit(&self) -> bool {
        self.flags & (1 << 8) != 0
    }

    /// Check if the reset register is supported (bit 10 of flags).
    pub fn reset_supported(&self) -> bool {
        self.flags & (1 << 10) != 0
    }
}
