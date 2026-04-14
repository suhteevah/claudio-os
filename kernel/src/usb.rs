//! USB subsystem — xHCI host controller detection and USB keyboard integration.
//!
//! Detects an xHCI controller via PCI (class 0x0C, subclass 0x03, prog-if 0x30),
//! initializes the `XhciController`, enumerates USB devices, and provides a
//! polling function that feeds USB keyboard events into the existing PS/2
//! scancode queue (`keyboard::push_scancode`).
//!
//! Since we don't have MSI-X interrupt routing yet, the USB keyboard is polled
//! periodically from the async executor via `poll_usb_keyboard()`.

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

/// Wrapper around `XhciController` to implement `Send`.
///
/// The raw pointers inside `XhciController` are MMIO and DMA addresses that
/// are valid for the entire lifetime of the OS (single address space, no
/// deallocation). We only access the controller through a `Mutex`, so
/// there is no concurrent access. This is safe in our single-address-space,
/// cooperative-multitasking kernel.
pub(crate) struct SendableXhci(pub(crate) claudio_xhci::XhciController);

// SAFETY: XhciController contains raw pointers to MMIO registers and DMA
// buffers that are permanently mapped and never freed. Access is serialised
// by the XHCI Mutex. In our single-address-space kernel there is no thread
// migration or preemption boundary that would make this unsound.
unsafe impl Send for SendableXhci {}

/// Global xHCI controller instance, initialised by `init()`.
pub(crate) static XHCI: Mutex<Option<SendableXhci>> = Mutex::new(None);

/// Discovered USB mass storage devices, populated during `init()`.
/// Each entry is `(slot_id, MassStorageInfo)`.
pub(crate) static MASS_STORAGE_DEVICES: Mutex<alloc::vec::Vec<(u8, claudio_xhci::MassStorageInfo)>> =
    Mutex::new(alloc::vec::Vec::new());

/// Whether a USB keyboard was detected during enumeration.
static USB_KEYBOARD_PRESENT: AtomicBool = AtomicBool::new(false);

/// Detect and initialise the xHCI host controller.
///
/// Looks for a PCI device with class 0x0C (Serial Bus Controller),
/// subclass 0x03 (USB Controller), prog-if 0x30 (xHCI).
///
/// If found, initialises the controller, enumerates USB ports, and checks
/// for a HID keyboard device.
///
/// This must be called after `pci::enumerate()` and after the heap is
/// available. Interrupts may be enabled or disabled.
pub fn init() {
    log::info!("[usb] looking for xHCI controller (class 0x0C/0x03/0x30)...");

    let pci_dev = match crate::pci::find_by_class(0x0C, 0x03, 0x30) {
        Some(dev) => dev,
        None => {
            log::info!("[usb] no xHCI controller found — USB keyboard support disabled");
            return;
        }
    };

    log::info!(
        "[usb] xHCI controller found: PCI {:02x}:{:02x}.{} vendor={:#06x} device={:#06x} BAR0={:#010x}",
        pci_dev.bus,
        pci_dev.device,
        pci_dev.function,
        pci_dev.vendor_id,
        pci_dev.device_id,
        pci_dev.bar0,
    );

    // Enable bus mastering for DMA (required for xHCI ring operations)
    crate::pci::enable_bus_master_for(pci_dev.bus, pci_dev.device, pci_dev.function);

    // Determine the MMIO base address from BAR0.
    //
    // PCI BAR encoding (per PCI Local Bus Spec 3.0, section 6.2.5.1):
    // - Bit 0: 0 = memory-mapped I/O, 1 = I/O port space
    // - Bits [2:1] for memory BARs: 00 = 32-bit, 10 = 64-bit
    //
    // For 64-bit BARs, the physical address spans BAR0 (low 32 bits, mask off
    // low 4 flag bits) and BAR1 (high 32 bits, at PCI config offset 0x14).
    // xHCI controllers almost always use 64-bit memory BARs because their
    // register spaces can be mapped above 4 GiB.
    let bar0_raw = pci_dev.bar0;
    if bar0_raw & 1 != 0 {
        log::error!("[usb] xHCI BAR0 is I/O-space ({:#x}), expected memory-mapped — aborting", bar0_raw);
        return;
    }

    let mmio_phys: u64 = if (bar0_raw >> 1) & 0x3 == 0x2 {
        // 64-bit BAR: read BAR1 (offset 0x14) for upper 32 bits
        let bar1 = crate::pci::read_config_pub(pci_dev.bus, pci_dev.device, pci_dev.function, 0x14);
        ((bar1 as u64) << 32) | ((bar0_raw & !0xF) as u64)
    } else {
        // 32-bit BAR
        (bar0_raw & !0xF) as u64
    };

    log::info!("[usb] xHCI MMIO physical address: {:#x}", mmio_phys);

    // Convert physical address to virtual address using the bootloader's
    // physical memory offset mapping.
    let phys_mem_offset = crate::PHYS_MEM_OFFSET.load(Ordering::Relaxed);
    let mmio_virt = phys_mem_offset + mmio_phys;
    log::info!("[usb] xHCI MMIO virtual address: {:#x}", mmio_virt);

    // Initialise the xHCI controller
    let mut controller = match unsafe { claudio_xhci::XhciController::init(mmio_virt as usize, crate::memory::virt_to_phys) } {
        Ok(c) => c,
        Err(e) => {
            log::error!("[usb] xHCI controller init failed: {:?}", e);
            return;
        }
    };

    // Enumerate ports to discover connected USB devices
    controller.enumerate_ports();

    if controller.has_keyboard() {
        log::info!("[usb] USB HID keyboard detected — events will be routed to PS/2 scancode queue");
        USB_KEYBOARD_PRESENT.store(true, Ordering::Relaxed);
    } else {
        log::info!("[usb] no USB keyboard found on any port");
    }

    // Check for USB mass storage devices
    let mass_storage = controller.mass_storage_devices();
    for (slot_id, info) in &mass_storage {
        log::info!(
            "[usb] mass storage device found: slot={} iface={} bulk_in_dci={} bulk_out_dci={}",
            slot_id, info.interface_num, info.bulk_in_dci, info.bulk_out_dci,
        );
    }
    if mass_storage.is_empty() {
        log::info!("[usb] no USB mass storage devices found");
    }
    *MASS_STORAGE_DEVICES.lock() = mass_storage;

    *XHCI.lock() = Some(SendableXhci(controller));
    log::info!("[usb] xHCI initialisation complete");
}

/// Poll the USB keyboard for new key events.
///
/// If a USB keyboard is present, this calls `XhciController::poll_keyboard()`
/// and converts each `KeyEvent` into a PS/2 scancode pushed to the existing
/// `keyboard::push_scancode()` queue. This way the dashboard's `ScancodeStream`
/// works identically for both PS/2 and USB keyboards.
///
/// For key presses, we push the make code (scancode).
/// For key releases, we push the break code (scancode | 0x80), matching PS/2
/// Scan Code Set 1 convention.
///
/// This should be called periodically from the async executor loop or a timer
/// task. It is non-blocking and returns quickly if no events are pending.
pub fn poll_usb_keyboard() {
    if !USB_KEYBOARD_PRESENT.load(Ordering::Relaxed) {
        return;
    }

    let mut guard = XHCI.lock();
    let controller = match guard.as_mut() {
        Some(c) => &mut c.0,
        None => return,
    };

    // Drain all pending keyboard events
    while let Some(event) = controller.poll_keyboard() {
        let scancode = event.scancode;
        if scancode == 0 {
            // Unmapped key — skip
            continue;
        }

        if event.pressed {
            // Make code: push the raw scancode
            crate::keyboard::push_scancode(scancode);
        } else {
            // Break code: PS/2 Set 1 uses scancode | 0x80 for key release
            crate::keyboard::push_scancode(scancode | 0x80);
        }
    }
}

/// Poll the USB mouse for new HID reports.
///
/// This is a stub awaiting xHCI crate support for mouse devices.
/// Once `XhciController` gains `has_mouse()`, `poll_mouse()`, and mouse
/// enumeration (HID class=3, subclass=1, protocol=2), this function will:
///
/// 1. Call `controller.poll_mouse()` to get raw boot protocol reports
/// 2. Feed each report to `crate::mouse::feed_report()`
/// 3. Call `crate::mouse::update_cursor()` if events were generated
///
/// For now this returns immediately. The mouse infrastructure in `mouse.rs`
/// is fully functional and can be exercised by calling `mouse::feed_report()`
/// with raw HID data from any source.
pub fn poll_usb_mouse() {
    if !crate::mouse::is_present() {
        return;
    }

    // TODO: Once the xHCI crate exposes mouse polling, add:
    //
    // let mut guard = XHCI.lock();
    // let controller = match guard.as_mut() {
    //     Some(c) => &mut c.0,
    //     None => return,
    // };
    //
    // while let Some(report) = controller.poll_mouse() {
    //     crate::mouse::feed_report(&report);
    // }
    // crate::mouse::update_cursor();
}
