//! Cell-by-cell frame diff engine.
//!
//! Compares two frames of [`Cell`]s and emits minimal ANSI escape sequences
//! to transition from `prev` to `next`.

extern crate alloc;

use alloc::vec::Vec;
use terminal_core::{Cell, Color};

// ---------------------------------------------------------------------------
// Number formatting helpers (no std::fmt needed)
// ---------------------------------------------------------------------------

/// Write a u16 as ASCII decimal digits into buf.
pub fn write_u16(buf: &mut Vec<u8>, mut n: u16) {
    if n == 0 {
        buf.push(b'0');
        return;
    }
    let start = buf.len();
    while n > 0 {
        buf.push(b'0' + (n % 10) as u8);
        n /= 10;
    }
    // digits are in reverse order — flip the slice we just appended
    buf[start..].reverse();
}

/// Write a u8 as ASCII decimal digits into buf.
pub fn write_u8(buf: &mut Vec<u8>, n: u8) {
    write_u16(buf, n as u16);
}

// ---------------------------------------------------------------------------
// ANSI sequence builders
// ---------------------------------------------------------------------------

/// Emit `ESC [ row ; col H` (CUP — cursor position, 1-based).
pub fn write_csi_cup(buf: &mut Vec<u8>, row: u16, col: u16) {
    buf.push(0x1B); // ESC
    buf.push(b'[');
    write_u16(buf, row);
    buf.push(b';');
    write_u16(buf, col);
    buf.push(b'H');
}

/// Emit `ESC [ 38;2;r;g;b ; 48;2;r;g;b m` (truecolor SGR for fg + bg).
pub fn write_sgr(buf: &mut Vec<u8>, fg: Color, bg: Color) {
    buf.push(0x1B); // ESC
    buf.push(b'[');
    // Foreground: 38;2;r;g;b
    buf.extend_from_slice(b"38;2;");
    write_u8(buf, fg.r);
    buf.push(b';');
    write_u8(buf, fg.g);
    buf.push(b';');
    write_u8(buf, fg.b);
    buf.push(b';');
    // Background: 48;2;r;g;b
    buf.extend_from_slice(b"48;2;");
    write_u8(buf, bg.r);
    buf.push(b';');
    write_u8(buf, bg.g);
    buf.push(b';');
    write_u8(buf, bg.b);
    buf.push(b'm');
}

// ---------------------------------------------------------------------------
// Main diff function
// ---------------------------------------------------------------------------

/// Walk `prev` and `next` cell-by-cell, emitting ANSI only for changed cells.
///
/// `cols` and `rows` define the frame dimensions. Frames are stored row-major:
/// index = row * cols + col.
///
/// Cursor tracking avoids redundant CUP sequences when the cursor naturally
/// advances left-to-right in the same row.
pub fn diff_frames(prev: &[Cell], next: &[Cell], cols: u16, rows: u16, buf: &mut Vec<u8>) {
    let total = (cols as usize) * (rows as usize);
    let len = total.min(prev.len()).min(next.len());

    // Last emitted colors — None means we haven't emitted any SGR yet.
    let mut last_fg: Option<Color> = None;
    let mut last_bg: Option<Color> = None;

    // Last cursor position we explicitly moved to (1-based row, col).
    // We track where the cursor "naturally" is after writing a character
    // so we can skip redundant CUP sequences.
    let mut natural_row: Option<u16> = None;
    let mut natural_col: Option<u16> = None;

    for idx in 0..len {
        let p = &prev[idx];
        let n = &next[idx];

        if p == n {
            // Cell unchanged — natural cursor position is now undefined for
            // the purposes of the next changed cell (we didn't emit anything).
            natural_row = None;
            natural_col = None;
            continue;
        }

        // Compute 1-based row/col for this cell.
        let col_0 = (idx % cols as usize) as u16;
        let row_0 = (idx / cols as usize) as u16;
        let term_row = row_0 + 1;
        let term_col = col_0 + 1;

        // Emit CUP only if the cursor isn't already here.
        let cursor_here = natural_row == Some(term_row) && natural_col == Some(term_col);
        if !cursor_here {
            write_csi_cup(buf, term_row, term_col);
        }

        // Emit SGR if colors changed.
        let colors_changed = last_fg != Some(n.fg) || last_bg != Some(n.bg);
        if colors_changed {
            write_sgr(buf, n.fg, n.bg);
            last_fg = Some(n.fg);
            last_bg = Some(n.bg);
        }

        // Emit the character. ASCII fast-path; non-ASCII encoded as UTF-8.
        if n.ch.is_ascii() {
            buf.push(n.ch as u8);
        } else {
            let mut tmp = [0u8; 4];
            let s = n.ch.encode_utf8(&mut tmp);
            buf.extend_from_slice(s.as_bytes());
        }

        // After writing, the cursor advances to the next column (same row).
        natural_row = Some(term_row);
        natural_col = Some(term_col + 1);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use terminal_core::{Cell, Color};

    fn cell(ch: char) -> Cell {
        Cell { ch, fg: Color::DEFAULT_FG, bg: Color::DEFAULT_BG }
    }

    #[test]
    fn identical_frames_emit_nothing() {
        let frame = vec![cell('A'), cell('B')];
        let mut buf = Vec::new();
        diff_frames(&frame, &frame, 2, 1, &mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn single_cell_change_emits_position_and_char() {
        let prev = vec![cell('A'), cell('B')];
        let next = vec![cell('A'), cell('X')];
        let mut buf = Vec::new();
        diff_frames(&prev, &next, 2, 1, &mut buf);
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("1;2H"), "expected cursor move, got: {}", output);
        assert!(output.contains('X'));
    }

    #[test]
    fn color_change_emits_sgr() {
        let prev = vec![cell('A')];
        let mut next_cell = cell('A');
        next_cell.fg = Color::RED;
        let next = vec![next_cell];
        let mut buf = Vec::new();
        diff_frames(&prev, &next, 1, 1, &mut buf);
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("38;2;204;0;0"), "expected red SGR, got: {}", output);
    }

    #[test]
    fn full_screen_change() {
        let prev = vec![cell(' '); 4];
        let next = vec![cell('A'), cell('B'), cell('C'), cell('D')];
        let mut buf = Vec::new();
        diff_frames(&prev, &next, 2, 2, &mut buf);
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains('A'));
        assert!(output.contains('D'));
    }
}
