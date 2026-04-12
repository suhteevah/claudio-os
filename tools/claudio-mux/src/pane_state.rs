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
