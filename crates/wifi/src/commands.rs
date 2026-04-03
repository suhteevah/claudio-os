//! Host command interface for the Intel WiFi firmware.
//!
//! After firmware is loaded and alive, all control-plane operations go through
//! a command/response protocol over the TFD transmit ring (queue 0 is the
//! command queue). The firmware processes commands and posts responses to the
//! RX queue.
//!
//! # Command format
//!
//! ```text
//! Offset  Size  Field
//! ------  ----  -----
//! 0x00    1     Command ID
//! 0x01    1     Group ID (0 for legacy commands)
//! 0x02    2     Sequence number
//! 0x04    2     Length of payload
//! 0x06    ...   Payload (command-specific)
//! ```

use alloc::vec::Vec;

/// Command IDs for the iwlwifi firmware command protocol.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum CommandId {
    /// Alive notification (firmware -> host).
    Alive = 0x01,
    /// Error notification (firmware -> host).
    Error = 0x05,
    /// NVM access (read/write calibration and regulatory data).
    NvmAccess = 0x88,
    /// PHY configuration.
    PhyConfig = 0x6A,
    /// PHY context command (band/channel/width).
    PhyContext = 0x08,
    /// MAC context command (add/modify/remove).
    MacContext = 0x28,
    /// Binding command (bind MAC context to PHY context).
    Binding = 0x2B,
    /// Time event (for scanning, association).
    TimeEvent = 0x29,
    /// Scan request (UMAC scan).
    ScanUmac = 0x0C,
    /// Scan complete notification (firmware -> host).
    ScanComplete = 0x84,
    /// Add station (for association).
    AddStation = 0x18,
    /// Remove station.
    RemoveStation = 0x19,
    /// TX command (data frame transmission).
    Tx = 0x1C,
    /// TX response (firmware -> host).
    TxResponse = 0x1D,
    /// Set encryption key.
    AddKey = 0x0E,
    /// Power management.
    PowerTable = 0x77,
    /// NIC configuration.
    NicConfig = 0xBB,
    /// Calibration result (firmware -> host).
    CalibResult = 0x14,
}

/// A host command to send to the firmware.
#[derive(Debug, Clone)]
pub struct HostCommand {
    /// Command ID.
    pub id: CommandId,
    /// Group ID (0 for legacy commands, non-zero for extended groups).
    pub group: u8,
    /// Payload bytes.
    pub data: Vec<u8>,
}

impl HostCommand {
    /// Create a new host command with no payload.
    pub fn new(id: CommandId) -> Self {
        Self {
            id,
            group: 0,
            data: Vec::new(),
        }
    }

    /// Create a new host command with the given payload.
    pub fn with_data(id: CommandId, data: Vec<u8>) -> Self {
        Self {
            id,
            group: 0,
            data,
        }
    }

    /// Serialize the command into a byte buffer suitable for TFD submission.
    pub fn serialize(&self, seq: u16) -> Vec<u8> {
        let len = self.data.len() as u16;
        let mut buf = Vec::with_capacity(6 + self.data.len());
        buf.push(self.id as u8);
        buf.push(self.group);
        buf.extend_from_slice(&seq.to_le_bytes());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }
}

/// Response from the firmware to a host command.
#[derive(Debug, Clone)]
pub struct CommandResponse {
    /// Command ID this is responding to.
    pub id: u8,
    /// Group ID.
    pub group: u8,
    /// Sequence number (matches the request).
    pub sequence: u16,
    /// Response status (0 = success).
    pub status: u32,
    /// Response payload.
    pub data: Vec<u8>,
}

impl CommandResponse {
    /// Parse a command response from raw bytes received on the RX queue.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            log::warn!("wifi::commands: response too short ({} bytes)", data.len());
            return None;
        }
        let id = data[0];
        let group = data[1];
        let sequence = u16::from_le_bytes([data[2], data[3]]);
        let status = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let payload = if data.len() > 8 {
            data[8..].to_vec()
        } else {
            Vec::new()
        };

        log::trace!(
            "wifi::commands: response id=0x{:02X} group={} seq={} status={}",
            id, group, sequence, status
        );

        Some(Self {
            id,
            group,
            sequence,
            status,
            data: payload,
        })
    }
}

// -------------------------------------------------------------------
// NVM (Non-Volatile Memory) access
// -------------------------------------------------------------------

/// NVM section identifiers.
#[repr(u16)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum NvmSection {
    /// Hardware (SKU, antenna config).
    Hardware = 0,
    /// Regulatory data.
    Regulatory = 1,
    /// Calibration data.
    Calibration = 2,
    /// Production data (serial number, MAC address).
    Production = 3,
    /// PHY SKU data.
    PhySku = 4,
}

/// Build an NVM read command for the given section.
pub fn nvm_read_cmd(section: NvmSection, offset: u16, length: u16) -> HostCommand {
    log::debug!(
        "wifi::commands: NVM read section={:?} offset=0x{:04X} len={}",
        section, offset, length
    );
    let mut data = Vec::with_capacity(8);
    data.extend_from_slice(&(section as u16).to_le_bytes()); // section
    data.extend_from_slice(&offset.to_le_bytes());            // offset
    data.extend_from_slice(&length.to_le_bytes());            // length
    data.extend_from_slice(&[0u8; 2]);                        // reserved
    HostCommand::with_data(CommandId::NvmAccess, data)
}

// -------------------------------------------------------------------
// PHY configuration
// -------------------------------------------------------------------

/// Radio band for PHY configuration.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// 2.4 GHz.
    Band2G = 0,
    /// 5 GHz.
    Band5G = 1,
    /// 6 GHz (Wi-Fi 6E, AX210 only).
    Band6G = 2,
}

/// Channel width for PHY configuration.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelWidth {
    /// 20 MHz.
    Mhz20 = 0,
    /// 40 MHz.
    Mhz40 = 1,
    /// 80 MHz.
    Mhz80 = 2,
    /// 160 MHz.
    Mhz160 = 3,
}

/// Build a PHY context command to configure band/channel/width.
pub fn phy_context_cmd(band: Band, channel: u8, width: ChannelWidth) -> HostCommand {
    log::info!(
        "wifi::commands: PHY context — band={:?} channel={} width={:?}",
        band, channel, width
    );
    let mut data = Vec::with_capacity(16);
    data.push(1); // action: add
    data.push(0); // apply_time: immediate
    data.extend_from_slice(&[0u8; 2]); // reserved
    data.push(band as u8);
    data.push(channel);
    data.push(width as u8);
    data.push(0); // reserved
    // Chain A/B config (2x2 MIMO)
    data.extend_from_slice(&[0x03, 0x00, 0x00, 0x00]); // chains: both A and B
    data.extend_from_slice(&[0u8; 4]); // reserved
    HostCommand::with_data(CommandId::PhyContext, data)
}

// -------------------------------------------------------------------
// Scan command
// -------------------------------------------------------------------

/// Build a UMAC scan command for the given channels and optional SSID.
///
/// If `ssid` is `None`, a passive scan is performed (listen-only).
/// If `ssid` is `Some`, an active scan with probe requests is performed.
pub fn scan_cmd(channels: &[u8], ssid: Option<&[u8]>) -> HostCommand {
    let active = ssid.is_some();
    log::info!(
        "wifi::commands: scan command — {} scan, {} channels",
        if active { "active" } else { "passive" },
        channels.len()
    );

    let mut data = Vec::with_capacity(64 + channels.len());

    // Scan flags
    let flags: u32 = if active { 0x01 } else { 0x00 };
    data.extend_from_slice(&flags.to_le_bytes());

    // Number of channels
    data.push(channels.len() as u8);
    data.extend_from_slice(&[0u8; 3]); // reserved/padding

    // SSID (if active scan)
    if let Some(ssid_bytes) = ssid {
        let ssid_len = ssid_bytes.len().min(32) as u8;
        data.push(ssid_len);
        data.extend_from_slice(&ssid_bytes[..ssid_len as usize]);
        // Pad to 33 bytes (1 len + 32 max SSID)
        for _ in ssid_bytes.len()..32 {
            data.push(0);
        }
    } else {
        // No SSID — passive scan
        data.extend_from_slice(&[0u8; 33]);
    }

    // Channel list
    for &ch in channels {
        data.push(ch);
        data.push(0); // band (0 = 2.4 GHz for channels 1-14, TODO: 5 GHz)
        data.extend_from_slice(&100u16.to_le_bytes()); // dwell time (ms)
    }

    HostCommand::with_data(CommandId::ScanUmac, data)
}

// -------------------------------------------------------------------
// Association
// -------------------------------------------------------------------

/// Build an add-station command for association with an AP.
pub fn add_station_cmd(bssid: &[u8; 6], station_id: u8) -> HostCommand {
    log::info!(
        "wifi::commands: add station {} BSSID={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        station_id,
        bssid[0], bssid[1], bssid[2], bssid[3], bssid[4], bssid[5]
    );
    let mut data = Vec::with_capacity(32);
    data.push(station_id);
    data.extend_from_slice(&[0u8; 3]); // reserved
    data.extend_from_slice(bssid);
    data.extend_from_slice(&[0u8; 2]); // reserved
    // Default station flags (HT capable, etc.)
    data.extend_from_slice(&[0u8; 20]); // station flags + reserved
    HostCommand::with_data(CommandId::AddStation, data)
}

// -------------------------------------------------------------------
// Key management
// -------------------------------------------------------------------

/// Key algorithm for encryption.
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum KeyAlgorithm {
    /// No encryption.
    None = 0,
    /// WEP (legacy, not recommended).
    Wep = 1,
    /// TKIP (WPA1, legacy).
    Tkip = 2,
    /// AES-CCMP (WPA2).
    Ccmp = 3,
}

/// Build an add-key command for installing an encryption key.
pub fn add_key_cmd(
    station_id: u8,
    key_id: u8,
    algorithm: KeyAlgorithm,
    key: &[u8],
) -> HostCommand {
    log::info!(
        "wifi::commands: add key — station={} key_id={} algo={:?} key_len={}",
        station_id, key_id, algorithm, key.len()
    );
    let mut data = Vec::with_capacity(48);
    data.push(station_id);
    data.push(key_id);
    data.push(algorithm as u8);
    data.push(key.len() as u8);
    // Key data (padded to 32 bytes)
    data.extend_from_slice(key);
    for _ in key.len()..32 {
        data.push(0);
    }
    data.extend_from_slice(&[0u8; 12]); // RSC + reserved
    HostCommand::with_data(CommandId::AddKey, data)
}
