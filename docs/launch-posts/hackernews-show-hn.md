# Show HN: ClaudioOS -- A bare-metal Rust OS that runs Claude agents without Linux

**TLDR**: I wrote a bare-metal operating system in Rust that boots via UEFI, brings up networking and TLS from scratch, authenticates to the Anthropic API, and runs multiple AI coding agents simultaneously. No Linux, no POSIX, no libc, no JavaScript runtime. 222K lines of Rust, 38 crates, 29 of them published as standalone no_std libraries.

---

**What it does:**

- Boots on x86_64 UEFI hardware (QEMU or real iron), initializes its own page tables, heap, interrupts, PCI bus, and NIC driver
- Brings up a full TCP/IP stack (smoltcp), does DHCP + DNS, negotiates TLS 1.3 with AES-128-GCM, and makes HTTPS calls to api.anthropic.com
- Runs multiple Claude agent sessions as async tasks, each with its own terminal pane, conversation state, and tool loop (up to 20 rounds of tool use per turn)
- Agents have access to a built-in nano-like editor, a Python interpreter, a Rust compiler (Cranelift backend), and a text-mode web browser

**Technical highlights HN might find interesting:**

- **TLS 1.3 from bare metal.** No OpenSSL, no ring, no OS. We use embedded-tls with AES-NI hardware instructions (requires `-cpu Haswell` in QEMU). The entire chain from raw Ethernet frames to encrypted HTTPS is ours.
- **Linux binary compatibility.** An ELF loader + syscall translation layer handles 150+ Linux syscalls (file I/O, mmap, sockets, signals, epoll). The goal is running static Linux binaries without Linux.
- **Post-quantum SSH daemon.** ML-KEM-768 + X25519 hybrid key exchange, ML-DSA-65 + Ed25519 dual host keys. 4,191 lines. (Crypto is still placeholder -- real curve ops not wired yet. Marked clearly in code.)
- **Cranelift on bare metal.** We forked 6 Cranelift crates to compile under `no_std`, giving agents the ability to compile and run Rust code without any host OS.
- **NVIDIA GPU scaffolding.** Bare-metal GPU compute driver targeting nouveau-style MMIO register access, Falcon microcontroller firmware, GPFIFO channels, and tensor operations. Not yet functional, but the architecture is laid out.
- **29 standalone crates** published as reusable `no_std` libraries: ext4, btrfs, NTFS filesystems; AHCI, NVMe, xHCI, Intel NIC, WiFi, Bluetooth drivers; a VFS layer; a Python interpreter; a JavaScript evaluator; an SSH daemon; and more.

**The most interesting crates** (all `#![no_std]`, all usable outside ClaudioOS):

| Crate | What it does |
|-------|-------------|
| sshd-pqc | Post-quantum SSH server (ML-KEM-768 + X25519, 4,191 lines) |
| gpu-compute-nostd | Bare-metal NVIDIA GPU (MMIO, Falcon, GPFIFO, tensors, 3,392 lines) |
| linux-compat-nostd | Linux syscall translation layer (4,090 lines) |
| btrfs-nostd | Read-write btrfs with COW semantics (4,006 lines) |
| js-lite | JavaScript evaluator for Cloudflare challenge solving (5,229 lines) |
| wifi-nostd | Intel AX201/AX200 WiFi with WPA2/WPA3 (3,513 lines) |
| xhci-nostd | USB 3.0 host controller + HID keyboard (4,204 lines) |
| python-lite | Python interpreter with vars, loops, functions (2,388 lines, 28 tests) |
| wraith-dom | no_std HTML parser + CSS selectors (2,070 lines, 32 tests) |
| shell-nostd | AI-native shell with 45+ builtins and natural language mode (2,884 lines) |

**Links:**

- GitHub: https://github.com/suhteevah/claudio-os
- Website: https://claudioos.vercel.app
- Architecture doc: https://github.com/suhteevah/claudio-os/blob/main/docs/ARCHITECTURE.md
- Open-source crates list: https://github.com/suhteevah/claudio-os/blob/main/docs/OPEN-SOURCE-CRATES.md

**The "built with Claude" angle:**

There is a certain irony here. A significant portion of ClaudioOS was built with the help of Claude (Opus/Sonnet), and the entire purpose of the OS is to run Claude agents on bare metal. An AI helped build the OS that exists to run AIs. The ouroboros is intentional.

**Why build this?**

I wanted to run multiple Claude coding agents on dedicated hardware without the overhead of a general-purpose OS. Linux is 30 million lines of C designed to do everything. ClaudioOS is 222K lines of Rust designed to do one thing: boot, authenticate, and run agents. Sub-2-second boot to a functional shell. Under 32 MB memory footprint idle.

The Anthropic Max subscription gives you access to Claude through the API. ClaudioOS is designed to make that subscription maximally useful -- multiple agents, each with their own tools, running on hardware you own, accessible over SSH.

**Ask me anything.** I'm Matt (suhteevah), a systems Rust developer at Ridge Cell Repair LLC. Happy to talk about the architecture, the no_std war stories, the TLS debugging, or anything else.
