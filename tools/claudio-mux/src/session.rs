use anyhow::Result;
use terminal_core::{Layout, InputRouter, DashboardCommand, SplitDirection, PaneId, KeyEvent};
use tokio::sync::mpsc;
use std::io::Write;
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

    pub fn config(&self) -> &Config { &self.config }

    pub async fn apply_command(&mut self, cmd: DashboardCommand, pty_tx: &mpsc::Sender<PtyEvent>) -> Result<()> {
        match cmd {
            DashboardCommand::SplitHorizontal => self.do_split(SplitDirection::Horizontal, PaneKind::Shell, pty_tx)?,
            DashboardCommand::SplitVertical => self.do_split(SplitDirection::Vertical, PaneKind::Shell, pty_tx)?,
            DashboardCommand::FocusNext => self.layout.focus_next(),
            DashboardCommand::FocusPrev => self.layout.focus_prev(),
            DashboardCommand::SpawnShell => self.do_split(SplitDirection::Horizontal, PaneKind::Shell, pty_tx)?,
            DashboardCommand::SpawnAgent => self.do_split(SplitDirection::Horizontal, PaneKind::Agent, pty_tx)?,
            DashboardCommand::ClosePane => self.close_focused(),
            DashboardCommand::Quit => {} // Handled by caller
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

    pub async fn forward_to_focused(&mut self, key: KeyEvent) -> Result<()> {
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

    fn do_split(&mut self, dir: SplitDirection, kind: PaneKind, pty_tx: &mpsc::Sender<PtyEvent>) -> Result<()> {
        self.layout.split(dir);
        let new_id = self.layout.focused_pane_id();
        let pane = self.layout.focused_pane();

        let pty = match kind {
            PaneKind::Shell => conpty::spawn_shell(
                pane.viewport.cols, pane.viewport.rows,
                &self.config.general.shell, &self.config.general.shell_args,
            )?,
            PaneKind::Agent => conpty::spawn_agent(
                pane.viewport.cols, pane.viewport.rows,
                &self.config.general.agent, &self.config.general.agent_args,
            )?,
        };

        Self::start_pty_reader(new_id, &pty, pty_tx.clone());

        self.pane_states.push(PaneState { id: new_id, kind, pty, exited: false });
        Ok(())
    }

    fn close_focused(&mut self) {
        if self.layout.pane_count() <= 1 { return; }
        let id = self.layout.focused_pane_id();
        if let Some(ps) = self.pane_states.iter_mut().find(|s| s.id == id) {
            let _ = ps.pty.child.kill();
        }
        self.pane_states.retain(|s| s.id != id);
        self.layout.close_focused();
    }

    fn start_pty_reader(pane_id: PaneId, pty: &conpty::PtyHandle, tx: mpsc::Sender<PtyEvent>) {
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

fn key_to_bytes(key: KeyEvent) -> Vec<u8> {
    use terminal_core::{KeyCode, Modifiers};
    match key.code {
        KeyCode::Char(c) => {
            if key.mods.contains(Modifiers::CTRL) {
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
