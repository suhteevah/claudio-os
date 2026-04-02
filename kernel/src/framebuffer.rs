//! GOP framebuffer management — double-buffered, memory-mapped pixel access.
//!
//! ## Architecture (TempleOS-inspired)
//!
//! The framebuffer is our only display output. Rather than calling `put_pixel`
//! for every pixel (which acquires a mutex lock 1M+ times per frame), we use:
//!
//! 1. **Back buffer** — a heap-allocated `Vec<u8>` the same size as the
//!    hardware framebuffer. All rendering happens here, lock-free.
//! 2. **Front buffer** — the actual hardware framebuffer, memory-mapped via
//!    the physical memory offset. We blast the back buffer here in one
//!    `copy_nonoverlapping` call ("page flip").
//! 3. **Dirty region tracking** — the dashboard tells us which pixel rows
//!    changed, so we only copy those rows to the front buffer.
//!
//! This eliminates:
//! - ~1M mutex lock/unlock cycles per frame (was: one per pixel)
//! - Tearing (was: pixels written directly to hardware mid-scanout)
//! - Redundant rendering (was: every pane re-rendered on every keypress)
//!
//! ## Framebuffer address mapping
//!
//! The bootloader v0.11 maps the framebuffer at its OWN virtual address
//! (e.g. 0x20000000000), separate from the physical memory offset mapping.
//! Writing to that address can cause a page fault if the bootloader's mapping
//! has restrictive flags. Instead, we translate the framebuffer's virtual
//! address to its physical address via page table walk, then access it through
//! the physical memory offset mapping which is known to be PRESENT + WRITABLE.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use bootloader_api::info::FrameBuffer;
use spin::Mutex;
use x86_64::structures::paging::{OffsetPageTable, PageTable, Translate};
use x86_64::VirtAddr;

static FB: Mutex<Option<FrameBufferState>> = Mutex::new(None);

pub struct FrameBufferState {
    /// Hardware framebuffer (front buffer) — memory-mapped, written via
    /// `write_volatile` / `copy_nonoverlapping`.
    pub front: &'static mut [u8],
    /// Off-screen back buffer — heap-allocated, all rendering targets this.
    pub back: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bytes_per_pixel: usize,
}

pub fn init(fb: &'static mut FrameBuffer, phys_mem_offset: u64) {
    let info = fb.info();
    let buf_len = fb.buffer().len();

    // The bootloader's buffer_mut() returns a slice at the bootloader's chosen
    // virtual address for the framebuffer. This address may not be writable
    // (the bootloader's mapping can lack WRITABLE flag or have cache attributes
    // that cause faults). Instead, we find the physical address and access it
    // through the physical memory offset mapping.
    let bootloader_virt = fb.buffer_mut().as_mut_ptr() as u64;
    log::info!(
        "[fb] bootloader mapped buffer at {:#x}, {} bytes",
        bootloader_virt,
        buf_len
    );

    // Walk the page table to find the physical address of the framebuffer
    let phys_offset_virt = VirtAddr::new(phys_mem_offset);
    let page_table = unsafe {
        let (level_4_frame, _) = x86_64::registers::control::Cr3::read();
        let phys = level_4_frame.start_address();
        let virt = phys_offset_virt + phys.as_u64();
        let ptr: *mut PageTable = virt.as_mut_ptr();
        &mut *ptr
    };
    let mapper = unsafe { OffsetPageTable::new(page_table, phys_offset_virt) };

    let fb_virt_addr = VirtAddr::new(bootloader_virt);
    let fb_phys_addr = mapper
        .translate_addr(fb_virt_addr)
        .expect("[fb] failed to translate framebuffer virtual address to physical");

    log::info!("[fb] framebuffer physical address: {:#x}", fb_phys_addr.as_u64());

    // Access the framebuffer through the physical memory offset mapping.
    // This mapping is set up by the bootloader with PRESENT + WRITABLE flags.
    let fb_via_phys_map = phys_mem_offset + fb_phys_addr.as_u64();
    log::info!("[fb] using phys-offset mapped address: {:#x}", fb_via_phys_map);

    let front = unsafe {
        core::slice::from_raw_parts_mut(fb_via_phys_map as *mut u8, buf_len)
    };

    // Allocate the back buffer on the heap — same size as the front buffer.
    let back = vec![0u8; buf_len];

    log::info!(
        "[fb] double buffer allocated: {} bytes back buffer + {} bytes front buffer",
        buf_len,
        buf_len
    );

    let state = FrameBufferState {
        front,
        back,
        width: info.width,
        height: info.height,
        stride: info.stride,
        bytes_per_pixel: info.bytes_per_pixel,
    };

    // Clear both buffers to black to prove writes work.
    // Front buffer: use write_volatile since it's memory-mapped hardware.
    for byte in state.front.iter_mut() {
        unsafe {
            core::ptr::write_volatile(byte as *mut u8, 0);
        }
    }
    log::info!("[fb] front buffer cleared to black ({} bytes)", buf_len);

    *FB.lock() = Some(state);
    log::info!(
        "[fb] framebuffer ready: {}x{} stride={} bpp={} double-buffered",
        info.width,
        info.height,
        info.stride,
        info.bytes_per_pixel
    );
}

/// Return the framebuffer width in pixels, or 0 if not initialised.
pub fn width() -> usize {
    FB.lock().as_ref().map_or(0, |fb| fb.width)
}

/// Return the framebuffer height in pixels, or 0 if not initialised.
pub fn height() -> usize {
    FB.lock().as_ref().map_or(0, |fb| fb.height)
}

/// Return stride in pixels.
pub fn stride() -> usize {
    FB.lock().as_ref().map_or(0, |fb| fb.stride)
}

/// Return bytes per pixel.
pub fn bytes_per_pixel() -> usize {
    FB.lock().as_ref().map_or(4, |fb| fb.bytes_per_pixel)
}

/// Draw a single pixel. Legacy API — still used by the panic handler.
/// For normal rendering, use the back-buffer `DrawTarget` instead.
#[inline]
pub fn put_pixel(x: usize, y: usize, r: u8, g: u8, b: u8) {
    if let Some(ref mut fb) = *FB.lock() {
        if x >= fb.width || y >= fb.height {
            return;
        }
        let offset = (y * fb.stride + x) * fb.bytes_per_pixel;
        // Write to the back buffer (not directly to hardware).
        if offset + 2 < fb.back.len() {
            fb.back[offset] = b;
            fb.back[offset + 1] = g;
            fb.back[offset + 2] = r;
        }
    }
}

/// Blit the entire back buffer to the front (hardware) buffer.
///
/// Uses `copy_nonoverlapping` for maximum throughput — one memcpy for the
/// entire framebuffer. On a 1280x800x4 display this copies ~4 MiB.
pub fn blit_full() {
    if let Some(ref mut fb) = *FB.lock() {
        let len = fb.back.len().min(fb.front.len());
        unsafe {
            core::ptr::copy_nonoverlapping(
                fb.back.as_ptr(),
                fb.front.as_mut_ptr(),
                len,
            );
        }
        log::trace!("[fb] blit_full: {} bytes copied to front buffer", len);
    }
}

/// Blit only the specified pixel rows from the back buffer to the front buffer.
///
/// This is the dirty-region fast path: when a single character row is typed,
/// only ~16 pixel rows need copying (~80 KiB for 1280x4x16) instead of the
/// full ~4 MiB framebuffer.
pub fn blit_rows(y_start: usize, y_end: usize) {
    if let Some(ref mut fb) = *FB.lock() {
        let y_start = y_start.min(fb.height);
        let y_end = y_end.min(fb.height);
        if y_start >= y_end {
            return;
        }
        let bpp = fb.bytes_per_pixel;
        let row_bytes = fb.stride * bpp;
        let start = y_start * row_bytes;
        let end = y_end * row_bytes;
        let len = (end - start).min(fb.back.len() - start).min(fb.front.len() - start);
        unsafe {
            core::ptr::copy_nonoverlapping(
                fb.back.as_ptr().add(start),
                fb.front.as_mut_ptr().add(start),
                len,
            );
        }
        log::trace!(
            "[fb] blit_rows: y={}..{} ({} bytes)",
            y_start,
            y_end,
            len
        );
    }
}

/// Acquire the back buffer for direct rendering.
///
/// The caller gets mutable access to the back buffer along with dimensions,
/// enabling the `DrawTarget` to write pixels directly without going through
/// the `put_pixel` function (which would re-acquire the mutex on each call).
///
/// # Safety
///
/// The closure must not call any other `framebuffer::` functions (deadlock).
/// It receives `(back_buffer, width, height, stride, bytes_per_pixel)`.
pub fn with_back_buffer<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut [u8], usize, usize, usize, usize) -> R,
{
    if let Some(ref mut fb) = *FB.lock() {
        Some(f(
            &mut fb.back,
            fb.width,
            fb.height,
            fb.stride,
            fb.bytes_per_pixel,
        ))
    } else {
        None
    }
}
