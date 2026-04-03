//! claudio-cc-lite — Minimal C11 compiler targeting x86_64 via Cranelift.
//!
//! Like TCC but written in Rust, `#![no_std]`, runs on bare metal.
//! Compiles C source to x86_64 machine code that can be executed directly.
//!
//! This is ClaudioOS's answer to HolyC.

#![no_std]
#![allow(unused_variables, unused_assignments)]
extern crate alloc;

pub mod lexer;
pub mod ast;
pub mod parser;
pub mod sema;
pub mod codegen;
pub mod libc;
pub mod driver;

pub use driver::{compile, execute, CompileError, CompiledProgram};
