# r/linux Post

**Subreddit**: r/linux
**Title**: ClaudioOS implements 150+ Linux syscalls on bare metal -- run static Linux binaries without Linux

---

**TLDR**: I built a bare-metal Rust OS that includes a Linux syscall translation layer. It parses ELF64 binaries, maps segments, traps SYSCALL instructions, and dispatches to native Rust implementations of 150+ Linux syscalls (file I/O, mmap, sockets, signals, epoll). The goal is running static Linux binaries on purpose-built hardware without a Linux kernel underneath.

---

## What this is

ClaudioOS is a bare-metal Rust OS designed to run AI coding agents. One of its components is a Linux compatibility layer -- an ELF loader + syscall dispatcher that aims to run statically-linked Linux binaries directly on ClaudioOS hardware.

I want to be upfront: this is NOT a Linux replacement. This is not "Linux bad, Rust good." I have immense respect for Linux's design. Implementing its syscall interface from the other side has given me a deeper appreciation for how thoughtfully the API was designed than I ever had as a user of it.

## The Linux compat layer

**ELF Loader** (1,213 lines): Parses ELF64 headers, maps PT_LOAD segments with correct permissions, processes relocations, sets up an initial stack with argc/argv/envp/auxv, and jumps to the entry point.

**Syscall Dispatcher** (528 lines): Intercepts the SYSCALL instruction, reads the syscall number from RAX and arguments from RDI/RSI/RDX/R10/R8/R9, dispatches to the appropriate handler.

**Implemented syscall categories:**

- **File I/O** (35+ syscalls): read, write, open, close, stat, fstat, lstat, lseek, pread64, pwrite64, readv, writev, access, pipe, dup, dup2, fcntl, ftruncate, getdents64, getcwd, chdir, rename, mkdir, rmdir, openat, newfstatat, creat, link, unlink, symlink, readlink, chmod, chown
- **Memory** (6 syscalls): mmap (anonymous + file-backed), mprotect, munmap, brk, mremap, madvise
- **Network** (16 syscalls): socket, connect, accept, sendto, recvfrom, sendmsg, recvmsg, shutdown, bind, listen, getsockname, getpeername, socketpair, setsockopt, getsockopt, accept4
- **Process** (16 syscalls): getpid, getppid, gettid, getuid/geteuid/getgid/getegid, fork (stub), clone (stub), execve (stub), exit, exit_group, kill, wait4, set_tid_address
- **Signals** (7 syscalls): rt_sigaction, rt_sigprocmask, rt_sigreturn, rt_sigpending, rt_sigtimedwait, rt_sigqueueinfo, rt_sigsuspend
- **I/O multiplexing** (5 syscalls): poll, select, epoll_create1, epoll_ctl, epoll_wait
- **Time** (7 syscalls): gettimeofday, clock_gettime, clock_getres, clock_nanosleep, nanosleep, getitimer, setitimer
- **Misc** (10 syscalls): ioctl, sched_yield, uname, getrlimit, setrlimit, arch_prctl, set_robust_list, prlimit64, getrandom, futex

Plus ~80 additional stubs that return success or ENOSYS for things like mlock, prctl, scheduling parameters, extended attributes, SysV IPC, inotify, fanotify, and timers.

## What works

- Simple statically-linked binaries that use basic file I/O, memory allocation, and string operations
- Network clients that use socket/connect/send/recv
- Programs that use epoll for I/O multiplexing

## What does not work (yet)

- **fork/exec**: The single-address-space design makes real fork() impractical. clone() for threads is possible but not yet implemented.
- **Dynamic linking**: No ld-linux.so, no dlopen. Static binaries only.
- **Signals**: Signal delivery infrastructure exists but signal interruption of blocking syscalls is incomplete.
- **/proc**: Partial emulation (/proc/self/maps, /proc/self/status) but most of /proc is not there.

## Things I learned about Linux by reimplementing its syscalls

1. **The fcntl/ioctl split is brilliant.** fcntl for file descriptor properties, ioctl for device-specific control. Clean separation that scales to thousands of device types.
2. **mmap's design is genius.** The combination of anonymous/file-backed, shared/private, and protection flags in a single syscall covers an enormous design space.
3. **epoll is surprisingly simple to implement** once you have the socket layer. The hard part is waking blocked waiters efficiently.
4. **The auxiliary vector (auxv)** passed to ELF programs is underappreciated. AT_PAGESZ, AT_RANDOM, AT_PHDR -- programs depend on these in subtle ways.
5. **errno conventions are rock-solid.** Negative return values for errors, zero or positive for success. Simple, consistent, works everywhere.

## Links

- **GitHub**: https://github.com/suhteevah/baremetal-claude
- **Website**: https://claudioos.vercel.app
- **Linux compat source**: https://github.com/suhteevah/baremetal-claude/tree/main/crates/linux-compat
- **ELF loader source**: https://github.com/suhteevah/baremetal-claude/tree/main/crates/elf-loader

I would be curious to hear from Linux kernel developers about how accurate or misguided my syscall implementations are. Also happy to discuss the design tradeoffs of the compatibility approach vs. just porting software natively.
