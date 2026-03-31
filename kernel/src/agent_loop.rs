//! Full agent loop: user input -> API call -> tool execution -> repeat.
//!
//! Implements the conversational tool-use cycle against the Anthropic Messages
//! API.  Uses the TLS proxy on the host (10.0.2.2:8443) for HTTPS, since
//! native TLS is still WIP.
//!
//! # Two entry points
//!
//! - [`run_agent`]: standalone serial-only interactive loop (reads keyboard,
//!   prints to serial). Used before the dashboard is available.
//! - [`run_tool_loop`]: the core send/tool/resend cycle. Called by the
//!   dashboard's `submit_input` after the user's message is already in the
//!   conversation. Returns the final assistant text (or error).

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};

use claudio_agent::{AgentSession, AgentState};
use claudio_api::messages::{
    ContentBlock, Message, MessageContent, MessagesRequest, MessagesResponse,
    Role, ToolDefinition,
};
use claudio_api::tools::{
    builtin_tool_definitions, execute_tool, extract_tool_calls_from_response,
};
use claudio_net::NetworkStack;

use crate::keyboard;

/// Ephemeral local port counter — each TCP connection needs a unique local port.
static LOCAL_PORT: AtomicU16 = AtomicU16::new(52000);

/// Maximum number of consecutive tool-use rounds before we force-stop.
/// Prevents runaway loops if the model keeps requesting tools.
const MAX_TOOL_ROUNDS: usize = 20;

// ---------------------------------------------------------------------------
// Public API: core tool-use loop (used by dashboard)
// ---------------------------------------------------------------------------

/// Outcome of a single tool-use cycle.
pub enum ToolLoopOutcome {
    /// Model produced a final text response.
    Text(String),
    /// An error occurred (serialization, network, parse, or too many rounds).
    Error(String),
}

/// Information about a tool call that was executed, for UI display.
pub struct ToolCallInfo {
    pub name: String,
    pub summary: String,
    pub result_preview: String,
    pub is_error: bool,
}

/// Run the API-call + tool-execution loop for a session whose conversation
/// already contains the latest user message.
///
/// This is the core function the dashboard calls after `session.handle_input()`.
/// It:
/// 1. Builds a `MessagesRequest` with tool definitions.
/// 2. Sends it to the API via the TLS proxy.
/// 3. If the model responds with `tool_use`, executes each tool, records
///    results in the session, and loops back to step 1.
/// 4. If the model responds with text (or `end_turn`), returns the text.
///
/// The `on_tool_call` callback is invoked for each executed tool so the
/// caller can update the UI (e.g. write to a pane).
pub fn run_tool_loop(
    session: &mut AgentSession,
    stack: &mut NetworkStack,
    api_key: &str,
    now: fn() -> claudio_net::Instant,
    mut on_tool_call: impl FnMut(&ToolCallInfo),
) -> ToolLoopOutcome {
    let tool_defs = builtin_tool_definitions();
    let mut tool_rounds = 0;

    loop {
        session.state = AgentState::Thinking;

        // Build the Messages API request from conversation state.
        let request = build_request(session, &tool_defs);

        // Serialize to JSON.
        let body_bytes = match request.to_json() {
            Ok(b) => b,
            Err(e) => {
                log::error!("[agent_loop] failed to serialize request: {}", e);
                session.set_error();
                return ToolLoopOutcome::Error(format!("failed to build request: {}", e));
            }
        };

        // Send to API via TLS proxy.
        let response_bytes = match send_via_proxy(stack, api_key, &body_bytes, now) {
            Ok(bytes) => bytes,
            Err(e) => {
                log::error!("[agent_loop] API request failed: {}", e);
                session.set_error();
                return ToolLoopOutcome::Error(e);
            }
        };

        // Parse the HTTP response body.
        let api_response = match parse_api_response(&response_bytes) {
            Ok(resp) => resp,
            Err(e) => {
                log::error!("[agent_loop] failed to parse API response: {}", e);
                session.set_error();
                return ToolLoopOutcome::Error(format!("bad API response: {}", e));
            }
        };

        // Record token usage.
        session.record_usage(
            api_response.usage.input_tokens,
            api_response.usage.output_tokens,
        );
        log::info!(
            "[agent_loop] tokens: {} in / {} out (total: {} in / {} out)",
            api_response.usage.input_tokens,
            api_response.usage.output_tokens,
            session.conversation.total_input_tokens,
            session.conversation.total_output_tokens,
        );

        // ── Check if the model wants to use tools ───────────────────
        if api_response.needs_tool_use() {
            tool_rounds += 1;
            if tool_rounds > MAX_TOOL_ROUNDS {
                log::warn!(
                    "[agent_loop] exceeded max tool rounds ({})",
                    MAX_TOOL_ROUNDS
                );
                session.set_error();
                return ToolLoopOutcome::Error(format!(
                    "too many tool calls ({} rounds)",
                    MAX_TOOL_ROUNDS
                ));
            }

            // Extract tool calls.
            let tool_calls = extract_tool_calls_from_response(&api_response);
            log::info!(
                "[agent_loop] model requested {} tool call(s) (round {})",
                tool_calls.len(),
                tool_rounds
            );

            // Also capture any text the model produced alongside tool calls.
            let text_alongside = api_response.text();

            // Record the assistant's tool_use blocks in the conversation.
            // The assistant message may contain both text and tool_use blocks.
            // We need to record the full assistant response as-is.
            for tc in &tool_calls {
                let input_str = serde_json::to_string(&tc.input).unwrap_or_default();
                session.handle_tool_use(tc.id.clone(), tc.name.clone(), input_str, 0);
            }

            // Execute each tool and collect results.
            for tc in &tool_calls {
                session.state = AgentState::ToolExecuting;

                let result = execute_tool(tc);

                // Build summary for UI callback.
                let summary = if let Some(path) = tc.input.get("path").and_then(|v| v.as_str()) {
                    format!("\"{}\"", path)
                } else if let Some(cmd) = tc.input.get("command").and_then(|v| v.as_str()) {
                    format!("\"{}\"", cmd)
                } else {
                    String::from("...")
                };

                let preview = if result.content.len() > 120 {
                    format!("{}...", &result.content[..120])
                } else {
                    result.content.clone()
                };

                on_tool_call(&ToolCallInfo {
                    name: tc.name.clone(),
                    summary,
                    result_preview: preview,
                    is_error: result.is_error,
                });

                // Feed result back into conversation.
                session.handle_tool_result(
                    result.tool_use_id.clone(),
                    result.content.clone(),
                    result.is_error,
                    0,
                );
            }

            // Continue the loop — send tool results back to the API.
            log::info!(
                "[agent_loop] sent {} tool result(s) back, continuing loop",
                tool_calls.len()
            );
            continue;
        }

        // ── Model produced a final text response ────────────────────
        let text = api_response.text();
        session.handle_response_text(text.clone(), 0);
        return ToolLoopOutcome::Text(text);
    }
}

// ---------------------------------------------------------------------------
// Public API: standalone serial-only interactive loop
// ---------------------------------------------------------------------------

/// Run the interactive agent loop on serial I/O (no framebuffer dashboard).
///
/// This is a self-contained loop that reads keyboard input, sends messages
/// to the API with tool definitions, executes tools, and prints responses
/// to the serial console.
pub async fn run_agent(
    stack: &mut NetworkStack,
    api_key: &str,
    now: fn() -> claudio_net::Instant,
) {
    log::info!("[agent_loop] starting interactive agent loop");
    log::info!("[agent_loop] type a message and press Enter to chat with Claude");
    log::info!("[agent_loop] tools: file_read, file_write, list_directory, execute_command");
    log::info!("[agent_loop] commands: /quit, /clear, /tokens");

    // Create a session with id=0, pane=0 (single-agent for now).
    let mut session = AgentSession::new(0, String::from("main"), 0);

    loop {
        // ── Step 1: Read user input from keyboard ───────────────────
        session.state = AgentState::WaitingForInput;
        crate::serial_print!("\n> ");

        let user_input = read_line().await;
        let trimmed = user_input.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Special commands.
        if trimmed == "/quit" || trimmed == "/exit" {
            log::info!("[agent_loop] user requested exit");
            crate::serial_print!("\n[agent] goodbye!\n");
            break;
        }
        if trimmed == "/clear" {
            session = AgentSession::new(0, String::from("main"), 0);
            crate::serial_print!("\n[agent] conversation cleared\n");
            continue;
        }
        if trimmed == "/tokens" {
            crate::serial_print!(
                "\n[agent] tokens used: {} in / {} out\n",
                session.conversation.total_input_tokens,
                session.conversation.total_output_tokens
            );
            continue;
        }

        // Add user message to conversation (timestamp 0 — no RTC yet).
        session.handle_input(String::from(trimmed), 0);

        // ── Step 2-6: API call + tool loop ──────────────────────────
        crate::serial_print!("\n[thinking...]\n");

        let outcome = run_tool_loop(
            &mut session,
            stack,
            api_key,
            now,
            |info| {
                // Print tool calls to serial.
                if info.is_error {
                    crate::serial_print!(
                        "\n[tool] {}({}) -> ERROR: {}\n",
                        info.name,
                        info.summary,
                        info.result_preview
                    );
                } else {
                    crate::serial_print!(
                        "\n[tool] {}({}) -> {}\n",
                        info.name,
                        info.summary,
                        info.result_preview
                    );
                }
            },
        );

        match outcome {
            ToolLoopOutcome::Text(text) => {
                crate::serial_print!("\n{}\n", text);
            }
            ToolLoopOutcome::Error(e) => {
                crate::serial_print!("\n[error] {}\n", e);
            }
        }
    }

    log::info!("[agent_loop] agent loop exited");
}

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

/// Build a [`MessagesRequest`] from the current session state.
fn build_request(session: &AgentSession, tool_defs: &[ToolDefinition]) -> MessagesRequest {
    let messages = session_to_api_messages(session);

    MessagesRequest {
        model: session.model.clone(),
        max_tokens: session.max_tokens,
        messages,
        stream: false,
        system: Some(session.conversation.system_prompt.clone()),
        tools: Some(tool_defs.to_vec()),
    }
}

/// Convert the agent session's conversation history into api-client [`Message`]s.
fn session_to_api_messages(session: &AgentSession) -> Vec<Message> {
    let mut messages = Vec::new();

    for conv_msg in &session.conversation.messages {
        let role = match conv_msg.role {
            claudio_agent::ConversationRole::User => Role::User,
            claudio_agent::ConversationRole::Assistant => Role::Assistant,
        };

        let blocks: Vec<ContentBlock> = conv_msg
            .content
            .iter()
            .map(|block| match block {
                claudio_agent::ContentBlock::Text { text } => ContentBlock::Text {
                    text: text.clone(),
                },
                claudio_agent::ContentBlock::ToolUse { id, name, input } => {
                    // The agent crate stores input as a JSON string; parse it
                    // back to a Value for the API.
                    let input_value: serde_json::Value =
                        serde_json::from_str(input).unwrap_or(serde_json::Value::Object(
                            serde_json::Map::new(),
                        ));
                    ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input_value,
                    }
                }
                claudio_agent::ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                },
            })
            .collect();

        messages.push(Message {
            role,
            content: MessageContent::Blocks(blocks),
        });
    }

    messages
}

// ---------------------------------------------------------------------------
// Network: send request via TLS proxy
// ---------------------------------------------------------------------------

/// Send an API request through the TLS proxy running on the host.
///
/// The proxy listens on 10.0.2.2:8443 and forwards HTTPS to api.anthropic.com.
/// We send a plain HTTP/1.1 request; the proxy handles TLS upstream.
pub fn send_via_proxy(
    stack: &mut NetworkStack,
    api_key: &str,
    body: &[u8],
    now: fn() -> claudio_net::Instant,
) -> Result<Vec<u8>, String> {
    let proxy_ip = claudio_net::Ipv4Address::new(10, 0, 2, 2);
    let local_port = LOCAL_PORT.fetch_add(1, Ordering::Relaxed);

    // Build HTTP request.
    let http_req = claudio_net::http::HttpRequest::post(
        "api.anthropic.com",
        "/v1/messages",
        body.to_vec(),
    )
    .header("Content-Type", "application/json")
    .header("x-api-key", api_key)
    .header("anthropic-version", "2023-06-01")
    .header("Connection", "close");

    let req_bytes = http_req.to_bytes();
    log::debug!(
        "[agent_loop] sending {} byte request to proxy",
        req_bytes.len()
    );

    // TCP connect to proxy.
    let handle =
        claudio_net::tls::tcp_connect(stack, proxy_ip, 8443, local_port, || now()).map_err(
            |e| {
                format!(
                    "proxy connect failed: {:?} — run: python tools/tls-proxy.py 8443",
                    e
                )
            },
        )?;

    // Send request.
    if let Err(e) = claudio_net::tls::tcp_send(stack, handle, &req_bytes, || now()) {
        claudio_net::tls::tcp_close(stack, handle);
        return Err(format!("send failed: {:?}", e));
    }

    // Read response.
    let mut buf = vec![0u8; 65536];
    let mut total = 0;
    for _ in 0..1000 {
        match claudio_net::tls::tcp_recv(stack, handle, &mut buf[total..], || now()) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                // Check if we have a complete HTTP response.
                if claudio_net::http::HttpResponse::parse(&buf[..total]).is_ok() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    claudio_net::tls::tcp_close(stack, handle);

    if total == 0 {
        return Err(String::from("no response from proxy"));
    }

    Ok(buf[..total].to_vec())
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// Parse the raw HTTP response bytes into a [`MessagesResponse`].
pub fn parse_api_response(raw: &[u8]) -> Result<MessagesResponse, String> {
    // Find the body after \r\n\r\n.
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| String::from("no HTTP header terminator found"))?;

    let headers = &raw[..header_end];
    let body_raw = &raw[header_end + 4..];

    // Check HTTP status.
    let hdr_str = core::str::from_utf8(headers).unwrap_or("");
    let status = hdr_str
        .split(' ')
        .nth(1)
        .unwrap_or("0")
        .parse::<u16>()
        .unwrap_or(0);

    if status != 200 {
        let body_decoded =
            claudio_net::http::decode_chunked(body_raw).unwrap_or_else(|_| body_raw.to_vec());
        let body_text = core::str::from_utf8(&body_decoded).unwrap_or("<binary>");
        return Err(format!("HTTP {}: {}", status, body_text));
    }

    // Decode chunked transfer encoding if present.
    let body_decoded =
        claudio_net::http::decode_chunked(body_raw).unwrap_or_else(|_| body_raw.to_vec());

    log::debug!("[agent_loop] response body: {} bytes", body_decoded.len());

    // Deserialize the JSON body.
    MessagesResponse::from_json(&body_decoded).map_err(|e| format!("JSON parse error: {}", e))
}

// ---------------------------------------------------------------------------
// Keyboard input (for serial-only mode)
// ---------------------------------------------------------------------------

/// Read a line of input from the keyboard, echoing to serial.
///
/// Blocks (async) until the user presses Enter. Supports backspace.
async fn read_line() -> String {
    let stream = keyboard::ScancodeStream::new();
    let mut line = String::new();

    loop {
        let key = stream.next_key().await;
        match key {
            pc_keyboard::DecodedKey::Unicode(c) => {
                if c == '\n' || c == '\r' {
                    crate::serial_print!("\n");
                    return line;
                } else if c == '\x08' || c == '\x7f' {
                    // Backspace / Delete
                    if !line.is_empty() {
                        line.pop();
                        crate::serial_print!("\x08 \x08");
                    }
                } else if !c.is_control() {
                    line.push(c);
                    crate::serial_print!("{}", c);
                }
            }
            pc_keyboard::DecodedKey::RawKey(_) => {
                // Ignore raw keys (arrows, function keys, etc.)
            }
        }
    }
}
