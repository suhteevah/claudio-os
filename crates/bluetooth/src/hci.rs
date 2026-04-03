//! HCI (Host Controller Interface) command/event packet layer
//!
//! Implements the core HCI protocol: building command packets, parsing event
//! packets, and defining the opcodes used throughout the Bluetooth stack.
//! All packets are constructed manually in byte buffers -- no std, no macros.
//!
//! HCI packet types:
//! - Command (host -> controller): opcode (2) + param_len (1) + params
//! - Event (controller -> host): event_code (1) + param_len (1) + params
//! - ACL data: handle+flags (2) + data_len (2) + data

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

// ---------------------------------------------------------------------------
// Bluetooth Device Address
// ---------------------------------------------------------------------------

/// 6-byte Bluetooth device address (BD_ADDR), stored in little-endian order.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BdAddr(pub [u8; 6]);

impl BdAddr {
    pub const ZERO: BdAddr = BdAddr([0; 6]);

    /// Parse from a 6-byte slice (little-endian as received over HCI).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 6 {
            return None;
        }
        let mut addr = [0u8; 6];
        addr.copy_from_slice(&bytes[..6]);
        Some(BdAddr(addr))
    }
}

impl fmt::Debug for BdAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.0[5], self.0[4], self.0[3], self.0[2], self.0[1], self.0[0]
        )
    }
}

impl fmt::Display for BdAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

// ---------------------------------------------------------------------------
// HCI errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HciError {
    /// USB transport failure
    TransportError,
    /// Controller returned a non-zero status code
    CommandFailed(u8),
    /// Response packet too short or malformed
    MalformedPacket,
    /// Timed out waiting for an event
    Timeout,
    /// Unsupported feature or parameter
    Unsupported,
    /// No Bluetooth controller found
    NoController,
    /// Command disallowed in current state
    CommandDisallowed,
}

impl fmt::Display for HciError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TransportError => write!(f, "HCI transport error"),
            Self::CommandFailed(s) => write!(f, "HCI command failed (status 0x{:02X})", s),
            Self::MalformedPacket => write!(f, "malformed HCI packet"),
            Self::Timeout => write!(f, "HCI timeout"),
            Self::Unsupported => write!(f, "unsupported HCI feature"),
            Self::NoController => write!(f, "no Bluetooth controller found"),
            Self::CommandDisallowed => write!(f, "HCI command disallowed"),
        }
    }
}

// ---------------------------------------------------------------------------
// HCI packet indicators
// ---------------------------------------------------------------------------

pub const HCI_COMMAND_PKT: u8 = 0x01;
pub const HCI_ACL_DATA_PKT: u8 = 0x02;
pub const HCI_SCO_DATA_PKT: u8 = 0x03;
pub const HCI_EVENT_PKT: u8 = 0x04;

// ---------------------------------------------------------------------------
// OGF (Opcode Group Field) definitions
// ---------------------------------------------------------------------------

pub const OGF_LINK_CONTROL: u16 = 0x01;
pub const OGF_LINK_POLICY: u16 = 0x02;
pub const OGF_HOST_CTL: u16 = 0x03;
pub const OGF_INFO_PARAM: u16 = 0x04;
pub const OGF_STATUS_PARAM: u16 = 0x05;
pub const OGF_LE_CTL: u16 = 0x08;

/// Build an opcode from OGF + OCF.
pub const fn opcode(ogf: u16, ocf: u16) -> u16 {
    (ogf << 10) | ocf
}

// ---------------------------------------------------------------------------
// HCI Command Opcodes
// ---------------------------------------------------------------------------

/// OGF 0x01 - Link Control
pub const HCI_OP_INQUIRY: u16 = opcode(OGF_LINK_CONTROL, 0x0001);
pub const HCI_OP_INQUIRY_CANCEL: u16 = opcode(OGF_LINK_CONTROL, 0x0002);
pub const HCI_OP_CREATE_CONNECTION: u16 = opcode(OGF_LINK_CONTROL, 0x0005);
pub const HCI_OP_DISCONNECT: u16 = opcode(OGF_LINK_CONTROL, 0x0006);
pub const HCI_OP_ACCEPT_CONN_REQUEST: u16 = opcode(OGF_LINK_CONTROL, 0x0009);
pub const HCI_OP_REMOTE_NAME_REQUEST: u16 = opcode(OGF_LINK_CONTROL, 0x0019);

/// OGF 0x03 - Host Controller & Baseband
pub const HCI_OP_RESET: u16 = opcode(OGF_HOST_CTL, 0x0003);
pub const HCI_OP_SET_EVENT_MASK: u16 = opcode(OGF_HOST_CTL, 0x0001);
pub const HCI_OP_WRITE_SCAN_ENABLE: u16 = opcode(OGF_HOST_CTL, 0x001A);
pub const HCI_OP_WRITE_CLASS_OF_DEVICE: u16 = opcode(OGF_HOST_CTL, 0x0024);
pub const HCI_OP_WRITE_LOCAL_NAME: u16 = opcode(OGF_HOST_CTL, 0x0013);

/// OGF 0x04 - Informational Parameters
pub const HCI_OP_READ_LOCAL_VERSION: u16 = opcode(OGF_INFO_PARAM, 0x0001);
pub const HCI_OP_READ_BD_ADDR: u16 = opcode(OGF_INFO_PARAM, 0x0009);
pub const HCI_OP_READ_BUFFER_SIZE: u16 = opcode(OGF_INFO_PARAM, 0x0005);

/// OGF 0x08 - LE Controller
pub const HCI_OP_LE_SET_EVENT_MASK: u16 = opcode(OGF_LE_CTL, 0x0001);
pub const HCI_OP_LE_READ_BUFFER_SIZE: u16 = opcode(OGF_LE_CTL, 0x0002);
pub const HCI_OP_LE_SET_SCAN_PARAMETERS: u16 = opcode(OGF_LE_CTL, 0x000B);
pub const HCI_OP_LE_SET_SCAN_ENABLE: u16 = opcode(OGF_LE_CTL, 0x000C);
pub const HCI_OP_LE_CREATE_CONNECTION: u16 = opcode(OGF_LE_CTL, 0x000D);
pub const HCI_OP_LE_CREATE_CONNECTION_CANCEL: u16 = opcode(OGF_LE_CTL, 0x000E);
pub const HCI_OP_LE_SET_ADVERTISING_PARAMETERS: u16 = opcode(OGF_LE_CTL, 0x0006);
pub const HCI_OP_LE_SET_ADVERTISING_DATA: u16 = opcode(OGF_LE_CTL, 0x0008);
pub const HCI_OP_LE_SET_ADVERTISING_ENABLE: u16 = opcode(OGF_LE_CTL, 0x000A);

// ---------------------------------------------------------------------------
// HCI Event Codes
// ---------------------------------------------------------------------------

pub const HCI_EVT_INQUIRY_COMPLETE: u8 = 0x01;
pub const HCI_EVT_INQUIRY_RESULT: u8 = 0x02;
pub const HCI_EVT_CONNECTION_COMPLETE: u8 = 0x03;
pub const HCI_EVT_CONNECTION_REQUEST: u8 = 0x04;
pub const HCI_EVT_DISCONNECTION_COMPLETE: u8 = 0x05;
pub const HCI_EVT_REMOTE_NAME_REQUEST_COMPLETE: u8 = 0x07;
pub const HCI_EVT_COMMAND_COMPLETE: u8 = 0x0E;
pub const HCI_EVT_COMMAND_STATUS: u8 = 0x0F;
pub const HCI_EVT_NUM_COMPLETED_PACKETS: u8 = 0x13;
pub const HCI_EVT_INQUIRY_RESULT_WITH_RSSI: u8 = 0x22;
pub const HCI_EVT_EXTENDED_INQUIRY_RESULT: u8 = 0x2F;
pub const HCI_EVT_LE_META: u8 = 0x3E;

// LE sub-event codes (inside LE Meta Event)
pub const HCI_LE_EVT_CONNECTION_COMPLETE: u8 = 0x01;
pub const HCI_LE_EVT_ADVERTISING_REPORT: u8 = 0x02;
pub const HCI_LE_EVT_CONNECTION_UPDATE_COMPLETE: u8 = 0x03;

// ---------------------------------------------------------------------------
// HCI command packet builder
// ---------------------------------------------------------------------------

/// Build a raw HCI command packet (without the HCI packet indicator byte).
///
/// Layout: opcode_lo, opcode_hi, param_len, params...
pub fn build_command(opcode: u16, params: &[u8]) -> Vec<u8> {
    let param_len = params.len() as u8;
    let mut pkt = Vec::with_capacity(3 + params.len());
    pkt.push((opcode & 0xFF) as u8);
    pkt.push((opcode >> 8) as u8);
    pkt.push(param_len);
    pkt.extend_from_slice(params);
    pkt
}

// ---------------------------------------------------------------------------
// Convenience command builders
// ---------------------------------------------------------------------------

/// HCI_Reset: no parameters.
pub fn cmd_reset() -> Vec<u8> {
    log::debug!("HCI: building Reset command");
    build_command(HCI_OP_RESET, &[])
}

/// HCI_Read_Local_Version_Information: no parameters.
pub fn cmd_read_local_version() -> Vec<u8> {
    log::debug!("HCI: building Read_Local_Version command");
    build_command(HCI_OP_READ_LOCAL_VERSION, &[])
}

/// HCI_Read_BD_ADDR: no parameters.
pub fn cmd_read_bd_addr() -> Vec<u8> {
    log::debug!("HCI: building Read_BD_ADDR command");
    build_command(HCI_OP_READ_BD_ADDR, &[])
}

/// HCI_Set_Event_Mask: 8-byte mask.
pub fn cmd_set_event_mask(mask: u64) -> Vec<u8> {
    log::debug!("HCI: building Set_Event_Mask (mask=0x{:016X})", mask);
    build_command(HCI_OP_SET_EVENT_MASK, &mask.to_le_bytes())
}

/// HCI_Write_Scan_Enable: scan_enable byte.
/// 0x00 = no scans, 0x01 = inquiry scan, 0x02 = page scan, 0x03 = both.
pub fn cmd_write_scan_enable(enable: u8) -> Vec<u8> {
    log::debug!("HCI: building Write_Scan_Enable (enable=0x{:02X})", enable);
    build_command(HCI_OP_WRITE_SCAN_ENABLE, &[enable])
}

/// HCI_Inquiry: LAP (3 bytes) + inquiry_length + num_responses.
/// Standard GIAC LAP = 0x9E8B33.
pub fn cmd_inquiry(inquiry_length: u8, num_responses: u8) -> Vec<u8> {
    log::debug!(
        "HCI: building Inquiry (length={}, max_responses={})",
        inquiry_length,
        num_responses
    );
    // GIAC LAP: 0x9E8B33 in little-endian byte order
    let params = [0x33, 0x8B, 0x9E, inquiry_length, num_responses];
    build_command(HCI_OP_INQUIRY, &params)
}

/// HCI_LE_Set_Scan_Parameters: active scan, 10ms interval, 10ms window, public addr.
pub fn cmd_le_set_scan_parameters(active: bool) -> Vec<u8> {
    let scan_type = if active { 0x01u8 } else { 0x00 };
    // Interval and window in units of 0.625ms: 0x0010 = 10ms
    let interval: u16 = 0x0010;
    let window: u16 = 0x0010;
    let own_addr_type: u8 = 0x00; // Public
    let filter_policy: u8 = 0x00; // Accept all
    log::debug!(
        "HCI: building LE_Set_Scan_Parameters (active={}, interval={}, window={})",
        active,
        interval,
        window
    );
    let mut params = vec![scan_type];
    params.extend_from_slice(&interval.to_le_bytes());
    params.extend_from_slice(&window.to_le_bytes());
    params.push(own_addr_type);
    params.push(filter_policy);
    build_command(HCI_OP_LE_SET_SCAN_PARAMETERS, &params)
}

/// HCI_LE_Set_Scan_Enable: enable/disable, filter_duplicates.
pub fn cmd_le_set_scan_enable(enable: bool, filter_duplicates: bool) -> Vec<u8> {
    log::debug!(
        "HCI: building LE_Set_Scan_Enable (enable={}, filter_dup={})",
        enable,
        filter_duplicates
    );
    let params = [enable as u8, filter_duplicates as u8];
    build_command(HCI_OP_LE_SET_SCAN_ENABLE, &params)
}

/// HCI_LE_Create_Connection to a specific peer address.
pub fn cmd_le_create_connection(peer_addr: &BdAddr, peer_addr_type: u8) -> Vec<u8> {
    log::debug!(
        "HCI: building LE_Create_Connection (peer={}, type=0x{:02X})",
        peer_addr,
        peer_addr_type
    );
    let mut params = Vec::with_capacity(25);
    // Scan interval: 0x0010 (10ms)
    params.extend_from_slice(&0x0010u16.to_le_bytes());
    // Scan window: 0x0010 (10ms)
    params.extend_from_slice(&0x0010u16.to_le_bytes());
    // Initiator filter policy: 0x00 (use peer address)
    params.push(0x00);
    // Peer address type
    params.push(peer_addr_type);
    // Peer address (6 bytes)
    params.extend_from_slice(&peer_addr.0);
    // Own address type: 0x00 (public)
    params.push(0x00);
    // Connection interval min: 0x0018 (30ms)
    params.extend_from_slice(&0x0018u16.to_le_bytes());
    // Connection interval max: 0x0028 (50ms)
    params.extend_from_slice(&0x0028u16.to_le_bytes());
    // Latency: 0
    params.extend_from_slice(&0x0000u16.to_le_bytes());
    // Supervision timeout: 0x002A (420ms)
    params.extend_from_slice(&0x002Au16.to_le_bytes());
    // Min CE length: 0
    params.extend_from_slice(&0x0000u16.to_le_bytes());
    // Max CE length: 0
    params.extend_from_slice(&0x0000u16.to_le_bytes());
    build_command(HCI_OP_LE_CREATE_CONNECTION, &params)
}

// ---------------------------------------------------------------------------
// HCI event parsing
// ---------------------------------------------------------------------------

/// Parsed HCI event header.
#[derive(Debug, Clone)]
pub struct HciEvent {
    pub event_code: u8,
    pub params: Vec<u8>,
}

impl HciEvent {
    /// Parse an HCI event from raw bytes (without the HCI packet indicator).
    /// Layout: event_code (1) + param_total_len (1) + params (N)
    pub fn parse(data: &[u8]) -> Result<Self, HciError> {
        if data.len() < 2 {
            log::warn!("HCI: event packet too short ({} bytes)", data.len());
            return Err(HciError::MalformedPacket);
        }
        let event_code = data[0];
        let param_len = data[1] as usize;
        if data.len() < 2 + param_len {
            log::warn!(
                "HCI: event 0x{:02X} truncated (expected {} param bytes, got {})",
                event_code,
                param_len,
                data.len() - 2
            );
            return Err(HciError::MalformedPacket);
        }
        let params = data[2..2 + param_len].to_vec();
        log::trace!(
            "HCI: parsed event 0x{:02X} ({} param bytes)",
            event_code,
            param_len
        );
        Ok(HciEvent { event_code, params })
    }

    /// If this is a Command Complete event, extract (num_hci_cmds, opcode, status, return_params).
    pub fn as_command_complete(&self) -> Option<(u8, u16, u8, &[u8])> {
        if self.event_code != HCI_EVT_COMMAND_COMPLETE {
            return None;
        }
        if self.params.len() < 4 {
            return None;
        }
        let num_cmds = self.params[0];
        let op = u16::from_le_bytes([self.params[1], self.params[2]]);
        let status = self.params[3];
        let ret = &self.params[4..];
        Some((num_cmds, op, status, ret))
    }

    /// If this is a Command Status event, extract (status, num_hci_cmds, opcode).
    pub fn as_command_status(&self) -> Option<(u8, u8, u16)> {
        if self.event_code != HCI_EVT_COMMAND_STATUS {
            return None;
        }
        if self.params.len() < 4 {
            return None;
        }
        let status = self.params[0];
        let num_cmds = self.params[1];
        let op = u16::from_le_bytes([self.params[2], self.params[3]]);
        Some((status, num_cmds, op))
    }

    /// If this is an LE Meta Event, extract (subevent_code, subevent_params).
    pub fn as_le_meta(&self) -> Option<(u8, &[u8])> {
        if self.event_code != HCI_EVT_LE_META {
            return None;
        }
        if self.params.is_empty() {
            return None;
        }
        Some((self.params[0], &self.params[1..]))
    }
}

// ---------------------------------------------------------------------------
// ACL data packet
// ---------------------------------------------------------------------------

/// Parsed HCI ACL data packet.
#[derive(Debug, Clone)]
pub struct HciAclData {
    /// Connection handle (12 bits)
    pub handle: u16,
    /// Packet boundary flag (2 bits)
    pub pb_flag: u8,
    /// Broadcast flag (2 bits)
    pub bc_flag: u8,
    /// Payload data
    pub data: Vec<u8>,
}

impl HciAclData {
    /// Parse an ACL data packet from raw bytes (without the HCI packet indicator).
    /// Layout: handle+flags (2) + data_total_len (2) + data (N)
    pub fn parse(data: &[u8]) -> Result<Self, HciError> {
        if data.len() < 4 {
            return Err(HciError::MalformedPacket);
        }
        let hdr = u16::from_le_bytes([data[0], data[1]]);
        let handle = hdr & 0x0FFF;
        let pb_flag = ((hdr >> 12) & 0x03) as u8;
        let bc_flag = ((hdr >> 14) & 0x03) as u8;
        let data_len = u16::from_le_bytes([data[2], data[3]]) as usize;
        if data.len() < 4 + data_len {
            return Err(HciError::MalformedPacket);
        }
        let payload = data[4..4 + data_len].to_vec();
        log::trace!(
            "HCI: parsed ACL data handle=0x{:03X} pb={} bc={} len={}",
            handle,
            pb_flag,
            bc_flag,
            data_len
        );
        Ok(HciAclData {
            handle,
            pb_flag,
            bc_flag,
            data: payload,
        })
    }

    /// Serialize this ACL data packet to bytes (without the HCI packet indicator).
    pub fn to_bytes(&self) -> Vec<u8> {
        let hdr = self.handle | ((self.pb_flag as u16) << 12) | ((self.bc_flag as u16) << 14);
        let data_len = self.data.len() as u16;
        let mut pkt = Vec::with_capacity(4 + self.data.len());
        pkt.extend_from_slice(&hdr.to_le_bytes());
        pkt.extend_from_slice(&data_len.to_le_bytes());
        pkt.extend_from_slice(&self.data);
        pkt
    }
}

// ---------------------------------------------------------------------------
// Local version info (from Read_Local_Version response)
// ---------------------------------------------------------------------------

/// Parsed response from HCI_Read_Local_Version_Information.
#[derive(Debug, Clone)]
pub struct LocalVersionInfo {
    pub hci_version: u8,
    pub hci_revision: u16,
    pub lmp_version: u8,
    pub manufacturer: u16,
    pub lmp_subversion: u16,
}

impl LocalVersionInfo {
    /// Parse from the return parameters of a Command Complete event for Read_Local_Version.
    pub fn parse(params: &[u8]) -> Result<Self, HciError> {
        if params.len() < 8 {
            return Err(HciError::MalformedPacket);
        }
        Ok(LocalVersionInfo {
            hci_version: params[0],
            hci_revision: u16::from_le_bytes([params[1], params[2]]),
            lmp_version: params[3],
            manufacturer: u16::from_le_bytes([params[4], params[5]]),
            lmp_subversion: u16::from_le_bytes([params[6], params[7]]),
        })
    }
}
