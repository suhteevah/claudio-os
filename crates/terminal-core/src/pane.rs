//! Pane — a terminal cell grid with VTE parser integration.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use crate::{Cell, Color, CellViewport, PaneId};

// ---------------------------------------------------------------------------
// 256-color palette lookup
// ---------------------------------------------------------------------------

fn color256(idx: u8) -> Color {
    match idx {
        0 => Color::BLACK,
        1 => Color::RED,
        2 => Color::GREEN,
        3 => Color::YELLOW,
        4 => Color::BLUE,
        5 => Color::MAGENTA,
        6 => Color::CYAN,
        7 => Color::WHITE,
        8 => Color::BRIGHT_BLACK,
        9 => Color::BRIGHT_RED,
        10 => Color::BRIGHT_GREEN,
        11 => Color::BRIGHT_YELLOW,
        12 => Color::BRIGHT_BLUE,
        13 => Color::BRIGHT_MAGENTA,
        14 => Color::BRIGHT_CYAN,
        15 => Color::BRIGHT_WHITE,
        16..=231 => {
            let n = idx - 16;
            let b = n % 6;
            let g = (n / 6) % 6;
            let r = n / 36;
            Color::new(r * 51, g * 51, b * 51)
        }
        232..=255 => {
            let v = (idx - 232) * 10 + 8;
            Color::new(v, v, v)
        }
    }
}

// ---------------------------------------------------------------------------
// Pane
// ---------------------------------------------------------------------------

pub struct Pane {
    pub id: PaneId,
    pub viewport: CellViewport,
    cols: usize,
    rows: usize,
    cursor_row: usize,
    cursor_col: usize,
    cells: Vec<Vec<Cell>>,
    current_fg: Color,
    current_bg: Color,
    scroll_offset: usize,
    vte_parser: vte::Parser,
    dirty: bool,
    dirty_rows: Vec<bool>,
    saved_cursor: Option<(usize, usize)>,
    prev_cursor: Option<(usize, usize)>,
}

impl Pane {
    pub fn new(id: PaneId, viewport: CellViewport) -> Self {
        let cols = viewport.cols as usize;
        let rows = viewport.rows as usize;
        let cells = (0..rows).map(|_| vec![Cell::default(); cols]).collect();
        let dirty_rows = vec![false; rows];
        Self {
            id,
            viewport,
            cols,
            rows,
            cursor_row: 0,
            cursor_col: 0,
            cells,
            current_fg: Color::DEFAULT_FG,
            current_bg: Color::DEFAULT_BG,
            scroll_offset: 0,
            vte_parser: vte::Parser::new(),
            dirty: false,
            dirty_rows,
            saved_cursor: None,
            prev_cursor: None,
        }
    }

    pub fn cols(&self) -> usize { self.cols }
    pub fn rows(&self) -> usize { self.rows }
    pub fn is_dirty(&self) -> bool { self.dirty }
    pub fn dirty_rows(&self) -> &[bool] { &self.dirty_rows }

    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        static DEFAULT: Cell = Cell { ch: ' ', fg: Color::DEFAULT_FG, bg: Color::DEFAULT_BG };
        if row < self.rows && col < self.cols {
            &self.cells[row][col]
        } else {
            &DEFAULT
        }
    }

    pub fn cursor_pos(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    pub fn prev_cursor_pos(&self) -> Option<(usize, usize)> {
        self.prev_cursor
    }

    pub fn update_prev_cursor(&mut self) {
        self.prev_cursor = Some((self.cursor_row, self.cursor_col));
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
        for d in self.dirty_rows.iter_mut() { *d = false; }
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        let mut parser = core::mem::replace(&mut self.vte_parser, vte::Parser::new());
        for &byte in bytes {
            let mut performer = PanePerformer { pane: self };
            parser.advance(&mut performer, &[byte]);
        }
        self.vte_parser = parser;
    }

    pub fn write_str(&mut self, s: &str) {
        self.write_bytes(s.as_bytes());
    }

    pub fn resize(&mut self, viewport: CellViewport) {
        let new_cols = viewport.cols as usize;
        let new_rows = viewport.rows as usize;

        // Preserve existing content
        let new_cells: Vec<Vec<Cell>> = (0..new_rows)
            .map(|r| {
                let mut row = vec![Cell::default(); new_cols];
                if r < self.rows {
                    let copy_cols = new_cols.min(self.cols);
                    row[..copy_cols].copy_from_slice(&self.cells[r][..copy_cols]);
                }
                row
            })
            .collect();

        // Clamp cursor
        let new_cursor_row = self.cursor_row.min(new_rows.saturating_sub(1));
        let new_cursor_col = self.cursor_col.min(new_cols.saturating_sub(1));

        let new_dirty_rows = vec![false; new_rows];

        self.viewport = viewport;
        self.cols = new_cols;
        self.rows = new_rows;
        self.cells = new_cells;
        self.dirty_rows = new_dirty_rows;
        self.cursor_row = new_cursor_row;
        self.cursor_col = new_cursor_col;
        self.dirty = true;
    }

    // Internal helpers used by PanePerformer
    fn put_char(&mut self, c: char) {
        if self.cursor_row >= self.rows {
            self.scroll_up(1);
            self.cursor_row = self.rows - 1;
        }
        if self.cursor_col < self.cols {
            self.cells[self.cursor_row][self.cursor_col] = Cell {
                ch: c,
                fg: self.current_fg,
                bg: self.current_bg,
            };
            self.dirty = true;
            if self.cursor_row < self.dirty_rows.len() {
                self.dirty_rows[self.cursor_row] = true;
            }
            self.cursor_col += 1;
        }
        // wrap
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.cursor_row += 1;
            if self.cursor_row >= self.rows {
                self.scroll_up(1);
                self.cursor_row = self.rows - 1;
            }
        }
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n {
            if !self.cells.is_empty() {
                self.cells.remove(0);
                self.cells.push(vec![Cell::default(); self.cols]);
                // shift dirty_rows
                if !self.dirty_rows.is_empty() {
                    self.dirty_rows.remove(0);
                    self.dirty_rows.push(true);
                }
            }
        }
        self.dirty = true;
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    fn erase_in_display(&mut self, mode: u16) {
        match mode {
            // Erase from cursor to end
            0 => {
                if self.cursor_row < self.rows {
                    for col in self.cursor_col..self.cols {
                        self.cells[self.cursor_row][col] = Cell::default();
                    }
                    self.dirty_rows[self.cursor_row] = true;
                    for row in (self.cursor_row + 1)..self.rows {
                        self.cells[row] = vec![Cell::default(); self.cols];
                        self.dirty_rows[row] = true;
                    }
                }
            }
            // Erase from beginning to cursor
            1 => {
                for row in 0..self.cursor_row {
                    self.cells[row] = vec![Cell::default(); self.cols];
                    self.dirty_rows[row] = true;
                }
                if self.cursor_row < self.rows {
                    for col in 0..=self.cursor_col.min(self.cols.saturating_sub(1)) {
                        self.cells[self.cursor_row][col] = Cell::default();
                    }
                    self.dirty_rows[self.cursor_row] = true;
                }
            }
            // Erase all
            2 | 3 => {
                for row in 0..self.rows {
                    self.cells[row] = vec![Cell::default(); self.cols];
                    self.dirty_rows[row] = true;
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn erase_in_line(&mut self, mode: u16) {
        if self.cursor_row >= self.rows { return; }
        match mode {
            0 => {
                for col in self.cursor_col..self.cols {
                    self.cells[self.cursor_row][col] = Cell::default();
                }
            }
            1 => {
                for col in 0..=self.cursor_col.min(self.cols.saturating_sub(1)) {
                    self.cells[self.cursor_row][col] = Cell::default();
                }
            }
            2 => {
                for col in 0..self.cols {
                    self.cells[self.cursor_row][col] = Cell::default();
                }
            }
            _ => {}
        }
        self.dirty_rows[self.cursor_row] = true;
        self.dirty = true;
    }

    fn insert_lines(&mut self, n: usize) {
        let row = self.cursor_row;
        for _ in 0..n {
            if self.cells.len() > row {
                if self.cells.len() >= self.rows {
                    self.cells.pop();
                    self.dirty_rows.pop();
                }
                self.cells.insert(row, vec![Cell::default(); self.cols]);
                self.dirty_rows.insert(row, true);
            }
        }
        // Trim to rows
        self.cells.truncate(self.rows);
        self.dirty_rows.truncate(self.rows);
        while self.cells.len() < self.rows {
            self.cells.push(vec![Cell::default(); self.cols]);
            self.dirty_rows.push(true);
        }
        self.dirty = true;
    }

    fn delete_lines(&mut self, n: usize) {
        let row = self.cursor_row;
        for _ in 0..n {
            if row < self.cells.len() {
                self.cells.remove(row);
                self.dirty_rows.remove(row);
            }
        }
        while self.cells.len() < self.rows {
            self.cells.push(vec![Cell::default(); self.cols]);
            self.dirty_rows.push(true);
        }
        self.dirty = true;
    }

    fn apply_sgr(&mut self, params: &vte::Params) {
        let mut iter = params.iter();
        loop {
            let sub = match iter.next() {
                Some(s) => s,
                None => break,
            };
            let code = sub[0];
            match code {
                0 => {
                    self.current_fg = Color::DEFAULT_FG;
                    self.current_bg = Color::DEFAULT_BG;
                }
                30..=37 => { self.current_fg = ansi_color(code - 30, false); }
                39 => { self.current_fg = Color::DEFAULT_FG; }
                40..=47 => { self.current_bg = ansi_color(code - 40, false); }
                49 => { self.current_bg = Color::DEFAULT_BG; }
                90..=97 => { self.current_fg = ansi_color(code - 90, true); }
                100..=107 => { self.current_bg = ansi_color(code - 100, true); }
                38 => {
                    // Need next subparams — peek ahead in flat list
                    // vte Params iter gives sub-parameters per ; separated group
                    // For 38;2;r;g;b or 38;5;n we need to peek into next items
                    if sub.len() >= 5 && sub[1] == 2 {
                        self.current_fg = Color::new(sub[2] as u8, sub[3] as u8, sub[4] as u8);
                    } else if sub.len() >= 3 && sub[1] == 5 {
                        self.current_fg = color256(sub[2] as u8);
                    } else {
                        // Try reading from separate params
                        let mode = match iter.next() { Some(s) => s[0], None => break };
                        if mode == 2 {
                            let r = match iter.next() { Some(s) => s[0] as u8, None => 0 };
                            let g = match iter.next() { Some(s) => s[0] as u8, None => 0 };
                            let b = match iter.next() { Some(s) => s[0] as u8, None => 0 };
                            self.current_fg = Color::new(r, g, b);
                        } else if mode == 5 {
                            let n = match iter.next() { Some(s) => s[0] as u8, None => 0 };
                            self.current_fg = color256(n);
                        }
                    }
                }
                48 => {
                    if sub.len() >= 5 && sub[1] == 2 {
                        self.current_bg = Color::new(sub[2] as u8, sub[3] as u8, sub[4] as u8);
                    } else if sub.len() >= 3 && sub[1] == 5 {
                        self.current_bg = color256(sub[2] as u8);
                    } else {
                        let mode = match iter.next() { Some(s) => s[0], None => break };
                        if mode == 2 {
                            let r = match iter.next() { Some(s) => s[0] as u8, None => 0 };
                            let g = match iter.next() { Some(s) => s[0] as u8, None => 0 };
                            let b = match iter.next() { Some(s) => s[0] as u8, None => 0 };
                            self.current_bg = Color::new(r, g, b);
                        } else if mode == 5 {
                            let n = match iter.next() { Some(s) => s[0] as u8, None => 0 };
                            self.current_bg = color256(n);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// VTE performer
// ---------------------------------------------------------------------------

struct PanePerformer<'a> {
    pane: &'a mut Pane,
}

impl<'a> vte::Perform for PanePerformer<'a> {
    fn print(&mut self, c: char) {
        self.pane.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x0A | 0x0B | 0x0C => {
                // LF / VT / FF — newline mode: move down and return to col 0
                self.pane.cursor_col = 0;
                self.pane.cursor_row += 1;
                if self.pane.cursor_row >= self.pane.rows {
                    self.pane.scroll_up(1);
                    self.pane.cursor_row = self.pane.rows - 1;
                }
            }
            0x0D => {
                // CR
                self.pane.cursor_col = 0;
            }
            0x09 => {
                // TAB — advance to next 8-col boundary
                let next = (self.pane.cursor_col / 8 + 1) * 8;
                self.pane.cursor_col = next.min(self.pane.cols - 1);
            }
            0x08 => {
                // BS
                if self.pane.cursor_col > 0 {
                    self.pane.cursor_col -= 1;
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let p = &mut ParamsHelper::new(params);
        match action {
            'A' => {
                // Cursor Up
                let n = p.next_or(1) as usize;
                self.pane.cursor_row = self.pane.cursor_row.saturating_sub(n);
            }
            'B' => {
                // Cursor Down
                let n = p.next_or(1) as usize;
                self.pane.cursor_row = (self.pane.cursor_row + n).min(self.pane.rows.saturating_sub(1));
            }
            'C' => {
                // Cursor Forward
                let n = p.next_or(1) as usize;
                self.pane.cursor_col = (self.pane.cursor_col + n).min(self.pane.cols.saturating_sub(1));
            }
            'D' => {
                // Cursor Back
                let n = p.next_or(1) as usize;
                self.pane.cursor_col = self.pane.cursor_col.saturating_sub(n);
            }
            'H' | 'f' => {
                // Cursor Position (1-based)
                let row = p.next_or(1) as usize;
                let col = p.next_or(1) as usize;
                self.pane.cursor_row = row.saturating_sub(1).min(self.pane.rows.saturating_sub(1));
                self.pane.cursor_col = col.saturating_sub(1).min(self.pane.cols.saturating_sub(1));
            }
            'G' => {
                // Cursor Horizontal Absolute (1-based)
                let col = p.next_or(1) as usize;
                self.pane.cursor_col = col.saturating_sub(1).min(self.pane.cols.saturating_sub(1));
            }
            'd' => {
                // Line Position Absolute (1-based)
                let row = p.next_or(1) as usize;
                self.pane.cursor_row = row.saturating_sub(1).min(self.pane.rows.saturating_sub(1));
            }
            's' => {
                // Save cursor
                self.pane.saved_cursor = Some((self.pane.cursor_row, self.pane.cursor_col));
            }
            'u' => {
                // Restore cursor
                if let Some((r, c)) = self.pane.saved_cursor {
                    self.pane.cursor_row = r;
                    self.pane.cursor_col = c;
                }
            }
            'J' => {
                let mode = p.next_or(0);
                self.pane.erase_in_display(mode);
            }
            'K' => {
                let mode = p.next_or(0);
                self.pane.erase_in_line(mode);
            }
            'S' => {
                // Scroll Up
                let n = p.next_or(1) as usize;
                self.pane.scroll_up(n);
            }
            'L' => {
                // Insert Lines
                let n = p.next_or(1) as usize;
                self.pane.insert_lines(n);
            }
            'M' => {
                // Delete Lines
                let n = p.next_or(1) as usize;
                self.pane.delete_lines(n);
            }
            'm' => {
                // SGR
                self.pane.apply_sgr(params);
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => {
                // DECSC — save cursor
                self.pane.saved_cursor = Some((self.pane.cursor_row, self.pane.cursor_col));
            }
            b'8' => {
                // DECRC — restore cursor
                if let Some((r, c)) = self.pane.saved_cursor {
                    self.pane.cursor_row = r;
                    self.pane.cursor_col = c;
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: iterate params as a flat stream of u16 values
// ---------------------------------------------------------------------------

struct ParamsHelper<'a> {
    outer: vte::ParamsIter<'a>,
    inner: core::slice::Iter<'a, u16>,
}

impl<'a> ParamsHelper<'a> {
    fn new(params: &'a vte::Params) -> Self {
        let mut outer = params.iter();
        let inner: core::slice::Iter<'a, u16> = match outer.next() {
            Some(s) => s.iter(),
            None => [].iter(),
        };
        Self { outer, inner }
    }

    fn next_or(&mut self, default: u16) -> u16 {
        loop {
            if let Some(&v) = self.inner.next() {
                return if v == 0 { default } else { v };
            }
            match self.outer.next() {
                Some(s) => self.inner = s.iter(),
                None => return default,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SGR color helpers
// ---------------------------------------------------------------------------

fn ansi_color(idx: u16, bright: bool) -> Color {
    match (idx, bright) {
        (0, false) => Color::BLACK,
        (1, false) => Color::RED,
        (2, false) => Color::GREEN,
        (3, false) => Color::YELLOW,
        (4, false) => Color::BLUE,
        (5, false) => Color::MAGENTA,
        (6, false) => Color::CYAN,
        (7, false) => Color::WHITE,
        (0, true) => Color::BRIGHT_BLACK,
        (1, true) => Color::BRIGHT_RED,
        (2, true) => Color::BRIGHT_GREEN,
        (3, true) => Color::BRIGHT_YELLOW,
        (4, true) => Color::BRIGHT_BLUE,
        (5, true) => Color::BRIGHT_MAGENTA,
        (6, true) => Color::BRIGHT_CYAN,
        (7, true) => Color::BRIGHT_WHITE,
        _ => Color::DEFAULT_FG,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewport::CellViewport;

    fn make_pane(cols: u16, rows: u16) -> Pane {
        Pane::new(0, CellViewport::new(0, 0, cols, rows))
    }

    #[test]
    fn new_pane_has_correct_dimensions() {
        let p = make_pane(80, 24);
        assert_eq!(p.cols(), 80);
        assert_eq!(p.rows(), 24);
    }

    #[test]
    fn write_str_places_chars() {
        let mut p = make_pane(80, 24);
        p.write_str("Hi");
        assert_eq!(p.cell(0, 0).ch, 'H');
        assert_eq!(p.cell(0, 1).ch, 'i');
        assert_eq!(p.cell(0, 2).ch, ' ');
    }

    #[test]
    fn newline_advances_row() {
        let mut p = make_pane(80, 24);
        p.write_str("A\nB");
        assert_eq!(p.cell(0, 0).ch, 'A');
        assert_eq!(p.cell(1, 0).ch, 'B');
    }

    #[test]
    fn write_wraps_at_column_limit() {
        let mut p = make_pane(3, 2);
        p.write_str("ABCD");
        assert_eq!(p.cell(0, 0).ch, 'A');
        assert_eq!(p.cell(0, 1).ch, 'B');
        assert_eq!(p.cell(0, 2).ch, 'C');
        assert_eq!(p.cell(1, 0).ch, 'D');
    }

    #[test]
    fn sgr_color_changes() {
        let mut p = make_pane(80, 24);
        p.write_bytes(b"\x1b[31mX");
        assert_eq!(p.cell(0, 0).fg, Color::RED);
    }

    #[test]
    fn cursor_movement() {
        let mut p = make_pane(80, 24);
        p.write_bytes(b"\x1b[3;5HX");
        assert_eq!(p.cell(2, 4).ch, 'X');
    }

    #[test]
    fn dirty_tracking() {
        let mut p = make_pane(80, 24);
        assert!(!p.is_dirty());
        p.write_str("A");
        assert!(p.is_dirty());
        assert!(p.dirty_rows()[0]);
        assert!(!p.dirty_rows()[1]);
        p.clear_dirty();
        assert!(!p.is_dirty());
    }

    #[test]
    fn resize_preserves_content() {
        let mut p = make_pane(80, 24);
        p.write_str("Hello");
        p.resize(CellViewport::new(0, 0, 40, 12));
        assert_eq!(p.cols(), 40);
        assert_eq!(p.rows(), 12);
        assert_eq!(p.cell(0, 0).ch, 'H');
    }

    #[test]
    fn erase_in_display() {
        let mut p = make_pane(80, 24);
        p.write_str("ABCDEF");
        p.write_bytes(b"\x1b[2J");
        assert_eq!(p.cell(0, 0).ch, ' ');
        assert_eq!(p.cell(0, 5).ch, ' ');
    }

    #[test]
    fn scroll_up() {
        let mut p = make_pane(10, 3);
        p.write_str("AAA\nBBB\nCCC");
        p.write_str("\nDDD");
        assert_eq!(p.cell(0, 0).ch, 'B');
        assert_eq!(p.cell(2, 0).ch, 'D');
    }
}
