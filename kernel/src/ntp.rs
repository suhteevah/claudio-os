//! NTP time synchronization for ClaudioOS (RFC 5905 simplified).
//!
//! Implements a SNTP client that sends a single NTP request to a configurable
//! server (default: pool.ntp.org) via UDP port 123, parses the response, and
//! adjusts the RTC module's time offset.
//!
//! # Usage
//! - `sync_time(stack, now)` — one-shot NTP sync
//! - `periodic_sync(stack, now)` — re-syncs every 6 hours (call from tick handler)
//! - Shell command `ntpdate` — manual sync, prints offset

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use claudio_net::{Instant, NetworkStack};

// ---------------------------------------------------------------------------
// NTP packet structure (48 bytes)
// ---------------------------------------------------------------------------

/// NTP packet — 48 bytes as per RFC 5905.
///
/// We only need the transmit timestamp from the response, so the struct is
/// kept minimal. All multi-byte fields are big-endian on the wire.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct NtpPacket {
    /// LI (2 bits) | VN (3 bits) | Mode (3 bits)
    li_vn_mode: u8,
    /// Stratum level (0 = unspecified, 1 = primary, 2-15 = secondary)
    stratum: u8,
    /// Maximum interval between successive messages (log2 seconds)
    poll: u8,
    /// Precision of the system clock (log2 seconds)
    precision: i8,
    /// Total round-trip delay to the reference clock (fixed-point 32.32)
    root_delay: u32,
    /// Maximum error due to clock frequency tolerance (fixed-point 32.32)
    root_dispersion: u32,
    /// Reference identifier (4 bytes, depends on stratum)
    ref_id: u32,
    /// Reference timestamp — seconds part (NTP epoch: 1900-01-01)
    ref_ts_secs: u32,
    /// Reference timestamp — fraction part
    ref_ts_frac: u32,
    /// Originate timestamp — seconds
    orig_ts_secs: u32,
    /// Originate timestamp — fraction
    orig_ts_frac: u32,
    /// Receive timestamp — seconds
    recv_ts_secs: u32,
    /// Receive timestamp — fraction
    recv_ts_frac: u32,
    /// Transmit timestamp — seconds (this is what we want from the response)
    tx_ts_secs: u32,
    /// Transmit timestamp — fraction
    tx_ts_frac: u32,
}

const NTP_PACKET_SIZE: usize = 48;

/// Seconds between NTP epoch (1900-01-01) and Unix epoch (1970-01-01).
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// Default NTP server — resolved via DNS.
const DEFAULT_NTP_SERVER: &str = "pool.ntp.org";

/// NTP UDP port.
const NTP_PORT: u16 = 123;

/// Re-sync interval: 6 hours in milliseconds.
const RESYNC_INTERVAL_MS: u64 = 6 * 60 * 60 * 1000;

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

/// NTP-applied offset in seconds (added to RTC wall clock).
/// Positive means RTC was behind, negative means RTC was ahead.
static NTP_OFFSET_SECS: AtomicI64 = AtomicI64::new(0);

/// Last sync timestamp (millis since boot) — used for periodic re-sync.
static LAST_SYNC_MS: AtomicU64 = AtomicU64::new(0);

/// Whether NTP has ever synced successfully.
static NTP_SYNCED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Ephemeral port counter for NTP UDP sockets.
static NTP_LOCAL_PORT: core::sync::atomic::AtomicU16 =
    core::sync::atomic::AtomicU16::new(50123);

fn next_local_port() -> u16 {
    let p = NTP_LOCAL_PORT.fetch_add(1, Ordering::Relaxed);
    if p >= 50999 {
        NTP_LOCAL_PORT.store(50123, Ordering::Relaxed);
    }
    p
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Get the current NTP offset in seconds.
pub fn ntp_offset() -> i64 {
    NTP_OFFSET_SECS.load(Ordering::Relaxed)
}

/// Check if NTP has synced at least once.
pub fn is_synced() -> bool {
    NTP_SYNCED.load(Ordering::Relaxed)
}

/// Apply the NTP offset to the RTC wall clock, returning a corrected Unix timestamp.
pub fn corrected_unix_time() -> i64 {
    let rtc_unix = crate::rtc::wall_clock().to_unix_timestamp();
    rtc_unix + NTP_OFFSET_SECS.load(Ordering::Relaxed)
}

/// One-shot NTP time synchronization.
///
/// Resolves the NTP server via DNS, sends a SNTP request over UDP, and
/// computes the offset between the RTC and the NTP server's time. The
/// offset is stored globally and applied to `corrected_unix_time()`.
///
/// Returns `Ok(offset_seconds)` on success, `Err(description)` on failure.
pub fn sync_time(
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> Result<i64, String> {
    sync_time_with_server(stack, now, DEFAULT_NTP_SERVER)
}

/// Sync time with a specific NTP server hostname.
pub fn sync_time_with_server(
    stack: &mut NetworkStack,
    now: fn() -> Instant,
    server: &str,
) -> Result<i64, String> {
    log::info!("[ntp] resolving {}...", server);

    // Resolve server IP via DNS.
    let server_ip = claudio_net::dns::resolve(stack, server, now)
        .map_err(|e| format!("NTP DNS resolution failed for {}: {:?}", server, e))?;
    log::info!("[ntp] {} = {}", server, server_ip);

    // Build the NTP request packet (client mode 3, version 4).
    let mut req = [0u8; NTP_PACKET_SIZE];
    // LI=0 (no warning), VN=4, Mode=3 (client) => 0b00_100_011 = 0x23
    req[0] = 0x23;

    // We need to send via UDP. smoltcp supports UDP sockets.
    // Use the raw network stack to create a UDP socket, send, and receive.
    let local_port = next_local_port();

    log::info!(
        "[ntp] sending NTP request to {}:{} (local port {})",
        server_ip, NTP_PORT, local_port
    );

    // Send UDP packet and receive response using smoltcp's UDP support.
    let response = udp_exchange(stack, server_ip, NTP_PORT, local_port, &req, now)?;

    if response.len() < NTP_PACKET_SIZE {
        return Err(format!(
            "NTP response too short: {} bytes (need {})",
            response.len(),
            NTP_PACKET_SIZE
        ));
    }

    // Parse the transmit timestamp from the response.
    let tx_secs = u32::from_be_bytes([response[40], response[41], response[42], response[43]]);
    let tx_frac = u32::from_be_bytes([response[44], response[45], response[46], response[47]]);

    if tx_secs == 0 {
        return Err(String::from("NTP server returned zero transmit timestamp"));
    }

    // Parse stratum for informational logging.
    let stratum = response[1];
    log::info!("[ntp] response: stratum={}, tx_secs={}", stratum, tx_secs);

    // Convert NTP timestamp to Unix timestamp.
    let ntp_unix = tx_secs as i64 - NTP_UNIX_OFFSET as i64;
    let _ = tx_frac; // sub-second precision not needed for wall clock

    // Get current RTC-based Unix time.
    let rtc_unix = crate::rtc::wall_clock().to_unix_timestamp();

    // Compute offset: how many seconds RTC is behind NTP.
    let offset = ntp_unix - rtc_unix;

    log::info!(
        "[ntp] NTP time: {} (unix {}), RTC time: unix {}, offset: {}s",
        ntp_unix, ntp_unix, rtc_unix, offset
    );

    // Store the offset globally.
    NTP_OFFSET_SECS.store(offset, Ordering::Relaxed);
    NTP_SYNCED.store(true, Ordering::Relaxed);
    LAST_SYNC_MS.store(now().total_millis() as u64, Ordering::Relaxed);

    log::info!("[ntp] clock synchronized (offset {}s)", offset);
    Ok(offset)
}

/// Check if it's time for a periodic re-sync, and perform it if so.
///
/// Call this from a timer tick or main loop. It will only actually sync
/// if at least `RESYNC_INTERVAL_MS` (6 hours) has passed since the last sync.
///
/// Returns `Some(offset)` if a sync was performed, `None` if skipped.
pub fn periodic_sync(
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> Option<i64> {
    let last = LAST_SYNC_MS.load(Ordering::Relaxed);
    let current = now().total_millis() as u64;

    // If never synced, or 6 hours have passed, sync now.
    if last == 0 || current.saturating_sub(last) >= RESYNC_INTERVAL_MS {
        match sync_time(stack, now) {
            Ok(offset) => Some(offset),
            Err(e) => {
                log::warn!("[ntp] periodic sync failed: {}", e);
                None
            }
        }
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Shell command handler
// ---------------------------------------------------------------------------

/// Handle the `ntpdate` shell command.
///
/// Usage:
/// - `ntpdate` — sync with pool.ntp.org, show offset
/// - `ntpdate <server>` — sync with a specific server
/// - `ntpdate status` — show current NTP status without syncing
pub fn handle_command(
    args: &str,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> String {
    let args = args.trim();

    if args == "status" {
        let synced = is_synced();
        let offset = ntp_offset();
        let corrected = corrected_unix_time();
        if synced {
            format!(
                "NTP status: synchronized\n\
                 Offset:     {}s\n\
                 Corrected:  unix {}\n\
                 RTC:        unix {}\n\
                 Server:     {}\n",
                offset,
                corrected,
                crate::rtc::wall_clock().to_unix_timestamp(),
                DEFAULT_NTP_SERVER,
            )
        } else {
            String::from("NTP status: not synchronized\nRun 'ntpdate' to sync.\n")
        }
    } else {
        let server = if args.is_empty() { DEFAULT_NTP_SERVER } else { args };
        match sync_time_with_server(stack, now, server) {
            Ok(offset) => {
                let rtc_time = crate::rtc::wall_clock().format();
                format!(
                    "NTP sync successful!\n\
                     Server:     {}\n\
                     Offset:     {}s\n\
                     RTC time:   {}\n\
                     Corrected:  unix {}\n",
                    server,
                    offset,
                    rtc_time,
                    corrected_unix_time(),
                )
            }
            Err(e) => {
                format!("NTP sync failed: {}\n", e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// UDP exchange via smoltcp
// ---------------------------------------------------------------------------

/// Send a UDP packet and wait for a response, using smoltcp's UDP socket support.
///
/// This creates a temporary UDP socket, sends the request, polls for a response
/// with a timeout, and returns the response payload.
fn udp_exchange(
    stack: &mut NetworkStack,
    server_ip: claudio_net::Ipv4Address,
    server_port: u16,
    local_port: u16,
    payload: &[u8],
    now: fn() -> Instant,
) -> Result<alloc::vec::Vec<u8>, String> {
    use smoltcp::socket::udp;
    use smoltcp::wire::{IpAddress, IpEndpoint};

    // Create a temporary UDP socket, add to the stack's socket set,
    // send the NTP request, poll for response, then clean up.

    let udp_rx_buf = udp::PacketBuffer::new(
        alloc::vec![udp::PacketMetadata::EMPTY; 4],
        alloc::vec![0u8; 512],
    );
    let udp_tx_buf = udp::PacketBuffer::new(
        alloc::vec![udp::PacketMetadata::EMPTY; 4],
        alloc::vec![0u8; 512],
    );
    let udp_socket = udp::Socket::new(udp_rx_buf, udp_tx_buf);

    let handle = stack.sockets.add(udp_socket);

    // Bind to local port.
    {
        let sock = stack.sockets.get_mut::<udp::Socket>(handle);
        sock.bind(local_port).map_err(|e| format!("UDP bind failed: {:?}", e))?;
    }

    // Send the NTP request.
    let dest = IpEndpoint::new(IpAddress::Ipv4(server_ip), server_port);
    {
        let sock = stack.sockets.get_mut::<udp::Socket>(handle);
        sock.send_slice(payload, dest)
            .map_err(|e| format!("UDP send failed: {:?}", e))?;
    }

    // Poll the interface to actually transmit.
    let _ = stack.iface.poll(now(), &mut stack.device, &mut stack.sockets);

    // Wait for response with timeout (5 seconds).
    let deadline = now().total_millis() + 5000;
    let mut response = alloc::vec::Vec::new();

    loop {
        let _ = stack.iface.poll(now(), &mut stack.device, &mut stack.sockets);

        let sock = stack.sockets.get_mut::<udp::Socket>(handle);
        if let Ok((data, _endpoint)) = sock.recv() {
            response.extend_from_slice(data);
            break;
        }

        if now().total_millis() > deadline {
            // Clean up socket before returning error.
            stack.sockets.remove(handle);
            return Err(String::from("NTP response timeout (5s)"));
        }

        // Brief spin to avoid busy-waiting too aggressively.
        for _ in 0..1000 {
            core::hint::spin_loop();
        }
    }

    // Clean up: close and remove the UDP socket.
    {
        let sock = stack.sockets.get_mut::<udp::Socket>(handle);
        sock.close();
    }
    stack.sockets.remove(handle);

    Ok(response)
}
