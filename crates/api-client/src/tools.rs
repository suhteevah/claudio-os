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
