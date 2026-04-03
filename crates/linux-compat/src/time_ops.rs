//! Time-related syscall implementations.
//!
//! Implements gettimeofday, clock_gettime, clock_getres, clock_nanosleep,
//! nanosleep, getitimer, setitimer.

use crate::errno::*;
use crate::types::*;
use core::sync::atomic::{AtomicU64, Ordering};

/// Monotonic tick counter. Incremented by the PIT timer interrupt.
/// At 18.2 Hz, each tick ≈ 54.9 ms.
static MONOTONIC_TICKS: AtomicU64 = AtomicU64::new(0);

/// Boot time in Unix epoch seconds (set from RTC at boot).
static BOOT_TIME_SECS: AtomicU64 = AtomicU64::new(0);

/// Increment the monotonic tick counter. Called from timer interrupt.
pub fn tick() {
    MONOTONIC_TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Set the boot time from RTC. Called once during init.
pub fn set_boot_time(epoch_secs: u64) {
    BOOT_TIME_SECS.store(epoch_secs, Ordering::Relaxed);
}

/// Get current monotonic time as (seconds, nanoseconds).
fn monotonic_now() -> (i64, i64) {
    let ticks = MONOTONIC_TICKS.load(Ordering::Relaxed);
    // PIT at 18.2 Hz: each tick = 1_000_000_000 / 18.2 ≈ 54_945_055 ns
    let total_ns = ticks * 54_945_055;
    let secs = (total_ns / 1_000_000_000) as i64;
    let nsecs = (total_ns % 1_000_000_000) as i64;
    (secs, nsecs)
}

/// Get current real (wall clock) time as (seconds, nanoseconds).
fn realtime_now() -> (i64, i64) {
    let boot = BOOT_TIME_SECS.load(Ordering::Relaxed) as i64;
    let (mono_secs, mono_nsecs) = monotonic_now();
    (boot + mono_secs, mono_nsecs)
}

/// sys_gettimeofday(tv_ptr, tz_ptr) -> 0
pub fn sys_gettimeofday(_tv_ptr: u64, _tz_ptr: u64) -> i64 {
    let (secs, nsecs) = realtime_now();

    // In kernel: write Timeval to tv_ptr
    let _tv = Timeval {
        tv_sec: secs,
        tv_usec: nsecs / 1000,
    };

    // In kernel: if tz_ptr != 0, write Timezone
    let _tz = Timezone {
        tz_minuteswest: 0,
        tz_dsttime: 0,
    };

    // Would write: unsafe { *(tv_ptr as *mut Timeval) = tv; }
    0
}

/// sys_clock_gettime(clockid, tp_ptr) -> 0
pub fn sys_clock_gettime(clockid: u64, _tp_ptr: u64) -> i64 {
    let (secs, nsecs) = match clockid as u32 {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => realtime_now(),
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_COARSE | CLOCK_BOOTTIME => {
            monotonic_now()
        }
        CLOCK_PROCESS_CPUTIME_ID | CLOCK_THREAD_CPUTIME_ID => {
            // CPU time ≈ monotonic for a single-process system
            monotonic_now()
        }
        _ => return EINVAL.as_neg(),
    };

    let _ts = Timespec {
        tv_sec: secs,
        tv_nsec: nsecs,
    };

    // Would write: unsafe { *(tp_ptr as *mut Timespec) = ts; }
    0
}

/// sys_clock_getres(clockid, res_ptr) -> 0
pub fn sys_clock_getres(clockid: u64, _res_ptr: u64) -> i64 {
    let _res = match clockid as u32 {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE | CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW
        | CLOCK_MONOTONIC_COARSE | CLOCK_BOOTTIME | CLOCK_PROCESS_CPUTIME_ID
        | CLOCK_THREAD_CPUTIME_ID => {
            // Resolution of our PIT timer: ~55 ms
            Timespec {
                tv_sec: 0,
                tv_nsec: 54_945_055,
            }
        }
        _ => return EINVAL.as_neg(),
    };

    // Would write: unsafe { if res_ptr != 0 { *(res_ptr as *mut Timespec) = res; } }
    0
}

/// sys_clock_nanosleep(clockid, flags, rqtp_ptr, rmtp_ptr) -> 0
pub fn sys_clock_nanosleep(clockid: u64, _flags: u64, _rqtp_ptr: u64, _rmtp_ptr: u64) -> i64 {
    match clockid as u32 {
        CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME => {
            // In kernel: read Timespec from rqtp_ptr, compute wake time,
            // hlt loop until wake time is reached.
            // For now, return immediately (no blocking).
            0
        }
        _ => EINVAL.as_neg(),
    }
}

/// sys_nanosleep(rqtp_ptr, rmtp_ptr) -> 0
pub fn sys_nanosleep(_rqtp_ptr: u64, _rmtp_ptr: u64) -> i64 {
    // In kernel: read Timespec from rqtp_ptr, sleep that duration.
    // For now, return immediately.
    0
}

/// sys_getitimer(which, value_ptr) -> 0
pub fn sys_getitimer(_which: u64, _value_ptr: u64) -> i64 {
    // Return zeroed itimerval (no timer set)
    // In kernel: unsafe { *(value_ptr as *mut Itimerval) = Itimerval { ... } }
    0
}

/// sys_setitimer(which, value_ptr, ovalue_ptr) -> 0
pub fn sys_setitimer(_which: u64, _value_ptr: u64, _ovalue_ptr: u64) -> i64 {
    // Accept the request but don't actually set up interval timers yet.
    // Would need to integrate with the PIT or APIC timer.
    0
}
