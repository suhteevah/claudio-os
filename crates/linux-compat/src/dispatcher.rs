//! Syscall dispatcher — the main entry point for all Linux syscalls.
//!
//! Called from the SYSCALL instruction handler. Reads the syscall number from
//! RAX and arguments from RDI, RSI, RDX, R10, R8, R9, dispatches to the
//! appropriate handler, and returns the result in RAX (-errno on error).

use crate::errno::*;
use crate::syscall_table::*;
use crate::file_ops::FileDescriptorTable;
use crate::memory_ops::MemoryManager;
use crate::process_ops::ProcessState;
use crate::signal_ops::SignalState;
use crate::network_ops::SocketTable;
use crate::io_ops::EpollTable;

/// Syscall arguments extracted from registers.
#[derive(Debug, Clone, Copy)]
pub struct SyscallArgs {
    /// Syscall number (from RAX).
    pub nr: u64,
    /// First argument (RDI).
    pub arg0: u64,
    /// Second argument (RSI).
    pub arg1: u64,
    /// Third argument (RDX).
    pub arg2: u64,
    /// Fourth argument (R10 — NOT RCX, which is clobbered by SYSCALL).
    pub arg3: u64,
    /// Fifth argument (R8).
    pub arg4: u64,
    /// Sixth argument (R9).
    pub arg5: u64,
}

/// Combined process state for syscall dispatch.
pub struct ProcessContext {
    pub fdt: FileDescriptorTable,
    pub mm: MemoryManager,
    pub ps: ProcessState,
    pub ss: SignalState,
    pub st: SocketTable,
    pub et: EpollTable,
}

impl ProcessContext {
    /// Create a new process context with default state.
    pub fn new(brk_base: u64) -> Self {
        Self {
            fdt: FileDescriptorTable::new(),
            mm: MemoryManager::new(brk_base),
            ps: ProcessState::new(),
            ss: SignalState::new(),
            st: SocketTable::new(),
            et: EpollTable::new(),
        }
    }
}

/// Dispatch a syscall to the appropriate handler.
///
/// This is the main entry point called from the low-level SYSCALL handler
/// in the kernel. It logs the syscall at trace level, dispatches to the
/// implementation, and returns the result (or -errno).
pub fn dispatch_syscall(ctx: &mut ProcessContext, args: SyscallArgs) -> i64 {
    let name = syscall_name(args.nr);
    log::trace!(
        "syscall {}({}) args=[0x{:X}, 0x{:X}, 0x{:X}, 0x{:X}, 0x{:X}, 0x{:X}]",
        args.nr,
        name,
        args.arg0,
        args.arg1,
        args.arg2,
        args.arg3,
        args.arg4,
        args.arg5,
    );

    let result = match args.nr {
        // ===== File operations =====
        SYS_READ => crate::file_ops::sys_read(&mut ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_WRITE => crate::file_ops::sys_write(&mut ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_OPEN => crate::file_ops::sys_open(&mut ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_CLOSE => crate::file_ops::sys_close(&mut ctx.fdt, args.arg0),
        SYS_STAT => crate::file_ops::sys_stat(&ctx.fdt, args.arg0, args.arg1),
        SYS_FSTAT => crate::file_ops::sys_fstat(&ctx.fdt, args.arg0, args.arg1),
        SYS_LSTAT => crate::file_ops::sys_lstat(&ctx.fdt, args.arg0, args.arg1),
        SYS_LSEEK => crate::file_ops::sys_lseek(&mut ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_PREAD64 => crate::file_ops::sys_pread64(&mut ctx.fdt, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_PWRITE64 => crate::file_ops::sys_pwrite64(&mut ctx.fdt, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_READV => crate::file_ops::sys_readv(&mut ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_WRITEV => crate::file_ops::sys_writev(&mut ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_ACCESS => crate::file_ops::sys_access(&ctx.fdt, args.arg0, args.arg1),
        SYS_PIPE => crate::file_ops::sys_pipe(&mut ctx.fdt, args.arg0),
        SYS_DUP => crate::file_ops::sys_dup(&mut ctx.fdt, args.arg0),
        SYS_DUP2 => crate::file_ops::sys_dup2(&mut ctx.fdt, args.arg0, args.arg1),
        SYS_FCNTL => crate::file_ops::sys_fcntl(&mut ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_FTRUNCATE => crate::file_ops::sys_ftruncate(&mut ctx.fdt, args.arg0, args.arg1),
        SYS_GETDENTS => crate::file_ops::sys_getdents(&ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_GETDENTS64 => crate::file_ops::sys_getdents64(&ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_GETCWD => crate::file_ops::sys_getcwd(&ctx.fdt, args.arg0, args.arg1),
        SYS_CHDIR => crate::file_ops::sys_chdir(&mut ctx.fdt, args.arg0),
        SYS_RENAME => crate::file_ops::sys_rename(&ctx.fdt, args.arg0, args.arg1),
        SYS_MKDIR => crate::file_ops::sys_mkdir(&ctx.fdt, args.arg0, args.arg1),
        SYS_RMDIR => crate::file_ops::sys_rmdir(&ctx.fdt, args.arg0),
        SYS_CREAT => crate::file_ops::sys_creat(&mut ctx.fdt, args.arg0, args.arg1),
        SYS_LINK => crate::file_ops::sys_link(&ctx.fdt, args.arg0, args.arg1),
        SYS_UNLINK => crate::file_ops::sys_unlink(&ctx.fdt, args.arg0),
        SYS_SYMLINK => crate::file_ops::sys_symlink(&ctx.fdt, args.arg0, args.arg1),
        SYS_READLINK => crate::file_ops::sys_readlink(&ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_CHMOD => crate::file_ops::sys_chmod(&ctx.fdt, args.arg0, args.arg1),
        SYS_FCHMOD => crate::file_ops::sys_fchmod(&ctx.fdt, args.arg0, args.arg1),
        SYS_CHOWN => crate::file_ops::sys_chown(&ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_OPENAT => crate::file_ops::sys_openat(&mut ctx.fdt, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_NEWFSTATAT => crate::file_ops::sys_newfstatat(&ctx.fdt, args.arg0, args.arg1, args.arg2, args.arg3),

        // ===== Memory operations =====
        SYS_MMAP => crate::memory_ops::sys_mmap(&mut ctx.mm, args.arg0, args.arg1, args.arg2, args.arg3, args.arg4, args.arg5),
        SYS_MPROTECT => crate::memory_ops::sys_mprotect(&mut ctx.mm, args.arg0, args.arg1, args.arg2),
        SYS_MUNMAP => crate::memory_ops::sys_munmap(&mut ctx.mm, args.arg0, args.arg1),
        SYS_BRK => crate::memory_ops::sys_brk(&mut ctx.mm, args.arg0),
        SYS_MREMAP => crate::memory_ops::sys_mremap(&mut ctx.mm, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_MADVISE => crate::memory_ops::sys_madvise(&ctx.mm, args.arg0, args.arg1, args.arg2),

        // ===== Process operations =====
        SYS_GETPID => crate::process_ops::sys_getpid(&ctx.ps),
        SYS_GETPPID => crate::process_ops::sys_getppid(&ctx.ps),
        SYS_GETTID => crate::process_ops::sys_gettid(&ctx.ps),
        SYS_GETUID => crate::process_ops::sys_getuid(&ctx.ps),
        SYS_GETEUID => crate::process_ops::sys_geteuid(&ctx.ps),
        SYS_GETGID => crate::process_ops::sys_getgid(&ctx.ps),
        SYS_GETEGID => crate::process_ops::sys_getegid(&ctx.ps),
        SYS_FORK => crate::process_ops::sys_fork(),
        SYS_VFORK => crate::process_ops::sys_vfork(),
        SYS_CLONE => crate::process_ops::sys_clone(args.arg0, args.arg1, args.arg2, args.arg3, args.arg4),
        SYS_EXECVE => crate::process_ops::sys_execve(args.arg0, args.arg1, args.arg2),
        SYS_EXIT => crate::process_ops::sys_exit(&mut ctx.ps, args.arg0),
        SYS_EXIT_GROUP => crate::process_ops::sys_exit_group(&mut ctx.ps, args.arg0),
        SYS_KILL => crate::process_ops::sys_kill(&mut ctx.ps, args.arg0, args.arg1),
        SYS_WAIT4 => crate::process_ops::sys_wait4(args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_SET_TID_ADDRESS => crate::process_ops::sys_set_tid_address(&mut ctx.ps, args.arg0),

        // ===== Time operations =====
        SYS_GETTIMEOFDAY => crate::time_ops::sys_gettimeofday(args.arg0, args.arg1),
        SYS_CLOCK_GETTIME => crate::time_ops::sys_clock_gettime(args.arg0, args.arg1),
        SYS_CLOCK_GETRES => crate::time_ops::sys_clock_getres(args.arg0, args.arg1),
        SYS_CLOCK_NANOSLEEP => crate::time_ops::sys_clock_nanosleep(args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_NANOSLEEP => crate::time_ops::sys_nanosleep(args.arg0, args.arg1),
        SYS_GETITIMER => crate::time_ops::sys_getitimer(args.arg0, args.arg1),
        SYS_SETITIMER => crate::time_ops::sys_setitimer(args.arg0, args.arg1, args.arg2),

        // ===== Network operations =====
        SYS_SOCKET => crate::network_ops::sys_socket(&mut ctx.fdt, &mut ctx.st, args.arg0, args.arg1, args.arg2),
        SYS_CONNECT => crate::network_ops::sys_connect(&mut ctx.st, args.arg0, args.arg1, args.arg2),
        SYS_ACCEPT => crate::network_ops::sys_accept(&mut ctx.fdt, &mut ctx.st, args.arg0, args.arg1, args.arg2),
        SYS_SENDTO => crate::network_ops::sys_sendto(&ctx.st, args.arg0, args.arg1, args.arg2, args.arg3, args.arg4, args.arg5),
        SYS_RECVFROM => crate::network_ops::sys_recvfrom(&mut ctx.st, args.arg0, args.arg1, args.arg2, args.arg3, args.arg4, args.arg5),
        SYS_SENDMSG => crate::network_ops::sys_sendmsg(&ctx.st, args.arg0, args.arg1, args.arg2),
        SYS_RECVMSG => crate::network_ops::sys_recvmsg(&mut ctx.st, args.arg0, args.arg1, args.arg2),
        SYS_SHUTDOWN => crate::network_ops::sys_shutdown(&mut ctx.st, args.arg0, args.arg1),
        SYS_BIND => crate::network_ops::sys_bind(&mut ctx.st, args.arg0, args.arg1, args.arg2),
        SYS_LISTEN => crate::network_ops::sys_listen(&mut ctx.st, args.arg0, args.arg1),
        SYS_GETSOCKNAME => crate::network_ops::sys_getsockname(&ctx.st, args.arg0, args.arg1, args.arg2),
        SYS_GETPEERNAME => crate::network_ops::sys_getpeername(&ctx.st, args.arg0, args.arg1, args.arg2),
        SYS_SOCKETPAIR => crate::network_ops::sys_socketpair(&mut ctx.fdt, &mut ctx.st, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_SETSOCKOPT => crate::network_ops::sys_setsockopt(&mut ctx.st, args.arg0, args.arg1, args.arg2, args.arg3, args.arg4),
        SYS_GETSOCKOPT => crate::network_ops::sys_getsockopt(&ctx.st, args.arg0, args.arg1, args.arg2, args.arg3, args.arg4),

        // ===== Signal operations =====
        SYS_RT_SIGACTION => crate::signal_ops::sys_rt_sigaction(&mut ctx.ss, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_RT_SIGPROCMASK => crate::signal_ops::sys_rt_sigprocmask(&mut ctx.ss, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_RT_SIGRETURN => crate::signal_ops::sys_rt_sigreturn(),
        SYS_RT_SIGPENDING => crate::signal_ops::sys_rt_sigpending(&ctx.ss, args.arg0, args.arg1),
        SYS_RT_SIGTIMEDWAIT => crate::signal_ops::sys_rt_sigtimedwait(&ctx.ss, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_RT_SIGQUEUEINFO => crate::signal_ops::sys_rt_sigqueueinfo(&mut ctx.ss, args.arg0, args.arg1, args.arg2),
        SYS_RT_SIGSUSPEND => crate::signal_ops::sys_rt_sigsuspend(&ctx.ss, args.arg0, args.arg1),

        // ===== I/O multiplexing =====
        SYS_POLL => crate::io_ops::sys_poll(&ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_SELECT => crate::io_ops::sys_select(&ctx.fdt, args.arg0, args.arg1, args.arg2, args.arg3, args.arg4),
        SYS_EPOLL_CREATE1 => crate::io_ops::sys_epoll_create1(&mut ctx.fdt, &mut ctx.et, args.arg0),
        SYS_EPOLL_CTL => crate::io_ops::sys_epoll_ctl(&mut ctx.et, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_EPOLL_WAIT => crate::io_ops::sys_epoll_wait(&ctx.fdt, &ctx.et, args.arg0, args.arg1, args.arg2, args.arg3),

        // ===== Misc operations =====
        SYS_IOCTL => crate::misc_ops::sys_ioctl(&ctx.fdt, args.arg0, args.arg1, args.arg2),
        SYS_SCHED_YIELD => crate::misc_ops::sys_sched_yield(),
        SYS_UNAME => crate::misc_ops::sys_uname(args.arg0),
        SYS_GETRLIMIT => crate::misc_ops::sys_getrlimit(args.arg0, args.arg1),
        SYS_SETRLIMIT => crate::misc_ops::sys_setrlimit(args.arg0, args.arg1),
        SYS_ARCH_PRCTL => crate::misc_ops::sys_arch_prctl(args.arg0, args.arg1),
        SYS_SET_ROBUST_LIST => crate::misc_ops::sys_set_robust_list(args.arg0, args.arg1),
        SYS_PRLIMIT64 => crate::misc_ops::sys_prlimit64(args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_GETRANDOM => crate::misc_ops::sys_getrandom(args.arg0, args.arg1, args.arg2),
        SYS_FUTEX => crate::misc_ops::sys_futex(args.arg0, args.arg1, args.arg2, args.arg3, args.arg4, args.arg5),

        // ===== Stubs that return success (harmless no-ops) =====
        SYS_SIGALTSTACK => 0,     // Accept signal alternate stack setup
        SYS_MLOCK => 0,           // Memory locking is a no-op
        SYS_MUNLOCK => 0,
        SYS_MLOCKALL => 0,
        SYS_MUNLOCKALL => 0,
        SYS_PRCTL => 0,           // Process control — accept silently
        SYS_UMASK => 0o022,       // Return previous umask
        SYS_SYNC => 0,            // Sync is a no-op in our setup
        SYS_FSYNC => 0,
        SYS_FDATASYNC => 0,
        SYS_FLOCK => 0,           // File locking — no-op
        SYS_FADVISE64 => 0,       // Advisory — ignore
        SYS_RSEQ => ENOSYS.as_neg(), // Restartable sequences
        SYS_MEMBARRIER => 0,      // Memory barrier — single-threaded, no-op

        // ===== Time-related stubs =====
        SYS_TIME => {
            // Return seconds since epoch
            let (secs, _) = (0i64, 0i64); // Would use realtime_now()
            secs
        }
        SYS_ALARM => 0,           // No alarm support yet

        // ===== Process stubs =====
        SYS_SETUID => 0,
        SYS_SETGID => 0,
        SYS_SETPGID => 0,
        SYS_GETPGRP => ctx.ps.pid as i64,
        SYS_SETSID => ctx.ps.pid as i64,
        SYS_SETREUID => 0,
        SYS_SETREGID => 0,
        SYS_GETGROUPS => 0,       // No supplementary groups
        SYS_SETGROUPS => 0,
        SYS_SETRESUID => 0,
        SYS_GETRESUID => 0,       // Would write 0,0,0 to output pointers
        SYS_SETRESGID => 0,
        SYS_GETRESGID => 0,
        SYS_GETPGID => ctx.ps.pid as i64,
        SYS_SETFSUID => 0,
        SYS_SETFSGID => 0,
        SYS_GETSID => ctx.ps.pid as i64,
        SYS_CAPGET => 0,
        SYS_CAPSET => 0,
        SYS_PERSONALITY => 0,     // Return current personality (Linux)
        SYS_SCHED_GETAFFINITY => {
            // Return a single-CPU affinity mask
            // In kernel: write a 1-bit mask to args.arg2
            0
        }
        SYS_SCHED_SETAFFINITY => 0,
        SYS_SCHED_SETPARAM => 0,
        SYS_SCHED_GETPARAM => 0,
        SYS_SCHED_SETSCHEDULER => 0,
        SYS_SCHED_GETSCHEDULER => 0, // SCHED_OTHER
        SYS_SCHED_GET_PRIORITY_MAX => 0,
        SYS_SCHED_GET_PRIORITY_MIN => 0,
        SYS_SCHED_RR_GET_INTERVAL => 0,
        SYS_GETRUSAGE => 0,       // Return zeroed rusage
        SYS_TIMES => 0,           // Return zeroed tms
        SYS_SYSINFO => 0,         // Return basic sysinfo
        SYS_GETCPU => 0,          // CPU 0, node 0

        // ===== File stubs =====
        SYS_FCHDIR => 0,
        SYS_FCHOWN => 0,
        SYS_LCHOWN => 0,
        SYS_TRUNCATE => 0,
        SYS_STATFS => 0,
        SYS_FSTATFS => 0,
        SYS_UTIMES => 0,
        SYS_UTIME => 0,
        SYS_UTIMENSAT => 0,
        SYS_UNLINKAT => 0,
        SYS_RENAMEAT => 0,
        SYS_RENAMEAT2 => 0,
        SYS_LINKAT => 0,
        SYS_SYMLINKAT => 0,
        SYS_READLINKAT => EINVAL.as_neg(),
        SYS_FCHMODAT => 0,
        SYS_FCHMODAT2 => 0,
        SYS_FACCESSAT => 0,
        SYS_FACCESSAT2 => 0,
        SYS_FCHOWNAT => 0,
        SYS_FUTIMESAT => 0,
        SYS_MKDIRAT => 0,
        SYS_MKNODAT => 0,
        SYS_SENDFILE => ENOSYS.as_neg(),
        SYS_PIPE2 => crate::file_ops::sys_pipe(&mut ctx.fdt, args.arg0),
        SYS_DUP3 => crate::file_ops::sys_dup2(&mut ctx.fdt, args.arg0, args.arg1),
        SYS_COPY_FILE_RANGE => ENOSYS.as_neg(),
        SYS_SPLICE => ENOSYS.as_neg(),
        SYS_TEE => ENOSYS.as_neg(),
        SYS_VMSPLICE => ENOSYS.as_neg(),
        SYS_FALLOCATE => 0,
        SYS_SYNC_FILE_RANGE => 0,
        SYS_READAHEAD => 0,
        SYS_CLOSE_RANGE => 0,

        // ===== Network stubs =====
        SYS_ACCEPT4 => crate::network_ops::sys_accept(&mut ctx.fdt, &mut ctx.st, args.arg0, args.arg1, args.arg2),
        SYS_RECVMMSG => ENOSYS.as_neg(),
        SYS_SENDMMSG => ENOSYS.as_neg(),

        // ===== epoll stubs =====
        SYS_EPOLL_CREATE => crate::io_ops::sys_epoll_create1(&mut ctx.fdt, &mut ctx.et, 0),
        SYS_EPOLL_PWAIT => crate::io_ops::sys_epoll_wait(&ctx.fdt, &ctx.et, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_EPOLL_PWAIT2 => crate::io_ops::sys_epoll_wait(&ctx.fdt, &ctx.et, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_PSELECT6 => crate::io_ops::sys_select(&ctx.fdt, args.arg0, args.arg1, args.arg2, args.arg3, args.arg4),
        SYS_PPOLL => crate::io_ops::sys_poll(&ctx.fdt, args.arg0, args.arg1, args.arg2),

        // ===== Signal stubs =====
        SYS_TGKILL => 0,
        SYS_TKILL => 0,
        SYS_PAUSE => EINTR.as_neg(),
        SYS_SIGNALFD => ENOSYS.as_neg(),
        SYS_SIGNALFD4 => ENOSYS.as_neg(),
        SYS_RT_TGSIGQUEUEINFO => 0,

        // ===== Timer stubs =====
        SYS_TIMER_CREATE => ENOSYS.as_neg(),
        SYS_TIMER_SETTIME => ENOSYS.as_neg(),
        SYS_TIMER_GETTIME => ENOSYS.as_neg(),
        SYS_TIMER_GETOVERRUN => ENOSYS.as_neg(),
        SYS_TIMER_DELETE => ENOSYS.as_neg(),
        SYS_TIMERFD_CREATE => ENOSYS.as_neg(),
        SYS_TIMERFD_SETTIME => ENOSYS.as_neg(),
        SYS_TIMERFD_GETTIME => ENOSYS.as_neg(),
        SYS_CLOCK_SETTIME => EPERM.as_neg(),
        SYS_CLOCK_ADJTIME => EPERM.as_neg(),

        // ===== Event fd =====
        SYS_EVENTFD => ENOSYS.as_neg(),
        SYS_EVENTFD2 => ENOSYS.as_neg(),

        // ===== inotify =====
        SYS_INOTIFY_INIT => ENOSYS.as_neg(),
        SYS_INOTIFY_INIT1 => ENOSYS.as_neg(),
        SYS_INOTIFY_ADD_WATCH => ENOSYS.as_neg(),
        SYS_INOTIFY_RM_WATCH => ENOSYS.as_neg(),

        // ===== fanotify =====
        SYS_FANOTIFY_INIT => ENOSYS.as_neg(),
        SYS_FANOTIFY_MARK => ENOSYS.as_neg(),

        // ===== Extended attributes =====
        SYS_SETXATTR => ENOSYS.as_neg(),
        SYS_LSETXATTR => ENOSYS.as_neg(),
        SYS_FSETXATTR => ENOSYS.as_neg(),
        SYS_GETXATTR => ENOSYS.as_neg(),
        SYS_LGETXATTR => ENOSYS.as_neg(),
        SYS_FGETXATTR => ENOSYS.as_neg(),
        SYS_LISTXATTR => ENOSYS.as_neg(),
        SYS_LLISTXATTR => ENOSYS.as_neg(),
        SYS_FLISTXATTR => ENOSYS.as_neg(),
        SYS_REMOVEXATTR => ENOSYS.as_neg(),
        SYS_LREMOVEXATTR => ENOSYS.as_neg(),
        SYS_FREMOVEXATTR => ENOSYS.as_neg(),

        // ===== SysV IPC =====
        SYS_SEMGET => ENOSYS.as_neg(),
        SYS_SEMOP => ENOSYS.as_neg(),
        SYS_SEMCTL => ENOSYS.as_neg(),
        SYS_SEMTIMEDOP => ENOSYS.as_neg(),
        SYS_SHMGET => ENOSYS.as_neg(),
        SYS_SHMAT => ENOSYS.as_neg(),
        SYS_SHMCTL => ENOSYS.as_neg(),
        SYS_SHMDT => ENOSYS.as_neg(),
        SYS_MSGGET => ENOSYS.as_neg(),
        SYS_MSGSND => ENOSYS.as_neg(),
        SYS_MSGRCV => ENOSYS.as_neg(),
        SYS_MSGCTL => ENOSYS.as_neg(),

        // ===== POSIX MQ =====
        SYS_MQ_OPEN => ENOSYS.as_neg(),
        SYS_MQ_UNLINK => ENOSYS.as_neg(),
        SYS_MQ_TIMEDSEND => ENOSYS.as_neg(),
        SYS_MQ_TIMEDRECEIVE => ENOSYS.as_neg(),
        SYS_MQ_NOTIFY => ENOSYS.as_neg(),
        SYS_MQ_GETSETATTR => ENOSYS.as_neg(),

        // ===== AIO =====
        SYS_IO_SETUP => ENOSYS.as_neg(),
        SYS_IO_DESTROY => ENOSYS.as_neg(),
        SYS_IO_GETEVENTS => ENOSYS.as_neg(),
        SYS_IO_SUBMIT => ENOSYS.as_neg(),
        SYS_IO_CANCEL => ENOSYS.as_neg(),
        SYS_IO_PGETEVENTS => ENOSYS.as_neg(),

        // ===== io_uring =====
        SYS_IO_URING_SETUP => ENOSYS.as_neg(),
        SYS_IO_URING_ENTER => ENOSYS.as_neg(),
        SYS_IO_URING_REGISTER => ENOSYS.as_neg(),

        // ===== Keyring =====
        SYS_ADD_KEY => ENOSYS.as_neg(),
        SYS_REQUEST_KEY => ENOSYS.as_neg(),
        SYS_KEYCTL => ENOSYS.as_neg(),

        // ===== Namespace / security =====
        SYS_UNSHARE => ENOSYS.as_neg(),
        SYS_SETNS => ENOSYS.as_neg(),
        SYS_SECCOMP => ENOSYS.as_neg(),
        SYS_LANDLOCK_CREATE_RULESET => ENOSYS.as_neg(),
        SYS_LANDLOCK_ADD_RULE => ENOSYS.as_neg(),
        SYS_LANDLOCK_RESTRICT_SELF => ENOSYS.as_neg(),

        // ===== BPF =====
        SYS_BPF => ENOSYS.as_neg(),

        // ===== pidfd =====
        SYS_PIDFD_SEND_SIGNAL => ENOSYS.as_neg(),
        SYS_PIDFD_OPEN => ENOSYS.as_neg(),
        SYS_PIDFD_GETFD => ENOSYS.as_neg(),

        // ===== Mount =====
        SYS_MOUNT => EPERM.as_neg(),
        SYS_UMOUNT2 => EPERM.as_neg(),
        SYS_PIVOT_ROOT => EPERM.as_neg(),
        SYS_CHROOT => EPERM.as_neg(),
        SYS_OPEN_TREE => ENOSYS.as_neg(),
        SYS_MOVE_MOUNT => ENOSYS.as_neg(),
        SYS_FSOPEN => ENOSYS.as_neg(),
        SYS_FSCONFIG => ENOSYS.as_neg(),
        SYS_FSMOUNT => ENOSYS.as_neg(),
        SYS_FSPICK => ENOSYS.as_neg(),
        SYS_MOUNT_SETATTR => ENOSYS.as_neg(),
        SYS_SYNCFS => 0,

        // ===== Module =====
        SYS_CREATE_MODULE => ENOSYS.as_neg(),
        SYS_INIT_MODULE => ENOSYS.as_neg(),
        SYS_DELETE_MODULE => ENOSYS.as_neg(),
        SYS_FINIT_MODULE => ENOSYS.as_neg(),
        SYS_GET_KERNEL_SYMS => ENOSYS.as_neg(),
        SYS_QUERY_MODULE => ENOSYS.as_neg(),

        // ===== Misc privileged =====
        SYS_IOPL => EPERM.as_neg(),
        SYS_IOPERM => EPERM.as_neg(),
        SYS_REBOOT => EPERM.as_neg(),
        SYS_SWAPON => EPERM.as_neg(),
        SYS_SWAPOFF => EPERM.as_neg(),
        SYS_SETHOSTNAME => EPERM.as_neg(),
        SYS_SETDOMAINNAME => EPERM.as_neg(),
        SYS_SETTIMEOFDAY => EPERM.as_neg(),
        SYS_ADJTIMEX => EPERM.as_neg(),
        SYS_ACCT => EPERM.as_neg(),
        SYS_QUOTACTL => ENOSYS.as_neg(),
        SYS_QUOTACTL_FD => ENOSYS.as_neg(),
        SYS_VHANGUP => 0,
        SYS_SYSLOG => EPERM.as_neg(),
        SYS_PTRACE => EPERM.as_neg(),

        // ===== Misc legacy/unimplemented =====
        SYS_MODIFY_LDT => ENOSYS.as_neg(),
        SYS__SYSCTL => ENOSYS.as_neg(),
        SYS_USELIB => ENOSYS.as_neg(),
        SYS_USTAT => ENOSYS.as_neg(),
        SYS_SYSFS => ENOSYS.as_neg(),
        SYS_GETPRIORITY => 0,
        SYS_SETPRIORITY => 0,
        SYS_IOPRIO_SET => 0,
        SYS_IOPRIO_GET => 0,
        SYS_NFSSERVCTL => ENOSYS.as_neg(),
        SYS_GETPMSG => ENOSYS.as_neg(),
        SYS_PUTPMSG => ENOSYS.as_neg(),
        SYS_AFS_SYSCALL => ENOSYS.as_neg(),
        SYS_TUXCALL => ENOSYS.as_neg(),
        SYS_SECURITY => ENOSYS.as_neg(),
        SYS_SET_THREAD_AREA => ENOSYS.as_neg(),
        SYS_GET_THREAD_AREA => ENOSYS.as_neg(),
        SYS_LOOKUP_DCOOKIE => ENOSYS.as_neg(),
        SYS_EPOLL_CTL_OLD => ENOSYS.as_neg(),
        SYS_EPOLL_WAIT_OLD => ENOSYS.as_neg(),
        SYS_REMAP_FILE_PAGES => ENOSYS.as_neg(),
        SYS_RESTART_SYSCALL => EINTR.as_neg(),
        SYS_MSYNC => 0,
        SYS_MINCORE => ENOSYS.as_neg(),
        SYS_MKNOD => ENOSYS.as_neg(),
        SYS_WAITID => ECHILD.as_neg(),
        SYS_MIGRATE_PAGES => ENOSYS.as_neg(),
        SYS_MOVE_PAGES => ENOSYS.as_neg(),
        SYS_KEXEC_LOAD => ENOSYS.as_neg(),
        SYS_KEXEC_FILE_LOAD => ENOSYS.as_neg(),
        SYS_PERF_EVENT_OPEN => ENOSYS.as_neg(),
        SYS_NAME_TO_HANDLE_AT => ENOSYS.as_neg(),
        SYS_OPEN_BY_HANDLE_AT => ENOSYS.as_neg(),
        SYS_KCMP => ENOSYS.as_neg(),
        SYS_SCHED_SETATTR => 0,
        SYS_SCHED_GETATTR => 0,
        SYS_EXECVEAT => ENOSYS.as_neg(),
        SYS_USERFAULTFD => ENOSYS.as_neg(),
        SYS_MLOCK2 => 0,
        SYS_PKEY_MPROTECT => ENOSYS.as_neg(),
        SYS_PKEY_ALLOC => ENOSYS.as_neg(),
        SYS_PKEY_FREE => ENOSYS.as_neg(),
        SYS_STATX => ENOSYS.as_neg(),
        SYS_CLONE3 => ENOSYS.as_neg(),
        SYS_OPENAT2 => crate::file_ops::sys_openat(&mut ctx.fdt, args.arg0, args.arg1, args.arg2, args.arg3),
        SYS_VSERVER => ENOSYS.as_neg(),
        SYS_MBIND => 0,
        SYS_SET_MEMPOLICY => 0,
        SYS_GET_MEMPOLICY => 0,
        SYS_GET_ROBUST_LIST => ENOSYS.as_neg(),
        SYS_PROCESS_VM_READV => ENOSYS.as_neg(),
        SYS_PROCESS_VM_WRITEV => ENOSYS.as_neg(),
        SYS_MEMFD_CREATE => ENOSYS.as_neg(),
        SYS_MEMFD_SECRET => ENOSYS.as_neg(),
        SYS_PROCESS_MADVISE => ENOSYS.as_neg(),
        SYS_PROCESS_MRELEASE => ENOSYS.as_neg(),
        SYS_FUTEX_WAITV => ENOSYS.as_neg(),
        SYS_SET_MEMPOLICY_HOME_NODE => ENOSYS.as_neg(),
        SYS_CACHESTAT => ENOSYS.as_neg(),
        SYS_MAP_SHADOW_STACK => ENOSYS.as_neg(),
        SYS_FUTEX_WAKE => 0,
        SYS_FUTEX_WAIT => EAGAIN.as_neg(),
        SYS_FUTEX_REQUEUE => ENOSYS.as_neg(),
        SYS_PREADV => ENOSYS.as_neg(),
        SYS_PWRITEV => ENOSYS.as_neg(),
        SYS_PREADV2 => ENOSYS.as_neg(),
        SYS_PWRITEV2 => ENOSYS.as_neg(),

        // ===== Catch-all =====
        _ => {
            log::warn!("unhandled syscall {} ({})", args.nr, name);
            ENOSYS.as_neg()
        }
    };

    log::trace!("syscall {}({}) -> {}", args.nr, name, result);
    result
}
