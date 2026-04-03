//! VirtIO-net driver for QEMU's `virtio-net-pci` device.
//!
//! Implements the VirtIO 0.9.5 (legacy) interface over PCI I/O ports.
//! Two virtqueues are used: RX (queue 0) and TX (queue 1). Each queue has a
//! descriptor table, available ring, and used ring allocated as a single
//! contiguous page-aligned region per the legacy spec.
//!
//! The driver pre-populates RX descriptors with 2048-byte buffers so the
//! device can DMA incoming frames into them. On transmit, a descriptor is
//! filled with a VirtIO-net header followed by the Ethernet frame and the
//! device is notified.

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{self, Ordering};
use x86_64::instructions::port::Port;

use crate::{NicDriver, NicError};

// ---------------------------------------------------------------------------
// PCI vendor/device for VirtIO transitional network device
// ---------------------------------------------------------------------------
pub const VIRTIO_NET_VENDOR: u16 = 0x1AF4;
pub const VIRTIO_NET_DEVICE_ID: u16 = 0x1000;

// ---------------------------------------------------------------------------
// VirtIO legacy PCI register offsets (relative to I/O base from BAR0)
// ---------------------------------------------------------------------------
const VIRTIO_DEVICE_FEATURES: u16 = 0x00; // 4 bytes, R
const VIRTIO_GUEST_FEATURES: u16 = 0x04; // 4 bytes, R+W
const VIRTIO_QUEUE_ADDR: u16 = 0x08; // 4 bytes, R+W (PFN)
const VIRTIO_QUEUE_SIZE: u16 = 0x0C; // 2 bytes, R
const VIRTIO_QUEUE_SELECT: u16 = 0x0E; // 2 bytes, R+W
const VIRTIO_QUEUE_NOTIFY: u16 = 0x10; // 2 bytes, R+W
const VIRTIO_DEVICE_STATUS: u16 = 0x12; // 1 byte,  R+W
const VIRTIO_ISR_STATUS: u16 = 0x13; // 1 byte,  R
const VIRTIO_MAC_BASE: u16 = 0x14; // 6 bytes, R  (device-specific config)

// ---------------------------------------------------------------------------
// Device status bits
// ---------------------------------------------------------------------------
const VIRTIO_STATUS_ACK: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
const _VIRTIO_STATUS_FAILED: u8 = 128;

// ---------------------------------------------------------------------------
// Feature bits we care about
// ---------------------------------------------------------------------------
const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTIO_NET_F_STATUS: u32 = 1 << 16;

// ---------------------------------------------------------------------------
// VirtIO-net header prepended to every frame (legacy, no mergeable-rx-bufs)
// ---------------------------------------------------------------------------
const VIRTIO_NET_HDR_SIZE: usize = 10;

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioNetHdr {
    flags: u8,
    gso_type: u8,
    hdr_len: u16,
    gso_size: u16,
    csum_start: u16,
    csum_offset: u16,
}

impl VirtioNetHdr {
    const fn zeroed() -> Self {
        Self {
            flags: 0,
            gso_type: 0,
            hdr_len: 0,
            gso_size: 0,
            csum_start: 0,
            csum_offset: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Virtqueue structures — 16-byte aligned descriptors, avail ring, used ring
// ---------------------------------------------------------------------------

/// Maximum queue size we support. QEMU's default is 256.
const QUEUE_SIZE: usize = 256;

/// Size of each receive/transmit buffer (including VirtIO-net header space).
const BUF_SIZE: usize = 2048;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

#[repr(C, align(2))]
struct VirtqAvail {
    flags: u16,
    idx: u16,
    ring: [u16; QUEUE_SIZE],
    /// Used-event suppression (virtio 1.0), not used in legacy but present in
    /// memory layout.
    used_event: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

#[repr(C, align(4))]
struct VirtqUsed {
    flags: u16,
    idx: u16,
    ring: [VirtqUsedElem; QUEUE_SIZE],
    /// Avail-event suppression.
    avail_event: u16,
}

// ---------------------------------------------------------------------------
// VirtQueue bookkeeping
// ---------------------------------------------------------------------------

struct VirtQueue {
    /// Base pointer of the contiguous legacy allocation.
    /// Descriptor table starts here.
    base: *mut u8,
    /// Pointer to the descriptor table (same as base, typed).
    descs: *mut VirtqDesc,
    /// Pointer to the available ring.
    avail: *mut VirtqAvail,
    /// Pointer to the used ring.
    used: *mut VirtqUsed,
    /// Actual queue size reported by the device (<= QUEUE_SIZE).
    queue_size: u16,
    /// Head of the free descriptor chain.
    free_head: u16,
    /// Number of free descriptors remaining.
    num_free: u16,
    /// Last observed `used.idx` for detecting newly completed descriptors.
    last_used_idx: u16,
    /// Pre-allocated DMA-safe buffers, one per descriptor.
    buffers: Vec<Box<[u8; BUF_SIZE]>>,
    /// Physical memory offset for virt->phys translation.
    ///
    /// With the bootloader's offset page table mapping:
    ///   virt_addr = phys_addr + phys_mem_offset
    /// So: phys = virt - phys_mem_offset
    ///
    /// IMPORTANT: This only works for memory in the bootloader's physical
    /// memory mapping region. Our kernel heap is mapped separately at
    /// HEAP_START (0x4444_4444_0000) and does NOT follow this formula.
    /// For heap allocations we need the actual physical frames backing them.
    /// However, the bootloader crate's OffsetPageTable means all physical
    /// memory is accessible at phys + offset, and for DMA we need the
    /// reverse mapping. Since the heap pages were allocated from the
    /// physical frame allocator, we can walk the page tables to find the
    /// physical address. For simplicity, we use a large page-aligned
    /// allocation which the frame allocator backs with contiguous frames
    /// starting from the allocated virtual address's backing frame.
    phys_mem_offset: u64,
}

impl VirtQueue {
    /// Convert a virtual address to a physical address.
    ///
    /// This walks the page table to find the physical address backing a
    /// virtual address. For heap-allocated memory, the simple
    /// `virt - phys_mem_offset` formula does NOT work because the heap is
    /// mapped at a different virtual range (0x4444_4444_0000).
    ///
    /// We use the x86_64 page table registers to do the translation.
    fn virt_to_phys(&self, virt: usize) -> Option<u64> {
        // For addresses in the physical memory mapping region
        // (phys_mem_offset .. phys_mem_offset + phys_mem_size),
        // the formula is: phys = virt - phys_mem_offset.
        //
        // For heap addresses (starting at 0x4444_4444_0000), we need to
        // walk the page tables. We do this by reading CR3 and manually
        // traversing the 4-level page table.
        let virt_addr = x86_64::VirtAddr::new(virt as u64);
        let phys_mem_offset = x86_64::VirtAddr::new(self.phys_mem_offset);

        // Read CR3 to get the level 4 page table physical address
        let (l4_frame, _) = x86_64::registers::control::Cr3::read();
        let l4_phys = l4_frame.start_address();
        let l4_virt = phys_mem_offset + l4_phys.as_u64();
        let l4_table = unsafe { &*(l4_virt.as_ptr() as *const x86_64::structures::paging::PageTable) };

        // Level 4 index
        let l4_idx = virt_addr.p4_index();
        let l4_entry = &l4_table[l4_idx];
        if l4_entry.is_unused() {
            log::error!("[virtio-net] virt_to_phys: L4 entry unused for {:#x}", virt);
            return None;
        }
        let l3_phys = l4_entry.addr();
        let l3_virt = phys_mem_offset + l3_phys.as_u64();
        let l3_table = unsafe { &*(l3_virt.as_ptr() as *const x86_64::structures::paging::PageTable) };

        // Level 3 index
        let l3_idx = virt_addr.p3_index();
        let l3_entry = &l3_table[l3_idx];
        if l3_entry.is_unused() {
            log::error!("[virtio-net] virt_to_phys: L3 entry unused for {:#x}", virt);
            return None;
        }
        // Check for 1GiB huge page
        if l3_entry.flags().contains(x86_64::structures::paging::PageTableFlags::HUGE_PAGE) {
            let base = l3_entry.addr().as_u64();
            let offset = virt as u64 & 0x3FFF_FFFF; // 30-bit offset within 1GiB page
            return Some(base + offset);
        }

        let l2_phys = l3_entry.addr();
        let l2_virt = phys_mem_offset + l2_phys.as_u64();
        let l2_table = unsafe { &*(l2_virt.as_ptr() as *const x86_64::structures::paging::PageTable) };

        // Level 2 index
        let l2_idx = virt_addr.p2_index();
        let l2_entry = &l2_table[l2_idx];
        if l2_entry.is_unused() {
            log::error!("[virtio-net] virt_to_phys: L2 entry unused for {:#x}", virt);
            return None;
        }
        // Check for 2MiB huge page
        if l2_entry.flags().contains(x86_64::structures::paging::PageTableFlags::HUGE_PAGE) {
            let base = l2_entry.addr().as_u64();
            let offset = virt as u64 & 0x1F_FFFF; // 21-bit offset within 2MiB page
            return Some(base + offset);
        }

        let l1_phys = l2_entry.addr();
        let l1_virt = phys_mem_offset + l1_phys.as_u64();
        let l1_table = unsafe { &*(l1_virt.as_ptr() as *const x86_64::structures::paging::PageTable) };

        // Level 1 index
        let l1_idx = virt_addr.p1_index();
        let l1_entry = &l1_table[l1_idx];
        if l1_entry.is_unused() {
            log::error!("[virtio-net] virt_to_phys: L1 entry unused for {:#x}", virt);
            return None;
        }
        let frame_phys = l1_entry.addr().as_u64();
        let page_offset = virt as u64 & 0xFFF; // 12-bit offset within 4KiB page
        Some(frame_phys + page_offset)
    }

    /// Physical address of the descriptor table.
    fn descs_phys(&self) -> u64 {
        self.virt_to_phys(self.descs as usize)
            .unwrap_or_else(|| {
                log::error!("[virtio-net] descs_phys: failed to translate descriptor table address");
                0
            })
    }

    /// The legacy VirtIO address register receives the queue's physical page
    /// frame number -- the physical address of the descriptor table shifted
    /// right by 12 bits.
    fn pfn(&self) -> u32 {
        (self.descs_phys() >> 12) as u32
    }

    /// Allocate one descriptor index from the free list. Returns `None` if
    /// the queue is full.
    fn alloc_desc(&mut self) -> Option<u16> {
        if self.num_free == 0 {
            return None;
        }
        let idx = self.free_head;
        let desc = unsafe { &*self.descs.add(idx as usize) };
        self.free_head = desc.next;
        self.num_free -= 1;
        Some(idx)
    }

    /// Return a descriptor to the free list.
    fn free_desc(&mut self, idx: u16) {
        unsafe {
            let desc = &mut *self.descs.add(idx as usize);
            desc.flags = 0;
            desc.next = self.free_head;
        }
        self.free_head = idx;
        self.num_free += 1;
    }

    /// Push a descriptor head into the available ring so the device can see
    /// it.
    fn push_avail(&mut self, desc_idx: u16) {
        unsafe {
            let avail = &mut *self.avail;
            let ring_idx = (avail.idx as usize) % (self.queue_size as usize);
            avail.ring[ring_idx] = desc_idx;
            // Memory barrier: make sure descriptor writes are visible before
            // we update the index.
            atomic::fence(Ordering::Release);
            avail.idx = avail.idx.wrapping_add(1);
        }
    }

    /// Check whether the device has completed any descriptors since our last
    /// check. Returns `Some((descriptor_index, bytes_written))`.
    fn pop_used(&mut self) -> Option<(u16, u32)> {
        atomic::fence(Ordering::Acquire);
        let used = unsafe { &*self.used };
        if self.last_used_idx == used.idx {
            return None;
        }
        let ring_idx = (self.last_used_idx as usize) % (self.queue_size as usize);
        let elem = used.ring[ring_idx];
        self.last_used_idx = self.last_used_idx.wrapping_add(1);
        Some((elem.id as u16, elem.len))
    }
}

// ---------------------------------------------------------------------------
// Legacy contiguous layout allocation
// ---------------------------------------------------------------------------

/// Allocate the legacy virtqueue layout as a single contiguous region.
///
/// Legacy VirtIO 0.9.5 spec layout:
///   - Descriptor table: 16 * queue_size bytes (16-byte aligned)
///   - Available ring:   6 + 2 * queue_size bytes (2-byte aligned)
///   - Padding to next page boundary
///   - Used ring:        6 + 8 * queue_size bytes (4-byte aligned)
fn alloc_legacy_virtqueue(queue_size: u16, phys_mem_offset: u64) -> Option<VirtQueue> {
    let qs = queue_size as usize;

    let desc_size = 16 * qs;
    let avail_size = 6 + 2 * qs;
    let avail_end = desc_size + avail_size;
    // Pad to next 4096-byte page boundary
    let used_offset = (avail_end + 4095) & !4095;
    let used_size = 6 + 8 * qs;
    let total_size = used_offset + used_size;

    // Allocate page-aligned memory
    let layout = match alloc::alloc::Layout::from_size_align(total_size, 4096) {
        Ok(l) => l,
        Err(_) => {
            log::error!("[virtio-net] invalid virtqueue layout (size={}, align=4096)", total_size);
            return None;
        }
    };
    let base = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if base.is_null() {
        log::error!("[virtio-net] failed to allocate virtqueue memory ({} bytes)", total_size);
        return None;
    }

    let descs = base as *mut VirtqDesc;
    let avail = unsafe { base.add(desc_size) } as *mut VirtqAvail;
    let used = unsafe { base.add(used_offset) } as *mut VirtqUsed;

    // Chain free descriptors: each points to next, last has no NEXT flag.
    for i in 0..qs {
        unsafe {
            let desc = &mut *descs.add(i);
            desc.addr = 0;
            desc.len = 0;
            if i + 1 < qs {
                desc.flags = VIRTQ_DESC_F_NEXT;
                desc.next = (i + 1) as u16;
            } else {
                desc.flags = 0;
                desc.next = 0;
            }
        }
    }

    // Pre-allocate buffers
    let mut buffers = Vec::with_capacity(qs);
    for _ in 0..qs {
        buffers.push(Box::new([0u8; BUF_SIZE]));
    }

    Some(VirtQueue {
        base,
        descs,
        avail,
        used,
        queue_size,
        free_head: 0,
        num_free: queue_size,
        last_used_idx: 0,
        buffers,
        phys_mem_offset,
    })
}

// ---------------------------------------------------------------------------
// Port I/O helpers
// ---------------------------------------------------------------------------

unsafe fn port_read_u8(base: u16, offset: u16) -> u8 {
    unsafe { Port::<u8>::new(base + offset).read() }
}

unsafe fn port_read_u16(base: u16, offset: u16) -> u16 {
    unsafe { Port::<u16>::new(base + offset).read() }
}

unsafe fn port_read_u32(base: u16, offset: u16) -> u32 {
    unsafe { Port::<u32>::new(base + offset).read() }
}

unsafe fn port_write_u8(base: u16, offset: u16, val: u8) {
    unsafe { Port::<u8>::new(base + offset).write(val) }
}

unsafe fn port_write_u16(base: u16, offset: u16, val: u16) {
    unsafe { Port::<u16>::new(base + offset).write(val) }
}

unsafe fn port_write_u32(base: u16, offset: u16, val: u32) {
    unsafe { Port::<u32>::new(base + offset).write(val) }
}

// ---------------------------------------------------------------------------
// VirtioNet — the NIC driver
// ---------------------------------------------------------------------------

/// VirtIO-net driver using legacy (0.9.5) PCI transport.
///
/// Created by [`VirtioNet::new`] after PCI enumeration has discovered the
/// device and read its BAR0 I/O base address.
pub struct VirtioNet {
    io_base: u16,
    mac: [u8; 6],
    rx_queue: VirtQueue,
    tx_queue: VirtQueue,
}

// SAFETY: VirtQueue contains raw pointers to heap memory that is only accessed
// through &mut self methods. The driver is designed to be owned by a single
// task (the network stack poller). The raw pointers are stable heap allocations
// that won't be moved.
unsafe impl Send for VirtioNet {}

/// Errors from driver initialization.
#[derive(Debug)]
pub enum VirtioInitError {
    /// The device did not report a usable queue size.
    InvalidQueueSize,
    /// The device rejected our feature negotiation.
    FeatureNegotiationFailed,
    /// Generic device error during init.
    DeviceError,
}

impl VirtioNet {
    /// Initialize the VirtIO-net device at the given I/O base.
    ///
    /// # Arguments
    /// * `io_base` -- I/O port base from PCI BAR0 (masked to I/O space).
    /// * `phys_mem_offset` -- the bootloader-provided physical memory offset
    ///   for virtual-to-physical address translation.
    ///
    /// # Safety
    /// The caller must ensure `io_base` points to a valid VirtIO legacy device
    /// and that `phys_mem_offset` is correct. PCI bus mastering must be enabled
    /// before calling this.
    pub unsafe fn new(
        io_base: u16,
        phys_mem_offset: u64,
    ) -> Result<Self, VirtioInitError> {
        log::info!("[virtio-net] initializing device at I/O base {:#x}", io_base);

        // -- Step 1: Reset --
        unsafe { port_write_u8(io_base, VIRTIO_DEVICE_STATUS, 0) };
        log::debug!("[virtio-net] device reset");

        // -- Step 2: Acknowledge --
        unsafe { port_write_u8(io_base, VIRTIO_DEVICE_STATUS, VIRTIO_STATUS_ACK) };

        // -- Step 3: Driver --
        unsafe {
            port_write_u8(
                io_base,
                VIRTIO_DEVICE_STATUS,
                VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER,
            )
        };

        // -- Step 4: Feature negotiation --
        let device_features = unsafe { port_read_u32(io_base, VIRTIO_DEVICE_FEATURES) };
        log::debug!("[virtio-net] device features: {:#010x}", device_features);

        // We request MAC address and link status features. We explicitly do
        // NOT request VIRTIO_NET_F_MRG_RXBUF so the header stays at 10 bytes.
        let guest_features = device_features & (VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS);
        unsafe { port_write_u32(io_base, VIRTIO_GUEST_FEATURES, guest_features) };
        log::debug!("[virtio-net] guest features: {:#010x}", guest_features);

        // Legacy devices don't require FEATURES_OK, but set it anyway for
        // transitional devices that support the modern interface.
        unsafe {
            port_write_u8(
                io_base,
                VIRTIO_DEVICE_STATUS,
                VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
            )
        };

        // Verify FEATURES_OK was accepted (only meaningful for modern devices,
        // but harmless to check).
        let status = unsafe { port_read_u8(io_base, VIRTIO_DEVICE_STATUS) };
        if status & VIRTIO_STATUS_FEATURES_OK == 0 {
            log::warn!("[virtio-net] FEATURES_OK not set (legacy device), continuing");
            // This is expected for pure legacy devices; not an error.
        }

        // -- Step 5: Read MAC address --
        let mut mac = [0u8; 6];
        if device_features & VIRTIO_NET_F_MAC != 0 {
            for i in 0..6 {
                mac[i] = unsafe { port_read_u8(io_base, VIRTIO_MAC_BASE + i as u16) };
            }
        } else {
            // Device doesn't advertise MAC feature. Use a locally-administered
            // fallback MAC for QEMU SLIRP (which assigns one anyway).
            mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
            log::warn!("[virtio-net] no MAC feature, using fallback");
        }
        log::info!(
            "[virtio-net] MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );

        // -- Step 6: Set up RX queue (index 0) --
        unsafe { port_write_u16(io_base, VIRTIO_QUEUE_SELECT, 0) };
        let rx_size = unsafe { port_read_u16(io_base, VIRTIO_QUEUE_SIZE) };
        log::debug!("[virtio-net] RX queue size: {}", rx_size);
        if rx_size == 0 {
            return Err(VirtioInitError::InvalidQueueSize);
        }
        let rx_size = rx_size.min(QUEUE_SIZE as u16);

        let mut rx_queue = alloc_legacy_virtqueue(rx_size, phys_mem_offset);

        // Write the physical page frame number of the descriptor table.
        let rx_pfn = rx_queue.pfn();
        unsafe { port_write_u32(io_base, VIRTIO_QUEUE_ADDR, rx_pfn) };
        log::debug!(
            "[virtio-net] RX queue PFN: {:#x} (phys: {:#x})",
            rx_pfn,
            (rx_pfn as u64) << 12
        );

        // -- Step 7: Set up TX queue (index 1) --
        unsafe { port_write_u16(io_base, VIRTIO_QUEUE_SELECT, 1) };
        let tx_size = unsafe { port_read_u16(io_base, VIRTIO_QUEUE_SIZE) };
        log::debug!("[virtio-net] TX queue size: {}", tx_size);
        if tx_size == 0 {
            return Err(VirtioInitError::InvalidQueueSize);
        }
        let tx_size = tx_size.min(QUEUE_SIZE as u16);

        let tx_queue = alloc_legacy_virtqueue(tx_size, phys_mem_offset);

        let tx_pfn = tx_queue.pfn();
        unsafe { port_write_u32(io_base, VIRTIO_QUEUE_ADDR, tx_pfn) };
        log::debug!(
            "[virtio-net] TX queue PFN: {:#x} (phys: {:#x})",
            tx_pfn,
            (tx_pfn as u64) << 12
        );

        // -- Step 8: Pre-populate RX queue with receive buffers --
        Self::populate_rx_queue(&mut rx_queue);

        // -- Step 9: Set DRIVER_OK — device is live --
        unsafe {
            port_write_u8(
                io_base,
                VIRTIO_DEVICE_STATUS,
                VIRTIO_STATUS_ACK
                    | VIRTIO_STATUS_DRIVER
                    | VIRTIO_STATUS_FEATURES_OK
                    | VIRTIO_STATUS_DRIVER_OK,
            )
        };

        // Read back status to verify device accepted initialization
        let final_status = unsafe { port_read_u8(io_base, VIRTIO_DEVICE_STATUS) };
        if final_status & VIRTIO_STATUS_DRIVER_OK == 0 {
            log::error!(
                "[virtio-net] device did not accept DRIVER_OK, status: {:#x}",
                final_status
            );
            return Err(VirtioInitError::DeviceError);
        }

        log::info!("[virtio-net] device initialized successfully (status: {:#x})", final_status);

        Ok(Self {
            io_base,
            mac,
            rx_queue,
            tx_queue,
        })
    }

    /// Fill every free RX descriptor with a device-writable buffer so the NIC
    /// can DMA received frames into them.
    fn populate_rx_queue(rx_queue: &mut VirtQueue) {
        let mut populated = 0u16;
        loop {
            let idx = match rx_queue.alloc_desc() {
                Some(i) => i,
                None => break,
            };

            // Point the descriptor at the buffer's physical address.
            let buf_ptr = rx_queue.buffers[idx as usize].as_ptr() as usize;
            let buf_phys = match rx_queue.virt_to_phys(buf_ptr) {
                Some(p) => p,
                None => {
                    log::error!("[virtio-net] populate_rx: failed to translate buffer {}", idx);
                    rx_queue.free_desc(idx);
                    continue;
                }
            };

            unsafe {
                let desc = &mut *rx_queue.descs.add(idx as usize);
                desc.addr = buf_phys;
                desc.len = BUF_SIZE as u32;
                desc.flags = VIRTQ_DESC_F_WRITE; // device writes to this buffer
                desc.next = 0;
            }

            rx_queue.push_avail(idx);
            populated += 1;
        }

        log::debug!("[virtio-net] populated {} RX descriptors", populated);
    }

    /// Acknowledge an interrupt by reading the ISR status register.
    ///
    /// Returns the ISR bits (bit 0 = used-buffer notification, bit 1 = config
    /// change). Reading ISR automatically clears it.
    pub fn ack_interrupt(&self) -> u8 {
        unsafe { port_read_u8(self.io_base, VIRTIO_ISR_STATUS) }
    }

    /// Reclaim completed TX descriptors so they can be reused.
    /// Returns the number of descriptors reclaimed.
    fn reclaim_tx(&mut self) -> usize {
        let mut count = 0;
        while let Some((desc_idx, _len)) = self.tx_queue.pop_used() {
            self.tx_queue.free_desc(desc_idx);
            count += 1;
        }
        count
    }

    /// Notify the device that new buffers are available on a queue.
    fn notify_queue(&self, queue_idx: u16) {
        unsafe { port_write_u16(self.io_base, VIRTIO_QUEUE_NOTIFY, queue_idx) };
    }
}

impl NicDriver for VirtioNet {
    fn transmit(&mut self, frame: &[u8]) -> Result<(), NicError> {
        // Reclaim any completed TX descriptors first.
        let reclaimed = self.reclaim_tx();
        if reclaimed > 0 {
            log::trace!("[virtio-net] TX: reclaimed {} descriptors", reclaimed);
        }

        let desc_idx = self.tx_queue.alloc_desc().ok_or_else(|| {
            log::warn!("[virtio-net] TX: no free descriptors (all {} in use)", self.tx_queue.queue_size);
            NicError::BufferFull
        })?;

        // Build the buffer: VirtIO-net header (10 bytes) + Ethernet frame.
        let buf = &mut *self.tx_queue.buffers[desc_idx as usize];
        if VIRTIO_NET_HDR_SIZE + frame.len() > BUF_SIZE {
            self.tx_queue.free_desc(desc_idx);
            return Err(NicError::BufferFull);
        }

        // Write the virtio-net header (all zeros = no offload).
        let hdr = VirtioNetHdr::zeroed();
        unsafe {
            ptr::copy_nonoverlapping(
                &hdr as *const VirtioNetHdr as *const u8,
                buf.as_mut_ptr(),
                VIRTIO_NET_HDR_SIZE,
            );
        }
        // Write the Ethernet frame after the header.
        buf[VIRTIO_NET_HDR_SIZE..VIRTIO_NET_HDR_SIZE + frame.len()].copy_from_slice(frame);

        // Set up the descriptor with the physical address.
        let buf_ptr = buf.as_ptr() as usize;
        let buf_phys = match self.tx_queue.virt_to_phys(buf_ptr) {
            Some(p) => p,
            None => {
                log::error!("[virtio-net] send: failed to translate TX buffer address");
                self.tx_queue.free_desc(desc_idx);
                return Err(NicError::DeviceError);
            }
        };
        unsafe {
            let desc = &mut *self.tx_queue.descs.add(desc_idx as usize);
            desc.addr = buf_phys;
            desc.len = (VIRTIO_NET_HDR_SIZE + frame.len()) as u32;
            desc.flags = 0; // device reads (no WRITE flag)
            desc.next = 0;
        }

        // Make it available and kick the device.
        self.tx_queue.push_avail(desc_idx);
        self.notify_queue(1); // queue 1 = TX

        log::trace!(
            "[virtio-net] TX: queued {} byte frame (desc {}, phys {:#x}, free: {})",
            frame.len(), desc_idx, buf_phys, self.tx_queue.num_free
        );

        Ok(())
    }

    fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, NicError> {
        // Check for completed RX descriptors.
        let (desc_idx, total_len) = match self.rx_queue.pop_used() {
            Some(v) => v,
            None => return Ok(None),
        };

        let total_len = total_len as usize;

        // The device wrote VirtIO-net header + Ethernet frame into the buffer.
        if total_len <= VIRTIO_NET_HDR_SIZE {
            // Runt or header-only -- recycle the buffer and ignore.
            log::trace!("[virtio-net] RX: runt frame ({} bytes), recycling", total_len);
            self.recycle_rx_desc(desc_idx);
            return Ok(None);
        }

        let frame_len = total_len - VIRTIO_NET_HDR_SIZE;
        let buf = &self.rx_queue.buffers[desc_idx as usize];

        if frame_len > out.len() {
            // Caller's buffer is too small -- drop the frame.
            log::warn!("[virtio-net] RX: frame too large ({} > {}), dropping", frame_len, out.len());
            self.recycle_rx_desc(desc_idx);
            return Err(NicError::BufferFull);
        }

        out[..frame_len]
            .copy_from_slice(&buf[VIRTIO_NET_HDR_SIZE..VIRTIO_NET_HDR_SIZE + frame_len]);

        // Recycle the descriptor back into the RX ring.
        self.recycle_rx_desc(desc_idx);

        log::trace!(
            "[virtio-net] RX: received {} byte frame (desc {})",
            frame_len, desc_idx
        );

        Ok(Some(frame_len))
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}

impl VirtioNet {
    /// Recycle a received descriptor back into the RX available ring.
    fn recycle_rx_desc(&mut self, desc_idx: u16) {
        let buf_ptr = self.rx_queue.buffers[desc_idx as usize].as_ptr() as usize;
        let buf_phys = match self.rx_queue.virt_to_phys(buf_ptr) {
            Some(p) => p,
            None => {
                log::error!("[virtio-net] recycle_rx: failed to translate buffer {} address", desc_idx);
                return;
            }
        };

        unsafe {
            let desc = &mut *self.rx_queue.descs.add(desc_idx as usize);
            desc.addr = buf_phys;
            desc.len = BUF_SIZE as u32;
            desc.flags = VIRTQ_DESC_F_WRITE;
            desc.next = 0;
        }

        self.rx_queue.push_avail(desc_idx);
        // Notify the device that new RX buffers are available.
        self.notify_queue(0); // queue 0 = RX
    }
}
