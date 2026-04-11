//! Power Management — suspend/resume, lid close, battery, idle savings.
//!
//! Provides ACPI-based power state transitions (S1 sleep, S3 suspend-to-RAM,
//! S4 hibernate), lid close detection via the ACPI Embedded Controller,
//! power button handling, battery status reading, and idle power saving
//! with HLT and P-state frequency scaling hints.
//!
//! ## Shell commands
//!
//! - `suspend`  — Enter S3 (suspend-to-RAM)
//! - `hibernate` — Enter S4 (hibernate, requires swap)
//! - `poweroff` — Enter S5 (shutdown), delegated to `acpi_init::shutdown()`
//! - `battery`  — Display battery percentage, state, and time remaining

extern crate alloc;

use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

// ---------------------------------------------------------------------------
// Power state model
// ---------------------------------------------------------------------------

/// ACPI-aligned power states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PowerState {
    /// S0 — fully running.
    Running = 0,
    /// S1 — CPU caches flushed, CPU stopped, RAM refreshed. Quick wake.
    Sleeping = 1,
    /// S3 — suspend-to-RAM. Only RAM is powered.
    Suspended = 3,
    /// S4 — hibernate. RAM image saved to disk, full power off.
    Hibernating = 4,
}

/// Current power state, atomically accessible from interrupt handlers.
static CURRENT_STATE: AtomicU8 = AtomicU8::new(PowerState::Running as u8);

/// Whether a suspend/resume cycle is in progress (guards re-entrance).
static TRANSITION_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Get the current power state.
pub fn current_state() -> PowerState {
    match CURRENT_STATE.load(Ordering::Acquire) {
        0 => PowerState::Running,
        1 => PowerState::Sleeping,
        3 => PowerState::Suspended,
        4 => PowerState::Hibernating,
        _ => PowerState::Running,
    }
}

// ---------------------------------------------------------------------------
// CPU state save/restore for S3
// ---------------------------------------------------------------------------

/// Saved CPU register context for resume-from-RAM.
///
/// On x86_64, control registers, segment selectors, and the IDT/GDT pointers
/// must be restored after an S3 wake (the firmware restarts at the waking
/// vector with a minimal environment).
#[repr(C)]
struct CpuContext {
    cr0: u64,
    cr3: u64,
    cr4: u64,
    rsp: u64,
    rbp: u64,
    rflags: u64,
    /// IA32_EFER MSR (needed for long mode re-entry).
    efer: u64,
    /// GDT pseudo-descriptor (limit + base).
    gdt_base: u64,
    gdt_limit: u16,
    /// IDT pseudo-descriptor.
    idt_base: u64,
    idt_limit: u16,
}

static SAVED_CPU: Mutex<Option<CpuContext>> = Mutex::new(None);

/// Save the current CPU state into the global context.
///
/// # Safety
///
/// Reads control registers and MSRs. Must be called with interrupts disabled.
unsafe fn save_cpu_state() {
    let cr0: u64;
    let cr3: u64;
    let cr4: u64;
    let rsp: u64;
    let rbp: u64;
    let rflags: u64;

    unsafe {
        core::arch::asm!("mov {}, cr0", out(reg) cr0);
        core::arch::asm!("mov {}, cr3", out(reg) cr3);
        core::arch::asm!("mov {}, cr4", out(reg) cr4);
        core::arch::asm!("mov {}, rsp", out(reg) rsp);
        core::arch::asm!("mov {}, rbp", out(reg) rbp);
        core::arch::asm!("pushfq; pop {}", out(reg) rflags);
    }

    // IA32_EFER (MSR 0xC0000080) — Extended Feature Enable Register.
    let efer = unsafe {
        x86_64::registers::model_specific::Msr::new(0xC000_0080).read()
    };

    // Read GDT and IDT pseudo-descriptors via SGDT/SIDT.
    let mut gdt_buf = [0u8; 10]; // 2-byte limit + 8-byte base
    let mut idt_buf = [0u8; 10];
    unsafe {
        core::arch::asm!("sgdt [{}]", in(reg) gdt_buf.as_mut_ptr(), options(nostack));
        core::arch::asm!("sidt [{}]", in(reg) idt_buf.as_mut_ptr(), options(nostack));
    }

    let gdt_limit = u16::from_le_bytes([gdt_buf[0], gdt_buf[1]]);
    let gdt_base = u64::from_le_bytes([
        gdt_buf[2], gdt_buf[3], gdt_buf[4], gdt_buf[5],
        gdt_buf[6], gdt_buf[7], gdt_buf[8], gdt_buf[9],
    ]);
    let idt_limit = u16::from_le_bytes([idt_buf[0], idt_buf[1]]);
    let idt_base = u64::from_le_bytes([
        idt_buf[2], idt_buf[3], idt_buf[4], idt_buf[5],
        idt_buf[6], idt_buf[7], idt_buf[8], idt_buf[9],
    ]);

    let ctx = CpuContext {
        cr0, cr3, cr4, rsp, rbp, rflags, efer,
        gdt_base, gdt_limit,
        idt_base, idt_limit,
    };

    log::debug!("[power] CPU state saved: CR0={:#x} CR3={:#x} CR4={:#x} RSP={:#x}",
        cr0, cr3, cr4, rsp);

    *SAVED_CPU.lock() = Some(ctx);
}

/// Restore the saved CPU state after an S3 wake.
///
/// # Safety
///
/// Writes control registers, reloads GDT/IDT, and restores the stack pointer.
/// Must be called very early in the resume path.
unsafe fn restore_cpu_state() {
    let guard = SAVED_CPU.lock();
    let ctx = match guard.as_ref() {
        Some(c) => c,
        None => {
            log::error!("[power] no saved CPU state to restore!");
            return;
        }
    };

    log::debug!("[power] restoring CPU state: CR0={:#x} CR3={:#x} CR4={:#x}",
        ctx.cr0, ctx.cr3, ctx.cr4);

    // Rebuild GDT/IDT pseudo-descriptor buffers for LGDT/LIDT.
    let mut gdt_buf = [0u8; 10];
    gdt_buf[0..2].copy_from_slice(&ctx.gdt_limit.to_le_bytes());
    gdt_buf[2..10].copy_from_slice(&ctx.gdt_base.to_le_bytes());
    let mut idt_buf = [0u8; 10];
    idt_buf[0..2].copy_from_slice(&ctx.idt_limit.to_le_bytes());
    idt_buf[2..10].copy_from_slice(&ctx.idt_base.to_le_bytes());

    unsafe {
        // Restore IA32_EFER first (needed for long mode).
        x86_64::registers::model_specific::Msr::new(0xC000_0080).write(ctx.efer);

        // Restore control registers.
        core::arch::asm!("mov cr0, {}", in(reg) ctx.cr0);
        core::arch::asm!("mov cr3, {}", in(reg) ctx.cr3);
        core::arch::asm!("mov cr4, {}", in(reg) ctx.cr4);

        // Reload GDT and IDT.
        core::arch::asm!("lgdt [{}]", in(reg) gdt_buf.as_ptr(), options(nostack));
        core::arch::asm!("lidt [{}]", in(reg) idt_buf.as_ptr(), options(nostack));

        // Restore RFLAGS.
        core::arch::asm!("push {}; popfq", in(reg) ctx.rflags);
    }

    log::info!("[power] CPU state restored");
}

// ---------------------------------------------------------------------------
// Suspend-to-RAM (S3)
// ---------------------------------------------------------------------------

/// Suspend the system to RAM (ACPI S3).
///
/// Saves CPU state, flushes caches, writes SLP_TYPa|SLP_EN to PM1a_CNT.
/// On wake, firmware jumps to the waking vector and we restore state.
///
/// Returns `Ok(())` after successful wake, or `Err` if ACPI info is missing.
pub fn suspend_to_ram() -> Result<(), &'static str> {
    if TRANSITION_IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return Err("power state transition already in progress");
    }

    log::info!("[power] ============================================");
    log::info!("[power]   Suspend to RAM (S3)");
    log::info!("[power] ============================================");

    // Read FADT info from ACPI subsystem.
    let acpi_snap = crate::acpi_init::info()
        .ok_or("ACPI not initialized")?;
    let fadt = acpi_snap.fadt_info
        .ok_or("FADT not available — cannot determine PM registers")?;

    // Disable interrupts for the critical section.
    x86_64::instructions::interrupts::disable();

    CURRENT_STATE.store(PowerState::Suspended as u8, Ordering::Release);

    // Save CPU state.
    unsafe { save_cpu_state(); }

    // Flush all CPU caches (WBINVD).
    log::info!("[power] flushing CPU caches (WBINVD)...");
    unsafe {
        core::arch::asm!("wbinvd", options(nostack));
    }

    // Parse S3 sleep type from DSDT.  The \_S3 object is structured the same
    // way as \_S5 but with a different name.  We search for it ourselves rather
    // than modifying the claudio-acpi crate.
    let (s3_typ_a, s3_typ_b) = find_sleep_type_from_dsdt(fadt.dsdt_address, b"_S3_")
        .unwrap_or((1, 1)); // Fallback: SLP_TYP=1 is common for S3

    log::info!("[power] S3 sleep types: SLP_TYPa={} SLP_TYPb={}", s3_typ_a, s3_typ_b);

    // Write SLP_TYP | SLP_EN to PM1a_CNT.
    let slp_en: u16 = 1 << 13;
    let val_a: u16 = ((s3_typ_a as u16) << 10) | slp_en;

    log::info!("[power] writing PM1a_CNT ({:#X}) <- {:#X}", fadt.pm1a_cnt_port, val_a);

    unsafe {
        Port::<u16>::new(fadt.pm1a_cnt_port).write(val_a);
    }

    if fadt.pm1b_cnt_port != 0 {
        let val_b: u16 = ((s3_typ_b as u16) << 10) | slp_en;
        log::info!("[power] writing PM1b_CNT ({:#X}) <- {:#X}", fadt.pm1b_cnt_port, val_b);
        unsafe {
            Port::<u16>::new(fadt.pm1b_cnt_port).write(val_b);
        }
    }

    // --- If we reach here, the system woke up from S3. ---
    // The firmware has restored minimal state and jumped to the waking vector.

    log::info!("[power] *** RESUMED from S3 ***");
    resume_from_ram();

    CURRENT_STATE.store(PowerState::Running as u8, Ordering::Release);
    TRANSITION_IN_PROGRESS.store(false, Ordering::Release);

    Ok(())
}

/// Resume from S3 — restore CPU state and re-initialize devices.
fn resume_from_ram() {
    log::info!("[power] resume_from_ram: restoring CPU state...");

    unsafe { restore_cpu_state(); }

    // Re-enable SSE/AVX (firmware may have reset CR0/CR4/XCR0).
    log::info!("[power] re-enabling SSE/AVX...");
    unsafe {
        let mut cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0);
        cr0 &= !(1 << 2); // clear EM
        cr0 |= 1 << 1;    // set MP
        core::arch::asm!("mov cr0, {}", in(reg) cr0);

        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4);
        cr4 |= (1 << 9) | (1 << 10); // OSFXSR + OSXMMEXCPT
        core::arch::asm!("mov cr4, {}", in(reg) cr4);
    }

    // Re-initialize the PIC (firmware may have reconfigured it).
    log::info!("[power] re-initializing interrupts...");
    crate::interrupts::init();

    // Re-initialize keyboard.
    crate::keyboard::init();

    // Re-enable interrupts.
    crate::interrupts::enable();

    log::info!("[power] resume complete — system running");
}

// ---------------------------------------------------------------------------
// Hibernate (S4)
// ---------------------------------------------------------------------------

/// Hibernate the system (ACPI S4).
///
/// In a full implementation this would:
/// 1. Save entire RAM contents to a swap/hibernate partition
/// 2. Enter S4 sleep state (or S5 if S4 is not supported)
///
/// Currently, we save CPU state and enter S4 via ACPI if the sleep type
/// is available, falling back to a clean shutdown.
pub fn hibernate() -> Result<(), &'static str> {
    if TRANSITION_IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return Err("power state transition already in progress");
    }

    log::info!("[power] ============================================");
    log::info!("[power]   Hibernate (S4)");
    log::info!("[power] ============================================");

    let acpi_snap = crate::acpi_init::info()
        .ok_or("ACPI not initialized")?;
    let fadt = acpi_snap.fadt_info
        .ok_or("FADT not available")?;

    CURRENT_STATE.store(PowerState::Hibernating as u8, Ordering::Release);

    // Write a hibernate marker file through the VFS. This is NOT a real
    // RAM snapshot — full hibernate requires a disk driver, page-table
    // walk, and contiguous swap-slot allocation. But a marker file lets
    // the next boot detect that S4 was entered cleanly and log it.
    let marker_bytes = write_hibernate_marker();
    log::info!(
        "[power] wrote hibernate marker to /claudio/hibernate.snapshot ({} bytes). Full RAM snapshot requires disk driver + page table walk.",
        marker_bytes,
    );

    x86_64::instructions::interrupts::disable();

    unsafe { save_cpu_state(); }

    // Flush caches.
    unsafe { core::arch::asm!("wbinvd", options(nostack)); }

    // Find S4 sleep type from DSDT.
    let (s4_typ_a, s4_typ_b) = find_sleep_type_from_dsdt(fadt.dsdt_address, b"_S4_")
        .unwrap_or_else(|| {
            log::warn!("[power] S4 sleep type not found in DSDT, falling back to shutdown");
            // Use S5 as fallback — this will power off the machine.
            (5, 5)
        });

    let slp_en: u16 = 1 << 13;
    let val_a: u16 = ((s4_typ_a as u16) << 10) | slp_en;

    log::info!("[power] writing PM1a_CNT ({:#X}) <- {:#X} (S4)", fadt.pm1a_cnt_port, val_a);

    unsafe {
        Port::<u16>::new(fadt.pm1a_cnt_port).write(val_a);
    }

    if fadt.pm1b_cnt_port != 0 {
        let val_b: u16 = ((s4_typ_b as u16) << 10) | slp_en;
        unsafe {
            Port::<u16>::new(fadt.pm1b_cnt_port).write(val_b);
        }
    }

    // If we get here, we woke from S4 (unlikely without RAM restore).
    CURRENT_STATE.store(PowerState::Running as u8, Ordering::Release);
    TRANSITION_IN_PROGRESS.store(false, Ordering::Release);

    log::info!("[power] returned from S4 (unexpected without RAM restore)");
    Ok(())
}

/// Path for the hibernate marker file.
const HIBERNATE_MARKER_PATH: &str = "/claudio/hibernate.snapshot";

/// Write a minimal hibernate marker to the VFS. Returns the number of
/// bytes written (0 on failure).
///
/// Layout (little-endian):
/// - 16 bytes: magic `"CLAUDIOHIBERN\0\0\0"`
/// - 8 bytes:  unix timestamp (best effort from RTC)
/// - 8 bytes:  kernel tick count
/// - 8 bytes:  heap_used
/// - 8 bytes:  heap_total
/// - rest:     zero padding to 1024 bytes total (reserved for future snapshot index)
fn write_hibernate_marker() -> usize {
    const MARKER_SIZE: usize = 1024;
    let mut buf = alloc::vec![0u8; MARKER_SIZE];

    // Magic (16 bytes).
    let magic = b"CLAUDIOHIBERN\0\0\0";
    buf[0..16].copy_from_slice(magic);

    // Timestamp from RTC.
    let ts = crate::rtc::wall_clock().to_unix_timestamp() as u64;
    buf[16..24].copy_from_slice(&ts.to_le_bytes());

    // Tick count.
    let ticks = crate::interrupts::tick_count();
    buf[24..32].copy_from_slice(&ticks.to_le_bytes());

    // Heap stats.
    let (heap_used, heap_total) = crate::memory::heap_stats();
    buf[32..40].copy_from_slice(&(heap_used as u64).to_le_bytes());
    buf[40..48].copy_from_slice(&(heap_total as u64).to_le_bytes());

    match claudio_fs::write_file(HIBERNATE_MARKER_PATH, &buf) {
        Ok(()) => MARKER_SIZE,
        Err(e) => {
            log::warn!(
                "[power] failed to write hibernate marker {}: {:?}",
                HIBERNATE_MARKER_PATH, e
            );
            0
        }
    }
}

/// Check for a hibernate marker left by a previous S4 cycle.
///
/// Called on boot (optional). Logs what was found and (best effort) parses
/// the embedded timestamp and memory stats. Does NOT perform a real resume
/// — that would require paging/VMM work.
pub fn resume_from_snapshot() {
    match claudio_fs::read_file(HIBERNATE_MARKER_PATH) {
        Ok(bytes) => {
            if bytes.len() < 48 {
                log::warn!(
                    "[power] hibernate marker too small ({} bytes), ignoring",
                    bytes.len()
                );
                return;
            }
            if &bytes[0..13] != b"CLAUDIOHIBERN" {
                log::warn!("[power] hibernate marker has unexpected magic, ignoring");
                return;
            }
            let ts = u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19],
                bytes[20], bytes[21], bytes[22], bytes[23],
            ]);
            let ticks = u64::from_le_bytes([
                bytes[24], bytes[25], bytes[26], bytes[27],
                bytes[28], bytes[29], bytes[30], bytes[31],
            ]);
            let heap_used = u64::from_le_bytes([
                bytes[32], bytes[33], bytes[34], bytes[35],
                bytes[36], bytes[37], bytes[38], bytes[39],
            ]);
            let heap_total = u64::from_le_bytes([
                bytes[40], bytes[41], bytes[42], bytes[43],
                bytes[44], bytes[45], bytes[46], bytes[47],
            ]);
            log::info!(
                "[power] found hibernate marker: ts={} ticks={} heap={}/{} KiB",
                ts, ticks, heap_used / 1024, heap_total / 1024,
            );
            log::info!(
                "[power] marker present — full RAM restore pending VM subsystem; starting fresh"
            );
        }
        Err(_) => {
            log::debug!("[power] no hibernate marker found at {}", HIBERNATE_MARKER_PATH);
        }
    }
}

// ---------------------------------------------------------------------------
// DSDT sleep type parser (S1/S3/S4)
// ---------------------------------------------------------------------------

/// Search the DSDT AML bytecode for a named sleep object (\_S1_, \_S3_, \_S4_)
/// and extract its SLP_TYPa and SLP_TYPb values.
///
/// Returns `None` if the DSDT is not available or the object is not found.
fn find_sleep_type_from_dsdt(dsdt_addr: Option<u64>, name: &[u8; 4]) -> Option<(u8, u8)> {
    let dsdt_addr = dsdt_addr?;
    let phys_offset = crate::PHYS_MEM_OFFSET.load(Ordering::Relaxed);
    let mapped_addr = dsdt_addr + phys_offset;

    // Read the SDT header to get the table length.
    let header_ptr = mapped_addr as *const u8;
    let length = unsafe {
        // Bytes 4-7 of the SDT header are the total table length (u32 LE).
        let len_bytes = core::slice::from_raw_parts(header_ptr.add(4), 4);
        u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize
    };

    if length < 36 || length > 4 * 1024 * 1024 {
        log::warn!("[power] DSDT length looks invalid: {}", length);
        return None;
    }

    let data = unsafe { core::slice::from_raw_parts(mapped_addr as *const u8, length) };

    // Search for the 4-byte name in AML bytecode.
    for i in 0..data.len().saturating_sub(10) {
        if &data[i..i + 4] == name {
            let mut j = i + 4;

            // Expect PackageOp (0x12) after the name.
            if j < data.len() && data[j] == 0x12 {
                j += 1;

                // Skip PkgLength (variable 1-4 bytes).
                if j < data.len() {
                    let lead = data[j];
                    if lead & 0xC0 == 0 {
                        j += 1; // 1-byte length
                    } else {
                        j += 1 + ((lead >> 6) as usize); // multi-byte
                    }
                }

                // Skip NumElements byte.
                if j < data.len() { j += 1; }

                // Read SLP_TYPa.
                if j < data.len() {
                    let slp_a = if data[j] == 0x0A {
                        j += 1;
                        if j < data.len() { data[j] } else { return None; }
                    } else {
                        data[j]
                    };
                    j += 1;

                    // Read SLP_TYPb (may be absent).
                    let slp_b = if j < data.len() {
                        if data[j] == 0x0A {
                            j += 1;
                            if j < data.len() { data[j] } else { slp_a }
                        } else {
                            data[j]
                        }
                    } else {
                        slp_a
                    };

                    log::info!("[power] found {:?} in DSDT: SLP_TYPa={} SLP_TYPb={}",
                        core::str::from_utf8(name).unwrap_or("????"), slp_a, slp_b);
                    return Some((slp_a, slp_b));
                }
            }
        }
    }

    log::warn!("[power] {:?} object not found in DSDT ({} bytes)",
        core::str::from_utf8(name).unwrap_or("????"), length);
    None
}

// ---------------------------------------------------------------------------
// Lid close detection — ACPI Embedded Controller
// ---------------------------------------------------------------------------

/// ACPI EC (Embedded Controller) data port — standard is 0x62.
const EC_DATA_PORT: u16 = 0x62;
/// ACPI EC command/status port — standard is 0x66.
const EC_CMD_PORT: u16 = 0x66;

/// EC command: read byte.
const EC_CMD_READ: u8 = 0x80;
/// EC status: Output Buffer Full (bit 0).
const EC_OBF: u8 = 1 << 0;
/// EC status: Input Buffer Full (bit 1).
const EC_IBF: u8 = 1 << 1;

/// Common EC offset for lid status on many laptops.
/// This varies by OEM; common values are 0x03 or 0x50.
const EC_LID_OFFSET: u8 = 0x03;

/// Whether lid-close auto-suspend is enabled.
static LID_CLOSE_ENABLED: AtomicBool = AtomicBool::new(true);

/// Enable or disable lid-close auto-suspend.
pub fn set_lid_close_suspend(enabled: bool) {
    LID_CLOSE_ENABLED.store(enabled, Ordering::Relaxed);
    log::info!("[power] lid-close suspend: {}", if enabled { "enabled" } else { "disabled" });
}

/// Wait for the EC input buffer to be empty.
fn ec_wait_ibf_clear() -> bool {
    for _ in 0..10_000 {
        let status: u8 = unsafe { Port::<u8>::new(EC_CMD_PORT).read() };
        if status & EC_IBF == 0 {
            return true;
        }
    }
    false
}

/// Wait for the EC output buffer to have data.
fn ec_wait_obf_set() -> bool {
    for _ in 0..10_000 {
        let status: u8 = unsafe { Port::<u8>::new(EC_CMD_PORT).read() };
        if status & EC_OBF != 0 {
            return true;
        }
    }
    false
}

/// Read a byte from the ACPI Embedded Controller at the given offset.
fn ec_read(offset: u8) -> Option<u8> {
    if !ec_wait_ibf_clear() {
        return None;
    }
    unsafe { Port::<u8>::new(EC_CMD_PORT).write(EC_CMD_READ); }

    if !ec_wait_ibf_clear() {
        return None;
    }
    unsafe { Port::<u8>::new(EC_DATA_PORT).write(offset); }

    if !ec_wait_obf_set() {
        return None;
    }
    let val = unsafe { Port::<u8>::new(EC_DATA_PORT).read() };
    Some(val)
}

/// Check the lid state via the ACPI Embedded Controller.
///
/// Returns `Some(true)` if the lid is open, `Some(false)` if closed,
/// `None` if the EC is not accessible.
pub fn lid_is_open() -> Option<bool> {
    let val = ec_read(EC_LID_OFFSET)?;
    // Bit 0 set = lid open on most implementations.
    Some(val & 0x01 != 0)
}

/// Poll lid state and suspend if closed. Call this periodically from the
/// idle loop or a timer callback.
pub fn check_lid_and_suspend() {
    if !LID_CLOSE_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    if let Some(open) = lid_is_open() {
        if !open {
            log::info!("[power] lid closed detected — suspending to RAM");
            let _ = suspend_to_ram();
        }
    }
}

// ---------------------------------------------------------------------------
// Power button handling
// ---------------------------------------------------------------------------

/// Ticks at which the power button was first seen pressed.
static POWER_BUTTON_PRESS_TICK: AtomicU64 = AtomicU64::new(0);

/// Whether we are tracking a power button press.
static POWER_BUTTON_DOWN: AtomicBool = AtomicBool::new(false);

/// Threshold for a "long press" in PIT ticks (~18.2 Hz).
/// ~3 seconds = 55 ticks.
const LONG_PRESS_TICKS: u64 = 55;

/// Called when a power button press ACPI event (SCI) is detected.
///
/// On real hardware, the SCI interrupt handler would call this when
/// the power button fixed event fires (PM1_STS bit 8).
pub fn power_button_pressed() {
    let tick = crate::interrupts::tick_count();
    POWER_BUTTON_PRESS_TICK.store(tick, Ordering::Release);
    POWER_BUTTON_DOWN.store(true, Ordering::Release);
    log::info!("[power] power button pressed at tick {}", tick);
}

/// Called when the power button is released. Decides action based on duration.
pub fn power_button_released() {
    if !POWER_BUTTON_DOWN.swap(false, Ordering::AcqRel) {
        return;
    }

    let press_tick = POWER_BUTTON_PRESS_TICK.load(Ordering::Acquire);
    let now_tick = crate::interrupts::tick_count();
    let held_ticks = now_tick.saturating_sub(press_tick);

    if held_ticks >= LONG_PRESS_TICKS {
        log::info!("[power] long press ({} ticks) — shutting down", held_ticks);
        crate::acpi_init::shutdown();
    } else {
        log::info!("[power] short press ({} ticks) — suspending to RAM", held_ticks);
        let _ = suspend_to_ram();
    }
}

/// Check the PM1 status register for a power button event.
/// Call this periodically or from the SCI interrupt handler.
pub fn check_power_button() {
    let acpi_snap = match crate::acpi_init::info() {
        Some(s) => s,
        None => return,
    };
    let fadt = match acpi_snap.fadt_info {
        Some(f) => f,
        None => return,
    };

    // PM1a event block contains the PM1_STS register.
    // Power button status is bit 8.
    let _pm1a_sts_port = fadt.pm1a_cnt_port.wrapping_sub(
        // PM1_STS is at the event block base; PM1_CNT is at control block base.
        // We stored pm1a_cnt_port but need pm1a_evt. On most systems, the event
        // block is at (pm1a_control_block - pm1_event_length). Since we don't
        // have the raw event block port stored, we read it from the FADT snapshot.
        // For safety, skip if we can't determine the port.
        0
    );

    // If pm1a event block address is available, check bit 8.
    // Since FadtInfo doesn't expose pm1a_event_block, we use a heuristic:
    // the event block is typically at pm1a_cnt_port - 4 on PIIX4/ICH.
    let pm1_sts_port = if fadt.pm1a_cnt_port > 4 {
        fadt.pm1a_cnt_port - 4
    } else {
        return;
    };

    let status: u16 = unsafe { Port::<u16>::new(pm1_sts_port).read() };
    if status & (1 << 8) != 0 {
        // Clear the power button status bit by writing 1 to it.
        unsafe { Port::<u16>::new(pm1_sts_port).write(1 << 8); }
        power_button_pressed();
    }
}

// ---------------------------------------------------------------------------
// Battery status via ACPI
// ---------------------------------------------------------------------------

/// Battery charge state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryChargeState {
    /// AC powered, battery charging.
    Charging,
    /// Running on battery.
    Discharging,
    /// Battery full, on AC power.
    Full,
    /// Cannot determine state.
    Unknown,
}

/// Battery information snapshot.
#[derive(Debug, Clone)]
pub struct BatteryInfo {
    /// Battery present.
    pub present: bool,
    /// Charge state.
    pub state: BatteryChargeState,
    /// Charge percentage (0-100), if available.
    pub percentage: Option<u8>,
    /// Estimated time remaining in minutes, if available.
    pub time_remaining_minutes: Option<u32>,
    /// Current capacity in mWh, if available.
    pub current_capacity_mwh: Option<u32>,
    /// Full charge capacity in mWh, if available.
    pub full_capacity_mwh: Option<u32>,
}

/// Read battery status from the ACPI Embedded Controller.
///
/// Battery information is typically at EC offsets that vary by OEM.
/// Common layouts (e.g., Lenovo ThinkPad, HP):
/// - EC offset 0x38: battery status flags
/// - EC offset 0x3C-0x3D: remaining capacity (LE u16, in mWh or mAh)
/// - EC offset 0x3E-0x3F: full charge capacity
///
/// This function attempts a best-effort read. On systems without an
/// accessible EC or with different EC layouts, it returns a default.
pub fn battery_status() -> BatteryInfo {
    let default = BatteryInfo {
        present: false,
        state: BatteryChargeState::Unknown,
        percentage: None,
        time_remaining_minutes: None,
        current_capacity_mwh: None,
        full_capacity_mwh: None,
    };

    // Try reading battery status byte from EC.
    let status_byte = match ec_read(0x38) {
        Some(v) => v,
        None => {
            log::debug!("[power] EC not accessible — no battery info");
            return default;
        }
    };

    // Bit 0: battery present
    // Bit 1: battery charging
    // Bit 2: battery discharging
    // Bit 7: AC power present
    let present = status_byte & 0x01 != 0;
    if !present {
        return BatteryInfo { present: false, ..default };
    }

    let state = if status_byte & 0x02 != 0 {
        BatteryChargeState::Charging
    } else if status_byte & 0x04 != 0 {
        BatteryChargeState::Discharging
    } else if status_byte & 0x80 != 0 {
        BatteryChargeState::Full
    } else {
        BatteryChargeState::Unknown
    };

    // Read remaining capacity (EC offsets 0x3C-0x3D, little-endian u16).
    let cap_lo = ec_read(0x3C).unwrap_or(0) as u32;
    let cap_hi = ec_read(0x3D).unwrap_or(0) as u32;
    let current_cap = (cap_hi << 8) | cap_lo;

    // Read full charge capacity (EC offsets 0x3E-0x3F).
    let full_lo = ec_read(0x3E).unwrap_or(0) as u32;
    let full_hi = ec_read(0x3F).unwrap_or(0) as u32;
    let full_cap = (full_hi << 8) | full_lo;

    let percentage = if full_cap > 0 {
        Some(((current_cap * 100) / full_cap).min(100) as u8)
    } else {
        None
    };

    // Rough time estimate: assume ~15W average draw for a laptop.
    // time_remaining = (current_cap_mwh / 15000) * 60 minutes.
    let time_remaining = if state == BatteryChargeState::Discharging && current_cap > 0 {
        Some((current_cap * 60) / 15000)
    } else {
        None
    };

    BatteryInfo {
        present,
        state,
        percentage,
        time_remaining_minutes: time_remaining,
        current_capacity_mwh: Some(current_cap),
        full_capacity_mwh: Some(full_cap),
    }
}

/// Format battery info as a human-readable string for the shell.
pub fn battery_status_string() -> String {
    let info = battery_status();
    if !info.present {
        return String::from("No battery detected (desktop or EC not accessible)");
    }

    let state_str = match info.state {
        BatteryChargeState::Charging => "Charging",
        BatteryChargeState::Discharging => "Discharging",
        BatteryChargeState::Full => "Full",
        BatteryChargeState::Unknown => "Unknown",
    };

    let pct_str = match info.percentage {
        Some(p) => alloc::format!("{}%", p),
        None => String::from("?%"),
    };

    let time_str = match info.time_remaining_minutes {
        Some(m) if m > 0 => alloc::format!(" (~{}h{}m remaining)", m / 60, m % 60),
        _ => String::new(),
    };

    alloc::format!("Battery: {} {} {}{}", state_str, pct_str,
        if let (Some(cur), Some(full)) = (info.current_capacity_mwh, info.full_capacity_mwh) {
            alloc::format!("[{}/{}mWh]", cur, full)
        } else {
            String::new()
        },
        time_str,
    )
}

// ---------------------------------------------------------------------------
// Idle power saving
// ---------------------------------------------------------------------------

/// Execute the HLT instruction to halt the CPU until the next interrupt.
///
/// This is the primary idle power-saving mechanism. The CPU enters a low-power
/// state (C1) and wakes on any interrupt (timer, keyboard, NIC).
///
/// The kernel's main executor loop should call this when there is no work.
#[inline]
pub fn idle_halt() {
    x86_64::instructions::hlt();
}

/// Attempt to set a P-state (performance state) hint via ACPI.
///
/// P-states control CPU frequency/voltage scaling. Lower P-states = lower
/// frequency = lower power. P0 is the highest performance state.
///
/// On modern Intel CPUs with HWP (Hardware P-states), the OS hint is advisory.
/// The CPU's power management unit makes the final decision.
///
/// `pstate`: 0 = max performance, higher = lower power.
pub fn set_pstate_hint(pstate: u8) {
    // Check for Intel HWP (Hardware-Controlled Performance States).
    // CPUID.06H:EAX bit 7 indicates HWP support.
    let cpuid_06 = core::arch::x86_64::__cpuid(6);
    let hwp_supported = (cpuid_06.eax & (1 << 7)) != 0;

    if hwp_supported {
        // IA32_HWP_REQUEST MSR (0x774).
        // Bits 7:0   = Minimum_Performance
        // Bits 15:8  = Maximum_Performance
        // Bits 23:16 = Desired_Performance
        // Bits 31:24 = Energy_Performance_Preference (0=perf, 255=power)
        let epp = match pstate {
            0 => 0u64,         // max performance
            1 => 64,           // balanced-performance
            2 => 128,          // balanced
            _ => 255,          // max power saving
        };

        // Read current HWP_REQUEST, modify EPP field.
        let mut msr = x86_64::registers::model_specific::Msr::new(0x774);
        let current = unsafe { msr.read() };
        let new_val = (current & !0xFF00_0000) | (epp << 24);

        unsafe { msr.write(new_val); }

        log::debug!("[power] HWP EPP set to {} (pstate hint={})", epp, pstate);
    } else {
        // Legacy P-state control: write to PERF_CTL MSR (0x199).
        // The ratio is encoded in bits 15:8. We use a simple mapping.
        let ratio: u64 = match pstate {
            0 => 0xFF, // request max ratio
            1 => 0xC0,
            2 => 0x80,
            _ => 0x40, // low ratio
        };

        let mut msr = x86_64::registers::model_specific::Msr::new(0x199);
        let val = ratio << 8;
        unsafe { msr.write(val); }

        log::debug!("[power] legacy PERF_CTL ratio={:#X} (pstate hint={})", ratio, pstate);
    }
}

// ---------------------------------------------------------------------------
// Shell command handlers
// ---------------------------------------------------------------------------

/// Handle the `suspend` shell command.
pub fn cmd_suspend() -> String {
    match suspend_to_ram() {
        Ok(()) => String::from("Resumed from suspend (S3)."),
        Err(e) => alloc::format!("Suspend failed: {}", e),
    }
}

/// Handle the `hibernate` shell command.
pub fn cmd_hibernate() -> String {
    match hibernate() {
        Ok(()) => String::from("Resumed from hibernate (S4)."),
        Err(e) => alloc::format!("Hibernate failed: {}", e),
    }
}

/// Handle the `poweroff` shell command.
pub fn cmd_poweroff() -> ! {
    log::info!("[power] poweroff command received");
    crate::acpi_init::shutdown()
}

/// Handle the `battery` shell command.
pub fn cmd_battery() -> String {
    battery_status_string()
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize the power management subsystem.
///
/// Call after ACPI init. Sets up power button monitoring and idle hints.
pub fn init() {
    log::info!("[power] ============================================");
    log::info!("[power]   Power Management Init");
    log::info!("[power] ============================================");

    // Log battery status if available.
    let bat = battery_status();
    if bat.present {
        log::info!("[power] {}", battery_status_string());
    } else {
        log::info!("[power] no battery detected (desktop or EC not accessible)");
    }

    // Log lid state if accessible.
    match lid_is_open() {
        Some(true) => log::info!("[power] lid state: open"),
        Some(false) => log::info!("[power] lid state: closed"),
        None => log::info!("[power] lid state: not accessible (no EC or desktop)"),
    }

    // Set a balanced P-state by default.
    set_pstate_hint(2);

    log::info!("[power] idle power saving: HLT in idle loop");
    log::info!("[power] power button: short=suspend, long=shutdown");
    log::info!("[power] initialization complete");
}
