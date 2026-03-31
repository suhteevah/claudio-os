//! TLS transport layer.
//!
//! Provides a [`TlsStream`] type that wraps a smoltcp TCP socket with
//! TLS encryption via `embedded-tls`.  The API client crate consumes this
//! type to make HTTPS requests to api.anthropic.com.
//!
//! # Architecture
//!
//! `embedded-tls` operates over a read/write trait pair.  We implement those
//! traits on top of smoltcp TCP sockets by pulling bytes through the network
//! stack poll loop.
//!
//! The TLS handshake and record encryption are handled entirely by
//! `embedded-tls` — we just need to provide:
//! 1. A transport (TCP read/write)
//! 2. An RNG source
//! 3. A server certificate verifier (or skip verification for dev)
//!
//! # Current Status
//!
//! This module defines the interface that the rest of the system uses.  The
//! inner handshake and record layer are stubbed with `todo!()` markers at the
//! points where `embedded-tls` API calls will go — those require the full
//! network stack to be wired up end-to-end.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use smoltcp::iface::SocketHandle;
use smoltcp::time::Instant;

use crate::stack::NetworkStack;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors from TLS operations.
#[derive(Debug)]
pub enum TlsError {
    /// TCP connection failed.
    TcpConnect(TcpError),
    /// TLS handshake failed (bad certificate, protocol error, etc.).
    HandshakeFailed,
    /// The connection was closed by the peer.
    ConnectionClosed,
    /// Read/write error on the underlying TCP socket.
    Io,
    /// DNS resolution for the hostname failed.
    DnsError,
    /// Timed out waiting for a network operation.
    Timeout,
}

/// Errors from raw TCP operations.
#[derive(Debug)]
pub enum TcpError {
    /// No more TCP sockets available in the socket set.
    NoSocket,
    /// TCP connect timed out.
    Timeout,
    /// Connection refused.
    Refused,
    /// Connection reset by peer.
    Reset,
    /// Generic smoltcp error.
    Other,
}

// ---------------------------------------------------------------------------
// TCP connection helper
// ---------------------------------------------------------------------------

/// Maximum number of poll iterations while waiting for TCP connect / data.
const TCP_POLL_LIMIT: usize = 50_000;

/// Establish a raw TCP connection using smoltcp.
///
/// Returns the socket handle for the connected TCP socket.  The socket is
/// added to `stack.sockets` and must be removed by the caller when done.
pub fn tcp_connect(
    stack: &mut NetworkStack,
    remote_ip: smoltcp::wire::Ipv4Address,
    remote_port: u16,
    local_port: u16,
    now: impl Fn() -> Instant,
) -> Result<SocketHandle, TcpError> {
    use smoltcp::socket::tcp;

    let rx_buf = tcp::SocketBuffer::new(alloc::vec![0u8; 8192]);
    let tx_buf = tcp::SocketBuffer::new(alloc::vec![0u8; 8192]);
    let tcp_socket = tcp::Socket::new(rx_buf, tx_buf);
    let handle = stack.sockets.add(tcp_socket);

    // Initiate the connection.
    {
        let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
        let remote_endpoint = (smoltcp::wire::IpAddress::Ipv4(remote_ip), remote_port);
        socket
            .connect(stack.iface.context(), remote_endpoint, local_port)
            .map_err(|_| TcpError::Other)?;
    }

    log::debug!(
        "[tcp] connecting to {}:{} from local port {}",
        remote_ip,
        remote_port,
        local_port
    );

    // Poll until connected or failed.
    // HLT between polls to let the PIT timer tick — smoltcp needs advancing
    // timestamps for ARP resolution and TCP retransmission.
    let mut last_tick = 0i64;
    for i in 0..TCP_POLL_LIMIT {
        let ts = now();
        stack.iface.poll(ts, &mut stack.device, &mut stack.sockets);

        let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
        if socket.is_active() && socket.may_send() {
            log::info!("[tcp] connected to {}:{}", remote_ip, remote_port);
            return Ok(handle);
        }
        if socket.state() == tcp::State::Closed {
            log::warn!("[tcp] connection refused or reset");
            stack.sockets.remove(handle);
            return Err(TcpError::Refused);
        }

        // Log progress every ~2 seconds (36 ticks at 18 Hz)
        let current_ms = ts.total_millis();
        if current_ms - last_tick > 2000 {
            log::debug!("[tcp] waiting... ({}ms, iter {})", current_ms, i);
            last_tick = current_ms;
        }

        // Spin hint to let the CPU relax between polls.
        // The caller's `now()` function reads from a timer interrupt counter,
        // so we just need to not peg the CPU in a pure busy loop.
        for _ in 0..1000 {
            core::hint::spin_loop();
        }
    }

    log::warn!("[tcp] connection timed out");
    stack.sockets.remove(handle);
    Err(TcpError::Timeout)
}

/// Send data on a connected TCP socket, polling until all bytes are sent.
pub fn tcp_send(
    stack: &mut NetworkStack,
    handle: SocketHandle,
    data: &[u8],
    now: impl Fn() -> Instant,
) -> Result<(), TcpError> {
    let mut offset = 0;

    for _ in 0..TCP_POLL_LIMIT {
        let ts = now();
        stack.iface.poll(ts, &mut stack.device, &mut stack.sockets);

        let socket = stack.sockets.get_mut::<smoltcp::socket::tcp::Socket>(handle);
        if !socket.is_active() {
            return Err(TcpError::Reset);
        }
        if socket.can_send() {
            let sent = socket.send_slice(&data[offset..]).map_err(|_| TcpError::Other)?;
            offset += sent;
            if offset >= data.len() {
                // Flush: poll a few more times to actually transmit the buffered data
                for _ in 0..50 {
                    let ts2 = now();
                    stack.iface.poll(ts2, &mut stack.device, &mut stack.sockets);
                    for _ in 0..1000 { core::hint::spin_loop(); }
                }
                return Ok(());
            }
        }
        for _ in 0..100 { core::hint::spin_loop(); }
    }

    Err(TcpError::Timeout)
}

/// Receive data from a connected TCP socket into `buf`.
///
/// Returns the number of bytes received.  Returns `Ok(0)` if the remote peer
/// has closed the connection gracefully.
pub fn tcp_recv(
    stack: &mut NetworkStack,
    handle: SocketHandle,
    buf: &mut [u8],
    now: impl Fn() -> Instant,
) -> Result<usize, TcpError> {
    for _ in 0..TCP_POLL_LIMIT {
        let ts = now();
        stack.iface.poll(ts, &mut stack.device, &mut stack.sockets);

        let socket = stack.sockets.get_mut::<smoltcp::socket::tcp::Socket>(handle);

        if socket.can_recv() {
            let n = socket.recv_slice(buf).map_err(|_| TcpError::Other)?;
            return Ok(n);
        }

        if !socket.is_active() {
            // Connection closed gracefully — return what we have.
            return Ok(0);
        }
    }

    Err(TcpError::Timeout)
}

/// Close a TCP socket and remove it from the socket set.
pub fn tcp_close(stack: &mut NetworkStack, handle: SocketHandle) {
    {
        let socket = stack.sockets.get_mut::<smoltcp::socket::tcp::Socket>(handle);
        socket.close();
    }
    stack.sockets.remove(handle);
}

// ---------------------------------------------------------------------------
// TLS stream
// ---------------------------------------------------------------------------

/// A TLS-encrypted stream over a smoltcp TCP socket.
///
/// This type owns a TCP socket handle and layers TLS record
/// encryption/decryption on top.  It is the primary transport type consumed
/// by the HTTP client and API client crates.
pub struct TlsStream {
    /// The underlying TCP socket handle in the [`NetworkStack`] socket set.
    tcp_handle: SocketHandle,
    /// Server hostname for SNI and certificate verification.
    hostname: String,
    /// Whether the TLS handshake has completed.
    handshake_done: bool,
    // In a full implementation, this would hold the embedded-tls
    // `Connection<TcpTransport>` state.  That requires wiring up
    // embedded-tls's async read/write traits to our TCP helpers above.
}

impl TlsStream {
    /// Perform a TLS handshake over an already-connected TCP socket.
    ///
    /// # Arguments
    /// * `stack` — the network stack (for TCP I/O during handshake).
    /// * `tcp_handle` — a connected TCP socket from [`tcp_connect`].
    /// * `hostname` — server name for SNI extension.
    /// * `now` — timestamp provider.
    ///
    /// # Current Status
    /// This is a structural stub.  The actual handshake requires integrating
    /// `embedded-tls` with a certificate store and RNG, which will be
    /// completed when the full TLS pipeline is assembled.
    pub fn handshake(
        _stack: &mut NetworkStack,
        tcp_handle: SocketHandle,
        hostname: String,
        _now: impl Fn() -> Instant,
    ) -> Result<Self, TlsError> {
        log::info!("[tls] starting handshake with {}", hostname);

        // TODO: Initialize embedded-tls TlsConnection with:
        //   - TcpTransport adapter around our tcp_send/tcp_recv helpers
        //   - Server name (SNI)
        //   - Certificate verifier (trust-dns root certs or skip for dev)
        //   - RNG source (rdrand instruction or software PRNG)
        //
        // The handshake sequence:
        //   1. ClientHello (with SNI, supported cipher suites)
        //   2. ServerHello + Certificate + ServerKeyExchange
        //   3. Verify certificate chain
        //   4. ClientKeyExchange + ChangeCipherSpec + Finished
        //   5. Server ChangeCipherSpec + Finished
        //
        // After handshake, all subsequent reads/writes go through the
        // TLS record layer.

        todo!(
            "[tls] embedded-tls handshake not yet wired up — \
             requires RNG + cert store integration"
        );

        #[allow(unreachable_code)]
        Ok(Self {
            tcp_handle,
            hostname,
            handshake_done: true,
        })
    }

    /// Send encrypted data over the TLS connection.
    ///
    /// The plaintext `data` is encrypted by the TLS record layer and sent
    /// over the underlying TCP socket.
    pub fn send(
        &mut self,
        _stack: &mut NetworkStack,
        _data: &[u8],
        _now: impl Fn() -> Instant,
    ) -> Result<usize, TlsError> {
        if !self.handshake_done {
            return Err(TlsError::HandshakeFailed);
        }

        // TODO: encrypted_tls::Connection::write(data) -> TCP
        todo!("[tls] send not yet wired up");
    }

    /// Receive and decrypt data from the TLS connection.
    ///
    /// Returns the number of plaintext bytes written to `buf`.
    pub fn recv(
        &mut self,
        _stack: &mut NetworkStack,
        _buf: &mut [u8],
        _now: impl Fn() -> Instant,
    ) -> Result<usize, TlsError> {
        if !self.handshake_done {
            return Err(TlsError::HandshakeFailed);
        }

        // TODO: encrypted_tls::Connection::read(buf) <- TCP
        todo!("[tls] recv not yet wired up");
    }

    /// Close the TLS connection gracefully (send close_notify) and tear down
    /// the underlying TCP socket.
    pub fn close(self, stack: &mut NetworkStack) {
        // TODO: send TLS close_notify alert
        tcp_close(stack, self.tcp_handle);
        log::debug!("[tls] connection to {} closed", self.hostname);
    }

    /// The underlying TCP socket handle, for use in advanced scenarios.
    pub fn tcp_handle(&self) -> SocketHandle {
        self.tcp_handle
    }

    /// The server hostname this stream is connected to.
    pub fn hostname(&self) -> &str {
        &self.hostname
    }
}
