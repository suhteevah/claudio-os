//! Timing/profiling — disabled for no_std bare metal.
//! The real module uses thread_local! and std::time which need std.

/// A pass identifier for timing purposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PassTime(u8);

impl PassTime {
    fn idx(self) -> usize { self.0 as usize }
}

impl core::fmt::Display for PassTime {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "pass{}", self.0)
    }
}

/// Timing token — no-op on no_std.
pub struct TimingToken;

/// Start timing a pass — returns immediately (no-op).
pub fn take_current() -> PassTimes { PassTimes }

/// Accumulated timing data — empty on no_std.
pub struct PassTimes;

impl core::fmt::Display for PassTimes {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "(timing disabled)")
    }
}
