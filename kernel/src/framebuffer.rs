//! GOP framebuffer management — pixel drawing, viewport clipping.
//!
//! The framebuffer is our only display output. Terminal panes are viewports
//! into this buffer. The panic handler can force-render here.
//!
//! ## Framebuffer address mapping
//!
//! The bootloader v0.11 maps the framebuffer at its OWN virtual address
//! (e.g. 0x20000000000), separate from the physical memory offset mapping.
//! Writing to that address can cause a page fault if the bootloader's mapping
//! has restrictive flags. Instead, we translate the framebuffer's virtual
//! address to its physical address via page table walk, then access it through
//! the physical memory offset mapping which is known to be PRESENT + WRITABLE.

use bootloader_api::info::FrameBuffer;
use spin::Mutex;
use x86_64::structures::paging::{OffsetPageTable, PageTable, Translate};
use x86_64::VirtAddr;

static FB: Mutex<Option<FrameBufferState>> = Mutex::new(None);

pub struct FrameBufferState {
    pub buffer: &'static mut [u8],
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

    let buffer = unsafe {
        core::slice::from_raw_parts_mut(fb_via_phys_map as *mut u8, buf_len)
    };

    let state = FrameBufferState {
        buffer,
        width: info.width,
        height: info.height,
        stride: info.stride,
        bytes_per_pixel: info.bytes_per_pixel,
    };

    // Clear the framebuffer to black to prove writes work
    for byte in state.buffer.iter_mut() {
        *byte = 0;
    }
    log::info!("[fb] framebuffer cleared to black ({} bytes written)", buf_len);

    *FB.lock() = Some(state);
    log::info!("[fb] framebuffer ready");
}

/// Return the framebuffer width in pixels, or 0 if not initialised.
pub fn width() -> usize {
    FB.lock().as_ref().map_or(0, |fb| fb.width)
}

/// Return the framebuffer height in pixels, or 0 if not initialised.
pub fn height() -> usize {
    FB.lock().as_ref().map_or(0, |fb| fb.height)
}

/// Draw a single pixel. Used by terminal renderer.
#[inline]
pub fn put_pixel(x: usize, y: usize, r: u8, g: u8, b: u8) {
    // TODO: lock-free fast path using atomic framebuffer access
    if let Some(ref mut fb) = *FB.lock() {
        if x >= fb.width || y >= fb.height {
            return;
        }
        let offset = (y * fb.stride + x) * fb.bytes_per_pixel;
        // Assume BGR pixel format (common for UEFI GOP)
        fb.buffer[offset] = b;
        fb.buffer[offset + 1] = g;
        fb.buffer[offset + 2] = r;
    }
}
