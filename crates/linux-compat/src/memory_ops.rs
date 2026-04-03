//! Memory management syscall implementations.
//!
//! Implements mmap, mprotect, munmap, brk, mremap, madvise.

use crate::errno::*;
use crate::types::*;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Page size (4 KiB).
const PAGE_SIZE: u64 = 4096;

/// A single memory mapping.
#[derive(Debug, Clone)]
pub struct MemoryMapping {
    /// Start address.
    pub addr: u64,
    /// Length in bytes.
    pub length: u64,
    /// Protection flags (PROT_*).
    pub prot: u32,
    /// Mapping flags (MAP_*).
    pub flags: u32,
    /// Backing data (for anonymous mappings).
    pub data: Vec<u8>,
}

/// Process memory state for mmap/brk management.
pub struct MemoryManager {
    /// Current program break.
    pub brk_current: u64,
    /// Initial program break (set by ELF loader).
    pub brk_base: u64,
    /// Anonymous memory mappings, keyed by start address.
    pub mappings: BTreeMap<u64, MemoryMapping>,
    /// Next anonymous mmap address hint.
    pub mmap_hint: u64,
}

impl MemoryManager {
    /// Create a new memory manager.
    pub fn new(brk_base: u64) -> Self {
        Self {
            brk_current: brk_base,
            brk_base,
            mappings: BTreeMap::new(),
            // Start anonymous mmaps above the typical brk region
            mmap_hint: 0x0000_7F00_0000_0000,
        }
    }

    /// Align up to page boundary.
    fn page_align_up(addr: u64) -> u64 {
        (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
    }

    /// Find a free region of the given size.
    fn find_free_region(&self, length: u64) -> u64 {
        let mut candidate = self.mmap_hint;
        let aligned_len = Self::page_align_up(length);

        // Simple linear scan for a gap
        for (&addr, mapping) in &self.mappings {
            let end = addr + mapping.length;
            if candidate >= addr && candidate < end {
                candidate = Self::page_align_up(end);
            }
        }
        candidate
    }
}

/// sys_mmap(addr, length, prot, flags, fd, offset) -> mapped address
pub fn sys_mmap(
    mm: &mut MemoryManager,
    addr: u64,
    length: u64,
    prot: u64,
    flags: u64,
    fd: u64,
    _offset: u64,
) -> i64 {
    if length == 0 {
        return EINVAL.as_neg();
    }

    let flags = flags as u32;
    let prot = prot as u32;
    let aligned_length = MemoryManager::page_align_up(length);

    let map_addr = if flags & MAP_FIXED != 0 {
        // MAP_FIXED: use exactly the requested address
        if addr == 0 {
            return EINVAL.as_neg();
        }
        addr
    } else if addr != 0 {
        // Hint address — try to use it, fall back to finding free space
        addr
    } else {
        // No hint — find free space
        mm.find_free_region(aligned_length)
    };

    let is_anonymous = flags & MAP_ANONYMOUS != 0;

    if !is_anonymous && fd as i64 >= 0 {
        // File-backed mapping — in kernel integration, would read from the fd.
        // For now, create a zero-filled region.
    }

    let mapping = MemoryMapping {
        addr: map_addr,
        length: aligned_length,
        prot,
        flags,
        data: alloc::vec![0u8; aligned_length as usize],
    };

    mm.mappings.insert(map_addr, mapping);

    // Advance hint for next allocation
    let end = map_addr + aligned_length;
    if end > mm.mmap_hint {
        mm.mmap_hint = end;
    }

    log::trace!("mmap: addr=0x{:X}, len={}, prot={}, flags={} -> 0x{:X}",
        addr, length, prot, flags, map_addr);

    map_addr as i64
}

/// sys_mprotect(addr, len, prot) -> 0
pub fn sys_mprotect(mm: &mut MemoryManager, addr: u64, len: u64, prot: u64) -> i64 {
    let _aligned_len = MemoryManager::page_align_up(len);

    // Find the mapping that contains this address
    if let Some(mapping) = mm.mappings.get_mut(&addr) {
        mapping.prot = prot as u32;
        0
    } else {
        // Check if the address falls within any mapping
        for mapping in mm.mappings.values_mut() {
            if addr >= mapping.addr && addr < mapping.addr + mapping.length {
                mapping.prot = prot as u32;
                return 0;
            }
        }
        // On Linux, mprotect on unmapped memory returns ENOMEM
        ENOMEM.as_neg()
    }
}

/// sys_munmap(addr, len) -> 0
pub fn sys_munmap(mm: &mut MemoryManager, addr: u64, len: u64) -> i64 {
    if len == 0 {
        return EINVAL.as_neg();
    }

    let _aligned_len = MemoryManager::page_align_up(len);

    // Remove any mappings that overlap with [addr, addr+len)
    let to_remove: Vec<u64> = mm
        .mappings
        .keys()
        .filter(|&&k| {
            let mapping = &mm.mappings[&k];
            let m_end = k + mapping.length;
            let u_end = addr + len;
            // Overlaps if not (m_end <= addr || k >= u_end)
            !(m_end <= addr || k >= u_end)
        })
        .copied()
        .collect();

    for key in to_remove {
        mm.mappings.remove(&key);
    }

    0
}

/// sys_brk(addr) -> new brk
pub fn sys_brk(mm: &mut MemoryManager, addr: u64) -> i64 {
    if addr == 0 {
        // Query current brk
        return mm.brk_current as i64;
    }

    let new_brk = MemoryManager::page_align_up(addr);

    if new_brk < mm.brk_base {
        // Can't shrink below initial brk
        return mm.brk_current as i64;
    }

    // Grow or shrink the brk
    mm.brk_current = new_brk;

    log::trace!("brk: requested=0x{:X}, set=0x{:X}", addr, new_brk);

    new_brk as i64
}

/// sys_mremap(old_addr, old_size, new_size, flags) -> new address
pub fn sys_mremap(
    mm: &mut MemoryManager,
    old_addr: u64,
    old_size: u64,
    new_size: u64,
    flags: u64,
) -> i64 {
    let _ = old_size;

    if new_size == 0 {
        return EINVAL.as_neg();
    }

    let new_aligned = MemoryManager::page_align_up(new_size);

    if let Some(mut mapping) = mm.mappings.remove(&old_addr) {
        if flags as u32 & MREMAP_MAYMOVE != 0 {
            // Can move the mapping
            let new_addr = mm.find_free_region(new_aligned);
            mapping.data.resize(new_aligned as usize, 0);
            mapping.addr = new_addr;
            mapping.length = new_aligned;
            mm.mappings.insert(new_addr, mapping);
            new_addr as i64
        } else {
            // Try to expand in place
            mapping.data.resize(new_aligned as usize, 0);
            mapping.length = new_aligned;
            mm.mappings.insert(old_addr, mapping);
            old_addr as i64
        }
    } else {
        EINVAL.as_neg()
    }
}

/// sys_madvise(addr, length, advice) -> 0
pub fn sys_madvise(_mm: &MemoryManager, _addr: u64, _length: u64, advice: u64) -> i64 {
    // madvise is advisory — we can accept most advice values silently.
    match advice as u32 {
        MADV_NORMAL | MADV_RANDOM | MADV_SEQUENTIAL | MADV_WILLNEED | MADV_DONTNEED => 0,
        _ => EINVAL.as_neg(),
    }
}
