//! Timing/profiling — no-op stubs for no_std bare metal.

/// Timing token — drops silently (no measurement).
/// No-op timing token.
pub struct TimingToken;

/// Accumulated pass times — empty on no_std.
/// Empty pass times.
pub struct PassTimes;

impl core::fmt::Display for PassTimes {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "(timing disabled)")
    }
}

/// Take accumulated timing data.
/// No-op timing function.
pub fn take_current() -> PassTimes { PassTimes }

// All pass timing functions — return no-op tokens
/// No-op timing function.
pub fn canonicalize_nans() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn compile() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn domtree() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn egraph() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn flowgraph() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn layout_renumber() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn loop_analysis() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn regalloc() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn regalloc_checker() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn remove_constant_phis() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn store_incremental_cache() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn try_incremental_cache() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn unreachable_code() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn vcode_emit() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn vcode_emit_finish() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn vcode_lower() -> TimingToken { TimingToken }
/// No-op timing function.
pub fn verifier() -> TimingToken { TimingToken }
