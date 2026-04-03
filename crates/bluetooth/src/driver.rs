//! High-level Bluetooth Controller Driver
//!
//! `BluetoothController` is the top-level API. It wraps HCI command/event
//! handling, GAP discovery, L2CAP channel management, GATT service access,
//! and HID keyboard/mouse input into a single struct.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::gap::{DeviceType, DiscoveredDevice, GapManager, ScanResult};
use crate::gatt::{self, GattClient};
use crate::hci::{self, BdAddr, HciAclData, HciError, HciEvent, LocalVersionInfo};
use crate::hid::{self, BtHidEvent, BtKeyboardReport, BtKeyboardState, BtMouseReport};
use crate::l2cap::{self, L2capManager, L2capPacket, L2CAP_CID_ATT, L2CAP_CID_SIGNALING};
use crate::usb_transport::HciTransport;

// ---------------------------------------------------------------------------
// Controller state
// ---------------------------------------------------------------------------

/// Overall state of the Bluetooth controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerState {
    /// Not yet initialized
    Uninitialized,
    /// HCI Reset sent, waiting for completion
    Resetting,
    /// Controller ready for operations
    Ready,
    /// Scanning for devices (inquiry or LE scan)
    Scanning,
    /// Error state
    Error,
}

/// A tracked Bluetooth device with connection + HID state.
#[derive(Debug)]
struct ConnectedDevice {
    addr: BdAddr,
    handle: u16,
    device_type: DeviceType,
    /// GATT client (BLE connections)
    gatt: Option<GattClient>,
    /// Keyboard state (if this device is a keyboard)
    keyboard: Option<BtKeyboardState>,
    /// Whether we've set this device to boot protocol
    boot_protocol: bool,
}

// ---------------------------------------------------------------------------
// BluetoothController
// ---------------------------------------------------------------------------

/// The Bluetooth host controller driver.
///
/// Manages the full Bluetooth stack from HCI through to HID input events.
/// The kernel creates one instance and calls `poll()` regularly to process
/// incoming events.
pub struct BluetoothController<T: HciTransport> {
    /// USB HCI transport
    transport: T,
    /// Controller state
    state: ControllerState,
    /// Local BD_ADDR (read during init)
    local_addr: BdAddr,
    /// Local version info
    version: Option<LocalVersionInfo>,
    /// GAP manager (discovery + connections)
    gap: GapManager,
    /// L2CAP manager (channels)
    l2cap: L2capManager,
    /// Connected devices with per-device state, keyed by HCI handle
    devices: BTreeMap<u16, ConnectedDevice>,
    /// Pending HID events from all connected keyboards/mice
    hid_events: Vec<BtHidEvent>,
}

impl<T: HciTransport> BluetoothController<T> {
    /// Create a new Bluetooth controller with the given USB transport.
    pub fn new(transport: T) -> Self {
        log::info!("BT: creating Bluetooth controller");
        BluetoothController {
            transport,
            state: ControllerState::Uninitialized,
            local_addr: BdAddr::ZERO,
            version: None,
            gap: GapManager::new(),
            l2cap: L2capManager::new(),
            devices: BTreeMap::new(),
            hid_events: Vec::new(),
        }
    }

    /// Initialize the Bluetooth controller: reset, read version, read BD_ADDR.
    pub fn init(&mut self) -> Result<(), HciError> {
        log::info!("BT: initializing controller...");
        self.state = ControllerState::Resetting;

        // Step 1: HCI Reset
        let cmd = hci::cmd_reset();
        self.transport.send_command(&cmd)?;
        let _evt = self.wait_command_complete(hci::HCI_OP_RESET)?;
        log::info!("BT: controller reset complete");

        // Step 2: Read Local Version
        let cmd = hci::cmd_read_local_version();
        self.transport.send_command(&cmd)?;
        let evt = self.wait_command_complete(hci::HCI_OP_READ_LOCAL_VERSION)?;
        if let Some((_ncmds, _op, status, ret)) = evt.as_command_complete() {
            if status == 0 {
                if let Ok(ver) = LocalVersionInfo::parse(ret) {
                    log::info!(
                        "BT: HCI v{} rev=0x{:04X} LMP v{} mfr=0x{:04X} sub=0x{:04X}",
                        ver.hci_version,
                        ver.hci_revision,
                        ver.lmp_version,
                        ver.manufacturer,
                        ver.lmp_subversion
                    );
                    self.version = Some(ver);
                }
            }
        }

        // Step 3: Read BD_ADDR
        let cmd = hci::cmd_read_bd_addr();
        self.transport.send_command(&cmd)?;
        let evt = self.wait_command_complete(hci::HCI_OP_READ_BD_ADDR)?;
        if let Some((_ncmds, _op, status, ret)) = evt.as_command_complete() {
            if status == 0 {
                if let Some(addr) = BdAddr::from_bytes(ret) {
                    self.local_addr = addr;
                    log::info!("BT: local BD_ADDR = {}", self.local_addr);
                }
            }
        }

        // Step 4: Set Event Mask (enable common events + LE meta)
        let mask: u64 = 0x20001FFFFFFFFFFF; // Default + LE Meta Event
        let cmd = hci::cmd_set_event_mask(mask);
        self.transport.send_command(&cmd)?;
        let _ = self.wait_command_complete(hci::HCI_OP_SET_EVENT_MASK)?;
        log::info!("BT: event mask configured");

        self.state = ControllerState::Ready;
        log::info!("BT: controller ready");
        Ok(())
    }

    /// Get the controller state.
    pub fn state(&self) -> ControllerState {
        self.state
    }

    /// Get the local Bluetooth address.
    pub fn local_address(&self) -> BdAddr {
        self.local_addr
    }

    // -----------------------------------------------------------------------
    // Scanning / Discovery
    // -----------------------------------------------------------------------

    /// Start a Classic Bluetooth inquiry (discover nearby devices).
    pub fn start_inquiry(&mut self, duration_1_28s: u8, max_responses: u8) -> Result<(), HciError> {
        if self.state != ControllerState::Ready {
            return Err(HciError::CommandDisallowed);
        }
        log::info!(
            "BT: starting inquiry (duration={}, max={})",
            duration_1_28s,
            max_responses
        );
        self.gap.clear_discovered();
        self.gap.inquiry_active = true;
        let cmd = hci::cmd_inquiry(duration_1_28s, max_responses);
        self.transport.send_command(&cmd)?;
        self.state = ControllerState::Scanning;
        Ok(())
    }

    /// Start BLE scanning.
    pub fn start_le_scan(&mut self, active: bool) -> Result<(), HciError> {
        if self.state != ControllerState::Ready {
            return Err(HciError::CommandDisallowed);
        }
        log::info!("BT: starting LE scan (active={})", active);
        self.gap.clear_discovered();

        // Set scan parameters
        let cmd = hci::cmd_le_set_scan_parameters(active);
        self.transport.send_command(&cmd)?;
        let _ = self.wait_command_complete(hci::HCI_OP_LE_SET_SCAN_PARAMETERS)?;

        // Enable scanning
        let cmd = hci::cmd_le_set_scan_enable(true, true);
        self.transport.send_command(&cmd)?;
        let _ = self.wait_command_complete(hci::HCI_OP_LE_SET_SCAN_ENABLE)?;

        self.gap.le_scan_active = true;
        self.state = ControllerState::Scanning;
        log::info!("BT: LE scan started");
        Ok(())
    }

    /// Stop BLE scanning.
    pub fn stop_le_scan(&mut self) -> Result<(), HciError> {
        log::info!("BT: stopping LE scan");
        let cmd = hci::cmd_le_set_scan_enable(false, false);
        self.transport.send_command(&cmd)?;
        let _ = self.wait_command_complete(hci::HCI_OP_LE_SET_SCAN_ENABLE)?;
        self.gap.le_scan_active = false;
        if !self.gap.inquiry_active {
            self.state = ControllerState::Ready;
        }
        Ok(())
    }

    /// Get current scan results.
    pub fn scan_results(&self) -> ScanResult {
        self.gap.scan_results()
    }

    /// Get the list of discovered devices.
    pub fn list_devices(&self) -> Vec<&DiscoveredDevice> {
        self.gap.discovered.values().collect()
    }

    // -----------------------------------------------------------------------
    // Connection management
    // -----------------------------------------------------------------------

    /// Connect to a BLE device.
    pub fn connect_le(&mut self, addr: &BdAddr, addr_type: u8) -> Result<(), HciError> {
        log::info!("BT: initiating LE connection to {} (type=0x{:02X})", addr, addr_type);

        // Stop scanning first if active
        if self.gap.le_scan_active {
            self.stop_le_scan()?;
        }

        let cmd = hci::cmd_le_create_connection(addr, addr_type);
        self.transport.send_command(&cmd)?;
        // Connection complete comes as an async event, handled in poll()
        Ok(())
    }

    /// Disconnect from a device by HCI handle.
    pub fn disconnect(&mut self, handle: u16) -> Result<(), HciError> {
        log::info!("BT: disconnecting handle=0x{:04X}", handle);
        let mut params = Vec::with_capacity(3);
        params.extend_from_slice(&handle.to_le_bytes());
        params.push(0x13); // Remote User Terminated Connection
        let cmd = hci::build_command(hci::HCI_OP_DISCONNECT, &params);
        self.transport.send_command(&cmd)?;
        Ok(())
    }

    /// Get a list of active connections.
    pub fn connections(&self) -> Vec<(u16, &BdAddr)> {
        self.devices
            .iter()
            .map(|(&h, d)| (h, &d.addr))
            .collect()
    }

    // -----------------------------------------------------------------------
    // HID input
    // -----------------------------------------------------------------------

    /// Get the next HID event (keyboard/mouse input), if any.
    pub fn next_hid_event(&mut self) -> Option<BtHidEvent> {
        // First drain per-device keyboard event queues
        for dev in self.devices.values_mut() {
            if let Some(kb) = &mut dev.keyboard {
                while let Some(evt) = kb.next_event() {
                    self.hid_events.push(evt);
                }
            }
        }
        if self.hid_events.is_empty() {
            None
        } else {
            Some(self.hid_events.remove(0))
        }
    }

    // -----------------------------------------------------------------------
    // Event processing (call regularly)
    // -----------------------------------------------------------------------

    /// Poll for and process incoming HCI events and ACL data.
    /// Call this regularly (e.g., from the async executor or a timer interrupt).
    pub fn poll(&mut self) -> Result<(), HciError> {
        // Process HCI events
        while let Some(raw) = self.transport.receive_event()? {
            let event = HciEvent::parse(&raw)?;
            self.handle_event(event);
        }

        // Process ACL data
        while let Some(raw) = self.transport.receive_acl_data()? {
            let acl = HciAclData::parse(&raw)?;
            self.handle_acl_data(acl);
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal event handling
    // -----------------------------------------------------------------------

    fn handle_event(&mut self, event: HciEvent) {
        // Let GAP handle discovery/connection events
        if self.gap.handle_event(&event) {
            // Check for new connections from GAP
            for (handle, conn) in &self.gap.connections {
                if !self.devices.contains_key(handle) {
                    log::info!(
                        "BT: new connection handle=0x{:04X} addr={} type={:?}",
                        handle,
                        conn.addr,
                        conn.device_type
                    );
                    let gatt = if conn.device_type == DeviceType::LowEnergy {
                        Some(GattClient::new(*handle))
                    } else {
                        None
                    };
                    self.devices.insert(
                        *handle,
                        ConnectedDevice {
                            addr: conn.addr,
                            handle: *handle,
                            device_type: conn.device_type,
                            gatt,
                            keyboard: None,
                            boot_protocol: false,
                        },
                    );
                }
            }

            // Check for disconnections
            let handles: Vec<u16> = self.devices.keys().copied().collect();
            for handle in handles {
                if !self.gap.connections.contains_key(&handle) {
                    log::info!("BT: device disconnected handle=0x{:04X}", handle);
                    self.devices.remove(&handle);
                    self.l2cap.disconnect_handle(handle);
                }
            }

            // Update controller state
            if !self.gap.inquiry_active && !self.gap.le_scan_active {
                if self.state == ControllerState::Scanning {
                    self.state = ControllerState::Ready;
                }
            }
            return;
        }

        // Handle other events
        match event.event_code {
            hci::HCI_EVT_NUM_COMPLETED_PACKETS => {
                // Flow control -- just log for now
                log::trace!("BT: num completed packets event");
            }
            _ => {
                log::trace!("BT: unhandled event 0x{:02X}", event.event_code);
            }
        }
    }

    fn handle_acl_data(&mut self, acl: HciAclData) {
        let handle = acl.handle;
        let l2cap = match L2capPacket::parse(&acl.data) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("BT: failed to parse L2CAP packet: {:?}", e);
                return;
            }
        };

        match l2cap.cid {
            L2CAP_CID_SIGNALING | l2cap::L2CAP_CID_LE_SIGNALING => {
                if let Some(response) = self.l2cap.handle_signaling(handle, &l2cap.payload) {
                    let acl_out = HciAclData {
                        handle,
                        pb_flag: 0x02,
                        bc_flag: 0x00,
                        data: response,
                    };
                    let _ = self.transport.send_acl_data(&acl_out.to_bytes());
                }
            }
            L2CAP_CID_ATT => {
                // GATT/ATT data for BLE connections
                if let Some(dev) = self.devices.get_mut(&handle) {
                    if let Some(gatt) = &mut dev.gatt {
                        if let Some(response) = gatt.handle_att_pdu(&l2cap.payload) {
                            let out = self.l2cap.wrap_acl(handle, L2CAP_CID_ATT, &response);
                            let _ = self.transport.send_acl_data(&out);
                        }

                        // Check for HID notifications
                        if !l2cap.payload.is_empty()
                            && l2cap.payload[0] == gatt::ATT_HANDLE_VALUE_NTF
                            && l2cap.payload.len() >= 3
                        {
                            let value = &l2cap.payload[3..];
                            self.process_hid_data(handle, value);
                        }
                    }
                }
            }
            cid => {
                // Check if this is a HID interrupt channel
                if let Some(ch) = self.l2cap.channels.get(&cid) {
                    if ch.psm == l2cap::PSM_HID_INTERRUPT {
                        if let Some((_report_type, report_data)) =
                            hid::parse_hid_data(&l2cap.payload)
                        {
                            self.process_hid_data(handle, report_data);
                        }
                    }
                } else {
                    log::trace!("BT: ACL data on unknown CID 0x{:04X}", cid);
                }
            }
        }
    }

    fn process_hid_data(&mut self, handle: u16, data: &[u8]) {
        if let Some(dev) = self.devices.get_mut(&handle) {
            // Try keyboard report (8 bytes)
            if data.len() >= 8 {
                if let Some(report) = BtKeyboardReport::parse(data) {
                    if dev.keyboard.is_none() {
                        log::info!(
                            "BT: keyboard detected on handle=0x{:04X} addr={}",
                            handle,
                            dev.addr
                        );
                        dev.keyboard = Some(BtKeyboardState::new());
                    }
                    if let Some(kb) = &mut dev.keyboard {
                        kb.process_report(&report);
                    }
                    return;
                }
            }

            // Try mouse report (3+ bytes)
            if data.len() >= 3 {
                if let Some(report) = BtMouseReport::parse(data) {
                    log::trace!(
                        "BT: mouse report from handle=0x{:04X}: buttons=0x{:02X} x={} y={} wheel={}",
                        handle,
                        report.buttons,
                        report.x,
                        report.y,
                        report.wheel
                    );
                    self.hid_events.push(BtHidEvent::Mouse(report));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Wait for a Command Complete event for the given opcode.
    fn wait_command_complete(&mut self, expected_opcode: u16) -> Result<HciEvent, HciError> {
        for attempt in 0..10_000 {
            if let Some(raw) = self.transport.receive_event()? {
                let event = HciEvent::parse(&raw)?;
                if let Some((_ncmds, op, status, _ret)) = event.as_command_complete() {
                    if op == expected_opcode {
                        if status != 0 {
                            log::warn!(
                                "BT: command 0x{:04X} failed with status 0x{:02X}",
                                op,
                                status
                            );
                            return Err(HciError::CommandFailed(status));
                        }
                        return Ok(event);
                    }
                }
                // Not our event -- let GAP handle it
                self.gap.handle_event(&event);
            }
            if attempt % 1000 == 999 {
                log::trace!(
                    "BT: still waiting for command complete 0x{:04X} (attempt {})",
                    expected_opcode,
                    attempt + 1
                );
            }
        }
        log::error!(
            "BT: timeout waiting for command complete 0x{:04X}",
            expected_opcode
        );
        Err(HciError::Timeout)
    }
}
