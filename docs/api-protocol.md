# Anthropic API Integration

This document covers the Anthropic Messages API client, SSE streaming protocol,
tool use, OAuth authentication, and token persistence.

**Status:** COMPLETE -- The Messages API, SSE streaming, and tool use protocol are all
active and tested. Claude Haiku has been called from bare metal and responded with
token-by-token SSE streaming.

**Source files:**
- `crates/api-client/src/lib.rs` -- Client struct and auth header logic
- `crates/api-client/src/messages.rs` -- Messages API types
- `crates/api-client/src/streaming.rs` -- SSE stream consumer
- `crates/api-client/src/tools.rs` -- Tool use protocol
- `crates/auth/src/lib.rs` -- OAuth device flow and credential types
- `crates/agent/src/lib.rs` -- Agent session lifecycle + tool loop
- `crates/net/src/http.rs` -- Low-level HTTP/1.1 + SSE parsing

---

## Table of Contents

- [Messages API](#messages-api)
- [SSE Streaming Protocol](#sse-streaming-protocol)
- [Tool Use Protocol](#tool-use-protocol)
- [OAuth 2.0 Device Authorization Grant](#oauth-20-device-authorization-grant)
- [Token Persistence](#token-persistence)
- [Client Architecture](#client-architecture)

---

## Messages API

### Endpoint

```
POST https://api.anthropic.com/v1/messages
```

### Request Format

```json
{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 8192,
    "stream": true,
    "messages": [
        {
            "role": "user",
            "content": "Hello, Claude."
        }
    ]
}
```

### Required Headers

| Header | Value | Purpose |
|--------|-------|---------|
| `Content-Type` | `application/json` | Request body format |
| `anthropic-version` | `2023-06-01` | API version pin |
| `x-api-key` | `sk-ant-...` | API key auth (OR use Bearer token) |
| `Authorization` | `Bearer <oauth_token>` | OAuth auth (alternative to x-api-key) |
| `Accept` | `text/event-stream` | Request SSE streaming response |
| `Connection` | `close` | No keep-alive (simplifies bare-metal handling) |

### Response (Non-Streaming)

```json
{
    "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
    "type": "message",
    "role": "assistant",
    "content": [
        {
            "type": "text",
            "text": "Hello! How can I help you today?"
        }
    ],
    "model": "claude-sonnet-4-20250514",
    "stop_reason": "end_turn",
    "stop_sequence": null,
    "usage": {
        "input_tokens": 10,
        "output_tokens": 15
    }
}
```

### How ClaudioOS Sends Requests

Since there is no `reqwest`, no `hyper`, and no `tokio`, the API client builds raw
HTTP/1.1 bytes and sends them over a TLS stream:

```
AnthropicClient
    |
    v  Build JSON body (serde_json, #[no_std] with alloc feature)
    |
    v  Wrap in HttpRequest via anthropic_messages_request()
    |     Sets all required headers (Content-Type, x-api-key,
    |     anthropic-version, Accept: text/event-stream)
    |
    v  Serialize to raw HTTP/1.1 bytes via .to_bytes()
    |     Produces: "POST /v1/messages HTTP/1.1\r\nHost: ...\r\n...\r\n{json}"
    |
    v  Send over TlsStream (encrypted TCP)
    |     TLS record layer encrypts, TCP sends segments
    |
    v  Read response bytes from TlsStream
    |
    v  Parse HTTP headers via HttpResponse::parse_headers()
    |     Check status code (200 = success, 4xx/5xx = error)
    |
    v  If streaming: feed body bytes to parse_sse_events()
    |     Extract content deltas, tool use blocks, usage stats
    |
    v  If non-streaming: parse full JSON response body
```

The helper function in `crates/net/src/http.rs`:

```rust
pub fn anthropic_messages_request(api_key: &str, body_json: &[u8]) -> HttpRequest {
    HttpRequest::post("api.anthropic.com", "/v1/messages", body_json.to_vec())
        .header("Content-Type", "application/json")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Accept", "text/event-stream")
        .header("Connection", "close")
}
```

---

## SSE Streaming Protocol

When `"stream": true` is set in the request, the API responds with Server-Sent
Events. The HTTP response has `Content-Type: text/event-stream` and typically
uses `Transfer-Encoding: chunked`.

### Event Sequence

A complete streaming response produces these event types in order:

```
1. message_start
   data: {"type":"message_start","message":{"id":"msg_...","type":"message",
          "role":"assistant","content":[],"model":"claude-sonnet-4-20250514",
          "stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0}}}

2. content_block_start  (one per content block)
   data: {"type":"content_block_start","index":0,
          "content_block":{"type":"text","text":""}}

3. content_block_delta  (repeated, one per text chunk)
   data: {"type":"content_block_delta","index":0,
          "delta":{"type":"text_delta","text":"Hello"}}

4. content_block_stop
   data: {"type":"content_block_stop","index":0}

5. message_delta
   data: {"type":"message_delta",
          "delta":{"stop_reason":"end_turn","stop_sequence":null},
          "usage":{"output_tokens":15}}

6. message_stop
   data: {"type":"message_stop"}
```

### Processing in ClaudioOS

```
Incoming chunked HTTP body bytes
    |
    v  decode_chunked() -> raw SSE text
    |
    v  parse_sse_events() -> Vec<SseEvent>
    |
    v  For each SseEvent:
       |
       |-- event == "content_block_delta"
       |     Parse JSON: extract delta.text
       |     Write text to the agent's terminal pane
       |     (This is what the user sees as Claude "typing")
       |
       |-- event == "content_block_start" && type == "tool_use"
       |     Begin accumulating tool input JSON
       |     (Tool use is a separate content block)
       |
       |-- event == "content_block_delta" && type == "input_json_delta"
       |     Append partial JSON to tool input accumulator
       |
       |-- event == "content_block_stop"
       |     If tool_use block: parse complete tool input, execute tool
       |     Send tool_result in next conversation turn
       |
       |-- event == "message_delta"
       |     Extract stop_reason and output_tokens usage
       |
       |-- event == "message_stop"
       |     Conversation turn is complete
       |     Update token usage display in status bar
```

### Incremental Buffer Strategy

SSE events can arrive split across TCP segments and TLS records. The consumer
maintains a growing buffer:

```rust
let mut buffer = Vec::new();
loop {
    let new_data = tls.recv(&mut stack, &mut tmp, now)?;
    if new_data == 0 { break; }  // connection closed
    buffer.extend_from_slice(&tmp[..new_data]);

    let (events, consumed) = parse_sse_events(&buffer);
    buffer.drain(..consumed);  // remove processed bytes

    for event in events {
        handle_event(event);
    }
}
```

The `bytes_consumed` return value from `parse_sse_events()` ensures that incomplete
events at the end of the buffer are preserved for the next read cycle.

---

## Tool Use Protocol

When Claude wants to use a tool (file read, web search, code execution, etc.), the
streaming response includes a `tool_use` content block instead of (or alongside)
a `text` content block.

### Tool Use Content Block (SSE)

```
event: content_block_start
data: {"type":"content_block_start","index":1,
       "content_block":{"type":"tool_use","id":"toolu_01A...","name":"read_file","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,
       "delta":{"type":"input_json_delta","partial_json":"{\"path\":\"/etc/hosts\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}
```

The tool input JSON arrives incrementally via `input_json_delta` events. The client
must accumulate all partial JSON fragments and parse the complete input only after
`content_block_stop`.

### Tool Result Response

After executing the tool, the client sends a new message with role `"user"` containing
a `tool_result` content block:

```json
{
    "role": "user",
    "content": [
        {
            "type": "tool_result",
            "tool_use_id": "toolu_01A...",
            "content": "127.0.0.1 localhost\n::1 localhost"
        }
    ]
}
```

The conversation then continues -- Claude processes the tool result and may respond
with more text, request another tool use, or end the turn.

### ClaudioOS Tool Execution Flow

```
1. Detect tool_use in SSE stream (content_block_start with type "tool_use")
2. Accumulate input JSON fragments from input_json_delta events
3. On content_block_stop: parse complete tool input JSON
4. Match tool name and execute:
     edit_file       -> nano-like text editor (crates/editor)
     execute_python  -> python-lite interpreter (crates/python-lite)
     compile_rust    -> Rust compilation via build-server (tools/build-server.py)
     read_file       -> read from FAT32 via fs-persist (stubbed)
     write_file      -> write to FAT32 (stubbed)
     bash/exec       -> NOT AVAILABLE (no shell, no process execution)
5. Build tool_result message with output text
6. Append to conversation history
7. Send as next API call (full history + tool result)
8. Repeat up to max 20 tool rounds per conversation turn
```

### Available Tools

| Tool | Implementation | Description |
|------|---------------|-------------|
| `edit_file` | `crates/editor/` | Nano-like text editor (~400 LOC, 11 tests) |
| `execute_python` | `crates/python-lite/` | Minimal Python interpreter (vars, loops, functions, 28 tests) |
| `compile_rust` | `tools/build-server.py` | Host-side Rust compilation via HTTP |

**Important**: ClaudioOS has no shell, no subprocess execution, and no process
isolation. Tools requiring arbitrary command execution (like `bash`) are not and
cannot be supported. The agent tool set provides file editing, Python execution,
and Rust compilation as alternatives.

---

## OAuth 2.0 Device Authorization Grant

**Source:** `crates/auth/src/lib.rs`

ClaudioOS uses RFC 8628 (Device Authorization Grant) for OAuth authentication. This
flow is ideal for devices without a web browser -- the user completes authorization
on a separate device (phone, laptop).

### Why Device Flow?

ClaudioOS has no web browser. The standard OAuth authorization code flow requires
redirecting the user to a browser. The device flow instead:
1. Displays a short code on the ClaudioOS framebuffer
2. The user visits a URL on their phone/laptop and enters the code
3. ClaudioOS polls for the token in the background

### Flow Diagram

```
ClaudioOS Framebuffer           Anthropic Auth Server       User's Phone
       |                                |                        |
       |  POST /oauth/device/code       |                        |
       |  {"client_id":"...",           |                        |
       |   "scope":"org:read user:read"}|                        |
       |------------------------------->|                        |
       |                                |                        |
       |  {"device_code":"XXXXXX",      |                        |
       |   "user_code":"ABCD-1234",     |                        |
       |   "verification_uri":          |                        |
       |    "https://auth.anthropic.com"|                        |
       |   "interval":5,               |                        |
       |   "expires_in":900}           |                        |
       |<-------------------------------|                        |
       |                                |                        |
       | Display on framebuffer:        |                        |
       | +--------------------------+   |                        |
       | | Go to:                   |   |                        |
       | | https://auth.anthropic.com|  |                        |
       | | Enter code: ABCD-1234    |   |                        |
       | +--------------------------+   |                        |
       |                                |  User opens URL ------>|
       |                                |  Enters ABCD-1234 ---->|
       |                                |  Clicks "Authorize" -->|
       |                                |                        |
       |  POST /oauth/token             |                        |
       |  {"grant_type":                |                        |
       |   "urn:ietf:params:oauth:      |                        |
       |    grant-type:device_code",    |                        |
       |   "device_code":"XXXXXX",      |                        |
       |   "client_id":"..."}           |                        |
       |------------------------------->|                        |
       |                                |                        |
       |  (if pending:                  |                        |
       |   {"error":                    |                        |
       |    "authorization_pending"})   |                        |
       |<-------------------------------|                        |
       |                                |                        |
       |  ... wait interval seconds ... |                        |
       |  ... poll again ...            |                        |
       |------------------------------->|                        |
       |                                |                        |
       |  {"access_token":"sk-ant-oat-..|                        |
       |   "refresh_token":"sk-ant-ort..|                        |
       |   "expires_in":3600,           |                        |
       |   "token_type":"Bearer"}       |                        |
       |<-------------------------------|                        |
       |                                |                        |
       | Persist tokens to FAT32        |                        |
       | Start agent sessions           |                        |
```

### Credential Types

```rust
// crates/auth/src/lib.rs
pub enum Credentials {
    ApiKey(String),                   // From CLAUDIO_API_KEY env var (dev only)
    OAuth {
        access_token: String,         // Bearer token for API calls
        refresh_token: String,        // For getting new access tokens
        expires_at: u64,              // Unix timestamp when access_token expires
    },
}

impl Credentials {
    pub fn is_expired(&self, now_unix: u64) -> bool {
        match self {
            Credentials::ApiKey(_) => false,  // API keys don't expire
            Credentials::OAuth { expires_at, .. } => now_unix >= *expires_at,
        }
    }
}
```

### Device Flow Prompt

```rust
pub struct DeviceFlowPrompt {
    pub verification_uri: String,    // URL user should visit
    pub user_code: String,           // Code to enter (e.g., "ABCD-1234")
    pub expires_in: u32,             // Seconds until code expires
    pub interval: u32,               // Seconds between poll attempts
}
```

### API Key Fallback

The `CLAUDIO_API_KEY` environment variable provides a compile-time API key that
bypasses OAuth entirely (for development):

```bash
CLAUDIO_API_KEY=sk-ant-api03-xxx cargo build
```

### Token Refresh

A background async task (`token_refresh_loop`) monitors the token's `expires_at`
timestamp and refreshes before expiration:

```
POST /oauth/token
{
    "grant_type": "refresh_token",
    "refresh_token": "<refresh_token>",
    "client_id": "<client_id>"
}
```

Refreshed tokens are persisted to FAT32 immediately.

### Boot-Time Auth Gate

```
main_async()
    |
    v
Check FAT32 for saved credentials (auth/token.json)
    |
    +-- Found valid (non-expired) token -> use it
    |
    +-- Found expired token -> attempt refresh
    |     +-- Refresh success -> use new token, persist
    |     +-- Refresh failed  -> start device flow from scratch
    |
    +-- No saved credentials
    |     +-- CLAUDIO_API_KEY set -> use API key
    |     +-- Not set -> start device flow
    |
    v
Credentials obtained
    |
    v
Spawn token_refresh_loop(creds) as background async task
    |
    v
Start agent sessions
```

---

## Token Persistence

**Source:** `crates/fs-persist/` (stubbed, Phase 3)

### File Layout on FAT32

```
/claudio/
    config.json       Client ID, model preferences, log level
    auth/
        token.json    Access + refresh tokens with expiry
        api_key.txt   Optional baked-in key (dev only)
    agents/
        session_0.json   Conversation history for agent 0
        session_1.json   Conversation history for agent 1
    logs/
        boot.log         Serial log mirror
```

### Token File Format (auth/token.json)

```json
{
    "access_token": "sk-ant-oat-...",
    "refresh_token": "sk-ant-ort-...",
    "expires_at": 1711900000,
    "token_type": "Bearer"
}
```

### Filesystem Implementation

The `fatfs` crate (v0.3, `default-features = false, features = ["alloc"]`) provides
FAT32 support. The `fs-persist` crate wraps it with typed accessors:

```rust
pub fn load_credentials() -> Option<Credentials>;
pub fn save_credentials(creds: &Credentials) -> Result<(), FsError>;
pub fn load_config() -> Config;
pub fn save_agent_session(id: usize, history: &[Message]) -> Result<(), FsError>;
```

---

## Client Architecture

**Source:** `crates/api-client/src/lib.rs`

### AnthropicClient

```rust
pub struct AnthropicClient {
    pub api_key: Option<String>,       // From CLAUDIO_API_KEY or None
    pub oauth_token: Option<String>,   // From OAuth device flow or None
    pub model: String,                 // Default: "claude-sonnet-4-20250514"
    pub max_tokens: u32,               // Default: 8192
}
```

The client is stateless per-call -- the caller provides the TLS stream for each
request. This avoids long-lived connections which would complicate error handling
in a bare-metal environment.

### Auth Header Selection

API key takes precedence over OAuth token:

```rust
pub fn auth_header(&self) -> Option<String> {
    if let Some(ref key) = self.api_key {
        Some(format!("x-api-key: {}", key))
    } else if let Some(ref token) = self.oauth_token {
        Some(format!("Authorization: Bearer {}", token))
    } else {
        None
    }
}
```

### Conversation State

Each agent session maintains its own conversation history as a `Vec<Message>`.
The Messages API is stateless -- the server does not remember previous turns.
The full history is sent with every API call.

The conversation history is periodically persisted to FAT32 so sessions survive
reboots.
