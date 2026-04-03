//! Signal syscall implementations.
//!
//! Implements rt_sigaction, rt_sigprocmask, rt_sigreturn, rt_sigpending,
//! rt_sigtimedwait, rt_sigqueueinfo, rt_sigsuspend.
//!
//! ClaudioOS provides basic signal support. Most signals are either ignored
//! or result in process termination. Full POSIX signal semantics are not
//! needed for most statically-linked binaries.

use crate::errno::*;
use crate::types::*;

/// Maximum signal number.
const MAX_SIGNALS: usize = 64;

/// Signal disposition for a single signal.
#[derive(Debug, Clone, Copy)]
pub struct SignalDisposition {
    /// Handler address (SIG_DFL, SIG_IGN, or a function pointer).
    pub handler: u64,
    /// Signal action flags.
    pub flags: u64,
    /// Restorer function address.
    pub restorer: u64,
    /// Blocked signals mask during handler execution.
    pub mask: u64,
}

impl Default for SignalDisposition {
    fn default() -> Self {
        Self {
            handler: SIG_DFL,
            flags: 0,
            restorer: 0,
            mask: 0,
        }
    }
}

/// Signal state for the process.
pub struct SignalState {
    /// Per-signal dispositions.
    pub dispositions: [SignalDisposition; MAX_SIGNALS],
    /// Blocked signal mask.
    pub blocked_mask: u64,
    /// Pending signal mask.
    pub pending_mask: u64,
}

impl SignalState {
    pub fn new() -> Self {
        Self {
            dispositions: [SignalDisposition::default(); MAX_SIGNALS],
            blocked_mask: 0,
            pending_mask: 0,
        }
    }

    /// Queue a signal for delivery.
    pub fn raise_signal(&mut self, signum: u32) {
        if signum > 0 && (signum as usize) < MAX_SIGNALS {
            self.pending_mask |= 1u64 << (signum - 1);
        }
    }

    /// Check if a signal is pending and not blocked.
    pub fn has_deliverable_signal(&self) -> Option<u32> {
        let deliverable = self.pending_mask & !self.blocked_mask;
        if deliverable == 0 {
            return None;
        }
        // Find lowest set bit
        let bit = deliverable.trailing_zeros();
        Some(bit as u32 + 1)
    }
}

/// sys_rt_sigaction(signum, act_ptr, oldact_ptr, sigsetsize) -> 0
pub fn sys_rt_sigaction(
    ss: &mut SignalState,
    signum: u64,
    _act_ptr: u64,
    _oldact_ptr: u64,
    sigsetsize: u64,
) -> i64 {
    if sigsetsize != 8 {
        return EINVAL.as_neg();
    }

    let sig = signum as u32;
    if sig == 0 || sig as usize >= MAX_SIGNALS {
        return EINVAL.as_neg();
    }

    // Can't change SIGKILL or SIGSTOP
    if sig == SIGKILL || sig == SIGSTOP {
        return EINVAL.as_neg();
    }

    // In kernel: if oldact_ptr != 0, write current disposition to oldact_ptr
    // In kernel: if act_ptr != 0, read new Sigaction from act_ptr

    // For now, accept the request. In kernel integration we'd read/write
    // Sigaction structs from/to user memory.
    let _ = ss;

    log::trace!("rt_sigaction(sig={}) -> 0", sig);
    0
}

/// sys_rt_sigprocmask(how, set_ptr, oldset_ptr, sigsetsize) -> 0
pub fn sys_rt_sigprocmask(
    ss: &mut SignalState,
    how: u64,
    _set_ptr: u64,
    _oldset_ptr: u64,
    sigsetsize: u64,
) -> i64 {
    if sigsetsize != 8 {
        return EINVAL.as_neg();
    }

    // In kernel: if oldset_ptr != 0, write ss.blocked_mask to oldset_ptr
    // In kernel: if set_ptr != 0, read new mask from set_ptr

    // For now, accept the request.
    match how as u32 {
        SIG_BLOCK => {
            // ss.blocked_mask |= new_mask;
        }
        SIG_UNBLOCK => {
            // ss.blocked_mask &= !new_mask;
        }
        SIG_SETMASK => {
            // ss.blocked_mask = new_mask;
        }
        _ => return EINVAL.as_neg(),
    }

    let _ = ss;
    0
}

/// sys_rt_sigreturn() -> restored register state
pub fn sys_rt_sigreturn() -> i64 {
    // In kernel: restore the register state from the signal frame on the stack.
    // This is called by the signal trampoline after a signal handler returns.
    // For now, return 0.
    0
}

/// sys_rt_sigpending(set_ptr, sigsetsize) -> 0
pub fn sys_rt_sigpending(ss: &SignalState, _set_ptr: u64, sigsetsize: u64) -> i64 {
    if sigsetsize != 8 {
        return EINVAL.as_neg();
    }
    // In kernel: write ss.pending_mask to set_ptr
    let _ = ss;
    0
}

/// sys_rt_sigtimedwait(set_ptr, info_ptr, ts_ptr, sigsetsize) -> signal number
pub fn sys_rt_sigtimedwait(
    _ss: &SignalState,
    _set_ptr: u64,
    _info_ptr: u64,
    _ts_ptr: u64,
    sigsetsize: u64,
) -> i64 {
    if sigsetsize != 8 {
        return EINVAL.as_neg();
    }
    // Would block until a signal in the set becomes pending.
    // Return EAGAIN for now (timeout expired).
    EAGAIN.as_neg()
}

/// sys_rt_sigqueueinfo(pid, sig, info_ptr) -> 0
pub fn sys_rt_sigqueueinfo(ss: &mut SignalState, _pid: u64, sig: u64, _info_ptr: u64) -> i64 {
    let signum = sig as u32;
    if signum == 0 || signum as usize >= MAX_SIGNALS {
        return EINVAL.as_neg();
    }
    ss.raise_signal(signum);
    0
}

/// sys_rt_sigsuspend(mask_ptr, sigsetsize) -> -EINTR
pub fn sys_rt_sigsuspend(_ss: &SignalState, _mask_ptr: u64, sigsetsize: u64) -> i64 {
    if sigsetsize != 8 {
        return EINVAL.as_neg();
    }
    // Would temporarily replace the signal mask and suspend until a signal arrives.
    // Return EINTR as if a signal was delivered.
    EINTR.as_neg()
}
