//! PCI detection for Intel WiFi adapters.
//!
//! All supported adapters are PCI vendor `0x8086` (Intel). Device IDs are
//! mapped to [`WifiVariant`] for family-specific initialization paths.

use crate::WifiVariant;

/// PCI vendor ID for Intel Corporation.
pub const INTEL_VENDOR_ID: u16 = 0x8086;

// -------------------------------------------------------------------
// Device IDs — sourced from Linux iwlwifi/pcie/drv.c
// -------------------------------------------------------------------

// Intel Wireless-AC 9260
const AC9260_DEVICE_IDS: &[u16] = &[
    0x2526, // Wireless-AC 9260 (generic)
    0x06F0, // 9260/9560 CNVi variant
    0x0A10, // 9260/9560 CNVi variant
];

// Intel Wi-Fi 6 AX200
const AX200_DEVICE_IDS: &[u16] = &[
    0x2723, // AX200 (discrete PCIe)
    0x2720, // AX200 variant
];

// Intel Wi-Fi 6 AX201 (CNVi — Comet Lake / Ice Lake / Tiger Lake)
const AX201_DEVICE_IDS: &[u16] = &[
    0x06F0, // AX201 CNVi (Comet Lake)
    0xA0F0, // AX201 CNVi (Ice Lake)
    0x02F0, // AX201 CNVi variant
    0x34F0, // AX201 CNVi (Ice Lake-LP)
    0x4DF0, // AX201 CNVi (Jasper Lake)
    0x43F0, // AX201 CNVi (Tiger Lake-H)
    0xA0F4, // AX201 CNVi variant
    0x54F0, // AX201 CNVi (Alder Lake-P)
    0x51F0, // AX201 CNVi (Raptor Lake)
    0x7AF0, // AX201 CNVi (Alder Lake-S)
];

// Intel Wi-Fi 6E AX210
const AX210_DEVICE_IDS: &[u16] = &[
    0x2725, // AX210 (discrete PCIe)
    0x2726, // AX210 variant
];

/// Result of probing a PCI bus/device/function for an Intel WiFi adapter.
#[derive(Debug, Clone)]
pub struct WifiDevice {
    /// PCI bus number.
    pub bus: u8,
    /// PCI device number (0..31).
    pub device: u8,
    /// PCI function number (0..7).
    pub function: u8,
    /// PCI device ID.
    pub device_id: u16,
    /// Identified WiFi variant.
    pub variant: WifiVariant,
    /// BAR0 base address (MMIO).
    pub bar0: u64,
    /// Interrupt line from PCI config space.
    pub irq_line: u8,
}

/// Identify an Intel WiFi variant from PCI vendor and device IDs.
///
/// Returns `None` if the device is not a recognized Intel WiFi adapter.
///
/// Note: device ID `0x06F0` appears in both AC9260 and AX201 tables. In
/// practice the subsystem ID disambiguates. We default to AX201 since it is
/// far more common on modern hardware. If disambiguation is needed, pass the
/// subsystem device ID and compare against known values.
pub fn identify(vendor: u16, device: u16) -> Option<WifiVariant> {
    if vendor != INTEL_VENDOR_ID {
        return None;
    }

    // Check AX201 first (most common on real hardware we target).
    if AX201_DEVICE_IDS.contains(&device) {
        log::info!("wifi::pci: matched device 0x{:04X} as AX201", device);
        return Some(WifiVariant::AX201);
    }
    if AX200_DEVICE_IDS.contains(&device) {
        log::info!("wifi::pci: matched device 0x{:04X} as AX200", device);
        return Some(WifiVariant::AX200);
    }
    if AX210_DEVICE_IDS.contains(&device) {
        log::info!("wifi::pci: matched device 0x{:04X} as AX210", device);
        return Some(WifiVariant::AX210);
    }
    if AC9260_DEVICE_IDS.contains(&device) {
        log::info!("wifi::pci: matched device 0x{:04X} as AC9260", device);
        return Some(WifiVariant::AC9260);
    }

    log::trace!("wifi::pci: Intel device 0x{:04X} is not a known WiFi adapter", device);
    None
}

/// Scan the PCI bus for Intel WiFi devices.
///
/// # Safety
///
/// Caller must ensure PCI config space I/O ports (0xCF8/0xCFC) are accessible
/// and no concurrent PCI access is occurring.
pub unsafe fn scan_pci_bus() -> Option<WifiDevice> {
    log::info!("wifi::pci: scanning PCI bus for Intel WiFi adapters...");

    for bus in 0u8..=255 {
        for dev in 0u8..32 {
            for func in 0u8..8 {
                let vendor = pci_read_u16(bus, dev, func, 0x00);
                if vendor == 0xFFFF || vendor != INTEL_VENDOR_ID {
                    if func == 0 {
                        break; // no device at this slot
                    }
                    continue;
                }

                let device_id = pci_read_u16(bus, dev, func, 0x02);
                if let Some(variant) = identify(vendor, device_id) {
                    let bar0_low = pci_read_u32(bus, dev, func, 0x10);
                    let bar0_high = pci_read_u32(bus, dev, func, 0x14);
                    let bar0 = ((bar0_high as u64) << 32) | ((bar0_low & 0xFFFF_FFF0) as u64);
                    let irq_line = pci_read_u8(bus, dev, func, 0x3C);

                    // Enable bus mastering + memory space access
                    let cmd = pci_read_u16(bus, dev, func, 0x04);
                    pci_write_u16(bus, dev, func, 0x04, cmd | 0x06);

                    log::info!(
                        "wifi::pci: found {} at {:02X}:{:02X}.{} BAR0=0x{:016X} IRQ={}",
                        variant.name(), bus, dev, func, bar0, irq_line
                    );

                    return Some(WifiDevice {
                        bus,
                        device: dev,
                        function: func,
                        device_id,
                        variant,
                        bar0,
                        irq_line,
                    });
                }

                // If function 0 is not multi-function, skip functions 1..7
                if func == 0 {
                    let header_type = pci_read_u8(bus, dev, func, 0x0E);
                    if header_type & 0x80 == 0 {
                        break;
                    }
                }
            }
        }
    }

    log::warn!("wifi::pci: no Intel WiFi adapter found on PCI bus");
    None
}

// -------------------------------------------------------------------
// Raw PCI config space I/O
// -------------------------------------------------------------------

unsafe fn pci_config_addr(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
}

unsafe fn pci_read_u32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr = pci_config_addr(bus, dev, func, offset);
    core::arch::asm!("out dx, eax", in("dx") 0xCF8u16, in("eax") addr, options(nomem, nostack));
    let val: u32;
    core::arch::asm!("in eax, dx", in("dx") 0xCFCu16, out("eax") val, options(nomem, nostack));
    val
}

unsafe fn pci_read_u16(bus: u8, dev: u8, func: u8, offset: u8) -> u16 {
    let dword = pci_read_u32(bus, dev, func, offset & 0xFC);
    ((dword >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

unsafe fn pci_read_u8(bus: u8, dev: u8, func: u8, offset: u8) -> u8 {
    let dword = pci_read_u32(bus, dev, func, offset & 0xFC);
    ((dword >> ((offset & 3) * 8)) & 0xFF) as u8
}

unsafe fn pci_write_u16(bus: u8, dev: u8, func: u8, offset: u8, value: u16) {
    let mut dword = pci_read_u32(bus, dev, func, offset & 0xFC);
    let shift = (offset & 2) * 8;
    dword &= !(0xFFFF << shift);
    dword |= (value as u32) << shift;
    let addr = pci_config_addr(bus, dev, func, offset);
    core::arch::asm!("out dx, eax", in("dx") 0xCF8u16, in("eax") addr, options(nomem, nostack));
    core::arch::asm!("out dx, eax", in("dx") 0xCFCu16, in("eax") dword, options(nomem, nostack));
}
