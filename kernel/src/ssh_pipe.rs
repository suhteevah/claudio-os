//! SSH <-> shell pane bidirectional pipe registry.
//!
//! Provides a global keyed-by-`channel_id` map of byte ring buffers used to
//! shuttle data between an active SSH channel (driven by `ssh_server.rs`) and
//! a corresponding `PaneType::Shell` shell pane in the dashboard.
//!
//! ## Direction conventions
//!
//! - `input` — bytes the SSH client sent us. Producer: `ssh_server` channel
//!   data handler. Consumer: dashboard shell-pane input pump.
//! - `output` — bytes the shell pane wants to display. Producer: dashboard
//!   shell-pane (mirror of `pane.write_str`). Consumer: `ssh_server` outgoing
//!   drain that calls `session.send_channel_data`.
//!
//! Both sides operate via short-lived `spin::Mutex` critical sections; no
//! `Arc` is needed because the registry itself owns the buffers and both
//! sides look them up by `channel_id` on every access.
//!
//! ## Lifecycle
//!
//! 1. `ssh_server::handle_action::StartShell` → [`register`] (creates empty
//!    queues for `channel_id`).
//! 2. Dashboard materialises a `PaneType::Shell` tagged with `Some(channel_id)`.
//! 3. Each tick:
//!    - Dashboard calls [`drain_input`] for every SSH-attached shell pane.
//!    - Dashboard calls [`push_output`] for every byte the shell pane writes.
//!    - SSH server polls [`drain_output`] and forwards bytes over the SSH
//!      channel.
//! 4. On `on_channel_close` → [`unregister`].

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

/// One bidirectional byte pipe attached to a single SSH channel.
struct PipeBuf {
    /// SSH client → shell pane.
    input: VecDeque<u8>,
    /// Shell pane → SSH client.
    output: VecDeque<u8>,
}

impl PipeBuf {
    const fn new() -> Self {
        Self {
            input: VecDeque::new(),
            output: VecDeque::new(),
        }
    }
}

/// Hard cap on per-direction buffered bytes. Anything beyond this is dropped
/// with a warning so a stuck consumer can't OOM the kernel.
const PIPE_CAP: usize = 64 * 1024;

/// Global registry. Locked briefly on every push/drain.
static SSH_PIPES: spin::Mutex<BTreeMap<u32, PipeBuf>> = spin::Mutex::new(BTreeMap::new());

/// Allocate a new pipe pair for the given SSH channel id.
///
/// Idempotent: a re-register on an existing id resets both buffers (which
/// is the right thing to do if the previous channel was torn down without a
/// proper close event).
pub fn register(channel_id: u32) {
    let mut map = SSH_PIPES.lock();
    map.insert(channel_id, PipeBuf::new());
    log::debug!("[ssh_pipe] registered channel {}", channel_id);
}

/// Drop the pipe pair for an SSH channel that has closed.
pub fn unregister(channel_id: u32) {
    let mut map = SSH_PIPES.lock();
    if map.remove(&channel_id).is_some() {
        log::debug!("[ssh_pipe] unregistered channel {}", channel_id);
    }
}

/// Push bytes from the SSH client toward the shell pane. Returns the number
/// of bytes accepted (may be less than `data.len()` if the buffer is full).
pub fn push_input(channel_id: u32, data: &[u8]) -> usize {
    let mut map = SSH_PIPES.lock();
    let Some(pipe) = map.get_mut(&channel_id) else {
        log::warn!(
            "[ssh_pipe] push_input on unregistered channel {} ({} bytes dropped)",
            channel_id,
            data.len()
        );
        return 0;
    };
    let avail = PIPE_CAP.saturating_sub(pipe.input.len());
    let n = core::cmp::min(avail, data.len());
    if n < data.len() {
        log::warn!(
            "[ssh_pipe] input buffer for channel {} full, dropped {} bytes",
            channel_id,
            data.len() - n
        );
    }
    pipe.input.extend(data[..n].iter().copied());
    n
}

/// Drain everything currently buffered as input for the given channel.
///
/// Returns an empty vector if the channel doesn't exist or has nothing
/// pending. Used by the dashboard to feed SSH input into a shell pane's
/// `InputBuffer` once per tick.
pub fn drain_input(channel_id: u32) -> Vec<u8> {
    let mut map = SSH_PIPES.lock();
    let Some(pipe) = map.get_mut(&channel_id) else {
        return Vec::new();
    };
    let n = pipe.input.len();
    if n == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n);
    out.extend(pipe.input.drain(..));
    out
}

/// Push shell-pane output bytes toward the SSH client.
pub fn push_output(channel_id: u32, data: &[u8]) -> usize {
    let mut map = SSH_PIPES.lock();
    let Some(pipe) = map.get_mut(&channel_id) else {
        // Not all shell panes are SSH-attached; this is a no-op for them.
        return 0;
    };
    let avail = PIPE_CAP.saturating_sub(pipe.output.len());
    let n = core::cmp::min(avail, data.len());
    if n < data.len() {
        log::warn!(
            "[ssh_pipe] output buffer for channel {} full, dropped {} bytes",
            channel_id,
            data.len() - n
        );
    }
    pipe.output.extend(data[..n].iter().copied());
    n
}

/// Drain everything currently buffered as output for the given channel.
///
/// Used by the SSH server when forwarding shell pane output back over the
/// network channel.
pub fn drain_output(channel_id: u32) -> Vec<u8> {
    let mut map = SSH_PIPES.lock();
    let Some(pipe) = map.get_mut(&channel_id) else {
        return Vec::new();
    };
    let n = pipe.output.len();
    if n == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n);
    out.extend(pipe.output.drain(..));
    out
}

/// Snapshot every channel id that currently has pending output bytes.
///
/// The SSH server iterates this each tick to know which channels to flush.
pub fn channels_with_pending_output() -> Vec<u32> {
    let map = SSH_PIPES.lock();
    map.iter()
        .filter_map(|(id, pipe)| if pipe.output.is_empty() { None } else { Some(*id) })
        .collect()
}

/// Snapshot every channel id that currently has registered pipes (regardless
/// of buffer state). The dashboard uses this to know which shell panes need
/// SSH input draining each tick.
pub fn registered_channels() -> Vec<u32> {
    let map = SSH_PIPES.lock();
    map.keys().copied().collect()
}
