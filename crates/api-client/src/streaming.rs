//! SSE streaming response handler for the Anthropic Messages API.
//!
//! The Anthropic API streams responses as Server-Sent Events. Each SSE event
//! has an `event:` type and a `data:` JSON payload.  This module provides:
//!
//! - [`StreamEvent`] — typed representation of each SSE event kind.
//! - [`StreamParser`] — incremental byte-level SSE parser that yields events.
//! - [`StreamAccumulator`] — builds up the full response from a sequence of events.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Stream event types
// ---------------------------------------------------------------------------

/// A parsed, typed event from the Anthropic Messages API SSE stream.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// First event in the stream. Contains the message ID and model name.
    MessageStart {
        message_id: String,
        model: String,
    },
    /// A new content block is starting at the given index.
    ContentBlockStart {
        index: usize,
        content_type: String,
    },
    /// Incremental content within an existing content block.
    ContentBlockDelta {
        index: usize,
        delta: Delta,
    },
    /// A content block has finished.
    ContentBlockStop {
        index: usize,
    },
    /// Message-level metadata update (stop reason, token usage).
    MessageDelta {
        stop_reason: Option<String>,
        usage: Option<DeltaUsage>,
    },
    /// Final event — the message is complete.
    MessageStop,
    /// Keep-alive ping from the server.
    Ping,
    /// An error reported inline in the stream.
    Error {
        error_type: String,
        message: String,
    },
}

/// Incremental content delta within a content block.
#[derive(Debug, Clone)]
pub enum Delta {
    /// A chunk of text content.
    TextDelta { text: String },
    /// A chunk of tool-use input JSON (streamed incrementally).
    InputJsonDelta { partial_json: String },
}

/// Token usage reported in a `message_delta` event.
#[derive(Debug, Clone)]
pub struct DeltaUsage {
    pub output_tokens: u32,
}

// ---------------------------------------------------------------------------
// Serde helper structs for JSON deserialization
// ---------------------------------------------------------------------------

/// Top-level `message_start` event data.
#[derive(Deserialize)]
struct MessageStartData {
    message: MessageStartInner,
}

#[derive(Deserialize)]
struct MessageStartInner {
    id: String,
    model: String,
    #[allow(dead_code)]
    usage: Option<MessageStartUsage>,
}

#[derive(Deserialize)]
struct MessageStartUsage {
    #[allow(dead_code)]
    input_tokens: Option<u32>,
    #[allow(dead_code)]
    output_tokens: Option<u32>,
}

/// `content_block_start` event data.
#[derive(Deserialize)]
struct ContentBlockStartData {
    index: usize,
    content_block: ContentBlockInfo,
}

#[derive(Deserialize)]
struct ContentBlockInfo {
    #[serde(rename = "type")]
    block_type: String,
    /// Present for text blocks.
    #[allow(dead_code)]
    text: Option<String>,
    /// Present for tool_use blocks.
    #[allow(dead_code)]
    id: Option<String>,
    #[allow(dead_code)]
    name: Option<String>,
}

/// `content_block_delta` event data.
#[derive(Deserialize)]
struct ContentBlockDeltaData {
    index: usize,
    delta: DeltaRaw,
}

#[derive(Deserialize)]
struct DeltaRaw {
    #[serde(rename = "type")]
    delta_type: String,
    /// Present when `type` == `"text_delta"`.
    text: Option<String>,
    /// Present when `type` == `"input_json_delta"`.
    partial_json: Option<String>,
}

/// `content_block_stop` event data.
#[derive(Deserialize)]
struct ContentBlockStopData {
    index: usize,
}

/// `message_delta` event data.
#[derive(Deserialize)]
struct MessageDeltaData {
    delta: MessageDeltaInner,
    usage: Option<DeltaUsageRaw>,
}

#[derive(Deserialize)]
struct MessageDeltaInner {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct DeltaUsageRaw {
    output_tokens: Option<u32>,
}

/// Inline error event data.
#[derive(Deserialize)]
struct ErrorData {
    error: ErrorInner,
}

#[derive(Deserialize)]
struct ErrorInner {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

// ---------------------------------------------------------------------------
// StreamParser — incremental SSE byte parser
// ---------------------------------------------------------------------------

/// Accumulates raw bytes from the HTTP response body and yields parsed
/// [`StreamEvent`]s as complete SSE events arrive.
pub struct StreamParser {
    /// Partial data buffer — may contain incomplete SSE events.
    buffer: Vec<u8>,
}

impl StreamParser {
    /// Create a new, empty stream parser.
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(4096),
        }
    }

    /// Feed raw bytes from the HTTP response body into the parser.
    ///
    /// Returns any complete [`StreamEvent`]s that could be parsed from the
    /// accumulated buffer.  Incomplete events remain in the internal buffer
    /// for the next call.
    pub fn feed(&mut self, data: &[u8]) -> Vec<StreamEvent> {
        self.buffer.extend_from_slice(data);

        let mut events = Vec::new();

        // Process all complete SSE events (delimited by blank lines).
        loop {
            match find_event_boundary(&self.buffer) {
                Some(end) => {
                    // `end` is the byte offset right after the blank-line delimiter.
                    let event_bytes = self.buffer[..end].to_vec();
                    self.buffer = self.buffer[end..].to_vec();

                    if let Some(evt) = parse_single_sse_event(&event_bytes) {
                        events.push(evt);
                    }
                }
                None => break,
            }
        }

        events
    }

    /// Returns `true` if the internal buffer is empty (no pending partial data).
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Drain any remaining bytes from the buffer (e.g. on stream close).
    /// Attempts to parse them as a final event.
    pub fn finish(&mut self) -> Vec<StreamEvent> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        // Try to parse whatever remains.
        let remaining = core::mem::take(&mut self.buffer);
        let mut events = Vec::new();
        if let Some(evt) = parse_single_sse_event(&remaining) {
            events.push(evt);
        }
        events
    }
}

/// Find the first SSE event boundary in `data`.
///
/// SSE events are separated by one or more blank lines.  We look for `\n\n`
/// (the standard delimiter).  Returns the byte offset immediately after the
/// blank line, or `None` if no complete event is present.
fn find_event_boundary(data: &[u8]) -> Option<usize> {
    // Look for \n\n (LF LF).
    for i in 0..data.len().saturating_sub(1) {
        if data[i] == b'\n' && data[i + 1] == b'\n' {
            return Some(i + 2);
        }
    }
    // Also handle \r\n\r\n (CRLF CRLF).
    if data.len() >= 4 {
        for i in 0..data.len().saturating_sub(3) {
            if data[i] == b'\r'
                && data[i + 1] == b'\n'
                && data[i + 2] == b'\r'
                && data[i + 3] == b'\n'
            {
                return Some(i + 4);
            }
        }
    }
    None
}

/// Parse a single SSE event block (the bytes before the blank-line delimiter)
/// into a typed [`StreamEvent`].
fn parse_single_sse_event(raw: &[u8]) -> Option<StreamEvent> {
    let text = core::str::from_utf8(raw).ok()?;

    let mut event_type = "";
    let mut data_lines: Vec<&str> = Vec::new();

    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some(val) = line.strip_prefix("event:") {
            event_type = val.trim();
        } else if let Some(val) = line.strip_prefix("data:") {
            data_lines.push(val.trim_start());
        }
        // Ignore `id:`, `retry:`, and comment lines starting with `:`
    }

    if data_lines.is_empty() && event_type.is_empty() {
        return None;
    }

    // Join multiple data: lines with newlines (per SSE spec).
    let data: String = if data_lines.len() == 1 {
        String::from(data_lines[0])
    } else {
        let mut s = String::new();
        for (i, line) in data_lines.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            s.push_str(line);
        }
        s
    };

    deserialize_event(event_type, &data)
}

/// Given a parsed SSE event type and its JSON data payload, deserialize into
/// the appropriate [`StreamEvent`] variant.
fn deserialize_event(event_type: &str, data: &str) -> Option<StreamEvent> {
    match event_type {
        "message_start" => {
            let parsed: MessageStartData = serde_json::from_str(data).ok()?;
            Some(StreamEvent::MessageStart {
                message_id: parsed.message.id,
                model: parsed.message.model,
            })
        }

        "content_block_start" => {
            let parsed: ContentBlockStartData = serde_json::from_str(data).ok()?;
            Some(StreamEvent::ContentBlockStart {
                index: parsed.index,
                content_type: parsed.content_block.block_type,
            })
        }

        "content_block_delta" => {
            let parsed: ContentBlockDeltaData = serde_json::from_str(data).ok()?;
            let delta = match parsed.delta.delta_type.as_str() {
                "text_delta" => Delta::TextDelta {
                    text: parsed.delta.text.unwrap_or_default(),
                },
                "input_json_delta" => Delta::InputJsonDelta {
                    partial_json: parsed.delta.partial_json.unwrap_or_default(),
                },
                other => {
                    log::warn!("Unknown delta type: {}", other);
                    return None;
                }
            };
            Some(StreamEvent::ContentBlockDelta {
                index: parsed.index,
                delta,
            })
        }

        "content_block_stop" => {
            let parsed: ContentBlockStopData = serde_json::from_str(data).ok()?;
            Some(StreamEvent::ContentBlockStop {
                index: parsed.index,
            })
        }

        "message_delta" => {
            let parsed: MessageDeltaData = serde_json::from_str(data).ok()?;
            let usage = parsed.usage.and_then(|u| {
                u.output_tokens.map(|tokens| DeltaUsage {
                    output_tokens: tokens,
                })
            });
            Some(StreamEvent::MessageDelta {
                stop_reason: parsed.delta.stop_reason,
                usage,
            })
        }

        "message_stop" => Some(StreamEvent::MessageStop),

        "ping" => Some(StreamEvent::Ping),

        "error" => {
            let parsed: ErrorData = serde_json::from_str(data).ok()?;
            Some(StreamEvent::Error {
                error_type: parsed.error.error_type,
                message: parsed.error.message,
            })
        }

        other => {
            log::debug!("Unknown SSE event type: {}", other);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// StreamAccumulator — builds full response from stream events
// ---------------------------------------------------------------------------

/// Accumulated state of a tool call being streamed.
#[derive(Debug, Clone)]
pub struct ToolCallAccum {
    /// Index of the content block this tool call belongs to.
    pub index: usize,
    /// Accumulated partial JSON input for the tool call.
    pub input_json: String,
}

/// Builds up the full response from a sequence of [`StreamEvent`]s.
///
/// Feed events via [`process()`](StreamAccumulator::process) and read the
/// accumulated state at any time.
pub struct StreamAccumulator {
    /// Message ID from the `message_start` event.
    pub message_id: String,
    /// Model name from the `message_start` event.
    pub model: String,
    /// Accumulated text from all `TextDelta` events across all text content blocks.
    pub text: String,
    /// Accumulated tool calls (one per `tool_use` content block).
    pub tool_calls: Vec<ToolCallAccum>,
    /// Stop reason from the `message_delta` event.
    pub stop_reason: Option<String>,
    /// Input token count (from `message_start` usage, if provided).
    pub input_tokens: u32,
    /// Output token count (from `message_delta` usage, accumulated).
    pub output_tokens: u32,

    /// Tracks the content type for each content block index so we know
    /// whether deltas are text or tool-use JSON.
    content_block_types: Vec<(usize, String)>,
}

impl StreamAccumulator {
    /// Create a new, empty accumulator.
    pub fn new() -> Self {
        Self {
            message_id: String::new(),
            model: String::new(),
            text: String::new(),
            tool_calls: Vec::new(),
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            content_block_types: Vec::new(),
        }
    }

    /// Process a single stream event, updating accumulated state.
    pub fn process(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::MessageStart { message_id, model } => {
                self.message_id = message_id.clone();
                self.model = model.clone();
            }

            StreamEvent::ContentBlockStart {
                index,
                content_type,
            } => {
                self.content_block_types
                    .push((*index, content_type.clone()));

                if content_type == "tool_use" {
                    self.tool_calls.push(ToolCallAccum {
                        index: *index,
                        input_json: String::new(),
                    });
                }
            }

            StreamEvent::ContentBlockDelta { index, delta } => match delta {
                Delta::TextDelta { text } => {
                    self.text.push_str(text);
                }
                Delta::InputJsonDelta { partial_json } => {
                    // Find the tool call accumulator for this content block index.
                    if let Some(tc) = self
                        .tool_calls
                        .iter_mut()
                        .find(|tc| tc.index == *index)
                    {
                        tc.input_json.push_str(partial_json);
                    } else {
                        log::warn!(
                            "InputJsonDelta for unknown content block index {}",
                            index
                        );
                    }
                }
            },

            StreamEvent::ContentBlockStop { index: _ } => {
                // Nothing to do — the block is complete.
            }

            StreamEvent::MessageDelta {
                stop_reason,
                usage,
            } => {
                if stop_reason.is_some() {
                    self.stop_reason = stop_reason.clone();
                }
                if let Some(u) = usage {
                    self.output_tokens = u.output_tokens;
                }
            }

            StreamEvent::MessageStop => {
                // Stream is complete. No additional state to update.
            }

            StreamEvent::Ping => {
                // Keep-alive — nothing to do.
            }

            StreamEvent::Error {
                error_type,
                message,
            } => {
                log::error!("Stream error [{}]: {}", error_type, message);
            }
        }
    }

    /// Returns `true` if the stream has completed (received `message_stop`
    /// or a stop_reason has been set).
    pub fn is_complete(&self) -> bool {
        self.stop_reason.is_some()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec;

    #[test]
    fn test_parse_message_start() {
        let raw = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",",
            "\"type\":\"message\",\"role\":\"assistant\",\"content\":[],",
            "\"model\":\"claude-sonnet-4-20250514\",\"stop_reason\":null,",
            "\"usage\":{\"input_tokens\":25,\"output_tokens\":0}}}\n",
            "\n"
        );

        let mut parser = StreamParser::new();
        let events = parser.feed(raw.as_bytes());
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::MessageStart { message_id, model } => {
                assert_eq!(message_id, "msg_123");
                assert_eq!(model, "claude-sonnet-4-20250514");
            }
            other => panic!("Expected MessageStart, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_text_delta() {
        let raw = concat!(
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,",
            "\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n",
            "\n"
        );

        let mut parser = StreamParser::new();
        let events = parser.feed(raw.as_bytes());
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ContentBlockDelta {
                index,
                delta: Delta::TextDelta { text },
            } => {
                assert_eq!(*index, 0);
                assert_eq!(text, "Hello");
            }
            other => panic!("Expected ContentBlockDelta/TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_ping() {
        let raw = "event: ping\ndata: {\"type\":\"ping\"}\n\n";
        let mut parser = StreamParser::new();
        let events = parser.feed(raw.as_bytes());
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Ping));
    }

    #[test]
    fn test_incremental_feed() {
        let full = concat!(
            "event: ping\n",
            "data: {\"type\":\"ping\"}\n",
            "\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n",
            "\n",
        );
        let bytes = full.as_bytes();

        let mut parser = StreamParser::new();

        // Feed one byte at a time.
        let mut all_events = Vec::new();
        for &b in bytes {
            let evts = parser.feed(&[b]);
            all_events.extend(evts);
        }

        assert_eq!(all_events.len(), 2);
        assert!(matches!(&all_events[0], StreamEvent::Ping));
        assert!(matches!(&all_events[1], StreamEvent::MessageStop));
    }

    #[test]
    fn test_accumulator_text() {
        let mut acc = StreamAccumulator::new();

        acc.process(&StreamEvent::MessageStart {
            message_id: String::from("msg_abc"),
            model: String::from("claude-sonnet-4-20250514"),
        });
        acc.process(&StreamEvent::ContentBlockStart {
            index: 0,
            content_type: String::from("text"),
        });
        acc.process(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: Delta::TextDelta {
                text: String::from("Hello"),
            },
        });
        acc.process(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: Delta::TextDelta {
                text: String::from(" world"),
            },
        });
        acc.process(&StreamEvent::ContentBlockStop { index: 0 });
        acc.process(&StreamEvent::MessageDelta {
            stop_reason: Some(String::from("end_turn")),
            usage: Some(DeltaUsage {
                output_tokens: 15,
            }),
        });
        acc.process(&StreamEvent::MessageStop);

        assert_eq!(acc.message_id, "msg_abc");
        assert_eq!(acc.text, "Hello world");
        assert_eq!(acc.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(acc.output_tokens, 15);
        assert!(acc.is_complete());
    }

    #[test]
    fn test_accumulator_tool_use() {
        let mut acc = StreamAccumulator::new();

        acc.process(&StreamEvent::ContentBlockStart {
            index: 1,
            content_type: String::from("tool_use"),
        });
        acc.process(&StreamEvent::ContentBlockDelta {
            index: 1,
            delta: Delta::InputJsonDelta {
                partial_json: String::from("{\"path\":"),
            },
        });
        acc.process(&StreamEvent::ContentBlockDelta {
            index: 1,
            delta: Delta::InputJsonDelta {
                partial_json: String::from("\"/tmp\"}"),
            },
        });
        acc.process(&StreamEvent::ContentBlockStop { index: 1 });

        assert_eq!(acc.tool_calls.len(), 1);
        assert_eq!(acc.tool_calls[0].index, 1);
        assert_eq!(acc.tool_calls[0].input_json, "{\"path\":\"/tmp\"}");
    }

    #[test]
    fn test_parse_error_event() {
        let raw = concat!(
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",",
            "\"message\":\"Overloaded\"}}\n",
            "\n"
        );

        let mut parser = StreamParser::new();
        let events = parser.feed(raw.as_bytes());
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Error {
                error_type,
                message,
            } => {
                assert_eq!(error_type, "overloaded_error");
                assert_eq!(message, "Overloaded");
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_crlf_event_boundary() {
        let raw = "event: ping\r\ndata: {\"type\":\"ping\"}\r\n\r\n";
        let mut parser = StreamParser::new();
        let events = parser.feed(raw.as_bytes());
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Ping));
    }
}
