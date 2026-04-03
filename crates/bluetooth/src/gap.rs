//! GAP (Generic Access Profile) -- Discovery, Advertising, Scanning, Connection
//!
//! GAP manages device discovery for both Bluetooth Classic (Inquiry) and BLE
//! (LE scanning / advertising). It also handles connection establishment and
//! tracks known/paired devices.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use crate::hci::{
    BdAddr, HciEvent,
    HCI_EVT_INQUIRY_COMPLETE, HCI_EVT_INQUIRY_RESULT, HCI_EVT_INQUIRY_RESULT_WITH_RSSI,
    HCI_EVT_EXTENDED_INQUIRY_RESULT, HCI_EVT_CONNECTION_COMPLETE,
    HCI_EVT_DISCONNECTION_COMPLETE, HCI_EVT_REMOTE_NAME_REQUEST_COMPLETE,
    HCI_EVT_LE_META, HCI_LE_EVT_ADVERTISING_REPORT, HCI_LE_EVT_CONNECTION_COMPLETE,
};

// ---------------------------------------------------------------------------
// Scan / discovery types
// ---------------------------------------------------------------------------

/// Advertising data type codes (used in AD structures).
pub const AD_TYPE_FLAGS: u8 = 0x01;
pub const AD_TYPE_INCOMPLETE_16_UUID: u8 = 0x02;
pub const AD_TYPE_COMPLETE_16_UUID: u8 = 0x03;
pub const AD_TYPE_INCOMPLETE_128_UUID: u8 = 0x06;
pub const AD_TYPE_COMPLETE_128_UUID: u8 = 0x07;
pub const AD_TYPE_SHORT_LOCAL_NAME: u8 = 0x08;
pub const AD_TYPE_COMPLETE_LOCAL_NAME: u8 = 0x09;
pub const AD_TYPE_TX_POWER_LEVEL: u8 = 0x0A;
pub const AD_TYPE_APPEARANCE: u8 = 0x19;

/// Type of Bluetooth device (Classic, LE, or Dual-Mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Classic,
    LowEnergy,
    DualMode,
}

/// A device discovered during scanning/inquiry.
#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    /// Bluetooth address
    pub addr: BdAddr,
    /// Address type (0x00 = public, 0x01 = random for BLE)
    pub addr_type: u8,
    /// Device name (if available from EIR/AD data)
    pub name: Option<String>,
    /// RSSI in dBm
    pub rssi: Option<i8>,
    /// Class of device (Classic only)
    pub class_of_device: Option<u32>,
    /// Device type
    pub device_type: DeviceType,
    /// Raw advertising/EIR data
    pub ad_data: Vec<u8>,
}

/// Result of a scan operation.
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// Devices found during the scan
    pub devices: Vec<DiscoveredDevice>,
    /// Whether the scan completed normally
    pub complete: bool,
}

/// State of a GAP connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
}

/// A tracked GAP connection.
#[derive(Debug, Clone)]
pub struct GapConnection {
    pub addr: BdAddr,
    pub addr_type: u8,
    pub handle: u16,
    pub state: ConnectionState,
    pub device_type: DeviceType,
}

// ---------------------------------------------------------------------------
// AD structure parsing
// ---------------------------------------------------------------------------

/// Parse AD (Advertising Data) / EIR (Extended Inquiry Response) structures.
/// Returns (name, list of service UUIDs).
pub fn parse_ad_structures(data: &[u8]) -> (Option<String>, Vec<u16>) {
    let mut name = None;
    let mut uuids = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        let len = data[offset] as usize;
        if len == 0 || offset + 1 + len > data.len() {
            break;
        }
        let ad_type = data[offset + 1];
        let ad_data = &data[offset + 2..offset + 1 + len];

        match ad_type {
            AD_TYPE_COMPLETE_LOCAL_NAME | AD_TYPE_SHORT_LOCAL_NAME => {
                if let Ok(s) = core::str::from_utf8(ad_data) {
                    log::debug!("GAP: parsed device name: {}", s);
                    name = Some(String::from(s));
                }
            }
            AD_TYPE_COMPLETE_16_UUID | AD_TYPE_INCOMPLETE_16_UUID => {
                let mut i = 0;
                while i + 1 < ad_data.len() {
                    let uuid = u16::from_le_bytes([ad_data[i], ad_data[i + 1]]);
                    uuids.push(uuid);
                    i += 2;
                }
            }
            _ => {
                log::trace!("GAP: AD type 0x{:02X} len={}", ad_type, ad_data.len());
            }
        }
        offset += 1 + len;
    }

    (name, uuids)
}

// ---------------------------------------------------------------------------
// GAP manager
// ---------------------------------------------------------------------------

/// Manages device discovery, connections, and pairing state.
pub struct GapManager {
    /// Discovered devices from the latest scan, keyed by BD_ADDR bytes.
    pub discovered: BTreeMap<[u8; 6], DiscoveredDevice>,
    /// Active connections, keyed by HCI connection handle.
    pub connections: BTreeMap<u16, GapConnection>,
    /// Whether a Classic inquiry is in progress.
    pub inquiry_active: bool,
    /// Whether LE scanning is in progress.
    pub le_scan_active: bool,
}

impl GapManager {
    pub fn new() -> Self {
        log::debug!("GAP: manager initialized");
        GapManager {
            discovered: BTreeMap::new(),
            connections: BTreeMap::new(),
            inquiry_active: false,
            le_scan_active: false,
        }
    }

    /// Clear all discovered devices (start fresh scan).
    pub fn clear_discovered(&mut self) {
        log::debug!("GAP: clearing discovered devices");
        self.discovered.clear();
    }

    /// Get the current scan results.
    pub fn scan_results(&self) -> ScanResult {
        ScanResult {
            devices: self.discovered.values().cloned().collect(),
            complete: !self.inquiry_active && !self.le_scan_active,
        }
    }

    /// Process an HCI event and update GAP state. Returns true if the event was consumed.
    pub fn handle_event(&mut self, event: &HciEvent) -> bool {
        match event.event_code {
            HCI_EVT_INQUIRY_RESULT => {
                self.handle_inquiry_result(&event.params);
                true
            }
            HCI_EVT_INQUIRY_RESULT_WITH_RSSI => {
                self.handle_inquiry_result_rssi(&event.params);
                true
            }
            HCI_EVT_EXTENDED_INQUIRY_RESULT => {
                self.handle_extended_inquiry_result(&event.params);
                true
            }
            HCI_EVT_INQUIRY_COMPLETE => {
                log::info!("GAP: inquiry complete (status=0x{:02X})", event.params.first().copied().unwrap_or(0xFF));
                self.inquiry_active = false;
                true
            }
            HCI_EVT_CONNECTION_COMPLETE => {
                self.handle_connection_complete(&event.params);
                true
            }
            HCI_EVT_DISCONNECTION_COMPLETE => {
                self.handle_disconnection_complete(&event.params);
                true
            }
            HCI_EVT_REMOTE_NAME_REQUEST_COMPLETE => {
                self.handle_remote_name_complete(&event.params);
                true
            }
            HCI_EVT_LE_META => {
                if let Some((sub, sub_params)) = event.as_le_meta() {
                    match sub {
                        HCI_LE_EVT_ADVERTISING_REPORT => {
                            self.handle_le_advertising_report(sub_params);
                            return true;
                        }
                        HCI_LE_EVT_CONNECTION_COMPLETE => {
                            self.handle_le_connection_complete(sub_params);
                            return true;
                        }
                        _ => {}
                    }
                }
                false
            }
            _ => false,
        }
    }

    // -----------------------------------------------------------------------
    // Classic inquiry result handlers
    // -----------------------------------------------------------------------

    fn handle_inquiry_result(&mut self, params: &[u8]) {
        if params.is_empty() {
            return;
        }
        let num_responses = params[0] as usize;
        log::info!("GAP: inquiry result ({} responses)", num_responses);

        // Each response: BD_ADDR (6) + Page_Scan_Rep_Mode (1) + Reserved (2) +
        // Class_of_Device (3) + Clock_Offset (2) = 14 bytes per response
        let mut offset = 1;
        for _ in 0..num_responses {
            if offset + 14 > params.len() {
                break;
            }
            if let Some(addr) = BdAddr::from_bytes(&params[offset..]) {
                let cod = u32::from_le_bytes([
                    params[offset + 9],
                    params[offset + 10],
                    params[offset + 11],
                    0,
                ]);
                log::info!("GAP: discovered Classic device {} CoD=0x{:06X}", addr, cod);
                self.discovered.insert(
                    addr.0,
                    DiscoveredDevice {
                        addr,
                        addr_type: 0x00,
                        name: None,
                        rssi: None,
                        class_of_device: Some(cod),
                        device_type: DeviceType::Classic,
                        ad_data: Vec::new(),
                    },
                );
            }
            offset += 14;
        }
    }

    fn handle_inquiry_result_rssi(&mut self, params: &[u8]) {
        if params.is_empty() {
            return;
        }
        let num_responses = params[0] as usize;
        log::info!("GAP: inquiry result with RSSI ({} responses)", num_responses);

        // Each: BD_ADDR (6) + Page_Scan_Rep_Mode (1) + Reserved (1) +
        // Class_of_Device (3) + Clock_Offset (2) + RSSI (1) = 14 bytes
        let mut offset = 1;
        for _ in 0..num_responses {
            if offset + 14 > params.len() {
                break;
            }
            if let Some(addr) = BdAddr::from_bytes(&params[offset..]) {
                let cod = u32::from_le_bytes([
                    params[offset + 8],
                    params[offset + 9],
                    params[offset + 10],
                    0,
                ]);
                let rssi = params[offset + 13] as i8;
                log::info!(
                    "GAP: discovered {} CoD=0x{:06X} RSSI={}dBm",
                    addr,
                    cod,
                    rssi
                );
                self.discovered.insert(
                    addr.0,
                    DiscoveredDevice {
                        addr,
                        addr_type: 0x00,
                        name: None,
                        rssi: Some(rssi),
                        class_of_device: Some(cod),
                        device_type: DeviceType::Classic,
                        ad_data: Vec::new(),
                    },
                );
            }
            offset += 14;
        }
    }

    fn handle_extended_inquiry_result(&mut self, params: &[u8]) {
        // EIR: Num_Responses (1) + BD_ADDR (6) + Page_Scan_Rep_Mode (1) +
        // Reserved (1) + Class_of_Device (3) + Clock_Offset (2) + RSSI (1) +
        // EIR_Data (240) = 255 bytes total
        if params.len() < 15 {
            return;
        }
        if let Some(addr) = BdAddr::from_bytes(&params[1..]) {
            let cod = u32::from_le_bytes([params[9], params[10], params[11], 0]);
            let rssi = params[14] as i8;
            let eir_data = if params.len() > 15 {
                &params[15..]
            } else {
                &[]
            };
            let (name, _uuids) = parse_ad_structures(eir_data);
            log::info!(
                "GAP: extended inquiry result {} name={:?} CoD=0x{:06X} RSSI={}dBm",
                addr,
                name,
                cod,
                rssi
            );
            self.discovered.insert(
                addr.0,
                DiscoveredDevice {
                    addr,
                    addr_type: 0x00,
                    name,
                    rssi: Some(rssi),
                    class_of_device: Some(cod),
                    device_type: DeviceType::Classic,
                    ad_data: eir_data.to_vec(),
                },
            );
        }
    }

    // -----------------------------------------------------------------------
    // Connection handlers
    // -----------------------------------------------------------------------

    fn handle_connection_complete(&mut self, params: &[u8]) {
        if params.len() < 11 {
            return;
        }
        let status = params[0];
        let handle = u16::from_le_bytes([params[1], params[2]]) & 0x0FFF;
        if let Some(addr) = BdAddr::from_bytes(&params[3..]) {
            if status == 0 {
                log::info!(
                    "GAP: Classic connection complete handle=0x{:04X} addr={}",
                    handle,
                    addr
                );
                self.connections.insert(
                    handle,
                    GapConnection {
                        addr,
                        addr_type: 0x00,
                        handle,
                        state: ConnectionState::Connected,
                        device_type: DeviceType::Classic,
                    },
                );
            } else {
                log::warn!(
                    "GAP: connection failed to {} status=0x{:02X}",
                    addr,
                    status
                );
            }
        }
    }

    fn handle_disconnection_complete(&mut self, params: &[u8]) {
        if params.len() < 4 {
            return;
        }
        let status = params[0];
        let handle = u16::from_le_bytes([params[1], params[2]]) & 0x0FFF;
        let reason = params[3];
        if status == 0 {
            log::info!(
                "GAP: disconnection complete handle=0x{:04X} reason=0x{:02X}",
                handle,
                reason
            );
            self.connections.remove(&handle);
        }
    }

    fn handle_remote_name_complete(&mut self, params: &[u8]) {
        if params.len() < 7 {
            return;
        }
        let status = params[0];
        if let Some(addr) = BdAddr::from_bytes(&params[1..]) {
            if status == 0 {
                let name_bytes = &params[7..];
                let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(name_bytes.len());
                if let Ok(name) = core::str::from_utf8(&name_bytes[..end]) {
                    log::info!("GAP: remote name for {}: {}", addr, name);
                    if let Some(dev) = self.discovered.get_mut(&addr.0) {
                        dev.name = Some(String::from(name));
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // BLE handlers
    // -----------------------------------------------------------------------

    fn handle_le_advertising_report(&mut self, params: &[u8]) {
        if params.is_empty() {
            return;
        }
        let num_reports = params[0] as usize;
        log::debug!("GAP: LE advertising report ({} reports)", num_reports);

        let mut offset = 1;
        for _ in 0..num_reports {
            // Event_Type (1) + Address_Type (1) + Address (6) + Data_Length (1) +
            // Data (N) + RSSI (1)
            if offset + 9 > params.len() {
                break;
            }
            let _event_type = params[offset];
            let addr_type = params[offset + 1];
            let addr = match BdAddr::from_bytes(&params[offset + 2..]) {
                Some(a) => a,
                None => break,
            };
            let data_len = params[offset + 8] as usize;
            offset += 9;

            if offset + data_len + 1 > params.len() {
                break;
            }
            let ad_data = &params[offset..offset + data_len];
            let rssi = params[offset + data_len] as i8;
            offset += data_len + 1;

            let (name, _uuids) = parse_ad_structures(ad_data);

            log::info!(
                "GAP: LE device {} (type=0x{:02X}) name={:?} RSSI={}dBm",
                addr,
                addr_type,
                name,
                rssi
            );

            self.discovered.insert(
                addr.0,
                DiscoveredDevice {
                    addr,
                    addr_type,
                    name,
                    rssi: Some(rssi),
                    class_of_device: None,
                    device_type: DeviceType::LowEnergy,
                    ad_data: ad_data.to_vec(),
                },
            );
        }
    }

    fn handle_le_connection_complete(&mut self, params: &[u8]) {
        if params.len() < 18 {
            return;
        }
        let status = params[0];
        let handle = u16::from_le_bytes([params[1], params[2]]) & 0x0FFF;
        let _role = params[3];
        let addr_type = params[4];
        if let Some(addr) = BdAddr::from_bytes(&params[5..]) {
            if status == 0 {
                log::info!(
                    "GAP: LE connection complete handle=0x{:04X} addr={} type=0x{:02X}",
                    handle,
                    addr,
                    addr_type
                );
                self.connections.insert(
                    handle,
                    GapConnection {
                        addr,
                        addr_type,
                        handle,
                        state: ConnectionState::Connected,
                        device_type: DeviceType::LowEnergy,
                    },
                );
            } else {
                log::warn!(
                    "GAP: LE connection failed to {} status=0x{:02X}",
                    addr,
                    status
                );
            }
        }
    }
}
