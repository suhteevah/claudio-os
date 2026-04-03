//! ELF64 header parsing.
//!
//! Decodes the 64-byte ELF64 file header including the 16-byte e_ident magic,
//! class, data encoding, OS/ABI, type, machine, entry point, and offsets to
//! program header and section header tables.

/// Size of the ELF64 file header in bytes.
pub const ELF64_HEADER_SIZE: usize = 64;

/// ELF magic bytes: 0x7F 'E' 'L' 'F'.
pub const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

// --- e_ident offsets ---
pub const EI_MAG0: usize = 0;
pub const EI_MAG3: usize = 3;
pub const EI_CLASS: usize = 4;
pub const EI_DATA: usize = 5;
pub const EI_VERSION: usize = 6;
pub const EI_OSABI: usize = 7;
pub const EI_ABIVERSION: usize = 8;
pub const EI_PAD: usize = 9;
pub const EI_NIDENT: usize = 16;

// --- ELF class ---
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ElfClass {
    None = 0,
    Class32 = 1,
    Class64 = 2,
}

impl ElfClass {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Class32),
            2 => Some(Self::Class64),
            _ => None,
        }
    }
}

// --- Data encoding ---
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ElfData {
    None = 0,
    Lsb = 1, // Little-endian
    Msb = 2, // Big-endian
}

impl ElfData {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Lsb),
            2 => Some(Self::Msb),
            _ => None,
        }
    }
}

// --- OS/ABI ---
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ElfOsAbi {
    None = 0,        // UNIX System V ABI
    Linux = 3,
    FreeBsd = 9,
    Other(u8),
}

impl ElfOsAbi {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::None,
            3 => Self::Linux,
            9 => Self::FreeBsd,
            x => Self::Other(x),
        }
    }
}

// --- ELF type ---
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ElfType {
    None = 0,
    Rel = 1,     // ET_REL — Relocatable
    Exec = 2,    // ET_EXEC — Executable
    Dyn = 3,     // ET_DYN — Shared object / PIE
    Core = 4,    // ET_CORE — Core dump
    Unknown(u16),
}

impl ElfType {
    pub fn from_u16(v: u16) -> Self {
        match v {
            0 => Self::None,
            1 => Self::Rel,
            2 => Self::Exec,
            3 => Self::Dyn,
            4 => Self::Core,
            x => Self::Unknown(x),
        }
    }
}

// --- ELF machine types ---
pub const EM_X86_64: u16 = 62;
pub const EM_386: u16 = 3;
pub const EM_AARCH64: u16 = 183;

// --- ELF version ---
pub const EV_CURRENT: u8 = 1;

/// Parsed ELF64 file header.
#[derive(Debug, Clone)]
pub struct Elf64Header {
    /// ELF identification bytes (16 bytes).
    pub e_ident: [u8; EI_NIDENT],
    /// ELF class (32/64-bit).
    pub class: ElfClass,
    /// Data encoding (little/big-endian).
    pub data: ElfData,
    /// ELF version in ident.
    pub version_ident: u8,
    /// OS/ABI.
    pub os_abi: ElfOsAbi,
    /// ABI version.
    pub abi_version: u8,
    /// Object file type.
    pub e_type: ElfType,
    /// Target architecture.
    pub e_machine: u16,
    /// Object file version.
    pub e_version: u32,
    /// Entry point virtual address.
    pub e_entry: u64,
    /// Program header table file offset.
    pub e_phoff: u64,
    /// Section header table file offset.
    pub e_shoff: u64,
    /// Processor-specific flags.
    pub e_flags: u32,
    /// ELF header size.
    pub e_ehsize: u16,
    /// Program header table entry size.
    pub e_phentsize: u16,
    /// Number of program header entries.
    pub e_phnum: u16,
    /// Section header table entry size.
    pub e_shentsize: u16,
    /// Number of section header entries.
    pub e_shnum: u16,
    /// Section header string table index.
    pub e_shstrndx: u16,
}

/// Error type for header parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    TooShort,
    BadMagic,
    Not64Bit,
    NotLittleEndian,
    BadVersion,
    NotExecutable,
    WrongArchitecture,
}

impl Elf64Header {
    /// Parse an ELF64 header from the beginning of a binary blob.
    pub fn parse(data: &[u8]) -> Result<Self, HeaderError> {
        if data.len() < ELF64_HEADER_SIZE {
            return Err(HeaderError::TooShort);
        }

        // Check magic
        if data[EI_MAG0..=EI_MAG3] != ELF_MAGIC {
            return Err(HeaderError::BadMagic);
        }

        let class = ElfClass::from_u8(data[EI_CLASS]).unwrap_or(ElfClass::None);
        if class != ElfClass::Class64 {
            return Err(HeaderError::Not64Bit);
        }

        let data_enc = ElfData::from_u8(data[EI_DATA]).unwrap_or(ElfData::None);
        if data_enc != ElfData::Lsb {
            return Err(HeaderError::NotLittleEndian);
        }

        let version_ident = data[EI_VERSION];
        if version_ident != EV_CURRENT {
            return Err(HeaderError::BadVersion);
        }

        let os_abi = ElfOsAbi::from_u8(data[EI_OSABI]);
        let abi_version = data[EI_ABIVERSION];

        let mut ident = [0u8; EI_NIDENT];
        ident.copy_from_slice(&data[..EI_NIDENT]);

        // Remaining fields are little-endian
        let e_type = ElfType::from_u16(u16::from_le_bytes([data[16], data[17]]));
        match e_type {
            ElfType::Exec | ElfType::Dyn => {}
            _ => return Err(HeaderError::NotExecutable),
        }

        let e_machine = u16::from_le_bytes([data[18], data[19]]);
        if e_machine != EM_X86_64 {
            return Err(HeaderError::WrongArchitecture);
        }

        let e_version = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
        let e_entry = u64::from_le_bytes(data[24..32].try_into().unwrap());
        let e_phoff = u64::from_le_bytes(data[32..40].try_into().unwrap());
        let e_shoff = u64::from_le_bytes(data[40..48].try_into().unwrap());
        let e_flags = u32::from_le_bytes(data[48..52].try_into().unwrap());
        let e_ehsize = u16::from_le_bytes([data[52], data[53]]);
        let e_phentsize = u16::from_le_bytes([data[54], data[55]]);
        let e_phnum = u16::from_le_bytes([data[56], data[57]]);
        let e_shentsize = u16::from_le_bytes([data[58], data[59]]);
        let e_shnum = u16::from_le_bytes([data[60], data[61]]);
        let e_shstrndx = u16::from_le_bytes([data[62], data[63]]);

        Ok(Self {
            e_ident: ident,
            class,
            data: data_enc,
            version_ident,
            os_abi,
            abi_version,
            e_type,
            e_machine,
            e_version,
            e_entry,
            e_phoff,
            e_shoff,
            e_flags,
            e_ehsize,
            e_phentsize,
            e_phnum,
            e_shentsize,
            e_shnum,
            e_shstrndx,
        })
    }

    /// Returns true if this is a position-independent executable (PIE).
    pub fn is_pie(&self) -> bool {
        self.e_type == ElfType::Dyn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_too_short() {
        assert_eq!(Elf64Header::parse(&[0; 10]), Err(HeaderError::TooShort));
    }

    #[test]
    fn test_bad_magic() {
        let mut buf = [0u8; 64];
        buf[0] = 0x00;
        assert_eq!(Elf64Header::parse(&buf), Err(HeaderError::BadMagic));
    }
}
