//! Interrupt Descriptor Table — timer, keyboard, page fault, double fault.
//!
//! Hardware interrupts (PIC remapped to 32-47) drive the async executor:
//! - Timer (IRQ0/32): tick the executor, poll smoltcp
//! - Keyboard (IRQ1/33): push keycode to input queue, wake terminal future

use core::sync::atomic::{AtomicU64, Ordering};
use spin::{Lazy, Mutex};
use x86_64::instructions::port::Port;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

/// Global tick counter incremented by the PIT timer interrupt (IRQ0).
///
/// The PIT fires at ~18.2 Hz by default (actually 1193182/65536 ≈ 18.2065 Hz).
/// Each tick is ~54.925 ms. We use this as the time source for smoltcp.
static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// Return the current tick count (incremented ~18.2 times/second by IRQ0).
pub fn tick_count() -> u64 {
    TICK_COUNT.load(Ordering::Relaxed)
}

/// Return the current time as milliseconds since boot, derived from the PIT
/// tick counter. At ~18.2 Hz, each tick ≈ 54.925 ms. We approximate with
/// tick * 55 for simplicity.
pub fn millis_since_boot() -> i64 {
    (TICK_COUNT.load(Ordering::Relaxed) * 55) as i64
}

use crate::gdt;

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();

    // CPU exceptions
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.general_protection_fault.set_handler_fn(general_protection_handler);
    idt.page_fault.set_handler_fn(page_fault_debug);
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }

    // Hardware interrupts (PIC offsets)
    idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_handler);
    idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_handler);

    idt
});

// ── 8259 PIC constants ──────────────────────────────────────────────

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;
const PIC1_OFFSET: u8 = 32;
const PIC2_OFFSET: u8 = 40;

/// PIC interrupt vector indices.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC1_OFFSET,
    Keyboard = PIC1_OFFSET + 1,
}

impl InterruptIndex {
    fn as_usize(self) -> usize {
        self as usize
    }

    fn as_u8(self) -> u8 {
        self as u8
    }
}

// ── Chained 8259 PIC abstraction ────────────────────────────────────

/// Chained 8259 PIC controller pair (master + slave).
///
/// PIC1 (master): IRQ 0-7 mapped to vectors 32-39
/// PIC2 (slave):  IRQ 8-15 mapped to vectors 40-47
struct Pics {
    pic1_cmd: Port<u8>,
    pic1_data: Port<u8>,
    pic2_cmd: Port<u8>,
    pic2_data: Port<u8>,
}

impl Pics {
    const fn new() -> Self {
        Self {
            pic1_cmd: Port::new(PIC1_CMD),
            pic1_data: Port::new(PIC1_DATA),
            pic2_cmd: Port::new(PIC2_CMD),
            pic2_data: Port::new(PIC2_DATA),
        }
    }

    /// Full PIC initialization with ICW1-ICW4 command sequence.
    ///
    /// Remaps PIC1 to offset 32 and PIC2 to offset 40 to avoid collisions
    /// with CPU exception vectors (0-31). Unmasks only IRQ0 (timer) and
    /// IRQ1 (keyboard).
    unsafe fn init(&mut self) {
        unsafe {
            // Write to port 0x80 between commands for I/O wait (gives PIC
            // time to process each command on real hardware).
            let mut wait_port: Port<u8> = Port::new(0x80);

            // ICW1: begin init sequence, cascade mode, ICW4 needed
            self.pic1_cmd.write(0x11);
            wait_port.write(0);
            self.pic2_cmd.write(0x11);
            wait_port.write(0);

            // ICW2: vector offsets
            self.pic1_data.write(PIC1_OFFSET); // IRQ 0-7 -> vectors 32-39
            wait_port.write(0);
            self.pic2_data.write(PIC2_OFFSET); // IRQ 8-15 -> vectors 40-47
            wait_port.write(0);

            // ICW3: cascade wiring
            self.pic1_data.write(4); // PIC1 bit 2 set = slave on IRQ2
            wait_port.write(0);
            self.pic2_data.write(2); // PIC2 cascade identity = 2
            wait_port.write(0);

            // ICW4: 8086/88 mode
            self.pic1_data.write(0x01);
            wait_port.write(0);
            self.pic2_data.write(0x01);
            wait_port.write(0);

            // OCW1: IRQ masks — timer + keyboard for Phase 1.
            // Now that the Local APIC is disabled, IRQ0 (timer) is safe to unmask.
            self.pic1_data.write(0b1111_1100); // IRQ0 (timer) + IRQ1 (keyboard) unmasked
            self.pic2_data.write(0b1111_1111); // PIC2: all masked
        }
    }

    /// Send End-Of-Interrupt for the given interrupt vector.
    ///
    /// For PIC2 interrupts (vectors 40-47), EOI must be sent to BOTH PIC2
    /// and PIC1 (because PIC2 cascades through PIC1's IRQ2 line).
    /// For PIC1 interrupts (vectors 32-39), only PIC1 gets EOI.
    unsafe fn end_of_interrupt(&mut self, interrupt_id: u8) {
        unsafe {
            if interrupt_id >= PIC2_OFFSET {
                self.pic2_cmd.write(0x20);
            }
            self.pic1_cmd.write(0x20);
        }
    }
}

/// Global PIC instance, protected by a spinlock.
static PICS: Mutex<Pics> = Mutex::new(Pics::new());

/// Send EOI for the given interrupt vector. Called from ISR handlers.
///
/// # Safety
/// Must only be called at the end of an interrupt handler for a valid
/// PIC-sourced interrupt vector.
unsafe fn notify_end_of_interrupt(interrupt_id: u8) {
    unsafe { PICS.lock().end_of_interrupt(interrupt_id) };
}

pub fn init() {
    // Force the Lazy IDT to initialize, then log its address
    let idt_ptr = &*IDT as *const InterruptDescriptorTable;
    log::info!("[int] IDT at {:p}", idt_ptr);
    log::info!("[int] breakpoint handler at {:p}", breakpoint_handler as *const ());
    log::info!("[int] timer handler at {:p}", timer_handler as *const ());

    IDT.load();

    // Disable the Local APIC before initializing the legacy 8259 PIC.
    //
    // UEFI firmware enables the Local APIC as part of its boot process.
    // If we initialize the 8259 PIC without disabling the APIC first,
    // BOTH can deliver timer interrupts on the same vector, causing a
    // double fault when IRQ0 fires. The APIC's timer may be programmed
    // by the firmware to fire periodically, and its interrupt delivery
    // conflicts with the PIC's IRQ routing.
    //
    // We disable the APIC globally by clearing bit 11 (Global Enable)
    // of the IA32_APIC_BASE MSR (0x1B). This causes all APIC interrupts
    // to stop, letting the 8259 PIC handle hardware interrupts cleanly.
    unsafe {
        let mut apic_base_msr = x86_64::registers::model_specific::Msr::new(0x1B);
        let val = apic_base_msr.read();
        apic_base_msr.write(val & !(1 << 11));
        log::trace!("[int] Local APIC disabled (was {:#x}, now {:#x})", val, val & !(1 << 11));
    }

    unsafe {
        PICS.lock().init();
    }
    // NOTE: Do NOT enable interrupts here — let the caller decide when it's safe.
    // Interrupts should be enabled only after all init is complete and the
    // executor is ready to service them.
}

/// Enable hardware interrupts. Call this after all init is complete.
pub fn enable() {
    x86_64::instructions::interrupts::enable();
}

// ── Exception handlers ──────────────────────────────────────────────

extern "x86-interrupt" fn breakpoint_handler(_stack_frame: InterruptStackFrame) {
    // Minimal handler — log formatting overflows the kernel stack.
    // Use raw serial for ISR debugging instead.
    unsafe {
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x3F8);
        for &b in b"[int] breakpoint hit\r\n" {
            port.write(b);
        }
    }
}

extern "x86-interrupt" fn general_protection_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    // #GP (General Protection) fault.  Common causes:
    //   - Unaligned memory access with SSE instructions (MOVAPS on non-16B addr)
    //   - Segment violations
    //   - Privileged instruction in user mode
    //
    // For SSE alignment faults, error_code is 0 and RIP points to the
    // faulting instruction.  Print diagnostics via raw serial to avoid
    // any chance of faulting again during log formatting.
    unsafe {
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x3F8);
        for &b in b"\r\n!!! GENERAL PROTECTION FAULT !!!\r\nerror_code=" {
            port.write(b);
        }
        for i in (0..16).rev() {
            let nibble = ((error_code >> (i * 4)) & 0xF) as u8;
            port.write(if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 });
        }
        for &b in b"\r\nRIP=" {
            port.write(b);
        }
        let rip = stack_frame.instruction_pointer.as_u64();
        for i in (0..16).rev() {
            let nibble = ((rip >> (i * 4)) & 0xF) as u8;
            port.write(if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 });
        }
        for &b in b"\r\nRSP=" {
            port.write(b);
        }
        let rsp = stack_frame.stack_pointer.as_u64();
        for i in (0..16).rev() {
            let nibble = ((rsp >> (i * 4)) & 0xF) as u8;
            port.write(if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 });
        }
        for &b in b"\r\n" { port.write(b); }
    }
    log::error!(
        "[int] GENERAL PROTECTION FAULT (error_code={:#x})\n{:#?}",
        error_code,
        stack_frame,
    );
    crate::halt_loop();
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    // Use raw serial to avoid any stack-heavy formatting
    unsafe {
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x3F8);
        for &b in b"\r\n!!! PAGE FAULT !!!\r\n" {
            port.write(b);
        }
    }
    log::error!(
        "[int] PAGE FAULT at {:?} ({:?})\n{:#?}",
        Cr2::read(),
        error_code,
        stack_frame,
    );
    crate::halt_loop();
}

extern "x86-interrupt" fn page_fault_debug(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    // Raw serial to avoid stack overflow from log formatting
    unsafe {
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x3F8);
        for &b in b"\r\n!!! PAGE FAULT !!!\r\nCR2=" {
            port.write(b);
        }
        let addr = Cr2::read().unwrap_or(x86_64::VirtAddr::new(0)).as_u64();
        for i in (0..16).rev() {
            let nibble = ((addr >> (i * 4)) & 0xF) as u8;
            port.write(if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 });
        }
        for &b in b"\r\n" { port.write(b); }
    }
    crate::halt_loop();
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    use x86_64::registers::control::Cr2;
    // Print CR2 (faulting address), RSP, and RIP via raw serial FIRST,
    // before any formatting that might itself fault.  A double fault
    // during TLS handshake is often caused by a #GP from unaligned SSE
    // access, which doesn't set CR2, but if it's a page fault escalation
    // then CR2 tells us exactly which address was inaccessible.
    unsafe {
        let mut port = x86_64::instructions::port::Port::<u8>::new(0x3F8);
        for &b in b"\r\n!!! DOUBLE FAULT !!!\r\nCR2=" {
            port.write(b);
        }
        let addr = Cr2::read().unwrap_or(x86_64::VirtAddr::new(0)).as_u64();
        for i in (0..16).rev() {
            let nibble = ((addr >> (i * 4)) & 0xF) as u8;
            port.write(if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 });
        }
        for &b in b"\r\nRSP=" {
            port.write(b);
        }
        let rsp = stack_frame.stack_pointer.as_u64();
        for i in (0..16).rev() {
            let nibble = ((rsp >> (i * 4)) & 0xF) as u8;
            port.write(if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 });
        }
        for &b in b"\r\nRIP=" {
            port.write(b);
        }
        let rip = stack_frame.instruction_pointer.as_u64();
        for i in (0..16).rev() {
            let nibble = ((rip >> (i * 4)) & 0xF) as u8;
            port.write(if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 });
        }
        for &b in b"\r\n" { port.write(b); }
    }
    log::error!("[int] DOUBLE FAULT\n{:#?}", stack_frame);
    crate::halt_loop();
}

// ── Hardware interrupt handlers ─────────────────────────────────────

extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    // Increment global tick counter for smoltcp timestamps.
    TICK_COUNT.fetch_add(1, Ordering::Relaxed);
    // EOI — must be last.
    unsafe {
        x86_64::instructions::port::Port::<u8>::new(0x20).write(0x20);
    }
}

extern "x86-interrupt" fn keyboard_handler(_stack_frame: InterruptStackFrame) {
    // Read scancode from PS/2 data port (must read BEFORE sending EOI,
    // otherwise the PIC may deliver the next IRQ before we read this one)
    let scancode: u8 = unsafe { Port::new(0x60).read() };

    // Track modifier state and intercept Ctrl+Alt+F1-F6 for virtual console switching.
    if let Some(console_idx) = crate::vconsole::process_scancode(scancode) {
        crate::vconsole::switch_console(console_idx);
        // Consume the scancode — don't pass F-key to the keyboard decoder.
        unsafe { notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8()); }
        return;
    }

    // Push scancode to the async keyboard queue and wake the reader
    crate::keyboard::push_scancode(scancode);

    // Acknowledge the interrupt
    unsafe {
        notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}
