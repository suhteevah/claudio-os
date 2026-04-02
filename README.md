# ClaudioOS -- Bare-Metal AI Agent OS

A bare-metal Rust operating system purpose-built for running multiple AI coding agents
(Anthropic Claude) simultaneously. No Linux kernel, no POSIX, no JavaScript runtime --
just Rust, UEFI, and direct HTTPS to Claude.

**33 crates. ~80,000+ lines of Rust. Zero external OS dependencies.**

ClaudioOS boots your machine into a split-pane terminal dashboard where each pane is an
independent Claude agent session with tool use (text editor, Python interpreter, Rust
compiler, JavaScript evaluator). The entire stack -- from hardware interrupts to TLS 1.3
handshakes to SSE streaming -- is a single-address-space async Rust application.

**GitHub**: [suhteevah/baremetal-claude](https://github.com/suhteevah/baremetal-claude)
**Site**: [claudioos.vercel.app](https://claudioos.vercel.app)
**License**: AGPL-3.0-or-later ([Ridge Cell Repair LLC](https://github.com/suhteevah))

<!-- Screenshot placeholder: Add a screenshot of the agent dashboard here -->
<!-- ![ClaudioOS Dashboard](docs/screenshot.png) -->

---

## Feature Highlights

- **Multi-agent dashboard** -- tmux-style split panes, each running an independent Claude session
- **Native TLS 1.3** -- AES-128-GCM-SHA256 with hardware AES-NI, direct HTTPS to Claude APIs
- **Two auth modes** -- claude.ai Max subscription (OAuth) or Anthropic API key
- **AI-native shell** -- 28 Unix-like builtins + natural language mode (type English, get commands)
- **Full filesystem stack** -- ext4, btrfs, NTFS, FAT32, VFS layer, GPT/MBR partition detection
- **Hardware drivers** -- AHCI/SATA, NVMe, Intel NIC, xHCI USB, HDA audio, NVIDIA GPU, SMP
- **Post-quantum SSH** -- ML-KEM-768 + X25519 hybrid KEX, ML-DSA-65 host keys
- **Dev tools** -- Python interpreter, JavaScript evaluator, Rust compiler (Cranelift JIT), nano-like editor
- **Text-mode browser** -- HTML parser, CSS selectors, HTTP/HTTPS transport (wraith)
- **Session persistence** -- Conversations survive reboots via QEMU fw_cfg

---

## Architecture

```
+=====================================================================+
|  Agent Dashboard (tmux-style split panes)                           |
|  +-------------------+ +-------------------+ +-------------------+  |
|  | Agent 1 (Claude)  | | Agent 2 (Claude)  | | Agent 3 (Claude)  |  |
|  +-------------------+ +-------------------+ +-------------------+  |
+=====================================================================+
|  Shell (28 builtins + AI)  |  SSH Daemon (post-quantum)             |
+============================+========================================+
|  API Client (SSE) | Auth (OAuth/key) | Editor | Python | JS | Rust |
+=====================================================================+
|  VFS: ext4 | btrfs | NTFS | FAT32 | GPT/MBR                       |
+=====================================================================+
|  TLS 1.3 (embedded-tls) | smoltcp TCP/IP (DHCP, DNS)               |
+=====================================================================+
|  VirtIO-net | Intel NIC | AHCI | NVMe | xHCI | HDA | GPU | SMP    |
+=====================================================================+
|  Kernel: async executor, 48 MiB heap, GDT/IDT, PIC, PCI, PIT      |
+=====================================================================+
|  UEFI Boot (bootloader crate v0.11)                                 |
+=====================================================================+
```

---

## Quick Start

### Prerequisites

- **Rust nightly** (auto-installed via `rust-toolchain.toml`)
- **QEMU** with OVMF firmware
- **Windows**: MSVC build tools for the image builder

### Build and Run

```bash
# 1. Build the kernel (33 crates, ~80k lines)
cargo build

# 2. Create bootable disk image
cargo run --manifest-path tools/image-builder/Cargo.toml -- \
    target/x86_64-unknown-none/debug/claudio-os

# 3. Run in QEMU (-cpu Haswell required for AES-NI / TLS 1.3)
qemu-system-x86_64 \
    -bios /usr/share/OVMF/OVMF_CODE.fd \
    -drive format=raw,file=target/x86_64-unknown-none/debug/claudio-os-uefi.img \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0 \
    -serial stdio \
    -m 1G \
    -cpu Haswell
```

**Windows**: Use `run.ps1` for one-click launch with session persistence.

### With API Key

```bash
CLAUDIO_API_KEY=sk-ant-api03-xxx cargo build
```

See [docs/BUILDING.md](docs/BUILDING.md) for full build instructions, platform-specific
setup, and troubleshooting.

---

## Documentation

| Document | Description |
|----------|-------------|
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | Full system architecture, boot sequence, memory layout, crate graph |
| [HARDWARE.md](docs/HARDWARE.md) | Hardware drivers: AHCI, NVMe, Intel NIC, xHCI, HDA, GPU, SMP, ACPI |
| [NETWORKING.md](docs/networking.md) | Network stack: VirtIO-net, smoltcp, TLS 1.3, HTTP, claude.ai API, SSH |
| [FILESYSTEMS.md](docs/FILESYSTEMS.md) | VFS layer, ext4, btrfs, NTFS, FAT32, GPT/MBR |
| [SHELL.md](docs/SHELL.md) | AI-native shell: 28 builtins, pipes, env vars, scripting |
| [AGENTS.md](docs/AGENTS.md) | Multi-agent system: auth modes, dashboard, tool loop, session persistence |
| [BUILDING.md](docs/building.md) | Build instructions, QEMU setup, run.ps1, troubleshooting |
| [OPEN-SOURCE-CRATES.md](docs/OPEN-SOURCE-CRATES.md) | 19 published crates with usage examples |
| [ROADMAP.md](docs/ROADMAP.md) | Feature roadmap and TODO list |

---

## Published Crates (19)

These crates are standalone `#![no_std]` libraries usable in any bare-metal or
embedded Rust project:

| Category | Crates |
|----------|--------|
| **Filesystems** | ext4-rw, btrfs-nostd, ntfs-rw |
| **Storage drivers** | ahci-nostd, nvme-nostd |
| **Network drivers** | intel-nic-nostd |
| **USB** | xhci-nostd |
| **Audio** | hda-nostd |
| **System** | acpi-nostd, smp-nostd, gpu-compute-nostd |
| **Security** | sshd-pqc (post-quantum SSH) |
| **Languages** | python-lite, js-lite, rustc-lite |
| **Tools** | editor-nostd |
| **Web** | wraith-dom, wraith-render, wraith-transport |

See [OPEN-SOURCE-CRATES.md](docs/OPEN-SOURCE-CRATES.md) for usage examples and
API documentation.

---

## All 33 Crates

| Crate | Lines | Description |
|-------|-------|-------------|
| kernel | 4,537 | Boot, hardware init, async executor, dashboard |
| claudio-terminal | 1,794 | Framebuffer terminal, split panes, ANSI/VTE |
| claudio-net | 3,172 | VirtIO-net, smoltcp, TLS 1.3, HTTP/SSE |
| claudio-api | 1,849 | Anthropic Messages API, SSE streaming, tools |
| claudio-auth | 395 | OAuth device flow, API key, token refresh |
| claudio-agent | 501 | Agent session lifecycle, tool loop (20 rounds) |
| claudio-shell | 2,884 | AI-native shell, 28 builtins, pipes |
| claudio-vfs | 1,930 | Virtual filesystem, mount table, POSIX API |
| claudio-ext4 | 3,013 | ext4: superblock, inodes, extent trees |
| claudio-btrfs | 4,006 | btrfs: B-trees, chunks, CRC32C, COW |
| claudio-ntfs | 3,561 | NTFS: MFT, data runs, B+ tree indexes |
| claudio-ahci | 2,139 | AHCI/SATA: HBA registers, sector I/O |
| claudio-nvme | 2,563 | NVMe: queue pairs, doorbell registers |
| claudio-intel-nic | 1,986 | Intel e1000/e1000e/igc: DMA rings, PHY |
| claudio-xhci | 4,204 | xHCI USB 3.0 + HID keyboard |
| claudio-acpi | 2,433 | ACPI: RSDP, MADT, FADT, MCFG, HPET |
| claudio-hda | 2,631 | HD Audio: CORB/RIRB, codec discovery, PCM |
| claudio-smp | 3,391 | SMP: APIC, trampoline, work-stealing scheduler |
| claudio-gpu | 3,392 | NVIDIA GPU: Falcon, FIFO, tensor ops |
| claudio-sshd | 4,191 | Post-quantum SSH: ML-KEM-768, ML-DSA-65 |
| claudio-editor | 534 | Nano-like text editor (11 tests) |
| python-lite | 2,388 | Python interpreter (28 tests) |
| js-lite | 5,229 | JavaScript evaluator |
| rustc-lite | 142 | Rust compiler via Cranelift |
| wraith-dom | 2,070 | HTML parser, CSS selectors (32 tests) |
| wraith-render | 1,225 | HTML to text-mode renderer (12 tests) |
| wraith-transport | 572 | HTTP/HTTPS over smoltcp |
| claudio-fs | 40 | FAT32 persistence (stubbed) |
| cranelift-*-nostd | -- | 4 forked Cranelift crates for no_std |
| rustc-hash-nostd | -- | Forked rustc-hash for no_std |
| arbitrary-stub | -- | no_std stub for arbitrary crate |

---

## Target Hardware

| Machine | CPU | GPU | NIC | Status |
|---------|-----|-----|-----|--------|
| QEMU | Haswell (emulated) | -- | VirtIO-net | Primary dev target |
| Desktop | i9-11900K | RTX 3070 Ti | I219-V | Planned |
| Supermicro SYS-4028GR-TRT | Dual Xeon | 8x GPU | 10GbE | Planned |
| HP Victus laptop | i5-12500H | RTX 3050 | Intel Wi-Fi | Planned |
| Arch Linux box | -- | -- | Intel NIC | Planned |

---

## License

- **ClaudioOS** (kernel + integration): [AGPL-3.0-or-later](LICENSE)
- **Published crates** (19 standalone libraries): MIT + Apache-2.0 dual license

Copyright (c) [Ridge Cell Repair LLC](https://github.com/suhteevah)

---

## Support

If you find this project useful, consider supporting development:

[![Donate via PayPal](https://img.shields.io/badge/Donate-PayPal-blue.svg?logo=paypal)](https://www.paypal.me/baal_hosting)

**PayPal**: [baal_hosting@live.com](https://paypal.me/baal_hosting)
