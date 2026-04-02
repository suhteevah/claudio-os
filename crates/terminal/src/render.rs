//! Font rendering using noto-sans-mono-bitmap.
//!
//! Provides character-level rendering to any [`DrawTarget`](super::DrawTarget)
//! using pre-rasterised glyphs from the Noto Sans Mono font.
//!
//! ## Fast-path rendering
//!
//! When the `DrawTarget` exposes a contiguous pixel buffer via `buffer_mut()`,
//! `render_char` and `fill_rect` bypass individual `put_pixel` calls entirely,
//! writing 4-byte BGR32 pixels directly into the buffer using
//! `core::ptr::write_volatile`. This is the TempleOS-style approach: treat the
//! framebuffer as a flat array and memcpy scanlines.

use noto_sans_mono_bitmap::{get_raster, get_raster_width, FontWeight, RasterHeight};

/// Height of each character cell in pixels.
pub const FONT_HEIGHT: usize = 16;

/// Width of each character cell in pixels (monospace — constant for all glyphs).
pub const FONT_WIDTH: usize = get_raster_width(FontWeight::Regular, RasterHeight::Size16);

/// An RGB colour triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Pack into a BGR32 4-byte array (UEFI GOP pixel format: B, G, R, 0).
    #[inline(always)]
    pub const fn to_bgr32(self) -> [u8; 4] {
        [self.b, self.g, self.r, 0]
    }

    // Standard 8-colour palette (SGR 30–37) — CGA-ish values.
    pub const BLACK: Self = Self::new(0, 0, 0);
    pub const RED: Self = Self::new(204, 0, 0);
    pub const GREEN: Self = Self::new(0, 204, 0);
    pub const YELLOW: Self = Self::new(204, 204, 0);
    pub const BLUE: Self = Self::new(0, 0, 204);
    pub const MAGENTA: Self = Self::new(204, 0, 204);
    pub const CYAN: Self = Self::new(0, 204, 204);
    pub const WHITE: Self = Self::new(204, 204, 204);

    // Bright variants (SGR 90–97).
    pub const BRIGHT_BLACK: Self = Self::new(128, 128, 128);
    pub const BRIGHT_RED: Self = Self::new(255, 85, 85);
    pub const BRIGHT_GREEN: Self = Self::new(85, 255, 85);
    pub const BRIGHT_YELLOW: Self = Self::new(255, 255, 85);
    pub const BRIGHT_BLUE: Self = Self::new(85, 85, 255);
    pub const BRIGHT_MAGENTA: Self = Self::new(255, 85, 255);
    pub const BRIGHT_CYAN: Self = Self::new(85, 255, 255);
    pub const BRIGHT_WHITE: Self = Self::new(255, 255, 255);

    // Semantic aliases used as defaults.
    pub const DEFAULT_FG: Self = Self::WHITE;
    pub const DEFAULT_BG: Self = Self::new(16, 16, 16);
}

/// Render a single character glyph into `target` at pixel position (`x`, `y`).
///
/// When the target provides a direct buffer via `buffer_mut()`, this writes
/// pixels directly into the buffer — no per-pixel function call overhead.
/// Missing glyphs are replaced with `'?'`.
pub fn render_char<D: super::DrawTarget>(
    target: &mut D,
    x: usize,
    y: usize,
    c: char,
    fg: Color,
    bg: Color,
) {
    let raster = get_raster(c, FontWeight::Regular, RasterHeight::Size16)
        .unwrap_or_else(|| {
            get_raster('?', FontWeight::Regular, RasterHeight::Size16)
                .expect("fallback glyph '?' must exist")
        });

    let stride = target.stride();
    let bpp = target.bytes_per_pixel();
    let tw = target.width();
    let th = target.height();
    let fg_bgr = fg.to_bgr32();
    let bg_bgr = bg.to_bgr32();

    // Fast path: direct buffer writes
    if let Some(buf) = target.buffer_mut() {
        for (row_idx, row) in raster.raster().iter().enumerate() {
            let py = y + row_idx;
            if py >= th {
                break;
            }
            let row_base = py * stride * bpp;
            for (col_idx, &intensity) in row.iter().enumerate() {
                let px = x + col_idx;
                if px >= tw {
                    break;
                }
                let offset = row_base + px * bpp;
                // Safety: we bounds-checked px < tw and py < th, and the buffer
                // is sized to stride * height * bpp.
                if offset + 3 < buf.len() {
                    let pixel = if intensity > 128 { &fg_bgr } else { &bg_bgr };
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
    for (row_idx, row) in raster.raster().iter().enumerate() {
        for (col_idx, &intensity) in row.iter().enumerate() {
            let px = x + col_idx;
            let py = y + row_idx;
            if intensity > 128 {
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
pub fn fill_rect<D: super::DrawTarget>(
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
        // For a typical 1280-wide fill, this is ~5KB on the stack — fine.
        let clamped_w = w.min(tw.saturating_sub(x));
        let pixel = color.to_bgr32();

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
