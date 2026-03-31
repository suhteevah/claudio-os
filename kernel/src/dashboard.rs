//! Multi-agent dashboard — keyboard routing, pane management, agent sessions.
//!
//! This module wires together:
//! - `claudio_agent::Dashboard` (agent session lifecycle)
//! - `claudio_terminal::Layout` (split-pane layout tree)
//! - `crate::keyboard::ScancodeStream` (async keyboard input)
//! - `crate::framebuffer` (GOP pixel output)
//!
//! Keyboard input uses a tmux-style Ctrl+B prefix for pane management commands.
//! Regular keypresses are forwarded to the focused agent session's input buffer.
//! Enter submits the buffered input to the focused agent's conversation.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use pc_keyboard::DecodedKey;

use claudio_agent::{AgentState, Dashboard};
use claudio_net::{Instant, NetworkStack};
use claudio_terminal::{Layout, SplitDirection};

use crate::keyboard::ScancodeStream;

// ---------------------------------------------------------------------------
// Framebuffer DrawTarget adapter
// ---------------------------------------------------------------------------

/// Wraps the kernel's global `framebuffer::put_pixel` function into a
/// [`claudio_terminal::DrawTarget`] so the layout/pane renderer can draw.
struct FbDrawTarget {
    width: usize,
    height: usize,
}

impl FbDrawTarget {
    fn new() -> Self {
        Self {
            width: crate::framebuffer::width(),
            height: crate::framebuffer::height(),
        }
    }
}

impl claudio_terminal::DrawTarget for FbDrawTarget {
    fn put_pixel(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8) {
        crate::framebuffer::put_pixel(x, y, r, g, b);
    }

    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }
}

// ---------------------------------------------------------------------------
// Per-agent input buffer
// ---------------------------------------------------------------------------

/// Input line buffer for each agent session. Characters accumulate here until
/// Enter is pressed, at which point the buffer is drained and submitted.
struct InputBuffer {
    /// The agent session id this buffer belongs to.
    agent_id: usize,
    /// Characters typed so far (before Enter).
    buf: String,
}

impl InputBuffer {
    fn new(agent_id: usize) -> Self {
        Self {
            agent_id,
            buf: String::new(),
        }
    }

    fn push(&mut self, c: char) {
        self.buf.push(c);
    }

    fn backspace(&mut self) {
        self.buf.pop();
    }

    fn drain(&mut self) -> String {
        core::mem::replace(&mut self.buf, String::new())
    }

    fn as_str(&self) -> &str {
        &self.buf
    }
}

// ---------------------------------------------------------------------------
// Prefix key state machine
// ---------------------------------------------------------------------------

/// Tracks whether we are in "prefix mode" (Ctrl+B was pressed, waiting for
/// the next key to determine the command).
#[derive(Debug, Clone, Copy, PartialEq)]
enum PrefixState {
    /// Normal mode — keys go to the focused agent.
    Normal,
    /// Prefix key (Ctrl+B) was pressed — next key is a command.
    AwaitingCommand,
}

// ---------------------------------------------------------------------------
// Dashboard runner
// ---------------------------------------------------------------------------

/// Main entry point for the multi-agent dashboard.
///
/// This replaces the simple keyboard echo loop in `main_async`. It:
/// 1. Creates the initial layout (single pane) and agent session.
/// 2. Enters an async loop reading keyboard events.
/// 3. Routes prefix-key commands to layout/dashboard operations.
/// 4. Routes regular keys to the focused agent's input buffer.
/// 5. On Enter, submits the input buffer to the agent's conversation and
///    dispatches an API call.
/// 6. Renders all panes after every input event.
pub async fn run_dashboard(
    stack: &mut NetworkStack,
    api_key: &str,
    fb_width: usize,
    fb_height: usize,
    now: fn() -> Instant,
) {
    log::info!("[dashboard] starting multi-agent dashboard ({}x{})", fb_width, fb_height);

    // -- Initialise layout + first agent session ----------------------------

    let mut layout = Layout::new(fb_width, fb_height);
    let mut dashboard = Dashboard::new();
    let mut input_buffers: Vec<InputBuffer> = Vec::new();

    // Create the first agent session bound to pane 0.
    let first_pane_id = layout.focused_pane_id();
    let first_agent_id = dashboard.create_session(String::from("agent-0"), first_pane_id);
    input_buffers.push(InputBuffer::new(first_agent_id));

    // Draw initial prompt into the first pane.
    render_prompt(&mut layout, &dashboard, &input_buffers);
    render_all(&layout);

    // -- Keyboard event loop ------------------------------------------------

    let stream = ScancodeStream::new();
    let mut prefix_state = PrefixState::Normal;

    loop {
        let key = stream.next_key().await;

        match key {
            DecodedKey::Unicode(c) => {
                match prefix_state {
                    PrefixState::AwaitingCommand => {
                        prefix_state = PrefixState::Normal;
                        handle_prefix_command(
                            c,
                            &mut layout,
                            &mut dashboard,
                            &mut input_buffers,
                        );
                    }
                    PrefixState::Normal => {
                        // Ctrl+B detection: pc-keyboard with HandleControl::Ignore
                        // delivers Ctrl+letter as the raw control code. Ctrl+B = 0x02.
                        if c == '\x02' {
                            prefix_state = PrefixState::AwaitingCommand;
                            log::debug!("[dashboard] prefix key (Ctrl+B) pressed");
                            continue;
                        }

                        // Enter key — submit input to focused agent.
                        if c == '\n' || c == '\r' {
                            submit_input(
                                &mut layout,
                                &mut dashboard,
                                &mut input_buffers,
                                stack,
                                api_key,
                                now,
                            ).await;
                        } else if c == '\x08' || c == '\x7f' {
                            // Backspace / DEL — remove last character from input buffer.
                            if let Some(buf) = focused_input_buffer_mut(&mut input_buffers, &dashboard) {
                                buf.backspace();
                            }
                        } else if !c.is_control() || c == '\t' {
                            // Regular printable character or tab — append to input buffer.
                            if let Some(buf) = focused_input_buffer_mut(&mut input_buffers, &dashboard) {
                                buf.push(c);
                            }
                        }
                    }
                }
            }
            DecodedKey::RawKey(k) => {
                // If we were waiting for a prefix command and got a raw key, cancel.
                if prefix_state == PrefixState::AwaitingCommand {
                    prefix_state = PrefixState::Normal;
                    log::debug!("[dashboard] prefix cancelled by raw key: {:?}", k);
                }
                // Raw keys (arrows, function keys, etc.) are logged but not routed yet.
                log::trace!("[dashboard] raw key: {:?}", k);
            }
        }

        // Re-render prompt + panes after every keypress.
        render_prompt(&mut layout, &dashboard, &input_buffers);
        render_all(&layout);
    }
}

// ---------------------------------------------------------------------------
// Prefix command dispatch
// ---------------------------------------------------------------------------

/// Handle the key pressed after Ctrl+B.
fn handle_prefix_command(
    c: char,
    layout: &mut Layout,
    dashboard: &mut Dashboard,
    input_buffers: &mut Vec<InputBuffer>,
) {
    match c {
        // Split horizontal: Ctrl+B then "
        '"' => {
            log::info!("[dashboard] split horizontal");
            layout.split(SplitDirection::Horizontal);
            let new_pane_id = layout.focused_pane_id();
            let n = dashboard.sessions.len();
            let agent_id = dashboard.create_session(
                format!("agent-{}", n),
                new_pane_id,
            );
            input_buffers.push(InputBuffer::new(agent_id));
            // Move dashboard focus to match the new layout focus.
            sync_dashboard_focus(layout, dashboard);
        }

        // Split vertical: Ctrl+B then %
        '%' => {
            log::info!("[dashboard] split vertical");
            layout.split(SplitDirection::Vertical);
            let new_pane_id = layout.focused_pane_id();
            let n = dashboard.sessions.len();
            let agent_id = dashboard.create_session(
                format!("agent-{}", n),
                new_pane_id,
            );
            input_buffers.push(InputBuffer::new(agent_id));
            sync_dashboard_focus(layout, dashboard);
        }

        // Focus next pane: Ctrl+B then n
        'n' => {
            log::info!("[dashboard] focus next");
            layout.focus_next();
            dashboard.focus_next();
        }

        // Focus previous pane: Ctrl+B then p
        'p' => {
            log::info!("[dashboard] focus prev");
            layout.focus_prev();
            dashboard.focus_prev();
        }

        // New agent session: Ctrl+B then c
        'c' => {
            log::info!("[dashboard] new agent (split horizontal)");
            layout.split(SplitDirection::Horizontal);
            let new_pane_id = layout.focused_pane_id();
            let n = dashboard.sessions.len();
            let agent_id = dashboard.create_session(
                format!("agent-{}", n),
                new_pane_id,
            );
            input_buffers.push(InputBuffer::new(agent_id));
            sync_dashboard_focus(layout, dashboard);
        }

        // Close focused pane: Ctrl+B then x
        'x' => {
            if layout.pane_count() <= 1 {
                log::warn!("[dashboard] cannot close last pane");
                return;
            }
            log::info!("[dashboard] close focused pane");

            // Remove the agent session and its input buffer.
            if let Some(session) = dashboard.focused_session() {
                let agent_id = session.id;
                input_buffers.retain(|b| b.agent_id != agent_id);
            }
            dashboard.close_focused();
            layout.close_focused();
            sync_dashboard_focus(layout, dashboard);
        }

        other => {
            log::debug!("[dashboard] unknown prefix command: {:?}", other);
        }
    }
}

// ---------------------------------------------------------------------------
// Input submission
// ---------------------------------------------------------------------------

/// Submit the focused agent's input buffer: add it to the conversation,
/// transition to Thinking, and run the full API call + tool-use loop.
///
/// This replaces the previous single-shot API call with the tool-use loop
/// from `agent_loop::run_tool_loop`. The model can now invoke tools
/// (file_read, file_write, list_directory, execute_command) and have the
/// results fed back automatically until a final text response is produced.
async fn submit_input(
    layout: &mut Layout,
    dashboard: &mut Dashboard,
    input_buffers: &mut Vec<InputBuffer>,
    stack: &mut NetworkStack,
    api_key: &str,
    now: fn() -> Instant,
) {
    use crate::agent_loop::{run_tool_loop, ToolLoopOutcome};

    let agent_id = match dashboard.focused_session() {
        Some(s) => s.id,
        None => return,
    };

    // Drain the input buffer.
    let input_text = match input_buffers.iter_mut().find(|b| b.agent_id == agent_id) {
        Some(buf) => buf.drain(),
        None => return,
    };

    if input_text.is_empty() {
        return;
    }

    log::info!("[dashboard] agent {} input: {}", agent_id, input_text);

    // Write the user's input into the pane as a visual echo.
    let pane_id = dashboard.focused_session().map(|s| s.pane_id).unwrap_or(0);
    if let Some(pane) = layout.pane_by_id_mut(pane_id) {
        pane.write_str(&format!("\r\n\x1b[32m> {}\x1b[0m\r\n", input_text));
    }

    // Feed input to the agent conversation.
    let timestamp = now().total_millis() as u64;
    let ready = match dashboard.session_by_id_mut(agent_id) {
        Some(session) => session.handle_input(input_text, timestamp),
        None => false,
    };

    if !ready {
        return;
    }

    // Render "thinking..." indicator.
    if let Some(pane) = layout.pane_by_id_mut(pane_id) {
        pane.write_str("\x1b[33m[thinking...]\x1b[0m");
    }
    render_all(layout);

    // --- Run the full API call + tool-use loop ------------------------------
    // Collect tool call info into a Vec since we can't borrow layout inside
    // the closure (the session borrow from dashboard would conflict).
    let mut tool_log: Vec<(String, String, String, bool)> = Vec::new();

    let outcome = {
        let session = match dashboard.session_by_id_mut(agent_id) {
            Some(s) => s,
            None => return,
        };

        run_tool_loop(session, stack, api_key, now, |info| {
            log::info!(
                "[dashboard] tool: {}({}) -> {}",
                info.name,
                info.summary,
                if info.is_error { "ERROR" } else { "ok" }
            );
            tool_log.push((
                info.name.clone(),
                info.summary.clone(),
                info.result_preview.clone(),
                info.is_error,
            ));
        })
    };

    // Display tool calls in the pane.
    for (name, summary, preview, is_error) in &tool_log {
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            if *is_error {
                pane.write_str(&format!(
                    "\r\n\x1b[33m[tool] {}({})\x1b[31m -> error: {}\x1b[0m\r\n",
                    name, summary, preview
                ));
            } else {
                pane.write_str(&format!(
                    "\r\n\x1b[33m[tool] {}({})\x1b[90m -> {}\x1b[0m\r\n",
                    name, summary, preview
                ));
            }
        }
    }

    // Display the final outcome.
    match outcome {
        ToolLoopOutcome::Text(text) => {
            if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                pane.write_str(&format!("\r\n\x1b[36m{}\x1b[0m\r\n", text));
            }
        }
        ToolLoopOutcome::Error(e) => {
            log::error!("[dashboard] agent {} tool loop error: {}", agent_id, e);
            if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                pane.write_str(&format!("\r\n\x1b[31m[error: {}]\x1b[0m\r\n", e));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Render the prompt line (with input buffer contents) into the focused pane.
///
/// This writes the prompt at the bottom of the pane content. For simplicity,
/// we render the prompt as part of the pane's VTE stream — the cursor is
/// already managed by the pane.
fn render_prompt(
    layout: &mut Layout,
    dashboard: &Dashboard,
    input_buffers: &[InputBuffer],
) {
    let session = match dashboard.focused_session() {
        Some(s) => s,
        None => return,
    };

    let input_text = input_buffers
        .iter()
        .find(|b| b.agent_id == session.id)
        .map(|b| b.as_str())
        .unwrap_or("");

    let state_indicator = match session.state {
        AgentState::Idle => "[idle]",
        AgentState::WaitingForInput => "",
        AgentState::Thinking => "[thinking]",
        AgentState::ToolExecuting => "[tool]",
        AgentState::Streaming => "[streaming]",
        AgentState::Error => "[ERROR]",
    };

    // Build the status/prompt line. We use ANSI escapes to:
    //   - Save cursor position (\x1b[s)
    //   - Move to the last row (\x1b[{rows};1H)
    //   - Clear the line (\x1b[2K)
    //   - Write the prompt
    //   - Restore cursor position (\x1b[u)
    //
    // This way the prompt is always at the bottom without disrupting content.
    let pane_id = session.pane_id;
    if let Some(pane) = layout.pane_by_id_mut(pane_id) {
        let rows = pane.rows();
        // Save cursor, move to last row, clear line, draw prompt, restore.
        let prompt_line = format!(
            "\x1b[s\x1b[{};1H\x1b[2K\x1b[33m{}\x1b[37m {} \x1b[32m> \x1b[0m{}\x1b[u",
            rows,
            session.name,
            state_indicator,
            input_text,
        );
        pane.write_str(&prompt_line);
    }
}

/// Render all panes and separators to the framebuffer.
fn render_all(layout: &Layout) {
    let mut target = FbDrawTarget::new();
    layout.render_all(&mut target);
}

// ---------------------------------------------------------------------------
// Focus synchronisation
// ---------------------------------------------------------------------------

/// After a layout operation (split, close), sync the dashboard's focused
/// session to match the layout's focused pane.
///
/// The layout tracks focus by pane id; the dashboard tracks focus by session
/// index. We find which session owns the layout's focused pane and set
/// dashboard focus to that session's index.
fn sync_dashboard_focus(layout: &Layout, dashboard: &mut Dashboard) {
    let focused_pane_id = layout.focused_pane_id();
    for (idx, session) in dashboard.sessions.iter().enumerate() {
        if session.pane_id == focused_pane_id {
            dashboard.focused = idx;
            return;
        }
    }
    // If no session matches (shouldn't happen), leave focus as-is.
    log::warn!(
        "[dashboard] no session found for pane {}, focus unchanged",
        focused_pane_id
    );
}

// ---------------------------------------------------------------------------
// Input buffer helpers
// ---------------------------------------------------------------------------

/// Get a mutable reference to the input buffer for the currently focused agent.
fn focused_input_buffer_mut<'a>(
    input_buffers: &'a mut Vec<InputBuffer>,
    dashboard: &Dashboard,
) -> Option<&'a mut InputBuffer> {
    let agent_id = dashboard.focused_session()?.id;
    input_buffers.iter_mut().find(|b| b.agent_id == agent_id)
}
