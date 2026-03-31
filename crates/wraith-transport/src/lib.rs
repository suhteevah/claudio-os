//! Wraith-browser HTTP transport over ClaudioOS's smoltcp TCP + TLS stack.
//!
//! This crate bridges wraith-browser's [`HttpTransport`] trait into ClaudioOS's
//! bare-metal network stack.  It converts between wraith's
//! `TransportRequest`/`TransportResponse` types and ClaudioOS's
//! `HttpRequest`/`HttpResponse`, using smoltcp TCP sockets for HTTP and
//! `TlsStream` for HTTPS.
//!
//! # Design
//!
//! ClaudioOS has no tokio, no threads — everything is synchronous or
//! cooperative async via a custom executor.  The wraith `HttpTransport` trait
//! uses `async fn`, but on bare metal the returned future is polled by that
//! custom executor.  Our implementation is actually synchronous internally
//! (busy-polling the smoltcp stack), wrapped in an `async fn` signature.
//!
//! The transport holds a raw pointer to the [`NetworkStack`] because:
//! - ClaudioOS is single-threaded bare-metal, so there is no data race.
//! - The TCP/TLS helpers require `&mut NetworkStack`, and the pointer lets us
//!   obtain that mutable reference when `execute()` is called.
//! - The pointer must remain valid for the lifetime of the transport.

#![no_std]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use claudio_net::http::{decode_chunked, HttpRequest, HttpResponse};
use claudio_net::stack::NetworkStack;
use claudio_net::tls::{
    tcp_close, tcp_connect, tcp_recv, tcp_send, TcpError, TlsError, TlsStream,
};
use smoltcp::time::Instant;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from the smoltcp-based transport.
#[derive(Debug)]
pub enum SmoltcpTransportError {
    /// Failed to parse the request URL.
    InvalidUrl(String),
    /// DNS resolution failed.
    DnsError(String),
    /// TCP connection failed.
    TcpError(TcpError),
    /// TLS error (handshake, send, recv).
    TlsError(TlsError),
    /// HTTP response parsing failed.
    HttpError(String),
    /// The network stack does not have an IP address yet (DHCP incomplete).
    NoNetwork,
    /// Request timed out.
    Timeout,
}

impl core::fmt::Display for SmoltcpTransportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidUrl(msg) => write!(f, "invalid URL: {}", msg),
            Self::DnsError(msg) => write!(f, "DNS error: {}", msg),
            Self::TcpError(e) => write!(f, "TCP error: {:?}", e),
            Self::TlsError(e) => write!(f, "TLS error: {:?}", e),
            Self::HttpError(msg) => write!(f, "HTTP error: {}", msg),
            Self::NoNetwork => write!(f, "network stack not ready (no IP)"),
            Self::Timeout => write!(f, "request timed out"),
        }
    }
}

// ---------------------------------------------------------------------------
// URL parsing (minimal, no_std)
// ---------------------------------------------------------------------------

/// Parsed URL components.
struct ParsedUrl {
    /// `true` for HTTPS, `false` for HTTP.
    is_https: bool,
    /// Hostname (e.g. `"example.com"`).
    host: String,
    /// Port number (defaults to 443 for HTTPS, 80 for HTTP).
    port: u16,
    /// Path + query string (e.g. `"/api/v1?foo=bar"`).  Defaults to `"/"`.
    path: String,
}

fn parse_url(url: &str) -> Result<ParsedUrl, SmoltcpTransportError> {
    let (is_https, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(SmoltcpTransportError::InvalidUrl(format!(
            "unsupported scheme in: {}",
            url
        )));
    };

    // Split host from path at the first '/'.
    let (host_port, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };

    // Split host from port at ':'.
    let (host, port) = match host_port.rfind(':') {
        Some(idx) => {
            let port_str = &host_port[idx + 1..];
            let port: u16 = port_str.parse().map_err(|_| {
                SmoltcpTransportError::InvalidUrl(format!("bad port: {}", port_str))
            })?;
            (&host_port[..idx], port)
        }
        None => {
            let default_port = if is_https { 443 } else { 80 };
            (host_port, default_port)
        }
    };

    if host.is_empty() {
        return Err(SmoltcpTransportError::InvalidUrl(
            "empty hostname".into(),
        ));
    }

    Ok(ParsedUrl {
        is_https,
        host: String::from(host),
        port,
        path: String::from(path),
    })
}

// ---------------------------------------------------------------------------
// Ephemeral local port allocator (mirrors claudio-net's approach)
// ---------------------------------------------------------------------------

/// Simple local port counter.  Starts at 50000 to avoid colliding with
/// the net crate's counter that starts at 49152.
static mut LOCAL_PORT_COUNTER: u16 = 50000;

fn next_local_port() -> u16 {
    unsafe {
        let port = LOCAL_PORT_COUNTER;
        LOCAL_PORT_COUNTER = if LOCAL_PORT_COUNTER >= 65534 {
            50000
        } else {
            LOCAL_PORT_COUNTER + 1
        };
        port
    }
}

// ---------------------------------------------------------------------------
// SmoltcpTransport
// ---------------------------------------------------------------------------

/// HTTP transport implementation over ClaudioOS's smoltcp network stack.
///
/// This struct holds a raw pointer to the [`NetworkStack`] and a function
/// pointer for obtaining timestamps.  It is intended to be created once
/// during kernel initialization and shared (via the wraith engine) for the
/// lifetime of the system.
///
/// # Safety
///
/// The caller must ensure:
/// - The `NetworkStack` pointed to by the internal pointer remains valid and
///   is not concurrently accessed (guaranteed on single-threaded bare-metal).
/// - The `now` function pointer returns valid `smoltcp::time::Instant` values.
pub struct SmoltcpTransport {
    /// Raw pointer to the network stack.  We use a raw pointer because:
    /// - The transport must be `Send + Sync` (required by `HttpTransport`).
    /// - ClaudioOS is single-threaded, so no data races are possible.
    /// - The TCP/TLS helpers need `&mut NetworkStack`.
    stack: *mut NetworkStack,
    /// Timestamp provider (reads from PIT timer or similar).
    now: fn() -> Instant,
    /// Seed base for TLS PRNG.  Incremented per request to avoid reuse.
    rng_seed_base: u64,
}

// SAFETY: ClaudioOS is single-threaded bare-metal.  There are no other
// threads that could race on the NetworkStack pointer.
unsafe impl Send for SmoltcpTransport {}
unsafe impl Sync for SmoltcpTransport {}

impl SmoltcpTransport {
    /// Create a new transport.
    ///
    /// # Safety
    ///
    /// - `stack` must point to a valid `NetworkStack` for the lifetime of
    ///   this transport.
    /// - No other code may hold a mutable reference to the stack while
    ///   `execute()` is running (bare-metal single-threaded guarantees this).
    pub unsafe fn new(
        stack: *mut NetworkStack,
        now: fn() -> Instant,
        rng_seed: u64,
    ) -> Self {
        Self {
            stack,
            now,
            rng_seed_base: rng_seed,
        }
    }

    /// Get a mutable reference to the network stack.
    fn stack_mut(&self) -> &mut NetworkStack {
        unsafe { &mut *self.stack }
    }

    /// Execute an HTTP or HTTPS request synchronously.
    ///
    /// This is the core implementation called by the async `execute()` wrapper.
    fn execute_sync(
        &self,
        method: &str,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: Option<&[u8]>,
    ) -> Result<SmoltcpResponse, SmoltcpTransportError> {
        let parsed = parse_url(url)?;
        let stack = self.stack_mut();

        if !stack.has_ip {
            return Err(SmoltcpTransportError::NoNetwork);
        }

        // Step 1: DNS resolution.
        let now_fn = self.now;
        let remote_ip =
            claudio_net::dns::resolve(stack, &parsed.host, || now_fn()).map_err(|e| {
                SmoltcpTransportError::DnsError(format!("{:?}", e))
            })?;

        log::info!(
            "[wraith-transport] {} {}:{}{} (resolved: {})",
            method,
            parsed.host,
            parsed.port,
            parsed.path,
            remote_ip
        );

        // Step 2: Build the HTTP/1.1 request bytes.
        let http_req = if let Some(body_bytes) = body {
            HttpRequest::post(&parsed.host, &parsed.path, body_bytes.to_vec())
        } else {
            HttpRequest::get(&parsed.host, &parsed.path)
        };

        // Add headers from the wraith request.
        let mut http_req = http_req;
        for (name, value) in headers {
            // Skip Host header — HttpRequest adds it automatically.
            if name.eq_ignore_ascii_case("host") {
                continue;
            }
            http_req = http_req.header(name.clone(), value.clone());
        }

        // Add Connection: close so the server closes the connection after
        // sending the response, giving us a clean EOF signal.
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("connection")) {
            http_req = http_req.header("Connection", "close");
        }

        // Override method for GET requests (HttpRequest::post sets "POST").
        // We constructed with get() or post() above, but we should verify
        // the method matches.  HttpRequest doesn't have a set_method, so we
        // handle this by choosing the right constructor.  If the wraith
        // request uses GET but has a body, we still use GET (rare but valid).
        let request_bytes = if method == "GET" && body.is_some() {
            // Edge case: GET with body — build manually.
            let mut req = HttpRequest::get(&parsed.host, &parsed.path);
            req.body = body.map(|b| b.to_vec());
            for (name, value) in headers {
                if name.eq_ignore_ascii_case("host") {
                    continue;
                }
                req = req.header(name.clone(), value.clone());
            }
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("connection")) {
                req = req.header("Connection", "close");
            }
            req.to_bytes()
        } else {
            http_req.to_bytes()
        };

        // Step 3: Connect and send/receive based on HTTP vs HTTPS.
        let response_bytes = if parsed.is_https {
            self.execute_https(stack, remote_ip, parsed.port, &parsed.host, &request_bytes)?
        } else {
            self.execute_http(stack, remote_ip, parsed.port, &request_bytes)?
        };

        // Step 4: Parse the HTTP response.
        self.parse_response(&response_bytes, url)
    }

    /// Perform an HTTPS request via TLS.
    fn execute_https(
        &self,
        stack: &mut NetworkStack,
        remote_ip: smoltcp::wire::Ipv4Address,
        port: u16,
        hostname: &str,
        request_bytes: &[u8],
    ) -> Result<Vec<u8>, SmoltcpTransportError> {
        let now_fn = self.now;

        // Use a unique RNG seed per request to avoid nonce reuse.
        let rng_seed = self.rng_seed_base.wrapping_add((now_fn)().total_millis() as u64);

        // TCP connect + TLS handshake.
        let mut tls = TlsStream::connect(stack, remote_ip, port, hostname, now_fn, rng_seed)
            .map_err(SmoltcpTransportError::TlsError)?;

        // Send the HTTP request over TLS.
        tls.send(stack, request_bytes, now_fn)
            .map_err(SmoltcpTransportError::TlsError)?;

        log::debug!(
            "[wraith-transport] sent {} byte HTTPS request",
            request_bytes.len()
        );

        // Receive the response.
        let response = self.read_tls_response(&mut tls, stack)?;

        // Close the TLS connection.
        tls.close(stack);

        Ok(response)
    }

    /// Perform an HTTP (plaintext) request via raw TCP.
    fn execute_http(
        &self,
        stack: &mut NetworkStack,
        remote_ip: smoltcp::wire::Ipv4Address,
        port: u16,
        request_bytes: &[u8],
    ) -> Result<Vec<u8>, SmoltcpTransportError> {
        let now_fn = self.now;
        let local_port = next_local_port();

        // TCP connect.
        let handle = tcp_connect(stack, remote_ip, port, local_port, now_fn)
            .map_err(SmoltcpTransportError::TcpError)?;

        // Send the HTTP request.
        tcp_send(stack, handle, request_bytes, now_fn)
            .map_err(SmoltcpTransportError::TcpError)?;

        log::debug!(
            "[wraith-transport] sent {} byte HTTP request",
            request_bytes.len()
        );

        // Receive the response.
        let mut response = Vec::new();
        let mut buf = [0u8; 4096];

        loop {
            match tcp_recv(stack, handle, &mut buf, now_fn) {
                Ok(0) => {
                    // EOF — peer closed.
                    break;
                }
                Ok(n) => {
                    response.extend_from_slice(&buf[..n]);

                    // Check if we have a complete response.
                    if self.response_complete(&response) {
                        break;
                    }
                }
                Err(TcpError::Timeout) => {
                    // If we already have data, treat timeout as end of response.
                    if !response.is_empty() {
                        log::debug!(
                            "[wraith-transport] TCP recv timeout after {} bytes",
                            response.len()
                        );
                        break;
                    }
                    tcp_close(stack, handle);
                    return Err(SmoltcpTransportError::Timeout);
                }
                Err(e) => {
                    if !response.is_empty() {
                        break;
                    }
                    tcp_close(stack, handle);
                    return Err(SmoltcpTransportError::TcpError(e));
                }
            }
        }

        tcp_close(stack, handle);
        Ok(response)
    }

    /// Read a full HTTP response from a TLS stream.
    fn read_tls_response(
        &self,
        tls: &mut TlsStream,
        stack: &mut NetworkStack,
    ) -> Result<Vec<u8>, SmoltcpTransportError> {
        let now_fn = self.now;
        let mut response = Vec::new();
        let mut buf = [0u8; 4096];

        loop {
            match tls.recv(stack, &mut buf, now_fn) {
                Ok(0) => {
                    // EOF.
                    break;
                }
                Ok(n) => {
                    response.extend_from_slice(&buf[..n]);

                    if self.response_complete(&response) {
                        break;
                    }
                }
                Err(e) => {
                    // On any error, if we already have partial data, return
                    // what we have (the connection may have been closed by
                    // the server after sending the response).
                    if !response.is_empty() {
                        log::debug!(
                            "[wraith-transport] TLS recv error after {} bytes, using partial response",
                            response.len()
                        );
                        break;
                    }
                    return Err(SmoltcpTransportError::TlsError(e));
                }
            }
        }

        Ok(response)
    }

    /// Check whether the raw response bytes contain a complete HTTP response.
    fn response_complete(&self, data: &[u8]) -> bool {
        // Find end of headers.
        let header_end = match find_subsequence(data, b"\r\n\r\n") {
            Some(pos) => pos,
            None => return false,
        };

        let headers = &data[..header_end];
        let body_start = header_end + 4;
        let body = &data[body_start..];

        // Check Content-Length.
        if let Some(cl) = parse_content_length_from_raw(headers) {
            return body.len() >= cl;
        }

        // Check chunked transfer encoding.
        if header_contains_value(headers, b"transfer-encoding", b"chunked") {
            return find_subsequence(body, b"0\r\n\r\n").is_some();
        }

        // Unknown framing — rely on connection close.
        false
    }

    /// Parse raw HTTP response bytes into our response type.
    fn parse_response(
        &self,
        raw: &[u8],
        original_url: &str,
    ) -> Result<SmoltcpResponse, SmoltcpTransportError> {
        let http_resp = HttpResponse::parse(raw).map_err(|e| {
            SmoltcpTransportError::HttpError(format!("parse error: {:?}", e))
        })?;

        // Check for chunked transfer encoding and decode if needed.
        let body = if http_resp.is_chunked() {
            // The body in http_resp is the raw chunked data.
            decode_chunked(&http_resp.body).unwrap_or_else(|_| http_resp.body.clone())
        } else {
            http_resp.body
        };

        // Convert headers from Vec<(String, String)> to BTreeMap.
        let mut headers = BTreeMap::new();
        for (name, value) in &http_resp.headers {
            headers.insert(name.clone(), value.clone());
        }

        Ok(SmoltcpResponse {
            status: http_resp.status,
            headers,
            body,
            url: String::from(original_url),
        })
    }
}

// ---------------------------------------------------------------------------
// Response type (mirrors wraith's TransportResponse)
// ---------------------------------------------------------------------------

/// HTTP response from the smoltcp transport.
///
/// This mirrors wraith-browser's `TransportResponse` structure so that
/// callers can easily convert between them.
pub struct SmoltcpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: BTreeMap<String, String>,
    /// Decoded response body.
    pub body: Vec<u8>,
    /// The request URL (no redirect following in this implementation).
    pub url: String,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Find a byte subsequence in a slice.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Parse Content-Length value from raw header bytes.
fn parse_content_length_from_raw(headers: &[u8]) -> Option<usize> {
    let lower: Vec<u8> = headers.iter().map(|b| b.to_ascii_lowercase()).collect();
    let needle = b"content-length:";
    let pos = find_subsequence(&lower, needle)?;
    let after = &headers[pos + needle.len()..];
    let trimmed = after.iter().skip_while(|b| **b == b' ').copied();
    let digits: Vec<u8> = trimmed.take_while(|b| b.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    core::str::from_utf8(&digits).ok()?.parse().ok()
}

/// Case-insensitive check for a header name containing a value.
fn header_contains_value(headers: &[u8], name: &[u8], value: &[u8]) -> bool {
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
