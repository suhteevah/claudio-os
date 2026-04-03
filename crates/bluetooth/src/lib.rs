//! # claudio-bluetooth -- Bare-metal Bluetooth HCI-over-USB Driver
//!
//! This crate implements a Bluetooth host stack for ClaudioOS, communicating with
//! a USB Bluetooth controller via the HCI (Host Controller Interface) protocol.
//! It targets keyboard and mouse support via the HID profile over Bluetooth Classic
//! and BLE.
//!
//! ## Architecture
//!
//! ```text
//! +---------------------------------------------+
//! |           BluetoothController                |  driver.rs -- top-level API
//! +-------------------+-------------------------+
//! |  GAP (discovery)  |  GATT (services/chars)  |  gap.rs, gatt.rs
//! +-------------------+-------------------------+
//! |  HID Profile (keyboard/mouse)               |  hid.rs
//! +---------------------------------------------+
//! |         L2CAP (channels, flow control)       |  l2cap.rs
//! +---------------------------------------------+
//! |         HCI (commands, events, data)         |  hci.rs
//! +---------------------------------------------+
//! |     USB Transport (bulk/interrupt EPs)       |  usb_transport.rs
//! +---------------------------------------------+
//! ```

#![no_std]

extern crate alloc;

pub mod hci;
pub mod usb_transport;
pub mod l2cap;
pub mod gap;
pub mod gatt;
pub mod hid;
pub mod driver;

pub use driver::BluetoothController;
pub use hci::{BdAddr, HciError};
pub use gap::{DiscoveredDevice, ScanResult};
pub use hid::{BtHidEvent, BtKeyboardReport, BtMouseReport};
