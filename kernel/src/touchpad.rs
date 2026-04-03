//! PS/2 Touchpad Driver — Synaptics/ALPS detection, absolute mode, gestures.
//!
//! Detects PS/2 touchpads via the IDENTIFY command sequence, switches to
//! absolute mode for coordinate tracking, and generates `MouseEvent`s that
//! integrate with the existing `mouse.rs` cursor system.
//!
//! ## Supported hardware
//!
//! - Synaptics PS/2 touchpads (most common on laptops)
//! - ALPS PS/2 touchpads (older ThinkPads, some Dell/HP)
//! - Generic PS/2 mouse fallback (if no touchpad is detected)
//!
//! ## Gesture support
//!
//! - Single finger move: cursor movement
//! - Single finger tap: left click
//! - Two-finger scroll: vertical/horizontal scrolling
//! - Two-finger tap: right click
//! - Palm rejection: ignore large contact areas

extern crate alloc;

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

// Touch events are translated to mouse reports via crate::mouse::feed_report().

// ---------------------------------------------------------------------------
// PS/2 controller ports
// ---------------------------------------------------------------------------

/// PS/2 data port.
const PS2_DATA: u16 = 0x60;
/// PS/2 command/status port.
const PS2_CMD: u16 = 0x64;

/// Status register bits.
const PS2_OUTPUT_FULL: u8 = 1 << 0;
const PS2_INPUT_FULL: u8 = 1 << 1;

// ---------------------------------------------------------------------------
// Touchpad type detection
// ---------------------------------------------------------------------------

/// Detected touchpad hardware type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchpadType {
    /// No touchpad detected; device is a plain PS/2 mouse.
    None,
    /// Synaptics touchpad (identified via 0x47 response to IDENTIFY).
    Synaptics,
    /// ALPS touchpad (identified via specific E6/E7 report sequence).
    Alps,
    /// Generic PS/2 pointing device with basic 3-byte packets.
    GenericPS2,
}

/// The detected touchpad type, set during `init()`.
static TOUCHPAD_TYPE: AtomicU8 = AtomicU8::new(TouchpadType::None as u8);

/// Whether the touchpad driver is initialized and active.
static TOUCHPAD_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Get the detected touchpad type.
pub fn detected_type() -> TouchpadType {
    match TOUCHPAD_TYPE.load(Ordering::Relaxed) {
        0 => TouchpadType::None,
        1 => TouchpadType::Synaptics,
        2 => TouchpadType::Alps,
        3 => TouchpadType::GenericPS2,
        _ => TouchpadType::None,
    }
}

// ---------------------------------------------------------------------------
// Touch events
// ---------------------------------------------------------------------------

/// A touch event from the touchpad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchEvent {
    /// Single finger movement (absolute x, y in touchpad coordinates).
    Move(u16, u16),
    /// Single finger tap (generates left click).
    Tap,
    /// Two-finger scroll (dx, dy in scroll units).
    TwoFingerScroll(i16, i16),
    /// Two-finger tap (generates right click).
    TwoFingerTap,
}

// ---------------------------------------------------------------------------
// Touchpad state
// ---------------------------------------------------------------------------

/// Internal touchpad tracking state.
struct TouchpadState {
    /// Screen dimensions for coordinate mapping.
    screen_w: u16,
    screen_h: u16,

    /// Touchpad resolution (varies by model).
    tp_max_x: u16,
    tp_max_y: u16,

    /// Previous absolute position (for delta calculation).
    prev_x: u16,
    prev_y: u16,
    /// Whether we have a valid previous position.
    has_prev: bool,

    /// Number of fingers currently detected.
    finger_count: u8,
    /// Previous finger count (for tap detection).
    prev_finger_count: u8,

    /// Finger-down tick (for tap vs drag detection).
    finger_down_tick: u64,
    /// Whether the current touch has moved significantly (not a tap).
    moved_significantly: bool,

    /// Two-finger scroll accumulator.
    scroll_accum_x: i32,
    scroll_accum_y: i32,

    /// Previous two-finger midpoint (for scroll delta).
    prev_two_x: u16,
    prev_two_y: u16,
    has_prev_two: bool,

    /// Sensitivity multiplier (1-10, default 5).
    sensitivity: u8,

    /// Palm rejection: contact size threshold (arbitrary units).
    palm_width_threshold: u8,

    /// Packet buffer for multi-byte PS/2 packets.
    packet_buf: [u8; 6],
    packet_idx: usize,
    /// Expected packet size (3 for basic, 6 for Synaptics absolute).
    packet_size: usize,
}

impl TouchpadState {
    fn new(screen_w: u16, screen_h: u16) -> Self {
        Self {
            screen_w,
            screen_h,
            tp_max_x: 6143,  // Synaptics default max X
            tp_max_y: 6143,  // Synaptics default max Y
            prev_x: 0,
            prev_y: 0,
            has_prev: false,
            finger_count: 0,
            prev_finger_count: 0,
            finger_down_tick: 0,
            moved_significantly: false,
            scroll_accum_x: 0,
            scroll_accum_y: 0,
            prev_two_x: 0,
            prev_two_y: 0,
            has_prev_two: false,
            sensitivity: 5,
            palm_width_threshold: 10,
            packet_buf: [0; 6],
            packet_idx: 0,
            packet_size: 6, // Synaptics absolute mode default
        }
    }

    /// Map touchpad absolute coordinates to screen pixel coordinates.
    fn map_to_screen(&self, tp_x: u16, tp_y: u16) -> (i32, i32) {
        let sx = ((tp_x as u32) * (self.screen_w as u32)) / (self.tp_max_x as u32);
        // Touchpad Y is inverted (0 = bottom on Synaptics).
        let inverted_y = self.tp_max_y.saturating_sub(tp_y);
        let sy = ((inverted_y as u32) * (self.screen_h as u32)) / (self.tp_max_y as u32);
        (sx.min(self.screen_w as u32 - 1) as i32,
         sy.min(self.screen_h as u32 - 1) as i32)
    }
}

static TOUCHPAD_STATE: Mutex<Option<TouchpadState>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// PS/2 low-level helpers
// ---------------------------------------------------------------------------

/// Wait for the PS/2 output buffer to be full (data ready to read).
fn ps2_wait_output() -> bool {
    for _ in 0..100_000 {
        let status: u8 = unsafe { Port::<u8>::new(PS2_CMD).read() };
        if status & PS2_OUTPUT_FULL != 0 {
            return true;
        }
    }
    false
}

/// Wait for the PS/2 input buffer to be empty (ready to write).
fn ps2_wait_input() -> bool {
    for _ in 0..100_000 {
        let status: u8 = unsafe { Port::<u8>::new(PS2_CMD).read() };
        if status & PS2_INPUT_FULL == 0 {
            return true;
        }
    }
    false
}

/// Send a command byte to the PS/2 controller command port.
fn ps2_controller_cmd(cmd: u8) {
    ps2_wait_input();
    unsafe { Port::<u8>::new(PS2_CMD).write(cmd); }
}

/// Send a byte to the PS/2 data port (auxiliary device via controller).
fn ps2_send_aux(byte: u8) -> Option<u8> {
    // Tell controller to send the next data byte to the aux (mouse/touchpad) device.
    ps2_controller_cmd(0xD4);
    ps2_wait_input();
    unsafe { Port::<u8>::new(PS2_DATA).write(byte); }

    // Read the ACK.
    if ps2_wait_output() {
        Some(unsafe { Port::<u8>::new(PS2_DATA).read() })
    } else {
        None
    }
}

/// Send a command to the aux device and read the response byte.
fn ps2_aux_cmd_response(cmd: u8) -> Option<u8> {
    let ack = ps2_send_aux(cmd)?;
    if ack != 0xFA {
        log::debug!("[touchpad] aux cmd {:#X}: unexpected ACK {:#X}", cmd, ack);
    }
    // Read the response byte.
    if ps2_wait_output() {
        Some(unsafe { Port::<u8>::new(PS2_DATA).read() })
    } else {
        None
    }
}

/// Flush any pending data from the PS/2 output buffer.
fn ps2_flush() {
    for _ in 0..16 {
        let status: u8 = unsafe { Port::<u8>::new(PS2_CMD).read() };
        if status & PS2_OUTPUT_FULL == 0 {
            break;
        }
        let _ = unsafe { Port::<u8>::new(PS2_DATA).read() };
    }
}

// ---------------------------------------------------------------------------
// Synaptics detection and setup
// ---------------------------------------------------------------------------

/// Attempt to identify a Synaptics touchpad.
///
/// Synaptics touchpads respond to the "Identify" sequence:
///   SET_SAMPLE_RATE 10, SET_SAMPLE_RATE 0, GET_DEVICE_ID
/// If byte 1 of the response is 0x47, it's a Synaptics device.
fn detect_synaptics() -> bool {
    log::debug!("[touchpad] probing for Synaptics touchpad...");

    // Send the magic identify sequence: set sample rates 10, 0, then get ID.
    // SET_SAMPLE_RATE = 0xF3
    ps2_send_aux(0xF3); // SET_SAMPLE_RATE
    ps2_send_aux(10);
    ps2_send_aux(0xF3);
    ps2_send_aux(0);
    // GET_DEVICE_ID = 0xF2
    ps2_send_aux(0xF2);

    // Read the device ID response.
    if !ps2_wait_output() { return false; }
    let id = unsafe { Port::<u8>::new(PS2_DATA).read() };

    log::debug!("[touchpad] Synaptics probe: device ID = {:#X}", id);

    // Synaptics returns 0x47 as the device ID after the identify sequence.
    id == 0x47
}

/// Enable Synaptics absolute mode.
///
/// Sends the mode byte via the "Set Mode" sequence:
///   SET_SAMPLE_RATE 20, SET_SAMPLE_RATE mode_byte, GET_DEVICE_ID
fn synaptics_set_absolute_mode() -> bool {
    log::debug!("[touchpad] enabling Synaptics absolute mode...");

    // Mode byte: 0xC1 = absolute + W mode (for finger width/count).
    // Bit 7: absolute mode
    // Bit 6: high packet rate
    // Bit 0: W mode (finger width)
    let mode_byte: u8 = 0xC1;

    ps2_send_aux(0xF3); // SET_SAMPLE_RATE
    ps2_send_aux(20);
    ps2_send_aux(0xF3);
    ps2_send_aux(mode_byte);
    ps2_send_aux(0xF2); // GET_DEVICE_ID

    if ps2_wait_output() {
        let resp = unsafe { Port::<u8>::new(PS2_DATA).read() };
        log::info!("[touchpad] Synaptics absolute mode set (resp={:#X})", resp);
        true
    } else {
        log::warn!("[touchpad] Synaptics mode set timed out");
        false
    }
}

// ---------------------------------------------------------------------------
// ALPS detection
// ---------------------------------------------------------------------------

/// Attempt to identify an ALPS touchpad via the E6/E7 report sequence.
///
/// ALPS touchpads respond to:
///   SET_RESOLUTION(0) x3, GET_DEVICE_ID -> returns ALPS signature bytes.
fn detect_alps() -> bool {
    log::debug!("[touchpad] probing for ALPS touchpad...");

    // E6 report: SET_SCALING 1:1 (0xE6) x3, then GET_STATUS (0xE9).
    for _ in 0..3 {
        ps2_send_aux(0xE6); // SET_SCALING 1:1
    }
    ps2_send_aux(0xE9); // STATUS REQUEST

    // Read 3-byte status response.
    let mut resp = [0u8; 3];
    for byte in resp.iter_mut() {
        if ps2_wait_output() {
            *byte = unsafe { Port::<u8>::new(PS2_DATA).read() };
        } else {
            return false;
        }
    }

    log::debug!("[touchpad] ALPS E6 report: [{:#X}, {:#X}, {:#X}]",
        resp[0], resp[1], resp[2]);

    // Known ALPS E6 report signatures.
    // 0x00,0x00,0x0A = ALPS v1
    // 0x00,0x00,0x64 = ALPS v2
    // 0x73,0x02,0x64 = ALPS v3
    matches!(
        (resp[0], resp[1], resp[2]),
        (0x00, 0x00, 0x0A) |
        (0x00, 0x00, 0x64) |
        (0x73, 0x02, 0x64) |
        (0x73, 0x02, 0x50)
    )
}

// ---------------------------------------------------------------------------
// Packet processing
// ---------------------------------------------------------------------------

/// Process a complete Synaptics absolute-mode packet (6 bytes).
///
/// Synaptics 6-byte absolute packet format:
///   Byte 0: [1][0][F][x11:x8 bits][1][0][R][L]
///   Byte 1: Y[7:0]
///   Byte 2: Z (pressure)
///   Byte 3: [1][1][Y[11:8]][X[11:8]][1][0][R][L]
///   Byte 4: X[7:0]
///   Byte 5: Y[7:0] (duplicate) or W (finger width) depending on mode
fn process_synaptics_packet(state: &mut TouchpadState) {
    let pkt = &state.packet_buf;

    // Validate sync bits.
    if pkt[0] & 0xC0 != 0x80 || pkt[3] & 0xC0 != 0xC0 {
        log::trace!("[touchpad] bad sync: [{:#X},{:#X},{:#X},{:#X},{:#X},{:#X}]",
            pkt[0], pkt[1], pkt[2], pkt[3], pkt[4], pkt[5]);
        return;
    }

    // Extract X coordinate (12 bits).
    let x_lo = pkt[4] as u16;
    let x_hi = ((pkt[3] as u16 >> 4) & 0x0F) | (((pkt[0] as u16 >> 4) & 0x0F) << 4);
    let x = (x_hi << 8) | x_lo;

    // Extract Y coordinate (12 bits).
    let y_lo = pkt[1] as u16;
    let y_hi = ((pkt[3] as u16) & 0x0F) | (((pkt[0] as u16) & 0x03) << 4);
    let y = (y_hi << 8) | y_lo;

    // Z = pressure (0 = no finger, >30 = finger touching).
    let z = pkt[2];

    // W = finger width (from byte 5 in W mode, or from bits in byte 0/3).
    // In W mode: W = ((pkt[0] & 0x30) >> 2) | ((pkt[3] & 0x04) >> 1) | ((pkt[0] & 0x04) >> 2)
    let w = ((pkt[0] & 0x30) >> 2) | ((pkt[3] & 0x04) >> 1) | ((pkt[0] & 0x04) >> 2);

    // Buttons.
    let left = pkt[0] & 0x01 != 0;
    let right = pkt[0] & 0x02 != 0;

    // Palm rejection: if W >= threshold, ignore.
    if w >= state.palm_width_threshold {
        log::trace!("[touchpad] palm rejected: w={}", w);
        state.has_prev = false;
        return;
    }

    // Determine finger count from W value.
    // W=0: two fingers, W=1: three+ fingers, W>=4: one finger
    let fingers = if z < 25 {
        0u8 // no finger
    } else if w == 0 {
        2
    } else if w == 1 {
        3
    } else {
        1
    };

    let prev_fingers = state.finger_count;
    state.prev_finger_count = prev_fingers;
    state.finger_count = fingers;

    let tick = crate::interrupts::tick_count();

    if fingers == 0 {
        // Finger lifted — check for tap.
        if prev_fingers == 1 && !state.moved_significantly {
            let held = tick.saturating_sub(state.finger_down_tick);
            if held < 10 {
                // Short tap = left click.
                crate::mouse::feed_report(&[0x01, 0, 0]); // button down
                crate::mouse::feed_report(&[0x00, 0, 0]); // button up
                log::trace!("[touchpad] tap -> left click");
            }
        } else if prev_fingers == 2 && !state.moved_significantly {
            let held = tick.saturating_sub(state.finger_down_tick);
            if held < 10 {
                // Two-finger tap = right click.
                crate::mouse::feed_report(&[0x02, 0, 0]); // right button down
                crate::mouse::feed_report(&[0x00, 0, 0]); // right button up
                log::trace!("[touchpad] two-finger tap -> right click");
            }
        }

        state.has_prev = false;
        state.has_prev_two = false;
        state.moved_significantly = false;
        state.scroll_accum_x = 0;
        state.scroll_accum_y = 0;
        return;
    }

    // Finger down transition.
    if prev_fingers == 0 {
        state.finger_down_tick = tick;
        state.moved_significantly = false;
        state.has_prev = false;
        state.has_prev_two = false;
    }

    if fingers == 1 {
        // Single finger: cursor movement.
        if state.has_prev {
            let dx_raw = x as i32 - state.prev_x as i32;
            let dy_raw = -(y as i32 - state.prev_y as i32); // invert Y

            // Apply sensitivity scaling.
            let scale = state.sensitivity as i32;
            let dx = (dx_raw * scale) / 50;
            let dy = (dy_raw * scale) / 50;

            if dx.unsigned_abs() > 2 || dy.unsigned_abs() > 2 {
                state.moved_significantly = true;
            }

            if dx != 0 || dy != 0 {
                // Convert to relative mouse report.
                let dx_clamped = dx.clamp(-127, 127) as i8;
                let dy_clamped = dy.clamp(-127, 127) as i8;

                let mut buttons: u8 = 0;
                if left { buttons |= 0x01; }
                if right { buttons |= 0x02; }

                crate::mouse::feed_report(&[
                    buttons,
                    dx_clamped as u8,
                    dy_clamped as u8,
                ]);
            }
        }

        state.prev_x = x;
        state.prev_y = y;
        state.has_prev = true;
        state.has_prev_two = false;

    } else if fingers == 2 {
        // Two fingers: scrolling.
        if state.has_prev_two {
            let dy_raw = y as i32 - state.prev_two_y as i32;
            let dx_raw = x as i32 - state.prev_two_x as i32;

            state.scroll_accum_y += dy_raw;
            state.scroll_accum_x += dx_raw;

            // Emit scroll events when accumulator exceeds threshold.
            let scroll_threshold = 100i32;

            if state.scroll_accum_y.abs() > scroll_threshold {
                let scroll_units = (state.scroll_accum_y / scroll_threshold).clamp(-127, 127) as i8;
                state.scroll_accum_y %= scroll_threshold;

                if scroll_units != 0 {
                    state.moved_significantly = true;
                    // Feed as scroll via 4-byte mouse report.
                    crate::mouse::feed_report(&[0, 0, 0, scroll_units as u8]);
                    log::trace!("[touchpad] two-finger scroll: dy={}", scroll_units);
                }
            }

            if state.scroll_accum_x.abs() > scroll_threshold {
                // Horizontal scroll — not directly supported by basic mouse protocol,
                // but we log it for future use.
                state.scroll_accum_x %= scroll_threshold;
                state.moved_significantly = true;
            }
        }

        state.prev_two_x = x;
        state.prev_two_y = y;
        state.has_prev_two = true;
        state.has_prev = false;
    }
}

/// Process a basic 3-byte PS/2 mouse packet (generic/ALPS fallback).
fn process_generic_packet(state: &mut TouchpadState) {
    let pkt = &state.packet_buf;

    // Standard PS/2 3-byte packet:
    // Byte 0: [YO][XO][YS][XS][1][M][R][L]
    // Byte 1: X movement (signed, combined with XS and XO)
    // Byte 2: Y movement (signed, combined with YS and YO)
    let buttons = pkt[0] & 0x07; // L, R, M
    let dx = pkt[1] as i8;
    let dy = -(pkt[2] as i8); // PS/2 Y is inverted vs screen

    // Apply sensitivity.
    let scale = state.sensitivity as i32;
    let sdx = ((dx as i32) * scale) / 5;
    let sdy = ((dy as i32) * scale) / 5;

    let dx_out = sdx.clamp(-127, 127) as i8;
    let dy_out = sdy.clamp(-127, 127) as i8;

    crate::mouse::feed_report(&[buttons, dx_out as u8, dy_out as u8]);
}

// ---------------------------------------------------------------------------
// Byte-level input from IRQ12
// ---------------------------------------------------------------------------

/// Feed a single byte from the PS/2 aux port (IRQ12) into the touchpad
/// packet assembler.
///
/// Called from the keyboard/aux interrupt handler when the aux flag is set
/// in the PS/2 status register.
pub fn feed_byte(byte: u8) {
    let mut guard = TOUCHPAD_STATE.lock();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return, // Not initialized.
    };

    state.packet_buf[state.packet_idx] = byte;
    state.packet_idx += 1;

    if state.packet_idx >= state.packet_size {
        state.packet_idx = 0;

        match detected_type() {
            TouchpadType::Synaptics => process_synaptics_packet(state),
            TouchpadType::Alps | TouchpadType::GenericPS2 => process_generic_packet(state),
            TouchpadType::None => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Set touchpad sensitivity (1 = slowest, 10 = fastest).
pub fn set_sensitivity(level: u8) {
    let level = level.clamp(1, 10);
    let mut guard = TOUCHPAD_STATE.lock();
    if let Some(state) = guard.as_mut() {
        state.sensitivity = level;
        log::info!("[touchpad] sensitivity set to {}", level);
    }
}

/// Get current sensitivity level.
pub fn sensitivity() -> u8 {
    let guard = TOUCHPAD_STATE.lock();
    guard.as_ref().map(|s| s.sensitivity).unwrap_or(5)
}

/// Set palm rejection width threshold (higher = more permissive).
pub fn set_palm_threshold(threshold: u8) {
    let mut guard = TOUCHPAD_STATE.lock();
    if let Some(state) = guard.as_mut() {
        state.palm_width_threshold = threshold;
        log::info!("[touchpad] palm rejection threshold set to {}", threshold);
    }
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize the touchpad subsystem.
///
/// Probes the PS/2 aux port for Synaptics and ALPS touchpads, enables
/// absolute mode if supported, and sets up the packet state machine.
///
/// Call after PS/2 controller and mouse subsystem are initialized.
pub fn init() {
    log::info!("[touchpad] ============================================");
    log::info!("[touchpad]   Touchpad Detection & Init");
    log::info!("[touchpad] ============================================");

    let screen_w = crate::framebuffer::width() as u16;
    let screen_h = crate::framebuffer::height() as u16;

    if screen_w == 0 || screen_h == 0 {
        log::warn!("[touchpad] framebuffer not available, touchpad disabled");
        return;
    }

    // Flush any stale data.
    ps2_flush();

    // Enable the PS/2 aux port (second PS/2 channel).
    ps2_controller_cmd(0xA8); // Enable second PS/2 port

    // Reset the aux device.
    log::debug!("[touchpad] resetting aux device...");
    let reset_ack = ps2_send_aux(0xFF); // RESET
    if let Some(ack) = reset_ack {
        log::debug!("[touchpad] reset ACK: {:#X}", ack);
        // Read self-test result (0xAA = pass) and device ID.
        if ps2_wait_output() {
            let self_test = unsafe { Port::<u8>::new(PS2_DATA).read() };
            log::debug!("[touchpad] self-test: {:#X}", self_test);
        }
        if ps2_wait_output() {
            let dev_id = unsafe { Port::<u8>::new(PS2_DATA).read() };
            log::debug!("[touchpad] device ID: {:#X}", dev_id);
        }
    }

    ps2_flush();

    // Probe for Synaptics.
    let mut tp_type = TouchpadType::None;
    let mut packet_size = 3usize;

    if detect_synaptics() {
        log::info!("[touchpad] Synaptics touchpad detected!");
        if synaptics_set_absolute_mode() {
            tp_type = TouchpadType::Synaptics;
            packet_size = 6;
        } else {
            log::warn!("[touchpad] failed to set Synaptics absolute mode, using generic");
            tp_type = TouchpadType::GenericPS2;
        }
    } else if detect_alps() {
        log::info!("[touchpad] ALPS touchpad detected!");
        tp_type = TouchpadType::Alps;
        // ALPS in basic PS/2 compatibility mode uses 3-byte packets.
        // Full ALPS absolute mode requires model-specific init sequences
        // that vary widely. We use the compatibility fallback.
        packet_size = 3;
    } else {
        // Check if there's any PS/2 pointing device at all.
        ps2_flush();
        let id_resp = ps2_send_aux(0xF2); // GET_DEVICE_ID
        if let Some(ack) = id_resp {
            if ack == 0xFA {
                if ps2_wait_output() {
                    let dev_id = unsafe { Port::<u8>::new(PS2_DATA).read() };
                    log::info!("[touchpad] generic PS/2 device ID: {:#X}", dev_id);
                    if dev_id == 0x00 || dev_id == 0x03 || dev_id == 0x04 {
                        tp_type = TouchpadType::GenericPS2;
                    }
                }
            }
        }
    }

    TOUCHPAD_TYPE.store(tp_type as u8, Ordering::Release);

    if tp_type == TouchpadType::None {
        log::info!("[touchpad] no touchpad or PS/2 pointing device found");
        return;
    }

    // Enable data reporting on the aux device.
    ps2_send_aux(0xF4); // ENABLE

    // Set up state.
    let mut state = TouchpadState::new(screen_w, screen_h);
    state.packet_size = packet_size;

    *TOUCHPAD_STATE.lock() = Some(state);
    TOUCHPAD_ACTIVE.store(true, Ordering::Release);

    log::info!("[touchpad] type={:?} packet_size={} screen={}x{}",
        tp_type, packet_size, screen_w, screen_h);
    log::info!("[touchpad] sensitivity=5 palm_threshold=10");
    log::info!("[touchpad] gestures: tap=left-click, 2-finger-tap=right-click, 2-finger-scroll");
    log::info!("[touchpad] initialization complete");
}

/// Returns true if the touchpad is active.
pub fn is_active() -> bool {
    TOUCHPAD_ACTIVE.load(Ordering::Relaxed)
}
