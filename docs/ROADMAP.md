---
name: ClaudioOS feature roadmap
description: Complete feature checklist for making ClaudioOS a real OS — track what's done and what's next
type: project
---

# ClaudioOS Feature Roadmap

## DONE
- [x] UEFI boot
- [x] GDT/IDT/PIC interrupts
- [x] Heap allocator (48 MiB)
- [x] PS/2 keyboard
- [x] Framebuffer (2560x1600, Terminus font, double-buffered, dirty regions)
- [x] VirtIO-net driver
- [x] smoltcp TCP/IP (DHCP, DNS)
- [x] TLS 1.3 (embedded-tls, AES-128-GCM)
- [x] HTTP/HTTPS client
- [x] claude.ai OAuth (email + code, source:"claude", anthropic-* headers)
- [x] claude.ai Max subscription chat API
- [x] Session persistence (fw_cfg, 28-day sessionKey)
- [x] Conversation reuse across reboots
- [x] Multi-agent dashboard (tmux-style split panes)
- [x] Agent tool loop (20 rounds max)
- [x] Python interpreter (python-lite)
- [x] JavaScript interpreter (js-lite)
- [x] Rust compiler (rustc-lite + Cranelift JIT)
- [x] Text editor (nano-like)
- [x] Wraith browser (DOM parser, transport, text renderer)
- [x] Cloudflare challenge solver
- [x] ext4 filesystem
- [x] btrfs filesystem
- [x] NTFS filesystem
- [x] AHCI/SATA driver
- [x] NVMe driver
- [x] Intel NIC driver (e1000/I219/I225)
- [x] xHCI USB 3.0 + HID keyboard
- [x] ACPI (RSDP/MADT/FADT/MCFG/HPET, shutdown, reboot)
- [x] HDA audio (PCM playback)
- [x] SMP multi-core (APIC, trampoline, work-stealing scheduler)
- [x] GPU compute (NVIDIA, Falcon, FIFO, tensor ops)
- [x] VFS layer (mount table, GPT/MBR, POSIX file API)
- [x] AI-native shell (28 builtins + natural language -> Claude)
- [x] Post-quantum SSH daemon (ML-KEM-768 + X25519, ML-DSA-65)
- [x] ACPI wired into boot sequence (hardware discovery, MADT->SMP, FADT->power, HPET->timer, MCFG->PCIe)
- [x] SMP wired to boot sequence (MADT-driven AP boot, APIC mode, work-stealing scheduler)
- [x] USB keyboard wired to dashboard (xHCI -> PS/2 scancode queue bridge)
- [x] Intel NIC wired to smoltcp (E1000 Device adapter, DHCP, full TCP/IP stack)
- [x] SSH server wired to smoltcp TCP (port 22 listener, session state machines, echo shell)
- [x] RTC wall clock (CMOS MC146818, BCD/binary, 12h/24h, PIT-corrected uptime)
- [x] USB mouse support (HID boot protocol, XOR crosshair cursor, event queue)
- [x] Inter-agent IPC (message bus, named channels, shared memory, 8 agent tools)
- [x] Init system (fw_cfg config loading, hostname, log level, auto-mount, startup scripts)
- [x] User accounts (SHA-256 password auth, SSH public key auth, user database)
- [x] System monitor pane (CPU/memory/network/agent stats, auto-refresh)
- [x] Boot splash screen (ASCII art logo, 4-stage progress bar)
- [x] Boot chime (PC speaker, C5-E5-G5 ascending triad)
- [x] Color themes (9 built-in: default, solarized-dark/light, monokai, dracula, nord, gruvbox, claudioos, templeos)
- [x] Screensaver (5 modes: starfield, matrix rain, bouncing logo, pipes, clock)
- [x] Web browser pane (wraith-based, URL bar, link following, history, scroll)
- [x] File manager pane (directory listing, navigation, copy/move/rename/delete/search)
- [x] Conversation management (list, select, rename, delete conversations via claude.ai API)
- [x] Session auto-refresh (JWT expiry parsing, automatic /api/auth/session refresh, warning thresholds)

## TODO -- Critical (can't ship without)
- [ ] Wire VFS to real storage drivers (AHCI/NVMe + ext4/btrfs)
- [ ] Boot on real hardware (i9-11900K test first)
- [ ] Fix keyboard input in QEMU graphical mode

## TODO -- Important (makes it usable daily)
- [ ] Agent naming (Ctrl+B , to rename)
- [ ] Config file persistence to disk (FAT32 or ext4 on disk)
- [ ] Log file output (serial to file, or disk-backed)
- [ ] Wire SSH shell to real shell crate (currently echo shell)
- [ ] Authorized keys management CLI
- [ ] Full pane integration for SSH (route SSH channel I/O to dashboard terminal panes)

## TODO -- Killer features (differentiators)
- [ ] Claude as shell (natural language -> executed commands)
- [ ] GPU LLM inference (run local models on RTX 3070 Ti)
- [ ] Live code reload (Cranelift JIT hot-swap)
- [ ] Boot from USB stick
- [ ] PXE network boot
- [ ] Wi-Fi driver (Intel AX201)

## TODO -- Polish
- [ ] Better font rendering (anti-aliased if GPU available)
- [ ] Tab completion for file paths and command names

## Published Open-Source Crates (19)
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
