//! f32/f64 math compatibility for no_std via libm.

/// Float extension trait for no_std.
pub trait F32Ext {
    fn trunc(self) -> f32;
    fn sqrt(self) -> f32;
    fn ceil(self) -> f32;
    fn floor(self) -> f32;
    fn round_ties_even(self) -> f32;
}

/// Float extension trait for no_std.
pub trait F64Ext {
    fn trunc(self) -> f64;
    fn sqrt(self) -> f64;
    fn ceil(self) -> f64;
    fn floor(self) -> f64;
    fn round_ties_even(self) -> f64;
}

impl F32Ext for f32 {
    fn trunc(self) -> f32 { libm::truncf(self) }
    fn sqrt(self) -> f32 { libm::sqrtf(self) }
    fn ceil(self) -> f32 { libm::ceilf(self) }
    fn floor(self) -> f32 { libm::floorf(self) }
    fn round_ties_even(self) -> f32 { libm::rintf(self) }
}

impl F64Ext for f64 {
    fn trunc(self) -> f64 { libm::trunc(self) }
    fn sqrt(self) -> f64 { libm::sqrt(self) }
    fn ceil(self) -> f64 { libm::ceil(self) }
    fn floor(self) -> f64 { libm::floor(self) }
    fn round_ties_even(self) -> f64 { libm::rint(self) }
}
