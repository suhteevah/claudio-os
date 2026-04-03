//! # claudio-linux-compat
//!
//! Linux syscall compatibility layer for ClaudioOS.
//!
//! This crate implements the x86_64 Linux syscall ABI, allowing statically-linked
//! Linux ELF binaries to run unmodified on ClaudioOS bare metal. The syscall
//! dispatcher reads the syscall number from RAX and arguments from RDI, RSI, RDX,
//! R10, R8, R9, dispatches to the appropriate handler, and returns the result
//! (or negated errno) in RAX.
//!
//! ## Implemented syscall categories:
//! - File operations (open, read, write, close, stat, etc.)
//! - Memory management (mmap, munmap, brk, mprotect)
//! - Process info (getpid, getuid, uname, etc.)
//! - Time (clock_gettime, gettimeofday, nanosleep)
//! - Networking (socket, connect, bind, listen, accept, send, recv)
//! - I/O multiplexing (poll, select, epoll)
//! - Signals (basic rt_sigaction, rt_sigprocmask)
//! - Misc (ioctl, getrandom, arch_prctl, etc.)

#![no_std]

extern crate alloc;

pub mod errno;
pub mod types;
pub mod syscall_table;
pub mod file_ops;
pub mod memory_ops;
pub mod process_ops;
pub mod time_ops;
pub mod network_ops;
pub mod signal_ops;
pub mod io_ops;
pub mod misc_ops;
pub mod dispatcher;

pub use errno::Errno;
pub use dispatcher::{dispatch_syscall, SyscallArgs};
pub use types::*;
