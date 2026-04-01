# Building ClaudioOS

Comprehensive guide for building, running, and debugging ClaudioOS on Windows,
Linux, and macOS.

---

## Table of Contents

- [Prerequisites](#prerequisites)
- [Two-Step Build Process](#two-step-build-process)
- [Running in QEMU](#running-in-qemu)
- [Troubleshooting](#troubleshooting)
- [The Stack Overflow Fix](#the-stack-overflow-fix)
- [Environment Variables](#environment-variables)
- [Project File Layout](#project-file-layout)

---

## Prerequisites

### Rust Toolchain

ClaudioOS requires **nightly Rust**. The `rust-toolchain.toml` in the repository root
automatically installs the correct toolchain on first build:

```toml
[toolchain]
channel = "nightly"
components = ["rust-src", "rustfmt", "clippy", "llvm-tools-preview"]
targets = ["x86_64-unknown-none"]
```

- **`rust-src`**: Required by `-Zbuild-std` for building `core` and `alloc` from
  source for the freestanding target
- **`llvm-tools-preview`**: Required by the bootloader's image builder for
  `llvm-objcopy` (converts ELF to raw binary)
- **`x86_64-unknown-none`**: The bare-metal target (no OS, no libc, no std)

You do **not** need to manually run `rustup target add` -- it happens automatically.

### Build Configuration

The `.cargo/config.toml` sets important build parameters:

```toml
[build]
target = "x86_64-unknown-none"

[unstable]
build-std = ["core", "alloc"]
build-std-features = ["compiler-builtins-mem"]
```

- **`target = "x86_64-unknown-none"`**: All `cargo build` commands default to the
  bare-metal target. The image builder is excluded from the workspace to avoid this.
- **`build-std = ["core", "alloc"]`**: Builds the standard library from source with
  our target's settings. Required because `x86_64-unknown-none` has no pre-built
  standard library.
- **`compiler-builtins-mem`**: Provides `memcpy`, `memset`, `memcmp` implementations
  that would normally come from libc.

### Platform-Specific Requirements

#### Windows

- **MSVC Build Tools**: The image builder (`tools/image-builder/`) is a host-side
  binary that links with the MSVC linker. Install "Desktop development with C++"
  from Visual Studio, or the MSVC Build Tools standalone installer.
- **QEMU for Windows**: Download from https://www.qemu.org/download/#windows.
  Add the QEMU install directory to your PATH.
- **OVMF** (for UEFI boot): QEMU for Windows may not include OVMF firmware.
  Download from https://retrage.github.io/edk2-nightly/ or use BIOS boot instead.

#### Linux

```bash
# Debian/Ubuntu
sudo apt install qemu-system-x86 ovmf

# Arch Linux
sudo pacman -S qemu-system-x86 edk2-ovmf

# Fedora
sudo dnf install qemu-system-x86 edk2-ovmf
```

The Rust toolchain "just works" on Linux -- no special setup beyond having a C
linker available (for the image builder, which is a host-side binary).

#### macOS

```bash
brew install qemu
# OVMF firmware is included with Homebrew's QEMU package
```

---

## Two-Step Build Process

Building ClaudioOS is a two-step process because the bootloader crate's disk image
builder is a separate host-side tool.

### Step 1: Compile the Kernel

```bash
cargo build
```

This compiles the `claudio-os` kernel crate (and its dependencies including
`claudio-terminal`) for the `x86_64-unknown-none` target. Output:

```
target/x86_64-unknown-none/debug/claudio-os
```

This is a bare ELF binary -- **not bootable on its own**. It needs to be wrapped
with the bootloader.

For release builds (smaller, optimized, LTO enabled):

```bash
cargo build --release
```

Release profile settings from `Cargo.toml`:
```toml
[profile.release]
panic = "abort"
lto = true
codegen-units = 1
opt-level = "z"    # Optimize for size
strip = true
```

### Step 2: Create Bootable Disk Images

```bash
cargo run --manifest-path tools/image-builder/Cargo.toml -- \
    target/x86_64-unknown-none/debug/claudio-os
```

The image builder (`tools/image-builder/src/main.rs`) uses the `bootloader` crate
(v0.11) to produce two disk images:

| Image | Path | Boot method |
|-------|------|-------------|
| UEFI | `target/.../claudio-os-uefi.img` | UEFI firmware (OVMF) |
| BIOS | `target/.../claudio-os-bios.img` | Legacy BIOS (simpler) |

The image builder:
1. Uses `bootloader::UefiBoot::new(kernel_path).create_disk_image(&uefi_path)`
2. Uses `bootloader::BiosBoot::new(kernel_path).create_disk_image(&bios_path)`
3. Prints file sizes and QEMU invocation hints

**Why a separate tool?** The image builder runs on the host (`x86_64-pc-windows-msvc`
or `x86_64-unknown-linux-gnu`), not the bare-metal target. It is excluded from the
workspace (`exclude = ["tools/image-builder"]`) to prevent it from inheriting the
`x86_64-unknown-none` build target.

### Quick Build Script

For repeated builds during development:

```bash
# Build kernel + create images + run QEMU (BIOS mode)
cargo build && \
cargo run --manifest-path tools/image-builder/Cargo.toml -- \
    target/x86_64-unknown-none/debug/claudio-os && \
qemu-system-x86_64 \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-bios.img \
    -serial stdio -m 512M
```

---

## Running in QEMU

### BIOS Boot (Simplest, Recommended for Quick Testing)

No OVMF firmware needed. Works out of the box:

```bash
qemu-system-x86_64 \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-bios.img \
    -serial stdio \
    -m 512M
```

### UEFI Boot

Requires OVMF firmware. Path varies by platform:

```bash
# Linux (Debian/Ubuntu)
qemu-system-x86_64 \
    -bios /usr/share/OVMF/OVMF_CODE.fd \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-uefi.img \
    -serial stdio \
    -m 512M

# Linux (Arch)
qemu-system-x86_64 \
    -bios /usr/share/edk2-ovmf/x64/OVMF_CODE.fd \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-uefi.img \
    -serial stdio \
    -m 512M

# macOS (Homebrew)
qemu-system-x86_64 \
    -bios $(brew --prefix)/share/qemu/edk2-x86_64-code.fd \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-uefi.img \
    -serial stdio \
    -m 512M
```

### With Networking + TLS (Full Stack)

Add VirtIO-net device with SLIRP user-mode networking. **`-cpu Haswell` is required**
for AES-NI instructions used by TLS 1.3:

```bash
qemu-system-x86_64 \
    -bios /usr/share/OVMF/OVMF_CODE.fd \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-uefi.img \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0 \
    -serial stdio \
    -m 512M \
    -smp 4 \
    -cpu Haswell
```

SLIRP networking provides:
- **DHCP**: Guest gets 10.0.2.x automatically
- **DNS**: Available at 10.0.2.3
- **NAT**: Outbound TCP/UDP works (HTTPS to api.anthropic.com)
- **No host configuration**: No bridges, no tap devices, no root required

With port forwarding (host port 5555 -> guest port 5555):

```bash
-netdev user,id=net0,hostfwd=tcp::5555-:5555
```

### Useful QEMU Flags

| Flag | Purpose |
|------|---------|
| `-serial stdio` | Route serial port to terminal (see log output) |
| `-m 512M` | 512 MiB RAM |
| `-smp 4` | 4 CPU cores |
| `-d int` | Log all interrupts to stderr (very verbose) |
| `-no-reboot` | Stop on triple fault instead of rebooting |
| `-no-shutdown` | Keep VM alive after power off |
| `-s -S` | Start GDB server on port 1234, wait for debugger |
| `-monitor stdio` | QEMU monitor (use `-serial file:serial.log` for serial) |
| `-display none` | No graphical window (serial only) |
| `-enable-kvm` | Use KVM acceleration (Linux, much faster) |

### GDB Debugging

```bash
# Terminal 1: Start QEMU with GDB server
qemu-system-x86_64 \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-bios.img \
    -serial stdio -m 512M \
    -s -S

# Terminal 2: Connect GDB
gdb target/x86_64-unknown-none/debug/claudio-os \
    -ex "target remote :1234" \
    -ex "break kernel_main" \
    -ex "continue"
```

---

## Troubleshooting

### Windows: MSVC Linker Errors

The image builder is a host-side binary requiring the MSVC linker. If you see:

```
error: linker `link.exe` not found
```

or:

```
LINK : fatal error LNK1104: cannot open file 'kernel32.lib'
```

**Fix**: Install "Desktop development with C++" in Visual Studio. For partial
installs where the linker is found but libraries are not, set the `LIB` variable:

```powershell
$env:LIB = "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.39.33519\lib\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.22621.0\ucrt\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.22621.0\um\x64"
```

Adjust version numbers to match your installation. Use `vswhere` or check the
Visual Studio install directories to find the correct paths.

### Windows: sccache Issues

If `sccache` is configured as a Rust compiler wrapper and kernel compilation fails:

```powershell
$env:RUSTC_WRAPPER = ""
cargo build
```

The `x86_64-unknown-none` target with `#![feature(abi_x86_interrupt)]` can confuse
some sccache versions.

### `fatfs` v0.3 Compilation Failure

The `fatfs` crate at v0.3 has known compilation issues in `no_std` environments:

```
error[E0433]: failed to resolve: use of undeclared type `std`
```

**Fix**: The workspace pins `fatfs` with `default-features = false`:

```toml
fatfs = { version = "0.3", default-features = false, features = ["alloc"] }
```

If compilation still fails, the `fs-persist` crate (Phase 3) may need to use a
fork or wait for v0.4.

### `embedded-tls` Issues

The `embedded-tls` crate requires specific build configuration for bare-metal:

1. **Custom target required**: The default `x86_64-unknown-none` target uses
   soft-float which conflicts with AES-NI crypto. The project uses a custom target
   `x86_64-claudio.json` with `+sse,+sse2,+aes,+pclmulqdq` features enabled.

2. **QEMU CPU**: TLS will crash with an illegal instruction fault if QEMU uses its
   default CPU model. Always use `-cpu Haswell` or later for AES-NI support.

3. **Aligned buffers**: AES-NI instructions require 16-byte aligned memory. TLS
   buffers must be explicitly aligned in allocations.

### Boot Hangs (No Serial Output)

If QEMU starts but no serial output appears:

1. Verify `-serial stdio` is in the QEMU command
2. Check the disk image path is correct and the file exists
3. Try BIOS boot instead of UEFI (fewer things can go wrong)
4. Add `-d int` to see if the CPU is receiving interrupts
5. Add `-no-reboot` to catch triple faults (VM stops instead of reboot loop)
6. Check that the kernel was built recently (`cargo build` succeeded)

### Page Fault During Boot

If `[int] PAGE FAULT` appears in serial output:

- **During Phase 2**: Heap pages not mapping correctly. Check `memory::init()`.
  Verify `phys_mem_offset` is correct (print it to serial).
- **During Phase 4**: Framebuffer address translation failed. The kernel walks the
  page table to find the framebuffer's physical address. If the bootloader's mapping
  is unusual, this walk can fail.
- **After Phase 6**: Likely the heap stack switch failed. Check that the new stack
  is properly aligned and large enough.

### Double Fault After Enabling Interrupts

This was the most common Phase 1 failure mode. Possible causes:

1. **Missing data segment in GDT**: See the [GDT data segment bug](kernel-internals.md#the-data-segment-bug-critical-phase-1-fix)
   in kernel-internals.md. SS must be loaded with a valid data segment selector.
   This was the hardest Phase 1 bug.
2. **APIC not disabled**: UEFI enables the Local APIC. Both APIC and PIC delivering
   timer interrupts simultaneously causes a double fault. Fix: clear bit 11 of
   `IA32_APIC_BASE` MSR (0x1B) before PIC init.
3. **Stack overflow**: The bootloader's 128 KiB stack is exhausted after init.
   See the next section.

---

## The Stack Overflow Fix

This is documented here because it is a critical build/run issue that manifests as
seemingly random double faults after enabling interrupts.

### Symptom

After all init phases complete and interrupts are enabled, the first timer or
keyboard interrupt causes a double fault. The fault appears to be a stack overflow
(page fault at a guard page address).

### Root Cause

The bootloader provides a 128 KiB kernel stack. During boot, every `log::info!()`
call performs `format_args!()` which pushes large stack frames (the `fmt::Arguments`
type and its formatting machinery). After 6 phases of init with dozens of log calls
plus PCI enumeration logging 30+ devices, the stack is nearly full.

When interrupts are enabled, the ISR pushes an interrupt frame plus any handler
logic onto this nearly-full stack -> overflow -> page fault -> double fault.

### Solution (Two Parts)

**Part 1**: Increase the bootloader stack to 128 KiB (from default ~16 KiB):

```rust
// kernel/src/main.rs
static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.kernel_stack_size = 128 * 1024;
    config
};
```

**Part 2**: Allocate a fresh 4 MiB stack on the heap and switch to it before
enabling interrupts:

```rust
const NEW_STACK_SIZE: usize = 4 * 1024 * 1024;  // 4 MiB
let new_stack = alloc::vec![0u8; NEW_STACK_SIZE];
let new_stack_top = new_stack.as_ptr() as u64 + NEW_STACK_SIZE as u64;
core::mem::forget(new_stack);  // Leak -- stack must never be freed

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

The `mem::forget()` is essential -- if the Vec were dropped, the stack memory would
be freed while still in active use.

---

## Environment Variables

Build-time environment variables read by `env!()` or `option_env!()` during
compilation:

| Variable | Default | Description |
|----------|---------|-------------|
| `CLAUDIO_API_KEY` | (none) | Baked-in Anthropic API key. Skips OAuth device flow at boot. **Dev only** -- do not bake production keys. |
| `CLAUDIO_LOG_LEVEL` | `info` | Log level filter: `trace`, `debug`, `info`, `warn`, `error`. (Not yet wired up -- all levels currently enabled.) |
| `CLAUDIO_QEMU` | `0` | Set to `1` for QEMU-friendly defaults (VirtIO assumptions, SLIRP network defaults). |

Example:

```bash
# Build with a dev API key
CLAUDIO_API_KEY=sk-ant-api03-xxx cargo build

# Windows PowerShell
$env:CLAUDIO_API_KEY = "sk-ant-api03-xxx"
cargo build
```

---

## Project File Layout

```
J:\baremetal claude\
|-- CLAUDE.md                  Project design document and architecture spec
|-- README.md                  User-facing README with status and build guide
|-- HANDOFF.md                 Complete session summary and handoff notes
|-- Cargo.toml                 Workspace root (shared deps + [patch.crates-io])
|-- x86_64-claudio.json        Custom target with SSE+AES-NI for TLS crypto
|-- rust-toolchain.toml        Nightly Rust + components + targets
|-- .cargo/
|   +-- config.toml            Default target, build-std, QEMU runner
|
|-- kernel/
|   |-- Cargo.toml             Binary crate: claudio-os
|   +-- src/
|       |-- main.rs            Entry point, boot phases, stack switch, panic
|       |-- gdt.rs             GDT + TSS (data segment fix, IST stacks)
|       |-- memory.rs          Frame allocator + 16 MiB heap mapping
|       |-- interrupts.rs      IDT + APIC disable + PIC ICW + ISR handlers
|       |-- keyboard.rs        Async PS/2 keyboard (ScancodeStream)
|       |-- serial.rs          16550 UART + serial_print! macros
|       |-- logger.rs          log crate backend -> serial
|       |-- framebuffer.rs     GOP framebuffer (page table walk + put_pixel)
|       |-- pci.rs             PCI config space enumeration
|       +-- executor.rs        Cooperative async task executor
|
|-- crates/
|   |-- terminal/              Split-pane framebuffer terminal renderer
|   |-- net/                   VirtIO-net + smoltcp + TLS 1.3 + HTTP/SSE
|   |-- api-client/            Anthropic Messages API + SSE streaming
|   |-- auth/                  OAuth device flow + credential types
|   |-- agent/                 Agent session lifecycle + tool loop
|   |-- editor/                Nano-like text editor (~400 LOC, 11 tests)
|   |-- python-lite/           Minimal Python interpreter (28 tests)
|   |-- rustc-lite/            Bare-metal Rust compiler via Cranelift
|   |-- cranelift-codegen-nostd/       Forked cranelift-codegen for no_std
|   |-- cranelift-frontend-nostd/      Forked cranelift-frontend for no_std
|   |-- cranelift-codegen-shared-nostd/ Forked shared crate for no_std
|   |-- cranelift-control-nostd/       Forked cranelift-control for no_std
|   |-- rustc-hash-nostd/              Forked rustc-hash for no_std
|   |-- arbitrary-stub/                no_std stub for arbitrary crate
|   |-- wraith-dom/            no_std HTML parser + CSS selectors (WIP)
|   |-- wraith-transport/      HTTP/HTTPS over smoltcp (WIP)
|   |-- wraith-render/         HTML -> text-mode character grid (WIP)
|   +-- fs-persist/            FAT32 persistence (stubbed)
|
|-- tools/
|   |-- image-builder/         Host-side UEFI/BIOS disk image builder
|   |-- auth-relay.py          HTTP proxy for API key management
|   |-- build-server.py        Host-side Rust compilation service
|   |-- tls-proxy.py           TLS termination proxy (dev/debug)
|   +-- tls-bridge.py          TLS bridge utility
|
+-- docs/                      Documentation
    |-- architecture.md        System overview, boot sequence, memory map
    |-- kernel-internals.md    GDT, memory, PIC, serial, PCI
    |-- terminal.md            Font rendering, VTE, split panes, colors
    |-- networking.md          VirtIO-net, smoltcp, DNS, TLS 1.3, HTTP
    |-- api-protocol.md        Messages API, SSE, OAuth, tokens, tools
    |-- building.md            This file
    |-- contributing.md        Dev workflow, conventions, testing
    |-- WRAITH-BAREMETAL-PORT.md    Wraith browser port specification
    +-- WRAITH-CRATES-HANDOFF.md    Wraith crates handoff notes
```
