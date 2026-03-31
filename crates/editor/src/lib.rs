//! Minimal text editor for ClaudioOS terminal panes.
//!
//! A tiny nano-like editor that manages a text buffer and renders via ANSI
//! escape sequences. It does not write to disk — the caller receives buffer
//! contents on save (Ctrl+S) and can persist however it likes.
//!
//! This crate is `#![no_std]` and depends only on `alloc`.

#![no_std]
extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;
use pc_keyboard::{DecodedKey, KeyCode};

// ─── Public types ────────────────────────────────────────────────────────────

/// Action returned by the editor after processing a keypress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorAction {
    /// Nothing special happened; the caller should re-render.
    Continue,
    /// The user pressed Ctrl+S. Contains the filename.
    Save(String),
    /// The user pressed Ctrl+Q.
    Quit,
}

/// A minimal text editor that operates on an in-memory buffer.
pub struct Editor {
    /// Lines of text, each a `Vec<char>` so we can handle any Unicode.
    lines: Vec<Vec<char>>,
    /// Current cursor row (0-indexed into `lines`).
    cursor_row: usize,
    /// Current cursor column (0-indexed into the current line).
    cursor_col: usize,
    /// First visible line index (vertical scroll offset).
    scroll_offset: usize,
    /// Name shown in the status bar.
    filename: String,
    /// Whether the buffer has been modified since last save/load.
    modified: bool,
    /// Terminal width in columns.
    width: usize,
    /// Terminal height in rows.
    height: usize,
    /// Clipboard for cut-line (Ctrl+X).
    cut_buffer: Option<Vec<char>>,
}

// ─── Line-number gutter width ────────────────────────────────────────────────
// We use a fixed 5-char gutter: "NNN| " where NNN is right-justified.
const GUTTER_WIDTH: usize = 6; // e.g. "  1 | "

// ─── Implementation ──────────────────────────────────────────────────────────

impl Editor {
    /// Create a new empty editor sized for the given terminal dimensions.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            lines: vec![Vec::new()],
            cursor_row: 0,
            cursor_col: 0,
            scroll_offset: 0,
            filename: String::from("[untitled]"),
            modified: false,
            width,
            height,
            cut_buffer: None,
        }
    }

    /// Load file content into the editor, replacing whatever was there.
    pub fn load(&mut self, filename: &str, content: &str) {
        self.filename = String::from(filename);
        self.lines.clear();
        for line in content.split('\n') {
            self.lines.push(line.chars().collect());
        }
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.scroll_offset = 0;
        self.modified = false;
    }

    /// Resize the editor viewport.
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.ensure_cursor_visible();
    }

    /// Process a single keypress and return the resulting action.
    pub fn handle_key(&mut self, key: DecodedKey) -> EditorAction {
        match key {
            DecodedKey::Unicode(c) => self.handle_unicode(c),
            DecodedKey::RawKey(k) => self.handle_raw_key(k),
        }
    }

    /// Render the entire editor screen as a string of ANSI escape sequences.
    ///
    /// The output clears the screen, draws line-numbered content, a status bar,
    /// and a help bar, then positions the cursor.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.width * self.height * 2);

        // Hide cursor + move to top-left.
        let _ = out.write_str("\x1b[?25l\x1b[H");

        let text_rows = self.text_rows();
        let text_cols = self.text_cols();

        for row in 0..text_rows {
            let line_idx = self.scroll_offset + row;

            // Move to start of this row.
            let _ = write!(out, "\x1b[{};1H", row + 1);

            if line_idx < self.lines.len() {
                // Line number in gutter (1-indexed).
                let _ = write!(out, "\x1b[90m{:>4} \x1b[34m\u{2502}\x1b[0m", line_idx + 1);

                // Line content, truncated to fit.
                let line = &self.lines[line_idx];
                let end = core::cmp::min(line.len(), text_cols);
                for ch in &line[..end] {
                    out.push(*ch);
                }
            } else {
                // Tilde for lines past end of file.
                let _ = write!(out, "\x1b[90m   ~ \x1b[34m\u{2502}\x1b[0m");
            }

            // Clear to end of line.
            let _ = out.write_str("\x1b[K");
        }

        // ── Status bar ───────────────────────────────────────────────────
        let _ = write!(out, "\x1b[{};1H", text_rows + 1);
        let _ = out.write_str("\x1b[7m"); // Reverse video.

        let mod_indicator = if self.modified { " [+]" } else { "" };
        let left = {
            let mut s = String::new();
            let _ = write!(s, " {}{}", self.filename, mod_indicator);
            s
        };
        let right = {
            let mut s = String::new();
            let _ = write!(
                s,
                "Ln {}, Col {} ",
                self.cursor_row + 1,
                self.cursor_col + 1
            );
            s
        };

        let _ = out.write_str(&left);

        // Fill the gap with spaces.
        let fill = if self.width > left.len() + right.len() {
            self.width - left.len() - right.len()
        } else {
            0
        };
        for _ in 0..fill {
            out.push(' ');
        }

        let _ = out.write_str(&right);
        let _ = out.write_str("\x1b[0m"); // Reset.

        // ── Help bar ─────────────────────────────────────────────────────
        let _ = write!(out, "\x1b[{};1H", text_rows + 2);
        let _ = out.write_str("\x1b[90m ^S Save  ^Q Quit  ^X Cut line\x1b[0m\x1b[K");

        // ── Position the cursor ──────────────────────────────────────────
        let screen_row = self.cursor_row - self.scroll_offset + 1;
        let screen_col = self.cursor_col + GUTTER_WIDTH + 1;
        let _ = write!(out, "\x1b[{};{}H", screen_row, screen_col);

        // Show cursor.
        let _ = out.write_str("\x1b[?25h");

        out
    }

    /// Return the full buffer content as a plain `String`.
    pub fn content(&self) -> String {
        let mut s = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            for ch in line {
                s.push(*ch);
            }
        }
        s
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    /// Number of rows available for text (excludes status bar + help bar).
    fn text_rows(&self) -> usize {
        if self.height > 2 {
            self.height - 2
        } else {
            1
        }
    }

    /// Number of columns available for text (excludes the gutter).
    fn text_cols(&self) -> usize {
        if self.width > GUTTER_WIDTH {
            self.width - GUTTER_WIDTH
        } else {
            1
        }
    }

    /// Ensure the cursor is within the visible scroll window.
    fn ensure_cursor_visible(&mut self) {
        let rows = self.text_rows();
        if self.cursor_row < self.scroll_offset {
            self.scroll_offset = self.cursor_row;
        } else if self.cursor_row >= self.scroll_offset + rows {
            self.scroll_offset = self.cursor_row - rows + 1;
        }
    }

    /// Clamp cursor_col so it does not exceed the current line length.
    fn clamp_col(&mut self) {
        let len = self.lines[self.cursor_row].len();
        if self.cursor_col > len {
            self.cursor_col = len;
        }
    }

    fn handle_unicode(&mut self, c: char) -> EditorAction {
        match c {
            // Ctrl+S — save.
            '\x13' => EditorAction::Save(self.filename.clone()),

            // Ctrl+Q — quit.
            '\x11' => EditorAction::Quit,

            // Ctrl+X — cut current line.
            '\x18' => {
                self.cut_buffer = Some(self.lines[self.cursor_row].clone());
                if self.lines.len() > 1 {
                    self.lines.remove(self.cursor_row);
                    if self.cursor_row >= self.lines.len() {
                        self.cursor_row = self.lines.len() - 1;
                    }
                } else {
                    self.lines[0].clear();
                }
                self.clamp_col();
                self.ensure_cursor_visible();
                self.modified = true;
                EditorAction::Continue
            }

            // Enter — split line at cursor.
            '\n' | '\r' => {
                let rest: Vec<char> = self.lines[self.cursor_row]
                    .drain(self.cursor_col..)
                    .collect();
                self.cursor_row += 1;
                self.lines.insert(self.cursor_row, rest);
                self.cursor_col = 0;
                self.ensure_cursor_visible();
                self.modified = true;
                EditorAction::Continue
            }

            // Backspace (some terminals send 0x08, some send 0x7F).
            '\x08' | '\x7f' => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                    self.lines[self.cursor_row].remove(self.cursor_col);
                    self.modified = true;
                } else if self.cursor_row > 0 {
                    // Join with previous line.
                    let current = self.lines.remove(self.cursor_row);
                    self.cursor_row -= 1;
                    self.cursor_col = self.lines[self.cursor_row].len();
                    self.lines[self.cursor_row].extend(current);
                    self.ensure_cursor_visible();
                    self.modified = true;
                }
                EditorAction::Continue
            }

            // Tab — insert 4 spaces.
            '\t' => {
                for _ in 0..4 {
                    self.lines[self.cursor_row].insert(self.cursor_col, ' ');
                    self.cursor_col += 1;
                }
                self.modified = true;
                EditorAction::Continue
            }

            // Regular printable character.
            c if c >= ' ' => {
                self.lines[self.cursor_row].insert(self.cursor_col, c);
                self.cursor_col += 1;
                self.modified = true;
                EditorAction::Continue
            }

            // Ignore other control characters.
            _ => EditorAction::Continue,
        }
    }

    fn handle_raw_key(&mut self, key: KeyCode) -> EditorAction {
        match key {
            KeyCode::ArrowUp => {
                if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    self.clamp_col();
                    self.ensure_cursor_visible();
                }
            }
            KeyCode::ArrowDown => {
                if self.cursor_row + 1 < self.lines.len() {
                    self.cursor_row += 1;
                    self.clamp_col();
                    self.ensure_cursor_visible();
                }
            }
            KeyCode::ArrowLeft => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                } else if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    self.cursor_col = self.lines[self.cursor_row].len();
                    self.ensure_cursor_visible();
                }
            }
            KeyCode::ArrowRight => {
                let len = self.lines[self.cursor_row].len();
                if self.cursor_col < len {
                    self.cursor_col += 1;
                } else if self.cursor_row + 1 < self.lines.len() {
                    self.cursor_row += 1;
                    self.cursor_col = 0;
                    self.ensure_cursor_visible();
                }
            }
            KeyCode::Home => {
                self.cursor_col = 0;
            }
            KeyCode::End => {
                self.cursor_col = self.lines[self.cursor_row].len();
            }
            KeyCode::PageUp => {
                let jump = self.text_rows();
                self.cursor_row = self.cursor_row.saturating_sub(jump);
                self.clamp_col();
                self.ensure_cursor_visible();
            }
            KeyCode::PageDown => {
                let jump = self.text_rows();
                self.cursor_row =
                    core::cmp::min(self.cursor_row + jump, self.lines.len().saturating_sub(1));
                self.clamp_col();
                self.ensure_cursor_visible();
            }
            KeyCode::Delete => {
                let len = self.lines[self.cursor_row].len();
                if self.cursor_col < len {
                    self.lines[self.cursor_row].remove(self.cursor_col);
                    self.modified = true;
                } else if self.cursor_row + 1 < self.lines.len() {
                    // Join next line onto current.
                    let next = self.lines.remove(self.cursor_row + 1);
                    self.lines[self.cursor_row].extend(next);
                    self.modified = true;
                }
            }
            _ => {}
        }
        EditorAction::Continue
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn new_editor_has_one_empty_line() {
        let ed = Editor::new(80, 24);
        assert_eq!(ed.lines.len(), 1);
        assert!(ed.lines[0].is_empty());
        assert_eq!(ed.cursor_row, 0);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn load_splits_lines() {
        let mut ed = Editor::new(80, 24);
        ed.load("test.rs", "hello\nworld\nfoo");
        assert_eq!(ed.lines.len(), 3);
        assert_eq!(ed.content(), "hello\nworld\nfoo");
        assert!(!ed.modified);
    }

    #[test]
    fn insert_characters() {
        let mut ed = Editor::new(80, 24);
        ed.handle_key(DecodedKey::Unicode('H'));
        ed.handle_key(DecodedKey::Unicode('i'));
        assert_eq!(ed.content(), "Hi");
        assert!(ed.modified);
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn enter_splits_line() {
        let mut ed = Editor::new(80, 24);
        ed.load("f.txt", "abcd");
        ed.cursor_col = 2;
        ed.handle_key(DecodedKey::Unicode('\n'));
        assert_eq!(ed.lines.len(), 2);
        assert_eq!(ed.content(), "ab\ncd");
        assert_eq!(ed.cursor_row, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn backspace_deletes_char() {
        let mut ed = Editor::new(80, 24);
        ed.load("f.txt", "abc");
        ed.cursor_col = 3;
        ed.handle_key(DecodedKey::Unicode('\x08'));
        assert_eq!(ed.content(), "ab");
    }

    #[test]
    fn backspace_joins_lines() {
        let mut ed = Editor::new(80, 24);
        ed.load("f.txt", "ab\ncd");
        ed.cursor_row = 1;
        ed.cursor_col = 0;
        ed.handle_key(DecodedKey::Unicode('\x08'));
        assert_eq!(ed.content(), "abcd");
        assert_eq!(ed.cursor_row, 0);
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn ctrl_s_returns_save() {
        let mut ed = Editor::new(80, 24);
        ed.load("myfile.rs", "code");
        let action = ed.handle_key(DecodedKey::Unicode('\x13'));
        assert_eq!(action, EditorAction::Save(String::from("myfile.rs")));
    }

    #[test]
    fn ctrl_q_returns_quit() {
        let mut ed = Editor::new(80, 24);
        let action = ed.handle_key(DecodedKey::Unicode('\x11'));
        assert_eq!(action, EditorAction::Quit);
    }

    #[test]
    fn ctrl_x_cuts_line() {
        let mut ed = Editor::new(80, 24);
        ed.load("f.txt", "aaa\nbbb\nccc");
        ed.cursor_row = 1;
        ed.handle_key(DecodedKey::Unicode('\x18'));
        assert_eq!(ed.content(), "aaa\nccc");
        assert_eq!(ed.cursor_row, 1);
    }

    #[test]
    fn arrow_keys_move_cursor() {
        let mut ed = Editor::new(80, 24);
        ed.load("f.txt", "hello\nworld");
        ed.handle_key(DecodedKey::RawKey(KeyCode::ArrowDown));
        assert_eq!(ed.cursor_row, 1);
        ed.handle_key(DecodedKey::RawKey(KeyCode::ArrowRight));
        assert_eq!(ed.cursor_col, 1);
        ed.handle_key(DecodedKey::RawKey(KeyCode::ArrowUp));
        assert_eq!(ed.cursor_row, 0);
        assert_eq!(ed.cursor_col, 1);
        ed.handle_key(DecodedKey::RawKey(KeyCode::ArrowLeft));
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn render_produces_output() {
        let mut ed = Editor::new(80, 24);
        ed.load("test.txt", "line 1\nline 2");
        let rendered = ed.render();
        // Should contain ANSI escape sequences and the filename.
        assert!(rendered.contains("test.txt"));
        assert!(rendered.contains("Ln 1"));
    }

    #[test]
    fn scroll_on_many_lines() {
        let mut ed = Editor::new(80, 10); // 8 text rows (10 - status - help)
        let mut content = String::new();
        for i in 0..20 {
            if i > 0 {
                content.push('\n');
            }
            content.push_str("line");
        }
        ed.load("big.txt", &content);

        // Move to the bottom.
        for _ in 0..19 {
            ed.handle_key(DecodedKey::RawKey(KeyCode::ArrowDown));
        }
        assert_eq!(ed.cursor_row, 19);
        // scroll_offset should have adjusted.
        assert!(ed.scroll_offset > 0);
    }
}
