use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyEvent as CtKeyEvent, KeyCode as CtKeyCode, KeyModifiers},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use std::io::{self, Write};
use terminal_core::{KeyEvent, KeyCode, Modifiers};
use tokio::sync::mpsc;

pub struct Host {
    stdout: io::Stdout,
}

impl Host {
    pub fn new() -> Result<Self> {
        let mut stdout = io::stdout();
        terminal::enable_raw_mode()?;
        stdout.execute(EnterAlternateScreen)?;
        // Don't hide cursor — the renderer positions it in the focused pane
        // so users can see where they're typing.
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
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = self.stdout.execute(LeaveAlternateScreen);
        let _ = self.stdout.execute(crossterm::cursor::Show);
    }
}

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
