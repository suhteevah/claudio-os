//! Transmit (TFD) and receive (RXQ) descriptor ring management.
//!
//! Intel WiFi devices use a split-queue DMA architecture:
//!
//! - **Transmit**: TFD (Transmit Frame Descriptor) rings. Each TFD points to
//!   up to 20 scatter-gather fragments in host memory. Queue 0 is the command
//!   queue; queues 1+ are data queues (one per TID for QoS).
//!
//! - **Receive**: A single RXQ with large buffers (4 KiB each). The firmware
//!   writes received frames and command responses into RX buffers and posts
//!   status entries to a status ring.
//!
//! # Memory layout
//!
//! All descriptor rings and buffers must be physically contiguous and
//! 256-byte aligned. The driver allocates them from the kernel heap and
//! converts virtual addresses to physical addresses via the known
//! `phys_mem_offset`.

use alloc::vec::Vec;
use alloc::boxed::Box;

/// Number of TFDs per transmit queue (must be a power of 2, max 256).
pub const TFD_QUEUE_SIZE: usize = 256;

/// Number of RX buffers in the receive queue.
pub const RXQ_SIZE: usize = 256;

/// Size of each RX buffer in bytes.
pub const RX_BUF_SIZE: usize = 4096;

/// Maximum scatter-gather fragments per TFD.
pub const TFD_MAX_FRAGMENTS: usize = 20;

/// A single scatter-gather fragment within a TFD.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct TfdFragment {
    /// Physical address of the data buffer.
    pub addr: u64,
    /// Length of the fragment in bytes (low 16 bits) + TB2_CTRL (upper 16 bits).
    pub len_ctrl: u32,
}

/// Transmit Frame Descriptor — one entry in a TFD ring.
///
/// Each TFD can point to up to 20 fragments. The total frame (header + payload)
/// is assembled from these fragments by the DMA engine.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Tfd {
    /// Reserved / scratch DWORDs used by firmware.
    pub scratch: [u32; 2],
    /// Number of valid fragments (TBs) in this TFD.
    pub num_tbs: u8,
    /// Reserved.
    pub _reserved: [u8; 3],
    /// Scatter-gather fragment array.
    pub tbs: [TfdFragment; TFD_MAX_FRAGMENTS],
}

impl Tfd {
    /// Create a zeroed TFD.
    pub const fn zeroed() -> Self {
        Self {
            scratch: [0; 2],
            num_tbs: 0,
            _reserved: [0; 3],
            tbs: [TfdFragment { addr: 0, len_ctrl: 0 }; TFD_MAX_FRAGMENTS],
        }
    }
}

/// A transmit queue (TFD ring + associated byte count table).
pub struct TxQueue {
    /// TFD ring (physically contiguous, 256-byte aligned).
    pub tfds: Box<[Tfd; TFD_QUEUE_SIZE]>,
    /// Byte count table — one u16 per TFD, firmware uses for scheduling.
    pub byte_count_table: Box<[u16; TFD_QUEUE_SIZE]>,
    /// Pending transmit buffers (kept alive until firmware ACKs).
    pub pending_bufs: [Option<Vec<u8>>; TFD_QUEUE_SIZE],
    /// Write index (next TFD to fill).
    pub write_idx: usize,
    /// Read index (next TFD the firmware will consume).
    pub read_idx: usize,
    /// Queue ID (0 = command queue).
    pub queue_id: u8,
}

impl TxQueue {
    /// Allocate a new transmit queue.
    pub fn new(queue_id: u8) -> Self {
        log::info!("wifi::tx_rx: allocating TX queue {} ({} TFDs)", queue_id, TFD_QUEUE_SIZE);

        let tfds = Box::new([Tfd::zeroed(); TFD_QUEUE_SIZE]);
        let byte_count_table = Box::new([0u16; TFD_QUEUE_SIZE]);

        Self {
            tfds,
            byte_count_table,
            pending_bufs: core::array::from_fn(|_| None),
            write_idx: 0,
            read_idx: 0,
            queue_id,
        }
    }

    /// Enqueue a frame for transmission. Returns the TFD index used.
    ///
    /// The caller must also poke the device's write pointer register to notify
    /// the firmware that new work is available.
    pub fn enqueue(&mut self, data: Vec<u8>, phys_addr: u64) -> Option<usize> {
        let idx = self.write_idx;
        let next = (idx + 1) % TFD_QUEUE_SIZE;

        if next == self.read_idx {
            log::warn!("wifi::tx_rx: TX queue {} full", self.queue_id);
            return None;
        }

        let len = data.len();
        self.tfds[idx].num_tbs = 1;
        self.tfds[idx].tbs[0] = TfdFragment {
            addr: phys_addr,
            len_ctrl: len as u32,
        };
        self.byte_count_table[idx] = len as u16;
        self.pending_bufs[idx] = Some(data);
        self.write_idx = next;

        log::trace!(
            "wifi::tx_rx: TX queue {} enqueue idx={} len={} phys=0x{:016X}",
            self.queue_id, idx, len, phys_addr
        );

        Some(idx)
    }

    /// Mark a TFD as completed by the firmware. Frees the pending buffer.
    pub fn complete(&mut self, idx: usize) {
        if let Some(buf) = self.pending_bufs[idx].take() {
            log::trace!("wifi::tx_rx: TX queue {} complete idx={} (freed {} bytes)", self.queue_id, idx, buf.len());
        }
        self.read_idx = (idx + 1) % TFD_QUEUE_SIZE;
    }

    /// Number of free TFD slots.
    pub fn free_slots(&self) -> usize {
        if self.write_idx >= self.read_idx {
            TFD_QUEUE_SIZE - 1 - (self.write_idx - self.read_idx)
        } else {
            self.read_idx - self.write_idx - 1
        }
    }
}

/// A single entry in the RX status ring.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct RxStatus {
    /// Length of the received data.
    pub len: u16,
    /// Flags (frame type, error bits, etc.).
    pub flags: u16,
}

/// Receive queue (RX buffer ring + status ring).
pub struct RxQueue {
    /// RX buffer pointers (physical addresses posted to device).
    pub buf_phys_addrs: Box<[u64; RXQ_SIZE]>,
    /// RX buffer virtual addresses (for reading received data).
    pub bufs: Vec<Vec<u8>>,
    /// Read index (next buffer to check for received data).
    pub read_idx: usize,
    /// Write index (posted to device — how far ahead we've prepared buffers).
    pub write_idx: usize,
}

impl RxQueue {
    /// Allocate a new receive queue with pre-allocated buffers.
    pub fn new(phys_mem_offset: u64) -> Self {
        log::info!("wifi::tx_rx: allocating RX queue ({} buffers x {} bytes)", RXQ_SIZE, RX_BUF_SIZE);

        let mut bufs = Vec::with_capacity(RXQ_SIZE);
        let mut buf_phys_addrs = Box::new([0u64; RXQ_SIZE]);

        for i in 0..RXQ_SIZE {
            let buf = alloc::vec![0u8; RX_BUF_SIZE];
            let virt_addr = buf.as_ptr() as u64;
            // Convert virtual address to physical address.
            // In ClaudioOS single-address-space model, phys = virt - phys_mem_offset.
            let phys_addr = virt_addr.wrapping_sub(phys_mem_offset);
            buf_phys_addrs[i] = phys_addr;
            bufs.push(buf);
        }

        log::debug!("wifi::tx_rx: RX queue allocated, {} buffers ready", RXQ_SIZE);

        Self {
            buf_phys_addrs,
            bufs,
            read_idx: 0,
            write_idx: RXQ_SIZE,
        }
    }

    /// Read a received frame from the next RX buffer.
    ///
    /// Returns `None` if no new frames are available.
    pub fn receive(&mut self, status_ring: &[RxStatus]) -> Option<(usize, &[u8])> {
        let idx = self.read_idx % RXQ_SIZE;
        let status = &status_ring[idx];

        if status.len == 0 {
            return None;
        }

        let len = status.len as usize;
        let data = &self.bufs[idx][..len.min(RX_BUF_SIZE)];

        log::trace!("wifi::tx_rx: RX idx={} len={} flags=0x{:04X}", idx, len, status.flags);

        self.read_idx += 1;
        Some((idx, data))
    }

    /// Recycle a buffer after the caller has finished processing the received data.
    pub fn recycle(&mut self, _idx: usize) {
        // Buffer stays in place — just advance write_idx so the device can reuse it.
        self.write_idx += 1;
        log::trace!("wifi::tx_rx: RX buffer recycled, write_idx={}", self.write_idx);
    }
}

// -------------------------------------------------------------------
// Device register helpers for queue management
// -------------------------------------------------------------------

/// TFD queue write pointer register base (one per queue).
pub const TFH_WRITE_PTR_BASE: u32 = 0x1C00;
/// Stride between queue write pointer registers.
pub const TFH_WRITE_PTR_STRIDE: u32 = 0x20;

/// RXQ write pointer register.
pub const RXQ_WRITE_PTR: u32 = 0x2000;

/// Write the TFD queue write pointer to notify firmware of new work.
///
/// # Safety
///
/// `mmio_base` must be valid and mapped.
pub unsafe fn poke_tx_write_ptr(mmio_base: *mut u8, queue_id: u8, write_idx: usize) {
    let reg = TFH_WRITE_PTR_BASE + (queue_id as u32) * TFH_WRITE_PTR_STRIDE;
    let ptr = mmio_base.add(reg as usize) as *mut u32;
    core::ptr::write_volatile(ptr, write_idx as u32);
    log::trace!("wifi::tx_rx: poked TX write ptr queue={} idx={}", queue_id, write_idx);
}

/// Write the RXQ write pointer to tell firmware how many RX buffers are available.
///
/// # Safety
///
/// `mmio_base` must be valid and mapped.
pub unsafe fn poke_rx_write_ptr(mmio_base: *mut u8, write_idx: usize) {
    let ptr = mmio_base.add(RXQ_WRITE_PTR as usize) as *mut u32;
    core::ptr::write_volatile(ptr, (write_idx % RXQ_SIZE) as u32);
    log::trace!("wifi::tx_rx: poked RX write ptr idx={}", write_idx % RXQ_SIZE);
}
