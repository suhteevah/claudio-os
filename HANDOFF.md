# Session Handoff — 2026-04-12 (session 2, final)

## Last Updated
2026-04-12

## Project Status
🟡 claudio-mux working, DMA fixes shipped, real hardware boot blocked (limine switch planned), LLM training queued

## What Was Done This Session

### claudio-mux (feat/claudio-mux branch, pushed)
- Wrote + executed 19-task implementation plan via subagent-driven development
- Created 3 new crates: terminal-core (29 tests), terminal-fb, terminal-ansi (4 tests)
- Migrated kernel dashboard to terminal-core InputRouter
- Built working Windows terminal multiplexer with ConPTY, ANSI rendering, prefix commands
- Fixed 9 bugs including DSR response (shells block on ESC[6n), shifted command keys, double key events
- User tested interactively: Ctrl+B s/n/p/q work, shell renders, status bar works
- Known issue: tiling layout degenerates after 3-4 splits (needs dwm-style redesign)

### DMA fixes (main branch, pushed)
- AHCI: virt_to_phys page table walker for all 6 DMA sites + shared kernel/src/memory.rs
- xHCI: same pattern for all 9 DMA allocation sites
- xHCI bulk API: bulk_in/bulk_out/class_control_request/mass_storage_devices()
- USB mass storage: XhciBulkTransport adapter, wired into disks::init registry
- MSVC linker fix: LIB env in .cargo/config.toml (VS2022 onecore libs)
- `cargo check` passes cleanly (551 warnings, 0 errors)

### Real hardware boot attempt (HP Victus 15-fa2)
- Built UEFI image, overcame FAT16 rejection (HP needs FAT32)
- Bootloader loads kernel, prints "jumping to kernel entry point" then hangs
- Added RGB proof-of-life bars — don't appear (kernel_main never reached)
- **Verified kernel boots fine in QEMU** — serial output, SSE, GDT all work
- Root cause: bootloader crate 0.11 page table setup fails on 12th gen Intel
- **Decision: switch to limine bootloader** (~100 lines of glue, all kernel code stays)

### Build tooling
- Image-builder must run outside workspace tree (/tmp/image-builder) due to build-std leak
- Python raw-disk FAT writer for USB flashing (Windows won't mount ESP on removable media)
- QEMU verified working: `edk2-x86_64-code.fd` as pflash, -nographic for serial

## Current State

### Working
- All kernel code compiles (cargo check clean)
- Kernel boots in QEMU (serial output confirmed)
- claudio-mux binary on Windows (shell rendering, prefix commands, status bar)
- AHCI/xHCI DMA fixes committed
- USB mass storage pipeline wired

### Not Working
- Real hardware boot (bootloader 0.11 incompatible with HP Victus / 12th gen Intel)

### Existing LLM crate
- `crates/llm/` — 2,692 lines: GGUF loader, tokenizer, transformer, tensor ops, sampler
- `no_std` compatible, designed for bare-metal inference
- Used by agent_loop for local model fallback

## Blocking Issues
- Real hardware boot: needs limine switch (not bootloader 0.11 bug fix)

## What's Next
1. **LLM training for ClaudioOS** — user's next priority. Has existing inference crate (crates/llm/). Need to discuss: fine-tune existing model vs train from scratch, target hardware, dataset
2. **Limine bootloader switch** — ~100 lines, unblocks real hardware boot
3. **Tiling layout for claudio-mux** — dwm/awesome-style master+stack
4. **Merge feat/claudio-mux to main**

## Notes for Next Session
- Branch state: `main` has DMA fixes + USB storage + debug bars. `feat/claudio-mux` has multiplexer (pushed, not merged)
- QEMU test: `timeout 45 "C:/Program Files/qemu/qemu-system-x86_64.exe" -drive "if=pflash,format=raw,readonly=on,file=/tmp/ovmf-code.fd" -drive "format=raw,file=<img>" -serial stdio -m 512M -nographic`
- QEMU OVMF: copy `C:/Program Files/qemu/share/edk2-x86_64-code.fd` to `/tmp/ovmf-code.fd`
- USB is PHYSICALDRIVE6 (SanDisk 128GB), formatted GPT+FAT32
- The LLM crate already loads GGUF models and does inference — question is what model to train/fine-tune
- User prefers subagent-driven development (memory: feedback_subagents.md)
- Keep verbose logging, output to scratch/ files not chat (memory: feedback_scratchpad.md)
