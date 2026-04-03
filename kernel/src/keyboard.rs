//! Async keyboard input system.
//!
//! The keyboard ISR pushes raw scancodes into a shared queue.
//! [`ScancodeStream`] provides an async interface that decodes scancodes
//! into [`DecodedKey`] values using the `pc-keyboard` crate.

extern crate alloc;

use alloc::collections::VecDeque;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use spin::Mutex;

/// Ring buffer of raw scancodes, written by the ISR, read by the async stream.
static SCANCODE_QUEUE: Mutex<VecDeque<u8>> = Mutex::new(VecDeque::new());

/// Waker stored by the async reader so the ISR can wake it when a key arrives.
static KEYBOARD_WAKER: Mutex<Option<Waker>> = Mutex::new(None);

/// The shared `pc-keyboard` decoder instance.
static KEYBOARD_DECODER: Mutex<Option<Keyboard<layouts::Us104Key, ScancodeSet1>>> =
    Mutex::new(None);

/// Initialize the keyboard decoder. Call once during boot, after the heap is available.
pub fn init() {
    *KEYBOARD_DECODER.lock() = Some(Keyboard::new(
        ScancodeSet1::new(),
        layouts::Us104Key,
        HandleControl::MapLettersToUnicode,
    ));
    log::info!("[kbd] keyboard decoder initialized");
}

/// Called from the keyboard ISR in `interrupts.rs`.
///
/// Pushes a raw scancode into the queue and wakes the async reader.
/// This function is called with interrupts disabled (inside an ISR),
/// so it must not allocate or block.
pub fn push_scancode(scancode: u8) {
    SCANCODE_QUEUE.lock().push_back(scancode);

    // Wake the async reader if one is waiting.
    if let Some(waker) = KEYBOARD_WAKER.lock().take() {
        waker.wake();
    }
}

/// Async stream of decoded keyboard events.
///
/// Usage:
/// ```ignore
/// let stream = ScancodeStream::new();
/// loop {
///     let key = stream.next_key().await;
///     // handle key ...
/// }
/// ```
pub struct ScancodeStream;

impl ScancodeStream {
    pub fn new() -> Self {
        ScancodeStream
    }

    /// Wait asynchronously for the next decoded keypress.
    ///
    /// Returns when a scancode in the queue decodes to a full key event.
    /// Scancodes that do not produce a key (e.g. break codes for modifier
    /// releases) are consumed silently and the future keeps waiting.
    pub async fn next_key(&self) -> DecodedKey {
        NextKey.await
    }

    /// Non-blocking check for the next decoded keypress.
    ///
    /// Drains available scancodes through the decoder. Returns `Some(key)` if
    /// a full key event is available, `None` if the queue is empty or only
    /// contains partial/modifier scancodes.
    pub fn try_next_key(&self) -> Option<DecodedKey> {
        loop {
            let scancode = x86_64::instructions::interrupts::without_interrupts(|| {
                SCANCODE_QUEUE.lock().pop_front()
            });
            match scancode {
                Some(code) => {
                    let decoded = x86_64::instructions::interrupts::without_interrupts(|| {
                        let mut decoder = KEYBOARD_DECODER.lock();
                        let decoder = decoder.as_mut()?;
                        if let Ok(Some(key_event)) = decoder.add_byte(code) {
                            decoder.process_keyevent(key_event)
                        } else {
                            None
                        }
                    });
                    if decoded.is_some() {
                        return decoded;
                    }
                    // Keep draining — might be a modifier scancode
                }
                None => return None,
            }
        }
    }
}

/// Future that resolves to the next [`DecodedKey`] from the scancode queue.
///
/// On each poll it drains available scancodes through the `pc-keyboard`
/// decoder. If no decoded key is produced, it stores its waker in
/// [`KEYBOARD_WAKER`] and returns `Pending`.
struct NextKey;

impl Future for NextKey {
    type Output = DecodedKey;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<DecodedKey> {
        // Try to decode scancodes while any are queued.
        loop {
            let scancode = x86_64::instructions::interrupts::without_interrupts(|| {
                SCANCODE_QUEUE.lock().pop_front()
            });

            match scancode {
                Some(code) => {
                    // Feed the scancode to the decoder.
                    let decoded = x86_64::instructions::interrupts::without_interrupts(|| {
                        let mut decoder = KEYBOARD_DECODER.lock();
                        let decoder = decoder.as_mut().expect("keyboard decoder not initialized");
                        if let Ok(Some(key_event)) = decoder.add_byte(code) {
                            decoder.process_keyevent(key_event)
                        } else {
                            None
                        }
                    });

                    if let Some(key) = decoded {
                        return Poll::Ready(key);
                    }
                    // Scancode didn't produce a key (e.g. modifier release) —
                    // keep draining the queue.
                }
                None => {
                    // Queue is empty — register our waker and return Pending.
                    x86_64::instructions::interrupts::without_interrupts(|| {
                        *KEYBOARD_WAKER.lock() = Some(cx.waker().clone());
                    });

                    // Double-check: a scancode may have arrived between the
                    // pop_front and storing the waker.
                    let recheck = x86_64::instructions::interrupts::without_interrupts(|| {
                        SCANCODE_QUEUE.lock().pop_front()
                    });

                    if let Some(code) = recheck {
                        let decoded = x86_64::instructions::interrupts::without_interrupts(|| {
                            let mut decoder = KEYBOARD_DECODER.lock();
                            let decoder =
                                decoder.as_mut().expect("keyboard decoder not initialized");
                            if let Ok(Some(key_event)) = decoder.add_byte(code) {
                                decoder.process_keyevent(key_event)
                            } else {
                                None
                            }
                        });

                        if let Some(key) = decoded {
                            return Poll::Ready(key);
                        }
                        // Still no decoded key from the recheck scancode,
                        // continue waiting.
                    }

                    return Poll::Pending;
                }
            }
        }
    }
}
