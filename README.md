# ClaudioOS -- Bare-Metal AI Agent OS

A bare-metal Rust operating system purpose-built for running multiple AI coding agents
(Anthropic Claude) simultaneously. No Linux kernel, no POSIX, no JavaScript runtime --
just Rust, UEFI, and direct HTTPS to Claude.

**36 crates. 42 kernel modules. ~221,000 lines of Rust. Zero external OS dependencies.**

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
- **6 pane types** -- Agent, Shell, Web Browser, File Manager, System Monitor, Screensaver
- **Native TLS 1.3** -- AES-128-GCM-SHA256 with hardware AES-NI, direct HTTPS to Claude APIs
- **Two auth modes** -- claude.ai Max subscription (OAuth) or Anthropic API key
- **Session auto-refresh** -- JWT expiry detection, automatic token refresh before expiry
- **AI-native shell** -- 45+ Unix-like builtins + natural language mode (type English, get commands)
- **Full filesystem stack** -- ext4, btrfs, NTFS, FAT32, VFS layer, GPT/MBR partition detection
- **Hardware drivers** -- AHCI/SATA, NVMe, Intel NIC, WiFi, Bluetooth, USB storage, xHCI USB (keyboard + mouse + touchpad), HDA audio, NVIDIA GPU, SMP
- **WiFi networking** -- Intel AX201/AX200 driver with WPA2/WPA3, network scanning, association
- **Bluetooth** -- HCI/L2CAP/GAP/GATT stack over USB transport, HID device support
- **USB mass storage** -- BOT (Bulk-Only Transport) + SCSI command set for thumb drives
- **ACPI hardware discovery** -- MADT (CPU cores, I/O APICs), FADT (power management), HPET (precision timer), MCFG (PCIe ECAM)
- **SMP multi-core** -- APIC-mode interrupt routing, AP core startup via SIPI, work-stealing scheduler
- **Post-quantum SSH** -- ML-KEM-768 + X25519 hybrid KEX, ML-DSA-65 host keys, port 22
- **Inter-agent IPC** -- message bus, named channels, shared memory, 8 IPC tools for Claude agents
- **Dev tools** -- Python interpreter, JavaScript evaluator, Rust compiler (Cranelift JIT), nano-like editor
- **Text-mode browser** -- HTML parser, CSS selectors, HTTP/HTTPS transport (wraith), link following
- **File manager** -- visual directory browser with copy, move, rename, delete, search
- **System monitor** -- real-time CPU, memory, network, and agent stats dashboard
- **Conversation management** -- list, select, rename, delete claude.ai conversations
- **Firewall** -- stateful packet filtering, allow/deny rules, port-based and IP-based filtering
- **Disk encryption** -- LUKS-compatible encryption layer for persistent storage
- **Swap management** -- virtual memory swap to disk, configurable swap partitions
- **Cron scheduler** -- periodic task execution with crontab-style scheduling
- **Virtual consoles** -- multiple independent terminal sessions, Ctrl+Alt+F1-F6 switching
- **Clipboard** -- system-wide copy/paste buffer shared across panes
- **Power management** -- ACPI S3/S5 suspend/resume, battery status monitoring
- **Touchpad support** -- PS/2 and USB touchpad driver with gesture recognition
- **Network tools** -- ping, wget, curl, netstat, ifconfig, dns, traceroute, nslookup
- **Man pages** -- built-in manual pages for all commands
- **Init system** -- fw_cfg config, hostname, log level, auto-mount, startup scripts
- **User accounts** -- SHA-256 password auth, SSH public key auth, user database
- **RTC wall clock** -- CMOS real-time clock for timestamps, uptime tracking
- **Color themes** -- 9 built-in themes (solarized, monokai, dracula, nord, gruvbox, and more)
- **Boot splash** -- ASCII art logo with 4-stage progress bar
- **Boot chime** -- PC speaker C5-E5-G5 ascending triad
- **Screensaver** -- 5 modes: starfield, matrix rain, bouncing logo, pipes, digital clock
- **Session persistence** -- Conversations survive reboots via QEMU fw_cfg

---

## Architecture

```
+=====================================================================+
|  Agent Dashboard (tmux-style split panes)                           |
|  +--------+ +--------+ +--------+ +--------+ +--------+ +--------+ |
|  | Agent  | | Shell  | |Browser | |FileMgr | |SysMon  | |Screen- | |
|  | (Claude| | (45+   | |(wraith | |(visual | |(CPU/   | | saver  | |
|  |  tools)| |  cmds) | | + TLS) | | dirs)  | | mem)   | | (5x)   | |
|  +--------+ +--------+ +--------+ +--------+ +--------+ +--------+ |
+=====================================================================+
|  Shell (45+ builtins + AI) |  SSH Daemon (post-quantum, port 22)    |
+============================+========================================+
|  API Client (SSE) | Auth (OAuth/key) | Editor | Python | JS | Rust |
|  IPC (msg bus + channels)  | Conversations | Session Refresh        |
|  Firewall | Encryption | Swap | Cron | VConsoles | Clipboard        |
+=====================================================================+
|  VFS: ext4 | btrfs | NTFS | FAT32 | GPT/MBR                       |
+=====================================================================+
|  TLS 1.3 (embedded-tls) | smoltcp TCP/IP (DHCP, DNS)               |
+=====================================================================+
|  VirtIO-net | Intel NIC | WiFi | AHCI | NVMe | xHCI | HDA | GPU   |
|  Bluetooth | USB Storage | Touchpad | SMP | ACPI | RTC | Speaker   |
+=====================================================================+
|  Init | Users | Themes | Splash | Screensaver | Power | ManPages   |
+=====================================================================+
|  Kernel: async executor, 48 MiB heap, GDT/IDT, APIC, PCI, PIT     |
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
# 1. Build the kernel (36 crates, ~221k lines)
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
| [HARDWARE.md](docs/HARDWARE.md) | Hardware drivers: AHCI, NVMe, Intel NIC, WiFi, Bluetooth, USB storage, xHCI, HDA, GPU, SMP, ACPI, touchpad |
| [NETWORKING.md](docs/networking.md) | Network stack: VirtIO-net, smoltcp, TLS 1.3, HTTP, claude.ai API, SSH |
| [FILESYSTEMS.md](docs/FILESYSTEMS.md) | VFS layer, ext4, btrfs, NTFS, FAT32, GPT/MBR |
| [SHELL.md](docs/SHELL.md) | AI-native shell: 45+ builtins, pipes, env vars, scripting, network tools, themes |
| [AGENTS.md](docs/AGENTS.md) | Multi-agent system: auth modes, dashboard, tool loop, IPC, session management |
| [BUILDING.md](docs/building.md) | Build instructions, QEMU setup, run.ps1, troubleshooting |
| [OPEN-SOURCE-CRATES.md](docs/OPEN-SOURCE-CRATES.md) | 22 published crates with usage examples |
| [ROADMAP.md](docs/ROADMAP.md) | Feature roadmap and TODO list |

---

## Published Crates (22)

These crates are standalone `#![no_std]` libraries usable in any bare-metal or
embedded Rust project:

| Category | Crates |
|----------|--------|
| **Filesystems** | ext4-rw, btrfs-nostd, ntfs-rw |
| **Storage drivers** | ahci-nostd, nvme-nostd, usb-storage-nostd |
| **Network drivers** | intel-nic-nostd, wifi-nostd |
| **Wireless** | bluetooth-nostd |
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

## All 36 Crates

| Crate | Lines | Description |
|-------|-------|-------------|
| kernel | 18,000+ | Boot, hardware init, async executor, dashboard, 42 kernel modules |
| claudio-terminal | 1,794 | Framebuffer terminal, split panes, ANSI/VTE |
| claudio-net | 3,172 | VirtIO-net, smoltcp, TLS 1.3, HTTP/SSE |
| claudio-api | 1,849 | Anthropic Messages API, SSE streaming, tools |
| claudio-auth | 395 | OAuth device flow, API key, token refresh |
| claudio-agent | 501 | Agent session lifecycle, tool loop (20 rounds) |
| claudio-shell | 2,884 | AI-native shell, 45+ builtins, pipes |
| claudio-vfs | 1,930 | Virtual filesystem, mount table, POSIX API |
| claudio-ext4 | 3,013 | ext4: superblock, inodes, extent trees |
| claudio-btrfs | 4,006 | btrfs: B-trees, chunks, CRC32C, COW |
| claudio-ntfs | 3,561 | NTFS: MFT, data runs, B+ tree indexes |
| claudio-ahci | 2,139 | AHCI/SATA: HBA registers, sector I/O |
| claudio-nvme | 2,563 | NVMe: queue pairs, doorbell registers |
| claudio-intel-nic | 1,986 | Intel e1000/e1000e/igc: DMA rings, PHY |
| claudio-wifi | 3,513 | WiFi: Intel AX201/AX200, WPA2/WPA3, scanning |
| claudio-bluetooth | 3,075 | Bluetooth: HCI/L2CAP/GAP/GATT, USB transport, HID |
| claudio-usb-storage | 1,357 | USB mass storage: BOT protocol, SCSI commands |
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

### Kernel Modules (42)

These are in-kernel modules under `kernel/src/` that wire the standalone crates
to the hardware and dashboard:

| Module | Lines | Description |
|--------|-------|-------------|
| `dashboard.rs` | 1,862 | Main dashboard loop, pane management, input dispatch |
| `main.rs` | 1,248 | Boot sequence, hardware init, async entry point |
| `screensaver.rs` | 951 | 5 modes: starfield, matrix rain, bouncing logo, pipes, digital clock |
| `power.rs` | 921 | ACPI S3/S5 suspend/resume, battery monitoring, power profiles |
| `encryption.rs` | 905 | LUKS-compatible disk encryption, key derivation, crypto layer |
| `filemanager.rs` | 843 | Visual file manager pane: directory listing, copy/move/rename/delete/search |
| `firewall.rs` | 788 | Stateful packet filtering, allow/deny rules, IP/port filtering |
| `nettools.rs` | 787 | ping, wget, curl, netstat, ifconfig, dns, traceroute, nslookup |
| `ipc.rs` | 783 | Message bus, named channels, shared memory, 8 IPC tools for agents |
| `agent_loop.rs` | 774 | Agent tool loop, SSE streaming, tool execution |
| `touchpad.rs` | 734 | PS/2 and USB touchpad driver, gesture recognition |
| `manpages.rs` | 674 | Built-in manual pages for all commands |
| `browser.rs` | 659 | Text-mode web browser pane: wraith + smoltcp, URL bar, link following |
| `ssh_server.rs` | 568 | SSH listener on port 22, TCP session management, echo shell |
| `acpi_init.rs` | 523 | ACPI table discovery: MADT, FADT, HPET, MCFG parsing |
| `conversations.rs` | 517 | Conversation management: list, select, rename, delete via claude.ai API |
| `init.rs` | 505 | fw_cfg config loading, hostname, log level, auto-mount, startup scripts |
| `swap.rs` | 499 | Virtual memory swap to disk, configurable swap partitions |
| `session_manager.rs` | 487 | Session auto-refresh: JWT expiry parsing, automatic token refresh |
| `intel_nic.rs` | 454 | Intel NIC -> smoltcp Device adapter, full network stack with DHCP |
| `users.rs` | 440 | User database, SHA-256 password auth, SSH public key auth |
| `cron.rs` | 410 | Periodic task scheduler, crontab-style scheduling |
| `mouse.rs` | 402 | USB HID mouse state, XOR crosshair cursor, event queue |
| `interrupts.rs` | 387 | IDT setup, exception handlers, IRQ routing |
| `vconsole.rs` | 372 | Virtual consoles, Ctrl+Alt+F1-F6 switching |
| `themes.rs` | 365 | 9 color themes with ANSI 24-bit escape generation |
| `sysmon.rs` | 306 | System monitor: CPU, memory, network, agent stats with ANSI rendering |
| `rtc.rs` | 299 | CMOS RTC wall clock, BCD/binary decode, PIT-corrected uptime |
| `executor.rs` | 287 | Interrupt-driven async executor, hlt when idle |
| `framebuffer.rs` | 263 | GOP framebuffer init, double-buffered, dirty region tracking |
| `pci.rs` | 245 | PCI bus enumeration, BAR mapping, bus mastering |
| `smp_init.rs` | 233 | Multi-core boot: MADT-driven AP startup, APIC mode, legacy PIC disable |
| `splash.rs` | 214 | Boot splash: ASCII art logo, 4-stage progress bar |
| `usb.rs` | 186 | xHCI controller init, USB keyboard -> PS/2 scancode bridge |
| `keyboard.rs` | 180 | PS/2 keyboard decoder, scancode queue |
| `memory.rs` | 124 | Page table setup, physical memory offset |
| `boot_sound.rs` | 111 | PC speaker boot chime: C5-E5-G5 via PIT channel 2 |
| `clipboard.rs` | 108 | System-wide copy/paste buffer shared across panes |
| `serial.rs` | 103 | UART 16550 serial output at 115200 baud |
| `gdt.rs` | 76 | GDT + TSS setup |
| `logger.rs` | 32 | Log framework: serial + framebuffer sinks |
| `terminal.rs` | 28 | Terminal crate bridge |

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
- **Published crates** (22 standalone libraries): MIT + Apache-2.0 dual license

Copyright (c) [Ridge Cell Repair LLC](https://github.com/suhteevah)

---

## Support

If you find this project useful, consider supporting development:

[![Donate via PayPal](https://img.shields.io/badge/Donate-PayPal-blue.svg?logo=paypal)](https://www.paypal.me/baal_hosting)

**PayPal**: [baal_hosting@live.com](https://paypal.me/baal_hosting)
