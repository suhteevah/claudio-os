# Anthropic API Integration

This document covers the Anthropic Messages API client and OAuth authentication
system. These are Phase 3 components.

**Source files:**
- `crates/api-client/src/lib.rs` -- Client struct and auth header logic
- `crates/api-client/src/messages.rs` -- Messages API types (planned)
- `crates/api-client/src/streaming.rs` -- SSE stream consumer (planned)
- `crates/api-client/src/tools.rs` -- Tool use protocol (planned)
- `crates/auth/src/lib.rs` -- OAuth device flow and credential types
- `crates/net/src/http.rs` -- Low-level HTTP + SSE parsing

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
| `Accept` | `text/event-stream` | Request streaming response |

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

Since there is no `reqwest` or `hyper`, the API client:

1. Builds raw HTTP/1.1 bytes via `HttpRequest::post()` (see `crates/net/src/http.rs`)
2. Sends them over a `TlsStream` to port 443
3. Reads the response bytes
4. Parses headers via `HttpResponse::parse_headers()`
5. For streaming, feeds body bytes to `parse_sse_events()`

```
AnthropicClient
    |
    v  build JSON body (serde_json, no_std)
    |
    v  wrap in HttpRequest via anthropic_messages_request()
    |
    v  serialize to bytes via .to_bytes()
    |
    v  send over TlsStream
    |
    v  read response bytes
    |
    v  parse SSE events
    |
    v  extract content deltas, tool use blocks
```

---

## SSE Streaming Protocol

When `"stream": true` is set in the request, the API responds with Server-Sent
Events. The response has `Content-Type: text/event-stream` and `Transfer-Encoding: chunked`.

### Event Types

The stream produces these event types in order:

```
1. message_start
   data: {"type":"message_start","message":{"id":"msg_...","type":"message",...}}

2. content_block_start
   data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

3. content_block_delta (repeated, one per text chunk)
   data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

4. content_block_stop
   data: {"type":"content_block_stop","index":0}

5. message_delta
   data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}

6. message_stop
   data: {"type":"message_stop"}
```

### Processing in ClaudioOS

The SSE parser (`parse_sse_events()` in `crates/net/src/http.rs`) processes the raw
byte stream incrementally:

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
       |     Parse data JSON, extract delta.text
       |     Write text to the agent's terminal pane
       |
       |-- event == "content_block_start" && type == "tool_use"
       |     Begin accumulating tool input JSON
       |
       |-- event == "message_stop"
       |     Mark conversation turn as complete
       |
       |-- event == "message_delta"
             Extract stop_reason, usage stats
```

### Incremental Buffer Strategy

SSE events can arrive split across TCP segments and TLS records. The consumer
maintains a buffer and calls `parse_sse_events()` repeatedly. The function returns
`bytes_consumed` so the caller can remove processed bytes and append new ones:

```rust
let mut buffer = Vec::new();
loop {
    let new_data = tls.receive(&mut stack, &mut tmp, now)?;
    buffer.extend_from_slice(&tmp[..new_data]);

    let (events, consumed) = parse_sse_events(&buffer);
    buffer.drain(..consumed);

    for event in events {
        handle_event(event);
    }
}
```

---

## Tool Use Protocol

When Claude wants to use a tool (file read, web search, code execution, etc.), the
streaming response includes a `tool_use` content block:

### Tool Use Content Block

```json
{
    "type": "content_block_start",
    "index": 1,
    "content_block": {
        "type": "tool_use",
        "id": "toolu_01A...",
        "name": "read_file",
        "input": {}
    }
}
```

Followed by input deltas:

```json
{
    "type": "content_block_delta",
    "index": 1,
    "delta": {
        "type": "input_json_delta",
        "partial_json": "{\"path\":\"/etc/hosts\"}"
    }
}
```

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

### ClaudioOS Tool Execution

In ClaudioOS, tool execution happens within the agent session's async task:

```
1. Detect tool_use in SSE stream
2. Accumulate input JSON from input_json_delta events
3. Parse tool name and input
4. Execute tool:
   - read_file  -> read from FAT32 via fs-persist
   - write_file -> write to FAT32 via fs-persist
   - bash       -> not available (no shell)
5. Build tool_result message
6. Send as next turn in conversation
```

**Important**: ClaudioOS has no shell or process execution. Tools that require
running arbitrary commands (like `bash`) are not supported. The agent will be
configured with a restricted tool set appropriate for the bare-metal environment.

---

## OAuth 2.0 Device Authorization Grant

**Source:** `crates/auth/src/lib.rs`

ClaudioOS uses RFC 8628 (Device Authorization Grant) for OAuth authentication. This
flow is ideal for devices without a web browser -- the user completes authorization
on a separate device (phone, laptop).

### Flow Diagram

```
ClaudioOS                          Anthropic Auth Server
    |                                      |
    |  POST /oauth/device/code             |
    |  {client_id, scope}                  |
    |------------------------------------->|
    |                                      |
    |  {device_code, user_code,            |
    |   verification_uri, interval,        |
    |   expires_in}                        |
    |<-------------------------------------|
    |                                      |
    |  Display to user on framebuffer:     |
    |  "Go to: https://auth.anthropic.com" |
    |  "Enter code: ABCD-1234"            |
    |                                      |
    |                                      |    User visits URL on phone,
    |                                      |    enters code, authorizes
    |                                      |
    |  POST /oauth/token (poll every N sec)|
    |  {grant_type=device_code,            |
    |   device_code, client_id}            |
    |------------------------------------->|
    |                                      |
    |  (if pending: {"error":              |
    |   "authorization_pending"})          |
    |<-------------------------------------|
    |                                      |
    |  ... poll again ...                  |
    |------------------------------------->|
    |                                      |
    |  {access_token, refresh_token,       |
    |   expires_in, token_type}            |
    |<-------------------------------------|
    |                                      |
    |  Persist tokens to FAT32             |
    |  Start agent sessions                |
```

### Credential Types

```rust
pub enum Credentials {
    ApiKey(String),
    OAuth {
        access_token: String,
        refresh_token: String,
        expires_at: u64,
    },
}
```

The `CLAUDIO_API_KEY` environment variable provides a compile-time API key that
bypasses OAuth entirely (for development). In production, OAuth is the expected path.

### Token Refresh

A background async task (`token_refresh_loop`) monitors the token's `expires_at`
timestamp and refreshes it using the `refresh_token` before expiration:

```
POST /oauth/token
{
    "grant_type": "refresh_token",
    "refresh_token": "<refresh_token>",
    "client_id": "<client_id>"
}
```

The refreshed tokens are persisted to FAT32 immediately.

### Boot-Time Auth Gate

The authentication flow gates agent session startup:

```
kernel_main
    |
    v
main_async()
    |
    v
Check FAT32 for saved credentials
    |
    +-- Found valid token -> use it
    |
    +-- Found expired token -> attempt refresh
    |     |
    |     +-- Refresh success -> use new token
    |     +-- Refresh failed -> start device flow
    |
    +-- No saved credentials -> start device flow
    |
    v
auth::authenticate() returns Credentials
    |
    v
Spawn token_refresh_loop(creds) as background task
    |
    v
Start agent sessions
```

---

## Token Persistence

**Source:** `crates/fs-persist/` (stubbed)

Tokens are stored on a FAT32 partition (either the boot disk's data partition or a
separate QEMU disk image).

### File Layout

```
/claudio/
    config.json       -- client_id, model preferences, log level
    auth/
        token.json    -- {access_token, refresh_token, expires_at}
        api_key.txt   -- optional baked-in key (dev only)
    agents/
        session_0.json -- conversation history for agent 0
        session_1.json -- conversation history for agent 1
    logs/
        boot.log      -- serial log mirror
```

### Token File Format

```json
{
    "access_token": "sk-ant-oat-...",
    "refresh_token": "sk-ant-ort-...",
    "expires_at": 1711900000,
    "token_type": "Bearer"
}
```

The `fatfs` crate (v0.4) provides the FAT32 filesystem implementation. The
`fs-persist` crate wraps it with typed accessors:

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
    pub api_key: Option<String>,
    pub oauth_token: Option<String>,
    pub model: String,         // default: "claude-sonnet-4-20250514"
    pub max_tokens: u32,       // default: 8192
}
```

The client is stateless per-call. The caller provides the TLS stream for each
request. This design avoids keeping long-lived connections (which would complicate
error handling in a bare-metal environment).

### Auth Header Selection

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

API key takes precedence over OAuth token. This allows the `CLAUDIO_API_KEY`
environment variable to override OAuth in development.

### Conversation State

Each agent session maintains its own conversation history as a `Vec<Message>`.
Messages are appended after each turn and the full history is sent with every
API call (the Messages API is stateless -- the server does not remember previous
turns).

The conversation history is periodically persisted to FAT32 so sessions survive
reboots.
