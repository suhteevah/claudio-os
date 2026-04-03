# ClaudioOS Multi-Agent System

## Overview

ClaudioOS runs multiple Claude AI coding agents simultaneously, each in its own
terminal pane with independent conversation state. The system supports two
authentication modes: direct claude.ai Max subscription access, and standard
Anthropic API key.

---

## Authentication Modes

### ClaudeAi Mode (Max Subscription)

Connects directly to `claude.ai` using the web chat API. This uses the same
protocol as the Claude web interface.

**Flow:**
1. On first boot, ClaudioOS sends an email login request to `claude.ai/api/auth`
2. User enters the verification code received via email
3. ClaudioOS receives a `sessionKey` cookie (valid for 28 days)
4. Session is persisted to `target/session.txt` via QEMU `fw_cfg`
5. On subsequent boots, the session is loaded automatically

**Headers:**
- `anthropic-client-platform: web`
- `anthropic-client-sha: <build hash>`
- Custom `source: "claude"` in request bodies
- Standard browser-like headers for compatibility

**Endpoints:**
- `claude.ai/api/auth/send_email_code` -- initiate login
- `claude.ai/api/auth/verify_email_code` -- complete login
- `claude.ai/api/organizations/<org_id>/chat_conversations` -- create/list conversations
- `claude.ai/api/organizations/<org_id>/chat_conversations/<id>/completion` -- send messages (SSE)

### ApiKey Mode

Standard Anthropic Messages API with an API key.

**Configuration:**
- Compile-time: `CLAUDIO_API_KEY=sk-ant-api03-... cargo build`
- Runtime: auth relay server (`tools/auth-relay.py`) on host port 8444

**Endpoint:** `api.anthropic.com/v1/messages`

**Headers:**
- `x-api-key: <key>`
- `anthropic-version: 2023-06-01`
- `Content-Type: application/json`

---

## Session Persistence

Sessions survive reboots via QEMU's `fw_cfg` mechanism:

1. **Save**: When ClaudioOS obtains a session, it writes credentials to serial
2. **Host capture**: `run.ps1` saves serial output to `target/session.txt`
3. **Load**: On next boot, QEMU passes `-fw_cfg name=opt/claudio/session,file=target/session.txt`
4. **Kernel reads**: The kernel reads `fw_cfg` at boot and restores the session

This enables conversation reuse across reboots without re-authentication.

---

## Session Auto-Refresh (`kernel/src/session_manager.rs`)

The session manager monitors cookie expiry and automatically refreshes tokens
before they expire, preventing stale sessions during long uptime.

### Refresh Flow

1. On auth completion, the session manager parses the JWT expiry from the
   sessionKey cookie (base64url-decoded `exp` claim)
2. The dashboard event loop calls `periodic_check()` roughly every 60 minutes
3. When less than 24 hours remain before expiry, a refresh is attempted
4. Refresh calls `GET /api/auth/session` with the existing cookie
5. If the session is still valid, new Set-Cookie headers extend the expiry
6. Updated cookies are emitted via the `SAVE_SESSION:` serial marker for
   host-side persistence

### Warning Thresholds

| Threshold | Action |
|-----------|--------|
| 24 hours | Log warning about approaching expiry |
| 2 hours | Elevated warning |
| 30 minutes | Critical warning |
| 0 (expired) | Flag `needs_reauth` for full re-authentication |

---

## Dashboard (tmux-style Panes)

The dashboard provides a split-pane terminal interface, similar to tmux.
It supports 6 pane types:

| Pane Type | Description |
|-----------|-------------|
| **Agent** | Claude AI coding agent with tool loop |
| **Shell** | AI-native shell with 28 builtins |
| **Browser** | Text-mode web browser (wraith-based) |
| **FileManager** | Visual directory browser with file operations |
| **SysMonitor** | Real-time CPU/memory/network/agent stats |
| **Screensaver** | 5 modes: starfield, matrix, bouncing, pipes, clock |

### Layout

```
+---------------------------+---------------------------+
|                           |                           |
|   Agent 1 (focused)       |   Shell                   |
|   claude session          |   28 builtins + AI        |
|   [typing/streaming]      |   [idle]                  |
|                           |                           |
+---------------------------+---------------------------+
|                           |                           |
|   Browser                 |   System Monitor          |
|   https://example.com     |   CPU [###------] 35%     |
|   [browsing]              |   MEM [######---] 64%     |
|                           |                           |
+---------------------------+---------------------------+
```

### Keyboard Shortcuts

All shortcuts use a **Ctrl+B prefix** (press Ctrl+B, release, then press the
action key), matching tmux conventions.

| Shortcut | Action |
|----------|--------|
| `Ctrl+B "` | Split pane horizontally |
| `Ctrl+B %` | Split pane vertically |
| `Ctrl+B o` | Switch focus to next pane |
| `Ctrl+B n` | Switch focus to next pane |
| `Ctrl+B p` | Switch focus to previous pane |
| `Ctrl+B c` | Create new agent session in current pane |
| `Ctrl+B s` | Create new shell pane |
| `Ctrl+B x` | Close current pane / kill agent |
| `Ctrl+B ,` | Rename current agent |
| `Ctrl+B Up/Down/Left/Right` | Move focus directionally |

### Layout Engine

The layout uses a binary tree of viewports:

```rust
pub enum LayoutNode {
    Leaf { pane: Pane },
    Split {
        direction: SplitDirection, // Horizontal or Vertical
        ratio: f32,                // 0.0 to 1.0
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}
```

Each `Pane` contains:
- A `Terminal` instance (VTE parser + character grid)
- Viewport coordinates (x, y, width, height) in pixels
- Scroll position and history buffer
- Reference to the agent session, shell, browser, file manager, or sysmon (depending on type)

---

## Agent Sessions

Each agent session is an async task with its own:
- Conversation history (message list)
- Authentication credentials (shared via reference)
- Terminal pane (for rendering output)
- Tool execution context
- IPC registration (message inbox, agent name)

### Session Lifecycle

```
1. User creates agent (Ctrl+B c)
2. Agent session spawns as async task
3. Agent registered with IPC message bus
4. Welcome banner displayed (themed)
5. User types a prompt
6. Prompt sent to Claude (API key or claude.ai)
7. SSE stream received, tokens rendered to pane
8. If tool_use: execute tool, send result, repeat (up to 20 rounds)
9. Final response displayed
10. Wait for next user input
```

### Tool Loop

The agent tool loop handles multi-turn tool use:

```
User prompt
  |
  v
Send to Claude API (with tools declaration)
  |
  v
Parse response:
  +-- text content -> render to pane
  +-- tool_use content -> execute tool
        |
        v
      Build tool_result message
        |
        v
      Send back to Claude (with tool_result)
        |
        v
      Parse response (repeat up to 20 rounds)
  |
  v
Final text response -> render to pane
```

---

## Available Tools

Tools are declared in the API request and executed locally when Claude requests them.

| Tool | Description | Implementation |
|------|-------------|----------------|
| `file_read` | Read a file's contents | VFS `read_file()` |
| `file_write` | Write content to a file | VFS `write_file()` |
| `edit_file` | Edit a file (nano-like operations) | `claudio-editor` crate |
| `execute_python` | Run Python code | `python-lite` interpreter |
| `execute_javascript` | Run JavaScript code | `js-lite` evaluator |
| `compile_rust` | Compile Rust code | `rustc-lite` + Cranelift, or host build server |
| `list_files` | List directory contents | VFS `list_dir()` |
| `search_files` | Search for files by pattern | VFS traversal |
| `send_to_agent` | Send a message to another agent | IPC message bus |
| `read_agent_messages` | Read messages from inbox | IPC message bus |
| `list_agents_ipc` | List all agents available for messaging | IPC registry |
| `create_channel` | Create a named data channel | IPC channel registry |
| `channel_write` | Write data to a named channel | IPC channel |
| `channel_read` | Read data from a named channel | IPC channel |
| `shared_memory_write` | Write to shared memory region | IPC shared memory |
| `shared_memory_read` | Read from shared memory region | IPC shared memory |

---

## Inter-Agent Communication (IPC) (`kernel/src/ipc.rs`)

The IPC system enables Claude agents to collaborate by sending messages,
streaming data through channels, and sharing memory regions.

### Components

| Component | Description |
|-----------|-------------|
| **MessageBus** | Global per-agent inboxes. Messages accumulate until drained. |
| **Channel** | Named SPSC ring buffer (4 KiB default) for streaming data between agents. |
| **SharedMemory** | Named byte buffers that grow on demand, readable/writable by any agent. |

### Agent Registration

When an agent session starts, it is registered with the IPC system:

```rust
ipc.bus.register_agent(agent_id, agent_name);
```

This creates an inbox and a name -> ID mapping. Agents can send messages
by name or numeric ID, or broadcast to all agents.

### IPC Tools for Claude

8 IPC tools are exposed to Claude agents via the tool-use protocol:

- `send_to_agent` -- send a message to a specific agent or broadcast
- `read_agent_messages` -- drain pending messages from inbox
- `list_agents_ipc` -- list all registered agents
- `create_channel` -- create a named data channel
- `channel_write` -- write data to a channel
- `channel_read` -- read data from a channel
- `shared_memory_write` -- write to a shared memory region
- `shared_memory_read` -- read from a shared memory region

### Example: Agent Collaboration

```
Agent 1: "Research the x86 APIC and send me a summary"
  -> Claude calls send_to_agent(to="agent-2", message="Research x86 APIC...")
  -> Agent 2 receives the message via read_agent_messages
  -> Agent 2 researches and sends results back via send_to_agent(to="agent-1", ...)
  -> Agent 1 reads the results and continues its work
```

---

## Conversation Management (`kernel/src/conversations.rs`)

Manages claude.ai conversations: listing, selecting, renaming, and deleting.

### Shell Commands

| Command | Description |
|---------|-------------|
| `conversations` / `convos` | List the 20 most recent conversations |
| `conv use <uuid>` | Switch the active conversation for this agent |
| `conv rename <uuid> <name>` | Rename a conversation |
| `conv delete <uuid>` | Delete a conversation |
| `conv new [name]` | Start a new conversation (clears active conv_id) |

### API Integration

Uses the claude.ai REST API:
- `GET /api/organizations/{org}/chat_conversations` -- list
- `PATCH /api/organizations/{org}/chat_conversations/{id}` -- rename
- `DELETE /api/organizations/{org}/chat_conversations/{id}` -- delete

Per-agent active conversation tracking allows different agents to use
different conversations simultaneously.

---

## Conversation State

Each agent maintains a message list:

```rust
pub struct Conversation {
    pub id: String,              // conversation UUID
    pub messages: Vec<Message>,  // alternating user/assistant messages
    pub model: String,           // e.g., "claude-sonnet-4-6"
    pub system_prompt: Option<String>,
}

pub struct Message {
    pub role: Role,              // User or Assistant
    pub content: Vec<ContentBlock>,
}

pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String },
}
```

In ClaudeAi mode, conversations persist on claude.ai servers and can be
resumed across reboots using the saved session.
