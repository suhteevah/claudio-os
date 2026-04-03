//! # claudio-elf-loader
//!
//! A `no_std` ELF64 binary loader for ClaudioOS.
//!
//! Parses ELF64 headers, program headers, and section headers, then loads
//! PT_LOAD segments into memory at their specified virtual addresses and
//! constructs the initial process stack (argc, argv, envp, auxv).
//!
//! This loader supports both ET_EXEC (static) and ET_DYN (PIE) binaries.
//! For PIE binaries, a random base address is applied for ASLR.

#![no_std]

extern crate alloc;

pub mod header;
pub mod program;
pub mod section;
pub mod loader;

pub use header::{Elf64Header, ElfClass, ElfData, ElfType, ElfOsAbi};
pub use program::{Elf64Phdr, PhdrType, PhdrFlags};
pub use section::{Elf64Shdr, ShdrType, Elf64Sym, SectionHeaders};
pub use loader::{LoadedProgram, Segment, ElfError, load_elf, load_elf_with_args};
