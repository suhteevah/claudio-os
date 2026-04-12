# claudio-mux Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract a shared `terminal-core` crate from ClaudioOS's terminal + dashboard code, then build `claudio-mux` — a Windows-native tmux-like terminal multiplexer that shares core logic with ClaudioOS.

**Architecture:** Four new crates (`terminal-core`, `terminal-fb`, `terminal-ansi`, `claudio-mux`) replace the monolithic `crates/terminal/` and decouple `kernel/src/dashboard.rs`'s prefix-key state machine. `terminal-core` is `no_std + alloc` and knows nothing about pixels or ConPTY. ClaudioOS renders through `terminal-fb`; the Windows binary renders through `terminal-ansi`.

**Tech Stack:** Rust (`no_std` for core/fb, `std` for ansi/mux), VTE 0.15, bitflags 2, crossterm 0.28, portable-pty 0.9, tokio 1, clap 4, serde + toml, tracing, directories 5.

**Spec:** `docs/superpowers/specs/2026-04-11-claudio-mux-design.md`

---

## File Map

### New crates

| File | Responsibility |
|------|----------------|
| `crates/terminal-core/Cargo.toml` | `no_std + alloc` manifest: vte, log, spin, bitflags |
| `crates/terminal-core/src/lib.rs` | Re-exports, PaneId type alias, crate docs |
| `crates/terminal-core/src/key.rs` | `KeyEvent`, `KeyCode`, `Modifiers` — source-agnostic key types |
| `crates/terminal-core/src/command.rs` | `DashboardCommand` enum (12 variants) |
| `crates/terminal-core/src/input.rs` | `InputRouter` state machine: Normal / AwaitingCommand, `RouterOutcome` |
| `crates/terminal-core/src/viewport.rs` | `CellViewport { col, row, cols, rows }` |
| `crates/terminal-core/src/color.rs` | `Color` struct + palette constants (moved from render.rs) |
| `crates/terminal-core/src/cell.rs` | `Cell { ch, fg, bg }` (moved from pane.rs) |
| `crates/terminal-core/src/pane.rs` | `Pane` — cell grid + VTE + dirty tracking, NO pixel math, NO DrawTarget |
| `crates/terminal-core/src/layout.rs` | `LayoutNode`, `Layout`, `SplitDirection` — cell-based, no pixel separator |
| `crates/terminal-fb/Cargo.toml` | `no_std + alloc` manifest: depends on terminal-core |
| `crates/terminal-fb/src/lib.rs` | `DrawTarget` trait, pixel `Viewport`, `pixels_to_cells()`, `FbLayout` adapter |
| `crates/terminal-fb/src/render.rs` | `render_char`, `fill_rect` (moved from terminal/src/render.rs) |
| `crates/terminal-fb/src/pane_renderer.rs` | `render_pane`, `render_pane_dirty`, `render_cursor_delta` — pixel rendering for core Panes |
| `crates/terminal-fb/src/terminus.rs` | Bitmap font glyphs (moved unchanged) |
| `crates/terminal-fb/src/unicode_font.rs` | Unicode glyph map (moved unchanged) |
| `crates/terminal-ansi/Cargo.toml` | `std` manifest: depends on terminal-core, crossterm |
| `crates/terminal-ansi/src/lib.rs` | `AnsiRenderer`, `Scene`, `StatusContext` |
| `crates/terminal-ansi/src/diff.rs` | Cell-by-cell diff, minimal ANSI byte emission |
| `tools/claudio-mux/Cargo.toml` | Binary manifest: terminal-core, terminal-ansi, tokio, portable-pty, etc. |
| `tools/claudio-mux/src/main.rs` | Arg parsing, tracing init, config load, runtime bootstrap |
| `tools/claudio-mux/src/app.rs` | tokio select! event loop |
| `tools/claudio-mux/src/session.rs` | `Session`: Layout + pane states + InputRouter |
| `tools/claudio-mux/src/pane_state.rs` | `PaneKind`, per-pane runtime state |
| `tools/claudio-mux/src/conpty.rs` | `PtyHandle`, `spawn_shell`, `spawn_agent` |
| `tools/claudio-mux/src/host.rs` | Raw mode guard, alt screen, key reader |
| `tools/claudio-mux/src/render.rs` | Session → Scene glue, flush |
| `tools/claudio-mux/src/config.rs` | Config struct, TOML parsing |
| `tools/claudio-mux/src/layouts.rs` | Named layout load/save, SerializedLayoutNode |
| `tools/claudio-mux/src/cli.rs` | clap arg definitions |

### Modified files

| File | Change |
|------|--------|
| `Cargo.toml` (workspace root) | Add terminal-core, terminal-fb, terminal-ansi, tools/claudio-mux to members |
| `crates/terminal/src/lib.rs` | Gut: re-export from terminal-core + terminal-fb, deprecate |
| `crates/terminal/src/pane.rs` | Gut: delegate to terminal-core Pane + terminal-fb renderer |
| `crates/terminal/src/layout.rs` | Gut: delegate to terminal-fb FbLayout |
| `crates/terminal/src/render.rs` | Gut: re-export from terminal-fb |
| `kernel/Cargo.toml` | Add terminal-core, terminal-fb deps |
| `kernel/src/dashboard.rs` | Replace PrefixState + handle_prefix_command with InputRouter + apply_command |

---

## Phase 1: terminal-core (Tasks 1–8)

### Task 1: Scaffold terminal-core crate with key types

**Files:**
- Create: `crates/terminal-core/Cargo.toml`
- Create: `crates/terminal-core/src/lib.rs`
- Create: `crates/terminal-core/src/key.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "terminal-core"
version.workspace = true
edition.workspace = true

[dependencies]
bitflags = "2"
log = { workspace = true }
```

- [ ] **Step 2: Create src/lib.rs**

```rust
//! terminal-core — shared grammar for terminal multiplexers.
//!
//! This crate is `#![no_std]` + `alloc`. It knows nothing about pixels,
//! framebuffers, ConPTY, or ANSI escape output. It defines the abstract
//! brain of a terminal multiplexer: key events, commands, input routing,
//! pane grids, and layout trees.

#![no_std]
extern crate alloc;

pub mod key;
pub mod command;
pub mod viewport;
pub mod color;
pub mod cell;
pub mod input;
pub mod pane;
pub mod layout;

/// Stable pane identifier. u64 so it survives serialization across process
/// boundaries (v2 session persistence, v3 named-pipe IPC).
pub type PaneId = u64;

pub use key::{KeyEvent, KeyCode, Modifiers};
pub use command::DashboardCommand;
pub use input::{InputRouter, RouterOutcome};
pub use viewport::CellViewport;
pub use color::Color;
pub use cell::Cell;
pub use pane::Pane;
pub use layout::{Layout, LayoutNode, SplitDirection};
```

- [ ] **Step 3: Create src/key.rs**

```rust
//! Source-agnostic key event types.
//!
//! Both ClaudioOS (pc-keyboard crate) and claudio-mux (crossterm) translate
//! their native key representations into these types before feeding them to
//! the InputRouter.

use bitflags::bitflags;

/// A keyboard event with modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub mods: Modifiers,
}

impl KeyEvent {
    pub const fn new(code: KeyCode, mods: Modifiers) -> Self {
        Self { code, mods }
    }

    /// Shorthand: plain key with no modifiers.
    pub const fn plain(code: KeyCode) -> Self {
        Self { code, mods: Modifiers::empty() }
    }

    /// Shorthand: Ctrl + character.
    pub const fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: Modifiers::CTRL,
        }
    }
}

/// Key code — what physical/logical key was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    Delete,
    Insert,
    /// Key not representable by other variants.
    Unknown(u32),
}

bitflags! {
    /// Modifier keys held during a key press.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Modifiers: u8 {
        const CTRL  = 0b0001;
        const SHIFT = 0b0010;
        const ALT   = 0b0100;
        const META  = 0b1000;
    }
}

/// Alias used for prefix key configuration.
pub type KeyCombo = KeyEvent;
```

- [ ] **Step 4: Add terminal-core to workspace members**

In `Cargo.toml` (workspace root), add `"crates/terminal-core"` to the `[workspace] members` list.

- [ ] **Step 5: Verify it builds**

Run: `cargo build -p terminal-core`
Expected: clean build, no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/terminal-core/ Cargo.toml
git commit -m "feat(terminal-core): scaffold crate with KeyEvent, KeyCode, Modifiers"
```

---

### Task 2: Add DashboardCommand and InputRouter

**Files:**
- Create: `crates/terminal-core/src/command.rs`
- Create: `crates/terminal-core/src/input.rs`

- [ ] **Step 1: Write InputRouter tests first**

Add to `crates/terminal-core/src/input.rs`:

```rust
//! Prefix-key state machine for terminal multiplexer commands.
//!
//! Lifted from kernel/src/dashboard.rs (PrefixState enum at line 691,
//! handle_prefix_command at line 1199). The router receives KeyEvents and
//! produces RouterOutcomes: either a parsed command, a "forward to pane"
//! signal, or a "swallow" (prefix key consumed, unknown command ignored).

use alloc::collections::BTreeMap;
use crate::key::{KeyEvent, KeyCode, KeyCombo, Modifiers};
use crate::command::DashboardCommand;

/// What the router decided about a key event.
#[derive(Debug, PartialEq, Eq)]
pub enum RouterOutcome {
    /// Prefix sequence completed — dispatch this command.
    Command(DashboardCommand),
    /// Not a prefix sequence — pass this key to the focused pane.
    ForwardToPane,
    /// Prefix key itself consumed, or unknown command key after prefix.
    Swallow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    AwaitingCommand,
}

/// Prefix-key input router.
///
/// Consumers (ClaudioOS dashboard, claudio-mux) feed every keystroke through
/// `handle_key()` and act on the returned `RouterOutcome`.
pub struct InputRouter {
    mode: Mode,
    prefix: KeyCombo,
    bindings: BTreeMap<char, DashboardCommand>,
}

impl InputRouter {
    /// Create a router with default Ctrl+B prefix and standard bindings.
    pub fn new() -> Self {
        let mut bindings = BTreeMap::new();
        bindings.insert('"', DashboardCommand::SplitHorizontal);
        bindings.insert('%', DashboardCommand::SplitVertical);
        bindings.insert('n', DashboardCommand::FocusNext);
        bindings.insert('p', DashboardCommand::FocusPrev);
        bindings.insert('x', DashboardCommand::ClosePane);
        bindings.insert('c', DashboardCommand::SpawnAgent);
        bindings.insert('s', DashboardCommand::SpawnShell);
        bindings.insert('f', DashboardCommand::OpenFiles);
        bindings.insert('w', DashboardCommand::OpenBrowser);
        bindings.insert('L', DashboardCommand::NextLayout);
        bindings.insert('q', DashboardCommand::Quit);
        bindings.insert('t', DashboardCommand::ToggleStatusBar);

        Self {
            mode: Mode::Normal,
            prefix: KeyEvent::ctrl('b'),
            bindings,
        }
    }

    /// Change the prefix key (e.g., to Ctrl+A).
    pub fn with_prefix(mut self, prefix: KeyCombo) -> Self {
        self.prefix = prefix;
        self
    }

    /// Rebind a command key.
    pub fn rebind(&mut self, key: char, cmd: DashboardCommand) {
        self.bindings.insert(key, cmd);
    }

    /// Process one key event. Returns what the consumer should do.
    pub fn handle_key(&mut self, key: KeyEvent) -> RouterOutcome {
        match self.mode {
            Mode::Normal => {
                if key == self.prefix {
                    self.mode = Mode::AwaitingCommand;
                    RouterOutcome::Swallow
                } else {
                    RouterOutcome::ForwardToPane
                }
            }
            Mode::AwaitingCommand => {
                self.mode = Mode::Normal;
                if let KeyCode::Char(c) = key.code {
                    if let Some(cmd) = self.bindings.get(&c) {
                        RouterOutcome::Command(*cmd)
                    } else {
                        log::debug!("[input-router] unknown command key: {:?}", c);
                        RouterOutcome::Swallow
                    }
                } else {
                    RouterOutcome::Swallow
                }
            }
        }
    }

    /// Whether the router is currently waiting for a command key.
    pub fn is_awaiting_command(&self) -> bool {
        self.mode == Mode::AwaitingCommand
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{KeyEvent, KeyCode, Modifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::plain(KeyCode::Char(c))
    }

    fn ctrl_b() -> KeyEvent {
        KeyEvent::ctrl('b')
    }

    #[test]
    fn normal_keys_forward_to_pane() {
        let mut router = InputRouter::new();
        assert_eq!(router.handle_key(key('a')), RouterOutcome::ForwardToPane);
        assert_eq!(router.handle_key(key('z')), RouterOutcome::ForwardToPane);
        assert_eq!(
            router.handle_key(KeyEvent::plain(KeyCode::Enter)),
            RouterOutcome::ForwardToPane,
        );
    }

    #[test]
    fn prefix_then_command_yields_command() {
        let mut router = InputRouter::new();
        assert_eq!(router.handle_key(ctrl_b()), RouterOutcome::Swallow);
        assert!(router.is_awaiting_command());
        assert_eq!(
            router.handle_key(key('"')),
            RouterOutcome::Command(DashboardCommand::SplitHorizontal),
        );
        assert!(!router.is_awaiting_command());
    }

    #[test]
    fn prefix_then_unknown_swallows() {
        let mut router = InputRouter::new();
        assert_eq!(router.handle_key(ctrl_b()), RouterOutcome::Swallow);
        assert_eq!(router.handle_key(key('z')), RouterOutcome::Swallow);
        // Back to normal mode — next key forwards.
        assert_eq!(router.handle_key(key('a')), RouterOutcome::ForwardToPane);
    }

    #[test]
    fn all_default_bindings() {
        let cases: &[(char, DashboardCommand)] = &[
            ('"', DashboardCommand::SplitHorizontal),
            ('%', DashboardCommand::SplitVertical),
            ('n', DashboardCommand::FocusNext),
            ('p', DashboardCommand::FocusPrev),
            ('x', DashboardCommand::ClosePane),
            ('c', DashboardCommand::SpawnAgent),
            ('s', DashboardCommand::SpawnShell),
            ('f', DashboardCommand::OpenFiles),
            ('w', DashboardCommand::OpenBrowser),
            ('L', DashboardCommand::NextLayout),
            ('q', DashboardCommand::Quit),
            ('t', DashboardCommand::ToggleStatusBar),
        ];
        for &(c, ref expected) in cases {
            let mut router = InputRouter::new();
            assert_eq!(router.handle_key(ctrl_b()), RouterOutcome::Swallow);
            assert_eq!(
                router.handle_key(key(c)),
                RouterOutcome::Command(*expected),
                "binding for {:?} failed",
                c,
            );
        }
    }

    #[test]
    fn custom_prefix() {
        let mut router = InputRouter::new().with_prefix(KeyEvent::ctrl('a'));
        // Ctrl+B no longer triggers prefix.
        assert_eq!(router.handle_key(ctrl_b()), RouterOutcome::ForwardToPane);
        // Ctrl+A does.
        assert_eq!(
            router.handle_key(KeyEvent::ctrl('a')),
            RouterOutcome::Swallow,
        );
        assert_eq!(
            router.handle_key(key('n')),
            RouterOutcome::Command(DashboardCommand::FocusNext),
        );
    }

    #[test]
    fn rebind_command() {
        let mut router = InputRouter::new();
        router.rebind('z', DashboardCommand::Quit);
        assert_eq!(router.handle_key(ctrl_b()), RouterOutcome::Swallow);
        assert_eq!(
            router.handle_key(key('z')),
            RouterOutcome::Command(DashboardCommand::Quit),
        );
    }

    #[test]
    fn non_char_key_after_prefix_swallows() {
        let mut router = InputRouter::new();
        assert_eq!(router.handle_key(ctrl_b()), RouterOutcome::Swallow);
        assert_eq!(
            router.handle_key(KeyEvent::plain(KeyCode::Up)),
            RouterOutcome::Swallow,
        );
        // Back to normal.
        assert_eq!(router.handle_key(key('a')), RouterOutcome::ForwardToPane);
    }
}
```

- [ ] **Step 2: Create src/command.rs**

```rust
//! Dashboard commands — the vocabulary of prefix-key actions.
//!
//! This enum is intentionally policy-free. It encodes *what the user pressed*,
//! not *what each consumer does in response*. ClaudioOS and claudio-mux both
//! match on DashboardCommand and apply their own platform-specific behavior.

/// A multiplexer command triggered by a prefix-key sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardCommand {
    SplitHorizontal,
    SplitVertical,
    FocusNext,
    FocusPrev,
    ClosePane,
    SpawnShell,
    SpawnAgent,
    /// Reserved — ClaudioOS opens file manager, claudio-mux rejects.
    OpenFiles,
    /// Reserved — ClaudioOS opens browser, claudio-mux rejects.
    OpenBrowser,
    ToggleStatusBar,
    PreviousLayout,
    NextLayout,
    Quit,
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p terminal-core`
Expected: 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/terminal-core/src/command.rs crates/terminal-core/src/input.rs
git commit -m "feat(terminal-core): add DashboardCommand enum and InputRouter with 7 tests"
```

---

### Task 3: Add Color and Cell types

**Files:**
- Create: `crates/terminal-core/src/color.rs`
- Create: `crates/terminal-core/src/cell.rs`

- [ ] **Step 1: Create src/color.rs**

Moved from `crates/terminal/src/render.rs` lines 10-80. Strip the `to_bgr32` method (pixel-specific, belongs in terminal-fb).

```rust
//! Terminal color representation.
//!
//! Standard 16-color palette matching ClaudioOS's existing colors.
//! Consumers (terminal-fb, terminal-ansi) map these to their native
//! color formats.

/// RGB color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    // Standard 8-color palette (SGR 30-37).
    pub const BLACK: Self = Self::new(0, 0, 0);
    pub const RED: Self = Self::new(204, 0, 0);
    pub const GREEN: Self = Self::new(0, 204, 0);
    pub const YELLOW: Self = Self::new(204, 204, 0);
    pub const BLUE: Self = Self::new(0, 0, 204);
    pub const MAGENTA: Self = Self::new(204, 0, 204);
    pub const CYAN: Self = Self::new(0, 204, 204);
    pub const WHITE: Self = Self::new(204, 204, 204);

    // Bright variants (SGR 90-97).
    pub const BRIGHT_BLACK: Self = Self::new(128, 128, 128);
    pub const BRIGHT_RED: Self = Self::new(255, 85, 85);
    pub const BRIGHT_GREEN: Self = Self::new(85, 255, 85);
    pub const BRIGHT_YELLOW: Self = Self::new(255, 255, 85);
    pub const BRIGHT_BLUE: Self = Self::new(85, 85, 255);
    pub const BRIGHT_MAGENTA: Self = Self::new(255, 85, 255);
    pub const BRIGHT_CYAN: Self = Self::new(85, 255, 255);
    pub const BRIGHT_WHITE: Self = Self::new(255, 255, 255);

    pub const DEFAULT_FG: Self = Self::WHITE;
    pub const DEFAULT_BG: Self = Self::new(16, 16, 16);
}
```

- [ ] **Step 2: Create src/cell.rs**

Moved from `crates/terminal/src/pane.rs` lines 19-33.

```rust
//! A single character cell in the terminal grid.

use crate::color::Color;

/// One cell of the terminal grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
```

- [ ] **Step 3: Verify build**

Run: `cargo build -p terminal-core`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/terminal-core/src/color.rs crates/terminal-core/src/cell.rs
git commit -m "feat(terminal-core): add Color palette and Cell type"
```

---

### Task 4: Add CellViewport

**Files:**
- Create: `crates/terminal-core/src/viewport.rs`

- [ ] **Step 1: Create src/viewport.rs with tests**

```rust
//! Cell-based viewport — a rectangular region in character coordinates.
//!
//! Unlike the pixel-based `Viewport` in the old terminal crate, CellViewport
//! uses column/row coordinates. Pixel conversion is the renderer's job
//! (terminal-fb or terminal-ansi).

/// A rectangular region measured in character cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellViewport {
    /// Column offset (0-based, from left edge of the screen).
    pub col: u16,
    /// Row offset (0-based, from top edge of the screen).
    pub row: u16,
    /// Width in columns.
    pub cols: u16,
    /// Height in rows.
    pub rows: u16,
}

impl CellViewport {
    pub const fn new(col: u16, row: u16, cols: u16, rows: u16) -> Self {
        Self { col, row, cols, rows }
    }

    /// Right edge column (exclusive).
    pub const fn right(&self) -> u16 {
        self.col + self.cols
    }

    /// Bottom edge row (exclusive).
    pub const fn bottom(&self) -> u16 {
        self.row + self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds() {
        let vp = CellViewport::new(5, 10, 80, 24);
        assert_eq!(vp.right(), 85);
        assert_eq!(vp.bottom(), 34);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p terminal-core`
Expected: all pass (8 tests from input + 1 from viewport).

- [ ] **Step 3: Commit**

```bash
git add crates/terminal-core/src/viewport.rs
git commit -m "feat(terminal-core): add CellViewport (cell-based coordinates)"
```

---

### Task 5: Add core Pane (cell grid + VTE, no pixel math)

**Files:**
- Create: `crates/terminal-core/src/pane.rs`
- Modify: `crates/terminal-core/Cargo.toml` (add vte dependency)

This is the largest single task. The Pane is adapted from `crates/terminal/src/pane.rs` (687 lines) with all pixel math removed. The grid uses `CellViewport` for its dimensions. Rendering is NOT part of this pane — it exposes its cell grid for external renderers.

- [ ] **Step 1: Add vte and spin to terminal-core Cargo.toml**

```toml
[package]
name = "terminal-core"
version.workspace = true
edition.workspace = true

[dependencies]
bitflags = "2"
log = { workspace = true }
vte = { workspace = true }
spin = { workspace = true }
```

- [ ] **Step 2: Write Pane tests**

Add to end of `crates/terminal-core/src/pane.rs`:

```rust
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
        // ESC[31m = red foreground, then "X"
        p.write_bytes(b"\x1b[31mX");
        assert_eq!(p.cell(0, 0).fg, Color::RED);
    }

    #[test]
    fn cursor_movement() {
        let mut p = make_pane(80, 24);
        // ESC[3;5H = move cursor to row 3, col 5 (1-based in ANSI)
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
        // Content in the intersection is preserved.
        assert_eq!(p.cell(0, 0).ch, 'H');
    }

    #[test]
    fn erase_in_display() {
        let mut p = make_pane(80, 24);
        p.write_str("ABCDEF");
        // ESC[2J = erase entire display
        p.write_bytes(b"\x1b[2J");
        assert_eq!(p.cell(0, 0).ch, ' ');
        assert_eq!(p.cell(0, 5).ch, ' ');
    }

    #[test]
    fn scroll_up() {
        let mut p = make_pane(10, 3);
        p.write_str("AAA\nBBB\nCCC");
        // Writing on last row with newline triggers scroll.
        p.write_str("\nDDD");
        // First row (AAA) scrolled off.
        assert_eq!(p.cell(0, 0).ch, 'B');
        assert_eq!(p.cell(2, 0).ch, 'D');
    }
}
```

- [ ] **Step 3: Implement Pane**

Write the full `crates/terminal-core/src/pane.rs`. This is adapted from `crates/terminal/src/pane.rs` with these changes:
- `id` field: `PaneId` (u64) instead of `usize`
- `viewport` field: `CellViewport` instead of pixel `Viewport`
- `cols/rows` come directly from `CellViewport` (no `viewport.width / FONT_WIDTH`)
- NO `render()`, `render_dirty()`, `render_cursor()`, `render_cursor_delta()` methods — rendering is external
- NEW: `cell(row, col) -> &Cell` accessor for external renderers
- NEW: `cursor_pos() -> (usize, usize)` accessor for cursor rendering
- NEW: `prev_cursor_pos() -> Option<(usize, usize)>` accessor
- All VTE handling (PanePerformer, print, execute, csi_dispatch, esc_dispatch) moves over unchanged
- SGR color parsing moves over unchanged (it uses Color, which is now in terminal-core)

```rust
//! Terminal pane — a character-cell grid backed by a VTE parser.
//!
//! Each pane owns a grid of Cells, a cursor, colour state, and a vte::Parser
//! that interprets incoming byte streams as ANSI escape sequences.
//!
//! This pane knows NOTHING about pixels or rendering. It exposes its cell grid
//! and cursor position for external renderers (terminal-fb, terminal-ansi).

use alloc::vec;
use alloc::vec::Vec;

use crate::cell::Cell;
use crate::color::Color;
use crate::viewport::CellViewport;
use crate::PaneId;

/// A virtual terminal pane with a character-cell grid.
pub struct Pane {
    /// Unique identifier.
    pub id: PaneId,
    /// Cell-based viewport (position + dimensions in columns/rows).
    pub viewport: CellViewport,
    /// Grid dimensions.
    cols: usize,
    rows: usize,
    /// Cursor position (0-based).
    cursor_row: usize,
    cursor_col: usize,
    /// The visible cell grid.
    cells: Vec<Vec<Cell>>,
    /// Current drawing colours.
    current_fg: Color,
    current_bg: Color,
    /// Reserved for scrollback.
    scroll_offset: usize,
    /// VTE parser.
    vte_parser: vte::Parser,
    /// Dirty flag.
    dirty: bool,
    /// Per-row dirty flags.
    dirty_rows: Vec<bool>,
    /// Saved cursor (CSI s / CSI u).
    saved_cursor: Option<(usize, usize)>,
    /// Previous cursor position for delta rendering.
    prev_cursor: Option<(usize, usize)>,
}

impl Pane {
    pub fn new(id: PaneId, viewport: CellViewport) -> Self {
        let cols = (viewport.cols as usize).max(1);
        let rows = (viewport.rows as usize).max(1);
        let cells = vec![vec![Cell::default(); cols]; rows];
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
            dirty_rows: vec![false; rows],
            saved_cursor: None,
            prev_cursor: None,
        }
    }

    // -- accessors ----------------------------------------------------------

    pub fn cols(&self) -> usize { self.cols }
    pub fn rows(&self) -> usize { self.rows }
    pub fn is_dirty(&self) -> bool { self.dirty }
    pub fn dirty_rows(&self) -> &[bool] { &self.dirty_rows }

    /// Access a cell by (row, col). Returns default cell if out of bounds.
    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        if row < self.rows && col < self.cols {
            &self.cells[row][col]
        } else {
            // Return a static default for OOB reads rather than panicking.
            static DEFAULT: Cell = Cell { ch: ' ', fg: Color::DEFAULT_FG, bg: Color::DEFAULT_BG };
            &DEFAULT
        }
    }

    /// Current cursor position (row, col).
    pub fn cursor_pos(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    /// Previous cursor position, for delta rendering.
    pub fn prev_cursor_pos(&self) -> Option<(usize, usize)> {
        self.prev_cursor
    }

    /// Mark prev_cursor as current (call after rendering cursor).
    pub fn update_prev_cursor(&mut self) {
        self.prev_cursor = Some((self.cursor_row, self.cursor_col));
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
        for d in self.dirty_rows.iter_mut() {
            *d = false;
        }
    }

    // -- writing ------------------------------------------------------------

    /// Feed raw bytes (may contain ANSI sequences) through the VTE parser.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let mut performer = PanePerformer { pane: self };
            let mut parser = core::mem::replace(&mut self.vte_parser, vte::Parser::new());
            parser.advance(&mut performer, byte);
            self.vte_parser = parser;
        }
    }

    /// Write a UTF-8 string (convenience wrapper).
    pub fn write_str(&mut self, s: &str) {
        self.write_bytes(s.as_bytes());
    }

    /// Resize the pane. Preserves content in the intersection of old and new grids.
    pub fn resize(&mut self, viewport: CellViewport) {
        let new_cols = (viewport.cols as usize).max(1);
        let new_rows = (viewport.rows as usize).max(1);
        let mut new_cells = vec![vec![Cell::default(); new_cols]; new_rows];
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
        self.dirty_rows = vec![true; new_rows];
        self.dirty = true;
        self.cursor_row = self.cursor_row.min(new_rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(new_cols.saturating_sub(1));
    }

    // -- internal helpers ---------------------------------------------------

    fn mark_row_dirty(&mut self, row: usize) {
        if row < self.dirty_rows.len() {
            self.dirty_rows[row] = true;
            self.dirty = true;
        }
    }

    fn mark_all_dirty(&mut self) {
        for d in self.dirty_rows.iter_mut() {
            *d = true;
        }
        self.dirty = true;
    }

    fn scroll_up(&mut self) {
        self.cells.remove(0);
        self.cells.push(vec![Cell::default(); self.cols]);
        self.mark_all_dirty();
    }

    fn advance_cursor(&mut self) {
        self.cursor_col += 1;
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.cursor_row += 1;
            if self.cursor_row >= self.rows {
                self.scroll_up();
                self.cursor_row = self.rows - 1;
            }
        }
    }

    fn newline(&mut self) {
        self.cursor_col = 0;
        self.cursor_row += 1;
        if self.cursor_row >= self.rows {
            self.scroll_up();
            self.cursor_row = self.rows - 1;
        }
    }

    fn erase_cells(&mut self, row: usize, col_start: usize, col_end: usize) {
        if row >= self.rows { return; }
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
}

// ---------------------------------------------------------------------------
// VTE performer
// ---------------------------------------------------------------------------

struct PanePerformer<'a> {
    pane: &'a mut Pane,
}

impl<'a> vte::Perform for PanePerformer<'a> {
    fn print(&mut self, c: char) {
        let p = &mut self.pane;
        if p.cursor_row < p.rows && p.cursor_col < p.cols {
            p.cells[p.cursor_row][p.cursor_col] = Cell {
                ch: c,
                fg: p.current_fg,
                bg: p.current_bg,
            };
            p.mark_row_dirty(p.cursor_row);
        }
        p.advance_cursor();
    }

    fn execute(&mut self, byte: u8) {
        let p = &mut self.pane;
        match byte {
            0x0A => p.newline(),                    // LF
            0x0D => { p.cursor_col = 0; }           // CR
            0x09 => {                               // TAB
                let next = (p.cursor_col + 8) & !7;
                p.cursor_col = next.min(p.cols.saturating_sub(1));
            }
            0x08 => {                               // BS
                if p.cursor_col > 0 { p.cursor_col -= 1; }
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
        let p = &mut self.pane;
        let params_vec: Vec<u16> = params.iter()
            .flat_map(|sub| sub.iter().copied())
            .collect();

        match action {
            // Cursor movement
            'A' => { // CUU — up
                let n = (*params_vec.first().unwrap_or(&1)).max(1) as usize;
                p.cursor_row = p.cursor_row.saturating_sub(n);
            }
            'B' => { // CUD — down
                let n = (*params_vec.first().unwrap_or(&1)).max(1) as usize;
                p.cursor_row = (p.cursor_row + n).min(p.rows.saturating_sub(1));
            }
            'C' => { // CUF — right
                let n = (*params_vec.first().unwrap_or(&1)).max(1) as usize;
                p.cursor_col = (p.cursor_col + n).min(p.cols.saturating_sub(1));
            }
            'D' => { // CUB — left
                let n = (*params_vec.first().unwrap_or(&1)).max(1) as usize;
                p.cursor_col = p.cursor_col.saturating_sub(n);
            }
            'H' | 'f' => { // CUP — absolute position (1-based)
                let row = (*params_vec.first().unwrap_or(&1)).max(1) as usize - 1;
                let col = (*params_vec.get(1).unwrap_or(&1)).max(1) as usize - 1;
                p.cursor_row = row.min(p.rows.saturating_sub(1));
                p.cursor_col = col.min(p.cols.saturating_sub(1));
            }
            'G' => { // CHA — absolute column (1-based)
                let col = (*params_vec.first().unwrap_or(&1)).max(1) as usize - 1;
                p.cursor_col = col.min(p.cols.saturating_sub(1));
            }
            'd' => { // VPA — absolute row (1-based)
                let row = (*params_vec.first().unwrap_or(&1)).max(1) as usize - 1;
                p.cursor_row = row.min(p.rows.saturating_sub(1));
            }
            's' => { // Save cursor
                p.saved_cursor = Some((p.cursor_row, p.cursor_col));
            }
            'u' => { // Restore cursor
                if let Some((r, c)) = p.saved_cursor {
                    p.cursor_row = r.min(p.rows.saturating_sub(1));
                    p.cursor_col = c.min(p.cols.saturating_sub(1));
                }
            }
            // Erase
            'J' => { // ED — erase in display
                let mode = *params_vec.first().unwrap_or(&0);
                match mode {
                    0 => { // from cursor to end
                        p.erase_cells(p.cursor_row, p.cursor_col, p.cols);
                        for r in (p.cursor_row + 1)..p.rows {
                            p.erase_cells(r, 0, p.cols);
                        }
                    }
                    1 => { // from start to cursor
                        for r in 0..p.cursor_row {
                            p.erase_cells(r, 0, p.cols);
                        }
                        p.erase_cells(p.cursor_row, 0, p.cursor_col + 1);
                    }
                    2 | 3 => { // entire display
                        for r in 0..p.rows {
                            p.erase_cells(r, 0, p.cols);
                        }
                    }
                    _ => {}
                }
            }
            'K' => { // EL — erase in line
                let mode = *params_vec.first().unwrap_or(&0);
                match mode {
                    0 => p.erase_cells(p.cursor_row, p.cursor_col, p.cols),
                    1 => p.erase_cells(p.cursor_row, 0, p.cursor_col + 1),
                    2 => p.erase_cells(p.cursor_row, 0, p.cols),
                    _ => {}
                }
            }
            // Scroll
            'S' => { // SU — scroll up
                let n = (*params_vec.first().unwrap_or(&1)).max(1) as usize;
                for _ in 0..n { p.scroll_up(); }
            }
            // Insert / delete lines
            'L' => { // IL — insert lines
                let n = (*params_vec.first().unwrap_or(&1)).max(1) as usize;
                for _ in 0..n {
                    if p.cursor_row < p.rows {
                        p.cells.pop();
                        p.cells.insert(p.cursor_row, vec![Cell::default(); p.cols]);
                    }
                }
                p.mark_all_dirty();
            }
            'M' => { // DL — delete lines
                let n = (*params_vec.first().unwrap_or(&1)).max(1) as usize;
                for _ in 0..n {
                    if p.cursor_row < p.rows {
                        p.cells.remove(p.cursor_row);
                        p.cells.push(vec![Cell::default(); p.cols]);
                    }
                }
                p.mark_all_dirty();
            }
            // SGR — colors
            'm' => {
                if params_vec.is_empty() {
                    p.current_fg = Color::DEFAULT_FG;
                    p.current_bg = Color::DEFAULT_BG;
                    return;
                }
                let mut i = 0;
                while i < params_vec.len() {
                    match params_vec[i] {
                        0 => {
                            p.current_fg = Color::DEFAULT_FG;
                            p.current_bg = Color::DEFAULT_BG;
                        }
                        1 => {} // bold — not tracked yet
                        30 => p.current_fg = Color::BLACK,
                        31 => p.current_fg = Color::RED,
                        32 => p.current_fg = Color::GREEN,
                        33 => p.current_fg = Color::YELLOW,
                        34 => p.current_fg = Color::BLUE,
                        35 => p.current_fg = Color::MAGENTA,
                        36 => p.current_fg = Color::CYAN,
                        37 => p.current_fg = Color::WHITE,
                        39 => p.current_fg = Color::DEFAULT_FG,
                        40 => p.current_bg = Color::BLACK,
                        41 => p.current_bg = Color::RED,
                        42 => p.current_bg = Color::GREEN,
                        43 => p.current_bg = Color::YELLOW,
                        44 => p.current_bg = Color::BLUE,
                        45 => p.current_bg = Color::MAGENTA,
                        46 => p.current_bg = Color::CYAN,
                        47 => p.current_bg = Color::WHITE,
                        49 => p.current_bg = Color::DEFAULT_BG,
                        90 => p.current_fg = Color::BRIGHT_BLACK,
                        91 => p.current_fg = Color::BRIGHT_RED,
                        92 => p.current_fg = Color::BRIGHT_GREEN,
                        93 => p.current_fg = Color::BRIGHT_YELLOW,
                        94 => p.current_fg = Color::BRIGHT_BLUE,
                        95 => p.current_fg = Color::BRIGHT_MAGENTA,
                        96 => p.current_fg = Color::BRIGHT_CYAN,
                        97 => p.current_fg = Color::BRIGHT_WHITE,
                        100 => p.current_bg = Color::BRIGHT_BLACK,
                        101 => p.current_bg = Color::BRIGHT_RED,
                        102 => p.current_bg = Color::BRIGHT_GREEN,
                        103 => p.current_bg = Color::BRIGHT_YELLOW,
                        104 => p.current_bg = Color::BRIGHT_BLUE,
                        105 => p.current_bg = Color::BRIGHT_MAGENTA,
                        106 => p.current_bg = Color::BRIGHT_CYAN,
                        107 => p.current_bg = Color::BRIGHT_WHITE,
                        38 => { // Extended foreground
                            if i + 1 < params_vec.len() && params_vec[i + 1] == 2 {
                                if i + 4 < params_vec.len() {
                                    p.current_fg = Color::new(
                                        params_vec[i + 2] as u8,
                                        params_vec[i + 3] as u8,
                                        params_vec[i + 4] as u8,
                                    );
                                    i += 4;
                                }
                            } else if i + 1 < params_vec.len() && params_vec[i + 1] == 5 {
                                // 256-color mode — map to nearest basic color.
                                if i + 2 < params_vec.len() {
                                    p.current_fg = color_256(params_vec[i + 2]);
                                    i += 2;
                                }
                            }
                        }
                        48 => { // Extended background
                            if i + 1 < params_vec.len() && params_vec[i + 1] == 2 {
                                if i + 4 < params_vec.len() {
                                    p.current_bg = Color::new(
                                        params_vec[i + 2] as u8,
                                        params_vec[i + 3] as u8,
                                        params_vec[i + 4] as u8,
                                    );
                                    i += 4;
                                }
                            } else if i + 1 < params_vec.len() && params_vec[i + 1] == 5 {
                                if i + 2 < params_vec.len() {
                                    p.current_bg = color_256(params_vec[i + 2]);
                                    i += 2;
                                }
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => {
                log::trace!("[pane] unhandled CSI: {:?} {:?}", params_vec, action);
            }
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        let p = &mut self.pane;
        match byte {
            b'7' => { // DECSC — save cursor
                p.saved_cursor = Some((p.cursor_row, p.cursor_col));
            }
            b'8' => { // DECRC — restore cursor
                if let Some((r, c)) = p.saved_cursor {
                    p.cursor_row = r.min(p.rows.saturating_sub(1));
                    p.cursor_col = c.min(p.cols.saturating_sub(1));
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// 256-color lookup
// ---------------------------------------------------------------------------

fn color_256(idx: u16) -> Color {
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
            let idx = idx - 16;
            let r = ((idx / 36) % 6) as u8 * 51;
            let g = ((idx / 6) % 6) as u8 * 51;
            let b = (idx % 6) as u8 * 51;
            Color::new(r, g, b)
        }
        232..=255 => {
            let v = ((idx - 232) as u8) * 10 + 8;
            Color::new(v, v, v)
        }
        _ => Color::DEFAULT_FG,
    }
}
```

Note: The `write_bytes` method has a subtle issue — VTE parser and Pane are in the same struct, but the performer borrows Pane mutably while the parser is also owned by Pane. The solution is the same swap trick used in the existing code: temporarily move the parser out with `core::mem::replace`, advance it, then move it back. This is already shown in the implementation above.

- [ ] **Step 3: Run tests**

Run: `cargo test -p terminal-core`
Expected: all 19 tests pass (7 input + 1 viewport + 1 bounds + 10 pane).

- [ ] **Step 4: Commit**

```bash
git add crates/terminal-core/src/pane.rs crates/terminal-core/Cargo.toml
git commit -m "feat(terminal-core): add Pane with VTE integration and 10 tests (no pixel math)"
```

---

### Task 6: Add cell-based Layout

**Files:**
- Create: `crates/terminal-core/src/layout.rs`

Adapted from `crates/terminal/src/layout.rs` (458 lines). Key changes:
- `Layout::new(cols: u16, rows: u16)` instead of `(screen_width, screen_height)`
- Uses `CellViewport` instead of pixel `Viewport`
- `LayoutNode` uses `CellViewport`
- No `SEPARATOR_PX`, no pixel separator rendering, no `render_*` methods
- Separator is 1 column (vertical split) or 1 row (horizontal split) in cell coordinates
- `pane_id` is `PaneId` (u64) instead of `usize`

- [ ] **Step 1: Write Layout tests**

Add to end of `crates/terminal-core/src/layout.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_layout_has_one_pane() {
        let layout = Layout::new(80, 24);
        assert_eq!(layout.pane_count(), 1);
        assert_eq!(layout.focused_pane().viewport.cols, 80);
        assert_eq!(layout.focused_pane().viewport.rows, 24);
    }

    #[test]
    fn split_vertical_creates_two_panes() {
        let mut layout = Layout::new(80, 24);
        layout.split(SplitDirection::Vertical);
        assert_eq!(layout.pane_count(), 2);
        // Left pane: 39 cols (80/2 - 1 separator col)
        let p0 = &layout.panes()[0];
        let p1 = &layout.panes()[1];
        assert_eq!(p0.viewport.cols + 1 + p1.viewport.cols, 80);
    }

    #[test]
    fn split_horizontal_creates_two_panes() {
        let mut layout = Layout::new(80, 24);
        layout.split(SplitDirection::Horizontal);
        assert_eq!(layout.pane_count(), 2);
        let p0 = &layout.panes()[0];
        let p1 = &layout.panes()[1];
        assert_eq!(p0.viewport.rows + 1 + p1.viewport.rows, 24);
    }

    #[test]
    fn focus_next_cycles() {
        let mut layout = Layout::new(80, 24);
        layout.split(SplitDirection::Vertical);
        let first = layout.focused_pane_id();
        layout.focus_next();
        assert_ne!(layout.focused_pane_id(), first);
        layout.focus_next();
        assert_eq!(layout.focused_pane_id(), first);
    }

    #[test]
    fn focus_prev_cycles() {
        let mut layout = Layout::new(80, 24);
        layout.split(SplitDirection::Vertical);
        let first = layout.focused_pane_id();
        layout.focus_prev();
        assert_ne!(layout.focused_pane_id(), first);
        layout.focus_prev();
        assert_eq!(layout.focused_pane_id(), first);
    }

    #[test]
    fn close_focused_removes_pane() {
        let mut layout = Layout::new(80, 24);
        layout.split(SplitDirection::Vertical);
        assert_eq!(layout.pane_count(), 2);
        layout.close_focused();
        assert_eq!(layout.pane_count(), 1);
        // Remaining pane takes full width.
        assert_eq!(layout.focused_pane().viewport.cols, 80);
    }

    #[test]
    fn close_last_pane_is_noop() {
        let mut layout = Layout::new(80, 24);
        layout.close_focused();
        assert_eq!(layout.pane_count(), 1);
    }

    #[test]
    fn resize_reflows_all_panes() {
        let mut layout = Layout::new(80, 24);
        layout.split(SplitDirection::Vertical);
        layout.resize(120, 40);
        let total_cols: u16 = layout.panes().iter()
            .map(|p| p.viewport.cols)
            .sum::<u16>() + 1; // +1 for separator
        assert_eq!(total_cols, 120);
    }

    #[test]
    fn pane_by_id_returns_correct_pane() {
        let mut layout = Layout::new(80, 24);
        layout.split(SplitDirection::Vertical);
        let id = layout.panes()[1].id;
        assert!(layout.pane_by_id_mut(id).is_some());
        assert!(layout.pane_by_id_mut(9999).is_none());
    }

    #[test]
    fn nested_splits() {
        let mut layout = Layout::new(120, 40);
        layout.split(SplitDirection::Vertical);   // 2 panes
        layout.split(SplitDirection::Horizontal);  // 3 panes (focused splits again)
        assert_eq!(layout.pane_count(), 3);
    }
}
```

- [ ] **Step 2: Implement Layout**

```rust
//! Layout tree — manages terminal panes in a binary split tree.
//!
//! All coordinates are in character cells (not pixels). Separator between
//! splits is 1 cell wide (vertical) or 1 cell tall (horizontal).

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use crate::pane::Pane;
use crate::viewport::CellViewport;
use crate::PaneId;

/// Separator thickness in cells.
const SEPARATOR_CELLS: u16 = 1;

/// Direction of a split between two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    /// Left | Right
    Vertical,
    /// Top / Bottom
    Horizontal,
}

/// A node in the binary layout tree.
#[derive(Debug)]
pub enum LayoutNode {
    Leaf {
        pane_id: PaneId,
        viewport: CellViewport,
    },
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

/// Manages the pane layout tree and the collection of live panes.
pub struct Layout {
    root: LayoutNode,
    panes: Vec<Pane>,
    focused_idx: usize,
    next_pane_id: PaneId,
    /// Total screen dimensions in cells.
    total_cols: u16,
    total_rows: u16,
}

impl Layout {
    /// Create a layout with a single pane filling the entire screen.
    pub fn new(cols: u16, rows: u16) -> Self {
        let viewport = CellViewport::new(0, 0, cols, rows);
        let pane = Pane::new(0, viewport.clone());
        Self {
            root: LayoutNode::Leaf {
                pane_id: 0,
                viewport,
            },
            panes: vec![pane],
            focused_idx: 0,
            next_pane_id: 1,
            total_cols: cols,
            total_rows: rows,
        }
    }

    // -- accessors ----------------------------------------------------------

    pub fn focused_pane(&self) -> &Pane { &self.panes[self.focused_idx] }
    pub fn focused_pane_mut(&mut self) -> &mut Pane { &mut self.panes[self.focused_idx] }
    pub fn pane_count(&self) -> usize { self.panes.len() }
    pub fn panes(&self) -> &[Pane] { &self.panes }
    pub fn panes_mut(&mut self) -> &mut [Pane] { &mut self.panes }
    pub fn focused_pane_id(&self) -> PaneId { self.panes[self.focused_idx].id }
    pub fn root(&self) -> &LayoutNode { &self.root }

    pub fn pane_by_id_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    // -- focus --------------------------------------------------------------

    pub fn focus_next(&mut self) {
        if !self.panes.is_empty() {
            self.focused_idx = (self.focused_idx + 1) % self.panes.len();
        }
    }

    pub fn focus_prev(&mut self) {
        if !self.panes.is_empty() {
            self.focused_idx = if self.focused_idx == 0 {
                self.panes.len() - 1
            } else {
                self.focused_idx - 1
            };
        }
    }

    // -- structural operations ----------------------------------------------

    /// Split the focused pane. The focused pane becomes the first child;
    /// a new empty pane becomes the second child and receives focus.
    pub fn split(&mut self, direction: SplitDirection) {
        let target_id = self.panes[self.focused_idx].id;
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;

        if Self::split_node(&mut self.root, target_id, new_id, direction) {
            let vp = CellViewport::new(0, 0, 1, 1); // placeholder
            self.panes.push(Pane::new(new_id, vp));
            self.recompute_viewports();
            // Focus the new pane.
            self.focused_idx = self.panes.iter()
                .position(|p| p.id == new_id)
                .unwrap_or(self.focused_idx);
        }
    }

    /// Close the focused pane (no-op if it's the last one).
    pub fn close_focused(&mut self) {
        if self.panes.len() <= 1 { return; }
        let target_id = self.panes[self.focused_idx].id;
        if Self::remove_leaf(&mut self.root, target_id) {
            self.panes.retain(|p| p.id != target_id);
            if self.focused_idx >= self.panes.len() {
                self.focused_idx = self.panes.len().saturating_sub(1);
            }
            self.recompute_viewports();
        }
    }

    /// Resize the entire layout to new dimensions.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.total_cols = cols;
        self.total_rows = rows;
        self.recompute_viewports();
    }

    // -- tree helpers -------------------------------------------------------

    fn split_node(
        node: &mut LayoutNode,
        target_id: PaneId,
        new_id: PaneId,
        direction: SplitDirection,
    ) -> bool {
        match node {
            LayoutNode::Leaf { pane_id, viewport } if *pane_id == target_id => {
                let old_vp = viewport.clone();
                let first = Box::new(LayoutNode::Leaf {
                    pane_id: target_id,
                    viewport: old_vp.clone(),
                });
                let second = Box::new(LayoutNode::Leaf {
                    pane_id: new_id,
                    viewport: old_vp,
                });
                *node = LayoutNode::Split {
                    direction,
                    ratio: 0.5,
                    first,
                    second,
                };
                true
            }
            LayoutNode::Split { first, second, .. } => {
                Self::split_node(first, target_id, new_id, direction)
                    || Self::split_node(second, target_id, new_id, direction)
            }
            _ => false,
        }
    }

    fn remove_leaf(node: &mut LayoutNode, target_id: PaneId) -> bool {
        match node {
            LayoutNode::Leaf { .. } => false,
            LayoutNode::Split { first, second, .. } => {
                // Check if first child is the target leaf.
                if matches!(first.as_ref(), LayoutNode::Leaf { pane_id, .. } if *pane_id == target_id) {
                    let replacement = core::mem::replace(
                        second.as_mut(),
                        LayoutNode::Leaf { pane_id: 0, viewport: CellViewport::new(0, 0, 1, 1) },
                    );
                    *node = replacement;
                    return true;
                }
                // Check if second child is the target leaf.
                if matches!(second.as_ref(), LayoutNode::Leaf { pane_id, .. } if *pane_id == target_id) {
                    let replacement = core::mem::replace(
                        first.as_mut(),
                        LayoutNode::Leaf { pane_id: 0, viewport: CellViewport::new(0, 0, 1, 1) },
                    );
                    *node = replacement;
                    return true;
                }
                // Recurse.
                Self::remove_leaf(first, target_id)
                    || Self::remove_leaf(second, target_id)
            }
        }
    }

    fn recompute_viewports(&mut self) {
        let root_vp = CellViewport::new(0, 0, self.total_cols, self.total_rows);
        Self::assign_viewports(&mut self.root, &root_vp);
        // Sync pane dimensions from tree.
        Self::sync_panes(&self.root, &mut self.panes);
    }

    fn assign_viewports(node: &mut LayoutNode, vp: &CellViewport) {
        match node {
            LayoutNode::Leaf { viewport, .. } => {
                *viewport = vp.clone();
            }
            LayoutNode::Split { direction, ratio, first, second } => {
                match direction {
                    SplitDirection::Vertical => {
                        let first_cols = ((vp.cols as f32 * *ratio) as u16).max(1);
                        let sep = SEPARATOR_CELLS;
                        let second_cols = vp.cols.saturating_sub(first_cols + sep).max(1);
                        let first_vp = CellViewport::new(vp.col, vp.row, first_cols, vp.rows);
                        let second_vp = CellViewport::new(
                            vp.col + first_cols + sep,
                            vp.row,
                            second_cols,
                            vp.rows,
                        );
                        Self::assign_viewports(first, &first_vp);
                        Self::assign_viewports(second, &second_vp);
                    }
                    SplitDirection::Horizontal => {
                        let first_rows = ((vp.rows as f32 * *ratio) as u16).max(1);
                        let sep = SEPARATOR_CELLS;
                        let second_rows = vp.rows.saturating_sub(first_rows + sep).max(1);
                        let first_vp = CellViewport::new(vp.col, vp.row, vp.cols, first_rows);
                        let second_vp = CellViewport::new(
                            vp.col,
                            vp.row + first_rows + sep,
                            vp.cols,
                            second_rows,
                        );
                        Self::assign_viewports(first, &first_vp);
                        Self::assign_viewports(second, &second_vp);
                    }
                }
            }
        }
    }

    fn sync_panes(node: &LayoutNode, panes: &mut [Pane]) {
        match node {
            LayoutNode::Leaf { pane_id, viewport } => {
                if let Some(pane) = panes.iter_mut().find(|p| p.id == *pane_id) {
                    if pane.viewport != *viewport {
                        pane.resize(viewport.clone());
                    }
                }
            }
            LayoutNode::Split { first, second, .. } => {
                Self::sync_panes(first, panes);
                Self::sync_panes(second, panes);
            }
        }
    }
}

fn root_viewport(node: &LayoutNode) -> &CellViewport {
    match node {
        LayoutNode::Leaf { viewport, .. } => viewport,
        LayoutNode::Split { first, .. } => root_viewport(first),
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p terminal-core`
Expected: all tests pass (~20 tests total).

- [ ] **Step 4: Commit**

```bash
git add crates/terminal-core/src/layout.rs
git commit -m "feat(terminal-core): add cell-based Layout with binary split tree and 10 tests"
```

---

### Task 7: Verify terminal-core builds clean

Sanity check — the entire crate should build and all tests pass.

- [ ] **Step 1: Full build**

Run: `cargo build -p terminal-core`
Expected: clean build, no warnings.

- [ ] **Step 2: Full test suite**

Run: `cargo test -p terminal-core -- --nocapture`
Expected: ~20 tests pass.

- [ ] **Step 3: Commit if any fixups were needed**

Only commit if fixups were applied; otherwise skip.

---

## Phase 2: terminal-fb (Tasks 8–10)

### Task 8: Scaffold terminal-fb crate

**Files:**
- Create: `crates/terminal-fb/Cargo.toml`
- Create: `crates/terminal-fb/src/lib.rs`
- Move: `crates/terminal/src/render.rs` → `crates/terminal-fb/src/render.rs` (with edits)
- Move: `crates/terminal/src/terminus.rs` → `crates/terminal-fb/src/terminus.rs` (unchanged)
- Move: `crates/terminal/src/unicode_font.rs` → `crates/terminal-fb/src/unicode_font.rs` (unchanged)
- Modify: `Cargo.toml` (workspace root) — add terminal-fb to members

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "terminal-fb"
version.workspace = true
edition.workspace = true

[dependencies]
terminal-core = { path = "../terminal-core" }
log = { workspace = true }
```

- [ ] **Step 2: Copy font files unchanged**

```bash
cp "crates/terminal/src/terminus.rs" "crates/terminal-fb/src/terminus.rs"
cp "crates/terminal/src/unicode_font.rs" "crates/terminal-fb/src/unicode_font.rs"
```

- [ ] **Step 3: Create src/render.rs**

Copy from `crates/terminal/src/render.rs` with these changes:
- Import `Color` from `terminal_core::Color` instead of defining it locally
- Add `Color::to_bgr32()` as a method here (or a free function) since it's pixel-specific
- Keep `render_char`, `fill_rect`, `FONT_WIDTH`, `FONT_HEIGHT` unchanged

```rust
//! Pixel-level rendering: character glyphs and filled rectangles.

use terminal_core::Color;

pub use crate::terminus::{CHAR_HEIGHT as FONT_HEIGHT, CHAR_WIDTH as FONT_WIDTH};

/// Abstraction over a pixel framebuffer.
pub trait DrawTarget {
    fn put_pixel(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8);
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn bytes_per_pixel(&self) -> usize { 4 }
    fn stride(&self) -> usize { self.width() }
    fn buffer_mut(&mut self) -> Option<&mut [u8]> { None }
    fn fill_scanline(&mut self, x: usize, y: usize, width: usize, r: u8, g: u8, b: u8) {
        for px in x..x + width {
            self.put_pixel(px, y, r, g, b);
        }
    }
}

/// Convert terminal-core Color to UEFI GOP BGR32 bytes.
pub fn color_to_bgr32(c: Color) -> [u8; 4] {
    [c.b, c.g, c.r, 0]
}

/// Convert pixel dimensions to cell dimensions.
pub fn pixels_to_cells(width: usize, height: usize) -> (u16, u16) {
    let cols = (width / FONT_WIDTH).max(1) as u16;
    let rows = (height / FONT_HEIGHT).max(1) as u16;
    (cols, rows)
}

// render_char and fill_rect are copied from crates/terminal/src/render.rs
// with `Color` imported from terminal_core instead of locally defined.
// (Full implementations preserved from the existing crate — the bodies are
// identical, only the Color import path changes.)

pub fn render_char<D: DrawTarget>(
    target: &mut D,
    x: usize,
    y: usize,
    c: char,
    fg: Color,
    bg: Color,
) {
    let glyph = crate::terminus::get_glyph(c);
    // Fast path: direct buffer write.
    if let Some(buf) = target.buffer_mut() {
        let bpp = target.bytes_per_pixel();
        let stride_px = target.stride();
        let stride_bytes = stride_px * bpp;
        let fg_bytes = color_to_bgr32(fg);
        let bg_bytes = color_to_bgr32(bg);
        for (row_idx, &scanline) in glyph.iter().enumerate() {
            let py = y + row_idx;
            if py >= target.height() { break; }
            let row_start = py * stride_bytes;
            for bit in 0..FONT_WIDTH {
                let px = x + bit;
                if px >= target.width() { break; }
                let offset = row_start + px * bpp;
                let pixel = if (scanline >> (7 - bit)) & 1 != 0 {
                    &fg_bytes
                } else {
                    &bg_bytes
                };
                if offset + bpp <= buf.len() {
                    unsafe {
                        core::ptr::write_volatile(
                            buf.as_mut_ptr().add(offset) as *mut [u8; 4],
                            [pixel[0], pixel[1], pixel[2], pixel[3]],
                        );
                    }
                }
            }
        }
    } else {
        // Slow fallback.
        for (row_idx, &scanline) in glyph.iter().enumerate() {
            for bit in 0..FONT_WIDTH {
                let px = x + bit;
                let py = y + row_idx;
                if (scanline >> (7 - bit)) & 1 != 0 {
                    target.put_pixel(px, py, fg.r, fg.g, fg.b);
                } else {
                    target.put_pixel(px, py, bg.r, bg.g, bg.b);
                }
            }
        }
    }
}

pub fn fill_rect<D: DrawTarget>(
    target: &mut D,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    color: Color,
) {
    for row in y..y + h {
        target.fill_scanline(x, row, w, color.r, color.g, color.b);
    }
}
```

- [ ] **Step 4: Create src/lib.rs**

```rust
//! terminal-fb — ClaudioOS framebuffer renderer for terminal-core.
//!
//! This crate bridges terminal-core's cell-based model to pixel rendering
//! on a GOP framebuffer. It owns the DrawTarget trait, font data, and
//! pixel math.

#![no_std]
extern crate alloc;

pub mod render;
pub mod terminus;
pub mod unicode_font;
pub mod pane_renderer;

pub use render::{DrawTarget, fill_rect, render_char, FONT_HEIGHT, FONT_WIDTH, pixels_to_cells, color_to_bgr32};
```

- [ ] **Step 5: Create src/pane_renderer.rs**

This contains the rendering functions that were previously methods on Pane. They read from terminal-core's Pane (cell grid + cursor) and render to a DrawTarget.

```rust
//! Framebuffer rendering for terminal-core Panes.
//!
//! These functions replace the render methods that were previously on Pane.
//! They read the Pane's cell grid and cursor position, and draw pixels
//! to a DrawTarget.

use terminal_core::Pane;
use crate::render::{self, DrawTarget, FONT_WIDTH, FONT_HEIGHT};

/// Render all cells of a pane to the framebuffer.
pub fn render_pane<D: DrawTarget>(pane: &Pane, target: &mut D) {
    let vp = &pane.viewport;
    for row in 0..pane.rows() {
        for col in 0..pane.cols() {
            let cell = pane.cell(row, col);
            let x = vp.col as usize * FONT_WIDTH + col * FONT_WIDTH;
            let y = vp.row as usize * FONT_HEIGHT + row * FONT_HEIGHT;
            render::render_char(target, x, y, cell.ch, cell.fg, cell.bg);
        }
    }
}

/// Render only dirty rows of a pane.
pub fn render_pane_dirty<D: DrawTarget>(pane: &Pane, target: &mut D) {
    let vp = &pane.viewport;
    let dirty = pane.dirty_rows();
    for row in 0..pane.rows() {
        if row < dirty.len() && dirty[row] {
            for col in 0..pane.cols() {
                let cell = pane.cell(row, col);
                let x = vp.col as usize * FONT_WIDTH + col * FONT_WIDTH;
                let y = vp.row as usize * FONT_HEIGHT + row * FONT_HEIGHT;
                render::render_char(target, x, y, cell.ch, cell.fg, cell.bg);
            }
        }
    }
}

/// Render cursor delta: un-invert old position, invert new position.
pub fn render_cursor_delta<D: DrawTarget>(pane: &mut Pane, target: &mut D) {
    let vp = &pane.viewport;
    let base_x = vp.col as usize * FONT_WIDTH;
    let base_y = vp.row as usize * FONT_HEIGHT;

    // Restore old cursor cell.
    if let Some((prev_r, prev_c)) = pane.prev_cursor_pos() {
        if prev_r < pane.rows() && prev_c < pane.cols() {
            let cell = pane.cell(prev_r, prev_c);
            let x = base_x + prev_c * FONT_WIDTH;
            let y = base_y + prev_r * FONT_HEIGHT;
            render::render_char(target, x, y, cell.ch, cell.fg, cell.bg);
        }
    }

    // Draw new cursor (inverted).
    let (cur_r, cur_c) = pane.cursor_pos();
    if cur_r < pane.rows() && cur_c < pane.cols() {
        let cell = pane.cell(cur_r, cur_c);
        let x = base_x + cur_c * FONT_WIDTH;
        let y = base_y + cur_r * FONT_HEIGHT;
        render::render_char(target, x, y, cell.ch, cell.bg, cell.fg);
    }

    pane.update_prev_cursor();
}

/// Render separator lines between split panes.
pub fn render_separators<D: DrawTarget>(
    node: &terminal_core::LayoutNode,
    target: &mut D,
) {
    match node {
        terminal_core::LayoutNode::Leaf { .. } => {}
        terminal_core::LayoutNode::Split { direction, first, second, .. } => {
            let vp1 = leaf_viewport(first);
            let color = terminal_core::Color::BRIGHT_BLACK;

            match direction {
                terminal_core::SplitDirection::Vertical => {
                    // Vertical line at right edge of first child.
                    let sx = (vp1.col + vp1.cols) as usize * FONT_WIDTH;
                    let sy = vp1.row as usize * FONT_HEIGHT;
                    let sh = vp1.rows as usize * FONT_HEIGHT;
                    render::fill_rect(target, sx, sy, 1 * FONT_WIDTH, sh, color);
                }
                terminal_core::SplitDirection::Horizontal => {
                    // Horizontal line at bottom edge of first child.
                    let sx = vp1.col as usize * FONT_WIDTH;
                    let sy = (vp1.row + vp1.rows) as usize * FONT_HEIGHT;
                    let sw = vp1.cols as usize * FONT_WIDTH;
                    render::fill_rect(target, sx, sy, sw, 1 * FONT_HEIGHT, color);
                }
            }

            render_separators(first, target);
            render_separators(second, target);
        }
    }
}

fn leaf_viewport(node: &terminal_core::LayoutNode) -> &terminal_core::CellViewport {
    match node {
        terminal_core::LayoutNode::Leaf { viewport, .. } => viewport,
        terminal_core::LayoutNode::Split { first, .. } => leaf_viewport(first),
    }
}
```

- [ ] **Step 6: Add terminal-fb to workspace members**

In root `Cargo.toml`, add `"crates/terminal-fb"` to `[workspace] members`.

- [ ] **Step 7: Build**

Run: `cargo build -p terminal-fb`
Expected: clean build.

- [ ] **Step 8: Commit**

```bash
git add crates/terminal-fb/ Cargo.toml
git commit -m "feat(terminal-fb): framebuffer renderer bridging terminal-core to pixel output"
```

---

### Task 9: Make crates/terminal/ a thin shim over terminal-core + terminal-fb

**Files:**
- Modify: `crates/terminal/Cargo.toml` — add terminal-core, terminal-fb deps
- Modify: `crates/terminal/src/lib.rs` — re-export from new crates
- Modify: `crates/terminal/src/pane.rs` — wrapper around terminal-core Pane
- Modify: `crates/terminal/src/layout.rs` — wrapper around terminal-core Layout
- Modify: `crates/terminal/src/render.rs` — re-export from terminal-fb

The goal is to make the old `claudio-terminal` crate a compatibility shim so the kernel can continue to `use claudio_terminal::*` without changes — temporarily. This keeps the kernel building while we prepare the dashboard migration in the next phase.

- [ ] **Step 1: Update crates/terminal/Cargo.toml**

Add terminal-core and terminal-fb as dependencies:

```toml
[package]
name = "claudio-terminal"
version.workspace = true
edition.workspace = true

[dependencies]
terminal-core = { path = "../terminal-core" }
terminal-fb = { path = "../terminal-fb" }
vte = { workspace = true }
log = { workspace = true }
spin = { workspace = true }
```

- [ ] **Step 2: Rewrite crates/terminal/src/lib.rs as a shim**

```rust
//! Split-pane terminal renderer — compatibility shim.
//!
//! This crate re-exports types from terminal-core and terminal-fb to
//! maintain backward compatibility with kernel code during the migration.
//! After the kernel is updated to import terminal-core and terminal-fb
//! directly, this crate can be removed.

#![no_std]
extern crate alloc;

pub mod layout;
pub mod pane;
pub mod render;
pub mod terminus {
    pub use terminal_fb::terminus::*;
}
pub mod unicode_font {
    pub use terminal_fb::unicode_font::*;
}

// Re-export the main public types (backward compat).
pub use layout::Layout;
pub use pane::{Cell, Pane};
pub use render::{fill_rect, render_char, Color, FONT_HEIGHT, FONT_WIDTH};

// Re-export DrawTarget from terminal-fb.
pub use terminal_fb::DrawTarget;

// Re-export Viewport (pixel-based, for backward compat).
pub use terminal_fb::render::pixels_to_cells;

/// A rectangular pixel region within the framebuffer.
/// Kept for backward compatibility — new code should use CellViewport.
#[derive(Debug, Clone)]
pub struct Viewport {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Node in the layout tree — backward compat wrapper.
#[derive(Debug)]
pub enum LayoutNode {
    Leaf { pane_id: usize, viewport: Viewport },
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: alloc::boxed::Box<LayoutNode>,
        second: alloc::boxed::Box<LayoutNode>,
    },
}

pub use terminal_core::SplitDirection;

/// High-level commands — re-export from core.
pub use terminal_core::DashboardCommand;
```

- [ ] **Step 3: Rewrite crates/terminal/src/pane.rs as a shim**

The shim wraps terminal-core's Pane and adds back the pixel-based render methods that the kernel currently calls. This is temporary — the kernel migration in Phase 3 will remove these calls.

```rust
//! Pane compatibility shim — wraps terminal-core Pane, adds pixel rendering.

use alloc::vec;
use alloc::vec::Vec;
use crate::Viewport;
use terminal_fb::render::{FONT_WIDTH, FONT_HEIGHT};

pub use terminal_core::Color;
pub use terminal_core::Cell;

/// Backward-compatible pane wrapping terminal-core::Pane.
pub struct Pane {
    pub inner: terminal_core::Pane,
    /// Pixel viewport (kept for backward compat with kernel rendering).
    pub viewport: Viewport,
}

impl Pane {
    pub fn new(id: usize, viewport: Viewport) -> Self {
        let cols = (viewport.width / FONT_WIDTH).max(1) as u16;
        let rows = (viewport.height / FONT_HEIGHT).max(1) as u16;
        let cell_vp = terminal_core::CellViewport::new(
            (viewport.x / FONT_WIDTH) as u16,
            (viewport.y / FONT_HEIGHT) as u16,
            cols,
            rows,
        );
        Self {
            inner: terminal_core::Pane::new(id as u64, cell_vp),
            viewport,
        }
    }

    pub fn id(&self) -> usize { self.inner.id as usize }
    pub fn cols(&self) -> usize { self.inner.cols() }
    pub fn rows(&self) -> usize { self.inner.rows() }
    pub fn is_dirty(&self) -> bool { self.inner.is_dirty() }
    pub fn clear_dirty(&mut self) { self.inner.clear_dirty() }
    pub fn dirty_rows(&self) -> &[bool] { self.inner.dirty_rows() }

    pub fn write_bytes(&mut self, bytes: &[u8]) { self.inner.write_bytes(bytes) }
    pub fn write_str(&mut self, s: &str) { self.inner.write_str(s) }

    pub fn resize(&mut self, viewport: Viewport) {
        let cols = (viewport.width / FONT_WIDTH).max(1) as u16;
        let rows = (viewport.height / FONT_HEIGHT).max(1) as u16;
        let cell_vp = terminal_core::CellViewport::new(
            (viewport.x / FONT_WIDTH) as u16,
            (viewport.y / FONT_HEIGHT) as u16,
            cols,
            rows,
        );
        self.inner.resize(cell_vp);
        self.viewport = viewport;
    }

    // Pixel-based render methods (backward compat).
    pub fn render<D: terminal_fb::DrawTarget>(&self, target: &mut D) {
        terminal_fb::pane_renderer::render_pane(&self.inner, target);
    }

    pub fn render_dirty<D: terminal_fb::DrawTarget>(&self, target: &mut D) {
        terminal_fb::pane_renderer::render_pane_dirty(&self.inner, target);
    }

    pub fn render_cursor_delta<D: terminal_fb::DrawTarget>(&mut self, target: &mut D) {
        terminal_fb::pane_renderer::render_cursor_delta(&mut self.inner, target);
    }

    pub fn render_cursor<D: terminal_fb::DrawTarget>(&self, target: &mut D) {
        let vp = &self.inner.viewport;
        let (cur_r, cur_c) = self.inner.cursor_pos();
        if cur_r < self.inner.rows() && cur_c < self.inner.cols() {
            let cell = self.inner.cell(cur_r, cur_c);
            let x = vp.col as usize * FONT_WIDTH + cur_c * FONT_WIDTH;
            let y = vp.row as usize * FONT_HEIGHT + cur_r * FONT_HEIGHT;
            terminal_fb::render_char(target, x, y, cell.ch, cell.bg, cell.fg);
        }
    }
}
```

- [ ] **Step 4: Rewrite crates/terminal/src/layout.rs as a shim**

The shim wraps terminal-core's Layout but presents a pixel-based API that the kernel currently expects.

```rust
//! Layout compatibility shim — wraps terminal-core Layout with pixel API.

use alloc::vec::Vec;
use crate::pane::Pane;
use crate::Viewport;
use terminal_core::SplitDirection;
use terminal_fb::render::{FONT_WIDTH, FONT_HEIGHT};

/// Backward-compatible Layout wrapping terminal-core::Layout.
pub struct Layout {
    pub inner: terminal_core::Layout,
    /// Shim panes with pixel viewports.
    panes: Vec<Pane>,
    focused_idx: usize,
}

impl Layout {
    /// Create from pixel dimensions (backward compat).
    pub fn new(screen_width: usize, screen_height: usize) -> Self {
        let (cols, rows) = terminal_fb::pixels_to_cells(screen_width, screen_height);
        let inner = terminal_core::Layout::new(cols, rows);
        // Create initial shim pane.
        let viewport = Viewport { x: 0, y: 0, width: screen_width, height: screen_height };
        let pane = Pane::new(0, viewport);
        Self {
            inner,
            panes: vec![pane],
            focused_idx: 0,
        }
    }

    pub fn focused_pane(&self) -> &Pane { &self.panes[self.focused_idx] }
    pub fn focused_pane_mut(&mut self) -> &mut Pane { &mut self.panes[self.focused_idx] }
    pub fn pane_count(&self) -> usize { self.inner.pane_count() }
    pub fn panes(&self) -> &[Pane] { &self.panes }

    pub fn focused_pane_id(&self) -> usize {
        self.inner.focused_pane_id() as usize
    }

    pub fn pane_by_id_mut(&mut self, id: usize) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.inner.id == id as u64)
    }

    pub fn focus_next(&mut self) {
        self.inner.focus_next();
        self.sync_focus();
    }

    pub fn focus_prev(&mut self) {
        self.inner.focus_prev();
        self.sync_focus();
    }

    pub fn split(&mut self, direction: SplitDirection) {
        self.inner.split(direction);
        self.rebuild_shim_panes();
    }

    pub fn close_focused(&mut self) {
        self.inner.close_focused();
        self.rebuild_shim_panes();
    }

    pub fn render_all<D: terminal_fb::DrawTarget>(&self, target: &mut D) {
        for pane in &self.panes {
            pane.render(target);
        }
        terminal_fb::pane_renderer::render_separators(self.inner.root(), target);
    }

    pub fn render_dirty<D: terminal_fb::DrawTarget>(&mut self, target: &mut D) -> bool {
        let mut any = false;
        for (i, pane) in self.panes.iter_mut().enumerate() {
            if pane.is_dirty() {
                pane.render_dirty(target);
                if i == self.focused_idx {
                    pane.render_cursor_delta(target);
                }
                pane.clear_dirty();
                any = true;
            }
        }
        any
    }

    pub fn render_all_and_clear<D: terminal_fb::DrawTarget>(&mut self, target: &mut D) {
        for (i, pane) in self.panes.iter_mut().enumerate() {
            pane.render(target);
            if i == self.focused_idx {
                pane.render_cursor_delta(target);
            }
            pane.clear_dirty();
        }
        terminal_fb::pane_renderer::render_separators(self.inner.root(), target);
    }

    // -- internal -----------------------------------------------------------

    fn rebuild_shim_panes(&mut self) {
        let core_panes = self.inner.panes();
        let mut new_panes = Vec::with_capacity(core_panes.len());
        for cp in core_panes {
            // Reuse existing shim pane if it exists.
            let existing = self.panes.iter()
                .position(|p| p.inner.id == cp.id);
            if let Some(idx) = existing {
                let mut shim = self.panes.swap_remove(idx);
                // Update pixel viewport from cell viewport.
                shim.viewport = cell_to_pixel_viewport(&cp.viewport);
                new_panes.push(shim);
            } else {
                let vp = cell_to_pixel_viewport(&cp.viewport);
                new_panes.push(Pane::new(cp.id as usize, vp));
            }
        }
        self.panes = new_panes;
        self.sync_focus();
    }

    fn sync_focus(&mut self) {
        let focused_id = self.inner.focused_pane_id();
        self.focused_idx = self.panes.iter()
            .position(|p| p.inner.id == focused_id)
            .unwrap_or(0);
    }
}

fn cell_to_pixel_viewport(cv: &terminal_core::CellViewport) -> Viewport {
    Viewport {
        x: cv.col as usize * FONT_WIDTH,
        y: cv.row as usize * FONT_HEIGHT,
        width: cv.cols as usize * FONT_WIDTH,
        height: cv.rows as usize * FONT_HEIGHT,
    }
}
```

- [ ] **Step 5: Rewrite crates/terminal/src/render.rs as re-exports**

```rust
//! Rendering compatibility shim — re-exports from terminal-fb.

pub use terminal_core::Color;
pub use terminal_fb::render::{FONT_HEIGHT, FONT_WIDTH};
pub use terminal_fb::{render_char, fill_rect, DrawTarget};
```

- [ ] **Step 6: Build the full workspace to verify backward compat**

Run: `cargo build` (full workspace)
Expected: kernel and all crates build clean. The shim layer preserves the exact public API that `kernel/src/dashboard.rs` uses.

- [ ] **Step 7: Commit**

```bash
git add crates/terminal/ crates/terminal-fb/ Cargo.toml
git commit -m "refactor(terminal): rewrite as shim over terminal-core + terminal-fb"
```

---

### Task 10: Run full kernel build + QEMU smoke test

- [ ] **Step 1: Full build**

Run: `cargo build`
Expected: clean workspace build.

- [ ] **Step 2: QEMU smoke test (if available)**

Run the QEMU boot sequence from CLAUDE.md. Verify:
- Dashboard boots
- Ctrl+B " splits horizontal
- Ctrl+B n cycles focus
- Ctrl+B c spawns agent
- Ctrl+B x closes pane
- Visual rendering matches previous behavior

If QEMU is not available on this Windows machine, verify with `cargo build` only and note the QEMU test is deferred.

- [ ] **Step 3: Commit any fixups**

Only if needed.

---

## Phase 3: Kernel Dashboard Migration (Tasks 11–12)

### Task 11: Replace PrefixState with InputRouter in dashboard.rs

**Files:**
- Modify: `kernel/Cargo.toml` — add terminal-core dep
- Modify: `kernel/src/dashboard.rs`

This is the key migration step. We replace the hand-rolled prefix state machine in `dashboard.rs` (lines 691-696 PrefixState, lines 944-969 keyboard dispatch, lines 1199-1411 handle_prefix_command) with terminal-core's `InputRouter`.

- [ ] **Step 1: Add terminal-core to kernel/Cargo.toml**

Add: `terminal-core = { path = "../crates/terminal-core" }`

- [ ] **Step 2: Import InputRouter in dashboard.rs**

At the top of `kernel/src/dashboard.rs`, add:
```rust
use terminal_core::{InputRouter, RouterOutcome, DashboardCommand as CoreCommand, KeyEvent as CoreKeyEvent, KeyCode as CoreKeyCode, Modifiers as CoreModifiers};
```

- [ ] **Step 3: Replace PrefixState enum with InputRouter**

Delete the `PrefixState` enum (lines 691-696).

In `run_dashboard`, replace:
```rust
let mut prefix_state = PrefixState::Normal;
```
with:
```rust
let mut router = InputRouter::new();
```

- [ ] **Step 4: Create key conversion function**

Add a helper to convert `pc_keyboard::DecodedKey` to terminal-core's `KeyEvent`:

```rust
fn to_core_key_event(c: char) -> CoreKeyEvent {
    // pc-keyboard with HandleControl::MapLettersToUnicode delivers
    // Ctrl+letter as Unicode control codes (e.g., Ctrl+B = 0x02).
    if c <= '\x1a' && c != '\n' && c != '\r' && c != '\t' && c != '\x08' {
        // Control code: map back to the letter + CTRL modifier.
        let letter = (c as u8 + b'a' - 1) as char;
        CoreKeyEvent::ctrl(letter)
    } else {
        CoreKeyEvent::plain(CoreKeyCode::Char(c))
    }
}
```

- [ ] **Step 5: Replace the keyboard dispatch block**

Replace the `match prefix_state { ... }` block (lines 943-969) with:

```rust
DecodedKey::Unicode(c) => {
    // Clipboard shortcuts bypass the router (modifier tracking via scancode).
    if crate::vconsole::shift_held() {
        // ... existing clipboard handling (lines 974-1032, unchanged) ...
        continue; // only if a clipboard shortcut was handled
    }

    let core_key = to_core_key_event(c);
    match router.handle_key(core_key) {
        RouterOutcome::Command(cmd) => {
            apply_command(
                cmd,
                &mut layout,
                &mut dashboard,
                &mut pane_types,
                &mut input_buffers,
                &mut next_shell_id,
                stack_send.0,
                now,
            );
            let focused_pane_id = layout.focused_pane_id();
            render_prompt_for_pane(&mut layout, &pane_types, &input_buffers, focused_pane_id, &dashboard);
            render_full(&mut layout);
            continue;
        }
        RouterOutcome::ForwardToPane => {
            // ... existing key-to-pane routing (browser, file manager, etc.) ...
            // ... existing Enter/Backspace/char handling ...
        }
        RouterOutcome::Swallow => {
            continue;
        }
    }
}
```

- [ ] **Step 6: Rename handle_prefix_command to apply_command**

Rename the function and change signature from `c: char` to `cmd: CoreCommand`:

```rust
fn apply_command(
    cmd: CoreCommand,
    layout: &mut Layout,
    dashboard: &mut Dashboard,
    pane_types: &mut Vec<PaneType>,
    input_buffers: &mut Vec<InputBuffer>,
    next_shell_id: &mut usize,
    stack_ptr: *mut NetworkStack,
    now: fn() -> Instant,
) {
    match cmd {
        CoreCommand::SplitHorizontal => {
            // ... existing '"' arm body (lines 1211-1224), unchanged ...
        }
        CoreCommand::SplitVertical => {
            // ... existing '%' arm body (lines 1227-1240), unchanged ...
        }
        CoreCommand::FocusNext => {
            layout.focus_next();
        }
        CoreCommand::FocusPrev => {
            layout.focus_prev();
        }
        CoreCommand::SpawnAgent => {
            // ... existing 'c' arm body (lines 1255-1275), unchanged ...
        }
        CoreCommand::SpawnShell => {
            // ... existing 's' arm body (lines 1278-1292), unchanged ...
        }
        CoreCommand::ClosePane => {
            // ... existing 'x' arm body (lines 1295-1330), unchanged ...
        }
        CoreCommand::OpenFiles => {
            // ... existing 'f' arm body (lines 1349-1363), unchanged ...
        }
        CoreCommand::OpenBrowser => {
            // Handle 'w' browser pane similarly to 'f'
        }
        CoreCommand::ToggleStatusBar => {
            log::info!("[dashboard] toggle status bar (TODO)");
        }
        CoreCommand::NextLayout | CoreCommand::PreviousLayout => {
            log::info!("[dashboard] layout switch (TODO)");
        }
        CoreCommand::Quit => {
            log::info!("[dashboard] quit requested");
            // Could set a flag to break the loop, but ClaudioOS doesn't have a quit path yet.
        }
    }
}
```

The key insight: per-arm bodies are a copy-paste of the existing per-`char` bodies. Only the match discriminant changes. No cleanup inside arms.

- [ ] **Step 7: Build**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 8: Commit**

```bash
git add kernel/Cargo.toml kernel/src/dashboard.rs
git commit -m "refactor(dashboard): replace PrefixState with terminal-core InputRouter"
```

---

### Task 12: QEMU smoke test after migration

- [ ] **Step 1: Build and run QEMU**

Same as Task 10 Step 2. Verify all prefix commands work identically to before.

- [ ] **Step 2: Commit any fixups**

---

## Phase 4: terminal-ansi (Tasks 13–14)

### Task 13: Scaffold terminal-ansi crate with AnsiRenderer

**Files:**
- Create: `crates/terminal-ansi/Cargo.toml`
- Create: `crates/terminal-ansi/src/lib.rs`
- Create: `crates/terminal-ansi/src/diff.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "terminal-ansi"
version.workspace = true
edition.workspace = true

[dependencies]
terminal-core = { path = "../terminal-core" }
log = "0.4"
```

Note: this is `std` (no `no_std`). crossterm is NOT a dependency of the renderer — the binary owns the host terminal. The renderer only emits ANSI byte sequences.

- [ ] **Step 2: Write diff tests**

Create `crates/terminal-ansi/src/diff.rs`:

```rust
//! Cell-by-cell diff engine for minimal ANSI output.

use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as FmtWrite;
use terminal_core::{Cell, Color};

/// Compute the ANSI byte sequence to transition from `prev` to `next` frame.
/// Returns the bytes to write to stdout.
///
/// Only emits sequences for cells that changed. Unchanged cells produce zero bytes.
pub fn diff_frames(
    prev: &[Cell],
    next: &[Cell],
    cols: u16,
    rows: u16,
    buf: &mut Vec<u8>,
) {
    let mut last_fg = Color::DEFAULT_FG;
    let mut last_bg = Color::DEFAULT_BG;
    let mut last_row: u16 = u16::MAX;
    let mut last_col: u16 = u16::MAX;

    for row in 0..rows {
        for col in 0..cols {
            let idx = (row as usize) * (cols as usize) + (col as usize);
            let prev_cell = prev.get(idx).copied().unwrap_or_default();
            let next_cell = next.get(idx).copied().unwrap_or_default();

            if prev_cell == next_cell {
                continue;
            }

            // Position cursor if not already there.
            if row != last_row || col != last_col {
                // CSI row;col H (1-based).
                write_csi_cup(buf, row + 1, col + 1);
            }

            // Emit SGR if color changed.
            if next_cell.fg != last_fg || next_cell.bg != last_bg {
                write_sgr(buf, next_cell.fg, next_cell.bg);
                last_fg = next_cell.fg;
                last_bg = next_cell.bg;
            }

            buf.push(next_cell.ch as u8);
            last_row = row;
            last_col = col + 1; // cursor advances after write
        }
    }
}

fn write_csi_cup(buf: &mut Vec<u8>, row: u16, col: u16) {
    // ESC [ row ; col H
    buf.extend_from_slice(b"\x1b[");
    write_u16(buf, row);
    buf.push(b';');
    write_u16(buf, col);
    buf.push(b'H');
}

fn write_sgr(buf: &mut Vec<u8>, fg: Color, bg: Color) {
    // ESC [ 38;2;r;g;b;48;2;r;g;b m (truecolor)
    buf.extend_from_slice(b"\x1b[38;2;");
    write_u8(buf, fg.r); buf.push(b';');
    write_u8(buf, fg.g); buf.push(b';');
    write_u8(buf, fg.b);
    buf.extend_from_slice(b";48;2;");
    write_u8(buf, bg.r); buf.push(b';');
    write_u8(buf, bg.g); buf.push(b';');
    write_u8(buf, bg.b);
    buf.push(b'm');
}

fn write_u16(buf: &mut Vec<u8>, n: u16) {
    let mut tmp = [0u8; 5];
    let s = format_u16(n, &mut tmp);
    buf.extend_from_slice(s);
}

fn write_u8(buf: &mut Vec<u8>, n: u8) {
    let mut tmp = [0u8; 3];
    let s = format_u8(n, &mut tmp);
    buf.extend_from_slice(s);
}

fn format_u16(mut n: u16, buf: &mut [u8; 5]) -> &[u8] {
    if n == 0 { return b"0"; }
    let mut i = 5;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    &buf[i..]
}

fn format_u8(mut n: u8, buf: &mut [u8; 3]) -> &[u8] {
    if n == 0 { return b"0"; }
    let mut i = 3;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10);
        n /= 10;
    }
    &buf[i..]
}

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
        // Should contain cursor positioning to (1,2) and the char 'X'.
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
```

- [ ] **Step 3: Create src/lib.rs**

```rust
//! terminal-ansi — ANSI escape sequence renderer for terminal-core.
//!
//! Produces minimal ANSI byte output by diffing frames cell-by-cell.
//! Used by claudio-mux to render to a host terminal (Windows Terminal).

extern crate alloc;

pub mod diff;

use alloc::vec;
use alloc::vec::Vec;
use terminal_core::{Cell, Layout, PaneId, CellViewport};

/// Rendering context assembled by the binary each frame.
pub struct Scene<'a> {
    pub layout: &'a Layout,
    pub focused: PaneId,
    pub status_line: Option<&'a str>,
}

/// Diff-based ANSI renderer.
pub struct AnsiRenderer {
    prev_frame: Vec<Cell>,
    cols: u16,
    rows: u16,
}

impl AnsiRenderer {
    pub fn new(cols: u16, rows: u16) -> Self {
        let size = cols as usize * rows as usize;
        Self {
            prev_frame: vec![Cell::default(); size],
            cols,
            rows,
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.prev_frame = vec![Cell::default(); cols as usize * rows as usize];
    }

    /// Render a scene, returning the ANSI bytes to write to stdout.
    pub fn render(&mut self, scene: &Scene) -> Vec<u8> {
        let mut next_frame = vec![Cell::default(); self.cols as usize * self.rows as usize];

        // Compose pane grids into the frame.
        for pane in scene.layout.panes() {
            let vp = &pane.viewport;
            for row in 0..pane.rows() {
                for col in 0..pane.cols() {
                    let screen_row = vp.row as usize + row;
                    let screen_col = vp.col as usize + col;
                    if screen_row < self.rows as usize && screen_col < self.cols as usize {
                        let idx = screen_row * self.cols as usize + screen_col;
                        next_frame[idx] = *pane.cell(row, col);
                    }
                }
            }
        }

        // Draw separators (single characters).
        self.draw_separators(scene.layout.root(), &mut next_frame);

        // Draw status line on bottom row if present.
        if let Some(status) = scene.status_line {
            let bottom = (self.rows as usize).saturating_sub(1);
            for (i, ch) in status.chars().enumerate() {
                if i < self.cols as usize {
                    let idx = bottom * self.cols as usize + i;
                    next_frame[idx] = Cell {
                        ch,
                        fg: terminal_core::Color::BLACK,
                        bg: terminal_core::Color::WHITE,
                    };
                }
            }
        }

        // Diff and emit.
        let mut buf = Vec::new();
        diff::diff_frames(&self.prev_frame, &next_frame, self.cols, self.rows, &mut buf);

        // Place hardware cursor in focused pane.
        if let Some(pane) = scene.layout.panes().iter().find(|p| p.id == scene.focused) {
            let (cr, cc) = pane.cursor_pos();
            let screen_row = pane.viewport.row as usize + cr + 1; // 1-based
            let screen_col = pane.viewport.col as usize + cc + 1;
            buf.extend_from_slice(b"\x1b[");
            buf.extend_from_slice(screen_row.to_string().as_bytes());
            buf.push(b';');
            buf.extend_from_slice(screen_col.to_string().as_bytes());
            buf.push(b'H');
        }

        self.prev_frame = next_frame;
        buf
    }

    fn draw_separators(&self, node: &terminal_core::LayoutNode, frame: &mut [Cell]) {
        match node {
            terminal_core::LayoutNode::Leaf { .. } => {}
            terminal_core::LayoutNode::Split { direction, first, second, .. } => {
                let vp = first_leaf_viewport(first);
                let sep_cell = Cell {
                    ch: match direction {
                        terminal_core::SplitDirection::Vertical => '│',
                        terminal_core::SplitDirection::Horizontal => '─',
                    },
                    fg: terminal_core::Color::BRIGHT_BLACK,
                    bg: terminal_core::Color::DEFAULT_BG,
                };

                match direction {
                    terminal_core::SplitDirection::Vertical => {
                        let col = (vp.col + vp.cols) as usize;
                        for row in vp.row as usize..(vp.row + vp.rows) as usize {
                            if row < self.rows as usize && col < self.cols as usize {
                                frame[row * self.cols as usize + col] = sep_cell;
                            }
                        }
                    }
                    terminal_core::SplitDirection::Horizontal => {
                        let row = (vp.row + vp.rows) as usize;
                        for col in vp.col as usize..(vp.col + vp.cols) as usize {
                            if row < self.rows as usize && col < self.cols as usize {
                                frame[row * self.cols as usize + col] = sep_cell;
                            }
                        }
                    }
                }

                self.draw_separators(first, frame);
                self.draw_separators(second, frame);
            }
        }
    }
}

fn first_leaf_viewport(node: &terminal_core::LayoutNode) -> &CellViewport {
    match node {
        terminal_core::LayoutNode::Leaf { viewport, .. } => viewport,
        terminal_core::LayoutNode::Split { first, .. } => first_leaf_viewport(first),
    }
}
```

- [ ] **Step 4: Add terminal-ansi to workspace**

Add `"crates/terminal-ansi"` to workspace members in root `Cargo.toml`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p terminal-ansi`
Expected: 4 diff tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/terminal-ansi/ Cargo.toml
git commit -m "feat(terminal-ansi): ANSI diff renderer with 4 tests"
```

---

### Task 14: Verify all crates build together

- [ ] **Step 1: Full workspace build**

Run: `cargo build`
Expected: all crates build clean.

- [ ] **Step 2: Full test suite**

Run: `cargo test`
Expected: all tests pass across terminal-core, terminal-ansi, and existing crates.

- [ ] **Step 3: Commit any fixups**

---

## Phase 5: claudio-mux binary (Tasks 15–19)

### Task 15: Scaffold claudio-mux binary with CLI and config

**Files:**
- Create: `tools/claudio-mux/Cargo.toml`
- Create: `tools/claudio-mux/src/main.rs`
- Create: `tools/claudio-mux/src/cli.rs`
- Create: `tools/claudio-mux/src/config.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "claudio-mux"
version.workspace = true
edition.workspace = true

[[bin]]
name = "claudio-mux"
path = "src/main.rs"

[dependencies]
terminal-core = { path = "../../crates/terminal-core" }
terminal-ansi = { path = "../../crates/terminal-ansi" }

tokio        = { version = "1", features = ["rt-multi-thread", "macros", "sync", "io-util", "signal"] }
crossterm    = "0.28"
portable-pty = "0.9"
bytes        = "1"
tracing              = "0.1"
tracing-subscriber   = { version = "0.3", features = ["fmt", "env-filter"] }
tracing-appender     = "0.2"
serde        = { version = "1", features = ["derive"] }
toml         = "0.8"
directories  = "5"
anyhow       = "1"
clap         = { version = "4", features = ["derive"] }
```

- [ ] **Step 2: Create src/cli.rs**

```rust
use clap::Parser;

/// claudio-mux — terminal multiplexer for Windows
#[derive(Parser, Debug)]
#[command(name = "claudio-mux", version, about)]
pub struct Cli {
    /// Named layout to load from config
    #[arg(short, long)]
    pub layout: Option<String>,

    /// Session name
    #[arg(short, long, default_value = "main")]
    pub session: String,
}
```

- [ ] **Step 3: Create src/config.rs**

```rust
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub keybindings: KeybindingsConfig,
    pub status_bar: StatusBarConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub shell: String,
    pub shell_args: Vec<String>,
    pub agent: String,
    pub agent_args: Vec<String>,
    pub require_windows_terminal: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct KeybindingsConfig {
    pub prefix: String,
    pub bindings: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct StatusBarConfig {
    pub enabled: bool,
    pub left: String,
    pub right: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            keybindings: KeybindingsConfig::default(),
            status_bar: StatusBarConfig::default(),
        }
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            shell: "pwsh.exe".into(),
            shell_args: vec!["-NoLogo".into()],
            agent: "claude".into(),
            agent_args: vec![],
            require_windows_terminal: true,
        }
    }
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            prefix: "Ctrl+b".into(),
            bindings: HashMap::new(),
        }
    }
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            left: " {session} │ panes:{pane_count} ".into(),
            right: " {focus} │ {time} ".into(),
        }
    }
}

/// Load config from standard path, or return defaults.
pub fn load_config() -> anyhow::Result<Config> {
    let config_dir = config_dir();
    let config_path = config_dir.join("config.toml");
    if config_path.exists() {
        let text = std::fs::read_to_string(&config_path)?;
        let config: Config = toml::from_str(&text)?;
        Ok(config)
    } else {
        Ok(Config::default())
    }
}

pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("", "ridge-cell", "claudio-mux")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn log_dir() -> PathBuf {
    directories::ProjectDirs::from("", "ridge-cell", "claudio-mux")
        .map(|d| d.data_local_dir().join("logs"))
        .unwrap_or_else(|| PathBuf::from("logs"))
}
```

- [ ] **Step 4: Create src/main.rs**

```rust
mod cli;
mod config;

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    let config = config::load_config()?;

    // Check Windows Terminal requirement.
    if config.general.require_windows_terminal {
        if std::env::var("WT_SESSION").is_err() {
            anyhow::bail!(
                "claudio-mux requires Windows Terminal (WT_SESSION not found).\n\
                 Set general.require_windows_terminal = false in config to override."
            );
        }
    }

    // Init tracing (file only, never stdout).
    let log_dir = config::log_dir();
    std::fs::create_dir_all(&log_dir)?;
    let file_appender = tracing_appender::rolling::daily(&log_dir, "claudio-mux.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            EnvFilter::try_from_env("CLAUDIO_MUX_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info"))
        )
        .init();

    tracing::info!("claudio-mux starting, session={}", cli.session);

    // TODO: Tasks 16-19 wire up the runtime.
    println!("claudio-mux v0.1 — session: {}", cli.session);
    println!("Runtime not yet wired. Use --help for options.");

    Ok(())
}
```

- [ ] **Step 5: Add to workspace**

In root `Cargo.toml`, add `"tools/claudio-mux"` to workspace members. Also add it to the `exclude` list for the `x86_64-unknown-none` target builds (since it's a std binary) if the workspace has target-specific filtering, OR add `default-members` that excludes it from kernel builds.

- [ ] **Step 6: Build**

Run: `cargo build -p claudio-mux`
Expected: compiles, binary at `target/debug/claudio-mux.exe`.

- [ ] **Step 7: Test CLI**

Run: `cargo run -p claudio-mux -- --help`
Expected: shows usage with `--layout` and `--session` flags.

- [ ] **Step 8: Commit**

```bash
git add tools/claudio-mux/ Cargo.toml
git commit -m "feat(claudio-mux): scaffold binary with CLI, config, and tracing"
```

---

### Task 16: Add ConPTY spawning and host terminal

**Files:**
- Create: `tools/claudio-mux/src/conpty.rs`
- Create: `tools/claudio-mux/src/host.rs`

- [ ] **Step 1: Create src/conpty.rs**

```rust
use anyhow::Result;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize, Child};
use std::io::Write;

pub struct PtyHandle {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub writer: Box<dyn Write + Send>,
}

pub fn spawn_shell(cols: u16, rows: u16, shell: &str, args: &[String]) -> Result<PtyHandle> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(shell);
    for arg in args {
        cmd.arg(arg);
    }
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let writer = pair.master.take_writer()?;
    Ok(PtyHandle { master: pair.master, child, writer })
}

pub fn spawn_agent(cols: u16, rows: u16, agent: &str, args: &[String]) -> Result<PtyHandle> {
    spawn_shell(cols, rows, agent, args)
}
```

- [ ] **Step 2: Create src/host.rs**

```rust
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyEvent as CtKeyEvent, KeyCode as CtKeyCode, KeyModifiers},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use std::io::{self, Write};
use terminal_core::{KeyEvent, KeyCode, Modifiers};
use tokio::sync::mpsc;

/// RAII guard for raw mode + alternate screen.
pub struct Host {
    stdout: io::Stdout,
}

impl Host {
    pub fn new() -> Result<Self> {
        let mut stdout = io::stdout();
        terminal::enable_raw_mode()?;
        stdout.execute(EnterAlternateScreen)?;
        stdout.execute(crossterm::cursor::Hide)?;
        Ok(Self { stdout })
    }

    pub fn size() -> Result<(u16, u16)> {
        let (cols, rows) = terminal::size()?;
        Ok((cols, rows))
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stdout.write_all(bytes)?;
        self.stdout.flush()
    }

    /// Spawn a background thread that reads crossterm events and sends them
    /// through channels.
    pub fn spawn_input_reader(
        key_tx: mpsc::Sender<KeyEvent>,
        resize_tx: mpsc::Sender<(u16, u16)>,
    ) {
        std::thread::spawn(move || {
            loop {
                match event::read() {
                    Ok(Event::Key(ct_key)) => {
                        if let Some(key) = convert_key(ct_key) {
                            if key_tx.blocking_send(key).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(Event::Resize(cols, rows)) => {
                        if resize_tx.blocking_send((cols, rows)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {} // Mouse, paste, etc. — ignored in v1.
                    Err(_) => break,
                }
            }
        });
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = crossterm::cursor::Show;
        let _ = self.stdout.execute(LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

/// Convert crossterm key to terminal-core key.
fn convert_key(ct: CtKeyEvent) -> Option<KeyEvent> {
    let mut mods = Modifiers::empty();
    if ct.modifiers.contains(KeyModifiers::CONTROL) { mods |= Modifiers::CTRL; }
    if ct.modifiers.contains(KeyModifiers::SHIFT) { mods |= Modifiers::SHIFT; }
    if ct.modifiers.contains(KeyModifiers::ALT) { mods |= Modifiers::ALT; }

    let code = match ct.code {
        CtKeyCode::Char(c) => KeyCode::Char(c),
        CtKeyCode::Enter => KeyCode::Enter,
        CtKeyCode::Tab => KeyCode::Tab,
        CtKeyCode::Backspace => KeyCode::Backspace,
        CtKeyCode::Esc => KeyCode::Esc,
        CtKeyCode::Up => KeyCode::Up,
        CtKeyCode::Down => KeyCode::Down,
        CtKeyCode::Left => KeyCode::Left,
        CtKeyCode::Right => KeyCode::Right,
        CtKeyCode::Home => KeyCode::Home,
        CtKeyCode::End => KeyCode::End,
        CtKeyCode::PageUp => KeyCode::PageUp,
        CtKeyCode::PageDown => KeyCode::PageDown,
        CtKeyCode::Delete => KeyCode::Delete,
        CtKeyCode::Insert => KeyCode::Insert,
        CtKeyCode::F(n) => KeyCode::F(n),
        _ => return None,
    };

    Some(KeyEvent { code, mods })
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p claudio-mux`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add tools/claudio-mux/src/conpty.rs tools/claudio-mux/src/host.rs
git commit -m "feat(claudio-mux): add ConPTY spawning and host terminal RAII"
```

---

### Task 17: Add Session and PaneState

**Files:**
- Create: `tools/claudio-mux/src/session.rs`
- Create: `tools/claudio-mux/src/pane_state.rs`

- [ ] **Step 1: Create src/pane_state.rs**

```rust
use crate::conpty::PtyHandle;
use terminal_core::PaneId;

pub enum PaneKind {
    Shell,
    Agent,
}

pub struct PaneState {
    pub id: PaneId,
    pub kind: PaneKind,
    pub pty: PtyHandle,
    pub exited: bool,
}
```

- [ ] **Step 2: Create src/session.rs**

```rust
use anyhow::Result;
use terminal_core::{Layout, InputRouter, RouterOutcome, DashboardCommand, SplitDirection, PaneId};
use tokio::sync::mpsc;
use crate::config::Config;
use crate::conpty;
use crate::pane_state::{PaneState, PaneKind};

pub enum PtyEvent {
    Output { pane_id: PaneId, bytes: Vec<u8> },
    Exited { pane_id: PaneId },
}

pub struct Session {
    pub layout: Layout,
    pub router: InputRouter,
    pub pane_states: Vec<PaneState>,
    pub session_name: String,
    config: Config,
}

impl Session {
    pub fn new(cols: u16, rows: u16, config: Config, session_name: String, pty_tx: &mpsc::Sender<PtyEvent>) -> Result<Self> {
        let status_rows = if config.status_bar.enabled { 1u16 } else { 0u16 };
        let layout = Layout::new(cols, rows.saturating_sub(status_rows));

        let mut session = Self {
            layout,
            router: InputRouter::new(),
            pane_states: Vec::new(),
            session_name,
            config,
        };

        // Spawn initial shell in the first pane.
        let first_id = session.layout.focused_pane_id();
        let pane = session.layout.focused_pane();
        let pty = conpty::spawn_shell(
            pane.viewport.cols,
            pane.viewport.rows,
            &session.config.general.shell,
            &session.config.general.shell_args,
        )?;

        Self::start_pty_reader(first_id, &pty, pty_tx.clone());

        session.pane_states.push(PaneState {
            id: first_id,
            kind: PaneKind::Shell,
            pty,
            exited: false,
        });

        Ok(session)
    }

    pub async fn apply_command(&mut self, cmd: DashboardCommand, pty_tx: &mpsc::Sender<PtyEvent>) -> Result<()> {
        match cmd {
            DashboardCommand::SplitHorizontal => self.do_split(SplitDirection::Horizontal, PaneKind::Shell, pty_tx)?,
            DashboardCommand::SplitVertical => self.do_split(SplitDirection::Vertical, PaneKind::Shell, pty_tx)?,
            DashboardCommand::FocusNext => self.layout.focus_next(),
            DashboardCommand::FocusPrev => self.layout.focus_prev(),
            DashboardCommand::SpawnShell => self.do_split(SplitDirection::Horizontal, PaneKind::Shell, pty_tx)?,
            DashboardCommand::SpawnAgent => self.do_split(SplitDirection::Horizontal, PaneKind::Agent, pty_tx)?,
            DashboardCommand::ClosePane => self.close_focused(),
            DashboardCommand::Quit => {} // Handled by caller.
            DashboardCommand::OpenFiles | DashboardCommand::OpenBrowser => {
                tracing::info!("command not available on Windows");
            }
            DashboardCommand::ToggleStatusBar | DashboardCommand::NextLayout | DashboardCommand::PreviousLayout => {
                tracing::info!("command not yet implemented");
            }
        }
        Ok(())
    }

    pub fn feed_pane(&mut self, pane_id: PaneId, bytes: &[u8]) {
        if let Some(pane) = self.layout.pane_by_id_mut(pane_id) {
            pane.write_bytes(bytes);
        }
    }

    pub fn mark_pane_exited(&mut self, pane_id: PaneId) {
        if let Some(ps) = self.pane_states.iter_mut().find(|s| s.id == pane_id) {
            ps.exited = true;
        }
    }

    pub async fn forward_to_focused(&mut self, key: terminal_core::KeyEvent) -> Result<()> {
        let focused_id = self.layout.focused_pane_id();
        if let Some(ps) = self.pane_states.iter_mut().find(|s| s.id == focused_id) {
            let bytes = key_to_bytes(key);
            ps.pty.writer.write_all(&bytes)?;
        }
        Ok(())
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        let status_rows = if self.config.status_bar.enabled { 1u16 } else { 0u16 };
        self.layout.resize(cols, rows.saturating_sub(status_rows));
        // Resize ConPTYs.
        for ps in &self.pane_states {
            if let Some(pane) = self.layout.panes().iter().find(|p| p.id == ps.id) {
                let _ = ps.pty.master.resize(portable_pty::PtySize {
                    rows: pane.viewport.rows,
                    cols: pane.viewport.cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }
        Ok(())
    }

    pub fn pane_count(&self) -> usize { self.layout.pane_count() }

    // -- internal -----------------------------------------------------------

    fn do_split(&mut self, dir: SplitDirection, kind: PaneKind, pty_tx: &mpsc::Sender<PtyEvent>) -> Result<()> {
        self.layout.split(dir);
        let new_id = self.layout.focused_pane_id();
        let pane = self.layout.focused_pane();

        let pty = match kind {
            PaneKind::Shell => conpty::spawn_shell(
                pane.viewport.cols,
                pane.viewport.rows,
                &self.config.general.shell,
                &self.config.general.shell_args,
            )?,
            PaneKind::Agent => conpty::spawn_agent(
                pane.viewport.cols,
                pane.viewport.rows,
                &self.config.general.agent,
                &self.config.general.agent_args,
            )?,
        };

        Self::start_pty_reader(new_id, &pty, pty_tx.clone());

        self.pane_states.push(PaneState {
            id: new_id,
            kind,
            pty,
            exited: false,
        });

        Ok(())
    }

    fn close_focused(&mut self) {
        if self.layout.pane_count() <= 1 { return; }
        let id = self.layout.focused_pane_id();
        // Kill ConPTY child.
        if let Some(ps) = self.pane_states.iter_mut().find(|s| s.id == id) {
            let _ = ps.pty.child.kill();
        }
        self.pane_states.retain(|s| s.id != id);
        self.layout.close_focused();
    }

    fn start_pty_reader(pane_id: PaneId, pty: &PtyHandle, tx: mpsc::Sender<PtyEvent>) {
        let mut reader = pty.master.try_clone_reader().expect("clone pty reader");
        tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut reader, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = tx.blocking_send(PtyEvent::Output {
                            pane_id,
                            bytes: buf[..n].to_vec(),
                        });
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.blocking_send(PtyEvent::Exited { pane_id });
        });
    }
}

fn key_to_bytes(key: terminal_core::KeyEvent) -> Vec<u8> {
    use terminal_core::{KeyCode, Modifiers};
    match key.code {
        KeyCode::Char(c) => {
            if key.mods.contains(Modifiers::CTRL) {
                // Ctrl+letter → control code.
                let code = (c as u8).wrapping_sub(b'a').wrapping_add(1);
                vec![code]
            } else {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                s.as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            _ => vec![],
        },
        _ => vec![],
    }
}

use std::io::Write;
```

- [ ] **Step 3: Build**

Run: `cargo build -p claudio-mux`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add tools/claudio-mux/src/session.rs tools/claudio-mux/src/pane_state.rs
git commit -m "feat(claudio-mux): add Session (layout + ConPTY lifecycle) and PaneState"
```

---

### Task 18: Wire up the event loop

**Files:**
- Create: `tools/claudio-mux/src/app.rs`
- Create: `tools/claudio-mux/src/render.rs`
- Modify: `tools/claudio-mux/src/main.rs` — launch runtime

- [ ] **Step 1: Create src/render.rs**

```rust
use terminal_ansi::{AnsiRenderer, Scene};
use crate::session::Session;
use crate::host::Host;
use anyhow::Result;

pub fn flush(session: &Session, renderer: &mut AnsiRenderer, host: &mut Host) -> Result<()> {
    let status = if session.config().status_bar.enabled {
        Some(format_status(session))
    } else {
        None
    };

    let scene = Scene {
        layout: &session.layout,
        focused: session.layout.focused_pane_id(),
        status_line: status.as_deref(),
    };

    let bytes = renderer.render(&scene);
    if !bytes.is_empty() {
        host.write_all(&bytes)?;
    }
    Ok(())
}

fn format_status(session: &Session) -> String {
    format!(
        " {} │ panes:{} ",
        session.session_name,
        session.pane_count(),
    )
}
```

Add a `pub fn config(&self) -> &Config` accessor to `Session` in session.rs:
```rust
pub fn config(&self) -> &crate::config::Config { &self.config }
```

- [ ] **Step 2: Create src/app.rs**

```rust
use anyhow::Result;
use tokio::sync::mpsc;
use terminal_core::{RouterOutcome, DashboardCommand, KeyEvent};
use terminal_ansi::AnsiRenderer;
use crate::config::Config;
use crate::host::Host;
use crate::session::{Session, PtyEvent};
use crate::render;

pub async fn run(config: Config, session_name: String) -> Result<()> {
    let mut host = Host::new()?;
    let (cols, rows) = Host::size()?;

    let (pty_tx, mut pty_rx) = mpsc::channel::<PtyEvent>(256);
    let (key_tx, mut key_rx) = mpsc::channel::<KeyEvent>(64);
    let (resize_tx, mut resize_rx) = mpsc::channel::<(u16, u16)>(8);

    Host::spawn_input_reader(key_tx, resize_tx);

    let mut session = Session::new(cols, rows, config, session_name, &pty_tx)?;
    let mut renderer = AnsiRenderer::new(cols, rows);

    // Initial render.
    render::flush(&session, &mut renderer, &mut host)?;

    loop {
        tokio::select! {
            biased;

            _ = tokio::signal::ctrl_c() => {
                tracing::info!("ctrl-c received, exiting");
                break;
            }

            Some(key) = key_rx.recv() => {
                match session.router.handle_key(key) {
                    RouterOutcome::Command(DashboardCommand::Quit) => {
                        tracing::info!("quit command");
                        break;
                    }
                    RouterOutcome::Command(cmd) => {
                        session.apply_command(cmd, &pty_tx).await?;
                    }
                    RouterOutcome::ForwardToPane => {
                        session.forward_to_focused(key).await?;
                    }
                    RouterOutcome::Swallow => {}
                }
            }

            Some(evt) = pty_rx.recv() => {
                match evt {
                    PtyEvent::Output { pane_id, bytes } => {
                        session.feed_pane(pane_id, &bytes);
                    }
                    PtyEvent::Exited { pane_id } => {
                        session.mark_pane_exited(pane_id);
                        if session.pane_count() == 0 {
                            break;
                        }
                    }
                }
            }

            Some((cols, rows)) = resize_rx.recv() => {
                session.resize(cols, rows)?;
                renderer.resize(cols, rows);
            }
        }

        render::flush(&session, &mut renderer, &mut host)?;
    }

    // Cleanup: kill children, leave alt screen (Host::drop handles terminal restore).
    drop(session);
    drop(host);
    Ok(())
}
```

- [ ] **Step 3: Update src/main.rs to launch runtime**

Replace the TODO block in `main.rs`:

```rust
mod cli;
mod config;
mod conpty;
mod host;
mod pane_state;
mod session;
mod app;
mod render;

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    let config = config::load_config()?;

    if config.general.require_windows_terminal {
        if std::env::var("WT_SESSION").is_err() {
            anyhow::bail!(
                "claudio-mux requires Windows Terminal (WT_SESSION not found).\n\
                 Set general.require_windows_terminal = false in config to override."
            );
        }
    }

    let log_dir = config::log_dir();
    std::fs::create_dir_all(&log_dir)?;
    let file_appender = tracing_appender::rolling::daily(&log_dir, "claudio-mux.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            EnvFilter::try_from_env("CLAUDIO_MUX_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info"))
        )
        .init();

    tracing::info!("claudio-mux starting, session={}", cli.session);

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(app::run(config, cli.session))?;

    Ok(())
}
```

- [ ] **Step 4: Build**

Run: `cargo build -p claudio-mux`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add tools/claudio-mux/src/
git commit -m "feat(claudio-mux): wire event loop with ConPTY, keyboard, and ANSI rendering"
```

---

### Task 19: Manual smoke test + named layouts

**Files:**
- Create: `tools/claudio-mux/src/layouts.rs`

- [ ] **Step 1: Create src/layouts.rs**

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use terminal_core::SplitDirection;
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct LayoutFile {
    pub name: String,
    pub description: Option<String>,
    pub root: SerializedNode,
    #[serde(default)]
    pub panes: std::collections::HashMap<String, PaneDef>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SerializedNode {
    Leaf {
        pane: String,
    },
    Split {
        direction: SerializedDirection,
        ratio: f32,
        first: Box<SerializedNode>,
        second: Box<SerializedNode>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SerializedDirection {
    Vertical,
    Horizontal,
}

impl From<&SerializedDirection> for SplitDirection {
    fn from(d: &SerializedDirection) -> Self {
        match d {
            SerializedDirection::Vertical => SplitDirection::Vertical,
            SerializedDirection::Horizontal => SplitDirection::Horizontal,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PaneDef {
    pub spawn: String,
    #[serde(default)]
    pub command: Vec<String>,
    pub cwd: Option<String>,
}

pub fn load_layout(path: &Path) -> Result<LayoutFile> {
    let text = std::fs::read_to_string(path)?;
    let layout: LayoutFile = toml::from_str(&text)?;
    Ok(layout)
}
```

- [ ] **Step 2: Add mod to main.rs**

Add `mod layouts;` to the module declarations in `main.rs`.

- [ ] **Step 3: Build and manual test**

Run: `cargo build -p claudio-mux`

Manual smoke test (in Windows Terminal):
1. `cargo run -p claudio-mux` — should launch with a single pwsh pane
2. `Ctrl+B "` — should split horizontal, new shell appears
3. `Ctrl+B n` — focus cycles
4. `Ctrl+B x` — close pane
5. `Ctrl+B q` — quit
6. Terminal should restore to normal state after exit

- [ ] **Step 4: Commit**

```bash
git add tools/claudio-mux/src/layouts.rs tools/claudio-mux/src/main.rs
git commit -m "feat(claudio-mux): add named layout loading"
```

---

## Self-Review Checklist

After all tasks, verify:

1. **Spec coverage**: Every section of the design spec has a corresponding task:
   - Section 3 (arch decisions) → Tasks 1-6 (terminal-core)
   - Section 4 (crate topology) → Tasks 1-8 (all crate scaffolding)
   - Section 5 (kernel migration) → Tasks 11-12
   - Section 6 (claudio-mux binary) → Tasks 15-19
   - Section 7 (config/layouts) → Tasks 15, 19
   - Section 8 (forward-compat) → Built into type designs throughout
   - Section 9 (testing) → Tests embedded in each task
   - Section 10 (open questions) → Resolved in implementation (key mapping in host.rs, writer lifecycle in conpty.rs)

2. **Type consistency**: `PaneId = u64` everywhere. `CellViewport` in core, pixel `Viewport` only in shim. `DashboardCommand` has 13 variants (matching spec's 12 + `PreviousLayout`). `InputRouter` matches spec's API.

3. **No placeholders**: Every code block is complete. Every command has expected output.
