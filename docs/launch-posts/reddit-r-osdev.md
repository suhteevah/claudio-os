# r/osdev Post

**Subreddit**: r/osdev
**Title**: ClaudioOS: Linux binary compatibility on bare metal via 150+ implemented syscalls

---

**TLDR**: ClaudioOS is a bare-metal Rust OS (x86_64, UEFI) with an ELF loader and Linux syscall translation layer. It dispatches 150+ syscalls to native implementations (file I/O, mmap, sockets, signals, epoll) with the goal of running static Linux binaries without Linux. 222K lines, 38 crates, full TCP/IP + TLS 1.3 networking.

---

Hey r/osdev,

I wanted to share a project I have been building: ClaudioOS, a bare-metal Rust OS designed to run AI coding agents (Anthropic Claude) directly on hardware. I think the Linux compatibility layer might be the most interesting part for this community.

## Architecture

```
UEFI -> bootloader v0.11 -> kernel_main
  -> GDT/IDT/PIC, heap (48 MiB), PCI enumeration
  -> VirtIO-net + smoltcp TCP/IP + DHCP/DNS
  -> TLS 1.3 (embedded-tls, AES-128-GCM-SHA256, AES-NI)
  -> Anthropic API client (HTTP/1.1 + SSE streaming)
  -> Multi-agent dashboard (split-pane terminal, async tasks)
```

Single address space. No kernel/user boundary. No MMU-based process isolation. Every agent session is an async task on a custom interrupt-driven executor. Hardware interrupts (NIC rx, keyboard IRQ1, PIT timer at 18.2 Hz) wake futures directly.

## Linux Syscall Compatibility

The `linux-compat` crate (4,090 lines) implements a syscall dispatch table covering the x86_64 Linux ABI:

**Fully implemented** (native handlers with real logic):
- File operations: read, write, open, close, stat, fstat, lstat, lseek, pread64, pwrite64, readv, writev, access, pipe, dup, dup2, fcntl, getdents64, getcwd, chdir, rename, mkdir, rmdir, openat, newfstatat, and more
- Memory: mmap, mprotect, munmap, brk, mremap, madvise
- Process: getpid, getppid, gettid, getuid/geteuid/getgid/getegid, fork (stub), clone (stub), execve (stub), exit, exit_group, kill, wait4, set_tid_address
- Time: gettimeofday, clock_gettime, clock_getres, clock_nanosleep, nanosleep
- Network: socket, connect, accept, sendto, recvfrom, sendmsg, recvmsg, shutdown, bind, listen, getsockname, getpeername, socketpair, setsockopt, getsockopt
- Signals: rt_sigaction, rt_sigprocmask, rt_sigreturn, rt_sigpending, rt_sigtimedwait, rt_sigqueueinfo, rt_sigsuspend
- I/O multiplexing: poll, select, epoll_create1, epoll_ctl, epoll_wait

**Stubs** (return success or ENOSYS, harmless no-ops):
- Memory locking (mlock/munlock), prctl, umask, sync/fsync, file locking
- Process identity (setuid/setgid/setpgid, capabilities)
- Scheduling (sched_setaffinity, sched_setparam, etc.)
- Extended attributes, SysV IPC, inotify, fanotify, timers

The ELF loader (1,213 lines) handles ELF64 parsing, section/segment mapping, relocation, and entry point execution. The idea is: load a statically-linked Linux binary, trap SYSCALL instructions, dispatch to our handlers.

## Interesting syscall implementation details

- `mmap` maintains a region list and hands out pages from the kernel heap. MAP_ANONYMOUS works. MAP_FIXED works. File-backed mappings go through the VFS.
- `epoll` is backed by a table of interest sets, with `epoll_wait` checking socket readiness via smoltcp.
- `brk` tracks the program break and allocates from a dedicated region.
- `/proc` emulation is partial -- `/proc/self/maps` and `/proc/self/status` return plausible data.

## Boot sequence

1. UEFI firmware loads bootloader crate v0.11
2. Bootloader sets up identity-mapped page tables, GOP framebuffer, UEFI memory map
3. `kernel_main`: SSE/SSE2/AVX enable (CR0/CR4/XCR0), GDT+TSS, IDT, PIC, PIT, heap init
4. Switches to 4 MiB heap-allocated stack (the bootloader's stack is tiny)
5. PCI bus enumeration with bus mastering
6. ACPI table parsing (RSDP, RSDT/XSDT, MADT, FADT, MCFG)
7. SMP: AP trampoline boot, per-CPU data init
8. VirtIO-net init, smoltcp interface, DHCP, DNS
9. TLS 1.3 handshake, HTTPS connectivity test
10. Auth (API key or OAuth device flow)
11. SSH daemon start, dashboard launch

## NIC drivers

- **VirtIO-net** (legacy 0.9.5): PCI discovery, virtqueue setup, DMA descriptor rings. Primary dev driver under QEMU.
- **Intel e1000/e1000e/igc** (1,986 lines): Supports I219-V (Skylake+) and I225-V (Tiger Lake+). Descriptor rings, PHY configuration, link detection. For real hardware.

## Other hardware

- AHCI/SATA: HBA registers, port command engine, ATA IDENTIFY, sector I/O (2,139 lines)
- NVMe: Admin/IO queue pairs, doorbell registers, PRP scatter-gather (2,563 lines)
- xHCI USB 3.0: TRB rings, device enumeration, HID keyboard (4,204 lines)
- HD Audio: CORB/RIRB protocol, codec discovery, PCM playback (2,631 lines)
- NVIDIA GPU: MMIO, Falcon microcontroller, GPFIFO, compute class (3,392 lines, scaffolding)

## What I learned

1. **The bootloader crate's stack is 16 KiB.** That is not enough for TLS handshakes. Switching to a 4 MiB heap-allocated stack was a turning point.
2. **AES-NI matters.** TLS 1.3 without hardware AES is painfully slow on bare metal. `-cpu Haswell` in QEMU was required.
3. **smoltcp is excellent** but you need to poll it from interrupts, not a busy loop. The power savings from `hlt`-when-idle are significant.
4. **Linux's syscall interface is remarkably well-designed.** Implementing it from the other side gave me deep appreciation for the consistency and composability of the POSIX API.

## Links

- **GitHub**: https://github.com/suhteevah/claudio-os
- **Website**: https://claudioos.vercel.app
- **Architecture diagram**: https://github.com/suhteevah/claudio-os/blob/main/docs/ARCHITECTURE.md

Would love to hear from other osdev folks about the approach. Especially interested in feedback on the Linux compat strategy and whether the ELF loader + syscall shim approach is viable for real-world binaries.
