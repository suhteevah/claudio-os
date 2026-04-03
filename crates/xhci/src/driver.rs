//! High-level xHCI Host Controller Driver
//!
//! `XhciController` is the top-level API. It wraps register access, ring
//! management, device enumeration, and HID keyboard integration into a single
//! struct that the kernel can init once and then poll for keyboard events.

use alloc::vec::Vec;

use crate::XhciError;
use crate::context::{
    Dcbaa, EndpointContext, InputContext, SlotContext,
    EP_TYPE_CONTROL, EP_TYPE_INTERRUPT_IN,
};
use crate::device::{
    alloc_dma_buffer, read_dma_buffer, DeviceDescriptor, EndpointDescriptor,
    ParsedConfiguration, UsbDevice, UsbSpeed,
    USB_DESC_CONFIGURATION, USB_DESC_DEVICE,
    USB_DIR_IN, USB_DIR_OUT, USB_RECIP_DEVICE, USB_RECIP_INTERFACE,
    USB_REQ_GET_DESCRIPTOR, USB_REQ_SET_CONFIGURATION,
    USB_TYPE_CLASS, USB_TYPE_STANDARD,
};
use crate::hid::{
    BootKeyboardReport, KeyEvent, KeyboardState,
    HID_PROTOCOL_BOOT, HID_REQ_SET_IDLE, HID_REQ_SET_PROTOCOL,
};
use crate::registers::*;
use crate::ring::*;

/// Maximum number of command completion retries before giving up.
const MAX_EVENT_POLL_RETRIES: u32 = 100_000;

/// The xHCI Host Controller driver.
pub struct XhciController {
    /// PCI BAR0 base address (MMIO)
    bar0: usize,
    /// Capability registers
    cap: CapabilityRegs,
    /// Operational registers
    op: OperationalRegs,
    /// Runtime registers
    rt: RuntimeRegs,
    /// Doorbell registers
    db: DoorbellRegs,
    /// Context size (32 or 64 bytes)
    ctx_size: usize,
    /// Maximum device slots
    max_slots: u8,
    /// Maximum ports
    max_ports: u8,
    /// Command ring
    cmd_ring: CommandRing,
    /// Event ring (interrupter 0)
    evt_ring: EventRing,
    /// Device Context Base Address Array
    dcbaa: Dcbaa,
    /// Tracked USB devices (indexed by slot ID, slot 0 unused)
    devices: Vec<Option<UsbDevice>>,
    /// Transfer rings per slot/endpoint (slot_id -> dci -> ring)
    transfer_rings: Vec<Vec<Option<TransferRing>>>,
    /// HID keyboard state (if a keyboard is found)
    keyboard: Option<KeyboardInfo>,
}

/// Keyboard-specific state bundled together.
struct KeyboardInfo {
    /// Slot ID of the keyboard device
    slot_id: u8,
    /// Device Context Index of the interrupt IN endpoint
    dci: u8,
    /// DMA buffer for interrupt reports
    report_buf_va: *mut u8,
    report_buf_phys: u64,
    /// Keyboard state tracker for event generation
    state: KeyboardState,
    /// Whether we have an outstanding interrupt transfer
    transfer_pending: bool,
}

impl XhciController {
    /// Initialize the xHCI controller from a PCI BAR0 address.
    ///
    /// This performs the full initialization sequence per xHCI spec 4.2:
    /// 1. Read capability registers
    /// 2. Wait for CNR = 0 (Controller Not Ready)
    /// 3. Reset the controller (HCRST)
    /// 4. Program MaxSlotsEn
    /// 5. Allocate and program DCBAA
    /// 6. Allocate and program Command Ring (CRCR)
    /// 7. Allocate and program Event Ring (ERSTBA/ERSTSZ/ERDP)
    /// 8. Enable interrupts, set Run/Stop = 1
    ///
    /// # Safety
    /// `pci_bar0` must be a valid, identity-mapped MMIO address for an xHCI controller.
    pub unsafe fn init(pci_bar0: usize) -> Result<Self, XhciError> {
        log::info!("xhci: initializing controller at BAR0={:#x}", pci_bar0);

        // --- Step 1: Read capability registers ---
        let cap = CapabilityRegs::new(pci_bar0);
        let caplength = cap.caplength();
        let hciversion = cap.hciversion();
        let max_slots = cap.max_slots();
        let max_ports = cap.max_ports();
        let max_intrs = cap.max_intrs();
        let ctx_size = cap.context_size();
        let scratchpad = cap.max_scratchpad_buffers();

        log::info!(
            "xhci: CAPLENGTH={:#x} HCIVERSION={:#x} MaxSlots={} MaxPorts={} MaxIntrs={} CtxSize={} Scratchpad={}",
            caplength, hciversion, max_slots, max_ports, max_intrs, ctx_size, scratchpad
        );

        let op = OperationalRegs::new(cap.operational_base());
        let rt = RuntimeRegs::new(cap.runtime_base());
        let db = DoorbellRegs::new(cap.doorbell_base());

        log::debug!(
            "xhci: operational base={:#x} runtime base={:#x} doorbell base={:#x}",
            cap.operational_base(),
            cap.runtime_base(),
            cap.doorbell_base(),
        );

        // --- Step 2: Wait for Controller Not Ready = 0 ---
        log::debug!("xhci: waiting for controller ready (CNR=0)...");
        let mut timeout = 1_000_000u32;
        while !op.is_ready() {
            timeout -= 1;
            if timeout == 0 {
                log::error!("xhci: controller not ready after timeout (USBSTS={:#x})", op.usbsts());
                return Err(XhciError::Timeout("controller not ready (CNR did not clear)"));
            }
        }
        log::info!("xhci: controller ready");

        // --- Step 3: Reset the controller ---
        log::debug!("xhci: issuing HCRST...");
        // First, ensure the controller is halted
        if !op.is_halted() {
            log::debug!("xhci: stopping controller (clearing RS)...");
            op.set_usbcmd(op.usbcmd() & !USBCMD_RS);

            timeout = 1_000_000;
            while !op.is_halted() {
                timeout -= 1;
                if timeout == 0 {
                    log::error!("xhci: controller did not halt (USBSTS={:#x})", op.usbsts());
                    return Err(XhciError::Timeout("controller did not halt"));
                }
            }
            log::debug!("xhci: controller halted");
        }

        op.set_usbcmd(op.usbcmd() | USBCMD_HCRST);

        // Wait for HCRST to clear
        timeout = 1_000_000;
        while op.usbcmd() & USBCMD_HCRST != 0 {
            timeout -= 1;
            if timeout == 0 {
                log::error!("xhci: HCRST did not clear");
                return Err(XhciError::Timeout("HCRST did not clear"));
            }
        }

        // Wait for CNR to clear again after reset
        timeout = 1_000_000;
        while !op.is_ready() {
            timeout -= 1;
            if timeout == 0 {
                log::error!("xhci: controller not ready after reset");
                return Err(XhciError::Timeout("controller not ready after reset"));
            }
        }
        log::info!("xhci: controller reset complete");

        // --- Step 4: Program MaxSlotsEn ---
        op.set_config(max_slots as u32);
        log::debug!("xhci: CONFIG.MaxSlotsEn = {}", max_slots);

        // --- Step 5: Allocate DCBAA ---
        let dcbaa = match Dcbaa::new(max_slots, ctx_size, scratchpad) {
            Some(d) => d,
            None => return Err(XhciError::AllocFailed("DCBAA")),
        };
        op.set_dcbaap(dcbaa.phys_addr());
        log::debug!("xhci: DCBAAP = {:#x}", dcbaa.phys_addr());

        // --- Step 6: Allocate Command Ring ---
        let cmd_ring = match CommandRing::new() {
            Some(r) => r,
            None => return Err(XhciError::AllocFailed("command ring")),
        };
        op.set_crcr(cmd_ring.phys_addr_with_cycle());
        log::debug!("xhci: CRCR = {:#x}", cmd_ring.phys_addr_with_cycle());

        // --- Step 7: Allocate Event Ring for interrupter 0 ---
        let evt_ring = match EventRing::new() {
            Some(r) => r,
            None => return Err(XhciError::AllocFailed("event ring")),
        };

        // Program ERSTSZ, ERDP, then ERSTBA (order matters per spec 5.5.2.3.2)
        rt.set_erstsz(0, evt_ring.erst_size());
        rt.set_erdp(0, evt_ring.dequeue_phys());
        rt.set_erstba(0, evt_ring.erst_phys());

        log::debug!(
            "xhci: interrupter 0: ERSTSZ={} ERDP={:#x} ERSTBA={:#x}",
            evt_ring.erst_size(),
            evt_ring.dequeue_phys(),
            evt_ring.erst_phys(),
        );

        // Set IMOD for interrupter 0 (4000 = ~1ms at 250ns intervals)
        rt.set_imod(0, 4000);

        // Enable interrupter 0
        rt.set_iman(0, IMAN_IP | IMAN_IE);
        log::debug!("xhci: interrupter 0 enabled");

        // --- Step 8: Set Run/Stop = 1, Interrupter Enable ---
        let usbcmd = op.usbcmd() | USBCMD_RS | USBCMD_INTE;
        op.set_usbcmd(usbcmd);
        log::info!("xhci: controller started (USBCMD={:#x})", op.usbcmd());

        // Verify the controller is running
        if op.is_halted() {
            log::error!(
                "xhci: controller still halted after setting RS (USBSTS={:#x})",
                op.usbsts()
            );
            return Err(XhciError::ControllerError("controller still halted after setting RS"));
        }

        // Initialize device tracking arrays
        let mut devices = Vec::with_capacity(max_slots as usize + 1);
        let mut transfer_rings = Vec::with_capacity(max_slots as usize + 1);
        for _ in 0..=max_slots {
            devices.push(None);
            transfer_rings.push(Vec::new());
        }

        log::info!("xhci: initialization complete, {} ports available", max_ports);

        Ok(Self {
            bar0: pci_bar0,
            cap,
            op,
            rt,
            db,
            ctx_size,
            max_slots,
            max_ports,
            cmd_ring,
            evt_ring,
            dcbaa,
            devices,
            transfer_rings,
            keyboard: None,
        })
    }

    // -----------------------------------------------------------------------
    // Port enumeration
    // -----------------------------------------------------------------------

    /// Scan all ports for connected devices and enumerate them.
    pub fn enumerate_ports(&mut self) {
        log::info!("xhci: scanning {} ports for connected devices...", self.max_ports);

        for port in 1..=self.max_ports {
            let portsc = self.op.portsc(port);
            let connected = portsc & PORTSC_CCS != 0;
            let enabled = portsc & PORTSC_PED != 0;
            let speed = (portsc & PORTSC_SPEED_MASK) >> PORTSC_SPEED_SHIFT;

            log::debug!(
                "xhci: port {}: PORTSC={:#010x} connected={} enabled={} speed={}",
                port, portsc, connected, enabled, speed
            );

            if connected {
                let usb_speed = UsbSpeed::from_port_speed(speed);
                log::info!(
                    "xhci: port {}: device connected, speed={:?}",
                    port, usb_speed
                );

                // If not yet enabled, issue a port reset
                if !enabled {
                    self.reset_port(port);
                }

                // Try to enumerate the device
                if let Some(slot_id) = self.enable_slot() {
                    self.initialize_device(slot_id, port, usb_speed);
                }
            }
        }
    }

    /// Reset a port to enable it.
    fn reset_port(&mut self, port: u8) {
        log::debug!("xhci: resetting port {}...", port);

        let portsc = self.op.portsc(port);
        // Preserve power, set reset, clear status change bits
        let val = (portsc & !(PORTSC_CHANGE_BITS | PORTSC_PED)) | PORTSC_PR;
        self.op.set_portsc(port, val);

        // Wait for reset to complete (PRC set)
        let mut timeout = 500_000u32;
        loop {
            let portsc = self.op.portsc(port);
            if portsc & PORTSC_PRC != 0 {
                // Clear PRC
                self.op.set_portsc(
                    port,
                    (portsc & !PORTSC_CHANGE_BITS) | PORTSC_PRC,
                );
                break;
            }
            timeout -= 1;
            if timeout == 0 {
                log::warn!("xhci: port {} reset timeout", port);
                return;
            }
        }

        let portsc = self.op.portsc(port);
        let enabled = portsc & PORTSC_PED != 0;
        let speed = (portsc & PORTSC_SPEED_MASK) >> PORTSC_SPEED_SHIFT;
        log::info!(
            "xhci: port {} reset complete: enabled={} speed={}",
            port, enabled, speed
        );
    }

    // -----------------------------------------------------------------------
    // Command ring helpers
    // -----------------------------------------------------------------------

    /// Send a command TRB and wait for the completion event.
    /// Returns the completion event TRB, or None on timeout.
    fn send_command(&mut self, trb: Trb) -> Option<Trb> {
        let _phys = self.cmd_ring.enqueue(trb);
        self.db.ring_command();

        // Poll event ring for completion
        for _ in 0..MAX_EVENT_POLL_RETRIES {
            if let Some(evt) = self.evt_ring.dequeue() {
                // Update ERDP
                unsafe {
                    self.rt.set_erdp(0, self.evt_ring.dequeue_phys() | (1 << 3));
                }

                let evt_type = evt.trb_type();

                if evt_type == TRB_TYPE_COMMAND_COMPLETION {
                    let code = evt.completion_code();
                    let slot = evt.slot_id();
                    log::debug!(
                        "xhci: command completion: code={} slot={}",
                        code, slot
                    );
                    return Some(evt);
                } else if evt_type == TRB_TYPE_PORT_STATUS_CHANGE {
                    let port_id = (evt.parameter() >> 24) as u8;
                    log::info!("xhci: port status change event: port={}", port_id);
                    // Could handle hot-plug here; for now continue polling
                    continue;
                } else {
                    log::trace!("xhci: unexpected event type {} while waiting for command completion", evt_type);
                    continue;
                }
            }

            // Tiny busy-wait between polls
            core::hint::spin_loop();
        }

        log::warn!("xhci: command completion timeout after {} retries", MAX_EVENT_POLL_RETRIES);
        None
    }

    // -----------------------------------------------------------------------
    // Device slot management
    // -----------------------------------------------------------------------

    /// Issue an Enable Slot command. Returns the allocated slot ID on success.
    fn enable_slot(&mut self) -> Option<u8> {
        log::debug!("xhci: sending Enable Slot command");

        let trb = Trb::enable_slot(false);
        let evt = self.send_command(trb)?;

        if evt.completion_code() != TRB_COMPLETION_SUCCESS {
            log::warn!(
                "xhci: Enable Slot failed: completion code={}",
                evt.completion_code()
            );
            return None;
        }

        let slot_id = evt.slot_id();
        log::info!("xhci: slot {} enabled", slot_id);

        // Allocate device context in DCBAA
        if unsafe { self.dcbaa.alloc_device_context(slot_id, self.ctx_size) }.is_none() {
            log::error!("xhci: failed to allocate device context for slot {}", slot_id);
            return None;
        }

        // Initialize transfer ring storage for this slot (32 possible DCIs: 0..31)
        if self.transfer_rings.len() <= slot_id as usize {
            self.transfer_rings.resize_with(slot_id as usize + 1, Vec::new);
        }
        self.transfer_rings[slot_id as usize] = Vec::new();
        for _ in 0..32 {
            self.transfer_rings[slot_id as usize].push(None);
        }

        Some(slot_id)
    }

    /// Initialize a device: Address Device, Get Descriptors, Configure.
    fn initialize_device(&mut self, slot_id: u8, port: u8, speed: UsbSpeed) {
        log::info!(
            "xhci: initializing device slot={} port={} speed={:?}",
            slot_id, port, speed
        );

        let device = UsbDevice::new(slot_id, port, speed);
        self.devices[slot_id as usize] = Some(device);

        // --- Address Device (BSR=0: set address immediately) ---
        if !self.address_device(slot_id, port, speed) {
            log::warn!("xhci: Address Device failed for slot {}", slot_id);
            return;
        }

        // --- GET_DESCRIPTOR(Device) ---
        let dev_desc = match self.get_device_descriptor(slot_id) {
            Some(d) => d,
            None => {
                log::warn!("xhci: failed to get device descriptor for slot {}", slot_id);
                return;
            }
        };

        // Update EP0 max packet size if needed
        let actual_mps = dev_desc.b_max_packet_size0;
        if actual_mps != speed.default_max_packet_size0() as u8 {
            log::debug!(
                "xhci: slot {} EP0 max packet size: default={} actual={}",
                slot_id,
                speed.default_max_packet_size0(),
                actual_mps
            );
            // Would issue Evaluate Context to update EP0 here
        }

        if let Some(ref mut dev) = self.devices[slot_id as usize] {
            dev.device_desc = Some(dev_desc.clone());
        }

        // --- GET_DESCRIPTOR(Configuration, index=0) ---
        let parsed_config = match self.get_configuration_descriptor(slot_id, 0) {
            Some(c) => c,
            None => {
                log::warn!("xhci: failed to get config descriptor for slot {}", slot_id);
                return;
            }
        };

        // Check if this is a keyboard (clone the result so parsed_config can be moved later)
        let keyboard_info = parsed_config.find_hid_keyboard()
            .map(|(iface_num, ep_desc)| (iface_num, ep_desc.clone()));

        // --- SET_CONFIGURATION ---
        let config_val = parsed_config.config.b_configuration_value;
        if !self.set_configuration(slot_id, config_val, &parsed_config) {
            log::warn!("xhci: SET_CONFIGURATION failed for slot {}", slot_id);
            return;
        }

        if let Some(ref mut dev) = self.devices[slot_id as usize] {
            dev.config = Some(parsed_config);
            dev.configured = true;
        }

        // --- Setup keyboard if found ---
        if let Some((iface_num, ep_desc)) = keyboard_info {
            log::info!(
                "xhci: setting up HID keyboard on slot={} interface={}",
                slot_id, iface_num
            );
            self.setup_keyboard(slot_id, iface_num, &ep_desc);
        }
    }

    /// Issue an Address Device command.
    fn address_device(&mut self, slot_id: u8, port: u8, speed: UsbSpeed) -> bool {
        log::debug!("xhci: Address Device slot={} port={}", slot_id, port);

        let ctx_size = self.ctx_size;

        // Allocate EP0 transfer ring
        let ep0_ring = match unsafe { TransferRing::new() } {
            Some(r) => r,
            None => {
                log::error!("xhci: failed to allocate EP0 transfer ring for slot {}", slot_id);
                return false;
            }
        };
        let ep0_ring_phys = ep0_ring.phys_addr_with_dcs();
        self.transfer_rings[slot_id as usize][1] = Some(ep0_ring);

        // Build Input Context
        let input_ctx = match unsafe { InputContext::new(ctx_size) } {
            Some(c) => c,
            None => {
                log::error!("xhci: failed to allocate input context for slot {}", slot_id);
                return false;
            }
        };

        // Add flags: Slot Context (bit 0) + EP0 Context (bit 1)
        input_ctx.set_add_flags(0x3);

        // Slot Context
        let mut slot = SlotContext::new(ctx_size);
        slot.set_route_string(0)
            .set_speed(speed.to_slot_speed())
            .set_context_entries(1) // Only EP0
            .set_root_hub_port(port);
        input_ctx.write_slot_context(&slot);

        // EP0 Context
        let mut ep0 = EndpointContext::new(ctx_size);
        let max_pkt = speed.default_max_packet_size0();
        ep0.set_ep_type(EP_TYPE_CONTROL)
            .set_max_packet_size(max_pkt)
            .set_cerr(3)
            .set_tr_dequeue_pointer(ep0_ring_phys)
            .set_average_trb_length(8); // Control transfers average ~8 bytes
        input_ctx.write_endpoint_context(1, &ep0);

        log::debug!(
            "xhci: Address Device input ctx at {:#x}: speed={} max_pkt={} ep0_ring={:#x}",
            input_ctx.phys_addr(), speed.to_slot_speed(), max_pkt, ep0_ring_phys,
        );

        // Send command
        let trb = Trb::address_device(input_ctx.phys_addr(), slot_id, false, false);
        match self.send_command(trb) {
            Some(evt) if evt.completion_code() == TRB_COMPLETION_SUCCESS => {
                log::info!("xhci: slot {} addressed successfully", slot_id);
                true
            }
            Some(evt) => {
                log::warn!(
                    "xhci: Address Device failed for slot {}: code={}",
                    slot_id, evt.completion_code()
                );
                false
            }
            None => {
                log::warn!("xhci: Address Device timeout for slot {}", slot_id);
                false
            }
        }
    }

    /// Issue GET_DESCRIPTOR(Device) on EP0.
    fn get_device_descriptor(&mut self, slot_id: u8) -> Option<DeviceDescriptor> {
        log::debug!("xhci: GET_DESCRIPTOR(Device) slot={}", slot_id);

        let buf_size = DeviceDescriptor::SIZE;
        let (buf_va, buf_phys) = unsafe { alloc_dma_buffer(buf_size) }?;

        let ring = self.transfer_rings[slot_id as usize][1].as_mut()?;
        ring.enqueue_control_transfer(
            USB_DIR_IN | USB_TYPE_STANDARD | USB_RECIP_DEVICE,
            USB_REQ_GET_DESCRIPTOR,
            (USB_DESC_DEVICE as u16) << 8,
            0,
            buf_phys,
            buf_size as u16,
        );

        // Ring EP0 doorbell (DCI=1)
        self.db.ring_endpoint(slot_id, 1);

        // Wait for transfer event
        let evt = self.wait_transfer_event(slot_id)?;
        if evt.completion_code() != TRB_COMPLETION_SUCCESS
            && evt.completion_code() != TRB_COMPLETION_SHORT_PACKET
        {
            log::warn!(
                "xhci: GET_DESCRIPTOR(Device) failed: code={}",
                evt.completion_code()
            );
            return None;
        }

        let data = unsafe { read_dma_buffer(buf_va, buf_size) };
        DeviceDescriptor::parse(&data)
    }

    /// Issue GET_DESCRIPTOR(Configuration) on EP0.
    fn get_configuration_descriptor(
        &mut self,
        slot_id: u8,
        config_index: u8,
    ) -> Option<ParsedConfiguration> {
        log::debug!(
            "xhci: GET_DESCRIPTOR(Configuration, index={}) slot={}",
            config_index, slot_id
        );

        // First, get just the header to learn wTotalLength
        let header_size = 9;
        let (hdr_va, hdr_phys) = unsafe { alloc_dma_buffer(header_size) }?;

        let ring = self.transfer_rings[slot_id as usize][1].as_mut()?;
        ring.enqueue_control_transfer(
            USB_DIR_IN | USB_TYPE_STANDARD | USB_RECIP_DEVICE,
            USB_REQ_GET_DESCRIPTOR,
            (USB_DESC_CONFIGURATION as u16) << 8 | config_index as u16,
            0,
            hdr_phys,
            header_size as u16,
        );
        self.db.ring_endpoint(slot_id, 1);

        let evt = self.wait_transfer_event(slot_id)?;
        if evt.completion_code() != TRB_COMPLETION_SUCCESS
            && evt.completion_code() != TRB_COMPLETION_SHORT_PACKET
        {
            log::warn!(
                "xhci: GET_DESCRIPTOR(Config header) failed: code={}",
                evt.completion_code()
            );
            return None;
        }

        let hdr_data = unsafe { read_dma_buffer(hdr_va, header_size) };
        let total_len = u16::from_le_bytes([hdr_data[2], hdr_data[3]]) as usize;
        log::debug!("xhci: config descriptor total length = {}", total_len);

        // Now get the full descriptor set
        let (full_va, full_phys) = unsafe { alloc_dma_buffer(total_len) }?;

        let ring = self.transfer_rings[slot_id as usize][1].as_mut()?;
        ring.enqueue_control_transfer(
            USB_DIR_IN | USB_TYPE_STANDARD | USB_RECIP_DEVICE,
            USB_REQ_GET_DESCRIPTOR,
            (USB_DESC_CONFIGURATION as u16) << 8 | config_index as u16,
            0,
            full_phys,
            total_len as u16,
        );
        self.db.ring_endpoint(slot_id, 1);

        let evt = self.wait_transfer_event(slot_id)?;
        if evt.completion_code() != TRB_COMPLETION_SUCCESS
            && evt.completion_code() != TRB_COMPLETION_SHORT_PACKET
        {
            log::warn!(
                "xhci: GET_DESCRIPTOR(Config full) failed: code={}",
                evt.completion_code()
            );
            return None;
        }

        let full_data = unsafe { read_dma_buffer(full_va, total_len) };
        ParsedConfiguration::parse(&full_data)
    }

    /// Issue SET_CONFIGURATION and Configure Endpoint command.
    fn set_configuration(
        &mut self,
        slot_id: u8,
        config_value: u8,
        config: &ParsedConfiguration,
    ) -> bool {
        log::debug!(
            "xhci: SET_CONFIGURATION slot={} value={}",
            slot_id, config_value
        );

        // First, send the USB SET_CONFIGURATION request on EP0
        let ring = match self.transfer_rings[slot_id as usize][1].as_mut() {
            Some(r) => r,
            None => return false,
        };
        ring.enqueue_control_transfer(
            USB_DIR_OUT | USB_TYPE_STANDARD | USB_RECIP_DEVICE,
            USB_REQ_SET_CONFIGURATION,
            config_value as u16,
            0,
            0,
            0,
        );
        self.db.ring_endpoint(slot_id, 1);

        match self.wait_transfer_event(slot_id) {
            Some(evt) if evt.completion_code() == TRB_COMPLETION_SUCCESS => {
                log::debug!("xhci: SET_CONFIGURATION USB request succeeded");
            }
            Some(evt) => {
                log::warn!(
                    "xhci: SET_CONFIGURATION USB request failed: code={}",
                    evt.completion_code()
                );
                return false;
            }
            None => {
                log::warn!("xhci: SET_CONFIGURATION timeout");
                return false;
            }
        }

        // Now issue xHCI Configure Endpoint command with all non-EP0 endpoints
        let ctx_size = self.ctx_size;
        let input_ctx = match unsafe { InputContext::new(ctx_size) } {
            Some(c) => c,
            None => {
                log::error!("xhci: failed to allocate input context for configure endpoint");
                return false;
            }
        };

        // Calculate the highest DCI we need
        let mut max_dci: u8 = 1; // At least EP0

        // Add flags start with Slot Context (bit 0)
        let mut add_flags: u32 = 1; // Bit 0 = Slot Context

        for (_iface_num, ep_desc) in &config.endpoints {
            let dci = ep_desc.dci();
            if dci > max_dci {
                max_dci = dci;
            }
            add_flags |= 1 << dci;

            // Allocate transfer ring for this endpoint
            let tr = match unsafe { TransferRing::new() } {
                Some(r) => r,
                None => {
                    log::error!("xhci: failed to allocate transfer ring for DCI {}", dci);
                    return false;
                }
            };
            let tr_phys = tr.phys_addr_with_dcs();

            // Build endpoint context
            let mut ep_ctx = EndpointContext::new(ctx_size);
            ep_ctx
                .set_ep_type(ep_desc.xhci_ep_type())
                .set_max_packet_size(ep_desc.w_max_packet_size)
                .set_cerr(3)
                .set_interval(ep_desc.b_interval)
                .set_tr_dequeue_pointer(tr_phys)
                .set_average_trb_length(if ep_desc.is_interrupt() { 8 } else { 1024 });

            input_ctx.write_endpoint_context(dci, &ep_ctx);

            log::debug!(
                "xhci: configure EP DCI={} type={} max_pkt={} interval={} ring={:#x}",
                dci,
                ep_desc.xhci_ep_type(),
                ep_desc.w_max_packet_size,
                ep_desc.b_interval,
                tr_phys,
            );

            // Ensure storage
            while self.transfer_rings[slot_id as usize].len() <= dci as usize {
                self.transfer_rings[slot_id as usize].push(None);
            }
            self.transfer_rings[slot_id as usize][dci as usize] = Some(tr);
        }

        input_ctx.set_add_flags(add_flags);

        // Update Slot Context with new Context Entries
        let speed = self.devices[slot_id as usize]
            .as_ref()
            .map(|d| d.speed)
            .unwrap_or(UsbSpeed::Unknown);
        let port = self.devices[slot_id as usize]
            .as_ref()
            .map(|d| d.port)
            .unwrap_or(0);

        let mut slot = SlotContext::new(ctx_size);
        slot.set_route_string(0)
            .set_speed(speed.to_slot_speed())
            .set_context_entries(max_dci)
            .set_root_hub_port(port);
        input_ctx.write_slot_context(&slot);

        log::debug!(
            "xhci: Configure Endpoint: add_flags={:#x} max_dci={}",
            add_flags, max_dci
        );

        let trb = Trb::configure_endpoint(input_ctx.phys_addr(), slot_id, false);
        match self.send_command(trb) {
            Some(evt) if evt.completion_code() == TRB_COMPLETION_SUCCESS => {
                log::info!("xhci: slot {} configured successfully", slot_id);
                true
            }
            Some(evt) => {
                log::warn!(
                    "xhci: Configure Endpoint failed for slot {}: code={}",
                    slot_id, evt.completion_code()
                );
                false
            }
            None => {
                log::warn!("xhci: Configure Endpoint timeout for slot {}", slot_id);
                false
            }
        }
    }

    // -----------------------------------------------------------------------
    // HID Keyboard setup
    // -----------------------------------------------------------------------

    /// Set up a HID keyboard: SET_PROTOCOL(Boot), SET_IDLE, start interrupt transfers.
    fn setup_keyboard(&mut self, slot_id: u8, iface_num: u8, ep_desc: &EndpointDescriptor) {
        let dci = ep_desc.dci();

        log::info!(
            "xhci: keyboard setup: slot={} iface={} ep_addr={:#x} dci={} max_pkt={}",
            slot_id, iface_num, ep_desc.b_endpoint_address, dci, ep_desc.w_max_packet_size
        );

        // SET_PROTOCOL(Boot Protocol = 0)
        log::debug!("xhci: SET_PROTOCOL(Boot) on interface {}", iface_num);
        if let Some(ring) = self.transfer_rings[slot_id as usize][1].as_mut() {
            ring.enqueue_control_transfer(
                USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE,
                HID_REQ_SET_PROTOCOL,
                HID_PROTOCOL_BOOT,
                iface_num as u16,
                0,
                0,
            );
            self.db.ring_endpoint(slot_id, 1);

            match self.wait_transfer_event(slot_id) {
                Some(evt) if evt.completion_code() == TRB_COMPLETION_SUCCESS => {
                    log::info!("xhci: SET_PROTOCOL(Boot) succeeded");
                }
                Some(evt) => {
                    log::warn!("xhci: SET_PROTOCOL(Boot) failed: code={}", evt.completion_code());
                }
                None => {
                    log::warn!("xhci: SET_PROTOCOL(Boot) timeout");
                }
            }
        }

        // SET_IDLE(0) — don't wait for changes, report constantly
        log::debug!("xhci: SET_IDLE(0) on interface {}", iface_num);
        if let Some(ring) = self.transfer_rings[slot_id as usize][1].as_mut() {
            ring.enqueue_control_transfer(
                USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE,
                HID_REQ_SET_IDLE,
                0, // duration=0, report_id=0
                iface_num as u16,
                0,
                0,
            );
            self.db.ring_endpoint(slot_id, 1);

            match self.wait_transfer_event(slot_id) {
                Some(evt) if evt.completion_code() == TRB_COMPLETION_SUCCESS => {
                    log::debug!("xhci: SET_IDLE succeeded");
                }
                Some(evt) => {
                    // Some keyboards STALL on SET_IDLE, which is fine
                    log::debug!(
                        "xhci: SET_IDLE returned code={} (may be STALL, continuing)",
                        evt.completion_code()
                    );
                }
                None => {
                    log::debug!("xhci: SET_IDLE timeout (continuing anyway)");
                }
            }
        }

        // Allocate report buffer for interrupt transfers
        let (report_va, report_phys) = match unsafe { alloc_dma_buffer(8) } {
            Some(b) => b,
            None => {
                log::error!("xhci: failed to allocate keyboard report buffer");
                return;
            }
        };

        // Queue the first interrupt IN transfer
        if let Some(ring) = self.transfer_rings[slot_id as usize].get_mut(dci as usize) {
            if let Some(ring) = ring.as_mut() {
                ring.enqueue_interrupt_in(report_phys, 8);
                self.db.ring_endpoint(slot_id, dci);
                log::debug!("xhci: first keyboard interrupt transfer queued");
            }
        }

        // Record keyboard info
        if let Some(ref mut dev) = self.devices[slot_id as usize] {
            dev.keyboard_interface = Some(iface_num);
            dev.keyboard_endpoint_dci = Some(dci);
        }

        self.keyboard = Some(KeyboardInfo {
            slot_id,
            dci,
            report_buf_va: report_va,
            report_buf_phys: report_phys,
            state: KeyboardState::new(),
            transfer_pending: true,
        });

        log::info!("xhci: keyboard ready on slot={} dci={}", slot_id, dci);
    }

    // -----------------------------------------------------------------------
    // Keyboard polling
    // -----------------------------------------------------------------------

    /// Poll for keyboard events. Call this from the kernel's main loop or
    /// interrupt handler. Returns the next key event, if any.
    pub fn poll_keyboard(&mut self) -> Option<KeyEvent> {
        let kb = self.keyboard.as_mut()?;

        // First, check if we have buffered events
        if let Some(evt) = kb.state.next_event() {
            return Some(evt);
        }

        // Check the event ring for transfer completion events
        while let Some(evt) = self.evt_ring.dequeue() {
            // Update ERDP
            unsafe {
                self.rt.set_erdp(0, self.evt_ring.dequeue_phys() | (1 << 3));
            }

            let evt_type = evt.trb_type();

            if evt_type == TRB_TYPE_TRANSFER_EVENT {
                let evt_slot = evt.slot_id();
                let evt_ep = evt.endpoint_id();
                let code = evt.completion_code();

                // Re-borrow keyboard info
                let kb = match self.keyboard.as_mut() {
                    Some(kb) => kb,
                    None => continue,
                };

                if evt_slot == kb.slot_id && evt_ep == kb.dci {
                    log::trace!(
                        "xhci: keyboard transfer event: code={} residual={}",
                        code,
                        evt.transfer_length()
                    );

                    if code == TRB_COMPLETION_SUCCESS || code == TRB_COMPLETION_SHORT_PACKET {
                        // Parse the HID report
                        let report_data = unsafe {
                            read_dma_buffer(kb.report_buf_va, 8)
                        };

                        if let Some(report) = BootKeyboardReport::parse(&report_data) {
                            log::trace!(
                                "xhci: keyboard report: mods={:#x} keys=[{:#x},{:#x},{:#x},{:#x},{:#x},{:#x}]",
                                report.modifiers,
                                report.keycodes[0], report.keycodes[1], report.keycodes[2],
                                report.keycodes[3], report.keycodes[4], report.keycodes[5],
                            );
                            kb.state.process_report(&report);
                        }
                    } else {
                        log::warn!("xhci: keyboard transfer error: code={}", code);
                    }

                    // Re-queue the interrupt transfer
                    let buf_phys = kb.report_buf_phys;
                    let slot_id = kb.slot_id;
                    let dci = kb.dci;

                    if let Some(ring) = self.transfer_rings[slot_id as usize]
                        .get_mut(dci as usize)
                        .and_then(|r| r.as_mut())
                    {
                        ring.enqueue_interrupt_in(buf_phys, 8);
                        self.db.ring_endpoint(slot_id, dci);
                    }
                }
            } else if evt_type == TRB_TYPE_PORT_STATUS_CHANGE {
                let port_id = (evt.parameter() >> 24) as u8;
                log::info!("xhci: port status change event: port={}", port_id);
            } else {
                log::trace!("xhci: unhandled event type {} during keyboard poll", evt_type);
            }
        }

        // Clear any pending interrupt
        let iman = self.rt.iman(0);
        if iman & IMAN_IP != 0 {
            self.rt.set_iman(0, iman | IMAN_IP); // W1C to clear IP
        }

        // Return buffered event if any were generated
        let kb = self.keyboard.as_mut()?;
        kb.state.next_event()
    }

    /// Check if a keyboard has been found and initialized.
    pub fn has_keyboard(&self) -> bool {
        self.keyboard.is_some()
    }

    // -----------------------------------------------------------------------
    // Transfer event waiting
    // -----------------------------------------------------------------------

    /// Wait for a transfer event on any endpoint. Returns the event TRB.
    fn wait_transfer_event(&mut self, _expected_slot: u8) -> Option<Trb> {
        for _ in 0..MAX_EVENT_POLL_RETRIES {
            if let Some(evt) = self.evt_ring.dequeue() {
                // Update ERDP
                unsafe {
                    self.rt.set_erdp(0, self.evt_ring.dequeue_phys() | (1 << 3));
                }

                let evt_type = evt.trb_type();

                if evt_type == TRB_TYPE_TRANSFER_EVENT
                    || evt_type == TRB_TYPE_COMMAND_COMPLETION
                {
                    return Some(evt);
                }

                if evt_type == TRB_TYPE_PORT_STATUS_CHANGE {
                    let port_id = (evt.parameter() >> 24) as u8;
                    log::debug!("xhci: port status change during transfer wait: port={}", port_id);
                    continue;
                }

                log::trace!("xhci: unexpected event type {} during transfer wait", evt_type);
                continue;
            }
            core::hint::spin_loop();
        }

        log::warn!("xhci: transfer event timeout");
        None
    }

    // -----------------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------------

    /// Print controller status to log.
    pub fn dump_status(&self) {
        let usbsts = self.op.usbsts();
        let usbcmd = self.op.usbcmd();

        log::info!("xhci: === Controller Status ===");
        log::info!("xhci: USBCMD={:#010x} USBSTS={:#010x}", usbcmd, usbsts);
        log::info!(
            "xhci:   RS={} HCH={} EINT={} PCD={} CNR={} HCE={}",
            usbcmd & USBCMD_RS != 0,
            usbsts & USBSTS_HCH != 0,
            usbsts & USBSTS_EINT != 0,
            usbsts & USBSTS_PCD != 0,
            usbsts & USBSTS_CNR != 0,
            usbsts & USBSTS_HCE != 0,
        );

        for port in 1..=self.max_ports {
            let portsc = self.op.portsc(port);
            if portsc & PORTSC_CCS != 0 {
                let speed = (portsc & PORTSC_SPEED_MASK) >> PORTSC_SPEED_SHIFT;
                let enabled = portsc & PORTSC_PED != 0;
                log::info!(
                    "xhci:   port {}: connected speed={} enabled={}",
                    port, speed, enabled
                );
            }
        }

        for (slot_id, dev) in self.devices.iter().enumerate() {
            if let Some(dev) = dev {
                log::info!(
                    "xhci:   slot {}: port={} speed={:?} configured={} keyboard={}",
                    slot_id,
                    dev.port,
                    dev.speed,
                    dev.configured,
                    dev.is_keyboard(),
                );
                if let Some(ref desc) = dev.device_desc {
                    log::info!(
                        "xhci:     VID={:#06x} PID={:#06x}",
                        desc.id_vendor, desc.id_product
                    );
                }
            }
        }
    }
}
