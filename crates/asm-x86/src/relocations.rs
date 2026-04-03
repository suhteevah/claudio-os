//! Label resolution and fixups for forward/backward references.
//!
//! Two-pass assembly: first collect labels and encode (with placeholder offsets),
//! then resolve all fixups.

use alloc::collections::BTreeMap;
use alloc::string::String;

/// A fixup to be applied after all labels are known.
#[derive(Debug, Clone)]
pub struct Fixup {
    /// Byte offset within the output where the fixup value goes.
    pub offset: usize,
    /// The symbol/label being referenced.
    pub symbol: String,
    /// What kind of fixup (relative or absolute).
    pub kind: FixupKind,
}

#[derive(Debug, Clone)]
pub enum FixupKind {
    /// 32-bit PC-relative offset. `from` is the address of the instruction AFTER
    /// the 4-byte field (i.e., the next instruction's address).
    Rel32 { from: usize },
    /// 64-bit absolute address.
    Abs64,
}

/// Symbol table mapping label names to byte offsets in the output.
pub type SymbolTable = BTreeMap<String, usize>;

/// Resolve all fixups in `code` using the symbol table.
/// Also resolves `equ` constants from `constants`.
pub fn resolve_fixups(
    code: &mut [u8],
    fixups: &[Fixup],
    symbols: &SymbolTable,
    constants: &BTreeMap<String, i64>,
    base_address: usize,
) -> Result<(), String> {
    for fixup in fixups {
        // Try symbol table first, then constants
        let target = if let Some(&offset) = symbols.get(&fixup.symbol) {
            base_address + offset
        } else if let Some(&val) = constants.get(&fixup.symbol) {
            val as usize
        } else {
            return Err(alloc::format!("undefined symbol: {}", fixup.symbol));
        };

        match fixup.kind {
            FixupKind::Rel32 { from } => {
                let rel = (target as isize - (base_address + from) as isize) as i32;
                let off = fixup.offset;
                if off + 4 > code.len() {
                    return Err(alloc::format!(
                        "fixup offset {} out of bounds (code len {})",
                        off,
                        code.len()
                    ));
                }
                code[off..off + 4].copy_from_slice(&rel.to_le_bytes());
            }
            FixupKind::Abs64 => {
                let off = fixup.offset;
                if off + 8 > code.len() {
                    return Err(alloc::format!(
                        "fixup offset {} out of bounds for abs64",
                        off
                    ));
                }
                code[off..off + 8].copy_from_slice(&(target as u64).to_le_bytes());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_rel32() {
        // Simulate: at offset 1, a rel32 fixup to "target" which is at code offset 10
        // Instruction ends at offset 5 (from = 5)
        let mut code = vec![0xE9, 0, 0, 0, 0, 0x90, 0x90, 0x90, 0x90, 0x90];
        let fixups = alloc::vec![Fixup {
            offset: 1,
            symbol: String::from("target"),
            kind: FixupKind::Rel32 { from: 5 },
        }];
        let mut syms = SymbolTable::new();
        syms.insert(String::from("target"), 10);

        resolve_fixups(&mut code, &fixups, &syms, &BTreeMap::new(), 0).unwrap();
        // rel32 = target(10) - from(5) = 5
        let rel = i32::from_le_bytes([code[1], code[2], code[3], code[4]]);
        assert_eq!(rel, 5);
    }
}
