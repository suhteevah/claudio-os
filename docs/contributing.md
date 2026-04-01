# Contributing to ClaudioOS

Development workflow, code conventions, testing, and PR guidelines for ClaudioOS.

**Owner:** Matt Gates (suhteevah) -- Ridge Cell Repair LLC
**License:** AGPL-3.0-or-later

---

## Table of Contents

- [Development Workflow](#development-workflow)
- [Code Conventions](#code-conventions)
- [Testing](#testing)
- [Debugging Techniques](#debugging-techniques)
- [PR Guidelines](#pr-guidelines)
- [Common Pitfalls](#common-pitfalls)
- [Phase Development Model](#phase-development-model)

---

## Development Workflow

### Setup

1. Clone the repository
2. Ensure nightly Rust is available (auto-installed via `rust-toolchain.toml`)
3. Install QEMU and OVMF for your platform (see `docs/building.md`)
4. Run the build:

```bash
cargo build
cargo run --manifest-path tools/image-builder/Cargo.toml -- \
    target/x86_64-unknown-none/debug/claudio-os
```

### Development Cycle

The typical edit-build-test cycle for kernel development:

```bash
# 1. Edit source files
# 2. Build
cargo build

# 3. Create disk image (only needed if kernel binary changed)
cargo run --manifest-path tools/image-builder/Cargo.toml -- \
    target/x86_64-unknown-none/debug/claudio-os

# 4. Test in QEMU
qemu-system-x86_64 \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-bios.img \
    -serial stdio -m 512M -no-reboot

# 5. Watch serial output for log messages, errors, panics
```

**Tip**: Use `-no-reboot` during development so triple faults are visible instead
of causing an instant reboot.

### Branch Strategy

- `main` -- stable, always boots in QEMU
- Feature branches -- one per significant change
- Name branches descriptively: `phase2/virtio-net-init`, `fix/gdt-data-segment`,
  `refactor/executor-lock-ordering`

---

## Code Conventions

### no_std Requirements

All crates in the workspace are `#![no_std]`. This means:

- No `std::*` types or traits -- use `core::*` and `alloc::*` instead
- No `println!` -- use `log::info!()`, `serial_print!()`, or `serial_println!()`
- No `std::sync::Mutex` -- use `spin::Mutex`
- No `std::thread` -- use the async executor
- No `std::io` -- implement I/O traits manually
- No `std::fs` -- use `fatfs` crate for FAT32

### Heap Usage

`extern crate alloc;` is available in all crates after Phase 2 (heap init). Heap
types used throughout:

- `alloc::boxed::Box` -- heap allocation
- `alloc::vec::Vec` -- growable arrays
- `alloc::string::String` -- owned strings
- `alloc::collections::BTreeMap` -- ordered map (executor task storage)
- `alloc::sync::Arc` -- reference-counted pointers (wakers)

### Logging

Use the `log` crate macros everywhere. The kernel provides a serial log sink.

```rust
log::trace!("[module] detailed debug info: {:#x}", value);
log::debug!("[module] debug message");
log::info!("[module] important state change");
log::warn!("[module] unexpected but handled condition");
log::error!("[module] error that affects functionality");
```

**Convention**: Prefix log messages with `[module_name]` for easy grep filtering:

```
[boot] ClaudioOS v0.1.0 starting
[gdt] IST[0] (double fault) top: 0x1234
[int] IDT at 0x5678
[mem] heap initialized at 0x4444_4444_0000, size 1024 KiB
[pci] 00:03.0 vendor=0x1af4 device=0x1000 class=0x02/0x00
[kbd] keyboard decoder initialized
[exec] executor started, main task id=TaskId(0)
```

**Verbose logging**: Log every network event, every API call, every auth state
change. In a bare-metal system, serial logs are the primary debugging tool.

### Interrupt Safety

When holding a lock, disable interrupts to prevent deadlock:

```rust
// GOOD: Interrupts disabled while holding lock
x86_64::instructions::interrupts::without_interrupts(|| {
    SOME_LOCK.lock().do_something();
});

// BAD: ISR fires while lock is held -> tries to acquire same lock -> deadlock
let guard = SOME_LOCK.lock();
// ... interrupt fires here ...
```

The serial port's `_print()` function already wraps its lock in
`without_interrupts()`. Any new shared state accessed from both ISR and non-ISR
contexts must follow this pattern.

### Shared State

Use `spin::Mutex` for all shared mutable state. `spin::Lazy` for lazy
initialization (replaces `lazy_static!`):

```rust
use spin::{Lazy, Mutex};

static MY_STATE: Mutex<MyType> = Mutex::new(MyType::new());
static LAZY_INIT: Lazy<ExpensiveType> = Lazy::new(|| {
    ExpensiveType::compute()
});
```

### Panic Behavior

Both `[profile.dev]` and `[profile.release]` set `panic = "abort"`. There is no
unwinding -- panics immediately invoke the panic handler which prints to serial
and halts. This means:

- No `catch_unwind`
- No destructors run on panic (RAII cleanup does not happen)
- Locks held at panic time stay locked (hence `force_unlock()` in panic handler)

### Unsafe Code

Unsafe code is necessary for hardware interaction but should be:
1. Contained in small, well-documented functions
2. Marked with `// SAFETY:` comments explaining why it is sound
3. Never used for convenience -- only when the operation genuinely requires it

Common legitimate uses of unsafe in ClaudioOS:
- Port I/O (`Port::read()`, `Port::write()`)
- Page table manipulation
- Inline assembly (stack switch)
- Raw pointer dereference (framebuffer, DMA buffers)
- `static mut` for IST stacks (must be static, must be mutable for init)

---

## Testing

### Host-Side Unit Tests

Pure logic crates can be tested on the host with standard `cargo test`:

```bash
# Test the HTTP/SSE parser
cd crates/net
cargo test

# Test the terminal pane logic (if test feature is added)
cd crates/terminal
cargo test
```

The `http.rs` module in `crates/net` includes tests for:
- Request serialization
- Response parsing (complete and incomplete)
- Chunked transfer encoding
- SSE event parsing
- Case-insensitive header lookup

### QEMU Integration Testing

There is no automated test harness for kernel-level testing (yet). Manual testing
in QEMU:

1. Boot the kernel with `-serial stdio`
2. Watch serial output for expected log messages
3. Type on the keyboard -- characters should echo to serial
4. Verify no panics, page faults, or double faults
5. Use Ctrl+A, X to exit QEMU

### Expected Boot Output (Phase 1)

A successful Phase 1 boot should produce serial output similar to:

```
[claudio] kernel_main entered
[ INFO] [boot] ClaudioOS v0.1.0 starting
[ INFO] [boot] bootloader handed off control
[ INFO] [gdt] IST[0] (double fault) top: 0x...
[ INFO] [gdt] IST[1] (timer)        top: 0x...
[ INFO] [boot] GDT initialized with TSS
[ INFO] [mem] heap initialized at 0x4444_4444_0000, size 1024 KiB
[ INFO] [boot] heap allocator initialized
[ INFO] [int] IDT at 0x...
[ INFO] [boot] IDT loaded, PIC initialized (interrupts still disabled)
[ INFO] [kbd] keyboard decoder initialized
[ INFO] [boot] framebuffer: <W>x<H> stride=<S> bpp=<F>
[ INFO] [boot] clearing framebuffer...
[ INFO] [boot] framebuffer initialized
[ INFO] [boot] starting PCI enumeration...
[ INFO] [pci] scanning bus 0...
[ INFO] [pci] 00:XX.0 vendor=... device=... class=... bar0=... irq=...
...
[ INFO] [pci] scan complete, N devices found
[ INFO] [boot] PCI enumeration complete
[ INFO] [boot] allocating new kernel stack on heap...
[ INFO] [boot] new stack top: 0x...
[ INFO] [boot] running on new stack!
[ INFO] [boot] enabling interrupts and starting async executor
[ INFO] [main] async runtime started
[ INFO] [main] ClaudioOS Phase 1 -- Boot to Terminal
[ INFO] [main] keyboard input active, type away!
```

After this, typing on the keyboard should echo characters to serial.

### Adding Tests to a Crate

For host-testable logic, add `#[cfg(test)]` modules:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_my_function() {
        assert_eq!(my_function(42), expected_result);
    }
}
```

These tests run on the host (not bare-metal) so they can use `std` features like
`assert_eq!`, `#[should_panic]`, etc.

---

## Debugging Techniques

### Serial Output

The primary debugging tool. Every important state change should be logged.

For ISR debugging where `log::*` is too stack-heavy, write directly to serial:

```rust
unsafe {
    let mut port = Port::<u8>::new(0x3F8);
    for &b in b"debug message\r\n" {
        port.write(b);
    }
}
```

### QEMU Interrupt Tracing

```bash
qemu-system-x86_64 ... -d int 2>interrupts.log
```

This logs every interrupt delivered to the CPU, including vector number and
register state. Very verbose but invaluable for debugging interrupt issues.

### GDB

```bash
# Start QEMU paused with GDB server
qemu-system-x86_64 ... -s -S

# In another terminal
gdb target/x86_64-unknown-none/debug/claudio-os
(gdb) target remote :1234
(gdb) break kernel_main
(gdb) continue
(gdb) info registers
(gdb) x/20x $rsp    # examine stack
```

### Common Debug Patterns

**"Did we reach this point?"** -- Add a raw serial write:
```rust
unsafe { Port::<u8>::new(0x3F8).write(b'X'); }
```

**"What is this value?"** -- Use `log::info!` with `{:#x}` for hex:
```rust
log::info!("value = {:#x}", some_address);
```

**"Why did we page fault?"** -- The page fault handler prints CR2 (the faulting
address) and the error code. Check if the address is:
- Near 0 -> null pointer dereference
- Near the stack region -> stack overflow
- In the heap region -> heap corruption or use-after-free
- In the framebuffer region -> bad framebuffer address mapping

---

## PR Guidelines

### Before Submitting

1. **Builds cleanly**: `cargo build` with no errors or warnings
2. **Boots in QEMU**: Test with both BIOS and UEFI boot if possible
3. **No regressions**: Keyboard echo still works, no new panics
4. **Tests pass**: `cargo test` on any modified crates with test modules
5. **Formatted**: `cargo fmt` (if `rustfmt` is available for nightly)
6. **Clippy clean**: `cargo clippy` (where possible -- some lints conflict with
   `no_std` or `abi_x86_interrupt`)

### PR Description

Include:
- **What**: Brief description of the change
- **Why**: Motivation (bug fix, new feature, refactor)
- **How**: Technical approach, especially for non-obvious changes
- **Testing**: How you verified it works (QEMU output, test results)
- **Screenshots**: Serial output showing the change working (if applicable)

### Commit Messages

Follow conventional commit style:
- `feat: add VirtIO-net RX buffer recycling`
- `fix: load data segment in GDT init to fix interrupt SS`
- `refactor: split executor into separate ready queue lock`
- `docs: add kernel-internals documentation`

---

## Common Pitfalls

### 1. Stack Overflow in ISR Handlers

ISR handlers run on the interrupted code's stack (unless IST is configured).
Keep handlers minimal:
- No `log::*` calls (formatting allocates stack space)
- No complex data structure operations
- Send EOI, push to queue, wake waker, return

### 2. Deadlock from Lock Ordering

If code holds lock A and tries to acquire lock B, but an ISR holds lock B and
tries to acquire lock A -> deadlock. Solutions:
- Disable interrupts while holding locks (`without_interrupts`)
- Keep ISR locks separate from non-ISR locks (see executor design)
- Never hold two locks simultaneously if possible

### 3. Forgetting EOI

If an interrupt handler returns without sending End-of-Interrupt (EOI) to the
PIC, no further interrupts of that priority or lower will be delivered. The system
appears to hang.

### 4. Physical vs Virtual Addresses

DMA devices (VirtIO) need **physical** addresses. Heap allocations are at
**virtual** addresses. The simple `virt - phys_mem_offset` formula only works for
addresses in the physical memory mapping region, NOT for heap addresses
(0x4444_4444_0000+). Use page table walks for heap-to-physical translation.

### 5. Bootloader Framebuffer Not Writable

The bootloader maps the GOP framebuffer at a virtual address, but this mapping
may lack the WRITABLE flag. Always access the framebuffer through the physical
memory offset mapping after translating the address via page table walk.

### 6. `mem::forget` for Long-Lived Allocations

The heap stack (4 MiB Vec) must be leaked with `mem::forget()` because it must
never be freed. If the Vec's destructor ran, the active stack would be deallocated.
Same pattern applies to any allocation that must outlive normal Rust ownership.

---

## Phase Development Model

ClaudioOS is developed in sequential phases. Each phase builds on the previous:

| Phase | Focus | Status |
|-------|-------|--------|
| 1 | Boot to terminal (kernel, GDT, heap, interrupts, keyboard, framebuffer, PCI) | COMPLETE |
| 2 | Networking + TLS (VirtIO-net, smoltcp, DHCP, DNS, TLS 1.3) | COMPLETE |
| 3 | API client + Auth (Messages API, SSE streaming, auth relay) | COMPLETE |
| 4 | Multi-agent dashboard (split panes, agent sessions, Ctrl+B keybindings) | COMPLETE |
| 5 | Development environment (editor, Python interpreter, Rust compiler) | COMPLETE |
| 6 | Self-hosting foundation (Cranelift no_std fork, rustc-lite) | COMPLETE |
| 7 | Wraith browser integration (HTML parser, HTTP transport, text renderer) | WIP |
| 8 | Real hardware (physical NICs, USB keyboard, encryption, USB boot) | Future |

### All Core Crates Are Active

All workspace members in `Cargo.toml` are uncommented and active. The only stubbed
crate is `fs-persist` (FAT32 persistence). Six Cranelift crates are forked under
`crates/` and patched in via `[patch.crates-io]`.

### Next Steps

Priority items for continued development:
1. Wire wraith browser crates into kernel for OAuth page rendering
2. Implement FAT32 persistence for tokens and agent state
3. Add status bar with token usage and network status
4. Test on physical hardware (Arch Linux box, HP Victus laptop)
5. Add e1000/I219-V NIC driver for real Intel NICs

Each phase follows this pattern: write the crate code, test in isolation where
possible, then integrate into the kernel boot sequence.
