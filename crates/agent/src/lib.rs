//! Agent session manager — multi-agent dashboard with conversation state.

#![no_std]
extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Content blocks — the building blocks of conversation messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        /// JSON-encoded input string.
        input: String,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

// ---------------------------------------------------------------------------
// Conversation message
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum ConversationRole {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
}

#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub role: ConversationRole,
    pub content: Vec<ContentBlock>,
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// Conversation — ordered history + token accounting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Conversation {
    pub messages: Vec<ConversationMessage>,
    pub system_prompt: String,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}

impl Conversation {
    pub fn new(system_prompt: String) -> Self {
        Self {
            messages: Vec::new(),
            system_prompt,
            total_input_tokens: 0,
            total_output_tokens: 0,
        }
    }

    /// Append a plain-text user message.
    pub fn add_user_message(&mut self, text: String, timestamp: u64) {
        self.messages.push(ConversationMessage {
            role: ConversationRole::User,
            content: vec![ContentBlock::Text { text }],
            timestamp,
        });
    }

    /// Append a plain-text assistant message.
    pub fn add_assistant_text(&mut self, text: String, timestamp: u64) {
        self.messages.push(ConversationMessage {
            role: ConversationRole::Assistant,
            content: vec![ContentBlock::Text { text }],
            timestamp,
        });
    }

    /// Append an assistant message containing a tool-use request.
    pub fn add_tool_use(
        &mut self,
        id: String,
        name: String,
        input: String,
        timestamp: u64,
    ) {
        // If the last message is already an assistant message we can append the
        // tool_use block to it (the API allows multiple content blocks per
        // assistant turn).  Otherwise create a new assistant message.
        let append_to_last = self
            .messages
            .last()
            .map(|m| m.role == ConversationRole::Assistant)
            .unwrap_or(false);

        if append_to_last {
            if let Some(msg) = self.messages.last_mut() {
                msg.content.push(ContentBlock::ToolUse { id, name, input });
                msg.timestamp = timestamp;
            }
        } else {
            self.messages.push(ConversationMessage {
                role: ConversationRole::Assistant,
                content: vec![ContentBlock::ToolUse { id, name, input }],
                timestamp,
            });
        }
    }

    /// Append a user message containing a tool result.
    pub fn add_tool_result(
        &mut self,
        tool_use_id: String,
        content: String,
        is_error: bool,
        timestamp: u64,
    ) {
        // Tool results are sent as role=user. If the last message is already a
        // user message with tool results we append; otherwise create a new one.
        let append_to_last = self
            .messages
            .last()
            .map(|m| {
                m.role == ConversationRole::User
                    && m.content
                        .iter()
                        .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
            })
            .unwrap_or(false);

        if append_to_last {
            if let Some(msg) = self.messages.last_mut() {
                msg.content.push(ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                });
                msg.timestamp = timestamp;
            }
        } else {
            self.messages.push(ConversationMessage {
                role: ConversationRole::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                }],
                timestamp,
            });
        }
    }

    /// Serialize the conversation into the `messages` array expected by the
    /// Anthropic Messages API.
    ///
    /// Returns a `Vec<serde_json::Value>` where each element is a JSON object
    /// with `role` and `content` fields.
    pub fn to_api_messages(&self) -> Vec<serde_json::Value> {
        let mut out = Vec::with_capacity(self.messages.len());

        for msg in &self.messages {
            let role_str = match msg.role {
                ConversationRole::User => "user",
                ConversationRole::Assistant => "assistant",
            };

            let content_arr: Vec<serde_json::Value> = msg
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => {
                        serde_json::json!({
                            "type": "text",
                            "text": text
                        })
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        // `input` is a JSON string — parse it so the API gets
                        // an object, falling back to a raw string value.
                        let input_value: serde_json::Value =
                            serde_json::from_str(input).unwrap_or_else(|_| {
                                serde_json::Value::String(input.clone())
                            });
                        serde_json::json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input_value
                        })
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": tool_use_id,
                            "content": content,
                            "is_error": is_error
                        })
                    }
                })
                .collect();

            out.push(serde_json::json!({
                "role": role_str,
                "content": content_arr
            }));
        }

        out
    }

    /// Rough token estimation: ~4 characters per token for English text.
    fn estimate_tokens(text: &str) -> u32 {
        (text.len() as u32 + 3) / 4
    }

    /// Drop the oldest messages (preserving the first user message) until the
    /// estimated total token count is within `max_tokens`.
    pub fn truncate_to_budget(&mut self, max_tokens: u32) {
        loop {
            let total: u32 = self
                .messages
                .iter()
                .map(|m| {
                    m.content
                        .iter()
                        .map(|b| match b {
                            ContentBlock::Text { text } => Self::estimate_tokens(text),
                            ContentBlock::ToolUse { input, name, .. } => {
                                Self::estimate_tokens(input) + Self::estimate_tokens(name)
                            }
                            ContentBlock::ToolResult { content, .. } => {
                                Self::estimate_tokens(content)
                            }
                        })
                        .sum::<u32>()
                })
                .sum();

            if total <= max_tokens || self.messages.len() <= 1 {
                break;
            }

            // Remove the second message (index 1) to keep the original user
            // prompt at index 0 for context.
            self.messages.remove(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Agent session state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AgentState {
    Idle,
    WaitingForInput,
    Thinking,
    ToolExecuting,
    Streaming,
    Error,
}

// ---------------------------------------------------------------------------
// Tool definitions — kept for reference by callers
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum Tool {
    FileRead { path: String },
    FileWrite { path: String, content: String },
    ListDir { path: String },
}

// ---------------------------------------------------------------------------
// Agent session
// ---------------------------------------------------------------------------

/// Default model used when creating new sessions.
const DEFAULT_MODEL: &str = "claude-sonnet-4-20250514";
/// Default max output tokens per API call.
const DEFAULT_MAX_TOKENS: u32 = 8192;
/// Default system prompt baked into every new conversation.
const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a coding assistant running inside ClaudioOS, a bare-metal Rust operating \
     system. You can read and write files and list directories. Be concise.";

pub struct AgentSession {
    pub id: usize,
    pub name: String,
    pub state: AgentState,
    pub conversation: Conversation,
    pub pane_id: usize,
    pub model: String,
    pub max_tokens: u32,
}

impl AgentSession {
    pub fn new(id: usize, name: String, pane_id: usize) -> Self {
        Self {
            id,
            name,
            state: AgentState::WaitingForInput,
            conversation: Conversation::new(String::from(DEFAULT_SYSTEM_PROMPT)),
            pane_id,
            model: String::from(DEFAULT_MODEL),
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    /// Process user input — adds to conversation and transitions to `Thinking`.
    /// Returns `true` if the session is ready for an API call.
    pub fn handle_input(&mut self, input: String, timestamp: u64) -> bool {
        if input.is_empty() {
            return false;
        }

        self.conversation.add_user_message(input, timestamp);
        self.state = AgentState::Thinking;
        log::debug!("[agent:{}] user input received, state -> Thinking", self.id);
        true
    }

    /// Process streaming or complete assistant text from the API.
    pub fn handle_response_text(&mut self, text: String, timestamp: u64) {
        self.conversation.add_assistant_text(text, timestamp);
        self.state = AgentState::WaitingForInput;
        log::debug!(
            "[agent:{}] response received, state -> WaitingForInput",
            self.id
        );
    }

    /// Process a tool-use request from the API response.
    pub fn handle_tool_use(
        &mut self,
        id: String,
        name: String,
        input: String,
        timestamp: u64,
    ) {
        self.conversation.add_tool_use(id, name, input, timestamp);
        self.state = AgentState::ToolExecuting;
        log::debug!(
            "[agent:{}] tool use requested, state -> ToolExecuting",
            self.id
        );
    }

    /// Feed the result of tool execution back into the conversation.
    /// After a tool result the session returns to `Thinking` because the API
    /// needs to be called again with the tool result.
    pub fn handle_tool_result(
        &mut self,
        tool_use_id: String,
        content: String,
        is_error: bool,
        timestamp: u64,
    ) {
        self.conversation
            .add_tool_result(tool_use_id, content, is_error, timestamp);
        self.state = AgentState::Thinking;
        log::debug!(
            "[agent:{}] tool result added, state -> Thinking",
            self.id
        );
    }

    /// Mark the session as errored.
    pub fn set_error(&mut self) {
        self.state = AgentState::Error;
    }

    /// Update token accounting from an API response.
    pub fn record_usage(&mut self, input_tokens: u32, output_tokens: u32) {
        self.conversation.total_input_tokens += input_tokens;
        self.conversation.total_output_tokens += output_tokens;
    }
}

// ---------------------------------------------------------------------------
// Dashboard — manages multiple agent sessions
// ---------------------------------------------------------------------------

pub struct Dashboard {
    pub sessions: Vec<AgentSession>,
    pub focused: usize,
    next_id: usize,
}

impl Dashboard {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            focused: 0,
            next_id: 0,
        }
    }

    /// Create a new agent session and return its id.
    pub fn create_session(&mut self, name: String, pane_id: usize) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let session = AgentSession::new(id, name, pane_id);
        self.sessions.push(session);
        log::info!("[dashboard] created session {} (pane {})", id, pane_id);
        id
    }

    /// Get a shared reference to the currently focused session.
    pub fn focused_session(&self) -> Option<&AgentSession> {
        if self.sessions.is_empty() {
            None
        } else {
            self.sessions.get(self.focused)
        }
    }

    /// Get a mutable reference to the currently focused session.
    pub fn focused_session_mut(&mut self) -> Option<&mut AgentSession> {
        if self.sessions.is_empty() {
            None
        } else {
            self.sessions.get_mut(self.focused)
        }
    }

    /// Move focus to the next session (wraps around).
    pub fn focus_next(&mut self) {
        if !self.sessions.is_empty() {
            self.focused = (self.focused + 1) % self.sessions.len();
            log::debug!("[dashboard] focus -> session {}", self.focused);
        }
    }

    /// Move focus to the previous session (wraps around).
    pub fn focus_prev(&mut self) {
        if !self.sessions.is_empty() {
            if self.focused == 0 {
                self.focused = self.sessions.len() - 1;
            } else {
                self.focused -= 1;
            }
            log::debug!("[dashboard] focus -> session {}", self.focused);
        }
    }

    /// Close the currently focused session. Focus moves to the previous one.
    pub fn close_focused(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let removed_id = self.sessions[self.focused].id;
        self.sessions.remove(self.focused);
        log::info!("[dashboard] closed session {}", removed_id);

        if self.sessions.is_empty() {
            self.focused = 0;
        } else if self.focused >= self.sessions.len() {
            self.focused = self.sessions.len() - 1;
        }
    }

    /// Look up a session by id.
    pub fn session_by_id(&self, id: usize) -> Option<&AgentSession> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// Look up a session by id (mutable).
    pub fn session_by_id_mut(&mut self, id: usize) -> Option<&mut AgentSession> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }
}

/// Top-level entry point: start the agent dashboard. Called after auth succeeds.
pub async fn dashboard(_creds: claudio_auth::Credentials) {
    log::info!("[agent] dashboard starting");
    // TODO: agent dashboard event loop — create initial session, render panes,
    // process keyboard input, dispatch API calls.
}
