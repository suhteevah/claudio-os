//! Font rendering using embedded Terminus 8×16 bitmap font.
//!
//! Provides character-level rendering to any [`DrawTarget`]
//! using the Terminus font — a crisp, pixel-perfect bitmap font designed
//! specifically for terminal use. No anti-aliasing, no sub-pixel fuzz.
//!
//! ## Fast-path rendering
//!
//! When the `DrawTarget` exposes a contiguous pixel buffer via `buffer_mut()`,
//! `render_char` and `fill_rect` bypass individual `put_pixel` calls entirely,
//! writing 4-byte BGR32 pixels directly into the buffer using
//! `core::ptr::write_volatile`. This is the TempleOS-style approach: treat the
//! framebuffer as a flat array and memcpy scanlines.

use super::terminus;
use super::unicode_font;
use terminal_core::Color;

/// Height of each character cell in pixels.
pub const FONT_HEIGHT: usize = terminus::CHAR_HEIGHT;

/// Width of each character cell in pixels (monospace — constant for all glyphs).
pub const FONT_WIDTH: usize = terminus::CHAR_WIDTH;

/// Pack a Color into a BGR32 4-byte array (UEFI GOP pixel format: B, G, R, reserved).
///
/// UEFI's `EFI_GRAPHICS_OUTPUT_BLT_PIXEL` uses Blue-Green-Red byte order
/// (opposite of the more common RGB). The 4th byte is reserved/padding
/// and must be 0. This is the native pixel format for all framebuffer
/// writes in ClaudioOS.
#[inline(always)]
pub const fn color_to_bgr32(c: Color) -> [u8; 4] {
    [c.b, c.g, c.r, 0]
}

/// Convert pixel dimensions to cell dimensions.
pub fn pixels_to_cells(width: usize, height: usize) -> (u16, u16) {
    ((width / FONT_WIDTH) as u16, (height / FONT_HEIGHT) as u16)
}

/// Anything that can accept individual pixel writes and expose framebuffer metadata.
pub trait DrawTarget {
    fn put_pixel(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8);
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn bytes_per_pixel(&self) -> usize { 4 }
    fn stride(&self) -> usize { self.width() }
    fn buffer_mut(&mut self) -> Option<&mut [u8]> { None }
    fn fill_scanline(&mut self, x: usize, y: usize, width: usize, r: u8, g: u8, b: u8) {
        for px in x..x + width {
            self.put_pixel(px, y, r, g, b);
        }
    }
}

/// Render a single character glyph into `target` at pixel position (`x`, `y`).
///
/// Uses the Terminus 8×16 bitmap font. Each glyph row is a single byte where
/// bit 7 (MSB) = leftmost pixel. A set bit renders as `fg`, clear bit as `bg`.
///
/// When the target provides a direct buffer via `buffer_mut()`, this writes
/// pixels directly into the buffer — no per-pixel function call overhead.
pub fn render_char<D: DrawTarget>(
    target: &mut D,
    x: usize,
    y: usize,
    c: char,
    fg: Color,
    bg: Color,
) {
    let glyph = unicode_font::get_glyph(c);

    let stride = target.stride();
    let bpp = target.bytes_per_pixel();
    let tw = target.width();
    let th = target.height();
    let fg_bgr = color_to_bgr32(fg);
    let bg_bgr = color_to_bgr32(bg);

    // Fast path: direct buffer writes
    if let Some(buf) = target.buffer_mut() {
        for (row_idx, &row_bits) in glyph.iter().enumerate() {
            let py = y + row_idx;
            if py >= th {
                break;
            }
            let row_base = py * stride * bpp;
            for col_idx in 0..FONT_WIDTH {
                let px = x + col_idx;
                if px >= tw {
                    break;
                }
                let offset = row_base + px * bpp;
                // Test bit (MSB = leftmost pixel): bit 7 for col 0, bit 6 for col 1, etc.
                let is_set = (row_bits >> (7 - col_idx)) & 1 != 0;
                if offset + 3 < buf.len() {
                    let pixel = if is_set { &fg_bgr } else { &bg_bgr };
                    // Use write_volatile to ensure the write hits the backing
                    // store (important for memory-mapped framebuffers).
                    unsafe {
                        core::ptr::write_volatile(buf.as_mut_ptr().add(offset) as *mut [u8; 4], *pixel);
                    }
                }
            }
        }
        return;
    }

    // Slow fallback: per-pixel function calls (backward compat).
    for (row_idx, &row_bits) in glyph.iter().enumerate() {
        for col_idx in 0..FONT_WIDTH {
            let px = x + col_idx;
            let py = y + row_idx;
            let is_set = (row_bits >> (7 - col_idx)) & 1 != 0;
            if is_set {
                target.put_pixel(px, py, fg.r, fg.g, fg.b);
            } else {
                target.put_pixel(px, py, bg.r, bg.g, bg.b);
            }
        }
    }
}

/// Fill a rectangular region with a solid colour.
///
/// Fast path: when `buffer_mut()` is available, writes entire scanlines with
/// `copy_nonoverlapping` (memcpy) — one call per row instead of W*H put_pixel calls.
pub fn fill_rect<D: DrawTarget>(
    target: &mut D,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    color: Color,
) {
    if w == 0 || h == 0 {
        return;
    }

    let stride = target.stride();
    let bpp = target.bytes_per_pixel();
    let tw = target.width();
    let th = target.height();

    // Fast path: direct buffer writes with scanline memcpy.
    if let Some(buf) = target.buffer_mut() {
        // Build one scanline worth of pixel data, then memcpy it to each row.
        let clamped_w = w.min(tw.saturating_sub(x));
        let pixel = color_to_bgr32(color);

        // Write the first row pixel-by-pixel into the buffer.
        let first_y = y;
        if first_y < th {
            let row_base = first_y * stride * bpp;
            for col in 0..clamped_w {
                let px = x + col;
                let offset = row_base + px * bpp;
                if offset + 3 < buf.len() {
                    unsafe {
                        core::ptr::write_volatile(
                            buf.as_mut_ptr().add(offset) as *mut [u8; 4],
                            pixel,
                        );
                    }
                }
            }

            // For remaining rows, copy the first row's span.
            let src_start = row_base + x * bpp;
            let span_bytes = clamped_w * bpp;
            for row_idx in 1..h {
                let py = y + row_idx;
                if py >= th {
                    break;
                }
                let dst_start = py * stride * bpp + x * bpp;
                if dst_start + span_bytes <= buf.len() && src_start + span_bytes <= buf.len() {
                    unsafe {
                        // copy_nonoverlapping: rows never overlap since py > first_y.
                        core::ptr::copy_nonoverlapping(
                            buf.as_ptr().add(src_start),
                            buf.as_mut_ptr().add(dst_start),
                            span_bytes,
                        );
                    }
                }
            }
        }
        return;
    }

    // Slow fallback: per-pixel writes.
    for py in y..y + h {
        for px in x..x + w {
            target.put_pixel(px, py, color.r, color.g, color.b);
        }
    }
}
