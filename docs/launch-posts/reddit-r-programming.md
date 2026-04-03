# r/programming Post

**Subreddit**: r/programming
**Title**: Why I replaced Linux with 222K lines of Rust for AI workloads

---

**TLDR**: I built a bare-metal OS in Rust that boots in under 2 seconds, uses less than 32 MB of RAM, and exists for one purpose: running multiple AI coding agents on dedicated hardware. No Linux, no containers, no VMs. Here is why, and what I learned.

---

## The problem

I wanted to run multiple Claude (Anthropic) coding agents on dedicated hardware. The standard approach is: Linux + Docker + Node.js + some orchestration layer. That works, but it felt absurd. I am running a text-based API client that sends JSON and receives SSE streams. Why do I need 30 million lines of kernel code, a container runtime, a JavaScript engine, and 200 MB of base memory for that?

## The experiment

What if I wrote an OS that does exactly one thing? Boot, get a network connection, authenticate to the Anthropic API, and run agents. Nothing else.

That experiment became ClaudioOS: 222K lines of Rust, 38 crates, all `#![no_std]`. It boots via UEFI, initializes its own hardware (page tables, heap, interrupts, PCI, NIC), brings up TCP/IP with DHCP and DNS, negotiates TLS 1.3, and starts streaming Claude responses -- all on bare metal.

## What "no Linux" actually means

Every layer that Linux provides, I had to build or integrate:

| What Linux gives you | What ClaudioOS does |
|---------------------|-------------------|
| Bootloader (GRUB) | bootloader crate v0.11 (UEFI) |
| Memory management | linked_list_allocator, 48 MiB heap, manual page tables |
| Interrupts | Custom GDT/IDT/PIC, IRQ handlers in Rust |
| NIC driver | VirtIO-net driver, Intel e1000e driver |
| TCP/IP stack | smoltcp (pure Rust, no_std) |
| TLS | embedded-tls (TLS 1.3, AES-128-GCM, AES-NI) |
| Filesystem | FAT32, ext4, btrfs, NTFS -- all from scratch |
| Process model | Async tasks on custom executor (no processes) |
| Terminal | Framebuffer renderer with split panes, VTE parser |
| Shell | Custom shell with 45+ builtins |

## The results

**Boot time**: Under 2 seconds from UEFI to a functional shell with networking. A minimal Linux (Alpine, no services) takes 5-10 seconds.

**Memory**: Under 32 MB idle. Minimal Linux idles at 100-200 MB.

**Attack surface**: 222K lines of Rust vs. 30 million lines of C. Rust's memory safety eliminates buffer overflows, use-after-free, and most of the CVE classes that plague C kernels.

**No syscall overhead**: Single address space means function calls, not context switches. An API call goes from userspace logic directly to the TLS layer directly to the NIC driver. No copying between kernel and user buffers.

## What I actually learned (the honest version)

**1. Linux is incredibly well-designed.** Implementing 150+ syscalls from the other side gave me a deep appreciation for how composable and consistent the POSIX interface is. Every shortcut I tried to take eventually led me back to "oh, this is why Linux does it that way."

**2. TLS from bare metal is brutal.** The handshake alone requires: a working TCP stack, a source of randomness, big-integer arithmetic, AES hardware instructions, and correct handling of fragmented records. One wrong byte and the server hangs up silently. Debugging this without strace or Wireshark (inside the guest) was the hardest part of the project.

**3. The Rust `no_std` ecosystem is better than you think.** smoltcp, embedded-tls, fatfs, vte, serde_json -- all compile cleanly for `x86_64-unknown-none`. The gap is narrowing fast. Where it falls short (Cranelift, for example), forking and patching is feasible if tedious.

**4. An OS is really a collection of drivers.** The "kernel" is maybe 5K lines. The other 217K lines are drivers, protocol implementations, and application logic. The interesting engineering is not in the page tables -- it is in making TLS work, parsing SSE streams, and handling VirtIO DMA correctly.

**5. "Just use Linux" is usually the right answer.** ClaudioOS exists because I wanted to learn, and because dedicated AI agent hardware is a niche where the tradeoffs work. For almost any other use case, Linux is the correct choice. I am not trying to replace it.

## The meta angle

A significant portion of this OS was built with the help of Claude itself. An AI helped build the OS that exists to run AIs. Make of that what you will.

## Links

- **GitHub**: https://github.com/suhteevah/baremetal-claude
- **Website**: https://claudioos.vercel.app
- **29 standalone no_std crates**: https://github.com/suhteevah/baremetal-claude/blob/main/docs/OPEN-SOURCE-CRATES.md

Happy to answer questions about any layer of the stack.
