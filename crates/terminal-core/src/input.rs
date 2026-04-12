//! Input router — prefix-key state machine for multiplexer commands.

extern crate alloc;
use alloc::collections::BTreeMap;

use crate::command::DashboardCommand;
use crate::key::{KeyCode, KeyCombo, KeyEvent, Modifiers};

/// Internal state of the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    AwaitingCommand,
}

/// What the caller should do with a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterOutcome {
    /// Execute this dashboard command.
    Command(DashboardCommand),
    /// Forward the key to the focused pane unchanged.
    ForwardToPane,
    /// Swallow the key (prefix itself, or unknown command key).
    Swallow,
}

/// Prefix-key state machine.
///
/// Default prefix: Ctrl+B (tmux-compatible).
pub struct InputRouter {
    mode: Mode,
    prefix: KeyCombo,
    bindings: BTreeMap<char, DashboardCommand>,
}

impl InputRouter {
    /// Create a new router with default Ctrl+B prefix and tmux-compatible bindings.
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

    /// Change the prefix key.
    pub fn with_prefix(mut self, prefix: KeyCombo) -> Self {
        self.prefix = prefix;
        self
    }

    /// Rebind a character key to a command.
    pub fn rebind(&mut self, key: char, cmd: DashboardCommand) {
        self.bindings.insert(key, cmd);
    }

    /// Process one key event and return what the caller should do.
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
                match key.code {
                    KeyCode::Char(c) if key.mods == Modifiers::empty() => {
                        match self.bindings.get(&c) {
                            Some(&cmd) => RouterOutcome::Command(cmd),
                            None => RouterOutcome::Swallow,
                        }
                    }
                    _ => RouterOutcome::Swallow,
                }
            }
        }
    }

    /// Returns true when the router is waiting for a command key.
    pub fn is_awaiting_command(&self) -> bool {
        self.mode == Mode::AwaitingCommand
    }
}

impl Default for InputRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{KeyCode, KeyEvent};

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
            RouterOutcome::ForwardToPane
        );
    }

    #[test]
    fn prefix_then_command_yields_command() {
        let mut router = InputRouter::new();
        assert_eq!(router.handle_key(ctrl_b()), RouterOutcome::Swallow);
        assert_eq!(
            router.handle_key(key('"')),
            RouterOutcome::Command(DashboardCommand::SplitHorizontal)
        );
    }

    #[test]
    fn prefix_then_unknown_swallows() {
        let mut router = InputRouter::new();
        assert_eq!(router.handle_key(ctrl_b()), RouterOutcome::Swallow);
        assert_eq!(router.handle_key(key('z')), RouterOutcome::Swallow);
        // Back to normal — next plain key should forward
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

        for &(ch, cmd) in cases {
            let mut router = InputRouter::new();
            router.handle_key(ctrl_b());
            assert_eq!(
                router.handle_key(key(ch)),
                RouterOutcome::Command(cmd),
                "binding for '{}' failed",
                ch
            );
        }
    }

    #[test]
    fn custom_prefix() {
        let mut router = InputRouter::new().with_prefix(KeyEvent::ctrl('a'));
        // Ctrl+B should no longer trigger prefix mode
        assert_eq!(router.handle_key(ctrl_b()), RouterOutcome::ForwardToPane);
        // Ctrl+A should trigger it
        assert_eq!(
            router.handle_key(KeyEvent::ctrl('a')),
            RouterOutcome::Swallow
        );
        assert!(router.is_awaiting_command());
        assert_eq!(
            router.handle_key(key('"')),
            RouterOutcome::Command(DashboardCommand::SplitHorizontal)
        );
    }

    #[test]
    fn rebind_command() {
        let mut router = InputRouter::new();
        router.rebind('z', DashboardCommand::Quit);
        router.handle_key(ctrl_b());
        assert_eq!(
            router.handle_key(key('z')),
            RouterOutcome::Command(DashboardCommand::Quit)
        );
    }

    #[test]
    fn non_char_key_after_prefix_swallows() {
        let mut router = InputRouter::new();
        router.handle_key(ctrl_b());
        assert_eq!(
            router.handle_key(KeyEvent::plain(KeyCode::Up)),
            RouterOutcome::Swallow
        );
        // Back to normal after swallow
        assert_eq!(router.handle_key(key('a')), RouterOutcome::ForwardToPane);
    }
}
