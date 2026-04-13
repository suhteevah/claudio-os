//! Block device registry — owns AHCI/NVMe controllers instantiated at boot.
//!
//! The kernel's [`init`] function runs as boot phase 5d (after PCI enumeration
//! and SMP bring-up, before the stack-switch / executor). It walks PCI for
//! mass-storage controllers (class 0x01), calls the per-type `init`, and
//! stashes owned handles in a global registry. Downstream consumers
//! (swap partition scanner, dashboard `df`, VFS mount adapters) then borrow
//! the live controllers via [`with_disks`] / [`as_block_device`] without
//! re-initialising hardware.
//!
//! ## Stable-address invariant
//!
//! The [`AhciBlockDeviceAdapter`] / [`NvmeBlockDeviceAdapter`] types in
//! `claudio-vfs` take raw pointers to `AhciDisk` / `HbaRegs` / `NvmeController`
//! because those types are owned elsewhere and we need long-lived handles.
//!
//! This module upholds that invariant by:
//!
//! 1. Allocating each controller inside a `Box`, which gives a stable heap
//!    address that never moves.
//! 2. Storing the `Box`es in `Vec`s that are only ever **pushed to** during
//!    boot-time `init`. After `init` returns the vecs are effectively frozen;
//!    downstream consumers may only read from them. We never pop, swap_remove,
//!    or trigger reallocation-on-push by calling `push` post-init. (The boot-
//!    time init itself does call `push`, and because the elements are heap
//!    `Box`es the inner pointee never moves even if the outer `Vec` reallocates.)
//! 3. `AhciDisk` values live inside `AhciController.disks: Vec<AhciDisk>` —
//!    those are moved only during `AhciController::init`. After we `Box` the
//!    controller and store it here, the inner `Vec<AhciDisk>` is never pushed
//!    again, so its elements retain stable addresses.
//!
//! ## QEMU behaviour
//!
//! Under plain `qemu-system-x86_64 -device virtio-net-pci ...` (no `-drive`)
//! the guest has zero PCI mass-storage controllers. `init` finds nothing,
//! the registry stays empty, and downstream consumers report "no disks"
//! gracefully. Pass `-drive file=disk.img,if=none,id=d0 -device ahci,id=ahci0
//! -device ide-hd,drive=d0,bus=ahci0.0` (or a `-device nvme`) to exercise
//! the full path.

#![allow(dead_code)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use spin::{Mutex, Once};

use claudio_ahci::hba::HbaRegs;
use claudio_ahci::{AhciController, AhciError};
use claudio_nvme::{NvmeController, NvmeError};
use claudio_vfs::adapters::{AhciBlockDeviceAdapter, NvmeBlockDeviceAdapter};
use claudio_usb_storage::UsbStorageDevice;
use claudio_vfs::BlockDevice;

use crate::usb_storage::XhciBulkTransport;

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

/// Per-disk backend reference. Indices point into the owning `Registry`'s
/// vecs, not into external storage — the registry must stay alive for the
/// entire OS lifetime (it does; it's a `Once` singleton).
#[derive(Debug, Clone, Copy)]
enum DiskBackendRef {
    Ahci {
        /// Index into `Registry::ahci_controllers`.
        ctrl_idx: usize,
        /// Index into `Registry::ahci_hbas` (parallel array to ahci_controllers).
        hba_idx: usize,
        /// Which slot in `AhciController::disks` this disk occupies.
        disk_idx: usize,
    },
    Nvme {
        /// Index into `Registry::nvme_controllers`.
        ctrl_idx: usize,
        /// Namespace ID (1-based).
        nsid: u32,
    },
    Usb {
        /// Index into `Registry::usb_devices`.
        dev_idx: usize,
    },
}

/// A single disk visible to the kernel — one AHCI port *or* one NVMe namespace.
#[derive(Debug)]
pub struct DiskEntry {
    /// Short label, e.g. "ahci0", "nvme0n1".
    pub label: String,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub sector_size: u32,
    pub sector_count: u64,
    pub total_bytes: u64,
    /// Human-readable model string (from IDENTIFY / Identify Controller).
    pub model: String,
    backend: DiskBackendRef,
}

/// Registry state — owned controllers and the disks they expose.
struct Registry {
    /// Owned AHCI controllers. Each `Box` gives a stable address for the
    /// adapter's raw pointer.
    ahci_controllers: Vec<Box<AhciController>>,
    /// Parallel to `ahci_controllers`: a fresh `HbaRegs` handle (which is
    /// just a wrapper around a base MMIO address) for each controller.
    ///
    /// We duplicate the HbaRegs because `AhciController::hba` is private.
    /// Since `HbaRegs` only stores `{ base: usize }` pointing at MMIO, two
    /// handles to the same ABAR are equivalent — all reads/writes are
    /// volatile and the hardware doesn't care which handle is used.
    ahci_hbas: Vec<Box<HbaRegs>>,
    /// Owned NVMe controllers.
    nvme_controllers: Vec<Box<NvmeController>>,
    /// Owned USB mass storage devices. Each `Box` gives a stable heap
    /// address for the `BlockDevice` trait object returned by
    /// `as_block_device`. Only pushed during `init`, never modified after.
    usb_devices: Vec<Box<UsbStorageDevice<XhciBulkTransport>>>,
    /// All disks discovered across all controllers.
    disks: Vec<DiskEntry>,
}

impl Registry {
    const fn empty() -> Self {
        Self {
            ahci_controllers: Vec::new(),
            ahci_hbas: Vec::new(),
            nvme_controllers: Vec::new(),
            usb_devices: Vec::new(),
            disks: Vec::new(),
        }
    }
}

/// Global block-device registry. Populated exactly once during boot.
static REGISTRY: Once<Mutex<Registry>> = Once::new();

fn registry() -> &'static Mutex<Registry> {
    REGISTRY.call_once(|| Mutex::new(Registry::empty()))
}

// ---------------------------------------------------------------------------
// PCI enumeration helper
// ---------------------------------------------------------------------------

/// A raw PCI hit before we decide whether/how to instantiate it.
#[derive(Debug, Clone, Copy)]
struct StorageHit {
    bus: u8,
    device: u8,
    function: u8,
    vendor_id: u16,
    device_id: u16,
    subclass: u8,
    prog_if: u8,
    bar0: u32,
    /// BAR5 (ABAR) — only meaningful for AHCI subclass 0x06.
    bar5: u32,
}

fn scan_pci_storage() -> Vec<StorageHit> {
    // `find_by_predicate` is `Fn`-based so we collect via interior mutability.
    let hits: Mutex<Vec<StorageHit>> = Mutex::new(Vec::new());
    let _: Option<()> = crate::pci::find_by_predicate(|dev| {
        if dev.class == 0x01 {
            // Read BAR5 for AHCI ABAR. For NVMe we use bar0 from PciDevice.
            let bar5 = crate::pci::read_config_pub(
                dev.bus, dev.device, dev.function, 0x24,
            );
            hits.lock().push(StorageHit {
                bus: dev.bus,
                device: dev.device,
                function: dev.function,
                vendor_id: dev.vendor_id,
                device_id: dev.device_id,
                subclass: dev.subclass,
                prog_if: dev.prog_if,
                bar0: dev.bar0,
                bar5,
            });
        }
        None::<()>
    });
    hits.into_inner()
}

// ---------------------------------------------------------------------------
// Boot-time initialisation
// ---------------------------------------------------------------------------

/// Walk PCI for mass-storage controllers and instantiate real driver handles.
///
/// Errors from any individual controller `init` are logged as warnings and
/// that device is skipped — one bad controller must never halt the boot.
pub fn init() {
    let hits = scan_pci_storage();
    log::info!(
        "[disks] scanning {} PCI mass-storage candidate(s)",
        hits.len(),
    );

    // Ensure the singleton exists and lock it for the whole of init.
    let reg_mutex = registry();
    let mut reg = reg_mutex.lock();

    let mut ahci_count = 0usize;
    let mut nvme_count = 0usize;

    for hit in hits.iter() {
        match hit.subclass {
            // ── AHCI (SATA) ──────────────────────────────────────────
            0x06 => {
                // Mask off the BAR type bits (lowest 4 bits) to get the
                // actual ABAR physical base address.
                let abar_phys = (hit.bar5 & 0xFFFF_FFF0) as u64;
                if abar_phys == 0 {
                    log::warn!(
                        "[disks] ahci {:02x}:{:02x}.{} has BAR5=0 — skipping",
                        hit.bus, hit.device, hit.function,
                    );
                    continue;
                }
                let phys_offset = crate::phys_mem_offset();
                let abar_virt = abar_phys + phys_offset;
                log::info!(
                    "[disks] initializing AHCI controller {:02x}:{:02x}.{} ABAR phys={:#x} virt={:#x}",
                    hit.bus, hit.device, hit.function, abar_phys, abar_virt,
                );
                // SAFETY: `abar_virt` = BAR5 physical + PHYS_MEM_OFFSET, and
                // the bootloader has mapped all physical memory into that
                // window, so dereferencing this address is sound. No other
                // code in the kernel touches this ABAR before us.
                //
                // `memory::virt_to_phys` walks CR3 page tables to translate
                // heap virtual addresses to physical for DMA registers.
                let ctrl_result = unsafe {
                    AhciController::init(abar_virt, crate::memory::virt_to_phys)
                };
                let ctrl = match ctrl_result {
                    Ok(c) => c,
                    Err(e) => {
                        log::warn!(
                            "[disks] AhciController::init {:02x}:{:02x}.{} failed: {:?}",
                            hit.bus, hit.device, hit.function, e,
                        );
                        continue;
                    }
                };

                // Box the controller so its address is stable.
                let ctrl_box: Box<AhciController> = Box::new(ctrl);
                // Create a *second* HbaRegs handle for the adapter. The
                // controller owns its own (private) handle; we can't share
                // that one, but both handles point at the same MMIO base
                // and MMIO access is inherently safe at the hardware level.
                //
                // SAFETY: same virtual ABAR and mapping invariants as above.
                let hba_box: Box<HbaRegs> =
                    Box::new(unsafe { HbaRegs::from_base_addr(abar_virt) });

                let ctrl_idx = reg.ahci_controllers.len();
                let hba_idx = reg.ahci_hbas.len();
                // ctrl_idx and hba_idx stay aligned: we always push to both
                // together, so ctrl_idx == hba_idx in practice. The two
                // indices are kept separate to document which field each
                // controls inside DiskBackendRef::Ahci.
                debug_assert_eq!(ctrl_idx, hba_idx);

                // Create one DiskEntry per detected AHCI disk before we
                // move the Box into the registry.
                let disk_metas: Vec<(usize, u32, u32, u64, String)> = ctrl_box
                    .disks
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        (
                            i,
                            d.port,
                            d.sector_size,
                            d.sector_count,
                            d.identify.model.clone(),
                        )
                    })
                    .collect();

                reg.ahci_controllers.push(ctrl_box);
                reg.ahci_hbas.push(hba_box);
                ahci_count += 1;

                for (disk_idx, port_num, sector_size, sector_count, model) in disk_metas {
                    let label = alloc::format!("ahci{}p{}", ctrl_idx, port_num);
                    let total_bytes = sector_count * sector_size as u64;
                    log::info!(
                        "[disks]   -> {}: {} ({} sectors x {} B = {} MiB)",
                        label, model, sector_count, sector_size,
                        total_bytes / (1024 * 1024),
                    );
                    reg.disks.push(DiskEntry {
                        label,
                        bus: hit.bus,
                        device: hit.device,
                        function: hit.function,
                        vendor_id: hit.vendor_id,
                        device_id: hit.device_id,
                        sector_size,
                        sector_count,
                        total_bytes,
                        model,
                        backend: DiskBackendRef::Ahci {
                            ctrl_idx,
                            hba_idx,
                            disk_idx,
                        },
                    });
                }
            }

            // ── NVMe ─────────────────────────────────────────────────
            0x08 => {
                let bar0_phys = (hit.bar0 & 0xFFFF_FFF0) as usize;
                if bar0_phys == 0 {
                    log::warn!(
                        "[disks] nvme {:02x}:{:02x}.{} has BAR0=0 — skipping",
                        hit.bus, hit.device, hit.function,
                    );
                    continue;
                }
                // TODO: Same issue as AHCI — NVMe is untested end-to-end
                // and almost certainly has the same virt→phys DMA
                // translation gap. See AHCI TODO above.
                log::warn!(
                    "[disks] skipping NVMe {:02x}:{:02x}.{} BAR0={:#x} — \
                     NVMe crate needs virt→phys DMA translation (TODO)",
                    hit.bus, hit.device, hit.function, bar0_phys,
                );
                continue;
                #[allow(unreachable_code)]
                let phys_offset = crate::phys_mem_offset() as usize;
                #[allow(unreachable_code)]
                let bar0_virt = bar0_phys + phys_offset;
                #[allow(unreachable_code)]
                log::info!(
                    "[disks] initializing NVMe controller {:02x}:{:02x}.{} BAR0 phys={:#x} virt={:#x}",
                    hit.bus, hit.device, hit.function, bar0_phys, bar0_virt,
                );
                // SAFETY: `bar0_virt` = BAR0 physical + PHYS_MEM_OFFSET,
                // which the bootloader maps for us. Dereferencing the
                // resulting address is sound.
                #[allow(unreachable_code)]
                let ctrl_result = unsafe { NvmeController::init(bar0_virt) };
                let mut ctrl = match ctrl_result {
                    Ok(c) => c,
                    Err(e @ NvmeError::NvmCssNotSupported) => {
                        log::warn!(
                            "[disks] NvmeController::init {:02x}:{:02x}.{}: {}",
                            hit.bus, hit.device, hit.function, e,
                        );
                        continue;
                    }
                    Err(e) => {
                        log::warn!(
                            "[disks] NvmeController::init {:02x}:{:02x}.{} failed: {}",
                            hit.bus, hit.device, hit.function, e,
                        );
                        continue;
                    }
                };

                // Probe namespace 1. If present, create a DiskEntry; otherwise
                // log and skip (controllers with no namespaces aren't useful).
                let num_ns = ctrl.identity.num_namespaces;
                let mut ns_meta: Vec<(u32, u32, u64, String)> = Vec::new();
                for nsid in 1..=num_ns.min(4) {
                    match ctrl.namespace(nsid) {
                        Ok(disk) => {
                            let model = ctrl.identity.model.clone();
                            ns_meta.push((nsid, disk.sector_size, disk.sector_count, model));
                        }
                        Err(e) => {
                            log::debug!(
                                "[disks]   nvme nsid={} not available: {}",
                                nsid, e,
                            );
                        }
                    }
                }

                if ns_meta.is_empty() {
                    log::warn!(
                        "[disks] nvme {:02x}:{:02x}.{}: no usable namespaces, dropping",
                        hit.bus, hit.device, hit.function,
                    );
                    continue;
                }

                let ctrl_idx = reg.nvme_controllers.len();
                reg.nvme_controllers.push(Box::new(ctrl));
                nvme_count += 1;

                for (nsid, sector_size, sector_count, model) in ns_meta {
                    let label = alloc::format!("nvme{}n{}", ctrl_idx, nsid);
                    let total_bytes = sector_count * sector_size as u64;
                    log::info!(
                        "[disks]   -> {}: {} ({} sectors x {} B = {} MiB)",
                        label, model, sector_count, sector_size,
                        total_bytes / (1024 * 1024),
                    );
                    reg.disks.push(DiskEntry {
                        label,
                        bus: hit.bus,
                        device: hit.device,
                        function: hit.function,
                        vendor_id: hit.vendor_id,
                        device_id: hit.device_id,
                        sector_size,
                        sector_count,
                        total_bytes,
                        model,
                        backend: DiskBackendRef::Nvme { ctrl_idx, nsid },
                    });
                }
            }

            // ── Unsupported subclass ─────────────────────────────────
            other => {
                log::info!(
                    "[disks] skipping unsupported mass-storage subclass {:#04x} at {:02x}:{:02x}.{}",
                    other, hit.bus, hit.device, hit.function,
                );
            }
        }
    }

    // ── USB Mass Storage ─────────────────────────────────────────────
    //
    // After xHCI enumeration (usb::init), MASS_STORAGE_DEVICES holds
    // (slot_id, MassStorageInfo) tuples for every BOT device found on
    // the bus. We create a XhciBulkTransport adapter for each, feed it
    // into UsbStorageDevice::init (INQUIRY + TEST UNIT READY + READ
    // CAPACITY + MODE SENSE), and register the resulting block device.
    let usb_ms = crate::usb::MASS_STORAGE_DEVICES.lock().clone();
    let mut usb_count = 0usize;
    for (slot_id, info) in usb_ms {
        log::info!(
            "[disks] initializing USB mass storage: slot={} if={} bulk_in_dci={} bulk_out_dci={}",
            slot_id, info.interface_num, info.bulk_in_dci, info.bulk_out_dci,
        );

        let transport = XhciBulkTransport::new(slot_id, info);
        let mut device = UsbStorageDevice::new(transport, 0);

        match device.init() {
            Ok(()) => {
                let sector_size = device.sector_size();
                let total_size = device.total_size();
                let sector_count = if sector_size > 0 {
                    total_size / sector_size as u64
                } else {
                    0
                };
                let model = device
                    .inquiry()
                    .map(|inq| {
                        alloc::format!("{} {}", inq.vendor, inq.product)
                    })
                    .unwrap_or_else(|| alloc::format!("USB slot {}", slot_id));

                let dev_idx = reg.usb_devices.len();
                let label = alloc::format!("usb{}", dev_idx);
                let total_bytes = total_size;

                log::info!(
                    "[disks]   -> {}: {} ({} sectors x {} B = {} MiB)",
                    label, model, sector_count, sector_size,
                    total_bytes / (1024 * 1024),
                );

                reg.usb_devices.push(Box::new(device));
                usb_count += 1;

                reg.disks.push(DiskEntry {
                    label,
                    bus: 0,
                    device: slot_id,
                    function: 0,
                    vendor_id: 0,
                    device_id: 0,
                    sector_size,
                    sector_count,
                    total_bytes,
                    model,
                    backend: DiskBackendRef::Usb { dev_idx },
                });
            }
            Err(e) => {
                log::error!(
                    "[disks] USB mass storage init failed for slot {}: {:?}",
                    slot_id, e,
                );
            }
        }
    }

    let total_disks = reg.disks.len();
    log::info!(
        "[boot] disks: initialized {} controller(s) ({} AHCI + {} NVMe + {} USB), {} disk(s) total",
        ahci_count + nvme_count + usb_count, ahci_count, nvme_count, usb_count, total_disks,
    );

    // Suppress unused-variable warnings if the AHCI error type grows new
    // variants in the future. This also doubles as a compile-time reminder
    // that AhciError::* values are intentionally discarded as warnings.
    let _ = core::marker::PhantomData::<AhciError>;
}

// ---------------------------------------------------------------------------
// Public accessors
// ---------------------------------------------------------------------------

/// Run `f` with an immutable view of all registered disks.
///
/// Returns the closure's result. The registry mutex is held for the duration
/// of `f`, so avoid doing long-running I/O inside the closure.
pub fn with_disks<F, R>(f: F) -> R
where
    F: FnOnce(&[DiskEntry]) -> R,
{
    let reg = registry().lock();
    f(&reg.disks)
}

/// Run `f` with a mutable view of all registered disks.
///
/// Currently only used by boot-time code; downstream consumers should use
/// [`with_disks`] instead. Exposed for completeness.
pub fn with_disks_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut [DiskEntry]) -> R,
{
    let mut reg = registry().lock();
    f(&mut reg.disks)
}

/// Return the number of disks in the registry.
pub fn len() -> usize {
    registry().lock().disks.len()
}

/// Construct a `BlockDevice` trait object for disk at `index`.
///
/// Returns `None` if `index` is out of range. The returned adapter holds
/// raw pointers into the registry's owned controllers; those pointers are
/// valid for the lifetime of the program (see the module doc's
/// "Stable-address invariant"). The adapter is safe to pass to
/// `claudio_vfs::device::parse_gpt`, mount into the VFS, etc.
pub fn as_block_device(index: usize) -> Option<Box<dyn BlockDevice + Send + Sync>> {
    let reg = registry().lock();
    let entry = reg.disks.get(index)?;
    match entry.backend {
        DiskBackendRef::Ahci { ctrl_idx, hba_idx, disk_idx } => {
            let ctrl_box = reg.ahci_controllers.get(ctrl_idx)?;
            let hba_box = reg.ahci_hbas.get(hba_idx)?;
            // Raw-pointer casts from stable Box allocations.
            //
            // `ctrl_box.disks[disk_idx]` lives inside the boxed controller's
            // `Vec<AhciDisk>`. Because no code mutates `disks` post-init
            // (we never push after `init` returns), the element address is
            // stable for program lifetime.
            let disk_ref: &claudio_ahci::AhciDisk = ctrl_box.disks.get(disk_idx)?;
            // Cast away the shared borrow into a raw pointer. The adapter
            // needs `*mut AhciDisk` because `AhciDisk::read_sectors` etc.
            // take `&self` but the adapter type is parameterised as mut
            // for uniformity with NVMe. All real writes go through volatile
            // MMIO so aliasing is tolerated at the hardware level.
            let disk_ptr = disk_ref as *const claudio_ahci::AhciDisk
                as *mut claudio_ahci::AhciDisk;
            let hba_ptr: *const HbaRegs = &**hba_box as *const HbaRegs;
            // SAFETY: disk_ptr and hba_ptr point at heap-`Box`ed values
            // owned by the global `REGISTRY`, which outlives every possible
            // use of the returned adapter. No other code holds a `&mut` to
            // these, and AHCI MMIO is safe to access concurrently through
            // the adapter's internal Mutex (see the SAFETY note on
            // `AhciBlockDeviceAdapter` in `claudio_vfs::adapters`).
            let adapter = unsafe { AhciBlockDeviceAdapter::new(disk_ptr, hba_ptr) };
            Some(Box::new(adapter))
        }
        DiskBackendRef::Nvme { ctrl_idx, nsid } => {
            let ctrl_box = reg.nvme_controllers.get(ctrl_idx)?;
            let ctrl_ptr: *mut NvmeController =
                &**ctrl_box as *const NvmeController as *mut NvmeController;
            // SAFETY: `ctrl_ptr` points at the heap-`Box`ed NvmeController
            // owned by REGISTRY, which outlives the adapter. Concurrent
            // access is serialised by the adapter's internal Mutex.
            let adapter = unsafe {
                NvmeBlockDeviceAdapter::new(
                    ctrl_ptr,
                    nsid,
                    entry.sector_count,
                    entry.sector_size,
                )
            };
            Some(Box::new(adapter))
        }
        DiskBackendRef::Usb { dev_idx } => {
            let usb_box = reg.usb_devices.get(dev_idx)?;
            // UsbStorageDevice<XhciBulkTransport> already implements
            // BlockDevice + Send + Sync. We create a thin wrapper that
            // holds a raw pointer to the heap-boxed device (stable for
            // program lifetime, same invariant as AHCI/NVMe above).
            let dev_ptr: *const UsbStorageDevice<XhciBulkTransport> =
                &**usb_box as *const UsbStorageDevice<XhciBulkTransport>;
            // SAFETY: `dev_ptr` points at the heap-`Box`ed
            // UsbStorageDevice owned by REGISTRY. The registry is a
            // `Once` singleton that outlives all callers. The
            // `usb_devices` vec is never mutated after `init`.
            let adapter = UsbBlockDeviceAdapter { ptr: dev_ptr };
            Some(Box::new(adapter))
        }
    }
}

// ---------------------------------------------------------------------------
// USB block device adapter
// ---------------------------------------------------------------------------

/// Thin adapter that delegates `BlockDevice` calls to an owned
/// `UsbStorageDevice<XhciBulkTransport>` via a raw pointer into the
/// registry. Follows the same stable-address pattern as AHCI/NVMe adapters.
struct UsbBlockDeviceAdapter {
    ptr: *const UsbStorageDevice<XhciBulkTransport>,
}

// SAFETY: The pointee lives in a heap `Box` inside the global `REGISTRY`
// singleton. The registry is never deallocated and `usb_devices` is never
// mutated after `init`. `UsbStorageDevice` serialises transport access
// through its internal `Mutex<XhciBulkTransport>`.
unsafe impl Send for UsbBlockDeviceAdapter {}
unsafe impl Sync for UsbBlockDeviceAdapter {}

impl BlockDevice for UsbBlockDeviceAdapter {
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<usize, claudio_vfs::device::DeviceError> {
        // SAFETY: pointer validity guaranteed by registry lifetime invariant.
        let dev = unsafe { &*self.ptr };
        dev.read_bytes(offset, buf)
    }

    fn write_bytes(&self, offset: u64, data: &[u8]) -> Result<usize, claudio_vfs::device::DeviceError> {
        let dev = unsafe { &*self.ptr };
        dev.write_bytes(offset, data)
    }

    fn flush(&self) -> Result<(), claudio_vfs::device::DeviceError> {
        let dev = unsafe { &*self.ptr };
        dev.flush()
    }

    fn sector_size(&self) -> u32 {
        let dev = unsafe { &*self.ptr };
        dev.sector_size()
    }

    fn total_size(&self) -> u64 {
        let dev = unsafe { &*self.ptr };
        dev.total_size()
    }
}
