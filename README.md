# ClaudioOS

A bare-metal Rust operating system purpose-built for running multiple AI coding agents (Anthropic Claude) simultaneously. No Linux kernel, no POSIX, no JavaScript runtime -- just Rust, UEFI, and direct HTTPS to `api.anthropic.com`.

ClaudioOS boots your machine into a split-pane terminal dashboard where each pane is an independent Claude agent session. The entire stack -- from hardware interrupts to TLS handshakes to SSE streaming -- is a single-address-space async Rust application.


## Architecture

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
|              Async Executor (interrupt-driven)        |
+-----------------------------------------------------+
|     Net (smoltcp + TLS)    |    FS (FAT32 persist)  |
+----------------------------+------------------------+
|   NIC Driver (virtio-net / e1000)  |  PS/2 Keyboard |
+------------------------------------+----------------+
|              x86_64 Kernel Core                      |
|   (paging, heap, GDT/IDT, interrupts, PCI)          |
+-----------------------------------------------------+
|              UEFI Boot (bootloader crate)             |
+-----------------------------------------------------+
```

## Current Status: Phase 1 -- Boot to Terminal

Phase 1 compiles and boots in QEMU. The following subsystems are implemented:

- **Kernel core** -- GDT, IDT, dual-PIC (8259) initialization, heap allocator with physical page mapping (`linked_list_allocator`)
- **Async executor** -- interrupt-driven waker system; hardware interrupts wake futures, `hlt` when idle
- **PS/2 keyboard** -- async `ScancodeStream` via IRQ1, decoded with `pc-keyboard`
- **Terminal renderer** -- GOP framebuffer output with `noto-sans-mono-bitmap` bitmap fonts, ANSI escape sequence parsing via `vte`, split-pane layout engine (in `crates/terminal/`)
- **Serial output** -- 16550 UART on port 0x3F8, used for `log` crate backend
- **PCI bus enumeration** -- brute-force scan of all buses/devices/functions

Phases 2-5 (networking, TLS, API client, multi-agent dashboard, real hardware) are stubbed but not yet active.

## Crate Structure

| Crate | Path | Status |
|-------|------|--------|
| `claudio-os` (kernel) | `kernel/` | Active -- `#![no_std]` `#![no_main]` binary entry point |
| `claudio-terminal` | `crates/terminal/` | Active -- framebuffer renderer, split panes, ANSI |
| `claudio-net` | `crates/net/` | Stubbed -- smoltcp + TLS |
| `claudio-api` | `crates/api-client/` | Stubbed -- Anthropic Messages API |
| `claudio-auth` | `crates/auth/` | Stubbed -- OAuth device flow |
| `claudio-agent` | `crates/agent/` | Stubbed -- agent session lifecycle |
| `claudio-fs` | `crates/fs-persist/` | Stubbed -- FAT32 persistence |
| `claudio-image-builder` | `tools/image-builder/` | Active -- host-side disk image builder |

## Building

### Prerequisites

- **Nightly Rust** with `x86_64-unknown-none` target (pinned via `rust-toolchain.toml`)
- **QEMU** + **OVMF** for testing
- **Windows only:** MSVC linker is required for build scripts (the image builder runs on the host). If you have a partial Visual Studio install, you may need to set the `LIB` environment variable to point to the MSVC and Windows SDK lib directories.
- **Linux:** standard Rust toolchain just works; install `qemu-system-x86` and `ovmf` from your package manager.

The `rust-toolchain.toml` will auto-install the correct nightly and components on first build:

```toml
[toolchain]
channel = "nightly"
components = ["rust-src", "rustfmt", "clippy", "llvm-tools-preview"]
targets = ["x86_64-unknown-none"]
```

### Two-Step Build Process

**Step 1: Compile the kernel**

```bash
cargo build
```

This produces `target/x86_64-unknown-none/debug/claudio-os` (a bare ELF binary, not yet bootable).

**Step 2: Create bootable disk images**

```bash
cargo run --manifest-path tools/image-builder/Cargo.toml -- \
    target/x86_64-unknown-none/debug/claudio-os
```

This uses the `bootloader` crate (v0.11) to wrap the kernel into BIOS and UEFI disk images.

**Step 3: Run in QEMU**

BIOS boot (simpler, no OVMF needed):
```bash
qemu-system-x86_64 \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-bios.img \
    -serial stdio \
    -m 512M
```

UEFI boot (requires OVMF):
```bash
qemu-system-x86_64 \
    -bios /usr/share/OVMF/OVMF_CODE.fd \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-uefi.img \
    -serial stdio \
    -m 512M
```

With networking (Phase 2+):
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

### Build Troubleshooting

**Windows: MSVC linker errors**
The image builder (`tools/image-builder/`) is a host-side Rust binary that needs the MSVC linker. If you see linker errors, ensure you have "Desktop development with C++" installed in Visual Studio, or at minimum the MSVC build tools. For partial installs, set the `LIB` environment variable to include paths like:
```
C:\Program Files\Microsoft Visual Studio\...\MSVC\...\lib\x64
C:\Program Files (x86)\Windows Kits\10\Lib\...\ucrt\x64
C:\Program Files (x86)\Windows Kits\10\Lib\...\um\x64
```

**`fatfs` v0.3 is broken for `no_std`**
The `fatfs` crate at v0.3 has compilation issues in `no_std` environments. This is a known upstream issue. The workspace pins v0.3 with `default-features = false`; if it fails, Phase 3 (persistence) will need a fork or upgrade to v0.4 when available.

**`embedded-tls` crashes LLVM on bare-metal**
The `embedded-tls` crate's cryptographic operations can trigger LLVM codegen crashes when compiled for `x86_64-unknown-none`. This is a known issue with certain LLVM optimization passes on freestanding targets. Phase 2 may need to use `rustls` with `no_std` support instead, or a custom TLS implementation.

## Development Roadmap

### Phase 1: Boot to Terminal (current)
Kernel boots, framebuffer works, keyboard input, terminal rendering with ANSI support.

### Phase 2: Networking + TLS
VirtIO-net driver, smoltcp TCP/IP with DHCP/DNS, TLS handshake, HTTPS connectivity to the outside world.

### Phase 3: API Client + Auth
OAuth 2.0 device flow, token persistence to FAT32, Anthropic Messages API with SSE streaming, tool use protocol.

### Phase 4: Multi-Agent Dashboard
Split-pane layout with independent agent sessions, tmux-style keyboard shortcuts (Ctrl+B prefix), status bar.

### Phase 5: Real Hardware + Hardening
Physical hardware boot, e1000/I219-V NIC drivers, USB keyboard via xHCI, encrypted persistence, USB boot images.

## Design Decisions

- **No JavaScript runtime.** Direct HTTP/1.1 + SSE to the Anthropic API from Rust. No Node.js, no reqwest, no hyper.
- **Single address space.** No kernel/user boundary, no syscalls. Every agent is an async task. We trust our own code.
- **Interrupt-driven async.** Hardware interrupts wake futures. `hlt` when idle for power savings. No busy-polling.
- **FAT32 for persistence.** Simple, well-supported via the `fatfs` crate. No ext4, no journaling.

## Environment Variables (build-time)

| Variable | Description |
|----------|-------------|
| `CLAUDIO_API_KEY` | Optional baked-in API key for dev (skips OAuth) |
| `CLAUDIO_LOG_LEVEL` | `trace`/`debug`/`info`/`warn`/`error` (default: `info`) |
| `CLAUDIO_QEMU` | Set to `1` for QEMU-friendly defaults (VirtIO, SLIRP) |

## License

AGPL-3.0-or-later -- [Ridge Cell Repair LLC](https://github.com/suhteevah)

---

## Support This Project

If you find this project useful, consider buying me a coffee! Your support helps me keep building and sharing open-source tools.

[![Donate via PayPal](https://img.shields.io/badge/Donate-PayPal-blue.svg?logo=paypal)](https://www.paypal.me/baal_hosting)

**PayPal:** [baal_hosting@live.com](https://paypal.me/baal_hosting)

Every donation, no matter how small, is greatly appreciated and motivates continued development. Thank you!
