# claudio-mux: tmux-like Terminal Multiplexer for Windows

**Design date:** 2026-04-11
**Author:** Matt Gates (design session with Claude)
**Status:** draft — awaiting implementation plan

## 1. Summary

Build a Windows-native terminal multiplexer, `claudio-mux`, that runs inside Windows
Terminal and provides split panes, focus navigation, a status bar, named layouts,
session persistence (v2), and multi-client sessions (v3). The multiplexer **shares
its core logic with ClaudioOS** — the layout tree, pane grid, VTE parsing, keybind
vocabulary, and prefix-key state machine all live in a new `terminal-core` crate
that both ClaudioOS's framebuffer dashboard and the Windows binary depend on.

The goal is not "tmux on Windows." It's **one brain, two frontends**: the same
grammar of "multiplexed terminal with prefix commands" drives ClaudioOS's bare-metal
dashboard and the hosted Windows tool. Muscle memory, keybindings, status bar
format, and layout semantics transfer between the two environments without effort.

## 2. Goals, non-goals, and scope

### Goals (all versions)

1. Persistent sessions — detach and reattach, survive disconnect (v2)
2. Named, scriptable layouts — "the 4-pane dev setup" loaded with one command (v1)
3. Multi-client sessions — two humans (or a human and an agent) driving the same
   session simultaneously, locally first, then over the network (v3)
4. Prefix-key command mode matching ClaudioOS exactly (v1)
5. Persistent status bar showing session state (v1)

### Non-goals (v1)

- **Mouse support.** crossterm reports mouse events but terminal-core has no model
  for them. Defer to v2+.
- **Scrollback beyond the pane grid.** Matches ClaudioOS's current `Pane`, which has
  `scroll_offset` reserved but unimplemented. Host terminal's own scrollback covers
  most needs.
- **Copy / search mode** (tmux `Ctrl+B [`). Real feature, real work, not v1.
- **Config hot reload.** Restart to apply config changes.
- **Plugin API.** Zellij has WASM plugins. We do not.
- **Theme system.** Hardcoded defaults; config file can override specific SGR codes
  for the status bar.
- **Cross-platform packaging.** v1 is Windows-only. Dependencies (`directories`,
  `portable-pty`) work on other OSes — if it builds there, bonus — but we don't test
  and don't claim support.

### Phasing

- **v1**: shared-core refactor of ClaudioOS + single-process `claudio-mux` with
  splits, focus, close, prefix commands, status bar, named layouts. No daemon, no
  persistence, no IPC. Features 2, 4, 5 from the goals list.
- **v2**: extract a server daemon. ConPTY-owned shells survive client detach.
  Named-pipe protocol for `claudio-mux attach` / `detach`. Session state persists
  to `%APPDATA%`. Feature 1.
- **v3**: multi-client sessions. Starts with local multi-attach (trivial once v2's
  pipe protocol exists), then adds TLS transport for remote. Per-client viewport
  cropping. Optional read-only role. Feature 3.

The design below covers v1 exhaustively and v2/v3 only at the level of
forward-compatibility seams.

## 3. Key architectural decisions

| Decision | Choice | Reasoning |
|---|---|---|
| Code sharing between ClaudioOS and Windows tool | **Share actual code** (not mirror by convention) | User roadmap already values a reusable core crate; the kernel's prefix-key state machine is genuinely portable and currently tangled with kernel-specific side effects |
| Crate structure | **Workspace split: `terminal-core` + `terminal-fb` + `terminal-ansi` + `tools/claudio-mux`** | Clean boundary; renderers are pluggable; future frontends (web, iOS) follow the same pattern |
| Core crate constraints | `no_std + alloc`, no `serde` | Compatibility with ClaudioOS kernel; consumers add `std` / `serde` at their edge |
| Runtime (claudio-mux) | **tokio multi-threaded** | mpsc + `select!` is the natural shape for `keys + N PTY outputs + resize + signal` |
| ConPTY integration | **`portable-pty` crate** (from wezterm) | Handles ClosePseudoConsole race condition, well-tested on Windows 10/11, abstracts the Win32 plumbing |
| Host terminal library | **`crossterm`** | Handles ConIn raw mode, decodes keys to structured enum, emits resize events, works on Windows without WSL |
| Host terminal requirement (v1) | **Require Windows Terminal** (detected via presence of `WT_SESSION` env var at startup; refuses to run otherwise) | 24-bit color, full VT, mouse pass-through, predictable resize events. conhost works too but we don't test it. |
| Persistence semantics (v2) | **tmux model**: daemon owns ConPTYs, shells survive detach | Matches user expectations; agent panes serialize conversation state instead (future) |
| `Ctrl+B c` on Windows | **v1: spawn `claude` CLI as child process** (functionally just another shell); v2/v3: explore porting `api-client` crate natively | Unblocks instantly; the ambitious native port deserves its own design brainstorm |
| `Ctrl+B f` / `Ctrl+B w` on Windows | **Reserved**: core has `OpenFiles` / `OpenBrowser` variants, Windows rejects with "not available on this platform" status bar flash | Preserves muscle memory; enum stays honest |

## 4. Crate topology

### Workspace changes

The current `crates/terminal/` (2943 lines, 6 files) is split:

```
crates/
  terminal-core/        [no_std + alloc]   ← shared grammar
    src/
      lib.rs            Re-exports, crate docs
      viewport.rs       CellViewport { col, row, cols, rows }
      pane.rs           Pane, Cell, VTE integration — no pixel math
      layout.rs         LayoutNode, Layout, SplitDirection (from current layout.rs)
      command.rs        DashboardCommand (the keybind vocabulary)
      input.rs          InputRouter state machine (Normal / AwaitingCommand)
      status.rs         StatusBar — cell-based status line rendering
      key.rs            KeyEvent, KeyCode, Modifiers — source-agnostic

  terminal-fb/          [no_std + alloc]   ← ClaudioOS framebuffer renderer
    src/
      lib.rs            DrawTarget trait, pixel Viewport, adapter to core
      render.rs         Cell grid → framebuffer pixels (from current render.rs)
      unicode_font.rs   Bitmap font (unchanged)
    Cargo.toml: depends on terminal-core

  terminal-ansi/        [std]              ← hosted-terminal renderer
    src/
      lib.rs            AnsiRenderer — diff-based ANSI emitter
      diff.rs           Dirty-cell tracking; minimize bytes per flush
      host.rs           HostTerminal: raw mode, alt screen, resize events
    Cargo.toml: depends on terminal-core, crossterm

tools/
  claudio-mux/          [std, bin]         ← Windows binary
    src/
      main.rs           Arg parsing, tracing init, config load, runtime bootstrap
      app.rs            Top-level event loop — the tokio select! spine
      session.rs        Session struct: Layout + pane_states + InputRouter
      pane_state.rs     Per-pane runtime state: PaneKind, ConPTY handle
      conpty.rs         spawn_shell / spawn_agent / resize_pty (portable-pty shims)
      host.rs           Host terminal I/O: raw mode guard, alt screen, key decoding
      render.rs         Session ↔ terminal-ansi glue
      layouts.rs        Named layout load/save
      config.rs         Top-level config parse
      cli.rs            clap arg definitions
```

Estimated v1 footprint for `tools/claudio-mux/`: ~1500 lines across 9 files.

### `terminal-core` public API (key types)

```rust
// Identity type used throughout core.
// Chosen as u64 (not usize) so pane ids are stable if/when they cross a
// process or disk boundary — v2 session persistence and v3 named-pipe IPC
// both serialize pane ids. The current ClaudioOS code uses usize in most
// places and u64 in `PaneRequest` for the same reason; the refactor
// standardizes on u64. Ripple: layout.rs pane-id arithmetic widens from
// usize to u64 (no semantic change, no call-site changes in the kernel
// beyond one cast at the dashboard ↔ layout boundary).
pub type PaneId = u64;

// command.rs — the mirror point
pub enum DashboardCommand {
    SplitHorizontal,
    SplitVertical,
    FocusNext,
    FocusPrev,
    ClosePane,
    SpawnShell,           // Ctrl+B s
    SpawnAgent,           // Ctrl+B c
    OpenFiles,            // reserved; Windows rejects
    OpenBrowser,          // reserved; Windows rejects
    ToggleStatusBar,
    PreviousLayout,
    NextLayout,
    Quit,
}

// input.rs — lifted from kernel/src/dashboard.rs
pub struct InputRouter {
    mode: Mode,
    prefix: KeyCombo,                              // default: Ctrl+B
    bindings: alloc::collections::BTreeMap<char, DashboardCommand>,
}

enum Mode { Normal, AwaitingCommand }

pub enum RouterOutcome {
    Command(DashboardCommand),    // prefix sequence completed
    ForwardToPane,                // pass this key to the focused pane
    Swallow,                      // prefix key itself, or unknown command key
}

impl InputRouter {
    pub fn new() -> Self;
    pub fn with_prefix(self, prefix: KeyCombo) -> Self;
    pub fn rebind(&mut self, key: char, cmd: DashboardCommand);
    pub fn handle_key(&mut self, key: KeyEvent) -> RouterOutcome;
}

// key.rs — source-agnostic
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent { pub code: KeyCode, pub mods: Modifiers }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter, Tab, Backspace, Esc,
    Up, Down, Left, Right,
    Home, End, PageUp, PageDown,
    F(u8),
    Unknown(u32),
}

bitflags! { pub struct Modifiers: u8 { CTRL, SHIFT, ALT, META } }
// terminal-core depends on `bitflags = "2"` (no_std compatible).

pub type KeyCombo = KeyEvent;

// layout.rs — updated from current crates/terminal/src/layout.rs
pub struct Layout { /* tree, focus, geometry in cells */ }

impl Layout {
    pub fn new(cols: u16, rows: u16) -> Self;
    pub fn split(&mut self, dir: SplitDirection);
    pub fn focus_next(&mut self);
    pub fn focus_prev(&mut self);
    pub fn close_focused(&mut self);
    pub fn pane_by_id_mut(&mut self, id: PaneId) -> Option<&mut Pane>;
    pub fn resize(&mut self, cols: u16, rows: u16);   // reflows ratios
    pub fn pane_count(&self) -> usize;
    pub fn focused_pane_id(&self) -> PaneId;
}
```

**Critical property:** `terminal-core` knows nothing about pixels, nothing about
ConPTY, nothing about framebuffers, nothing about `std`. It is the pure grammar of
"a terminal multiplexer's brain." Both ClaudioOS and claudio-mux translate their
native keyboard events into `KeyEvent`s, feed them to `InputRouter`, and render the
resulting layout through their own renderer.

The `DashboardCommand` enum is intentionally **policy-free**. It encodes *what the
user pressed*, not *what each consumer does in response*. Splits happen, but what
goes in the new slot is consumer policy:

- **ClaudioOS**: `SpawnShell` → `ShellPaneState::new(...)`; `SpawnAgent` →
  `dashboard.create_session(...)` + IPC register
- **claudio-mux**: `SpawnShell` → ConPTY spawn of `cfg.shell`; `SpawnAgent` →
  ConPTY spawn of `cfg.agent` (defaults to `claude`)
- **`OpenFiles` / `OpenBrowser`**: ClaudioOS dispatches to its file manager / wraith
  browser; claudio-mux logs "not available" and flashes the status bar

## 5. Kernel migration (ClaudioOS side)

### Current state

`kernel/src/dashboard.rs` is 2667 lines. It contains:

- `PrefixState::{Normal, AwaitingCommand}` at line 691 (identical shape to proposed `InputRouter::Mode`)
- `handle_prefix_command(c, layout, dashboard, pane_types, input_buffers, next_shell_id, stack_ptr, now)` at line 1199 — 10 match arms, ~220 lines, takes 8 ClaudioOS-specific arguments
- Per-arm bodies mix two things: (1) an abstract command like "split horizontal" and (2) ClaudioOS-specific side effects (create `Dashboard` session, register with `IPC`, push `PaneType::Agent`, push `InputBuffer`, write welcome banner)

The stub `DashboardCommand` enum in `crates/terminal/src/lib.rs:108` currently has
zero consumers — it becomes real after the migration.

### Target shape

```rust
// Pseudocode of the new kernel loop
let mut router = InputRouter::new();

loop {
    let key = keyboard.next_key().await;
    match router.handle_key(to_key_event(key)) {
        RouterOutcome::Command(cmd)  => apply_command(cmd, &mut layout, &mut dashboard, ...),
        RouterOutcome::ForwardToPane => forward_to_focused_pane(key, ...).await,
        RouterOutcome::Swallow       => {}
    }
    render_dirty(&mut layout);
}

fn apply_command(
    cmd: DashboardCommand,
    layout: &mut Layout,
    dashboard: &mut Dashboard,
    pane_types: &mut Vec<PaneType>,
    ...
) {
    match cmd {
        DashboardCommand::SplitHorizontal => { /* split + populate per kernel policy */ }
        DashboardCommand::SpawnShell      => { /* existing shell setup — unchanged */ }
        DashboardCommand::SpawnAgent      => { /* existing agent setup — unchanged */ }
        DashboardCommand::OpenFiles       => { /* existing file manager setup */ }
        DashboardCommand::OpenBrowser     => { /* existing browser setup */ }
        /* etc. */
    }
}
```

`apply_command` is essentially the old `handle_prefix_command` with the `match c`
layer peeled off — it receives an already-parsed command and only handles the
ClaudioOS policy. Line count drops by ~150. Per-arm bodies are unchanged except for
the discriminant.

### `Layout::new` signature change

The current `Layout::new(fb_width: usize, fb_height: usize)` takes pixel
dimensions and internally computes `cols / rows` via `FONT_WIDTH` / `FONT_HEIGHT`.
The new `terminal-core::Layout::new(cols: u16, rows: u16)` takes cell dimensions
directly. Pixel-to-cell conversion is `terminal-fb`'s responsibility, handled at
the kernel ↔ `terminal-fb` boundary:

```rust
// In kernel/src/dashboard.rs — after the refactor
let (cols, rows) = terminal_fb::pixels_to_cells(fb_width, fb_height);
let mut layout = Layout::new(cols, rows);
```

No kernel call site outside `run_dashboard` constructs a `Layout`, so the ripple
is bounded to that file.

### Things that do NOT move into `terminal-core`

- **`Ctrl+Alt+F1-F6` virtual-console switching** — pre-empts the router, handled
  directly in the kernel keyboard interrupt path, not a dashboard concern
- **`Ctrl+Shift+C/V/H` clipboard** — requires modifier-state tracking via
  `vconsole::shift_held()`; stays in the kernel, handled *before* the router sees
  the key
- **`PaneRequest` cross-task bus** (`request_open_editor`, `request_open_shell`,
  `request_close_pane`) — cross-task messaging, orthogonal to key handling, stays
  put
- **ClaudioOS-specific pane types** (`PaneType::Agent`, `PaneType::Shell`,
  `PaneType::Editor`, `PaneType::Browser`, `PaneType::FileManager`) — policy, not
  grammar; stay in the kernel
- **`Dashboard` session tracking, IPC registration** — kernel policy

### Incremental migration plan

The main risk is subtle behavior drift in edge cases during refactor. Mitigation:

1. **Capture current behavior as golden tests first.** Before lifting anything,
   write `terminal-core` tests that script every existing binding through the
   *existing* dashboard.rs state machine (temporarily copy it into a test) and
   record the resulting `Layout` mutations as goldens. These become the oracle.
2. **Lift InputRouter incrementally.** Three small commits:
   - (a) add `InputRouter` to terminal-core and route *only* the `Normal`-mode
     key-forwarding case through it. Prefix state machine stays in dashboard.rs.
   - (b) move the `AwaitingCommand` state machine to `InputRouter`.
   - (c) move the binding map (`BindingMap`) to `InputRouter`.
3. **`DashboardCommand` dispatching stays identical.** The new `apply_command(cmd,
   ...)` function has per-arm bodies that are a copy-paste of the current
   per-`char` bodies, modulo the match discriminant. No cleanup, no refactoring
   inside arms. Cleanup is a later PR.
4. **Run in QEMU after each step.** Visual smoke test: boot, `Ctrl+B "`,
   `Ctrl+B n`, `Ctrl+B c`, `Ctrl+B x` — looks the same as before? Ship that step.

### Cost estimate

- **New code**: ~300 lines in terminal-core (InputRouter + KeyEvent + BindingMap + tests)
- **Moved code**: ~200 lines from `kernel/src/dashboard.rs` to terminal-core
- **Deleted code**: the dead `DashboardCommand` stub in `crates/terminal/src/lib.rs` (becomes real in the new home)
- **Edited code**: dashboard run loop (~30 lines), `handle_prefix_command` renamed to `apply_command` (~220 lines, mostly unchanged bodies)

## 6. `claudio-mux` v1 binary

### Process model

Single OS process. Tokio multi-threaded runtime. No daemon. The process owns
`Layout`, all `Pane`s, and all ConPTY children directly. When the process exits
(`Ctrl+B q`, host terminal closes, or last pane exits), children are killed and
state is lost. That's fine for v1 — session persistence is v2's problem.

### Dependencies

```toml
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
strum        = { version = "0.26", features = ["derive"] }
```

### ConPTY wiring

```rust
// conpty.rs
pub struct PtyHandle {
    master: Box<dyn MasterPty + Send>,
    child:  Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
}

pub fn spawn_shell(cols: u16, rows: u16, cfg: &Config) -> Result<PtyHandle> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        cols, rows, pixel_width: 0, pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&cfg.general.shell);
    for arg in &cfg.general.shell_args { cmd.arg(arg); }
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let writer = pair.master.take_writer()?;
    Ok(PtyHandle { master: pair.master, child, writer })
}

pub fn spawn_agent(cols: u16, rows: u16, cfg: &Config) -> Result<PtyHandle> {
    // Same shape, runs cfg.general.agent (default: "claude").
    // Inherits environment; claude CLI picks up ANTHROPIC_API_KEY itself.
}
```

Each spawned pane also launches a dedicated tokio task that reads from `master`
in a loop and pushes bytes into an mpsc channel tagged with the pane id:

```rust
let mut reader = pty.master.try_clone_reader()?;
tokio::task::spawn_blocking(move || {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0)  => break,
            Ok(n)  => { let _ = tx.blocking_send(PtyEvent::Output { pane_id, bytes: buf[..n].to_vec() }); }
            Err(_) => break,
        }
    }
    let _ = tx.blocking_send(PtyEvent::Exited { pane_id });
});
```

`spawn_blocking` is used because `portable-pty`'s reader is synchronous — wrapping
it in a blocking task keeps it off the main runtime threads.

### Event loop

```rust
// app.rs
pub async fn run(mut session: Session, mut host: Host) -> Result<()> {
    let (pty_tx, mut pty_rx)       = mpsc::channel::<PtyEvent>(256);
    let (key_tx, mut key_rx)       = mpsc::channel::<KeyEvent>(64);
    let (resize_tx, mut resize_rx) = mpsc::channel::<(u16, u16)>(8);

    host.spawn_input_reader(key_tx, resize_tx);  // crossterm on its own thread

    loop {
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => break,

            Some(key) = key_rx.recv() => {
                match session.router.handle_key(key) {
                    RouterOutcome::Command(cmd)  => session.apply_command(cmd, &pty_tx).await?,
                    RouterOutcome::ForwardToPane => session.forward_to_focused(key).await?,
                    RouterOutcome::Swallow       => {}
                }
            }

            Some(evt) = pty_rx.recv() => match evt {
                PtyEvent::Output { pane_id, bytes } => session.feed_pane(pane_id, &bytes),
                PtyEvent::Exited { pane_id }        => session.mark_pane_exited(pane_id),
            },

            Some((cols, rows)) = resize_rx.recv() => session.resize(cols, rows)?,
        }

        render::flush(&mut session, &mut host)?;
    }

    Ok(())
}
```

`biased;` polls arms in declaration order: signal first, then user input, then PTY
output (chatty panes won't starve the keyboard), then resize.

### Rendering through `terminal-ansi`

`terminal-ansi` is a separate crate and must not depend on `claudio-mux`, so the
renderer takes a neutral `Scene` value that the binary assembles from its own
`Session` state each frame:

```rust
// terminal-ansi — Scene is the stable interface between session code and the renderer.
pub struct Scene<'a> {
    pub layout: &'a terminal_core::Layout,
    pub focused: terminal_core::PaneId,
    pub status_bar: Option<&'a terminal_core::StatusBar>,
    pub status_context: StatusContext<'a>,  // format-string substitution values
}

pub struct StatusContext<'a> {
    pub session_name: &'a str,
    pub layout_name: &'a str,
    pub pane_count: usize,
    pub focused_title: &'a str,
    pub time: &'a str,  // pre-formatted; binary owns strftime
}

pub struct AnsiRenderer {
    prev_frame: Vec<Cell>,
    cursor_prev: (u16, u16),
}

impl AnsiRenderer {
    pub fn new(cols: u16, rows: u16) -> Self;
    pub fn resize(&mut self, cols: u16, rows: u16);
    pub fn render<W: Write>(&mut self, scene: &Scene, out: &mut W) -> io::Result<usize>;
}
```

`Session` (defined in `tools/claudio-mux/src/session.rs`) exposes a
`fn scene(&self) -> Scene<'_>` helper that the event loop calls each frame. The
renderer never sees the binary's internal types.

**Diffing strategy:**

- Walk the `Layout` tree leaves, composing a single virtual frame from each
  `Pane`'s grid into the merged output.
- Compare against `prev_frame` cell-by-cell. Emit runs of changed cells using
  `CSI row;col H` positioning and SGR sequences only when color/attrs change.
  Unchanged runs emit zero bytes.
- Borders between panes are drawn once from the `Layout` tree — they live in the
  gaps between leaves, not inside any `Pane`. Redrawn only on layout change or
  resize.
- Status bar rendered last, always on bottom row, always full-width. Generated
  from `StatusBar` in `terminal-core` with configurable format tokens.
- **Cursor position**: at the end of each frame, emit `CSI r;c H` to place the
  hardware cursor inside the *focused* pane at its logical cursor position.
  This is what makes typing feel native.

Per-frame overhead for an idle session: zero bytes. Per-keystroke overhead:
typically one SGR sequence + one character + maybe a cursor move.

### Resize handling

When crossterm fires `Event::Resize(cols, rows)`:

1. `(cols, rows)` flows through `resize_tx`
2. Event loop → `session.resize(cols, rows)`:
   - `layout.resize(cols, rows - status_bar_rows)` reflows ratios, recalculates
     cell viewports for every leaf
   - Each pane: `pane.resize(new_cols, new_rows)` (grid re-allocs, VTE gets a fresh
     dirty flag)
   - Each ConPTY: `master.resize(PtySize { ... })` so child shell sees a `WINSIZE`
     change and re-prompts
3. Renderer's `prev_frame` is resized; first post-resize frame emits a full screen
   (no cheap diff across a resize)

Reflow is O(panes). Not a perf concern.

### Shutdown

Three trigger paths, all land in the same cleanup:

1. **User**: `Ctrl+B q` → `DashboardCommand::Quit` through the router
2. **Signal**: `tokio::signal::ctrl_c()` arm in the select loop
3. **Natural**: last pane exits and `Session::pane_count() == 0`

Cleanup sequence:

1. Kill all surviving ConPTY children (`child.kill()`)
2. Wait up to 500 ms for exit, then detach
3. `host.leave_alt_screen()`, `host.disable_raw_mode()`, show cursor, flush
4. Drop `Session` (drops PTY handles, drops tokio tasks — they cancel on channel
   close)
5. Exit 0

`Host` is RAII-guarded: `Host::new()` enters raw mode and alt screen; its `Drop`
impl leaves them. Even a panic path leaves the terminal in a usable state.

## 7. Config, layouts, and storage

### Paths

Using the `directories` crate with qualifier `ridge-cell`:

```
%APPDATA%\ridge-cell\claudio-mux\config\
├── config.toml                           Top-level settings
└── layouts\
    ├── default.toml                      Auto-created on first run
    ├── dev.toml                          User-authored
    └── pair.toml                          User-authored

%LOCALAPPDATA%\ridge-cell\claudio-mux\data\
└── logs\
    └── claudio-mux.log.<date>            Rolling daily tracing sink
```

Session state intentionally absent in v1 — that's v2's directory (`sessions\`).

### `config.toml`

```toml
[general]
shell = "pwsh.exe"
shell_args = ["-NoLogo"]
agent = "claude"
agent_args = []
require_windows_terminal = true

[keybindings]
prefix = "Ctrl+b"

[keybindings.bindings]
'"' = "SplitHorizontal"
"%" = "SplitVertical"
"n" = "FocusNext"
"p" = "FocusPrev"
"c" = "SpawnAgent"
"s" = "SpawnShell"
"x" = "ClosePane"
"L" = "NextLayout"
"q" = "Quit"
"f" = "OpenFiles"         # reserved on Windows
"w" = "OpenBrowser"       # reserved on Windows

[status_bar]
enabled = true
left  = " {session} │ layout:{layout} │ panes:{pane_count} "
right = " {focus} │ {time} "
sgr_normal  = [7]
sgr_focused = [1, 7]
```

**Loading semantics:**

- File is optional; missing means "use defaults"
- Parses to a `Config` struct via `serde::Deserialize` with `#[serde(default)]` on
  every field — adding fields in a later version doesn't break existing configs
- Parse errors abort startup with a readable error pointing at the line (no silent
  fallback on malformed input)
- `#[serde(deny_unknown_fields)]` is explicitly **off** — v1 binary reads v2
  config files gracefully
- Rebinding validation at load time: each binding value must `FromStr`-parse into
  a `DashboardCommand` variant (via `strum::EnumString`); unknown names → error
  with the list of valid names. Same for the prefix key combo.

### Layout files

```toml
# ~/config/layouts/dev.toml
name = "dev"
description = "Three-pane editor + logs + shell"

[root]
kind = "split"
direction = "vertical"       # left | right
ratio = 0.6

[root.first]
kind = "split"
direction = "horizontal"     # top / bottom
ratio = 0.7

[root.first.first]
kind = "leaf"
pane = "editor"

[root.first.second]
kind = "leaf"
pane = "logs"

[root.second]
kind = "leaf"
pane = "shell"

[panes.editor]
spawn = "command"
command = ["nvim", "."]
cwd = "${project_dir}"

[panes.logs]
spawn = "command"
command = ["powershell", "-NoLogo", "-Command", "Get-Content -Wait .\\logs\\app.log"]

[panes.shell]
spawn = "shell"
```

**Variable expansion** is deliberately tiny:

- `${project_dir}` — the directory where `claudio-mux --layout dev` was invoked
- `${home}` — user profile
- `${env:FOO}` — environment variable

No shell expansion, no globbing, no conditional logic. For logic, use a script
that launches claudio-mux.

**Loading:** `claudio-mux --layout dev` resolves `layouts\dev.toml`, parses, walks
the tree, spawns the referenced pane definitions. `claudio-mux` with no flags
loads `layouts\default.toml` if present, else creates a single `SpawnShell` pane.

### Tree serialization

`terminal-core::LayoutNode` has no serde because the core crate stays `no_std +
alloc`. `tools/claudio-mux/src/layouts.rs` defines a parallel type with derives:

```rust
#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum SerializedLayoutNode {
    Leaf {
        pane: String,       // references [panes.xxx]
    },
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<SerializedLayoutNode>,
        second: Box<SerializedLayoutNode>,
    },
}
```

The conversion `SerializedLayoutNode → LayoutNode` at load time is a straight tree
walk that allocates `PaneId`s and spawns PTYs. The inverse (`Layout::save_as(name)`
for a runtime "save current layout" feature) is the same walk in reverse.

### Feature 2 UX

- `Ctrl+B L` → reads `layouts\` directory → shows a picker in the status bar area
  → selection tears down current panes, loads new layout (two keypresses to
  confirm teardown)
- `Ctrl+B S` → prompts for a name via status bar → serializes current layout +
  pane kinds to `layouts\<name>.toml`

Small features once the loader works.

### Logging sink

```rust
let file_appender = tracing_appender::rolling::daily(&log_dir, "claudio-mux.log");
let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
tracing_subscriber::fmt()
    .with_writer(non_blocking)
    .with_env_filter(
        EnvFilter::try_from_env("CLAUDIO_MUX_LOG").unwrap_or_else(|_| EnvFilter::new("info"))
    )
    .init();
```

**Never log to stdout** — stdout is the UI; anything the logger writes there
corrupts the rendered frame. All diagnostics go to the rolling file.
`CLAUDIO_MUX_LOG=debug` at invocation gives verbose output per the ClaudioOS
verbose-logging preference.

## 8. Forward-compat hooks (v2 and v3)

These are v1 decisions embedded precisely so v2/v3 are not rewrites:

| Seam | v1 shape | v2/v3 cost |
|---|---|---|
| Event loop | mpsc channels between keyboard / PTY / render | v2 swaps `key_rx` to read from a named pipe; rest of loop unchanged |
| `Session` | Owns Layout + panes, unaware of where clients are | v3 adds `clients: Vec<ClientId>`; loops in `render::flush`; layout untouched |
| `PtyHandle` | Holds ConPTY master side, not child stdin | v2 daemon keeps the handle across client detach; child is unaware |
| `DashboardCommand` | Policy-free enum in terminal-core | New consumers (daemon, remote client, hypothetical iOS frontend) bind keys without touching core |
| `Config` | `deny_unknown_fields` off | v1 binary reads v2 configs gracefully; future `[daemon]` section silently ignored |
| Paths | `sessions\` dir reserved but absent | v2 writes there without a migration |
| Terminal size | Per-pane grid authoritative; renderer stateless beyond its `prev_frame` | v3 renders per-client with different viewport crops from the same grid |

**Critical property:** none of the v2/v3 features require changing `terminal-core`
itself. Core is the stable foundation; everything that changes between versions is
a consumer of core.

### v3 multi-client framing

The v3 design rests on a single framing:

> A "client" is anything that sends keystrokes in and receives rendered output.
> Humans attaching from Windows Terminal, agents attaching over a local pipe,
> remote humans attaching over TLS — all the same interface, different transports.

This generalizes the multi-agent story the project already has: a Claude agent
watching or driving a pane is structurally a second attached client. Pair
programming isn't bolted onto a single-user tool — "sessions with multiple
participants" was always the shape of the thing.

Hard parts flagged for v3:

- **Terminal-size reconciliation**: two clients with different dimensions → per-
  client viewport cropping is the chosen approach (tmux's lowest-common-denominator
  resize is annoying)
- **Input conflict**: if two clients type at once, bytes interleave (tmux's
  approach; fine for pair programming)
- **Auth and transport**: local (named pipe, trust the OS) is trivial; remote (TLS
  + token) is a real project — decision deferred
- **Read-only mode**: nice to have for "let me show you what's happening." Cheap if
  the client protocol distinguishes input from output.

## 9. Testing strategy

| Layer | Test kind | Location | CI? |
|---|---|---|---|
| `terminal-core` layout operations | Unit — split, focus, close, resize, tree invariants | `crates/terminal-core/src/layout.rs` mod tests | yes |
| `terminal-core` InputRouter | Unit — scripted `KeyEvent` → `RouterOutcome` transitions covering every binding | `crates/terminal-core/src/input.rs` mod tests | yes |
| `terminal-core` Pane + VTE | Unit — feed ANSI byte sequences, assert resulting `Cell` grid (existing tests from `crates/terminal/src/pane.rs` move over) | `crates/terminal-core/src/pane.rs` mod tests | yes |
| `terminal-ansi` diff | Unit — given before/after `Scene` values, assert exact byte sequence emitted (golden tests); empty diff emits zero bytes | `crates/terminal-ansi/tests/diff.rs` | yes |
| `terminal-fb` framebuffer | Unit — existing bitmap font rendering tests, unchanged | existing | yes |
| `claudio-mux` session glue | Integration — scripted `KeyEvent` stream + mock PTY backing → assert resulting terminal state + assert exact ANSI output | `tools/claudio-mux/tests/session.rs` | yes |
| `claudio-mux` real ConPTY spawn | Manual smoke test only | manual only | no |
| Kernel dashboard migration | Existing QEMU smoke test (cargo build → image-builder → qemu) confirms `apply_command` behaves like `handle_prefix_command` | existing | yes via kernel build |

### Mock PTY for integration tests

```rust
trait PtyBackend {
    fn spawn(&mut self) -> Box<dyn AsyncRead + AsyncWrite>;
    fn resize(&mut self, id: PaneId, cols: u16, rows: u16);
}
```

Real impl wraps `portable-pty`. Test impl is a pair of `tokio::io::duplex` channels
the test harness drives directly. This is how integration tests run on Linux CI
despite ConPTY being Windows-only.

### Golden ANSI tests

The core confidence signal. Every interesting behavior in `claudio-mux` reduces
to: "given this input sequence and starting state, the bytes emitted to stdout
should be exactly this." A test suite of ~30 of these covers all v1 commands and
their rendering. When something regresses visually, a diff on the golden file
tells you exactly which cell got wrong.

### Observability

`CLAUDIO_MUX_LOG=debug` → verbose rolling log at
`%LOCALAPPDATA%\ridge-cell\claudio-mux\data\logs\claudio-mux.log.<date>`.

Tracing spans at:

- `session::apply_command` — command dispatch (variant, before/after layout stats)
- `session::feed_pane` — PTY bytes entering a pane (pane_id, byte count, sample)
- `render::flush` — frame render (cells changed, bytes emitted, elapsed µs)
- `conpty::spawn` — process launch (argv, pid, pty dims)
- `app::event_loop` — each select arm that fires

Anything surprising discovered about ConPTY, `portable-pty`, `crossterm`, or
Windows Terminal edge cases gets documented in
`J:\openclaw-vault\Projects\ClaudioOS\` with a memory pointer *immediately* when
verified (per the wheel-capture rule).

## 10. Open questions deferred to implementation

Questions that don't need answers for the spec but will come up during plan
execution:

- Exact `crossterm::KeyEvent` → `terminal_core::KeyEvent` mapping — verify `Char`
  + `Modifiers::CTRL` is how crossterm delivers `Ctrl+B`, or if it's a separate
  control-code form. Mirror whatever `pc-keyboard` does on the ClaudioOS side.
- Exact `portable-pty` writer lifecycle — does `take_writer()` give us a `Send`
  handle we can stash in an async task, or do we need to route writes through a
  separate channel?
- Status bar `{time}` token format — 24-hour, seconds, time zone, or configurable?
  Probably configurable via the existing format string with strftime-like
  substitution.
- How to render a pane's cursor when it's not the focused pane — dim block, hide
  entirely, or outline? tmux hides; zellij outlines. I lean toward hiding.
- `Ctrl+B :` command mode for ad-hoc commands (tmux parity) — not in v1 goals, but
  reserve the keybind.
