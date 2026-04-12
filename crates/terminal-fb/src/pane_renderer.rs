//! Pane renderer — converts terminal-core Panes to pixel output on a DrawTarget.

use terminal_core::{Color, LayoutNode, Pane, SplitDirection};
use crate::render::{DrawTarget, FONT_HEIGHT, FONT_WIDTH, fill_rect, render_char};

/// Render all cells in a pane to the draw target.
pub fn render_pane<D: DrawTarget>(pane: &Pane, target: &mut D) {
    for row in 0..pane.rows() {
        for col in 0..pane.cols() {
            let cell = pane.cell(row, col);
            let x = pane.viewport.col as usize * FONT_WIDTH + col * FONT_WIDTH;
            let y = pane.viewport.row as usize * FONT_HEIGHT + row * FONT_HEIGHT;
            render_char(target, x, y, cell.ch, cell.fg, cell.bg);
        }
    }
}

/// Render only dirty rows of a pane to the draw target.
pub fn render_pane_dirty<D: DrawTarget>(pane: &Pane, target: &mut D) {
    let dirty_rows = pane.dirty_rows();
    for row in 0..pane.rows() {
        if row < dirty_rows.len() && !dirty_rows[row] {
            continue;
        }
        for col in 0..pane.cols() {
            let cell = pane.cell(row, col);
            let x = pane.viewport.col as usize * FONT_WIDTH + col * FONT_WIDTH;
            let y = pane.viewport.row as usize * FONT_HEIGHT + row * FONT_HEIGHT;
            render_char(target, x, y, cell.ch, cell.fg, cell.bg);
        }
    }
}

/// Un-invert the previous cursor position and invert the current cursor position.
///
/// "Invert" means swapping fg and bg of the cell at the cursor location.
pub fn render_cursor_delta<D: DrawTarget>(pane: &mut Pane, target: &mut D) {
    // Un-invert previous cursor position if any.
    if let Some((prev_row, prev_col)) = pane.prev_cursor_pos() {
        let cell = pane.cell(prev_row, prev_col);
        let x = pane.viewport.col as usize * FONT_WIDTH + prev_col * FONT_WIDTH;
        let y = pane.viewport.row as usize * FONT_HEIGHT + prev_row * FONT_HEIGHT;
        // Restore normal (un-inverted) rendering.
        render_char(target, x, y, cell.ch, cell.fg, cell.bg);
    }

    // Invert current cursor position.
    let (cur_row, cur_col) = pane.cursor_pos();
    let cell = pane.cell(cur_row, cur_col);
    let x = pane.viewport.col as usize * FONT_WIDTH + cur_col * FONT_WIDTH;
    let y = pane.viewport.row as usize * FONT_HEIGHT + cur_row * FONT_HEIGHT;
    // Swap fg and bg to invert.
    render_char(target, x, y, cell.ch, cell.bg, cell.fg);

    pane.update_prev_cursor();
}

/// Walk the LayoutNode tree and draw separator lines between split panes.
///
/// Vertical splits get a 1-cell-wide vertical separator at the right edge of
/// the first child. Horizontal splits get a 1-cell-tall horizontal separator
/// at the bottom edge of the first child. Separator color is BRIGHT_BLACK.
pub fn render_separators<D: DrawTarget>(node: &LayoutNode, target: &mut D) {
    match node {
        LayoutNode::Leaf { .. } => {
            // No separator for leaves.
        }
        LayoutNode::Split { direction, first, second, .. } => {
            // Recursively render separators in children first.
            render_separators(first, target);
            render_separators(second, target);

            // Draw the separator between first and second.
            match direction {
                SplitDirection::Vertical => {
                    // Separator is a vertical line at the right edge of `first`.
                    // `second.viewport.col - 1` is the separator column.
                    let sep_col = separator_col_of(second);
                    let sep_row = separator_row_of(first);
                    let sep_height = separator_rows_of(first);

                    let x = sep_col as usize * FONT_WIDTH;
                    let y = sep_row as usize * FONT_HEIGHT;
                    let w = FONT_WIDTH;
                    let h = sep_height as usize * FONT_HEIGHT;

                    fill_rect(target, x, y, w, h, Color::BRIGHT_BLACK);
                }
                SplitDirection::Horizontal => {
                    // Separator is a horizontal line at the bottom edge of `first`.
                    // `second.viewport.row - 1` is the separator row.
                    let sep_col = separator_col_of(first);
                    let sep_row = separator_row_of(second).saturating_sub(1);
                    let sep_width = separator_cols_of(first);

                    let x = sep_col as usize * FONT_WIDTH;
                    let y = sep_row as usize * FONT_HEIGHT;
                    let w = sep_width as usize * FONT_WIDTH;
                    let h = FONT_HEIGHT;

                    fill_rect(target, x, y, w, h, Color::BRIGHT_BLACK);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers to extract viewport info from LayoutNode
// ---------------------------------------------------------------------------

fn separator_col_of(node: &LayoutNode) -> u16 {
    match node {
        LayoutNode::Leaf { viewport, .. } => viewport.col,
        LayoutNode::Split { first, .. } => separator_col_of(first),
    }
}

fn separator_row_of(node: &LayoutNode) -> u16 {
    match node {
        LayoutNode::Leaf { viewport, .. } => viewport.row,
        LayoutNode::Split { first, .. } => separator_row_of(first),
    }
}

fn separator_rows_of(node: &LayoutNode) -> u16 {
    match node {
        LayoutNode::Leaf { viewport, .. } => viewport.rows,
        LayoutNode::Split { first, second, direction, .. } => {
            match direction {
                SplitDirection::Vertical => separator_rows_of(first).max(separator_rows_of(second)),
                SplitDirection::Horizontal => separator_rows_of(first) + 1 + separator_rows_of(second),
            }
        }
    }
}

fn separator_cols_of(node: &LayoutNode) -> u16 {
    match node {
        LayoutNode::Leaf { viewport, .. } => viewport.cols,
        LayoutNode::Split { first, second, direction, .. } => {
            match direction {
                SplitDirection::Horizontal => separator_cols_of(first).max(separator_cols_of(second)),
                SplitDirection::Vertical => separator_cols_of(first) + 1 + separator_cols_of(second),
            }
        }
    }
}
