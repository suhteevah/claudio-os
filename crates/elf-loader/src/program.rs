//! ELF64 program header (segment) parsing.
//!
//! Parses PT_LOAD, PT_INTERP, PT_NOTE, PT_PHDR, PT_TLS, PT_GNU_STACK,
//! and PT_GNU_RELRO segments from the program header table.

/// Size of an ELF64 program header entry.
pub const ELF64_PHDR_SIZE: usize = 56;

// --- Segment types ---
pub const PT_NULL: u32 = 0;
pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP: u32 = 3;
pub const PT_NOTE: u32 = 4;
pub const PT_SHLIB: u32 = 5;
pub const PT_PHDR: u32 = 6;
pub const PT_TLS: u32 = 7;
pub const PT_GNU_EH_FRAME: u32 = 0x6474_e550;
pub const PT_GNU_STACK: u32 = 0x6474_e551;
pub const PT_GNU_RELRO: u32 = 0x6474_e552;
pub const PT_GNU_PROPERTY: u32 = 0x6474_e553;

/// Segment type classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhdrType {
    Null,
    Load,
    Dynamic,
    Interp,
    Note,
    Shlib,
    Phdr,
    Tls,
    GnuEhFrame,
    GnuStack,
    GnuRelro,
    GnuProperty,
    Unknown(u32),
}

impl PhdrType {
    pub fn from_u32(v: u32) -> Self {
        match v {
            PT_NULL => Self::Null,
            PT_LOAD => Self::Load,
            PT_DYNAMIC => Self::Dynamic,
            PT_INTERP => Self::Interp,
            PT_NOTE => Self::Note,
            PT_SHLIB => Self::Shlib,
            PT_PHDR => Self::Phdr,
            PT_TLS => Self::Tls,
            PT_GNU_EH_FRAME => Self::GnuEhFrame,
            PT_GNU_STACK => Self::GnuStack,
            PT_GNU_RELRO => Self::GnuRelro,
            PT_GNU_PROPERTY => Self::GnuProperty,
            x => Self::Unknown(x),
        }
    }
}

// --- Segment flags ---
pub const PF_X: u32 = 1; // Execute
pub const PF_W: u32 = 2; // Write
pub const PF_R: u32 = 4; // Read

/// Segment permission flags.
#[derive(Debug, Clone, Copy)]
pub struct PhdrFlags(pub u32);

impl PhdrFlags {
    pub fn readable(&self) -> bool {
        self.0 & PF_R != 0
    }
    pub fn writable(&self) -> bool {
        self.0 & PF_W != 0
    }
    pub fn executable(&self) -> bool {
        self.0 & PF_X != 0
    }
}

/// Parsed ELF64 program header entry.
#[derive(Debug, Clone)]
pub struct Elf64Phdr {
    /// Segment type.
    pub p_type: PhdrType,
    /// Raw segment type value.
    pub p_type_raw: u32,
    /// Segment flags (R/W/X).
    pub p_flags: PhdrFlags,
    /// Offset of segment in file.
    pub p_offset: u64,
    /// Virtual address of segment in memory.
    pub p_vaddr: u64,
    /// Physical address (usually same as vaddr).
    pub p_paddr: u64,
    /// Size of segment in file.
    pub p_filesz: u64,
    /// Size of segment in memory (may be > filesz for BSS).
    pub p_memsz: u64,
    /// Alignment.
    pub p_align: u64,
}

/// Error type for program header parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhdrError {
    /// Binary data is too short for the program header table.
    TooShort,
    /// Segment extends beyond binary data.
    SegmentOutOfBounds,
}

impl Elf64Phdr {
    /// Parse a single program header entry from raw bytes.
    pub fn parse(data: &[u8]) -> Result<Self, PhdrError> {
        if data.len() < ELF64_PHDR_SIZE {
            return Err(PhdrError::TooShort);
        }

        let p_type_raw = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let p_flags_raw = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let p_offset = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let p_vaddr = u64::from_le_bytes(data[16..24].try_into().unwrap());
        let p_paddr = u64::from_le_bytes(data[24..32].try_into().unwrap());
        let p_filesz = u64::from_le_bytes(data[32..40].try_into().unwrap());
        let p_memsz = u64::from_le_bytes(data[40..48].try_into().unwrap());
        let p_align = u64::from_le_bytes(data[48..56].try_into().unwrap());

        Ok(Self {
            p_type: PhdrType::from_u32(p_type_raw),
            p_type_raw,
            p_flags: PhdrFlags(p_flags_raw),
            p_offset,
            p_vaddr,
            p_paddr,
            p_filesz,
            p_memsz,
            p_align,
        })
    }

    /// Parse all program headers from a binary given phoff, phentsize, phnum.
    pub fn parse_all(
        data: &[u8],
        phoff: u64,
        phentsize: u16,
        phnum: u16,
    ) -> Result<alloc::vec::Vec<Self>, PhdrError> {
        let mut headers = alloc::vec::Vec::with_capacity(phnum as usize);
        for i in 0..phnum as usize {
            let offset = phoff as usize + i * phentsize as usize;
            let end = offset + ELF64_PHDR_SIZE;
            if end > data.len() {
                return Err(PhdrError::TooShort);
            }
            headers.push(Self::parse(&data[offset..end])?);
        }
        Ok(headers)
    }

    /// Returns true if this is a PT_LOAD segment.
    pub fn is_load(&self) -> bool {
        self.p_type == PhdrType::Load
    }

    /// Returns true if the memory size exceeds file size (BSS region).
    pub fn has_bss(&self) -> bool {
        self.p_memsz > self.p_filesz
    }

    /// Validate that the segment's file data fits within the binary.
    pub fn validate(&self, binary_len: usize) -> Result<(), PhdrError> {
        if self.p_offset as usize + self.p_filesz as usize > binary_len {
            return Err(PhdrError::SegmentOutOfBounds);
        }
        Ok(())
    }
}
