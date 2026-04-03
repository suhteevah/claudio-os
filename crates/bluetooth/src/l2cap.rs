//! L2CAP (Logical Link Control and Adaptation Protocol)
//!
//! L2CAP provides connection-oriented and connectionless data channels over HCI.
//! For Bluetooth Classic, it uses connection-oriented channels with signaling.
//! For BLE, it uses fixed channels and LE credit-based flow control.
//!
//! Fixed channel IDs:
//! - 0x0001: L2CAP signaling (Classic)
//! - 0x0004: ATT (BLE attribute protocol)
//! - 0x0005: L2CAP LE signaling
//! - 0x0006: Security Manager Protocol (SMP)

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use crate::hci::{HciAclData, HciError};

// ---------------------------------------------------------------------------
// L2CAP constants
// ---------------------------------------------------------------------------

/// L2CAP signaling channel (Classic)
pub const L2CAP_CID_SIGNALING: u16 = 0x0001;
/// L2CAP connectionless channel
pub const L2CAP_CID_CONNECTIONLESS: u16 = 0x0002;
/// ATT (Attribute Protocol) fixed channel (BLE)
pub const L2CAP_CID_ATT: u16 = 0x0004;
/// L2CAP LE signaling channel
pub const L2CAP_CID_LE_SIGNALING: u16 = 0x0005;
/// Security Manager Protocol fixed channel
pub const L2CAP_CID_SMP: u16 = 0x0006;

/// First dynamically allocated CID
pub const L2CAP_CID_DYNAMIC_START: u16 = 0x0040;

// ---------------------------------------------------------------------------
// L2CAP signaling command codes
// ---------------------------------------------------------------------------

pub const L2CAP_CMD_REJECT: u8 = 0x01;
pub const L2CAP_CMD_CONN_REQ: u8 = 0x02;
pub const L2CAP_CMD_CONN_RSP: u8 = 0x03;
pub const L2CAP_CMD_CONFIG_REQ: u8 = 0x04;
pub const L2CAP_CMD_CONFIG_RSP: u8 = 0x05;
pub const L2CAP_CMD_DISCONN_REQ: u8 = 0x06;
pub const L2CAP_CMD_DISCONN_RSP: u8 = 0x07;
pub const L2CAP_CMD_INFO_REQ: u8 = 0x0A;
pub const L2CAP_CMD_INFO_RSP: u8 = 0x0B;

// LE signaling commands
pub const L2CAP_CMD_LE_CREDIT_CONN_REQ: u8 = 0x14;
pub const L2CAP_CMD_LE_CREDIT_CONN_RSP: u8 = 0x15;
pub const L2CAP_CMD_LE_FLOW_CONTROL_CREDIT: u8 = 0x16;

// ---------------------------------------------------------------------------
// L2CAP connection result codes
// ---------------------------------------------------------------------------

pub const L2CAP_CONN_SUCCESS: u16 = 0x0000;
pub const L2CAP_CONN_PENDING: u16 = 0x0001;
pub const L2CAP_CONN_REFUSED_PSM: u16 = 0x0002;
pub const L2CAP_CONN_REFUSED_SECURITY: u16 = 0x0003;
pub const L2CAP_CONN_REFUSED_RESOURCES: u16 = 0x0004;

// ---------------------------------------------------------------------------
// Common PSM values
// ---------------------------------------------------------------------------

/// Protocol/Service Multiplexer for SDP
pub const PSM_SDP: u16 = 0x0001;
/// PSM for HID Control
pub const PSM_HID_CONTROL: u16 = 0x0011;
/// PSM for HID Interrupt
pub const PSM_HID_INTERRUPT: u16 = 0x0013;
/// PSM for ATT (over BR/EDR)
pub const PSM_ATT: u16 = 0x001F;

// ---------------------------------------------------------------------------
// L2CAP packet parsing
// ---------------------------------------------------------------------------

/// Parsed L2CAP basic header.
#[derive(Debug, Clone)]
pub struct L2capPacket {
    /// Payload length
    pub length: u16,
    /// Channel ID
    pub cid: u16,
    /// Payload data
    pub payload: Vec<u8>,
}

impl L2capPacket {
    /// Parse an L2CAP packet from ACL data payload.
    /// Layout: length (2) + cid (2) + payload (length bytes)
    pub fn parse(data: &[u8]) -> Result<Self, HciError> {
        if data.len() < 4 {
            log::warn!("L2CAP: packet too short ({} bytes)", data.len());
            return Err(HciError::MalformedPacket);
        }
        let length = u16::from_le_bytes([data[0], data[1]]);
        let cid = u16::from_le_bytes([data[2], data[3]]);
        let payload_end = 4 + length as usize;
        if data.len() < payload_end {
            log::warn!(
                "L2CAP: truncated packet cid=0x{:04X} (expected {} bytes, got {})",
                cid,
                length,
                data.len() - 4
            );
            return Err(HciError::MalformedPacket);
        }
        let payload = data[4..payload_end].to_vec();
        log::trace!("L2CAP: parsed packet cid=0x{:04X} len={}", cid, length);
        Ok(L2capPacket {
            length,
            cid,
            payload,
        })
    }

    /// Serialize this L2CAP packet to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(4 + self.payload.len());
        pkt.extend_from_slice(&(self.payload.len() as u16).to_le_bytes());
        pkt.extend_from_slice(&self.cid.to_le_bytes());
        pkt.extend_from_slice(&self.payload);
        pkt
    }
}

// ---------------------------------------------------------------------------
// L2CAP signaling command
// ---------------------------------------------------------------------------

/// Parsed L2CAP signaling command.
#[derive(Debug, Clone)]
pub struct SignalingCommand {
    pub code: u8,
    pub identifier: u8,
    pub data: Vec<u8>,
}

impl SignalingCommand {
    /// Parse a signaling command from L2CAP payload.
    pub fn parse(data: &[u8]) -> Result<Self, HciError> {
        if data.len() < 4 {
            return Err(HciError::MalformedPacket);
        }
        let code = data[0];
        let identifier = data[1];
        let length = u16::from_le_bytes([data[2], data[3]]) as usize;
        if data.len() < 4 + length {
            return Err(HciError::MalformedPacket);
        }
        log::trace!(
            "L2CAP: signaling cmd=0x{:02X} id={} len={}",
            code,
            identifier,
            length
        );
        Ok(SignalingCommand {
            code,
            identifier,
            data: data[4..4 + length].to_vec(),
        })
    }

    /// Serialize to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(4 + self.data.len());
        pkt.push(self.code);
        pkt.push(self.identifier);
        pkt.extend_from_slice(&(self.data.len() as u16).to_le_bytes());
        pkt.extend_from_slice(&self.data);
        pkt
    }
}

// ---------------------------------------------------------------------------
// L2CAP channel state
// ---------------------------------------------------------------------------

/// State of an L2CAP connection-oriented channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelState {
    Closed,
    WaitConnect,
    WaitConfig,
    Open,
    WaitDisconnect,
}

/// An L2CAP connection-oriented channel.
#[derive(Debug, Clone)]
pub struct L2capChannel {
    /// Local CID
    pub local_cid: u16,
    /// Remote CID
    pub remote_cid: u16,
    /// PSM
    pub psm: u16,
    /// Channel state
    pub state: ChannelState,
    /// HCI connection handle
    pub handle: u16,
    /// MTU (negotiated)
    pub mtu: u16,
}

/// LE credit-based flow control channel.
#[derive(Debug, Clone)]
pub struct LeCreditChannel {
    /// Local CID
    pub local_cid: u16,
    /// Remote CID
    pub remote_cid: u16,
    /// PSM
    pub psm: u16,
    /// Local credits remaining
    pub local_credits: u16,
    /// Remote credits remaining
    pub remote_credits: u16,
    /// Maximum PDU size
    pub mps: u16,
    /// MTU
    pub mtu: u16,
    /// Channel state
    pub state: ChannelState,
    /// HCI connection handle
    pub handle: u16,
}

// ---------------------------------------------------------------------------
// L2CAP manager
// ---------------------------------------------------------------------------

/// Manages L2CAP channels across multiple HCI connections.
pub struct L2capManager {
    /// Next signaling command identifier
    next_id: u8,
    /// Next dynamically allocated local CID
    next_cid: u16,
    /// Active connection-oriented channels, keyed by local CID
    pub channels: BTreeMap<u16, L2capChannel>,
    /// Active LE credit-based channels, keyed by local CID
    pub le_channels: BTreeMap<u16, LeCreditChannel>,
}

impl L2capManager {
    /// Create a new L2CAP manager.
    pub fn new() -> Self {
        log::debug!("L2CAP: manager initialized");
        L2capManager {
            next_id: 1,
            next_cid: L2CAP_CID_DYNAMIC_START,
            channels: BTreeMap::new(),
            le_channels: BTreeMap::new(),
        }
    }

    /// Allocate the next signaling command identifier.
    fn next_identifier(&mut self) -> u8 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        id
    }

    /// Allocate a local CID for a new channel.
    fn alloc_cid(&mut self) -> u16 {
        let cid = self.next_cid;
        self.next_cid += 1;
        cid
    }

    /// Build a Connection Request signaling command for Classic Bluetooth.
    pub fn build_connection_request(&mut self, handle: u16, psm: u16) -> (u16, Vec<u8>) {
        let local_cid = self.alloc_cid();
        let id = self.next_identifier();

        log::info!(
            "L2CAP: connection request handle=0x{:04X} psm=0x{:04X} local_cid=0x{:04X}",
            handle,
            psm,
            local_cid
        );

        self.channels.insert(
            local_cid,
            L2capChannel {
                local_cid,
                remote_cid: 0,
                psm,
                state: ChannelState::WaitConnect,
                handle,
                mtu: 672, // Default L2CAP MTU
            },
        );

        let mut data = Vec::with_capacity(4);
        data.extend_from_slice(&psm.to_le_bytes());
        data.extend_from_slice(&local_cid.to_le_bytes());

        let cmd = SignalingCommand {
            code: L2CAP_CMD_CONN_REQ,
            identifier: id,
            data,
        };

        let l2cap = L2capPacket {
            length: 0, // filled by to_bytes
            cid: L2CAP_CID_SIGNALING,
            payload: cmd.to_bytes(),
        };

        (local_cid, l2cap.to_bytes())
    }

    /// Build a Connection Response signaling command.
    pub fn build_connection_response(
        &mut self,
        identifier: u8,
        remote_cid: u16,
        result: u16,
        handle: u16,
        psm: u16,
    ) -> (u16, Vec<u8>) {
        let local_cid = self.alloc_cid();

        log::info!(
            "L2CAP: connection response remote_cid=0x{:04X} local_cid=0x{:04X} result=0x{:04X}",
            remote_cid,
            local_cid,
            result
        );

        if result == L2CAP_CONN_SUCCESS {
            self.channels.insert(
                local_cid,
                L2capChannel {
                    local_cid,
                    remote_cid,
                    psm,
                    state: ChannelState::WaitConfig,
                    handle,
                    mtu: 672,
                },
            );
        }

        let mut data = Vec::with_capacity(8);
        data.extend_from_slice(&local_cid.to_le_bytes());
        data.extend_from_slice(&remote_cid.to_le_bytes());
        data.extend_from_slice(&result.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes()); // status

        let cmd = SignalingCommand {
            code: L2CAP_CMD_CONN_RSP,
            identifier,
            data,
        };

        let l2cap = L2capPacket {
            length: 0,
            cid: L2CAP_CID_SIGNALING,
            payload: cmd.to_bytes(),
        };

        (local_cid, l2cap.to_bytes())
    }

    /// Build an LE Credit Based Connection Request.
    pub fn build_le_credit_connection_request(
        &mut self,
        handle: u16,
        psm: u16,
        mtu: u16,
        mps: u16,
        initial_credits: u16,
    ) -> (u16, Vec<u8>) {
        let local_cid = self.alloc_cid();
        let id = self.next_identifier();

        log::info!(
            "L2CAP: LE credit conn req handle=0x{:04X} psm=0x{:04X} local_cid=0x{:04X} credits={}",
            handle,
            psm,
            local_cid,
            initial_credits
        );

        self.le_channels.insert(
            local_cid,
            LeCreditChannel {
                local_cid,
                remote_cid: 0,
                psm,
                local_credits: initial_credits,
                remote_credits: 0,
                mps,
                mtu,
                state: ChannelState::WaitConnect,
                handle,
            },
        );

        let mut data = Vec::with_capacity(10);
        data.extend_from_slice(&psm.to_le_bytes());
        data.extend_from_slice(&local_cid.to_le_bytes());
        data.extend_from_slice(&mtu.to_le_bytes());
        data.extend_from_slice(&mps.to_le_bytes());
        data.extend_from_slice(&initial_credits.to_le_bytes());

        let cmd = SignalingCommand {
            code: L2CAP_CMD_LE_CREDIT_CONN_REQ,
            identifier: id,
            data,
        };

        let l2cap = L2capPacket {
            length: 0,
            cid: L2CAP_CID_LE_SIGNALING,
            payload: cmd.to_bytes(),
        };

        (local_cid, l2cap.to_bytes())
    }

    /// Handle an incoming signaling command. Returns optional response bytes.
    pub fn handle_signaling(&mut self, handle: u16, data: &[u8]) -> Option<Vec<u8>> {
        let cmd = match SignalingCommand::parse(data) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("L2CAP: failed to parse signaling command: {:?}", e);
                return None;
            }
        };

        match cmd.code {
            L2CAP_CMD_CONN_REQ => {
                if cmd.data.len() < 4 {
                    return None;
                }
                let psm = u16::from_le_bytes([cmd.data[0], cmd.data[1]]);
                let remote_cid = u16::from_le_bytes([cmd.data[2], cmd.data[3]]);
                log::info!(
                    "L2CAP: incoming connection request psm=0x{:04X} remote_cid=0x{:04X}",
                    psm,
                    remote_cid
                );
                let (_local_cid, response) = self.build_connection_response(
                    cmd.identifier,
                    remote_cid,
                    L2CAP_CONN_SUCCESS,
                    handle,
                    psm,
                );
                Some(response)
            }
            L2CAP_CMD_CONN_RSP => {
                if cmd.data.len() < 8 {
                    return None;
                }
                let dest_cid = u16::from_le_bytes([cmd.data[0], cmd.data[1]]);
                let source_cid = u16::from_le_bytes([cmd.data[2], cmd.data[3]]);
                let result = u16::from_le_bytes([cmd.data[4], cmd.data[5]]);
                log::info!(
                    "L2CAP: connection response dest=0x{:04X} src=0x{:04X} result=0x{:04X}",
                    dest_cid,
                    source_cid,
                    result
                );
                if result == L2CAP_CONN_SUCCESS {
                    if let Some(ch) = self.channels.get_mut(&dest_cid) {
                        ch.remote_cid = source_cid;
                        ch.state = ChannelState::WaitConfig;
                    }
                }
                None
            }
            L2CAP_CMD_DISCONN_REQ => {
                if cmd.data.len() < 4 {
                    return None;
                }
                let dest_cid = u16::from_le_bytes([cmd.data[0], cmd.data[1]]);
                let source_cid = u16::from_le_bytes([cmd.data[2], cmd.data[3]]);
                log::info!(
                    "L2CAP: disconnect request dest=0x{:04X} src=0x{:04X}",
                    dest_cid,
                    source_cid
                );
                self.channels.remove(&dest_cid);

                // Build disconnect response
                let mut resp_data = Vec::with_capacity(4);
                resp_data.extend_from_slice(&dest_cid.to_le_bytes());
                resp_data.extend_from_slice(&source_cid.to_le_bytes());
                let resp_cmd = SignalingCommand {
                    code: L2CAP_CMD_DISCONN_RSP,
                    identifier: cmd.identifier,
                    data: resp_data,
                };
                let l2cap = L2capPacket {
                    length: 0,
                    cid: L2CAP_CID_SIGNALING,
                    payload: resp_cmd.to_bytes(),
                };
                Some(l2cap.to_bytes())
            }
            other => {
                log::debug!("L2CAP: unhandled signaling command 0x{:02X}", other);
                None
            }
        }
    }

    /// Wrap payload data into an L2CAP packet for a given CID, then into an ACL packet.
    pub fn wrap_acl(&self, handle: u16, cid: u16, payload: &[u8]) -> Vec<u8> {
        let l2cap = L2capPacket {
            length: payload.len() as u16,
            cid,
            payload: payload.to_vec(),
        };
        let l2cap_bytes = l2cap.to_bytes();

        let acl = HciAclData {
            handle,
            pb_flag: 0x02, // First automatically-flushable packet
            bc_flag: 0x00, // Point-to-point
            data: l2cap_bytes,
        };
        acl.to_bytes()
    }

    /// Remove all channels associated with a given HCI connection handle.
    pub fn disconnect_handle(&mut self, handle: u16) {
        log::info!("L2CAP: removing all channels for handle 0x{:04X}", handle);
        self.channels.retain(|_, ch| ch.handle != handle);
        self.le_channels.retain(|_, ch| ch.handle != handle);
    }
}
