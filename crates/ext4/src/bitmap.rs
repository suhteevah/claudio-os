//! Block and inode bitmap management.
//!
//! ext4 uses bitmaps to track which blocks and inodes are allocated within each
//! block group. Each bit in the bitmap corresponds to one block (or inode):
//! - Bit 0 = first block/inode in the group
//! - Bit set (1) = allocated
//! - Bit clear (0) = free
//!
//! The bitmap is stored in a single block whose location is recorded in the
//! block group descriptor.

/// Bitmap allocator for blocks and inodes.
///
/// Operates on a raw bitmap buffer (one block worth of bytes).
/// Each byte holds 8 bits, LSB first (bit 0 of byte 0 = item 0).
pub struct BitmapAllocator;

impl BitmapAllocator {
    /// Check whether a specific bit is set (allocated) in the bitmap.
    ///
    /// `index` is the 0-based bit position.
    #[inline]
    pub fn is_set(bitmap: &[u8], index: u32) -> bool {
        let byte_idx = (index / 8) as usize;
        let bit_idx = index % 8;
        if byte_idx >= bitmap.len() {
            log::warn!("[ext4::bitmap] is_set: index {} out of range (bitmap size {} bytes)", index, bitmap.len());
            return false;
        }
        bitmap[byte_idx] & (1 << bit_idx) != 0
    }

    /// Set a bit (mark as allocated).
    ///
    /// Returns `true` if the bit was previously clear (successful allocation),
    /// `false` if it was already set.
    pub fn set(bitmap: &mut [u8], index: u32) -> bool {
        let byte_idx = (index / 8) as usize;
        let bit_idx = index % 8;
        if byte_idx >= bitmap.len() {
            log::error!("[ext4::bitmap] set: index {} out of range (bitmap size {} bytes)", index, bitmap.len());
            return false;
        }
        let was_clear = bitmap[byte_idx] & (1 << bit_idx) == 0;
        bitmap[byte_idx] |= 1 << bit_idx;
        if was_clear {
            log::trace!("[ext4::bitmap] allocated index {}", index);
        } else {
            log::warn!("[ext4::bitmap] index {} was already allocated", index);
        }
        was_clear
    }

    /// Clear a bit (mark as free).
    ///
    /// Returns `true` if the bit was previously set (successful free),
    /// `false` if it was already clear.
    pub fn clear(bitmap: &mut [u8], index: u32) -> bool {
        let byte_idx = (index / 8) as usize;
        let bit_idx = index % 8;
        if byte_idx >= bitmap.len() {
            log::error!("[ext4::bitmap] clear: index {} out of range (bitmap size {} bytes)", index, bitmap.len());
            return false;
        }
        let was_set = bitmap[byte_idx] & (1 << bit_idx) != 0;
        bitmap[byte_idx] &= !(1 << bit_idx);
        if was_set {
            log::trace!("[ext4::bitmap] freed index {}", index);
        } else {
            log::warn!("[ext4::bitmap] index {} was already free", index);
        }
        was_set
    }

    /// Find the first free (clear) bit in the bitmap, starting from `start`.
    ///
    /// `total` is the total number of valid bits in the bitmap (e.g., blocks_per_group
    /// or inodes_per_group). Returns `None` if no free bit is found.
    pub fn find_first_free(bitmap: &[u8], start: u32, total: u32) -> Option<u32> {
        log::trace!("[ext4::bitmap] searching for free bit starting at {} (total={})", start, total);

        // First pass: from start to total
        for i in start..total {
            if !Self::is_set(bitmap, i) {
                log::trace!("[ext4::bitmap] found free bit at index {}", i);
                return Some(i);
            }
        }

        // Wrap-around pass: from 0 to start
        for i in 0..start {
            if !Self::is_set(bitmap, i) {
                log::trace!("[ext4::bitmap] found free bit at index {} (wrapped)", i);
                return Some(i);
            }
        }

        log::debug!("[ext4::bitmap] no free bit found (total={})", total);
        None
    }

    /// Find a contiguous run of `count` free bits, starting the search from `start`.
    ///
    /// Returns the starting index of the run, or `None` if no suitable run exists.
    /// This is useful for allocating contiguous blocks for extents.
    pub fn find_contiguous_free(bitmap: &[u8], start: u32, total: u32, count: u32) -> Option<u32> {
        if count == 0 {
            return Some(start);
        }

        log::trace!("[ext4::bitmap] searching for {} contiguous free bits starting at {} (total={})",
            count, start, total);

        let mut run_start = start;
        let mut run_len = 0u32;

        for i in start..total {
            if !Self::is_set(bitmap, i) {
                if run_len == 0 {
                    run_start = i;
                }
                run_len += 1;
                if run_len >= count {
                    log::debug!("[ext4::bitmap] found {} contiguous free bits at index {}", count, run_start);
                    return Some(run_start);
                }
            } else {
                run_len = 0;
            }
        }

        // Wrap-around: search from 0 to start
        run_len = 0;
        for i in 0..start {
            if !Self::is_set(bitmap, i) {
                if run_len == 0 {
                    run_start = i;
                }
                run_len += 1;
                if run_len >= count {
                    log::debug!("[ext4::bitmap] found {} contiguous free bits at index {} (wrapped)", count, run_start);
                    return Some(run_start);
                }
            } else {
                run_len = 0;
            }
        }

        log::debug!("[ext4::bitmap] no contiguous run of {} bits found (total={})", count, total);
        None
    }

    /// Count the number of free (clear) bits in the bitmap.
    pub fn count_free(bitmap: &[u8], total: u32) -> u32 {
        let mut count = 0u32;
        for i in 0..total {
            if !Self::is_set(bitmap, i) {
                count += 1;
            }
        }
        log::trace!("[ext4::bitmap] counted {} free bits out of {}", count, total);
        count
    }

    /// Allocate a single bit (find first free and set it).
    ///
    /// Returns the allocated index, or `None` if the bitmap is full.
    pub fn allocate_one(bitmap: &mut [u8], start: u32, total: u32) -> Option<u32> {
        log::debug!("[ext4::bitmap] allocating one bit (start={}, total={})", start, total);
        let idx = Self::find_first_free(bitmap, start, total)?;
        Self::set(bitmap, idx);
        log::info!("[ext4::bitmap] allocated bit {}", idx);
        Some(idx)
    }

    /// Allocate a contiguous run of `count` bits.
    ///
    /// Returns the starting index of the allocated run, or `None` if not enough
    /// contiguous space exists.
    pub fn allocate_contiguous(bitmap: &mut [u8], start: u32, total: u32, count: u32) -> Option<u32> {
        log::debug!("[ext4::bitmap] allocating {} contiguous bits (start={}, total={})", count, start, total);
        let run_start = Self::find_contiguous_free(bitmap, start, total, count)?;
        for i in 0..count {
            Self::set(bitmap, run_start + i);
        }
        log::info!("[ext4::bitmap] allocated {} contiguous bits starting at {}", count, run_start);
        Some(run_start)
    }

    /// Free a contiguous run of `count` bits starting at `start_index`.
    pub fn free_range(bitmap: &mut [u8], start_index: u32, count: u32) {
        log::debug!("[ext4::bitmap] freeing {} bits starting at {}", count, start_index);
        for i in 0..count {
            Self::clear(bitmap, start_index + i);
        }
        log::info!("[ext4::bitmap] freed {} bits starting at {}", count, start_index);
    }
}
