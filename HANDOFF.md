# Session Handoff — 2026-04-12 (session 2, part 2)

## Last Updated
2026-04-12

## Project Status
🟡 claudio-mux working, DMA fixes committed, real hardware boot BLOCKED at kernel entry

## What Was Done This Session

### claudio-mux (feat/claudio-mux branch, merged to working state)
- Wrote + executed 19-task implementation plan via subagent-driven development
- Created 3 new crates: terminal-core (29 tests), terminal-fb, terminal-ansi (4 tests)
- Migrated kernel dashboard from PrefixState to terminal-core InputRouter
- Built claudio-mux binary — spawns shells, renders output, prefix commands work
- Fixed 9 bugs: DSR response, Ctrl+char encoding, Drop order, F5-F12, double keys, shifted commands, shell default
- Interactive testing: typing works, Ctrl+B s/n/p/q work, status bar renders

### DMA fixes (main branch)
- `f766307` — AHCI DMA: virt_to_phys page table walker, shared kernel/src/memory.rs
- `a9996e9` — xHCI DMA: same pattern, 9 allocation sites fixed
- xHCI bulk API: bulk_in/bulk_out/class_control_request/mass_storage_devices()
- `34aebaa` — USB mass storage wired into disks::init (XhciBulkTransport adapter)
- `941b0bf` — Fixed MSVC linker: LIB env in .cargo/config.toml (VS2022 onecore libs)
- `cargo check` passes cleanly — 551 warnings, 0 errors

### Real hardware boot attempt (HP Victus 15-fa2)
- Built bootable UEFI image via bootloader 0.11 image-builder
- Overcame FAT16 rejection by HP firmware — reformatted USB as FAT32 via diskpart
- Bootloader loads and prints "jumping to kernel entry point at virtaddr 0x8000391720"
- **Kernel hangs immediately** — no colored bars from Phase -2 framebuffer test
- Added visual proof-of-life code (RGB bars) to kernel_main before any other init
- Wrote Python raw-disk FAT I/O tool to update kernel on USB without mounting

### Build tooling fixes
- Image-builder must run outside workspace tree (build-std leaks from parent .cargo/config.toml)
- Command: `export LIB="..." && cd /tmp/image-builder && cargo +nightly run -- <kernel-path>`
- USB flashing: Python raw-disk write to FAT16 clusters on \\.\PHYSICALDRIVE6
- Windows won't mount ESP on removable media — must use raw disk I/O or diskpart (elevated)

## Current State

### Working
- All kernel code compiles (`cargo check` clean)
- claudio-mux binary runs on Windows, spawns shells, renders ANSI
- AHCI/xHCI DMA translation fixed
- USB mass storage pipeline wired (untested on hardware)

### Broken / Blocked
- **Real hardware boot hangs at kernel entry** — bootloader jumps to kernel, but kernel_main never executes (no framebuffer output, no serial). The "jumping to kernel entry point" message comes from the bootloader, not the kernel.
- The colored-bar proof-of-life (first thing in kernel_main, writes directly to bootloader's framebuffer) does NOT appear — meaning kernel_main is never reached OR the bootloader's framebuffer mapping is invalid on this hardware.

## Blocking Issues

### Boot hang on HP Victus 15-fa2 (12th gen Intel)
The bootloader 0.11 UEFI stub loads the kernel from FAT, sets up page tables, and jumps to the entry point. On real hardware, execution never reaches `kernel_main`. Possible causes:

1. **Bootloader page table incompatibility** — bootloader 0.11 may set up page tables that work in QEMU but fault on real hardware (e.g., different NX bit behavior, PAT/MTRR issues with 12th gen Intel)
2. **Kernel entry point issue** — the entry point at 0x8000391720 is in high memory; maybe the jump itself faults
3. **SSE/AVX state** — the bootloader may not initialize FPU/SSE state properly, and any SSE instruction in the kernel prologue would #UD or #GP
4. **Stack setup** — if the bootloader's stack is in memory that becomes invalid after the jump

### Investigation plan for next session
1. **Try QEMU first** — verify the same kernel boots in QEMU (it should, but confirm after all the DMA changes)
2. **Build a minimal kernel** — strip kernel_main down to ONLY the framebuffer write (no SSE, no serial, no alloc) to isolate whether it's our code or the bootloader handoff
3. **Check bootloader 0.11 known issues** — search for real-hardware boot failures with bootloader crate 0.11 on recent Intel hardware
4. **Consider bootloader alternatives** — limine, BOOTBOOT, or bootloader 0.9 which had different page table setup
5. **Add triple-fault handler** — if the kernel is faulting, a triple fault would reboot; adding a basic exception handler before anything else might catch it

## What's Next
1. **Debug boot hang** — minimal kernel test, QEMU verification, bootloader investigation
2. **Add Ctrl+Alt+Del reset** — user requested keyboard reset handler (needs working kernel first)
3. **Tiling layout for claudio-mux** — dwm/awesome-style master+stack (design brainstorm saved in memory)
4. **Merge feat/claudio-mux** — once tiling is done

## Notes for Next Session
- Branch state: `main` has DMA fixes + USB storage. `feat/claudio-mux` has the multiplexer (pushed, not merged).
- Build kernel: `cargo build` from workspace root
- Build image: `export LIB="C:/Program Files/Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/14.43.34808/lib/onecore/x64;C:/Program Files (x86)/Windows Kits/10/Lib/10.0.22621.0/ucrt/x64;C:/Program Files (x86)/Windows Kits/10/Lib/10.0.22621.0/um/x64" && cd /tmp/image-builder && cargo +nightly run -- "J:/baremetal claude/target/x86_64-claudio/debug/claudio-os"`
- Flash USB: use the Python raw-disk FAT writer (Windows won't mount ESP on removable media)
- HP Victus boot: F9 for boot menu, F10 for BIOS setup, Secure Boot must be disabled
- The 128GB SanDisk is at PHYSICALDRIVE6, formatted GPT+FAT32 with EFI/BOOT/BOOTX64.EFI + kernel-x86_64
- Image-builder MUST run from outside the workspace tree (/tmp/image-builder) to avoid build-std conflict
- The "jumping to kernel entry point" address 0x8000391720 is consistent across boots — not a random crash
