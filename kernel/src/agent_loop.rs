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
use claudio_api::{RetryConfig, RetryDecision, should_retry, parse_http_status, extract_http_headers};
use claudio_llm::{LoadedModel, ModelConfig};
use claudio_net::NetworkStack;
use spin::Mutex;

use crate::keyboard;
use crate::storage;

/// RNG seed counter — each TLS connection needs a unique seed for its
/// handshake nonces.  We use an incrementing counter rather than true
/// randomness because embedded-tls only needs uniqueness, not cryptographic
/// unpredictability, for session key derivation (the actual entropy comes
/// from the TLS key exchange).  Starts at 1 because 0 can cause issues
/// with some PRNG implementations.
pub(crate) static RNG_SEED: AtomicU64 = AtomicU64::new(1);

/// Maximum number of consecutive tool-use rounds before we force-stop.
/// Prevents runaway loops if the model keeps requesting tools endlessly
/// (e.g., recursive file exploration or compilation retry loops).
/// 20 rounds is generous -- most real tasks complete in 3-5 rounds.
const MAX_TOOL_ROUNDS: usize = 20;

// ---------------------------------------------------------------------------
// Spin-wait helper for retry back-off
// ---------------------------------------------------------------------------

/// Spin-wait for `ms` milliseconds using the PIT-based `now` function.
///
/// Uses `core::hint::spin_loop()` to yield to the CPU while waiting, which
/// reduces power consumption on bare metal (the CPU can enter a low-power
/// state between spins).
fn spin_wait_ms(now: fn() -> claudio_net::Instant, ms: u64) {
    let start = now().total_millis();
    let target = start + ms as i64;
    while now().total_millis() < target {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// Build server compile handler
// ---------------------------------------------------------------------------

/// Global network stack reference for the compile handler.
///
/// The compile handler is a plain `fn` pointer (not a closure), so it cannot
/// capture the network stack. We store a pointer here during init.
///
/// SAFETY: Set once during single-threaded boot, read during tool execution.
/// The network stack outlives all agent sessions. Using spin::Once with a
/// Send+Sync wrapper ensures safe one-time initialization without `static mut`.
struct SendNetPtr(*mut NetworkStack);
unsafe impl Send for SendNetPtr {}
unsafe impl Sync for SendNetPtr {}

static BUILD_STACK: spin::Once<SendNetPtr> = spin::Once::new();
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
    BUILD_STACK.call_once(|| SendNetPtr(stack));
    BUILD_NOW_FN.call_once(|| now);
    claudio_api::tools::set_compile_handler(compile_handler);
    log::info!("[agent_loop] compile_rust handler registered (build server port {})", build_server_port());
}

/// The compile handler called by `execute_tool("compile_rust")`.
///
/// Sends an HTTP POST to the build server at 10.0.2.2:{BUILD_SERVER_PORT}
/// and returns the raw HTTP response bytes.
fn compile_handler(body: &[u8]) -> Result<Vec<u8>, String> {
    let send_ptr = BUILD_STACK
        .get()
        .ok_or_else(|| String::from("network stack not initialized"))?;
    let stack = unsafe {
        send_ptr.0.as_mut()
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
    claudio_api::tools::set_local_compile_handler(local_compile_handler);
    claudio_api::tools::set_local_model_handler(local_model_handler);
    log::info!("[agent_loop] VFS + command + local compiler + local model tool handlers registered");
}

/// Local Rust compiler handler — compiles via rustc-lite (Cranelift JIT).
fn local_compile_handler(source: &str) -> Result<String, String> {
    log::info!("[tool_handler] local compile_rust: {} bytes", source.len());

    match claudio_rustc::check(source) {
        Ok(errors) => {
            if errors.is_empty() {
                match claudio_rustc::compile(source) {
                    Ok(output) => {
                        let mut result = alloc::string::String::from("Compilation successful!\n");
                        for f in &output.functions {
                            result.push_str(&alloc::format!(
                                "  fn {} -> {} bytes of x86_64 machine code\n",
                                f.name, f.code_size
                            ));
                        }
                        for d in &output.diagnostics {
                            result.push_str(d);
                            result.push('\n');
                        }
                        Ok(result)
                    }
                    Err(e) => Err(e),
                }
            } else {
                let mut result = alloc::string::String::from("Type check errors:\n");
                for e in &errors {
                    result.push_str("  ");
                    result.push_str(e);
                    result.push('\n');
                }
                Err(result)
            }
        }
        Err(e) => Err(e),
    }
}

/// Global registry for the local GGUF-loaded model. Set once via
/// `init_local_model_from_bytes()` (e.g. from a build-time `include_bytes!`
/// or once FAT32 reads land), then `local_model_handler` consults it.
static LOCAL_MODEL: Mutex<Option<LoadedModel>> = Mutex::new(None);

/// Parse a GGUF buffer and install it as the local model. The byte slice
/// can be dropped after this call returns — weights are copied/dequantized
/// into owned storage by `LoadedModel::from_bytes`.
pub fn init_local_model_from_bytes(data: &[u8]) -> Result<(), String> {
    log::info!("[llm] init_local_model_from_bytes: {} bytes", data.len());
    let cfg = ModelConfig::default();
    let loaded = LoadedModel::from_bytes(data, &cfg)?;
    *LOCAL_MODEL.lock() = Some(loaded);
    log::info!("[llm] local model installed");
    Ok(())
}

/// Local model handler — runs GGUF model inference on bare metal.
fn local_model_handler(prompt: &str, max_tokens: usize, temperature: f32) -> Result<String, String> {
    log::info!("[tool_handler] run_local_model: {} chars, max_tokens={}, temp={}", prompt.len(), max_tokens, temperature);
    let guard = LOCAL_MODEL.lock();
    let model = guard.as_ref().ok_or_else(|| {
        String::from("no GGUF model loaded — call init_local_model_from_bytes() at boot, or download a model to /models/ once FAT32 is mounted")
    })?;
    let cfg = ModelConfig {
        temperature,
        ..ModelConfig::default()
    };
    model.generate(prompt, max_tokens, &cfg)
}

/// file_read handler — reads a file through the kernel VFS and returns the
/// contents as a (lossy) UTF-8 string.
fn file_read_handler(path: &str) -> Result<String, String> {
    log::info!("[tool_handler] file_read: {}", path);
    storage::with_vfs(|vfs| {
        // stat to get size
        let info = vfs
            .stat(path)
            .map_err(|e| alloc::format!("stat failed: {}", e))?;
        if info.file_type != claudio_vfs::FileType::File {
            return Err(alloc::format!("{} is not a regular file", path));
        }
        let size = info.size as usize;
        // open
        let fd = vfs
            .open(path, claudio_vfs::OpenFlags::read_only())
            .map_err(|e| alloc::format!("open failed: {}", e))?;
        let mut buf = alloc::vec![0u8; size];
        let mut total = 0usize;
        while total < size {
            let n = vfs
                .read(fd, &mut buf[total..])
                .map_err(|e| alloc::format!("read failed: {}", e))?;
            if n == 0 {
                break;
            }
            total += n;
        }
        let _ = vfs.close(fd);
        buf.truncate(total);
        Ok(String::from_utf8(buf).unwrap_or_else(|e| {
            String::from_utf8_lossy(&e.into_bytes()).into_owned()
        }))
    })
}

/// file_write handler — creates/truncates the file through the kernel VFS and
/// writes the content bytes.
fn file_write_handler(path: &str, content: &str) -> Result<(), String> {
    log::info!("[tool_handler] file_write: {} ({} bytes)", path, content.len());
    storage::with_vfs(|vfs| {
        let fd = vfs
            .open(path, claudio_vfs::OpenFlags::create_truncate())
            .map_err(|e| alloc::format!("open failed: {}", e))?;
        let bytes = content.as_bytes();
        let mut written = 0usize;
        while written < bytes.len() {
            let n = vfs
                .write(fd, &bytes[written..])
                .map_err(|e| alloc::format!("write failed: {}", e))?;
            if n == 0 {
                return Err(String::from("short write"));
            }
            written += n;
        }
        let _ = vfs.close(fd);
        Ok(())
    })
}

/// list_directory handler — enumerates directory contents through the VFS and
/// formats them as `<type> <size> <name>` lines.
fn list_directory_handler(path: &str) -> Result<String, String> {
    log::info!("[tool_handler] list_directory: {}", path);
    storage::with_vfs(|vfs| {
        let entries = vfs
            .readdir(path)
            .map_err(|e| alloc::format!("readdir failed: {}", e))?;
        let mut out = alloc::string::String::new();
        use core::fmt::Write;
        for entry in entries {
            let kind = if entry.is_dir() { "d" } else { "f" };
            let _ = writeln!(out, "{} {:>10} {}", kind, entry.size, entry.name);
        }
        Ok(out)
    })
}

/// execute_command handler — minimal built-in command interpreter.
///
/// ClaudioOS has no POSIX shell and no process model. This handler supports a
/// small set of built-ins backed by the VFS: `ls`, `cat`, `echo`, `pwd`,
/// `mkdir`. Unknown commands return an error.
fn execute_command_handler(command: &str) -> Result<String, String> {
    log::info!("[tool_handler] execute_command: {}", command);
    let trimmed = command.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    match cmd {
        "pwd" => Ok(alloc::string::String::from("/")),
        "echo" => Ok(alloc::format!("{}\n", rest)),
        "ls" => {
            let path = if rest.is_empty() { "/" } else { rest };
            list_directory_handler(path)
        }
        "cat" => {
            if rest.is_empty() {
                return Err(alloc::string::String::from("cat: missing path"));
            }
            file_read_handler(rest)
        }
        "mkdir" => {
            if rest.is_empty() {
                return Err(alloc::string::String::from("mkdir: missing path"));
            }
            storage::with_vfs(|vfs| {
                vfs.mkdir(rest)
                    .map_err(|e| alloc::format!("mkdir failed: {}", e))?;
                Ok(alloc::format!("created {}\n", rest))
            })
        }
        "" => Ok(alloc::string::String::new()),
        other => Err(alloc::format!("unknown command: {}", other)),
    }
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
///
/// Retries on 429 (rate limit), 500, 502, 503, and 529 (overloaded) with
/// exponential back-off. Honours the server's `Retry-After` header when present.
fn send_via_api_key(
    stack: &mut NetworkStack,
    api_key: &str,
    body: &[u8],
    now: fn() -> claudio_net::Instant,
) -> Result<Vec<u8>, String> {
    let retry_config = RetryConfig::default();
    let mut attempt: u32 = 0;

    loop {
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
        log::debug!("[agent] sending {} bytes to api.anthropic.com (attempt {})", req_bytes.len(), attempt);

        let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);
        let resp = match claudio_net::https_request(stack, "api.anthropic.com", 443, &req_bytes, now, seed) {
            Ok(r) => r,
            Err(e) => {
                // Network-level failure (DNS, TLS, TCP) — not retryable at
                // this layer; the connection didn't even complete.
                return Err(alloc::format!("api.anthropic.com request failed: {:?}", e));
            }
        };

        log::debug!("[agent] received {} bytes from api.anthropic.com", resp.len());

        // Check HTTP status for retryable errors.
        let status = parse_http_status(&resp);
        if status >= 200 && status < 300 {
            return Ok(resp);
        }

        // Non-success status — check if retryable.
        let headers = extract_http_headers(&resp);
        match should_retry(&retry_config, attempt, status, headers) {
            RetryDecision::RetryAfter(delay_ms) => {
                log::warn!(
                    "[agent] HTTP {} from api.anthropic.com — retrying in {}ms (attempt {}/{})",
                    status, delay_ms, attempt + 1, retry_config.max_retries
                );
                spin_wait_ms(now, delay_ms);
                attempt += 1;
                continue;
            }
            RetryDecision::GiveUp => {
                let body_preview = extract_body_preview(&resp);
                log::error!(
                    "[agent] API request failed with HTTP {} after {} attempt(s), giving up: {}",
                    status, attempt + 1, body_preview
                );
                return Err(alloc::format!("HTTP {}: {}", status, body_preview));
            }
        }
    }
}

/// Extract a short preview of the HTTP response body for error logging.
fn extract_body_preview(raw: &[u8]) -> String {
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(raw.len());
    let body = &raw[(header_end + 4).min(raw.len())..];
    let body_decoded =
        claudio_net::http::decode_chunked(body).unwrap_or_else(|_| body.to_vec());
    let text = core::str::from_utf8(&body_decoded).unwrap_or("<binary>");
    if text.len() > 300 {
        alloc::format!("{}...", &text[..300])
    } else {
        String::from(text)
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
///
/// Retries on 429, 500, 502, 503, 529 with exponential back-off.
/// The SSE reader (`stream_sse_response`) already parses the HTTP status
/// and returns an error string starting with "HTTP <code>:" on non-200.
/// We parse that status out and decide whether to retry.
fn send_streaming_api_key(
    stack: &mut NetworkStack,
    api_key: &str,
    body: &[u8],
    now: fn() -> claudio_net::Instant,
    mut on_token: impl FnMut(&str),
) -> Result<crate::streaming::StreamResult, String> {
    let retry_config = RetryConfig::default();
    let mut attempt: u32 = 0;

    loop {
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
        log::debug!("[streaming] sending {} bytes to api.anthropic.com (attempt {})", req_bytes.len(), attempt);

        let seed = RNG_SEED.fetch_add(1, Ordering::Relaxed);

        // DNS resolve.
        let ip = claudio_net::dns::resolve(stack, "api.anthropic.com", || now())
            .map_err(|e| alloc::format!("DNS failed: {:?}", e))?;

        // TLS connect.
        let mut tls = claudio_net::TlsStream::connect(stack, ip, 443, "api.anthropic.com", now, seed)
            .map_err(|e| alloc::format!("TLS connect failed: {:?}", e))?;

        // Send the HTTP request.
        tls.send(stack, &req_bytes, now)
            .map_err(|e| alloc::format!("TLS send failed: {:?}", e))?;

        // Stream the response. The SSE reader handles headers internally
        // and returns an error like "HTTP 429: ..." on non-200 status.
        match crate::streaming::stream_sse_response(&mut tls, stack, now, &mut on_token) {
            Ok(result) => {
                tls.close(stack);
                return Ok(result);
            }
            Err(e) => {
                tls.close(stack);

                // Parse the HTTP status from the error message (format: "HTTP <code>: ...").
                let status = parse_streaming_error_status(&e);

                if status > 0 {
                    match should_retry(&retry_config, attempt, status, "") {
                        RetryDecision::RetryAfter(delay_ms) => {
                            log::warn!(
                                "[streaming] HTTP {} from api.anthropic.com — retrying in {}ms (attempt {}/{})",
                                status, delay_ms, attempt + 1, retry_config.max_retries
                            );
                            spin_wait_ms(now, delay_ms);
                            attempt += 1;
                            continue;
                        }
                        RetryDecision::GiveUp => {
                            log::error!(
                                "[streaming] API request failed with HTTP {} after {} attempt(s)",
                                status, attempt + 1
                            );
                            return Err(e);
                        }
                    }
                }

                // Not an HTTP status error (DNS, TLS, timeout, etc.) — not retryable.
                return Err(e);
            }
        }
    }
}

/// Parse an HTTP status code from a streaming error message.
///
/// The SSE reader produces errors like "HTTP 429: Too Many Requests...".
/// Returns the status code, or 0 if the error doesn't match that pattern.
fn parse_streaming_error_status(err: &str) -> u16 {
    if let Some(rest) = err.strip_prefix("HTTP ") {
        // Take characters until non-digit.
        let code_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        code_str.parse::<u16>().unwrap_or(0)
    } else {
        0
    }
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
