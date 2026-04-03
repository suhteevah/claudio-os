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

/// Handle the `swapon` shell command.
///
/// Usage: `swapon <device>` or `swapon -a` (auto-detect from GPT).
pub fn shell_swapon(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        return "usage: swapon <device> | swapon -a\n".to_string();
    }

    if args == "-a" {
        return "swapon: scanning GPT for swap partitions... (not yet wired to disk driver)\n"
            .to_string();
    }

    format!(
        "swapon: would enable swap on {} (disk driver not yet wired)\n",
        args,
    )
}

/// Handle the `swapoff` shell command.
///
/// Usage: `swapoff <device>` or `swapoff -a` (disable all).
pub fn shell_swapoff(args: &str) -> String {
    let args = args.trim();

    let mut mgr = SWAP_MANAGER.lock();
    if !mgr.enabled {
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
    // TODO: Auto-detect swap partition from GPT when disk drivers are wired.
}
