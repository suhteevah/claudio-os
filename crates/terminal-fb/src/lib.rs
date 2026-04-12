//! terminal-fb — ClaudioOS framebuffer renderer for terminal-core.

#![no_std]
extern crate alloc;

pub mod render;
pub mod terminus;
pub mod unicode_font;
pub mod pane_renderer;

pub use render::{DrawTarget, fill_rect, render_char, FONT_HEIGHT, FONT_WIDTH, pixels_to_cells, color_to_bgr32};
