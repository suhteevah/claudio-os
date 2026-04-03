//! Conversation management for claude.ai sessions.
//!
//! Provides listing, selecting, renaming, and deleting conversations via the
//! claude.ai REST API. Shell commands (`conversations`, `conv use`, etc.) are
//! handled by [`handle_conv_command`].

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use claudio_net::NetworkStack;

use crate::agent_loop::{auth_mode, AuthMode, RNG_SEED};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Summary of a single claude.ai conversation.
#[derive(Clone, Debug)]
pub struct ConvSummary {
    /// Conversation UUID.
    pub uuid: String,
    /// User-visible name (may be empty).
    pub name: String,
    /// Short summary text (may be empty).
    pub summary: String,
    /// Last-updated timestamp string (ISO 8601).
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

/// Perform an HTTPS request to claude.ai and return the raw response bytes.
fn claude_ai_request(
    stack: &mut NetworkStack,
    method: &'static str,
    path: &str,
    session_cookie: &str,
    body: Option<Vec<u8>>,
    now: fn() -> claudio_net::Instant,
) -> Result<Vec<u8>, String> {
    let http_req = claudio_net::http::HttpRequest {
        method,
        path: String::from(path),
        host: String::from("claude.ai"),
        headers: Vec::new(),
        body,
    };

    let http_req = http_req
        .header("Cookie", session_cookie)
        .header("Content-Type", "application/json")
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:136.0) Gecko/20100101 Firefox/136.0")
        .header("Accept", "application/json")
        .header("Origin", "https://claude.ai")
        .header("Referer", "https://claude.ai")
        .header("Connection", "close");

    let req_bytes = http_req.to_bytes();
    log::debug!("[conversations] {} {} ({} bytes)", method, path, req_bytes.len());

    let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);
    let resp = claudio_net::https_request(stack, "claude.ai", 443, &req_bytes, now, seed)
        .map_err(|e| format!("claude.ai request failed: {:?}", e))?;

    log::debug!("[conversations] received {} bytes", resp.len());
    Ok(resp)
}

/// Extract the HTTP response body (after \r\n\r\n) and check status.
fn extract_body(raw: &[u8], expect_status: u16) -> Result<Vec<u8>, String> {
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| String::from("no HTTP header terminator"))?;

    let headers = core::str::from_utf8(&raw[..header_end]).unwrap_or("");
    let status = headers
        .split(' ')
        .nth(1)
        .unwrap_or("0")
        .parse::<u16>()
        .unwrap_or(0);

    let body_raw = &raw[header_end + 4..];
    let body = claudio_net::http::decode_chunked(body_raw)
        .unwrap_or_else(|_| body_raw.to_vec());

    if status != expect_status {
        let body_text = core::str::from_utf8(&body).unwrap_or("<binary>");
        return Err(format!("HTTP {} (expected {}): {}", status, expect_status,
            &body_text[..body_text.len().min(300)]));
    }

    Ok(body)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// List conversations for the authenticated claude.ai organization.
///
/// Calls `GET /api/organizations/{org}/chat_conversations` and parses the
/// JSON array response. Returns conversations sorted by `updated_at` descending.
pub fn list_conversations(
    stack: &mut NetworkStack,
    session_cookie: &str,
    org_id: &str,
    now: fn() -> claudio_net::Instant,
) -> Result<Vec<ConvSummary>, String> {
    let path = format!("/api/organizations/{}/chat_conversations", org_id);
    let resp = claude_ai_request(stack, "GET", &path, session_cookie, None, now)?;
    let body = extract_body(&resp, 200)?;

    let body_str = core::str::from_utf8(&body)
        .map_err(|_| String::from("response not UTF-8"))?;

    // Parse the JSON array of conversation objects.
    // We use serde_json since it's already a dependency.
    let arr: serde_json::Value = serde_json::from_str(body_str)
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let items = arr.as_array().ok_or_else(|| String::from("expected JSON array"))?;

    let mut convos: Vec<ConvSummary> = items
        .iter()
        .filter_map(|item| {
            let uuid = item.get("uuid")?.as_str()?.into();
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into();
            let summary = item
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into();
            let updated_at = item
                .get("updated_at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into();
            Some(ConvSummary {
                uuid,
                name,
                summary,
                updated_at,
            })
        })
        .collect();

    // Sort by updated_at descending (lexicographic on ISO 8601 works).
    convos.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    log::info!("[conversations] listed {} conversations", convos.len());
    Ok(convos)
}

/// Rename a conversation.
///
/// Calls `PATCH /api/organizations/{org}/chat_conversations/{conv_id}`.
pub fn rename_conversation(
    stack: &mut NetworkStack,
    session_cookie: &str,
    org_id: &str,
    conv_id: &str,
    new_name: &str,
    now: fn() -> claudio_net::Instant,
) -> Result<(), String> {
    let path = format!(
        "/api/organizations/{}/chat_conversations/{}",
        org_id, conv_id
    );
    let body = format!(r#"{{"name":"{}"}}"#, new_name.replace('"', "\\\""));
    let resp = claude_ai_request(
        stack,
        "PATCH",
        &path,
        session_cookie,
        Some(body.into_bytes()),
        now,
    )?;
    let _ = extract_body(&resp, 200)?;
    log::info!("[conversations] renamed {} to \"{}\"", conv_id, new_name);
    Ok(())
}

/// Delete a conversation.
///
/// Calls `DELETE /api/organizations/{org}/chat_conversations/{conv_id}`.
pub fn delete_conversation(
    stack: &mut NetworkStack,
    session_cookie: &str,
    org_id: &str,
    conv_id: &str,
    now: fn() -> claudio_net::Instant,
) -> Result<(), String> {
    let path = format!(
        "/api/organizations/{}/chat_conversations/{}",
        org_id, conv_id
    );
    let resp = claude_ai_request(stack, "DELETE", &path, session_cookie, None, now)?;
    // DELETE may return 200 or 204.
    let header_end = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| String::from("no HTTP header terminator"))?;
    let headers = core::str::from_utf8(&resp[..header_end]).unwrap_or("");
    let status = headers
        .split(' ')
        .nth(1)
        .unwrap_or("0")
        .parse::<u16>()
        .unwrap_or(0);
    if status != 200 && status != 204 {
        let body_raw = &resp[header_end + 4..];
        let body_text = core::str::from_utf8(body_raw).unwrap_or("<binary>");
        return Err(format!("HTTP {}: {}", status, &body_text[..body_text.len().min(300)]));
    }
    log::info!("[conversations] deleted {}", conv_id);
    Ok(())
}

// ---------------------------------------------------------------------------
// Active conversation tracking per agent
// ---------------------------------------------------------------------------

/// Per-agent active conversation ID. Indexed by agent session id.
/// Protected by the kernel's single-threaded async executor.
static mut ACTIVE_CONVS: Option<Vec<(usize, String)>> = None;

fn active_convs() -> &'static mut Vec<(usize, String)> {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(ACTIVE_CONVS);
        if (*ptr).is_none() {
            *ptr = Some(Vec::new());
        }
        (*ptr).as_mut().unwrap()
    }
}

/// Set the active conversation ID for an agent.
pub fn set_active_conv(agent_id: usize, conv_id: String) {
    let convs = active_convs();
    if let Some(entry) = convs.iter_mut().find(|(id, _)| *id == agent_id) {
        entry.1 = conv_id;
    } else {
        convs.push((agent_id, conv_id));
    }
}

/// Get the active conversation ID for an agent.
pub fn get_active_conv(agent_id: usize) -> Option<&'static str> {
    let convs = active_convs();
    convs.iter().find(|(id, _)| *id == agent_id).map(|(_, c)| c.as_str())
}

// ---------------------------------------------------------------------------
// Shell command handler
// ---------------------------------------------------------------------------

/// Handle conversation management commands typed in a shell or agent pane.
///
/// Recognized commands:
/// - `conversations` / `convos` — list recent conversations
/// - `conv use <uuid>` — switch agent to use this conversation
/// - `conv rename <uuid> <name>` — rename a conversation
/// - `conv delete <uuid>` — delete a conversation
/// - `conv new [name]` — create a new conversation (sets active conv to empty)
///
/// Returns `Some(output_string)` if the command was handled, `None` otherwise.
pub fn handle_conv_command(
    input: &str,
    stack: &mut NetworkStack,
    now: fn() -> claudio_net::Instant,
    agent_id: Option<usize>,
) -> Option<String> {
    let trimmed = input.trim();

    // Check for our commands.
    let is_list = trimmed == "conversations" || trimmed == "convos";
    let is_conv = trimmed.starts_with("conv ");

    if !is_list && !is_conv {
        return None;
    }

    // We need claude.ai auth mode.
    let (session_cookie, org_id, _conv_id) = match auth_mode() {
        Some(AuthMode::ClaudeAi {
            session_cookie,
            org_id,
            conv_id,
        }) => (session_cookie.as_str(), org_id.as_str(), conv_id.as_str()),
        _ => {
            return Some(String::from(
                "\x1b[31mConversation management requires claude.ai auth mode.\x1b[0m",
            ));
        }
    };

    if is_list {
        return Some(cmd_list(stack, session_cookie, org_id, now));
    }

    // Parse `conv <subcommand> ...`
    let rest = &trimmed[5..]; // skip "conv "

    if let Some(uuid) = rest.strip_prefix("use ") {
        let uuid = uuid.trim();
        if uuid.is_empty() {
            return Some(String::from("\x1b[31mUsage: conv use <uuid>\x1b[0m"));
        }
        return Some(cmd_use(uuid, agent_id));
    }

    if let Some(rest) = rest.strip_prefix("rename ") {
        let mut parts = rest.splitn(2, ' ');
        let uuid = parts.next().unwrap_or("").trim();
        let name = parts.next().unwrap_or("").trim();
        if uuid.is_empty() || name.is_empty() {
            return Some(String::from(
                "\x1b[31mUsage: conv rename <uuid> <new-name>\x1b[0m",
            ));
        }
        return Some(cmd_rename(stack, session_cookie, org_id, uuid, name, now));
    }

    if let Some(uuid) = rest.strip_prefix("delete ") {
        let uuid = uuid.trim();
        if uuid.is_empty() {
            return Some(String::from("\x1b[31mUsage: conv delete <uuid>\x1b[0m"));
        }
        return Some(cmd_delete(stack, session_cookie, org_id, uuid, now));
    }

    if rest == "new" || rest.starts_with("new ") {
        let name = if rest.len() > 4 { rest[4..].trim() } else { "" };
        return Some(cmd_new(name, agent_id));
    }

    Some(format!(
        "\x1b[31mUnknown conv subcommand. Usage:\r\n  conversations | convos\r\n  conv use <uuid>\r\n  conv rename <uuid> <name>\r\n  conv delete <uuid>\r\n  conv new [name]\x1b[0m"
    ))
}

// ---------------------------------------------------------------------------
// Individual command implementations
// ---------------------------------------------------------------------------

fn cmd_list(
    stack: &mut NetworkStack,
    session_cookie: &str,
    org_id: &str,
    now: fn() -> claudio_net::Instant,
) -> String {
    match list_conversations(stack, session_cookie, org_id, now) {
        Ok(convos) => {
            if convos.is_empty() {
                return String::from("\x1b[90mNo conversations found.\x1b[0m");
            }
            let mut out = String::from("\x1b[96mRecent Conversations:\x1b[0m\r\n");
            out.push_str("\x1b[90m────────────────────────────────────────────────────\x1b[0m\r\n");
            // Show up to 20 most recent.
            for (i, c) in convos.iter().take(20).enumerate() {
                let name_display = if c.name.is_empty() {
                    "\x1b[90m(untitled)\x1b[0m"
                } else {
                    &c.name
                };
                let short_uuid = if c.uuid.len() > 8 {
                    &c.uuid[..8]
                } else {
                    &c.uuid
                };
                out.push_str(&format!(
                    "  \x1b[33m{:>2}.\x1b[0m \x1b[37m{}\x1b[0m  \x1b[90m{}...\x1b[0m",
                    i + 1,
                    name_display,
                    short_uuid,
                ));
                if !c.summary.is_empty() {
                    let short_summary = if c.summary.len() > 50 {
                        format!("{}...", &c.summary[..50])
                    } else {
                        c.summary.clone()
                    };
                    out.push_str(&format!("\r\n      \x1b[90m{}\x1b[0m", short_summary));
                }
                if !c.updated_at.is_empty() {
                    // Show just date portion.
                    let date = if c.updated_at.len() >= 10 {
                        &c.updated_at[..10]
                    } else {
                        &c.updated_at
                    };
                    out.push_str(&format!("  \x1b[90m({})\x1b[0m", date));
                }
                out.push_str("\r\n");
            }
            if convos.len() > 20 {
                out.push_str(&format!(
                    "\x1b[90m  ... and {} more\x1b[0m\r\n",
                    convos.len() - 20
                ));
            }
            out.push_str("\r\n\x1b[90mUse: conv use <full-uuid> to switch\x1b[0m");
            out
        }
        Err(e) => format!("\x1b[31mFailed to list conversations: {}\x1b[0m", e),
    }
}

fn cmd_use(uuid: &str, agent_id: Option<usize>) -> String {
    match agent_id {
        Some(aid) => {
            // Update the global auth mode's conv_id.
            unsafe {
                if let Some(AuthMode::ClaudeAi { conv_id, .. }) =
                    &mut *core::ptr::addr_of_mut!(crate::agent_loop::AUTH_MODE)
                {
                    *conv_id = String::from(uuid);
                }
            }
            set_active_conv(aid, String::from(uuid));
            format!(
                "\x1b[92mSwitched to conversation {}.\x1b[0m\r\n\x1b[90mNext message will go to this conversation.\x1b[0m",
                &uuid[..uuid.len().min(8)]
            )
        }
        None => {
            // Shell pane — just update the global auth mode.
            unsafe {
                if let Some(AuthMode::ClaudeAi { conv_id, .. }) =
                    &mut *core::ptr::addr_of_mut!(crate::agent_loop::AUTH_MODE)
                {
                    *conv_id = String::from(uuid);
                }
            }
            format!(
                "\x1b[92mGlobal conversation set to {}.\x1b[0m\r\n\x1b[90mAll new agent messages will use this conversation.\x1b[0m",
                &uuid[..uuid.len().min(8)]
            )
        }
    }
}

fn cmd_rename(
    stack: &mut NetworkStack,
    session_cookie: &str,
    org_id: &str,
    uuid: &str,
    new_name: &str,
    now: fn() -> claudio_net::Instant,
) -> String {
    match rename_conversation(stack, session_cookie, org_id, uuid, new_name, now) {
        Ok(()) => format!(
            "\x1b[92mRenamed {} to \"{}\".\x1b[0m",
            &uuid[..uuid.len().min(8)],
            new_name
        ),
        Err(e) => format!("\x1b[31mFailed to rename: {}\x1b[0m", e),
    }
}

fn cmd_delete(
    stack: &mut NetworkStack,
    session_cookie: &str,
    org_id: &str,
    uuid: &str,
    now: fn() -> claudio_net::Instant,
) -> String {
    match delete_conversation(stack, session_cookie, org_id, uuid, now) {
        Ok(()) => format!(
            "\x1b[92mDeleted conversation {}.\x1b[0m",
            &uuid[..uuid.len().min(8)]
        ),
        Err(e) => format!("\x1b[31mFailed to delete: {}\x1b[0m", e),
    }
}

fn cmd_new(name: &str, agent_id: Option<usize>) -> String {
    // "New conversation" means clearing the active conv_id so the next message
    // creates a fresh conversation on claude.ai.
    // For now we set conv_id to an empty string which the agent_loop can check.
    let label = if name.is_empty() {
        String::from("(untitled)")
    } else {
        String::from(name)
    };

    if let Some(aid) = agent_id {
        set_active_conv(aid, String::new());
    }

    // Update global auth mode conv_id to empty so next request creates new conv.
    unsafe {
        if let Some(AuthMode::ClaudeAi { conv_id, .. }) =
            &mut *core::ptr::addr_of_mut!(crate::agent_loop::AUTH_MODE)
        {
            *conv_id = String::new();
        }
    }

    format!(
        "\x1b[92mNew conversation: {}.\x1b[0m\r\n\x1b[90mNext message will start a fresh conversation.\x1b[0m",
        label
    )
}
