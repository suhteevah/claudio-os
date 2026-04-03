//! Full agent loop: user input -> API call -> tool execution -> repeat.
//!
//! Implements the conversational tool-use cycle against the Anthropic Messages
//! API.  Uses native TLS via `claudio_net::https_request()` to connect directly
//! to api.anthropic.com:443.
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
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use claudio_agent::{AgentSession, AgentState};
use claudio_api::messages::{
    ContentBlock, Message, MessageContent, MessagesRequest, MessagesResponse,
    Role, ToolDefinition,
};
use claudio_api::tools::{
    builtin_tool_definitions, execute_tool, extract_tool_calls_from_response,
    build_server_port,
};
use claudio_net::NetworkStack;

use crate::keyboard;

/// RNG seed counter — each TLS connection needs a unique seed.
pub(crate) static RNG_SEED: AtomicU64 = AtomicU64::new(1);

/// Maximum number of consecutive tool-use rounds before we force-stop.
/// Prevents runaway loops if the model keeps requesting tools.
const MAX_TOOL_ROUNDS: usize = 20;

// ---------------------------------------------------------------------------
// Build server compile handler
// ---------------------------------------------------------------------------

/// Global network stack reference for the compile handler.
///
/// The compile handler is a plain `fn` pointer (not a closure), so it cannot
/// capture the network stack. We store a pointer here during init.
///
/// SAFETY: Set once during single-threaded boot, read during tool execution.
/// The network stack outlives all agent sessions. Using spin::Once ensures
/// safe one-time initialization without `static mut`.
static BUILD_STACK: spin::Once<*mut NetworkStack> = spin::Once::new();
// SAFETY: The raw pointer is only dereferenced in the single-threaded executor
// context. We need Send+Sync for the static.
struct SendNetPtr(*mut NetworkStack);
unsafe impl Send for SendNetPtr {}
unsafe impl Sync for SendNetPtr {}

static BUILD_NOW_FN: spin::Once<fn() -> claudio_net::Instant> = spin::Once::new();

/// Initialize the compile_rust tool handler.
///
/// Must be called once during kernel init, before any agent sessions start.
/// Registers a function pointer that the api-client's `execute_tool` can call
/// to reach the host-side build server.
///
/// # Safety
/// - `stack` must remain valid for the entire runtime.
/// - Must be called once from a single thread during init.
pub unsafe fn init_compile_handler(
    stack: *mut NetworkStack,
    now: fn() -> claudio_net::Instant,
) {
    BUILD_STACK.call_once(|| stack);
    BUILD_NOW_FN.call_once(|| now);
    claudio_api::tools::set_compile_handler(compile_handler);
    log::info!("[agent_loop] compile_rust handler registered (build server port {})", build_server_port());
}

/// The compile handler called by `execute_tool("compile_rust")`.
///
/// Sends an HTTP POST to the build server at 10.0.2.2:{BUILD_SERVER_PORT}
/// and returns the raw HTTP response bytes.
fn compile_handler(body: &[u8]) -> Result<Vec<u8>, String> {
    let stack_ptr = BUILD_STACK
        .get()
        .ok_or_else(|| String::from("network stack not initialized"))?;
    let stack = unsafe {
        stack_ptr.as_mut()
            .ok_or_else(|| String::from("network stack pointer is null"))?
    };
    let now = *BUILD_NOW_FN
        .get()
        .ok_or_else(|| String::from("time function not initialized"))?;

    let port = build_server_port();

    // Build HTTP request to the build server.
    let http_req = claudio_net::http::HttpRequest::post(
        "10.0.2.2",        // QEMU SLIRP host gateway
        "/compile",
        body.to_vec(),
    )
    .header("Content-Type", "application/json")
    .header("Connection", "close");

    let req_bytes = http_req.to_bytes();
    log::debug!(
        "[compile] sending {} bytes to build server at 10.0.2.2:{}",
        req_bytes.len(),
        port
    );

    // Connect via TCP (plain HTTP, no TLS needed for local build server).
    let local_port = RNG_SEED.fetch_add(1, Ordering::Relaxed) as u16 + 55000;
    let server_ip = claudio_net::Ipv4Address::new(10, 0, 2, 2);

    let handle = claudio_net::tls::tcp_connect(stack, server_ip, port, local_port, now)
        .map_err(|e| {
            alloc::format!(
                "build server connect failed: {:?}. Run: python tools/build-server.py",
                e
            )
        })?;

    // Send the request.
    claudio_net::tls::tcp_send(stack, handle, &req_bytes, now).map_err(|e| {
        claudio_net::tls::tcp_close(stack, handle);
        alloc::format!("build server send failed: {:?}", e)
    })?;

    // Read the response.
    let mut buf = alloc::vec![0u8; 65536];
    let mut total = 0;
    for _ in 0..500 {
        match claudio_net::tls::tcp_recv(stack, handle, &mut buf[total..], now) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if total >= buf.len() - 1024 {
                    break; // Buffer nearly full
                }
            }
            Err(_) => break,
        }
    }
    claudio_net::tls::tcp_close(stack, handle);

    if total == 0 {
        return Err(String::from(
            "no response from build server. Is it running? python tools/build-server.py",
        ));
    }

    log::debug!("[compile] received {} bytes from build server", total);
    Ok(buf[..total].to_vec())
}

// ---------------------------------------------------------------------------
// VFS + command tool handlers
// ---------------------------------------------------------------------------

/// Initialize the file_read, file_write, list_directory, and execute_command
/// tool handlers. Called once during kernel init alongside `init_compile_handler`.
///
/// # Safety
/// Must be called once from a single thread during init.
pub unsafe fn init_tool_handlers() {
    claudio_api::tools::set_file_read_handler(file_read_handler);
    claudio_api::tools::set_file_write_handler(file_write_handler);
    claudio_api::tools::set_list_directory_handler(list_directory_handler);
    claudio_api::tools::set_execute_command_handler(execute_command_handler);
    log::info!("[agent_loop] VFS + command tool handlers registered");
}

/// file_read handler — reads from the kernel's VFS (fs-persist).
///
/// Currently stubs with a log message since FAT32 is not yet mounted.
fn file_read_handler(path: &str) -> Result<String, String> {
    log::info!("[tool_handler] file_read: {}", path);
    // TODO: delegate to fs-persist once FAT32 is mounted
    Err(String::from("VFS not mounted — FAT32 filesystem not yet available"))
}

/// file_write handler — writes to the kernel's VFS (fs-persist).
///
/// Currently stubs with a log message since FAT32 is not yet mounted.
fn file_write_handler(path: &str, content: &str) -> Result<(), String> {
    log::info!("[tool_handler] file_write: {} ({} bytes)", path, content.len());
    // TODO: delegate to fs-persist once FAT32 is mounted
    Err(String::from("VFS not mounted — FAT32 filesystem not yet available"))
}

/// list_directory handler — lists directory contents from the kernel's VFS.
///
/// Currently stubs with a log message since FAT32 is not yet mounted.
fn list_directory_handler(path: &str) -> Result<String, String> {
    log::info!("[tool_handler] list_directory: {}", path);
    // TODO: delegate to fs-persist once FAT32 is mounted
    Err(String::from("VFS not mounted — FAT32 filesystem not yet available"))
}

/// execute_command handler — executes a command via the kernel's shell.
///
/// ClaudioOS has no POSIX shell. This handler interprets a limited set of
/// built-in commands. Currently stubs with a log message.
fn execute_command_handler(command: &str) -> Result<String, String> {
    log::info!("[tool_handler] execute_command: {}", command);
    // TODO: implement a minimal command interpreter for built-in ops
    Err(String::from("shell not available — ClaudioOS has no process model yet"))
}

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

/// Run the API-call + tool-execution loop **with real-time streaming**.
///
/// Same as [`run_tool_loop`] but uses SSE streaming: each text token is
/// delivered to `on_token` as it arrives from the API, so the dashboard
/// can render Claude's response in real-time instead of waiting for the
/// full response.
///
/// `on_token` is called with each text fragment. `on_tool_call` is called
/// for each tool execution (same as the non-streaming version).
pub fn run_tool_loop_streaming(
    session: &mut AgentSession,
    stack: &mut NetworkStack,
    api_key: &str,
    now: fn() -> claudio_net::Instant,
    mut on_token: impl FnMut(&str),
    mut on_tool_call: impl FnMut(&ToolCallInfo),
) -> ToolLoopOutcome {
    let tool_defs = builtin_tool_definitions();
    let mut tool_rounds = 0;

    loop {
        session.state = AgentState::Thinking;

        // Build the Messages API request with stream=true.
        let mut request = build_request(session, &tool_defs);
        request.stream = true;

        let body_bytes = match request.to_json() {
            Ok(b) => b,
            Err(e) => {
                log::error!("[agent_loop] failed to serialize request: {}", e);
                session.set_error();
                return ToolLoopOutcome::Error(format!("failed to build request: {}", e));
            }
        };

        // Try streaming path first.
        let stream_result = send_streaming(stack, api_key, &body_bytes, now, |chunk| {
            on_token(chunk);
        });

        match stream_result {
            Ok(result) => {
                let text = result.text.clone();

                // Record usage (streaming doesn't give us exact counts easily,
                // estimate 0 and let the non-streaming fallback path handle it).
                session.record_usage(0, 0);

                // For streaming, we get plain text — no tool_use detection.
                // Record it and return.
                session.handle_response_text(text.clone(), 0);
                session.state = AgentState::WaitingForInput;
                return ToolLoopOutcome::Text(text);
            }
            Err(e) => {
                // Streaming failed — fall back to non-streaming path.
                log::warn!("[agent_loop] streaming failed, falling back: {}", e);

                // Re-build without stream=true.
                let request = build_request(session, &tool_defs);
                let body_bytes = match request.to_json() {
                    Ok(b) => b,
                    Err(e) => {
                        session.set_error();
                        return ToolLoopOutcome::Error(format!("failed to build request: {}", e));
                    }
                };

                let response_bytes = match send_via_https(stack, api_key, &body_bytes, now) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        session.set_error();
                        return ToolLoopOutcome::Error(e);
                    }
                };

                let api_response = match parse_api_response(&response_bytes) {
                    Ok(resp) => resp,
                    Err(e) => {
                        session.set_error();
                        return ToolLoopOutcome::Error(format!("bad API response: {}", e));
                    }
                };

                session.record_usage(
                    api_response.usage.input_tokens,
                    api_response.usage.output_tokens,
                );

                if api_response.needs_tool_use() {
                    tool_rounds += 1;
                    if tool_rounds > MAX_TOOL_ROUNDS {
                        session.set_error();
                        return ToolLoopOutcome::Error(format!(
                            "too many tool calls ({} rounds)",
                            MAX_TOOL_ROUNDS
                        ));
                    }

                    let tool_calls = extract_tool_calls_from_response(&api_response);
                    for tc in &tool_calls {
                        let input_str = serde_json::to_string(&tc.input).unwrap_or_default();
                        session.handle_tool_use(tc.id.clone(), tc.name.clone(), input_str, 0);
                    }
                    for tc in &tool_calls {
                        session.state = AgentState::ToolExecuting;
                        let result = execute_tool(tc);
                        let summary = tool_call_summary(tc);
                        let preview = tool_result_preview(&result.content);
                        on_tool_call(&ToolCallInfo {
                            name: tc.name.clone(),
                            summary,
                            result_preview: preview,
                            is_error: result.is_error,
                        });
                        session.handle_tool_result(
                            result.tool_use_id.clone(),
                            result.content.clone(),
                            result.is_error,
                            0,
                        );
                    }
                    continue;
                }

                let text = api_response.text();
                // Emit text token-by-token for the fallback path too.
                on_token(&text);
                session.handle_response_text(text.clone(), 0);
                return ToolLoopOutcome::Text(text);
            }
        }
    }
}

/// Build a one-line summary of a tool call for UI display.
fn tool_call_summary(tc: &claudio_api::tools::ToolCall) -> String {
    if let Some(path) = tc.input.get("path").and_then(|v| v.as_str()) {
        format!("\"{}\"", path)
    } else if let Some(cmd) = tc.input.get("command").and_then(|v| v.as_str()) {
        format!("\"{}\"", cmd)
    } else if let Some(src) = tc.input.get("source").and_then(|v| v.as_str()) {
        let lines = src.lines().count();
        format!("{} bytes, {} lines", src.len(), lines)
    } else {
        String::from("...")
    }
}

/// Truncate a tool result for UI preview.
fn tool_result_preview(content: &str) -> String {
    if content.len() > 120 {
        format!("{}...", &content[..120])
    } else {
        String::from(content)
    }
}

/// Run the API-call + tool-execution loop for a session whose conversation
/// already contains the latest user message.
///
/// This is the core function the dashboard calls after `session.handle_input()`.
/// It:
/// 1. Builds a `MessagesRequest` with tool definitions.
/// 2. Sends it to the API via native TLS to api.anthropic.com:443.
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

        // Send to API via native TLS.
        let response_bytes = match send_via_https(stack, api_key, &body_bytes, now) {
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
                } else if let Some(src) = tc.input.get("source").and_then(|v| v.as_str()) {
                    let lines = src.lines().count();
                    format!("{} bytes, {} lines", src.len(), lines)
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
    log::info!("[agent_loop] tools: file_read, file_write, list_directory, execute_command, compile_rust, execute_python");
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
// Network: send request via native TLS
// ---------------------------------------------------------------------------

/// Authentication mode for API calls.
#[derive(Clone)]
pub enum AuthMode {
    /// Traditional API key → api.anthropic.com
    ApiKey(String),
    /// claude.ai session cookie + org UUID → claude.ai Max subscription
    ClaudeAi {
        session_cookie: String,
        org_id: String,
        conv_id: String,
    },
}

/// Global auth mode — set during init, read during agent loops.
/// Protected by spin::Mutex for thread-safe access and mutation
/// (conversations module needs to update conv_id).
pub(crate) static AUTH_MODE: spin::Mutex<Option<AuthMode>> = spin::Mutex::new(None);

/// Set the authentication mode. Call once during boot.
///
/// # Safety
/// Safe to call — uses spin::Mutex which handles synchronization.
/// Kept as `unsafe fn` to preserve API compatibility with callers.
pub unsafe fn set_auth_mode(mode: AuthMode) {
    let mut guard = AUTH_MODE.lock();
    *guard = Some(mode);
}

/// Get a clone of the current auth mode.
pub fn auth_mode() -> Option<AuthMode> {
    AUTH_MODE.lock().clone()
}

/// Get a reference to the auth mode via the lock. For callers that need
/// to read fields without cloning.
pub fn with_auth_mode<R>(f: impl FnOnce(Option<&AuthMode>) -> R) -> R {
    let guard = AUTH_MODE.lock();
    f(guard.as_ref())
}

/// Send an API request — routes to either api.anthropic.com or claude.ai
/// depending on the configured auth mode.
pub fn send_via_https(
    stack: &mut NetworkStack,
    api_key: &str,
    body: &[u8],
    now: fn() -> claudio_net::Instant,
) -> Result<Vec<u8>, String> {
    // Check if we have claude.ai auth mode
    if let Some(AuthMode::ClaudeAi { ref session_cookie, ref org_id, ref conv_id }) = auth_mode() {
        return send_via_claude_ai(stack, session_cookie, org_id, conv_id, body, now);
    }

    // Fall back to api.anthropic.com with API key
    send_via_api_key(stack, api_key, body, now)
}

/// Send via api.anthropic.com with API key (original path).
fn send_via_api_key(
    stack: &mut NetworkStack,
    api_key: &str,
    body: &[u8],
    now: fn() -> claudio_net::Instant,
) -> Result<Vec<u8>, String> {
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
    log::debug!("[agent] sending {} bytes to api.anthropic.com", req_bytes.len());

    let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);
    match claudio_net::https_request(stack, "api.anthropic.com", 443, &req_bytes, now, seed) {
        Ok(resp) => {
            log::debug!("[agent] received {} bytes from api.anthropic.com", resp.len());
            Ok(resp)
        }
        Err(e) => Err(alloc::format!("api.anthropic.com request failed: {:?}", e)),
    }
}

/// Send via claude.ai using session cookie (Max subscription, unlimited).
///
/// Converts the Messages API request body into the claude.ai completion format
/// and sends to `/api/organizations/{org}/chat_conversations/{conv}/completion`.
fn send_via_claude_ai(
    stack: &mut NetworkStack,
    session_cookie: &str,
    org_id: &str,
    conv_id: &str,
    body: &[u8],
    now: fn() -> claudio_net::Instant,
) -> Result<Vec<u8>, String> {
    // Extract the user's latest message from the Messages API request body.
    // The body is a MessagesRequest JSON. We need to pull out the last user message
    // and send it as a claude.ai completion request.
    let body_str = core::str::from_utf8(body).unwrap_or("{}");

    // Find the last user message text
    let prompt = extract_last_user_message(body_str);
    log::info!("[claude.ai] sending: {}...", &prompt[..prompt.len().min(80)]);

    let selected_model = crate::model_select::claude_ai_model_id();
    let claude_body = alloc::format!(
        r#"{{"prompt":"{}","timezone":"America/New_York","attachments":[],"files":[],"model":"{}","rendering_mode":"messages"}}"#,
        prompt.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"),
        selected_model
    );

    let path = alloc::format!(
        "/api/organizations/{}/chat_conversations/{}/completion",
        org_id, conv_id
    );

    let http_req = claudio_net::http::HttpRequest::post(
        "claude.ai", &path, claude_body.into_bytes(),
    )
    .header("Content-Type", "application/json")
    .header("Cookie", session_cookie)
    .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:136.0) Gecko/20100101 Firefox/136.0")
    .header("Accept", "text/event-stream")
    .header("Origin", "https://claude.ai")
    .header("Referer", "https://claude.ai/new")
    .header("Connection", "close");

    let req_bytes = http_req.to_bytes();
    log::debug!("[claude.ai] sending {} bytes", req_bytes.len());

    let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);
    let resp = claudio_net::https_request(stack, "claude.ai", 443, &req_bytes, now, seed)
        .map_err(|e| alloc::format!("claude.ai request failed: {:?}", e))?;

    log::debug!("[claude.ai] received {} bytes", resp.len());

    // Parse SSE response and convert to a fake Messages API response
    // so the existing parsing code works unchanged
    let resp_str = core::str::from_utf8(&resp).unwrap_or("");
    let mut text = String::new();
    for line in resp_str.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if data.contains("\"text_delta\"") {
                if let Some(s) = data.find("\"text\":\"") {
                    let rest = &data[s + 8..];
                    if let Some(e) = rest.find('"') {
                        text.push_str(&rest[..e]);
                    }
                }
            }
        }
    }

    if text.is_empty() {
        // Check for error in response
        if let Some(pos) = resp_str.find("\r\n\r\n") {
            let body = &resp_str[pos + 4..];
            if body.contains("\"error\"") || body.contains("rate_limit") {
                return Err(alloc::format!("claude.ai error: {}", &body[..body.len().min(300)]));
            }
        }
        return Err(String::from("empty response from claude.ai"));
    }

    // Build a fake Messages API JSON response so parse_api_response works
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    let resp_model = crate::model_select::claude_ai_model_id();
    let json_body = alloc::format!(
        r#"{{"id":"msg_claude_ai","type":"message","role":"assistant","content":[{{"type":"text","text":"{}"}}],"model":"{}","stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":0,"output_tokens":0}}}}"#,
        escaped, resp_model
    );
    let mut fake_response = alloc::string::String::new();
    fake_response.push_str("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n");
    fake_response.push_str(&json_body);

    Ok(fake_response.into_bytes())
}

// ---------------------------------------------------------------------------
// Streaming: send request and read SSE tokens incrementally
// ---------------------------------------------------------------------------

/// Send an API request via streaming SSE, calling `on_token` for each text
/// chunk as it arrives.  Routes to either api.anthropic.com or claude.ai.
pub fn send_streaming(
    stack: &mut NetworkStack,
    api_key: &str,
    body: &[u8],
    now: fn() -> claudio_net::Instant,
    on_token: impl FnMut(&str),
) -> Result<crate::streaming::StreamResult, String> {
    if let Some(AuthMode::ClaudeAi { ref session_cookie, ref org_id, ref conv_id }) = auth_mode() {
        return send_streaming_claude_ai(stack, session_cookie, org_id, conv_id, body, now, on_token);
    }
    send_streaming_api_key(stack, api_key, body, now, on_token)
}

/// Streaming via api.anthropic.com with API key.
fn send_streaming_api_key(
    stack: &mut NetworkStack,
    api_key: &str,
    body: &[u8],
    now: fn() -> claudio_net::Instant,
    on_token: impl FnMut(&str),
) -> Result<crate::streaming::StreamResult, String> {
    let http_req = claudio_net::http::HttpRequest::post(
        "api.anthropic.com",
        "/v1/messages",
        body.to_vec(),
    )
    .header("Content-Type", "application/json")
    .header("x-api-key", api_key)
    .header("anthropic-version", "2023-06-01")
    .header("Accept", "text/event-stream")
    .header("Connection", "close");

    let req_bytes = http_req.to_bytes();
    log::debug!("[streaming] sending {} bytes to api.anthropic.com", req_bytes.len());

    let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);

    // DNS resolve.
    let ip = claudio_net::dns::resolve(stack, "api.anthropic.com", || now())
        .map_err(|e| alloc::format!("DNS failed: {:?}", e))?;

    // TLS connect (returns a TlsStream we can read incrementally).
    let mut tls = claudio_net::TlsStream::connect(stack, ip, 443, "api.anthropic.com", now, seed)
        .map_err(|e| alloc::format!("TLS connect failed: {:?}", e))?;

    // Send the HTTP request.
    tls.send(stack, &req_bytes, now)
        .map_err(|e| alloc::format!("TLS send failed: {:?}", e))?;

    // Stream the response.
    let result = crate::streaming::stream_sse_response(&mut tls, stack, now, on_token)?;

    tls.close(stack);
    Ok(result)
}

/// Streaming via claude.ai using session cookie.
fn send_streaming_claude_ai(
    stack: &mut NetworkStack,
    session_cookie: &str,
    org_id: &str,
    conv_id: &str,
    body: &[u8],
    now: fn() -> claudio_net::Instant,
    on_token: impl FnMut(&str),
) -> Result<crate::streaming::StreamResult, String> {
    let body_str = core::str::from_utf8(body).unwrap_or("{}");
    let prompt = extract_last_user_message(body_str);
    log::info!("[streaming] claude.ai: {}...", &prompt[..prompt.len().min(80)]);

    let selected_model = crate::model_select::claude_ai_model_id();
    let claude_body = alloc::format!(
        r#"{{"prompt":"{}","timezone":"America/New_York","attachments":[],"files":[],"model":"{}","rendering_mode":"messages"}}"#,
        prompt.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"),
        selected_model
    );

    let path = alloc::format!(
        "/api/organizations/{}/chat_conversations/{}/completion",
        org_id, conv_id
    );

    let http_req = claudio_net::http::HttpRequest::post(
        "claude.ai", &path, claude_body.into_bytes(),
    )
    .header("Content-Type", "application/json")
    .header("Cookie", session_cookie)
    .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:136.0) Gecko/20100101 Firefox/136.0")
    .header("Accept", "text/event-stream")
    .header("Origin", "https://claude.ai")
    .header("Referer", "https://claude.ai/new")
    .header("Connection", "close");

    let req_bytes = http_req.to_bytes();
    log::debug!("[streaming] sending {} bytes to claude.ai", req_bytes.len());

    let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);

    let ip = claudio_net::dns::resolve(stack, "claude.ai", || now())
        .map_err(|e| alloc::format!("DNS failed: {:?}", e))?;

    let mut tls = claudio_net::TlsStream::connect(stack, ip, 443, "claude.ai", now, seed)
        .map_err(|e| alloc::format!("TLS connect failed: {:?}", e))?;

    tls.send(stack, &req_bytes, now)
        .map_err(|e| alloc::format!("TLS send failed: {:?}", e))?;

    let result = crate::streaming::stream_sse_response(&mut tls, stack, now, on_token)?;

    tls.close(stack);
    Ok(result)
}

/// Extract the last user message text from a Messages API request JSON.
fn extract_last_user_message(body: &str) -> String {
    // Find the last "role":"user" message and extract its text
    // Simple approach: find last occurrence of "role":"user" then find "text":"
    let mut last_text = String::from("Hello");

    let mut search_from = 0;
    while let Some(pos) = body[search_from..].find("\"role\":\"user\"") {
        let abs_pos = search_from + pos;
        // Find the text content after this role marker
        if let Some(text_pos) = body[abs_pos..].find("\"text\":\"") {
            let text_start = abs_pos + text_pos + 8;
            // Find the closing quote (handling escaped quotes)
            let rest = &body[text_start..];
            let mut end = 0;
            let mut escaped = false;
            for (i, c) in rest.char_indices() {
                if escaped { escaped = false; continue; }
                if c == '\\' { escaped = true; continue; }
                if c == '"' { end = i; break; }
            }
            if end > 0 {
                last_text = String::from(&rest[..end]);
            }
        }
        search_from = abs_pos + 13; // skip past "role":"user"
    }

    last_text
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
