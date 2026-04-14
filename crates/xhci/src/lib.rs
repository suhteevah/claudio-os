//! # claudio-xhci — Bare-metal xHCI (USB 3.0) Host Controller Driver
//!
//! This crate implements an xHCI host controller driver for ClaudioOS, targeting
//! USB keyboard support via the HID boot protocol. It operates directly on
//! memory-mapped PCI BAR0 registers with no OS abstractions.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────┐
//! │         XhciController          │  driver.rs — top-level API
//! ├─────────────────────────────────┤
//! │  Device Manager  │  HID/Keyboard│  device.rs, hid.rs
//! ├──────────────────┴──────────────┤
//! │  Command Ring │ Event Ring │ TR  │  ring.rs — TRB ring management
//! ├─────────────────────────────────┤
//! │  Device/Endpoint/Input Contexts │  context.rs — xHCI data structures
//! ├─────────────────────────────────┤
//! │  Capability │ Operational │ RT   │  registers.rs — MMIO register access
//! └─────────────────────────────────┘
//! ```

#![no_std]

extern crate alloc;

pub mod registers;
pub mod ring;
pub mod context;
pub mod device;
pub mod hid;
pub mod driver;

pub use driver::XhciController;
pub use device::MassStorageInfo;
pub use hid::KeyEvent;

/// Callback to translate a virtual address to a physical DMA address.
/// In ClaudioOS, heap addresses are NOT identity-mapped, so every pointer
/// destined for hardware DMA must go through this translation.
pub type VirtToPhys = fn(usize) -> u64;

/// Errors from xHCI controller operations.
#[derive(Debug)]
pub enum XhciError {
    /// The controller did not become ready within the timeout period.
    Timeout(&'static str),
    /// The controller is in an unexpected state.
    ControllerError(&'static str),
    /// A memory allocation failed.
    AllocFailed(&'static str),
}
