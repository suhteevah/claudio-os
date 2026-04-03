//! Incremental SSE streaming reader for the Anthropic Messages API.
//!
//! Instead of buffering the entire HTTP response and parsing SSE events
//! after the fact, [`StreamingReader`] reads TLS data chunk-by-chunk and
//! invokes a callback for every text token as it arrives. This lets the
//! dashboard render Claude's response in real-time.
//!
//! Supports both the api.anthropic.com streaming protocol and claude.ai's
//! SSE format.  Handles all SSE event types:
//!   - `message_start`, `content_block_start`, `content_block_delta` (text_delta),
//!     `content_block_stop`, `message_delta`, `message_stop`
//!
//! # Timeout
//! If no data arrives for 30 seconds the stream is aborted.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use claudio_net::{Instant, NetworkStack, TlsStream};

/// 30-second read timeout (in milliseconds).
const STREAM_TIMEOUT_MS: i64 = 30_000;

/// Result of a completed streaming session.
pub struct StreamResult {
    /// The full accumulated response text.
    pub text: String,
    /// Whether we saw a `message_stop` event (clean finish).
    pub finished: bool,
    /// Number of text chunks delivered via callback.
    pub chunks_delivered: usize,
}

/// Reads an SSE stream from an open TLS connection, calling `on_token`
/// for every text fragment as it arrives.
///
/// The connection must already have the HTTP request sent; this function
/// only reads the response.
///
/// Returns the fully accumulated text plus metadata.
pub fn stream_sse_response(
    tls: &mut TlsStream,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
    mut on_token: impl FnMut(&str),
) -> Result<StreamResult, String> {
    let mut buf = [0u8; 4096];
    let mut leftover = Vec::<u8>::new();
    let mut accumulated_text = String::new();
    let mut chunks_delivered: usize = 0;
    let mut finished = false;
    let mut headers_done = false;
    let mut last_data_time = now().total_millis();

    loop {
        // Check timeout.
        let elapsed = now().total_millis() - last_data_time;
        if elapsed > STREAM_TIMEOUT_MS {
            log::warn!("[streaming] timeout after {}ms with no data", elapsed);
            return Err(String::from("streaming timeout: no data for 30s"));
        }

        // Try to read a chunk from TLS.
        let n = match tls.recv(stack, &mut buf, now) {
            Ok(0) => {
                // EOF — connection closed.
                log::debug!("[streaming] EOF after {} chunks", chunks_delivered);
                break;
            }
            Ok(n) => n,
            Err(e) => {
                // Any error after we've received data — treat as EOF.
                if chunks_delivered > 0 || !accumulated_text.is_empty() {
                    log::debug!("[streaming] TLS error after {} chunks, treating as EOF: {:?}", chunks_delivered, e);
                    break;
                }
                return Err(alloc::format!("TLS error during streaming: {:?}", e));
            }
        };

        last_data_time = now().total_millis();
        leftover.extend_from_slice(&buf[..n]);

        // Skip HTTP headers if we haven't passed them yet.
        if !headers_done {
            if let Some(pos) = find_header_end(&leftover) {
                // Check status code before continuing.
                let header_slice = &leftover[..pos];
                if let Ok(hdr_str) = core::str::from_utf8(header_slice) {
                    let status = hdr_str
                        .split(' ')
                        .nth(1)
                        .and_then(|s| s.parse::<u16>().ok())
                        .unwrap_or(0);
                    if status != 200 {
                        let body = core::str::from_utf8(&leftover[pos + 4..]).unwrap_or("<binary>");
                        return Err(alloc::format!("HTTP {}: {}", status, &body[..body.len().min(300)]));
                    }
                }
                leftover = leftover[pos + 4..].to_vec();
                headers_done = true;
                log::debug!("[streaming] headers parsed, starting SSE processing");
            } else {
                // Haven't received full headers yet, keep reading.
                continue;
            }
        }

        // Process complete lines from the leftover buffer.
        // SSE protocol: events are separated by blank lines.
        // Each line is either "event: <type>", "data: <json>", or empty.
        loop {
            let line_end = match find_line_end(&leftover) {
                Some(pos) => pos,
                None => break, // No complete line yet.
            };

            // Copy the line to an owned String so we can reassign `leftover`.
            let line = {
                let line_bytes = &leftover[..line_end];
                String::from(core::str::from_utf8(line_bytes).unwrap_or(""))
            };

            // Advance past the line + newline character(s).
            let skip = if leftover.get(line_end) == Some(&b'\r')
                && leftover.get(line_end + 1) == Some(&b'\n')
            {
                line_end + 2
            } else {
                line_end + 1
            };
            leftover = leftover[skip..].to_vec();

            // Parse SSE data lines.
            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    finished = true;
                    break;
                }

                // Try to extract text from content_block_delta events.
                if let Some(text_chunk) = extract_text_delta(data) {
                    // Unescape JSON string escapes.
                    let unescaped = unescape_json_str(&text_chunk);
                    if !unescaped.is_empty() {
                        on_token(&unescaped);
                        accumulated_text.push_str(&unescaped);
                        chunks_delivered += 1;
                    }
                }

                // Check for message_stop event type in data.
                if data.contains("\"type\":\"message_stop\"") {
                    finished = true;
                }
            }
        }

        if finished {
            break;
        }
    }

    log::info!(
        "[streaming] complete: {} chars, {} chunks, finished={}",
        accumulated_text.len(),
        chunks_delivered,
        finished
    );

    Ok(StreamResult {
        text: accumulated_text,
        finished,
        chunks_delivered,
    })
}

/// Find the `\r\n\r\n` boundary that separates HTTP headers from body.
fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Find the end of the next line (position of `\n` or `\r`).
fn find_line_end(data: &[u8]) -> Option<usize> {
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' || b == b'\r' {
            return Some(i);
        }
    }
    None
}

/// Extract the text content from a `content_block_delta` SSE data payload.
///
/// Handles both api.anthropic.com format:
///   `{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}`
/// and claude.ai format:
///   `{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}`
///
/// Uses lightweight string scanning (no full JSON parse) for speed.
fn extract_text_delta(data: &str) -> Option<String> {
    // Must be a content_block_delta with text_delta.
    if !data.contains("\"text_delta\"") {
        return None;
    }

    // Find "text":" after "text_delta" — this is the actual text content.
    // We need to find the delta's text field, not the type field.
    let delta_pos = data.find("\"text_delta\"")?;
    let after_delta = &data[delta_pos..];

    // Find "text":"  after the text_delta marker.
    let text_key = after_delta.find("\"text\":\"")?;
    let text_start = text_key + 8; // skip past "text":"
    let rest = &after_delta[text_start..];

    // Find the closing quote, handling escaped quotes.
    let end = find_json_string_end(rest)?;
    Some(String::from(&rest[..end]))
}

/// Find the end of a JSON string value (the closing unescaped `"`).
fn find_json_string_end(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            return Some(i);
        }
    }
    None
}

/// Unescape common JSON string escape sequences.
fn unescape_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    // \uXXXX unicode escape.
                    let mut hex = String::new();
                    for _ in 0..4 {
                        if let Some(h) = chars.next() {
                            hex.push(h);
                        }
                    }
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                        }
                    }
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
