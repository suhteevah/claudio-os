//! HCI over USB Transport Layer
//!
//! Bluetooth controllers present as USB devices with class 0xE0 (Wireless Controller),
//! subclass 0x01 (RF Controller), protocol 0x01 (Bluetooth). The HCI transport uses:
//!
//! - **Control endpoint (EP0)**: HCI commands (host -> controller)
//! - **Interrupt IN endpoint**: HCI events (controller -> host)
//! - **Bulk OUT endpoint**: ACL data (host -> controller)
//! - **Bulk IN endpoint**: ACL data (controller -> host)
//!
//! This module provides a trait-based transport abstraction so the HCI layer can
//! send commands and receive events without knowing USB details.

use alloc::vec::Vec;
use crate::hci::HciError;

// ---------------------------------------------------------------------------
// USB Bluetooth class constants
// ---------------------------------------------------------------------------

/// USB class code for Wireless Controller
pub const USB_CLASS_WIRELESS: u8 = 0xE0;
/// USB subclass for RF Controller
pub const USB_SUBCLASS_RF: u8 = 0x01;
/// USB protocol for Bluetooth Programming Interface
pub const USB_PROTOCOL_BLUETOOTH: u8 = 0x01;

/// HCI command request type for USB control transfer (host-to-device, class, device).
pub const HCI_CMD_REQUEST_TYPE: u8 = 0x20;
/// bRequest value for sending HCI commands over USB control endpoint.
pub const HCI_CMD_REQUEST: u8 = 0x00;

// ---------------------------------------------------------------------------
// USB endpoint types
// ---------------------------------------------------------------------------

/// Endpoint direction: IN (device to host)
pub const EP_DIR_IN: u8 = 0x80;
/// Endpoint direction: OUT (host to device)
pub const EP_DIR_OUT: u8 = 0x00;
/// Endpoint transfer type: Bulk
pub const EP_TYPE_BULK: u8 = 0x02;
/// Endpoint transfer type: Interrupt
pub const EP_TYPE_INTERRUPT: u8 = 0x03;

// ---------------------------------------------------------------------------
// USB Bluetooth device descriptor
// ---------------------------------------------------------------------------

/// Describes a detected USB Bluetooth controller and its endpoint addresses.
#[derive(Debug, Clone)]
pub struct BtUsbDevice {
    /// USB device slot / address (from xHCI)
    pub slot_id: u8,
    /// Interface number for the Bluetooth HCI
    pub interface_num: u8,
    /// Interrupt IN endpoint address (for HCI events)
    pub evt_endpoint: u8,
    /// Bulk IN endpoint address (for ACL data in)
    pub acl_in_endpoint: u8,
    /// Bulk OUT endpoint address (for ACL data out)
    pub acl_out_endpoint: u8,
    /// Maximum packet size for the interrupt endpoint
    pub evt_max_packet: u16,
    /// Maximum packet size for the bulk endpoints
    pub acl_max_packet: u16,
}

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

/// Abstraction over the USB transport layer for HCI packets.
///
/// The kernel's xHCI driver implements this trait to provide the actual USB
/// transfer operations. This decouples the Bluetooth stack from USB details.
pub trait HciTransport {
    /// Send an HCI command via USB control transfer (EP0).
    ///
    /// The command bytes should be the raw HCI command (opcode + params),
    /// without the HCI packet indicator byte.
    fn send_command(&mut self, cmd: &[u8]) -> Result<(), HciError>;

    /// Receive an HCI event from the interrupt IN endpoint.
    ///
    /// Returns the raw event bytes (event_code + param_len + params),
    /// without the HCI packet indicator byte. Returns `None` if no event
    /// is pending (non-blocking).
    fn receive_event(&mut self) -> Result<Option<Vec<u8>>, HciError>;

    /// Send ACL data via the bulk OUT endpoint.
    fn send_acl_data(&mut self, data: &[u8]) -> Result<(), HciError>;

    /// Receive ACL data from the bulk IN endpoint.
    /// Returns `None` if no data is pending (non-blocking).
    fn receive_acl_data(&mut self) -> Result<Option<Vec<u8>>, HciError>;
}

// ---------------------------------------------------------------------------
// USB device detection
// ---------------------------------------------------------------------------

/// Check whether a USB device's class/subclass/protocol identifies it as a
/// Bluetooth HCI controller.
pub fn is_bluetooth_controller(class: u8, subclass: u8, protocol: u8) -> bool {
    let result = class == USB_CLASS_WIRELESS
        && subclass == USB_SUBCLASS_RF
        && protocol == USB_PROTOCOL_BLUETOOTH;
    if result {
        log::info!(
            "USB: detected Bluetooth controller (class=0x{:02X} sub=0x{:02X} proto=0x{:02X})",
            class,
            subclass,
            protocol
        );
    }
    result
}

/// Parse USB interface descriptors to find Bluetooth HCI endpoints.
///
/// Scans the configuration descriptor data for an interface with the BT class
/// triple, then extracts the interrupt IN and bulk IN/OUT endpoint addresses.
pub fn parse_bt_endpoints(config_desc: &[u8]) -> Option<BtUsbDevice> {
    log::debug!(
        "USB: scanning {} bytes of config descriptor for BT endpoints",
        config_desc.len()
    );

    let mut offset = 0;
    let mut found_interface = false;
    let mut interface_num = 0u8;
    let mut evt_ep = None;
    let mut acl_in_ep = None;
    let mut acl_out_ep = None;
    let mut evt_max = 0u16;
    let mut acl_max = 0u16;

    while offset + 1 < config_desc.len() {
        let len = config_desc[offset] as usize;
        let desc_type = config_desc[offset + 1];

        if len == 0 || offset + len > config_desc.len() {
            break;
        }

        // Interface descriptor (type 0x04)
        if desc_type == 0x04 && len >= 9 {
            let intf_class = config_desc[offset + 5];
            let intf_subclass = config_desc[offset + 6];
            let intf_protocol = config_desc[offset + 7];
            if is_bluetooth_controller(intf_class, intf_subclass, intf_protocol) {
                found_interface = true;
                interface_num = config_desc[offset + 2];
                log::info!("USB: found BT interface #{}", interface_num);
            } else if found_interface {
                // We've moved past the BT interface, stop looking for endpoints
                break;
            }
        }

        // Endpoint descriptor (type 0x05) -- only collect if inside BT interface
        if desc_type == 0x05 && len >= 7 && found_interface {
            let ep_addr = config_desc[offset + 2];
            let ep_attrs = config_desc[offset + 3];
            let ep_max = u16::from_le_bytes([config_desc[offset + 4], config_desc[offset + 5]]);
            let ep_type = ep_attrs & 0x03;
            let ep_dir_in = (ep_addr & 0x80) != 0;

            log::debug!(
                "USB: BT endpoint 0x{:02X} type={} dir={} max_pkt={}",
                ep_addr,
                ep_type,
                if ep_dir_in { "IN" } else { "OUT" },
                ep_max
            );

            match (ep_type, ep_dir_in) {
                (EP_TYPE_INTERRUPT, true) if evt_ep.is_none() => {
                    evt_ep = Some(ep_addr);
                    evt_max = ep_max;
                }
                (EP_TYPE_BULK, true) if acl_in_ep.is_none() => {
                    acl_in_ep = Some(ep_addr);
                    acl_max = ep_max;
                }
                (EP_TYPE_BULK, false) if acl_out_ep.is_none() => {
                    acl_out_ep = Some(ep_addr);
                    if acl_max == 0 {
                        acl_max = ep_max;
                    }
                }
                _ => {}
            }
        }

        offset += len;
    }

    if let (Some(evt), Some(acl_in), Some(acl_out)) = (evt_ep, acl_in_ep, acl_out_ep) {
        log::info!(
            "USB: BT endpoints -- evt=0x{:02X} acl_in=0x{:02X} acl_out=0x{:02X}",
            evt,
            acl_in,
            acl_out
        );
        Some(BtUsbDevice {
            slot_id: 0, // Caller must fill in
            interface_num,
            evt_endpoint: evt,
            acl_in_endpoint: acl_in,
            acl_out_endpoint: acl_out,
            evt_max_packet: evt_max,
            acl_max_packet: acl_max,
        })
    } else {
        log::warn!("USB: BT interface found but missing endpoints");
        None
    }
}
