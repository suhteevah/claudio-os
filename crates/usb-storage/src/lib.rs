//! # claudio-usb-storage — Bare-metal USB Mass Storage (Bulk-Only Transport)
//!
//! This crate implements the USB Mass Storage class driver using the
//! Bulk-Only Transport (BOT) protocol defined in the USB Mass Storage
//! Class specification. It wraps SCSI commands (INQUIRY, READ_CAPACITY_10,
//! READ_10, WRITE_10, TEST_UNIT_READY, REQUEST_SENSE, MODE_SENSE) inside
//! BOT Command Block Wrappers and shuttles them over bulk endpoints.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │       UsbStorageDevice              │  driver.rs — top-level API + BlockDevice
//! ├─────────────────────────────────────┤
//! │   SCSI command builders/parsers     │  scsi.rs — CDB construction + response parse
//! ├─────────────────────────────────────┤
//! │   Bulk-Only Transport (CBW / CSW)   │  bot.rs — 31-byte CBW, 13-byte CSW, reset
//! ├─────────────────────────────────────┤
//! │   xHCI bulk endpoint I/O            │  (provided by claudio-xhci)
//! └─────────────────────────────────────┘
//! ```
//!
//! ## Detection
//!
//! USB mass storage devices are identified by:
//! - **Class**: 0x08 (Mass Storage)
//! - **Subclass**: 0x06 (SCSI Transparent Command Set)
//! - **Protocol**: 0x50 (Bulk-Only Transport)
//!
//! ## VFS Integration
//!
//! `UsbStorageDevice` implements `claudio_vfs::BlockDevice`, so it plugs
//! directly into the VFS mount pipeline alongside AHCI and NVMe drives.

#![no_std]

extern crate alloc;

pub mod bot;
pub mod scsi;
pub mod driver;

pub use driver::UsbStorageDevice;
pub use bot::{CommandBlockWrapper, CommandStatusWrapper, BotError};
pub use scsi::{ScsiCommand, InquiryData, ReadCapacity10Data};
