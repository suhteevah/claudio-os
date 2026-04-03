//! Basic image viewer — BMP (24/32-bit uncompressed) and PPM (P6 binary).
//!
//! Parses image data in-memory and renders pixels directly to the framebuffer
//! back buffer using nearest-neighbour scaling to fit a target pane region.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use alloc::string::ToString;

// ---------------------------------------------------------------------------
// Decoded image
// ---------------------------------------------------------------------------

/// A decoded image in 32-bit RGBA (row-major, top-to-bottom).
pub struct DecodedImage {
    pub width: usize,
    pub height: usize,
    /// Row-major pixel data: [R, G, B, A] per pixel, top row first.
    pub pixels: Vec<u8>,
}

impl DecodedImage {
    /// Get the (R, G, B) at the given coordinates.
    #[inline]
    pub fn pixel(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let idx = (y * self.width + x) * 4;
        if idx + 2 < self.pixels.len() {
            (self.pixels[idx], self.pixels[idx + 1], self.pixels[idx + 2])
        } else {
            (0, 0, 0)
        }
    }
}

// ---------------------------------------------------------------------------
// BMP parser
// ---------------------------------------------------------------------------

/// Parse an uncompressed BMP file (24-bit or 32-bit).
pub fn parse_bmp(data: &[u8]) -> Result<DecodedImage, &'static str> {
    if data.len() < 54 {
        return Err("BMP: file too small for header");
    }

    // BMP magic.
    if data[0] != b'B' || data[1] != b'M' {
        return Err("BMP: invalid magic (expected BM)");
    }

    // File header.
    let data_offset = read_u32_le(data, 10) as usize;

    // DIB header (BITMAPINFOHEADER).
    let dib_size = read_u32_le(data, 14);
    if dib_size < 40 {
        return Err("BMP: unsupported DIB header (need BITMAPINFOHEADER, 40+ bytes)");
    }

    let width = read_i32_le(data, 18);
    let height = read_i32_le(data, 22);
    let bpp = read_u16_le(data, 28);
    let compression = read_u32_le(data, 30);

    if width <= 0 {
        return Err("BMP: invalid width");
    }
    let width = width as usize;

    // Height can be negative (top-down) or positive (bottom-up).
    let top_down = height < 0;
    let height = if height < 0 { (-height) as usize } else { height as usize };

    // Only uncompressed (0) or BI_BITFIELDS (3) for 32-bit.
    if compression != 0 && compression != 3 {
        return Err("BMP: compressed BMPs not supported (only BI_RGB / BI_BITFIELDS)");
    }

    if bpp != 24 && bpp != 32 {
        return Err("BMP: only 24-bit and 32-bit supported");
    }

    let bytes_per_pixel = (bpp as usize) / 8;
    // BMP rows are padded to 4-byte boundaries.
    let row_size = ((width * bytes_per_pixel + 3) / 4) * 4;

    let pixel_data_len = row_size * height;
    if data_offset + pixel_data_len > data.len() {
        return Err("BMP: pixel data exceeds file size");
    }

    let mut pixels = Vec::with_capacity(width * height * 4);

    for row in 0..height {
        // BMP is bottom-up by default; top-down if height was negative.
        let src_row = if top_down { row } else { height - 1 - row };
        let row_start = data_offset + src_row * row_size;

        for col in 0..width {
            let px = row_start + col * bytes_per_pixel;
            // BMP stores BGR(A).
            let b = data[px];
            let g = data[px + 1];
            let r = data[px + 2];
            let a = if bytes_per_pixel == 4 { data[px + 3] } else { 255 };
            pixels.push(r);
            pixels.push(g);
            pixels.push(b);
            pixels.push(a);
        }
    }

    Ok(DecodedImage { width, height, pixels })
}

// ---------------------------------------------------------------------------
// PPM (P6) parser
// ---------------------------------------------------------------------------

/// Parse a PPM P6 (binary) image.
pub fn parse_ppm(data: &[u8]) -> Result<DecodedImage, &'static str> {
    if data.len() < 7 {
        return Err("PPM: file too small");
    }

    // Magic: "P6\n" or "P6 " or "P6\r\n".
    if data[0] != b'P' || data[1] != b'6' {
        return Err("PPM: invalid magic (expected P6)");
    }

    let mut pos = 2;

    // Skip whitespace/comments.
    pos = skip_ppm_whitespace(data, pos)?;

    // Read width.
    let (w, new_pos) = read_ppm_number(data, pos)?;
    pos = new_pos;
    pos = skip_ppm_whitespace(data, pos)?;

    // Read height.
    let (h, new_pos) = read_ppm_number(data, pos)?;
    pos = new_pos;
    pos = skip_ppm_whitespace(data, pos)?;

    // Read max value.
    let (max_val, new_pos) = read_ppm_number(data, pos)?;
    pos = new_pos;

    if max_val == 0 || max_val > 255 {
        return Err("PPM: max color value must be 1-255");
    }

    // Exactly one whitespace character after max value before pixel data.
    if pos >= data.len() || !is_whitespace(data[pos]) {
        return Err("PPM: expected whitespace after max value");
    }
    pos += 1;

    let width = w as usize;
    let height = h as usize;
    let pixel_bytes = width * height * 3;

    if pos + pixel_bytes > data.len() {
        return Err("PPM: pixel data truncated");
    }

    let mut pixels = Vec::with_capacity(width * height * 4);
    let scale = if max_val == 255 { false } else { true };

    for i in 0..(width * height) {
        let idx = pos + i * 3;
        let (r, g, b) = if scale {
            (
                ((data[idx] as u32 * 255) / max_val as u32) as u8,
                ((data[idx + 1] as u32 * 255) / max_val as u32) as u8,
                ((data[idx + 2] as u32 * 255) / max_val as u32) as u8,
            )
        } else {
            (data[idx], data[idx + 1], data[idx + 2])
        };
        pixels.push(r);
        pixels.push(g);
        pixels.push(b);
        pixels.push(255);
    }

    Ok(DecodedImage { width, height, pixels })
}

// ---------------------------------------------------------------------------
// Auto-detect format
// ---------------------------------------------------------------------------

/// Try to decode image data, auto-detecting BMP or PPM.
pub fn decode_image(data: &[u8]) -> Result<DecodedImage, &'static str> {
    if data.len() >= 2 {
        if data[0] == b'B' && data[1] == b'M' {
            return parse_bmp(data);
        }
        if data[0] == b'P' && data[1] == b'6' {
            return parse_ppm(data);
        }
    }
    Err("Unknown image format (supported: BMP, PPM/P6)")
}

// ---------------------------------------------------------------------------
// Renderer — scale and blit to framebuffer back buffer
// ---------------------------------------------------------------------------

/// Render a decoded image into a rectangular region of a raw pixel buffer
/// (the framebuffer back buffer). Uses nearest-neighbour scaling.
///
/// Parameters:
///   image       — decoded image
///   buf         — back buffer bytes (BGR32 pixel format)
///   buf_stride  — stride of the back buffer in pixels
///   bpp         — bytes per pixel of the back buffer (usually 4)
///   region_x/y  — top-left corner of the target region in pixels
///   region_w/h  — size of the target region in pixels
pub fn render_image_to_buffer(
    image: &DecodedImage,
    buf: &mut [u8],
    buf_stride: usize,
    bpp: usize,
    region_x: usize,
    region_y: usize,
    region_w: usize,
    region_h: usize,
) {
    if image.width == 0 || image.height == 0 || region_w == 0 || region_h == 0 {
        return;
    }

    // Compute the scaling to fit while preserving aspect ratio.
    let scale_x = (image.width as f64) / (region_w as f64);
    let scale_y = (image.height as f64) / (region_h as f64);
    let scale = if scale_x > scale_y { scale_x } else { scale_y };

    // Actual rendered size (may be smaller than region if aspect ratios differ).
    let render_w = ((image.width as f64) / scale) as usize;
    let render_h = ((image.height as f64) / scale) as usize;

    // Center within region.
    let offset_x = (region_w.saturating_sub(render_w)) / 2;
    let offset_y = (region_h.saturating_sub(render_h)) / 2;

    // Clear the region to black first.
    for py in 0..region_h {
        let row_offset = ((region_y + py) * buf_stride + region_x) * bpp;
        let end = row_offset + region_w * bpp;
        if end <= buf.len() {
            for b in &mut buf[row_offset..end] {
                *b = 0;
            }
        }
    }

    // Nearest-neighbour blit.
    for py in 0..render_h {
        let src_y = ((py as f64) * scale) as usize;
        if src_y >= image.height {
            continue;
        }
        let dest_row = region_y + offset_y + py;
        for px in 0..render_w {
            let src_x = ((px as f64) * scale) as usize;
            if src_x >= image.width {
                continue;
            }
            let (r, g, b) = image.pixel(src_x, src_y);
            let dest_col = region_x + offset_x + px;
            let offset = (dest_row * buf_stride + dest_col) * bpp;
            if offset + 2 < buf.len() {
                // BGR32 pixel format (UEFI GOP standard).
                buf[offset] = b;
                buf[offset + 1] = g;
                buf[offset + 2] = r;
            }
        }
    }
}

/// Convenience: render to the global framebuffer back buffer at a pane viewport.
pub fn render_image_to_pane(
    image: &DecodedImage,
    pane_x: usize,
    pane_y: usize,
    pane_w: usize,
    pane_h: usize,
) {
    crate::framebuffer::with_back_buffer(|buf, width, _height, stride, bpp| {
        let _ = width; // suppress unused
        render_image_to_buffer(image, buf, stride, bpp, pane_x, pane_y, pane_w, pane_h);
    });
    // Blit the changed rows.
    crate::framebuffer::blit_rows(pane_y, pane_y + pane_h);
}

// ---------------------------------------------------------------------------
// Shell command handler
// ---------------------------------------------------------------------------

/// Handle the `view <path>` shell command.
///
/// In the current stub VFS, this returns an error since we don't have real
/// filesystem access yet. The parsing + rendering code is fully functional
/// and can be wired to FAT32/fs-persist when available.
pub fn handle_command(args: &str) -> String {
    let path = args.trim();
    if path.is_empty() {
        return "Usage: view <image_path>\nSupported formats: .bmp (24/32-bit), .ppm (P6)\n"
            .to_string();
    }

    // Determine format from extension.
    let lower = path.to_lowercase();
    if !lower.ends_with(".bmp") && !lower.ends_with(".ppm") {
        return format!(
            "Unsupported format: {}\nSupported: .bmp (24/32-bit uncompressed), .ppm (P6 binary)\n",
            path
        );
    }

    // In a full implementation, we'd read from FAT32 here:
    //   let data = crate::fs_persist::read_file(path)?;
    //   let img = decode_image(&data)?;
    //   render_image_to_pane(&img, ...pane viewport...);
    //
    // For now, report that the viewer is ready but the filesystem isn't wired.
    format!(
        "Image viewer ready.\n\
         File: {}\n\
         Status: FAT32 filesystem not yet wired — use `view` after fs-persist integration.\n\
         Supported formats:\n\
         \x1b[36m  .bmp\x1b[0m  24-bit or 32-bit uncompressed (BI_RGB / BI_BITFIELDS)\n\
         \x1b[36m  .ppm\x1b[0m  P6 binary (RGB, max value 1-255)\n",
        path
    )
}

/// Check if a filename has a supported image extension.
pub fn is_image_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".bmp") || lower.ends_with(".ppm")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[inline]
fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    (data[offset] as u16) | ((data[offset + 1] as u16) << 8)
}

#[inline]
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    (data[offset] as u32)
        | ((data[offset + 1] as u32) << 8)
        | ((data[offset + 2] as u32) << 16)
        | ((data[offset + 3] as u32) << 24)
}

#[inline]
fn read_i32_le(data: &[u8], offset: usize) -> i32 {
    read_u32_le(data, offset) as i32
}

#[inline]
fn is_whitespace(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r'
}

fn skip_ppm_whitespace(data: &[u8], mut pos: usize) -> Result<usize, &'static str> {
    loop {
        if pos >= data.len() {
            return Err("PPM: unexpected end of file");
        }
        if data[pos] == b'#' {
            // Skip comment line.
            while pos < data.len() && data[pos] != b'\n' {
                pos += 1;
            }
            if pos < data.len() {
                pos += 1; // skip the newline
            }
        } else if is_whitespace(data[pos]) {
            pos += 1;
        } else {
            break;
        }
    }
    Ok(pos)
}

fn read_ppm_number(data: &[u8], mut pos: usize) -> Result<(u32, usize), &'static str> {
    if pos >= data.len() || !data[pos].is_ascii_digit() {
        return Err("PPM: expected number");
    }
    let mut val: u32 = 0;
    while pos < data.len() && data[pos].is_ascii_digit() {
        val = val
            .checked_mul(10)
            .and_then(|v| v.checked_add((data[pos] - b'0') as u32))
            .ok_or("PPM: number overflow")?;
        pos += 1;
    }
    Ok((val, pos))
}
