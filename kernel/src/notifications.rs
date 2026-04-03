//! Notification system — centralised alerts from agents, SSH, timers, cron, system events.
//!
//! Provides a `NotificationCenter` singleton behind a `spin::Mutex`, a set of shell
//! commands (`notifications`, `notif`, `notif clear`), toast rendering that auto-
//! dismisses after 3 seconds, and a PC speaker beep for critical alerts.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use alloc::string::ToString;
use spin::Mutex;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Where the notification originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationSource {
    Agent,
    Ssh,
    System,
    Timer,
    Cron,
    Encryption,
}

impl core::fmt::Display for NotificationSource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NotificationSource::Agent      => write!(f, "agent"),
            NotificationSource::Ssh        => write!(f, "ssh"),
            NotificationSource::System     => write!(f, "system"),
            NotificationSource::Timer      => write!(f, "timer"),
            NotificationSource::Cron       => write!(f, "cron"),
            NotificationSource::Encryption => write!(f, "encrypt"),
        }
    }
}

/// How important the notification is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Info,
    Warning,
    Critical,
}

impl core::fmt::Display for Priority {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Priority::Info     => write!(f, "info"),
            Priority::Warning  => write!(f, "warn"),
            Priority::Critical => write!(f, "CRIT"),
        }
    }
}

/// A single notification entry.
#[derive(Debug, Clone)]
pub struct Notification {
    pub source: NotificationSource,
    pub title: String,
    pub message: String,
    /// Milliseconds since boot (from `interrupts::millis_since_boot()`).
    pub timestamp_ms: i64,
    pub priority: Priority,
    pub read: bool,
}

// ---------------------------------------------------------------------------
// NotificationCenter
// ---------------------------------------------------------------------------

/// Maximum number of notifications retained (oldest are dropped).
const MAX_NOTIFICATIONS: usize = 100;

pub struct NotificationCenter {
    queue: Vec<Notification>,
    /// Timestamp (ms since boot) of the most recent notification that should
    /// be displayed as a toast. 0 = no active toast.
    toast_start_ms: i64,
    /// The text currently showing as a toast overlay.
    toast_text: String,
    /// The priority of the current toast (for colour).
    toast_priority: Priority,
}

impl NotificationCenter {
    pub const fn new() -> Self {
        Self {
            queue: Vec::new(),
            toast_start_ms: 0,
            toast_text: String::new(),
            toast_priority: Priority::Info,
        }
    }

    /// Add a notification. Trims oldest if over capacity.
    pub fn push(&mut self, notif: Notification) {
        // Set up toast display.
        self.toast_start_ms = notif.timestamp_ms;
        self.toast_text = format!("[{}] {}: {}", notif.source, notif.title, notif.message);
        self.toast_priority = notif.priority;

        // Critical notifications beep.
        if notif.priority == Priority::Critical {
            beep();
        }

        self.queue.push(notif);

        // Trim oldest.
        while self.queue.len() > MAX_NOTIFICATIONS {
            self.queue.remove(0);
        }
    }

    /// Number of unread notifications.
    pub fn unread_count(&self) -> usize {
        self.queue.iter().filter(|n| !n.read).count()
    }

    /// Mark all notifications as read.
    pub fn mark_all_read(&mut self) {
        for n in &mut self.queue {
            n.read = true;
        }
    }

    /// Return the most recent `count` notifications (newest first).
    pub fn recent(&self, count: usize) -> Vec<&Notification> {
        let start = if self.queue.len() > count {
            self.queue.len() - count
        } else {
            0
        };
        self.queue[start..].iter().rev().collect()
    }

    /// If a toast should currently be shown, return its text and priority.
    /// Toasts auto-dismiss 3 seconds after they were posted.
    pub fn active_toast(&self, now_ms: i64) -> Option<(&str, Priority)> {
        if self.toast_start_ms == 0 {
            return None;
        }
        let elapsed = now_ms.saturating_sub(self.toast_start_ms);
        if elapsed < 3000 {
            Some((&self.toast_text, self.toast_priority))
        } else {
            None
        }
    }

    /// Clear the toast (called after it has been dismissed or rendered).
    pub fn clear_toast(&mut self) {
        self.toast_start_ms = 0;
        self.toast_text.clear();
    }
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

pub static NOTIFICATIONS: Mutex<NotificationCenter> = Mutex::new(NotificationCenter::new());

// ---------------------------------------------------------------------------
// Convenience function — the main public API
// ---------------------------------------------------------------------------

/// Push a notification into the global centre.
pub fn notify(source: NotificationSource, title: &str, message: &str, priority: Priority) {
    let ts = crate::interrupts::millis_since_boot();
    log::info!(
        "[notif] [{}] [{}] {}: {}",
        priority,
        source,
        title,
        message,
    );
    NOTIFICATIONS.lock().push(Notification {
        source,
        title: title.into(),
        message: message.into(),
        timestamp_ms: ts,
        priority,
        read: false,
    });
}

// ---------------------------------------------------------------------------
// PC speaker beep for critical notifications
// ---------------------------------------------------------------------------

/// Short alert beep — 1 kHz for 150 ms.
fn beep() {
    crate::boot_sound::play_tone(1000, 150);
}

// ---------------------------------------------------------------------------
// Shell command handler
// ---------------------------------------------------------------------------

/// Handle the `notifications` / `notif` shell command.
///
/// Subcommands:
///   (none)       — list recent 20 notifications
///   clear        — mark all as read
///   count        — show unread count
pub fn handle_command(args: &str) -> String {
    let args = args.trim();

    if args == "clear" {
        NOTIFICATIONS.lock().mark_all_read();
        return "All notifications marked as read.\n".to_string();
    }

    if args == "count" {
        let count = NOTIFICATIONS.lock().unread_count();
        return format!("Unread notifications: {}\n", count);
    }

    // Default: list recent notifications.
    let center = NOTIFICATIONS.lock();
    let recent = center.recent(20);

    if recent.is_empty() {
        return "No notifications.\n".to_string();
    }

    let mut out = String::new();
    out.push_str(&format!(
        "  Notifications ({} unread / {} total)\n",
        center.unread_count(),
        center.queue.len(),
    ));
    out.push_str("  ──────────────────────────────────────────────\n");

    for n in &recent {
        let read_marker = if n.read { " " } else { "*" };
        let prio_color = match n.priority {
            Priority::Info     => "37", // white
            Priority::Warning  => "33", // yellow
            Priority::Critical => "31", // red
        };
        // Timestamp: show seconds since boot.
        let secs = n.timestamp_ms / 1000;
        let mins = secs / 60;
        let hrs = mins / 60;
        let time_str = format!("{:02}:{:02}:{:02}", hrs, mins % 60, secs % 60);

        out.push_str(&format!(
            "  {} \x1b[90m{}\x1b[0m \x1b[{}m[{}]\x1b[0m \x1b[36m{}\x1b[0m \x1b[93m{}\x1b[0m: {}\n",
            read_marker,
            time_str,
            prio_color,
            n.priority,
            n.source,
            n.title,
            n.message,
        ));
    }

    out
}

/// Return a short status indicator for the prompt area, e.g. "[3 notif]".
/// Returns empty string if no unread notifications.
pub fn prompt_indicator() -> String {
    let count = NOTIFICATIONS.lock().unread_count();
    if count == 0 {
        String::new()
    } else {
        format!("\x1b[33m[{} notif]\x1b[0m ", count)
    }
}

/// Render a toast overlay at the top of the given pane text.
/// Returns the ANSI string to write, or None if no toast active.
pub fn toast_overlay(now_ms: i64) -> Option<String> {
    let center = NOTIFICATIONS.lock();
    if let Some((text, priority)) = center.active_toast(now_ms) {
        let color = match priority {
            Priority::Info     => "46", // cyan bg
            Priority::Warning  => "43", // yellow bg
            Priority::Critical => "41", // red bg
        };
        // Save cursor, go to row 1, clear line, write toast, restore cursor.
        let toast = format!(
            "\x1b[s\x1b[1;1H\x1b[2K\x1b[{}m\x1b[97m {} \x1b[0m\x1b[u",
            color, text,
        );
        Some(toast)
    } else {
        None
    }
}
