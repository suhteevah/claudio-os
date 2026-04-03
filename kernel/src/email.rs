//! Email client for ClaudioOS — SMTP (sending) and IMAP (reading).
//!
//! Provides a bare-metal email client that operates directly on the smoltcp
//! `NetworkStack` via TLS connections. Supports:
//!
//! - **SMTP**: SMTPS on port 465 (implicit TLS)
//! - **IMAP**: IMAPS on port 993 (implicit TLS)
//!
//! ## Shell commands
//! - `mail` — list inbox (recent 20 messages)
//! - `mail read <uid>` — read a specific message
//! - `mail send <to> <subject>` — compose and send (body in args)
//! - `mail config <smtp> <imap> <user> <pass>` — configure account
//!
//! ## Agent tools
//! - `send_email` — send an email programmatically
//! - `check_email` — list/read inbox messages

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use spin::Mutex;

use claudio_net::{Instant, NetworkStack};

// ---------------------------------------------------------------------------
// RNG seed for TLS connections
// ---------------------------------------------------------------------------

use crate::agent_loop::RNG_SEED;

// ---------------------------------------------------------------------------
// Base64 encoder (minimal, for AUTH LOGIN)
// ---------------------------------------------------------------------------

const B64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i] as u32;
        let b1 = if i + 1 < input.len() { input[i + 1] as u32 } else { 0 };
        let b2 = if i + 2 < input.len() { input[i + 2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(B64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(B64_CHARS[((triple >> 12) & 0x3F) as usize] as char);

        if i + 1 < input.len() {
            out.push(B64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < input.len() {
            out.push(B64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Summary of an email message (from IMAP FETCH headers).
#[derive(Clone, Debug)]
pub struct EmailSummary {
    /// IMAP UID.
    pub uid: u32,
    /// From header.
    pub from: String,
    /// Subject header.
    pub subject: String,
    /// Date header.
    pub date: String,
    /// Whether the message has been read (\Seen flag).
    pub seen: bool,
}

/// Full email message.
#[derive(Clone, Debug)]
pub struct EmailMessage {
    /// IMAP UID.
    pub uid: u32,
    /// From header.
    pub from: String,
    /// To header.
    pub to: String,
    /// Subject header.
    pub subject: String,
    /// Date header.
    pub date: String,
    /// Plain text body.
    pub body: String,
    /// List of attachment filenames.
    pub attachments: Vec<String>,
}

/// Email account configuration.
#[derive(Clone, Debug)]
pub struct EmailConfig {
    /// IMAP server hostname.
    pub imap_server: String,
    /// SMTP server hostname.
    pub smtp_server: String,
    /// Username (usually the email address).
    pub user: String,
    /// Password or app password.
    pub pass: String,
    /// From address.
    pub from_addr: String,
}

// ---------------------------------------------------------------------------
// Global config (stored in memory, `mail config` sets it)
// ---------------------------------------------------------------------------

static EMAIL_CONFIG: Mutex<Option<EmailConfig>> = Mutex::new(None);

/// Set the global email configuration.
pub fn set_config(config: EmailConfig) {
    *EMAIL_CONFIG.lock() = Some(config);
}

/// Get a clone of the current config, if set.
pub fn get_config() -> Option<EmailConfig> {
    EMAIL_CONFIG.lock().clone()
}

// ---------------------------------------------------------------------------
// TLS helper — send command, read response
// ---------------------------------------------------------------------------

/// Send bytes over TLS and return the response as a String.
fn tls_cmd(
    tls: &mut claudio_net::TlsStream,
    stack: &mut NetworkStack,
    cmd: &[u8],
    buf: &mut [u8],
    now: fn() -> Instant,
) -> Result<String, String> {
    tls.send(stack, cmd, || now())
        .map_err(|e| format!("TLS send failed: {:?}", e))?;
    let n = tls.recv(stack, buf, || now())
        .map_err(|e| format!("TLS recv failed: {:?}", e))?;
    Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// Read from TLS without sending first.
fn tls_read(
    tls: &mut claudio_net::TlsStream,
    stack: &mut NetworkStack,
    buf: &mut [u8],
    now: fn() -> Instant,
) -> Result<String, String> {
    let n = tls.recv(stack, buf, || now())
        .map_err(|e| format!("TLS recv failed: {:?}", e))?;
    Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
}

// ---------------------------------------------------------------------------
// SMTP client — send_email
// ---------------------------------------------------------------------------

/// Send an email via SMTPS (port 465, implicit TLS).
///
/// Uses AUTH LOGIN with base64-encoded credentials.
pub fn send_email(
    stack: &mut NetworkStack,
    smtp_server: &str,
    user: &str,
    pass: &str,
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
    now: fn() -> Instant,
) -> Result<String, String> {
    log::info!("[email] sending via {} from {} to {}", smtp_server, from, to);

    // Resolve SMTP server.
    let ip = claudio_net::dns::resolve(stack, smtp_server, || now())
        .map_err(|e| format!("DNS resolution failed for {}: {:?}", smtp_server, e))?;

    // Build the raw email message.
    let message = format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{}",
        from, to, subject, body
    );

    // Implicit TLS on port 465 (SMTPS).
    let port = 465u16;
    let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);

    let mut tls = claudio_net::TlsStream::connect(stack, ip, port, smtp_server, now, seed)
        .map_err(|e| format!("TLS connect to {}:{} failed: {:?}", smtp_server, port, e))?;

    let mut buf = [0u8; 2048];

    // Read server greeting (220).
    let greeting = tls_read(&mut tls, stack, &mut buf, now)?;
    log::debug!("[smtp] greeting: {}", greeting.trim());
    if !greeting.starts_with("220") {
        tls.close(stack);
        return Err(format!("SMTP unexpected greeting: {}", greeting.trim()));
    }

    // EHLO.
    let ehlo_resp = tls_cmd(&mut tls, stack, b"EHLO claudioos\r\n", &mut buf, now)?;
    log::debug!("[smtp] EHLO response: {}", ehlo_resp.trim());

    // AUTH LOGIN.
    let auth_resp = tls_cmd(&mut tls, stack, b"AUTH LOGIN\r\n", &mut buf, now)?;
    log::debug!("[smtp] AUTH response: {}", auth_resp.trim());
    if !auth_resp.starts_with("334") {
        tls.close(stack);
        return Err(format!("SMTP AUTH not accepted: {}", auth_resp.trim()));
    }

    // Send base64-encoded username.
    let user_b64 = format!("{}\r\n", base64_encode(user.as_bytes()));
    let user_resp = tls_cmd(&mut tls, stack, user_b64.as_bytes(), &mut buf, now)?;
    if !user_resp.starts_with("334") {
        tls.close(stack);
        return Err(format!("SMTP username rejected: {}", user_resp.trim()));
    }

    // Send base64-encoded password.
    let pass_b64 = format!("{}\r\n", base64_encode(pass.as_bytes()));
    let pass_resp = tls_cmd(&mut tls, stack, pass_b64.as_bytes(), &mut buf, now)?;
    if !pass_resp.starts_with("235") {
        tls.close(stack);
        return Err(format!("SMTP authentication failed: {}", pass_resp.trim()));
    }

    // MAIL FROM.
    let mail_from = format!("MAIL FROM:<{}>\r\n", from);
    let from_resp = tls_cmd(&mut tls, stack, mail_from.as_bytes(), &mut buf, now)?;
    if !from_resp.starts_with("250") {
        tls.close(stack);
        return Err(format!("SMTP MAIL FROM rejected: {}", from_resp.trim()));
    }

    // RCPT TO.
    let rcpt_to = format!("RCPT TO:<{}>\r\n", to);
    let rcpt_resp = tls_cmd(&mut tls, stack, rcpt_to.as_bytes(), &mut buf, now)?;
    if !rcpt_resp.starts_with("250") {
        tls.close(stack);
        return Err(format!("SMTP RCPT TO rejected: {}", rcpt_resp.trim()));
    }

    // DATA.
    let data_resp = tls_cmd(&mut tls, stack, b"DATA\r\n", &mut buf, now)?;
    if !data_resp.starts_with("354") {
        tls.close(stack);
        return Err(format!("SMTP DATA not accepted: {}", data_resp.trim()));
    }

    // Send the message body, terminated by \r\n.\r\n.
    let mut msg_data = message.into_bytes();
    if !msg_data.ends_with(b"\r\n") {
        msg_data.extend_from_slice(b"\r\n");
    }
    msg_data.extend_from_slice(b".\r\n");
    let msg_resp = tls_cmd(&mut tls, stack, &msg_data, &mut buf, now)?;
    if !msg_resp.starts_with("250") {
        tls.close(stack);
        return Err(format!("SMTP message not accepted: {}", msg_resp.trim()));
    }

    // QUIT.
    let _ = tls_cmd(&mut tls, stack, b"QUIT\r\n", &mut buf, now);
    tls.close(stack);

    log::info!("[email] sent successfully to {}", to);
    Ok(format!("Email sent to {} via {}", to, smtp_server))
}

// ---------------------------------------------------------------------------
// IMAP client — fetch_inbox, fetch_message
// ---------------------------------------------------------------------------

/// Send a tagged IMAP command and read the response.
fn imap_cmd(
    tls: &mut claudio_net::TlsStream,
    stack: &mut NetworkStack,
    tag: &str,
    cmd: &str,
    buf: &mut [u8],
    now: fn() -> Instant,
) -> Result<String, String> {
    let line = format!("{} {}\r\n", tag, cmd);
    log::debug!("[imap] >>> {}", line.trim());
    tls_cmd(tls, stack, line.as_bytes(), buf, now)
}

/// Fetch the N most recent messages from the inbox.
pub fn fetch_inbox(
    stack: &mut NetworkStack,
    imap_server: &str,
    user: &str,
    pass: &str,
    count: usize,
    now: fn() -> Instant,
) -> Result<Vec<EmailSummary>, String> {
    log::info!("[email] fetching inbox from {} (last {} msgs)", imap_server, count);

    let ip = claudio_net::dns::resolve(stack, imap_server, || now())
        .map_err(|e| format!("DNS resolution failed for {}: {:?}", imap_server, e))?;

    let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);
    let mut tls = claudio_net::TlsStream::connect(stack, ip, 993, imap_server, now, seed)
        .map_err(|e| format!("IMAPS connect failed: {:?}", e))?;

    let mut buf = [0u8; 8192];

    // Read greeting.
    let greeting = tls_read(&mut tls, stack, &mut buf, now)?;
    if !greeting.contains("OK") {
        tls.close(stack);
        return Err(format!("IMAP bad greeting: {}", greeting.trim()));
    }

    // LOGIN.
    let login_cmd = format!("LOGIN {} {}", user, pass);
    let login_resp = imap_cmd(&mut tls, stack, "A1", &login_cmd, &mut buf, now)?;
    if !login_resp.contains("A1 OK") {
        tls.close(stack);
        return Err(format!("IMAP LOGIN failed: {}", login_resp.trim()));
    }

    // SELECT INBOX.
    let select_resp = imap_cmd(&mut tls, stack, "A2", "SELECT INBOX", &mut buf, now)?;
    let exists = parse_exists(&select_resp).unwrap_or(0);
    log::info!("[imap] INBOX has {} messages", exists);

    if exists == 0 {
        let _ = imap_cmd(&mut tls, stack, "A9", "LOGOUT", &mut buf, now);
        tls.close(stack);
        return Ok(Vec::new());
    }

    // Fetch headers of last N messages.
    let start = if exists > count as u32 { exists - count as u32 + 1 } else { 1 };
    let fetch_cmd = format!(
        "FETCH {}:{} (UID FLAGS BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE)])",
        start, exists
    );
    let _ = imap_cmd(&mut tls, stack, "A3", &fetch_cmd, &mut buf, now)?;

    // Read all response data (may come in multiple chunks).
    let mut all_data = String::new();
    // First response already received above, but it may be incomplete.
    // Read more until we see the tagged completion.
    // Actually the imap_cmd already read one chunk. Let's read more.
    loop {
        match tls.recv(stack, &mut buf, || now()) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                let has_tag = chunk.contains("A3 OK") || chunk.contains("A3 NO") || chunk.contains("A3 BAD");
                all_data.push_str(&chunk);
                if has_tag { break; }
            }
            Err(_) => break,
        }
        if all_data.len() > 256 * 1024 { break; }
    }

    let summaries = parse_fetch_headers(&all_data);

    // LOGOUT.
    let _ = imap_cmd(&mut tls, stack, "A9", "LOGOUT", &mut buf, now);
    tls.close(stack);

    Ok(summaries)
}

/// Fetch a single message by UID.
pub fn fetch_message(
    stack: &mut NetworkStack,
    imap_server: &str,
    user: &str,
    pass: &str,
    uid: u32,
    now: fn() -> Instant,
) -> Result<EmailMessage, String> {
    log::info!("[email] fetching message UID {} from {}", uid, imap_server);

    let ip = claudio_net::dns::resolve(stack, imap_server, || now())
        .map_err(|e| format!("DNS resolution failed for {}: {:?}", imap_server, e))?;

    let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);
    let mut tls = claudio_net::TlsStream::connect(stack, ip, 993, imap_server, now, seed)
        .map_err(|e| format!("IMAPS connect failed: {:?}", e))?;

    let mut buf = [0u8; 16384];

    // Greeting.
    let _ = tls_read(&mut tls, stack, &mut buf, now)?;

    // LOGIN.
    let login_cmd = format!("LOGIN {} {}", user, pass);
    let login_resp = imap_cmd(&mut tls, stack, "A1", &login_cmd, &mut buf, now)?;
    if !login_resp.contains("A1 OK") {
        tls.close(stack);
        return Err(format!("IMAP LOGIN failed: {}", login_resp.trim()));
    }

    // SELECT INBOX.
    let _ = imap_cmd(&mut tls, stack, "A2", "SELECT INBOX", &mut buf, now)?;

    // FETCH full message by UID.
    let fetch_cmd = format!("UID FETCH {} (FLAGS BODY[])", uid);
    let first_chunk = imap_cmd(&mut tls, stack, "A3", &fetch_cmd, &mut buf, now)?;

    // Read all data.
    let mut all_data = first_chunk;
    loop {
        match tls.recv(stack, &mut buf, || now()) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                let has_tag = chunk.contains("A3 OK") || chunk.contains("A3 NO") || chunk.contains("A3 BAD");
                all_data.push_str(&chunk);
                if has_tag { break; }
            }
            Err(_) => break,
        }
        if all_data.len() > 1024 * 1024 { break; }
    }

    // LOGOUT.
    let _ = imap_cmd(&mut tls, stack, "A9", "LOGOUT", &mut buf, now);
    tls.close(stack);

    // Parse the full message.
    parse_full_message(uid, &all_data)
}

// ---------------------------------------------------------------------------
// IMAP response parsers
// ---------------------------------------------------------------------------

/// Parse the EXISTS count from a SELECT response.
fn parse_exists(resp: &str) -> Option<u32> {
    for line in resp.lines() {
        let line = line.trim();
        // e.g. "* 42 EXISTS"
        if line.starts_with("* ") && line.ends_with(" EXISTS") {
            let num_part = line.strip_prefix("* ")?.strip_suffix(" EXISTS")?;
            return num_part.trim().parse().ok();
        }
    }
    None
}

/// Parse FETCH header responses into EmailSummary items.
fn parse_fetch_headers(data: &str) -> Vec<EmailSummary> {
    let mut summaries = Vec::new();
    let mut current_uid: u32 = 0;
    let mut current_from = String::new();
    let mut current_subject = String::new();
    let mut current_date = String::new();
    let mut current_seen = false;
    let mut in_header_block = false;

    for line in data.lines() {
        let trimmed = line.trim();

        // Start of a FETCH response: * N FETCH (UID xxx FLAGS (...) ...)
        if trimmed.starts_with("* ") && trimmed.contains("FETCH") {
            // Save previous if we had one.
            if current_uid > 0 {
                summaries.push(EmailSummary {
                    uid: current_uid,
                    from: core::mem::take(&mut current_from),
                    subject: core::mem::take(&mut current_subject),
                    date: core::mem::take(&mut current_date),
                    seen: current_seen,
                });
            }

            // Parse UID.
            if let Some(uid_start) = trimmed.find("UID ") {
                let after = &trimmed[uid_start + 4..];
                let uid_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
                current_uid = uid_str.parse().unwrap_or(0);
            }

            // Parse FLAGS.
            current_seen = trimmed.contains("\\Seen");
            in_header_block = true;
        }
        // Header lines inside FETCH.
        else if in_header_block {
            let lower = trimmed.to_lowercase();
            if lower.starts_with("from:") {
                current_from = trimmed[5..].trim().into();
            } else if lower.starts_with("subject:") {
                current_subject = trimmed[8..].trim().into();
            } else if lower.starts_with("date:") {
                current_date = trimmed[5..].trim().into();
            } else if trimmed == ")" {
                in_header_block = false;
            }
        }
    }

    // Don't forget the last one.
    if current_uid > 0 {
        summaries.push(EmailSummary {
            uid: current_uid,
            from: current_from,
            subject: current_subject,
            date: current_date,
            seen: current_seen,
        });
    }

    summaries
}

/// Parse a full FETCH BODY[] response into an EmailMessage.
fn parse_full_message(uid: u32, data: &str) -> Result<EmailMessage, String> {
    let mut from = String::new();
    let mut to = String::new();
    let mut subject = String::new();
    let mut date = String::new();
    let mut body = String::new();
    let mut attachments = Vec::new();
    let mut in_headers = true;
    let mut past_fetch_line = false;
    let mut content_type = String::new();
    let mut is_multipart = false;
    let mut boundary = String::new();

    for line in data.lines() {
        let trimmed = line.trim();

        // Skip IMAP wrapper lines.
        if !past_fetch_line {
            if trimmed.starts_with("* ") && trimmed.contains("FETCH") {
                past_fetch_line = true;
            }
            continue;
        }

        // End of IMAP response.
        if trimmed.starts_with("A3 ") {
            break;
        }

        if in_headers {
            if trimmed.is_empty() {
                in_headers = false;
                continue;
            }
            let lower = trimmed.to_lowercase();
            if lower.starts_with("from:") {
                from = trimmed[5..].trim().into();
            } else if lower.starts_with("to:") {
                to = trimmed[3..].trim().into();
            } else if lower.starts_with("subject:") {
                subject = trimmed[8..].trim().into();
            } else if lower.starts_with("date:") {
                date = trimmed[5..].trim().into();
            } else if lower.starts_with("content-type:") {
                content_type = trimmed[13..].trim().into();
                if content_type.contains("multipart") {
                    is_multipart = true;
                    if let Some(b_start) = content_type.find("boundary=") {
                        let after = &content_type[b_start + 9..];
                        let bnd = after.trim_start_matches('"');
                        let end = bnd.find('"').unwrap_or(bnd.len());
                        boundary = bnd[..end].into();
                    }
                }
            }
        } else {
            // Body section.
            if is_multipart && !boundary.is_empty() {
                if trimmed.starts_with("--") && trimmed.contains(&*boundary) {
                    continue;
                }
                let lower = trimmed.to_lowercase();
                if lower.starts_with("content-type:") {
                    if lower.contains("name=") || lower.contains("attachment") {
                        if let Some(name_start) = lower.find("name=") {
                            let after = &trimmed[name_start + 5..];
                            let fname = after.trim_matches('"').trim_matches('\'');
                            let end = fname.find(';').unwrap_or(fname.len());
                            attachments.push(fname[..end].into());
                        }
                    }
                    continue;
                }
                if lower.starts_with("content-transfer-encoding:") || lower.starts_with("content-disposition:") {
                    continue;
                }
            }
            if trimmed != ")" {
                body.push_str(line);
                body.push('\n');
            }
        }
    }

    Ok(EmailMessage {
        uid,
        from,
        to,
        subject,
        date,
        body,
        attachments,
    })
}

// ---------------------------------------------------------------------------
// Shell command handler
// ---------------------------------------------------------------------------

/// Handle `mail` shell commands. Returns output string.
///
/// - `mail` — list inbox
/// - `mail read <uid>` — read a message
/// - `mail send <to> <subject> [body...]` — send an email
/// - `mail config <smtp> <imap> <user> <pass>` — configure account
/// - `mail status` — show current config
pub fn handle_command(
    args: &str,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> String {
    let parts: Vec<&str> = args.split_whitespace().collect();

    if parts.is_empty() {
        // `mail` — list inbox.
        return list_inbox_cmd(stack, now);
    }

    match parts[0] {
        "read" => {
            if parts.len() < 2 {
                return "Usage: mail read <uid>\n".into();
            }
            let uid: u32 = match parts[1].parse() {
                Ok(u) => u,
                Err(_) => return format!("Invalid UID: {}\n", parts[1]),
            };
            read_message_cmd(stack, uid, now)
        }

        "send" => {
            if parts.len() < 3 {
                return "Usage: mail send <to> <subject> [body...]\n\
                        Example: mail send user@example.com Hello This is the body text\n".into();
            }
            let to_addr = parts[1];
            let subject = parts[2];
            let body_text = if parts.len() > 3 {
                parts[3..].join(" ")
            } else {
                String::from("(no body)")
            };
            send_email_cmd(stack, to_addr, subject, &body_text, now)
        }

        "config" => {
            if parts.len() < 5 {
                return "Usage: mail config <smtp_server> <imap_server> <user> <pass>\n\
                        Example: mail config smtp.gmail.com imap.gmail.com user@gmail.com mypassword\n".into();
            }
            let smtp = parts[1];
            let imap = parts[2];
            let user = parts[3];
            let pass = parts[4];
            set_config(EmailConfig {
                smtp_server: smtp.into(),
                imap_server: imap.into(),
                user: user.into(),
                pass: pass.into(),
                from_addr: user.into(),
            });
            format!("Email configured: SMTP={}, IMAP={}, user={}\n", smtp, imap, user)
        }

        "status" => {
            match get_config() {
                Some(cfg) => format!(
                    "Email configuration:\n  SMTP: {}\n  IMAP: {}\n  User: {}\n  From: {}\n",
                    cfg.smtp_server, cfg.imap_server, cfg.user, cfg.from_addr
                ),
                None => "No email account configured. Use: mail config <smtp> <imap> <user> <pass>\n".into(),
            }
        }

        "help" | "--help" | "-h" => {
            "ClaudioOS Mail Client\n\
             \n\
             Commands:\n\
             \x20 mail              List inbox (most recent 20 messages)\n\
             \x20 mail read <uid>   Read a specific message by UID\n\
             \x20 mail send <to> <subject> [body...]   Send an email\n\
             \x20 mail config <smtp> <imap> <user> <pass>   Configure account\n\
             \x20 mail status       Show current configuration\n\
             \x20 mail help         Show this help\n".into()
        }

        _ => format!("Unknown mail command: {}. Try 'mail help'.\n", parts[0]),
    }
}

fn list_inbox_cmd(stack: &mut NetworkStack, now: fn() -> Instant) -> String {
    let config = match get_config() {
        Some(c) => c,
        None => return "No email configured. Use: mail config <smtp> <imap> <user> <pass>\n".into(),
    };

    match fetch_inbox(stack, &config.imap_server, &config.user, &config.pass, 20, now) {
        Ok(msgs) => {
            if msgs.is_empty() {
                return "Inbox is empty.\n".into();
            }
            let mut out = format!("Inbox ({} messages):\n", msgs.len());
            out.push_str("  UID   Flags  From                           Subject                        Date\n");
            out.push_str("  ────────────────────────────────────────────────────────────────────────────────\n");
            for msg in &msgs {
                let flag = if msg.seen { "  " } else { "N " };
                let from_trunc = if msg.from.len() > 30 {
                    format!("{}...", &msg.from[..27])
                } else {
                    format!("{:<30}", msg.from)
                };
                let subj_trunc = if msg.subject.len() > 30 {
                    format!("{}...", &msg.subject[..27])
                } else {
                    format!("{:<30}", msg.subject)
                };
                let date_trunc = if msg.date.len() > 20 {
                    String::from(&msg.date[..20])
                } else {
                    msg.date.clone()
                };
                out.push_str(&format!(
                    "  {:<5} {}  {}  {}  {}\n",
                    msg.uid, flag, from_trunc, subj_trunc, date_trunc
                ));
            }
            out
        }
        Err(e) => format!("Failed to fetch inbox: {}\n", e),
    }
}

fn read_message_cmd(stack: &mut NetworkStack, uid: u32, now: fn() -> Instant) -> String {
    let config = match get_config() {
        Some(c) => c,
        None => return "No email configured. Use: mail config <smtp> <imap> <user> <pass>\n".into(),
    };

    match fetch_message(stack, &config.imap_server, &config.user, &config.pass, uid, now) {
        Ok(msg) => {
            let mut out = String::new();
            out.push_str(&format!("From:    {}\n", msg.from));
            out.push_str(&format!("To:      {}\n", msg.to));
            out.push_str(&format!("Subject: {}\n", msg.subject));
            out.push_str(&format!("Date:    {}\n", msg.date));
            if !msg.attachments.is_empty() {
                out.push_str(&format!("Attachments: {}\n", msg.attachments.join(", ")));
            }
            out.push_str("────────────────────────────────────────────\n");
            if msg.body.len() > 16384 {
                out.push_str(&msg.body[..16384]);
                out.push_str(&format!("\n\n... (truncated, {} bytes total)\n", msg.body.len()));
            } else {
                out.push_str(&msg.body);
            }
            out
        }
        Err(e) => format!("Failed to read message UID {}: {}\n", uid, e),
    }
}

fn send_email_cmd(
    stack: &mut NetworkStack,
    to: &str,
    subject: &str,
    body: &str,
    now: fn() -> Instant,
) -> String {
    let config = match get_config() {
        Some(c) => c,
        None => return "No email configured. Use: mail config <smtp> <imap> <user> <pass>\n".into(),
    };

    match send_email(
        stack,
        &config.smtp_server,
        &config.user,
        &config.pass,
        &config.from_addr,
        to,
        subject,
        body,
        now,
    ) {
        Ok(msg) => format!("{}\n", msg),
        Err(e) => format!("Failed to send email: {}\n", e),
    }
}

// ---------------------------------------------------------------------------
// Agent tool interface
// ---------------------------------------------------------------------------

/// Handle the `send_email` agent tool call.
///
/// Input JSON: { "to": "...", "subject": "...", "body": "..." }
pub fn tool_send_email(
    input: &str,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> String {
    let to = extract_json_str(input, "to").unwrap_or_default();
    let subject = extract_json_str(input, "subject").unwrap_or_default();
    let body = extract_json_str(input, "body").unwrap_or_default();

    if to.is_empty() || subject.is_empty() {
        return "Error: 'to' and 'subject' fields are required.".into();
    }

    let config = match get_config() {
        Some(c) => c,
        None => return "Error: no email account configured. Use 'mail config' first.".into(),
    };

    match send_email(
        stack,
        &config.smtp_server,
        &config.user,
        &config.pass,
        &config.from_addr,
        &to,
        &subject,
        &body,
        now,
    ) {
        Ok(msg) => msg,
        Err(e) => format!("Error sending email: {}", e),
    }
}

/// Handle the `check_email` agent tool call.
///
/// Input JSON: { "action": "list" | "read", "uid": N, "count": N }
pub fn tool_check_email(
    input: &str,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> String {
    let action = extract_json_str(input, "action").unwrap_or_else(|| String::from("list"));
    let uid = extract_json_num(input, "uid").unwrap_or(0) as u32;
    let count = extract_json_num(input, "count").unwrap_or(10) as usize;

    let config = match get_config() {
        Some(c) => c,
        None => return "Error: no email account configured. Use 'mail config' first.".into(),
    };

    match &*action {
        "list" => {
            match fetch_inbox(stack, &config.imap_server, &config.user, &config.pass, count, now) {
                Ok(msgs) => {
                    if msgs.is_empty() {
                        return "Inbox is empty.".into();
                    }
                    let mut out = format!("Inbox ({} messages):\n", msgs.len());
                    for msg in &msgs {
                        let flag = if msg.seen { " " } else { "N" };
                        out.push_str(&format!(
                            "[{}] UID:{} From:{} Subject:{} Date:{}\n",
                            flag, msg.uid, msg.from, msg.subject, msg.date
                        ));
                    }
                    out
                }
                Err(e) => format!("Error fetching inbox: {}", e),
            }
        }

        "read" => {
            if uid == 0 {
                return "Error: 'uid' field required for read action.".into();
            }
            match fetch_message(stack, &config.imap_server, &config.user, &config.pass, uid, now) {
                Ok(msg) => {
                    format!(
                        "From: {}\nTo: {}\nSubject: {}\nDate: {}\n\n{}",
                        msg.from, msg.to, msg.subject, msg.date, msg.body
                    )
                }
                Err(e) => format!("Error reading message: {}", e),
            }
        }

        _ => format!("Unknown action '{}'. Use 'list' or 'read'.", action),
    }
}

// ---------------------------------------------------------------------------
// Minimal JSON field extraction (no serde)
// ---------------------------------------------------------------------------

fn extract_json_str(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let start = json.find(&needle)?;
    let after_key = &json[start + needle.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_ws = after_colon.trim_start();
    if after_ws.starts_with('"') {
        let content = &after_ws[1..];
        let mut result = String::new();
        let mut chars = content.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(result),
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        match escaped {
                            'n' => result.push('\n'),
                            't' => result.push('\t'),
                            '"' => result.push('"'),
                            '\\' => result.push('\\'),
                            _ => {
                                result.push('\\');
                                result.push(escaped);
                            }
                        }
                    }
                }
                _ => result.push(c),
            }
        }
        Some(result)
    } else {
        None
    }
}

fn extract_json_num(json: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{}\"", key);
    let start = json.find(&needle)?;
    let after_key = &json[start + needle.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_ws = after_colon.trim_start();
    let num_str: String = after_ws.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
    num_str.parse().ok()
}
