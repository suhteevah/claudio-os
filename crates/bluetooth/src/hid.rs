//! Bluetooth HID (Human Interface Device) Profile
//!
//! Supports Bluetooth keyboards and mice over both Classic Bluetooth (L2CAP
//! HID channels on PSM 0x0011/0x0013) and BLE (GATT HID Service 0x1812).
//!
//! The HID boot protocol provides a standardized 8-byte keyboard report and
//! 3+ byte mouse report without requiring HID report descriptor parsing.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// HID protocol constants
// ---------------------------------------------------------------------------

/// HID protocol mode: Boot Protocol
pub const HID_PROTOCOL_BOOT: u8 = 0x00;
/// HID protocol mode: Report Protocol
pub const HID_PROTOCOL_REPORT: u8 = 0x01;

/// HID transaction types (Classic BT HID, over L2CAP control channel)
pub const HID_HANDSHAKE: u8 = 0x00;
pub const HID_CONTROL: u8 = 0x10;
pub const HID_GET_REPORT: u8 = 0x40;
pub const HID_SET_REPORT: u8 = 0x50;
pub const HID_GET_PROTOCOL: u8 = 0x60;
pub const HID_SET_PROTOCOL: u8 = 0x70;
pub const HID_DATA: u8 = 0xA0;

/// HID report types
pub const HID_REPORT_INPUT: u8 = 0x01;
pub const HID_REPORT_OUTPUT: u8 = 0x02;
pub const HID_REPORT_FEATURE: u8 = 0x03;

// ---------------------------------------------------------------------------
// Modifier key bits (boot protocol keyboard report byte 0)
// ---------------------------------------------------------------------------

pub const MOD_LEFT_CTRL: u8 = 1 << 0;
pub const MOD_LEFT_SHIFT: u8 = 1 << 1;
pub const MOD_LEFT_ALT: u8 = 1 << 2;
pub const MOD_LEFT_GUI: u8 = 1 << 3;
pub const MOD_RIGHT_CTRL: u8 = 1 << 4;
pub const MOD_RIGHT_SHIFT: u8 = 1 << 5;
pub const MOD_RIGHT_ALT: u8 = 1 << 6;
pub const MOD_RIGHT_GUI: u8 = 1 << 7;

// ---------------------------------------------------------------------------
// Boot protocol report structures
// ---------------------------------------------------------------------------

/// Boot protocol keyboard report (8 bytes).
///
/// Byte 0: Modifier keys (bitmap)
/// Byte 1: Reserved (0x00)
/// Bytes 2-7: Up to 6 simultaneous keycodes (USB HID Usage IDs)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BtKeyboardReport {
    pub modifiers: u8,
    pub keycodes: [u8; 6],
}

impl BtKeyboardReport {
    /// Parse from an 8-byte boot protocol report.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }
        let mut keycodes = [0u8; 6];
        keycodes.copy_from_slice(&data[2..8]);
        Some(BtKeyboardReport {
            modifiers: data[0],
            keycodes,
        })
    }

    /// Check if a modifier key is pressed.
    pub fn has_modifier(&self, modifier: u8) -> bool {
        self.modifiers & modifier != 0
    }

    /// Get the list of pressed keycodes (non-zero entries).
    pub fn pressed_keys(&self) -> impl Iterator<Item = u8> + '_ {
        self.keycodes.iter().copied().filter(|&k| k != 0)
    }
}

/// Boot protocol mouse report (3+ bytes).
///
/// Byte 0: Button bitmap (bit0=left, bit1=right, bit2=middle)
/// Byte 1: X displacement (signed)
/// Byte 2: Y displacement (signed)
/// Byte 3: Wheel displacement (signed, optional)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BtMouseReport {
    pub buttons: u8,
    pub x: i8,
    pub y: i8,
    pub wheel: i8,
}

impl BtMouseReport {
    /// Parse from a 3+ byte boot protocol report.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 3 {
            return None;
        }
        Some(BtMouseReport {
            buttons: data[0],
            x: data[1] as i8,
            y: data[2] as i8,
            wheel: if data.len() > 3 { data[3] as i8 } else { 0 },
        })
    }

    pub fn left_button(&self) -> bool {
        self.buttons & 0x01 != 0
    }
    pub fn right_button(&self) -> bool {
        self.buttons & 0x02 != 0
    }
    pub fn middle_button(&self) -> bool {
        self.buttons & 0x04 != 0
    }
}

// ---------------------------------------------------------------------------
// HID events
// ---------------------------------------------------------------------------

/// A Bluetooth HID event (keyboard or mouse input).
#[derive(Debug, Clone)]
pub enum BtHidEvent {
    /// Key pressed (usage_id, modifiers)
    KeyDown { usage_id: u8, modifiers: u8 },
    /// Key released (usage_id)
    KeyUp { usage_id: u8 },
    /// Mouse movement / button change
    Mouse(BtMouseReport),
}

// ---------------------------------------------------------------------------
// Keyboard state tracker
// ---------------------------------------------------------------------------

/// Tracks keyboard state across consecutive boot protocol reports to detect
/// key press and release events.
#[derive(Debug)]
pub struct BtKeyboardState {
    /// Previous report's keycodes
    prev_keycodes: [u8; 6],
    /// Previous report's modifiers
    prev_modifiers: u8,
    /// Event queue
    events: VecDeque<BtHidEvent>,
}

impl BtKeyboardState {
    pub fn new() -> Self {
        log::debug!("BT HID: keyboard state tracker initialized");
        BtKeyboardState {
            prev_keycodes: [0; 6],
            prev_modifiers: 0,
            events: VecDeque::new(),
        }
    }

    /// Process a new keyboard report and generate key press/release events.
    pub fn process_report(&mut self, report: &BtKeyboardReport) {
        // Detect key releases: keys in prev but not in current
        for &prev_key in &self.prev_keycodes {
            if prev_key != 0 && !report.keycodes.contains(&prev_key) {
                log::trace!("BT HID: key released usage_id=0x{:02X}", prev_key);
                self.events.push_back(BtHidEvent::KeyUp {
                    usage_id: prev_key,
                });
            }
        }

        // Detect key presses: keys in current but not in prev
        for &key in &report.keycodes {
            if key != 0 && !self.prev_keycodes.contains(&key) {
                log::trace!(
                    "BT HID: key pressed usage_id=0x{:02X} mods=0x{:02X}",
                    key,
                    report.modifiers
                );
                self.events.push_back(BtHidEvent::KeyDown {
                    usage_id: key,
                    modifiers: report.modifiers,
                });
            }
        }

        // Detect modifier changes as events too
        let mod_changes = self.prev_modifiers ^ report.modifiers;
        if mod_changes != 0 {
            log::trace!(
                "BT HID: modifier change 0x{:02X} -> 0x{:02X}",
                self.prev_modifiers,
                report.modifiers
            );
        }

        self.prev_keycodes = report.keycodes;
        self.prev_modifiers = report.modifiers;
    }

    /// Dequeue the next HID event, if any.
    pub fn next_event(&mut self) -> Option<BtHidEvent> {
        self.events.pop_front()
    }

    /// Check if any events are pending.
    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }
}

// ---------------------------------------------------------------------------
// HID-over-L2CAP (Classic BT) message parsing
// ---------------------------------------------------------------------------

/// Parse a Classic BT HID data message from the interrupt channel (PSM 0x0013).
/// The first byte is the HID header (transaction type + parameter).
pub fn parse_hid_data(data: &[u8]) -> Option<(u8, &[u8])> {
    if data.is_empty() {
        return None;
    }
    let header = data[0];
    let transaction_type = header & 0xF0;
    let param = header & 0x0F;

    if transaction_type == HID_DATA {
        let report_type = param;
        let report_data = &data[1..];
        log::trace!(
            "BT HID: data message report_type={} len={}",
            report_type,
            report_data.len()
        );
        Some((report_type, report_data))
    } else {
        log::trace!("BT HID: non-data message type=0x{:02X}", transaction_type);
        None
    }
}

/// Build a SET_PROTOCOL message for Classic BT HID control channel (PSM 0x0011).
pub fn build_set_protocol(protocol: u8) -> Vec<u8> {
    log::debug!("BT HID: SET_PROTOCOL ({})", if protocol == HID_PROTOCOL_BOOT { "boot" } else { "report" });
    alloc::vec![HID_SET_PROTOCOL | protocol]
}

// ---------------------------------------------------------------------------
// USB HID Usage ID to ASCII mapping (subset for common keys)
// ---------------------------------------------------------------------------

/// Convert a USB HID Usage ID to an ASCII character, considering modifiers.
/// Returns None for non-printable keys.
pub fn usage_id_to_char(usage_id: u8, shift: bool) -> Option<char> {
    match usage_id {
        // Letters a-z (Usage IDs 0x04-0x1D)
        0x04..=0x1D => {
            let base = b'a' + (usage_id - 0x04);
            Some(if shift {
                (base - 32) as char // uppercase
            } else {
                base as char
            })
        }
        // Numbers 1-9 (Usage IDs 0x1E-0x26)
        0x1E..=0x26 => {
            if shift {
                let symbols = b"!@#$%^&*(";
                Some(symbols[(usage_id - 0x1E) as usize] as char)
            } else {
                Some((b'1' + (usage_id - 0x1E)) as char)
            }
        }
        // 0 (Usage ID 0x27)
        0x27 => Some(if shift { ')' } else { '0' }),
        // Special keys
        0x28 => Some('\n'),  // Enter
        0x2C => Some(' '),   // Space
        0x2D => Some(if shift { '_' } else { '-' }),
        0x2E => Some(if shift { '+' } else { '=' }),
        0x2F => Some(if shift { '{' } else { '[' }),
        0x30 => Some(if shift { '}' } else { ']' }),
        0x31 => Some(if shift { '|' } else { '\\' }),
        0x33 => Some(if shift { ':' } else { ';' }),
        0x34 => Some(if shift { '"' } else { '\'' }),
        0x35 => Some(if shift { '~' } else { '`' }),
        0x36 => Some(if shift { '<' } else { ',' }),
        0x37 => Some(if shift { '>' } else { '.' }),
        0x38 => Some(if shift { '?' } else { '/' }),
        0x2B => Some('\t'),  // Tab
        _ => None,
    }
}
