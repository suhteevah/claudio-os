//! Swap partition/file support — page-out/page-in for heap memory.
//!
//! Since ClaudioOS uses a single address space with no hardware page faults
//! routed to a VMM, this is a *cooperative* swap system: the kernel
//! explicitly evicts least-recently-used pages to a swap device when heap
//! pressure is high, and reads them back on demand.
//!
//! Architecture:
//! - `SwapManager` tracks a bitmap of free/used swap slots.
//! - Each swap slot = 4 KiB (one page).
//! - LRU tracking is maintained with a simple timestamp-based list.
//! - The swap device is a raw partition identified by GPT type GUID
//!   `0657FD6D-A4AB-43C4-84E5-0933C84B4F4F` (Linux swap).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use alloc::format;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

// ── Armed swap partition (set by `swapon`, cleared by `swapoff`) ─────

/// Description of a swap partition that has been "armed" via `swapon`.
///
/// Armed means: we've located the partition on a real disk, verified its
/// geometry, and stashed the coordinates here. Real paging I/O still
/// requires a kernel VM subsystem, so this is metadata-only for now.
#[derive(Debug, Clone)]
pub struct SwapPartition {
    /// Human-readable source (e.g. "pci 0:2.0 ahci disk 0", "pci 0:3.0 nvme ns1").
    pub source: String,
    /// Partition index within the disk (0-based).
    pub partition_index: usize,
    /// Starting LBA of the partition.
    pub start_lba: u64,
    /// Partition size in bytes.
    pub size_bytes: u64,
    /// Partition name (GPT, may be empty).
    pub name: String,
    /// Whether the partition has the Linux-swap GPT type GUID.
    pub is_linux_swap: bool,
}

/// Globally armed swap partition (one slot — there's only ever one swap).
pub static SWAP_PARTITION: Mutex<Option<SwapPartition>> = Mutex::new(None);

// ── Constants ────────────────────────────────────────────────────────

/// Page size for swap (4 KiB, matching x86_64 page size).
pub const SWAP_PAGE_SIZE: usize = 4096;

/// GPT partition type GUID for Linux swap.
pub const SWAP_PARTITION_TYPE_GUID: [u8; 16] = [
    0x6d, 0xfd, 0x57, 0x06, 0xab, 0xa4, 0xc4, 0x43,
    0x84, 0xe5, 0x09, 0x33, 0xc8, 0x4b, 0x4f, 0x4f,
];

/// Magic number at the start of a swap partition header.
pub const SWAP_MAGIC: [u8; 10] = *b"CLAUSWAP\0\0";

// ── Swap slot bitmap ─────────────────────────────────────────────────

/// Bitmap tracking free/used swap slots.
struct SwapBitmap {
    /// Each bit = one swap slot. 1 = used, 0 = free.
    bits: Vec<u64>,
    /// Total number of slots.
    total_slots: u64,
    /// Number of currently used slots.
    used_slots: u64,
}

impl SwapBitmap {
    fn new(total_slots: u64) -> Self {
        let words = ((total_slots + 63) / 64) as usize;
        Self {
            bits: vec![0u64; words],
            total_slots,
            used_slots: 0,
        }
    }

    /// Allocate a free slot. Returns the slot index, or None if full.
    fn alloc(&mut self) -> Option<u64> {
        for (word_idx, word) in self.bits.iter_mut().enumerate() {
            if *word != u64::MAX {
                // Find first zero bit
                let bit = (!*word).trailing_zeros() as u64;
                let slot = word_idx as u64 * 64 + bit;
                if slot >= self.total_slots {
                    return None;
                }
                *word |= 1 << bit;
                self.used_slots += 1;
                return Some(slot);
            }
        }
        None
    }

    /// Free a slot by index.
    fn free(&mut self, slot: u64) {
        let word_idx = (slot / 64) as usize;
        let bit = slot % 64;
        if word_idx < self.bits.len() && (self.bits[word_idx] & (1 << bit)) != 0 {
            self.bits[word_idx] &= !(1 << bit);
            self.used_slots = self.used_slots.saturating_sub(1);
        }
    }

    fn is_used(&self, slot: u64) -> bool {
        let word_idx = (slot / 64) as usize;
        let bit = slot % 64;
        word_idx < self.bits.len() && (self.bits[word_idx] & (1 << bit)) != 0
    }
}

// ── LRU page tracker ─────────────────────────────────────────────────

/// Entry in the LRU page tracker.
#[derive(Debug, Clone, Copy)]
struct PageEntry {
    /// Virtual address of the page in the heap.
    virt_addr: u64,
    /// Swap slot index (if paged out, else u64::MAX).
    swap_slot: u64,
    /// Last access timestamp (PIT ticks).
    last_access: u64,
    /// Whether the page is currently resident in memory.
    resident: bool,
}

// ── Swap device trait ────────────────────────────────────────────────

/// Trait for the backing swap device (partition or file).
pub trait SwapDevice: Send {
    /// Read a swap slot into the provided 4 KiB buffer.
    fn read_slot(&self, slot: u64, buf: &mut [u8]) -> Result<(), SwapError>;
    /// Write a 4 KiB buffer to a swap slot.
    fn write_slot(&mut self, slot: u64, buf: &[u8]) -> Result<(), SwapError>;
    /// Total number of available swap slots.
    fn slot_count(&self) -> u64;
}

/// Swap subsystem errors.
#[derive(Debug, Clone, Copy)]
pub enum SwapError {
    /// I/O error on the swap device.
    IoError,
    /// No free swap slots available.
    SwapFull,
    /// Page not found in swap.
    NotFound,
    /// Swap not enabled.
    Disabled,
    /// Invalid slot index.
    InvalidSlot,
}

impl core::fmt::Display for SwapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SwapError::IoError => write!(f, "swap I/O error"),
            SwapError::SwapFull => write!(f, "swap space full"),
            SwapError::NotFound => write!(f, "page not found in swap"),
            SwapError::Disabled => write!(f, "swap not enabled"),
            SwapError::InvalidSlot => write!(f, "invalid swap slot"),
        }
    }
}

// ── Swap manager ─────────────────────────────────────────────────────

/// Global swap state.
pub static SWAP_ENABLED: AtomicBool = AtomicBool::new(false);
/// Total swap space in bytes.
pub static SWAP_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Used swap space in bytes.
pub static SWAP_USED: AtomicU64 = AtomicU64::new(0);

/// The swap manager — tracks swap slots and LRU page metadata.
pub struct SwapManager {
    /// Swap slot bitmap.
    bitmap: SwapBitmap,
    /// Page tracking: virtual address -> page entry.
    pages: BTreeMap<u64, PageEntry>,
    /// Whether swap is currently active.
    enabled: bool,
    /// Total swap slots.
    total_slots: u64,
    /// Statistics: total page-outs.
    stats_page_outs: u64,
    /// Statistics: total page-ins.
    stats_page_ins: u64,
}

impl SwapManager {
    /// Create a new swap manager for a device with the given number of slots.
    pub fn new(total_slots: u64) -> Self {
        Self {
            bitmap: SwapBitmap::new(total_slots),
            pages: BTreeMap::new(),
            enabled: false,
            total_slots,
            stats_page_outs: 0,
            stats_page_ins: 0,
        }
    }

    /// Enable swap.
    pub fn enable(&mut self) {
        self.enabled = true;
        SWAP_ENABLED.store(true, Ordering::Relaxed);
        SWAP_TOTAL.store(self.total_slots * SWAP_PAGE_SIZE as u64, Ordering::Relaxed);
        log::info!(
            "[swap] enabled: {} slots ({} MiB)",
            self.total_slots,
            (self.total_slots * SWAP_PAGE_SIZE as u64) / (1024 * 1024),
        );
    }

    /// Disable swap. All swapped-out pages must be paged in first.
    pub fn disable(&mut self) {
        self.enabled = false;
        SWAP_ENABLED.store(false, Ordering::Relaxed);
        SWAP_USED.store(0, Ordering::Relaxed);
        log::info!("[swap] disabled");
    }

    /// Register a page as tracked by the swap system.
    pub fn track_page(&mut self, virt_addr: u64) {
        let entry = PageEntry {
            virt_addr,
            swap_slot: u64::MAX,
            last_access: crate::interrupts::tick_count(),
            resident: true,
        };
        self.pages.insert(virt_addr, entry);
    }

    /// Record a page access (updates LRU timestamp).
    pub fn touch_page(&mut self, virt_addr: u64) {
        if let Some(entry) = self.pages.get_mut(&virt_addr) {
            entry.last_access = crate::interrupts::tick_count();
        }
    }

    /// Find the least-recently-used resident page.
    fn find_lru_page(&self) -> Option<u64> {
        self.pages
            .values()
            .filter(|e| e.resident)
            .min_by_key(|e| e.last_access)
            .map(|e| e.virt_addr)
    }

    /// Page out the LRU page to the swap device.
    ///
    /// Returns the virtual address of the evicted page, or an error.
    pub fn page_out<D: SwapDevice>(&mut self, device: &mut D) -> Result<u64, SwapError> {
        if !self.enabled {
            return Err(SwapError::Disabled);
        }

        let victim_addr = self.find_lru_page().ok_or(SwapError::NotFound)?;
        let slot = self.bitmap.alloc().ok_or(SwapError::SwapFull)?;

        // Read the page contents from memory
        let page_data = unsafe {
            core::slice::from_raw_parts(victim_addr as *const u8, SWAP_PAGE_SIZE)
        };

        // Write to swap device
        device.write_slot(slot, page_data)?;

        // Update tracking
        if let Some(entry) = self.pages.get_mut(&victim_addr) {
            entry.swap_slot = slot;
            entry.resident = false;
        }

        self.stats_page_outs += 1;
        SWAP_USED.store(
            self.bitmap.used_slots * SWAP_PAGE_SIZE as u64,
            Ordering::Relaxed,
        );

        log::debug!(
            "[swap] page-out: {:#x} -> slot {}",
            victim_addr,
            slot,
        );
        Ok(victim_addr)
    }

    /// Page in a previously evicted page from the swap device.
    pub fn page_in<D: SwapDevice>(
        &mut self,
        device: &D,
        virt_addr: u64,
    ) -> Result<(), SwapError> {
        if !self.enabled {
            return Err(SwapError::Disabled);
        }

        let entry = self.pages.get(&virt_addr).ok_or(SwapError::NotFound)?;
        if entry.resident {
            return Ok(()); // Already in memory
        }
        let slot = entry.swap_slot;
        if slot == u64::MAX {
            return Err(SwapError::NotFound);
        }

        // Read from swap device into memory
        let page_buf = unsafe {
            core::slice::from_raw_parts_mut(virt_addr as *mut u8, SWAP_PAGE_SIZE)
        };
        device.read_slot(slot, page_buf)?;

        // Free the swap slot
        self.bitmap.free(slot);

        // Update tracking
        if let Some(entry) = self.pages.get_mut(&virt_addr) {
            entry.swap_slot = u64::MAX;
            entry.resident = true;
            entry.last_access = crate::interrupts::tick_count();
        }

        self.stats_page_ins += 1;
        SWAP_USED.store(
            self.bitmap.used_slots * SWAP_PAGE_SIZE as u64,
            Ordering::Relaxed,
        );

        log::debug!("[swap] page-in: slot {} -> {:#x}", slot, virt_addr);
        Ok(())
    }

    /// Get swap statistics.
    pub fn stats(&self) -> SwapStats {
        SwapStats {
            enabled: self.enabled,
            total_slots: self.total_slots,
            used_slots: self.bitmap.used_slots,
            total_bytes: self.total_slots * SWAP_PAGE_SIZE as u64,
            used_bytes: self.bitmap.used_slots * SWAP_PAGE_SIZE as u64,
            tracked_pages: self.pages.len() as u64,
            resident_pages: self.pages.values().filter(|e| e.resident).count() as u64,
            page_outs: self.stats_page_outs,
            page_ins: self.stats_page_ins,
        }
    }
}

/// Swap usage statistics.
#[derive(Debug, Clone, Copy)]
pub struct SwapStats {
    pub enabled: bool,
    pub total_slots: u64,
    pub used_slots: u64,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub tracked_pages: u64,
    pub resident_pages: u64,
    pub page_outs: u64,
    pub page_ins: u64,
}

impl core::fmt::Display for SwapStats {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if !self.enabled {
            return write!(f, "swap: disabled");
        }
        write!(
            f,
            "swap: {} KiB used / {} KiB total ({} page-outs, {} page-ins, {} tracked, {} resident)",
            self.used_bytes / 1024,
            self.total_bytes / 1024,
            self.page_outs,
            self.page_ins,
            self.tracked_pages,
            self.resident_pages,
        )
    }
}

// ── Global swap instance ─────────────────────────────────────────────

/// Global swap manager (initialized to a dummy 0-slot manager).
pub static SWAP_MANAGER: Mutex<SwapManager> = Mutex::new(SwapManager {
    bitmap: SwapBitmap {
        bits: Vec::new(),
        total_slots: 0,
        used_slots: 0,
    },
    pages: BTreeMap::new(),
    enabled: false,
    total_slots: 0,
    stats_page_outs: 0,
    stats_page_ins: 0,
});

// ── GPT swap partition detection ─────────────────────────────────────

/// Check if a GPT partition type GUID matches the Linux swap type.
pub fn is_swap_partition_guid(guid: &[u8; 16]) -> bool {
    *guid == SWAP_PARTITION_TYPE_GUID
}

// ── Shell commands ───────────────────────────────────────────────────

/// Parse GPT partitions on the disk at `disk_index` in the global block
/// device registry (populated by `crate::disks::init` at boot phase 5d).
///
/// Returns `None` if the index is out of range, the block device adapter
/// cannot be constructed, or the GPT parser fails (disk not partitioned,
/// no GPT header, I/O error, etc.). Errors are logged at warn/debug level.
fn try_parse_gpt_for_disk(disk_index: usize) -> Option<Vec<claudio_vfs::PartitionEntry>> {
    let bd = match crate::disks::as_block_device(disk_index) {
        Some(bd) => bd,
        None => {
            log::debug!(
                "[swap] try_parse_gpt_for_disk({}): no adapter available",
                disk_index,
            );
            return None;
        }
    };
    match claudio_vfs::device::parse_gpt(&*bd) {
        Ok(entries) => {
            log::info!(
                "[swap] disk #{}: GPT parsed {} partition(s)",
                disk_index, entries.len(),
            );
            Some(entries)
        }
        Err(e) => {
            log::warn!(
                "[swap] disk #{}: GPT parse failed: {}",
                disk_index, e,
            );
            None
        }
    }
}

/// Handle the `swapon` shell command.
///
/// Usage:
/// - `swapon`          — show status (same as `swapon -s`)
/// - `swapon -s`       — show armed swap partition
/// - `swapon -a`       — auto-scan the live disk registry, arm the first
///                       GPT partition with the Linux-swap type GUID
/// - `swapon <device>` — arm a specific partition (e.g. `/dev/ahci0p2`,
///                       `/dev/nvme0n1p3`). The path must match a
///                       `DiskEntry::label` + trailing `p<index>`.
pub fn shell_swapon(args: &str) -> String {
    let args = args.trim();

    if args.is_empty() || args == "-s" {
        return status_string();
    }

    let auto = args == "-a";

    // Copy the registry into a lightweight local form so we can drop the
    // registry lock before doing GPT I/O (which would re-lock the registry
    // via `as_block_device`).
    struct DiskMeta {
        label: alloc::string::String,
        bus: u8,
        device: u8,
        function: u8,
        vendor_id: u16,
        device_id: u16,
        total_bytes: u64,
        sector_size: u32,
    }
    let metas: Vec<DiskMeta> = crate::disks::with_disks(|disks| {
        disks
            .iter()
            .map(|d| DiskMeta {
                label: d.label.clone(),
                bus: d.bus,
                device: d.device,
                function: d.function,
                vendor_id: d.vendor_id,
                device_id: d.device_id,
                total_bytes: d.total_bytes,
                sector_size: d.sector_size,
            })
            .collect()
    });

    let mut out = String::new();

    if metas.is_empty() {
        out.push_str("swapon: no disks in registry (disks::init found no AHCI/NVMe controllers)\n");
        if !auto {
            out.push_str(&format!(
                "swapon: explicit device '{}' requested but no storage hardware is probed\n",
                args,
            ));
        }
        return out;
    }

    out.push_str(&format!(
        "swapon: {} disk(s) registered:\n",
        metas.len(),
    ));
    for m in &metas {
        out.push_str(&format!(
            "  {} pci {:02x}:{:02x}.{} vendor={:#06x} device={:#06x} size={} MiB\n",
            m.label, m.bus, m.device, m.function,
            m.vendor_id, m.device_id,
            m.total_bytes / (1024 * 1024),
        ));
    }

    // Parse the user's explicit device path for match criteria, if any.
    // Accepts `ahci0p2`, `/dev/ahci0p2`, `nvme0n1p3`, `/dev/nvme0n1p3`.
    let want_label_partidx: Option<(alloc::string::String, usize)> = if auto {
        None
    } else {
        let raw = args.strip_prefix("/dev/").unwrap_or(args);
        // Find the last 'p' that introduces a numeric partition suffix.
        if let Some(pidx) = raw.rfind('p') {
            let (label, rest) = raw.split_at(pidx);
            // rest is like "p2"; drop the 'p'
            let suffix = &rest[1..];
            match suffix.parse::<usize>() {
                Ok(n) if !label.is_empty() => Some((label.into(), n)),
                _ => None,
            }
        } else {
            None
        }
    };

    // Iterate disks, parse GPT on each, and look for a matching partition.
    let mut armed: Option<SwapPartition> = None;
    for (disk_idx, m) in metas.iter().enumerate() {
        let entries = match try_parse_gpt_for_disk(disk_idx) {
            Some(e) => e,
            None => {
                out.push_str(&format!(
                    "  -> {} no GPT / parse failed\n",
                    m.label,
                ));
                continue;
            }
        };
        out.push_str(&format!(
            "  -> {} parsed {} GPT partition(s)\n",
            m.label, entries.len(),
        ));

        // Prefer a partition that matches an explicit request, else one
        // with the Linux swap GPT type GUID, else fall through.
        let mut chosen: Option<&claudio_vfs::PartitionEntry> = None;
        let mut matched_label = false;
        if let Some((want_label, want_idx)) = want_label_partidx.as_ref() {
            if &m.label == want_label {
                if let Some(e) = entries.iter().find(|e| e.index == *want_idx) {
                    chosen = Some(e);
                    matched_label = true;
                }
            }
        }
        if chosen.is_none() && (auto || !matched_label) {
            chosen = entries.iter().find(|e| is_swap_partition_guid(&e.type_id));
        }

        if let Some(entry) = chosen {
            let source = format!(
                "{} (pci {:02x}:{:02x}.{} {:#06x}:{:#06x})",
                m.label, m.bus, m.device, m.function, m.vendor_id, m.device_id,
            );
            armed = Some(SwapPartition {
                source,
                partition_index: entry.index,
                start_lba: entry.start_lba,
                size_bytes: entry.sector_count * m.sector_size as u64,
                name: entry.name.clone(),
                is_linux_swap: is_swap_partition_guid(&entry.type_id),
            });
            break;
        }
    }

    if let Some(p) = armed {
        let size_mb = p.size_bytes / (1024 * 1024);
        out.push_str(&format!(
            "swapon: armed swap on {} ({} MiB), paging activation pending kernel VM work\n",
            p.source, size_mb,
        ));
        log::info!(
            "[swap] armed: {} partition_index={} start_lba={} size={} MiB linux_swap_guid={}",
            p.source, p.partition_index, p.start_lba, size_mb, p.is_linux_swap,
        );
        *SWAP_PARTITION.lock() = Some(p);
    } else {
        out.push_str(
            "swapon: no matching swap partition found (need a GPT partition with \
             Linux-swap type GUID 0657FD6D-A4AB-43C4-84E5-0933C84B4F4F)\n",
        );
    }

    out
}

/// Handle the `swapoff` shell command.
///
/// Usage: `swapoff <device>` or `swapoff -a` (disable all).
pub fn shell_swapoff(args: &str) -> String {
    let args = args.trim();

    // Clear the armed partition regardless of the manager state.
    let armed_cleared = SWAP_PARTITION.lock().take().is_some();

    let mut mgr = SWAP_MANAGER.lock();
    if !mgr.enabled {
        if armed_cleared {
            return "swapoff: cleared armed swap partition (manager was not yet enabled)\n"
                .to_string();
        }
        return "swapoff: no swap is currently enabled\n".to_string();
    }

    if mgr.bitmap.used_slots > 0 {
        return format!(
            "swapoff: {} pages still in swap, page them in first\n",
            mgr.bitmap.used_slots,
        );
    }

    mgr.disable();
    if args == "-a" || args.is_empty() {
        "swapoff: all swap disabled\n".to_string()
    } else {
        format!("swapoff: disabled swap on {}\n", args)
    }
}

/// Current armed-swap status string.
fn status_string() -> String {
    match SWAP_PARTITION.lock().as_ref() {
        Some(p) => format!(
            "swap armed: source={} part_idx={} start_lba={} size={} MiB name={:?} linux_swap_guid={}\n",
            p.source,
            p.partition_index,
            p.start_lba,
            p.size_bytes / (1024 * 1024),
            p.name,
            p.is_linux_swap,
        ),
        None => "swap: no partition armed (run `swapon -a` to scan)\n".to_string(),
    }
}

/// Handle the `free` shell command — display memory and swap usage.
///
/// Usage: `free` or `free -h` (human-readable).
pub fn shell_free(args: &str) -> String {
    let (heap_used, heap_total) = crate::memory::heap_stats();
    let swap_stats = SWAP_MANAGER.lock().stats();

    let human = args.trim() == "-h";

    fn fmt_bytes(bytes: u64, human: bool) -> String {
        if human {
            if bytes >= 1024 * 1024 * 1024 {
                format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
            } else if bytes >= 1024 * 1024 {
                format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
            } else if bytes >= 1024 {
                format!("{:.1}K", bytes as f64 / 1024.0)
            } else {
                format!("{}B", bytes)
            }
        } else {
            format!("{}", bytes / 1024) // default: KiB
        }
    }

    let unit = if human { "" } else { " (KiB)" };

    let mut out = String::new();
    out.push_str(&format!(
        "{:<12} {:>12} {:>12} {:>12}\n",
        "", format!("total{}", unit), format!("used{}", unit), format!("free{}", unit),
    ));
    out.push_str(&format!(
        "{:<12} {:>12} {:>12} {:>12}\n",
        "Mem:",
        fmt_bytes(heap_total as u64, human),
        fmt_bytes(heap_used as u64, human),
        fmt_bytes((heap_total - heap_used) as u64, human),
    ));

    if swap_stats.enabled {
        out.push_str(&format!(
            "{:<12} {:>12} {:>12} {:>12}\n",
            "Swap:",
            fmt_bytes(swap_stats.total_bytes, human),
            fmt_bytes(swap_stats.used_bytes, human),
            fmt_bytes(swap_stats.total_bytes - swap_stats.used_bytes, human),
        ));
    } else {
        out.push_str(&format!(
            "{:<12} {:>12} {:>12} {:>12}\n",
            "Swap:", "0", "0", "0",
        ));
    }

    out
}

/// Initialize the swap subsystem. Call once at boot.
pub fn init() {
    log::info!("[swap] subsystem initialized (no swap device configured)");
    log::info!("[swap] run `swapon -a` at the shell to scan PCI mass-storage controllers");
}
