//! x64 System V unwind stubs for no_std.
extern crate alloc;
use alloc::vec::Vec;

pub struct RegisterMapper;

pub fn create_cie() -> Vec<u8> { Vec::new() }

pub fn map_reg(_reg: crate::machinst::Reg) -> Result<DwarfReg, crate::isa::unwind::systemv::RegisterMappingError> {
    Err(crate::isa::unwind::systemv::RegisterMappingError::InvalidRegister)
}

pub struct DwarfReg(pub u16);
