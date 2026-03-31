//! TLS transport layer.
//!
//! Provides a [`TlsStream`] type that wraps a smoltcp TCP socket with
//! TLS encryption via `embedded-tls`.  The API client crate consumes this
//! type to make HTTPS requests to api.anthropic.com.
//!
//! # Architecture
//!
//! `embedded-tls` operates over a blocking `embedded_io::Read + Write` pair.
//! We implement those traits on [`SmoltcpSocket`], which wraps a smoltcp TCP
//! socket handle and busy-polls the [`NetworkStack`] to move bytes.
//!
//! The TLS handshake and record encryption are handled entirely by
//! `embedded-tls` — we provide:
//! 1. A transport ([`SmoltcpSocket`] implementing `embedded_io::Read + Write`)
//! 2. An RNG source ([`DevRng`] — counter-based PRNG, seeded from PIT ticks)
//! 3. Certificate verification skipped via `NoVerify` (dev mode)

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::iface::SocketHandle;
use smoltcp::time::Instant;

use crate::stack::NetworkStack;

// Re-export embedded-tls blocking types we use.
use embedded_tls::blocking::{Aes128GcmSha256, NoVerify, TlsConfig, TlsConnection, TlsContext};

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
    /// embedded-tls error (wrapped).
    Tls(embedded_tls::TlsError),
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
// TCP connection helpers (kept for auth relay code)
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

    let rx_buf = tcp::SocketBuffer::new(vec![0u8; 8192]);
    let tx_buf = tcp::SocketBuffer::new(vec![0u8; 8192]);
    let tcp_socket = tcp::Socket::new(rx_buf, tx_buf);
    let handle = stack.sockets.add(tcp_socket);

    // Initiate the connection.
    {
        let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
        // Disable Nagle — send data immediately, don't wait to combine small packets.
        // Critical for the proxy flow where we send one HTTP request and wait for response.
        socket.set_nagle_enabled(false);
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

        // Log progress every ~2 seconds
        let current_ms = ts.total_millis();
        if current_ms - last_tick > 2000 {
            log::debug!("[tcp] waiting... ({}ms, iter {})", current_ms, i);
            last_tick = current_ms;
        }

        for _ in 0..1000 {
            core::hint::spin_loop();
        }
    }

    log::warn!("[tcp] connection timed out");
    stack.sockets.remove(handle);
    Err(TcpError::Timeout)
}

/// Send data on a connected TCP socket, polling until all bytes are sent
/// AND the TCP TX buffer is fully drained (data ACKed by remote peer).
pub fn tcp_send(
    stack: &mut NetworkStack,
    handle: SocketHandle,
    data: &[u8],
    now: impl Fn() -> Instant,
) -> Result<(), TcpError> {
    let mut offset = 0;

    log::debug!("[tcp] tcp_send: {} bytes to send", data.len());

    // Phase 1: Push all data into smoltcp's TCP socket TX buffer.
    for i in 0..TCP_POLL_LIMIT {
        let ts = now();
        stack.iface.poll(ts, &mut stack.device, &mut stack.sockets);

        let socket = stack.sockets.get_mut::<smoltcp::socket::tcp::Socket>(handle);
        if !socket.is_active() {
            log::error!("[tcp] tcp_send: socket became inactive during send (state: {:?})", socket.state());
            return Err(TcpError::Reset);
        }
        if socket.can_send() {
            let sent = socket.send_slice(&data[offset..]).map_err(|_| TcpError::Other)?;
            if sent > 0 {
                offset += sent;
                log::debug!("[tcp] tcp_send: buffered {} bytes ({}/{})", sent, offset, data.len());
            }
            if offset >= data.len() {
                log::debug!("[tcp] tcp_send: all {} bytes buffered in smoltcp TX", data.len());
                break;
            }
        }
        if i % 5000 == 0 && i > 0 {
            log::debug!("[tcp] tcp_send: still buffering... iter {}", i);
        }
        for _ in 0..100 { core::hint::spin_loop(); }
    }

    if offset < data.len() {
        log::error!("[tcp] tcp_send: timed out buffering data ({}/{})", offset, data.len());
        return Err(TcpError::Timeout);
    }

    // Phase 2: Flush — poll the interface repeatedly with real waits to ensure
    // smoltcp generates TCP segments, the VirtIO driver transmits them, and
    // the remote peer's ACKs are processed.
    //
    // This is critical: send_slice() only puts data in smoltcp's TX buffer.
    // iface.poll() generates TCP segments and passes them to the Device::transmit
    // callback, which pushes them through VirtIO. We need enough poll cycles for:
    //   1. smoltcp to segment the data and call Device::transmit
    //   2. VirtIO to DMA the frame to QEMU
    //   3. The remote TCP stack to ACK
    //   4. Our NIC to receive the ACK frame
    //   5. smoltcp to process the ACK and advance the TX window
    //
    // We monitor the socket's send queue size to know when all data has been
    // ACKed (send_queue drops to 0).
    log::debug!("[tcp] tcp_send: flushing TX buffer...");
    let mut flush_polls = 0u32;
    let max_flush = 2000u32; // Up to ~2000 iterations with HLT (~110 seconds at 18Hz timer)

    for _ in 0..max_flush {
        // Poll multiple times per HLT cycle to process both TX and RX
        for _ in 0..5 {
            let ts = now();
            stack.iface.poll(ts, &mut stack.device, &mut stack.sockets);
        }

        let socket = stack.sockets.get_mut::<smoltcp::socket::tcp::Socket>(handle);
        if !socket.is_active() {
            log::error!("[tcp] tcp_send flush: socket became inactive (state: {:?})", socket.state());
            return Err(TcpError::Reset);
        }

        let send_queue = socket.send_queue();
        flush_polls += 1;

        if send_queue == 0 {
            log::info!(
                "[tcp] tcp_send: TX buffer fully drained after {} flush cycles — all data ACKed",
                flush_polls
            );
            return Ok(());
        }

        if flush_polls % 100 == 0 {
            log::debug!(
                "[tcp] tcp_send flush: {} bytes still in TX queue (cycle {})",
                send_queue, flush_polls
            );
        }

        // Wait for a timer/NIC interrupt to avoid busy-spinning.
        // enable_and_hlt atomically enables interrupts and halts.
        // On QEMU with 18.2Hz PIT timer, each HLT is ~55ms.
        x86_64::instructions::interrupts::enable_and_hlt();
        x86_64::instructions::interrupts::disable();
    }

    // If we get here, the TX buffer didn't fully drain, but some data may have
    // been transmitted. This isn't necessarily fatal — the remote side may have
    // received enough to process. Log a warning and continue.
    let socket = stack.sockets.get_mut::<smoltcp::socket::tcp::Socket>(handle);
    let remaining = socket.send_queue();
    log::warn!(
        "[tcp] tcp_send flush: {} bytes still in TX queue after {} cycles (may be OK if peer got the data)",
        remaining, max_flush
    );

    Ok(())
}

/// Receive data from a connected TCP socket into `buf`.
///
/// Returns the number of bytes received.  Returns `Ok(0)` if the remote peer
/// has closed the connection gracefully.
///
/// This function polls the network stack with HLT waits between iterations,
/// allowing the NIC and timer interrupts to fire. Each iteration does multiple
/// smoltcp polls to process any queued frames before checking the socket.
pub fn tcp_recv(
    stack: &mut NetworkStack,
    handle: SocketHandle,
    buf: &mut [u8],
    now: impl Fn() -> Instant,
) -> Result<usize, TcpError> {
    for i in 0..TCP_POLL_LIMIT {
        // Poll NIC + smoltcp multiple times to drain any queued packets.
        // Each poll() call processes one RX frame and may generate TX frames
        // (e.g. TCP ACKs), so we need multiple polls to handle bursts.
        for _ in 0..10 {
            let ts = now();
            stack.iface.poll(ts, &mut stack.device, &mut stack.sockets);
        }

        let socket = stack.sockets.get_mut::<smoltcp::socket::tcp::Socket>(handle);

        if socket.can_recv() {
            let n = socket.recv_slice(buf).map_err(|_| TcpError::Other)?;
            log::debug!("[tcp] tcp_recv: got {} bytes", n);
            return Ok(n);
        }

        // Check socket state
        let state = socket.state();
        if !socket.is_active() {
            log::debug!("[tcp] tcp_recv: socket not active (state: {:?}), returning 0", state);
            return Ok(0);
        }
        // CloseWait = remote sent FIN, no more data coming. EOF.
        if state == smoltcp::socket::tcp::State::CloseWait && !socket.can_recv() {
            log::debug!("[tcp] tcp_recv: CloseWait + empty recv = EOF");
            return Ok(0);
        }
        // TimeWait / LastAck / Closing = connection is tearing down
        if matches!(state,
            smoltcp::socket::tcp::State::TimeWait |
            smoltcp::socket::tcp::State::LastAck |
            smoltcp::socket::tcp::State::Closing
        ) {
            log::debug!("[tcp] tcp_recv: connection closing (state: {:?}), returning 0", state);
            return Ok(0);
        }

        // Wait for a timer or NIC interrupt.
        // The PIT fires at ~18.2Hz (~55ms). NIC interrupts fire on packet arrival.
        // We keep interrupts disabled between HLTs to avoid re-entrancy issues
        // with our single-threaded bare-metal design. The HLT itself atomically
        // enables interrupts and waits.
        x86_64::instructions::interrupts::enable_and_hlt();
        x86_64::instructions::interrupts::disable();

        if i % 200 == 0 && i > 0 {
            log::debug!(
                "[tcp] tcp_recv: waiting... iter {} (~{}ms elapsed, state: {:?}, send_q: {}, recv_q: {})",
                i,
                i * 55, // approximate: 55ms per HLT at 18.2Hz
                state,
                socket.send_queue(),
                socket.recv_queue(),
            );
        }
    }

    log::warn!("[tcp] tcp_recv: timed out after {} iterations", TCP_POLL_LIMIT);
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
// SmoltcpSocket — bridge between smoltcp TCP and embedded_io traits
// ---------------------------------------------------------------------------

/// A wrapper around a smoltcp TCP socket handle + NetworkStack that implements
/// `embedded_io::Read` and `embedded_io::Write`.
///
/// This is the "glue" that lets `embedded-tls` do blocking I/O over our
/// smoltcp network stack.  Both sides are blocking/busy-poll, so Read loops
/// until data is available and Write loops until all bytes are accepted.
///
/// # Safety / Lifetime
///
/// The `SmoltcpSocket` holds a raw pointer to the `NetworkStack` because
/// `embedded-tls` requires the socket to be a single object that implements
/// both Read and Write, but our NetworkStack is behind a mutable reference
/// that the TlsConnection also needs to borrow.  The pointer is valid for
/// the duration of the TLS connection.
pub struct SmoltcpSocket {
    /// Raw pointer to the network stack.  Valid for the lifetime of the
    /// enclosing `TlsStream`.
    stack: *mut NetworkStack,
    /// The TCP socket handle in the NetworkStack's socket set.
    handle: SocketHandle,
    /// Timestamp provider.  Returns smoltcp `Instant` for polling.
    now: fn() -> Instant,
}

// SmoltcpSocket is only used within a single thread (bare-metal, no threading),
// so these impls are safe in our context.
unsafe impl Send for SmoltcpSocket {}
unsafe impl Sync for SmoltcpSocket {}

impl SmoltcpSocket {
    /// Create a new SmoltcpSocket.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - `stack` points to a valid `NetworkStack` for the entire lifetime of
    ///   this `SmoltcpSocket`.
    /// - `handle` is a valid TCP socket handle in that stack's socket set.
    /// - No other code mutates the `NetworkStack` while this socket is in use
    ///   (single-threaded bare-metal — this is guaranteed by construction).
    unsafe fn new(stack: *mut NetworkStack, handle: SocketHandle, now: fn() -> Instant) -> Self {
        Self { stack, handle, now }
    }

    /// Get a mutable reference to the network stack.
    fn stack_mut(&mut self) -> &mut NetworkStack {
        unsafe { &mut *self.stack }
    }
}

impl embedded_io::ErrorType for SmoltcpSocket {
    type Error = embedded_io::ErrorKind;
}

impl embedded_io::Read for SmoltcpSocket {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        log::trace!("[smoltcp-io] read: requested up to {} bytes", buf.len());

        // Poll the NIC + smoltcp in a loop, waiting for data via HLT between
        // iterations. Used by embedded-tls for TLS handshake and record reads.
        for i in 0..TCP_POLL_LIMIT {
            // Poll multiple times per iteration to catch incoming packets
            // and send any pending ACKs/responses
            for _ in 0..10 {
                let ts = (self.now)();
                let handle = self.handle;
                let stack = self.stack_mut();
                stack.iface.poll(ts, &mut stack.device, &mut stack.sockets);
            }

            let handle = self.handle;
            let stack = self.stack_mut();
            let socket = stack
                .sockets
                .get_mut::<smoltcp::socket::tcp::Socket>(handle);

            if socket.can_recv() {
                let n = socket
                    .recv_slice(buf)
                    .map_err(|_| embedded_io::ErrorKind::Other)?;
                log::debug!("[smoltcp-io] read: got {} bytes (requested up to {})", n, buf.len());
                return Ok(n);
            }

            if !socket.is_active() {
                log::debug!("[smoltcp-io] read: socket not active, returning EOF");
                return Ok(0);
            }

            // Wait for a timer/NIC interrupt instead of busy-spinning.
            // This saves CPU and ensures proper timing for smoltcp.
            // For the first few iterations, use short spin-waits for low-latency
            // responses (e.g. during TLS handshake). After that, use HLT.
            if i < 50 {
                for _ in 0..5000 { core::hint::spin_loop(); }
            } else {
                x86_64::instructions::interrupts::enable_and_hlt();
                x86_64::instructions::interrupts::disable();
            }

            if i % 500 == 0 && i > 0 {
                log::debug!("[smoltcp-io] read: waiting for data... iter {} (~{}ms)", i, i * 55);
            }
        }

        log::error!("[smoltcp-io] read: TIMED OUT after {} iterations", TCP_POLL_LIMIT);
        Err(embedded_io::ErrorKind::TimedOut)
    }
}

impl embedded_io::Write for SmoltcpSocket {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        log::trace!("[smoltcp-io] write: {} bytes to send", buf.len());

        // Busy-poll until the socket can accept data.
        for i in 0..TCP_POLL_LIMIT {
            let ts = (self.now)();
            let handle = self.handle;
            let stack = self.stack_mut();
            stack.iface.poll(ts, &mut stack.device, &mut stack.sockets);

            let socket = stack
                .sockets
                .get_mut::<smoltcp::socket::tcp::Socket>(handle);

            if !socket.is_active() {
                log::error!("[smoltcp-io] write: socket not active (connection reset)");
                return Err(embedded_io::ErrorKind::ConnectionReset);
            }

            if socket.can_send() {
                let n = socket
                    .send_slice(buf)
                    .map_err(|_| embedded_io::ErrorKind::Other)?;
                log::debug!("[smoltcp-io] write: sent {} of {} bytes", n, buf.len());
                return Ok(n);
            }

            if i % 1000 == 0 && i > 0 {
                log::debug!("[smoltcp-io] write: waiting for socket to accept data... iter {}", i);
            }

            for _ in 0..100 {
                core::hint::spin_loop();
            }
        }

        log::error!("[smoltcp-io] write: TIMED OUT after {} iterations", TCP_POLL_LIMIT);
        Err(embedded_io::ErrorKind::TimedOut)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        // Poll the network stack until the TCP TX buffer is drained, meaning
        // all data has been segmented, transmitted, and ACKed by the peer.
        // This is critical for TLS: after writing a handshake message or
        // application data, we need it to actually reach the peer before
        // we start reading the response.
        for i in 0..1000u32 {
            // Multiple polls per iteration to process both TX and RX
            for _ in 0..5 {
                let ts = (self.now)();
                let handle = self.handle;
                let stack = self.stack_mut();
                stack.iface.poll(ts, &mut stack.device, &mut stack.sockets);
            }

            // Check if the TX buffer is drained
            let handle = self.handle;
            let stack = self.stack_mut();
            let socket = stack.sockets.get_mut::<smoltcp::socket::tcp::Socket>(handle);
            let send_queue = socket.send_queue();

            if send_queue == 0 {
                if i > 10 {
                    log::trace!("[smoltcp-io] flush: TX drained after {} cycles", i);
                }
                return Ok(());
            }

            // Wait for interrupt to avoid pure busy-spin
            if i > 20 {
                x86_64::instructions::interrupts::enable_and_hlt();
                x86_64::instructions::interrupts::disable();
            } else {
                for _ in 0..500 {
                    core::hint::spin_loop();
                }
            }
        }

        // Even if not fully drained, don't fail — the data may still arrive.
        log::warn!("[smoltcp-io] flush: TX buffer not fully drained after 1000 cycles");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DevRng — development-mode PRNG for TLS handshake
// ---------------------------------------------------------------------------

/// A simple counter-based PRNG for use during TLS handshakes.
///
/// This is NOT cryptographically secure.  It uses a counter XOR'd with
/// a seed derived from the PIT tick count at creation time.  This is
/// sufficient for development / QEMU testing where we need the TLS
/// handshake to produce unique nonces but do not yet have a proper
/// entropy source.
///
/// TODO: Replace with RDRAND-based RNG on hardware that supports it,
/// or with a proper CSPRNG seeded from multiple entropy sources.
pub struct DevRng {
    state: u64,
}

impl DevRng {
    /// Create a new DevRng seeded from the given value.
    pub fn new(seed: u64) -> Self {
        let state = if seed == 0 { 0xDEAD_BEEF_CAFE_BABEu64 } else { seed };
        Self { state }
    }

    fn next(&mut self) -> u64 {
        // xorshift64* — RDRAND removed (crashes on QEMU default CPU)
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

impl rand_core::RngCore for DevRng {
    fn next_u32(&mut self) -> u32 {
        self.next() as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.next()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let mut i = 0;
        while i < dest.len() {
            let val = self.next();
            let bytes = val.to_le_bytes();
            let remaining = dest.len() - i;
            let to_copy = if remaining < 8 { remaining } else { 8 };
            dest[i..i + to_copy].copy_from_slice(&bytes[..to_copy]);
            i += to_copy;
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl rand_core::CryptoRng for DevRng {}

// ---------------------------------------------------------------------------
// Ephemeral local port allocator
// ---------------------------------------------------------------------------

/// Simple local port counter.  Starts at 49152 (start of IANA ephemeral
/// range) and increments.  Not thread-safe, but we are single-threaded.
static mut LOCAL_PORT_COUNTER: u16 = 49152;

fn next_local_port() -> u16 {
    unsafe {
        let port = LOCAL_PORT_COUNTER;
        LOCAL_PORT_COUNTER = if LOCAL_PORT_COUNTER >= 65534 {
            49152
        } else {
            LOCAL_PORT_COUNTER + 1
        };
        port
    }
}

// ---------------------------------------------------------------------------
// TLS record buffer size
// ---------------------------------------------------------------------------

/// Maximum TLS record size.  16384 bytes of payload + 256 bytes of overhead.
/// embedded-tls docs recommend 16640.
const TLS_RECORD_BUF_SIZE: usize = 16640;

// ---------------------------------------------------------------------------
// 16-byte aligned buffer allocation for SSE/AES-NI
// ---------------------------------------------------------------------------

/// Allocate a heap buffer with 16-byte alignment, required for AES-NI/SSE
/// instructions used by embedded-tls's AES-128-GCM cipher.
///
/// `vec![0u8; N]` only guarantees 1-byte alignment (align_of::<u8>()).
/// AES-NI instructions like AESENC/AESDEC and SSE moves (MOVAPS) require
/// 16-byte aligned operands.  If the allocator returns a non-16-byte-aligned
/// address, these instructions trigger a #GP (general protection) fault,
/// which escalates to a double fault.
fn alloc_aligned_buf(size: usize) -> Box<[u8]> {
    let layout = alloc::alloc::Layout::from_size_align(size, 16)
        .expect("[tls] invalid aligned buffer layout");
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "[tls] failed to allocate aligned TLS buffer");
    log::debug!(
        "[tls] allocated {} byte TLS buffer at {:p} (alignment: {})",
        size, ptr, 16
    );
    // Verify alignment
    debug_assert!(
        ptr as usize % 16 == 0,
        "[tls] buffer allocation not 16-byte aligned: {:p}",
        ptr
    );
    // SAFETY: We allocated `size` bytes with alignment 16 via alloc_zeroed.
    // Box<[u8]>::drop will deallocate with Layout { size, align: 1 }, which
    // is technically a mismatch.  This is sound with linked_list_allocator
    // because it tracks allocations by address and size only — alignment is
    // not stored or checked on dealloc.  The pointer and size are correct.
    unsafe { Box::from_raw(core::slice::from_raw_parts_mut(ptr, size)) }
}

// ---------------------------------------------------------------------------
// TlsStream — the main public type
// ---------------------------------------------------------------------------

/// A TLS-encrypted stream over a smoltcp TCP socket.
///
/// This type owns a TCP socket handle and layers TLS 1.3 record
/// encryption/decryption on top via `embedded-tls`.  It is the primary
/// transport type consumed by the HTTP client and API client crates.
///
/// The embedded-tls `TlsConnection` requires borrowed buffers and a socket
/// with lifetime `'a`.  To own everything in one struct (necessary because
/// we cannot return references to stack-local data), we heap-allocate the
/// record buffers and use `ManuallyDrop` / raw pointers for the connection.
///
/// # Lifecycle
///
/// 1. [`TlsStream::connect()`] — TCP connect + TLS handshake
/// 2. [`TlsStream::send()`] / [`TlsStream::recv()`] — encrypted I/O
/// 3. [`TlsStream::close()`] — close_notify + TCP teardown
pub struct TlsStream {
    /// The underlying TCP socket handle in the [`NetworkStack`] socket set.
    tcp_handle: SocketHandle,
    /// Server hostname for SNI and certificate verification.
    hostname: String,
    /// Whether the TLS handshake has completed.
    handshake_done: bool,
    /// Heap-allocated TLS connection state.
    ///
    /// This is `Option` so we can take it in `close()`.  After `connect()`
    /// succeeds, this is always `Some`.
    ///
    /// The inner type is erased to avoid leaking the lifetime parameter
    /// from `TlsConnection<'a, SmoltcpSocket, Aes128GcmSha256>`.  We use
    /// a boxed trait-object-like wrapper (`TlsState`) that owns the
    /// connection, its buffers, and the socket.
    tls_state: Option<Box<TlsState>>,
}

/// Owns the embedded-tls connection along with its backing buffers and socket.
///
/// All the borrowed data that `TlsConnection` references is heap-allocated
/// and pinned here so the borrows remain valid.
struct TlsState {
    /// The TLS connection.  This borrows from `read_buf` and `write_buf`
    /// below via raw pointers, so this field MUST be dropped before the
    /// buffers.
    conn: TlsConnection<'static, SmoltcpSocket, Aes128GcmSha256>,

    /// Heap-allocated TLS record read buffer.
    /// SAFETY: This must outlive `conn`.  `conn` holds a `&'static mut [u8]`
    /// pointing into this allocation, transmuted from the true lifetime.
    /// This is sound because we always drop `conn` (by dropping `TlsState`)
    /// before freeing the buffer (Rust drops fields in declaration order,
    /// but we `ManuallyDrop` to be explicit — actually, since `conn` is first,
    /// it gets dropped first, which is correct).
    _read_buf: Box<[u8]>,

    /// Heap-allocated TLS record write buffer.
    _write_buf: Box<[u8]>,
}

impl TlsStream {
    /// Establish a TLS connection to a remote server.
    ///
    /// This performs the full sequence:
    /// 1. TCP three-way handshake
    /// 2. TLS 1.3 handshake (ClientHello through Finished)
    ///
    /// # Arguments
    /// * `stack` — the network stack (must have IP from DHCP).
    /// * `remote_ip` — the server's IPv4 address (resolve with `dns::resolve` first).
    /// * `remote_port` — the server's port (usually 443 for HTTPS).
    /// * `hostname` — server name for SNI extension (e.g. `"api.anthropic.com"`).
    /// * `now` — timestamp provider function (must be a `fn` pointer for storage).
    /// * `rng_seed` — seed for the PRNG (e.g. PIT tick count).
    pub fn connect(
        stack: &mut NetworkStack,
        remote_ip: smoltcp::wire::Ipv4Address,
        remote_port: u16,
        hostname: &str,
        now: fn() -> Instant,
        rng_seed: u64,
    ) -> Result<Self, TlsError> {
        let local_port = next_local_port();
        log::info!(
            "[tls] connecting to {}:{} (SNI: {}, local port: {})",
            remote_ip,
            remote_port,
            hostname,
            local_port
        );

        // Step 1: TCP connect.
        let tcp_handle = tcp_connect(stack, remote_ip, remote_port, local_port, now)
            .map_err(TlsError::TcpConnect)?;

        // Step 2: Set up the embedded-tls connection.
        //
        // We need to create:
        //   - A SmoltcpSocket (embedded_io Read+Write over our TCP socket)
        //   - Heap-allocated read/write record buffers
        //   - TlsConfig with SNI
        //   - TlsConnection
        //   - Call open() for the handshake

        // Create the socket adapter.  We pass a raw pointer to the stack
        // because TlsConnection needs to own the socket, but we also need
        // the stack pointer to stay valid.
        let mut socket = unsafe { SmoltcpSocket::new(stack as *mut NetworkStack, tcp_handle, now) };

        // -----------------------------------------------------------
        // Pre-handshake socket validation: verify the SmoltcpSocket's
        // Read/Write impls work before handing control to embedded-tls.
        // If the raw pointer dereference or network polling is broken,
        // we'll get a clear error here instead of a double-fault inside
        // the TLS handshake.
        // -----------------------------------------------------------
        {
            use embedded_io::Write;
            log::debug!("[tls] pre-handshake: verifying SmoltcpSocket write works...");
            // Don't actually send data (that would confuse the TLS peer),
            // but verify that write(empty) and flush() don't fault.
            match socket.write(&[]) {
                Ok(0) => log::debug!("[tls] pre-handshake: write(empty) OK"),
                Ok(n) => log::warn!("[tls] pre-handshake: write(empty) returned {}", n),
                Err(e) => {
                    log::error!("[tls] pre-handshake: write(empty) failed: {:?}", e);
                    tcp_close(stack, tcp_handle);
                    return Err(TlsError::Io);
                }
            }
            match socket.flush() {
                Ok(()) => log::debug!("[tls] pre-handshake: flush OK"),
                Err(e) => {
                    log::error!("[tls] pre-handshake: flush failed: {:?}", e);
                    tcp_close(stack, tcp_handle);
                    return Err(TlsError::Io);
                }
            }
            log::debug!("[tls] pre-handshake: SmoltcpSocket validated successfully");
        }

        // Heap-allocate TLS record buffers with 16-byte alignment.
        //
        // CRITICAL: AES-NI instructions (AESENC, AESDEC, etc.) and SSE
        // operations (MOVAPS, PXOR on XMM registers) require 16-byte
        // aligned memory.  Our target (x86_64-claudio.json) enables
        // +sse,+sse2,+aes,+pclmulqdq, so LLVM will emit these instructions
        // for the AES-128-GCM cipher.
        //
        // vec![0u8; N] uses Layout { align: 1 } because align_of::<u8>() == 1.
        // linked_list_allocator may return addresses that are NOT 16-byte
        // aligned, causing a #GP fault on the first SSE-aligned memory access
        // inside embedded-tls, which escalates to a double fault.
        let mut read_buf: Box<[u8]> = alloc_aligned_buf(TLS_RECORD_BUF_SIZE);
        let mut write_buf: Box<[u8]> = alloc_aligned_buf(TLS_RECORD_BUF_SIZE);

        log::debug!(
            "[tls] record buffers: read={:p} write={:p} (both 16-byte aligned: {}, {})",
            read_buf.as_ptr(),
            write_buf.as_ptr(),
            read_buf.as_ptr() as usize % 16 == 0,
            write_buf.as_ptr() as usize % 16 == 0,
        );

        // SAFETY: We transmute the mutable slice references to 'static.
        // This is sound because:
        // 1. The Box<[u8]> allocations are owned by TlsState and live as
        //    long as the TlsConnection.
        // 2. TlsConnection is dropped before the buffers (field order).
        // 3. No other code accesses these buffers while TlsConnection exists.
        let read_buf_ref: &'static mut [u8] =
            unsafe { core::mem::transmute(read_buf.as_mut() as &mut [u8]) };
        let write_buf_ref: &'static mut [u8] =
            unsafe { core::mem::transmute(write_buf.as_mut() as &mut [u8]) };

        // Create the TLS connection on the heap immediately.
        //
        // CRITICAL: TlsConnection contains AES round key arrays and cipher
        // state that LLVM may access with aligned SSE instructions.  If
        // TlsConnection lives on the stack, the stack frame alignment may
        // not satisfy SSE requirements (the stack is typically 16-byte
        // aligned on x86_64, but nested function calls or large frames can
        // break this).  By Boxing immediately, we ensure the TlsConnection
        // struct is heap-allocated (where alloc_aligned_buf guarantees 16B
        // alignment for the buffers, and the global allocator typically
        // returns 8/16-byte aligned memory for large allocations).
        //
        // More importantly, TlsConnection is a LARGE struct (contains cipher
        // state, handshake transcript, etc.).  Keeping it on the stack during
        // the open() handshake puts enormous pressure on the stack frame.
        // Boxing it moves the bulk of the data to the heap.
        let conn: TlsConnection<'static, SmoltcpSocket, Aes128GcmSha256> =
            TlsConnection::new(socket, read_buf_ref, write_buf_ref);

        let mut tls_state = Box::new(TlsState {
            conn,
            _read_buf: read_buf,
            _write_buf: write_buf,
        });

        // Configure TLS: set server name for SNI.
        let hostname_owned: String = String::from(hostname);

        // SAFETY: We transmute the &str to 'static. The String is owned
        // by the TlsStream and lives until close().  The TlsConfig only
        // needs the name during the handshake (open() call below), so
        // this is safe.
        let hostname_static: &'static str =
            unsafe { core::mem::transmute(hostname_owned.as_str()) };

        let tls_config: TlsConfig<'static, Aes128GcmSha256> =
            TlsConfig::new().with_server_name(hostname_static);

        let mut rng = DevRng::new(rng_seed);
        let context = TlsContext::new(&tls_config, &mut rng);

        log::info!("[tls] starting TLS 1.3 handshake with {}", hostname);
        log::info!("[tls] handshake: conn.open() about to be called — this sends ClientHello and waits for ServerHello + Finished");
        log::info!("[tls] handshake: TlsConfig SNI = {}, cipher = AES-128-GCM-SHA256, verify = NoVerify", hostname);
        // Perform the TLS handshake on the heap-allocated TlsConnection.
        // The conn is inside Box<TlsState>, so the handshake crypto
        // (AES-GCM key expansion, ECDHE, etc.) operates on heap memory
        // with proper alignment, not on the stack.
        match tls_state.conn.open::<_, NoVerify>(context) {
            Err(e) => {
                log::error!("[tls] !! HANDSHAKE FAILED: {:?}", e);
                log::error!("[tls] !! This means the TLS 1.3 negotiation did not complete.");
                log::error!("[tls] !! Possible causes: timeout waiting for ServerHello, cipher mismatch, protocol error");
                // Drop tls_state before closing the TCP socket to ensure the
                // TlsConnection (which holds a SmoltcpSocket with a raw pointer
                // to the stack) is dropped while the stack is still valid.
                drop(tls_state);
                tcp_close(stack, tcp_handle);
                return Err(TlsError::Tls(e));
            }
            Ok(()) => {
                log::info!("[tls] !! HANDSHAKE SUCCESS with {} — TLS 1.3 negotiated!", hostname);
                log::info!("[tls] handshake complete — encrypted channel established");
            }
        };

        Ok(Self {
            tcp_handle,
            hostname: hostname_owned,
            handshake_done: true,
            tls_state: Some(tls_state),
        })
    }

    /// Perform a TLS handshake over an already-connected TCP socket.
    ///
    /// This is the legacy API.  Prefer [`TlsStream::connect()`] which handles
    /// both TCP and TLS connection setup.
    pub fn handshake(
        stack: &mut NetworkStack,
        tcp_handle: SocketHandle,
        hostname: String,
        now: impl Fn() -> Instant,
    ) -> Result<Self, TlsError> {
        log::info!("[tls] starting handshake with {}", hostname);

        // For backward compatibility, get the remote IP from the socket
        // (not strictly needed, but useful for logging).
        // Use the new connect() path with a function pointer.
        // Since this legacy API takes a closure, we cannot easily convert.
        // Instead, we note this as deprecated.
        log::warn!(
            "[tls] handshake() is deprecated — use TlsStream::connect() instead"
        );

        // We cannot implement the full TLS handshake through the legacy API
        // because we need a `fn() -> Instant` pointer (not a closure) for
        // the SmoltcpSocket.  Return an error directing callers to connect().
        Err(TlsError::HandshakeFailed)
    }

    /// Send encrypted data over the TLS connection.
    ///
    /// The plaintext `data` is encrypted by the TLS record layer and sent
    /// over the underlying TCP socket.  Returns the number of bytes written.
    pub fn send(
        &mut self,
        _stack: &mut NetworkStack,
        data: &[u8],
        _now: impl Fn() -> Instant,
    ) -> Result<usize, TlsError> {
        if !self.handshake_done {
            return Err(TlsError::HandshakeFailed);
        }

        let state = self.tls_state.as_mut().ok_or(TlsError::ConnectionClosed)?;

        let mut total = 0;
        while total < data.len() {
            let n = state.conn.write(&data[total..]).map_err(|e| {
                log::error!("[tls] write error: {:?}", e);
                TlsError::Tls(e)
            })?;
            if n == 0 {
                return Err(TlsError::ConnectionClosed);
            }
            total += n;
        }

        // Flush to ensure data is actually sent over the wire.
        state.conn.flush().map_err(|e| {
            log::error!("[tls] flush error: {:?}", e);
            TlsError::Tls(e)
        })?;

        log::debug!("[tls] sent {} bytes to {}", total, self.hostname);
        Ok(total)
    }

    /// Receive and decrypt data from the TLS connection.
    ///
    /// Returns the number of plaintext bytes written to `buf`.
    pub fn recv(
        &mut self,
        _stack: &mut NetworkStack,
        buf: &mut [u8],
        _now: impl Fn() -> Instant,
    ) -> Result<usize, TlsError> {
        if !self.handshake_done {
            return Err(TlsError::HandshakeFailed);
        }

        let state = self.tls_state.as_mut().ok_or(TlsError::ConnectionClosed)?;

        let n = state.conn.read(buf).map_err(|e| {
            log::error!("[tls] read error: {:?}", e);
            TlsError::Tls(e)
        })?;

        log::debug!("[tls] received {} bytes from {}", n, self.hostname);
        Ok(n)
    }

    /// Close the TLS connection gracefully (send close_notify) and tear down
    /// the underlying TCP socket.
    pub fn close(mut self, stack: &mut NetworkStack) {
        if let Some(state) = self.tls_state.take() {
            // Destructure the boxed TlsState to move `conn` out,
            // allowing us to call close() which consumes it.
            let TlsState {
                conn,
                _read_buf,
                _write_buf,
            } = *state;

            // Try to send close_notify.  If it fails, just log and continue.
            match conn.close() {
                Ok(_socket) => {
                    log::debug!("[tls] close_notify sent to {}", self.hostname);
                }
                Err((_socket, e)) => {
                    log::warn!(
                        "[tls] close_notify failed for {}: {:?}",
                        self.hostname,
                        e
                    );
                }
            }
            // _read_buf and _write_buf are dropped here.
        }
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

// ---------------------------------------------------------------------------
// High-level HTTPS request helper
// ---------------------------------------------------------------------------

/// Perform a complete HTTPS request and return the response bytes.
///
/// This is a convenience function that handles the full lifecycle:
/// 1. DNS resolution
/// 2. TCP connect
/// 3. TLS 1.3 handshake
/// 4. Send HTTP request
/// 5. Read HTTP response
/// 6. Close connection
///
/// # Arguments
/// * `stack` — the network stack (must have IP + DNS from DHCP).
/// * `hostname` — server hostname (e.g. `"api.anthropic.com"`).
/// * `port` — server port (usually 443).
/// * `request` — raw HTTP request bytes (e.g. `b"GET / HTTP/1.1\r\nHost: ...\r\n\r\n"`).
/// * `now` — timestamp provider function pointer.
/// * `rng_seed` — seed for the TLS PRNG.
///
/// # Returns
/// The raw HTTP response bytes (headers + body).
pub fn https_request(
    stack: &mut NetworkStack,
    hostname: &str,
    port: u16,
    request: &[u8],
    now: fn() -> Instant,
    rng_seed: u64,
) -> Result<Vec<u8>, TlsError> {
    log::info!("[https] requesting {}:{}", hostname, port);

    // Step 1: DNS resolve.
    let ip = crate::dns::resolve(stack, hostname, || now()).map_err(|e| {
        log::error!("[https] DNS resolution failed for {}: {:?}", hostname, e);
        TlsError::DnsError
    })?;
    log::info!("[https] resolved {} -> {}", hostname, ip);

    // Step 2+3: TCP connect + TLS handshake.
    let mut tls = TlsStream::connect(stack, ip, port, hostname, now, rng_seed)?;

    // Step 4: Send the HTTP request.
    tls.send(stack, request, now)?;
    log::debug!("[https] sent {} byte request", request.len());

    // Step 5: Read the HTTP response.
    // Read in chunks until the connection is closed or we detect end of response.
    let mut response = Vec::new();
    let mut buf = [0u8; 4096];

    loop {
        match tls.recv(stack, &mut buf, now) {
            Ok(0) => {
                // EOF — peer closed the connection.
                log::debug!("[https] peer closed connection, got {} bytes total", response.len());
                break;
            }
            Ok(n) => {
                response.extend_from_slice(&buf[..n]);
                log::debug!("[https] received chunk: {} bytes (total: {})", n, response.len());

                // Check if we've received a complete HTTP response.
                // Look for Content-Length or chunked transfer end.
                if http_response_complete(&response) {
                    log::debug!("[https] response complete ({} bytes)", response.len());
                    break;
                }
            }
            Err(TlsError::Tls(embedded_tls::TlsError::Io(_))) => {
                // I/O error might mean connection closed; return what we have.
                if !response.is_empty() {
                    log::debug!("[https] I/O error after {} bytes, returning partial", response.len());
                    break;
                }
                return Err(TlsError::Io);
            }
            Err(e) => {
                if !response.is_empty() {
                    log::debug!("[https] error after {} bytes, returning partial", response.len());
                    break;
                }
                return Err(e);
            }
        }
    }

    // Step 6: Close.
    tls.close(stack);
    log::info!("[https] complete: {} bytes from {}", response.len(), hostname);

    Ok(response)
}

/// Heuristic check: does `data` contain a complete HTTP response?
///
/// We check for:
/// 1. Headers are complete (contains `\r\n\r\n`)
/// 2. If Content-Length is present, we have that many body bytes
/// 3. If Transfer-Encoding: chunked, we see `0\r\n\r\n` terminator
fn http_response_complete(data: &[u8]) -> bool {
    // Find end of headers.
    let header_end = match find_subsequence(data, b"\r\n\r\n") {
        Some(pos) => pos,
        None => return false, // Headers not complete yet.
    };

    let headers = &data[..header_end];
    let body_start = header_end + 4;
    let body = &data[body_start..];

    // Check for Content-Length.
    if let Some(content_length) = parse_content_length(headers) {
        return body.len() >= content_length;
    }

    // Check for chunked transfer encoding.
    if header_contains(headers, b"transfer-encoding", b"chunked") {
        // Chunked encoding ends with "0\r\n\r\n" (or "0\r\n<trailers>\r\n\r\n").
        return find_subsequence(body, b"0\r\n\r\n").is_some();
    }

    // No Content-Length and not chunked — we can't know when it ends.
    // Return false and rely on connection close.
    false
}

/// Find a byte subsequence in a slice.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Parse the Content-Length header value from raw header bytes.
fn parse_content_length(headers: &[u8]) -> Option<usize> {
    // Simple case-insensitive search for "content-length: <digits>".
    let lower: Vec<u8> = headers.iter().map(|b| b.to_ascii_lowercase()).collect();
    let needle = b"content-length:";

    let pos = find_subsequence(&lower, needle)?;
    let after = &headers[pos + needle.len()..];

    // Skip whitespace.
    let trimmed = after.iter().skip_while(|b| **b == b' ').copied();

    // Parse digits.
    let digits: Vec<u8> = trimmed.take_while(|b| b.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }

    let s = core::str::from_utf8(&digits).ok()?;
    s.parse().ok()
}

/// Case-insensitive check for a header name/value pair.
fn header_contains(headers: &[u8], name: &[u8], value: &[u8]) -> bool {
    let lower: Vec<u8> = headers.iter().map(|b| b.to_ascii_lowercase()).collect();
    let name_lower: Vec<u8> = name.iter().map(|b| b.to_ascii_lowercase()).collect();
    let value_lower: Vec<u8> = value.iter().map(|b| b.to_ascii_lowercase()).collect();

    if let Some(pos) = find_subsequence(&lower, &name_lower) {
        let rest = &lower[pos + name_lower.len()..];
        find_subsequence(rest, &value_lower).is_some()
    } else {
        false
    }
}
