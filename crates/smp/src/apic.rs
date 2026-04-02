//! Local APIC driver for ClaudioOS.
//!
//! Implements memory-mapped xAPIC and MSR-based x2APIC access per
//! Intel SDM Volume 3, Chapter 10 (APIC).

use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// IA32_APIC_BASE MSR (0x1B)
// ---------------------------------------------------------------------------

/// MSR address for the APIC base register.
const IA32_APIC_BASE_MSR: u32 = 0x1B;

/// Bit 8: BSP flag — set if this is the bootstrap processor.
const APIC_BASE_BSP: u64 = 1 << 8;
/// Bit 10: x2APIC enable.
const APIC_BASE_X2APIC_ENABLE: u64 = 1 << 10;
/// Bit 11: APIC global enable.
const APIC_BASE_ENABLE: u64 = 1 << 11;
/// Bits 12..=35: physical base address of APIC registers (4 KiB aligned).
const APIC_BASE_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

// ---------------------------------------------------------------------------
// Local APIC register offsets (memory-mapped, relative to base)
// Intel SDM Vol 3, Table 10-1
// ---------------------------------------------------------------------------

/// Local APIC ID register.
const REG_ID: u32 = 0x020;
/// Local APIC version register.
const REG_VERSION: u32 = 0x030;
/// Task Priority Register.
const REG_TPR: u32 = 0x080;
/// Arbitration Priority Register (read-only).
const REG_APR: u32 = 0x090;
/// Processor Priority Register (read-only).
const REG_PPR: u32 = 0x0A0;
/// End-Of-Interrupt register (write-only).
const REG_EOI: u32 = 0x0B0;
/// Remote Read Register.
const REG_RRD: u32 = 0x0C0;
/// Logical Destination Register.
const REG_LDR: u32 = 0x0D0;
/// Destination Format Register.
const REG_DFR: u32 = 0x0E0;
/// Spurious Interrupt Vector Register.
const REG_SVR: u32 = 0x0F0;

/// In-Service Register (8 x 32-bit, offsets 0x100..0x170).
const REG_ISR_BASE: u32 = 0x100;
/// Trigger Mode Register (8 x 32-bit, offsets 0x180..0x1F0).
const REG_TMR_BASE: u32 = 0x180;
/// Interrupt Request Register (8 x 32-bit, offsets 0x200..0x270).
const REG_IRR_BASE: u32 = 0x200;

/// Error Status Register.
const REG_ERROR_STATUS: u32 = 0x280;
/// LVT Corrected Machine Check Interrupt.
const REG_LVT_CMCI: u32 = 0x2F0;
/// Interrupt Command Register (low 32 bits).
const REG_ICR_LOW: u32 = 0x300;
/// Interrupt Command Register (high 32 bits — destination).
const REG_ICR_HIGH: u32 = 0x310;
/// LVT Timer register.
const REG_LVT_TIMER: u32 = 0x320;
/// LVT Thermal Sensor register.
const REG_LVT_THERMAL: u32 = 0x330;
/// LVT Performance Monitoring Counters register.
const REG_LVT_PERF: u32 = 0x340;
/// LVT LINT0 register.
const REG_LVT_LINT0: u32 = 0x350;
/// LVT LINT1 register.
const REG_LVT_LINT1: u32 = 0x360;
/// LVT Error register.
const REG_LVT_ERROR: u32 = 0x370;
/// Timer Initial Count register.
const REG_TIMER_INITIAL: u32 = 0x380;
/// Timer Current Count register (read-only).
const REG_TIMER_CURRENT: u32 = 0x390;
/// Timer Divide Configuration register.
const REG_TIMER_DIVIDE: u32 = 0x3E0;

// ---------------------------------------------------------------------------
// ICR delivery modes / fields  (Intel SDM Vol 3, 10.6.1)
// ---------------------------------------------------------------------------

/// ICR delivery mode: Fixed.
pub const ICR_FIXED: u32 = 0b000 << 8;
/// ICR delivery mode: Lowest Priority.
pub const ICR_LOWEST_PRIORITY: u32 = 0b001 << 8;
/// ICR delivery mode: SMI.
pub const ICR_SMI: u32 = 0b010 << 8;
/// ICR delivery mode: NMI.
pub const ICR_NMI: u32 = 0b100 << 8;
/// ICR delivery mode: INIT.
pub const ICR_INIT: u32 = 0b101 << 8;
/// ICR delivery mode: Startup (SIPI).
pub const ICR_STARTUP: u32 = 0b110 << 8;

/// ICR destination mode: Physical.
pub const ICR_DEST_PHYSICAL: u32 = 0 << 11;
/// ICR destination mode: Logical.
pub const ICR_DEST_LOGICAL: u32 = 1 << 11;

/// ICR level: De-assert.
pub const ICR_LEVEL_DEASSERT: u32 = 0 << 14;
/// ICR level: Assert.
pub const ICR_LEVEL_ASSERT: u32 = 1 << 14;

/// ICR trigger mode: Edge.
pub const ICR_TRIGGER_EDGE: u32 = 0 << 15;
/// ICR trigger mode: Level.
pub const ICR_TRIGGER_LEVEL: u32 = 1 << 15;

/// ICR destination shorthand: No shorthand.
pub const ICR_NO_SHORTHAND: u32 = 0b00 << 18;
/// ICR destination shorthand: Self.
pub const ICR_SELF: u32 = 0b01 << 18;
/// ICR destination shorthand: All including self.
pub const ICR_ALL_INCLUDING_SELF: u32 = 0b10 << 18;
/// ICR destination shorthand: All excluding self.
pub const ICR_ALL_EXCLUDING_SELF: u32 = 0b11 << 18;

// ---------------------------------------------------------------------------
// LVT Timer modes
// ---------------------------------------------------------------------------

/// LVT Timer mode: One-shot.
pub const TIMER_ONESHOT: u32 = 0b00 << 17;
/// LVT Timer mode: Periodic.
pub const TIMER_PERIODIC: u32 = 0b01 << 17;
/// LVT Timer mode: TSC-Deadline.
pub const TIMER_TSC_DEADLINE: u32 = 0b10 << 17;

/// LVT mask bit.
pub const LVT_MASKED: u32 = 1 << 16;

// ---------------------------------------------------------------------------
// Timer divide values (encoded for TIMER_DIVIDE register)
// ---------------------------------------------------------------------------

/// Divide by 1.
pub const TIMER_DIV_1: u32 = 0b1011;
/// Divide by 2.
pub const TIMER_DIV_2: u32 = 0b0000;
/// Divide by 4.
pub const TIMER_DIV_4: u32 = 0b0001;
/// Divide by 8.
pub const TIMER_DIV_8: u32 = 0b0010;
/// Divide by 16.
pub const TIMER_DIV_16: u32 = 0b0011;
/// Divide by 32.
pub const TIMER_DIV_32: u32 = 0b1000;
/// Divide by 64.
pub const TIMER_DIV_64: u32 = 0b1001;
/// Divide by 128.
pub const TIMER_DIV_128: u32 = 0b1010;

// ---------------------------------------------------------------------------
// x2APIC MSR offsets (base 0x800)
// ---------------------------------------------------------------------------

const X2APIC_MSR_BASE: u32 = 0x800;

// ---------------------------------------------------------------------------
// LocalApic driver
// ---------------------------------------------------------------------------

/// Whether x2APIC mode is active globally.
static X2APIC_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Local APIC driver.
///
/// Supports both xAPIC (memory-mapped) and x2APIC (MSR-based) access modes.
pub struct LocalApic {
    /// Base virtual address of the memory-mapped APIC registers (xAPIC mode).
    base_addr: u64,
    /// Whether this instance uses x2APIC MSR access.
    x2apic: bool,
}

impl LocalApic {
    /// Create a new Local APIC driver instance.
    ///
    /// `base_vaddr` is the virtual address where the APIC MMIO page is mapped
    /// (typically identity-mapped at 0xFEE0_0000 in ClaudioOS).
    pub fn new(base_vaddr: u64) -> Self {
        log::info!("apic: creating LocalApic driver at base {:#X}", base_vaddr);
        Self {
            base_addr: base_vaddr,
            x2apic: false,
        }
    }

    // -----------------------------------------------------------------------
    // MSR helpers (unsafe inline asm)
    // -----------------------------------------------------------------------

    #[inline]
    unsafe fn rdmsr(msr: u32) -> u64 {
        let (low, high): (u32, u32);
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
        ((high as u64) << 32) | (low as u64)
    }

    #[inline]
    unsafe fn wrmsr(msr: u32, value: u64) {
        let low = value as u32;
        let high = (value >> 32) as u32;
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }

    // -----------------------------------------------------------------------
    // APIC base MSR (0x1B) access
    // -----------------------------------------------------------------------

    /// Read the IA32_APIC_BASE MSR.
    pub fn read_apic_base_msr(&self) -> u64 {
        let val = unsafe { Self::rdmsr(IA32_APIC_BASE_MSR) };
        log::trace!("apic: read IA32_APIC_BASE = {:#018X}", val);
        val
    }

    /// Write the IA32_APIC_BASE MSR.
    ///
    /// # Safety
    /// Caller must ensure the value is valid (correct base address, enable bits).
    pub unsafe fn write_apic_base_msr(&self, value: u64) {
        log::debug!("apic: write IA32_APIC_BASE = {:#018X}", value);
        Self::wrmsr(IA32_APIC_BASE_MSR, value);
    }

    /// Return the physical base address from the APIC_BASE MSR.
    pub fn physical_base(&self) -> u64 {
        self.read_apic_base_msr() & APIC_BASE_ADDR_MASK
    }

    /// Check if this processor is the BSP according to the APIC_BASE MSR.
    pub fn is_bsp(&self) -> bool {
        (self.read_apic_base_msr() & APIC_BASE_BSP) != 0
    }

    // -----------------------------------------------------------------------
    // Register read / write (xAPIC vs x2APIC)
    // -----------------------------------------------------------------------

    /// Read a 32-bit local APIC register.
    #[inline]
    fn read_reg(&self, offset: u32) -> u32 {
        if self.x2apic {
            // x2APIC: register offset / 16 + MSR base
            let msr = X2APIC_MSR_BASE + (offset >> 4);
            unsafe { Self::rdmsr(msr) as u32 }
        } else {
            unsafe { ptr::read_volatile((self.base_addr + offset as u64) as *const u32) }
        }
    }

    /// Write a 32-bit local APIC register.
    #[inline]
    fn write_reg(&self, offset: u32, value: u32) {
        if self.x2apic {
            let msr = X2APIC_MSR_BASE + (offset >> 4);
            unsafe { Self::wrmsr(msr, value as u64) };
        } else {
            unsafe {
                ptr::write_volatile((self.base_addr + offset as u64) as *mut u32, value);
            }
        }
    }

    // -----------------------------------------------------------------------
    // x2APIC detection and activation
    // -----------------------------------------------------------------------

    /// Detect whether the CPU supports x2APIC (CPUID.01H:ECX[21]).
    pub fn supports_x2apic() -> bool {
        let ecx: u32;
        unsafe {
            core::arch::asm!(
                "push rbx",
                "cpuid",
                "pop rbx",
                inout("eax") 1u32 => _,
                out("ecx") ecx,
                out("edx") _,
                options(nomem, preserves_flags),
            );
        }
        let supported = (ecx & (1 << 21)) != 0;
        log::info!("apic: x2APIC support detected = {}", supported);
        supported
    }

    /// Enable x2APIC mode (sets bit 10 of IA32_APIC_BASE).
    ///
    /// # Safety
    /// Must be called before any other APIC register access if switching modes.
    /// The xAPIC global enable (bit 11) must already be set.
    pub unsafe fn enable_x2apic(&mut self) {
        log::info!("apic: enabling x2APIC mode");
        let mut base = Self::rdmsr(IA32_APIC_BASE_MSR);
        base |= APIC_BASE_ENABLE | APIC_BASE_X2APIC_ENABLE;
        Self::wrmsr(IA32_APIC_BASE_MSR, base);
        self.x2apic = true;
        X2APIC_ACTIVE.store(true, Ordering::Release);
        log::info!("apic: x2APIC mode enabled, IA32_APIC_BASE = {:#018X}", base);
    }

    // -----------------------------------------------------------------------
    // Core APIC operations
    // -----------------------------------------------------------------------

    /// Enable the local APIC by setting bit 8 (APIC Software Enable) of the
    /// Spurious Interrupt Vector Register, with spurious vector 0xFF.
    pub fn enable(&self) {
        log::info!("apic: enabling local APIC (SVR bit 8)");
        // Set spurious vector to 0xFF and enable bit
        let svr = self.read_reg(REG_SVR);
        let new_svr = (svr & !0xFF) | 0xFF | (1 << 8);
        self.write_reg(REG_SVR, new_svr);
        log::info!("apic: SVR = {:#010X} -> {:#010X}", svr, new_svr);
    }

    /// Disable the local APIC by clearing bit 8 of SVR.
    pub fn disable(&self) {
        log::warn!("apic: disabling local APIC (SVR bit 8 clear)");
        let svr = self.read_reg(REG_SVR);
        self.write_reg(REG_SVR, svr & !(1u32 << 8));
    }

    /// Read the local APIC ID.
    pub fn id(&self) -> u32 {
        let raw = self.read_reg(REG_ID);
        // xAPIC: ID is in bits 24..31; x2APIC: full 32-bit
        let id = if self.x2apic { raw } else { raw >> 24 };
        log::trace!("apic: local APIC ID = {}", id);
        id
    }

    /// Read the local APIC version register.
    pub fn version(&self) -> u32 {
        let ver = self.read_reg(REG_VERSION);
        log::trace!(
            "apic: version = {:#010X} (version={}, max_lvt={})",
            ver,
            ver & 0xFF,
            ((ver >> 16) & 0xFF) + 1
        );
        ver
    }

    /// Write to the End-Of-Interrupt register (signals interrupt completion).
    pub fn eoi(&self) {
        log::trace!("apic: sending EOI");
        self.write_reg(REG_EOI, 0);
    }

    /// Read the Task Priority Register.
    pub fn read_tpr(&self) -> u32 {
        self.read_reg(REG_TPR)
    }

    /// Write the Task Priority Register.
    pub fn write_tpr(&self, value: u32) {
        log::trace!("apic: TPR = {:#010X}", value);
        self.write_reg(REG_TPR, value);
    }

    /// Read the Arbitration Priority Register.
    pub fn read_apr(&self) -> u32 {
        self.read_reg(REG_APR)
    }

    /// Read the Processor Priority Register.
    pub fn read_ppr(&self) -> u32 {
        self.read_reg(REG_PPR)
    }

    /// Read the Remote Read Register.
    pub fn read_rrd(&self) -> u32 {
        self.read_reg(REG_RRD)
    }

    /// Read the Logical Destination Register.
    pub fn read_ldr(&self) -> u32 {
        self.read_reg(REG_LDR)
    }

    /// Write the Logical Destination Register.
    pub fn write_ldr(&self, value: u32) {
        log::trace!("apic: LDR = {:#010X}", value);
        self.write_reg(REG_LDR, value);
    }

    /// Read the Destination Format Register.
    pub fn read_dfr(&self) -> u32 {
        self.read_reg(REG_DFR)
    }

    /// Write the Destination Format Register.
    pub fn write_dfr(&self, value: u32) {
        log::trace!("apic: DFR = {:#010X}", value);
        self.write_reg(REG_DFR, value);
    }

    /// Read the Spurious Interrupt Vector Register.
    pub fn read_svr(&self) -> u32 {
        self.read_reg(REG_SVR)
    }

    /// Read an In-Service Register word (index 0..7).
    pub fn read_isr(&self, index: u8) -> u32 {
        assert!(index < 8, "ISR index must be 0..7");
        self.read_reg(REG_ISR_BASE + (index as u32) * 0x10)
    }

    /// Read a Trigger Mode Register word (index 0..7).
    pub fn read_tmr(&self, index: u8) -> u32 {
        assert!(index < 8, "TMR index must be 0..7");
        self.read_reg(REG_TMR_BASE + (index as u32) * 0x10)
    }

    /// Read an Interrupt Request Register word (index 0..7).
    pub fn read_irr(&self, index: u8) -> u32 {
        assert!(index < 8, "IRR index must be 0..7");
        self.read_reg(REG_IRR_BASE + (index as u32) * 0x10)
    }

    /// Read the Error Status Register.
    pub fn read_error_status(&self) -> u32 {
        // Must write before read to latch errors (Intel SDM 10.5.3)
        self.write_reg(REG_ERROR_STATUS, 0);
        self.read_reg(REG_ERROR_STATUS)
    }

    // -----------------------------------------------------------------------
    // LVT register access
    // -----------------------------------------------------------------------

    /// Read LVT CMCI register.
    pub fn read_lvt_cmci(&self) -> u32 {
        self.read_reg(REG_LVT_CMCI)
    }

    /// Write LVT CMCI register.
    pub fn write_lvt_cmci(&self, value: u32) {
        log::trace!("apic: LVT_CMCI = {:#010X}", value);
        self.write_reg(REG_LVT_CMCI, value);
    }

    /// Read LVT Timer register.
    pub fn read_lvt_timer(&self) -> u32 {
        self.read_reg(REG_LVT_TIMER)
    }

    /// Write LVT Timer register.
    pub fn write_lvt_timer(&self, value: u32) {
        log::trace!("apic: LVT_TIMER = {:#010X}", value);
        self.write_reg(REG_LVT_TIMER, value);
    }

    /// Read LVT Thermal Sensor register.
    pub fn read_lvt_thermal(&self) -> u32 {
        self.read_reg(REG_LVT_THERMAL)
    }

    /// Write LVT Thermal Sensor register.
    pub fn write_lvt_thermal(&self, value: u32) {
        log::trace!("apic: LVT_THERMAL = {:#010X}", value);
        self.write_reg(REG_LVT_THERMAL, value);
    }

    /// Read LVT Performance Monitoring register.
    pub fn read_lvt_perf(&self) -> u32 {
        self.read_reg(REG_LVT_PERF)
    }

    /// Write LVT Performance Monitoring register.
    pub fn write_lvt_perf(&self, value: u32) {
        log::trace!("apic: LVT_PERF = {:#010X}", value);
        self.write_reg(REG_LVT_PERF, value);
    }

    /// Read LVT LINT0 register.
    pub fn read_lvt_lint0(&self) -> u32 {
        self.read_reg(REG_LVT_LINT0)
    }

    /// Write LVT LINT0 register.
    pub fn write_lvt_lint0(&self, value: u32) {
        log::trace!("apic: LVT_LINT0 = {:#010X}", value);
        self.write_reg(REG_LVT_LINT0, value);
    }

    /// Read LVT LINT1 register.
    pub fn read_lvt_lint1(&self) -> u32 {
        self.read_reg(REG_LVT_LINT1)
    }

    /// Write LVT LINT1 register.
    pub fn write_lvt_lint1(&self, value: u32) {
        log::trace!("apic: LVT_LINT1 = {:#010X}", value);
        self.write_reg(REG_LVT_LINT1, value);
    }

    /// Read LVT Error register.
    pub fn read_lvt_error(&self) -> u32 {
        self.read_reg(REG_LVT_ERROR)
    }

    /// Write LVT Error register.
    pub fn write_lvt_error(&self, value: u32) {
        log::trace!("apic: LVT_ERROR = {:#010X}", value);
        self.write_reg(REG_LVT_ERROR, value);
    }

    // -----------------------------------------------------------------------
    // IPI (Inter-Processor Interrupt) via ICR
    // -----------------------------------------------------------------------

    /// Wait until the ICR delivery status bit (bit 12) clears, indicating
    /// the previous IPI was accepted.
    fn wait_icr_idle(&self) {
        if self.x2apic {
            // x2APIC: ICR write is self-synchronising, no polling needed.
            return;
        }
        log::trace!("apic: waiting for ICR idle");
        loop {
            let icr_low = self.read_reg(REG_ICR_LOW);
            if (icr_low & (1 << 12)) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
    }

    /// Send an IPI (Inter-Processor Interrupt).
    ///
    /// `dest_apic_id` — target APIC ID (ignored if using a shorthand).
    /// `icr_low_flags` — combination of delivery mode, level, trigger, shorthand, vector.
    pub fn send_ipi(&self, dest_apic_id: u32, icr_low_flags: u32) {
        log::debug!(
            "apic: send_ipi dest={} icr_low={:#010X}",
            dest_apic_id,
            icr_low_flags
        );

        self.wait_icr_idle();

        if self.x2apic {
            // x2APIC: single 64-bit MSR write at 0x830
            let icr_val = ((dest_apic_id as u64) << 32) | (icr_low_flags as u64);
            unsafe {
                Self::wrmsr(X2APIC_MSR_BASE + (REG_ICR_LOW >> 4), icr_val);
            }
        } else {
            // xAPIC: write high (destination) first, then low (triggers send)
            self.write_reg(REG_ICR_HIGH, dest_apic_id << 24);
            self.write_reg(REG_ICR_LOW, icr_low_flags);
        }

        log::trace!("apic: IPI sent to APIC ID {}", dest_apic_id);
    }

    /// Send an INIT IPI to a specific AP.
    pub fn send_init_ipi(&self, dest_apic_id: u32) {
        log::info!("apic: sending INIT IPI to APIC ID {}", dest_apic_id);
        self.send_ipi(
            dest_apic_id,
            ICR_INIT | ICR_LEVEL_ASSERT | ICR_TRIGGER_LEVEL | ICR_DEST_PHYSICAL | ICR_NO_SHORTHAND,
        );
    }

    /// Send a de-assert INIT IPI (required after INIT assert for some hardware).
    pub fn send_init_deassert(&self, dest_apic_id: u32) {
        log::debug!("apic: sending INIT de-assert to APIC ID {}", dest_apic_id);
        self.send_ipi(
            dest_apic_id,
            ICR_INIT | ICR_LEVEL_DEASSERT | ICR_TRIGGER_LEVEL | ICR_DEST_PHYSICAL | ICR_NO_SHORTHAND,
        );
    }

    /// Send a STARTUP IPI (SIPI) to a specific AP.
    ///
    /// `trampoline_page` is the 4 KiB-aligned physical page number where the
    /// AP trampoline code resides (e.g., 0x08 for physical address 0x8000).
    pub fn send_startup_ipi(&self, dest_apic_id: u32, trampoline_page: u8) {
        log::info!(
            "apic: sending SIPI to APIC ID {}, trampoline page {:#04X} (phys {:#X})",
            dest_apic_id,
            trampoline_page,
            (trampoline_page as u32) << 12
        );
        self.send_ipi(
            dest_apic_id,
            ICR_STARTUP | ICR_DEST_PHYSICAL | ICR_NO_SHORTHAND | (trampoline_page as u32),
        );
    }

    // -----------------------------------------------------------------------
    // APIC Timer
    // -----------------------------------------------------------------------

    /// Set up the APIC timer.
    ///
    /// `vector` — interrupt vector to fire.
    /// `mode` — `TIMER_ONESHOT`, `TIMER_PERIODIC`, or `TIMER_TSC_DEADLINE`.
    /// `divide` — timer divide configuration (e.g., `TIMER_DIV_16`).
    /// `initial_count` — initial countdown value.
    pub fn setup_timer(&self, vector: u8, mode: u32, divide: u32, initial_count: u32) {
        log::info!(
            "apic: setup_timer vector={:#04X} mode={:#010X} divide={:#06X} initial={}",
            vector,
            mode,
            divide,
            initial_count
        );

        // Set divide configuration
        self.write_reg(REG_TIMER_DIVIDE, divide);

        // Configure LVT Timer: mode | vector (unmasked)
        self.write_reg(REG_LVT_TIMER, mode | (vector as u32));

        // Set initial count (starts the timer)
        self.write_reg(REG_TIMER_INITIAL, initial_count);

        log::debug!("apic: timer started, initial count = {}", initial_count);
    }

    /// Stop the APIC timer by masking the LVT Timer entry and zeroing the count.
    pub fn stop_timer(&self) {
        log::debug!("apic: stopping APIC timer");
        self.write_reg(REG_LVT_TIMER, LVT_MASKED);
        self.write_reg(REG_TIMER_INITIAL, 0);
    }

    /// Read the current timer count.
    pub fn timer_current_count(&self) -> u32 {
        self.read_reg(REG_TIMER_CURRENT)
    }

    // -----------------------------------------------------------------------
    // Calibration helper
    // -----------------------------------------------------------------------

    /// Calibrate the APIC timer frequency by counting ticks over a known delay.
    ///
    /// `delay_fn` should spin for approximately `ms` milliseconds (e.g., using
    /// PIT channel 2 or TSC). Returns approximate ticks per millisecond.
    pub fn calibrate_timer(&self, delay_ms: u32, delay_fn: impl FnOnce(u32)) -> u32 {
        log::info!("apic: calibrating timer over {}ms", delay_ms);

        self.write_reg(REG_TIMER_DIVIDE, TIMER_DIV_16);
        self.write_reg(REG_LVT_TIMER, LVT_MASKED); // masked, won't fire
        self.write_reg(REG_TIMER_INITIAL, 0xFFFF_FFFF);

        delay_fn(delay_ms);

        let remaining = self.timer_current_count();
        self.write_reg(REG_TIMER_INITIAL, 0); // stop

        let elapsed = 0xFFFF_FFFFu32.wrapping_sub(remaining);
        let ticks_per_ms = elapsed / delay_ms;
        log::info!(
            "apic: calibration complete: elapsed={} ticks in {}ms, ~{} ticks/ms",
            elapsed,
            delay_ms,
            ticks_per_ms
        );
        ticks_per_ms
    }
}
