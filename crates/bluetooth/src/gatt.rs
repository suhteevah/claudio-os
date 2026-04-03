//! GATT (Generic Attribute Profile) Client
//!
//! Implements the GATT client protocol over ATT (Attribute Protocol) on the
//! L2CAP fixed channel CID 0x0004. Supports service discovery, characteristic
//! discovery, and read/write operations.
//!
//! ATT PDU format: opcode (1) + parameters (variable)

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use crate::hci::HciError;

// ---------------------------------------------------------------------------
// ATT opcodes
// ---------------------------------------------------------------------------

pub const ATT_ERROR_RSP: u8 = 0x01;
pub const ATT_EXCHANGE_MTU_REQ: u8 = 0x02;
pub const ATT_EXCHANGE_MTU_RSP: u8 = 0x03;
pub const ATT_FIND_INFO_REQ: u8 = 0x04;
pub const ATT_FIND_INFO_RSP: u8 = 0x05;
pub const ATT_FIND_BY_TYPE_VALUE_REQ: u8 = 0x06;
pub const ATT_FIND_BY_TYPE_VALUE_RSP: u8 = 0x07;
pub const ATT_READ_BY_TYPE_REQ: u8 = 0x08;
pub const ATT_READ_BY_TYPE_RSP: u8 = 0x09;
pub const ATT_READ_REQ: u8 = 0x0A;
pub const ATT_READ_RSP: u8 = 0x0B;
pub const ATT_READ_BLOB_REQ: u8 = 0x0C;
pub const ATT_READ_BLOB_RSP: u8 = 0x0D;
pub const ATT_WRITE_REQ: u8 = 0x12;
pub const ATT_WRITE_RSP: u8 = 0x13;
pub const ATT_WRITE_CMD: u8 = 0x52;
pub const ATT_HANDLE_VALUE_NTF: u8 = 0x1B;
pub const ATT_HANDLE_VALUE_IND: u8 = 0x1D;
pub const ATT_HANDLE_VALUE_CFM: u8 = 0x1E;
pub const ATT_READ_BY_GROUP_TYPE_REQ: u8 = 0x10;
pub const ATT_READ_BY_GROUP_TYPE_RSP: u8 = 0x11;

// ---------------------------------------------------------------------------
// ATT error codes
// ---------------------------------------------------------------------------

pub const ATT_ERR_INVALID_HANDLE: u8 = 0x01;
pub const ATT_ERR_READ_NOT_PERMITTED: u8 = 0x02;
pub const ATT_ERR_WRITE_NOT_PERMITTED: u8 = 0x03;
pub const ATT_ERR_INVALID_PDU: u8 = 0x04;
pub const ATT_ERR_INSUFFICIENT_AUTHENTICATION: u8 = 0x05;
pub const ATT_ERR_ATTRIBUTE_NOT_FOUND: u8 = 0x0A;
pub const ATT_ERR_INSUFFICIENT_ENCRYPTION: u8 = 0x0F;

// ---------------------------------------------------------------------------
// Well-known GATT UUIDs (16-bit)
// ---------------------------------------------------------------------------

/// Primary Service Declaration
pub const UUID_PRIMARY_SERVICE: u16 = 0x2800;
/// Secondary Service Declaration
pub const UUID_SECONDARY_SERVICE: u16 = 0x2801;
/// Characteristic Declaration
pub const UUID_CHARACTERISTIC: u16 = 0x2803;
/// Client Characteristic Configuration Descriptor (CCCD)
pub const UUID_CCCD: u16 = 0x2902;
/// Device Name characteristic
pub const UUID_DEVICE_NAME: u16 = 0x2A00;
/// Appearance characteristic
pub const UUID_APPEARANCE: u16 = 0x2A01;
/// Battery Level characteristic
pub const UUID_BATTERY_LEVEL: u16 = 0x2A19;
/// HID Information
pub const UUID_HID_INFORMATION: u16 = 0x2A4A;
/// HID Report Map
pub const UUID_HID_REPORT_MAP: u16 = 0x2A4B;
/// HID Control Point
pub const UUID_HID_CONTROL_POINT: u16 = 0x2A4C;
/// HID Report
pub const UUID_HID_REPORT: u16 = 0x2A4D;
/// HID Protocol Mode
pub const UUID_HID_PROTOCOL_MODE: u16 = 0x2A4E;

/// Generic Access service
pub const UUID_SERVICE_GENERIC_ACCESS: u16 = 0x1800;
/// Generic Attribute service
pub const UUID_SERVICE_GENERIC_ATTRIBUTE: u16 = 0x1801;
/// Battery Service
pub const UUID_SERVICE_BATTERY: u16 = 0x180F;
/// Human Interface Device service
pub const UUID_SERVICE_HID: u16 = 0x1812;

// ---------------------------------------------------------------------------
// GATT data structures
// ---------------------------------------------------------------------------

/// A UUID that can be either 16-bit or 128-bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GattUuid {
    Uuid16(u16),
    Uuid128([u8; 16]),
}

impl GattUuid {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        match data.len() {
            2 => Some(GattUuid::Uuid16(u16::from_le_bytes([data[0], data[1]]))),
            16 => {
                let mut uuid = [0u8; 16];
                uuid.copy_from_slice(data);
                Some(GattUuid::Uuid128(uuid))
            }
            _ => None,
        }
    }
}

/// Characteristic properties bitmask.
#[derive(Debug, Clone, Copy)]
pub struct CharProperties(pub u8);

impl CharProperties {
    pub const BROADCAST: u8 = 0x01;
    pub const READ: u8 = 0x02;
    pub const WRITE_WITHOUT_RESPONSE: u8 = 0x04;
    pub const WRITE: u8 = 0x08;
    pub const NOTIFY: u8 = 0x10;
    pub const INDICATE: u8 = 0x20;

    pub fn can_read(self) -> bool {
        self.0 & Self::READ != 0
    }
    pub fn can_write(self) -> bool {
        self.0 & Self::WRITE != 0
    }
    pub fn can_notify(self) -> bool {
        self.0 & Self::NOTIFY != 0
    }
    pub fn can_indicate(self) -> bool {
        self.0 & Self::INDICATE != 0
    }
}

/// A discovered GATT service.
#[derive(Debug, Clone)]
pub struct GattService {
    /// Attribute handle of the service declaration
    pub handle: u16,
    /// End group handle
    pub end_handle: u16,
    /// Service UUID
    pub uuid: GattUuid,
}

/// A discovered GATT characteristic.
#[derive(Debug, Clone)]
pub struct GattCharacteristic {
    /// Attribute handle of the characteristic declaration
    pub decl_handle: u16,
    /// Value handle
    pub value_handle: u16,
    /// Properties
    pub properties: CharProperties,
    /// Characteristic UUID
    pub uuid: GattUuid,
    /// CCCD handle (if discovered)
    pub cccd_handle: Option<u16>,
}

// ---------------------------------------------------------------------------
// ATT PDU builders
// ---------------------------------------------------------------------------

/// Build an Exchange MTU Request.
pub fn build_exchange_mtu_req(client_mtu: u16) -> Vec<u8> {
    log::debug!("GATT: building Exchange MTU Request (mtu={})", client_mtu);
    let mut pdu = Vec::with_capacity(3);
    pdu.push(ATT_EXCHANGE_MTU_REQ);
    pdu.extend_from_slice(&client_mtu.to_le_bytes());
    pdu
}

/// Build a Read By Group Type Request (for service discovery).
pub fn build_read_by_group_type_req(start: u16, end: u16, uuid: u16) -> Vec<u8> {
    log::debug!(
        "GATT: Read By Group Type [0x{:04X}..0x{:04X}] uuid=0x{:04X}",
        start,
        end,
        uuid
    );
    let mut pdu = Vec::with_capacity(7);
    pdu.push(ATT_READ_BY_GROUP_TYPE_REQ);
    pdu.extend_from_slice(&start.to_le_bytes());
    pdu.extend_from_slice(&end.to_le_bytes());
    pdu.extend_from_slice(&uuid.to_le_bytes());
    pdu
}

/// Build a Read By Type Request (for characteristic discovery).
pub fn build_read_by_type_req(start: u16, end: u16, uuid: u16) -> Vec<u8> {
    log::debug!(
        "GATT: Read By Type [0x{:04X}..0x{:04X}] uuid=0x{:04X}",
        start,
        end,
        uuid
    );
    let mut pdu = Vec::with_capacity(7);
    pdu.push(ATT_READ_BY_TYPE_REQ);
    pdu.extend_from_slice(&start.to_le_bytes());
    pdu.extend_from_slice(&end.to_le_bytes());
    pdu.extend_from_slice(&uuid.to_le_bytes());
    pdu
}

/// Build a Find Information Request (for descriptor discovery).
pub fn build_find_info_req(start: u16, end: u16) -> Vec<u8> {
    log::debug!("GATT: Find Information [0x{:04X}..0x{:04X}]", start, end);
    let mut pdu = Vec::with_capacity(5);
    pdu.push(ATT_FIND_INFO_REQ);
    pdu.extend_from_slice(&start.to_le_bytes());
    pdu.extend_from_slice(&end.to_le_bytes());
    pdu
}

/// Build a Read Request.
pub fn build_read_req(handle: u16) -> Vec<u8> {
    log::debug!("GATT: Read Request handle=0x{:04X}", handle);
    let mut pdu = Vec::with_capacity(3);
    pdu.push(ATT_READ_REQ);
    pdu.extend_from_slice(&handle.to_le_bytes());
    pdu
}

/// Build a Write Request.
pub fn build_write_req(handle: u16, value: &[u8]) -> Vec<u8> {
    log::debug!(
        "GATT: Write Request handle=0x{:04X} len={}",
        handle,
        value.len()
    );
    let mut pdu = Vec::with_capacity(3 + value.len());
    pdu.push(ATT_WRITE_REQ);
    pdu.extend_from_slice(&handle.to_le_bytes());
    pdu.extend_from_slice(value);
    pdu
}

/// Build a Write Command (no response expected).
pub fn build_write_cmd(handle: u16, value: &[u8]) -> Vec<u8> {
    log::debug!(
        "GATT: Write Command handle=0x{:04X} len={}",
        handle,
        value.len()
    );
    let mut pdu = Vec::with_capacity(3 + value.len());
    pdu.push(ATT_WRITE_CMD);
    pdu.extend_from_slice(&handle.to_le_bytes());
    pdu.extend_from_slice(value);
    pdu
}

/// Build a Handle Value Confirmation (response to an Indication).
pub fn build_handle_value_cfm() -> Vec<u8> {
    log::trace!("GATT: Handle Value Confirmation");
    alloc::vec![ATT_HANDLE_VALUE_CFM]
}

/// Enable notifications on a CCCD (Client Characteristic Configuration Descriptor).
pub fn build_enable_notifications(cccd_handle: u16) -> Vec<u8> {
    log::debug!("GATT: enabling notifications on CCCD 0x{:04X}", cccd_handle);
    build_write_req(cccd_handle, &[0x01, 0x00])
}

/// Enable indications on a CCCD.
pub fn build_enable_indications(cccd_handle: u16) -> Vec<u8> {
    log::debug!("GATT: enabling indications on CCCD 0x{:04X}", cccd_handle);
    build_write_req(cccd_handle, &[0x02, 0x00])
}

// ---------------------------------------------------------------------------
// ATT response parsing
// ---------------------------------------------------------------------------

/// Parse a Read By Group Type Response to extract services.
pub fn parse_read_by_group_type_rsp(data: &[u8]) -> Result<Vec<GattService>, HciError> {
    if data.is_empty() || data[0] != ATT_READ_BY_GROUP_TYPE_RSP {
        return Err(HciError::MalformedPacket);
    }
    if data.len() < 2 {
        return Err(HciError::MalformedPacket);
    }
    let attr_data_len = data[1] as usize;
    if attr_data_len < 6 {
        return Err(HciError::MalformedPacket);
    }

    let mut services = Vec::new();
    let mut offset = 2;
    while offset + attr_data_len <= data.len() {
        let handle = u16::from_le_bytes([data[offset], data[offset + 1]]);
        let end_handle = u16::from_le_bytes([data[offset + 2], data[offset + 3]]);
        let uuid_bytes = &data[offset + 4..offset + attr_data_len];
        if let Some(uuid) = GattUuid::from_bytes(uuid_bytes) {
            log::debug!(
                "GATT: service handle=0x{:04X}-0x{:04X} uuid={:?}",
                handle,
                end_handle,
                uuid
            );
            services.push(GattService {
                handle,
                end_handle,
                uuid,
            });
        }
        offset += attr_data_len;
    }
    Ok(services)
}

/// Parse a Read By Type Response to extract characteristics.
pub fn parse_read_by_type_rsp(data: &[u8]) -> Result<Vec<GattCharacteristic>, HciError> {
    if data.is_empty() || data[0] != ATT_READ_BY_TYPE_RSP {
        return Err(HciError::MalformedPacket);
    }
    if data.len() < 2 {
        return Err(HciError::MalformedPacket);
    }
    let attr_data_len = data[1] as usize;
    if attr_data_len < 7 {
        return Err(HciError::MalformedPacket);
    }

    let mut chars = Vec::new();
    let mut offset = 2;
    while offset + attr_data_len <= data.len() {
        let decl_handle = u16::from_le_bytes([data[offset], data[offset + 1]]);
        let properties = data[offset + 2];
        let value_handle = u16::from_le_bytes([data[offset + 3], data[offset + 4]]);
        let uuid_bytes = &data[offset + 5..offset + attr_data_len];
        if let Some(uuid) = GattUuid::from_bytes(uuid_bytes) {
            log::debug!(
                "GATT: characteristic decl=0x{:04X} val=0x{:04X} props=0x{:02X} uuid={:?}",
                decl_handle,
                value_handle,
                properties,
                uuid
            );
            chars.push(GattCharacteristic {
                decl_handle,
                value_handle,
                properties: CharProperties(properties),
                uuid,
                cccd_handle: None,
            });
        }
        offset += attr_data_len;
    }
    Ok(chars)
}

/// Parse an ATT Error Response.
pub fn parse_error_rsp(data: &[u8]) -> Option<(u8, u16, u8)> {
    if data.len() < 5 || data[0] != ATT_ERROR_RSP {
        return None;
    }
    let req_opcode = data[1];
    let handle = u16::from_le_bytes([data[2], data[3]]);
    let error_code = data[4];
    log::debug!(
        "GATT: ATT error req=0x{:02X} handle=0x{:04X} error=0x{:02X}",
        req_opcode,
        handle,
        error_code
    );
    Some((req_opcode, handle, error_code))
}

// ---------------------------------------------------------------------------
// GATT client state
// ---------------------------------------------------------------------------

/// GATT client for a single BLE connection.
#[derive(Debug)]
pub struct GattClient {
    /// HCI connection handle
    pub handle: u16,
    /// Negotiated ATT MTU
    pub mtu: u16,
    /// Discovered services
    pub services: Vec<GattService>,
    /// Discovered characteristics, keyed by value handle
    pub characteristics: BTreeMap<u16, GattCharacteristic>,
}

impl GattClient {
    /// Create a new GATT client for a connection.
    pub fn new(handle: u16) -> Self {
        log::info!("GATT: client created for handle 0x{:04X}", handle);
        GattClient {
            handle,
            mtu: 23, // Default ATT MTU
            services: Vec::new(),
            characteristics: BTreeMap::new(),
        }
    }

    /// Handle an incoming ATT PDU on this connection.
    /// Returns an optional response PDU to send back.
    pub fn handle_att_pdu(&mut self, pdu: &[u8]) -> Option<Vec<u8>> {
        if pdu.is_empty() {
            return None;
        }

        let opcode = pdu[0];
        match opcode {
            ATT_EXCHANGE_MTU_RSP => {
                if pdu.len() >= 3 {
                    let server_mtu = u16::from_le_bytes([pdu[1], pdu[2]]);
                    self.mtu = self.mtu.min(server_mtu);
                    log::info!("GATT: MTU negotiated = {}", self.mtu);
                }
                None
            }
            ATT_READ_BY_GROUP_TYPE_RSP => {
                if let Ok(services) = parse_read_by_group_type_rsp(pdu) {
                    self.services.extend(services);
                }
                None
            }
            ATT_READ_BY_TYPE_RSP => {
                if let Ok(chars) = parse_read_by_type_rsp(pdu) {
                    for c in chars {
                        self.characteristics.insert(c.value_handle, c);
                    }
                }
                None
            }
            ATT_HANDLE_VALUE_NTF => {
                if pdu.len() >= 3 {
                    let attr_handle = u16::from_le_bytes([pdu[1], pdu[2]]);
                    let value = &pdu[3..];
                    log::debug!(
                        "GATT: notification handle=0x{:04X} len={}",
                        attr_handle,
                        value.len()
                    );
                }
                None // Notifications require no response
            }
            ATT_HANDLE_VALUE_IND => {
                if pdu.len() >= 3 {
                    let attr_handle = u16::from_le_bytes([pdu[1], pdu[2]]);
                    let value = &pdu[3..];
                    log::debug!(
                        "GATT: indication handle=0x{:04X} len={}",
                        attr_handle,
                        value.len()
                    );
                }
                Some(build_handle_value_cfm()) // Must confirm indications
            }
            ATT_ERROR_RSP => {
                parse_error_rsp(pdu);
                None
            }
            _ => {
                log::trace!("GATT: unhandled ATT opcode 0x{:02X}", opcode);
                None
            }
        }
    }
}
