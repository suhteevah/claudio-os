//! Dashboard commands — the vocabulary of prefix-key actions.

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
    OpenFiles,
    OpenBrowser,
    ToggleStatusBar,
    PreviousLayout,
    NextLayout,
    Quit,
}
