//! claudio-asm-x86 — x86_64 assembler with NASM-like syntax for ClaudioOS.
//!
//! Assembles x86_64 assembly source text into raw machine code bytes that can
//! be executed directly on bare metal (no linker, no ELF, just code).
//!
//! This crate is `#![no_std]` and runs on the ClaudioOS bare-metal kernel.

#![no_std]
extern crate alloc;

pub mod lexer;
pub mod parser;
pub mod encoder;
pub mod relocations;
pub mod driver;

pub use driver::{assemble, execute, AsmError, AssembledProgram};
