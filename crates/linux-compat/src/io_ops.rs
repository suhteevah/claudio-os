//! I/O multiplexing syscall implementations.
//!
//! Implements poll, select, epoll_wait, epoll_ctl, epoll_create1.

use crate::errno::*;
use crate::types::*;
use crate::file_ops::{FileDescriptorTable, FdType};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Epoll instance state.
#[derive(Debug, Clone)]
pub struct EpollInstance {
    /// Registered interest set: fd -> events.
    pub interests: BTreeMap<i32, EpollEvent>,
}

impl EpollInstance {
    pub fn new() -> Self {
        Self {
            interests: BTreeMap::new(),
        }
    }
}

/// Table of epoll instances, keyed by the epoll fd.
pub struct EpollTable {
    pub instances: BTreeMap<usize, EpollInstance>,
}

impl EpollTable {
    pub fn new() -> Self {
        Self {
            instances: BTreeMap::new(),
        }
    }
}

/// sys_poll(fds_ptr, nfds, timeout_ms) -> number of ready fds
pub fn sys_poll(fdt: &FileDescriptorTable, _fds_ptr: u64, nfds: u64, timeout: u64) -> i64 {
    // In kernel: read PollFd array from fds_ptr, check each fd for readiness,
    // write back revents.
    //
    // For a basic implementation:
    // - stdout/stderr are always writable (POLLOUT)
    // - stdin is readable if there's keyboard input
    // - regular files are always ready
    // - sockets check smoltcp state

    let _ = (fdt, nfds, timeout);

    // If timeout is 0 (non-blocking poll), return 0 (nothing ready).
    // If timeout > 0, we'd need to actually wait. Return 0 for now.
    0
}

/// sys_select(nfds, readfds_ptr, writefds_ptr, exceptfds_ptr, timeout_ptr) -> count
pub fn sys_select(
    fdt: &FileDescriptorTable,
    _nfds: u64,
    _readfds_ptr: u64,
    _writefds_ptr: u64,
    _exceptfds_ptr: u64,
    _timeout_ptr: u64,
) -> i64 {
    // Similar to poll but with fd_set bitmasks instead of pollfd arrays.
    // In kernel: read fd_set bitmaps, check each fd, write back ready sets.
    let _ = fdt;
    0
}

/// sys_epoll_create1(flags) -> epoll fd
pub fn sys_epoll_create1(
    fdt: &mut FileDescriptorTable,
    et: &mut EpollTable,
    flags: u64,
) -> i64 {
    let entry = crate::file_ops::FdEntry {
        path: alloc::string::String::from("<epoll>"),
        flags: if flags as u32 & EPOLL_CLOEXEC != 0 { O_CLOEXEC } else { 0 },
        position: 0,
        size: 0,
        buffer: Vec::new(),
        fd_type: FdType::Epoll,
        cloexec: (flags as u32 & EPOLL_CLOEXEC) != 0,
    };

    let fd = match fdt.alloc_fd(entry) {
        Ok(fd) => fd,
        Err(e) => return e.as_neg(),
    };

    et.instances.insert(fd, EpollInstance::new());

    log::trace!("epoll_create1(flags={}) -> fd {}", flags, fd);
    fd as i64
}

/// sys_epoll_ctl(epfd, op, fd, event_ptr) -> 0
pub fn sys_epoll_ctl(
    et: &mut EpollTable,
    epfd: u64,
    op: u64,
    fd: u64,
    _event_ptr: u64,
) -> i64 {
    let epfd = epfd as usize;
    let instance = match et.instances.get_mut(&epfd) {
        Some(inst) => inst,
        None => return EBADF.as_neg(),
    };

    let target_fd = fd as i32;

    match op as u32 {
        EPOLL_CTL_ADD => {
            if instance.interests.contains_key(&target_fd) {
                return EEXIST.as_neg();
            }
            // In kernel: read EpollEvent from event_ptr
            let event = EpollEvent {
                events: EPOLLIN | EPOLLOUT,
                data: fd,
            };
            instance.interests.insert(target_fd, event);
            0
        }
        EPOLL_CTL_MOD => {
            if !instance.interests.contains_key(&target_fd) {
                return ENOENT.as_neg();
            }
            // In kernel: read EpollEvent from event_ptr
            let event = EpollEvent {
                events: EPOLLIN | EPOLLOUT,
                data: fd,
            };
            instance.interests.insert(target_fd, event);
            0
        }
        EPOLL_CTL_DEL => {
            if instance.interests.remove(&target_fd).is_none() {
                return ENOENT.as_neg();
            }
            0
        }
        _ => EINVAL.as_neg(),
    }
}

/// sys_epoll_wait(epfd, events_ptr, maxevents, timeout) -> number of ready events
pub fn sys_epoll_wait(
    _fdt: &FileDescriptorTable,
    _et: &EpollTable,
    epfd: u64,
    _events_ptr: u64,
    _maxevents: u64,
    timeout: u64,
) -> i64 {
    let _ = epfd;

    // In kernel: check each registered fd for readiness,
    // write ready events to events_ptr, return count.
    //
    // If timeout == 0, return immediately.
    // If timeout == -1, block until at least one event.
    // If timeout > 0, wait up to timeout ms.

    if timeout == 0 {
        return 0; // Non-blocking, nothing ready
    }

    // Would block — return 0 for now
    0
}
