# ClaudioOS Architecture

## System Overview

ClaudioOS is a bare-metal Rust OS that boots via UEFI and runs AI coding agents
(Anthropic Claude) directly on hardware. No Linux kernel, no POSIX, no libc. A
single-address-space async Rust application manages all hardware.

**Target**: x86_64 UEFI machines (QEMU for dev, i9-11900K/RTX 3070 Ti for prod)

## Full Stack Diagram

```
+=====================================================================+
|                        USER-FACING LAYER                            |
|  +-------------------+ +-------------------+ +-------------------+  |
|  | Agent 1 (pane)    | | Agent 2 (pane)    | | Agent 3 (pane)    |  |
|  | claude session    | | claude session    | | claude session    |  |
|  +-------------------+ +-------------------+ +-------------------+  |
|  +---------+ +---------+ +----------+ +----------+ +-----------+   |
|  | Shell   | | Browser | | FileMgr  | | SysMon   | | Screen-   |   |
|  | (pane)  | | (pane)  | | (pane)   | | (pane)   | | saver     |   |
|  +---------+ +---------+ +----------+ +----------+ +-----------+   |
|  +-----------------------------------------------------------+     |
|  | AI-Native Shell  (28 builtins + natural language -> Claude)|     |
|  +-----------------------------------------------------------+     |
+=====================================================================+
|                      SESSION MANAGEMENT                             |
|  +-------------------+ +-------------------+ +-------------------+  |
|  | Agent Manager     | | Dashboard/Layout  | | SSH Daemon (PQ)   |  |
|  | tool loop, state  | | tmux-style panes  | | ML-KEM + X25519   |  |
|  +-------------------+ +-------------------+ +-------------------+  |
|  +-------------------+ +-------------------+ +-------------------+  |
|  | Session Refresh   | | Conversations     | | IPC (msg bus +    |  |
|  | JWT expiry, auto  | | list/select/del   | | channels + shmem) |  |
|  +-------------------+ +-------------------+ +-------------------+  |
+=====================================================================+
|                       APPLICATION SERVICES                          |
|  +-----------+ +----------+ +----------+ +---------+ +-----------+ |
|  | API Client| | Auth     | | Editor   | | Python  | | JS Lite   | |
|  | Messages  | | OAuth +  | | nano-    | | Interp  | | Cloudflare| |
|  | SSE + TLS | | API key  | | like     | | 28 tests| | solver    | |
|  +-----------+ +----------+ +----------+ +---------+ +-----------+ |
|  +-----------+ +---------------------------------------------------+|
|  | Rust Comp | | Wraith Browser (DOM + Transport + Render)         | |
|  | Cranelift | +---------------------------------------------------+|
|  +-----------+                                                      |
+=====================================================================+
|                       FILESYSTEM LAYER                              |
|  +---------------------------------------------------------------+ |
|  |                  VFS (mount table, POSIX API)                  | |
|  +-------+--------+--------+--------+----------------------------+ |
|  | ext4  | btrfs  | NTFS   | FAT32  | GPT/MBR partition detect   | |
|  +-------+--------+--------+--------+----------------------------+ |
+=====================================================================+
|                       NETWORK STACK                                 |
|  +---------------------------------------------------------------+ |
|  | HTTP/HTTPS Client | claude.ai API | SSH Daemon                | |
|  +-------------------+---------------+---------------------------+ |
|  | TLS 1.3 (embedded-tls, AES-128-GCM-SHA256)                   | |
|  +---------------------------------------------------------------+ |
|  | smoltcp TCP/IP (DHCP, DNS, TCP, UDP)                          | |
|  +---------------------------------------------------------------+ |
+=====================================================================+
|                       HARDWARE DRIVERS                              |
|  +----------+ +---------+ +-----------+ +--------+ +------------+ |
|  | VirtIO-  | | Intel   | | AHCI/SATA | | NVMe   | | xHCI USB   | |
|  | net      | | NIC     | | driver    | | driver | | 3.0 + HID  | |
|  +----------+ +---------+ +-----------+ +--------+ +------------+ |
|  +----------+ +---------+ +-----------+ +--------+ +------------+ |
|  | PS/2 Kbd | | HDA     | | GPU       | | ACPI   | | SMP/APIC   | |
|  | IRQ1     | | Audio   | | NVIDIA    | | tables | | multi-core | |
|  +----------+ +---------+ +-----------+ +--------+ +------------+ |
|  +----------+ +---------+ +-----------+                            |
|  | USB Mouse| | RTC     | | PC Speaker|                            |
|  | HID boot | | CMOS    | | PIT ch2   |                            |
|  +----------+ +---------+ +-----------+                            |
+=====================================================================+
|                       KERNEL SERVICES                               |
|  +---------------------------------------------------------------+ |
|  | Init System | Users | Themes | Splash | Screensaver | Chime   | |
|  +---------------------------------------------------------------+ |
|  | Async Executor (interrupt-driven, hlt when idle)              | |
|  +---------------------------------------------------------------+ |
|  | Memory: 48 MiB heap (linked_list_allocator), page tables      | |
|  | CPU: GDT + TSS, IDT, 8259 PIC / APIC, PIT timer (18.2 Hz)   | |
|  | PCI: bus enumeration, BAR mapping, bus mastering               | |
|  +---------------------------------------------------------------+ |
+=====================================================================+
|                       BOOT                                          |
|  +---------------------------------------------------------------+ |
|  | UEFI -> bootloader v0.11 -> kernel_main -> post_stack_switch  | |
|  |   -> splash -> ACPI -> SMP -> USB -> RTC -> network -> auth   | |
|  |   -> init system -> SSH -> dashboard                          | |
|  +---------------------------------------------------------------+ |
+=====================================================================+
```

## Boot Sequence

```
UEFI firmware
  |
  v
bootloader crate v0.11
  - Sets up identity-mapped page tables
  - Maps physical memory at offset
  - Initializes GOP framebuffer
  - Reads UEFI memory map
  - Jumps to kernel_main
  |
  v
kernel_main(boot_info)
  |-- Phase 0: Enable SSE/SSE2/AVX (CR0/CR4/XCR0 + CPUID)
  |-- Phase 0a: Serial UART init (0x3F8, 115200 baud)
  |-- Phase 0b: Logger init (serial + framebuffer sinks)
  |-- Phase 1: GDT + TSS
  |-- Phase 2: Heap allocator (48 MiB via linked_list_allocator)
  |-- Phase 3: IDT + 8259 PIC (APIC disabled for UEFI compat)
  |-- Phase 3b: PS/2 keyboard decoder
  |-- Phase 4: Framebuffer init + boot splash (Hardware stage)
  |-- Phase 4b: Boot chime (PC speaker C5-E5-G5)
  |-- Phase 5: PCI bus enumeration + device discovery
  |-- Phase 5b: ACPI table discovery (MADT, FADT, HPET, MCFG)
  |-- Phase 5c: SMP init (boot AP cores, switch to APIC mode)
  |-- Phase 5d: USB init (xHCI controller, keyboard, mouse)
  |-- Phase 5e: RTC init (read CMOS wall clock)
  |-- Phase 6: Allocate 4 MiB heap stack, switch RSP
  |
  v
post_stack_switch()
  |-- Enable interrupts (sti)
  |-- Start async executor
  |
  v
main_async()
  |-- Boot splash (Network stage)
  |-- Find NIC (VirtIO-net or Intel e1000)
  |-- Init network driver + smoltcp + DHCP
  |-- Resolve DNS
  |-- Boot splash (Authenticating stage)
  |-- Init system (load config from fw_cfg)
  |-- Init user database
  |-- Authenticate (API key or claude.ai OAuth)
  |-- Init session manager (JWT expiry tracking, auto-refresh)
  |-- Start SSH server on port 22
  |-- Boot splash (Ready stage)
  |-- Hide splash, launch agent dashboard
  |
  v
Dashboard loop (forever)
  |-- Render split-pane terminal (6 pane types)
  |-- Handle keyboard + USB mouse input
  |-- Dispatch to agent sessions / shell / browser / file manager
  |-- Agent tool loop (send -> tool_use -> execute -> resend)
  |-- IPC message delivery
  |-- Poll SSH server
  |-- Poll USB keyboard/mouse
  |-- Session refresh periodic check
  |-- Screensaver idle timeout check
  |-- System monitor auto-refresh
```

## Memory Layout

```
+---------------------+ 0xFFFF_FFFF_FFFF_FFFF
|                     |
| Physical memory     |   Bootloader maps all physical RAM at an offset
| offset mapping      |   (PHYS_MEM_OFFSET stored in AtomicU64)
|                     |
+---------------------+
|                     |
| Kernel heap         |   48 MiB, managed by linked_list_allocator
| (linked list alloc) |   Allocated from UEFI USABLE memory regions
|                     |
+---------------------+
|                     |
| Heap-allocated      |   4 MiB, 16-byte aligned
| kernel stack        |   RSP switched in kernel_main Phase 6
|                     |
+---------------------+
|                     |
| Framebuffer         |   Direct GOP framebuffer (e.g., 2560x1600x4 bytes)
| (write-combined)    |   Double-buffered with dirty region tracking
|                     |
+---------------------+
|                     |
| MMIO regions        |   PCI BARs for VirtIO-net, AHCI, NVMe, GPU, etc.
| (device memory)     |   Identity-mapped via physical memory offset
|                     |
+---------------------+
|                     |
| Kernel code + data  |   Loaded by bootloader from UEFI disk image
| (identity-mapped)   |
|                     |
+---------------------+ 0x0000_0000_0000_0000
```

## Crate Dependency Graph

```
kernel
  +-- claudio-agent
  |     +-- claudio-api (Messages API + SSE)
  |     |     +-- python-lite
  |     |     +-- js-lite
  |     +-- claudio-auth (OAuth + API key)
  |     +-- claudio-terminal (split-pane renderer)
  |     +-- claudio-net (VirtIO + smoltcp + TLS)
  +-- claudio-shell
  +-- claudio-vfs
  |     +-- (trait implemented by ext4, btrfs, ntfs)
  +-- claudio-ext4
  +-- claudio-btrfs
  +-- claudio-ntfs
  +-- claudio-ahci
  +-- claudio-nvme
  +-- claudio-intel-nic
  +-- claudio-xhci
  +-- claudio-acpi
  +-- claudio-hda
  +-- claudio-smp
  +-- claudio-gpu
  +-- claudio-sshd
  +-- claudio-editor
  +-- claudio-fs (FAT32 persistence, stubbed)
  +-- wraith-dom
  +-- wraith-render
  +-- wraith-transport
  +-- rustc-lite
  |     +-- cranelift-codegen-nostd
  |     +-- cranelift-frontend-nostd
  |     +-- cranelift-codegen-shared-nostd
  |     +-- cranelift-control-nostd
  |     +-- rustc-hash-nostd
  |     +-- arbitrary-stub
  +-- kernel modules (17):
        acpi_init, smp_init, usb, intel_nic, ssh_server,
        rtc, mouse, ipc, init, users, sysmon, splash,
        boot_sound, themes, screensaver, browser, filemanager,
        conversations, session_manager
```

## All 33 Crates + 17 Kernel Modules

| # | Crate | Path | Lines | Description |
|---|-------|------|-------|-------------|
| 1 | `claudio-os` | `kernel/` | 4,537+ | Kernel binary: boot, hardware init, async executor, dashboard |
| 2 | `claudio-terminal` | `crates/terminal/` | 1,794 | Framebuffer terminal, split panes, ANSI/VTE, font rendering |
| 3 | `claudio-net` | `crates/net/` | 3,172 | VirtIO-net driver, smoltcp TCP/IP, TLS 1.3, HTTP/SSE |
| 4 | `claudio-api` | `crates/api-client/` | 1,849 | Anthropic Messages API client, SSE streaming, tool use protocol |
| 5 | `claudio-auth` | `crates/auth/` | 395 | OAuth 2.0 device flow (RFC 8628), API key fallback, token refresh |
| 6 | `claudio-agent` | `crates/agent/` | 501 | Agent session lifecycle, tool loop (20 rounds), conversation state |
| 7 | `claudio-fs` | `crates/fs-persist/` | 40 | FAT32 persistence layer (stubbed) |
| 8 | `claudio-editor` | `crates/editor/` | 534 | Nano-like text editor, 11 tests |
| 9 | `python-lite` | `crates/python-lite/` | 2,388 | Minimal Python interpreter (vars, loops, functions), 28 tests |
| 10 | `js-lite` | `crates/js-lite/` | 5,229 | JavaScript evaluator for Cloudflare challenge solving |
| 11 | `rustc-lite` | `crates/rustc-lite/` | 142 | Bare-metal Rust compiler via Cranelift backend |
| 12 | `claudio-shell` | `crates/shell/` | 2,884 | AI-native shell: 28 builtins + natural language mode |
| 13 | `claudio-vfs` | `crates/vfs/` | 1,930 | Virtual filesystem: mount table, GPT/MBR, POSIX file API |
| 14 | `claudio-ext4` | `crates/ext4/` | 3,013 | ext4 filesystem: superblock, inodes, extent trees, directories |
| 15 | `claudio-btrfs` | `crates/btrfs/` | 4,006 | btrfs filesystem: B-trees, chunks, subvolumes, CRC32C, COW |
| 16 | `claudio-ntfs` | `crates/ntfs/` | 3,561 | NTFS filesystem: MFT, data runs, B+ tree indexes |
| 17 | `claudio-ahci` | `crates/ahci/` | 2,139 | AHCI/SATA driver: HBA registers, port commands, sector I/O |
| 18 | `claudio-nvme` | `crates/nvme/` | 2,563 | NVMe driver: admin/IO queue pairs, doorbell registers |
| 19 | `claudio-intel-nic` | `crates/intel-nic/` | 1,986 | Intel NIC driver: e1000/e1000e/igc, DMA rings, PHY config |
| 20 | `claudio-xhci` | `crates/xhci/` | 4,204 | xHCI USB 3.0 host controller + HID keyboard driver |
| 21 | `claudio-acpi` | `crates/acpi/` | 2,433 | ACPI table parser: RSDP, MADT, FADT, MCFG, HPET, shutdown |
| 22 | `claudio-hda` | `crates/hda/` | 2,631 | Intel HD Audio: CORB/RIRB, codec discovery, PCM playback |
| 23 | `claudio-smp` | `crates/smp/` | 3,391 | SMP: APIC, AP trampoline, per-CPU data, work-stealing scheduler |
| 24 | `claudio-gpu` | `crates/gpu/` | 3,392 | NVIDIA GPU: MMIO, Falcon, FIFO, compute dispatch, tensor ops |
| 25 | `claudio-sshd` | `crates/sshd/` | 4,191 | Post-quantum SSH daemon: ML-KEM-768 + X25519, ML-DSA-65 |
| 26 | `wraith-dom` | `crates/wraith-dom/` | 2,070 | no_std HTML parser, CSS selectors, form detection |
| 27 | `wraith-render` | `crates/wraith-render/` | 1,225 | HTML to text-mode character grid renderer |
| 28 | `wraith-transport` | `crates/wraith-transport/` | 572 | HTTP/HTTPS client over smoltcp |
| 29 | `cranelift-codegen-nostd` | `crates/cranelift-codegen-nostd/` | -- | Forked cranelift-codegen for no_std |
| 30 | `cranelift-frontend-nostd` | `crates/cranelift-frontend-nostd/` | -- | Forked cranelift-frontend for no_std |
| 31 | `cranelift-codegen-shared-nostd` | `crates/cranelift-codegen-shared-nostd/` | -- | Forked cranelift-codegen-shared for no_std |
| 32 | `cranelift-control-nostd` | `crates/cranelift-control-nostd/` | -- | Forked cranelift-control for no_std |
| 33 | `rustc-hash-nostd` | `crates/rustc-hash-nostd/` | -- | Forked rustc-hash for no_std |
| -- | `arbitrary-stub` | `crates/arbitrary-stub/` | -- | no_std stub for arbitrary crate (Cranelift dep) |

### Kernel Modules (17)

| Module | Path | Description |
|--------|------|-------------|
| `acpi_init` | `kernel/src/acpi_init.rs` | ACPI hardware discovery: MADT (CPUs, I/O APICs), FADT (power mgmt, S5 shutdown), HPET (precision timer), MCFG (PCIe ECAM) |
| `smp_init` | `kernel/src/smp_init.rs` | Multi-core boot: reads MADT, disables legacy PIC, boots AP cores via SIPI, enables APIC mode |
| `usb` | `kernel/src/usb.rs` | xHCI controller PCI detection, USB keyboard -> PS/2 scancode bridge, USB mouse polling stub |
| `intel_nic` | `kernel/src/intel_nic.rs` | Intel NIC -> smoltcp Device adapter, page-table virt-to-phys, IntelNetworkStack with DHCP |
| `ssh_server` | `kernel/src/ssh_server.rs` | SSH listener on port 22, TCP session management, version exchange, echo shell, up to 4 sessions |
| `rtc` | `kernel/src/rtc.rs` | CMOS RTC (MC146818), BCD/binary decode, 12h/24h, boot timestamp, PIT-corrected wall clock |
| `mouse` | `kernel/src/mouse.rs` | USB HID boot protocol mouse, XOR crosshair cursor on framebuffer, event queue |
| `ipc` | `kernel/src/ipc.rs` | Message bus (per-agent inboxes), named channels (4K ring buffers), shared memory, 8 tools |
| `init` | `kernel/src/init.rs` | fw_cfg config loading (key=value), hostname, log level, auto-mount, SSH, startup scripts |
| `users` | `kernel/src/users.rs` | User database, SHA-256 password auth (in-kernel impl), SSH public key auth, default "matt" user |
| `sysmon` | `kernel/src/sysmon.rs` | System monitor pane: CPU/memory/network/agent stats, ANSI progress bars, auto-refresh |
| `splash` | `kernel/src/splash.rs` | Boot splash: ASCII art "CLAUDIOOS" logo, 4-stage progress bar (Hardware/Network/Auth/Ready) |
| `boot_sound` | `kernel/src/boot_sound.rs` | PC speaker boot chime: PIT channel 2, C5-E5-G5 ascending triad (523/659/784 Hz) |
| `themes` | `kernel/src/themes.rs` | 9 color themes: default, solarized-dark/light, monokai, dracula, nord, gruvbox, claudioos, templeos |
| `screensaver` | `kernel/src/screensaver.rs` | 5 modes: 3D starfield, matrix rain, bouncing logo, pipes, digital clock. Idle timeout activation |
| `browser` | `kernel/src/browser.rs` | Text-mode web browser pane: wraith HTML/CSS rendering, URL bar, link following, history, scroll |
| `filemanager` | `kernel/src/filemanager.rs` | Visual file manager pane: directory listing, copy/move/rename/delete, search filter, VFS integration |
| `conversations` | `kernel/src/conversations.rs` | Conversation management: list/select/rename/delete via claude.ai REST API, per-agent active conv |
| `session_manager` | `kernel/src/session_manager.rs` | Session auto-refresh: JWT expiry parsing, periodic /api/auth/session refresh, warning thresholds |

## Network Stack

```
claude.ai API / api.anthropic.com
        |
        v
  HTTP/1.1 Client (raw request building, chunked encoding, SSE parsing)
        |
        v
  TLS 1.3 (embedded-tls, AES-128-GCM-SHA256, requires AES-NI)
        |    - 16-byte aligned buffers for AES-NI
        |    - Certificate verification via embedded CA roots
        v
  smoltcp TCP/IP Stack
        |    - DHCP client (10.0.2.x on QEMU SLIRP)
        |    - DNS resolver (10.0.2.3 on QEMU SLIRP)
        |    - TCP sockets with Nagle disabled
        v
  NIC Driver
        |-- VirtIO-net (legacy 0.9.5) for QEMU
        |-- Intel e1000/e1000e/igc for real hardware (via intel_nic module)
        v
  PCI Bus (BAR mapping, bus mastering, IRQ routing)
```

## Design Principles

1. **Single address space** -- No kernel/user boundary, no syscalls, no process isolation. Every agent is an async task.
2. **Interrupt-driven async** -- Hardware interrupts wake futures. `hlt` when idle. No busy-polling.
3. **Everything is no_std** -- All crates use `#![no_std]` with `extern crate alloc`. No libc, no POSIX.
4. **Direct hardware access** -- Volatile MMIO for all device drivers. No HAL abstraction layers.
5. **Minimal dependencies** -- Only well-audited no_std crates from crates.io. Forked when necessary.
6. **Multiple pane types** -- Dashboard supports 6 pane types: Agent, Shell, Browser, FileManager, SysMonitor, Screensaver.
7. **Agent collaboration** -- IPC message bus, named channels, and shared memory let agents communicate and collaborate.
