//! Session token refresh manager for claude.ai sessions.
//!
//! Monitors session cookie expiry and attempts automatic refresh before
//! the session expires. During long uptime, this prevents the claude.ai
//! session from going stale and requiring a full re-auth (magic link flow).
//!
//! ## Design
//!
//! - `SessionManager` is stored globally behind a `spin::Mutex`.
//! - The dashboard event loop calls `periodic_check()` roughly every hour
//!   (every ~65,536 PIT ticks at 18.2 Hz ≈ ~60 minutes).
//! - As expiry approaches, warnings are logged at 24h, 2h, and 30min thresholds.
//! - Refresh attempts use the existing session cookie to call `/api/auth/session`
//!   on claude.ai. If the session is still valid, `expires_at` is extended.
//! - If refresh fails, a full re-auth is flagged (the user sees a serial prompt).
//! - Updated session cookies are emitted via the `SAVE_SESSION:` serial marker
//!   so the host-side script can capture them for persistence across reboots.

extern crate alloc;

use alloc::string::String;
use spin::Mutex;

/// How often the dashboard loop should call `periodic_check()`, in PIT ticks.
/// ~65,536 ticks at 18.2 Hz ≈ 60 minutes.
pub const CHECK_INTERVAL_TICKS: u64 = 65_536;

/// Default session lifetime: 7 days (conservative estimate for claude.ai cookies).
const DEFAULT_SESSION_LIFETIME_SECS: i64 = 7 * 24 * 3600;

/// Refresh threshold: attempt refresh when less than 24 hours remain.
const REFRESH_THRESHOLD_SECS: i64 = 24 * 3600;

/// Warning thresholds (in seconds before expiry).
const WARN_24H_SECS: i64 = 24 * 3600;
const WARN_2H_SECS: i64 = 2 * 3600;
const WARN_30M_SECS: i64 = 30 * 60;

/// Global session manager instance.
static SESSION: Mutex<Option<SessionManager>> = Mutex::new(None);

/// Session state and metadata for a claude.ai session.
pub struct SessionManager {
    /// The full session cookie string (e.g. "sessionKey=sk-ant-...").
    pub session_cookie: String,
    /// Organization UUID for API calls.
    pub org_id: String,
    /// Current conversation UUID.
    pub conv_id: String,
    /// Unix timestamp when the session expires.
    pub expires_at: i64,
    /// Whether the 24-hour warning has been emitted.
    warned_24h: bool,
    /// Whether the 2-hour warning has been emitted.
    warned_2h: bool,
    /// Whether the 30-minute warning has been emitted.
    warned_30m: bool,
    /// Whether a refresh attempt is currently in progress (prevents re-entrancy).
    refresh_in_progress: bool,
    /// Whether a full re-auth has been flagged as needed.
    pub needs_reauth: bool,
}

impl SessionManager {
    /// Create a new session manager with the given credentials.
    /// Sets expiry to `now + 7 days` by default.
    pub fn new(session_cookie: String, org_id: String, conv_id: String, now_unix: i64) -> Self {
        let expires_at = parse_cookie_expiry(&session_cookie)
            .unwrap_or(now_unix + DEFAULT_SESSION_LIFETIME_SECS);

        log::info!(
            "[session] initialized — expires at unix {} (in {} hours)",
            expires_at,
            (expires_at - now_unix) / 3600,
        );

        Self {
            session_cookie,
            org_id,
            conv_id,
            expires_at,
            warned_24h: false,
            warned_2h: false,
            warned_30m: false,
            refresh_in_progress: false,
            needs_reauth: false,
        }
    }

    /// Returns true if the current time is past the refresh threshold
    /// (i.e., less than 24 hours remain before expiry).
    pub fn needs_refresh(&self, now_unix: i64) -> bool {
        let remaining = self.expires_at - now_unix;
        remaining <= REFRESH_THRESHOLD_SECS && remaining > 0
    }

    /// Returns true if the session has already expired.
    pub fn is_expired(&self, now_unix: i64) -> bool {
        now_unix >= self.expires_at
    }

    /// Returns the number of seconds remaining until expiry.
    pub fn seconds_remaining(&self, now_unix: i64) -> i64 {
        self.expires_at - now_unix
    }

    /// Check and emit expiry warnings at 24h, 2h, and 30min thresholds.
    fn check_warnings(&mut self, now_unix: i64) {
        let remaining = self.expires_at - now_unix;

        if remaining <= WARN_30M_SECS && !self.warned_30m {
            self.warned_30m = true;
            log::warn!(
                "[session] !! Session expires in {} minutes !!",
                remaining / 60,
            );
        } else if remaining <= WARN_2H_SECS && !self.warned_2h {
            self.warned_2h = true;
            log::warn!(
                "[session] Session expires in {} hours {} minutes",
                remaining / 3600,
                (remaining % 3600) / 60,
            );
        } else if remaining <= WARN_24H_SECS && !self.warned_24h {
            self.warned_24h = true;
            log::warn!(
                "[session] Session expires in ~{} hours",
                remaining / 3600,
            );
        }
    }

    /// Update the session cookie and reset expiry/warning state.
    fn update_session(&mut self, new_cookie: String, now_unix: i64) {
        let new_expiry = parse_cookie_expiry(&new_cookie)
            .unwrap_or(now_unix + DEFAULT_SESSION_LIFETIME_SECS);

        log::info!(
            "[session] refreshed — new expiry unix {} (in {} hours)",
            new_expiry,
            (new_expiry - now_unix) / 3600,
        );

        self.session_cookie = new_cookie;
        self.expires_at = new_expiry;
        // Reset warning flags for the new expiry window.
        self.warned_24h = false;
        self.warned_2h = false;
        self.warned_30m = false;
        self.needs_reauth = false;
    }
}

/// Initialize the global session manager. Call once during boot after auth.
pub fn init(session_cookie: String, org_id: String, conv_id: String) {
    let now_unix = crate::rtc::boot_timestamp()
        + crate::rtc::uptime_seconds() as i64;
    let mgr = SessionManager::new(session_cookie, org_id, conv_id, now_unix);
    *SESSION.lock() = Some(mgr);
}

/// Get a copy of the current valid session cookie, or None if no session / expired.
pub fn get_session_cookie() -> Option<String> {
    let guard = SESSION.lock();
    guard.as_ref().map(|mgr| mgr.session_cookie.clone())
}

/// Get the org_id from the session manager.
pub fn get_org_id() -> Option<String> {
    let guard = SESSION.lock();
    guard.as_ref().map(|mgr| mgr.org_id.clone())
}

/// Get the conv_id from the session manager.
pub fn get_conv_id() -> Option<String> {
    let guard = SESSION.lock();
    guard.as_ref().map(|mgr| mgr.conv_id.clone())
}

/// Check whether the session needs a full re-auth (magic link flow).
pub fn needs_reauth() -> bool {
    let guard = SESSION.lock();
    guard.as_ref().map(|mgr| mgr.needs_reauth).unwrap_or(false)
}

/// Periodic check — call from the dashboard event loop every `CHECK_INTERVAL_TICKS`.
///
/// This function:
/// 1. Emits expiry warnings as thresholds are crossed.
/// 2. Attempts automatic refresh if within the refresh window.
/// 3. Flags `needs_reauth` if the session has expired and refresh failed.
///
/// `stack` and `now` are passed through for making HTTPS requests.
pub fn periodic_check(
    stack: &mut claudio_net::NetworkStack,
    now_fn: fn() -> claudio_net::Instant,
) {
    let now_unix = crate::rtc::boot_timestamp()
        + crate::rtc::uptime_seconds() as i64;

    // Scope the lock to avoid holding it during network I/O.
    let (should_refresh, is_expired, cookie_clone) = {
        let mut guard = SESSION.lock();
        let mgr = match guard.as_mut() {
            Some(m) => m,
            None => return,
        };

        mgr.check_warnings(now_unix);

        if mgr.is_expired(now_unix) {
            if !mgr.needs_reauth {
                log::error!("[session] session has EXPIRED — full re-auth required");
                mgr.needs_reauth = true;
            }
            return;
        }

        let should = mgr.needs_refresh(now_unix) && !mgr.refresh_in_progress;
        if should {
            mgr.refresh_in_progress = true;
        }
        (should, mgr.is_expired(now_unix), mgr.session_cookie.clone())
    };

    if !should_refresh || is_expired {
        return;
    }

    log::info!("[session] attempting automatic session refresh...");

    // Attempt refresh by calling /api/auth/session with the existing cookie.
    match attempt_refresh(stack, &cookie_clone, now_fn) {
        Ok(new_cookie) => {
            let now_unix = crate::rtc::boot_timestamp()
                + crate::rtc::uptime_seconds() as i64;
            let mut guard = SESSION.lock();
            if let Some(mgr) = guard.as_mut() {
                mgr.update_session(new_cookie.clone(), now_unix);
                mgr.refresh_in_progress = false;

                // Also update the agent_loop AuthMode so subsequent API calls
                // use the refreshed cookie.
                {
                    let mut guard = crate::agent_loop::AUTH_MODE.lock();
                    if let Some(crate::agent_loop::AuthMode::ClaudeAi {
                        session_cookie,
                        ..
                    }) = guard.as_mut()
                    {
                        *session_cookie = new_cookie.clone();
                    }
                }

                // Emit SAVE_SESSION marker so the host script can persist it.
                log::info!("[oauth] SAVE_SESSION:{}", new_cookie);

                // Also persist the refreshed cookie to the VFS so the next boot
                // (or a kernel that re-reads /claudio/session.txt) sees it.
                let conv_id = mgr.conv_id.clone();
                let blob = alloc::format!("{}\n{}", new_cookie, conv_id);
                match claudio_fs::write_file("/claudio/session.txt", blob.as_bytes()) {
                    Ok(()) => log::debug!(
                        "[session] persisted refreshed cookie to VFS ({} bytes)",
                        blob.len(),
                    ),
                    Err(e) => log::warn!("[session] VFS persist failed: {}", e),
                }

                log::info!("[session] cookie refreshed ({} bytes) [REDACTED]", new_cookie.len());
                log::info!("[session] refresh successful — session extended");
            }
        }
        Err(e) => {
            log::warn!("[session] refresh failed: {}", e);
            let mut guard = SESSION.lock();
            if let Some(mgr) = guard.as_mut() {
                mgr.refresh_in_progress = false;
                // Don't flag reauth yet — we'll retry next check interval.
                // Only flag if actually expired.
            }
        }
    }
}

/// Attempt to refresh the session by calling `/api/auth/session` on claude.ai.
///
/// If the session is still valid, claude.ai responds with 200 and may set
/// updated cookies. If invalid, we get a 401/403 and the caller should
/// trigger re-auth.
fn attempt_refresh(
    stack: &mut claudio_net::NetworkStack,
    cookie: &str,
    now_fn: fn() -> claudio_net::Instant,
) -> Result<String, &'static str> {
    let req = claudio_net::http::HttpRequest::get("claude.ai", "/api/auth/session")
        .header("Cookie", cookie)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:136.0) Gecko/20100101 Firefox/136.0",
        )
        .header("Accept", "application/json")
        .header("Origin", "https://claude.ai")
        .header("Referer", "https://claude.ai/")
        .header("Connection", "close");

    let seed = crate::interrupts::tick_count();
    let resp_bytes = claudio_net::https_request(
        stack,
        "claude.ai",
        443,
        &req.to_bytes(),
        now_fn,
        seed,
    )
    .map_err(|_| "HTTPS request to /api/auth/session failed")?;

    let resp_str = core::str::from_utf8(&resp_bytes).unwrap_or("");

    // Parse HTTP status.
    let status = resp_str
        .lines()
        .next()
        .and_then(|line| {
            let parts: alloc::vec::Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                parts[1].parse::<u16>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    log::info!("[session] /api/auth/session responded HTTP {}", status);

    if status == 200 {
        // Session is still valid. Collect any Set-Cookie headers for updated tokens.
        let mut new_cookies = String::new();
        for line in resp_str.split("\r\n") {
            if let Some(rest) = line
                .strip_prefix("Set-Cookie:")
                .or_else(|| line.strip_prefix("set-cookie:"))
            {
                if let Some(nv) = rest.trim().split(';').next() {
                    if !new_cookies.is_empty() {
                        new_cookies.push_str("; ");
                    }
                    new_cookies.push_str(nv);
                }
            }
        }

        if new_cookies.is_empty() {
            // No new cookies — the existing session is still valid.
            // Return the original cookie but extend the expiry.
            Ok(String::from(cookie))
        } else {
            // Merge: prefer new cookies, keep old ones that weren't replaced.
            let merged = merge_cookies(cookie, &new_cookies);
            Ok(merged)
        }
    } else if status == 401 || status == 403 {
        Err("session invalid — re-auth required")
    } else {
        Err("unexpected status from /api/auth/session")
    }
}

/// Merge old and new cookie strings. New values override old ones with the same name.
fn merge_cookies(old: &str, new: &str) -> String {
    use alloc::vec::Vec;

    // Parse cookies into (name, full_pair) tuples.
    let mut cookies: Vec<(&str, &str)> = Vec::new();

    for pair in old.split("; ") {
        let name = pair.split('=').next().unwrap_or(pair);
        cookies.push((name, pair));
    }

    // Override with new cookies.
    for pair in new.split("; ") {
        let name = pair.split('=').next().unwrap_or(pair);
        if let Some(existing) = cookies.iter_mut().find(|(n, _)| *n == name) {
            existing.1 = pair;
        } else {
            cookies.push((name, pair));
        }
    }

    let parts: Vec<&str> = cookies.iter().map(|(_, v)| *v).collect();
    parts.join("; ")
}

/// Try to parse an expiry timestamp from a session cookie.
///
/// claude.ai session cookies are often JWT-like base64-encoded JSON with an
/// `exp` field. This function attempts a best-effort parse. If parsing fails,
/// returns None and the caller falls back to the 7-day default.
fn parse_cookie_expiry(cookie: &str) -> Option<i64> {
    // Look for sessionKey=... and try to decode the JWT payload.
    // JWT format: header.payload.signature (base64url-encoded).
    let value = if let Some(rest) = cookie.strip_prefix("sessionKey=") {
        rest.split(';').next().unwrap_or(rest)
    } else if let Some(pos) = cookie.find("sessionKey=") {
        let rest = &cookie[pos + 11..];
        rest.split(';').next().unwrap_or(rest).split("; ").next().unwrap_or(rest)
    } else {
        return None;
    };

    // JWT has 3 dot-separated parts. The payload is the second.
    let parts: alloc::vec::Vec<&str> = value.split('.').collect();
    if parts.len() < 2 {
        return None;
    }

    let payload = parts[1];
    // Base64url decode (no padding).
    let decoded = base64url_decode(payload)?;
    let json = core::str::from_utf8(&decoded).ok()?;

    // Look for "exp":<number> in the JSON.
    // Simple manual parse — no serde in no_std kernel.
    if let Some(pos) = json.find("\"exp\"") {
        let rest = &json[pos + 5..];
        // Skip whitespace and colon.
        let rest = rest.trim_start().strip_prefix(':')?.trim_start();
        // Parse the number.
        let num_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let exp: i64 = rest[..num_end].parse().ok()?;
        if exp > 1_000_000_000 {
            // Looks like a valid Unix timestamp.
            return Some(exp);
        }
    }

    None
}

/// Minimal base64url decoder (no padding, URL-safe alphabet).
fn base64url_decode(input: &str) -> Option<alloc::vec::Vec<u8>> {
    use alloc::vec::Vec;

    fn decode_char(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'-' | b'+' => Some(62),
            b'_' | b'/' => Some(63),
            b'=' => None, // padding — skip
            _ => None,
        }
    }

    let bytes: Vec<u8> = input.bytes().filter_map(decode_char).collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);

    let chunks = bytes.len() / 4;
    for i in 0..chunks {
        let b0 = bytes[i * 4] as u32;
        let b1 = bytes[i * 4 + 1] as u32;
        let b2 = bytes[i * 4 + 2] as u32;
        let b3 = bytes[i * 4 + 3] as u32;
        let triple = (b0 << 18) | (b1 << 12) | (b2 << 6) | b3;
        out.push((triple >> 16) as u8);
        out.push((triple >> 8) as u8);
        out.push(triple as u8);
    }

    let rem = bytes.len() % 4;
    let base = chunks * 4;
    match rem {
        2 => {
            let b0 = bytes[base] as u32;
            let b1 = bytes[base + 1] as u32;
            let triple = (b0 << 18) | (b1 << 12);
            out.push((triple >> 16) as u8);
        }
        3 => {
            let b0 = bytes[base] as u32;
            let b1 = bytes[base + 1] as u32;
            let b2 = bytes[base + 2] as u32;
            let triple = (b0 << 18) | (b1 << 12) | (b2 << 6);
            out.push((triple >> 16) as u8);
            out.push((triple >> 8) as u8);
        }
        _ => {}
    }

    Some(out)
}
