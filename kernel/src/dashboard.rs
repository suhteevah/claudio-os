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
use pc_keyboard::{DecodedKey, KeyCode};

use claudio_agent::{AgentState, Dashboard};
use claudio_net::{Instant, NetworkStack};
use claudio_shell::{Shell, Vfs, SystemInfo};
use claudio_terminal::{Layout, SplitDirection, FONT_HEIGHT};

use crate::filemanager::{self, FileManagerState, FileManagerAction};
use crate::ipc;
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

/// Each dashboard pane is either an agent chat session, a shell session,
/// or the system monitor.
enum PaneType {
    /// An agent chat session. The usize is the agent session id in the Dashboard.
    Agent(usize),
    /// A shell session with its own Shell state.
    Shell(ShellPaneState),
    /// System monitor pane. The usize is the layout pane id.
    SysMonitor(usize),
    /// Text-mode web browser pane.
    Browser(crate::browser::BrowserState),
    /// Visual file manager pane.
    FileManager(FileManagerState),
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
///
/// # Wiring the real VFS
///
/// Once storage drivers are initialized (AHCI/NVMe), replace `StubVfs` with
/// a real `claudio_vfs::Vfs` instance. Example:
///
/// ```rust,no_run
/// // TODO: Wire real VFS once storage drivers are initialized.
/// //
/// // use claudio_vfs::adapters::*;
/// // use claudio_vfs::{Vfs, MountOptions};
/// //
/// // // 1. Wrap the AHCI disk as a VFS BlockDevice:
/// // let ahci_dev = unsafe {
/// //     AhciBlockDeviceAdapter::new(ahci_disk_ptr, hba_ptr)
/// // };
/// //
/// // // 2. Auto-detect the filesystem on each partition:
/// // let fs_type = detect_filesystem(&ahci_dev);
/// //
/// // // 3. Create a partition view and mount the filesystem:
/// // let partition_dev = VfsToExt4BlockDevice {
/// //     device: &ahci_dev,
/// //     partition_offset: part.start_offset(sector_size),
/// //     partition_size: part.size_bytes(sector_size),
/// // };
/// // let ext4_fs = claudio_ext4::Ext4Fs::mount(partition_dev).unwrap();
/// // let adapter = Box::leak(Box::new(Ext4FilesystemAdapter::new(ext4_fs)));
/// //
/// // // 4. Mount into the VFS:
/// // let mut vfs = Vfs::new();
/// // vfs.mount("/", adapter, MountOptions::default()).unwrap();
/// ```
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
        // Use ACPI reboot (reset register) with keyboard controller fallback
        crate::acpi_init::reboot();
    }

    fn shutdown(&mut self) -> ! {
        // Use ACPI S5 shutdown with QEMU fallback
        crate::acpi_init::shutdown();
    }

    fn ifconfig(&self) -> Vec<(String, String, String, String)> {
        Vec::new()
    }

    fn ping(&self, host: &str) -> Result<String, String> {
        Err(format!("ping: {}: not yet implemented", host))
    }

    fn date(&self) -> String {
        crate::rtc::wall_clock_formatted()
    }

    fn uptime_secs(&self) -> u64 {
        crate::rtc::uptime_seconds()
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
    let mut screensaver = crate::screensaver::ScreensaverState::new();

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
        pane.write_str("\x1b[90mCtrl+B then \" = split | n/p = focus | c = new agent | s = new shell | f = files | w = browser | x = close\x1b[0m\r\n");
        pane.write_str("\x1b[90mCtrl+Alt+F1-F6 = virtual consoles | Ctrl+Shift+C/V = copy/paste | Ctrl+Shift+H = clipboard history\x1b[0m\r\n");
        pane.write_str("\x1b[90mIPC: /msg <agent> <text> | /broadcast <text> | /inbox | /agents | /channel create|read|write\x1b[0m\r\n");
        pane.write_str("\x1b[90mType commands or natural language. Type 'help' for builtins.\x1b[0m\r\n");
        pane.write_str("\r\n");
    }
    render_prompt_for_pane(&mut layout, &pane_types, &input_buffers, first_pane_id, &dashboard);

    // Initial full render: draw everything into the back buffer, then blit.
    render_full(&mut layout);

    // -- Keyboard event loop ------------------------------------------------

    let stream = ScancodeStream::new();
    let mut prefix_state = PrefixState::Normal;
    let mut last_sysmon_tick: u64 = crate::interrupts::tick_count();

    loop {
        // -- Screensaver: idle check + animation loop --
        if screensaver.active {
            if let Some(_key) = stream.try_next_key() {
                screensaver.record_input();
                render_full(&mut layout);
                continue;
            }
            screensaver.render_frame();
            crate::executor::yield_now().await;
            continue;
        }
        if screensaver.check_idle() {
            continue;
        }

        let key = stream.next_key().await;
        screensaver.record_input();

        // Virtual console check: if we're not on console 0 (dashboard),
        // the dashboard doesn't process input. The Ctrl+Alt+F1-F6 switching
        // is handled at the ISR level (interrupts.rs). We just skip here.
        if crate::vconsole::active_console() != 0 {
            // If on kernel log console (6), refresh the log display.
            if crate::vconsole::is_kernel_log_active() {
                render_kernel_log_fullscreen();
            }
            continue;
        }

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

                        // Clipboard shortcuts (Ctrl+Shift detected via scancode modifier tracking):
                        // Ctrl+C (0x03) with Shift held = copy, Ctrl+V (0x16) with Shift held = paste,
                        // Ctrl+H (0x08) with Shift held = cycle clipboard history.
                        if crate::vconsole::shift_held() {
                            match c {
                                '\x03' => {
                                    // Ctrl+Shift+C — copy current input buffer to clipboard.
                                    let focused_pane_id = layout.focused_pane_id();
                                    if let Some(buf) = input_buffers.iter().find(|b| b.pane_id == focused_pane_id) {
                                        let text = buf.as_str();
                                        if !text.is_empty() {
                                            crate::clipboard::copy(text);
                                            if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                                                pane.write_str("\x1b[90m[copied]\x1b[0m");
                                            }
                                        }
                                    }
                                    continue;
                                }
                                '\x16' => {
                                    // Ctrl+Shift+V — paste clipboard into current input buffer.
                                    let focused_pane_id = layout.focused_pane_id();
                                    let text = crate::clipboard::paste();
                                    if !text.is_empty() {
                                        if let Some(buf) = input_buffers.iter_mut().find(|b| b.pane_id == focused_pane_id) {
                                            for ch in text.chars() {
                                                if !ch.is_control() || ch == '\t' {
                                                    buf.push(ch);
                                                }
                                            }
                                        }
                                    }
                                    continue;
                                }
                                '\x08' => {
                                    // Ctrl+Shift+H — cycle clipboard history, paste selection.
                                    let focused_pane_id = layout.focused_pane_id();
                                    let text = crate::clipboard::cycle_history();
                                    if !text.is_empty() {
                                        if let Some(buf) = input_buffers.iter_mut().find(|b| b.pane_id == focused_pane_id) {
                                            // Replace current input with the history entry.
                                            buf.drain();
                                            for ch in text.chars() {
                                                if !ch.is_control() || ch == '\t' {
                                                    buf.push(ch);
                                                }
                                            }
                                        }
                                        if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                                            let preview = if text.len() > 40 {
                                                &text[..40]
                                            } else {
                                                &text
                                            };
                                            pane.write_str(&format!("\x1b[90m[clipboard: {}...]\x1b[0m", preview));
                                        }
                                    }
                                    continue;
                                }
                                _ => {}
                            }
                        }

                        let focused_pane_id = layout.focused_pane_id();

                        // Browser pane — route all keys to the browser.
                        let is_browser = pane_types.iter().any(|pt| {
                            matches!(pt, PaneType::Browser(bs) if bs.pane_id == focused_pane_id)
                        });
                        if is_browser {
                            handle_browser_char(
                                c,
                                &mut layout,
                                &mut pane_types,
                                stack,
                                now,
                                focused_pane_id,
                            );
                            render_dirty(&mut layout);
                            continue;
                        }

                        // File manager pane -- route all keys to the file manager.
                        let is_fm = pane_types.iter().any(|pt| {
                            matches!(pt, PaneType::FileManager(fm) if fm.pane_id == focused_pane_id)
                        });
                        if is_fm {
                            handle_filemanager_char(
                                c,
                                &mut layout,
                                &mut pane_types,
                                &mut vfs,
                                focused_pane_id,
                            );
                            render_dirty(&mut layout);
                            continue;
                        }

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
                                &mut screensaver,
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
                // Route raw keys to browser if focused.
                let focused_pane_id = layout.focused_pane_id();
                let is_browser = pane_types.iter().any(|pt| {
                    matches!(pt, PaneType::Browser(bs) if bs.pane_id == focused_pane_id)
                });
                if is_browser {
                    handle_browser_rawkey(
                        k,
                        &mut layout,
                        &mut pane_types,
                        focused_pane_id,
                    );
                } else {
                    // File manager pane -- route raw keys.
                    let is_fm = pane_types.iter().any(|pt| {
                        matches!(pt, PaneType::FileManager(fm) if fm.pane_id == focused_pane_id)
                    });
                    if is_fm {
                        handle_filemanager_rawkey(
                            k,
                            &mut layout,
                            &mut pane_types,
                            &mut vfs,
                            focused_pane_id,
                        );
                    } else {
                        log::trace!("[dashboard] raw key: {:?}", k);
                    }
                }
            }
        }

        // Re-render prompt + dirty panes (fast path).
        let focused_pane_id = layout.focused_pane_id();
        render_prompt_for_pane(&mut layout, &pane_types, &input_buffers, focused_pane_id, &dashboard);
        render_dirty(&mut layout);

        // Poll the SSH server for incoming connections and data.
        crate::ssh_server::poll_ssh_server(stack);

        // Auto-refresh system monitor pane (~every 1 second).
        let current_tick = crate::interrupts::tick_count();
        if current_tick.wrapping_sub(last_sysmon_tick) >= crate::sysmon::REFRESH_TICKS {
            last_sysmon_tick = current_tick;
            let focused_pane_id = layout.focused_pane_id();
            let is_sysmon_focused = pane_types.iter().any(|pt| {
                matches!(pt, PaneType::SysMonitor(pid) if *pid == focused_pane_id)
            });
            if is_sysmon_focused {
                // Clear the pane and re-render.
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str("[2J[H");
                }
                let stats = crate::sysmon::collect_stats(&dashboard);
                let rendered = crate::sysmon::render_to_string(&stats);
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&rendered);
                }
                render_dirty(&mut layout);
            }
        }
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
            let name = format!("agent-{}", n);
            let agent_id = dashboard.create_session(
                name.clone(),
                new_pane_id,
            );
            ipc::IPC.lock().bus.register_agent(agent_id, name);
            pane_types.push(PaneType::Agent(agent_id));
            input_buffers.push(InputBuffer::new(new_pane_id));
        }

        // Split vertical: Ctrl+B then %
        '%' => {
            log::info!("[dashboard] split vertical (agent)");
            layout.split(SplitDirection::Vertical);
            let new_pane_id = layout.focused_pane_id();
            let n = dashboard.sessions.len();
            let name = format!("agent-{}", n);
            let agent_id = dashboard.create_session(
                name.clone(),
                new_pane_id,
            );
            ipc::IPC.lock().bus.register_agent(agent_id, name);
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
            let name = format!("agent-{}", n);
            let agent_id = dashboard.create_session(
                name.clone(),
                new_pane_id,
            );
            ipc::IPC.lock().bus.register_agent(agent_id, name);
            pane_types.push(PaneType::Agent(agent_id));
            input_buffers.push(InputBuffer::new(new_pane_id));

            // Write agent welcome into the new pane.
            if let Some(pane) = layout.pane_by_id_mut(new_pane_id) {
                pane.write_str("\x1b[96mClaude Agent\x1b[0m — type a message and press Enter\r\n");
                pane.write_str("\x1b[90m────────────────────────────────────────────────────\x1b[0m\r\n");
                pane.write_str("\x1b[90mIPC: /msg <agent> <text> | /broadcast <text> | /inbox | /agents\x1b[0m\r\n");
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
                PaneType::SysMonitor(pid) => *pid == focused_pane_id,
                PaneType::Browser(bs) => bs.pane_id == focused_pane_id,
                PaneType::FileManager(fm) => fm.pane_id == focused_pane_id,
            }) {
                if let PaneType::Agent(aid) = &pane_types[idx] {
                    let aid = *aid;
                    ipc::IPC.lock().bus.unregister_agent(aid);
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

        // System monitor: Ctrl+B then m
        'm' => {
            log::info!("[dashboard] new sysmon pane (split horizontal)");
            layout.split(SplitDirection::Horizontal);
            let new_pane_id = layout.focused_pane_id();
            pane_types.push(PaneType::SysMonitor(new_pane_id));
            input_buffers.push(InputBuffer::new(new_pane_id));

            // Render initial monitor content.
            let stats = crate::sysmon::collect_stats(dashboard);
            let rendered = crate::sysmon::render_to_string(&stats);
            if let Some(pane) = layout.pane_by_id_mut(new_pane_id) {
                pane.write_str(&rendered);
            }
        }

        // File manager: Ctrl+B then f
        'f' => {
            log::info!("[dashboard] new file manager pane (split horizontal)");
            layout.split(SplitDirection::Horizontal);
            let new_pane_id = layout.focused_pane_id();
            let fm_state = FileManagerState::new(new_pane_id);
            // Render initial content into the pane.
            if let Some(pane) = layout.pane_by_id_mut(new_pane_id) {
                let cols = pane.cols();
                let rows = pane.rows();
                let rendered = filemanager::render_to_pane(&fm_state, cols, rows);
                pane.write_str(&rendered);
            }
            pane_types.push(PaneType::FileManager(fm_state));
            input_buffers.push(InputBuffer::new(new_pane_id));
        }

        // Rename focused agent: Ctrl+B then ,
        // Consumes the current input buffer as the new name.
        ',' => {
            let focused_pane_id = layout.focused_pane_id();
            if let Some(buf) = input_buffers.iter_mut().find(|b| b.pane_id == focused_pane_id) {
                let new_name = buf.drain();
                if !new_name.is_empty() {
                    // Find agent for this pane.
                    if let Some(pt) = pane_types.iter().find(|pt| match pt {
                        PaneType::Agent(aid) => {
                            dashboard.session_by_id(*aid).map(|s| s.pane_id) == Some(focused_pane_id)
                        }
                        _ => false,
                    }) {
                        if let PaneType::Agent(aid) = pt {
                            let aid = *aid;
                            if let Some(session) = dashboard.session_by_id_mut(aid) {
                                let old_name = session.name.clone();
                                session.name = new_name.clone();
                                ipc::IPC.lock().bus.rename_agent(aid, new_name.clone());
                                log::info!("[dashboard] renamed agent {} -> \"{}\"", old_name, new_name);
                                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                                    pane.write_str(&format!(
                                        "\r\n\x1b[93mRenamed: {} -> {}\x1b[0m\r\n",
                                        old_name, new_name
                                    ));
                                }
                            }
                        }
                    } else {
                        if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                            pane.write_str("\r\n\x1b[31mRename only works on agent panes.\x1b[0m\r\n");
                        }
                    }
                } else {
                    if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                        pane.write_str("\r\n\x1b[90mRename: type a name first, then Ctrl+B ,\x1b[0m\r\n");
                    }
                }
            }
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
    screensaver: &mut crate::screensaver::ScreensaverState,
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
        PaneType::SysMonitor(pid) => *pid == focused_pane_id,
        PaneType::Browser(bs) => bs.pane_id == focused_pane_id,
        PaneType::FileManager(fm) => fm.pane_id == focused_pane_id,
    });

    let pane_type_idx = match pane_type_idx {
        Some(i) => i,
        None => {
            log::warn!("[dashboard] no pane type for focused pane {}", focused_pane_id);
            return;
        }
    };

    // SysMonitor, Browser, and FileManager panes don't accept text input — ignore Enter.
    if matches!(&pane_types[pane_type_idx], PaneType::SysMonitor(_) | PaneType::Browser(_) | PaneType::FileManager(_)) {
        return;
    }

    // Check if this is an agent or shell pane.
    let is_agent = matches!(&pane_types[pane_type_idx], PaneType::Agent(_));

    if is_agent {
        let agent_id = match &pane_types[pane_type_idx] {
            PaneType::Agent(aid) => *aid,
            _ => unreachable!(),
        };

        // Intercept IPC slash-commands before sending to the API.
        if handle_ipc_command(layout, agent_id, focused_pane_id, &input_text, now) {
            return;
        }

        // Intercept /model command — per-agent model switching.
        {
            let trimmed = input_text.trim();
            if trimmed == "/model" || trimmed.starts_with("/model ") {
                let model_args = trimmed.strip_prefix("/model").unwrap_or("").trim();
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&format!("\r\n\x1b[32m> {}\x1b[0m\r\n", trimmed));
                }
                let output = if let Some(session) = dashboard.session_by_id_mut(agent_id) {
                    crate::model_select::handle_agent_command(session, model_args)
                } else {
                    alloc::string::String::from("Agent session not found.\n")
                };
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    let terminal_output = output.replace('\n', "\r\n");
                    pane.write_str(&format!("\x1b[36m{}\x1b[0m", terminal_output));
                }
                return;
            }
        }

        submit_agent_input(
            layout, dashboard, agent_id, focused_pane_id, input_text, stack, api_key, now,
        ).await;
    } else {
        // Intercept `screensaver` shell command.
        let trimmed_ss = input_text.trim();
        if trimmed_ss == "screensaver" || trimmed_ss.starts_with("screensaver ") {
            let ss_args = trimmed_ss.strip_prefix("screensaver").unwrap_or("").trim();
            if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                pane.write_str(&format!("\r\n\x1b[32m$ {}\x1b[0m\r\n", trimmed_ss));
            }
            let ss_output = crate::screensaver::handle_command(screensaver, ss_args);
            if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                let terminal_output = ss_output.replace('\n', "\r\n");
                pane.write_str(&terminal_output);
                pane.write_str("\r\n");
            }
            return;
        }

        // Intercept network utility commands (ping, wget, curl, netstat, etc.).
        {
            let trimmed_net = input_text.trim();
            if let Some(net_output) = crate::nettools::try_handle_netcmd(trimmed_net, stack, now) {
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&format!("\r\n\x1b[32m$ {}\x1b[0m\r\n", trimmed_net));
                }
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    let terminal_output = net_output.replace('\n', "\r\n");
                    pane.write_str(&terminal_output);
                    if !net_output.ends_with('\n') {
                        pane.write_str("\r\n");
                    }
                }
                // Add to shell history.
                if let PaneType::Shell(ss) = &mut pane_types[pane_type_idx] {
                    ss.shell.history.push(trimmed_net);
                }
                return;
            }
        }

        // Intercept `fw` firewall commands.
        {
            let trimmed_fw = input_text.trim();
            if trimmed_fw == "fw" || trimmed_fw.starts_with("fw ") {
                let fw_args = trimmed_fw.strip_prefix("fw").unwrap_or("").trim();
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&format!("\r\n\x1b[32m$ {}\x1b[0m\r\n", trimmed_fw));
                }
                let fw_output = crate::firewall::handle_command(fw_args);
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    let terminal_output = fw_output.replace('\n', "\r\n");
                    pane.write_str(&terminal_output);
                    if !fw_output.ends_with('\n') {
                        pane.write_str("\r\n");
                    }
                }
                if let PaneType::Shell(ss) = &mut pane_types[pane_type_idx] {
                    ss.shell.history.push(trimmed_fw);
                }
                return;
            }
        }

        // Intercept `ntpdate` NTP time sync command.
        {
            let trimmed_ntp = input_text.trim();
            if trimmed_ntp == "ntpdate" || trimmed_ntp.starts_with("ntpdate ") {
                let ntp_args = trimmed_ntp.strip_prefix("ntpdate").unwrap_or("").trim();
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&format!("\r\n\x1b[32m$ {}\x1b[0m\r\n", trimmed_ntp));
                }
                let ntp_output = crate::ntp::handle_command(ntp_args, stack, now);
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    let terminal_output = ntp_output.replace('\n', "\r\n");
                    pane.write_str(&terminal_output);
                    if !ntp_output.ends_with('\n') {
                        pane.write_str("\r\n");
                    }
                }
                if let PaneType::Shell(ss) = &mut pane_types[pane_type_idx] {
                    ss.shell.history.push(trimmed_ntp);
                }
                return;
            }
        }

        // Intercept `model` command for model selection.
        {
            let trimmed_model = input_text.trim();
            if trimmed_model == "model" || trimmed_model.starts_with("model ") {
                let model_args = trimmed_model.strip_prefix("model").unwrap_or("").trim();
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&format!("\r\n\x1b[32m$ {}\x1b[0m\r\n", trimmed_model));
                }
                let model_output = crate::model_select::handle_command(model_args);
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    let terminal_output = model_output.replace('\n', "\r\n");
                    pane.write_str(&terminal_output);
                    if !model_output.ends_with('\n') {
                        pane.write_str("\r\n");
                    }
                }
                if let PaneType::Shell(ss) = &mut pane_types[pane_type_idx] {
                    ss.shell.history.push(trimmed_model);
                }
                return;
            }
        }

        // Intercept `man` and enhanced `help` commands.
        {
            let trimmed_man = input_text.trim();
            if trimmed_man == "man" || trimmed_man.starts_with("man ") {
                let man_args = trimmed_man.strip_prefix("man").unwrap_or("").trim();
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&format!("\r\n\x1b[32m$ {}\x1b[0m\r\n", trimmed_man));
                }
                let man_output = crate::manpages::handle_command(man_args);
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    let terminal_output = man_output.replace('\n', "\r\n");
                    pane.write_str(&terminal_output);
                    if !man_output.ends_with('\n') {
                        pane.write_str("\r\n");
                    }
                }
                if let PaneType::Shell(ss) = &mut pane_types[pane_type_idx] {
                    ss.shell.history.push(trimmed_man);
                }
                return;
            }
            if trimmed_man == "help" {
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&format!("\r\n\x1b[32m$ {}\x1b[0m\r\n", trimmed_man));
                }
                let help_output = crate::manpages::help_with_manpages();
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    let terminal_output = help_output.replace('\n', "\r\n");
                    pane.write_str(&terminal_output);
                    if !help_output.ends_with('\n') {
                        pane.write_str("\r\n");
                    }
                }
                if let PaneType::Shell(ss) = &mut pane_types[pane_type_idx] {
                    ss.shell.history.push(trimmed_man);
                }
                return;
            }
        }

        // Intercept `notifications` / `notif` commands.
        {
            let trimmed_notif = input_text.trim();
            if trimmed_notif == "notifications" || trimmed_notif == "notif"
                || trimmed_notif.starts_with("notif ")
                || trimmed_notif.starts_with("notifications ")
            {
                let notif_args = if let Some(rest) = trimmed_notif.strip_prefix("notifications") {
                    rest.trim()
                } else if let Some(rest) = trimmed_notif.strip_prefix("notif") {
                    rest.trim()
                } else {
                    ""
                };
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&format!("\r\n\x1b[32m$ {}\x1b[0m\r\n", trimmed_notif));
                }
                let notif_output = crate::notifications::handle_command(notif_args);
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    let terminal_output = notif_output.replace('\n', "\r\n");
                    pane.write_str(&terminal_output);
                    if !notif_output.ends_with('\n') {
                        pane.write_str("\r\n");
                    }
                }
                if let PaneType::Shell(ss) = &mut pane_types[pane_type_idx] {
                    ss.shell.history.push(trimmed_notif);
                }
                return;
            }
        }

        // Intercept `view <path>` image viewer command.
        {
            let trimmed_view = input_text.trim();
            if trimmed_view == "view" || trimmed_view.starts_with("view ") {
                let view_args = trimmed_view.strip_prefix("view").unwrap_or("").trim();
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&format!("\r\n\x1b[32m$ {}\x1b[0m\r\n", trimmed_view));
                }
                let view_output = crate::image_viewer::handle_command(view_args);
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    let terminal_output = view_output.replace('\n', "\r\n");
                    pane.write_str(&terminal_output);
                    if !view_output.ends_with('\n') {
                        pane.write_str("\r\n");
                    }
                }
                if let PaneType::Shell(ss) = &mut pane_types[pane_type_idx] {
                    ss.shell.history.push(trimmed_view);
                }
                return;
            }
        }

        submit_shell_input(
            layout, dashboard, pane_types, pane_type_idx, focused_pane_id, input_text, vfs,
        );
    }
}

/// Submit input to an agent session — API call + tool-use loop.
///
/// Uses **streaming** so tokens appear in real-time as Claude generates them.
/// Each text chunk is written directly to the pane and the dirty region is
/// flushed to the framebuffer immediately, giving the user instant feedback.
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
    use crate::agent_loop::{run_tool_loop_streaming, ToolLoopOutcome};

    log::info!("[dashboard] agent {} input: {}", agent_id, input_text);

    // Write the user's input into the pane as a visual echo.
    if let Some(pane) = layout.pane_by_id_mut(pane_id) {
        pane.write_str(&format!("\r\n\x1b[32m> {}\x1b[0m\r\n", input_text));
    }

    // Inject any pending IPC messages into the input context.
    let enriched_input = inject_ipc_context(agent_id, &input_text);

    // Feed input to the agent conversation.
    let timestamp = now().total_millis() as u64;
    let ready = match dashboard.session_by_id_mut(agent_id) {
        Some(session) => session.handle_input(enriched_input, timestamp),
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

    // --- Run the streaming API call + tool-use loop -------------------------
    // `streaming_started` tracks whether we've begun writing cyan text.
    let mut streaming_started = false;
    let mut tool_log: Vec<(String, String, String, bool)> = Vec::new();

    let outcome = {
        let session = match dashboard.session_by_id_mut(agent_id) {
            Some(s) => s,
            None => return,
        };

        run_tool_loop_streaming(
            session,
            stack,
            api_key,
            now,
            // on_token: write each chunk to the pane immediately + re-render.
            |chunk| {
                if !streaming_started {
                    // Start the cyan text block and clear the "[thinking...]" line.
                    if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                        pane.write_str("\r\n\x1b[36m");
                    }
                    streaming_started = true;
                }
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    // Convert \n to \r\n for the terminal.
                    let display = chunk.replace('\n', "\r\n");
                    pane.write_str(&display);
                }
                // Flush to framebuffer so the user sees each token immediately.
                render_dirty(layout);
            },
            // on_tool_call: collect for display after the loop.
            |info| {
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
            },
        )
    };

    // Close the cyan text block if streaming was active.
    if streaming_started {
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            pane.write_str("\x1b[0m\r\n");
        }
    }

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

    // Display the final outcome — only for errors, since streaming already
    // wrote the text directly to the pane.
    match outcome {
        ToolLoopOutcome::Text(_text) => {
            // Already streamed to the pane token-by-token, nothing more to do.
        }
        ToolLoopOutcome::Error(e) => {
            log::error!("[dashboard] agent {} tool loop error: {}", agent_id, e);
            if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                pane.write_str(&format!("\r\n\x1b[31m[error: {}]\x1b[0m\r\n", e));
            }
        }
    }

    render_dirty(layout);
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
// IPC command handler — slash-commands for inter-agent messaging
// ---------------------------------------------------------------------------

/// Handle IPC slash-commands typed by the user in an agent pane.
///
/// Returns `true` if the input was an IPC command (consumed), `false` otherwise.
fn handle_ipc_command(
    layout: &mut Layout,
    agent_id: usize,
    pane_id: usize,
    input: &str,
    now: fn() -> Instant,
) -> bool {
    let trimmed = input.trim();

    // /msg <target> <message> — send a message to another agent.
    if let Some(rest) = trimmed.strip_prefix("/msg ") {
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            pane.write_str(&format!("\r\n\x1b[32m> {}\x1b[0m\r\n", trimmed));
        }
        let mut parts = rest.splitn(2, ' ');
        let target = parts.next().unwrap_or("");
        let message = parts.next().unwrap_or("");
        if target.is_empty() || message.is_empty() {
            if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                pane.write_str("\x1b[31mUsage: /msg <agent-name-or-id> <message>\x1b[0m\r\n");
            }
            return true;
        }
        let timestamp = now().total_millis() as u64;
        let input_json = serde_json::json!({"to": target, "message": message});
        match ipc::execute_send_to_agent(agent_id, &input_json, timestamp) {
            Ok(result) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    pane.write_str(&format!("\x1b[36m{}\x1b[0m\r\n", result));
                }
            }
            Err(e) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    pane.write_str(&format!("\x1b[31m{}\x1b[0m\r\n", e));
                }
            }
        }
        return true;
    }

    // /broadcast <message> — send to all agents.
    if let Some(message) = trimmed.strip_prefix("/broadcast ") {
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            pane.write_str(&format!("\r\n\x1b[32m> {}\x1b[0m\r\n", trimmed));
        }
        if message.is_empty() {
            if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                pane.write_str("\x1b[31mUsage: /broadcast <message>\x1b[0m\r\n");
            }
            return true;
        }
        let timestamp = now().total_millis() as u64;
        ipc::IPC.lock().bus.broadcast(agent_id, String::from(message), timestamp);
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            pane.write_str("\x1b[36mMessage broadcast to all agents.\x1b[0m\r\n");
        }
        return true;
    }

    // /inbox — read pending messages.
    if trimmed == "/inbox" {
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            pane.write_str(&format!("\r\n\x1b[32m> {}\x1b[0m\r\n", trimmed));
        }
        match ipc::execute_read_agent_messages(agent_id) {
            Ok(result) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    let terminal_output = result.replace('\n', "\r\n");
                    pane.write_str(&format!("\x1b[36m{}\x1b[0m\r\n", terminal_output));
                }
            }
            Err(e) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    pane.write_str(&format!("\x1b[31m{}\x1b[0m\r\n", e));
                }
            }
        }
        return true;
    }

    // /agents — list all agents in the IPC bus.
    if trimmed == "/agents" {
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            pane.write_str(&format!("\r\n\x1b[32m> {}\x1b[0m\r\n", trimmed));
        }
        match ipc::execute_list_agents_ipc() {
            Ok(result) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    let terminal_output = result.replace('\n', "\r\n");
                    pane.write_str(&format!("\x1b[36m{}\x1b[0m\r\n", terminal_output));
                }
            }
            Err(e) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    pane.write_str(&format!("\x1b[31m{}\x1b[0m\r\n", e));
                }
            }
        }
        return true;
    }

    // /channel create <name> — create a named channel.
    if let Some(name) = trimmed.strip_prefix("/channel create ") {
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            pane.write_str(&format!("\r\n\x1b[32m> {}\x1b[0m\r\n", trimmed));
        }
        let input_json = serde_json::json!({"name": name.trim()});
        match ipc::execute_create_channel(&input_json) {
            Ok(result) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    pane.write_str(&format!("\x1b[36m{}\x1b[0m\r\n", result));
                }
            }
            Err(e) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    pane.write_str(&format!("\x1b[31m{}\x1b[0m\r\n", e));
                }
            }
        }
        return true;
    }

    // /channel write <name> <data> — write to a channel.
    if let Some(rest) = trimmed.strip_prefix("/channel write ") {
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            pane.write_str(&format!("\r\n\x1b[32m> {}\x1b[0m\r\n", trimmed));
        }
        let mut parts = rest.splitn(2, ' ');
        let name = parts.next().unwrap_or("");
        let data = parts.next().unwrap_or("");
        let input_json = serde_json::json!({"channel": name, "data": data});
        match ipc::execute_channel_write(&input_json) {
            Ok(result) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    pane.write_str(&format!("\x1b[36m{}\x1b[0m\r\n", result));
                }
            }
            Err(e) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    pane.write_str(&format!("\x1b[31m{}\x1b[0m\r\n", e));
                }
            }
        }
        return true;
    }

    // /channel read <name> — read from a channel.
    if let Some(name) = trimmed.strip_prefix("/channel read ") {
        if let Some(pane) = layout.pane_by_id_mut(pane_id) {
            pane.write_str(&format!("\r\n\x1b[32m> {}\x1b[0m\r\n", trimmed));
        }
        let input_json = serde_json::json!({"channel": name.trim()});
        match ipc::execute_channel_read(&input_json) {
            Ok(result) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    let terminal_output = result.replace('\n', "\r\n");
                    pane.write_str(&format!("\x1b[36m{}\x1b[0m\r\n", terminal_output));
                }
            }
            Err(e) => {
                if let Some(pane) = layout.pane_by_id_mut(pane_id) {
                    pane.write_str(&format!("\x1b[31m{}\x1b[0m\r\n", e));
                }
            }
        }
        return true;
    }

    false
}

/// Inject any pending IPC messages into an agent's conversation context.
///
/// Called before submitting to the API so the agent sees messages from other agents.
/// Prepends a system-style notification to the user's input.
fn inject_ipc_context(agent_id: usize, user_input: &str) -> String {
    let messages = ipc::IPC.lock().bus.recv_messages(agent_id);
    if messages.is_empty() {
        return String::from(user_input);
    }

    let mut prefix = String::from("[IPC: You have new messages from other agents]\n");
    for msg in &messages {
        let kind = if msg.to_agent_id.is_none() {
            " (broadcast)"
        } else {
            ""
        };
        prefix.push_str(&format!(
            "From {}{}: {}\n",
            msg.from_agent_name, kind, msg.content
        ));
    }
    prefix.push_str("[End of IPC messages]\n\n");
    prefix.push_str(user_input);
    prefix
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
        PaneType::SysMonitor(pid) => *pid == pane_id,
        PaneType::Browser(bs) => bs.pane_id == pane_id,
        PaneType::FileManager(fm) => fm.pane_id == pane_id,
    });

    // SysMonitor, Browser, and FileManager panes don't show a standard prompt -- skip.
    if matches!(pane_type, Some(PaneType::SysMonitor(_) | PaneType::Browser(_) | PaneType::FileManager(_))) {
        return;
    }

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
        Some(PaneType::SysMonitor(_)) | Some(PaneType::Browser(_)) | Some(PaneType::FileManager(_)) => unreachable!(),
        None => ("???", "", ">"),
    };

    if let Some(pane) = layout.pane_by_id_mut(pane_id) {
        let rows = pane.rows();
        let notif_indicator = crate::notifications::prompt_indicator();
        let prompt_line = format!(
            "\x1b[s\x1b[{};1H\x1b[2K{}\x1b[33m{}\x1b[37m {} \x1b[32m{} \x1b[0m{}\x1b[u",
            rows,
            notif_indicator,
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

/// Render the kernel log (vconsole 6) as a full-screen text display.
/// Uses a simple line-by-line rendering approach into the back buffer.
fn render_kernel_log_fullscreen() {
    let fb_width = crate::framebuffer::width();
    let fb_height = crate::framebuffer::height();
    let font_height = FONT_HEIGHT;
    let max_rows = if font_height > 0 { fb_height / font_height } else { 40 };

    let header = format!(
        "\x1b[96m[ Kernel Log — Console 6 (read-only) — {} lines total ]\x1b[0m",
        crate::vconsole::kernel_log_line_count()
    );
    let log_text = crate::vconsole::kernel_log_text(max_rows.saturating_sub(3));

    // Use a temporary single-pane layout to render the log text with ANSI support.
    let mut tmp_layout = Layout::new(fb_width, fb_height);
    let pane_id = tmp_layout.focused_pane_id();
    if let Some(pane) = tmp_layout.pane_by_id_mut(pane_id) {
        pane.write_str(&header);
        pane.write_str("\r\n\x1b[90m");
        for _ in 0..60 { pane.write_str("\u{2500}"); }
        pane.write_str("\x1b[0m\r\n");
        pane.write_str(&log_text);
    }

    crate::framebuffer::with_back_buffer(|buf, w, h, stride, bpp| {
        let mut target = BackBufDrawTarget {
            buf,
            width: w,
            height: h,
            stride,
            bpp,
        };
        tmp_layout.render_all_and_clear(&mut target);
    });
    crate::framebuffer::blit_full();
}

// ---------------------------------------------------------------------------
// Browser pane keyboard handlers
// ---------------------------------------------------------------------------

/// Handle a character key in the focused browser pane.
fn handle_browser_char(
    c: char,
    layout: &mut Layout,
    pane_types: &mut Vec<PaneType>,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
    focused_pane_id: usize,
) {
    let browser_idx = match pane_types.iter().position(|pt| {
        matches!(pt, PaneType::Browser(bs) if bs.pane_id == focused_pane_id)
    }) {
        Some(idx) => idx,
        None => return,
    };

    let result = if let PaneType::Browser(bs) = &mut pane_types[browser_idx] {
        bs.handle_key(c, stack, now)
    } else {
        return;
    };

    match result {
        crate::browser::BrowserKeyResult::Consumed => {
            // Re-render the browser pane content.
            if let PaneType::Browser(bs) = &pane_types[browser_idx] {
                let rows = layout.pane_by_id_mut(focused_pane_id)
                    .map(|p| p.rows())
                    .unwrap_or(24);
                let rendered = bs.render_to_pane(rows);
                if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                    pane.write_str(&rendered);
                }
            }
        }
        crate::browser::BrowserKeyResult::CloseBrowser => {
            if layout.pane_count() > 1 {
                pane_types.remove(browser_idx);
                layout.close_focused();
                render_full(layout);
            }
        }
    }
}

/// Handle a raw key (arrows, etc.) in the focused browser pane.
fn handle_browser_rawkey(
    key: KeyCode,
    layout: &mut Layout,
    pane_types: &mut Vec<PaneType>,
    focused_pane_id: usize,
) {
    let browser_idx = match pane_types.iter().position(|pt| {
        matches!(pt, PaneType::Browser(bs) if bs.pane_id == focused_pane_id)
    }) {
        Some(idx) => idx,
        None => return,
    };

    if let PaneType::Browser(bs) = &mut pane_types[browser_idx] {
        bs.handle_raw_key(key);

        // Re-render the browser pane content.
        let rows = layout.pane_by_id_mut(focused_pane_id)
            .map(|p| p.rows())
            .unwrap_or(24);
        let rendered = bs.render_to_pane(rows);
        if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
            pane.write_str(&rendered);
        }
    }
}

// ---------------------------------------------------------------------------
// File manager input handlers
// ---------------------------------------------------------------------------

/// Handle a Unicode character input for the focused file manager pane.
fn handle_filemanager_char(
    c: char,
    layout: &mut Layout,
    pane_types: &mut Vec<PaneType>,
    vfs: &mut StubVfs,
    focused_pane_id: usize,
) {
    // Find the file manager pane type mutably.
    let fm = match pane_types.iter_mut().find_map(|pt| match pt {
        PaneType::FileManager(fm) if fm.pane_id == focused_pane_id => Some(fm),
        _ => None,
    }) {
        Some(fm) => fm,
        None => return,
    };

    let visible_rows = layout
        .pane_by_id_mut(focused_pane_id)
        .map(|p| p.rows())
        .unwrap_or(24);

    let action = fm.handle_char(c, vfs);

    match action {
        Some(FileManagerAction::Enter) => {
            let result = fm.enter_selected(vfs);
            if let Some(file_path) = result {
                // File was selected -- log it (editor integration is future work).
                fm.status_message = format!("Open: {} (editor not yet wired)", file_path);
            }
        }
        Some(FileManagerAction::GoParent) => {
            fm.go_parent(vfs);
        }
        Some(FileManagerAction::Redraw) | Some(FileManagerAction::OpenFile(_)) => {
            // Just redraw below.
        }
        None => return,
    }

    // Re-render the file manager pane.
    let cols = layout
        .pane_by_id_mut(focused_pane_id)
        .map(|p| p.cols())
        .unwrap_or(80);
    let rendered = filemanager::render_to_pane(fm, cols, visible_rows);
    if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
        pane.write_str(&rendered);
    }
}

/// Handle a raw key (arrows, etc.) for the focused file manager pane.
fn handle_filemanager_rawkey(
    k: KeyCode,
    layout: &mut Layout,
    pane_types: &mut Vec<PaneType>,
    vfs: &mut StubVfs,
    focused_pane_id: usize,
) {
    let visible_rows = layout
        .pane_by_id_mut(focused_pane_id)
        .map(|p| p.rows())
        .unwrap_or(24);

    let fm = match pane_types.iter_mut().find_map(|pt| match pt {
        PaneType::FileManager(fm) if fm.pane_id == focused_pane_id => Some(fm),
        _ => None,
    }) {
        Some(fm) => fm,
        None => return,
    };

    let action = fm.handle_raw_key(k, visible_rows);

    match action {
        Some(FileManagerAction::Redraw) => {
            // Re-render the file manager pane.
            let cols = layout
                .pane_by_id_mut(focused_pane_id)
                .map(|p| p.cols())
                .unwrap_or(80);
            let rendered = filemanager::render_to_pane(fm, cols, visible_rows);
            if let Some(pane) = layout.pane_by_id_mut(focused_pane_id) {
                pane.write_str(&rendered);
            }
        }
        _ => {}
    }
}
