//! Timing/profiling — no-op stubs for no_std bare metal.

/// Timing token — drops silently (no measurement).
pub struct TimingToken;

/// Accumulated pass times — empty on no_std.
pub struct PassTimes;

impl core::fmt::Display for PassTimes {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "(timing disabled)")
    }
}

/// Take accumulated timing data.
pub fn take_current() -> PassTimes { PassTimes }

// All pass timing functions — return no-op tokens
pub fn canonicalize_nans() -> TimingToken { TimingToken }
pub fn compile() -> TimingToken { TimingToken }
pub fn domtree() -> TimingToken { TimingToken }
pub fn egraph() -> TimingToken { TimingToken }
pub fn flowgraph() -> TimingToken { TimingToken }
pub fn layout_renumber() -> TimingToken { TimingToken }
pub fn loop_analysis() -> TimingToken { TimingToken }
pub fn regalloc() -> TimingToken { TimingToken }
pub fn regalloc_checker() -> TimingToken { TimingToken }
pub fn remove_constant_phis() -> TimingToken { TimingToken }
pub fn store_incremental_cache() -> TimingToken { TimingToken }
pub fn try_incremental_cache() -> TimingToken { TimingToken }
pub fn unreachable_code() -> TimingToken { TimingToken }
pub fn vcode_emit() -> TimingToken { TimingToken }
pub fn vcode_emit_finish() -> TimingToken { TimingToken }
pub fn vcode_lower() -> TimingToken { TimingToken }
pub fn verifier() -> TimingToken { TimingToken }
