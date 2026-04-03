//! ELF64 section header parsing with symbol and string table support.
//!
//! Provides section header parsing, symbol table iteration, and string
//! table lookups for symbol resolution in loaded ELF binaries.

use alloc::vec::Vec;
use alloc::string::String;

/// Size of an ELF64 section header entry.
pub const ELF64_SHDR_SIZE: usize = 64;

/// Size of an ELF64 symbol table entry.
pub const ELF64_SYM_SIZE: usize = 24;

// --- Section types ---
pub const SHT_NULL: u32 = 0;
pub const SHT_PROGBITS: u32 = 1;
pub const SHT_SYMTAB: u32 = 2;
pub const SHT_STRTAB: u32 = 3;
pub const SHT_RELA: u32 = 4;
pub const SHT_HASH: u32 = 5;
pub const SHT_DYNAMIC: u32 = 6;
pub const SHT_NOTE: u32 = 7;
pub const SHT_NOBITS: u32 = 8; // BSS
pub const SHT_REL: u32 = 9;
pub const SHT_SHLIB: u32 = 10;
pub const SHT_DYNSYM: u32 = 11;
pub const SHT_INIT_ARRAY: u32 = 14;
pub const SHT_FINI_ARRAY: u32 = 15;
pub const SHT_PREINIT_ARRAY: u32 = 16;
pub const SHT_GROUP: u32 = 17;
pub const SHT_SYMTAB_SHNDX: u32 = 18;
pub const SHT_GNU_HASH: u32 = 0x6FFF_FFF6;
pub const SHT_GNU_VERDEF: u32 = 0x6FFF_FFFD;
pub const SHT_GNU_VERNEED: u32 = 0x6FFF_FFFE;
pub const SHT_GNU_VERSYM: u32 = 0x6FFF_FFFF;

// --- Section flags ---
pub const SHF_WRITE: u64 = 1;
pub const SHF_ALLOC: u64 = 2;
pub const SHF_EXECINSTR: u64 = 4;
pub const SHF_MERGE: u64 = 0x10;
pub const SHF_STRINGS: u64 = 0x20;
pub const SHF_INFO_LINK: u64 = 0x40;
pub const SHF_TLS: u64 = 0x400;

/// Section type classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShdrType {
    Null,
    Progbits,
    Symtab,
    Strtab,
    Rela,
    Hash,
    Dynamic,
    Note,
    Nobits,
    Rel,
    Shlib,
    Dynsym,
    InitArray,
    FiniArray,
    PreinitArray,
    Group,
    SymtabShndx,
    GnuHash,
    GnuVerdef,
    GnuVerneed,
    GnuVersym,
    Unknown(u32),
}

impl ShdrType {
    pub fn from_u32(v: u32) -> Self {
        match v {
            SHT_NULL => Self::Null,
            SHT_PROGBITS => Self::Progbits,
            SHT_SYMTAB => Self::Symtab,
            SHT_STRTAB => Self::Strtab,
            SHT_RELA => Self::Rela,
            SHT_HASH => Self::Hash,
            SHT_DYNAMIC => Self::Dynamic,
            SHT_NOTE => Self::Note,
            SHT_NOBITS => Self::Nobits,
            SHT_REL => Self::Rel,
            SHT_SHLIB => Self::Shlib,
            SHT_DYNSYM => Self::Dynsym,
            SHT_INIT_ARRAY => Self::InitArray,
            SHT_FINI_ARRAY => Self::FiniArray,
            SHT_PREINIT_ARRAY => Self::PreinitArray,
            SHT_GROUP => Self::Group,
            SHT_SYMTAB_SHNDX => Self::SymtabShndx,
            SHT_GNU_HASH => Self::GnuHash,
            SHT_GNU_VERDEF => Self::GnuVerdef,
            SHT_GNU_VERNEED => Self::GnuVerneed,
            SHT_GNU_VERSYM => Self::GnuVersym,
            x => Self::Unknown(x),
        }
    }
}

/// Parsed ELF64 section header entry.
#[derive(Debug, Clone)]
pub struct Elf64Shdr {
    /// Index into the section header string table for this section's name.
    pub sh_name: u32,
    /// Section type.
    pub sh_type: ShdrType,
    /// Raw section type.
    pub sh_type_raw: u32,
    /// Section flags.
    pub sh_flags: u64,
    /// Virtual address if section is loaded.
    pub sh_addr: u64,
    /// Offset in file.
    pub sh_offset: u64,
    /// Size in file (0 for NOBITS).
    pub sh_size: u64,
    /// Link to another section (e.g., string table index for symtab).
    pub sh_link: u32,
    /// Additional info (section-type dependent).
    pub sh_info: u32,
    /// Alignment constraint.
    pub sh_addralign: u64,
    /// Entry size for fixed-size entries (e.g., symbol table entries).
    pub sh_entsize: u64,
}

/// Error type for section header parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShdrError {
    TooShort,
    InvalidIndex,
}

impl Elf64Shdr {
    /// Parse a single section header from raw bytes.
    pub fn parse(data: &[u8]) -> Result<Self, ShdrError> {
        if data.len() < ELF64_SHDR_SIZE {
            return Err(ShdrError::TooShort);
        }

        Ok(Self {
            sh_name: u32::from_le_bytes(data[0..4].try_into().unwrap()),
            sh_type: ShdrType::from_u32(u32::from_le_bytes(data[4..8].try_into().unwrap())),
            sh_type_raw: u32::from_le_bytes(data[4..8].try_into().unwrap()),
            sh_flags: u64::from_le_bytes(data[8..16].try_into().unwrap()),
            sh_addr: u64::from_le_bytes(data[16..24].try_into().unwrap()),
            sh_offset: u64::from_le_bytes(data[24..32].try_into().unwrap()),
            sh_size: u64::from_le_bytes(data[32..40].try_into().unwrap()),
            sh_link: u32::from_le_bytes(data[40..44].try_into().unwrap()),
            sh_info: u32::from_le_bytes(data[44..48].try_into().unwrap()),
            sh_addralign: u64::from_le_bytes(data[48..56].try_into().unwrap()),
            sh_entsize: u64::from_le_bytes(data[56..64].try_into().unwrap()),
        })
    }
}

/// Parsed ELF64 symbol table entry.
#[derive(Debug, Clone)]
pub struct Elf64Sym {
    /// Index into the string table for this symbol's name.
    pub st_name: u32,
    /// Symbol info (binding + type packed).
    pub st_info: u8,
    /// Symbol visibility.
    pub st_other: u8,
    /// Section header index this symbol is defined in.
    pub st_shndx: u16,
    /// Symbol value (address).
    pub st_value: u64,
    /// Symbol size.
    pub st_size: u64,
}

// Symbol binding (upper nibble of st_info)
pub const STB_LOCAL: u8 = 0;
pub const STB_GLOBAL: u8 = 1;
pub const STB_WEAK: u8 = 2;

// Symbol type (lower nibble of st_info)
pub const STT_NOTYPE: u8 = 0;
pub const STT_OBJECT: u8 = 1;
pub const STT_FUNC: u8 = 2;
pub const STT_SECTION: u8 = 3;
pub const STT_FILE: u8 = 4;
pub const STT_COMMON: u8 = 5;
pub const STT_TLS: u8 = 6;

// Special section indices
pub const SHN_UNDEF: u16 = 0;
pub const SHN_ABS: u16 = 0xFFF1;
pub const SHN_COMMON: u16 = 0xFFF2;

impl Elf64Sym {
    /// Parse a single symbol table entry from raw bytes.
    pub fn parse(data: &[u8]) -> Result<Self, ShdrError> {
        if data.len() < ELF64_SYM_SIZE {
            return Err(ShdrError::TooShort);
        }

        Ok(Self {
            st_name: u32::from_le_bytes(data[0..4].try_into().unwrap()),
            st_info: data[4],
            st_other: data[5],
            st_shndx: u16::from_le_bytes([data[6], data[7]]),
            st_value: u64::from_le_bytes(data[8..16].try_into().unwrap()),
            st_size: u64::from_le_bytes(data[16..24].try_into().unwrap()),
        })
    }

    /// Get the symbol binding (LOCAL, GLOBAL, WEAK).
    pub fn binding(&self) -> u8 {
        self.st_info >> 4
    }

    /// Get the symbol type (NOTYPE, OBJECT, FUNC, etc.).
    pub fn sym_type(&self) -> u8 {
        self.st_info & 0xF
    }

    /// Check if this symbol is defined (not UND).
    pub fn is_defined(&self) -> bool {
        self.st_shndx != SHN_UNDEF
    }
}

/// Wrapper around the complete set of section headers for convenient access.
pub struct SectionHeaders<'a> {
    pub headers: Vec<Elf64Shdr>,
    pub binary: &'a [u8],
}

impl<'a> SectionHeaders<'a> {
    /// Parse all section headers from a binary.
    pub fn parse(
        binary: &'a [u8],
        shoff: u64,
        shentsize: u16,
        shnum: u16,
    ) -> Result<Self, ShdrError> {
        let mut headers = Vec::with_capacity(shnum as usize);
        for i in 0..shnum as usize {
            let offset = shoff as usize + i * shentsize as usize;
            let end = offset + ELF64_SHDR_SIZE;
            if end > binary.len() {
                return Err(ShdrError::TooShort);
            }
            headers.push(Elf64Shdr::parse(&binary[offset..end])?);
        }
        Ok(Self { headers, binary })
    }

    /// Look up a null-terminated string in a string table section.
    pub fn get_string(&self, strtab_idx: usize, name_offset: u32) -> Option<String> {
        let shdr = self.headers.get(strtab_idx)?;
        let start = shdr.sh_offset as usize + name_offset as usize;
        if start >= self.binary.len() {
            return None;
        }
        let slice = &self.binary[start..];
        let end = slice.iter().position(|&b| b == 0)?;
        let bytes = &slice[..end];
        String::from_utf8(bytes.to_vec()).ok()
    }

    /// Parse all symbols from a SYMTAB or DYNSYM section.
    pub fn parse_symbols(&self, symtab_idx: usize) -> Result<Vec<Elf64Sym>, ShdrError> {
        let shdr = self.headers.get(symtab_idx).ok_or(ShdrError::InvalidIndex)?;
        let offset = shdr.sh_offset as usize;
        let size = shdr.sh_size as usize;
        let entsize = if shdr.sh_entsize > 0 {
            shdr.sh_entsize as usize
        } else {
            ELF64_SYM_SIZE
        };

        let count = size / entsize;
        let mut symbols = Vec::with_capacity(count);
        for i in 0..count {
            let sym_offset = offset + i * entsize;
            let end = sym_offset + ELF64_SYM_SIZE;
            if end > self.binary.len() {
                return Err(ShdrError::TooShort);
            }
            symbols.push(Elf64Sym::parse(&self.binary[sym_offset..end])?);
        }
        Ok(symbols)
    }

    /// Find a section by type.
    pub fn find_by_type(&self, ty: ShdrType) -> Option<(usize, &Elf64Shdr)> {
        self.headers
            .iter()
            .enumerate()
            .find(|(_, s)| s.sh_type == ty)
    }
}
