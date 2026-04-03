//! x86_64 instruction encoding: REX prefix, ModR/M, SIB, displacement, immediate.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::parser::{Instruction, MemOperand, Operand, RegKind, RegSize, Register};
use crate::relocations::Fixup;

/// Encoded instruction bytes with optional fixups for label references.
pub struct EncodedInst {
    pub bytes: Vec<u8>,
    pub fixups: Vec<Fixup>,
}

/// Encode a single instruction to machine code bytes.
pub fn encode_instruction(inst: &Instruction, current_offset: usize) -> Result<EncodedInst, String> {
    let mne = inst.mnemonic.as_str();
    let ops = &inst.operands;

    match mne {
        "nop" => Ok(simple(vec![0x90])),
        "ret" => Ok(simple(vec![0xC3])),
        "int" => encode_int(ops),
        "syscall" => Ok(simple(vec![0x0F, 0x05])),
        "sysret" => Ok(simple(vec![0x0F, 0x07])),
        "cdq" => Ok(simple(vec![0x99])),
        "cqo" => Ok(simple(vec![0x48, 0x99])),
        "hlt" => Ok(simple(vec![0xF4])),

        "push" => encode_push(ops),
        "pop" => encode_pop(ops),

        "mov" => encode_mov(ops),
        "movzx" => encode_movzx(ops),
        "movsx" | "movsxd" => encode_movsx(ops),
        "lea" => encode_lea(ops),
        "xchg" => encode_xchg(ops),

        "add" => encode_alu(ops, 0x00, 0),
        "or"  => encode_alu(ops, 0x08, 1),
        "adc" => encode_alu(ops, 0x10, 2),
        "sbb" => encode_alu(ops, 0x18, 3),
        "and" => encode_alu(ops, 0x20, 4),
        "sub" => encode_alu(ops, 0x28, 5),
        "xor" => encode_alu(ops, 0x30, 6),
        "cmp" => encode_alu(ops, 0x38, 7),
        "test" => encode_test(ops),

        "not" => encode_unary(ops, 2),
        "neg" => encode_unary(ops, 3),
        "mul" => encode_unary(ops, 4),
        "imul" => encode_imul(ops),
        "div" => encode_unary(ops, 6),
        "idiv" => encode_unary(ops, 7),

        "shl" | "sal" => encode_shift(ops, 4),
        "shr" => encode_shift(ops, 5),
        "sar" => encode_shift(ops, 7),

        "inc" => encode_incdec(ops, 0),
        "dec" => encode_incdec(ops, 1),

        "jmp" => encode_jmp(ops, current_offset),
        "je" | "jz" => encode_jcc(ops, 0x84, current_offset),
        "jne" | "jnz" => encode_jcc(ops, 0x85, current_offset),
        "jg" | "jnle" => encode_jcc(ops, 0x8F, current_offset),
        "jge" | "jnl" => encode_jcc(ops, 0x8D, current_offset),
        "jl" | "jnge" => encode_jcc(ops, 0x8C, current_offset),
        "jle" | "jng" => encode_jcc(ops, 0x8E, current_offset),
        "ja" | "jnbe" => encode_jcc(ops, 0x87, current_offset),
        "jae" | "jnb" | "jnc" => encode_jcc(ops, 0x83, current_offset),
        "jb" | "jnae" | "jc" => encode_jcc(ops, 0x82, current_offset),
        "jbe" | "jna" => encode_jcc(ops, 0x86, current_offset),
        "js" => encode_jcc(ops, 0x88, current_offset),
        "jns" => encode_jcc(ops, 0x89, current_offset),

        "call" => encode_call(ops, current_offset),

        "sete" | "setz" => encode_setcc(ops, 0x94),
        "setne" | "setnz" => encode_setcc(ops, 0x95),
        "setg" => encode_setcc(ops, 0x9F),
        "setge" => encode_setcc(ops, 0x9D),
        "setl" => encode_setcc(ops, 0x9C),
        "setle" => encode_setcc(ops, 0x9E),
        "seta" => encode_setcc(ops, 0x97),
        "setae" => encode_setcc(ops, 0x93),
        "setb" => encode_setcc(ops, 0x92),
        "setbe" => encode_setcc(ops, 0x96),

        "cmove" | "cmovz" => encode_cmovcc(ops, 0x44),
        "cmovne" | "cmovnz" => encode_cmovcc(ops, 0x45),
        "cmovg" => encode_cmovcc(ops, 0x4F),
        "cmovge" => encode_cmovcc(ops, 0x4D),
        "cmovl" => encode_cmovcc(ops, 0x4C),
        "cmovle" => encode_cmovcc(ops, 0x4E),

        // SSE scalar double
        "movsd" => encode_sse_op(ops, &[0xF2, 0x0F, 0x10], &[0xF2, 0x0F, 0x11]),
        "addsd" => encode_sse_arith(ops, &[0xF2, 0x0F, 0x58]),
        "subsd" => encode_sse_arith(ops, &[0xF2, 0x0F, 0x5C]),
        "mulsd" => encode_sse_arith(ops, &[0xF2, 0x0F, 0x59]),
        "divsd" => encode_sse_arith(ops, &[0xF2, 0x0F, 0x5E]),
        // SSE scalar single
        "movss" => encode_sse_op(ops, &[0xF3, 0x0F, 0x10], &[0xF3, 0x0F, 0x11]),
        "addss" => encode_sse_arith(ops, &[0xF3, 0x0F, 0x58]),
        "subss" => encode_sse_arith(ops, &[0xF3, 0x0F, 0x5C]),
        "mulss" => encode_sse_arith(ops, &[0xF3, 0x0F, 0x59]),
        "divss" => encode_sse_arith(ops, &[0xF3, 0x0F, 0x5E]),

        _ => Err(alloc::format!("unsupported mnemonic: {}", mne)),
    }
}

fn simple(bytes: Vec<u8>) -> EncodedInst {
    EncodedInst {
        bytes,
        fixups: Vec::new(),
    }
}

// === REX prefix helpers ===

/// Build REX prefix byte.  Returns None if not needed.
fn rex(w: bool, reg_ext: bool, index_ext: bool, rm_ext: bool) -> Option<u8> {
    let val = 0x40
        | if w { 0x08 } else { 0 }
        | if reg_ext { 0x04 } else { 0 }
        | if index_ext { 0x02 } else { 0 }
        | if rm_ext { 0x01 } else { 0 };
    if val != 0x40 { Some(val) } else { None }
}

fn needs_rex_w(reg: &Register) -> bool {
    reg.size == RegSize::Bits64
}

/// ModR/M byte: mod(2) | reg(3) | r/m(3).
fn modrm(mod_bits: u8, reg: u8, rm: u8) -> u8 {
    ((mod_bits & 3) << 6) | ((reg & 7) << 3) | (rm & 7)
}

/// SIB byte: scale(2) | index(3) | base(3).
fn sib(scale: u8, index: u8, base: u8) -> u8 {
    let s = match scale {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        _ => 0,
    };
    (s << 6) | ((index & 7) << 3) | (base & 7)
}

/// Encode a register-register ModR/M (mod=11).
fn encode_reg_reg(out: &mut Vec<u8>, opcode: &[u8], reg: &Register, rm: &Register, force_rex_w: bool) {
    let w = force_rex_w || needs_rex_w(reg) || needs_rex_w(rm);
    if let Some(r) = rex(w, reg.kind.needs_rex_ext(), false, rm.kind.needs_rex_ext()) {
        out.push(r);
    }
    out.extend_from_slice(opcode);
    out.push(modrm(0b11, reg.kind.encoding(), rm.kind.encoding()));
}

/// Encode a register + memory ModR/M + optional SIB + displacement.
fn encode_reg_mem(out: &mut Vec<u8>, opcode: &[u8], reg_bits: u8, reg_ext: bool, mem: &MemOperand, force_rex_w: bool) {
    let base_enc = mem.base.map(|r| r.kind.encoding()).unwrap_or(0);
    let base_ext = mem.base.map(|r| r.kind.needs_rex_ext()).unwrap_or(false);
    let index_ext = mem.index.map(|r| r.kind.needs_rex_ext()).unwrap_or(false);

    let need_sib = mem.index.is_some()
        || mem.base.map(|r| r.kind == RegKind::Rsp || r.kind == RegKind::R12).unwrap_or(false);

    let w = force_rex_w;
    if let Some(r) = rex(w, reg_ext, index_ext, base_ext) {
        out.push(r);
    }
    out.extend_from_slice(opcode);

    if mem.base.is_none() && mem.index.is_none() {
        // [disp32] absolute or RIP-relative
        if mem.rip_relative {
            out.push(modrm(0b00, reg_bits, 0b101));
        } else {
            // SIB with no base
            out.push(modrm(0b00, reg_bits, 0b100));
            out.push(sib(1, 0b100, 0b101));
        }
        out.extend_from_slice(&(mem.disp as i32).to_le_bytes());
        return;
    }

    let (mod_bits, disp_bytes): (u8, usize) = if mem.disp == 0
        && mem.base.map(|r| r.kind != RegKind::Rbp && r.kind != RegKind::R13).unwrap_or(true)
    {
        (0b00, 0)
    } else if mem.disp >= -128 && mem.disp <= 127 {
        (0b01, 1)
    } else {
        (0b10, 4)
    };

    if need_sib {
        let index_enc = mem.index.map(|r| r.kind.encoding()).unwrap_or(0b100);
        let scale = if mem.index.is_some() { mem.scale } else { 1 };
        out.push(modrm(mod_bits, reg_bits, 0b100));
        out.push(sib(scale, index_enc, base_enc));
    } else {
        out.push(modrm(mod_bits, reg_bits, base_enc));
    }

    match disp_bytes {
        1 => out.push(mem.disp as i8 as u8),
        4 => out.extend_from_slice(&(mem.disp as i32).to_le_bytes()),
        _ => {}
    }
}

// === Instruction encoders ===

fn encode_int(ops: &[Operand]) -> Result<EncodedInst, String> {
    if let Some(Operand::Immediate(v)) = ops.first() {
        Ok(simple(vec![0xCD, *v as u8]))
    } else {
        Err(String::from("int requires immediate operand"))
    }
}

fn encode_push(ops: &[Operand]) -> Result<EncodedInst, String> {
    match ops.first() {
        Some(Operand::Register(reg)) if reg.size == RegSize::Bits64 => {
            let mut bytes = Vec::new();
            if reg.kind.needs_rex_ext() {
                bytes.push(0x41);
            }
            bytes.push(0x50 + (reg.kind.encoding() & 7));
            Ok(simple(bytes))
        }
        Some(Operand::Immediate(v)) => {
            if *v >= -128 && *v <= 127 {
                Ok(simple(vec![0x6A, *v as u8]))
            } else {
                let mut bytes = vec![0x68];
                bytes.extend_from_slice(&(*v as i32).to_le_bytes());
                Ok(simple(bytes))
            }
        }
        _ => Err(String::from("push: unsupported operand")),
    }
}

fn encode_pop(ops: &[Operand]) -> Result<EncodedInst, String> {
    match ops.first() {
        Some(Operand::Register(reg)) if reg.size == RegSize::Bits64 => {
            let mut bytes = Vec::new();
            if reg.kind.needs_rex_ext() {
                bytes.push(0x41);
            }
            bytes.push(0x58 + (reg.kind.encoding() & 7));
            Ok(simple(bytes))
        }
        _ => Err(String::from("pop: unsupported operand")),
    }
}

fn encode_mov(ops: &[Operand]) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("mov requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        // mov reg, reg
        (Operand::Register(dst), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            encode_reg_reg(&mut bytes, &[0x89], src, dst, needs_rex_w(dst) || needs_rex_w(src));
            Ok(simple(bytes))
        }
        // mov reg, imm
        (Operand::Register(dst), Operand::Immediate(val)) => {
            let mut bytes = Vec::new();
            match dst.size {
                RegSize::Bits64 => {
                    if *val >= i32::MIN as i64 && *val <= i32::MAX as i64 {
                        // mov r64, imm32 (sign-extended)
                        if let Some(r) = rex(true, false, false, dst.kind.needs_rex_ext()) {
                            bytes.push(r);
                        }
                        bytes.push(0xC7);
                        bytes.push(modrm(0b11, 0, dst.kind.encoding()));
                        bytes.extend_from_slice(&(*val as i32).to_le_bytes());
                    } else {
                        // mov r64, imm64
                        if let Some(r) = rex(true, false, false, dst.kind.needs_rex_ext()) {
                            bytes.push(r);
                        }
                        bytes.push(0xB8 + (dst.kind.encoding() & 7));
                        bytes.extend_from_slice(&val.to_le_bytes());
                    }
                }
                RegSize::Bits32 => {
                    if dst.kind.needs_rex_ext() {
                        bytes.push(0x41);
                    }
                    bytes.push(0xB8 + (dst.kind.encoding() & 7));
                    bytes.extend_from_slice(&(*val as i32).to_le_bytes());
                }
                RegSize::Bits16 => {
                    bytes.push(0x66); // operand size prefix
                    if dst.kind.needs_rex_ext() {
                        bytes.push(0x41);
                    }
                    bytes.push(0xB8 + (dst.kind.encoding() & 7));
                    bytes.extend_from_slice(&(*val as i16).to_le_bytes());
                }
                RegSize::Bits8 => {
                    if dst.kind.needs_rex_ext() || matches!(dst.kind, RegKind::Rsp | RegKind::Rbp | RegKind::Rsi | RegKind::Rdi) {
                        if let Some(r) = rex(false, false, false, dst.kind.needs_rex_ext()) {
                            bytes.push(r);
                        }
                    }
                    bytes.push(0xB0 + (dst.kind.encoding() & 7));
                    bytes.push(*val as u8);
                }
                _ => return Err(String::from("mov: unsupported operand size")),
            }
            Ok(simple(bytes))
        }
        // mov reg, [mem]
        (Operand::Register(dst), Operand::Memory(mem)) => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(dst);
            encode_reg_mem(&mut bytes, &[0x8B], dst.kind.encoding(), dst.kind.needs_rex_ext(), mem, w);
            Ok(simple(bytes))
        }
        // mov [mem], reg
        (Operand::Memory(mem), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(src);
            encode_reg_mem(&mut bytes, &[0x89], src.kind.encoding(), src.kind.needs_rex_ext(), mem, w);
            Ok(simple(bytes))
        }
        // mov [mem], imm32
        (Operand::Memory(mem), Operand::Immediate(val)) => {
            let mut bytes = Vec::new();
            let w = mem.size.map(|s| s == crate::lexer::SizeKind::Qword).unwrap_or(true);
            encode_reg_mem(&mut bytes, &[0xC7], 0, false, mem, w);
            bytes.extend_from_slice(&(*val as i32).to_le_bytes());
            Ok(simple(bytes))
        }
        // mov reg, symbol (label reference — emit fixup)
        (Operand::Register(dst), Operand::Symbol(sym)) => {
            let mut bytes = Vec::new();
            if let Some(r) = rex(true, false, false, dst.kind.needs_rex_ext()) {
                bytes.push(r);
            }
            bytes.push(0xB8 + (dst.kind.encoding() & 7));
            let fixup_offset = bytes.len();
            bytes.extend_from_slice(&0i64.to_le_bytes());
            Ok(EncodedInst {
                bytes,
                fixups: alloc::vec![Fixup {
                    offset: fixup_offset,
                    symbol: sym.clone(),
                    kind: crate::relocations::FixupKind::Abs64,
                }],
            })
        }
        _ => Err(String::from("mov: unsupported operand combination")),
    }
}

fn encode_movzx(ops: &[Operand]) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("movzx requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        (Operand::Register(dst), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            let opcode = if src.size == RegSize::Bits8 { 0xB6 } else { 0xB7 };
            let w = needs_rex_w(dst);
            if let Some(r) = rex(w, dst.kind.needs_rex_ext(), false, src.kind.needs_rex_ext()) {
                bytes.push(r);
            }
            bytes.push(0x0F);
            bytes.push(opcode);
            bytes.push(modrm(0b11, dst.kind.encoding(), src.kind.encoding()));
            Ok(simple(bytes))
        }
        _ => Err(String::from("movzx: unsupported operand combination")),
    }
}

fn encode_movsx(ops: &[Operand]) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("movsx requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        (Operand::Register(dst), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            if src.size == RegSize::Bits32 {
                // movsxd r64, r32  (opcode 0x63 with REX.W)
                if let Some(r) = rex(true, dst.kind.needs_rex_ext(), false, src.kind.needs_rex_ext()) {
                    bytes.push(r);
                }
                bytes.push(0x63);
                bytes.push(modrm(0b11, dst.kind.encoding(), src.kind.encoding()));
            } else {
                let opcode = if src.size == RegSize::Bits8 { 0xBE } else { 0xBF };
                let w = needs_rex_w(dst);
                if let Some(r) = rex(w, dst.kind.needs_rex_ext(), false, src.kind.needs_rex_ext()) {
                    bytes.push(r);
                }
                bytes.push(0x0F);
                bytes.push(opcode);
                bytes.push(modrm(0b11, dst.kind.encoding(), src.kind.encoding()));
            }
            Ok(simple(bytes))
        }
        _ => Err(String::from("movsx: unsupported operand combination")),
    }
}

fn encode_lea(ops: &[Operand]) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("lea requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        (Operand::Register(dst), Operand::Memory(mem)) => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(dst);
            encode_reg_mem(&mut bytes, &[0x8D], dst.kind.encoding(), dst.kind.needs_rex_ext(), mem, w);
            Ok(simple(bytes))
        }
        _ => Err(String::from("lea: requires reg, [mem]")),
    }
}

fn encode_xchg(ops: &[Operand]) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("xchg requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        (Operand::Register(a), Operand::Register(b)) => {
            let mut bytes = Vec::new();
            // xchg rax, reg shortcut
            if a.kind == RegKind::Rax && a.size == RegSize::Bits64 {
                if let Some(r) = rex(true, false, false, b.kind.needs_rex_ext()) {
                    bytes.push(r);
                }
                bytes.push(0x90 + (b.kind.encoding() & 7));
            } else {
                encode_reg_reg(&mut bytes, &[0x87], a, b, needs_rex_w(a));
            }
            Ok(simple(bytes))
        }
        _ => Err(String::from("xchg: unsupported operands")),
    }
}

fn encode_alu(ops: &[Operand], base_opcode: u8, ext: u8) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("ALU op requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        (Operand::Register(dst), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            let opcode = base_opcode + 1; // reg/reg form
            encode_reg_reg(&mut bytes, &[opcode], src, dst, needs_rex_w(dst));
            Ok(simple(bytes))
        }
        (Operand::Register(dst), Operand::Immediate(val)) => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(dst);
            if *val >= -128 && *val <= 127 {
                // imm8 sign-extended
                if let Some(r) = rex(w, false, false, dst.kind.needs_rex_ext()) {
                    bytes.push(r);
                }
                bytes.push(0x83);
                bytes.push(modrm(0b11, ext, dst.kind.encoding()));
                bytes.push(*val as i8 as u8);
            } else {
                if let Some(r) = rex(w, false, false, dst.kind.needs_rex_ext()) {
                    bytes.push(r);
                }
                bytes.push(0x81);
                bytes.push(modrm(0b11, ext, dst.kind.encoding()));
                bytes.extend_from_slice(&(*val as i32).to_le_bytes());
            }
            Ok(simple(bytes))
        }
        (Operand::Register(dst), Operand::Memory(mem)) => {
            let mut bytes = Vec::new();
            let opcode = base_opcode + 3;
            let w = needs_rex_w(dst);
            encode_reg_mem(&mut bytes, &[opcode], dst.kind.encoding(), dst.kind.needs_rex_ext(), mem, w);
            Ok(simple(bytes))
        }
        (Operand::Memory(mem), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            let opcode = base_opcode + 1;
            let w = needs_rex_w(src);
            encode_reg_mem(&mut bytes, &[opcode], src.kind.encoding(), src.kind.needs_rex_ext(), mem, w);
            Ok(simple(bytes))
        }
        (Operand::Memory(mem), Operand::Immediate(val)) => {
            let mut bytes = Vec::new();
            let w = mem.size.map(|s| s == crate::lexer::SizeKind::Qword).unwrap_or(true);
            if *val >= -128 && *val <= 127 {
                encode_reg_mem(&mut bytes, &[0x83], ext, false, mem, w);
                bytes.push(*val as i8 as u8);
            } else {
                encode_reg_mem(&mut bytes, &[0x81], ext, false, mem, w);
                bytes.extend_from_slice(&(*val as i32).to_le_bytes());
            }
            Ok(simple(bytes))
        }
        _ => Err(String::from("ALU: unsupported operand combination")),
    }
}

fn encode_test(ops: &[Operand]) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("test requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        (Operand::Register(dst), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            encode_reg_reg(&mut bytes, &[0x85], src, dst, needs_rex_w(dst));
            Ok(simple(bytes))
        }
        (Operand::Register(dst), Operand::Immediate(val)) => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(dst);
            if let Some(r) = rex(w, false, false, dst.kind.needs_rex_ext()) {
                bytes.push(r);
            }
            bytes.push(0xF7);
            bytes.push(modrm(0b11, 0, dst.kind.encoding()));
            bytes.extend_from_slice(&(*val as i32).to_le_bytes());
            Ok(simple(bytes))
        }
        _ => Err(String::from("test: unsupported operands")),
    }
}

fn encode_unary(ops: &[Operand], ext: u8) -> Result<EncodedInst, String> {
    match ops.first() {
        Some(Operand::Register(reg)) => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(reg);
            if let Some(r) = rex(w, false, false, reg.kind.needs_rex_ext()) {
                bytes.push(r);
            }
            bytes.push(0xF7);
            bytes.push(modrm(0b11, ext, reg.kind.encoding()));
            Ok(simple(bytes))
        }
        _ => Err(String::from("unary op: requires register operand")),
    }
}

fn encode_imul(ops: &[Operand]) -> Result<EncodedInst, String> {
    match ops.len() {
        1 => encode_unary(ops, 5),
        2 => {
            // imul r64, r/m64 (0F AF)
            match (&ops[0], &ops[1]) {
                (Operand::Register(dst), Operand::Register(src)) => {
                    let mut bytes = Vec::new();
                    let w = needs_rex_w(dst);
                    if let Some(r) = rex(w, dst.kind.needs_rex_ext(), false, src.kind.needs_rex_ext()) {
                        bytes.push(r);
                    }
                    bytes.push(0x0F);
                    bytes.push(0xAF);
                    bytes.push(modrm(0b11, dst.kind.encoding(), src.kind.encoding()));
                    Ok(simple(bytes))
                }
                _ => Err(String::from("imul: unsupported 2-operand form")),
            }
        }
        3 => {
            // imul r64, r/m64, imm
            match (&ops[0], &ops[1], &ops[2]) {
                (Operand::Register(dst), Operand::Register(src), Operand::Immediate(val)) => {
                    let mut bytes = Vec::new();
                    let w = needs_rex_w(dst);
                    if let Some(r) = rex(w, dst.kind.needs_rex_ext(), false, src.kind.needs_rex_ext()) {
                        bytes.push(r);
                    }
                    if *val >= -128 && *val <= 127 {
                        bytes.push(0x6B);
                        bytes.push(modrm(0b11, dst.kind.encoding(), src.kind.encoding()));
                        bytes.push(*val as i8 as u8);
                    } else {
                        bytes.push(0x69);
                        bytes.push(modrm(0b11, dst.kind.encoding(), src.kind.encoding()));
                        bytes.extend_from_slice(&(*val as i32).to_le_bytes());
                    }
                    Ok(simple(bytes))
                }
                _ => Err(String::from("imul: unsupported 3-operand form")),
            }
        }
        _ => Err(String::from("imul: wrong number of operands")),
    }
}

fn encode_shift(ops: &[Operand], ext: u8) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("shift requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        (Operand::Register(dst), Operand::Immediate(1)) => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(dst);
            if let Some(r) = rex(w, false, false, dst.kind.needs_rex_ext()) {
                bytes.push(r);
            }
            bytes.push(0xD1);
            bytes.push(modrm(0b11, ext, dst.kind.encoding()));
            Ok(simple(bytes))
        }
        (Operand::Register(dst), Operand::Immediate(val)) => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(dst);
            if let Some(r) = rex(w, false, false, dst.kind.needs_rex_ext()) {
                bytes.push(r);
            }
            bytes.push(0xC1);
            bytes.push(modrm(0b11, ext, dst.kind.encoding()));
            bytes.push(*val as u8);
            Ok(simple(bytes))
        }
        (Operand::Register(dst), Operand::Register(cl)) if cl.kind == RegKind::Rcx && cl.size == RegSize::Bits8 => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(dst);
            if let Some(r) = rex(w, false, false, dst.kind.needs_rex_ext()) {
                bytes.push(r);
            }
            bytes.push(0xD3);
            bytes.push(modrm(0b11, ext, dst.kind.encoding()));
            Ok(simple(bytes))
        }
        _ => Err(String::from("shift: unsupported operands")),
    }
}

fn encode_incdec(ops: &[Operand], ext: u8) -> Result<EncodedInst, String> {
    match ops.first() {
        Some(Operand::Register(reg)) => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(reg);
            if let Some(r) = rex(w, false, false, reg.kind.needs_rex_ext()) {
                bytes.push(r);
            }
            bytes.push(0xFF);
            bytes.push(modrm(0b11, ext, reg.kind.encoding()));
            Ok(simple(bytes))
        }
        _ => Err(String::from("inc/dec: requires register")),
    }
}

fn encode_jmp(ops: &[Operand], current_offset: usize) -> Result<EncodedInst, String> {
    match ops.first() {
        Some(Operand::Symbol(sym)) => {
            // Near jump rel32
            let mut bytes = vec![0xE9];
            let fixup_offset = bytes.len();
            bytes.extend_from_slice(&0i32.to_le_bytes());
            Ok(EncodedInst {
                bytes,
                fixups: alloc::vec![Fixup {
                    offset: fixup_offset,
                    symbol: sym.clone(),
                    kind: crate::relocations::FixupKind::Rel32 { from: current_offset + 5 },
                }],
            })
        }
        Some(Operand::Register(reg)) => {
            // jmp r/m64
            let mut bytes = Vec::new();
            if reg.kind.needs_rex_ext() {
                bytes.push(0x41);
            }
            bytes.push(0xFF);
            bytes.push(modrm(0b11, 4, reg.kind.encoding()));
            Ok(simple(bytes))
        }
        _ => Err(String::from("jmp: unsupported operand")),
    }
}

fn encode_jcc(ops: &[Operand], cc_opcode: u8, current_offset: usize) -> Result<EncodedInst, String> {
    match ops.first() {
        Some(Operand::Symbol(sym)) => {
            let mut bytes = vec![0x0F, cc_opcode];
            let fixup_offset = bytes.len();
            bytes.extend_from_slice(&0i32.to_le_bytes());
            Ok(EncodedInst {
                bytes,
                fixups: alloc::vec![Fixup {
                    offset: fixup_offset,
                    symbol: sym.clone(),
                    kind: crate::relocations::FixupKind::Rel32 { from: current_offset + 6 },
                }],
            })
        }
        _ => Err(String::from("jcc: requires label operand")),
    }
}

fn encode_call(ops: &[Operand], current_offset: usize) -> Result<EncodedInst, String> {
    match ops.first() {
        Some(Operand::Symbol(sym)) => {
            let mut bytes = vec![0xE8];
            let fixup_offset = bytes.len();
            bytes.extend_from_slice(&0i32.to_le_bytes());
            Ok(EncodedInst {
                bytes,
                fixups: alloc::vec![Fixup {
                    offset: fixup_offset,
                    symbol: sym.clone(),
                    kind: crate::relocations::FixupKind::Rel32 { from: current_offset + 5 },
                }],
            })
        }
        Some(Operand::Register(reg)) => {
            let mut bytes = Vec::new();
            if reg.kind.needs_rex_ext() {
                bytes.push(0x41);
            }
            bytes.push(0xFF);
            bytes.push(modrm(0b11, 2, reg.kind.encoding()));
            Ok(simple(bytes))
        }
        _ => Err(String::from("call: unsupported operand")),
    }
}

fn encode_setcc(ops: &[Operand], cc_opcode: u8) -> Result<EncodedInst, String> {
    match ops.first() {
        Some(Operand::Register(reg)) => {
            let mut bytes = Vec::new();
            if reg.kind.needs_rex_ext() {
                bytes.push(0x41);
            }
            bytes.push(0x0F);
            bytes.push(cc_opcode);
            bytes.push(modrm(0b11, 0, reg.kind.encoding()));
            Ok(simple(bytes))
        }
        _ => Err(String::from("setcc: requires register")),
    }
}

fn encode_cmovcc(ops: &[Operand], cc_opcode: u8) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("cmovcc requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        (Operand::Register(dst), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            let w = needs_rex_w(dst);
            if let Some(r) = rex(w, dst.kind.needs_rex_ext(), false, src.kind.needs_rex_ext()) {
                bytes.push(r);
            }
            bytes.push(0x0F);
            bytes.push(cc_opcode);
            bytes.push(modrm(0b11, dst.kind.encoding(), src.kind.encoding()));
            Ok(simple(bytes))
        }
        _ => Err(String::from("cmovcc: unsupported operands")),
    }
}

fn encode_sse_op(ops: &[Operand], load_prefix: &[u8], store_prefix: &[u8]) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("SSE op requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        (Operand::Register(dst), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(load_prefix);
            bytes.push(modrm(0b11, dst.kind.encoding(), src.kind.encoding()));
            Ok(simple(bytes))
        }
        (Operand::Register(dst), Operand::Memory(mem)) => {
            let mut bytes = Vec::new();
            encode_reg_mem(&mut bytes, load_prefix, dst.kind.encoding(), dst.kind.needs_rex_ext(), mem, false);
            Ok(simple(bytes))
        }
        (Operand::Memory(mem), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            encode_reg_mem(&mut bytes, store_prefix, src.kind.encoding(), src.kind.needs_rex_ext(), mem, false);
            Ok(simple(bytes))
        }
        _ => Err(String::from("SSE mov: unsupported operands")),
    }
}

fn encode_sse_arith(ops: &[Operand], prefix: &[u8]) -> Result<EncodedInst, String> {
    if ops.len() != 2 {
        return Err(String::from("SSE arith requires 2 operands"));
    }
    match (&ops[0], &ops[1]) {
        (Operand::Register(dst), Operand::Register(src)) => {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(prefix);
            bytes.push(modrm(0b11, dst.kind.encoding(), src.kind.encoding()));
            Ok(simple(bytes))
        }
        (Operand::Register(dst), Operand::Memory(mem)) => {
            let mut bytes = Vec::new();
            encode_reg_mem(&mut bytes, prefix, dst.kind.encoding(), dst.kind.needs_rex_ext(), mem, false);
            Ok(simple(bytes))
        }
        _ => Err(String::from("SSE arith: unsupported operands")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Register, RegKind, RegSize};

    #[test]
    fn test_encode_nop() {
        let inst = Instruction {
            mnemonic: String::from("nop"),
            operands: Vec::new(),
        };
        let enc = encode_instruction(&inst, 0).unwrap();
        assert_eq!(enc.bytes, vec![0x90]);
    }

    #[test]
    fn test_encode_ret() {
        let inst = Instruction {
            mnemonic: String::from("ret"),
            operands: Vec::new(),
        };
        let enc = encode_instruction(&inst, 0).unwrap();
        assert_eq!(enc.bytes, vec![0xC3]);
    }

    #[test]
    fn test_encode_mov_reg_imm() {
        let inst = Instruction {
            mnemonic: String::from("mov"),
            operands: alloc::vec![
                Operand::Register(Register { kind: RegKind::Rax, size: RegSize::Bits64 }),
                Operand::Immediate(42),
            ],
        };
        let enc = encode_instruction(&inst, 0).unwrap();
        // REX.W + C7 /0 + imm32 (sign-extended mov)
        assert!(enc.bytes.len() > 0);
        assert_eq!(enc.bytes[0], 0x48); // REX.W
    }

    #[test]
    fn test_encode_push_pop() {
        let push = Instruction {
            mnemonic: String::from("push"),
            operands: alloc::vec![Operand::Register(Register {
                kind: RegKind::Rbp,
                size: RegSize::Bits64,
            })],
        };
        let enc = encode_instruction(&push, 0).unwrap();
        assert_eq!(enc.bytes, vec![0x55]); // push rbp

        let pop = Instruction {
            mnemonic: String::from("pop"),
            operands: alloc::vec![Operand::Register(Register {
                kind: RegKind::Rbp,
                size: RegSize::Bits64,
            })],
        };
        let enc = encode_instruction(&pop, 0).unwrap();
        assert_eq!(enc.bytes, vec![0x5D]); // pop rbp
    }
}
