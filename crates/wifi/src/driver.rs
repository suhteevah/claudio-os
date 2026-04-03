//! High-level WiFi controller: init, scan, connect, disconnect, status, DHCP.
//!
//! [`WifiController`] is the main entry point for WiFi operations. It owns the
//! device MMIO mapping, TX/RX queues, and connection state.

use alloc::string::String;
use alloc::vec::Vec;

use crate::commands;
use crate::firmware::{self, FirmwareImage};
use crate::ieee80211;
use crate::pci::WifiDevice;
use crate::scan::{ScanConfig, ScanResults, ScannedNetwork};
use crate::tx_rx::{self, TxQueue, RxQueue, RxStatus, RXQ_SIZE};
use crate::wpa;
use crate::WifiVariant;

/// Connection state of the WiFi controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiState {
    /// Hardware detected but not initialized.
    Uninitialized,
    /// Firmware loaded, device alive, ready for commands.
    Ready,
    /// Scanning for networks.
    Scanning,
    /// Authentication in progress (Open System or SAE).
    Authenticating,
    /// Association in progress.
    Associating,
    /// WPA2 4-way handshake in progress.
    Handshaking,
    /// Connected and authenticated to an AP.
    Connected,
    /// Disconnected (was previously connected).
    Disconnected,
    /// Error state.
    Error(String),
}

/// IP configuration obtained via DHCP after connection.
#[derive(Debug, Clone)]
pub struct IpConfig {
    /// Assigned IP address.
    pub ip: [u8; 4],
    /// Subnet mask.
    pub netmask: [u8; 4],
    /// Default gateway.
    pub gateway: [u8; 4],
    /// DNS server.
    pub dns: [u8; 4],
}

/// Information about the current WiFi connection.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    /// SSID of the connected network.
    pub ssid: String,
    /// BSSID of the AP.
    pub bssid: [u8; 6],
    /// Channel.
    pub channel: u8,
    /// Signal strength in dBm.
    pub signal_dbm: i8,
    /// Current state.
    pub state: WifiState,
    /// IP configuration (if DHCP completed).
    pub ip_config: Option<IpConfig>,
}

/// The main WiFi driver controller.
///
/// Manages the full lifecycle: PCI detection, firmware load, scanning,
/// connection, WPA2 handshake, and data path.
pub struct WifiController {
    /// MMIO base address (mapped from BAR0).
    mmio_base: *mut u8,
    /// Physical memory offset for virt-to-phys conversion.
    phys_mem_offset: u64,
    /// Device variant (AX201, AX200, etc.).
    variant: WifiVariant,
    /// IRQ line.
    irq_line: u8,
    /// Our MAC address (read from NVM after firmware load).
    mac_addr: [u8; 6],
    /// Current state.
    state: WifiState,
    /// Command queue (TFD queue 0).
    cmd_queue: TxQueue,
    /// Data TX queue (TFD queue 1).
    data_queue: TxQueue,
    /// Receive queue.
    rx_queue: RxQueue,
    /// RX status ring (firmware writes here).
    rx_status: Vec<RxStatus>,
    /// Command sequence counter.
    cmd_seq: u16,
    /// Connected SSID.
    connected_ssid: String,
    /// Connected BSSID.
    connected_bssid: [u8; 6],
    /// Connected channel.
    connected_channel: u8,
    /// WPA2 PTK (after successful handshake).
    ptk: Option<wpa::Ptk>,
    /// CCMP packet number counter (for TX).
    ccmp_pn: u64,
    /// IP configuration (after DHCP).
    ip: Option<IpConfig>,
}

impl WifiController {
    /// Initialize the WiFi controller from a detected PCI device.
    ///
    /// # Safety
    ///
    /// - `phys_mem_offset` must be the correct offset for virt-to-phys conversion.
    /// - The PCI device's BAR0 must be mapped into the kernel's address space.
    pub unsafe fn init(device: &WifiDevice, phys_mem_offset: u64) -> Result<Self, &'static str> {
        log::info!(
            "wifi::driver: initializing {} at BAR0=0x{:016X} IRQ={}",
            device.variant.name(), device.bar0, device.irq_line
        );

        let mmio_base = (device.bar0 + phys_mem_offset) as *mut u8;

        // Perform a NIC reset
        Self::nic_reset(mmio_base)?;

        let cmd_queue = TxQueue::new(0);
        let data_queue = TxQueue::new(1);
        let rx_queue = RxQueue::new(phys_mem_offset);
        let rx_status = alloc::vec![RxStatus { len: 0, flags: 0 }; RXQ_SIZE];

        log::info!("wifi::driver: hardware reset complete, awaiting firmware");

        Ok(Self {
            mmio_base,
            phys_mem_offset,
            variant: device.variant,
            irq_line: device.irq_line,
            mac_addr: [0; 6],
            state: WifiState::Uninitialized,
            cmd_queue,
            data_queue,
            rx_queue,
            rx_status,
            cmd_seq: 0,
            connected_ssid: String::new(),
            connected_bssid: [0; 6],
            connected_channel: 0,
            ptk: None,
            ccmp_pn: 0,
            ip: None,
        })
    }

    /// Reset the NIC hardware.
    unsafe fn nic_reset(mmio_base: *mut u8) -> Result<(), &'static str> {
        log::info!("wifi::driver: resetting NIC...");

        // CSR_RESET register
        const CSR_RESET: u32 = 0x020;
        const CSR_RESET_SW_RESET: u32 = 1 << 7;
        const CSR_GP_CNTRL: u32 = 0x024;
        const CSR_GP_CNTRL_INIT_DONE: u32 = 1 << 2;
        const CSR_GP_CNTRL_MAC_ACCESS_REQ: u32 = 1 << 3;

        let write_reg = |offset: u32, value: u32| {
            let ptr = mmio_base.add(offset as usize) as *mut u32;
            core::ptr::write_volatile(ptr, value);
        };

        let read_reg = |offset: u32| -> u32 {
            let ptr = mmio_base.add(offset as usize) as *const u32;
            core::ptr::read_volatile(ptr)
        };

        // Trigger software reset
        write_reg(CSR_RESET, CSR_RESET_SW_RESET);

        // Wait for reset to complete
        for _ in 0..1000 {
            let val = read_reg(CSR_RESET);
            if val & CSR_RESET_SW_RESET == 0 {
                break;
            }
            for _ in 0..10_000 {
                core::hint::spin_loop();
            }
        }

        // Request MAC access
        write_reg(CSR_GP_CNTRL, CSR_GP_CNTRL_MAC_ACCESS_REQ);

        // Wait for init done
        for _ in 0..1000 {
            let val = read_reg(CSR_GP_CNTRL);
            if val & CSR_GP_CNTRL_INIT_DONE != 0 {
                log::info!("wifi::driver: NIC reset complete, MAC access granted");
                return Ok(());
            }
            for _ in 0..10_000 {
                core::hint::spin_loop();
            }
        }

        log::error!("wifi::driver: NIC reset timed out waiting for init_done");
        Err("NIC reset timed out")
    }

    /// Load firmware microcode into the device.
    ///
    /// The firmware data should be the raw contents of the `.ucode` file.
    pub fn load_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str> {
        log::info!(
            "wifi::driver: loading firmware for {} ({} bytes)",
            self.variant.name(), fw_data.len()
        );

        let fw = firmware::parse_firmware(fw_data)
            .ok_or("failed to parse firmware image")?;

        log::info!(
            "wifi::driver: firmware parsed — API v{} build {}",
            fw.api_version, fw.build
        );

        unsafe {
            firmware::upload_to_device(self.mmio_base, &fw)?;
        }

        // Wait for alive notification from firmware
        self.wait_for_alive()?;

        // Read MAC address from NVM
        self.read_mac_address()?;

        self.state = WifiState::Ready;
        log::info!(
            "wifi::driver: firmware loaded, device ready. MAC={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.mac_addr[0], self.mac_addr[1], self.mac_addr[2],
            self.mac_addr[3], self.mac_addr[4], self.mac_addr[5],
        );

        Ok(())
    }

    /// Wait for the firmware alive notification on the RX queue.
    fn wait_for_alive(&mut self) -> Result<(), &'static str> {
        log::debug!("wifi::driver: waiting for firmware alive notification...");

        for _ in 0..5000 {
            if let Some((_idx, data)) = self.rx_queue.receive(&self.rx_status) {
                if let Some(resp) = commands::CommandResponse::parse(data) {
                    if resp.id == commands::CommandId::Alive as u8 {
                        log::info!("wifi::driver: firmware alive notification received");
                        return Ok(());
                    }
                }
            }
            for _ in 0..10_000 {
                core::hint::spin_loop();
            }
        }

        log::error!("wifi::driver: timed out waiting for firmware alive");
        Err("firmware alive timeout")
    }

    /// Read the MAC address from device NVM.
    fn read_mac_address(&mut self) -> Result<(), &'static str> {
        log::debug!("wifi::driver: reading MAC address from NVM...");

        let cmd = commands::nvm_read_cmd(commands::NvmSection::Production, 0, 6);
        self.send_command(&cmd)?;

        // Poll for response
        for _ in 0..1000 {
            if let Some((_idx, data)) = self.rx_queue.receive(&self.rx_status) {
                if let Some(resp) = commands::CommandResponse::parse(data) {
                    if resp.id == commands::CommandId::NvmAccess as u8 && resp.data.len() >= 6 {
                        self.mac_addr.copy_from_slice(&resp.data[..6]);
                        return Ok(());
                    }
                }
            }
            for _ in 0..10_000 {
                core::hint::spin_loop();
            }
        }

        log::warn!("wifi::driver: failed to read MAC from NVM, using default");
        self.mac_addr = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]; // Locally-administered
        Ok(())
    }

    /// Send a host command to the firmware via the command queue.
    fn send_command(&mut self, cmd: &commands::HostCommand) -> Result<(), &'static str> {
        let seq = self.cmd_seq;
        self.cmd_seq = self.cmd_seq.wrapping_add(1);

        let serialized = cmd.serialize(seq);
        let phys_addr = (serialized.as_ptr() as u64).wrapping_sub(self.phys_mem_offset);

        match self.cmd_queue.enqueue(serialized, phys_addr) {
            Some(idx) => {
                unsafe {
                    tx_rx::poke_tx_write_ptr(self.mmio_base, 0, self.cmd_queue.write_idx);
                }
                log::trace!("wifi::driver: sent command 0x{:02X} seq={} idx={}", cmd.id as u8, seq, idx);
                Ok(())
            }
            None => {
                log::error!("wifi::driver: command queue full");
                Err("command queue full")
            }
        }
    }

    /// Scan for available WiFi networks.
    pub fn scan_networks(&mut self) -> Result<Vec<ScannedNetwork>, &'static str> {
        self.scan_networks_with_config(ScanConfig::default())
    }

    /// Scan for networks with a custom configuration.
    pub fn scan_networks_with_config(
        &mut self,
        config: ScanConfig,
    ) -> Result<Vec<ScannedNetwork>, &'static str> {
        if self.state != WifiState::Ready && self.state != WifiState::Disconnected {
            log::error!("wifi::driver: cannot scan in state {:?}", self.state);
            return Err("cannot scan in current state");
        }

        log::info!("wifi::driver: starting WiFi scan...");
        self.state = WifiState::Scanning;

        let channels = config.channels_to_scan();
        let ssid_bytes: Option<Vec<u8>> = config.ssid.as_ref().map(|s| s.as_bytes().to_vec());

        let scan_cmd = commands::scan_cmd(
            &channels,
            ssid_bytes.as_deref(),
        );
        self.send_command(&scan_cmd)?;

        let mut results = ScanResults::new();

        // Poll for scan results until timeout or scan complete notification
        let max_iterations = config.timeout_ms as usize * 10; // ~100us per iteration
        for _ in 0..max_iterations {
            if let Some((idx, data)) = self.rx_queue.receive(&self.rx_status) {
                if let Some(resp) = commands::CommandResponse::parse(data) {
                    if resp.id == commands::CommandId::ScanComplete as u8 {
                        log::info!("wifi::driver: scan complete notification received");
                        self.rx_queue.recycle(idx);
                        break;
                    }
                }

                // Try to parse as a beacon/probe response
                if data.len() > 30 {
                    if let Some(beacon) = ieee80211::parse_beacon(data) {
                        // Signal strength would come from firmware metadata
                        // (prepended to the frame). For now, estimate from RX flags.
                        let signal_dbm = -50i8; // TODO: extract from firmware RX metadata
                        results.process_beacon(&beacon, signal_dbm, config.min_signal_dbm);
                    }
                }

                self.rx_queue.recycle(idx);
            }

            for _ in 0..100 {
                core::hint::spin_loop();
            }
        }

        self.state = WifiState::Ready;
        Ok(results.finalize())
    }

    /// Connect to a WiFi network with WPA2-PSK.
    ///
    /// Performs the full connection sequence:
    /// 1. PHY configuration (band/channel)
    /// 2. Authentication (Open System)
    /// 3. Association
    /// 4. WPA2 4-way handshake
    pub fn connect(&mut self, ssid: &str, password: &str) -> Result<(), &'static str> {
        log::info!("wifi::driver: connecting to \"{}\"...", ssid);

        if self.state != WifiState::Ready && self.state != WifiState::Disconnected {
            log::error!("wifi::driver: cannot connect in state {:?}", self.state);
            return Err("cannot connect in current state");
        }

        // Step 1: Find the target network
        log::info!("wifi::driver: scanning for target network...");
        let mut config = ScanConfig::default();
        config.ssid = Some(String::from(ssid));
        config.timeout_ms = 3000;
        let networks = self.scan_networks_with_config(config)?;

        let target = networks.iter().find(|n| n.ssid == ssid)
            .ok_or("target network not found")?;

        if !target.wpa2 {
            log::warn!("wifi::driver: target network does not advertise WPA2");
        }

        let bssid = target.bssid;
        let channel = target.channel;

        log::info!(
            "wifi::driver: found \"{}\" on ch{} signal={}dBm BSSID={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            ssid, channel, target.signal_dbm,
            bssid[0], bssid[1], bssid[2], bssid[3], bssid[4], bssid[5]
        );

        // Step 2: Configure PHY
        let band = if channel <= 14 {
            commands::Band::Band2G
        } else {
            commands::Band::Band5G
        };
        let phy_cmd = commands::phy_context_cmd(band, channel, commands::ChannelWidth::Mhz20);
        self.send_command(&phy_cmd)?;

        // Step 3: Authentication
        self.state = WifiState::Authenticating;
        log::info!("wifi::driver: sending authentication frame (Open System)...");
        let auth_frame = ieee80211::build_auth_frame(&bssid, &self.mac_addr);
        self.transmit_mgmt_frame(&auth_frame)?;

        // Wait for auth response
        self.wait_for_auth_response()?;

        // Step 4: Association
        self.state = WifiState::Associating;
        log::info!("wifi::driver: sending association request...");
        let assoc_frame = ieee80211::build_assoc_request(&bssid, &self.mac_addr, ssid);
        self.transmit_mgmt_frame(&assoc_frame)?;

        // Add station in firmware
        let add_sta = commands::add_station_cmd(&bssid, 0);
        self.send_command(&add_sta)?;

        // Wait for association response
        self.wait_for_assoc_response()?;

        // Step 5: WPA2 4-way handshake
        self.state = WifiState::Handshaking;
        log::info!("wifi::driver: deriving PSK...");
        let psk = wpa::derive_psk(password, ssid);

        log::info!("wifi::driver: starting WPA2 4-way handshake...");
        self.do_4way_handshake(&psk, &bssid)?;

        // Step 6: Install keys in firmware
        if let Some(ref ptk) = self.ptk {
            let key_cmd = commands::add_key_cmd(
                0,
                0,
                commands::KeyAlgorithm::Ccmp,
                &ptk.tk,
            );
            self.send_command(&key_cmd)?;
        }

        // Connection complete
        self.state = WifiState::Connected;
        self.connected_ssid = String::from(ssid);
        self.connected_bssid = bssid;
        self.connected_channel = channel;

        log::info!("wifi::driver: connected to \"{}\"!", ssid);
        Ok(())
    }

    /// Transmit a management frame.
    fn transmit_mgmt_frame(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        let buf = frame.to_vec();
        let phys_addr = (buf.as_ptr() as u64).wrapping_sub(self.phys_mem_offset);
        match self.data_queue.enqueue(buf, phys_addr) {
            Some(_) => {
                unsafe {
                    tx_rx::poke_tx_write_ptr(self.mmio_base, 1, self.data_queue.write_idx);
                }
                Ok(())
            }
            None => Err("data queue full"),
        }
    }

    /// Wait for an authentication response frame.
    fn wait_for_auth_response(&mut self) -> Result<(), &'static str> {
        log::debug!("wifi::driver: waiting for auth response...");
        for _ in 0..50_000 {
            if let Some((idx, data)) = self.rx_queue.receive(&self.rx_status) {
                if let Some(hdr) = ieee80211::MacHeader::parse(data) {
                    if hdr.frame_control.frame_type() == ieee80211::FrameType::Management
                        && hdr.frame_control.subtype() == ieee80211::MgmtSubtype::Auth as u8
                    {
                        log::info!("wifi::driver: authentication response received");
                        self.rx_queue.recycle(idx);
                        return Ok(());
                    }
                }
                self.rx_queue.recycle(idx);
            }
            for _ in 0..100 {
                core::hint::spin_loop();
            }
        }
        Err("auth response timeout")
    }

    /// Wait for an association response frame.
    fn wait_for_assoc_response(&mut self) -> Result<(), &'static str> {
        log::debug!("wifi::driver: waiting for association response...");
        for _ in 0..50_000 {
            if let Some((idx, data)) = self.rx_queue.receive(&self.rx_status) {
                if let Some(hdr) = ieee80211::MacHeader::parse(data) {
                    if hdr.frame_control.frame_type() == ieee80211::FrameType::Management
                        && hdr.frame_control.subtype() == ieee80211::MgmtSubtype::AssocResponse as u8
                    {
                        log::info!("wifi::driver: association response received");
                        self.rx_queue.recycle(idx);
                        return Ok(());
                    }
                }
                self.rx_queue.recycle(idx);
            }
            for _ in 0..100 {
                core::hint::spin_loop();
            }
        }
        Err("association response timeout")
    }

    /// Perform the WPA2 4-way handshake.
    fn do_4way_handshake(
        &mut self,
        pmk: &[u8; 32],
        bssid: &[u8; 6],
    ) -> Result<(), &'static str> {
        // Generate our SNonce (in production, use a proper RNG)
        let mut snonce = [0u8; 32];
        // Simple PRNG seeded from MAC address + a counter (NOT cryptographically secure —
        // real hardware would use RDRAND or a hardware RNG)
        for i in 0..32 {
            snonce[i] = self.mac_addr[i % 6].wrapping_add(i as u8).wrapping_mul(0x6D);
        }

        // Wait for Message 1 (ANonce from AP)
        log::debug!("wifi::driver: waiting for EAPOL message 1...");
        let msg1 = self.wait_for_eapol(1)?;
        let anonce = msg1.nonce;
        log::info!("wifi::driver: received EAPOL message 1 (ANonce)");

        // Derive PTK
        let ptk = wpa::derive_ptk(pmk, bssid, &self.mac_addr, &anonce, &snonce);

        // Build and send Message 2
        let rsn_ie = self.build_rsn_ie();
        let msg2 = wpa::build_eapol_msg2(&snonce, msg1.replay_counter, &ptk.kck, &rsn_ie);
        self.transmit_mgmt_frame(&msg2)?;
        log::info!("wifi::driver: sent EAPOL message 2 (SNonce + MIC)");

        // Wait for Message 3 (GTK from AP)
        log::debug!("wifi::driver: waiting for EAPOL message 3...");
        let msg3 = self.wait_for_eapol(3)?;
        log::info!("wifi::driver: received EAPOL message 3 (GTK + Install)");

        // Send Message 4 (ACK)
        let msg4 = wpa::build_eapol_msg4(msg3.replay_counter, &ptk.kck);
        self.transmit_mgmt_frame(&msg4)?;
        log::info!("wifi::driver: sent EAPOL message 4 (ACK) — handshake complete");

        self.ptk = Some(ptk);
        Ok(())
    }

    /// Wait for a specific EAPOL-Key message.
    fn wait_for_eapol(&mut self, expected_msg: u8) -> Result<wpa::EapolKeyFrame, &'static str> {
        for _ in 0..100_000 {
            if let Some((idx, data)) = self.rx_queue.receive(&self.rx_status) {
                // EAPOL frames arrive as data frames — look for ethertype 0x888E
                if data.len() > 40 {
                    if let Some(eapol) = wpa::EapolKeyFrame::parse(&data[data.len().saturating_sub(data.len())..]) {
                        if eapol.message_number() == expected_msg {
                            self.rx_queue.recycle(idx);
                            return Ok(eapol);
                        }
                    }
                }
                self.rx_queue.recycle(idx);
            }
            for _ in 0..100 {
                core::hint::spin_loop();
            }
        }
        Err("EAPOL message timeout")
    }

    /// Build RSN IE data for association/handshake.
    fn build_rsn_ie(&self) -> Vec<u8> {
        let mut ie = Vec::with_capacity(22);
        ie.push(ieee80211::ie_id::RSN);
        // RSN IE body
        ie.extend_from_slice(&1u16.to_le_bytes()); // version
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]); // group cipher: CCMP
        ie.extend_from_slice(&1u16.to_le_bytes()); // pairwise count
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]); // pairwise: CCMP
        ie.extend_from_slice(&1u16.to_le_bytes()); // AKM count
        ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x02]); // AKM: PSK
        ie.extend_from_slice(&0u16.to_le_bytes()); // capabilities
        ie
    }

    /// Disconnect from the current network.
    pub fn disconnect(&mut self) -> Result<(), &'static str> {
        if self.state != WifiState::Connected {
            log::warn!("wifi::driver: disconnect called but not connected");
            return Ok(());
        }

        log::info!("wifi::driver: disconnecting from \"{}\"...", self.connected_ssid);

        // Send disassociation frame
        let mut frame = Vec::with_capacity(26);
        let fc: u16 = (ieee80211::FrameType::Management as u16) << 2
            | (ieee80211::MgmtSubtype::Disassoc as u16) << 4;
        frame.extend_from_slice(&fc.to_le_bytes());
        frame.extend_from_slice(&0u16.to_le_bytes()); // duration
        frame.extend_from_slice(&self.connected_bssid);
        frame.extend_from_slice(&self.mac_addr);
        frame.extend_from_slice(&self.connected_bssid);
        frame.extend_from_slice(&0u16.to_le_bytes()); // seq ctrl
        frame.extend_from_slice(&3u16.to_le_bytes()); // reason: deauth leaving

        self.transmit_mgmt_frame(&frame)?;

        // Remove station from firmware
        let rm_sta = commands::HostCommand::new(commands::CommandId::RemoveStation);
        self.send_command(&rm_sta)?;

        self.state = WifiState::Disconnected;
        self.ptk = None;
        self.ccmp_pn = 0;
        self.ip = None;

        log::info!("wifi::driver: disconnected");
        Ok(())
    }

    /// Get the current connection status.
    pub fn status(&self) -> ConnectionInfo {
        ConnectionInfo {
            ssid: self.connected_ssid.clone(),
            bssid: self.connected_bssid,
            channel: self.connected_channel,
            signal_dbm: -50, // TODO: query from firmware
            state: self.state.clone(),
            ip_config: self.ip.clone(),
        }
    }

    /// Perform DHCP to obtain an IP address.
    ///
    /// This sends DHCP discover/request frames over the WiFi link and waits
    /// for an IP configuration. In practice, the kernel would integrate this
    /// with smoltcp's DHCP client after wiring WiFi as a NIC backend.
    pub fn ip_config(&mut self) -> Result<IpConfig, &'static str> {
        if self.state != WifiState::Connected {
            return Err("not connected");
        }

        if let Some(ref ip) = self.ip {
            return Ok(ip.clone());
        }

        log::info!("wifi::driver: initiating DHCP...");

        // In the real driver, DHCP frames would be sent/received through the
        // data queue. For now, this is a placeholder that documents the
        // integration point with smoltcp.
        //
        // The actual DHCP flow will use smoltcp's DhcpSocket, with this
        // WifiController providing the NIC transmit/receive interface:
        //
        //   smoltcp::iface::Interface
        //       -> WifiController::transmit(frame)  [wraps in 802.11 + CCMP]
        //       <- WifiController::receive()         [unwraps 802.11 + CCMP]
        //
        // For the initial integration, the kernel will:
        // 1. wifi.connect("SSID", "pass")
        // 2. Create a smoltcp Interface backed by the WiFi NIC
        // 3. Run DHCP through smoltcp as it does for VirtIO-net

        log::warn!("wifi::driver: DHCP not yet implemented over WiFi data path");
        log::warn!("wifi::driver: integration point: wire WifiController as smoltcp NIC backend");

        Err("DHCP over WiFi not yet implemented — wire into smoltcp")
    }

    /// Get the device variant.
    pub fn variant(&self) -> WifiVariant {
        self.variant
    }

    /// Get the MAC address.
    pub fn mac_address(&self) -> [u8; 6] {
        self.mac_addr
    }

    /// Get the IRQ line.
    pub fn irq_line(&self) -> u8 {
        self.irq_line
    }

    /// Transmit a data frame with CCMP encryption.
    ///
    /// This is the data path entry point. The caller provides an Ethernet-like
    /// payload (dst MAC + src MAC + ethertype + data), and this function wraps
    /// it in an 802.11 QoS data frame with AES-CCMP encryption.
    pub fn transmit(&mut self, payload: &[u8]) -> Result<(), &'static str> {
        if self.state != WifiState::Connected {
            return Err("not connected");
        }

        let ptk = self.ptk.as_ref().ok_or("no PTK — handshake incomplete")?;

        // Extract destination from payload (first 6 bytes)
        if payload.len() < 14 {
            return Err("payload too short");
        }
        let mut dst = [0u8; 6];
        dst.copy_from_slice(&payload[..6]);

        // Build 802.11 QoS data header
        let header = ieee80211::build_qos_data_header(
            &self.connected_bssid,
            &self.mac_addr,
            &dst,
            (self.ccmp_pn & 0xFFF) as u16,
            0, // TID 0 (best effort)
        );

        // Construct AAD from header (for CCMP authentication)
        let aad = &header[..header.len().min(22)]; // Frame control + addresses

        // CCMP nonce
        let nonce = wpa::CcmpNonce {
            priority: 0,
            addr: self.mac_addr,
            pn: self.ccmp_pn,
        };

        // Encrypt payload (skip the Ethernet header, encrypt from ethertype onward)
        let encrypted = wpa::ccmp_encrypt(&ptk.tk, &nonce, aad, &payload[12..]);
        self.ccmp_pn += 1;

        // Build CCMP header (8 bytes)
        let mut ccmp_hdr = [0u8; 8];
        ccmp_hdr[0] = (self.ccmp_pn & 0xFF) as u8;
        ccmp_hdr[1] = ((self.ccmp_pn >> 8) & 0xFF) as u8;
        ccmp_hdr[2] = 0; // reserved
        ccmp_hdr[3] = 0x20; // ExtIV bit set, key ID 0
        ccmp_hdr[4] = ((self.ccmp_pn >> 16) & 0xFF) as u8;
        ccmp_hdr[5] = ((self.ccmp_pn >> 24) & 0xFF) as u8;
        ccmp_hdr[6] = ((self.ccmp_pn >> 32) & 0xFF) as u8;
        ccmp_hdr[7] = ((self.ccmp_pn >> 40) & 0xFF) as u8;

        // Final frame: header + CCMP header + encrypted data
        let mut frame = Vec::with_capacity(header.len() + 8 + encrypted.len());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(&ccmp_hdr);
        frame.extend_from_slice(&encrypted);

        self.transmit_mgmt_frame(&frame)?;

        log::trace!("wifi::driver: transmitted {} byte encrypted frame", frame.len());
        Ok(())
    }
}
