//! Multi-agent dashboard — keyboard routing, pane management, agent & shell sessions.
//!
//! This module wires together:
//! - `claudio_agent::Dashboard` (agent session lifecycle)
//! - `claudio_shell::Shell` (hybrid AI/command shell)
//! - `claudio_terminal::Layout` (split-pane layout tree)
//! - `crate::keyboard::ScancodeStream` (async keyboard input)
//! - `crate::framebuffer` (GOP pixel output — double-buffered)
//!
//! ## Rendering strategy (TempleOS-inspired)
//!
//! Instead of re-rendering every pixel on every keypress (the old approach),
//! we now use dirty-region tracking + double buffering:
//!
//! 1. Each pane tracks which character rows changed (dirty_rows).
//! 2. On keypress, only the dirty rows of the focused pane are re-rendered
//!    into the back buffer (typically 1 row = 16 pixel-rows).
//! 3. The changed pixel rows are blitted from back buffer to front buffer
//!    in a single `copy_nonoverlapping` call.
//!
//! Keyboard input uses a tmux-style Ctrl+B prefix for pane management commands.
//! Regular keypresses are forwarded to the focused pane's input buffer.
//! Enter submits the buffered input to either the agent conversation or the shell.
//!
//! ## Pane types
//!
//! Each pane is either:
//! - **Agent** — Claude chat session (API calls, tool use loop)
//! - **Shell** — ClaudioOS shell (builtins, env vars, natural language)
//!
//! Pane 0 starts as a shell. Ctrl+B c = new agent, Ctrl+B s = new shell.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use pc_keyboard::DecodedKey;

use claudio_agent::{AgentState, Dashboard};
use claudio_net::{Instant, NetworkStack};
use claudio_shell::{Shell, Vfs, SystemInfo};
use claudio_terminal::{Layout, SplitDirection, FONT_HEIGHT};

use crate::keyboard::ScancodeStream;

// ---------------------------------------------------------------------------
// Framebuffer DrawTarget adapter — back-buffer variant
// ---------------------------------------------------------------------------

/// A `DrawTarget` that writes directly into a borrowed `&mut [u8]` back buffer.
///
/// This is the key performance win: instead of calling `framebuffer::put_pixel`
/// (which locks a mutex per pixel), the terminal renderer writes pixels directly
/// into the back buffer's memory. The buffer is only locked once per render pass.
struct BackBufDrawTarget<'a> {
    buf: &'a mut [u8],
    width: usize,
    height: usize,
    stride: usize,
    bpp: usize,
}

impl<'a> claudio_terminal::DrawTarget for BackBufDrawTarget<'a> {
    #[inline]
    fn put_pixel(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * self.stride + x) * self.bpp;
        if offset + 3 < self.buf.len() {
            // BGR32 pixel format (UEFI GOP standard).
            unsafe {
                let ptr = self.buf.as_mut_ptr().add(offset);
                core::ptr::write_volatile(ptr, b);
                core::ptr::write_volatile(ptr.add(1), g);
                core::ptr::write_volatile(ptr.add(2), r);
            }
        }
    }

    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn bytes_per_pixel(&self) -> usize {
        self.bpp
    }

    fn stride(&self) -> usize {
        self.stride
    }

    fn buffer_mut(&mut self) -> Option<&mut [u8]> {
        Some(self.buf)
    }
}

/// Legacy fallback DrawTarget that uses `framebuffer::put_pixel`.
/// Only used when we can't acquire the back buffer (shouldn't happen).
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
// Pane type — Agent or Shell
// ---------------------------------------------------------------------------

/// Each dashboard pane is either an agent chat session or a shell session.
enum PaneType {
    /// An agent chat session. The usize is the agent session id in the Dashboard.
    Agent(usize),
    /// A shell session with its own Shell state.
    Shell(ShellPaneState),
}

/// State for a shell pane. Wraps `claudio_shell::Shell` and adapts it to the
/// event-driven dashboard model (no blocking run loop).
struct ShellPaneState {
    /// The shell instance (env, history, prompt, builtins).
    shell: Shell,
    /// Layout pane id this shell is bound to.
    pane_id: usize,
    /// Unique shell id for tracking.
    id: usize,
}

impl ShellPaneState {
    fn new(id: usize, pane_id: usize) -> Self {
        Self {
            shell: Shell::new(),
            pane_id,
            id,
        }
    }
}

// ---------------------------------------------------------------------------
// Stub VFS for shell builtins (no filesystem mounted yet)
// ---------------------------------------------------------------------------

/// Minimal VFS that returns "no filesystem mounted" for all operations.
/// This is sufficient for builtins like echo, help, clear, ps, history, env.
struct StubVfs;

impl Vfs for StubVfs {
    fn list_dir(&self, path: &str) -> Result<Vec<String>, String> {
        Err(format!("ls: {}: no filesystem mounted", path))
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        Err(format!("cat: {}: no filesystem mounted", path))
    }

    fn write_file(&mut self, path: &str, _data: &[u8]) -> Result<(), String> {
        Err(format!("write: {}: no filesystem mounted", path))
    }

    fn append_file(&mut self, path: &str, _data: &[u8]) -> Result<(), String> {
        Err(format!("append: {}: no filesystem mounted", path))
    }

    fn copy_file(&mut self, src: &str, _dst: &str) -> Result<(), String> {
        Err(format!("cp: {}: no filesystem mounted", src))
    }

    fn move_file(&mut self, src: &str, _dst: &str) -> Result<(), String> {
        Err(format!("mv: {}: no filesystem mounted", src))
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        Err(format!("rm: {}: no filesystem mounted", path))
    }

    fn mkdir(&mut self, path: &str) -> Result<(), String> {
        Err(format!("mkdir: {}: no filesystem mounted", path))
    }

    fn touch(&mut self, path: &str) -> Result<(), String> {
        Err(format!("touch: {}: no filesystem mounted", path))
    }

    fn exists(&self, _path: &str) -> bool {
        false
    }

    fn is_dir(&self, _path: &str) -> bool {
        false
    }

    fn mount(&mut self, _device: &str, _path: &str, _fstype: &str) -> Result<(), String> {
        Err(String::from("mount: not yet implemented"))
    }

    fn umount(&mut self, _path: &str) -> Result<(), String> {
        Err(String::from("umount: not yet implemented"))
    }
}

// ---------------------------------------------------------------------------
// SystemInfo implementation — wired to the agent Dashboard
// ---------------------------------------------------------------------------

/// SystemInfo wired to the agent Dashboard for ps/kill. Stubs the rest.
struct DashboardSystemInfoMut<'a> {
    dashboard: &'a Dashboard,
}

impl<'a> SystemInfo for DashboardSystemInfoMut<'a> {
    fn list_agents(&self) -> Vec<(u64, String, String)> {
        self.dashboard
            .sessions
            .iter()
            .map(|s| {
                let status = match s.state {
                    AgentState::Idle => "idle",
                    AgentState::WaitingForInput => "waiting",
                    AgentState::Thinking => "thinking",
                    AgentState::ToolExecuting => "tool",
                    AgentState::Streaming => "streaming",
                    AgentState::Error => "ERROR",
                };
                (s.id as u64, s.name.clone(), String::from(status))
            })
            .collect()
    }

    fn kill_agent(&mut self, _id: u64) -> Result<(), String> {
        Err(String::from("kill: use Ctrl+B x to close panes"))
    }

    fn clear_screen(&mut self) {}

    fn reboot(&mut self) -> ! {
        unsafe {
            x86_64::instructions::port::Port::<u8>::new(0x64).write(0xFE);
        }
        loop {
            x86_64::instructions::hlt();
        }
    }

    fn shutdown(&mut self) -> ! {
        unsafe {
            x86_64::instructions::port::Port::<u16>::new(0x604).write(0x2000);
        }
        loop {
            x86_64::instructions::hlt();
        }
    }

    fn ifconfig(&self) -> Vec<(String, String, String, String)> {
        Vec::new()
    }

    fn ping(&self, host: &str) -> Result<String, String> {
        Err(format!("ping: {}: not yet implemented", host))
    }

    fn date(&self) -> String {
        String::from("(no RTC driver)")
    }

    fn uptime_secs(&self) -> u64 {
        crate::interrupts::tick_count() / 18
    }

    fn memory_info(&self) -> (u64, u64, u64) {
        let total = crate::memory::HEAP_SIZE as u64;
        (total, 0, total)
    }

    fn disk_usage(&self) -> Vec<(String, u64, u64, u64)> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Per-pane input buffer
// ---------------------------------------------------------------------------

/// Input line buffer for each pane. Characters accumulate here until
/// Enter is pressed, at which point the buffer is drained and submitted.
struct InputBuffer {
    /// The pane id this buffer belongs to.
    pane_id: usize,
    /// Characters typed so far (before Enter).
    buf: String,
}

impl InputBuffer {
    fn new(pane_id: usize) -> Self {
        Self {
            pane_id,
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
    /// Normal mode — keys go to the focused pane.
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
/// 1. Creates the initial layout (single pane) with a shell session.
/// 2. Enters an async loop reading keyboard events.
/// 3. Routes prefix-key commands to layout/dashboard operations.
/// 4. Routes regular keys to the focused pane's input buffer.
/// 5. On Enter, submits the input buffer to the shell or agent conversation.
/// 6. Renders only dirty regions after every input event (not the full screen).
pub async fn run_dashboard(
    stack: &mut NetworkStack,
    api_key: &str,
    fb_width: usize,
    fb_height: usize,
    now: fn() -> Instant,
) {
    log::info!(
        "[dashboard] starting multi-agent dashboard ({}x{}) with double-buffered dirty-region rendering",
        fb_width,
        fb_height
    );

    // -- Initialise layout + pane tracking -----------------------------------

    let mut layout = Layout::new(fb_width, fb_height);
    let mut dashboard = Dashboard::new();
    let mut pane_types: Vec<PaneType> = Vec::new();
    let mut input_buffers: Vec<InputBuffer> = Vec::new();
    let mut next_shell_id: usize = 0;
    let mut vfs = StubVfs;

    // Create the first pane as a shell session.
    let first_pane_id = layout.focused_pane_id();
    let shell_state = ShellPaneState::new(next_shell_id, first_pane_id);
    next_shell_id += 1;
    pane_types.push(PaneType::Shell(shell_state));
    input_buffers.push(InputBuffer::new(first_pane_id));

    // Draw welcome banner into the first pane.
    {
        let pane = layout.pane_by_id_mut(first_pane_id).unwrap();
        pane.write_str("\x1b[96mClaudioOS v0.1.0\x1b[0m — \x1b[93mBare Metal AI Agent Terminal\x1b[0m\r\n");
        pane.write_str("\x1b[90m────────────────────────────────────────────────────\x1b[0m\r\n");
        pane.write_str("\r\n");
        pane.write_str("  \x1b[32mPhase 1\x1b[0m: Boot to terminal ............. \x1b[92mOK\x1b[0m\r\n");
        pane.write_str("  \x1b[32mPhase 2\x1b[0m: Networking ................... \x1b[92mOK\x1b[0m\r\n");
        pane.write_str("  \x1b[32mPhase 3\x1b[0m: TLS + API .................... \x1b[92mOK\x1b[0m\r\n");
        pane.write_str("  \x1b[32mPhase 4\x1b[0m: Multi-agent dashboard ........ \x1b[92mOK\x1b[0m\r\n");
        pane.write_str("\r\n");
        pane.write_str("\x1b[90mCtrl+B then \" = split | n/p = focus | c = new agent | s = new shell | x = close\x1b[0m\r\n");
        pane.write_str("\x1b[90mType commands or natural language. Type 'help' for builtins.\x1b[0m\r\n");
        pane.write_str("\r\n");
    }
    render_prompt_for_pane(&mut layout, &pane_types, &input_buffers, first_pane_id, &dashboard);

    // Initial full render: draw everything into the back buffer, then blit.
    render_full(&mut layout);

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
                            &mut pane_types,
                            &mut input_buffers,
                            &mut next_shell_id,
                        );
                        // Structural change — do a full render.
                        let focused_pane_id = layout.focused_pane_id();
                        render_prompt_for_pane(&mut layout, &pane_types, &input_buffers, focused_pane_id, &dashboard);
                        render_full(&mut layout);
                        continue;
                    }
                    PrefixState::Normal => {
                        // Ctrl+B detection: pc-keyboard with HandleControl::MapLettersToUnicode
                        // delivers Ctrl+letter as the Unicode control code. Ctrl+B = 0x02.
                        if c == '\x02' {
                            prefix_state = PrefixState::AwaitingCommand;
                            log::debug!("[dashboard] prefix key (Ctrl+B) pressed");
                            continue;
                        }

                        let focused_pane_id = layout.focused_pane_id();

                        // Enter key — submit input.
                        if c == '\n' || c == '\r' {
                            submit_input_for_focused(
                                &mut layout,
                                &mut dashboard,
                                &mut pane_types,
                                &mut input_buffers,
                                &mut vfs,
                                stack,
                                api_key,
                                now,
                            ).await;
                        } else if c == '\x08' || c == '\x7f' {
                            // Backspace / DEL — remove last character from input buffer.
                            if let Some(buf) = input_buffers.iter_mut().find(|b| b.pane_id == focused_pane_id) {
                                buf.backspace();
                            }
                        } else if !c.is_control() || c == '\t' {
                            // Regular printable character or tab — append to input buffer.
                            if let Some(buf) = input_buffers.iter_mut().find(|b| b.pane_id == focused_pane_id) {
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

        // Re-render prompt + dirty panes (fast path).
        let focused_pane_id = layout.focused_pane_id();
        render_prompt_for_pane(&mut layout, &pane_types, &input_buffers, focused_pane_id, &dashboard);
        render_dirty(&mut layout);
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
    pane_types: &mut Vec<PaneType>,
    input_buffers: &mut Vec<InputBuffer>,
    next_shell_id: &mut usize,
) {
    match c {
        // Split horizontal: Ctrl+B then "
        '"' => {
            log::info!("[dashboard] split horizontal (agent)");
            layout.split(SplitDirection::Horizontal);
            let new_pane_id = layout.focused_pane_id();
            let n = dashboard.sessions.len();
            let agent_id = dashboard.create_session(
                format!("agent-{}", n),
                new_pane_id,
            );
            pane_types.push(PaneType::Agent(agent_id));
            input_buffers.push(InputBuffer::new(new_pane_id));
        }

        // Split vertical: Ctrl+B then %
        '%' => {
            log::info!("[dashboard] split vertical (agent)");
            layout.split(SplitDirection::Vertical);
            let new_pane_id = layout.focused_pane_id();
            let n = dashboard.sessions.len();
            let agent_id = dashboard.create_session(
                format!("agent-{}", n),
                new_pane_id,
            );
            pane_types.push(PaneType::Agent(agent_id));
            input_buffers.push(InputBuffer::new(new_pane_id));
        }

        // Focus next pane: Ctrl+B then n
        'n' => {
            log::info!("[dashboard] focus next");
            layout.focus_next();
        }

        // Focus previous pane: Ctrl+B then p
        'p' => {
            log::info!("[dashboard] focus prev");
            layout.focus_prev();
        }

        // New agent session: Ctrl+B then c
        'c' => {
            log::info!("[dashboard] new agent pane (split horizontal)");
            layout.split(SplitDirection::Horizontal);
            let new_pane_id = layout.focused_pane_id();
            let n = dashboard.sessions.len();
            let agent_id = dashboard.create_session(
                format!("agent-{}", n),
                new_pane_id,
            );
            pane_types.push(PaneType::Agent(agent_id));
            input_buffers.push(InputBuffer::new(new_pane_id));

            // Write agent welcome into the new pane.
            if let Some(pane) = layout.pane_by_id_mut(new_pane_id) {
                pane.write_str("\x1b[96mClaude Agent\x1b[0m — type a message and press Enter\r\n");
                pane.write_str("\x1b[90m────────────────────────────────────────────────────\x1b[0m\r\n");
            }
        }

        // New shell session: Ctrl+B then s
        's' => {
            log::info!("[dashboard] new shell pane (split horizontal)");
            layout.split(SplitDirection::Horizontal);
            let new_pane_id = layout.focused_pane_id();
            let shell_state = ShellPaneState::new(*next_shell_id, new_pane_id);
            *next_shell_id += 1;
            pane_types.push(PaneType::Shell(shell_state));
            input_buffers.push(InputBuffer::new(new_pane_id));

            // Write shell welcome into the new pane.
            if let Some(pane) = layout.pane_by_id_mut(new_pane_id) {
                pane.write_str("\x1b[96mClaudioOS Shell\x1b[0m\r\n");
                pane.write_str("\x1b[90mType 'help' for built-in commands.\x1b[0m\r\n");
            }
        }

        // Close focused pane: Ctrl+B then x
        'x' => {
            if layout.pane_count() <= 1 {
                log::warn!("[dashboard] cannot close last pane");
                return;
            }
            log::info!("[dashboard] close focused pane");

            let focused_pane_id = layout.focused_pane_id();

            // Remove the pane type entry and input buffer.
            // Also remove the agent session if it was an agent pane.
            if let Some(idx) = pane_types.iter().position(|pt| match pt {
                PaneType::Agent(aid) => {
                    dashboard.session_by_id(*aid).map(|s| s.pane_id) == Some(focused_pane_id)
                }
                PaneType::Shell(ss) => ss.pane_id == focused_pane_id,
            }) {
                if let PaneType::Agent(_) = &pane_types[idx] {
                    // Find and close the agent session matching this pane.
                    // We need to find the dashboard index for the session.
                    if let Some(si) = dashboard.sessions.iter().position(|s| s.pane_id == focused_pane_id) {
                        dashboard.focused = si;
                        dashboard.close_focused();
                    }
                }
                pane_types.remove(idx);
            }
            input_buffers.retain(|b| b.pane_id != focused_pane_id);
            layout.close_focused();
        }

        other => {
            log::debug!("[dashboard] unknown prefix command: {:?}", other);
        }
    }
}

// ---------------------------------------------------------------------------
// Input submission — dispatches to agent or shell
// ---------------------------------------------------------------------------

/// Submit the focused pane's input buffer.
/// Dispatches to either `submit_agent_input` or `submit_shell_input`.
async fn submit_input_for_focused(
    layout: &mut Layout,
    dashboard: &mut Dashboard,
    pane_types: &mut Vec<PaneType>,
    input_buffers: &mut Vec<InputBuffer>,
    vfs: &mut StubVfs,
    stack: &mut NetworkStack,
    api_key: &str,
    now: fn() -> Instant,
) {
    let focused_pane_id = layout.focused_pane_id();

    // Drain the input buffer.
    let input_text = match input_buffers.iter_mut().find(|b| b.pane_id == focused_pane_id) {
        Some(buf) => buf.drain(),
        None => return,
    };

    if input_text.is_empty() {
        return;
    }

    // Find the pane type for the focused pane.
    let pane_type_idx = pane_types.iter().position(|pt| match pt {
        PaneType::Agent(aid) => {
            dashboard.session_by_id(*aid).map(|s| s.pane_id) == Some(focused_pane_id)
        }
        PaneType::Shell(ss) => ss.pane_id == focused_pane_id,
    });

    let pane_type_idx = match pane_type_idx {
        Some(i) => i,
        None => {
            log::warn!("[dashboard] no pane type for focused pane {}", focused_pane_id);
            return;
        }
    };

    // Check if this is an agent or shell pane.
    let is_agent = matches!(&pane_types[pane_type_idx], PaneType::Agent(_));

    if is_agent {
        let agent_id = match &pane_types[pane_type_idx] {
            PaneType::Agent(aid) => *aid,
            _ => unreachable!(),
        };
        submit_agent_input(
            layout, dashboard, agent_id, focused_pane_id, input_text, stack, api_key, now,
        ).await;
    } else {
        submit_shell_input(
            layout, dashboard, pane_types, pane_type_idx, focused_pane_id, input_text, vfs,
        );
    }
}

/// Submit input to an agent session — API call + tool-use loop.
async fn submit_agent_input(
    layout: &mut Layout,
    dashboard: &mut Dashboard,
    agent_id: usize,
    pane_id: usize,
    input_text: String,
    stack: &mut NetworkStack,
    api_key: &str,
    now: fn() -> Instant,
) {
    use crate::agent_loop::{run_tool_loop, ToolLoopOutcome};

    log::info!("[dashboard] agent {} input: {}", agent_id, input_text);

    // Write the user's input into the pane as a visual echo.
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
    render_dirty(layout);

    // --- Run the full API call + tool-use loop ------------------------------
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

/// Submit input to a shell session — execute the command and write output to the pane.
fn submit_shell_input(
    layout: &mut Layout,
    dashboard: &Dashboard,
    pane_types: &mut Vec<PaneType>,
    pane_type_idx: usize,
    pane_id: usize,
    input_text: String,
    vfs: &mut StubVfs,
) {
    log::info!("[dashboard] shell input: {}", input_text);

    // Echo the input.
    if let Some(pane) = layout.pane_by_id_mut(pane_id) {
        pane.write_str(&format!("\r\n\x1b[32m$ {}\x1b[0m\r\n", input_text));
    }

    // Special handling for `history` — we need to read from the shell state.
    let shell_state = match &mut pane_types[pane_type_idx] {
        PaneType::Shell(ss) => ss,
        _ => return,
    };

    // Handle `history` specially since it reads from shell state directly.
    let trimmed = input_text.trim();
    if trimmed == "history" {
        let entries = shell_state.shell.history.entries();
        let mut output = String::new();
        for (i, entry) in entries.iter().enumerate() {
            output.push_str(&format!("  {:>4}  {}\r\n", i + 1, entry));
        }
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            pane.write_str(&output);
        }
        // Still add to history.
        shell_state.shell.history.push(trimmed);
        return;
    }

    // Execute using Shell::execute_input which handles both commands and natural language.
    // We need a dummy LineReader since execute_input takes one for AI confirmation.
    let mut dummy_reader = DummyLineReader;
    let mut sys = DashboardSystemInfoMut { dashboard };

    let (output, exit_code) = shell_state.shell.execute_input(
        trimmed,
        vfs,
        &mut sys,
        &mut dummy_reader,
    );

    // Add to history.
    shell_state.shell.history.push(trimmed);

    // Write output to the pane.
    if !output.is_empty() {
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            // Convert \n to \r\n for the terminal pane.
            let terminal_output = output.replace('\n', "\r\n");
            pane.write_str(&terminal_output);
            if !output.ends_with('\n') {
                pane.write_str("\r\n");
            }
        }
    }

    if exit_code != 0 {
        log::debug!("[dashboard] shell command exited with code {}", exit_code);
    }
}

/// Dummy LineReader for shell commands that don't need interactive input.
/// AI natural language proposals will auto-cancel since read_line returns None.
struct DummyLineReader;

impl claudio_shell::LineReader for DummyLineReader {
    fn read_line(&mut self, _prompt: &str) -> Option<String> {
        None // Cancel any AI proposals that need confirmation.
    }

    fn write_output(&mut self, _text: &str) {
        // Output is captured via execute_input return value instead.
    }

    fn check_interrupt(&self) -> bool {
        false
    }

    fn clear_interrupt(&mut self) {}
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Render the prompt line for a specific pane (agent or shell).
fn render_prompt_for_pane(
    layout: &mut Layout,
    pane_types: &[PaneType],
    input_buffers: &[InputBuffer],
    pane_id: usize,
    dashboard: &Dashboard,
) {
    let input_text = input_buffers
        .iter()
        .find(|b| b.pane_id == pane_id)
        .map(|b| b.as_str())
        .unwrap_or("");

    // Find the pane type.
    let pane_type = pane_types.iter().find(|pt| match pt {
        PaneType::Agent(aid) => {
            dashboard.session_by_id(*aid).map(|s| s.pane_id) == Some(pane_id)
        }
        PaneType::Shell(ss) => ss.pane_id == pane_id,
    });

    let (name, state_indicator, prompt_char) = match pane_type {
        Some(PaneType::Agent(aid)) => {
            if let Some(session) = dashboard.session_by_id(*aid) {
                let state = match session.state {
                    AgentState::Idle => "[idle]",
                    AgentState::WaitingForInput => "",
                    AgentState::Thinking => "[thinking]",
                    AgentState::ToolExecuting => "[tool]",
                    AgentState::Streaming => "[streaming]",
                    AgentState::Error => "[ERROR]",
                };
                (session.name.as_str(), state, ">")
            } else {
                ("agent", "", ">")
            }
        }
        Some(PaneType::Shell(_ss)) => {
            ("shell", "", "$")
        }
        None => ("???", "", ">"),
    };

    if let Some(pane) = layout.pane_by_id_mut(pane_id) {
        let rows = pane.rows();
        let prompt_line = format!(
            "\x1b[s\x1b[{};1H\x1b[2K\x1b[33m{}\x1b[37m {} \x1b[32m{} \x1b[0m{}\x1b[u",
            rows,
            name,
            state_indicator,
            prompt_char,
            input_text,
        );
        pane.write_str(&prompt_line);
    }
}

/// Render only dirty regions into the back buffer, then blit changed pixel
/// rows to the hardware framebuffer. This is the fast path for keypress rendering.
///
/// Typical cost: 1 character row = 16 pixel-rows ~= 80 KiB blit.
/// Compare to old full render: 800 pixel-rows ~= 4 MiB + 1M mutex locks.
fn render_dirty(layout: &mut Layout) {
    // Step 1: Collect the pixel row ranges that are dirty BEFORE we clear flags.
    let fb_height = crate::framebuffer::height();
    let mut min_y: usize = fb_height;
    let mut max_y: usize = 0;

    for pane in layout.panes() {
        let vp = &pane.viewport;
        for (row_idx, &dirty) in pane.dirty_rows().iter().enumerate() {
            if dirty {
                let py_start = vp.y + row_idx * FONT_HEIGHT;
                let py_end = (py_start + FONT_HEIGHT).min(fb_height);
                if py_start < min_y {
                    min_y = py_start;
                }
                if py_end > max_y {
                    max_y = py_end;
                }
            }
        }
    }

    // Always include a small region for cursor movement (cursor row +/- 1 row).
    let focused_pane = layout.focused_pane();
    let fvp = &focused_pane.viewport;
    if min_y > fvp.y {
        min_y = fvp.y;
    }
    if max_y < fvp.y + fvp.height {
        max_y = (fvp.y + fvp.height).min(fb_height);
    }

    // Step 2: Render dirty rows into the back buffer.
    crate::framebuffer::with_back_buffer(|buf, w, h, stride, bpp| {
        let mut target = BackBufDrawTarget {
            buf,
            width: w,
            height: h,
            stride,
            bpp,
        };
        layout.render_dirty(&mut target);
    });

    // Step 3: Blit only the dirty pixel rows to the front buffer.
    if min_y < max_y {
        crate::framebuffer::blit_rows(min_y, max_y);
        log::trace!(
            "[dashboard] dirty blit: pixel rows {}..{} ({} rows, ~{} KiB)",
            min_y,
            max_y,
            max_y - min_y,
            (max_y - min_y) * crate::framebuffer::stride() * crate::framebuffer::bytes_per_pixel() / 1024
        );
    }
}

/// Full render: render all panes + separators into the back buffer, then blit.
/// Used for structural changes (split, close, initial draw).
fn render_full(layout: &mut Layout) {
    crate::framebuffer::with_back_buffer(|buf, w, h, stride, bpp| {
        let mut target = BackBufDrawTarget {
            buf,
            width: w,
            height: h,
            stride,
            bpp,
        };
        layout.render_all_and_clear(&mut target);
    });
    crate::framebuffer::blit_full();
    log::debug!("[dashboard] full render completed");
}
