# Terminal Subsystem

The terminal subsystem provides framebuffer-based text rendering with ANSI escape
sequence support and a split-pane layout engine. It lives in `crates/terminal/`.

---

## Table of Contents

- [Overview](#overview)
- [Font Rendering Pipeline](#font-rendering-pipeline)
- [VTE Parser Integration](#vte-parser-integration)
- [Supported Escape Sequences](#supported-escape-sequences)
- [Split-Pane Layout Tree](#split-pane-layout-tree)
- [Color System](#color-system)
- [Pane Lifecycle](#pane-lifecycle)
- [Dashboard Commands](#dashboard-commands)

---

## Overview

The terminal crate is `#![no_std]` with `extern crate alloc`. It has no knowledge of
the framebuffer hardware -- instead, it renders through a `DrawTarget` trait that the
kernel implements:

```rust
// crates/terminal/src/lib.rs
pub trait DrawTarget {
    fn put_pixel(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8);
    fn width(&self) -> usize;
    fn height(&self) -> usize;
}
```

This abstraction means the terminal crate is testable on the host (with a mock
`DrawTarget`) and doesn't depend on any kernel internals.

### Module Structure

```
crates/terminal/src/
  lib.rs      DrawTarget trait, Viewport, LayoutNode, SplitDirection,
              DashboardCommand enum
  render.rs   Font rendering (noto-sans-mono-bitmap), Color type with
              16-color palette, fill_rect helper
  pane.rs     Terminal pane: cell grid, VTE parser, cursor, SGR color
              state, scroll, resize, render
  layout.rs   Binary split tree: Layout struct with focus management,
              split/close operations, viewport recomputation, separator
              rendering
```

### Data Flow Overview

```
Raw bytes (UTF-8 text + ANSI escape sequences)
    |
    v
Pane::write_bytes(bytes)
    |
    v
vte::Parser (state machine)
    |
    +-- printable char --> put_char(c) --> update cell grid
    +-- control byte   --> execute()   --> LF, CR, BS, Tab, BEL
    +-- CSI sequence   --> csi_dispatch() --> cursor move, erase, SGR color
    |
    v
Cell grid (2D array of {char, fg, bg})
    |
    v
Pane::render(target)
    |
    v
For each cell: render_char() using noto-sans-mono-bitmap
    |
    v
DrawTarget::put_pixel() --> framebuffer
```

---

## Font Rendering Pipeline

**Source:** `crates/terminal/src/render.rs`

### Glyph Rasterization

Characters are rendered using pre-rasterized bitmap glyphs from the
`noto-sans-mono-bitmap` crate. This crate embeds the Noto Sans Mono font as
compile-time bitmap data -- no font file loading, no TrueType parsing, no
runtime rasterization.

```
Character + weight + size
    |
    v
noto_sans_mono_bitmap::get_raster('A', FontWeight::Regular, RasterHeight::Size16)
    |
    v
Returns Option<RasterizedChar>
    |
    +-- Some(raster): 2D array of intensity values (0-255 per pixel)
    +-- None: character not in font -> fall back to '?'
```

### Font Constants

| Constant | Value | Source |
|----------|-------|--------|
| `FONT_HEIGHT` | 16 pixels | Hardcoded to `RasterHeight::Size16` |
| `FONT_WIDTH` | compile-time computed | `get_raster_width(FontWeight::Regular, RasterHeight::Size16)` |

All glyphs have identical width (monospace property). The width is a `const fn` call
evaluated at compile time.

### Rendering Algorithm

The `render_char()` function renders a single character cell at a pixel position:

```
For each pixel (row, col) in the glyph raster:
    intensity = raster[row][col]      // 0-255

    if intensity > 128:
        draw foreground color
    else:
        draw background color

    target.put_pixel(x + col, y + row, r, g, b)
```

**Binary threshold**: A full alpha blend (`out = bg + (fg - bg) * intensity / 255`)
would produce smoother anti-aliased text but costs a multiply and divide per pixel.
The binary threshold at intensity 128 keeps the rendering fast and branchless-friendly.
Sub-pixel rendering or full alpha blending may be added in Phase 4.

### fill_rect

The `fill_rect()` function fills a rectangular pixel region with a solid color.
Used for:
- Drawing pane separators (2-pixel-wide gray lines)
- Clearing rectangular regions
- Rendering background areas

---

## VTE Parser Integration

**Source:** `crates/terminal/src/pane.rs`

Each `Pane` contains a `vte::Parser` instance from the `vte` crate (v0.15). The
parser is a state machine implementing the DEC VT100/VT220 escape sequence grammar.
It classifies incoming bytes as printable characters, control codes, or multi-byte
escape sequences and dispatches them through the `vte::Perform` trait.

### Parser Ownership Trick

The VTE parser calls `Perform` trait methods with `&mut self`, but the performer
needs `&mut Pane` to mutate the cell grid. Since the parser is owned by the pane,
this creates a double-mutable-borrow problem. The solution uses `mem::replace`:

```rust
pub fn write_bytes(&mut self, bytes: &[u8]) {
    // Temporarily take the parser out of self (replace with a fresh one)
    let mut parser = core::mem::replace(&mut self.vte_parser, vte::Parser::new());
    // Now we can borrow self mutably for the performer
    let mut performer = PanePerformer { pane: self };
    // Advance the parser with the performer
    parser.advance(&mut performer, bytes);
    // Put the parser back
    performer.pane.vte_parser = parser;
}
```

The temporary `vte::Parser::new()` placeholder is never used -- it's immediately
overwritten when the real parser is put back.

### PanePerformer

The `PanePerformer` struct wraps a `&mut Pane` and implements `vte::Perform`:

```
vte::Perform trait callbacks:

  print(c: char)
    -> pane.put_char(c)
    Character goes into the cell grid at cursor position.

  execute(byte: u8)
    -> match byte:
       0x0A (LF), 0x0B (VT), 0x0C (FF) -> newline()
       0x0D (CR) -> cursor_col = 0
       0x08 (BS) -> cursor_col -= 1
       0x09 (HT) -> advance to next 8-column tab stop
       0x07 (BEL) -> ignored

  csi_dispatch(params, intermediates, ignore, action)
    -> match action:
       'm' -> SGR (colors, attributes)
       'H'|'f' -> cursor position
       'A'..'D' -> cursor movement
       'J' -> erase in display
       'K' -> erase in line
       'S' -> scroll up
       'L' -> insert lines
       'M' -> delete lines
       'G' -> cursor horizontal absolute
       'd' -> vertical position absolute

  hook/put/unhook -> no-op (DCS sequences not implemented)
  osc_dispatch -> no-op (OSC sequences not implemented)
  esc_dispatch -> no-op (standalone ESC sequences not implemented)
```

---

## Supported Escape Sequences

### Control Codes (execute)

| Byte | Name | Action |
|------|------|--------|
| `0x0A` | LF (Line Feed) | Move cursor to start of next line; scroll if at bottom |
| `0x0B` | VT (Vertical Tab) | Same as LF |
| `0x0C` | FF (Form Feed) | Same as LF |
| `0x0D` | CR (Carriage Return) | Move cursor to column 0, same row |
| `0x08` | BS (Backspace) | Move cursor left by 1 (no erase) |
| `0x09` | HT (Horizontal Tab) | Advance to next 8-column tab stop: `(col + 8) & !7` |
| `0x07` | BEL (Bell) | Ignored (no audio hardware) |

### CSI Sequences (csi_dispatch)

#### Cursor Positioning

| Sequence | Name | Action |
|----------|------|--------|
| `CSI row;col H` | CUP (Cursor Position) | Move cursor to (row, col), 1-indexed, defaults to (1,1) |
| `CSI row;col f` | HVP (same as CUP) | Move cursor to (row, col), 1-indexed |
| `CSI n A` | CUU (Cursor Up) | Move cursor up n rows (default 1), clamp at row 0 |
| `CSI n B` | CUD (Cursor Down) | Move cursor down n rows (default 1), clamp at last row |
| `CSI n C` | CUF (Cursor Forward) | Move cursor right n columns (default 1) |
| `CSI n D` | CUB (Cursor Back) | Move cursor left n columns (default 1) |
| `CSI n G` | CHA (Cursor Horizontal Absolute) | Move to column n (1-indexed) |
| `CSI n d` | VPA (Vertical Position Absolute) | Move to row n (1-indexed) |

#### Erase Sequences

| Sequence | Name | Action |
|----------|------|--------|
| `CSI 0 J` | ED (Erase in Display) | Erase from cursor to end of display |
| `CSI 1 J` | ED | Erase from start of display to cursor (inclusive) |
| `CSI 2 J` | ED | Erase entire display and reset cursor to (0,0) |
| `CSI 3 J` | ED | Same as `2 J` (no scrollback buffer to clear) |
| `CSI 0 K` | EL (Erase in Line) | Erase from cursor to end of line |
| `CSI 1 K` | EL | Erase from start of line to cursor (inclusive) |
| `CSI 2 K` | EL | Erase entire line |

#### Scroll and Line Manipulation

| Sequence | Name | Action |
|----------|------|--------|
| `CSI n S` | SU (Scroll Up) | Scroll content up by n lines (default 1) |
| `CSI n L` | IL (Insert Lines) | Insert n blank lines at cursor row, pushing below down |
| `CSI n M` | DL (Delete Lines) | Delete n lines at cursor row, pulling below up |

#### SGR (Select Graphic Rendition) -- `CSI ... m`

| Sequence | Action |
|----------|--------|
| `CSI 0 m` | Reset all attributes to default fg/bg |
| `CSI 1 m` | Bold (recognized but not yet visually distinct) |
| `CSI 30-37 m` | Set foreground to standard color 0-7 |
| `CSI 39 m` | Reset foreground to default |
| `CSI 40-47 m` | Set background to standard color 0-7 |
| `CSI 49 m` | Reset background to default |
| `CSI 90-97 m` | Set foreground to bright color 0-7 |
| `CSI 100-107 m` | Set background to bright color 0-7 |
| `CSI 38;5;n m` | Set foreground to 256-color index n (basic 16 mapped, 16-255 ignored) |
| `CSI 48;5;n m` | Set background to 256-color index n (basic 16 mapped, 16-255 ignored) |
| `CSI 38;2;r;g;b m` | Set foreground to truecolor RGB (fully supported) |
| `CSI 48;2;r;g;b m` | Set background to truecolor RGB (fully supported) |

---

## Split-Pane Layout Tree

**Source:** `crates/terminal/src/layout.rs`

### Binary Tree Structure

The layout is a binary tree where:
- **Leaf nodes** contain a pane ID and a pixel viewport
- **Split nodes** contain a direction (Vertical or Horizontal), a ratio (0.0-1.0),
  and two children (first and second)

```
Example: Three panes after vertical split, then horizontal split of right half

           Split(Vertical, 0.5)
          /                     \
    Leaf(pane 0)         Split(Horizontal, 0.5)
    viewport:            /                      \
    {0,0,639,480}   Leaf(pane 1)           Leaf(pane 2)
                    {641,0,639,238}        {641,240,639,240}

On screen:
+------------------+--+------------------+
|                  |  |                  |
|     Pane 0       |  |     Pane 1       |
|                  |  |                  |
|                  |  +--+--+--+--+--+--+
|                  |  |                  |
|                  |  |     Pane 2       |
|                  |  |                  |
+------------------+--+------------------+
                   ^^
                   separator (2px, BRIGHT_BLACK)
```

### Viewport Computation

When a split occurs, viewports are recomputed top-down from the root via
`Layout::recompute_viewports()`. The computation accounts for the separator:

```
Vertical split of viewport {x, y, w, h} at ratio r:
  separator width = SEPARATOR_PX (2 pixels)
  first_w  = floor(w * r) - sep/2
  second_x = x + first_w + sep
  second_w = w - first_w - sep

  first:  {x, y, first_w, h}
  second: {second_x, y, second_w, h}

Horizontal split of viewport {x, y, w, h} at ratio r:
  first_h  = floor(h * r) - sep/2
  second_y = y + first_h + sep
  second_h = h - first_h - sep

  first:  {x, y, w, first_h}
  second: {x, second_y, w, second_h}
```

### Split Operation

`layout.split(direction)` performs:

1. Find the focused pane's leaf node in the tree (by pane ID)
2. Replace the leaf with a Split node:
   - `first` = old leaf (same pane ID)
   - `second` = new leaf (new pane ID)
   - `ratio` = 0.5 (equal split)
   - `direction` = Vertical or Horizontal
3. Recompute all viewports from the root (top-down traversal)
4. Resize the existing pane to its new, smaller viewport
5. Create a new `Pane` for the new viewport
6. Move focus to the new pane

### Close Operation

`layout.close_focused()` performs:

1. If only one pane exists, no-op
2. Remove the focused pane from the pane Vec
3. In the tree, find the Split node whose child is the closing pane
4. Promote the sibling to take the parent Split's position
5. Recompute all viewports from the root
6. Resize all remaining panes to their new viewports
7. Adjust focus index (clamp to valid range)

### Focus Navigation

- `focus_next()`: `focused_idx = (focused_idx + 1) % panes.len()`
- `focus_prev()`: wraps around to `panes.len() - 1` from 0

### Rendering

`layout.render_all(target)`:
1. For each pane: call `pane.render(target)` to draw all cells
2. For the focused pane: call `pane.render_cursor(target)` (inverted colors)
3. Walk the tree recursively to draw 2-pixel separators between split children

---

## Color System

**Source:** `crates/terminal/src/render.rs`

### Color Type

```rust
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}
```

### Standard Palette (CGA-derived, SGR 30-37)

| Index | Constant | RGB | SGR Code |
|-------|----------|-----|----------|
| 0 | `BLACK` | (0, 0, 0) | 30 / 40 |
| 1 | `RED` | (204, 0, 0) | 31 / 41 |
| 2 | `GREEN` | (0, 204, 0) | 32 / 42 |
| 3 | `YELLOW` | (204, 204, 0) | 33 / 43 |
| 4 | `BLUE` | (0, 0, 204) | 34 / 44 |
| 5 | `MAGENTA` | (204, 0, 204) | 35 / 45 |
| 6 | `CYAN` | (0, 204, 204) | 36 / 46 |
| 7 | `WHITE` | (204, 204, 204) | 37 / 47 |

### Bright Palette (SGR 90-97)

| Index | Constant | RGB | SGR Code |
|-------|----------|-----|----------|
| 8 | `BRIGHT_BLACK` | (128, 128, 128) | 90 / 100 |
| 9 | `BRIGHT_RED` | (255, 85, 85) | 91 / 101 |
| 10 | `BRIGHT_GREEN` | (85, 255, 85) | 92 / 102 |
| 11 | `BRIGHT_YELLOW` | (255, 255, 85) | 93 / 103 |
| 12 | `BRIGHT_BLUE` | (85, 85, 255) | 94 / 104 |
| 13 | `BRIGHT_MAGENTA` | (255, 85, 255) | 95 / 105 |
| 14 | `BRIGHT_CYAN` | (85, 255, 255) | 96 / 106 |
| 15 | `BRIGHT_WHITE` | (255, 255, 255) | 97 / 107 |

### Default Colors

| Alias | Value | Usage |
|-------|-------|-------|
| `DEFAULT_FG` | White (204, 204, 204) | Default text foreground |
| `DEFAULT_BG` | Near-black (16, 16, 16) | Default terminal background |

The default background is not pure black `(0,0,0)` to visually distinguish the
terminal area from truly-black unused framebuffer regions.

### Extended Color Support

- **256-color mode** (`38;5;n` / `48;5;n`): Indices 0-7 map to standard palette,
  8-15 map to bright palette. Indices 16-255 (the 6x6x6 color cube and grayscale
  ramp) are currently ignored -- the color is not changed.
- **Truecolor** (`38;2;r;g;b` / `48;2;r;g;b`): Fully supported. RGB values from
  the escape sequence are used directly as a `Color`.

---

## Pane Lifecycle

### Cell Grid

Each pane maintains a 2D grid of `Cell` values:

```rust
pub struct Cell {
    pub ch: char,       // character to display (' ' for empty cells)
    pub fg: Color,      // foreground color for this cell
    pub bg: Color,      // background color for this cell
}
```

Grid dimensions are derived from the viewport size and font dimensions:
```
cols = viewport.width / FONT_WIDTH    (minimum 1)
rows = viewport.height / FONT_HEIGHT  (minimum 1)
```

The grid is stored as `Vec<Vec<Cell>>` (rows of columns).

### Cursor

The cursor position (`cursor_row`, `cursor_col`) is 0-indexed. The cursor is
rendered by `render_cursor()` as an inverted cell -- foreground and background
colors are swapped at the cursor position.

When `cursor_col` reaches or exceeds `cols` during `put_char()`, a newline is
triggered automatically (line wrap).

### Scrolling

When the cursor is on the last row and a newline occurs:
1. `cells.remove(0)` -- discard the top row
2. `cells.push(blank_row)` -- append a blank row at bottom
3. Increment `scroll_offset` (reserved for future scrollback buffer)

The cursor stays on the last row after scrolling.

### Dirty Tracking

The `dirty` flag is set whenever the cell grid changes (character written, cursor
moved, cells erased, scroll, resize). After rendering, the caller calls
`clear_dirty()`. This enables future optimizations where only dirty panes are
re-rendered each frame.

### Resize

When a pane's viewport changes (due to a split or close), `resize()`:
1. Computes new `cols` and `rows` from the new viewport
2. Creates a fresh cell grid (default cells)
3. Copies over as much old content as fits (min of old/new dimensions)
4. Clamps cursor to new bounds
5. Marks pane as dirty

---

## Dashboard Commands

**Source:** `crates/terminal/src/lib.rs`

The `DashboardCommand` enum defines high-level actions for pane management,
intended to be dispatched by keyboard shortcuts (Phase 4, tmux-style `Ctrl+B`
prefix):

```rust
pub enum DashboardCommand {
    SplitVertical,    // Split focused pane left|right
    SplitHorizontal,  // Split focused pane top/bottom
    FocusNext,        // Move focus to next pane (wrapping)
    FocusPrev,        // Move focus to previous pane (wrapping)
    ClosePane,        // Close the focused pane
    NewAgent,         // Create a new agent session in a new pane
    ToggleStatusBar,  // Show/hide the bottom status bar
}
```

These are not yet wired up to keyboard input -- Phase 1 only echoes keystrokes
to serial. Phase 4 will add the `Ctrl+B` prefix key handler that translates
key combos into `DashboardCommand` variants.
