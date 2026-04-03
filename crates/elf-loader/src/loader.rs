//! High-level ELF loader.
//!
//! Loads an ELF64 binary into memory: parses headers, maps PT_LOAD segments,
//! zeroes BSS, sets up the initial process stack (argc, argv, envp, auxv),
//! and returns a `LoadedProgram` ready for execution.

use alloc::vec::Vec;
use alloc::vec;
use alloc::string::String;
use crate::header::{Elf64Header, HeaderError};
use crate::program::{Elf64Phdr, PhdrError, PhdrType};

/// Default stack size for loaded programs: 8 MiB.
pub const DEFAULT_STACK_SIZE: usize = 8 * 1024 * 1024;

/// Default PIE base address (above typical kernel space).
pub const PIE_BASE_ADDRESS: u64 = 0x0000_5555_5555_0000;

/// Page size constant.
pub const PAGE_SIZE: u64 = 4096;

// --- Auxiliary vector types (auxv) ---
pub const AT_NULL: u64 = 0;
pub const AT_IGNORE: u64 = 1;
pub const AT_EXECFD: u64 = 2;
pub const AT_PHDR: u64 = 3;
pub const AT_PHENT: u64 = 4;
pub const AT_PHNUM: u64 = 5;
pub const AT_PAGESZ: u64 = 6;
pub const AT_BASE: u64 = 7;
pub const AT_FLAGS: u64 = 8;
pub const AT_ENTRY: u64 = 9;
pub const AT_NOTELF: u64 = 10;
pub const AT_UID: u64 = 11;
pub const AT_EUID: u64 = 12;
pub const AT_GID: u64 = 13;
pub const AT_EGID: u64 = 14;
pub const AT_PLATFORM: u64 = 15;
pub const AT_HWCAP: u64 = 16;
pub const AT_CLKTCK: u64 = 17;
pub const AT_SECURE: u64 = 23;
pub const AT_BASE_PLATFORM: u64 = 24;
pub const AT_RANDOM: u64 = 25;
pub const AT_HWCAP2: u64 = 26;
pub const AT_EXECFN: u64 = 31;
pub const AT_SYSINFO_EHDR: u64 = 33;

/// A memory segment loaded from an ELF binary.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Virtual address where this segment is loaded.
    pub vaddr: u64,
    /// Segment data (includes file data + zero-filled BSS).
    pub data: Vec<u8>,
    /// Whether this segment is readable.
    pub readable: bool,
    /// Whether this segment is writable.
    pub writable: bool,
    /// Whether this segment is executable.
    pub executable: bool,
}

/// A fully loaded ELF program ready for execution.
#[derive(Debug)]
pub struct LoadedProgram {
    /// Entry point address (adjusted for PIE base if applicable).
    pub entry_point: u64,
    /// Loaded memory segments.
    pub segments: Vec<Segment>,
    /// Stack allocation (top of stack = stack base + stack size).
    pub stack: Vec<u8>,
    /// Initial stack pointer (top of stack, 16-byte aligned).
    pub stack_pointer: u64,
    /// Base address of the stack allocation.
    pub stack_base: u64,
    /// Start of the program break (brk) for heap expansion.
    pub brk_start: u64,
    /// Current program break.
    pub brk_current: u64,
    /// Base address for PIE binaries (0 for ET_EXEC).
    pub load_base: u64,
    /// Program header virtual address (for AT_PHDR).
    pub phdr_vaddr: u64,
    /// Number of program headers (for AT_PHNUM).
    pub phdr_count: u16,
    /// Program header entry size (for AT_PHENT).
    pub phdr_entsize: u16,
    /// The initial stack contents (argc, argv pointers, envp, auxv).
    pub initial_stack_data: Vec<u8>,
}

/// Errors that can occur during ELF loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    Header(HeaderError),
    Program(PhdrError),
    NoLoadSegments,
    SegmentOverlap,
    InvalidAlignment,
    OutOfMemory,
}

impl From<HeaderError> for ElfError {
    fn from(e: HeaderError) -> Self {
        ElfError::Header(e)
    }
}

impl From<PhdrError> for ElfError {
    fn from(e: PhdrError) -> Self {
        ElfError::Program(e)
    }
}

/// Align a value down to the nearest page boundary.
fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}

/// Align a value up to the nearest page boundary.
fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

/// A simple xorshift64 PRNG for ASLR randomization.
fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Load an ELF64 binary from raw bytes.
///
/// Parses the ELF header and program headers, loads PT_LOAD segments into
/// heap-allocated memory, zeroes BSS regions, allocates a stack, and builds
/// the initial stack layout (argc, argv, envp, auxv) per the Linux ABI.
///
/// # Arguments
/// * `binary` - Raw ELF binary data
///
/// # Returns
/// A `LoadedProgram` with all segments loaded and the initial stack prepared.
pub fn load_elf(binary: &[u8]) -> Result<LoadedProgram, ElfError> {
    load_elf_with_args(binary, &[], &[])
}

/// Load an ELF64 binary with command-line arguments and environment variables.
pub fn load_elf_with_args(
    binary: &[u8],
    argv: &[&str],
    envp: &[&str],
) -> Result<LoadedProgram, ElfError> {
    let header = Elf64Header::parse(binary)?;
    let phdrs = Elf64Phdr::parse_all(binary, header.e_phoff, header.e_phentsize, header.e_phnum)?;

    // Determine base address for PIE/ASLR
    let load_base = if header.is_pie() {
        // For PIE, apply a base address. In a real system we'd randomize this.
        // Use a deterministic "random" offset based on binary length for reproducibility.
        let mut rng_state = binary.len() as u64 ^ 0xDEAD_BEEF_CAFE_BABE;
        let random_offset = xorshift64(&mut rng_state) & 0x000F_FFFF_F000; // Up to 4 GiB, page-aligned
        PIE_BASE_ADDRESS + (random_offset & !0xFFF) // Page-align
    } else {
        0
    };

    // Load PT_LOAD segments
    let mut segments = Vec::new();
    let mut max_addr: u64 = 0;
    let mut phdr_vaddr: u64 = 0;

    for phdr in &phdrs {
        // Record PT_PHDR address
        if phdr.p_type == PhdrType::Phdr {
            phdr_vaddr = load_base + phdr.p_vaddr;
        }

        if !phdr.is_load() {
            continue;
        }

        phdr.validate(binary.len())?;

        let seg_vaddr = load_base + phdr.p_vaddr;
        let seg_end = seg_vaddr + phdr.p_memsz;

        // Allocate segment memory (memsz may be > filesz for BSS)
        let mut data = vec![0u8; phdr.p_memsz as usize];

        // Copy file data
        let file_start = phdr.p_offset as usize;
        let file_end = file_start + phdr.p_filesz as usize;
        if phdr.p_filesz > 0 {
            data[..phdr.p_filesz as usize].copy_from_slice(&binary[file_start..file_end]);
        }
        // BSS region (filesz..memsz) is already zeroed by vec![0u8; ...]

        segments.push(Segment {
            vaddr: seg_vaddr,
            data,
            readable: phdr.p_flags.readable(),
            writable: phdr.p_flags.writable(),
            executable: phdr.p_flags.executable(),
        });

        if seg_end > max_addr {
            max_addr = seg_end;
        }
    }

    if segments.is_empty() {
        return Err(ElfError::NoLoadSegments);
    }

    // If no PT_PHDR, compute from file offset
    if phdr_vaddr == 0 && header.e_phoff > 0 {
        // Try to find which LOAD segment contains the program headers
        for seg in &segments {
            let phdr_file_start = header.e_phoff;
            if phdr_file_start >= seg.vaddr - load_base
                && phdr_file_start < (seg.vaddr - load_base) + seg.data.len() as u64
            {
                phdr_vaddr = seg.vaddr + (phdr_file_start - (seg.vaddr - load_base));
                break;
            }
        }
    }

    // Set up brk at the end of loaded segments (page-aligned)
    let brk_start = align_up(max_addr, PAGE_SIZE);

    // Allocate stack
    let stack = vec![0u8; DEFAULT_STACK_SIZE];
    // Stack grows downward on x86_64. We place it at a fixed high address.
    let stack_base: u64 = 0x0000_7FFF_FFFF_0000 - DEFAULT_STACK_SIZE as u64;
    let stack_top = stack_base + DEFAULT_STACK_SIZE as u64;

    // Build initial stack contents per Linux x86_64 ABI:
    //   [top]
    //   random bytes (16 bytes for AT_RANDOM)
    //   null-terminated strings for argv and envp
    //   padding for alignment
    //   AT_NULL (0, 0)
    //   auxv entries...
    //   NULL (end of envp)
    //   envp[n-1] pointer
    //   ...
    //   envp[0] pointer
    //   NULL (end of argv)
    //   argv[n-1] pointer
    //   ...
    //   argv[0] pointer
    //   argc
    //   [bottom = initial RSP]

    let mut stack_data: Vec<u8> = Vec::new();

    // First, write all strings and record their offsets from stack_top
    let mut string_area: Vec<u8> = Vec::new();
    let mut argv_offsets: Vec<usize> = Vec::new();
    let mut envp_offsets: Vec<usize> = Vec::new();

    // argv strings
    for arg in argv {
        argv_offsets.push(string_area.len());
        string_area.extend_from_slice(arg.as_bytes());
        string_area.push(0); // null terminator
    }

    // envp strings
    for env in envp {
        envp_offsets.push(string_area.len());
        string_area.extend_from_slice(env.as_bytes());
        string_area.push(0);
    }

    // 16 random bytes for AT_RANDOM
    let random_offset = string_area.len();
    let mut rng = binary.len() as u64 ^ 0x1234_5678_9ABC_DEF0;
    for _ in 0..16 {
        string_area.push((xorshift64(&mut rng) & 0xFF) as u8);
    }

    // Platform string "x86_64\0"
    let platform_offset = string_area.len();
    string_area.extend_from_slice(b"x86_64\0");

    // Executable name
    let execfn_offset = string_area.len();
    if !argv.is_empty() {
        string_area.extend_from_slice(argv[0].as_bytes());
    } else {
        string_area.extend_from_slice(b"unknown");
    }
    string_area.push(0);

    // Compute string area address (just below stack_top)
    let string_area_size = align_up(string_area.len() as u64, 16);
    let string_area_base = stack_top - string_area_size;

    // Convert string offsets to virtual addresses
    let argv_addrs: Vec<u64> = argv_offsets
        .iter()
        .map(|&off| string_area_base + off as u64)
        .collect();
    let envp_addrs: Vec<u64> = envp_offsets
        .iter()
        .map(|&off| string_area_base + off as u64)
        .collect();
    let random_addr = string_area_base + random_offset as u64;
    let platform_addr = string_area_base + platform_offset as u64;
    let execfn_addr = string_area_base + execfn_offset as u64;

    // Build the qword array that goes below the string area
    let mut qwords: Vec<u64> = Vec::new();

    // argc
    qwords.push(argv.len() as u64);

    // argv pointers
    for addr in &argv_addrs {
        qwords.push(*addr);
    }
    qwords.push(0); // NULL terminator

    // envp pointers
    for addr in &envp_addrs {
        qwords.push(*addr);
    }
    qwords.push(0); // NULL terminator

    // auxv entries
    let entry_point = load_base + header.e_entry;
    let auxv: &[(u64, u64)] = &[
        (AT_PAGESZ, PAGE_SIZE),
        (AT_PHDR, phdr_vaddr),
        (AT_PHENT, header.e_phentsize as u64),
        (AT_PHNUM, header.e_phnum as u64),
        (AT_ENTRY, entry_point),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_SECURE, 0),
        (AT_RANDOM, random_addr),
        (AT_PLATFORM, platform_addr),
        (AT_EXECFN, execfn_addr),
        (AT_HWCAP, 0),
        (AT_HWCAP2, 0),
        (AT_CLKTCK, 100), // 100 Hz
        (AT_NULL, 0),
    ];
    for &(key, val) in auxv {
        qwords.push(key);
        qwords.push(val);
    }

    // Compute total size and initial RSP
    let qwords_size = qwords.len() * 8;
    let total_below_strings = align_up(qwords_size as u64, 16);
    let initial_rsp = string_area_base - total_below_strings;

    // Build the actual stack bytes
    // The stack data vector represents [initial_rsp .. stack_top]
    let stack_frame_size = (stack_top - initial_rsp) as usize;
    stack_data.resize(stack_frame_size, 0);

    // Write qwords at the bottom of the frame
    for (i, &qword) in qwords.iter().enumerate() {
        let offset = i * 8;
        if offset + 8 <= stack_data.len() {
            stack_data[offset..offset + 8].copy_from_slice(&qword.to_le_bytes());
        }
    }

    // Write string area at the top of the frame
    let string_start = (string_area_base - initial_rsp) as usize;
    for (i, &byte) in string_area.iter().enumerate() {
        let offset = string_start + i;
        if offset < stack_data.len() {
            stack_data[offset] = byte;
        }
    }

    log::info!(
        "ELF loaded: entry=0x{:016X}, base=0x{:016X}, {} segments, brk=0x{:016X}, rsp=0x{:016X}",
        entry_point,
        load_base,
        segments.len(),
        brk_start,
        initial_rsp
    );

    Ok(LoadedProgram {
        entry_point,
        segments,
        stack,
        stack_pointer: initial_rsp,
        stack_base,
        brk_start,
        brk_current: brk_start,
        load_base,
        phdr_vaddr,
        phdr_count: header.e_phnum,
        phdr_entsize: header.e_phentsize,
        initial_stack_data: stack_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 4096), 0);
        assert_eq!(align_up(1, 4096), 4096);
        assert_eq!(align_up(4096, 4096), 4096);
        assert_eq!(align_up(4097, 4096), 8192);
    }

    #[test]
    fn test_align_down() {
        assert_eq!(align_down(0, 4096), 0);
        assert_eq!(align_down(4095, 4096), 0);
        assert_eq!(align_down(4096, 4096), 4096);
        assert_eq!(align_down(8191, 4096), 4096);
    }

    #[test]
    fn test_empty_binary() {
        assert!(load_elf(&[]).is_err());
    }
}
