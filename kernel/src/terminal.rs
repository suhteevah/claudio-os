//! Bridges the `claudio_terminal` crate to the kernel's GOP framebuffer.
//!
//! Provides [`FramebufferDrawTarget`] which implements
//! [`claudio_terminal::DrawTarget`] by forwarding pixel writes to
//! [`crate::framebuffer::put_pixel`].

use crate::framebuffer;

/// A [`claudio_terminal::DrawTarget`] backed by the GOP framebuffer.
///
/// This is a zero-size handle — all state lives in the global `FB` mutex
/// inside the framebuffer module.
pub struct FramebufferDrawTarget;

impl claudio_terminal::DrawTarget for FramebufferDrawTarget {
    #[inline]
    fn put_pixel(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8) {
        framebuffer::put_pixel(x, y, r, g, b);
    }

    fn width(&self) -> usize {
        framebuffer::width()
    }

    fn height(&self) -> usize {
        framebuffer::height()
    }
}
