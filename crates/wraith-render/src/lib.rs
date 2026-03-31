//! # wraith-render
//!
//! Text-mode HTML renderer for ClaudioOS.
//!
//! Takes a parsed `wraith_dom::Document` and renders it to a character-cell
//! grid ([`TextPage`]) suitable for display in a terminal pane. Think `lynx`
//! but simpler — just enough to render OAuth login pages.
//!
//! ## Usage
//!
//! ```ignore
//! let doc = wraith_dom::parse(html);
//! let page = wraith_render::render(&doc, 80, 24);
//! // iterate page.cells and draw to framebuffer
//! ```

#![no_std]
extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use wraith_dom::{Document, NodeData};

/// Look up an attribute value in a `Vec<(String, String)>` by name.
fn attr_get<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a String> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Check if an attribute exists in a `Vec<(String, String)>`.
fn attr_has(attrs: &[(String, String)], key: &str) -> bool {
    attrs.iter().any(|(k, _)| k == key)
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A styled character in the text grid.
#[derive(Clone, Copy)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub underline: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            underline: false,
        }
    }
}

/// Basic colours matching a typical terminal palette.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Color {
    Default,
    White,
    Black,
    Blue,
    Cyan,
    Green,
    Yellow,
    Red,
    Magenta,
    Gray,
}

/// A rendered text page.
pub struct TextPage {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<Vec<Cell>>,
    /// Clickable link regions.
    pub links: Vec<LinkRegion>,
    /// Form input regions.
    pub inputs: Vec<InputRegion>,
    /// Page title.
    pub title: String,
    /// Total content height (may exceed `height` — for scrolling).
    pub content_height: usize,
}

/// A clickable link region on the rendered page.
pub struct LinkRegion {
    pub row: usize,
    pub col_start: usize,
    pub col_end: usize,
    pub url: String,
}

/// An input field region on the rendered page.
pub struct InputRegion {
    pub row: usize,
    pub col_start: usize,
    pub col_end: usize,
    pub name: String,
    pub input_type: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Style context (pushed/popped as we walk the tree)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Style {
    fg: Color,
    bg: Color,
    bold: bool,
    underline: bool,
    /// True inside a <pre> block.
    preformatted: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            underline: false,
            preformatted: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Renderer state
// ---------------------------------------------------------------------------

struct Renderer<'a> {
    doc: &'a Document,
    width: usize,
    /// Current cursor position.
    row: usize,
    col: usize,
    /// The cell grid — grows as content is appended.
    lines: Vec<Vec<Cell>>,
    /// Active style stack.
    style: Style,
    /// Collected link regions.
    links: Vec<LinkRegion>,
    /// Collected input regions.
    inputs: Vec<InputRegion>,
    /// Page title.
    title: String,
    /// Ordered-list counters stack.
    ol_counters: Vec<usize>,
}

impl<'a> Renderer<'a> {
    fn new(doc: &'a Document, width: usize) -> Self {
        Self {
            doc,
            width,
            row: 0,
            col: 0,
            lines: vec![vec![Cell::default(); width]],
            style: Style::default(),
            links: Vec::new(),
            inputs: Vec::new(),
            title: String::new(),
            ol_counters: Vec::new(),
        }
    }

    // -- cursor helpers -----------------------------------------------------

    /// Ensure the grid has at least `row + 1` rows.
    fn ensure_row(&mut self, row: usize) {
        while self.lines.len() <= row {
            self.lines.push(vec![Cell::default(); self.width]);
        }
    }

    /// Move to the start of a new line.
    fn newline(&mut self) {
        self.row += 1;
        self.col = 0;
        self.ensure_row(self.row);
    }

    /// Insert a blank line (for paragraph spacing).
    fn blank_line(&mut self) {
        // Only add if we aren't already on a blank line.
        if self.col > 0 {
            self.newline();
        }
        self.newline();
    }

    /// Ensure we are at the start of a new line (for block elements).
    fn ensure_newline(&mut self) {
        if self.col > 0 {
            self.newline();
        }
    }

    /// Write a single styled character at the cursor.
    fn put_char(&mut self, ch: char) {
        if ch == '\n' {
            self.newline();
            return;
        }
        if self.col >= self.width {
            self.newline();
        }
        self.ensure_row(self.row);
        self.lines[self.row][self.col] = Cell {
            ch,
            fg: self.style.fg,
            bg: self.style.bg,
            bold: self.style.bold,
            underline: self.style.underline,
        };
        self.col += 1;
    }

    /// Write a string at the cursor, with word-wrapping.
    fn put_str(&mut self, s: &str) {
        if self.style.preformatted {
            for ch in s.chars() {
                self.put_char(ch);
            }
            return;
        }
        self.put_str_wrapped(s);
    }

    /// Write text with word-wrapping at width boundary.
    fn put_str_wrapped(&mut self, s: &str) {
        let words = split_words(s);
        for word in words {
            if word == " " {
                // Space — only emit if we aren't at line start.
                if self.col > 0 && self.col < self.width {
                    self.put_char(' ');
                }
                continue;
            }
            let len = word.len();
            // If the word doesn't fit on the current line, wrap.
            if self.col > 0 && self.col + len > self.width {
                self.newline();
            }
            for ch in word.chars() {
                self.put_char(ch);
            }
        }
    }

    /// Write a string without wrapping (for fixed-width elements like inputs).
    fn put_str_raw(&mut self, s: &str) {
        for ch in s.chars() {
            if self.col >= self.width {
                break; // truncate rather than wrap
            }
            self.put_char(ch);
        }
    }

    // -- DOM walking --------------------------------------------------------

    fn render_node(&mut self, idx: usize) {
        let node = &self.doc.nodes[idx];
        match &node.data {
            NodeData::Text(text) => {
                self.render_text(text);
            }
            NodeData::Comment(_) => {
                // Skip comments.
            }
            NodeData::Element { tag, attributes } => {
                let tag = tag.clone();
                let attrs = attributes.clone();
                // Root synthetic element — just render children
                if tag == "[root]" {
                    let children: Vec<usize> = self.doc.nodes[idx].children.clone();
                    for child in children {
                        self.render_node(child);
                    }
                } else {
                    self.render_element(idx, &tag, &attrs);
                }
            }
        }
    }

    fn render_text(&mut self, text: &str) {
        if self.style.preformatted {
            self.put_str(text);
        } else {
            // Collapse whitespace.
            let collapsed = collapse_whitespace(text);
            if !collapsed.is_empty() {
                self.put_str(&collapsed);
            }
        }
    }

    fn render_element(&mut self, idx: usize, tag: &str, attrs: &[(String, String)]) {
        // Hidden elements — skip entirely.
        match tag {
            "script" | "style" | "head" | "meta" | "link" | "noscript" => return,
            _ => {}
        }

        // Extract title from <title> within head (but head is skipped above,
        // so try to grab it if it appears).
        if tag == "title" {
            let mut title = String::new();
            let children: Vec<usize> = self.doc.nodes[idx].children.clone();
            for child in children {
                if let NodeData::Text(t) = &self.doc.nodes[child].data {
                    title.push_str(t);
                }
            }
            self.title = title;
            return;
        }

        let saved_style = self.style.clone();

        match tag {
            // -- Block elements ---------------------------------------------
            "html" | "body" | "div" | "section" | "article" | "main" | "nav"
            | "header" | "footer" | "aside" | "figure" | "figcaption"
            | "details" | "summary" | "fieldset" | "legend" => {
                self.ensure_newline();
                self.render_children(idx);
                self.ensure_newline();
            }

            "p" => {
                self.blank_line();
                self.render_children(idx);
                self.blank_line();
            }

            "br" => {
                self.newline();
            }

            "h1" => self.render_heading(idx, 1),
            "h2" => self.render_heading(idx, 2),
            "h3" => self.render_heading(idx, 3),
            "h4" => self.render_heading(idx, 4),
            "h5" => self.render_heading(idx, 5),
            "h6" => self.render_heading(idx, 6),

            "hr" => {
                self.ensure_newline();
                for _ in 0..self.width {
                    self.put_char('\u{2500}'); // ─
                }
                self.newline();
            }

            "pre" => {
                self.ensure_newline();
                self.style.preformatted = true;
                self.style.fg = Color::Green;
                self.render_children(idx);
                self.style = saved_style;
                self.ensure_newline();
            }

            "blockquote" => {
                self.render_blockquote(idx);
            }

            "ul" => {
                self.ensure_newline();
                self.render_children(idx);
                self.ensure_newline();
            }

            "ol" => {
                self.ensure_newline();
                self.ol_counters.push(1);
                self.render_children(idx);
                self.ol_counters.pop();
                self.ensure_newline();
            }

            "li" => {
                self.ensure_newline();
                // Check if parent is an <ol>.
                let is_ordered = if let Some(parent_idx) = self.doc.nodes[idx].parent {
                    parent_idx < self.doc.nodes.len() && matches!(
                        &self.doc.nodes[parent_idx].data,
                        NodeData::Element { tag, .. } if tag == "ol"
                    )
                } else {
                    false
                };

                if is_ordered {
                    let n = self.ol_counters.last().copied().unwrap_or(1);
                    let prefix = format_number(n);
                    self.put_str_raw(&prefix);
                    self.put_str_raw(". ");
                    if let Some(counter) = self.ol_counters.last_mut() {
                        *counter += 1;
                    }
                } else {
                    self.put_str_raw("\u{2022} "); // •
                }
                self.render_children(idx);
            }

            "table" => {
                self.render_table(idx);
            }

            "tr" | "td" | "th" | "thead" | "tbody" | "tfoot" | "caption" | "colgroup" | "col" => {
                // Table sub-elements handled by render_table.
                // If encountered outside a table, just render children.
                self.render_children(idx);
            }

            "form" => {
                self.ensure_newline();
                self.render_children(idx);
                self.ensure_newline();
            }

            // -- Inline elements --------------------------------------------
            "a" => {
                let url = attr_get(attrs,"href").cloned().unwrap_or_default();
                self.style.fg = Color::Blue;
                self.style.underline = true;
                let col_start = self.col;
                let row_start = self.row;
                self.put_char('[');
                self.render_children(idx);
                self.put_char(']');
                let col_end = self.col;
                self.links.push(LinkRegion {
                    row: row_start,
                    col_start,
                    col_end,
                    url,
                });
                self.style = saved_style;
            }

            "strong" | "b" => {
                self.style.bold = true;
                self.render_children(idx);
                self.style = saved_style;
            }

            "em" | "i" => {
                self.style.fg = Color::Cyan;
                self.render_children(idx);
                self.style = saved_style;
            }

            "code" => {
                self.style.fg = Color::Green;
                self.render_children(idx);
                self.style = saved_style;
            }

            "span" => {
                self.render_children(idx);
            }

            // -- Form elements ----------------------------------------------
            "input" => {
                self.render_input(attrs);
            }

            "textarea" => {
                self.render_textarea(idx, attrs);
            }

            "select" => {
                self.render_select(idx, attrs);
            }

            "button" => {
                self.render_button(idx);
            }

            "label" => {
                self.render_children(idx);
            }

            "option" | "optgroup" => {
                // Handled by render_select.
            }

            // -- Fallthrough: render children for unknown elements ----------
            _ => {
                self.render_children(idx);
            }
        }
    }

    fn render_children(&mut self, idx: usize) {
        let children: Vec<usize> = self.doc.nodes[idx].children.clone();
        for child in children {
            self.render_node(child);
        }
    }

    // -- Heading ------------------------------------------------------------

    fn render_heading(&mut self, idx: usize, level: usize) {
        self.blank_line();
        let saved = self.style.clone();
        self.style.bold = true;
        // Print markdown-style prefix: "# ", "## ", etc.
        for _ in 0..level {
            self.put_char('#');
        }
        self.put_char(' ');
        self.render_children(idx);
        self.style = saved;
        self.blank_line();
    }

    // -- Blockquote ---------------------------------------------------------

    fn render_blockquote(&mut self, idx: usize) {
        self.ensure_newline();
        // Render children into a temporary sub-renderer, then prefix each line.
        let saved_style = self.style.clone();
        self.style.fg = Color::Gray;

        // Simple approach: render children normally, but add "│ " prefix
        // at the start of each new line.
        let start_row = self.row;
        let start_col = self.col;
        // Reserve space for prefix
        self.col = 2;
        self.render_children(idx);
        self.ensure_newline();
        let end_row = self.row;

        // Go back and add the "│ " prefix to each line.
        for r in start_row..end_row {
            if r < self.lines.len() {
                self.lines[r][0] = Cell {
                    ch: '\u{2502}', // │
                    fg: Color::Gray,
                    bg: Color::Default,
                    bold: false,
                    underline: false,
                };
                if self.width > 1 {
                    self.lines[r][1] = Cell {
                        ch: ' ',
                        fg: Color::Default,
                        bg: Color::Default,
                        bold: false,
                        underline: false,
                    };
                }
            }
        }
        let _ = (start_col,); // suppress unused warning
        self.style = saved_style;
    }

    // -- Table --------------------------------------------------------------

    fn render_table(&mut self, table_idx: usize) {
        self.ensure_newline();

        // Collect rows and cells.
        let rows = self.collect_table_rows(table_idx);
        if rows.is_empty() {
            return;
        }

        // Determine column widths by rendering each cell to text.
        let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if num_cols == 0 {
            return;
        }

        let mut col_widths = vec![0usize; num_cols];
        let mut cell_texts: Vec<Vec<String>> = Vec::new();

        for row in &rows {
            let mut row_texts = Vec::new();
            for (ci, &cell_idx) in row.iter().enumerate() {
                let text = self.extract_node_text(cell_idx);
                let text = collapse_whitespace(&text);
                if ci < num_cols && text.len() > col_widths[ci] {
                    col_widths[ci] = text.len();
                }
                row_texts.push(text);
            }
            cell_texts.push(row_texts);
        }

        // Clamp column widths so total fits in width.
        let separators = num_cols + 1; // │ between + edges
        let available = self.width.saturating_sub(separators);
        let total: usize = col_widths.iter().sum();
        if total > available && total > 0 {
            for w in &mut col_widths {
                *w = (*w * available) / total;
                if *w == 0 {
                    *w = 1;
                }
            }
        }

        // Draw top border.
        self.draw_table_separator(&col_widths, '\u{2500}');

        // Draw each row.
        for (ri, row_texts) in cell_texts.iter().enumerate() {
            // Draw cells.
            self.put_char('\u{2502}'); // │
            for (ci, width) in col_widths.iter().enumerate() {
                let text = row_texts.get(ci).map(|s| s.as_str()).unwrap_or("");
                let truncated = if text.len() > *width {
                    &text[..*width]
                } else {
                    text
                };
                self.put_str_raw(truncated);
                // Pad remaining.
                for _ in truncated.len()..*width {
                    self.put_char(' ');
                }
                self.put_char('\u{2502}'); // │
            }
            self.newline();

            // Draw row separator (except after last row).
            if ri + 1 < cell_texts.len() {
                self.draw_table_separator(&col_widths, '\u{2500}');
            }
        }

        // Draw bottom border.
        self.draw_table_separator(&col_widths, '\u{2500}');
    }

    fn draw_table_separator(&mut self, col_widths: &[usize], ch: char) {
        self.put_char(ch);
        for (ci, &width) in col_widths.iter().enumerate() {
            for _ in 0..width {
                self.put_char(ch);
            }
            if ci + 1 < col_widths.len() {
                self.put_char(ch);
            }
        }
        self.put_char(ch);
        self.newline();
    }

    fn collect_table_rows(&self, table_idx: usize) -> Vec<Vec<usize>> {
        let mut rows = Vec::new();
        self.collect_rows_recursive(table_idx, &mut rows);
        rows
    }

    fn collect_rows_recursive(&self, idx: usize, rows: &mut Vec<Vec<usize>>) {
        let node = &self.doc.nodes[idx];
        if let NodeData::Element { tag, .. } = &node.data {
            if tag == "tr" {
                let mut cells = Vec::new();
                for &child in &node.children {
                    if let NodeData::Element { tag, .. } = &self.doc.nodes[child].data {
                        if tag == "td" || tag == "th" {
                            cells.push(child);
                        }
                    }
                }
                rows.push(cells);
                return;
            }
        }
        for &child in &node.children {
            self.collect_rows_recursive(child, rows);
        }
    }

    fn extract_node_text(&self, idx: usize) -> String {
        let mut out = String::new();
        self.extract_text_recursive(idx, &mut out);
        out
    }

    fn extract_text_recursive(&self, idx: usize, out: &mut String) {
        let node = &self.doc.nodes[idx];
        match &node.data {
            NodeData::Text(t) => out.push_str(t),
            NodeData::Element { tag, .. } => {
                match tag.as_str() {
                    "script" | "style" | "head" => return,
                    _ => {}
                }
                for &child in &node.children {
                    self.extract_text_recursive(child, out);
                }
            }
            // No other variants — exhaustive match
            NodeData::Comment(_) => {}
        }
    }

    // -- Form elements ------------------------------------------------------

    fn render_input(&mut self, attrs: &[(String, String)]) {
        let input_type = attr_get(attrs,"type").map(|s| s.as_str()).unwrap_or("text");
        let name = attr_get(attrs,"name").cloned().unwrap_or_default();
        let value = attr_get(attrs,"value").cloned().unwrap_or_default();

        match input_type {
            "hidden" => {
                // Track but don't render.
                self.inputs.push(InputRegion {
                    row: self.row,
                    col_start: self.col,
                    col_end: self.col,
                    name,
                    input_type: String::from("hidden"),
                    value,
                });
            }
            "submit" => {
                let label = if value.is_empty() {
                    String::from("Submit")
                } else {
                    value
                };
                let saved = self.style.clone();
                self.style.fg = Color::Black;
                self.style.bg = Color::White;
                let col_start = self.col;
                self.put_str_raw("[ ");
                self.put_str_raw(&label);
                self.put_str_raw(" ]");
                let col_end = self.col;
                self.inputs.push(InputRegion {
                    row: self.row,
                    col_start,
                    col_end,
                    name,
                    input_type: String::from("submit"),
                    value: label,
                });
                self.style = saved;
            }
            "checkbox" => {
                let checked = attr_has(attrs,"checked");
                if checked {
                    self.put_str_raw("[x] ");
                } else {
                    self.put_str_raw("[ ] ");
                }
                self.inputs.push(InputRegion {
                    row: self.row,
                    col_start: self.col.saturating_sub(4),
                    col_end: self.col,
                    name,
                    input_type: String::from("checkbox"),
                    value: if checked {
                        String::from("on")
                    } else {
                        String::new()
                    },
                });
            }
            "radio" => {
                let checked = attr_has(attrs,"checked");
                if checked {
                    self.put_str_raw("(\u{2022}) "); // (•)
                } else {
                    self.put_str_raw("( ) ");
                }
                self.inputs.push(InputRegion {
                    row: self.row,
                    col_start: self.col.saturating_sub(4),
                    col_end: self.col,
                    name,
                    input_type: String::from("radio"),
                    value: attr_get(attrs,"value").cloned().unwrap_or_default(),
                });
            }
            // text, email, password, etc.
            _ => {
                let field_width = 20usize.min(self.width.saturating_sub(self.col + 2));
                let col_start = self.col;
                self.put_char('[');
                let display = if !value.is_empty() {
                    if input_type == "password" {
                        let mut masked = String::new();
                        for _ in 0..value.len().min(field_width) {
                            masked.push('*');
                        }
                        masked
                    } else {
                        let truncated: String = value.chars().take(field_width).collect();
                        truncated
                    }
                } else {
                    String::new()
                };
                self.put_str_raw(&display);
                // Fill remainder with underscores.
                let used = display.len();
                for _ in used..field_width {
                    self.put_char('_');
                }
                self.put_char(']');
                let col_end = self.col;

                self.inputs.push(InputRegion {
                    row: self.row,
                    col_start,
                    col_end,
                    name,
                    input_type: String::from(input_type),
                    value,
                });
            }
        }
    }

    fn render_textarea(&mut self, idx: usize, attrs: &[(String, String)]) {
        let name = attr_get(attrs,"name").cloned().unwrap_or_default();
        let rows_attr: usize = attrs
            .get("rows")
            .and_then(|s| parse_usize(s))
            .unwrap_or(3);
        let field_width = 30usize.min(self.width.saturating_sub(2));

        self.ensure_newline();
        let col_start = self.col;
        let row_start = self.row;

        for _r in 0..rows_attr {
            self.put_char('[');
            for _ in 0..field_width {
                self.put_char('_');
            }
            self.put_char(']');
            self.newline();
        }

        // Extract initial value from text children.
        let value = self.extract_node_text(idx);

        self.inputs.push(InputRegion {
            row: row_start,
            col_start,
            col_end: col_start + field_width + 2,
            name,
            input_type: String::from("textarea"),
            value: collapse_whitespace(&value),
        });
    }

    fn render_select(&mut self, idx: usize, attrs: &[(String, String)]) {
        let name = attr_get(attrs,"name").cloned().unwrap_or_default();

        // Find the first <option> text.
        let first_option = self.find_first_option_text(idx);
        let display = if first_option.is_empty() {
            String::from("---")
        } else {
            first_option
        };

        let col_start = self.col;
        self.put_str_raw("[\u{25BC} "); // [▼
        self.put_str_raw(&display);
        self.put_str_raw(" ]");
        let col_end = self.col;

        self.inputs.push(InputRegion {
            row: self.row,
            col_start,
            col_end,
            name,
            input_type: String::from("select"),
            value: display,
        });
    }

    fn find_first_option_text(&self, idx: usize) -> String {
        let node = &self.doc.nodes[idx];
        for &child in &node.children {
            if let NodeData::Element { tag, .. } = &self.doc.nodes[child].data {
                if tag == "option" {
                    return collapse_whitespace(&self.extract_node_text(child));
                }
                if tag == "optgroup" {
                    let text = self.find_first_option_text(child);
                    if !text.is_empty() {
                        return text;
                    }
                }
            }
        }
        String::new()
    }

    fn render_button(&mut self, idx: usize) {
        let saved = self.style.clone();
        self.style.fg = Color::Black;
        self.style.bg = Color::White;
        let col_start = self.col;
        self.put_str_raw("[ ");
        self.render_children(idx);
        self.put_str_raw(" ]");
        let col_end = self.col;

        // Record as a submit-like input.
        let text = collapse_whitespace(&self.extract_node_text(idx));
        self.inputs.push(InputRegion {
            row: self.row,
            col_start,
            col_end,
            name: String::new(),
            input_type: String::from("button"),
            value: text,
        });
        self.style = saved;
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Render a parsed HTML document to a text-mode page.
///
/// `width` is the number of character columns. `max_height` caps the output
/// grid height (content beyond this is still measured in `content_height`).
pub fn render(doc: &Document, width: usize, max_height: usize) -> TextPage {
    let width = width.max(1);
    let mut r = Renderer::new(doc, width);

    // Walk the DOM starting from the root.
    r.render_node(0);

    let content_height = r.lines.len();

    // Trim or extend to fit max_height.
    let visible_height = if max_height == 0 {
        content_height
    } else {
        max_height
    };

    let mut cells = r.lines;
    // Ensure we have at least visible_height rows.
    while cells.len() < visible_height {
        cells.push(vec![Cell::default(); width]);
    }

    TextPage {
        width,
        height: visible_height,
        cells,
        links: r.links,
        inputs: r.inputs,
        title: r.title,
        content_height,
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Collapse runs of whitespace into a single space, trim leading/trailing.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::new();
    let mut last_was_space = true; // suppress leading space
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    // Trim trailing space.
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Split text into words and whitespace tokens for wrapping.
fn split_words(s: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if s.as_bytes()[i].is_ascii_whitespace() {
            let start = i;
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            words.push(&s[start..i]);
        } else {
            let start = i;
            while i < len && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            words.push(&s[start..i]);
        }
    }
    words
}

/// Format a number as a string (no_std-friendly).
fn format_number(n: usize) -> String {
    if n == 0 {
        return String::from("0");
    }
    let mut digits = Vec::new();
    let mut val = n;
    while val > 0 {
        digits.push((b'0' + (val % 10) as u8) as char);
        val /= 10;
    }
    digits.reverse();
    digits.into_iter().collect()
}

/// Parse a usize from a string (no_std-friendly).
fn parse_usize(s: &str) -> Option<usize> {
    let mut val = 0usize;
    let mut any = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            val = val.checked_mul(10)?.checked_add((ch as u8 - b'0') as usize)?;
            any = true;
        } else {
            break;
        }
    }
    if any { Some(val) } else { None }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    #[allow(unused_imports)]
    use alloc::string::ToString;

    fn render_html(html: &str, width: usize) -> TextPage {
        let doc = wraith_dom::parse(html);
        render(&doc, width, 0)
    }

    fn page_text(page: &TextPage) -> String {
        let mut out = String::new();
        for (i, row) in page.cells.iter().enumerate() {
            if i >= page.content_height {
                break;
            }
            let line: String = row.iter().map(|c| c.ch).collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        // Trim trailing blank lines.
        while out.ends_with("\n\n") {
            out.pop();
        }
        out
    }

    #[test]
    fn test_plain_text() {
        let page = render_html("<p>Hello world</p>", 40);
        let text = page_text(&page);
        assert!(text.contains("Hello world"), "got: {text}");
    }

    #[test]
    fn test_heading() {
        let page = render_html("<h1>Title</h1>", 40);
        let text = page_text(&page);
        assert!(text.contains("# Title"), "got: {text}");
    }

    #[test]
    fn test_heading_levels() {
        let page = render_html("<h3>Sub</h3>", 40);
        let text = page_text(&page);
        assert!(text.contains("### Sub"), "got: {text}");
    }

    #[test]
    fn test_link_rendering() {
        let page = render_html(r#"<a href="https://example.com">Click</a>"#, 40);
        let text = page_text(&page);
        assert!(text.contains("[Click]"), "got: {text}");
        assert_eq!(page.links.len(), 1);
        assert_eq!(page.links[0].url, "https://example.com");
    }

    #[test]
    fn test_input_field() {
        let page = render_html(
            r#"<input type="text" name="email" />"#,
            40,
        );
        assert_eq!(page.inputs.len(), 1);
        assert_eq!(page.inputs[0].name, "email");
        assert_eq!(page.inputs[0].input_type, "text");
        let text = page_text(&page);
        assert!(text.contains("[____"), "got: {text}");
    }

    #[test]
    fn test_submit_button() {
        let page = render_html(
            r#"<input type="submit" value="Log in" />"#,
            40,
        );
        let text = page_text(&page);
        assert!(text.contains("[ Log in ]"), "got: {text}");
    }

    #[test]
    fn test_hidden_elements_skipped() {
        let page = render_html(
            "<div><script>alert('x')</script><style>body{}</style>Visible</div>",
            40,
        );
        let text = page_text(&page);
        assert!(text.contains("Visible"), "got: {text}");
        assert!(!text.contains("alert"), "script content leaked: {text}");
        assert!(!text.contains("body{}"), "style content leaked: {text}");
    }

    #[test]
    fn test_unordered_list() {
        let page = render_html("<ul><li>One</li><li>Two</li></ul>", 40);
        let text = page_text(&page);
        assert!(text.contains("\u{2022} One"), "got: {text}");
        assert!(text.contains("\u{2022} Two"), "got: {text}");
    }

    #[test]
    fn test_ordered_list() {
        let page = render_html("<ol><li>First</li><li>Second</li></ol>", 40);
        let text = page_text(&page);
        assert!(text.contains("1. First"), "got: {text}");
        assert!(text.contains("2. Second"), "got: {text}");
    }

    #[test]
    fn test_hr() {
        let page = render_html("<hr>", 10);
        let text = page_text(&page);
        assert!(text.contains("\u{2500}\u{2500}\u{2500}"), "got: {text}");
    }

    #[test]
    fn test_word_wrap() {
        // 10-char wide, text longer than that
        let page = render_html("<p>hello world foo</p>", 10);
        // Should have wrapped to multiple lines
        assert!(
            page.content_height > 2,
            "expected wrapping, got {} rows",
            page.content_height
        );
    }

    #[test]
    fn test_login_form() {
        let html = r#"
        <html><body>
        <h1>Sign In</h1>
        <form action="/login" method="post">
            <label>Email</label>
            <input type="email" name="email" />
            <label>Password</label>
            <input type="password" name="password" />
            <input type="submit" value="Sign In" />
        </form>
        </body></html>
        "#;
        let page = render_html(html, 60);
        let text = page_text(&page);

        assert!(text.contains("# Sign In"), "missing heading: {text}");
        assert!(text.contains("Email"), "missing label: {text}");
        assert!(text.contains("[ Sign In ]"), "missing submit: {text}");

        // Should have 3 inputs: email, password, submit
        assert_eq!(
            page.inputs.len(),
            3,
            "expected 3 inputs, got {}",
            page.inputs.len()
        );
    }
}
