//! Combined serial + framebuffer + VFS file logger using the `log` crate.
//!
//! Sinks:
//! 1. **Serial** (port 0x3F8) — fires unconditionally for every record. Works
//!    from the very first line of `kernel_main`, before the heap exists.
//! 2. **vconsole kernel log ring buffer** — fires once `HEAP_READY` is set
//!    (formatting requires `format!()`). Powers the in-OS kernel log viewer.
//! 3. **VFS file** (`/claudio/logs/kernel.log`) — fires once the `claudio_fs`
//!    backend is installed by `storage::init()`. Lines logged before that point
//!    are stashed in an in-memory ring buffer and drained to the file by
//!    [`flush_ring_buffer_to_vfs`], which `kernel_main` calls right after
//!    storage init.
//!
//! Important details:
//! - The file sink uses a re-entry guard ([`IN_FILE_SINK`]) because
//!   `claudio_fs::{read_file,write_file}` themselves call `log::debug!`.
//!   Without the guard the file path would recurse into itself and either
//!   deadlock the VFS lock or burn the stack.
//! - File writes are batched: lines accumulate in [`PENDING`] and are flushed
//!   in groups of [`FLUSH_BATCH_SIZE`] or on explicit [`flush`] calls. Each
//!   flush is a read-modify-write because `claudio_fs` does not expose an
//!   append primitive.
//! - All buffers (`RING_BUFFER`, `PENDING`) are gated on `HEAP_READY` because
//!   formatting and `VecDeque` push need the allocator. Pre-heap log lines
//!   still reach the serial port — they just are not retained in memory.

extern crate alloc;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

/// Cap on the pre-VFS ring buffer (number of log lines retained).
const RING_BUFFER_CAPACITY: usize = 256;

/// Maximum number of pending lines we batch before forcing a VFS flush.
const FLUSH_BATCH_SIZE: usize = 16;

/// Cap on the post-VFS pending batch (defensive — if a flush keeps failing
/// we don't want to grow without bound).
const PENDING_CAPACITY: usize = 1024;

/// Path to the on-disk kernel log file. The parent directory is created by
/// `storage::init()` so we never have to mkdir from inside the logger.
const KERNEL_LOG_PATH: &str = "/claudio/logs/kernel.log";

/// Pre-VFS in-memory ring of formatted log lines. Drained to the VFS by
/// [`flush_ring_buffer_to_vfs`] once the filesystem backend is installed.
static RING_BUFFER: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Post-VFS pending lines waiting to be batched into the file. Drained on
/// every `FLUSH_BATCH_SIZE`th line and on explicit [`flush`] calls.
static PENDING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Set once `flush_ring_buffer_to_vfs` has run, signaling that all subsequent
/// lines may attempt to hit the file sink.
static FILE_SINK_READY: AtomicBool = AtomicBool::new(false);

/// Re-entry guard for the file sink. `claudio_fs::{read_file,write_file}`
/// call `log::debug!` internally; without this flag the file path would
/// recurse and either deadlock the VFS mutex or run away on the stack.
static IN_FILE_SINK: AtomicBool = AtomicBool::new(false);

/// Counter used to decide when to drain `PENDING` to the VFS. Incremented on
/// every successful enqueue.
static PENDING_COUNT: AtomicUsize = AtomicUsize::new(0);

struct KernelLogger;

impl log::Log for KernelLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        // ── Sink 1: serial (always available, no allocator required) ──
        crate::serial_println!("[{:5}] {}", record.level(), record.args());

        // Everything below this point allocates, so it has to wait for the
        // heap. Pre-heap (Phases -1..1) we silently drop the buffered copies;
        // the serial line above is enough to debug those.
        if !crate::memory::HEAP_READY.load(Ordering::Acquire) {
            return;
        }

        // If we are *inside* the file sink (claudio_fs::write_file is itself
        // calling log::debug!), we must not touch any of the buffers either —
        // PENDING is locked by the outer call. Just emit serial above and bail.
        if IN_FILE_SINK.load(Ordering::Acquire) {
            return;
        }

        let line = format!("[{:5}] {}", record.level(), record.args());

        // ── Sink 2: vconsole kernel log (in-OS viewer) ────────────────
        crate::vconsole::push_kernel_log(&line);

        // ── Sink 3: VFS file ──────────────────────────────────────────
        if FILE_SINK_READY.load(Ordering::Acquire) {
            // Storage is up — enqueue into PENDING and maybe flush.
            {
                let mut pending = PENDING.lock();
                if pending.len() >= PENDING_CAPACITY {
                    // Drop the oldest to keep the cap. Logging-induced OOM is
                    // worse than dropping a line.
                    pending.pop_front();
                }
                pending.push_back(line);
            }
            let n = PENDING_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            if n % FLUSH_BATCH_SIZE == 0 {
                flush_pending_to_vfs();
            }
        } else {
            // Pre-storage — stash in the ring buffer for later replay.
            let mut ring = RING_BUFFER.lock();
            if ring.len() >= RING_BUFFER_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(line);
        }
    }

    fn flush(&self) {
        flush();
    }
}

static LOGGER: KernelLogger = KernelLogger;

pub fn init() {
    log::set_logger(&LOGGER).expect("logger already initialized");
    log::set_max_level(log::LevelFilter::Trace);
}

/// Drain the pre-VFS ring buffer into the kernel log file and arm the
/// post-storage file sink. Called from `kernel_main` immediately after
/// `storage::init()` — at that point the heap is up, the VFS is mounted,
/// `/claudio/logs` exists, and `claudio_fs` has a backend.
///
/// Safe to call more than once: the second call will find an empty ring
/// buffer and just re-arm the flag.
pub fn flush_ring_buffer_to_vfs() {
    // Snapshot the ring buffer (drop the lock before any VFS work).
    let drained: Vec<String> = {
        let mut ring = RING_BUFFER.lock();
        ring.drain(..).collect()
    };

    if !drained.is_empty() {
        // Build a single payload: existing file (if any) + drained lines.
        // Use the re-entry guard so claudio_fs's internal log::debug! calls
        // don't recurse back into us.
        if IN_FILE_SINK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            let mut payload: Vec<u8> = claudio_fs::read_file(KERNEL_LOG_PATH).unwrap_or_default();
            for line in &drained {
                payload.extend_from_slice(line.as_bytes());
                payload.push(b'\n');
            }
            let _ = claudio_fs::write_file(KERNEL_LOG_PATH, &payload);
            IN_FILE_SINK.store(false, Ordering::Release);
        }
    }

    FILE_SINK_READY.store(true, Ordering::Release);

    // Echo a confirmation through the normal logger so it's visible on
    // serial / framebuffer / file. The file sink is armed now, so this line
    // will be enqueued into PENDING normally.
    log::info!(
        "[logger] file sink armed -> {} (drained {} buffered lines)",
        KERNEL_LOG_PATH,
        drained.len(),
    );
}

/// Drain the pending batch to the VFS. Idempotent — does nothing if pending
/// is empty, the file sink is not yet armed, or we're already inside a flush.
pub fn flush() {
    if !FILE_SINK_READY.load(Ordering::Acquire) {
        return;
    }
    flush_pending_to_vfs();
}

/// Internal: drain `PENDING` and append it to the kernel log file via RMW.
///
/// `claudio_fs` has no append primitive, so we read the existing file (if
/// any), concatenate the pending lines, and write it back. The re-entry
/// guard prevents `claudio_fs`'s own `log::debug!` calls from recursing
/// into the file sink during the read or write.
fn flush_pending_to_vfs() {
    // Re-entry guard FIRST — if we're already inside a flush (because
    // claudio_fs is calling log::debug! which is calling us), bail.
    if IN_FILE_SINK
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    // Snapshot pending under its own lock, then drop it before touching VFS.
    let drained: Vec<String> = {
        let mut pending = PENDING.lock();
        if pending.is_empty() {
            IN_FILE_SINK.store(false, Ordering::Release);
            return;
        }
        pending.drain(..).collect()
    };

    let mut payload: Vec<u8> = claudio_fs::read_file(KERNEL_LOG_PATH).unwrap_or_default();
    for line in &drained {
        payload.extend_from_slice(line.as_bytes());
        payload.push(b'\n');
    }
    let _ = claudio_fs::write_file(KERNEL_LOG_PATH, &payload);

    IN_FILE_SINK.store(false, Ordering::Release);
}
