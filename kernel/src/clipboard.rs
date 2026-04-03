//! Global clipboard with history — shared across all panes and virtual consoles.
//!
//! Provides copy/paste via Ctrl+Shift+C / Ctrl+Shift+V and a 10-entry history
//! ring that can be cycled with Ctrl+Shift+H.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// Maximum number of clipboard history entries retained.
const HISTORY_SIZE: usize = 10;

/// Global clipboard state, protected by a spin mutex for interrupt safety.
static CLIPBOARD: Mutex<ClipboardState> = Mutex::new(ClipboardState::new());

/// Internal clipboard state: current buffer + ring history.
struct ClipboardState {
    /// The most recently copied text (the "active" clipboard).
    current: String,
    /// Ring buffer of previous clipboard entries (oldest first).
    history: Vec<String>,
    /// Index into the history ring for Ctrl+Shift+H cycling.
    /// `None` means "use `current`"; `Some(i)` means history[i].
    cycle_index: Option<usize>,
}

impl ClipboardState {
    const fn new() -> Self {
        Self {
            current: String::new(),
            history: Vec::new(),
            cycle_index: None,
        }
    }
}

/// Copy text into the clipboard. Pushes the previous value into history.
pub fn copy(text: &str) {
    if text.is_empty() {
        return;
    }
    let mut cb = CLIPBOARD.lock();
    // Push the old current into history (if non-empty).
    if !cb.current.is_empty() {
        if cb.history.len() >= HISTORY_SIZE {
            cb.history.remove(0);
        }
        let old = core::mem::replace(&mut cb.current, String::new());
        cb.history.push(old);
    }
    cb.current = String::from(text);
    cb.cycle_index = None;
    log::debug!("[clipboard] copied {} bytes", text.len());
}

/// Paste the current clipboard contents. Returns an empty string if empty.
pub fn paste() -> String {
    let cb = CLIPBOARD.lock();
    cb.current.clone()
}

/// Cycle through clipboard history. Each call advances to the next older entry.
/// Wraps around to the current clipboard after exhausting history.
/// Returns the selected entry text.
pub fn cycle_history() -> String {
    let mut cb = CLIPBOARD.lock();
    if cb.history.is_empty() && cb.current.is_empty() {
        return String::new();
    }

    let total = cb.history.len() + 1; // history entries + current
    let next = match cb.cycle_index {
        None => {
            if cb.history.is_empty() {
                // Only current exists, nothing to cycle.
                return cb.current.clone();
            }
            // Start cycling from the most recent history entry.
            Some(cb.history.len() - 1)
        }
        Some(i) => {
            if i == 0 {
                // Wrap back to "current".
                None
            } else {
                Some(i - 1)
            }
        }
    };

    cb.cycle_index = next;
    match next {
        None => cb.current.clone(),
        Some(i) => cb.history.get(i).cloned().unwrap_or_default(),
    }
}

/// Return the number of entries in clipboard history (not counting current).
pub fn history_len() -> usize {
    CLIPBOARD.lock().history.len()
}

/// Check if the clipboard has any content.
pub fn is_empty() -> bool {
    CLIPBOARD.lock().current.is_empty()
}
