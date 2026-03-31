# Building ClaudioOS

Comprehensive guide for building, running, and debugging ClaudioOS.

---

## Table of Contents

- [Prerequisites](#prerequisites)
- [Two-Step Build Process](#two-step-build-process)
- [Running in QEMU](#running-in-qemu)
- [Troubleshooting](#troubleshooting)
- [Environment Variables](#environment-variables)
- [Project File Layout](#project-file-layout)

---

## Prerequisites

### Rust Toolchain

ClaudioOS requires nightly Rust. The `rust-toolchain.toml` in the repository root
automatically installs the correct toolchain on first build:

```toml
[toolchain]
channel = "nightly"
components = ["rust-src", "rustfmt", "clippy", "llvm-tools-preview"]
targets = ["x86_64-unknown-none"]
```

- **`rust-src`**: Required for building core/alloc for the freestanding target
- **`llvm-tools-preview`**: Required by the bootloader's image builder for `llvm-objcopy`
- **`x86_64-unknown-none`**: The bare-metal target (no OS, no libc)

You do not need to manually run `rustup target add` -- it happens automatically.

### Platform-Specific Requirements

#### Windows

- **MSVC Build Tools**: The image builder (`tools/image-builder/`) is a host-side
  Rust binary that uses the MSVC linker. You need "Desktop development with C++"
  from Visual Studio, or at minimum the MSVC Build Tools standalone installer.
- **QEMU for Windows**: Download from https://www.qemu.org/download/#windows.
  Add the QEMU install directory to your PATH.
- **OVMF** (for UEFI boot): QEMU for Windows may not include OVMF. Download
  OVMF firmware from https://retrage.github.io/edk2-nightly/ or build from source.
  BIOS boot works without OVMF.

#### Linux

```bash
# Debian/Ubuntu
sudo apt install qemu-system-x86 ovmf

# Arch Linux
sudo pacman -S qemu-system-x86 edk2-ovmf

# Fedora
sudo dnf install qemu-system-x86 edk2-ovmf
```

The Rust toolchain "just works" on Linux -- no special setup needed beyond having
a C linker available (for the image builder).

#### macOS

```bash
brew install qemu
# OVMF firmware is included with Homebrew's QEMU package
```

---

## Two-Step Build Process

Building ClaudioOS is a two-step process: compile the kernel, then wrap it in a
bootable disk image.

### Step 1: Compile the Kernel

```bash
cargo build
```

This compiles the `claudio-os` kernel crate for `x86_64-unknown-none` (the target
is set in `.cargo/config.toml`). The output is a bare ELF binary:

```
target/x86_64-unknown-none/debug/claudio-os
```

This ELF is NOT bootable on its own -- it needs to be wrapped with the bootloader.

For a release build:

```bash
cargo build --release
```

### Step 2: Create Bootable Disk Images

```bash
cargo run --manifest-path tools/image-builder/Cargo.toml -- \
    target/x86_64-unknown-none/debug/claudio-os
```

The image builder uses the `bootloader` crate (v0.11) to produce two disk images:

| Image | Path | Boot method |
|-------|------|-------------|
| BIOS | `target/x86_64-unknown-none/debug/claudio-os-bios.img` | Legacy BIOS |
| UEFI | `target/x86_64-unknown-none/debug/claudio-os-uefi.img` | UEFI firmware |

For release builds, replace `debug` with `release` in the paths.

---

## Running in QEMU

### BIOS Boot (Simplest)

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

### With Networking (Phase 2+)

Add VirtIO-net device with SLIRP user-mode networking:

```bash
qemu-system-x86_64 \
    -bios /usr/share/OVMF/OVMF_CODE.fd \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-uefi.img \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0 \
    -serial stdio \
    -m 512M \
    -smp 4
```

SLIRP networking provides:
- DHCP: Guest gets 10.0.2.x automatically
- DNS: Available at 10.0.2.3
- NAT: Outbound TCP/UDP works (HTTPS to api.anthropic.com)
- No host configuration needed (no bridges, no tap devices)

### With Port Forwarding

To forward a port from host to guest (for debugging):

```bash
-netdev user,id=net0,hostfwd=tcp::5555-:5555
```

### Useful QEMU Flags

| Flag | Purpose |
|------|---------|
| `-serial stdio` | Route serial port to terminal (see log output) |
| `-m 512M` | Give the VM 512 MiB of RAM |
| `-smp 4` | 4 CPU cores (unused by Phase 1 but needed for SMP later) |
| `-d int` | Log all interrupts to stderr (very verbose, for debugging) |
| `-no-reboot` | Don't reboot on triple fault -- stop instead |
| `-no-shutdown` | Keep VM alive after power off |
| `-s -S` | Start GDB server on port 1234, wait for connection |
| `-monitor stdio` | QEMU monitor instead of serial (use `-serial file:serial.log`) |

### GDB Debugging

```bash
# Terminal 1: Start QEMU with GDB server
qemu-system-x86_64 \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-bios.img \
    -serial stdio \
    -m 512M \
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

The image builder (`tools/image-builder/`) is a host-side binary that requires the
MSVC linker. If you see errors like:

```
error: linker `link.exe` not found
```

or:

```
LINK : fatal error LNK1104: cannot open file 'kernel32.lib'
```

**Fix**: Ensure "Desktop development with C++" is installed in Visual Studio. For
partial installs, set the `LIB` environment variable:

```powershell
$env:LIB = "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.39.33519\lib\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.22621.0\ucrt\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.22621.0\um\x64"
```

Adjust version numbers to match your installation.

### Windows: sccache Issues

If you use `sccache` as a Rust compiler wrapper and see compilation failures on the
kernel crate, try building without it:

```powershell
$env:RUSTC_WRAPPER = ""
cargo build
```

The `x86_64-unknown-none` target with `#![feature(abi_x86_interrupt)]` can confuse
some sccache versions.

### `fatfs` v0.3 Compilation Failure

The `fatfs` crate at v0.3 has known compilation issues in `no_std` environments.
If you see errors from `fatfs`:

```
error[E0433]: failed to resolve: use of undeclared type `std`
```

Ensure the workspace has `fatfs` pinned with `default-features = false`:

```toml
[dependencies]
fatfs = { version = "0.4", default-features = false }
```

Version 0.4 resolves most `no_std` issues.

### `embedded-tls` LLVM Crashes

The `embedded-tls` crate can trigger LLVM codegen crashes when compiled for
`x86_64-unknown-none`:

```
error: could not compile `embedded-tls`
Caused by:
  process didn't exit successfully: `rustc ...` (signal: 11, SIGSEGV)
```

**Workarounds**:
- Try `opt-level = 1` instead of the default for debug builds
- Try `codegen-units = 1` in `[profile.dev]`
- As a last resort, the TLS implementation may need to be swapped

### Boot Hangs (No Serial Output)

If QEMU starts but no serial output appears:

1. Verify you are using `-serial stdio`
2. Check the disk image path is correct
3. Try BIOS boot instead of UEFI (simpler, fewer things can go wrong)
4. Add `-d int` to see if interrupts are firing
5. Add `-no-reboot` to catch triple faults

### Page Fault During Boot

If you see `[int] PAGE FAULT` in serial output during init:

- Check if the heap pages are being mapped correctly (`memory::init`)
- The framebuffer address from the bootloader may need page table fixup
- Try increasing `HEAP_SIZE` if the allocator is running out of space

---

## Environment Variables

These are **build-time** environment variables, read by `env!()` or `option_env!()`
macros during compilation:

| Variable | Default | Description |
|----------|---------|-------------|
| `CLAUDIO_API_KEY` | (none) | Baked-in Anthropic API key. If set, skips OAuth device flow at boot. For development only -- do not bake production keys. |
| `CLAUDIO_LOG_LEVEL` | `info` | Log level filter: `trace`, `debug`, `info`, `warn`, `error`. Currently not wired up (all levels enabled). |
| `CLAUDIO_QEMU` | `0` | Set to `1` to compile with QEMU-friendly defaults (VirtIO device assumptions, SLIRP network defaults). |

Example:

```bash
# Build with a dev API key baked in
CLAUDIO_API_KEY=sk-ant-api03-xxx cargo build
```

---

## Project File Layout

```
J:\baremetal claude\
|-- CLAUDE.md                  Project instructions and design doc
|-- README.md                  User-facing README
|-- Cargo.toml                 Workspace root
|-- rust-toolchain.toml        Nightly Rust pinning
|-- .cargo/
|   `-- config.toml            Sets default target to x86_64-unknown-none
|
|-- kernel/
|   |-- Cargo.toml             Binary crate: claudio-os
|   `-- src/
|       |-- main.rs            Entry point (kernel_main, panic handler)
|       |-- gdt.rs             GDT + TSS
|       |-- memory.rs          Physical frame allocator + heap
|       |-- interrupts.rs      IDT + PIC + ISR handlers
|       |-- keyboard.rs        Async PS/2 keyboard input
|       |-- serial.rs          16550 UART + macros
|       |-- logger.rs          log crate backend
|       |-- framebuffer.rs     GOP framebuffer state + put_pixel
|       |-- pci.rs             PCI config space enumeration
|       `-- executor.rs        Async task executor
|
|-- crates/
|   |-- terminal/              Framebuffer terminal renderer
|   |   `-- src/
|   |       |-- lib.rs         DrawTarget trait, LayoutNode types
|   |       |-- render.rs      Font rendering (noto-sans-mono-bitmap)
|   |       |-- pane.rs        Terminal pane (cell grid, VTE, cursor)
|   |       `-- layout.rs      Binary split tree, focus management
|   |
|   |-- net/                   Networking stack
|   |   `-- src/
|   |       |-- lib.rs         NicDriver trait, high-level init
|   |       |-- nic.rs         VirtIO-net driver
|   |       |-- stack.rs       smoltcp interface wrapper
|   |       |-- tls.rs         TLS stream
|   |       |-- dns.rs         DNS resolver
|   |       `-- http.rs        HTTP/1.1 client + SSE parser
|   |
|   |-- api-client/            Anthropic Messages API client
|   |   `-- src/
|   |       |-- lib.rs         AnthropicClient struct
|   |       |-- messages.rs    Message types (planned)
|   |       |-- streaming.rs   SSE stream consumer (planned)
|   |       `-- tools.rs       Tool use protocol (planned)
|   |
|   |-- auth/                  OAuth 2.0 device flow
|   |   `-- src/
|   |       `-- lib.rs         Credentials, DeviceFlowPrompt
|   |
|   |-- agent/                 Agent session lifecycle (planned)
|   |-- fs-persist/            FAT32 persistence (planned)
|
|-- tools/
|   `-- image-builder/         Host-side disk image builder
|       |-- Cargo.toml
|       `-- src/main.rs
|
`-- docs/                      This documentation
```
