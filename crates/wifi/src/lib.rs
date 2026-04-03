//! Intel WiFi driver for ClaudioOS (iwlwifi equivalent).
//!
//! Supports the Intel AX201, AX200, AX210, and AC 9260 families. These are all
//! PCIe devices that share the same command/response transport (TFD/RXQ rings)
//! and require firmware microcode loaded into device SRAM before any 802.11
//! operations can proceed.
//!
//! # Architecture
//!
//! ```text
//!   WifiController (driver.rs)          ← high-level API: scan, connect, disconnect
//!       ↓
//!   WPA2 handshake (wpa.rs)             ← 4-way handshake, PBKDF2-SHA1, AES-CCMP
//!       ↓
//!   Scanning (scan.rs)                  ← active/passive scan, SSID matching
//!       ↓
//!   Commands (commands.rs)              ← NVM access, PHY config, scan cmd, assoc
//!       ↓
//!   TX/RX queues (tx_rx.rs)             ← TFD ring (transmit), RXQ (receive)
//!       ↓
//!   Firmware loader (firmware.rs)        ← microcode upload to device SRAM
//!       ↓
//!   PCI detection (pci.rs)              ← vendor 0x8086, device ID matching
//!       ↓
//!   802.11 frames (ieee80211.rs)        ← beacon, probe, auth, assoc, data, QoS
//! ```
//!
//! # Firmware requirement
//!
//! Intel WiFi hardware is inert without firmware. The driver must load the
//! appropriate `.ucode` file into device SRAM via the BSM (Bootstrap State
//! Machine) before issuing any commands. See [`firmware`] module docs.
//!
//! # Usage
//!
//! ```ignore
//! use claudio_wifi::driver::WifiController;
//!
//! let mut wifi = unsafe { WifiController::init(bar0_base, irq_line, phys_mem_offset)? };
//! wifi.load_firmware(fw_data)?;
//!
//! let networks = wifi.scan_networks()?;
//! for net in &networks {
//!     log::info!("SSID={} signal={}dBm", net.ssid, net.signal_dbm);
//! }
//!
//! wifi.connect("MyNetwork", "hunter2")?;
//! let ip = wifi.ip_config()?;  // DHCP
//! ```

#![no_std]

extern crate alloc;

pub mod pci;
pub mod firmware;
pub mod commands;
pub mod tx_rx;
pub mod ieee80211;
pub mod wpa;
pub mod scan;
pub mod driver;

pub use driver::WifiController;
pub use pci::WifiDevice;
pub use scan::ScannedNetwork;

/// Intel WiFi device variant, selected from PCI device ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiVariant {
    /// Intel Wireless-AC 9260 (CNVi, 2x2 802.11ac).
    AC9260,
    /// Intel Wi-Fi 6 AX200 (discrete PCIe, 2x2 802.11ax).
    AX200,
    /// Intel Wi-Fi 6 AX201 (CNVi, 2x2 802.11ax). Most common in 10th/11th gen laptops.
    AX201,
    /// Intel Wi-Fi 6E AX210 (discrete PCIe, 2x2 802.11ax with 6 GHz).
    AX210,
}

impl WifiVariant {
    /// Human-readable name for log messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::AC9260 => "Intel Wireless-AC 9260",
            Self::AX200 => "Intel Wi-Fi 6 AX200",
            Self::AX201 => "Intel Wi-Fi 6 AX201",
            Self::AX210 => "Intel Wi-Fi 6E AX210",
        }
    }

    /// The firmware image filename expected on the FAT32 persist partition.
    pub fn firmware_name(self) -> &'static str {
        match self {
            Self::AC9260 => "iwlwifi-9260-th-b0-jf-b0-46.ucode",
            Self::AX200 => "iwlwifi-cc-a0-46.ucode",
            Self::AX201 => "iwlwifi-QuZ-a0-hr-b0-77.ucode",
            Self::AX210 => "iwlwifi-ty-a0-gf-a0-77.ucode",
        }
    }
}
