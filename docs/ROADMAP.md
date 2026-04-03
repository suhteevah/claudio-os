---
name: ClaudioOS feature roadmap
description: Complete feature checklist for making ClaudioOS a real OS — track what's done and what's next
type: project
---

# ClaudioOS Feature Roadmap

## DONE — Core OS
- [x] UEFI boot
- [x] GDT/IDT/PIC interrupts
- [x] Heap allocator (48 MiB)
- [x] PS/2 keyboard
- [x] Framebuffer (2560x1600, Terminus font, double-buffered, dirty regions)
- [x] VirtIO-net driver
- [x] smoltcp TCP/IP (DHCP, DNS)
- [x] TLS 1.3 (embedded-tls, AES-128-GCM)
- [x] HTTP/HTTPS client
- [x] PCI bus enumeration + device discovery

## DONE — Authentication & Sessions
- [x] claude.ai OAuth (email + code, source:"claude", anthropic-* headers)
- [x] claude.ai Max subscription chat API
- [x] Session persistence (fw_cfg, 28-day sessionKey)
- [x] Conversation reuse across reboots
- [x] Session auto-refresh (JWT expiry parsing, automatic /api/auth/session refresh, warning thresholds)
- [x] Conversation management (list, select, rename, delete conversations via claude.ai API)

## DONE — Multi-Agent System
- [x] Multi-agent dashboard (tmux-style split panes)
- [x] Agent tool loop (20 rounds max)
- [x] 6 pane types (Agent, Shell, Browser, FileManager, SysMonitor, Screensaver)
- [x] Inter-agent IPC (message bus, named channels, shared memory, 8 agent tools)

## DONE — Languages & Dev Tools
- [x] Python interpreter (python-lite, 28 tests)
- [x] JavaScript interpreter (js-lite)
- [x] Rust compiler (rustc-lite + Cranelift JIT)
- [x] Text editor (nano-like, 11 tests)
- [x] Wraith browser (DOM parser, transport, text renderer)
- [x] Cloudflare challenge solver

## DONE — Filesystems & Storage
- [x] ext4 filesystem
- [x] btrfs filesystem
- [x] NTFS filesystem
- [x] VFS layer (mount table, GPT/MBR, POSIX file API)
- [x] AHCI/SATA driver
- [x] NVMe driver
- [x] USB mass storage (BOT protocol + SCSI command set)
- [x] Disk encryption (LUKS-compatible encryption layer)
- [x] Swap management (virtual memory swap to disk)

## DONE — Hardware Drivers
- [x] Intel NIC driver (e1000/I219/I225)
- [x] WiFi driver (Intel AX201/AX200, WPA2/WPA3, scanning, association)
- [x] Bluetooth stack (HCI/L2CAP/GAP/GATT, USB transport, HID devices)
- [x] xHCI USB 3.0 + HID keyboard
- [x] USB mouse support (HID boot protocol, XOR crosshair cursor, event queue)
- [x] USB touchpad (PS/2 and USB driver, gesture recognition)
- [x] ACPI (RSDP/MADT/FADT/MCFG/HPET, shutdown, reboot)
- [x] HDA audio (PCM playback)
- [x] SMP multi-core (APIC, trampoline, work-stealing scheduler)
- [x] GPU compute (NVIDIA, Falcon, FIFO, tensor ops)
- [x] RTC wall clock (CMOS MC146818, BCD/binary, 12h/24h, PIT-corrected uptime)
- [x] PC speaker boot chime

## DONE — Networking & Security
- [x] Post-quantum SSH daemon (ML-KEM-768 + X25519, ML-DSA-65)
- [x] SSH server wired to smoltcp TCP (port 22 listener, session state machines)
- [x] Intel NIC wired to smoltcp (E1000 Device adapter, DHCP, full TCP/IP stack)
- [x] Firewall (stateful packet filtering, allow/deny rules, IP/port filtering)
- [x] Network tools: ping, wget, curl, netstat, ifconfig, dns, traceroute, nslookup

## DONE — Shell & CLI
- [x] AI-native shell (45+ builtins + natural language -> Claude)
- [x] Pipes and pipeline execution
- [x] Environment variables with expansion
- [x] Shell scripting (if/for/while)
- [x] Theme command (9 built-in color themes, runtime switching)
- [x] Screensaver command (5 modes)
- [x] Conversation management commands
- [x] IPC commands (/msg, /broadcast, /inbox, /agents, /channel)
- [x] Network tools: ping, wget, curl, netstat, ifconfig, dns, traceroute, nslookup
- [x] System tools: crontab, cryptsetup, swapon, fw, man, battery, suspend

## DONE — System Services
- [x] Init system (fw_cfg config loading, hostname, log level, auto-mount, startup scripts)
- [x] User accounts (SHA-256 password auth, SSH public key auth, user database)
- [x] Cron scheduler (periodic task execution, crontab-style scheduling)
- [x] Virtual consoles (multiple independent terminal sessions, Ctrl+Alt+F1-F6)
- [x] Clipboard (system-wide copy/paste buffer shared across panes)
- [x] Power management (ACPI S3/S5 suspend/resume, battery monitoring)
- [x] Man pages (built-in manual pages for all commands)

## DONE — UI & Polish
- [x] Boot splash screen (ASCII art logo, 4-stage progress bar)
- [x] Boot chime (PC speaker, C5-E5-G5 ascending triad)
- [x] Color themes (9 built-in: default, solarized-dark/light, monokai, dracula, nord, gruvbox, claudioos, templeos)
- [x] Screensaver (5 modes: starfield, matrix rain, bouncing logo, pipes, clock)
- [x] Web browser pane (wraith-based, URL bar, link following, history, scroll)
- [x] File manager pane (directory listing, navigation, copy/move/rename/delete/search)
- [x] System monitor pane (CPU/memory/network/agent stats, auto-refresh)

## TODO — Hardware Integration
- [ ] Wire VFS to real storage drivers (AHCI/NVMe + ext4/btrfs)
- [ ] Boot on real hardware (i9-11900K test first)
- [ ] Fix keyboard input in QEMU graphical mode
- [ ] Wire SSH shell to real shell crate (currently echo shell)

## TODO — Killer Features
- [ ] GPU LLM inference (run local models on RTX 3070 Ti)
- [ ] Live code reload (Cranelift JIT hot-swap)
- [ ] Boot from USB stick
- [ ] PXE network boot

## TODO — Polish
- [ ] Better font rendering (anti-aliased if GPU available)
- [ ] Tab completion for file paths and command names

## Published Open-Source Crates (22)
1. ext4-rw
2. btrfs-nostd
3. ntfs-rw
4. js-lite
5. python-lite
6. rustc-lite
7. wraith-dom
8. wraith-render
9. wraith-transport
10. ahci-nostd
11. nvme-nostd
12. intel-nic-nostd
13. xhci-nostd
14. acpi-nostd
15. hda-nostd
16. smp-nostd
17. gpu-compute-nostd
18. sshd-pqc
19. editor-nostd
20. wifi-nostd
21. bluetooth-nostd
22. usb-storage-nostd
