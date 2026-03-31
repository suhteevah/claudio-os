//! Cranelift control plane — minimal no_std stub.
//! The real implementation provides chaos mode for fuzzing.
//! We just need the ControlPlane type to exist (zero-sized, no-op).

#![no_std]

extern crate alloc;

/// A zero-sized control plane that does nothing.
/// In the real crate, this drives randomized compilation decisions for fuzzing.
#[derive(Debug, Clone)]
pub struct ControlPlane;

impl ControlPlane {
    /// Create a new control plane.
    pub fn new() -> Self {
        Self
    }

    /// Shuffle a mutable slice (no-op in this stub).
    pub fn shuffle<T>(&mut self, _slice: &mut [T]) {}

    /// Return an iterator that yields items in the original order (no shuffling).
    pub fn shuffled<I: Iterator>(&mut self, iter: I) -> alloc::vec::Vec<I::Item> {
        iter.collect()
    }

    /// Get a boolean decision (always returns false in this stub).
    pub fn get_decision(&mut self) -> bool {
        false
    }

    /// Get an arbitrary value (returns the default in this stub).
    pub fn get_arbitrary<T: Default>(&mut self) -> T {
        T::default()
    }
}

impl Default for ControlPlane {
    fn default() -> Self {
        Self
    }
}
