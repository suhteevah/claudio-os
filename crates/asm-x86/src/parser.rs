//! Parse assembly tokens into structured instructions.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use crate::lexer::{Token, SizeKind};

/// A parsed assembly program.
#[derive(Debug, Clone)]
pub struct AsmProgram {
    pub statements: Vec<Statement>,
}

/// A single assembly statement.
#[derive(Debug, Clone)]
pub enum Statement {
    /// Label definition
    Label(String),
    /// Instruction: mnemonic + operands
    Instruction(Instruction),
    /// Directive: section, db, dw, dd, dq, resb, equ, global, extern, align, times
    Directive(Directive),
}

#[derive(Debug, Clone)]
pub struct Instruction {
    pub mnemonic: String,
    pub operands: Vec<Operand>,
}

/// An instruction operand.
#[derive(Debug, Clone)]
pub enum Operand {
    /// Register operand (e.g., rax, ecx, r8)
    Register(Register),
    /// Immediate integer value
    Immediate(i64),
    /// Memory reference: [base + index*scale + disp]
    Memory(MemOperand),
    /// Symbol/label reference (resolved during assembly)
    Symbol(String),
}

#[derive(Debug, Clone)]
pub struct MemOperand {
    pub size: Option<SizeKind>,
    pub base: Option<Register>,
    pub index: Option<Register>,
    pub scale: u8, // 1, 2, 4, or 8
    pub disp: i64,
    pub rip_relative: bool,
    /// Symbol for RIP-relative or displacement
    pub symbol: Option<String>,
}

impl Default for MemOperand {
    fn default() -> Self {
        Self {
            size: None,
            base: None,
            index: None,
            scale: 1,
            disp: 0,
            rip_relative: false,
            symbol: None,
        }
    }
}

/// x86_64 register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Register {
    pub kind: RegKind,
    pub size: RegSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegSize {
    Bits8,
    Bits16,
    Bits32,
    Bits64,
    Xmm, // 128-bit SSE
}

impl RegSize {
    pub fn bits(self) -> u8 {
        match self {
            RegSize::Bits8 => 8,
            RegSize::Bits16 => 16,
            RegSize::Bits32 => 32,
            RegSize::Bits64 => 64,
            RegSize::Xmm => 128,
        }
    }
}

/// Register encoding: the 4-bit register number (low 3 bits for ModR/M, high bit for REX).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegKind {
    Rax,  // 0
    Rcx,  // 1
    Rdx,  // 2
    Rbx,  // 3
    Rsp,  // 4
    Rbp,  // 5
    Rsi,  // 6
    Rdi,  // 7
    R8,   // 8
    R9,   // 9
    R10,  // 10
    R11,  // 11
    R12,  // 12
    R13,  // 13
    R14,  // 14
    R15,  // 15
    Xmm0, Xmm1, Xmm2, Xmm3, Xmm4, Xmm5, Xmm6, Xmm7,
    Xmm8, Xmm9, Xmm10, Xmm11, Xmm12, Xmm13, Xmm14, Xmm15,
}

impl RegKind {
    /// 4-bit register encoding (low 3 bits = ModR/M field, bit 3 = REX extension).
    pub fn encoding(self) -> u8 {
        match self {
            RegKind::Rax => 0,
            RegKind::Rcx => 1,
            RegKind::Rdx => 2,
            RegKind::Rbx => 3,
            RegKind::Rsp => 4,
            RegKind::Rbp => 5,
            RegKind::Rsi => 6,
            RegKind::Rdi => 7,
            RegKind::R8 => 8,
            RegKind::R9 => 9,
            RegKind::R10 => 10,
            RegKind::R11 => 11,
            RegKind::R12 => 12,
            RegKind::R13 => 13,
            RegKind::R14 => 14,
            RegKind::R15 => 15,
            RegKind::Xmm0 => 0,
            RegKind::Xmm1 => 1,
            RegKind::Xmm2 => 2,
            RegKind::Xmm3 => 3,
            RegKind::Xmm4 => 4,
            RegKind::Xmm5 => 5,
            RegKind::Xmm6 => 6,
            RegKind::Xmm7 => 7,
            RegKind::Xmm8 => 8,
            RegKind::Xmm9 => 9,
            RegKind::Xmm10 => 10,
            RegKind::Xmm11 => 11,
            RegKind::Xmm12 => 12,
            RegKind::Xmm13 => 13,
            RegKind::Xmm14 => 14,
            RegKind::Xmm15 => 15,
        }
    }

    /// Whether this register requires a REX.B/R/X extension bit.
    pub fn needs_rex_ext(self) -> bool {
        self.encoding() >= 8
    }

    pub fn is_xmm(self) -> bool {
        matches!(
            self,
            RegKind::Xmm0 | RegKind::Xmm1 | RegKind::Xmm2 | RegKind::Xmm3
            | RegKind::Xmm4 | RegKind::Xmm5 | RegKind::Xmm6 | RegKind::Xmm7
            | RegKind::Xmm8 | RegKind::Xmm9 | RegKind::Xmm10 | RegKind::Xmm11
            | RegKind::Xmm12 | RegKind::Xmm13 | RegKind::Xmm14 | RegKind::Xmm15
        )
    }
}

/// Assembly directives.
#[derive(Debug, Clone)]
pub enum Directive {
    /// section .text / .data / .bss
    Section(String),
    /// db values...
    Db(Vec<DataItem>),
    /// dw values...
    Dw(Vec<i64>),
    /// dd values...
    Dd(Vec<i64>),
    /// dq values...
    Dq(Vec<i64>),
    /// resb count
    Resb(u64),
    /// resw count
    Resw(u64),
    /// resd count
    Resd(u64),
    /// resq count
    Resq(u64),
    /// name equ value
    Equ(String, i64),
    /// global name
    Global(String),
    /// extern name
    Extern(String),
    /// align boundary
    Align(u64),
    /// times count instruction
    Times(u64, Box<Statement>),
}

/// Data items for db directive (can be integers or string bytes).
#[derive(Debug, Clone)]
pub enum DataItem {
    Byte(u8),
    Bytes(Vec<u8>),
}

/// Try to parse an identifier as a register name.
pub fn parse_register(name: &str) -> Option<Register> {
    let lower: String = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (kind, size) = match lower.as_str() {
        // 64-bit
        "rax" => (RegKind::Rax, RegSize::Bits64),
        "rcx" => (RegKind::Rcx, RegSize::Bits64),
        "rdx" => (RegKind::Rdx, RegSize::Bits64),
        "rbx" => (RegKind::Rbx, RegSize::Bits64),
        "rsp" => (RegKind::Rsp, RegSize::Bits64),
        "rbp" => (RegKind::Rbp, RegSize::Bits64),
        "rsi" => (RegKind::Rsi, RegSize::Bits64),
        "rdi" => (RegKind::Rdi, RegSize::Bits64),
        "r8"  => (RegKind::R8, RegSize::Bits64),
        "r9"  => (RegKind::R9, RegSize::Bits64),
        "r10" => (RegKind::R10, RegSize::Bits64),
        "r11" => (RegKind::R11, RegSize::Bits64),
        "r12" => (RegKind::R12, RegSize::Bits64),
        "r13" => (RegKind::R13, RegSize::Bits64),
        "r14" => (RegKind::R14, RegSize::Bits64),
        "r15" => (RegKind::R15, RegSize::Bits64),
        // 32-bit
        "eax" => (RegKind::Rax, RegSize::Bits32),
        "ecx" => (RegKind::Rcx, RegSize::Bits32),
        "edx" => (RegKind::Rdx, RegSize::Bits32),
        "ebx" => (RegKind::Rbx, RegSize::Bits32),
        "esp" => (RegKind::Rsp, RegSize::Bits32),
        "ebp" => (RegKind::Rbp, RegSize::Bits32),
        "esi" => (RegKind::Rsi, RegSize::Bits32),
        "edi" => (RegKind::Rdi, RegSize::Bits32),
        "r8d"  => (RegKind::R8, RegSize::Bits32),
        "r9d"  => (RegKind::R9, RegSize::Bits32),
        "r10d" => (RegKind::R10, RegSize::Bits32),
        "r11d" => (RegKind::R11, RegSize::Bits32),
        "r12d" => (RegKind::R12, RegSize::Bits32),
        "r13d" => (RegKind::R13, RegSize::Bits32),
        "r14d" => (RegKind::R14, RegSize::Bits32),
        "r15d" => (RegKind::R15, RegSize::Bits32),
        // 16-bit
        "ax" => (RegKind::Rax, RegSize::Bits16),
        "cx" => (RegKind::Rcx, RegSize::Bits16),
        "dx" => (RegKind::Rdx, RegSize::Bits16),
        "bx" => (RegKind::Rbx, RegSize::Bits16),
        "sp" => (RegKind::Rsp, RegSize::Bits16),
        "bp" => (RegKind::Rbp, RegSize::Bits16),
        "si" => (RegKind::Rsi, RegSize::Bits16),
        "di" => (RegKind::Rdi, RegSize::Bits16),
        "r8w"  => (RegKind::R8, RegSize::Bits16),
        "r9w"  => (RegKind::R9, RegSize::Bits16),
        "r10w" => (RegKind::R10, RegSize::Bits16),
        "r11w" => (RegKind::R11, RegSize::Bits16),
        "r12w" => (RegKind::R12, RegSize::Bits16),
        "r13w" => (RegKind::R13, RegSize::Bits16),
        "r14w" => (RegKind::R14, RegSize::Bits16),
        "r15w" => (RegKind::R15, RegSize::Bits16),
        // 8-bit
        "al" => (RegKind::Rax, RegSize::Bits8),
        "cl" => (RegKind::Rcx, RegSize::Bits8),
        "dl" => (RegKind::Rdx, RegSize::Bits8),
        "bl" => (RegKind::Rbx, RegSize::Bits8),
        "spl" => (RegKind::Rsp, RegSize::Bits8),
        "bpl" => (RegKind::Rbp, RegSize::Bits8),
        "sil" => (RegKind::Rsi, RegSize::Bits8),
        "dil" => (RegKind::Rdi, RegSize::Bits8),
        "r8b"  => (RegKind::R8, RegSize::Bits8),
        "r9b"  => (RegKind::R9, RegSize::Bits8),
        "r10b" => (RegKind::R10, RegSize::Bits8),
        "r11b" => (RegKind::R11, RegSize::Bits8),
        "r12b" => (RegKind::R12, RegSize::Bits8),
        "r13b" => (RegKind::R13, RegSize::Bits8),
        "r14b" => (RegKind::R14, RegSize::Bits8),
        "r15b" => (RegKind::R15, RegSize::Bits8),
        // XMM registers
        "xmm0" => (RegKind::Xmm0, RegSize::Xmm),
        "xmm1" => (RegKind::Xmm1, RegSize::Xmm),
        "xmm2" => (RegKind::Xmm2, RegSize::Xmm),
        "xmm3" => (RegKind::Xmm3, RegSize::Xmm),
        "xmm4" => (RegKind::Xmm4, RegSize::Xmm),
        "xmm5" => (RegKind::Xmm5, RegSize::Xmm),
        "xmm6" => (RegKind::Xmm6, RegSize::Xmm),
        "xmm7" => (RegKind::Xmm7, RegSize::Xmm),
        "xmm8" => (RegKind::Xmm8, RegSize::Xmm),
        "xmm9" => (RegKind::Xmm9, RegSize::Xmm),
        "xmm10" => (RegKind::Xmm10, RegSize::Xmm),
        "xmm11" => (RegKind::Xmm11, RegSize::Xmm),
        "xmm12" => (RegKind::Xmm12, RegSize::Xmm),
        "xmm13" => (RegKind::Xmm13, RegSize::Xmm),
        "xmm14" => (RegKind::Xmm14, RegSize::Xmm),
        "xmm15" => (RegKind::Xmm15, RegSize::Xmm),
        _ => return None,
    };
    Some(Register { kind, size })
}

/// Parse tokens into an assembly program.
pub fn parse(tokens: &[Token]) -> Result<AsmProgram, String> {
    let mut stmts = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        // Skip newlines
        if tokens[i] == Token::Newline {
            i += 1;
            continue;
        }

        // Label
        if let Token::Label(ref name) = tokens[i] {
            stmts.push(Statement::Label(name.clone()));
            i += 1;
            continue;
        }

        // Directive: dot followed by ident, or bare directive keywords
        if tokens[i] == Token::Dot {
            if i + 1 < tokens.len() {
                if let Token::Ident(ref name) = tokens[i + 1] {
                    let lower: String = name.chars().map(|c| c.to_ascii_lowercase()).collect();
                    match lower.as_str() {
                        "text" | "data" | "bss" | "rodata" => {
                            stmts.push(Statement::Directive(Directive::Section(lower.clone())));
                            i += 2;
                            continue;
                        }
                        _ => {}
                    }
                }
            }
            i += 1;
            continue;
        }

        // Ident: could be instruction, directive keyword, or equ
        if let Token::Ident(ref word) = tokens[i] {
            let lower: String = word.chars().map(|c| c.to_ascii_lowercase()).collect();

            match lower.as_str() {
                // Data directives
                "db" => {
                    i += 1;
                    let (items, adv) = parse_db_items(&tokens[i..])?;
                    stmts.push(Statement::Directive(Directive::Db(items)));
                    i += adv;
                    continue;
                }
                "dw" => {
                    i += 1;
                    let (vals, adv) = parse_int_list(&tokens[i..])?;
                    stmts.push(Statement::Directive(Directive::Dw(vals)));
                    i += adv;
                    continue;
                }
                "dd" => {
                    i += 1;
                    let (vals, adv) = parse_int_list(&tokens[i..])?;
                    stmts.push(Statement::Directive(Directive::Dd(vals)));
                    i += adv;
                    continue;
                }
                "dq" => {
                    i += 1;
                    let (vals, adv) = parse_int_list(&tokens[i..])?;
                    stmts.push(Statement::Directive(Directive::Dq(vals)));
                    i += adv;
                    continue;
                }
                "resb" => {
                    i += 1;
                    let (v, adv) = expect_int(&tokens[i..])?;
                    stmts.push(Statement::Directive(Directive::Resb(v as u64)));
                    i += adv;
                    continue;
                }
                "resw" => {
                    i += 1;
                    let (v, adv) = expect_int(&tokens[i..])?;
                    stmts.push(Statement::Directive(Directive::Resw(v as u64)));
                    i += adv;
                    continue;
                }
                "resd" => {
                    i += 1;
                    let (v, adv) = expect_int(&tokens[i..])?;
                    stmts.push(Statement::Directive(Directive::Resd(v as u64)));
                    i += adv;
                    continue;
                }
                "resq" => {
                    i += 1;
                    let (v, adv) = expect_int(&tokens[i..])?;
                    stmts.push(Statement::Directive(Directive::Resq(v as u64)));
                    i += adv;
                    continue;
                }
                "global" => {
                    i += 1;
                    if let Some(Token::Ident(name)) = tokens.get(i) {
                        stmts.push(Statement::Directive(Directive::Global(name.clone())));
                        i += 1;
                    }
                    continue;
                }
                "extern" => {
                    i += 1;
                    if let Some(Token::Ident(name)) = tokens.get(i) {
                        stmts.push(Statement::Directive(Directive::Extern(name.clone())));
                        i += 1;
                    }
                    continue;
                }
                "align" => {
                    i += 1;
                    let (v, adv) = expect_int(&tokens[i..])?;
                    stmts.push(Statement::Directive(Directive::Align(v as u64)));
                    i += adv;
                    continue;
                }
                "section" => {
                    i += 1;
                    // Expect .text or .data etc.
                    if i < tokens.len() && tokens[i] == Token::Dot {
                        i += 1;
                    }
                    if let Some(Token::Ident(name)) = tokens.get(i) {
                        let lower_name: String = name.chars().map(|c| c.to_ascii_lowercase()).collect();
                        stmts.push(Statement::Directive(Directive::Section(lower_name)));
                        i += 1;
                    }
                    continue;
                }
                "times" => {
                    i += 1;
                    let (count, adv) = expect_int(&tokens[i..])?;
                    i += adv;
                    // Parse the following instruction
                    if let Token::Ident(ref mne) = tokens[i] {
                        let (inst, adv2) = parse_instruction(mne, &tokens[i + 1..])?;
                        i += 1 + adv2;
                        stmts.push(Statement::Directive(Directive::Times(
                            count as u64,
                            Box::new(Statement::Instruction(inst)),
                        )));
                    }
                    continue;
                }
                _ => {
                    // Check for equ: "name equ value"
                    if i + 2 < tokens.len() {
                        if let Token::Ident(ref next) = tokens[i + 1] {
                            if next.eq_ignore_ascii_case("equ") {
                                let (v, adv) = expect_int(&tokens[i + 2..])?;
                                stmts.push(Statement::Directive(Directive::Equ(
                                    word.clone(),
                                    v,
                                )));
                                i += 2 + adv;
                                continue;
                            }
                        }
                    }

                    // Regular instruction
                    let (inst, adv) = parse_instruction(&lower, &tokens[i + 1..])?;
                    stmts.push(Statement::Instruction(inst));
                    i += 1 + adv;
                    continue;
                }
            }
        }

        // Skip unrecognized
        i += 1;
    }

    Ok(AsmProgram { statements: stmts })
}

fn parse_instruction(mnemonic: &str, tokens: &[Token]) -> Result<(Instruction, usize), String> {
    let mut operands = Vec::new();
    let mut i = 0;

    // Parse operands until newline or end
    while i < tokens.len() && tokens[i] != Token::Newline {
        if tokens[i] == Token::Comma {
            i += 1;
            continue;
        }
        let (op, adv) = parse_operand(&tokens[i..])?;
        operands.push(op);
        i += adv;
    }

    Ok((
        Instruction {
            mnemonic: String::from(mnemonic),
            operands,
        },
        i,
    ))
}

fn parse_operand(tokens: &[Token]) -> Result<(Operand, usize), String> {
    let mut i = 0;

    // Size prefix (QWORD PTR [...])
    let mut size_prefix = None;
    if let Some(Token::SizePrefix(sz)) = tokens.get(i) {
        size_prefix = Some(*sz);
        i += 1;
        // Skip PTR if present
        if let Some(Token::Ptr) = tokens.get(i) {
            i += 1;
        }
    }

    // Memory operand [...]
    if let Some(Token::LBracket) = tokens.get(i) {
        i += 1;
        let mut mem = MemOperand::default();
        mem.size = size_prefix;

        // Parse memory contents: base + index*scale + disp
        while i < tokens.len() && tokens[i] != Token::RBracket {
            match &tokens[i] {
                Token::Ident(name) => {
                    let lower: String = name.chars().map(|c| c.to_ascii_lowercase()).collect();
                    if lower == "rip" {
                        mem.rip_relative = true;
                        i += 1;
                    } else if let Some(reg) = parse_register(name) {
                        // Check if this is index*scale
                        if i + 2 < tokens.len() && tokens[i + 1] == Token::Star {
                            if let Token::IntLiteral(scale) = tokens[i + 2] {
                                mem.index = Some(reg);
                                mem.scale = scale as u8;
                                i += 3;
                                continue;
                            }
                        }
                        if mem.base.is_none() {
                            mem.base = Some(reg);
                        } else {
                            mem.index = Some(reg);
                        }
                        i += 1;
                    } else {
                        // Symbol reference in memory
                        mem.symbol = Some(name.clone());
                        i += 1;
                    }
                }
                Token::IntLiteral(v) => {
                    mem.disp = *v;
                    i += 1;
                }
                Token::Plus => {
                    i += 1;
                }
                Token::Minus => {
                    i += 1;
                    if let Some(Token::IntLiteral(v)) = tokens.get(i) {
                        mem.disp = -*v;
                        i += 1;
                    }
                }
                _ => {
                    i += 1;
                }
            }
        }
        if i < tokens.len() && tokens[i] == Token::RBracket {
            i += 1;
        }
        return Ok((Operand::Memory(mem), i));
    }

    // Register
    if let Some(Token::Ident(name)) = tokens.get(i) {
        if let Some(reg) = parse_register(name) {
            return Ok((Operand::Register(reg), i + 1));
        }
        // Symbol reference
        return Ok((Operand::Symbol(name.clone()), i + 1));
    }

    // Immediate
    if let Some(Token::IntLiteral(v)) = tokens.get(i) {
        return Ok((Operand::Immediate(*v), i + 1));
    }

    Err(alloc::format!(
        "expected operand, got {:?}",
        tokens.get(i)
    ))
}

fn parse_db_items(tokens: &[Token]) -> Result<(Vec<DataItem>, usize), String> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < tokens.len() && tokens[i] != Token::Newline {
        if tokens[i] == Token::Comma {
            i += 1;
            continue;
        }
        match &tokens[i] {
            Token::IntLiteral(v) => {
                items.push(DataItem::Byte(*v as u8));
                i += 1;
            }
            Token::StringLiteral(bytes) => {
                items.push(DataItem::Bytes(bytes.clone()));
                i += 1;
            }
            _ => break,
        }
    }
    Ok((items, i))
}

fn parse_int_list(tokens: &[Token]) -> Result<(Vec<i64>, usize), String> {
    let mut vals = Vec::new();
    let mut i = 0;
    while i < tokens.len() && tokens[i] != Token::Newline {
        if tokens[i] == Token::Comma {
            i += 1;
            continue;
        }
        if let Token::IntLiteral(v) = tokens[i] {
            vals.push(v);
            i += 1;
        } else {
            break;
        }
    }
    Ok((vals, i))
}

fn expect_int(tokens: &[Token]) -> Result<(i64, usize), String> {
    if let Some(Token::IntLiteral(v)) = tokens.first() {
        Ok((*v, 1))
    } else {
        Err(alloc::format!("expected integer, got {:?}", tokens.first()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    #[test]
    fn test_parse_simple() {
        let tokens = tokenize("mov rax, 42\nret\n").unwrap();
        let prog = parse(&tokens).unwrap();
        assert_eq!(prog.statements.len(), 2);
    }

    #[test]
    fn test_parse_label() {
        let tokens = tokenize("start:\n  nop\n").unwrap();
        let prog = parse(&tokens).unwrap();
        assert!(matches!(prog.statements[0], Statement::Label(_)));
    }

    #[test]
    fn test_parse_memory() {
        let tokens = tokenize("mov QWORD PTR [rbp-8], rax\n").unwrap();
        let prog = parse(&tokens).unwrap();
        if let Statement::Instruction(ref inst) = prog.statements[0] {
            assert_eq!(inst.operands.len(), 2);
            assert!(matches!(inst.operands[0], Operand::Memory(_)));
        } else {
            panic!("expected instruction");
        }
    }
}
