# ClaudioOS

A bare-metal Rust operating system purpose-built for running multiple AI coding agents (Anthropic Claude) simultaneously. No Linux kernel, no POSIX, no JavaScript runtime -- just Rust, UEFI, and direct HTTPS to `api.anthropic.com`.

**Built in a single Claude Code session.** ~200,000+ lines of Rust, 40+ commits, zero external OS dependencies.

ClaudioOS boots your machine into a split-pane terminal dashboard where each pane is an independent Claude agent session with tool use (text editor, Python interpreter, Rust compiler). The entire stack -- from hardware interrupts to TLS 1.3 handshakes to SSE streaming -- is a single-address-space async Rust application.

**GitHub:** [suhteevah/baremetal-claude](https://github.com/suhteevah/baremetal-claude)
**Site:** [claudioos.vercel.app](https://claudioos.vercel.app)
**Wiki:** [10 pages](https://github.com/suhteevah/baremetal-claude/wiki)
**License:** AGPL-3.0-or-later -- [Ridge Cell Repair LLC](https://github.com/suhteevah)

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
|    (async tasks, tool loop, conversation state)      |
+---------+----------+---------+-----------+----------+
| API     | Auth     | Editor  | Python    | Rust     |
| Client  | Relay    | (nano)  | Interp    | Compiler |
| SSE+TLS | API key  | 400 LOC | Lite      | Cranelift|
+---------+----------+---------+-----------+----------+
|              Async Executor (interrupt-driven)        |
+-----------------------------------------------------+
|   Net (smoltcp + TLS 1.3)  |   Terminal (split-pane)|
+----------------------------+------------------------+
|   VirtIO-net (legacy 0.9.5)|  PS/2 Keyboard (IRQ1)  |
+----------------------------+------------------------+
|              x86_64 Kernel Core                      |
|   (paging, 16 MiB heap, GDT/IDT, PIC, PCI, PIT)    |
+-----------------------------------------------------+
|              UEFI Boot (bootloader crate v0.11)      |
+-----------------------------------------------------+
```

## Current Status: All Core Phases Complete

### Phase 1: Boot to Terminal -- COMPLETE
- UEFI boot via bootloader v0.11 with custom `x86_64-claudio.json` target
- GDT with TSS and kernel data segment (critical IRETQ #GP fix)
- IDT with 8259 PIC (APIC explicitly disabled for UEFI compatibility)
- 16 MiB heap with page-mapped frame allocator (`linked_list_allocator`)
- SSE/SSE2/AVX enabled via CR0/CR4/XCR0 with CPUID detection
- 4 MiB heap-allocated kernel stack (bootloader stack exhaustion fix)
- PS/2 keyboard with async `ScancodeStream` via IRQ1
- GOP framebuffer with `noto-sans-mono-bitmap` font rendering
- VTE-based ANSI terminal with full SGR color support
- Split-pane layout engine with binary tree viewports
- PIT timer at 18.2 Hz for timestamps
- Serial UART (16550) for debug output at 115200 baud
- PCI bus enumeration with bus mastering

### Phase 2: Networking + TLS -- COMPLETE
- VirtIO-net driver (legacy 0.9.5 spec, virtqueue DMA, page table walk)
- smoltcp TCP/IP stack with DHCP (10.0.2.x) + DNS (10.0.2.3)
- Native TLS 1.3 via `embedded-tls` (AES-128-GCM-SHA256, 16-byte aligned buffers)
- Requires `-cpu Haswell` in QEMU for AES-NI hardware instructions
- HTTP/1.1 client with chunked transfer encoding + SSE parsing
- TCP send queue drain fix (wait for ACK before recv)
- CloseWait EOF detection for clean connection teardown
- Nagle disabled for immediate packet transmission

### Phase 3: API Client + Auth -- COMPLETE
- Anthropic Messages API with SSE streaming (token-by-token rendering)
- Auth relay (`tools/auth-relay.py`) for API key management
- Compile-time `CLAUDIO_API_KEY` fallback via `option_env!()`
- **Claude Haiku responds from bare metal**: "Hi from bare metal: CPU running without OS abstractions here."

### Phase 4: Multi-Agent Dashboard -- COMPLETE
- Ctrl+B tmux-style keybindings (splits, focus, new agent, close)
- Per-pane agent sessions with independent conversation state
- Agent tool loop (send -> tool_use -> execute -> resend, max 20 rounds)
- Framebuffer rendering with welcome banner
- Focus switching between panes with visual cursor

### Phase 5: Development Environment -- COMPLETE
- **python-lite**: Minimal Python interpreter (variables, loops, functions, 28 tests)
- **Rust build server** (`tools/build-server.py`) + `compile_rust` tool for agents
- **Nano-like text editor** (`crates/editor`, ~400 lines, 11 tests)
- `execute_python` tool for Claude agents to run code

### Phase 6: Self-Hosting Foundation -- COMPLETE
- **Cranelift code generator compiles for bare metal** (cranelift-codegen + cranelift-frontend)
- Forked 6 crates for `no_std`: cranelift-codegen, cranelift-frontend, cranelift-codegen-shared, cranelift-control, rustc-hash, arbitrary
- `libm` for f32/f64 math in `no_std`
- Build script post-processing for generated code `std` -> `core` replacement
- `hashbrown` with `ahash` for HashMap/HashSet
- `crates/rustc-lite`: Bare-metal Rust compilation via Cranelift

### Wraith Browser Integration (Work in Progress)
- **wraith-dom**: `no_std` HTML parser + CSS selectors + form detection (1,610 lines, 32 tests)
- **wraith-transport**: HTTP/HTTPS over smoltcp (572 lines)
- **wraith-render**: HTML -> text-mode character grid renderer (1,221 lines, 12 tests)

## Crate Structure

| Crate | Path | Status | Description |
|-------|------|--------|-------------|
| `claudio-os` (kernel) | `kernel/` | Active | `#![no_std]` `#![no_main]` binary entry point |
| `claudio-terminal` | `crates/terminal/` | Active | Framebuffer renderer, split panes, ANSI/VTE |
| `claudio-net` | `crates/net/` | Active | VirtIO-net + smoltcp + TLS 1.3 + HTTP/SSE |
| `claudio-api` | `crates/api-client/` | Active | Anthropic Messages API + SSE streaming |
| `claudio-auth` | `crates/auth/` | Active | OAuth device flow + credential types |
| `claudio-agent` | `crates/agent/` | Active | Agent session lifecycle + tool loop |
| `claudio-fs` | `crates/fs-persist/` | Stubbed | FAT32 persistence layer |
| `claudio-editor` | `crates/editor/` | Active | Nano-like text editor (~400 lines) |
| `python-lite` | `crates/python-lite/` | Active | Minimal Python interpreter (28 tests) |
| `rustc-lite` | `crates/rustc-lite/` | Active | Bare-metal Rust compiler via Cranelift |
| `wraith-dom` | `crates/wraith-dom/` | Active | `no_std` HTML parser + CSS selectors (32 tests) |
| `wraith-transport` | `crates/wraith-transport/` | Active | HTTP/HTTPS over smoltcp |
| `wraith-render` | `crates/wraith-render/` | Active | HTML -> text-mode character grid |
| `cranelift-*-nostd` | `crates/cranelift-*-nostd/` | Active | Forked Cranelift crates for `no_std` |
| `arbitrary-stub` | `crates/arbitrary-stub/` | Active | `no_std` stub for arbitrary crate |
| `rustc-hash-nostd` | `crates/rustc-hash-nostd/` | Active | `no_std` fork of rustc-hash |
| `claudio-image-builder` | `tools/image-builder/` | Active | Host-side disk image builder |

## Building

### Prerequisites

- **Nightly Rust** with `x86_64-unknown-none` target (pinned via `rust-toolchain.toml`)
- **QEMU** + **OVMF** for testing
- **Windows only:** MSVC linker is required for build scripts (the image builder runs on the host).
- **Linux:** standard Rust toolchain just works; install `qemu-system-x86` and `ovmf` from your package manager.

The `rust-toolchain.toml` will auto-install the correct nightly and components on first build.

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

**Step 3: Run in QEMU** (full networking + TLS)

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

**Important:** `-cpu Haswell` (or higher) is required for AES-NI instructions used by TLS 1.3.

### With Auth Relay (for API key management)

```bash
# Terminal 1: Start the auth relay
python3 tools/auth-relay.py

# Terminal 2: Run QEMU with port forwarding
qemu-system-x86_64 \
    -bios /usr/share/OVMF/OVMF_CODE.fd \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-uefi.img \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0,hostfwd=tcp::5555-:5555 \
    -serial stdio \
    -m 512M \
    -cpu Haswell
```

Or set the API key at compile time:

```bash
CLAUDIO_API_KEY=sk-ant-api03-xxx cargo build
```

### Build Troubleshooting

**Windows: MSVC linker errors** -- Install "Desktop development with C++" in Visual Studio. For partial installs, set the `LIB` environment variable to include MSVC and Windows SDK lib directories.

**TLS crashes** -- Ensure you are using `-cpu Haswell` (or later) in QEMU. The default QEMU CPU lacks AES-NI, causing illegal instruction faults in the TLS handshake.

**Bootloader stack exhaustion** -- Already fixed. The kernel allocates a 4 MiB heap stack and switches to it before enabling interrupts. If you see double faults after PCI enumeration, the stack switch may have regressed.

## Design Decisions

- **No JavaScript runtime.** Direct HTTP/1.1 + SSE to the Anthropic API from Rust. No Node.js, no reqwest, no hyper.
- **Single address space.** No kernel/user boundary, no syscalls. Every agent is an async task. We trust our own code.
- **Interrupt-driven async.** Hardware interrupts wake futures. `hlt` when idle for power savings. No busy-polling.
- **Custom target with SSE+AES-NI.** `x86_64-claudio.json` enables SSE, SSE2, AES, and PCLMULQDQ at the LLVM level for TLS crypto performance. AVX disabled to avoid alignment issues.
- **FAT32 for persistence.** Simple, well-supported via the `fatfs` crate. No ext4, no journaling.
- **Cranelift for self-hosting.** Six crates forked to `no_std` to enable bare-metal code generation. Agents can compile and run Rust code without a host OS.

## Key Bugs Fixed (Session History)

| Bug | Symptom | Root Cause | Fix |
|-----|---------|-----------|-----|
| GDT missing data segment | IRETQ #GP on first interrupt | `SS=0` in interrupt frame | Add kernel data segment, load DS/ES/SS |
| UEFI APIC conflict | Double fault on timer | APIC + PIC both firing vec 32 | Disable APIC via MSR 0x1B |
| Framebuffer page fault | Crash writing pixels | Bootloader mapping not writable | Page table walk to phys-offset mapping |
| TLS double fault | memchr AVX2 crash | Default QEMU CPU lacks AES-NI | Require `-cpu Haswell` |
| TLS alignment | AES-NI illegal instruction | Buffers not 16-byte aligned | Aligned buffer allocations |
| TCP recv timeout | Hangs after HTTP send | Send queue not drained | Wait for ACK + CloseWait EOF |
| Stack exhaustion | Double fault after init | 128 KiB bootloader stack full | 4 MiB heap-allocated stack |

## Environment Variables (build-time)

| Variable | Description |
|----------|-------------|
| `CLAUDIO_API_KEY` | Optional baked-in API key for dev (skips OAuth) |
| `CLAUDIO_LOG_LEVEL` | `trace`/`debug`/`info`/`warn`/`error` (default: `info`) |
| `CLAUDIO_QEMU` | Set to `1` for QEMU-friendly defaults (VirtIO, SLIRP) |

## Tools

| Tool | Path | Purpose |
|------|------|---------|
| `auth-relay.py` | `tools/auth-relay.py` | HTTP proxy for API key management |
| `build-server.py` | `tools/build-server.py` | Host-side Rust compilation service for agents |
| `tls-proxy.py` | `tools/tls-proxy.py` | TLS termination proxy (dev/debug) |
| `tls-bridge.py` | `tools/tls-bridge.py` | TLS bridge utility |
| `image-builder` | `tools/image-builder/` | Bootable UEFI/BIOS disk image builder |

## License

AGPL-3.0-or-later -- [Ridge Cell Repair LLC](https://github.com/suhteevah)

---

---

---

## Support This Project

If you find this project useful, consider buying me a coffee! Your support helps me keep building and sharing open-source tools.

[![Donate via PayPal](https://img.shields.io/badge/Donate-PayPal-blue.svg?logo=paypal)](https://www.paypal.me/baal_hosting)

**PayPal:** [baal_hosting@live.com](https://paypal.me/baal_hosting)

Every donation, no matter how small, is greatly appreciated and motivates continued development. Thank you!
