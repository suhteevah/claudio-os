//! Agent memory persistence — cross-session key-value store for each agent.
//!
//! Each agent has its own memory namespace, persisted as JSON to the VFS at
//! `/var/claudio/agents/{agent_name}/memory.json`. Memory survives reboots
//! and is automatically loaded when an agent session starts.
//!
//! # Agent tools
//!
//! - `save_memory(key, value)` — store a key-value pair
//! - `load_memory(key)` — retrieve a value by key
//! - `list_memories()` — list all stored keys
//!
//! # Shell commands
//!
//! - `memory list [agent]` — list keys for an agent (or all agents)
//! - `memory clear [agent]` — clear an agent's memory

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// AgentMemory — per-agent key-value store
// ---------------------------------------------------------------------------

/// Key-value memory store for a single agent.
///
/// Values are arbitrary strings (often JSON or plain text). The agent decides
/// what to remember — project context, preferences, file locations, etc.
#[derive(Debug, Clone)]
pub struct AgentMemory {
    /// Agent name (used as namespace for persistence).
    pub agent_name: String,
    /// Key-value store.
    entries: BTreeMap<String, String>,
    /// Whether the memory has unsaved changes.
    dirty: bool,
}

impl AgentMemory {
    /// Create a new empty memory store for the given agent.
    pub fn new(agent_name: &str) -> Self {
        Self {
            agent_name: String::from(agent_name),
            entries: BTreeMap::new(),
            dirty: false,
        }
    }

    /// Store a key-value pair. Overwrites any existing value for the key.
    pub fn save(&mut self, key: &str, value: &str) {
        self.entries.insert(String::from(key), String::from(value));
        self.dirty = true;
        log::debug!(
            "[agent_memory] {}: saved key '{}' ({} bytes)",
            self.agent_name,
            key,
            value.len()
        );
    }

    /// Retrieve a value by key. Returns None if the key doesn't exist.
    pub fn load(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|s| s.as_str())
    }

    /// List all stored keys.
    pub fn list_keys(&self) -> Vec<&str> {
        self.entries.keys().map(|s| s.as_str()).collect()
    }

    /// Remove a key. Returns true if the key existed.
    pub fn remove(&mut self, key: &str) -> bool {
        let existed = self.entries.remove(key).is_some();
        if existed {
            self.dirty = true;
        }
        existed
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.entries.clear();
            self.dirty = true;
            log::info!("[agent_memory] {}: cleared all entries", self.agent_name);
        }
    }

    /// Number of stored entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the memory is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark as saved (no pending changes).
    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    // ── Serialization (simple JSON) ─────────────────────────────────

    /// Serialize the memory to JSON bytes.
    ///
    /// Format: `{"key1":"value1","key2":"value2",...}`
    ///
    /// Values are JSON-escaped (quotes, backslashes, control characters).
    pub fn to_json(&self) -> Vec<u8> {
        let mut buf = String::with_capacity(256);
        buf.push('{');
        let mut first = true;
        for (key, value) in &self.entries {
            if !first {
                buf.push(',');
            }
            first = false;
            buf.push('"');
            json_escape_into(&mut buf, key);
            buf.push_str("\":");
            buf.push('"');
            json_escape_into(&mut buf, value);
            buf.push('"');
        }
        buf.push('}');
        buf.into_bytes()
    }

    /// Deserialize memory from JSON bytes.
    ///
    /// Expects a flat JSON object `{"key":"value",...}`. Non-string values
    /// are stored as their JSON text representation.
    pub fn from_json(agent_name: &str, data: &[u8]) -> Result<Self, String> {
        let text = core::str::from_utf8(data)
            .map_err(|_| String::from("memory file is not valid UTF-8"))?;
        let text = text.trim();

        if !text.starts_with('{') || !text.ends_with('}') {
            return Err(String::from("memory file is not a JSON object"));
        }

        let inner = &text[1..text.len() - 1];
        let mut mem = AgentMemory::new(agent_name);

        if inner.trim().is_empty() {
            return Ok(mem);
        }

        // Simple JSON object parser — handles escaped strings.
        let mut chars = inner.chars().peekable();

        loop {
            skip_whitespace(&mut chars);
            if chars.peek().is_none() {
                break;
            }

            // Parse key.
            let key = parse_json_string(&mut chars)
                .map_err(|e| format!("bad key: {}", e))?;

            skip_whitespace(&mut chars);
            match chars.next() {
                Some(':') => {}
                other => return Err(format!("expected ':', got {:?}", other)),
            }

            skip_whitespace(&mut chars);

            // Parse value — could be string, number, bool, null.
            let value = if chars.peek() == Some(&'"') {
                parse_json_string(&mut chars)
                    .map_err(|e| format!("bad value for '{}': {}", key, e))?
            } else {
                // Non-string value: read until comma or end.
                let mut val = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch == ',' || ch == '}' {
                        break;
                    }
                    val.push(chars.next().unwrap());
                }
                val.trim().to_string()
            };

            mem.entries.insert(key, value);

            skip_whitespace(&mut chars);
            match chars.peek() {
                Some(&',') => {
                    chars.next();
                }
                _ => break,
            }
        }

        log::info!(
            "[agent_memory] loaded {} entries for agent '{}'",
            mem.entries.len(),
            agent_name
        );

        Ok(mem)
    }

    // ── System prompt injection ─────────────────────────────────────

    /// Generate a system prompt fragment summarizing this agent's memory.
    ///
    /// Injected at the start of the agent's system prompt so it has context
    /// from previous sessions.
    pub fn to_system_prompt(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let mut prompt = String::with_capacity(512);
        prompt.push_str("<agent_memory>\n");
        prompt.push_str("The following key-value pairs are from your persistent memory ");
        prompt.push_str("(saved across sessions). Use save_memory/load_memory tools to ");
        prompt.push_str("manage them.\n\n");

        for (key, value) in &self.entries {
            // Truncate very long values in the prompt.
            let display_value = if value.len() > 500 {
                format!("{}... ({} bytes total)", &value[..500], value.len())
            } else {
                value.clone()
            };
            prompt.push_str(&format!("- {}: {}\n", key, display_value));
        }

        prompt.push_str("</agent_memory>\n\n");
        prompt
    }

    // ── VFS persistence path ────────────────────────────────────────

    /// Return the VFS path where this agent's memory should be stored.
    pub fn vfs_path(&self) -> String {
        format!("/var/claudio/agents/{}/memory.json", self.agent_name)
    }
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

/// Escape a string for JSON output.
fn json_escape_into(buf: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                buf.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => buf.push(c),
        }
    }
}

fn skip_whitespace(chars: &mut core::iter::Peekable<core::str::Chars<'_>>) {
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
}

/// Parse a JSON string value (including the opening and closing quotes).
fn parse_json_string(
    chars: &mut core::iter::Peekable<core::str::Chars<'_>>,
) -> Result<String, String> {
    match chars.next() {
        Some('"') => {}
        other => return Err(format!("expected '\"', got {:?}", other)),
    }

    let mut s = String::new();
    loop {
        match chars.next() {
            None => return Err(String::from("unterminated string")),
            Some('"') => return Ok(s),
            Some('\\') => match chars.next() {
                Some('"') => s.push('"'),
                Some('\\') => s.push('\\'),
                Some('/') => s.push('/'),
                Some('n') => s.push('\n'),
                Some('r') => s.push('\r'),
                Some('t') => s.push('\t'),
                Some('u') => {
                    // Parse \uXXXX.
                    let mut hex = String::with_capacity(4);
                    for _ in 0..4 {
                        match chars.next() {
                            Some(c) => hex.push(c),
                            None => return Err(String::from("truncated \\u escape")),
                        }
                    }
                    let code = u32::from_str_radix(&hex, 16)
                        .map_err(|_| format!("bad \\u escape: {}", hex))?;
                    if let Some(c) = char::from_u32(code) {
                        s.push(c);
                    }
                }
                other => {
                    s.push('\\');
                    if let Some(c) = other {
                        s.push(c);
                    }
                }
            },
            Some(c) => s.push(c),
        }
    }
}

// ---------------------------------------------------------------------------
// Global memory store
// ---------------------------------------------------------------------------

/// Global store of all agent memories.
///
/// SAFETY: Single-threaded kernel — no concurrent access.
static mut MEMORY_STORE: Option<BTreeMap<String, AgentMemory>> = None;

fn store() -> &'static mut BTreeMap<String, AgentMemory> {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(MEMORY_STORE);
        if (*ptr).is_none() {
            *ptr = Some(BTreeMap::new());
        }
        (*ptr).as_mut().unwrap()
    }
}

/// Get or create an agent's memory.
pub fn get_memory(agent_name: &str) -> &'static mut AgentMemory {
    let store = store();
    if !store.contains_key(agent_name) {
        store.insert(
            String::from(agent_name),
            AgentMemory::new(agent_name),
        );
    }
    store.get_mut(agent_name).unwrap()
}

/// Load an agent's memory from VFS data. Call this when creating a session
/// if a memory file exists.
pub fn load_from_data(agent_name: &str, data: &[u8]) -> Result<(), String> {
    let mem = AgentMemory::from_json(agent_name, data)?;
    store().insert(String::from(agent_name), mem);
    Ok(())
}

/// Get the serialized JSON for an agent's memory (for VFS persistence).
pub fn serialize_memory(agent_name: &str) -> Option<Vec<u8>> {
    store().get(agent_name).map(|m| m.to_json())
}

/// Check if an agent has unsaved changes.
pub fn is_dirty(agent_name: &str) -> bool {
    store()
        .get(agent_name)
        .map(|m| m.is_dirty())
        .unwrap_or(false)
}

/// Mark an agent's memory as saved.
pub fn mark_clean(agent_name: &str) {
    if let Some(m) = store().get_mut(agent_name) {
        m.mark_clean();
    }
}

/// List all agents that have memory stored.
pub fn list_agents() -> Vec<String> {
    store().keys().cloned().collect()
}

// ---------------------------------------------------------------------------
// Agent tool handlers
// ---------------------------------------------------------------------------

/// Execute the `save_memory` tool: store a key-value pair.
///
/// Input JSON: `{"key": "...", "value": "..."}`
/// Returns confirmation string.
pub fn tool_save_memory(agent_name: &str, key: &str, value: &str) -> String {
    let mem = get_memory(agent_name);
    mem.save(key, value);
    format!("Saved '{}' ({} bytes) to agent memory.", key, value.len())
}

/// Execute the `load_memory` tool: retrieve a value.
///
/// Input JSON: `{"key": "..."}`
/// Returns the value or an error message.
pub fn tool_load_memory(agent_name: &str, key: &str) -> String {
    let mem = get_memory(agent_name);
    match mem.load(key) {
        Some(value) => value.to_string(),
        None => format!("Key '{}' not found in agent memory.", key),
    }
}

/// Execute the `list_memories` tool: list all keys.
///
/// Returns a formatted list of all stored keys.
pub fn tool_list_memories(agent_name: &str) -> String {
    let mem = get_memory(agent_name);
    let keys = mem.list_keys();
    if keys.is_empty() {
        String::from("Agent memory is empty.")
    } else {
        let mut output = format!("{} keys in memory:\n", keys.len());
        for key in keys {
            let value_preview = mem
                .load(key)
                .map(|v| {
                    if v.len() > 80 {
                        format!("{}...", &v[..80])
                    } else {
                        v.to_string()
                    }
                })
                .unwrap_or_default();
            output.push_str(&format!("  {} = {}\n", key, value_preview));
        }
        output
    }
}

// ---------------------------------------------------------------------------
// Shell command handler
// ---------------------------------------------------------------------------

/// Execute a `memory` shell command.
///
/// - `memory list [agent]` — list keys
/// - `memory clear [agent]` — clear memory
/// - `memory get <agent> <key>` — get a specific value
/// - `memory set <agent> <key> <value>` — set a value
pub fn execute_memory_command(args: &[&str]) -> String {
    if args.is_empty() {
        return String::from(
            "usage: memory <list|clear|get|set> [agent] [key] [value]\n\
             \n\
             Commands:\n\
             \x20 memory list              — list all agents with memory\n\
             \x20 memory list <agent>       — list keys for an agent\n\
             \x20 memory clear <agent>      — clear an agent's memory\n\
             \x20 memory get <agent> <key>  — get a value\n\
             \x20 memory set <agent> <key> <value> — set a value\n",
        );
    }

    match args[0] {
        "list" => {
            if args.len() > 1 {
                // List keys for a specific agent.
                let agent = args[1];
                let mem = get_memory(agent);
                if mem.is_empty() {
                    format!("Agent '{}' has no stored memories.\n", agent)
                } else {
                    let mut output = format!("Agent '{}' — {} keys:\n", agent, mem.len());
                    for key in mem.list_keys() {
                        let val = mem.load(key).unwrap_or("");
                        let preview = if val.len() > 60 {
                            format!("{}...", &val[..60])
                        } else {
                            val.to_string()
                        };
                        output.push_str(&format!("  {} = {}\n", key, preview));
                    }
                    output
                }
            } else {
                // List all agents.
                let agents = list_agents();
                if agents.is_empty() {
                    String::from("No agent memories stored.\n")
                } else {
                    let mut output = format!("{} agent(s) with memory:\n", agents.len());
                    for name in &agents {
                        let mem = get_memory(name);
                        output.push_str(&format!("  {} — {} keys\n", name, mem.len()));
                    }
                    output
                }
            }
        }

        "clear" => {
            if args.len() < 2 {
                return String::from("usage: memory clear <agent>\n");
            }
            let agent = args[1];
            let mem = get_memory(agent);
            let count = mem.len();
            mem.clear();
            format!("Cleared {} entries from agent '{}' memory.\n", count, agent)
        }

        "get" => {
            if args.len() < 3 {
                return String::from("usage: memory get <agent> <key>\n");
            }
            let agent = args[1];
            let key = args[2];
            match get_memory(agent).load(key) {
                Some(val) => format!("{}\n", val),
                None => format!("Key '{}' not found for agent '{}'.\n", key, agent),
            }
        }

        "set" => {
            if args.len() < 4 {
                return String::from("usage: memory set <agent> <key> <value>\n");
            }
            let agent = args[1];
            let key = args[2];
            let value = args[3..].join(" ");
            get_memory(agent).save(key, &value);
            format!("Saved '{}' for agent '{}'.\n", key, agent)
        }

        _ => format!("memory: unknown subcommand '{}'\n", args[0]),
    }
}

// ---------------------------------------------------------------------------
// Auto-save / auto-load helpers
// ---------------------------------------------------------------------------

/// Generate the system prompt fragment for an agent's memory.
///
/// Called when building the system prompt for an API request. Returns an
/// empty string if the agent has no stored memory.
pub fn system_prompt_for_agent(agent_name: &str) -> String {
    let mem = get_memory(agent_name);
    mem.to_system_prompt()
}

/// Get all agent names and their memory file paths, for VFS persistence.
///
/// The caller should write `serialize_memory(name)` to each path.
pub fn persistence_list() -> Vec<(String, String)> {
    store()
        .iter()
        .filter(|(_, mem)| !mem.is_empty())
        .map(|(name, mem)| (name.clone(), mem.vfs_path()))
        .collect()
}
