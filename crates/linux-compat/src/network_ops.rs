//! Network syscall implementations.
//!
//! Implements socket, connect, accept, sendto, recvfrom, sendmsg, recvmsg,
//! shutdown, bind, listen, getsockname, getpeername, socketpair, setsockopt,
//! getsockopt.
//!
//! These syscalls bridge to the smoltcp network stack via the kernel's
//! networking layer.

use crate::errno::*;
use crate::types::*;
use crate::file_ops::{FileDescriptorTable, FdEntry, FdType};
use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

/// Socket state for a single socket fd.
#[derive(Debug, Clone)]
pub struct SocketState {
    /// Address family (AF_INET, AF_INET6, AF_UNIX).
    pub domain: u32,
    /// Socket type (SOCK_STREAM, SOCK_DGRAM).
    pub sock_type: u32,
    /// Protocol (IPPROTO_TCP, IPPROTO_UDP).
    pub protocol: u32,
    /// Bound local address (if any).
    pub local_addr: Option<SockAddrIn>,
    /// Connected remote address (if any).
    pub remote_addr: Option<SockAddrIn>,
    /// Whether the socket is listening.
    pub listening: bool,
    /// Backlog for listen().
    pub backlog: u32,
    /// Socket options.
    pub options: BTreeMap<(u32, u32), Vec<u8>>,
    /// Receive buffer.
    pub recv_buf: Vec<u8>,
    /// Whether the socket is non-blocking.
    pub nonblocking: bool,
    /// Whether the socket has been shut down for reading.
    pub shut_rd: bool,
    /// Whether the socket has been shut down for writing.
    pub shut_wr: bool,
}

impl SocketState {
    pub fn new(domain: u32, sock_type: u32, protocol: u32) -> Self {
        Self {
            domain,
            sock_type: sock_type & !(SOCK_NONBLOCK | SOCK_CLOEXEC),
            protocol,
            local_addr: None,
            remote_addr: None,
            listening: false,
            backlog: 0,
            options: BTreeMap::new(),
            recv_buf: Vec::new(),
            nonblocking: (sock_type & SOCK_NONBLOCK) != 0,
            shut_rd: false,
            shut_wr: false,
        }
    }
}

/// Socket table: maps fd -> SocketState.
pub struct SocketTable {
    pub sockets: BTreeMap<usize, SocketState>,
}

impl SocketTable {
    pub fn new() -> Self {
        Self {
            sockets: BTreeMap::new(),
        }
    }

    pub fn get(&self, fd: usize) -> Result<&SocketState, Errno> {
        self.sockets.get(&fd).ok_or(ENOTSOCK)
    }

    pub fn get_mut(&mut self, fd: usize) -> Result<&mut SocketState, Errno> {
        self.sockets.get_mut(&fd).ok_or(ENOTSOCK)
    }
}

/// sys_socket(domain, type, protocol) -> fd
pub fn sys_socket(
    fdt: &mut FileDescriptorTable,
    st: &mut SocketTable,
    domain: u64,
    sock_type: u64,
    protocol: u64,
) -> i64 {
    let domain = domain as u32;
    let sock_type_raw = sock_type as u32;
    let protocol = protocol as u32;

    // Validate domain
    match domain {
        AF_INET | AF_INET6 | AF_UNIX => {}
        _ => return EAFNOSUPPORT.as_neg(),
    }

    // Validate socket type
    let base_type = sock_type_raw & !(SOCK_NONBLOCK | SOCK_CLOEXEC);
    match base_type {
        SOCK_STREAM | SOCK_DGRAM | SOCK_RAW => {}
        _ => return EINVAL.as_neg(),
    }

    let entry = FdEntry {
        path: String::from("<socket>"),
        flags: if sock_type_raw & SOCK_NONBLOCK != 0 { O_NONBLOCK } else { 0 },
        position: 0,
        size: 0,
        buffer: Vec::new(),
        fd_type: FdType::Socket,
        cloexec: (sock_type_raw & SOCK_CLOEXEC) != 0,
    };

    let fd = match fdt.alloc_fd(entry) {
        Ok(fd) => fd,
        Err(e) => return e.as_neg(),
    };

    let state = SocketState::new(domain, sock_type_raw, protocol);
    st.sockets.insert(fd, state);

    log::trace!("socket(domain={}, type={}, proto={}) -> fd {}", domain, sock_type_raw, protocol, fd);
    fd as i64
}

/// sys_connect(sockfd, addr_ptr, addrlen) -> 0
pub fn sys_connect(st: &mut SocketTable, sockfd: u64, _addr_ptr: u64, _addrlen: u64) -> i64 {
    let fd = sockfd as usize;
    let sock = match st.get_mut(fd) {
        Ok(s) => s,
        Err(e) => return e.as_neg(),
    };

    // In kernel: read sockaddr from addr_ptr, initiate TCP connection via smoltcp.
    // For now, mark as connected.
    sock.remote_addr = Some(SockAddrIn {
        sin_family: AF_INET as u16,
        sin_port: 0,
        sin_addr: 0,
        sin_zero: [0; 8],
    });

    log::trace!("connect(fd={}) -> 0", fd);
    0
}

/// sys_accept(sockfd, addr_ptr, addrlen_ptr) -> new fd
pub fn sys_accept(
    fdt: &mut FileDescriptorTable,
    st: &mut SocketTable,
    sockfd: u64,
    _addr_ptr: u64,
    _addrlen_ptr: u64,
) -> i64 {
    let fd = sockfd as usize;
    let sock = match st.get(fd) {
        Ok(s) => s,
        Err(e) => return e.as_neg(),
    };

    if !sock.listening {
        return EINVAL.as_neg();
    }

    // In kernel: wait for incoming connection via smoltcp, create new socket fd.
    // For now, return EAGAIN (would block).
    if sock.nonblocking {
        return EAGAIN.as_neg();
    }

    // Block until connection arrives — in kernel this would yield to the async executor.
    let _ = (fdt, st);
    EAGAIN.as_neg()
}

/// sys_sendto(sockfd, buf, len, flags, addr, addrlen) -> bytes sent
pub fn sys_sendto(
    st: &SocketTable,
    sockfd: u64,
    _buf: u64,
    len: u64,
    _flags: u64,
    _addr: u64,
    _addrlen: u64,
) -> i64 {
    let fd = sockfd as usize;
    match st.get(fd) {
        Ok(sock) => {
            if sock.shut_wr {
                return EPIPE.as_neg();
            }
            // In kernel: write data to smoltcp socket.
            len as i64
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_recvfrom(sockfd, buf, len, flags, addr, addrlen) -> bytes received
pub fn sys_recvfrom(
    st: &mut SocketTable,
    sockfd: u64,
    _buf: u64,
    _len: u64,
    _flags: u64,
    _addr: u64,
    _addrlen: u64,
) -> i64 {
    let fd = sockfd as usize;
    match st.get_mut(fd) {
        Ok(sock) => {
            if sock.shut_rd {
                return 0; // EOF
            }
            // In kernel: read from smoltcp socket receive buffer.
            // For now, return EAGAIN if nonblocking.
            if sock.nonblocking {
                return EAGAIN.as_neg();
            }
            0 // EOF
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_sendmsg(sockfd, msg_ptr, flags) -> bytes sent
pub fn sys_sendmsg(st: &SocketTable, sockfd: u64, _msg_ptr: u64, _flags: u64) -> i64 {
    let fd = sockfd as usize;
    match st.get(fd) {
        Ok(_) => 0, // In kernel: parse msghdr, scatter-gather send
        Err(e) => e.as_neg(),
    }
}

/// sys_recvmsg(sockfd, msg_ptr, flags) -> bytes received
pub fn sys_recvmsg(st: &mut SocketTable, sockfd: u64, _msg_ptr: u64, _flags: u64) -> i64 {
    let fd = sockfd as usize;
    match st.get_mut(fd) {
        Ok(sock) => {
            if sock.nonblocking {
                EAGAIN.as_neg()
            } else {
                0
            }
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_shutdown(sockfd, how) -> 0
pub fn sys_shutdown(st: &mut SocketTable, sockfd: u64, how: u64) -> i64 {
    let fd = sockfd as usize;
    match st.get_mut(fd) {
        Ok(sock) => {
            match how as u32 {
                SHUT_RD => sock.shut_rd = true,
                SHUT_WR => sock.shut_wr = true,
                SHUT_RDWR => {
                    sock.shut_rd = true;
                    sock.shut_wr = true;
                }
                _ => return EINVAL.as_neg(),
            }
            0
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_bind(sockfd, addr_ptr, addrlen) -> 0
pub fn sys_bind(st: &mut SocketTable, sockfd: u64, _addr_ptr: u64, _addrlen: u64) -> i64 {
    let fd = sockfd as usize;
    match st.get_mut(fd) {
        Ok(sock) => {
            // In kernel: read sockaddr, bind smoltcp socket.
            sock.local_addr = Some(SockAddrIn {
                sin_family: AF_INET as u16,
                sin_port: 0,
                sin_addr: 0,
                sin_zero: [0; 8],
            });
            0
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_listen(sockfd, backlog) -> 0
pub fn sys_listen(st: &mut SocketTable, sockfd: u64, backlog: u64) -> i64 {
    let fd = sockfd as usize;
    match st.get_mut(fd) {
        Ok(sock) => {
            if sock.sock_type != SOCK_STREAM {
                return EOPNOTSUPP.as_neg();
            }
            sock.listening = true;
            sock.backlog = backlog as u32;
            0
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_getsockname(sockfd, addr_ptr, addrlen_ptr) -> 0
pub fn sys_getsockname(st: &SocketTable, sockfd: u64, _addr_ptr: u64, _addrlen_ptr: u64) -> i64 {
    let fd = sockfd as usize;
    match st.get(fd) {
        Ok(_sock) => {
            // In kernel: write local address to addr_ptr
            0
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_getpeername(sockfd, addr_ptr, addrlen_ptr) -> 0
pub fn sys_getpeername(st: &SocketTable, sockfd: u64, _addr_ptr: u64, _addrlen_ptr: u64) -> i64 {
    let fd = sockfd as usize;
    match st.get(fd) {
        Ok(sock) => {
            if sock.remote_addr.is_none() {
                return ENOTCONN.as_neg();
            }
            0
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_socketpair(domain, type, protocol, sv_ptr) -> 0
pub fn sys_socketpair(
    fdt: &mut FileDescriptorTable,
    st: &mut SocketTable,
    domain: u64,
    sock_type: u64,
    protocol: u64,
    _sv_ptr: u64,
) -> i64 {
    if domain as u32 != AF_UNIX {
        return EAFNOSUPPORT.as_neg();
    }

    // Create two connected sockets
    let fd1 = sys_socket(fdt, st, domain, sock_type, protocol);
    if fd1 < 0 {
        return fd1;
    }
    let fd2 = sys_socket(fdt, st, domain, sock_type, protocol);
    if fd2 < 0 {
        let _ = fdt.close(fd1 as usize);
        return fd2;
    }

    // In kernel: write [fd1, fd2] to sv_ptr as i32 pair
    0
}

/// sys_setsockopt(sockfd, level, optname, optval_ptr, optlen) -> 0
pub fn sys_setsockopt(
    st: &mut SocketTable,
    sockfd: u64,
    level: u64,
    optname: u64,
    _optval_ptr: u64,
    _optlen: u64,
) -> i64 {
    let fd = sockfd as usize;
    match st.get_mut(fd) {
        Ok(sock) => {
            // Store the option value. In kernel: read optval from user memory.
            sock.options.insert((level as u32, optname as u32), alloc::vec![0u8; 4]);
            0
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_getsockopt(sockfd, level, optname, optval_ptr, optlen_ptr) -> 0
pub fn sys_getsockopt(
    st: &SocketTable,
    sockfd: u64,
    level: u64,
    optname: u64,
    _optval_ptr: u64,
    _optlen_ptr: u64,
) -> i64 {
    let fd = sockfd as usize;
    match st.get(fd) {
        Ok(sock) => {
            if sock.options.contains_key(&(level as u32, optname as u32)) {
                // In kernel: copy stored option to optval_ptr
                0
            } else {
                // Return default value (0)
                0
            }
        }
        Err(e) => e.as_neg(),
    }
}
