//! xHCI Device Context, Endpoint Context, and Input Context structures
//!
//! Per xHCI spec section 6.2, contexts are the data structures the controller uses
//! to track device state. The context size is either 32 or 64 bytes depending on
//! the CSZ flag in HCCPARAMS1.
//!
//! Layout:
//! - **Device Context**: Slot Context + 31 Endpoint Contexts
//! - **Input Context**: Input Control Context + Slot Context + 31 Endpoint Contexts
//! - **DCBAA**: Array of 64-bit pointers to Device Contexts, indexed by slot ID

use alloc::alloc::{alloc_zeroed, Layout};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;

// ---------------------------------------------------------------------------
// Slot Context fields (32 bytes, Table 6-4 in xHCI spec)
// ---------------------------------------------------------------------------

/// Slot Context — DW0: Route String (bits 19:0), Speed (bits 23:20), MTT (bit 25),
///                      Hub (bit 26), Context Entries (bits 31:27)
/// DW1: Max Exit Latency (bits 15:0), Root Hub Port Number (bits 23:16),
///      Number of Ports (bits 31:24)
/// DW2: TT Hub Slot ID (bits 7:0), TT Port Number (bits 15:8),
///      TTT (bits 17:16), Interrupter Target (bits 31:22)
/// DW3: USB Device Address (bits 7:0), Slot State (bits 31:27)

pub const SLOT_CTX_DW0_ROUTE_STRING_MASK: u32 = 0x000F_FFFF;
pub const SLOT_CTX_DW0_SPEED_SHIFT: u32 = 20;
pub const SLOT_CTX_DW0_SPEED_MASK: u32 = 0xF << 20;
pub const SLOT_CTX_DW0_MTT: u32 = 1 << 25;
pub const SLOT_CTX_DW0_HUB: u32 = 1 << 26;
pub const SLOT_CTX_DW0_CONTEXT_ENTRIES_SHIFT: u32 = 27;
pub const SLOT_CTX_DW0_CONTEXT_ENTRIES_MASK: u32 = 0x1F << 27;

pub const SLOT_CTX_DW1_MAX_EXIT_LATENCY_MASK: u32 = 0xFFFF;
pub const SLOT_CTX_DW1_ROOT_HUB_PORT_SHIFT: u32 = 16;
pub const SLOT_CTX_DW1_ROOT_HUB_PORT_MASK: u32 = 0xFF << 16;
pub const SLOT_CTX_DW1_NUM_PORTS_SHIFT: u32 = 24;
pub const SLOT_CTX_DW1_NUM_PORTS_MASK: u32 = 0xFF << 24;

pub const SLOT_CTX_DW3_DEVICE_ADDRESS_MASK: u32 = 0xFF;
pub const SLOT_CTX_DW3_SLOT_STATE_SHIFT: u32 = 27;
pub const SLOT_CTX_DW3_SLOT_STATE_MASK: u32 = 0x1F << 27;

/// Slot states per xHCI spec Table 6-6
pub const SLOT_STATE_DISABLED: u32 = 0;
pub const SLOT_STATE_DEFAULT: u32 = 1;
pub const SLOT_STATE_ADDRESSED: u32 = 2;
pub const SLOT_STATE_CONFIGURED: u32 = 3;

/// Speed values for slot context
pub const SPEED_FULL: u32 = 1;
pub const SPEED_LOW: u32 = 2;
pub const SPEED_HIGH: u32 = 3;
pub const SPEED_SUPER: u32 = 4;

// ---------------------------------------------------------------------------
// Endpoint Context fields (32 bytes, Table 6-7 in xHCI spec)
// ---------------------------------------------------------------------------

/// Endpoint Context — DW0: EP State (bits 2:0), Mult (bits 9:8),
///                         MaxPStreams (bits 14:10), Interval (bits 23:16),
///                         Max ESIT Payload Hi (bits 31:24)
/// DW1: CErr (bits 2:1), EP Type (bits 5:3), Max Burst Size (bits 15:8),
///      Max Packet Size (bits 31:16)
/// DW2-3: TR Dequeue Pointer (64-bit, bits 63:4), DCS (bit 0)
/// DW4: Average TRB Length (bits 15:0), Max ESIT Payload Lo (bits 31:16)

pub const EP_CTX_DW0_STATE_MASK: u32 = 0x7;
pub const EP_CTX_DW0_MULT_SHIFT: u32 = 8;
pub const EP_CTX_DW0_MULT_MASK: u32 = 0x3 << 8;
pub const EP_CTX_DW0_MAX_P_STREAMS_SHIFT: u32 = 10;
pub const EP_CTX_DW0_MAX_P_STREAMS_MASK: u32 = 0x1F << 10;
pub const EP_CTX_DW0_INTERVAL_SHIFT: u32 = 16;
pub const EP_CTX_DW0_INTERVAL_MASK: u32 = 0xFF << 16;

pub const EP_CTX_DW1_CERR_SHIFT: u32 = 1;
pub const EP_CTX_DW1_CERR_MASK: u32 = 0x3 << 1;
pub const EP_CTX_DW1_EP_TYPE_SHIFT: u32 = 3;
pub const EP_CTX_DW1_EP_TYPE_MASK: u32 = 0x7 << 3;
pub const EP_CTX_DW1_MAX_BURST_SHIFT: u32 = 8;
pub const EP_CTX_DW1_MAX_BURST_MASK: u32 = 0xFF << 8;
pub const EP_CTX_DW1_MAX_PACKET_SIZE_SHIFT: u32 = 16;
pub const EP_CTX_DW1_MAX_PACKET_SIZE_MASK: u32 = 0xFFFF << 16;

pub const EP_CTX_DW4_AVG_TRB_LENGTH_MASK: u32 = 0xFFFF;
pub const EP_CTX_DW4_MAX_ESIT_PAYLOAD_MASK: u32 = 0xFFFF << 16;

/// Endpoint states per xHCI spec Table 6-8
pub const EP_STATE_DISABLED: u32 = 0;
pub const EP_STATE_RUNNING: u32 = 1;
pub const EP_STATE_HALTED: u32 = 2;
pub const EP_STATE_STOPPED: u32 = 3;
pub const EP_STATE_ERROR: u32 = 4;

/// Endpoint types per xHCI spec Table 6-9 (EP Type field, 3 bits)
pub const EP_TYPE_NOT_VALID: u32 = 0;
pub const EP_TYPE_ISOCH_OUT: u32 = 1;
pub const EP_TYPE_BULK_OUT: u32 = 2;
pub const EP_TYPE_INTERRUPT_OUT: u32 = 3;
pub const EP_TYPE_CONTROL: u32 = 4;
pub const EP_TYPE_ISOCH_IN: u32 = 5;
pub const EP_TYPE_BULK_IN: u32 = 6;
pub const EP_TYPE_INTERRUPT_IN: u32 = 7;

// ---------------------------------------------------------------------------
// Input Control Context fields (Table 6-2 in xHCI spec)
// ---------------------------------------------------------------------------

/// Input Control Context DW0: Drop Context flags (bits 31:2)
pub const INPUT_CTX_DROP_SHIFT: u32 = 0;
/// Input Control Context DW1: Add Context flags (bits 31:0)
/// Bit 0 = Slot Context, Bit 1 = EP0, Bit 2 = EP1 OUT, Bit 3 = EP1 IN, etc.

// ---------------------------------------------------------------------------
// Context wrapper: raw 32-byte or 64-byte context as DWORDs
// ---------------------------------------------------------------------------

/// A raw xHCI context (Slot, Endpoint, or Input Control Context).
/// Stored as an array of DWORDs for direct field manipulation.
#[derive(Clone)]
pub struct RawContext {
    /// DWORDs (8 for 32-byte context, 16 for 64-byte context)
    pub dwords: [u32; 16],
    /// Context size: 32 or 64 bytes
    pub ctx_size: usize,
}

impl RawContext {
    /// Create a zeroed context of the given size (32 or 64 bytes).
    pub fn new(ctx_size: usize) -> Self {
        assert!(ctx_size == 32 || ctx_size == 64, "xhci: invalid context size {}", ctx_size);
        Self {
            dwords: [0u32; 16],
            ctx_size,
        }
    }

    /// Read a DWORD field.
    pub fn dw(&self, index: usize) -> u32 {
        self.dwords[index]
    }

    /// Write a DWORD field.
    pub fn set_dw(&mut self, index: usize, val: u32) {
        self.dwords[index] = val;
    }

    /// Read a 64-bit field from two consecutive DWORDs.
    pub fn qw(&self, lo_index: usize) -> u64 {
        (self.dwords[lo_index + 1] as u64) << 32 | (self.dwords[lo_index] as u64)
    }

    /// Write a 64-bit field to two consecutive DWORDs.
    pub fn set_qw(&mut self, lo_index: usize, val: u64) {
        self.dwords[lo_index] = val as u32;
        self.dwords[lo_index + 1] = (val >> 32) as u32;
    }

    /// Write this context to MMIO memory at the given address.
    ///
    /// # Safety
    /// `addr` must be a valid, aligned destination for `ctx_size` bytes.
    pub unsafe fn write_to(&self, addr: *mut u32) {
        let dword_count = self.ctx_size / 4;
        for i in 0..dword_count {
            ptr::write_volatile(addr.add(i), self.dwords[i]);
        }
    }

    /// Read this context from MMIO memory at the given address.
    ///
    /// # Safety
    /// `addr` must be a valid source for `ctx_size` bytes.
    pub unsafe fn read_from(addr: *const u32, ctx_size: usize) -> Self {
        let mut ctx = Self::new(ctx_size);
        let dword_count = ctx_size / 4;
        for i in 0..dword_count {
            ctx.dwords[i] = ptr::read_volatile(addr.add(i));
        }
        ctx
    }
}

// ---------------------------------------------------------------------------
// Slot Context builder
// ---------------------------------------------------------------------------

/// Builder for a Slot Context.
pub struct SlotContext {
    pub raw: RawContext,
}

impl SlotContext {
    pub fn new(ctx_size: usize) -> Self {
        Self {
            raw: RawContext::new(ctx_size),
        }
    }

    /// Set route string (bits 19:0 of DW0)
    pub fn set_route_string(&mut self, route: u32) -> &mut Self {
        let dw0 = (self.raw.dw(0) & !SLOT_CTX_DW0_ROUTE_STRING_MASK)
            | (route & SLOT_CTX_DW0_ROUTE_STRING_MASK);
        self.raw.set_dw(0, dw0);
        self
    }

    /// Set device speed (bits 23:20 of DW0)
    pub fn set_speed(&mut self, speed: u32) -> &mut Self {
        let dw0 = (self.raw.dw(0) & !SLOT_CTX_DW0_SPEED_MASK)
            | ((speed << SLOT_CTX_DW0_SPEED_SHIFT) & SLOT_CTX_DW0_SPEED_MASK);
        self.raw.set_dw(0, dw0);
        self
    }

    /// Set Context Entries (bits 31:27 of DW0) — highest valid endpoint context index
    pub fn set_context_entries(&mut self, entries: u8) -> &mut Self {
        let dw0 = (self.raw.dw(0) & !SLOT_CTX_DW0_CONTEXT_ENTRIES_MASK)
            | (((entries as u32) << SLOT_CTX_DW0_CONTEXT_ENTRIES_SHIFT)
                & SLOT_CTX_DW0_CONTEXT_ENTRIES_MASK);
        self.raw.set_dw(0, dw0);
        self
    }

    /// Set Root Hub Port Number (bits 23:16 of DW1)
    pub fn set_root_hub_port(&mut self, port: u8) -> &mut Self {
        let dw1 = (self.raw.dw(1) & !SLOT_CTX_DW1_ROOT_HUB_PORT_MASK)
            | (((port as u32) << SLOT_CTX_DW1_ROOT_HUB_PORT_SHIFT)
                & SLOT_CTX_DW1_ROOT_HUB_PORT_MASK);
        self.raw.set_dw(1, dw1);
        self
    }

    /// Set Max Exit Latency (bits 15:0 of DW1)
    pub fn set_max_exit_latency(&mut self, latency: u16) -> &mut Self {
        let dw1 = (self.raw.dw(1) & !SLOT_CTX_DW1_MAX_EXIT_LATENCY_MASK)
            | (latency as u32 & SLOT_CTX_DW1_MAX_EXIT_LATENCY_MASK);
        self.raw.set_dw(1, dw1);
        self
    }

    /// Set Number of Ports (bits 31:24 of DW1, for hub devices)
    pub fn set_num_ports(&mut self, n: u8) -> &mut Self {
        let dw1 = (self.raw.dw(1) & !SLOT_CTX_DW1_NUM_PORTS_MASK)
            | (((n as u32) << SLOT_CTX_DW1_NUM_PORTS_SHIFT) & SLOT_CTX_DW1_NUM_PORTS_MASK);
        self.raw.set_dw(1, dw1);
        self
    }

    /// Set Hub flag (bit 26 of DW0)
    pub fn set_hub(&mut self, is_hub: bool) -> &mut Self {
        let dw0 = if is_hub {
            self.raw.dw(0) | SLOT_CTX_DW0_HUB
        } else {
            self.raw.dw(0) & !SLOT_CTX_DW0_HUB
        };
        self.raw.set_dw(0, dw0);
        self
    }

    /// Set Multi-TT flag (bit 25 of DW0)
    pub fn set_mtt(&mut self, mtt: bool) -> &mut Self {
        let dw0 = if mtt {
            self.raw.dw(0) | SLOT_CTX_DW0_MTT
        } else {
            self.raw.dw(0) & !SLOT_CTX_DW0_MTT
        };
        self.raw.set_dw(0, dw0);
        self
    }

    /// Get device address from DW3
    pub fn device_address(&self) -> u8 {
        (self.raw.dw(3) & SLOT_CTX_DW3_DEVICE_ADDRESS_MASK) as u8
    }

    /// Get slot state from DW3
    pub fn slot_state(&self) -> u32 {
        (self.raw.dw(3) & SLOT_CTX_DW3_SLOT_STATE_MASK) >> SLOT_CTX_DW3_SLOT_STATE_SHIFT
    }
}

// ---------------------------------------------------------------------------
// Endpoint Context builder
// ---------------------------------------------------------------------------

/// Builder for an Endpoint Context.
pub struct EndpointContext {
    pub raw: RawContext,
}

impl EndpointContext {
    pub fn new(ctx_size: usize) -> Self {
        Self {
            raw: RawContext::new(ctx_size),
        }
    }

    /// Set endpoint type (bits 5:3 of DW1)
    pub fn set_ep_type(&mut self, ep_type: u32) -> &mut Self {
        let dw1 = (self.raw.dw(1) & !EP_CTX_DW1_EP_TYPE_MASK)
            | ((ep_type << EP_CTX_DW1_EP_TYPE_SHIFT) & EP_CTX_DW1_EP_TYPE_MASK);
        self.raw.set_dw(1, dw1);
        self
    }

    /// Set max packet size (bits 31:16 of DW1)
    pub fn set_max_packet_size(&mut self, size: u16) -> &mut Self {
        let dw1 = (self.raw.dw(1) & !EP_CTX_DW1_MAX_PACKET_SIZE_MASK)
            | (((size as u32) << EP_CTX_DW1_MAX_PACKET_SIZE_SHIFT)
                & EP_CTX_DW1_MAX_PACKET_SIZE_MASK);
        self.raw.set_dw(1, dw1);
        self
    }

    /// Set max burst size (bits 15:8 of DW1)
    pub fn set_max_burst(&mut self, burst: u8) -> &mut Self {
        let dw1 = (self.raw.dw(1) & !EP_CTX_DW1_MAX_BURST_MASK)
            | (((burst as u32) << EP_CTX_DW1_MAX_BURST_SHIFT) & EP_CTX_DW1_MAX_BURST_MASK);
        self.raw.set_dw(1, dw1);
        self
    }

    /// Set CErr (error count, bits 2:1 of DW1) — usually 3 for non-isoch
    pub fn set_cerr(&mut self, cerr: u8) -> &mut Self {
        let dw1 = (self.raw.dw(1) & !EP_CTX_DW1_CERR_MASK)
            | (((cerr as u32) << EP_CTX_DW1_CERR_SHIFT) & EP_CTX_DW1_CERR_MASK);
        self.raw.set_dw(1, dw1);
        self
    }

    /// Set interval (bits 23:16 of DW0) — encoded as 2^(interval-1) * 125us for HS/SS
    pub fn set_interval(&mut self, interval: u8) -> &mut Self {
        let dw0 = (self.raw.dw(0) & !EP_CTX_DW0_INTERVAL_MASK)
            | (((interval as u32) << EP_CTX_DW0_INTERVAL_SHIFT) & EP_CTX_DW0_INTERVAL_MASK);
        self.raw.set_dw(0, dw0);
        self
    }

    /// Set Mult (bits 9:8 of DW0) — for isoch endpoints
    pub fn set_mult(&mut self, mult: u8) -> &mut Self {
        let dw0 = (self.raw.dw(0) & !EP_CTX_DW0_MULT_MASK)
            | (((mult as u32) << EP_CTX_DW0_MULT_SHIFT) & EP_CTX_DW0_MULT_MASK);
        self.raw.set_dw(0, dw0);
        self
    }

    /// Set TR Dequeue Pointer (DW2-DW3, 64-bit, bits 63:4 + DCS in bit 0)
    pub fn set_tr_dequeue_pointer(&mut self, phys_with_dcs: u64) -> &mut Self {
        self.raw.set_qw(2, phys_with_dcs);
        self
    }

    /// Set Average TRB Length (bits 15:0 of DW4)
    pub fn set_average_trb_length(&mut self, len: u16) -> &mut Self {
        let dw4 = (self.raw.dw(4) & !EP_CTX_DW4_AVG_TRB_LENGTH_MASK)
            | (len as u32 & EP_CTX_DW4_AVG_TRB_LENGTH_MASK);
        self.raw.set_dw(4, dw4);
        self
    }

    /// Set Max ESIT Payload Lo (bits 31:16 of DW4)
    pub fn set_max_esit_payload_lo(&mut self, payload: u16) -> &mut Self {
        let dw4 = (self.raw.dw(4) & !(EP_CTX_DW4_MAX_ESIT_PAYLOAD_MASK))
            | ((payload as u32) << 16);
        self.raw.set_dw(4, dw4);
        self
    }

    /// Get endpoint state (bits 2:0 of DW0)
    pub fn ep_state(&self) -> u32 {
        self.raw.dw(0) & EP_CTX_DW0_STATE_MASK
    }
}

// ---------------------------------------------------------------------------
// Input Context
// ---------------------------------------------------------------------------

/// An xHCI Input Context: Input Control Context + Device Context (Slot + 31 EPs).
///
/// The Input Context is used for Address Device, Configure Endpoint, and Evaluate
/// Context commands. It includes add/drop flags that tell the controller which
/// contexts to update.
pub struct InputContext {
    /// Backing memory (physically contiguous, aligned)
    buffer: *mut u8,
    /// Physical address of the buffer
    phys: u64,
    /// Context size (32 or 64 bytes per context entry)
    ctx_size: usize,
}

impl InputContext {
    /// Allocate a new Input Context.
    /// Total size = (1 + 1 + 31) * ctx_size = 33 * ctx_size
    ///
    /// # Safety
    /// Memory must be identity-mapped for DMA.
    pub unsafe fn new(ctx_size: usize) -> Option<Self> {
        let total_size = 33 * ctx_size;
        let layout = match Layout::from_size_align(total_size, 64) {
            Ok(l) => l,
            Err(_) => {
                log::error!("xhci: invalid input context layout");
                return None;
            }
        };
        let buffer = alloc_zeroed(layout);
        if buffer.is_null() {
            log::error!("xhci: input context allocation failed");
            return None;
        }

        let phys = buffer as u64;

        log::debug!(
            "xhci: input context allocated at {:#x} ({}*{} = {} bytes)",
            phys, 33, ctx_size, total_size
        );

        Some(Self {
            buffer,
            phys,
            ctx_size,
        })
    }

    /// Physical address of the Input Context (for command TRBs).
    pub fn phys_addr(&self) -> u64 {
        self.phys
    }

    /// Get a mutable pointer to the Input Control Context (entry 0).
    fn input_control_ptr(&self) -> *mut u32 {
        self.buffer as *mut u32
    }

    /// Set the Add Context flags (DW1 of Input Control Context).
    /// Bit 0 = add Slot Context, Bit 1 = add EP0 Context, etc.
    pub fn set_add_flags(&self, flags: u32) {
        unsafe {
            let ptr = self.input_control_ptr();
            // DW1 of Input Control Context = Add Context flags
            ptr::write_volatile(ptr.add(1), flags);
        }
        log::trace!("xhci: input context add flags = {:#010x}", flags);
    }

    /// Set the Drop Context flags (DW0 of Input Control Context).
    pub fn set_drop_flags(&self, flags: u32) {
        unsafe {
            let ptr = self.input_control_ptr();
            ptr::write_volatile(ptr, flags);
        }
        log::trace!("xhci: input context drop flags = {:#010x}", flags);
    }

    /// Get a mutable pointer to context entry `n` (0 = Input Control, 1 = Slot, 2 = EP0, ...).
    fn entry_ptr(&self, n: usize) -> *mut u32 {
        unsafe { (self.buffer as *mut u32).add(n * self.ctx_size / 4) }
    }

    /// Write a Slot Context into entry 1 of the Input Context.
    pub fn write_slot_context(&self, slot: &SlotContext) {
        unsafe { slot.raw.write_to(self.entry_ptr(1)) }
    }

    /// Write an Endpoint Context into the Input Context.
    /// `dci` is the Device Context Index (1 = EP0, 2 = EP1 OUT, 3 = EP1 IN, ...).
    /// The endpoint goes into entry (dci + 1) of the Input Context.
    pub fn write_endpoint_context(&self, dci: u8, ep: &EndpointContext) {
        let entry = dci as usize + 1;
        log::trace!("xhci: writing endpoint context DCI={} to input ctx entry {}", dci, entry);
        unsafe { ep.raw.write_to(self.entry_ptr(entry)) }
    }

    /// Clear the entire Input Context to zeroes.
    pub fn clear(&self) {
        let total_size = 33 * self.ctx_size;
        unsafe {
            ptr::write_bytes(self.buffer, 0, total_size);
        }
    }
}

// ---------------------------------------------------------------------------
// Device Context Base Address Array (DCBAA)
// ---------------------------------------------------------------------------

/// The DCBAA is an array of 64-bit physical pointers to Device Contexts.
/// Entry 0 is the Scratchpad Buffer Array pointer (or 0 if no scratchpad).
/// Entries 1..MaxSlots are device slot contexts.
pub struct Dcbaa {
    /// Backing memory for the array
    buffer: *mut u64,
    /// Physical address (for DCBAAP register)
    phys: u64,
    /// Max slots (determines array size)
    max_slots: u8,
    /// Device contexts (heap-allocated per slot)
    device_contexts: Vec<Option<DeviceContextAlloc>>,
}

/// A heap-allocated Device Context (Slot + 31 Endpoint Contexts).
struct DeviceContextAlloc {
    /// Virtual address
    va: *mut u8,
    /// Physical address
    phys: u64,
    /// Total size
    _size: usize,
}

impl Dcbaa {
    /// Allocate and initialize the DCBAA.
    ///
    /// # Safety
    /// Memory must be identity-mapped for DMA.
    pub unsafe fn new(max_slots: u8, ctx_size: usize, scratchpad_count: u32) -> Option<Self> {
        let entry_count = max_slots as usize + 1; // slot 0 = scratchpad
        let array_size = entry_count * 8; // 8 bytes per entry (u64 pointer)
        let layout = match Layout::from_size_align(array_size, 64) {
            Ok(l) => l,
            Err(_) => {
                log::error!("xhci: invalid DCBAA layout");
                return None;
            }
        };
        let buffer = alloc_zeroed(layout) as *mut u64;
        if buffer.is_null() {
            log::error!("xhci: DCBAA allocation failed");
            return None;
        }

        let phys = buffer as u64;

        log::info!(
            "xhci: DCBAA allocated at {:#x} ({} entries, {} bytes)",
            phys, entry_count, array_size
        );

        // Allocate scratchpad buffers if needed
        if scratchpad_count > 0 {
            log::info!("xhci: allocating {} scratchpad buffers", scratchpad_count);

            // Scratchpad Buffer Array: array of 64-bit physical addresses
            let sp_array_size = scratchpad_count as usize * 8;
            let sp_array_layout = match Layout::from_size_align(sp_array_size, 64) {
                Ok(l) => l,
                Err(_) => {
                    log::error!("xhci: invalid scratchpad array layout");
                    return None;
                }
            };
            let sp_array = alloc_zeroed(sp_array_layout) as *mut u64;
            if sp_array.is_null() {
                log::error!("xhci: scratchpad array alloc failed");
                return None;
            }

            // Allocate each scratchpad buffer page (4K aligned)
            for i in 0..scratchpad_count as usize {
                let page_layout = match Layout::from_size_align(4096, 4096) {
                    Ok(l) => l,
                    Err(_) => {
                        log::error!("xhci: invalid scratchpad page layout");
                        return None;
                    }
                };
                let page = alloc_zeroed(page_layout);
                if page.is_null() {
                    log::error!("xhci: scratchpad page {} alloc failed", i);
                    return None;
                }
                ptr::write_volatile(sp_array.add(i), page as u64);
            }

            // Entry 0 of DCBAA = scratchpad buffer array pointer
            ptr::write_volatile(buffer, sp_array as u64);
            log::debug!("xhci: scratchpad buffer array at {:#x}", sp_array as u64);
        }

        let mut device_contexts = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            device_contexts.push(None);
        }

        Some(Self {
            buffer,
            phys,
            max_slots,
            device_contexts,
        })
    }

    /// Physical address of the DCBAA (for programming DCBAAP).
    pub fn phys_addr(&self) -> u64 {
        self.phys
    }

    /// Allocate a Device Context for the given slot and register it in the DCBAA.
    ///
    /// # Safety
    /// Memory must be identity-mapped.
    pub unsafe fn alloc_device_context(&mut self, slot_id: u8, ctx_size: usize) -> Option<u64> {
        if slot_id < 1 || slot_id > self.max_slots {
            log::error!("xhci: invalid slot_id {}", slot_id);
            return None;
        }

        let total_size = 32 * ctx_size; // Slot + 31 EP contexts
        let layout = match Layout::from_size_align(total_size, 64) {
            Ok(l) => l,
            Err(_) => {
                log::error!("xhci: invalid device context layout for slot {}", slot_id);
                return None;
            }
        };
        let va = alloc_zeroed(layout);
        if va.is_null() {
            log::error!("xhci: device context alloc failed for slot {}", slot_id);
            return None;
        }

        let phys = va as u64;

        // Write the physical address into the DCBAA entry
        ptr::write_volatile(self.buffer.add(slot_id as usize), phys);

        log::info!(
            "xhci: device context for slot {} at {:#x} ({} bytes)",
            slot_id, phys, total_size
        );

        self.device_contexts[slot_id as usize] = Some(DeviceContextAlloc {
            va,
            phys,
            _size: total_size,
        });

        phys
    }

    /// Get the physical address of a device context for a slot.
    pub fn device_context_phys(&self, slot_id: u8) -> Option<u64> {
        self.device_contexts
            .get(slot_id as usize)?
            .as_ref()
            .map(|dc| dc.phys)
    }

    /// Read the Slot Context from a device context.
    ///
    /// # Safety
    /// The device context must have been allocated.
    pub unsafe fn read_slot_context(&self, slot_id: u8, ctx_size: usize) -> Option<SlotContext> {
        let dc = self.device_contexts.get(slot_id as usize)?.as_ref()?;
        let raw = RawContext::read_from(dc.va as *const u32, ctx_size);
        Some(SlotContext { raw })
    }

    /// Read an Endpoint Context from a device context.
    /// `dci` is the Device Context Index (1 = EP0, 2 = EP1 OUT, etc.).
    ///
    /// # Safety
    /// The device context must have been allocated.
    pub unsafe fn read_endpoint_context(
        &self,
        slot_id: u8,
        dci: u8,
        ctx_size: usize,
    ) -> Option<EndpointContext> {
        let dc = self.device_contexts.get(slot_id as usize)?.as_ref()?;
        let offset = dci as usize * ctx_size;
        let ptr = (dc.va as *const u32).add(offset / 4);
        let raw = RawContext::read_from(ptr, ctx_size);
        Some(EndpointContext { raw })
    }
}
