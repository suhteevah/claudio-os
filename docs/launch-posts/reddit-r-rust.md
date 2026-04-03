# r/rust Post

**Subreddit**: r/rust
**Title**: I built a bare-metal OS in Rust that runs Claude agents -- 222K lines, 38 crates, no Linux

---

**TLDR**: ClaudioOS is a bare-metal Rust OS (`#![no_std]` everywhere) that boots via UEFI and runs AI coding agents directly on hardware. 222K lines of Rust across 38 workspace crates, 29 published as standalone reusable `no_std` libraries. No Linux, no libc, no POSIX, no JavaScript runtime.

---

Hey r/rust,

I have been building a bare-metal operating system in Rust for the past several months and wanted to share it with the community. ClaudioOS boots on x86_64 UEFI machines, brings up its own networking stack and TLS, authenticates to the Anthropic API, and runs multiple Claude coding agents simultaneously -- all without a host OS underneath.

## Why Rust was the only option

This project would not exist in C. The combination of `no_std` + `alloc`, the trait system, and async/await made it possible for a single developer to build a functioning OS with TLS, filesystems, device drivers, and language interpreters without losing my mind to memory bugs.

Some specific wins:

- **`no_std` + `alloc` is a superpower.** Every one of the 38 crates compiles for `x86_64-unknown-none`. The `alloc` crate gives you Vec, String, BTreeMap, and Box without pulling in std. This is the foundation of everything.
- **Traits for hardware abstraction.** Every driver implements a `BlockDevice` or `NicBackend` trait. Swap VirtIO for Intel NIC? Change one line. The filesystem crates do not care what disk they sit on.
- **Async without a runtime.** The kernel has a custom interrupt-driven async executor. Hardware interrupts (NIC rx, keyboard, timer) wake futures. No tokio, no polling, `hlt` when idle. Rust's async state machines compile down to exactly what you want on bare metal.
- **The type system catches real bugs.** Volatile MMIO access is wrapped in types that prevent misuse. Syscall argument validation is enforced at the type level. The borrow checker prevents double-free of DMA buffers.

## The 29 published crates

Every standalone crate is `#![no_std]` and designed to be usable outside ClaudioOS in any bare-metal or embedded Rust project:

**Filesystems:** ext4 (3,013 lines), btrfs with COW (4,006 lines), NTFS (3,561 lines), VFS layer (2,871 lines)

**Hardware drivers:** AHCI/SATA (2,139), NVMe (2,563), Intel NIC e1000/I219/I225 (1,986), WiFi AX201/AX200 (3,513), Bluetooth HCI/L2CAP (3,075), USB mass storage (1,357), xHCI USB 3.0 (4,204), ACPI tables (2,433), HD Audio (2,631), SMP/APIC multicore (3,391), NVIDIA GPU compute (3,392), ELF loader (1,213), Linux syscall compat (4,090)

**Networking/Security:** Post-quantum SSH daemon with ML-KEM-768 (4,191), smoltcp + TLS network stack (3,172)

**Languages/Tools:** Python interpreter (2,388, 28 tests), JavaScript evaluator (5,229), Rust compiler via Cranelift (142), text editor (534, 11 tests), AI-native shell (2,884), terminal renderer (2,930), agent lifecycle (501)

**Web/Browser:** HTML parser + CSS selectors (2,070, 32 tests), HTML-to-text renderer (1,225, 12 tests), HTTP/HTTPS client (572)

## Community contribution angle

These crates exist because the Rust `no_std` ecosystem still has gaps. If you are building a hobby OS, an embedded system, or a unikernel and you need a `no_std` ext4 driver, or an NVMe driver, or a WiFi stack -- these are yours to use. MIT + Apache-2.0 dual license on the standalone crates (kernel itself is AGPL).

I would love contributions, especially:
- Testing on real hardware (I have been doing most dev in QEMU)
- Improving the filesystem crates with journaling/crash recovery
- Wiring real curve25519 crypto into the SSH daemon (currently placeholder)
- Better error handling patterns (some crates still use strings for errors)

## The Cranelift fork story

Getting Cranelift to compile under `no_std` required forking 6 crates (cranelift-codegen, cranelift-frontend, cranelift-codegen-shared, cranelift-control, rustc-hash, arbitrary) and patching them. Build scripts do post-processing to replace `std::` with `core::` in generated code. It works. It is not pretty. If anyone from the Cranelift team is reading this and wants to discuss upstreaming `no_std` support, I am very interested.

## Links

- **GitHub**: https://github.com/suhteevah/baremetal-claude
- **Website**: https://claudioos.vercel.app
- **Crate catalog**: https://github.com/suhteevah/baremetal-claude/blob/main/docs/OPEN-SOURCE-CRATES.md
- **Architecture**: https://github.com/suhteevah/baremetal-claude/blob/main/docs/ARCHITECTURE.md

Happy to answer any questions about the architecture, specific crate implementations, or `no_std` war stories. This has been the most rewarding Rust project I have ever worked on.
