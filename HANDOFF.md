# Session Handoff — 2026-04-12 (session 2)

## Last Updated
2026-04-12

## Project Status
🟢 claudio-mux v1 scaffold complete, shell rendering verified

## What Was Done This Session

### Implementation Plan
- Wrote full 19-task implementation plan at `docs/superpowers/plans/2026-04-12-claudio-mux.md`
- Executed all 19 tasks via subagent-driven development on branch `feat/claudio-mux`

### New Crates Created (Phase 1-4)
- **terminal-core** (`crates/terminal-core/`) — `no_std + alloc`, 28 tests
  - KeyEvent, KeyCode, Modifiers (bitflags), DashboardCommand (13 variants)
  - InputRouter state machine (prefix-key → command dispatch)
  - Color palette, Cell type, CellViewport (cell-based coordinates)
  - Pane with full VTE integration (no pixel math)
  - Layout with binary split tree (cell-based separators)
- **terminal-fb** (`crates/terminal-fb/`) — framebuffer renderer bridging core to pixel output
  - DrawTarget trait, render_char, fill_rect, pane_renderer, terminus font
- **terminal-ansi** (`crates/terminal-ansi/`) — ANSI diff renderer, 4 tests
  - Cell-by-cell diff engine, AnsiRenderer, Scene type, separator chars (│ ─)
- **terminal shim** — `crates/terminal/` re-exports shared types from core/fb

### Kernel Migration (Phase 3)
- `kernel/src/dashboard.rs` — PrefixState replaced with InputRouter, handle_prefix_command renamed to apply_command

### claudio-mux Binary (Phase 5)
- **tools/claudio-mux/** — excluded workspace package (builds for host target)
  - CLI: --layout, --session flags (clap)
  - Config: TOML at %APPDATA%\ridge-cell\claudio-mux\ with smart defaults
  - ConPTY: portable-pty wrapper (spawn_shell, spawn_agent)
  - Host: RAII raw mode + alt screen, crossterm key reader → terminal-core KeyEvent
  - Session: Layout + InputRouter + PaneState lifecycle
  - Event loop: tokio select! (keyboard, PTY output, resize)
  - Render: Session → Scene → AnsiRenderer → ANSI bytes → stdout
  - Named layouts: TOML loading scaffolded (not yet wired to --layout flag)

### Bug Fixes (post-review)
- **DSR response** — shell sends ESC[6n asking cursor position; we now respond, unblocking shell output
- **Ctrl+char encoding** — guard on is_ascii_alphabetic(), use lowercase for correct control codes
- **Host::Drop order** — fixed: disable_raw_mode → leave alt screen → show cursor
- **F5-F12 keys** — added VT escape sequences (were silently dropped)
- **Ctrl+C forwarding** — removed tokio ctrl_c handler so Ctrl+C reaches the shell
- **Dead pane cleanup** — auto-close exited panes, all_exited() for proper exit detection

### Smoke Test Results
- cmd.exe renders: Microsoft Windows banner, copyright, `C:\Users\Matt>` prompt
- Status bar renders: session name, pane count, │ separator
- Alt screen enter/exit clean, cursor visible, terminal restored on exit

## Current State

### Working
- terminal-core: 28 tests passing, full VTE + layout + input routing
- terminal-ansi: 4 tests passing, diff rendering
- claudio-mux: builds, launches, spawns shell, renders output, status bar works
- Kernel: compiles with InputRouter migration

### Not Yet Tested Interactively
- Typing into the shell (key forwarding works in code, not manually verified)
- Ctrl+B prefix commands (split, focus, close, quit)
- Window resize handling
- Multiple panes / split rendering

### Not Done
- --layout flag not wired to layouts::load_layout
- Status bar {time} token
- No scrollback
- No mouse support (v2)

## Blocking Issues
- MSVC linker issue on this machine (msvcrt.lib not found) — only affects full workspace `cargo build`; tests run via GNU target, `cargo check` works
- Prior blockers (AHCI DMA, SSH pipe wiring) remain open and unrelated

## What's Next
1. **Interactive testing** — run claudio-mux live in Windows Terminal, test typing, Ctrl+B splits, focus cycling
2. **Wire --layout flag** — connect CLI arg to layouts::load_layout for named layouts
3. **Multi-pane rendering** — verify split rendering with separators works visually
4. **Merge to main** — once interactive testing passes
5. **v2 planning** — daemon mode, session persistence, detach/attach

## Notes for Next Session
- Build claudio-mux with: `cd tools/claudio-mux && rustup run stable-x86_64-pc-windows-gnu cargo build --target x86_64-pc-windows-gnu`
- Run with: `CLAUDIO_MUX_LOG=debug target/x86_64-pc-windows-gnu/debug/claudio-mux.exe`
- Logs at: `%LOCALAPPDATA%\ridge-cell\claudio-mux\data\logs\`
- The crate is EXCLUDED from the workspace (like image-builder) — has its own .cargo/config.toml
- DSR response was the key breakthrough — without it, shells produce no output
- The shell default auto-detects pwsh.exe vs cmd.exe at startup
- All design decisions are locked in `docs/superpowers/specs/2026-04-11-claudio-mux-design.md`
