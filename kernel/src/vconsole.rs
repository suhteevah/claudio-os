//! Virtual consoles — Ctrl+Alt+F1 through F6.
//!
//! Each virtual console owns an independent terminal buffer (a snapshot of pane
//! content) so switching consoles is instantaneous. Console 1 is the default
//! dashboard. Consoles 2-5 are additional dashboard/shell instances. Console 6
//! is a read-only kernel log viewer that captures all serial log output.
//!
//! ## Key detection
//!
//! `pc-keyboard` delivers F1-F6 as `RawKey(F1)` .. `RawKey(F6)`. Modifier state
//! (Ctrl, Alt) is tracked in the scancode decoder. Because the `HandleControl`
//! mode we use maps Ctrl+letter to Unicode control codes, we instead track
//! modifier state ourselves from raw scancodes to detect Ctrl+Alt+Fn combos.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

/// Number of virtual consoles.
pub const NUM_CONSOLES: usize = 6;

/// Index of the currently active console (0-based). Accessed from the keyboard
/// ISR path so we use an atomic.
static ACTIVE_CONSOLE: AtomicUsize = AtomicUsize::new(0);

/// Per-console saved framebuffer content. When switching away from a console we
/// snapshot the back buffer; when switching back we restore it and blit.
static CONSOLE_STATE: Mutex<ConsoleManager> = Mutex::new(ConsoleManager::new());

/// Kernel log ring buffer — console 6 (index 5) displays this.
static KERNEL_LOG: Mutex<KernelLogBuffer> = Mutex::new(KernelLogBuffer::new());

// ---------------------------------------------------------------------------
// Kernel log ring buffer
// ---------------------------------------------------------------------------

/// Fixed-capacity ring buffer that stores kernel log lines for console 6.
struct KernelLogBuffer {
    lines: Vec<String>,
    max_lines: usize,
}

impl KernelLogBuffer {
    const fn new() -> Self {
        Self {
            lines: Vec::new(),
            max_lines: 2000,
        }
    }

    fn push_line(&mut self, line: String) {
        if self.lines.len() >= self.max_lines {
            self.lines.remove(0);
        }
        self.lines.push(line);
    }

    fn as_text(&self, max_rows: usize) -> String {
        let start = if self.lines.len() > max_rows {
            self.lines.len() - max_rows
        } else {
            0
        };
        let mut out = String::new();
        for line in &self.lines[start..] {
            out.push_str(line);
            out.push_str("\r\n");
        }
        out
    }

    fn line_count(&self) -> usize {
        self.lines.len()
    }
}

/// Append a log line to the kernel log buffer. Called from the logger hook.
pub fn push_kernel_log(line: &str) {
    // Try-lock to avoid deadlock if called from interrupt context.
    if let Some(mut log) = KERNEL_LOG.try_lock() {
        log.push_line(String::from(line));
    }
}

/// Get the kernel log text (last `max_rows` lines) for rendering in console 6.
pub fn kernel_log_text(max_rows: usize) -> String {
    KERNEL_LOG.lock().as_text(max_rows)
}

/// Get total kernel log line count.
pub fn kernel_log_line_count() -> usize {
    KERNEL_LOG.lock().line_count()
}

// ---------------------------------------------------------------------------
// Console manager
// ---------------------------------------------------------------------------

/// Saved state for a single virtual console.
struct ConsoleSave {
    /// Snapshot of the back-buffer pixels (full framebuffer).
    framebuffer_snapshot: Vec<u8>,
    /// Whether this console has ever been activated (has valid snapshot).
    initialized: bool,
    /// Human-readable label.
    label: &'static str,
}

impl ConsoleSave {
    const fn new(label: &'static str) -> Self {
        Self {
            framebuffer_snapshot: Vec::new(),
            initialized: false,
            label,
        }
    }
}

/// Manages all virtual console state.
struct ConsoleManager {
    consoles: Vec<ConsoleSave>,
    fb_size: usize, // cached framebuffer byte size
}

impl ConsoleManager {
    const fn new() -> Self {
        Self {
            consoles: Vec::new(),
            fb_size: 0,
        }
    }

    fn ensure_init(&mut self) {
        if !self.consoles.is_empty() {
            return;
        }
        self.consoles.push(ConsoleSave::new("Dashboard"));
        self.consoles.push(ConsoleSave::new("Console 2"));
        self.consoles.push(ConsoleSave::new("Console 3"));
        self.consoles.push(ConsoleSave::new("Console 4"));
        self.consoles.push(ConsoleSave::new("Console 5"));
        self.consoles.push(ConsoleSave::new("Kernel Log"));
        // Mark console 0 as initialized (it's the one we booted into).
        self.consoles[0].initialized = true;

        // Cache framebuffer size.
        let h = crate::framebuffer::height();
        let stride = crate::framebuffer::stride();
        let bpp = crate::framebuffer::bytes_per_pixel();
        self.fb_size = h * stride * bpp;
    }

    /// Save the current back-buffer into the console at `index`.
    fn save_console(&mut self, index: usize) {
        self.ensure_init();
        if index >= self.consoles.len() {
            return;
        }

        // Read the back buffer into the console's snapshot.
        let fb_size = self.fb_size;
        let console = &mut self.consoles[index];

        crate::framebuffer::with_back_buffer(|buf, _w, _h, _stride, _bpp| {
            let needed = fb_size.min(buf.len());
            console.framebuffer_snapshot.clear();
            console.framebuffer_snapshot.reserve(needed);
            console.framebuffer_snapshot.extend_from_slice(&buf[..needed]);
            console.initialized = true;
        });
    }

    /// Restore a console's snapshot into the back-buffer and blit.
    fn restore_console(&mut self, index: usize) {
        self.ensure_init();
        if index >= self.consoles.len() {
            return;
        }

        let console = &self.consoles[index];
        if !console.initialized || console.framebuffer_snapshot.is_empty() {
            return;
        }

        crate::framebuffer::with_back_buffer(|buf, _w, _h, _stride, _bpp| {
            let copy_len = console.framebuffer_snapshot.len().min(buf.len());
            buf[..copy_len].copy_from_slice(&console.framebuffer_snapshot[..copy_len]);
        });
        crate::framebuffer::blit_full();
    }

    fn label(&self, index: usize) -> &'static str {
        if index < self.consoles.len() {
            self.consoles[index].label
        } else {
            "???"
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Get the currently active console index (0-based).
pub fn active_console() -> usize {
    ACTIVE_CONSOLE.load(Ordering::Relaxed)
}

/// Switch to virtual console `n` (0-based, must be < NUM_CONSOLES).
///
/// Saves the current console's framebuffer, restores the target console's
/// framebuffer, and updates the active index. Returns `true` if the switch
/// happened, `false` if already on that console or index is invalid.
pub fn switch_console(n: usize) -> bool {
    if n >= NUM_CONSOLES {
        return false;
    }
    let current = ACTIVE_CONSOLE.load(Ordering::Relaxed);
    if n == current {
        return false;
    }

    log::info!(
        "[vconsole] switching {} -> {} ({})",
        current + 1,
        n + 1,
        CONSOLE_STATE.lock().label(n)
    );

    let mut mgr = CONSOLE_STATE.lock();
    mgr.ensure_init();

    // Save current console.
    mgr.save_console(current);

    // Update active index.
    ACTIVE_CONSOLE.store(n, Ordering::Relaxed);

    // Restore target console.
    mgr.restore_console(n);

    true
}

/// Check if the kernel log console (console 6, index 5) is active.
pub fn is_kernel_log_active() -> bool {
    ACTIVE_CONSOLE.load(Ordering::Relaxed) == 5
}

/// Initialize the console manager. Call once after framebuffer init.
pub fn init() {
    let mut mgr = CONSOLE_STATE.lock();
    mgr.ensure_init();
    log::info!("[vconsole] {} virtual consoles initialized (Ctrl+Alt+F1-F6)", NUM_CONSOLES);
}

/// Render the kernel log console (console 6). Called when console 6 is active
/// and needs a refresh.
pub fn render_kernel_log_console() {
    let fb_height = crate::framebuffer::height();
    let font_height = claudio_terminal::FONT_HEIGHT;
    let max_rows = if font_height > 0 { fb_height / font_height } else { 40 };

    let text = kernel_log_text(max_rows.saturating_sub(2)); // leave room for header

    // Render into the back buffer: header + log lines.
    crate::framebuffer::with_back_buffer(|buf, w, h, stride, bpp| {
        // Clear to dark background.
        for byte in buf.iter_mut() {
            *byte = 0x10; // very dark grey
        }

        // Use a minimal text renderer — write header.
        let header = "\x1b[96m[ Kernel Log — Console 6 (read-only) ]\x1b[0m\r\n\x1b[90m────────────────────────────────────────\x1b[0m\r\n";

        // We render through a temporary terminal pane for ANSI support.
        // For simplicity, use claudio_terminal's standalone render if available,
        // or just write raw lines. Since we have Layout in dashboard, the actual
        // rendering will be driven from dashboard.rs when console 6 is switched to.
        let _ = (w, h, stride, bpp, header, text);
    });
}

// ---------------------------------------------------------------------------
// Modifier tracking for Ctrl+Alt detection
// ---------------------------------------------------------------------------

/// Raw scancode-level modifier tracking. The pc-keyboard crate handles key
/// decoding but doesn't expose modifier state directly. We track Ctrl and Alt
/// make/break scancodes ourselves.
static MODIFIERS: Mutex<ModifierState> = Mutex::new(ModifierState::new());

/// Tracked modifier keys.
struct ModifierState {
    ctrl_held: bool,
    alt_held: bool,
    shift_held: bool,
}

impl ModifierState {
    const fn new() -> Self {
        Self {
            ctrl_held: false,
            alt_held: false,
            shift_held: false,
        }
    }
}

/// Update modifier state from a raw scancode. Called from the keyboard ISR
/// BEFORE the scancode is pushed to the decoder queue.
///
/// Scancode set 1 (AT keyboard):
/// - Left Ctrl:  make=0x1D, break=0x9D
/// - Left Alt:   make=0x38, break=0xB8
/// - Left Shift: make=0x2A, break=0xAA
/// - Right Shift: make=0x36, break=0xB6
///
/// Returns `Some(console_index)` if Ctrl+Alt+F1-F6 was detected (consuming
/// the scancode), `None` otherwise.
pub fn process_scancode(scancode: u8) -> Option<usize> {
    let mut mods = MODIFIERS.lock();

    match scancode {
        // Make codes (key press).
        0x1D => { mods.ctrl_held = true; return None; }
        0x38 => { mods.alt_held = true; return None; }
        0x2A => { mods.shift_held = true; return None; }
        0x36 => { mods.shift_held = true; return None; }
        // Break codes (key release).
        0x9D => { mods.ctrl_held = false; return None; }
        0xB8 => { mods.alt_held = false; return None; }
        0xAA => { mods.shift_held = false; return None; }
        0xB6 => { mods.shift_held = false; return None; }
        _ => {}
    }

    // Check for Ctrl+Alt+F1-F6 (F1=0x3B .. F6=0x40).
    if mods.ctrl_held && mods.alt_held {
        match scancode {
            0x3B => return Some(0), // F1
            0x3C => return Some(1), // F2
            0x3D => return Some(2), // F3
            0x3E => return Some(3), // F4
            0x3F => return Some(4), // F5
            0x40 => return Some(5), // F6
            _ => {}
        }
    }

    None
}

/// Check if Ctrl+Shift is currently held (for clipboard shortcuts).
pub fn ctrl_shift_held() -> bool {
    let mods = MODIFIERS.lock();
    mods.ctrl_held && mods.shift_held
}

/// Check if Ctrl is currently held.
pub fn ctrl_held() -> bool {
    MODIFIERS.lock().ctrl_held
}

/// Check if Shift is currently held.
pub fn shift_held() -> bool {
    MODIFIERS.lock().shift_held
}
