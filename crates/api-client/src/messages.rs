//! Anthropic Messages API request and response types with JSON serialization.
//!
//! All types serialize/deserialize to match the Anthropic API wire format.
//! Uses serde with no_std + alloc.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during API message serialization/deserialization.
#[derive(Debug)]
pub enum ApiError {
    /// JSON serialization failed.
    SerializeError(String),
    /// JSON deserialization failed.
    DeserializeError(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::SerializeError(msg) => write!(f, "serialize error: {}", msg),
            ApiError::DeserializeError(msg) => write!(f, "deserialize error: {}", msg),
        }
    }
}

// ---------------------------------------------------------------------------
// Role
// ---------------------------------------------------------------------------

/// Conversation role: user or assistant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

// ---------------------------------------------------------------------------
// Content blocks (request side)
// ---------------------------------------------------------------------------

/// Image source for image content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    /// The encoding type, e.g. "base64".
    #[serde(rename = "type")]
    pub source_type: String,
    /// The media type, e.g. "image/png".
    pub media_type: String,
    /// The image data.
    pub data: String,
}

/// A single content block within a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content.
    Text {
        text: String,
    },
    /// Base64-encoded image.
    Image {
        source: ImageSource,
    },
    /// A tool invocation from the assistant.
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// The result of a tool invocation, sent by the user.
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

// ---------------------------------------------------------------------------
// MessageContent — text shorthand or array of blocks
// ---------------------------------------------------------------------------

/// Message content: either a plain text string or an array of content blocks.
///
/// The Anthropic API accepts both `"content": "hello"` (string shorthand) and
/// `"content": [{"type": "text", "text": "hello"}]` (block array). We handle
/// both directions with a custom serializer.
#[derive(Debug, Clone)]
pub enum MessageContent {
    /// Convenience: a single text string. Serializes as a JSON string.
    Text(String),
    /// An array of typed content blocks.
    Blocks(Vec<ContentBlock>),
}

impl Serialize for MessageContent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            MessageContent::Text(s) => serializer.serialize_str(s),
            MessageContent::Blocks(blocks) => blocks.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for MessageContent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de;

        struct MessageContentVisitor;

        impl<'de> de::Visitor<'de> for MessageContentVisitor {
            type Value = MessageContent;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string or array of content blocks")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(MessageContent::Text(String::from(v)))
            }

            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(MessageContent::Text(v))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                let blocks = Vec::<ContentBlock>::deserialize(
                    de::value::SeqAccessDeserializer::new(seq),
                )?;
                Ok(MessageContent::Blocks(blocks))
            }
        }

        deserializer.deserialize_any(MessageContentVisitor)
    }
}

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

impl Message {
    /// Create a user message with plain text content.
    pub fn user(text: String) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Text(text),
        }
    }

    /// Create an assistant message with plain text content.
    pub fn assistant(text: String) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(text),
        }
    }

    /// Create a user message containing tool results.
    pub fn tool_result(tool_use_id: String, content: String) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Blocks(alloc::vec![ContentBlock::ToolResult {
                tool_use_id,
                content,
            }]),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// A tool that the model may invoke.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the tool's input parameters.
    pub input_schema: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// A request to the Anthropic Messages API (`POST /v1/messages`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
}

impl MessagesRequest {
    /// Serialize this request to JSON bytes suitable for an HTTP body.
    pub fn to_json(&self) -> Result<Vec<u8>, ApiError> {
        serde_json::to_vec(self).map_err(|e| {
            ApiError::SerializeError(alloc::format!("{}", e))
        })
    }
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// A content block in the API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseBlock {
    /// Text output from the model.
    Text {
        text: String,
    },
    /// A tool the model wants to invoke.
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

/// Why the model stopped generating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
}

/// Token usage for a single API call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// The full response from the Anthropic Messages API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    pub model: String,
    pub role: Role,
    pub content: Vec<ResponseBlock>,
    pub stop_reason: Option<StopReason>,
    pub usage: Usage,
}

impl MessagesResponse {
    /// Deserialize a response from JSON bytes.
    pub fn from_json(data: &[u8]) -> Result<Self, ApiError> {
        serde_json::from_slice(data).map_err(|e| {
            ApiError::DeserializeError(alloc::format!("{}", e))
        })
    }

    /// Extract all text content from the response, concatenated.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ResponseBlock::Text { text } = block {
                out.push_str(text);
            }
        }
        out
    }

    /// Return all tool use blocks from the response.
    pub fn tool_uses(&self) -> Vec<(&str, &str, &serde_json::Value)> {
        let mut out = Vec::new();
        for block in &self.content {
            if let ResponseBlock::ToolUse { id, name, input } = block {
                out.push((id.as_str(), name.as_str(), input));
            }
        }
        out
    }

    /// Returns true if the model stopped because it wants to use a tool.
    pub fn needs_tool_use(&self) -> bool {
        self.stop_reason == Some(StopReason::ToolUse)
    }
}
