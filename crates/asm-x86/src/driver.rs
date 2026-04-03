//! Top-level assemble + execute API.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::encoder::encode_instruction;
use crate::lexer::tokenize;
use crate::parser::{parse, AsmProgram, DataItem, Directive, Statement};
use crate::relocations::{resolve_fixups, Fixup, SymbolTable};

/// Error from assembly.
#[derive(Debug)]
pub struct AsmError {
    pub message: String,
}

impl core::fmt::Display for AsmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "asm error: {}", self.message)
    }
}

/// Result of successful assembly.
pub struct AssembledProgram {
    /// Raw machine code bytes.
    pub code: Vec<u8>,
    /// Data section bytes.
    pub data: Vec<u8>,
    /// BSS size in bytes.
    pub bss_size: usize,
    /// Entry point offset (offset of "global" label, or 0).
    pub entry_offset: usize,
    /// Symbol table for debugging.
    pub symbols: SymbolTable,
}

/// Assemble x86_64 assembly source into machine code.
pub fn assemble(source: &str) -> Result<AssembledProgram, AsmError> {
    let tokens = tokenize(source).map_err(|e| AsmError { message: e })?;
    let program = parse(&tokens).map_err(|e| AsmError { message: e })?;
    assemble_program(&program)
}

fn assemble_program(program: &AsmProgram) -> Result<AssembledProgram, AsmError> {
    let mut code = Vec::new();
    let mut data = Vec::new();
    let mut bss_size: usize = 0;
    let mut symbols = SymbolTable::new();
    let mut constants: BTreeMap<String, i64> = BTreeMap::new();
    let mut fixups: Vec<Fixup> = Vec::new();
    let mut global_name: Option<String> = None;

    // Current section: 0 = text, 1 = data, 2 = bss
    let mut section = 0u8;

    for stmt in &program.statements {
        match stmt {
            Statement::Label(name) => {
                let offset = match section {
                    0 => code.len(),
                    1 => data.len(),
                    _ => bss_size,
                };
                symbols.insert(name.clone(), offset);
            }
            Statement::Directive(dir) => match dir {
                Directive::Section(name) => {
                    section = match name.as_str() {
                        "text" => 0,
                        "data" | "rodata" => 1,
                        "bss" => 2,
                        _ => 0,
                    };
                }
                Directive::Db(items) => {
                    for item in items {
                        match item {
                            DataItem::Byte(b) => data.push(*b),
                            DataItem::Bytes(bs) => data.extend_from_slice(bs),
                        }
                    }
                }
                Directive::Dw(vals) => {
                    for v in vals {
                        data.extend_from_slice(&(*v as i16).to_le_bytes());
                    }
                }
                Directive::Dd(vals) => {
                    for v in vals {
                        data.extend_from_slice(&(*v as i32).to_le_bytes());
                    }
                }
                Directive::Dq(vals) => {
                    for v in vals {
                        data.extend_from_slice(&v.to_le_bytes());
                    }
                }
                Directive::Resb(n) => bss_size += *n as usize,
                Directive::Resw(n) => bss_size += *n as usize * 2,
                Directive::Resd(n) => bss_size += *n as usize * 4,
                Directive::Resq(n) => bss_size += *n as usize * 8,
                Directive::Equ(name, val) => {
                    constants.insert(name.clone(), *val);
                }
                Directive::Global(name) => {
                    global_name = Some(name.clone());
                }
                Directive::Extern(_) => {
                    // External symbols — would need a linker, just record them
                }
                Directive::Align(boundary) => {
                    let b = *boundary as usize;
                    if b > 0 {
                        let target = &mut match section {
                            0 => &mut code,
                            _ => &mut data,
                        };
                        while target.len() % b != 0 {
                            target.push(0x90); // NOP padding for code, 0 for data
                        }
                    }
                }
                Directive::Times(count, inner) => {
                    if let Statement::Instruction(ref inst) = **inner {
                        for _ in 0..*count {
                            let encoded = encode_instruction(inst, code.len())
                                .map_err(|e| AsmError { message: e })?;
                            // Adjust fixup offsets
                            for mut f in encoded.fixups {
                                f.offset += code.len();
                                fixups.push(f);
                            }
                            code.extend_from_slice(&encoded.bytes);
                        }
                    }
                }
            },
            Statement::Instruction(inst) => {
                let encoded = encode_instruction(inst, code.len())
                    .map_err(|e| AsmError { message: e })?;
                // Adjust fixup offsets relative to code start
                for mut f in encoded.fixups {
                    f.offset += code.len();
                    fixups.push(f);
                }
                code.extend_from_slice(&encoded.bytes);
            }
        }
    }

    // Resolve all fixups
    resolve_fixups(&mut code, &fixups, &symbols, &constants, 0)
        .map_err(|e| AsmError { message: e })?;

    let entry_offset = global_name
        .as_ref()
        .and_then(|n| symbols.get(n).copied())
        .unwrap_or(0);

    Ok(AssembledProgram {
        code,
        data,
        bss_size,
        entry_offset,
        symbols,
    })
}

/// Execute an assembled program by copying code to heap and calling it.
///
/// # Safety
/// The assembled code runs with full kernel privileges. Only execute trusted code.
pub unsafe fn execute(program: &AssembledProgram) -> i64 {
    if program.code.is_empty() {
        return 0;
    }

    log::info!(
        "[asm] executing {} bytes of machine code (entry offset: {})",
        program.code.len(),
        program.entry_offset
    );

    // Allocate executable memory (on bare metal, heap is executable)
    let mut code_mem = vec![0u8; program.code.len()];
    core::ptr::copy_nonoverlapping(
        program.code.as_ptr(),
        code_mem.as_mut_ptr(),
        program.code.len(),
    );

    let base = code_mem.as_ptr();
    let entry = base.add(program.entry_offset);

    // Leak so it doesn't get freed while executing
    core::mem::forget(code_mem);

    // Call as fn() -> i64 (System V: return value in RAX)
    let func: fn() -> i64 = core::mem::transmute(entry);
    func()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assemble_simple() {
        let result = assemble("nop\nret\n");
        assert!(result.is_ok());
        let prog = result.unwrap();
        assert_eq!(prog.code, vec![0x90, 0xC3]);
    }

    #[test]
    fn test_assemble_mov_ret() {
        let result = assemble("mov eax, 42\nret\n");
        assert!(result.is_ok());
        let prog = result.unwrap();
        // Should end with C3 (ret)
        assert_eq!(*prog.code.last().unwrap(), 0xC3);
    }

    #[test]
    fn test_assemble_label_jump() {
        let source = "start:\n  jmp start\n";
        let result = assemble(source);
        assert!(result.is_ok());
        let prog = result.unwrap();
        // jmp rel32 backwards: E9 xx xx xx xx
        assert_eq!(prog.code[0], 0xE9);
        // Relative offset should be -5 (jump back to start)
        let rel = i32::from_le_bytes([prog.code[1], prog.code[2], prog.code[3], prog.code[4]]);
        assert_eq!(rel, -5);
    }

    #[test]
    fn test_assemble_data_section() {
        let source = "section .data\ndb 0x48, 0x65, 0x6C\n";
        let result = assemble(source);
        assert!(result.is_ok());
        let prog = result.unwrap();
        assert_eq!(prog.data, vec![0x48, 0x65, 0x6C]);
    }

    #[test]
    fn test_assemble_push_pop() {
        let result = assemble("push rbp\nmov rbp, rsp\npop rbp\nret\n");
        assert!(result.is_ok());
        let prog = result.unwrap();
        assert_eq!(prog.code[0], 0x55); // push rbp
    }
}
