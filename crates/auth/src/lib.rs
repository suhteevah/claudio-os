//! OAuth 2.0 Device Authorization Grant (RFC 8628) + API key fallback.
//!
//! Boot-time auth gate -> check persist -> refresh or device flow -> persist.
//!
//! Flow:
//! 1. `build_device_auth_request` -> POST to device auth endpoint
//! 2. `parse_device_auth_response` -> extract user code + verification URI
//! 3. Display prompt to user (on framebuffer)
//! 4. `build_token_poll_request` -> POST to token endpoint at `interval` seconds
//! 5. `parse_token_poll_response` -> check for success / pending / error
//! 6. `token_to_credentials` -> convert to `Credentials::OAuth`
//! 7. `credentials_to_json` -> persist to FAT32

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Anthropic OAuth endpoints
// ---------------------------------------------------------------------------

/// Device authorization endpoint path.
pub const DEVICE_AUTH_ENDPOINT: &str = "/oauth/device/code";

/// Token endpoint path.
pub const TOKEN_ENDPOINT: &str = "/oauth/token";

/// Authorization server host.
pub const AUTH_HOST: &str = "auth.anthropic.com";

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum AuthError {
    /// The server response was missing required fields or malformed.
    InvalidResponse,
    /// JSON parsing failed.
    JsonError,
    /// Network-level error (provided by caller, not generated here).
    NetworkError,
    /// The device code has expired before the user authorized.
    Expired,
    /// The user denied the authorization request.
    Denied,
    /// The requested auth method is not available.
    NotAvailable(String),
}

impl core::fmt::Display for AuthError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AuthError::InvalidResponse => write!(f, "invalid response from auth server"),
            AuthError::JsonError => write!(f, "JSON parse error"),
            AuthError::NetworkError => write!(f, "network error"),
            AuthError::Expired => write!(f, "device code expired"),
            AuthError::Denied => write!(f, "authorization denied by user"),
            AuthError::NotAvailable(msg) => write!(f, "not available: {}", msg),
        }
    }
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Credentials {
    ApiKey(String),
    OAuth {
        access_token: String,
        refresh_token: String,
        expires_at: u64,
    },
}

impl Credentials {
    /// Returns `true` if the credential has expired (or will never expire for API keys).
    pub fn is_expired(&self, now_unix: u64) -> bool {
        match self {
            Credentials::ApiKey(_) => false,
            Credentials::OAuth { expires_at, .. } => now_unix >= *expires_at,
        }
    }

    /// Returns the bearer token string to use in `Authorization` headers.
    pub fn bearer_token(&self) -> &str {
        match self {
            Credentials::ApiKey(key) => key,
            Credentials::OAuth { access_token, .. } => access_token,
        }
    }
}

// ---------------------------------------------------------------------------
// Device Flow Prompt (displayed to user on framebuffer)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct DeviceFlowPrompt {
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub user_code: String,
    pub expires_in: u32,
    pub interval: u32,
    /// Opaque device code needed for token polling. Caller must keep this.
    pub device_code: String,
}

// ---------------------------------------------------------------------------
// Device Authorization Response (from POST /oauth/device/code)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceAuthResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_expires_in")]
    pub expires_in: u32,
    #[serde(default = "default_interval")]
    pub interval: u32,
}

fn default_expires_in() -> u32 {
    900
}

fn default_interval() -> u32 {
    5
}

impl DeviceAuthResponse {
    /// Parse from raw JSON bytes.
    pub fn from_json(data: &[u8]) -> Result<Self, AuthError> {
        serde_json::from_slice(data).map_err(|e| {
            log::error!("[auth] failed to parse device auth response: {}", e);
            AuthError::JsonError
        })
    }
}

// ---------------------------------------------------------------------------
// Token Response (successful token grant)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u32,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

// ---------------------------------------------------------------------------
// Token Poll Result (token endpoint can return success or various errors)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum TokenPollResult {
    /// Token granted successfully.
    Success(TokenResponse),
    /// User has not yet completed authorization — keep polling.
    AuthorizationPending,
    /// Polling too fast — increase interval by 5 seconds (per RFC 8628).
    SlowDown,
    /// User denied the request.
    AccessDenied,
    /// The device code has expired.
    ExpiredToken,
    /// Some other error from the server.
    Error(String),
}

/// Helper struct for deserializing error responses from the token endpoint.
#[derive(Deserialize)]
struct TokenErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

impl TokenPollResult {
    /// Parse from raw JSON bytes. The token endpoint returns either a successful
    /// token response or an error object with an `error` field.
    pub fn from_json(data: &[u8]) -> Result<Self, AuthError> {
        // Try success first: if "access_token" is present, it's a success.
        if let Ok(token) = serde_json::from_slice::<TokenResponse>(data) {
            return Ok(TokenPollResult::Success(token));
        }

        // Otherwise parse as error response.
        let err: TokenErrorResponse = serde_json::from_slice(data).map_err(|e| {
            log::error!("[auth] failed to parse token poll response: {}", e);
            AuthError::JsonError
        })?;

        let result = match err.error.as_str() {
            "authorization_pending" => TokenPollResult::AuthorizationPending,
            "slow_down" => TokenPollResult::SlowDown,
            "access_denied" => TokenPollResult::AccessDenied,
            "expired_token" => TokenPollResult::ExpiredToken,
            other => {
                let desc = err
                    .error_description
                    .unwrap_or_else(|| String::from(other));
                TokenPollResult::Error(desc)
            }
        };

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Request builders
// ---------------------------------------------------------------------------

/// Build the HTTP request body for the device authorization endpoint.
///
/// Returns `(endpoint_path, json_body)`. The caller is responsible for
/// constructing the full HTTP/1.1 request with Host, Content-Type, etc.
pub fn build_device_auth_request(client_id: &str) -> (String, Vec<u8>) {
    let body = format!(
        "{{\"client_id\":\"{}\",\"scope\":\"messages:write\"}}",
        client_id
    );
    log::debug!(
        "[auth] device auth request: POST {} ({} bytes)",
        DEVICE_AUTH_ENDPOINT,
        body.len()
    );
    (String::from(DEVICE_AUTH_ENDPOINT), body.into_bytes())
}

/// Parse the device authorization response JSON and produce a `DeviceFlowPrompt`
/// that can be displayed to the user.
pub fn parse_device_auth_response(json: &[u8]) -> Result<DeviceFlowPrompt, AuthError> {
    let resp = DeviceAuthResponse::from_json(json)?;

    log::info!(
        "[auth] device flow: go to {} and enter code {}",
        resp.verification_uri,
        resp.user_code
    );

    Ok(DeviceFlowPrompt {
        verification_uri: resp.verification_uri.clone(),
        verification_uri_complete: resp.verification_uri_complete.clone(),
        user_code: resp.user_code.clone(),
        expires_in: resp.expires_in,
        interval: resp.interval,
        device_code: resp.device_code,
    })
}

/// Build the HTTP request body for polling the token endpoint.
///
/// Returns `(endpoint_path, json_body)`.
pub fn build_token_poll_request(client_id: &str, device_code: &str) -> (String, Vec<u8>) {
    let body = format!(
        "{{\"client_id\":\"{}\",\"device_code\":\"{}\",\"grant_type\":\"urn:ietf:params:oauth:grant-type:device_code\"}}",
        client_id, device_code
    );
    log::debug!(
        "[auth] token poll request: POST {} ({} bytes)",
        TOKEN_ENDPOINT,
        body.len()
    );
    (String::from(TOKEN_ENDPOINT), body.into_bytes())
}

/// Parse the token endpoint response and return the poll result.
pub fn parse_token_poll_response(json: &[u8]) -> Result<TokenPollResult, AuthError> {
    TokenPollResult::from_json(json)
}

/// Convert a successful `TokenResponse` into `Credentials::OAuth`.
///
/// `now_unix` is the current time as seconds since the Unix epoch. The
/// `expires_at` field is computed as `now_unix + expires_in`.
pub fn token_to_credentials(token: TokenResponse, now_unix: u64) -> Credentials {
    let expires_at = now_unix + token.expires_in as u64;
    log::info!(
        "[auth] token acquired, expires_at={} (in {}s)",
        expires_at,
        token.expires_in
    );
    Credentials::OAuth {
        access_token: token.access_token,
        refresh_token: token
            .refresh_token
            .unwrap_or_else(|| String::from("")),
        expires_at,
    }
}

// ---------------------------------------------------------------------------
// Build full HTTP request helper
// ---------------------------------------------------------------------------

/// Build a complete HTTP/1.1 POST request (headers + body) ready to send over
/// a TLS stream. This is a convenience for callers that need the raw bytes.
pub fn build_http_post(path: &str, body: &[u8]) -> Vec<u8> {
    let header = format!(
        "POST {} HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        path,
        AUTH_HOST,
        body.len()
    );
    let mut request = header.into_bytes();
    request.extend_from_slice(body);
    request
}

// ---------------------------------------------------------------------------
// Credential persistence (JSON <-> FAT32)
// ---------------------------------------------------------------------------

/// Serialize `Credentials` to JSON bytes for writing to FAT32 storage.
pub fn credentials_to_json(creds: &Credentials) -> Vec<u8> {
    // serde_json::to_vec should not fail for our types.
    serde_json::to_vec(creds).unwrap_or_else(|e| {
        log::error!("[auth] failed to serialize credentials: {}", e);
        Vec::new()
    })
}

/// Deserialize `Credentials` from JSON bytes read from FAT32 storage.
pub fn credentials_from_json(data: &[u8]) -> Result<Credentials, AuthError> {
    serde_json::from_slice(data).map_err(|e| {
        log::error!("[auth] failed to deserialize credentials: {}", e);
        AuthError::JsonError
    })
}

// ---------------------------------------------------------------------------
// Build refresh token request
// ---------------------------------------------------------------------------

/// Build an HTTP request body for refreshing an OAuth token.
///
/// Returns `(endpoint_path, json_body)`.
pub fn build_refresh_request(client_id: &str, refresh_token: &str) -> (String, Vec<u8>) {
    let body = format!(
        "{{\"client_id\":\"{}\",\"refresh_token\":\"{}\",\"grant_type\":\"refresh_token\"}}",
        client_id, refresh_token
    );
    log::debug!(
        "[auth] refresh request: POST {} ({} bytes)",
        TOKEN_ENDPOINT,
        body.len()
    );
    (String::from(TOKEN_ENDPOINT), body.into_bytes())
}

// ---------------------------------------------------------------------------
// High-level stubs (need networking to complete)
// ---------------------------------------------------------------------------

/// Attempt the OAuth device authorization flow.
///
/// Device flow is not available in ClaudioOS — authentication is handled
/// via claude.ai email+code flow or compile-time API key. The kernel's
/// `session_manager` orchestrates auth; this crate provides the building
/// blocks (request builders, response parsers, credential types).
///
/// Returns `Err(AuthError::NotAvailable)` with a guidance message.
pub async fn authenticate() -> Result<Credentials, AuthError> {
    log::warn!("[auth] authenticate() called — device flow is not available");
    Err(AuthError::NotAvailable(String::from(
        "Device flow not available — use claude.ai email+code authentication instead",
    )))
}

/// Callback hooks for [`token_refresh_loop`].
///
/// The auth crate has no access to the network stack or the filesystem
/// (adding `claudio-fs` as a dep would create a cycle, since `claudio-fs`
/// already depends on `claudio-auth` for the `Credentials` type). So the
/// refresh loop drives the *timing* and the caller injects whatever glue
/// it needs for network I/O, wall-clock time, async sleeping, and
/// persistence.
///
/// All methods are sync. The loop itself is `async` and calls them
/// between `await` points on a caller-supplied sleep future.
pub trait RefreshHooks {
    /// Return the current time as Unix seconds.
    fn now_unix(&self) -> u64;

    /// Perform an OAuth token refresh with the given refresh token and
    /// return the new `TokenResponse`, or `None` if the caller has not
    /// wired up a network path.
    fn refresh_token(&mut self, refresh_token: &str) -> Option<TokenResponse>;

    /// Persist freshly-minted credentials (e.g. via
    /// `claudio_fs::write_credentials`). The default no-op implementation
    /// is suitable for observer-only loops.
    fn persist(&mut self, _creds: &Credentials) {}
}

/// Background token refresh driver.
///
/// Polls the credential's expiry every `poll_interval_secs` and logs
/// time-to-expiry. When the token is within 5 minutes of expiring, the
/// loop calls [`RefreshHooks::refresh_token`] and, on success, hands the
/// new credentials to [`RefreshHooks::persist`] before looping.
///
/// For `Credentials::ApiKey` the loop still runs but only logs — API keys
/// don't expire.
///
/// The caller is responsible for driving the loop via `await`. Between
/// iterations this function calls the `sleep` future returned by the
/// provided `sleeper`, which lets the caller bridge to whatever async
/// timer source they have (e.g. the kernel executor's `Delay`). If
/// `sleeper` is `None` the function returns after a single iteration so
/// the caller can choose an external poll strategy.
pub async fn token_refresh_loop<H, S, F>(
    mut creds: Credentials,
    hooks: &mut H,
    mut sleeper: Option<S>,
) where
    H: RefreshHooks,
    S: FnMut(u64) -> F,
    F: core::future::Future<Output = ()>,
{
    /// Buffer in seconds before the actual expiry at which we trigger a
    /// refresh. Five minutes is the industry-standard skew.
    const REFRESH_BUFFER_SECS: u64 = 300;
    /// How often we wake up to check.
    const POLL_INTERVAL_SECS: u64 = 60;

    log::info!("[auth] token_refresh_loop started");

    loop {
        let now = hooks.now_unix();
        match &creds {
            Credentials::ApiKey(_) => {
                log::trace!("[auth] refresh loop: API key, no expiry");
            }
            Credentials::OAuth {
                access_token: _,
                refresh_token,
                expires_at,
            } => {
                if *expires_at <= now {
                    log::warn!(
                        "[auth] OAuth token already expired (expires_at={}, now={})",
                        expires_at, now,
                    );
                } else {
                    let remaining = *expires_at - now;
                    log::debug!(
                        "[auth] OAuth token valid for {} more seconds (expires_at={})",
                        remaining, expires_at,
                    );

                    if remaining <= REFRESH_BUFFER_SECS {
                        log::warn!(
                            "[auth] OAuth token expires in {}s — attempting refresh",
                            remaining,
                        );

                        let rt = refresh_token.clone();
                        match hooks.refresh_token(&rt) {
                            Some(token) => {
                                let new_creds = token_to_credentials(token, now);
                                log::info!(
                                    "[auth] OAuth token refreshed successfully",
                                );
                                hooks.persist(&new_creds);
                                creds = new_creds;
                            }
                            None => {
                                log::warn!(
                                    "[auth] refresher returned None — network path not wired",
                                );
                            }
                        }
                    }
                }
            }
        }

        match sleeper.as_mut() {
            Some(sleep_fn) => sleep_fn(POLL_INTERVAL_SECS).await,
            None => {
                log::info!(
                    "[auth] no sleeper injected — returning after one iteration",
                );
                return;
            }
        }
    }
}
