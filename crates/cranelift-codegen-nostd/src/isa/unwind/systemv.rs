//! System V unwind info stubs for no_std.
extern crate alloc;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnwindInfo;

#[derive(Debug)]
pub struct RegisterMappingError;

pub fn create_unwind_info_from_insts(
    _insts: &[crate::isa::unwind::UnwindInst],
    _len: usize,
    _mapper: &dyn RegisterMapper,
) -> crate::CodegenResult<UnwindInfo> {
    Ok(UnwindInfo)
}

pub fn create_cie() -> Vec<u8> { Vec::new() }

pub trait RegisterMapper {
    fn map(&self, reg: crate::machinst::RealReg) -> Result<u16, RegisterMappingError>;
}

pub fn map_reg(_reg: crate::machinst::Reg) -> Result<DwarfReg, RegisterMappingError> {
    Err(RegisterMappingError)
}

pub struct DwarfReg(pub u16);

pub fn caller_sp_to_cfa_offset() -> i64 { 0 }
