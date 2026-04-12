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
        tracing::debug!("render: {} bytes", bytes.len());
        host.write_all(&bytes)?;
    }
    Ok(())
}

fn format_status(session: &Session) -> String {
    format!(" {} \u{2502} panes:{} ", session.session_name, session.pane_count())
}
