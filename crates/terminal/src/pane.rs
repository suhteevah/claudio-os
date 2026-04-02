//! Terminal pane — a character-cell grid backed by a VTE parser.
//!
//! Each pane owns a grid of [`Cell`]s sized to fit its [`Viewport`], a cursor,
//! colour state, and a `vte::Parser` that interprets incoming byte streams as
//! ANSI escape sequences.

use alloc::vec;
use alloc::vec::Vec;

use crate::render::{self, Color, FONT_HEIGHT, FONT_WIDTH};
use crate::Viewport;

// ---------------------------------------------------------------------------
// Cell
// ---------------------------------------------------------------------------

/// A single character cell in the terminal grid.
#[derive(Clone, Copy)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::DEFAULT_FG,
            bg: Color::DEFAULT_BG,
        }
    }
}

// ---------------------------------------------------------------------------
// Pane
// ---------------------------------------------------------------------------

/// A virtual terminal occupying a rectangular region of the framebuffer.
pub struct Pane {
    /// Unique identifier.
    pub id: usize,
    /// Pixel region on the framebuffer.
    pub viewport: Viewport,
    /// Grid dimensions in character cells.
    cols: usize,
    rows: usize,
    /// Cursor position (0-based).
    cursor_row: usize,
    cursor_col: usize,
    /// The visible cell grid — `rows` rows of `cols` cells each.
    cells: Vec<Vec<Cell>>,
    /// Current drawing colours (set by SGR sequences).
    current_fg: Color,
    current_bg: Color,
    /// How many lines have been scrolled beyond the top of the grid.
    /// (reserved for future scrollback buffer)
    scroll_offset: usize,
    /// VTE escape-sequence parser.
    vte_parser: vte::Parser,
    /// Whether the pane needs to be redrawn (any row dirty).
    dirty: bool,
    /// Per-row dirty flags — only dirty rows need re-rendering.
    /// This is the key to TempleOS-style dirty-region tracking: instead of
    /// re-rendering 1280x800 pixels on every keypress, we re-render only the
    /// ~16 pixel-rows of the line that changed.
    dirty_rows: Vec<bool>,
    /// Saved cursor position (row, col) for CSI s / CSI u.
    saved_cursor: Option<(usize, usize)>,
    /// Previous cursor position for efficient cursor redraw.
    prev_cursor: Option<(usize, usize)>,
}

impl Pane {
    /// Create a new pane for the given viewport.
    pub fn new(id: usize, viewport: Viewport) -> Self {
        let cols = viewport.width / FONT_WIDTH;
        let rows = viewport.height / FONT_HEIGHT;
        // Ensure at least 1x1 so indexing never panics.
        let cols = cols.max(1);
        let rows = rows.max(1);

        let cells = vec![vec![Cell::default(); cols]; rows];
        let dirty_rows = vec![true; rows]; // All rows dirty on creation.

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
            dirty: true,
            dirty_rows,
            saved_cursor: None,
            prev_cursor: None,
        }
    }

    // -- public queries -----------------------------------------------------

    /// Grid width in columns.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Grid height in rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Whether the pane content has changed since last render.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the pane as clean (call after rendering).
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
        for row in self.dirty_rows.iter_mut() {
            *row = false;
        }
    }

    /// Mark a specific character row as dirty.
    #[inline]
    fn mark_row_dirty(&mut self, row: usize) {
        if row < self.dirty_rows.len() {
            self.dirty_rows[row] = true;
        }
        self.dirty = true;
    }

    /// Mark all rows as dirty (e.g. after scroll or full-screen erase).
    fn mark_all_dirty(&mut self) {
        for row in self.dirty_rows.iter_mut() {
            *row = true;
        }
        self.dirty = true;
    }

    /// Get the per-row dirty flags.
    pub fn dirty_rows(&self) -> &[bool] {
        &self.dirty_rows
    }

    // -- input --------------------------------------------------------------

    /// Feed a byte slice through the VTE parser.
    ///
    /// This is the main entry point: the caller pushes raw bytes (which may
    /// contain UTF-8 text interleaved with ANSI escape sequences) and the
    /// pane updates its cell grid accordingly.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        // We need to temporarily take the parser out of `self` because
        // `advance` borrows the performer mutably while we also need &mut self.
        let mut parser = core::mem::replace(&mut self.vte_parser, vte::Parser::new());
        let mut performer = PanePerformer { pane: self };
        parser.advance(&mut performer, bytes);
        performer.pane.vte_parser = parser;
    }

    /// Convenience wrapper that accepts a `&str`.
    pub fn write_str(&mut self, s: &str) {
        self.write_bytes(s.as_bytes());
    }

    // -- viewport resize ----------------------------------------------------

    /// Resize the pane to fit a new viewport.
    pub fn resize(&mut self, viewport: Viewport) {
        let new_cols = (viewport.width / FONT_WIDTH).max(1);
        let new_rows = (viewport.height / FONT_HEIGHT).max(1);

        let mut new_cells = vec![vec![Cell::default(); new_cols]; new_rows];

        // Copy over as much of the old content as fits.
        let copy_rows = self.rows.min(new_rows);
        let copy_cols = self.cols.min(new_cols);
        for r in 0..copy_rows {
            for c in 0..copy_cols {
                new_cells[r][c] = self.cells[r][c];
            }
        }

        self.viewport = viewport;
        self.cols = new_cols;
        self.rows = new_rows;
        self.cells = new_cells;
        self.cursor_row = self.cursor_row.min(new_rows - 1);
        self.cursor_col = self.cursor_col.min(new_cols - 1);
        // Clamp saved cursor to new dimensions.
        if let Some((ref mut r, ref mut c)) = self.saved_cursor {
            *r = (*r).min(new_rows - 1);
            *c = (*c).min(new_cols - 1);
        }
        self.dirty_rows = vec![true; new_rows]; // All rows dirty after resize.
        self.dirty = true;
        self.prev_cursor = None;
    }

    // -- internal mutation helpers ------------------------------------------

    /// Write a printable character at the cursor and advance.
    fn put_char(&mut self, c: char) {
        if self.cursor_col >= self.cols {
            self.newline();
        }

        self.cells[self.cursor_row][self.cursor_col] = Cell {
            ch: c,
            fg: self.current_fg,
            bg: self.current_bg,
        };
        self.mark_row_dirty(self.cursor_row);
        self.cursor_col += 1;
    }

    /// Carriage-return + line-feed.
    fn newline(&mut self) {
        self.cursor_col = 0;
        if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
            self.mark_row_dirty(self.cursor_row);
        } else {
            self.scroll_up(); // scroll_up marks all rows dirty
        }
    }

    /// Scroll the grid up by one line, discarding the top row.
    fn scroll_up(&mut self) {
        self.cells.remove(0);
        self.cells.push(vec![Cell::default(); self.cols]);
        self.scroll_offset = self.scroll_offset.saturating_add(1);
        // Scroll affects every visible row.
        self.mark_all_dirty();
    }

    /// Erase cells in a region, filling with the current background colour.
    fn erase_cells(&mut self, row: usize, col_start: usize, col_end: usize) {
        if row >= self.rows {
            return;
        }
        let end = col_end.min(self.cols);
        for c in col_start..end {
            self.cells[row][c] = Cell {
                ch: ' ',
                fg: self.current_fg,
                bg: self.current_bg,
            };
        }
        self.mark_row_dirty(row);
    }

    // -- rendering ----------------------------------------------------------

    /// Render the entire pane onto a [`DrawTarget`](super::DrawTarget).
    pub fn render<D: super::DrawTarget>(&self, target: &mut D) {
        for row in 0..self.rows {
            for col in 0..self.cols {
                let cell = &self.cells[row][col];
                let x = self.viewport.x + col * FONT_WIDTH;
                let y = self.viewport.y + row * FONT_HEIGHT;
                render::render_char(target, x, y, cell.ch, cell.fg, cell.bg);
            }
        }
    }

    /// Render only the rows that have been marked dirty since the last
    /// `clear_dirty()`. This is the hot path for typing: a single character
    /// insert only dirties one row (~16 pixel-rows of ~1280 pixels), not the
    /// entire 1280x800 screen.
    pub fn render_dirty<D: super::DrawTarget>(&self, target: &mut D) {
        for row in 0..self.rows {
            if row < self.dirty_rows.len() && self.dirty_rows[row] {
                for col in 0..self.cols {
                    let cell = &self.cells[row][col];
                    let x = self.viewport.x + col * FONT_WIDTH;
                    let y = self.viewport.y + row * FONT_HEIGHT;
                    render::render_char(target, x, y, cell.ch, cell.fg, cell.bg);
                }
            }
        }
    }

    /// Render the cursor, and also re-render the previous cursor position to
    /// un-invert it (avoids re-rendering the entire pane just to move the cursor).
    pub fn render_cursor_delta<D: super::DrawTarget>(&mut self, target: &mut D) {
        // Restore the old cursor cell to normal rendering.
        if let Some((prev_r, prev_c)) = self.prev_cursor {
            if prev_r < self.rows && prev_c < self.cols {
                let cell = &self.cells[prev_r][prev_c];
                let x = self.viewport.x + prev_c * FONT_WIDTH;
                let y = self.viewport.y + prev_r * FONT_HEIGHT;
                render::render_char(target, x, y, cell.ch, cell.fg, cell.bg);
            }
        }

        // Draw the new cursor position (inverted).
        if self.cursor_row < self.rows && self.cursor_col < self.cols {
            let cell = &self.cells[self.cursor_row][self.cursor_col];
            let x = self.viewport.x + self.cursor_col * FONT_WIDTH;
            let y = self.viewport.y + self.cursor_row * FONT_HEIGHT;
            render::render_char(target, x, y, cell.ch, cell.bg, cell.fg);
        }

        self.prev_cursor = Some((self.cursor_row, self.cursor_col));
    }

    /// Render a visible cursor block (inverted colours).
    pub fn render_cursor<D: super::DrawTarget>(&self, target: &mut D) {
        if self.cursor_row < self.rows && self.cursor_col < self.cols {
            let cell = &self.cells[self.cursor_row][self.cursor_col];
            let x = self.viewport.x + self.cursor_col * FONT_WIDTH;
            let y = self.viewport.y + self.cursor_row * FONT_HEIGHT;
            // Draw the glyph with swapped fg/bg so the cursor is visible.
            render::render_char(target, x, y, cell.ch, cell.bg, cell.fg);
        }
    }
}

// ---------------------------------------------------------------------------
// SGR colour helpers
// ---------------------------------------------------------------------------

/// Map an SGR standard-colour index (0–7) to a [`Color`].
fn sgr_standard_color(idx: u16) -> Color {
    match idx {
        0 => Color::BLACK,
        1 => Color::RED,
        2 => Color::GREEN,
        3 => Color::YELLOW,
        4 => Color::BLUE,
        5 => Color::MAGENTA,
        6 => Color::CYAN,
        7 => Color::WHITE,
        _ => Color::WHITE,
    }
}

/// Map an SGR bright-colour index (0–7) to a bright [`Color`].
fn sgr_bright_color(idx: u16) -> Color {
    match idx {
        0 => Color::BRIGHT_BLACK,
        1 => Color::BRIGHT_RED,
        2 => Color::BRIGHT_GREEN,
        3 => Color::BRIGHT_YELLOW,
        4 => Color::BRIGHT_BLUE,
        5 => Color::BRIGHT_MAGENTA,
        6 => Color::BRIGHT_CYAN,
        7 => Color::BRIGHT_WHITE,
        _ => Color::BRIGHT_WHITE,
    }
}

// ---------------------------------------------------------------------------
// VTE Performer
// ---------------------------------------------------------------------------

/// Adapter that connects the `vte::Perform` callbacks to a [`Pane`].
struct PanePerformer<'a> {
    pane: &'a mut Pane,
}

impl<'a> vte::Perform for PanePerformer<'a> {
    fn print(&mut self, c: char) {
        self.pane.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0B | 0x0C => self.pane.newline(), // LF, VT, FF
            b'\r' => {
                self.pane.cursor_col = 0;
                // CR doesn't change cell content, cursor delta handles it.
            }
            0x08 => {
                // Backspace
                if self.pane.cursor_col > 0 {
                    self.pane.cursor_col -= 1;
                    self.pane.mark_row_dirty(self.pane.cursor_row);
                }
            }
            0x09 => {
                // Tab — advance to next 8-column stop.
                let next = (self.pane.cursor_col + 8) & !7;
                self.pane.cursor_col = next.min(self.pane.cols - 1);
                // Tab doesn't change cell content.
            }
            0x07 => {} // BEL — ignored
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
        let pane = &mut *self.pane;

        match action {
            // -- SGR (Select Graphic Rendition) -----------------------------
            'm' => {
                let mut iter = params.iter();
                // If no params at all, treat as reset (SGR 0).
                if params.is_empty() {
                    pane.current_fg = Color::DEFAULT_FG;
                    pane.current_bg = Color::DEFAULT_BG;
                    return;
                }
                while let Some(param) = iter.next() {
                    let code = param.first().copied().unwrap_or(0);
                    match code {
                        0 => {
                            pane.current_fg = Color::DEFAULT_FG;
                            pane.current_bg = Color::DEFAULT_BG;
                        }
                        1 => {} // bold — not tracked yet
                        // Standard foreground 30–37
                        30..=37 => pane.current_fg = sgr_standard_color(code - 30),
                        39 => pane.current_fg = Color::DEFAULT_FG,
                        // Standard background 40–47
                        40..=47 => pane.current_bg = sgr_standard_color(code - 40),
                        49 => pane.current_bg = Color::DEFAULT_BG,
                        // Bright foreground 90–97
                        90..=97 => pane.current_fg = sgr_bright_color(code - 90),
                        // Bright background 100–107
                        100..=107 => pane.current_bg = sgr_bright_color(code - 100),
                        // 256-colour / truecolour — 38;5;n / 38;2;r;g;b
                        38 => {
                            if let Some(sub) = iter.next() {
                                let kind = sub.first().copied().unwrap_or(0);
                                if kind == 5 {
                                    // 256-colour — we only handle the basic 8+8
                                    if let Some(idx_p) = iter.next() {
                                        let idx = idx_p.first().copied().unwrap_or(0);
                                        if idx < 8 {
                                            pane.current_fg = sgr_standard_color(idx);
                                        } else if idx < 16 {
                                            pane.current_fg = sgr_bright_color(idx - 8);
                                        }
                                        // 16-255: ignore for now
                                    }
                                } else if kind == 2 {
                                    // Truecolour
                                    let r = iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                    let g = iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                    let b = iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                    pane.current_fg = Color::new(r as u8, g as u8, b as u8);
                                }
                            }
                        }
                        48 => {
                            if let Some(sub) = iter.next() {
                                let kind = sub.first().copied().unwrap_or(0);
                                if kind == 5 {
                                    if let Some(idx_p) = iter.next() {
                                        let idx = idx_p.first().copied().unwrap_or(0);
                                        if idx < 8 {
                                            pane.current_bg = sgr_standard_color(idx);
                                        } else if idx < 16 {
                                            pane.current_bg = sgr_bright_color(idx - 8);
                                        }
                                    }
                                } else if kind == 2 {
                                    let r = iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                    let g = iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                    let b = iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                    pane.current_bg = Color::new(r as u8, g as u8, b as u8);
                                }
                            }
                        }
                        _ => {} // Unhandled SGR codes ignored.
                    }
                }
            }

            // -- Cursor positioning -----------------------------------------
            // Pure cursor moves don't change cell data. The cursor is rendered
            // separately via render_cursor_delta, so no row dirtying needed.
            'H' | 'f' => {
                // CUP — Cursor Position. CSI row ; col H
                let mut iter = params.iter();
                let row = iter
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1)
                    .saturating_sub(1) as usize;
                let col = iter
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1)
                    .saturating_sub(1) as usize;
                pane.cursor_row = row.min(pane.rows.saturating_sub(1));
                pane.cursor_col = col.min(pane.cols.saturating_sub(1));
            }
            'A' => {
                // CUU — Cursor Up
                let n = first_param(params, 1) as usize;
                pane.cursor_row = pane.cursor_row.saturating_sub(n);
            }
            'B' => {
                // CUD — Cursor Down
                let n = first_param(params, 1) as usize;
                pane.cursor_row = (pane.cursor_row + n).min(pane.rows.saturating_sub(1));
            }
            'C' => {
                // CUF — Cursor Forward
                let n = first_param(params, 1) as usize;
                pane.cursor_col = (pane.cursor_col + n).min(pane.cols.saturating_sub(1));
            }
            'D' => {
                // CUB — Cursor Back
                let n = first_param(params, 1) as usize;
                pane.cursor_col = pane.cursor_col.saturating_sub(n);
            }
            'G' => {
                // CHA — Cursor Character Absolute
                let col = first_param(params, 1).saturating_sub(1) as usize;
                pane.cursor_col = col.min(pane.cols.saturating_sub(1));
            }
            'd' => {
                // VPA — Vertical Position Absolute
                let row = first_param(params, 1).saturating_sub(1) as usize;
                pane.cursor_row = row.min(pane.rows.saturating_sub(1));
            }

            // -- Erase sequences --------------------------------------------
            'J' => {
                // ED — Erase in Display
                let mode = first_param(params, 0);
                match mode {
                    0 => {
                        // Erase from cursor to end of display.
                        pane.erase_cells(pane.cursor_row, pane.cursor_col, pane.cols);
                        for r in (pane.cursor_row + 1)..pane.rows {
                            pane.erase_cells(r, 0, pane.cols);
                        }
                    }
                    1 => {
                        // Erase from start of display to cursor.
                        for r in 0..pane.cursor_row {
                            pane.erase_cells(r, 0, pane.cols);
                        }
                        pane.erase_cells(pane.cursor_row, 0, pane.cursor_col + 1);
                    }
                    2 | 3 => {
                        // Erase entire display.
                        for r in 0..pane.rows {
                            pane.erase_cells(r, 0, pane.cols);
                        }
                        pane.cursor_row = 0;
                        pane.cursor_col = 0;
                    }
                    _ => {}
                }
            }
            'K' => {
                // EL — Erase in Line
                let mode = first_param(params, 0);
                match mode {
                    0 => pane.erase_cells(pane.cursor_row, pane.cursor_col, pane.cols),
                    1 => pane.erase_cells(pane.cursor_row, 0, pane.cursor_col + 1),
                    2 => pane.erase_cells(pane.cursor_row, 0, pane.cols),
                    _ => {}
                }
            }

            // -- Scroll -----------------------------------------------------
            'S' => {
                // SU — Scroll Up
                let n = first_param(params, 1) as usize;
                for _ in 0..n {
                    pane.scroll_up();
                }
            }

            // -- Cursor save / restore (DECSC / DECRC via CSI) ---------------
            's' => {
                // SCP — Save Cursor Position
                pane.saved_cursor = Some((pane.cursor_row, pane.cursor_col));
            }
            'u' => {
                // RCP — Restore Cursor Position
                if let Some((row, col)) = pane.saved_cursor {
                    pane.cursor_row = row.min(pane.rows.saturating_sub(1));
                    pane.cursor_col = col.min(pane.cols.saturating_sub(1));
                    // Cursor move only — no cell data change.
                }
            }

            // -- Insert/Delete Lines ----------------------------------------
            'L' => {
                // IL — Insert Lines (shifts content down, affects all rows from cursor)
                let n = (first_param(params, 1) as usize).min(pane.rows - pane.cursor_row);
                for _ in 0..n {
                    pane.cells
                        .insert(pane.cursor_row, vec![Cell::default(); pane.cols]);
                    pane.cells.pop();
                }
                // All rows from cursor downward are affected.
                for r in pane.cursor_row..pane.rows {
                    pane.mark_row_dirty(r);
                }
            }
            'M' => {
                // DL — Delete Lines (shifts content up, affects all rows from cursor)
                let n = (first_param(params, 1) as usize).min(pane.rows - pane.cursor_row);
                for _ in 0..n {
                    pane.cells.remove(pane.cursor_row);
                    pane.cells.push(vec![Cell::default(); pane.cols]);
                }
                for r in pane.cursor_row..pane.rows {
                    pane.mark_row_dirty(r);
                }
            }

            _ => {
                // Unknown CSI sequence — silently ignored.
            }
        }
    }

    fn hook(
        &mut self,
        _params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        _action: char,
    ) {
    }

    fn put(&mut self, _byte: u8) {}

    fn unhook(&mut self) {}

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        let pane = &mut *self.pane;
        match byte {
            // DECSC — Save Cursor (ESC 7)
            b'7' => {
                pane.saved_cursor = Some((pane.cursor_row, pane.cursor_col));
            }
            // DECRC — Restore Cursor (ESC 8)
            b'8' => {
                if let Some((row, col)) = pane.saved_cursor {
                    pane.cursor_row = row.min(pane.rows.saturating_sub(1));
                    pane.cursor_col = col.min(pane.cols.saturating_sub(1));
                    // Cursor move only — no cell data change.
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Extract the first CSI parameter, defaulting to `default` when absent or zero.
fn first_param(params: &vte::Params, default: u16) -> u16 {
    params
        .iter()
        .next()
        .and_then(|p| p.first().copied())
        .map(|v| if v == 0 { default } else { v })
        .unwrap_or(default)
}
