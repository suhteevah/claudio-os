//! Process-related syscall implementations.
//!
//! Implements getpid, getuid, getgid, geteuid, getegid, getppid, gettid,
//! fork (ENOSYS), execve, exit, kill, exit_group, clone (stub), wait4.

use crate::errno::*;

/// Simulated process ID. ClaudioOS runs in a single address space,
/// so there's only ever one "process."
const CLAUDIO_PID: u64 = 1;

/// Simulated UID/GID — always root in our bare-metal environment.
const CLAUDIO_UID: u64 = 0;
const CLAUDIO_GID: u64 = 0;

/// Process state for the currently running Linux binary.
pub struct ProcessState {
    /// Process ID.
    pub pid: u64,
    /// Parent PID.
    pub ppid: u64,
    /// Thread ID (same as PID in single-threaded).
    pub tid: u64,
    /// User ID.
    pub uid: u64,
    /// Effective user ID.
    pub euid: u64,
    /// Group ID.
    pub gid: u64,
    /// Effective group ID.
    pub egid: u64,
    /// Set to true when the process has called exit/exit_group.
    pub exited: bool,
    /// Exit status code.
    pub exit_code: i32,
    /// Pointer for set_tid_address.
    pub tid_address: u64,
}

impl ProcessState {
    pub fn new() -> Self {
        Self {
            pid: CLAUDIO_PID,
            ppid: 0, // init has no parent
            tid: CLAUDIO_PID,
            uid: CLAUDIO_UID,
            euid: CLAUDIO_UID,
            gid: CLAUDIO_GID,
            egid: CLAUDIO_GID,
            exited: false,
            exit_code: 0,
            tid_address: 0,
        }
    }
}

/// sys_getpid() -> pid
pub fn sys_getpid(ps: &ProcessState) -> i64 {
    ps.pid as i64
}

/// sys_getppid() -> ppid
pub fn sys_getppid(ps: &ProcessState) -> i64 {
    ps.ppid as i64
}

/// sys_gettid() -> tid
pub fn sys_gettid(ps: &ProcessState) -> i64 {
    ps.tid as i64
}

/// sys_getuid() -> uid
pub fn sys_getuid(ps: &ProcessState) -> i64 {
    ps.uid as i64
}

/// sys_geteuid() -> euid
pub fn sys_geteuid(ps: &ProcessState) -> i64 {
    ps.euid as i64
}

/// sys_getgid() -> gid
pub fn sys_getgid(ps: &ProcessState) -> i64 {
    ps.gid as i64
}

/// sys_getegid() -> egid
pub fn sys_getegid(ps: &ProcessState) -> i64 {
    ps.egid as i64
}

/// sys_fork() -> -ENOSYS
/// ClaudioOS is single address space — fork is not supported.
pub fn sys_fork() -> i64 {
    log::warn!("fork() called — not supported in ClaudioOS single address space");
    ENOSYS.as_neg()
}

/// sys_vfork() -> -ENOSYS
pub fn sys_vfork() -> i64 {
    log::warn!("vfork() called — not supported in ClaudioOS");
    ENOSYS.as_neg()
}

/// sys_clone(flags, stack, ptid, ctid, regs) -> -ENOSYS
/// Threading requires significant infrastructure. Stub for now.
pub fn sys_clone(_flags: u64, _stack: u64, _ptid: u64, _ctid: u64, _regs: u64) -> i64 {
    log::warn!("clone() called — thread creation not yet supported");
    ENOSYS.as_neg()
}

/// sys_execve(filename, argv, envp) -> doesn't return on success
pub fn sys_execve(_filename_ptr: u64, _argv_ptr: u64, _envp_ptr: u64) -> i64 {
    // In a full implementation, this would:
    // 1. Read the filename from user memory
    // 2. Load the new ELF binary via the elf-loader
    // 3. Replace the current process image
    // 4. Set up new stack with argv/envp
    // 5. Jump to new entry point
    //
    // For now, return ENOSYS since we'd need VFS integration to read the binary.
    log::warn!("execve() called — not yet fully implemented");
    ENOSYS.as_neg()
}

/// sys_exit(status) -> never returns
pub fn sys_exit(ps: &mut ProcessState, status: u64) -> i64 {
    ps.exited = true;
    ps.exit_code = status as i32;
    log::info!("Process exited with status {}", ps.exit_code);
    // In the kernel, this would tear down the process and return to the dashboard.
    0
}

/// sys_exit_group(status) -> never returns
pub fn sys_exit_group(ps: &mut ProcessState, status: u64) -> i64 {
    // Same as exit for single-threaded
    sys_exit(ps, status)
}

/// sys_kill(pid, sig) -> 0
pub fn sys_kill(ps: &mut ProcessState, pid: u64, sig: u64) -> i64 {
    if pid as i64 == 0 || pid == ps.pid {
        // Sending signal to self
        match sig as u32 {
            0 => 0, // Signal 0 just checks if process exists
            9 => {
                // SIGKILL
                ps.exited = true;
                ps.exit_code = 137; // 128 + 9
                log::info!("Process killed by SIGKILL");
                0
            }
            15 => {
                // SIGTERM
                ps.exited = true;
                ps.exit_code = 143; // 128 + 15
                log::info!("Process terminated by SIGTERM");
                0
            }
            _ => 0, // Other signals accepted but mostly no-oped
        }
    } else {
        // Can't kill other processes — there are none
        ESRCH.as_neg()
    }
}

/// sys_wait4(pid, status, options, rusage) -> pid or error
pub fn sys_wait4(_pid: u64, _status_ptr: u64, _options: u64, _rusage_ptr: u64) -> i64 {
    // No child processes to wait for
    ECHILD.as_neg()
}

/// sys_set_tid_address(tidptr) -> tid
pub fn sys_set_tid_address(ps: &mut ProcessState, tidptr: u64) -> i64 {
    ps.tid_address = tidptr;
    ps.tid as i64
}
