//! Tool use protocol for the Anthropic Messages API.
//!
//! Implements the full tool-use loop:
//! 1. Define tools (via [`ToolSpec`] / [`builtin_tools`])
//! 2. Extract tool calls from API responses ([`extract_tool_calls`])
//! 3. Execute tools ([`execute_tool`])
//! 4. Format results for the next request ([`tool_result_to_content_block`])
//!
//! Actual tool implementations (filesystem, command execution) are stubbed —
//! they depend on kernel subsystems not yet available.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::messages::{ContentBlock, MessageContent, Message, Role, ToolDefinition};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A tool the model can call — describes its name, purpose, and input schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl ToolSpec {
    /// Convert to the wire-format [`ToolDefinition`] used in API requests.
    pub fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }
}

/// A tool call extracted from the model's response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this tool use (from the API).
    pub id: String,
    /// Name of the tool being invoked.
    pub name: String,
    /// Input arguments as a JSON object.
    pub input: Value,
}

/// Result of executing a tool, ready to send back to the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// The `id` of the [`ToolCall`] this result corresponds to.
    pub tool_use_id: String,
    /// Textual content of the result (stdout, file contents, error message, etc.).
    pub content: String,
    /// Whether the tool execution failed.
    pub is_error: bool,
}

// ---------------------------------------------------------------------------
// Extraction from API responses
// ---------------------------------------------------------------------------

/// Extract all `tool_use` blocks from a raw API response JSON value.
///
/// This operates on the raw `serde_json::Value` so it works even if the
/// response doesn't fully deserialize into [`MessagesResponse`].
pub fn extract_tool_calls(response_json: &Value) -> Vec<ToolCall> {
    let mut calls = Vec::new();

    let content = match response_json.get("content") {
        Some(Value::Array(arr)) => arr,
        _ => return calls,
    };

    for block in content {
        let block_type = match block.get("type").and_then(Value::as_str) {
            Some(t) => t,
            None => continue,
        };

        if block_type == "tool_use" {
            let id = block
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
            let input = block.get("input").cloned().unwrap_or(Value::Object(
                serde_json::Map::new(),
            ));

            calls.push(ToolCall { id, name, input });
        }
    }

    calls
}

/// Extract [`ToolCall`]s from a typed [`MessagesResponse`] (convenience wrapper).
pub fn extract_tool_calls_from_response(
    response: &crate::messages::MessagesResponse,
) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    for block in &response.content {
        if let crate::messages::ResponseBlock::ToolUse { id, name, input } = block {
            calls.push(ToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            });
        }
    }
    calls
}

// ---------------------------------------------------------------------------
// Formatting results for the API
// ---------------------------------------------------------------------------

/// Format a [`ToolResult`] as a JSON content block suitable for inclusion in
/// the `content` array of a user message.
///
/// Produces:
/// ```json
/// {
///   "type": "tool_result",
///   "tool_use_id": "...",
///   "content": "...",
///   "is_error": false
/// }
/// ```
pub fn tool_result_to_content_block(result: &ToolResult) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "type".into(),
        Value::String("tool_result".into()),
    );
    map.insert(
        "tool_use_id".into(),
        Value::String(result.tool_use_id.clone()),
    );
    map.insert(
        "content".into(),
        Value::String(result.content.clone()),
    );
    if result.is_error {
        map.insert("is_error".into(), Value::Bool(true));
    }
    Value::Object(map)
}

/// Build a user [`Message`] containing one or more tool results.
///
/// The Anthropic API expects tool results as an array of content blocks in a
/// single user message, one block per tool result.
pub fn tool_results_to_message(results: &[ToolResult]) -> Message {
    let blocks: Vec<ContentBlock> = results
        .iter()
        .map(|r| ContentBlock::ToolResult {
            tool_use_id: r.tool_use_id.clone(),
            content: r.content.clone(),
        })
        .collect();

    Message {
        role: Role::User,
        content: MessageContent::Blocks(blocks),
    }
}

// ---------------------------------------------------------------------------
// Built-in ClaudioOS tools
// ---------------------------------------------------------------------------

/// Return the set of built-in tools that ClaudioOS agents can use.
///
/// These represent the core capabilities exposed to the AI model:
/// file I/O, directory listing, and command execution.
pub fn builtin_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "file_read".into(),
            description: "Read the contents of a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to read"
                    }
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "file_write".into(),
            description: "Write content to a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Destination file path"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write"
                    }
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpec {
            name: "list_directory".into(),
            description: "List files and directories at the given path".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to list"
                    }
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "execute_command".into(),
            description: "Execute a shell command and return its output".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command to execute"
                    }
                },
                "required": ["command"]
            }),
        },
        ToolSpec {
            name: "compile_rust".into(),
            description: "Compile Rust source code via the remote build server. \
                Sends the source to the host-side build server which runs rustc \
                and returns compilation output (errors, warnings). Use this to \
                check if Rust code compiles, see error messages, and iterate on fixes."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Complete Rust source code to compile"
                    },
                    "edition": {
                        "type": "string",
                        "description": "Rust edition (default: 2021)",
                        "enum": ["2015", "2018", "2021", "2024"]
                    },
                    "mode": {
                        "type": "string",
                        "description": "Compilation mode: 'check' (default, fast) or 'build' (full)",
                        "enum": ["check", "build"]
                    }
                },
                "required": ["source"]
            }),
        },
    ]
}

/// Convert the built-in tools to [`ToolDefinition`]s for inclusion in an API request.
pub fn builtin_tool_definitions() -> Vec<ToolDefinition> {
    builtin_tools().iter().map(|t| t.to_definition()).collect()
}

// ---------------------------------------------------------------------------
// Tool execution router
// ---------------------------------------------------------------------------

/// Execute a tool call and return the result.
///
/// Routes to the appropriate handler based on the tool name. Unknown tools
/// return an error result rather than panicking.
pub fn execute_tool(call: &ToolCall) -> ToolResult {
    log::debug!("[tools] executing tool '{}' (id={})", call.name, call.id);

    let result = match call.name.as_str() {
        "file_read" => execute_file_read(call),
        "file_write" => execute_file_write(call),
        "list_directory" => execute_list_dir(call),
        "execute_command" => execute_command(call),
        "compile_rust" => execute_compile_rust(call),
        _ => {
            log::warn!("[tools] unknown tool: {}", call.name);
            return ToolResult {
                tool_use_id: call.id.clone(),
                content: format!("Unknown tool: {}", call.name),
                is_error: true,
            };
        }
    };

    log::debug!(
        "[tools] tool '{}' completed, is_error={}",
        call.name,
        result.is_error
    );
    result
}

// ---------------------------------------------------------------------------
// Individual tool implementations (stubbed)
// ---------------------------------------------------------------------------

/// Helper: extract a required string field from the tool input JSON.
fn get_string_field<'a>(input: &'a Value, field: &str) -> Result<&'a str, String> {
    input
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required field: '{}'", field))
}

fn execute_file_read(call: &ToolCall) -> ToolResult {
    let path = match get_string_field(&call.input, "path") {
        Ok(p) => p,
        Err(e) => {
            return ToolResult {
                tool_use_id: call.id.clone(),
                content: e,
                is_error: true,
            };
        }
    };

    log::info!("[tools] file_read: {}", path);

    // TODO: Implement via fs-persist crate once FAT32 filesystem is available.
    // Will read from the mounted FAT32 partition.
    ToolResult {
        tool_use_id: call.id.clone(),
        content: format!("file_read not yet implemented (requested path: {})", path),
        is_error: true,
    }
}

fn execute_file_write(call: &ToolCall) -> ToolResult {
    let path = match get_string_field(&call.input, "path") {
        Ok(p) => p,
        Err(e) => {
            return ToolResult {
                tool_use_id: call.id.clone(),
                content: e,
                is_error: true,
            };
        }
    };

    let content = match get_string_field(&call.input, "content") {
        Ok(c) => c,
        Err(e) => {
            return ToolResult {
                tool_use_id: call.id.clone(),
                content: e,
                is_error: true,
            };
        }
    };

    log::info!("[tools] file_write: {} ({} bytes)", path, content.len());

    // TODO: Implement via fs-persist crate once FAT32 filesystem is available.
    ToolResult {
        tool_use_id: call.id.clone(),
        content: format!(
            "file_write not yet implemented (requested path: {}, {} bytes)",
            path,
            content.len()
        ),
        is_error: true,
    }
}

fn execute_list_dir(call: &ToolCall) -> ToolResult {
    let path = match get_string_field(&call.input, "path") {
        Ok(p) => p,
        Err(e) => {
            return ToolResult {
                tool_use_id: call.id.clone(),
                content: e,
                is_error: true,
            };
        }
    };

    log::info!("[tools] list_directory: {}", path);

    // TODO: Implement via fs-persist crate once FAT32 filesystem is available.
    ToolResult {
        tool_use_id: call.id.clone(),
        content: format!("list_directory not yet implemented (requested path: {})", path),
        is_error: true,
    }
}

fn execute_command(call: &ToolCall) -> ToolResult {
    let command = match get_string_field(&call.input, "command") {
        Ok(c) => c,
        Err(e) => {
            return ToolResult {
                tool_use_id: call.id.clone(),
                content: e,
                is_error: true,
            };
        }
    };

    log::info!("[tools] execute_command: {}", command);

    // TODO: ClaudioOS has no shell or process model. This will need a custom
    // command interpreter that can run built-in operations (cargo build, git, etc.)
    // via direct kernel-level execution or a minimal shell implementation.
    ToolResult {
        tool_use_id: call.id.clone(),
        content: format!(
            "execute_command not yet implemented (requested command: {})",
            command
        ),
        is_error: true,
    }
}

// ---------------------------------------------------------------------------
// compile_rust — remote compilation via host-side build server
// ---------------------------------------------------------------------------

/// Port the build server listens on (host side).
const BUILD_SERVER_PORT: u16 = 8445;

/// Build a JSON request body for the build server's /compile endpoint.
fn build_compile_request(source: &str, edition: &str, mode: &str) -> Vec<u8> {
    let req = serde_json::json!({
        "source": source,
        "edition": edition,
        "mode": mode,
    });
    serde_json::to_vec(&req).unwrap_or_default()
}

/// Format the build server response into a human-readable string for the agent.
fn format_compile_result(success: bool, stdout: &str, stderr: &str) -> String {
    let mut out = String::new();

    if success {
        out.push_str("Compilation successful!\n");
    } else {
        out.push_str("Compilation FAILED.\n");
    }

    if !stdout.is_empty() {
        out.push_str("\n--- stdout ---\n");
        out.push_str(stdout);
    }

    if !stderr.is_empty() {
        out.push_str("\n--- stderr ---\n");
        out.push_str(stderr);
    }

    if stdout.is_empty() && stderr.is_empty() && success {
        out.push_str("(no warnings)");
    }

    out
}

/// Execute the `compile_rust` tool.
///
/// Sends the Rust source to the host-side build server at 10.0.2.2:8445
/// (QEMU SLIRP gateway) via plain HTTP. The build server runs `rustc` and
/// returns compilation output.
///
/// The actual network call is deferred to the kernel's agent loop, which has
/// access to the network stack. This function builds the request and parses
/// the response. The kernel injects a `COMPILE_RUST_HANDLER` function pointer
/// that performs the TCP I/O.
///
/// If no handler is registered (e.g. in unit tests), falls back to an error.
fn execute_compile_rust(call: &ToolCall) -> ToolResult {
    let source = match get_string_field(&call.input, "source") {
        Ok(s) => s,
        Err(e) => {
            return ToolResult {
                tool_use_id: call.id.clone(),
                content: e,
                is_error: true,
            };
        }
    };

    let edition = call
        .input
        .get("edition")
        .and_then(Value::as_str)
        .unwrap_or("2021");

    let mode = call
        .input
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("check");

    log::info!(
        "[tools] compile_rust: {} bytes, edition={}, mode={}",
        source.len(),
        edition,
        mode
    );

    // Build the JSON body for the build server.
    let body = build_compile_request(source, edition, mode);

    // Check if a compile handler has been registered by the kernel.
    let handler = unsafe { COMPILE_RUST_HANDLER };
    match handler {
        Some(h) => {
            // Call the kernel-provided handler to perform the HTTP request.
            match h(&body) {
                Ok(response_bytes) => parse_compile_response(call, &response_bytes),
                Err(e) => ToolResult {
                    tool_use_id: call.id.clone(),
                    content: format!(
                        "Build server request failed: {}. \
                         Make sure the build server is running: \
                         python tools/build-server.py",
                        e
                    ),
                    is_error: true,
                },
            }
        }
        None => ToolResult {
            tool_use_id: call.id.clone(),
            content: String::from(
                "compile_rust: no network handler registered. \
                 The kernel must call set_compile_handler() at init.",
            ),
            is_error: true,
        },
    }
}

/// Parse the HTTP response from the build server into a ToolResult.
fn parse_compile_response(call: &ToolCall, raw: &[u8]) -> ToolResult {
    // Find body after HTTP headers.
    let body_start = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(0);
    let body = &raw[body_start..];

    let body_str = match core::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => {
            return ToolResult {
                tool_use_id: call.id.clone(),
                content: String::from("build server returned non-UTF8 response"),
                is_error: true,
            };
        }
    };

    // Parse JSON response.
    let parsed: Result<Value, _> = serde_json::from_str(body_str);
    match parsed {
        Ok(val) => {
            let success = val.get("success").and_then(Value::as_bool).unwrap_or(false);
            let stdout = val.get("stdout").and_then(Value::as_str).unwrap_or("");
            let stderr = val.get("stderr").and_then(Value::as_str).unwrap_or("");

            let content = format_compile_result(success, stdout, stderr);
            ToolResult {
                tool_use_id: call.id.clone(),
                content,
                is_error: !success,
            }
        }
        Err(e) => {
            // Maybe the body is the error message directly.
            ToolResult {
                tool_use_id: call.id.clone(),
                content: format!(
                    "Failed to parse build server response: {}\nRaw: {}",
                    e,
                    &body_str[..core::cmp::min(body_str.len(), 500)]
                ),
                is_error: true,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Compile handler — function pointer injected by the kernel
// ---------------------------------------------------------------------------

/// Type for the compile handler function.
///
/// The kernel registers a function that takes the JSON request body bytes
/// and returns the raw HTTP response bytes (or an error string).
/// This allows the `no_std` api-client crate to delegate networking to the
/// kernel, which owns the network stack.
pub type CompileHandler = fn(&[u8]) -> Result<Vec<u8>, String>;

/// Global compile handler — set by the kernel at startup.
///
/// SAFETY: This is only written once during kernel init (single-threaded boot)
/// and read during tool execution. In a multi-agent scenario the handler is
/// read-only after init.
static mut COMPILE_RUST_HANDLER: Option<CompileHandler> = None;

/// Register the compile handler. Called by the kernel during init.
///
/// # Safety
/// Must be called once during single-threaded kernel initialization.
pub unsafe fn set_compile_handler(handler: CompileHandler) {
    COMPILE_RUST_HANDLER = Some(handler);
}

/// Get the build server port (for use by the kernel's handler implementation).
pub const fn build_server_port() -> u16 {
    BUILD_SERVER_PORT
}
