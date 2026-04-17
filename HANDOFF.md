# Session Handoff — 2026-04-17

## Last Updated
2026-04-17

## Project Status
🟢 **ClaudioOS boots on real hardware (HP Victus 15-fa2).** Limine + heap + VFS + ACPI + framebuffer + terminal + keyboard echo all working end-to-end. xHCI, SMP, and networking gated off on real HW pending driver hardening.

## What Was Done This Session

### Bootloader migration: `bootloader` 0.11 → Limine (shipped)
- New files: `kernel/linker.ld`, `kernel/build.rs`, `limine.conf`
- `Cargo.toml` + `kernel/Cargo.toml`: swap `bootloader_api` → `limine = "0.5"`
- `kernel/src/main.rs`: replaced `entry_point!` with `_start`, added static Limine request tags (BaseRevision + StackSize + Framebuffer + Hhdm + MemoryMap + Rsdp + Module) in `.requests` / `.requests_start_marker` / `.requests_end_marker` sections
- `kernel/src/memory.rs`: rewrote `BootInfoFrameAllocator` to iterate Limine memmap entries instead of `bootloader_api::MemoryRegions`
- `kernel/src/framebuffer.rs`: takes a `LimineFramebufferInfo` struct now
- `kernel/src/acpi_init.rs`: **dropped the `+ phys_offset` on the RSDP address** — Limine returns HHDM-mapped virtual addresses, not physical like bootloader 0.11 did. Double-offset was the silent page-fault on real HW.

### Real-hardware debugging + fixes
- `x86_64-claudio.json`: flipped `position-independent-executables` and `static-position-independent-executables` to `false`. The rust toolchain was emitting ET_DYN ELFs without a PT_DYNAMIC segment, which Limine rejects. Now ET_EXEC.
- `kernel/src/main.rs`: moved SSE/XSAVE/AVX enable from Phase 0 into `_start` BEFORE any Rust code that might emit SSE instructions (Limine `Framebuffer` accessors triggered #UD because they emit FP register ops).
- `kernel/src/framebuffer.rs`: removed "clear front buffer to black" on init — it wiped the Phase -2 proof-of-life bars, making a black screen indistinguishable from "kernel died silently."
- Added a visual `fb_checkpoint(n, color)` debug system: paints small squares in a row below the rainbow bars, one per phase checkpoint. Color indicates phase family (white = early init, cyan = LLM loader, yellow = ACPI/FB, magenta = inside ACPI, green = post-splash, orange = network). Real-HW boot is silent (no serial), so this is how we trace progress.

### Real-HW gates applied (still work in QEMU, disabled on laptop)
- **Phase 3c3 VFS model fallback** — `claudio_fs::read_file("/claudio/models/default.gguf")` deadlocks on real HW. Now only tries the Limine module path.
- **USB/xHCI (`usb::init`, `mouse::init`)** — guarded by `USB_ON_REAL_HW = false`. xHCI init hangs on 12th-gen Intel. TODO: proper port reset + event-ring handling.
- **SMP (`smp_init::init`)** — guarded by `SMP_ON_REAL_HW = false`. 12th-gen hybrid P-core + E-core + INIT-SIPI-SIPI trampoline doesn't survive real silicon.
- Fallback keyboard-echo terminal in `main_async` now calls `framebuffer::blit_full()` after `layout.render_all()` — without it the back-buffered text never reached the visible front buffer.

### Tools + image pipeline
- `tools/image-builder/src/main.rs` — **critical fix**: the shipped Limine `v7.x-binary` BOOTX64.EFI actually reads **v6-syntax `limine.cfg`** (uppercase `KEY=VALUE`, resource URIs like `boot:///kernel.elf`), not the v7 `limine.conf` / `key: value` the docs imply. Also forces `fatfs::FatType::Fat32` explicitly and bumps minimum volume size to 128 MiB (below ~66 MiB fatfs auto-selected FAT16 which Limine couldn't scan).
- `tools/image-builder/.cargo/config.toml` — target `x86_64-pc-windows-gnu` (msvc toolchain isn't installed on kokonoe).
- `tools/flash-usb.py` — new Win32 raw-disk flasher. Useless for the SanDisk Cruzer Glide because Windows re-auto-mounts the partition mid-write and ERROR_ACCESS_DENIED's us. Kept around; the working flow ended up being `diskpart` → create small FAT32 partition → copy files onto mounted drive letter.

### Working boot proof on Victus
- Limine banner renders, kernel loads, handoff reaches `_start`
- 6 rainbow bars at top (Phase -2 proof-of-life) ✓
- 11 white squares (Phase 0a–3c2) ✓
- 3 cyan squares (Phase 3c3, with expected gap because Limine had no modules) ✓
- 1 yellow (ACPI entered) ✓ — hung here on first boot due to the `+ phys_offset` bug; fixed
- 5 magenta (ACPI sub-steps) ✓ after RSDP fix
- ClaudioOS splash with "CLAUDIO OS / Bare Metal AI Agent Platform" + progress bar ✓
- 9 orange squares (post-splash, through network) ✓
- Fallback terminal: "ClaudioOS v0.1.0 — Bare Metal AI Agent Terminal" prompt + keyboard echo ✓

## Current State

### Working on real HW (HP Victus)
- Full bootloader → kernel handoff via Limine
- Heap, IDT, keyboard decoder, RTC, CSPRNG, ACPI, framebuffer, splash
- Fallback terminal with live keyboard echo

### Working in QEMU but disabled on real HW
- USB / xHCI
- SMP (multi-core boot)
- VFS `read_file` fallback on MemFs

### Not working on any target on the Victus specifically
- Networking — Victus has no wired Ethernet, only Intel AX Wi-Fi which we have no driver for

### Unchanged / still WIP
- Everything that depends on networking (OAuth, API client, agent dashboard) — reaches a "no NIC" fallback
- GPU — no driver work done

## Blocking Issues

None as of this session. Each gated driver is a known TODO, not a blocker.

## What's Next

**#1 user priority: SSH + Wi-Fi so you can SSH into the device and iterate on it in place.**

Ordered plan:
1. **Intel AX Wi-Fi driver** — this is the big one. AX201/AX211 is proprietary; needs firmware blob, 802.11ax MAC, association state machine. Multi-week effort. Short-term alternative: USB Ethernet dongle once xHCI is working.
2. **xHCI hardening** — real-HW port reset sequence, interrupt-driven event ring. Required for any USB device (keyboard/mouse/ethernet-dongle) on machines without PS/2.
3. **SSH server** — `crates/sshd` already exists. Wire it up once networking works.
4. **SMP rework** — 12th-gen hybrid topology. Not urgent but blocks any parallel workload.
5. **VFS read_file deadlock on real HW** — low priority, only blocks model loading.
6. **Merge `feat/claudio-mux` to main** (pending from prior session).

## Notes for Next Session

- **Use the `fb_checkpoint` debug system.** It's the only way to trace boot on real HW. Serial output goes nowhere on the Victus. Add checkpoints liberally when probing new driver paths. Colors are just visual grouping — pick whatever.
- **Limine `v7.x-binary` = v6 syntax.** Don't trust the Limine v7 docs that tell you to use `limine.conf` + `key: value`; the actual binary is older. If you upgrade the Limine binary tree make sure to re-test the config format.
- **RSDP address from Limine is HHDM-virtual, not physical.** Any ACPI-ish code that used to do `addr + phys_offset` on a bootloader-0.11-provided value is now broken in the exact same way — grep for that pattern if similar issues appear.
- **Windows auto-mount fights raw USB writes.** The `tools/flash-usb.py` path is fragile. Working flow is: `diskpart` → `select disk 6` → `clean` → `create partition primary size=512` → `format fs=fat32 quick` → `assign letter=V` → copy files onto `V:`. USB shows up on HP firmware as a boot device that lands you in a file picker; navigate to `/EFI/BOOT/BOOTX64.EFI` manually.
- **Image-builder runs from `/tmp/claudio-image-builder`** per the previous handoff (build-std leaks from the workspace config). Still true — `cp -r tools/image-builder /tmp/claudio-image-builder` then `LIMINE_DIR=/tmp/limine cargo run --release -- <kernel-elf>`.
- **Limine binaries** live at `/tmp/limine` (cloned from `v7.x-binary` branch). Version 7.13.3.
- **The rainbow bars survive until splash overdraws them.** That's by design — if you ever see the rainbow with no splash, splash didn't run.
- **SanDisk USB = PHYSICALDRIVE6.** GPT partition, 512 MiB FAT32 partition at the front labelled `CLAUDIO`. Drive letter `V:` when mounted.
- QEMU test command: `timeout 60 "C:/Program Files/qemu/qemu-system-x86_64.exe" -drive if=pflash,format=raw,readonly=on,file=/tmp/ovmf-code.fd -drive "format=raw,file=<img>" -display none -serial stdio -m 2G -cpu Haswell -no-reboot`
- QEMU screendump for visual debugging: `-vnc :5 -qmp tcp:127.0.0.1:PORT,server=on,wait=off` then drive `screendump` via QMP JSON.
