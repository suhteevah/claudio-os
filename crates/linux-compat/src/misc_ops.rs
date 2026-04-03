//! Miscellaneous syscall implementations.
//!
//! Implements ioctl (basic terminal), sched_yield, uname, getrlimit,
//! setrlimit, arch_prctl, set_tid_address, set_robust_list, prlimit64,
//! getrandom.

use crate::errno::*;
use crate::types::*;
use crate::file_ops::FileDescriptorTable;
use core::sync::atomic::{AtomicU64, Ordering};

/// Simple xorshift64 PRNG state for getrandom.
static PRNG_STATE: AtomicU64 = AtomicU64::new(0xDEAD_BEEF_CAFE_BABE);

/// Initialize the PRNG with a seed (e.g., from RDTSC or RTC).
pub fn init_prng(seed: u64) {
    PRNG_STATE.store(seed, Ordering::Relaxed);
}

fn xorshift64() -> u64 {
    let mut state = PRNG_STATE.load(Ordering::Relaxed);
    if state == 0 {
        state = 0xDEAD_BEEF_CAFE_BABE;
    }
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    PRNG_STATE.store(state, Ordering::Relaxed);
    state
}

/// FS base for TLS (set by arch_prctl ARCH_SET_FS).
static FS_BASE: AtomicU64 = AtomicU64::new(0);
/// GS base (set by arch_prctl ARCH_SET_GS).
static GS_BASE: AtomicU64 = AtomicU64::new(0);

/// sys_ioctl(fd, request, arg) -> result
pub fn sys_ioctl(fdt: &FileDescriptorTable, fd: u64, request: u64, _arg: u64) -> i64 {
    let fd = fd as usize;
    match fdt.get(fd) {
        Ok(entry) => {
            match request {
                TIOCGWINSZ => {
                    // Return terminal window size.
                    // In kernel: write Winsize struct to arg pointer.
                    let _ws = Winsize {
                        ws_row: 25,
                        ws_col: 80,
                        ws_xpixel: 640,
                        ws_ypixel: 400,
                    };
                    // unsafe { *(arg as *mut Winsize) = ws; }
                    0
                }
                TIOCSWINSZ => {
                    // Set terminal window size — accept silently.
                    0
                }
                TCGETS => {
                    // Get terminal attributes. Return zeroed struct.
                    // In kernel: write termios struct to arg.
                    0
                }
                TCSETS | TCSETSW | TCSETSF => {
                    // Set terminal attributes — accept silently.
                    0
                }
                TIOCGPGRP => {
                    // Get foreground process group.
                    // In kernel: write PID as i32 to arg.
                    0
                }
                TIOCSPGRP => {
                    // Set foreground process group — accept.
                    0
                }
                FIONREAD => {
                    // Return number of bytes available for reading.
                    let available = entry.buffer.len() as i32;
                    let _ = available;
                    // In kernel: unsafe { *(arg as *mut i32) = available; }
                    0
                }
                FIONBIO => {
                    // Set/clear non-blocking mode.
                    0
                }
                _ => {
                    log::trace!("ioctl(fd={}, req=0x{:X}) -> ENOTTY", fd, request);
                    ENOTTY.as_neg()
                }
            }
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_sched_yield() -> 0
pub fn sys_sched_yield() -> i64 {
    // In kernel: yield to the async executor. For now, just return.
    // Could issue `hlt` to give time to other tasks.
    0
}

/// sys_uname(buf_ptr) -> 0
pub fn sys_uname(_buf_ptr: u64) -> i64 {
    // Build the utsname struct
    let mut uname = Utsname {
        sysname: [0; 65],
        nodename: [0; 65],
        release: [0; 65],
        version: [0; 65],
        machine: [0; 65],
        domainname: [0; 65],
    };

    fn write_str(buf: &mut [u8; 65], s: &str) {
        let bytes = s.as_bytes();
        let len = core::cmp::min(bytes.len(), 64);
        buf[..len].copy_from_slice(&bytes[..len]);
        buf[len] = 0;
    }

    write_str(&mut uname.sysname, "ClaudioOS");
    write_str(&mut uname.nodename, "claudio");
    write_str(&mut uname.release, "0.1.0");
    write_str(&mut uname.version, "ClaudioOS 0.1.0 bare-metal Rust");
    write_str(&mut uname.machine, "x86_64");
    write_str(&mut uname.domainname, "(none)");

    // In kernel: unsafe { *(buf_ptr as *mut Utsname) = uname; }
    let _ = uname;
    0
}

/// sys_getrlimit(resource, rlim_ptr) -> 0
pub fn sys_getrlimit(resource: u64, _rlim_ptr: u64) -> i64 {
    let rlim = match resource as u32 {
        RLIMIT_STACK => Rlimit {
            rlim_cur: 8 * 1024 * 1024,  // 8 MiB default stack
            rlim_max: RLIM_INFINITY,
        },
        RLIMIT_NOFILE => Rlimit {
            rlim_cur: 1024,
            rlim_max: 4096,
        },
        RLIMIT_AS => Rlimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        },
        RLIMIT_DATA => Rlimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        },
        RLIMIT_FSIZE => Rlimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        },
        _ => Rlimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        },
    };

    // In kernel: unsafe { *(rlim_ptr as *mut Rlimit) = rlim; }
    let _ = rlim;
    0
}

/// sys_setrlimit(resource, rlim_ptr) -> 0
pub fn sys_setrlimit(_resource: u64, _rlim_ptr: u64) -> i64 {
    // Accept all setrlimit calls silently.
    0
}

/// sys_arch_prctl(code, addr) -> 0
pub fn sys_arch_prctl(code: u64, addr: u64) -> i64 {
    match code {
        ARCH_SET_FS => {
            FS_BASE.store(addr, Ordering::Relaxed);
            // In kernel: also write to IA32_FS_BASE MSR
            // unsafe { x86_64::registers::model_specific::FsBase::write(VirtAddr::new(addr)); }
            log::trace!("arch_prctl(ARCH_SET_FS, 0x{:X})", addr);
            0
        }
        ARCH_SET_GS => {
            GS_BASE.store(addr, Ordering::Relaxed);
            // In kernel: also write to IA32_GS_BASE MSR
            log::trace!("arch_prctl(ARCH_SET_GS, 0x{:X})", addr);
            0
        }
        ARCH_GET_FS => {
            // In kernel: write FS_BASE value to *(addr as *mut u64)
            let _ = FS_BASE.load(Ordering::Relaxed);
            0
        }
        ARCH_GET_GS => {
            // In kernel: write GS_BASE value to *(addr as *mut u64)
            let _ = GS_BASE.load(Ordering::Relaxed);
            0
        }
        _ => EINVAL.as_neg(),
    }
}

/// sys_set_robust_list(head_ptr, len) -> 0
pub fn sys_set_robust_list(_head_ptr: u64, len: u64) -> i64 {
    // Validate the length matches the struct size.
    if len as usize != core::mem::size_of::<RobustListHead>() {
        return EINVAL.as_neg();
    }
    // Accept and ignore — we don't use futexes yet.
    0
}

/// sys_prlimit64(pid, resource, new_rlim_ptr, old_rlim_ptr) -> 0
pub fn sys_prlimit64(
    _pid: u64,
    resource: u64,
    _new_rlim_ptr: u64,
    old_rlim_ptr: u64,
) -> i64 {
    // If old_rlim_ptr != 0, write current limit
    if old_rlim_ptr != 0 {
        return sys_getrlimit(resource, old_rlim_ptr);
    }
    // If new_rlim_ptr != 0, set new limit
    0
}

/// sys_getrandom(buf_ptr, buflen, flags) -> bytes written
pub fn sys_getrandom(_buf_ptr: u64, buflen: u64, _flags: u64) -> i64 {
    let len = buflen as usize;
    if len == 0 {
        return 0;
    }

    // Generate random bytes via xorshift64 PRNG.
    // In kernel: write random bytes to buf_ptr.
    //
    // unsafe {
    //     let buf = core::slice::from_raw_parts_mut(buf_ptr as *mut u8, len);
    //     let mut remaining = len;
    //     while remaining > 0 {
    //         let r = xorshift64();
    //         let bytes = r.to_le_bytes();
    //         let chunk = core::cmp::min(remaining, 8);
    //         buf[len - remaining..len - remaining + chunk].copy_from_slice(&bytes[..chunk]);
    //         remaining -= chunk;
    //     }
    // }

    let _ = xorshift64(); // Advance PRNG state
    len as i64
}

/// sys_futex(uaddr, futex_op, val, timeout, uaddr2, val3) -> result
/// Basic futex support — just enough for glibc/musl initialization.
pub fn sys_futex(
    _uaddr: u64,
    futex_op: u64,
    _val: u64,
    _timeout: u64,
    _uaddr2: u64,
    _val3: u64,
) -> i64 {
    const FUTEX_WAIT: u64 = 0;
    const FUTEX_WAKE: u64 = 1;
    const FUTEX_WAIT_PRIVATE: u64 = 128;
    const FUTEX_WAKE_PRIVATE: u64 = 129;

    let op = futex_op & 0x7F; // Mask out FUTEX_PRIVATE_FLAG

    match op {
        FUTEX_WAIT | FUTEX_WAIT_PRIVATE => {
            // In a single-threaded context, waiting on a futex would deadlock.
            // Return EAGAIN to indicate the value has changed.
            EAGAIN.as_neg()
        }
        FUTEX_WAKE | FUTEX_WAKE_PRIVATE => {
            // Nothing to wake in single-threaded mode.
            0
        }
        _ => {
            log::trace!("futex(op={}) -> ENOSYS", futex_op);
            ENOSYS.as_neg()
        }
    }
}
