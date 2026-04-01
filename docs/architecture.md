# ClaudioOS Architecture

Deep technical overview of ClaudioOS's design, from the virtual address space to
the async executor's task lifecycle.

**Source entry point:** `kernel/src/main.rs`

---

## Table of Contents

- [System Overview](#system-overview)
- [Virtual Address Space Map](#virtual-address-space-map)
- [Boot Sequence](#boot-sequence)
- [The Heap Stack Switch](#the-heap-stack-switch)
- [Interrupt Handling Flow](#interrupt-handling-flow)
- [Async Executor Architecture](#async-executor-architecture)
- [Crate Dependency Graph](#crate-dependency-graph)
- [Source File Map](#source-file-map)

---

## System Overview

ClaudioOS is a single-address-space, bare-metal Rust application that boots via UEFI
and runs entirely in Ring 0. There is no kernel/user boundary, no syscalls, and no
process isolation. Every agent session is a cooperatively-scheduled async task driven
by hardware interrupts.

```
+-----------------------------------------------------+
|                  Agent Dashboard                     |
|  +----------+ +----------+ +----------+             |
|  | Agent 1  | | Agent 2  | | Agent 3  |  ...       |
|  | (pane)   | | (pane)   | | (pane)   |             |
|  +----------+ +----------+ +----------+             |
+-----------------------------------------------------+
|              Agent Session Manager                   |
|         (async tasks, one per agent)                 |
+--------------+--------------+-----------------------+
|  API Client  |     Auth     |   Terminal Renderer   |
|  (Messages   |  (OAuth      |   (framebuffer +      |
|   API + SSE) |   device     |    ANSI + split       |
|              |   flow)      |    panes)             |
+--------------+--------------+-----------------------+
|              Async Executor (interrupt-driven)       |
+-----------------------------------------------------+
|     Net (smoltcp + TLS)    |    FS (FAT32 persist)  |
+----------------------------+------------------------+
|   NIC Driver (virtio-net / e1000)  |  PS/2 Keyboard |
+------------------------------------+----------------+
|              x86_64 Kernel Core                      |
|   (paging, heap, GDT/IDT, interrupts, PCI)          |
+-----------------------------------------------------+
|              UEFI Boot (bootloader crate v0.11)      |
+-----------------------------------------------------+
```

---

## Virtual Address Space Map

The bootloader (v0.11) sets up the initial page tables before handing control to
`kernel_main`. The virtual address space is arranged as follows:

```
Virtual Address Space (x86_64, 48-bit canonical addresses)
==========================================================

0x0000_0000_0000_0000 +----------------------------------+
                       |  (unmapped / guard)              |
                       +----------------------------------+
                       |                                  |
                       |  Kernel code + data + rodata     |
                       |  (identity-mapped by bootloader) |
                       |                                  |
                       +----------------------------------+
                       |                                  |
                       |  Physical memory offset mapping  |
                       |  (ALL physical RAM accessible    |
                       |   at phys + phys_mem_offset)     |
                       |                                  |
                       |  The bootloader provides the     |
                       |  offset as a dynamic value in    |
                       |  BootInfo. Configured via:       |
                       |  Mapping::Dynamic in             |
                       |  BOOTLOADER_CONFIG.              |
                       |                                  |
                       +----------------------------------+
                       |                                  |
0x0000_4444_4444_0000  |  Kernel Heap (16 MiB)            |
                       |  HEAP_START = 0x4444_4444_0000   |
                       |  HEAP_SIZE  = 16 MiB             |
                       |  Managed by linked_list_allocator|
                       |  (kernel/src/memory.rs)          |
                       |                                  |
0x0000_4444_5444_0000  +----------------------------------+
                       |  (unmapped, available for heap   |
                       |   growth in future phases)       |
                       +----------------------------------+
                       |                                  |
                       |  Bootloader kernel stack         |
                       |  (128 KiB, set via               |
                       |   BootloaderConfig::              |
                       |   kernel_stack_size)             |
                       |                                  |
                       +----------------------------------+
                       |                                  |
                       |  Heap-allocated kernel stack     |
                       |  (4 MiB, allocated at boot,      |
                       |   leaked via mem::forget)        |
                       |  Actual runtime stack after      |
                       |  post_stack_switch().            |
                       |                                  |
                       +----------------------------------+
                       |                                  |
                       |  GOP Framebuffer                 |
                       |  (mapped by UEFI at a high virt  |
                       |   address, e.g. 0x20000000000;   |
                       |   kernel accesses it through the |
                       |   phys_mem_offset mapping after  |
                       |   page table walk to find the    |
                       |   physical address)              |
                       |                                  |
                       +----------------------------------+
                       |                                  |
                       |  IST stacks (static .bss)        |
                       |  IST[0]: 20 KiB double-fault     |
                       |  IST[1]: 16 KiB timer interrupt  |
                       |                                  |
                       +----------------------------------+
```

### Key Address Details

- **Physical memory offset mapping**: Configured via `BootloaderConfig` with
  `physical_memory = Some(Mapping::Dynamic)`. The bootloader picks a base address
  and maps all physical memory contiguously starting there. This is how
  `OffsetPageTable` translates physical addresses to virtual ones.

- **Heap at 0x4444_4444_0000**: Chosen to be far from the kernel's identity-mapped
  region and the bootloader's physical memory mapping. The heap virtual pages are
  backed by physical frames allocated from the UEFI memory map. See
  `kernel/src/memory.rs` constants `HEAP_START` and `HEAP_SIZE`.

- **Two kernel stacks**: The bootloader provides a 128 KiB stack, but this is nearly
  exhausted after init (log formatting is extremely stack-heavy). The kernel allocates
  a fresh 4 MiB stack on the heap and switches to it. See
  [The Heap Stack Switch](#the-heap-stack-switch).

- **Framebuffer address indirection**: The bootloader maps the GOP framebuffer at its
  own chosen virtual address, but this mapping may have restrictive flags (lacking
  WRITABLE). The kernel translates to the physical address via page table walk, then
  accesses it through the physical memory offset mapping which is known to be
  PRESENT + WRITABLE. See `kernel/src/framebuffer.rs`.

---

## Boot Sequence

The boot sequence proceeds through numbered phases, each depending on the previous.
All phases execute in `kernel_main()` at `kernel/src/main.rs:48`.

```
UEFI Firmware
    |
    v
bootloader crate (v0.11)
    - Reads kernel ELF from disk image
    - Sets up page tables (identity map + physical memory offset)
    - Initializes GOP framebuffer
    - Reads UEFI memory map
    - Disables interrupts
    - Jumps to kernel entry point
    |
    v
kernel_main(boot_info: &'static mut BootInfo)       [kernel/src/main.rs:48]
    |
    |-- Phase -1: Proof of life
    |     Raw serial write to port 0x3F8 (no UART init).
    |     Pushes bytes directly: "[claudio] kernel_main entered\r\n"
    |     Proves we reached kernel_main in QEMU even if UART init fails.
    |
    |-- Phase 0a: serial::init()                     [kernel/src/serial.rs]
    |     Full 16550 UART init: disable IRQs, DLAB, baud divisor=1
    |     (115200 baud), 8N1, FIFO enable, normal operation.
    |
    |-- Phase 0b: logger::init()                     [kernel/src/logger.rs]
    |     Sets log crate global logger -> all log::* macros route
    |     to serial output. Max level = Trace.
    |
    |-- Phase 1: gdt::init()                         [kernel/src/gdt.rs]
    |     Loads GDT with:
    |       - Kernel code segment (64-bit, DPL=0)
    |       - Kernel DATA segment (64-bit, DPL=0)    *** CRITICAL ***
    |       - TSS segment (with IST[0] and IST[1])
    |     Sets CS, DS, ES, SS segment registers.
    |     (See kernel-internals.md for the data segment bug)
    |
    |-- Phase 2: memory::init(phys_mem_offset, memory_regions)
    |     1. Reads CR3 -> active L4 page table       [kernel/src/memory.rs]
    |     2. Creates OffsetPageTable mapper
    |     3. Creates BootInfoFrameAllocator from UEFI memory map
    |     4. Maps HEAP_SIZE (16 MiB) pages at HEAP_START (0x4444_4444_0000)
    |     5. Initializes linked_list_allocator as #[global_allocator]
    |
    |-- Phase 3: interrupts::init()                  [kernel/src/interrupts.rs]
    |     1. Disables Local APIC (clears bit 11 of IA32_APIC_BASE MSR 0x1B)
    |        UEFI firmware enables the APIC; if left on, BOTH APIC and PIC
    |        deliver timer interrupts on the same vector -> double fault.
    |     2. Loads IDT with exception + IRQ handlers:
    |        - breakpoint (vec 3), page_fault (vec 14),
    |          double_fault (vec 8, IST[0])
    |        - timer (vec 32), keyboard (vec 33)
    |     3. Initializes 8259 PIC pair (ICW1-ICW4 sequence):
    |        - PIC1: offset 32, PIC2: offset 40
    |        - Unmasks IRQ0 (timer) + IRQ1 (keyboard)
    |     NOTE: Interrupts NOT enabled yet (STI not called).
    |
    |-- Phase 3b: keyboard::init()                   [kernel/src/keyboard.rs]
    |     Creates pc-keyboard decoder (US 104-key, ScancodeSet1).
    |
    |-- Phase 4: framebuffer::init(fb, phys_mem_offset)
    |     Translates bootloader's FB virt addr -> physical  [kernel/src/framebuffer.rs]
    |     via page table walk. Accesses through phys_mem_offset
    |     mapping. Clears to black.
    |
    |-- Phase 5: pci::enumerate()                    [kernel/src/pci.rs]
    |     Brute-force scan of bus 0, devices 0-31, function 0.
    |     Identifies VirtIO-net (1AF4:1000), e1000 (8086:100E), etc.
    |     Enables bus mastering for DMA-capable devices.
    |
    |-- Phase 6: Heap stack switch + executor start
    |     (See detailed section below)
    |
    v
  executor::run() never returns (infinite poll/hlt loop)
```

**Critical ordering constraints**:
- Heap (Phase 2) MUST be before interrupts (Phase 3) because the IDT is lazily
  initialized via `spin::Lazy` and interrupt handlers may allocate.
- Interrupts MUST NOT be enabled until the executor is ready (Phase 6).
- The APIC MUST be disabled before PIC init to prevent conflicting timer delivery.

---

## The Heap Stack Switch

This is one of the most important implementation details in ClaudioOS and was the
solution to a critical Phase 1 bug: the bootloader's kernel stack getting exhausted
by `log` crate formatting during init.

### The Problem

The bootloader provides a 128 KiB kernel stack (configured via
`BootloaderConfig::kernel_stack_size`). During boot, every `log::info!()` call
performs `format_args!()` which pushes significant stack frames. By Phase 5 (PCI
enumeration), the stack is deeply nested:

```
kernel_main
  -> gdt::init -> log::info! -> serial_println! -> format_args! -> Write::write_fmt
  -> memory::init -> log::info! -> ...
  -> interrupts::init -> log::info! -> ...
  -> pci::enumerate -> log::info! -> ... (30+ devices * log formatting)
```

After all init phases complete, the remaining stack space is too small for the
executor's BTreeMap operations and interrupt handler log formatting. Enabling
interrupts at this point causes a stack overflow -> page fault -> double fault.

### The Solution

The kernel allocates a fresh 4 MiB stack on the heap and switches to it via inline
assembly before enabling interrupts:

```rust
// kernel/src/main.rs, Phase 6
const NEW_STACK_SIZE: usize = 4 * 1024 * 1024;  // 4 MiB
let new_stack = alloc::vec![0u8; NEW_STACK_SIZE];
let new_stack_top = new_stack.as_ptr() as u64 + NEW_STACK_SIZE as u64;
core::mem::forget(new_stack);  // Leak -- kernel stack must never be freed

unsafe {
    core::arch::asm!(
        "mov rsp, {stack}",
        "call {entry}",
        stack = in(reg) new_stack_top,
        entry = in(reg) post_stack_switch as *const (),
        options(noreturn)
    );
}
```

The `post_stack_switch()` function runs on the fresh stack, enables interrupts,
and starts the executor:

```
post_stack_switch()                     [kernel/src/main.rs:141]
    |
    |-- interrupts::enable()  -> x86 STI instruction
    |-- executor::run(main_async)
    |     |
    |     v
    |   main_async() [runs inside executor]
    |     - Creates keyboard::ScancodeStream (async)
    |     - Loops: await next_key(), echo to serial
    v
  (never returns)
```

### Why `mem::forget`?

The Vec backing the new stack is leaked intentionally. If it were dropped, the
stack memory would be returned to the allocator while still in active use --
instant corruption. The kernel stack must live for the entire lifetime of the
system.

---

## Interrupt Handling Flow

ClaudioOS uses the dual 8259 PIC (Programmable Interrupt Controller) for hardware
interrupt routing. The Local APIC is explicitly disabled because UEFI firmware
enables it and it would conflict with PIC timer delivery.

### PIC Configuration

```
PIC1 (Master)                    PIC2 (Slave)
Port 0x20 (cmd) / 0x21 (data)   Port 0xA0 (cmd) / 0xA1 (data)

ICW1: 0x11  (init, cascade, ICW4 needed)
ICW2: 0x20  (offset = 32)       ICW2: 0x28  (offset = 40)
ICW3: 0x04  (slave on IRQ2)     ICW3: 0x02  (cascade identity = 2)
ICW4: 0x01  (8086 mode)         ICW4: 0x01  (8086 mode)

OCW1 (mask):
  PIC1: 0b1111_1100  -> IRQ0 (timer) + IRQ1 (keyboard) unmasked
  PIC2: 0b1111_1111  -> all masked

Vector mapping:
  IRQ0  (timer)    -> Vector 32    [unmasked]
  IRQ1  (keyboard) -> Vector 33    [unmasked]
  IRQ2  (cascade)  -> (internal)
  IRQ3-7           -> Vectors 35-39 [masked]
  IRQ8-15          -> Vectors 40-47 [masked]
```

### APIC Disable

UEFI firmware enables the Local APIC as part of its boot process. If we initialize
the 8259 PIC without disabling the APIC first, BOTH can deliver timer interrupts on
the same vector, causing a double fault when IRQ0 fires. The fix:

```rust
// kernel/src/interrupts.rs
let mut apic_base_msr = Msr::new(0x1B);
let val = apic_base_msr.read();
apic_base_msr.write(val & !(1 << 11));  // Clear Global Enable bit
```

### Interrupt Flow Diagram

```
Hardware Event (e.g., key press)
    |
    v
PS/2 Controller raises IRQ1
    |
    v
PIC1 delivers Vector 33 to CPU
    |
    v
CPU saves state, looks up IDT[33]
    |
    v
keyboard_handler() [extern "x86-interrupt"]
    |
    |-- 1. Read scancode from port 0x60
    |      (MUST read BEFORE sending EOI to avoid losing data)
    |
    |-- 2. keyboard::push_scancode(scancode)
    |      |-- Lock SCANCODE_QUEUE, push_back
    |      |-- Lock KEYBOARD_WAKER, take + wake
    |      |      |-- waker.wake() -> wake_task(id)
    |      |      |      |-- Lock READY_QUEUE, push task_id
    |
    |-- 3. Send EOI (0x20) to PIC1 via notify_end_of_interrupt()
    |
    v
CPU restores state, returns to interrupted code
    |
    v
Executor sees task_id in READY_QUEUE on next loop iteration
    |
    v
Polls the keyboard consumer future (NextKey)
    |
    v
NextKey drains SCANCODE_QUEUE through pc-keyboard decoder
```

### Timer Handler

The timer handler is intentionally minimal -- just sends EOI. No tick counting,
no log output (log formatting in an ISR would overflow the stack). It writes
directly to port 0x20 without locking the PICS mutex:

```rust
extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    unsafe {
        Port::<u8>::new(0x20).write(0x20);
    }
}
```

### Exception Handlers

| Exception | Vector | Handler | Stack | Notes |
|-----------|--------|---------|-------|-------|
| Breakpoint | 3 | `breakpoint_handler` | Normal | Raw serial write (no log) |
| Page Fault | 14 | `page_fault_handler` | Normal | Reads CR2, logs, halts |
| Double Fault | 8 | `double_fault_handler` | IST[0] (20 KiB) | Logs stack frame, halts |

The double-fault handler runs on IST[0] specifically to handle stack overflow
cases where the page fault handler itself would triple-fault.

---

## Async Executor Architecture

The executor (`kernel/src/executor.rs`) is a cooperative, single-threaded scheduler
where hardware interrupts serve as the sole source of wake-ups.

### Architecture Diagram

```
+------------------+     +-----------------+     +------------------+
|   ISR Handlers   |     |   READY_QUEUE   |     |    Executor      |
|                  |     |   (Mutex<Vec>)  |     |    run() loop    |
|  keyboard_handler| --> | [TaskId(0),     | --> |                  |
|  timer_handler   |     |  TaskId(2), ...]|     | 1. drain queue   |
|  nic_handler     |     +-----------------+     | 2. poll tasks    |
+------------------+           ^                 | 3. hlt if empty  |
                               |                 +------------------+
                               |                        |
                      +--------+--------+               |
                      |   TaskWaker     |               v
                      |   (Arc<Wake>)   |     +------------------+
                      |                 | <-- |    EXECUTOR      |
                      | wake() pushes   |     | (Mutex<Option>)  |
                      | to READY_QUEUE  |     |                  |
                      +-----------------+     | tasks: BTreeMap  |
                                              | waker_cache      |
                                              | next_id: u64     |
                                              +------------------+
```

### Key Design: Two Separate Locks

The executor uses two separate `Mutex`-protected structures to prevent deadlocks
between ISR handlers and the scheduling loop:

1. **`READY_QUEUE`** (`Mutex<Vec<TaskId>>`): Only holds task IDs that need polling.
   Locked briefly by ISR wakers and by the executor's drain step.

2. **`EXECUTOR`** (`Mutex<Option<Executor>>`): Holds the full task map (`BTreeMap`)
   and waker cache. **Never locked by ISR handlers.** Locked by `run()` to
   extract/insert tasks and by `spawn()` to add new tasks.

### Task Lifecycle

```
spawn(future)
    |
    v
[1] Lock EXECUTOR -> assign TaskId(N) -> insert Task into BTreeMap
    |
    v
[2] Lock READY_QUEUE -> push TaskId(N)
    |  (EXECUTOR lock released first to avoid nested lock ordering issues)
    v
[3] Executor drain loop picks up TaskId(N)
    |
    v
[4] Lock EXECUTOR -> REMOVE Task from map (so lock is NOT held during poll)
    |                 (this prevents deadlock when poll() calls spawn())
    v
[5] Create Context from cached Waker -> poll future
    |
    +-- Poll::Pending  -> Lock EXECUTOR -> re-insert Task -> wait for wake
    |
    +-- Poll::Ready(()) -> Lock EXECUTOR -> remove waker cache entry -> done
```

### The HLT Race Condition Fix

When no tasks are ready, the executor must atomically check the queue and halt to
avoid missing work that arrives between the check and the halt:

```rust
// Disable interrupts to prevent this race:
//   check empty -> [interrupt fires, pushes to queue] -> hlt (misses work)
x86_64::instructions::interrupts::disable();
if READY_QUEUE.lock().is_empty() {
    // enable_and_hlt is a single x86 instruction pair (STI; HLT)
    // that atomically enables interrupts and halts. The CPU will
    // service one pending interrupt before halting.
    x86_64::instructions::interrupts::enable_and_hlt();
} else {
    x86_64::instructions::interrupts::enable();
}
```

### YieldNow

The `YieldNow` future (`executor::yield_now()`) provides cooperative multitasking:
- On first poll: wakes itself (re-enqueuing), returns `Pending`
- On second poll: returns `Ready(())`

This allows long-running tasks to voluntarily yield so other ready tasks get CPU time.

### Spawning

`executor::spawn()` can be called after `executor::run()` has initialized the global
state. It allocates a `TaskId`, inserts the future, and pushes the ID to the ready
queue. The EXECUTOR lock is released before touching READY_QUEUE to maintain a
consistent lock ordering.

---

## Crate Dependency Graph

```
                    kernel (binary, #![no_std] #![no_main])
                   /   |    |    \    \     \     \      \
                  v    v    v     v    v     v     v      v
           terminal  net  auth  agent  fs  editor python  rustc-lite
           (active) (act) (act) (act) (stub)(act) -lite   (Cranelift)
              |       |    |     |     |     |     |        |
              |       v    |     v     |     |     |        v
              |   api-client     |     |     |     |   cranelift-*-nostd
              |       |    |     |     |     |     |   (6 forked crates)
              +-------+----+-----+-----+-----+----+
              |                                    |
              v                                    v
         alloc + core                        alloc + core
         (#![no_std])                        (#![no_std])

    Wraith browser crates (WIP):
    wraith-dom  wraith-transport  wraith-render
         |            |                |
         v            v                v
     alloc+core   claudio-net      wraith-dom
```

### External Crate Dependencies by Module

| Module | Key Dependencies |
|--------|-----------------|
| `kernel` | `bootloader_api` 0.11, `x86_64` 0.15, `pc-keyboard` 0.8, `spin` 0.9, `linked_list_allocator` 0.10, `log` 0.4, `noto-sans-mono-bitmap` 0.3, `vte` 0.15 |
| `terminal` | `vte` 0.15, `noto-sans-mono-bitmap` 0.3 |
| `net` | `smoltcp` 0.12, `embedded-tls` 0.17, `embedded-io` 0.6, `rand_core` 0.6 |
| `api-client` | `serde` 1.x (no_std, derive, alloc), `serde_json` 1.x (no_std, alloc) |
| `auth` | (minimal, credential types + device flow prompt) |
| `editor` | (pure no_std + alloc, no external deps) |
| `python-lite` | (pure no_std + alloc, no external deps) |
| `rustc-lite` | `cranelift-codegen`, `cranelift-frontend` (forked no_std) |
| `cranelift-*-nostd` | `hashbrown`, `ahash`, `libm`, `target-lexicon` |
| `wraith-dom` | (pure no_std + alloc, no external deps) |
| `wraith-transport` | `claudio-net` (smoltcp, TLS) |
| `wraith-render` | `wraith-dom` |
| `fs-persist` | `fatfs` 0.3 (no_std, alloc) -- stubbed |

All crates are `#![no_std]` with `extern crate alloc` where heap allocation is needed.
No crate in the dependency tree pulls in `std`. The `tools/image-builder/` is a
host-side binary excluded from the workspace to avoid inheriting the
`x86_64-unknown-none` target. Six Cranelift crates are forked under `crates/` and
patched in via `[patch.crates-io]` in the workspace `Cargo.toml`.

---

## Source File Map

```
J:\baremetal claude\
  kernel/
    src/
      main.rs           Entry point, boot phases, stack switch, panic handler
      gdt.rs            GDT + TSS with IST[0] (double fault) and IST[1] (timer)
      interrupts.rs     IDT, APIC disable, 8259 PIC ICW sequence, ISR handlers
      memory.rs         BootInfoFrameAllocator, heap page mapping, OffsetPageTable
      executor.rs       Cooperative async executor (READY_QUEUE + EXECUTOR split)
      keyboard.rs       Async ScancodeStream, VecDeque queue, Waker integration
      serial.rs         16550 UART driver, serial_print!/force_println! macros
      framebuffer.rs    GOP framebuffer init (page table walk), put_pixel
      pci.rs            PCI config space scan, bus mastering, device recognition
      logger.rs         log crate backend -> serial output
    Cargo.toml
  crates/
    terminal/src/
      lib.rs            DrawTarget trait, LayoutNode enum, Viewport, SplitDirection
      render.rs         noto-sans-mono-bitmap glyph rendering, Color palette
      pane.rs           Pane (cell grid, VTE parser, cursor, SGR, scroll)
      layout.rs         Binary split tree, focus navigation, viewport recomputation
    net/src/
      lib.rs            NicDriver trait, PciDeviceInfo, high-level init + DHCP loop
      nic.rs            VirtIO-net legacy PCI driver (virtqueues, DMA, page walk)
      stack.rs          smoltcp Device adapter, NetworkStack, DHCP event handling
      dns.rs            DNS resolver via smoltcp socket (poll-loop style)
      tls.rs            TLS 1.3 stream (embedded-tls, AES-128-GCM-SHA256)
      http.rs           HTTP/1.1 request builder, response parser, chunked, SSE
    api-client/src/
      lib.rs            AnthropicClient struct, auth header selection
      messages.rs       Messages API types + SSE streaming
      streaming.rs      SSE stream consumer
      tools.rs          Tool use protocol
    auth/src/
      lib.rs            Credentials enum, DeviceFlowPrompt
    agent/src/
      lib.rs            Agent session lifecycle, tool loop (max 20 rounds)
    editor/src/
      lib.rs            Nano-like text editor (~400 lines, 11 tests)
    python-lite/src/
      lib.rs            Module root + execute_python API
      tokenizer.rs      Python tokenizer
      parser.rs         Python AST parser
      eval.rs           Python evaluator (vars, loops, functions)
    rustc-lite/src/     Bare-metal Rust compiler via Cranelift
    cranelift-codegen-nostd/     Forked cranelift-codegen for no_std
    cranelift-frontend-nostd/    Forked cranelift-frontend for no_std
    cranelift-codegen-shared-nostd/  Forked cranelift-codegen-shared for no_std
    cranelift-control-nostd/     Forked cranelift-control for no_std
    rustc-hash-nostd/            Forked rustc-hash for no_std
    arbitrary-stub/              no_std stub for arbitrary crate
    wraith-dom/src/
      lib.rs            Module root + re-exports
      parser.rs         HTML tokenizer + tree builder (619 lines)
      selector.rs       CSS selector matching (354 lines)
      forms.rs          Form detection + login heuristics (367 lines)
      text.rs           Text extraction utilities (237 lines)
    wraith-transport/src/
      lib.rs            HTTP/HTTPS over smoltcp (572 lines)
    wraith-render/src/
      lib.rs            HTML -> text-mode character grid (1,221 lines)
    fs-persist/         FAT32 persistence (stubbed)
  tools/
    image-builder/src/
      main.rs           Host-side UEFI + BIOS disk image builder
    auth-relay.py       HTTP proxy for API key management
    build-server.py     Host-side Rust compilation service for agents
    tls-proxy.py        TLS termination proxy (dev/debug)
    tls-bridge.py       TLS bridge utility
  x86_64-claudio.json  Custom target with SSE+AES-NI (no soft-float)
  Cargo.toml            Workspace root with shared deps + [patch.crates-io]
  .cargo/config.toml    Default target x86_64-unknown-none, build-std, QEMU runner
  rust-toolchain.toml   Nightly Rust + components (rust-src, llvm-tools-preview)
  CLAUDE.md             Project design document and build instructions
  README.md             User-facing README with status and build guide
  HANDOFF.md            Complete session summary and status
  docs/                 This documentation directory
```
