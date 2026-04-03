//! SSH server integration -- wires claudio-sshd to smoltcp TCP and dashboard panes.
//!
//! Binds a TCP listener on port 22, accepts incoming connections, drives
//! SSH session state machines, and creates terminal panes for shell requests.
//!
//! ## Architecture
//!
//! The SSH server is initialised by `start_ssh_server()` and polled each
//! iteration of the dashboard event loop via `poll_ssh_server()`. It shares
//! the network stack reference that the dashboard already owns. On each poll:
//!
//! 1. Checks the listener socket for new TCP connections
//! 2. Reads data from active connection sockets and feeds it to `SshSession`
//! 3. Sends outgoing SSH packets back over TCP
//! 4. Dispatches `ChannelAction`s (shell requests, channel data, etc.)

extern crate alloc;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use claudio_net::NetworkStack;
use claudio_sshd::channel::ChannelAction;
use claudio_sshd::session::SessionState;
use claudio_sshd::{PaneCallback, SshConfig, SshServer, SshSession};
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;
use smoltcp::time::Instant;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// SSH listen port.
const SSH_PORT: u16 = 22;

/// Maximum simultaneous SSH sessions.
const MAX_SSH_SESSIONS: usize = 4;

/// TCP RX/TX buffer size per connection.
const TCP_BUF_SIZE: usize = 16384;

/// Maximum data read from a TCP socket per poll cycle.
const READ_BUF_SIZE: usize = 4096;

// ---------------------------------------------------------------------------
// PaneCallback stub -- creates panes in the dashboard
// ---------------------------------------------------------------------------

/// Callback implementation that logs shell/exec requests.
///
/// In the current architecture the dashboard owns the layout and pane state,
/// so we cannot directly create panes from the SSH polling path. Instead we
/// record requested actions and provide a basic echo shell. Full pane
/// integration (routing SSH channel I/O to dashboard terminal panes) can
/// be added later via a shared action queue.
struct SshPaneCallback {
    /// Channel-to-pane mapping (channel_id -> pane_id).
    channel_panes: Vec<(u32, u32)>,
    /// Next synthetic pane ID.
    next_pane_id: u32,
}

impl SshPaneCallback {
    fn new() -> Self {
        Self {
            channel_panes: Vec::new(),
            next_pane_id: 1000, // Start high to avoid colliding with dashboard pane IDs
        }
    }
}

impl PaneCallback for SshPaneCallback {
    fn on_shell_request(
        &mut self,
        channel_id: u32,
        term: &str,
        width: u32,
        height: u32,
    ) -> Option<u32> {
        let pane_id = self.next_pane_id;
        self.next_pane_id += 1;
        self.channel_panes.push((channel_id, pane_id));
        log::info!(
            "[sshd] shell requested on channel {} (term={}, {}x{}) -> pane {}",
            channel_id, term, width, height, pane_id,
        );
        Some(pane_id)
    }

    fn on_exec_request(&mut self, channel_id: u32, command: &str) -> Option<u32> {
        let pane_id = self.next_pane_id;
        self.next_pane_id += 1;
        self.channel_panes.push((channel_id, pane_id));
        log::info!(
            "[sshd] exec requested on channel {}: '{}' -> pane {}",
            channel_id, command, pane_id,
        );
        Some(pane_id)
    }

    fn on_channel_data(&mut self, channel_id: u32, data: &[u8]) {
        log::trace!("[sshd] channel {} data: {} bytes", channel_id, data.len());
    }

    fn on_window_change(&mut self, channel_id: u32, width: u32, height: u32) {
        log::debug!("[sshd] channel {} window change: {}x{}", channel_id, width, height);
    }

    fn on_channel_close(&mut self, channel_id: u32) {
        log::info!("[sshd] channel {} closed", channel_id);
        self.channel_panes.retain(|(ch, _)| *ch != channel_id);
    }
}

// ---------------------------------------------------------------------------
// Active SSH connection
// ---------------------------------------------------------------------------

/// Tracks one active SSH connection: the TCP socket handle and the SSH session.
struct SshConnection {
    /// smoltcp TCP socket handle for this connection.
    tcp_handle: SocketHandle,
    /// SSH protocol session state machine.
    session: SshSession,
    /// Whether we've sent the server version string.
    version_sent: bool,
    /// Buffer for accumulating partial version string from client.
    version_buf: Vec<u8>,
    /// Pane callback for this connection.
    callback: SshPaneCallback,
}

// ---------------------------------------------------------------------------
// Free functions for SshConnection processing
// ---------------------------------------------------------------------------

/// Process SSH binary packet data for a connection.
fn process_session_data(conn: &mut SshConnection, data: &[u8]) {
    match conn.session.on_data_received(data) {
        Ok(actions) => {
            for action in actions {
                handle_action(conn, action);
            }
        }
        Err(e) => {
            log::error!("[sshd] session error: {}", e);
            conn.session.disconnect(2, "protocol error");
        }
    }
}

/// Handle a channel action from the SSH session.
fn handle_action(conn: &mut SshConnection, action: ChannelAction) {
    match action {
        ChannelAction::None => {}
        ChannelAction::StartShell { channel_id } => {
            // Create a pane via callback.
            let _pane_id = conn.callback.on_shell_request(
                channel_id,
                "xterm-256color",
                80,
                24,
            );

            // Send a welcome message on the channel (ASCII only).
            let welcome = b"\x1b[96mClaudioOS SSH Shell\x1b[0m\r\n\
                            \x1b[90m----------------------------------------\x1b[0m\r\n\
                            \r\nWelcome to ClaudioOS. Type 'help' for commands.\r\n\r\n\
                            claudio$ ";
            if let Err(e) = conn.session.send_channel_data(channel_id, welcome) {
                log::error!("[sshd] failed to send welcome: {}", e);
            }
        }
        ChannelAction::ExecCommand { channel_id, command } => {
            // In the Interactive state, ExecCommand is used for channel data
            // (the session repurposes this action type for data routing).
            log::trace!("[sshd] channel {} data/exec: '{}'", channel_id, command.escape_default());

            // Notify callback.
            conn.callback.on_channel_data(channel_id, command.as_bytes());

            // Echo back for now (basic interactive shell behavior).
            // In full integration, this would go to the pane's input buffer.
            let echo = if command.contains('\r') || command.contains('\n') {
                // Enter pressed -- echo newline and prompt.
                format!("\r\nclaudio$ ")
            } else {
                // Echo typed characters.
                command.clone()
            };

            if let Err(e) = conn.session.send_channel_data(channel_id, echo.as_bytes()) {
                log::error!("[sshd] failed to echo data: {}", e);
            }
        }
        ChannelAction::WindowChange { channel_id, width, height } => {
            conn.callback.on_window_change(channel_id, width, height);
        }
    }
}

// ---------------------------------------------------------------------------
// SshListener -- manages the listen socket and active connections
// ---------------------------------------------------------------------------

/// SSH listener that binds to port 22 on the smoltcp TCP stack.
///
/// Call `poll()` regularly to accept connections and drive sessions.
pub struct SshListener {
    /// The SSH server (host keys, auth config, session factory).
    server: SshServer,
    /// smoltcp socket handle for the listener.
    listen_handle: SocketHandle,
    /// Active SSH connections.
    connections: Vec<SshConnection>,
    /// Time source function.
    now: fn() -> Instant,
}

impl SshListener {
    /// Create a new SSH listener bound to port 22.
    ///
    /// Adds a TCP socket in listen mode to the network stack's socket set.
    pub fn new(stack: &mut NetworkStack, now: fn() -> Instant) -> Self {
        // Create the SSH server with host keys
        let config = SshConfig {
            port: SSH_PORT,
            max_connections: MAX_SSH_SESSIONS,
            ..SshConfig::default()
        };

        // Use PIT ticks as entropy source for host key generation
        let server = SshServer::new(config, rng_fill);

        // Create listener TCP socket
        let listen_handle = create_listen_socket(stack, SSH_PORT);

        log::info!(
            "[sshd] SSH listener bound to port {} (max {} sessions)",
            SSH_PORT, MAX_SSH_SESSIONS,
        );

        Self {
            server,
            listen_handle,
            connections: Vec::new(),
            now,
        }
    }

    /// Poll the listener and all active connections.
    ///
    /// This should be called from the main async loop on each iteration.
    /// It is non-blocking -- returns immediately if there is no work.
    pub fn poll(&mut self, stack: &mut NetworkStack) {
        let timestamp = (self.now)();

        // First, poll the network stack to process incoming frames.
        stack.poll(timestamp);

        // Check if the listener has an incoming connection.
        self.check_accept(stack);

        // Drive all active connections.
        self.drive_connections(stack);

        // Clean up dead connections.
        self.cleanup_dead(stack);

        // Re-listen if the listener socket closed (smoltcp closes it after accept).
        self.ensure_listening(stack);
    }

    /// Check the listener socket for a new incoming connection.
    fn check_accept(&mut self, stack: &mut NetworkStack) {
        let socket = stack.sockets.get_mut::<tcp::Socket>(self.listen_handle);

        if socket.is_active() && socket.may_recv() {
            // A client has connected to the listen socket.
            // In smoltcp, the listen socket itself becomes the connection socket.
            // We need to "steal" this handle and create a new listener.
            log::info!("[sshd] incoming TCP connection on port {}", SSH_PORT);

            if self.connections.len() >= MAX_SSH_SESSIONS {
                log::warn!("[sshd] at capacity -- rejecting connection");
                socket.close();
                return;
            }

            // Accept an SSH session from the server.
            match self.server.accept_connection() {
                None => {
                    log::warn!("[sshd] server rejected connection (at capacity)");
                    socket.close();
                }
                Some(session) => {
                    // Move the listen handle to the connection; we'll create a new listener.
                    let conn_handle = self.listen_handle;

                    // Create a new listen socket for the next connection.
                    self.listen_handle = create_listen_socket(stack, SSH_PORT);

                    let conn = SshConnection {
                        tcp_handle: conn_handle,
                        session,
                        version_sent: false,
                        version_buf: Vec::new(),
                        callback: SshPaneCallback::new(),
                    };

                    self.connections.push(conn);
                    log::info!(
                        "[sshd] SSH session started ({} active)",
                        self.connections.len(),
                    );
                }
            }
        }
    }

    /// Drive all active SSH connections: read TCP data, feed to session,
    /// send outgoing packets.
    fn drive_connections(&mut self, stack: &mut NetworkStack) {
        for conn in self.connections.iter_mut() {
            // Skip disconnected sessions.
            if !conn.session.is_alive() {
                continue;
            }

            // Step 1: Send version string if we haven't yet.
            if !conn.version_sent {
                let version_bytes = conn.session.version_bytes();
                let socket = stack.sockets.get_mut::<tcp::Socket>(conn.tcp_handle);
                if socket.can_send() {
                    match socket.send_slice(&version_bytes) {
                        Ok(n) if n == version_bytes.len() => {
                            conn.version_sent = true;
                            log::info!("[sshd] sent server version string ({} bytes)", n);
                        }
                        Ok(n) => {
                            log::warn!(
                                "[sshd] partial version send: {}/{}",
                                n, version_bytes.len()
                            );
                        }
                        Err(e) => {
                            log::error!("[sshd] failed to send version: {:?}", e);
                        }
                    }
                }
                continue; // Wait for version exchange before processing data
            }

            // Step 2: Read incoming TCP data.
            let mut read_buf = [0u8; READ_BUF_SIZE];
            let bytes_read = {
                let socket = stack.sockets.get_mut::<tcp::Socket>(conn.tcp_handle);
                if !socket.may_recv() {
                    if socket.state() == tcp::State::CloseWait
                        || socket.state() == tcp::State::Closed
                    {
                        log::info!("[sshd] TCP connection closed by peer");
                        conn.session.disconnect(11, "connection closed by peer");
                    }
                    continue;
                }
                match socket.recv_slice(&mut read_buf) {
                    Ok(n) => n,
                    Err(_) => continue,
                }
            };

            if bytes_read == 0 {
                continue;
            }

            log::trace!("[sshd] received {} TCP bytes", bytes_read);

            // Step 3: Feed data to the SSH session.
            let data = &read_buf[..bytes_read];

            if *conn.session.state() == SessionState::VersionExchange {
                // Accumulate version string (ends with \r\n).
                conn.version_buf.extend_from_slice(data);

                if let Some(pos) = conn.version_buf.windows(2).position(|w| w == b"\r\n") {
                    let version_data = conn.version_buf[..pos + 2].to_vec();
                    conn.version_buf.drain(..pos + 2);

                    match conn.session.on_version_received(&version_data) {
                        Ok(()) => {
                            log::info!("[sshd] version exchange complete");
                        }
                        Err(e) => {
                            log::error!("[sshd] version exchange failed: {}", e);
                            conn.session.disconnect(2, "version exchange failed");
                        }
                    }

                    // Process any remaining data after the version string.
                    if !conn.version_buf.is_empty() {
                        let remaining = core::mem::take(&mut conn.version_buf);
                        process_session_data(conn, &remaining);
                    }
                }
            } else {
                // Normal packet processing.
                process_session_data(conn, data);
            }

            // Step 4: Send outgoing packets from the session.
            let outgoing = conn.session.drain_outgoing();
            if !outgoing.is_empty() {
                let socket = stack.sockets.get_mut::<tcp::Socket>(conn.tcp_handle);
                for packet in &outgoing {
                    match socket.send_slice(packet) {
                        Ok(n) => {
                            log::trace!("[sshd] sent {} byte SSH packet", n);
                            if n < packet.len() {
                                log::warn!(
                                    "[sshd] partial packet send: {}/{}",
                                    n, packet.len()
                                );
                            }
                        }
                        Err(e) => {
                            log::error!("[sshd] TCP send error: {:?}", e);
                        }
                    }
                }
            }
        }
    }

    /// Clean up dead (disconnected) connections.
    fn cleanup_dead(&mut self, stack: &mut NetworkStack) {
        let before = self.connections.len();
        self.connections.retain(|conn| {
            if conn.session.is_alive() {
                true
            } else {
                // Close the TCP socket and notify the server.
                let socket = stack.sockets.get_mut::<tcp::Socket>(conn.tcp_handle);
                socket.close();
                log::info!("[sshd] cleaning up dead session");
                false
            }
        });
        let removed = before - self.connections.len();
        if removed > 0 {
            for _ in 0..removed {
                self.server.session_ended();
            }
            log::info!(
                "[sshd] removed {} dead sessions ({} active)",
                removed, self.connections.len(),
            );
        }
    }

    /// Ensure the listener socket is in the Listen state.
    fn ensure_listening(&mut self, stack: &mut NetworkStack) {
        let socket = stack.sockets.get_mut::<tcp::Socket>(self.listen_handle);
        if !socket.is_listening() && !socket.is_active() {
            log::debug!("[sshd] re-listening on port {}", SSH_PORT);
            if let Err(e) = socket.listen(SSH_PORT) {
                log::error!("[sshd] failed to re-listen: {:?}", e);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Create a TCP socket in listen mode on the given port.
fn create_listen_socket(stack: &mut NetworkStack, port: u16) -> SocketHandle {
    let rx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_BUF_SIZE]);
    let tx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_BUF_SIZE]);
    let mut socket = tcp::Socket::new(rx_buf, tx_buf);
    socket.set_nagle_enabled(false);

    if let Err(e) = socket.listen(port) {
        log::error!("[sshd] failed to listen on port {}: {:?}", port, e);
    } else {
        log::info!("[sshd] TCP listening on port {}", port);
    }

    stack.sockets.add(socket)
}

/// Cryptographically secure RNG fill function for SSH key generation
/// and session key material.
///
/// Delegates to the kernel's CSPRNG module which uses RDRAND (if available)
/// or a ChaCha20-based PRNG seeded from hardware entropy (TSC + PIT + RTC).
fn rng_fill(buf: &mut [u8]) {
    crate::csprng::random_bytes(buf);
}

// ---------------------------------------------------------------------------
// Integration: start_ssh_server -- called from main.rs
// ---------------------------------------------------------------------------

/// Wrapper to make the raw pointer Send-safe.
/// Safety: SshListener is only accessed from the single-threaded executor
/// and the dashboard event loop -- never concurrently from multiple threads.
struct SendPtr(*mut SshListener);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

/// Global SSH listener pointer. Set by `start_ssh_server`, polled by
/// `poll_ssh_server`. This follows the same raw-pointer pattern used
/// by `agent_loop::init_compile_handler`.
static SSH_LISTENER: spin::Mutex<Option<SendPtr>> = spin::Mutex::new(None);

/// Initialize and start the SSH server.
///
/// Creates an `SshListener` bound to port 22 on the provided network stack.
/// The listener is heap-allocated and leaked so it lives for the kernel's
/// lifetime. Call `poll_ssh_server()` from the dashboard loop to drive it.
pub fn start_ssh_server(
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) {
    log::info!("[sshd] ============================================");
    log::info!("[sshd]   ClaudioOS SSH Server -- Port {}", SSH_PORT);
    log::info!("[sshd] ============================================");

    let listener = SshListener::new(stack, now);

    // Leak the listener onto the heap so it lives forever.
    let listener_ptr = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(listener));

    let mut guard = SSH_LISTENER.lock();
    *guard = Some(SendPtr(listener_ptr));

    log::info!("[sshd] SSH server started -- listening on port {}", SSH_PORT);
    log::info!("[sshd] Connect with: ssh -p 22 <guest-ip>");
}

/// Poll the SSH server. Call this from the dashboard event loop.
///
/// Non-blocking: returns immediately if there is no SSH work to do.
pub fn poll_ssh_server(stack: &mut NetworkStack) {
    let guard = SSH_LISTENER.lock();
    if let Some(ref send_ptr) = *guard {
        let ptr = send_ptr.0;
        drop(guard); // Release lock before polling
        let listener = unsafe { &mut *ptr };
        listener.poll(stack);
    }
}
