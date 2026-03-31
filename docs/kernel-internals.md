# Kernel Internals

Detailed documentation of ClaudioOS kernel subsystems. All source lives in `kernel/src/`.

---

## Table of Contents

- [GDT and TSS (including the Data Segment Bug)](#gdt-and-tss)
- [Memory Management](#memory-management)
- [Interrupt System (PIC and APIC)](#interrupt-system)
- [Serial UART](#serial-uart)
- [Logger](#logger)
- [Framebuffer](#framebuffer)
- [PCI Enumeration](#pci-enumeration)
- [Panic Handler](#panic-handler)

---

## GDT and TSS

**Source:** `kernel/src/gdt.rs`

The Global Descriptor Table and Task State Segment are the first CPU structures
initialized after serial output is available (Phase 1 in the boot sequence).

### Why We Need a Custom GDT

Even in 64-bit long mode, x86_64 requires a valid GDT with at least a kernel code
segment. The bootloader sets one up, but we replace it with our own to include:

1. A TSS for safe double-fault and timer interrupt handling (IST stacks)
2. A **kernel data segment** -- this was the hardest Phase 1 bug to track down

### THE DATA SEGMENT BUG (Critical Phase 1 Fix)

This was the single hardest bug in Phase 1 development and is documented here
prominently so future developers understand why the GDT includes a data segment
and why `DS`, `ES`, and `SS` are explicitly loaded.

**Symptom**: After enabling interrupts, the very first hardware interrupt (timer or
keyboard) would cause an immediate double fault. The interrupt stack frame showed
`SS = 0x0` (null segment selector).

**Root Cause**: When the CPU pushes an interrupt stack frame, it saves the current
`SS` (Stack Segment) register value. The bootloader's GDT only contains a code
segment and a TSS -- no data segment at all. The bootloader sets `SS = 0` because
in long mode, SS is largely vestigial for normal code execution. However, when an
interrupt fires, the CPU:

1. Pushes `SS` onto the interrupt stack (value: 0x0 -- null selector)
2. Pushes `RSP`, `RFLAGS`, `CS`, `RIP`
3. After the ISR runs and executes `iretq`, the CPU pops `SS` back
4. Loading `SS = 0x0` via `iretq` triggers a **#GP (General Protection Fault)**
5. The #GP handler tries to push another interrupt frame with `SS = 0x0` -> **double fault**

**The Fix**: The GDT must include a kernel data segment, and all data segment registers
(`DS`, `ES`, `SS`) must be loaded with its selector:

```rust
// kernel/src/gdt.rs
static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code_selector = gdt.append(Descriptor::kernel_code_segment());
    let data_selector = gdt.append(Descriptor::kernel_data_segment());  // THE FIX
    let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));
    (gdt, Selectors { code_selector, data_selector, tss_selector })
});

pub fn init() {
    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        // Load data segment registers -- needed for interrupt frame SS field
        DS::set_reg(GDT.1.data_selector);
        ES::set_reg(GDT.1.data_selector);
        SS::set_reg(GDT.1.data_selector);
        load_tss(GDT.1.tss_selector);
    }
}
```

Without this fix, the system triple-faults on the first interrupt. This bug is
particularly insidious because:
- It only manifests when interrupts are enabled
- The double fault handler may itself fail if SS is bad
- Many bare-metal OS tutorials skip the data segment because it "isn't needed in long mode"
- The `x86_64` crate's blog_os tutorial only added it later

**Lesson**: In long mode, `SS` is still used by the interrupt mechanism for the
stack frame push/pop cycle. A valid, non-null `SS` selector is mandatory.

### GDT Layout

```
GDT Entry Map:
  Entry 0:   Null descriptor (required by x86)
  Entry 1:   Kernel code segment (64-bit, Execute/Read, DPL=0)
  Entry 2:   Kernel data segment (64-bit, Read/Write, DPL=0)
  Entry 3-4: TSS descriptor (16 bytes, spans two GDT entries)
```

### TSS and the Interrupt Stack Table

The TSS provides the Interrupt Stack Table (IST), which lets the CPU switch to a
known-good stack when certain exceptions or interrupts fire:

```
IST Stack Allocations:
  IST[0] = Double-fault handler (20 KiB = 4096 * 5)
            Statically allocated in .bss as [u8; 20480].
            Used by the double_fault IDT entry.

  IST[1] = Timer interrupt handler (16 KiB = 4096 * 4)
            Statically allocated in .bss as [u8; 16384].
            Gives the timer its own stack so IRQ0 works
            regardless of how deep the kernel stack is.
            Fixes double faults during executor BTreeMap
            operations or deep log formatting.
```

Both IST stacks are statically allocated (not on the heap) because the GDT/TSS
init happens before the heap is available (Phase 1 vs Phase 2).

Stack pointers stored in the TSS point to the TOP of each stack (stacks grow
downward on x86):

```rust
tss.interrupt_stack_table[0] = stack_start + STACK_SIZE;
```

### IST Stack Addresses (Debug Log)

During init, the IST stack top addresses are logged for debugging:
```
[gdt] IST[0] (double fault) top: 0x<address>
[gdt] IST[1] (timer)        top: 0x<address>
```

---

## Memory Management

**Source:** `kernel/src/memory.rs`

### Overview

ClaudioOS uses a two-tier memory system:

1. **Physical frame allocator** (`BootInfoFrameAllocator`): Doles out 4 KiB physical
   frames from regions marked `Usable` in the UEFI memory map.
2. **Kernel heap** (`linked_list_allocator`): A virtual memory region at a fixed
   address, backed by physical frames, providing `alloc::*` types.

### Physical Frame Allocator

```rust
pub struct BootInfoFrameAllocator {
    memory_regions: &'static MemoryRegions,  // from UEFI memory map
    next: usize,                              // index into usable frames
}
```

The allocator filters the bootloader-provided memory regions for
`MemoryRegionKind::Usable`, then iterates through them in 4096-byte steps. Each call
to `allocate_frame()` returns the `next`-th usable frame and increments the counter.

**Limitation**: This is a bump allocator -- frames are never freed. This is sufficient
for Phase 1 where memory is only allocated during boot (heap pages, stack, IST stacks).
Future phases will need a bitmap or buddy allocator for dynamic frame reclamation.

**Performance note**: The `usable_frames()` iterator is reconstructed on every
`allocate_frame()` call and iterated with `.nth(self.next)`. This is O(n) per
allocation where n is the number of usable frames visited so far. Acceptable for
boot-time use but would need optimization for runtime frame allocation.

### Heap Initialization Sequence

```
Step 1: Read CR3 to find the active Level 4 page table
        (kernel/src/memory.rs: active_level_4_table)
        |
        v
Step 2: Create OffsetPageTable using phys_mem_offset
        This type from the x86_64 crate translates
        physical addresses in page table entries to
        virtual addresses by adding phys_mem_offset.
        |
        v
Step 3: For each 4 KiB page in [HEAP_START, HEAP_START + HEAP_SIZE):
        |-- Allocate a physical frame from BootInfoFrameAllocator
        |-- Map: virtual page -> physical frame
        |   Flags: PRESENT | WRITABLE
        |-- Flush TLB entry for the new mapping
        |
        v
Step 4: Initialize linked_list_allocator
        ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE)
        |
        v
Heap is now operational.
alloc::Box, alloc::Vec, alloc::String, BTreeMap, etc. work.
```

### Address Translation Model

The bootloader maps all physical memory at a configurable offset:

```
virtual_addr  = physical_addr + phys_mem_offset
physical_addr = virtual_addr  - phys_mem_offset
```

The `OffsetPageTable` type uses this relationship to walk page tables (which store
physical addresses) by adding the offset to get dereferenceable virtual addresses.

**Important exception**: Heap memory at `0x4444_4444_0000` does NOT follow this
formula because it is a separate mapping. To get the physical address of a heap
allocation, a full 4-level page table walk is required. The VirtIO-net driver
(`crates/net/src/nic.rs`) implements `VirtQueue::virt_to_phys()` for exactly this
purpose (needed for DMA buffer addresses).

### Constants

| Constant | Value | Purpose |
|----------|-------|---------|
| `HEAP_START` | `0x4444_4444_0000` | Virtual address of heap start |
| `HEAP_SIZE` | `1048576` (1 MiB) | Initial heap size |

The heap start address is chosen to be far from both the kernel's identity-mapped
region and the bootloader's physical memory mapping to avoid collisions.

---

## Interrupt System

**Source:** `kernel/src/interrupts.rs`

### IDT Configuration

The Interrupt Descriptor Table is lazily initialized via `spin::Lazy`:

```
IDT Entries:
  Vector 3  : breakpoint_handler        (CPU exception)
  Vector 8  : double_fault_handler      (CPU exception, IST[0])
  Vector 14 : page_fault_handler        (CPU exception, reads CR2)
  Vector 32 : timer_handler             (PIC1 IRQ0)
  Vector 33 : keyboard_handler          (PIC1 IRQ1)
```

### APIC Disable (Critical Pre-PIC Step)

UEFI firmware enables the Local APIC as part of its boot process. If we initialize
the 8259 PIC without disabling the APIC first, **both** can deliver timer interrupts
on the same vector simultaneously. This causes a double fault because:

1. The APIC timer fires on vector 32
2. The PIC timer also fires on vector 32
3. The CPU tries to handle both, leading to stack corruption or re-entrance

The fix disables the APIC globally by clearing bit 11 (Global Enable) of the
`IA32_APIC_BASE` MSR (address `0x1B`):

```rust
unsafe {
    let mut apic_base_msr = Msr::new(0x1B);
    let val = apic_base_msr.read();
    apic_base_msr.write(val & !(1 << 11));
}
```

This is done **before** the PIC ICW sequence and logged at trace level for debugging.

### 8259 PIC Initialization (Full ICW Sequence)

The dual 8259 PIC is initialized with the complete ICW (Initialization Command Word)
sequence. I/O wait between commands uses writes to port `0x80` (the POST code
diagnostic port), which introduces a ~1 microsecond delay needed by real hardware
(QEMU doesn't need it but real PIC chips do).

```
ICW Sequence for PIC1 (Master, ports 0x20/0x21):

  ICW1 -> cmd port 0x20 = 0x11
    Bit 0 = 1: ICW4 will be needed
    Bit 4 = 1: Initialization command
    (0x11 = begin init, cascade mode, expect ICW4)

  ICW2 -> data port 0x21 = 0x20 (decimal 32)
    Vector offset for IRQ 0-7 -> interrupt vectors 32-39
    (Avoids collision with CPU exception vectors 0-31)

  ICW3 -> data port 0x21 = 0x04
    Bit 2 = 1: Slave PIC connected to IRQ line 2

  ICW4 -> data port 0x21 = 0x01
    Bit 0 = 1: 8086/88 mode (vs MCS-80 mode)

ICW Sequence for PIC2 (Slave, ports 0xA0/0xA1):

  ICW1 -> cmd port 0xA0 = 0x11  (same as master)
  ICW2 -> data port 0xA1 = 0x28 (decimal 40)
    Vector offset for IRQ 8-15 -> interrupt vectors 40-47
  ICW3 -> data port 0xA1 = 0x02
    Cascade identity = 2 (connected to master's IRQ2)
  ICW4 -> data port 0xA1 = 0x01  (8086/88 mode)

OCW1 (Interrupt Masks), set after ICW sequence:

  PIC1 data port 0x21 = 0b1111_1100
    Bit 0 = 0: IRQ0 (timer) UNMASKED
    Bit 1 = 0: IRQ1 (keyboard) UNMASKED
    Bits 2-7 = 1: IRQ2-7 MASKED

  PIC2 data port 0xA1 = 0b1111_1111
    All IRQ8-15 MASKED (no slave interrupts needed in Phase 1)
```

### End-of-Interrupt (EOI) Protocol

After handling a hardware interrupt, the handler must send EOI (byte `0x20`) to the
PIC command port:

- **PIC1 interrupts** (vectors 32-39): Send EOI to PIC1 only (port `0x20`)
- **PIC2 interrupts** (vectors 40-47): Send EOI to **both** PIC2 (port `0xA0`)
  AND PIC1 (because PIC2 cascades through PIC1's IRQ2 line)

The `notify_end_of_interrupt()` function handles this logic. The timer handler takes
a shortcut and writes directly to port `0x20` to avoid the mutex lock overhead.

### Handler Implementations

**Timer handler** (`timer_handler`): Absolute minimum -- just sends EOI by writing
`0x20` to port `0x20`. No logging, no asm, no serial. The comment in source says
"ONLY EOI. No asm!, no serial, no nothing. Pure minimal." This is because any
additional work risks stack overflow or deadlock (serial lock contention).

**Keyboard handler** (`keyboard_handler`):
1. Reads raw scancode from PS/2 data port (`0x60`) -- must read BEFORE EOI
2. Calls `keyboard::push_scancode()` which enqueues and wakes the async reader
3. Sends EOI via `notify_end_of_interrupt()`

**Breakpoint handler**: Writes directly to serial port `0x3F8` without using the
log framework. This avoids stack-heavy formatting in the ISR.

**Page fault handler**: Reads the faulting address from CR2, uses `log::error!()` to
output the address, error code, and stack frame, then halts.

**Double fault handler**: Uses `log::error!()` to output the stack frame, then halts.
Runs on IST[0] (20 KiB dedicated stack) so it can handle stack overflow cases.

### Interrupt Enable Timing

Interrupts are deliberately NOT enabled in `interrupts::init()`. A separate
`interrupts::enable()` function calls `x86_64::instructions::interrupts::enable()`
(the `STI` instruction). This is called only from `post_stack_switch()` after the
heap stack switch, ensuring:

1. The heap is initialized
2. The keyboard decoder is ready
3. The framebuffer is initialized
4. PCI enumeration is complete
5. The stack has plenty of room (256 KiB fresh allocation)
6. The executor is about to start

---

## Serial UART

**Source:** `kernel/src/serial.rs`

### 16550 UART Initialization

The serial port at I/O base `0x3F8` (COM1) is initialized with the standard 16550
register programming sequence:

```
Register Writes (relative to base 0x3F8):
  base+1 = 0x00  Disable all UART interrupts
  base+3 = 0x80  Enable DLAB (Divisor Latch Access Bit)
  base+0 = 0x01  Divisor low byte = 1  (115200 baud @ 1.8432 MHz clock)
  base+1 = 0x00  Divisor high byte = 0
  base+3 = 0x03  8 data bits, no parity, 1 stop bit (8N1), clear DLAB
  base+2 = 0xC7  Enable FIFO, clear TX/RX FIFOs, 14-byte trigger threshold
  base+4 = 0x0F  IRQs enabled, RTS/DSR set (normal operation mode)
```

### Output Protocol

The `Write` trait implementation busy-waits on the Line Status Register (base+5)
until the Transmit Holding Register Empty bit (bit 5) is set, then writes one byte
to the data register (base+0). This is blocking but fast at 115200 baud.

### Macros

| Macro | Purpose | Lock behavior |
|-------|---------|---------------|
| `serial_print!` | Normal serial output | `without_interrupts()` + mutex lock |
| `serial_println!` | Serial output + newline | Same as above |
| `force_println!` | Panic-safe serial output | `force_unlock()` + mutex lock |

**Interrupt safety**: `_print()` disables interrupts while holding the serial lock
via `without_interrupts()`. This prevents a deadlock scenario where:
1. Normal code acquires the serial lock to log something
2. An interrupt fires (e.g., keyboard)
3. The ISR tries to log (acquiring the serial lock) -> deadlock

**`force_println!`**: Used exclusively in the panic handler. Calls
`SERIAL.force_unlock()` to release the spinlock unconditionally, then acquires it
normally. This is only safe because the panic handler is a diverging function --
the code that previously held the lock will never resume.

---

## Logger

**Source:** `kernel/src/logger.rs`

A minimal `log::Log` implementation that forwards all log records to serial output
via `serial_println!()`. Format:

```
[LEVEL] message
```

Where LEVEL is padded to 5 characters: `TRACE`, `DEBUG`, ` INFO`, ` WARN`, `ERROR`.

Initialized with `log::set_max_level(LevelFilter::Trace)` -- all levels enabled.
The `CLAUDIO_LOG_LEVEL` environment variable is not yet wired up.

---

## Framebuffer

**Source:** `kernel/src/framebuffer.rs`

The UEFI GOP (Graphics Output Protocol) framebuffer is the sole display output.

### Framebuffer Address Mapping Problem

The bootloader v0.11 maps the framebuffer at its own chosen virtual address (e.g.,
`0x20000000000`). However, this mapping may have restrictive page table flags
(lacking the WRITABLE bit), causing a page fault when the kernel tries to write
pixels.

### Solution: Physical Address Indirection

The kernel does NOT write to the bootloader's virtual address. Instead:

1. **Page table walk**: Translate the bootloader's virtual address to the
   framebuffer's physical address using `OffsetPageTable::translate_addr()`
2. **Access via phys_mem_offset**: Compute `phys_mem_offset + physical_address`
   to get a virtual address within the physical memory mapping, which is guaranteed
   to be PRESENT + WRITABLE

```
Bootloader virt addr  --[page table walk]--> FB physical addr
                                                |
FB physical addr + phys_mem_offset  ----->  Writable virtual addr
```

### State

```rust
pub struct FrameBufferState {
    pub buffer: &'static mut [u8],   // raw pixel data (via phys_mem_offset)
    pub width: usize,                 // pixels
    pub height: usize,                // pixels
    pub stride: usize,                // pixels per row (may > width for alignment)
    pub bytes_per_pixel: usize,       // typically 4 (BGR + padding byte)
}
```

### Pixel Format

UEFI GOP typically provides BGR pixel format. The `put_pixel()` function writes
bytes in BGR order:

```
offset = (y * stride + x) * bytes_per_pixel
buffer[offset + 0] = blue
buffer[offset + 1] = green
buffer[offset + 2] = red
// buffer[offset + 3] = padding (unused)
```

### Init Sequence

1. Get framebuffer info (width, height, stride, pixel format)
2. Get the bootloader's virtual address for the buffer
3. Walk the page table to find the physical address
4. Compute the writable virtual address through phys_mem_offset
5. Create a `&'static mut [u8]` slice at that address
6. Clear the entire buffer to black (zero all bytes)
7. Store in global `FB: Mutex<Option<FrameBufferState>>`

---

## PCI Enumeration

**Source:** `kernel/src/pci.rs`

### Config Space Access Mechanism

PCI configuration space is accessed via the legacy I/O port mechanism:

```
Port 0xCF8 (CONFIG_ADDRESS): Write the target address
  Bit  31:    Enable bit (must be 1)
  Bits 23-16: Bus number (0-255)
  Bits 15-11: Device number (0-31)
  Bits 10-8:  Function number (0-7)
  Bits 7-2:   Register offset (aligned to 4 bytes)
  Bits 1-0:   Must be 0

Port 0xCFC (CONFIG_DATA): Read/write 32-bit config register value

Address formula:
  address = 0x8000_0000
          | (bus << 16)
          | (device << 11)
          | (function << 8)
          | (offset & 0xFC)
```

### Scan Strategy

Currently scans only bus 0, devices 0-31, function 0. This is sufficient for QEMU,
which places all emulated devices on bus 0. For each slot, reads:

- **Offset 0x00**: Vendor ID (low 16 bits), Device ID (high 16 bits)
- **Offset 0x08**: Class code (bits 31-24), Subclass (bits 23-16)
- **Offset 0x10**: BAR0 (Base Address Register 0)
- **Offset 0x3C**: Interrupt line (low 8 bits)

Vendor ID `0xFFFF` indicates an empty slot (skipped).

### Device Recognition and Bus Mastering

Known devices are identified and logged:

| Vendor | Device | Description | Action |
|--------|--------|-------------|--------|
| `0x1AF4` | `0x1000` | VirtIO network device | Enable bus mastering |
| `0x1AF4` | `0x1001` | VirtIO block device | Log only |
| `0x8086` | `0x100E` | Intel 82540EM (e1000) | Enable bus mastering |
| `0x8086` | `0x10D3` | Intel 82574L | Enable bus mastering |

**Bus mastering** is enabled by setting bit 2 in the PCI Command register
(offset `0x04`). This is required for any device that performs DMA (VirtIO, e1000).

### Device Storage

Discovered devices are stored in a static array (`PCI_DEVICES: Mutex<PciDevices>`)
with capacity for 32 devices. Lookup functions:

- `find_nic()`: Returns the first VirtIO-net device (`0x1AF4:0x1000`)
- `find_device(vendor, device)`: Generic lookup by vendor/device ID

The `PciDevice` struct includes `bar0` and an `io_base()` helper that strips the
I/O space indicator bits (`bar0 & !0x3`) to get the port base address.

---

## Panic Handler

**Source:** `kernel/src/main.rs` (bottom)

```rust
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial::force_println!("\n!!! KERNEL PANIC !!!");
    serial::force_println!("{}", info);
    halt_loop()
}
```

The panic handler:
1. Uses `force_println!` which calls `SERIAL.force_unlock()` to break any held lock
2. Prints the panic message to serial (includes file, line, and message)
3. Enters an infinite `HLT` loop (CPU sleeps, wakes on interrupt, halts again)

**No framebuffer panic rendering yet**: Future phases will also render the panic
message in red text on the framebuffer for visibility on physical hardware without
serial access.

**`halt_loop()`** is also used after page faults and double faults. It loops
`x86_64::instructions::hlt()` forever, which puts the CPU in a low-power state
between interrupts.
