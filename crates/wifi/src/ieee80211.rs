//! IEEE 802.11 frame parsing and construction.
//!
//! Handles the frame types relevant to station (STA) mode operation:
//! beacons, probe requests/responses, authentication, association,
//! data frames, and QoS data frames.
//!
//! All frames share a common MAC header with a 2-byte frame control field,
//! duration, and up to 4 address fields.

use alloc::string::String;
use alloc::vec::Vec;

// -------------------------------------------------------------------
// Frame Control field
// -------------------------------------------------------------------

/// Frame type (bits 3:2 of Frame Control).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Management = 0,
    Control = 1,
    Data = 2,
    Extension = 3,
}

/// Management frame subtypes (bits 7:4 of Frame Control).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MgmtSubtype {
    AssocRequest = 0,
    AssocResponse = 1,
    ReassocRequest = 2,
    ReassocResponse = 3,
    ProbeRequest = 4,
    ProbeResponse = 5,
    Beacon = 8,
    Disassoc = 10,
    Auth = 11,
    Deauth = 12,
    Action = 13,
}

/// Data frame subtypes (bits 7:4 of Frame Control).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DataSubtype {
    Data = 0,
    Null = 4,
    QosData = 8,
    QosNull = 12,
}

/// Parsed frame control field.
#[derive(Debug, Clone, Copy)]
pub struct FrameControl {
    /// Raw 16-bit value.
    pub raw: u16,
}

impl FrameControl {
    pub fn from_raw(raw: u16) -> Self {
        Self { raw }
    }

    /// Protocol version (bits 1:0). Always 0 for current 802.11.
    pub fn protocol_version(self) -> u8 {
        (self.raw & 0x03) as u8
    }

    /// Frame type (bits 3:2).
    pub fn frame_type(self) -> FrameType {
        match (self.raw >> 2) & 0x03 {
            0 => FrameType::Management,
            1 => FrameType::Control,
            2 => FrameType::Data,
            _ => FrameType::Extension,
        }
    }

    /// Frame subtype (bits 7:4).
    pub fn subtype(self) -> u8 {
        ((self.raw >> 4) & 0x0F) as u8
    }

    /// To DS bit (bit 8).
    pub fn to_ds(self) -> bool {
        self.raw & (1 << 8) != 0
    }

    /// From DS bit (bit 9).
    pub fn from_ds(self) -> bool {
        self.raw & (1 << 9) != 0
    }

    /// Protected frame bit (bit 14) — indicates frame body is encrypted.
    pub fn protected(self) -> bool {
        self.raw & (1 << 14) != 0
    }

    /// QoS subtype flag (bit 7 of subtype field).
    pub fn is_qos(self) -> bool {
        self.frame_type() == FrameType::Data && (self.subtype() & 0x08) != 0
    }
}

// -------------------------------------------------------------------
// MAC header
// -------------------------------------------------------------------

/// Parsed 802.11 MAC header.
#[derive(Debug, Clone)]
pub struct MacHeader {
    pub frame_control: FrameControl,
    pub duration: u16,
    /// Address 1: Receiver / Destination.
    pub addr1: [u8; 6],
    /// Address 2: Transmitter / Source.
    pub addr2: [u8; 6],
    /// Address 3: BSSID (for infrastructure mode).
    pub addr3: [u8; 6],
    /// Sequence control (fragment number + sequence number).
    pub seq_ctrl: u16,
    /// Address 4 (only present in WDS / mesh frames).
    pub addr4: Option<[u8; 6]>,
    /// QoS control field (only present in QoS data frames).
    pub qos_ctrl: Option<u16>,
    /// Total header length in bytes.
    pub header_len: usize,
}

impl MacHeader {
    /// Parse a MAC header from the beginning of a raw frame.
    ///
    /// Returns `None` if the frame is too short.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 24 {
            return None;
        }

        let fc = FrameControl::from_raw(u16::from_le_bytes([data[0], data[1]]));
        let duration = u16::from_le_bytes([data[2], data[3]]);

        let mut addr1 = [0u8; 6];
        let mut addr2 = [0u8; 6];
        let mut addr3 = [0u8; 6];
        addr1.copy_from_slice(&data[4..10]);
        addr2.copy_from_slice(&data[10..16]);
        addr3.copy_from_slice(&data[16..22]);

        let seq_ctrl = u16::from_le_bytes([data[22], data[23]]);

        let mut header_len = 24;
        let mut addr4 = None;
        let mut qos_ctrl = None;

        // Address 4 present when both To DS and From DS are set (WDS/mesh)
        if fc.to_ds() && fc.from_ds() {
            if data.len() < 30 {
                return None;
            }
            let mut a4 = [0u8; 6];
            a4.copy_from_slice(&data[24..30]);
            addr4 = Some(a4);
            header_len = 30;
        }

        // QoS control field present in QoS data frames
        if fc.is_qos() {
            if data.len() < header_len + 2 {
                return None;
            }
            qos_ctrl = Some(u16::from_le_bytes([data[header_len], data[header_len + 1]]));
            header_len += 2;
        }

        Some(Self {
            frame_control: fc,
            duration,
            addr1,
            addr2,
            addr3,
            seq_ctrl,
            addr4,
            qos_ctrl,
            header_len,
        })
    }
}

// -------------------------------------------------------------------
// Information Elements (IEs) — tagged parameters in management frames
// -------------------------------------------------------------------

/// An information element from a management frame body.
#[derive(Debug, Clone)]
pub struct InformationElement<'a> {
    /// Element ID.
    pub id: u8,
    /// Element data.
    pub data: &'a [u8],
}

/// Well-known IE IDs.
#[allow(dead_code)]
pub mod ie_id {
    pub const SSID: u8 = 0;
    pub const SUPPORTED_RATES: u8 = 1;
    pub const DS_PARAMETER_SET: u8 = 3;
    pub const TIM: u8 = 5;
    pub const COUNTRY: u8 = 7;
    pub const RSN: u8 = 48;
    pub const HT_CAPABILITIES: u8 = 45;
    pub const HT_OPERATION: u8 = 61;
    pub const VHT_CAPABILITIES: u8 = 191;
    pub const VHT_OPERATION: u8 = 192;
    pub const VENDOR_SPECIFIC: u8 = 221;
}

/// Iterator over information elements in a management frame body.
pub struct IeIterator<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> IeIterator<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
}

impl<'a> Iterator for IeIterator<'a> {
    type Item = InformationElement<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset + 2 > self.data.len() {
            return None;
        }
        let id = self.data[self.offset];
        let len = self.data[self.offset + 1] as usize;
        self.offset += 2;

        if self.offset + len > self.data.len() {
            return None;
        }
        let element_data = &self.data[self.offset..self.offset + len];
        self.offset += len;

        Some(InformationElement {
            id,
            data: element_data,
        })
    }
}

// -------------------------------------------------------------------
// Beacon / Probe Response parsing
// -------------------------------------------------------------------

/// Parsed beacon or probe response frame.
#[derive(Debug, Clone)]
pub struct BeaconInfo {
    /// BSSID (from MAC header addr3).
    pub bssid: [u8; 6],
    /// SSID (from IE 0). Empty string if hidden.
    pub ssid: String,
    /// Channel (from DS Parameter Set IE).
    pub channel: u8,
    /// Beacon interval in TUs (1 TU = 1024 microseconds).
    pub beacon_interval: u16,
    /// Capability information (ESS, IBSS, privacy, etc.).
    pub capability: u16,
    /// Whether WPA2 (RSN) is present.
    pub has_rsn: bool,
    /// Whether the network is open (no encryption).
    pub is_open: bool,
}

/// Parse a beacon or probe response frame.
///
/// `data` is the full 802.11 frame including MAC header.
pub fn parse_beacon(data: &[u8]) -> Option<BeaconInfo> {
    let header = MacHeader::parse(data)?;

    // Beacon / probe response body starts after MAC header.
    // First 12 bytes: timestamp(8) + beacon_interval(2) + capability(2)
    let body_start = header.header_len;
    if data.len() < body_start + 12 {
        return None;
    }

    let body = &data[body_start..];
    let beacon_interval = u16::from_le_bytes([body[8], body[9]]);
    let capability = u16::from_le_bytes([body[10], body[11]]);

    // Parse IEs starting at offset 12 in the body
    let ie_data = &body[12..];
    let mut ssid = String::new();
    let mut channel = 0u8;
    let mut has_rsn = false;

    for ie in IeIterator::new(ie_data) {
        match ie.id {
            ie_id::SSID => {
                if let Ok(s) = core::str::from_utf8(ie.data) {
                    ssid = String::from(s);
                }
            }
            ie_id::DS_PARAMETER_SET => {
                if !ie.data.is_empty() {
                    channel = ie.data[0];
                }
            }
            ie_id::RSN => {
                has_rsn = true;
            }
            _ => {}
        }
    }

    // Privacy bit (bit 4 of capability) indicates some form of encryption
    let privacy = capability & (1 << 4) != 0;
    let is_open = !privacy && !has_rsn;

    log::trace!(
        "wifi::ieee80211: beacon SSID=\"{}\" ch={} bssid={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X} rsn={} open={}",
        ssid, channel,
        header.addr3[0], header.addr3[1], header.addr3[2],
        header.addr3[3], header.addr3[4], header.addr3[5],
        has_rsn, is_open
    );

    Some(BeaconInfo {
        bssid: header.addr3,
        ssid,
        channel,
        beacon_interval,
        capability,
        has_rsn,
        is_open,
    })
}

// -------------------------------------------------------------------
// Frame construction helpers
// -------------------------------------------------------------------

/// Build an authentication frame (Open System, sequence 1).
pub fn build_auth_frame(
    bssid: &[u8; 6],
    src_addr: &[u8; 6],
) -> Vec<u8> {
    log::debug!("wifi::ieee80211: building auth frame -> {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        bssid[0], bssid[1], bssid[2], bssid[3], bssid[4], bssid[5]);

    let mut frame = Vec::with_capacity(30);

    // Frame control: Management, Auth subtype (0x00B0)
    let fc: u16 = (FrameType::Management as u16) << 2 | (MgmtSubtype::Auth as u16) << 4;
    frame.extend_from_slice(&fc.to_le_bytes());
    frame.extend_from_slice(&0u16.to_le_bytes()); // duration
    frame.extend_from_slice(bssid);                // addr1: destination (AP)
    frame.extend_from_slice(src_addr);             // addr2: source (us)
    frame.extend_from_slice(bssid);                // addr3: BSSID
    frame.extend_from_slice(&0u16.to_le_bytes()); // seq ctrl (filled by firmware)

    // Auth body: algorithm(2) + seq_num(2) + status(2)
    frame.extend_from_slice(&0u16.to_le_bytes()); // Open System
    frame.extend_from_slice(&1u16.to_le_bytes()); // Sequence 1
    frame.extend_from_slice(&0u16.to_le_bytes()); // Status: success

    frame
}

/// Build an association request frame.
pub fn build_assoc_request(
    bssid: &[u8; 6],
    src_addr: &[u8; 6],
    ssid: &str,
) -> Vec<u8> {
    log::debug!("wifi::ieee80211: building assoc request for SSID=\"{}\"", ssid);

    let mut frame = Vec::with_capacity(64 + ssid.len());

    // Frame control: Management, AssocRequest subtype
    let fc: u16 = (FrameType::Management as u16) << 2 | (MgmtSubtype::AssocRequest as u16) << 4;
    frame.extend_from_slice(&fc.to_le_bytes());
    frame.extend_from_slice(&0u16.to_le_bytes()); // duration
    frame.extend_from_slice(bssid);                // addr1: destination (AP)
    frame.extend_from_slice(src_addr);             // addr2: source (us)
    frame.extend_from_slice(bssid);                // addr3: BSSID
    frame.extend_from_slice(&0u16.to_le_bytes()); // seq ctrl

    // Assoc request body: capability(2) + listen_interval(2)
    let capability: u16 = 0x0431; // ESS + Privacy + Short Slot Time + Short Preamble
    frame.extend_from_slice(&capability.to_le_bytes());
    frame.extend_from_slice(&10u16.to_le_bytes()); // listen interval

    // IE: SSID
    frame.push(ie_id::SSID);
    frame.push(ssid.len() as u8);
    frame.extend_from_slice(ssid.as_bytes());

    // IE: Supported Rates (basic 802.11b/g rates)
    frame.push(ie_id::SUPPORTED_RATES);
    frame.push(8);
    frame.extend_from_slice(&[0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24]); // 1,2,5.5,11,6,9,12,18 Mbps

    // IE: RSN (WPA2-PSK, AES-CCMP)
    frame.push(ie_id::RSN);
    let rsn = build_rsn_ie_data();
    frame.push(rsn.len() as u8);
    frame.extend_from_slice(&rsn);

    frame
}

/// Build the data portion of an RSN information element for WPA2-PSK / AES-CCMP.
fn build_rsn_ie_data() -> Vec<u8> {
    let mut rsn = Vec::with_capacity(20);
    rsn.extend_from_slice(&1u16.to_le_bytes()); // RSN version 1
    // Group cipher: AES-CCMP
    rsn.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);
    // Pairwise cipher count: 1
    rsn.extend_from_slice(&1u16.to_le_bytes());
    // Pairwise cipher: AES-CCMP
    rsn.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);
    // AKM count: 1
    rsn.extend_from_slice(&1u16.to_le_bytes());
    // AKM: PSK
    rsn.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x02]);
    // RSN capabilities
    rsn.extend_from_slice(&0u16.to_le_bytes());
    rsn
}

/// Build a QoS data frame (802.11 header only, caller appends encrypted payload).
pub fn build_qos_data_header(
    bssid: &[u8; 6],
    src_addr: &[u8; 6],
    dst_addr: &[u8; 6],
    seq_num: u16,
    tid: u8,
) -> Vec<u8> {
    let mut frame = Vec::with_capacity(28);

    // Frame control: Data, QoS Data subtype, To DS, Protected
    let fc: u16 = (FrameType::Data as u16) << 2
        | (DataSubtype::QosData as u16) << 4
        | (1 << 8)   // To DS
        | (1 << 14); // Protected
    frame.extend_from_slice(&fc.to_le_bytes());
    frame.extend_from_slice(&0u16.to_le_bytes()); // duration
    frame.extend_from_slice(bssid);                // addr1: BSSID (AP)
    frame.extend_from_slice(src_addr);             // addr2: source (us)
    frame.extend_from_slice(dst_addr);             // addr3: destination
    // Sequence control: seq_num in bits 15:4, fragment 0 in bits 3:0
    let seq_ctrl = (seq_num << 4) & 0xFFF0;
    frame.extend_from_slice(&seq_ctrl.to_le_bytes());
    // QoS control: TID in bits 3:0
    frame.extend_from_slice(&(tid as u16).to_le_bytes());

    frame
}
