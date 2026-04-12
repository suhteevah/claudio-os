//! terminal-ansi — ANSI escape sequence renderer for terminal-core layouts.
//!
//! This is a `std` crate that consumes a [`terminal_core::Layout`] and produces
//! a minimal byte stream of ANSI escape sequences by diffing frames cell-by-cell.
//!
//! Used by claudio-mux (Windows host binary) to render to the host terminal.

extern crate alloc;

pub mod diff;

use alloc::vec;
use alloc::vec::Vec;
use terminal_core::{Cell, Color, Layout, LayoutNode, PaneId, SplitDirection};

// ---------------------------------------------------------------------------
// Scene
// ---------------------------------------------------------------------------

/// Rendering context passed to [`AnsiRenderer::render`].
pub struct Scene<'a> {
    /// The layout to render.
    pub layout: &'a Layout,
    /// The focused pane — its cursor position is where we leave the terminal
    /// cursor after rendering.
    pub focused: PaneId,
    /// Optional status line text drawn on the bottom row (reversed colors).
    pub status_line: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// AnsiRenderer
// ---------------------------------------------------------------------------

/// Stateful ANSI renderer. Holds the previous frame to enable cell diffing.
pub struct AnsiRenderer {
    prev_frame: Vec<Cell>,
    cols: u16,
    rows: u16,
}

impl AnsiRenderer {
    /// Create a new renderer for a terminal of the given dimensions.
    pub fn new(cols: u16, rows: u16) -> Self {
        let total = (cols as usize) * (rows as usize);
        Self {
            prev_frame: vec![Cell::default(); total],
            cols,
            rows,
        }
    }

    /// Resize the renderer, discarding the previous frame (forces full repaint).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        let total = (cols as usize) * (rows as usize);
        self.prev_frame = vec![Cell::default(); total];
    }

    /// Compose a frame from the scene and return the minimal ANSI byte stream
    /// to transition from the previous frame to the new one.
    pub fn render(&mut self, scene: &Scene) -> Vec<u8> {
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let total = cols * rows;

        // 1. Compose next frame as a flat cell buffer.
        let mut next_frame = vec![Cell::default(); total];

        // 2. Walk layout panes, blit their cells into the frame.
        blit_layout(scene.layout.root(), scene.layout, &mut next_frame, cols, rows);

        // 3. Draw separators between splits.
        draw_separators(scene.layout.root(), &mut next_frame, cols, rows);

        // 4. Draw status line on the bottom row (if any).
        if let Some(text) = scene.status_line {
            let row_idx = rows.saturating_sub(1);
            let fg = Color::BLACK;
            let bg = Color::WHITE;
            for (i, ch) in text.chars().enumerate() {
                if i >= cols { break; }
                next_frame[row_idx * cols + i] = Cell { ch, fg, bg };
            }
            // Pad the rest of the status row with spaces.
            let text_len = text.chars().count().min(cols);
            for i in text_len..cols {
                next_frame[row_idx * cols + i] = Cell { ch: ' ', fg, bg };
            }
        }

        // 5. Diff previous frame against next frame.
        let mut out = Vec::new();
        diff::diff_frames(&self.prev_frame, &next_frame, self.cols, self.rows, &mut out);

        // 6. Position cursor at the focused pane's cursor.
        if let Some(pane) = scene.layout.panes().iter().find(|p| p.id == scene.focused) {
            let (cur_row, cur_col) = pane.cursor_pos();
            // Cursor is 0-based within the pane; viewport gives pane origin.
            let abs_row = pane.viewport.row as usize + cur_row;
            let abs_col = pane.viewport.col as usize + cur_col;
            // Clamp to screen bounds.
            let term_row = (abs_row.min(rows.saturating_sub(1)) + 1) as u16;
            let term_col = (abs_col.min(cols.saturating_sub(1)) + 1) as u16;
            diff::write_csi_cup(&mut out, term_row, term_col);
        }

        // 7. Swap frames.
        self.prev_frame = next_frame;

        out
    }
}

// ---------------------------------------------------------------------------
// Frame composition helpers
// ---------------------------------------------------------------------------

/// Recursively walk the layout tree and blit pane cells into the flat frame.
fn blit_layout(node: &LayoutNode, layout: &Layout, frame: &mut Vec<Cell>, cols: usize, rows: usize) {
    match node {
        LayoutNode::Leaf { pane_id, .. } => {
            if let Some(pane) = layout.panes().iter().find(|p| p.id == *pane_id) {
                let vp = &pane.viewport;
                for r in 0..pane.rows() {
                    let abs_row = vp.row as usize + r;
                    if abs_row >= rows { break; }
                    for c in 0..pane.cols() {
                        let abs_col = vp.col as usize + c;
                        if abs_col >= cols { break; }
                        frame[abs_row * cols + abs_col] = *pane.cell(r, c);
                    }
                }
            }
        }
        LayoutNode::Split { first, second, .. } => {
            blit_layout(first, layout, frame, cols, rows);
            blit_layout(second, layout, frame, cols, rows);
        }
    }
}

/// Recursively draw separator lines between split panes.
fn draw_separators(node: &LayoutNode, frame: &mut Vec<Cell>, cols: usize, rows: usize) {
    if let LayoutNode::Split { direction, first, second, .. } = node {
        // Determine the separator column or row from the gap between first and second viewports.
        let sep_cell_fg = Color::BRIGHT_BLACK;
        let sep_cell_bg = Color::DEFAULT_BG;

        match direction {
            SplitDirection::Vertical => {
                // Find the x position of the separator (the column between the two panes).
                if let (
                    Some(first_right),
                    Some(second_left),
                ) = (viewport_right(first), viewport_left(second)) {
                    // Separator fills the gap: first_right..second_left (exclusive).
                    for sep_col in first_right..second_left {
                        let sep_row_start = viewport_top(first).unwrap_or(0);
                        let sep_row_end = viewport_bottom(first).unwrap_or(rows as u16);
                        for r in sep_row_start..sep_row_end {
                            let abs_row = r as usize;
                            let abs_col = sep_col as usize;
                            if abs_row < rows && abs_col < cols {
                                frame[abs_row * cols + abs_col] = Cell {
                                    ch: '│',
                                    fg: sep_cell_fg,
                                    bg: sep_cell_bg,
                                };
                            }
                        }
                    }
                }
            }
            SplitDirection::Horizontal => {
                if let (
                    Some(first_bottom),
                    Some(second_top),
                ) = (viewport_bottom(first), viewport_top(second)) {
                    for sep_row in first_bottom..second_top {
                        let sep_col_start = viewport_left(first).unwrap_or(0);
                        let sep_col_end = viewport_right(first).unwrap_or(cols as u16);
                        for c in sep_col_start..sep_col_end {
                            let abs_row = sep_row as usize;
                            let abs_col = c as usize;
                            if abs_row < rows && abs_col < cols {
                                frame[abs_row * cols + abs_col] = Cell {
                                    ch: '─',
                                    fg: sep_cell_fg,
                                    bg: sep_cell_bg,
                                };
                            }
                        }
                    }
                }
            }
        }

        // Recurse into children.
        draw_separators(first, frame, cols, rows);
        draw_separators(second, frame, cols, rows);
    }
}

// ---------------------------------------------------------------------------
// Viewport edge helpers (walk the leftmost/rightmost leaf of a subtree)
// ---------------------------------------------------------------------------

fn viewport_left(node: &LayoutNode) -> Option<u16> {
    match node {
        LayoutNode::Leaf { viewport, .. } => Some(viewport.col),
        LayoutNode::Split { first, .. } => viewport_left(first),
    }
}

fn viewport_right(node: &LayoutNode) -> Option<u16> {
    match node {
        LayoutNode::Leaf { viewport, .. } => Some(viewport.col + viewport.cols),
        LayoutNode::Split { second, .. } => viewport_right(second),
    }
}

fn viewport_top(node: &LayoutNode) -> Option<u16> {
    match node {
        LayoutNode::Leaf { viewport, .. } => Some(viewport.row),
        LayoutNode::Split { first, .. } => viewport_top(first),
    }
}

fn viewport_bottom(node: &LayoutNode) -> Option<u16> {
    match node {
        LayoutNode::Leaf { viewport, .. } => Some(viewport.row + viewport.rows),
        LayoutNode::Split { second, .. } => viewport_bottom(second),
    }
}
