//! System Description Table (SDT) parsing.
//!
//! All ACPI tables share a common 36-byte header. The RSDT and XSDT are
//! root tables that contain pointers to all other SDTs.

use alloc::string::String;
use alloc::vec::Vec;
use crate::{AcpiError, Rsdp};

/// SDT header size in bytes.
pub const SDT_HEADER_SIZE: usize = 36;

/// Common ACPI SDT header (36 bytes), shared by all ACPI tables.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct SdtHeader {
    /// 4-byte ASCII signature (e.g., "FACP", "APIC", "MCFG").
    pub signature: [u8; 4],
    /// Total length of the table including the header.
    pub length: u32,
    /// Table revision.
    pub revision: u8,
    /// Checksum (entire table must sum to 0 mod 256).
    pub checksum: u8,
    /// OEM ID (6 bytes).
    pub oem_id: [u8; 6],
    /// OEM table ID (8 bytes).
    pub oem_table_id: [u8; 8],
    /// OEM revision.
    pub oem_revision: u32,
    /// Creator ID (e.g., ASL compiler vendor).
    pub creator_id: u32,
    /// Creator revision.
    pub creator_revision: u32,
}

impl SdtHeader {
    /// Read an SDT header from a physical address.
    ///
    /// # Safety
    ///
    /// `phys_addr` must point to a valid, mapped SDT header.
    pub unsafe fn from_address(phys_addr: u64) -> Result<Self, AcpiError> {
        if phys_addr == 0 {
            log::error!("sdt: attempted to read header from null address");
            return Err(AcpiError::InvalidPointer);
        }

        let header = unsafe {
            core::ptr::read_unaligned(phys_addr as *const SdtHeader)
        };

        let sig_str = core::str::from_utf8(&header.signature).unwrap_or("????");
        let hdr_length = header.length;
        let hdr_revision = header.revision;
        log::trace!(
            "sdt: header at {:#X}: sig='{}' len={} rev={}",
            phys_addr,
            sig_str,
            hdr_length,
            hdr_revision,
        );

        Ok(header)
    }

    /// Get the signature as a string slice.
    pub fn signature_str(&self) -> &str {
        core::str::from_utf8(&self.signature).unwrap_or("????")
    }

    /// Validate the checksum of the entire table.
    ///
    /// # Safety
    ///
    /// The region `[phys_addr .. phys_addr + self.length]` must be mapped and readable.
    pub unsafe fn validate_checksum(&self, phys_addr: u64) -> Result<(), AcpiError> {
        let length = self.length as usize;
        let bytes = unsafe { core::slice::from_raw_parts(phys_addr as *const u8, length) };
        let sum: u8 = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));

        if sum != 0 {
            log::error!(
                "sdt: checksum failed for '{}' at {:#X}: sum={} len={}",
                self.signature_str(),
                phys_addr,
                sum,
                length,
            );
            return Err(AcpiError::ChecksumFailed);
        }

        log::trace!("sdt: checksum OK for '{}' at {:#X}", self.signature_str(), phys_addr);
        Ok(())
    }
}

/// RSDT (Root System Description Table) — contains 32-bit pointers to SDTs.
#[derive(Debug)]
pub struct Rsdt {
    /// Physical address of the RSDT.
    pub address: u64,
    /// Header of the RSDT.
    pub header: SdtHeader,
    /// 32-bit physical addresses of child SDTs.
    pub entries: Vec<u32>,
}

impl Rsdt {
    /// Parse the RSDT from its physical address.
    ///
    /// # Safety
    ///
    /// `phys_addr` must point to a valid, mapped RSDT.
    pub unsafe fn from_address(phys_addr: u64) -> Result<Self, AcpiError> {
        log::info!("rsdt: parsing at {:#X}", phys_addr);

        let header = unsafe { SdtHeader::from_address(phys_addr)? };

        if &header.signature != b"RSDT" {
            log::error!(
                "rsdt: expected 'RSDT' signature, got '{}'",
                header.signature_str()
            );
            return Err(AcpiError::SignatureMismatch);
        }

        unsafe { header.validate_checksum(phys_addr)?; }

        let entry_count =
            (header.length as usize - SDT_HEADER_SIZE) / core::mem::size_of::<u32>();
        let hdr_length = header.length;
        log::debug!("rsdt: {} entries (table length={})", entry_count, hdr_length);

        let entry_base = phys_addr + SDT_HEADER_SIZE as u64;
        let mut entries = Vec::with_capacity(entry_count);
        for i in 0..entry_count {
            let ptr_addr = entry_base + (i * 4) as u64;
            let entry: u32 = unsafe { core::ptr::read_unaligned(ptr_addr as *const u32) };
            log::trace!("rsdt: entry[{}] = {:#X}", i, entry);
            entries.push(entry);
        }

        Ok(Rsdt {
            address: phys_addr,
            header,
            entries,
        })
    }
}

/// XSDT (Extended System Description Table) — contains 64-bit pointers to SDTs.
#[derive(Debug)]
pub struct Xsdt {
    /// Physical address of the XSDT.
    pub address: u64,
    /// Header of the XSDT.
    pub header: SdtHeader,
    /// 64-bit physical addresses of child SDTs.
    pub entries: Vec<u64>,
}

impl Xsdt {
    /// Parse the XSDT from its physical address.
    ///
    /// # Safety
    ///
    /// `phys_addr` must point to a valid, mapped XSDT.
    pub unsafe fn from_address(phys_addr: u64) -> Result<Self, AcpiError> {
        log::info!("xsdt: parsing at {:#X}", phys_addr);

        let header = unsafe { SdtHeader::from_address(phys_addr)? };

        if &header.signature != b"XSDT" {
            log::error!(
                "xsdt: expected 'XSDT' signature, got '{}'",
                header.signature_str()
            );
            return Err(AcpiError::SignatureMismatch);
        }

        unsafe { header.validate_checksum(phys_addr)?; }

        let entry_count =
            (header.length as usize - SDT_HEADER_SIZE) / core::mem::size_of::<u64>();
        let hdr_length = header.length;
        log::debug!("xsdt: {} entries (table length={})", entry_count, hdr_length);

        let entry_base = phys_addr + SDT_HEADER_SIZE as u64;
        let mut entries = Vec::with_capacity(entry_count);
        for i in 0..entry_count {
            let ptr_addr = entry_base + (i * 8) as u64;
            let entry: u64 = unsafe { core::ptr::read_unaligned(ptr_addr as *const u64) };
            log::trace!("xsdt: entry[{}] = {:#X}", i, entry);
            entries.push(entry);
        }

        Ok(Xsdt {
            address: phys_addr,
            header,
            entries,
        })
    }
}

/// Collection of all discovered ACPI tables.
#[derive(Debug)]
pub struct AcpiTables {
    /// RSDT (may be None if XSDT is used).
    pub rsdt: Option<Rsdt>,
    /// XSDT (preferred over RSDT when available).
    pub xsdt: Option<Xsdt>,
    /// Discovered table entries: (signature, physical address).
    pub tables: Vec<(String, u64)>,
}

impl AcpiTables {
    /// Build the table collection from a validated RSDP.
    ///
    /// Prefers XSDT over RSDT when both are available.
    ///
    /// # Safety
    ///
    /// All table pointers in the RSDT/XSDT must be mapped and readable.
    pub unsafe fn from_rsdp(rsdp: &Rsdp) -> Result<Self, AcpiError> {
        let mut tables_out = AcpiTables {
            rsdt: None,
            xsdt: None,
            tables: Vec::new(),
        };

        // Prefer XSDT (64-bit pointers) over RSDT (32-bit pointers)
        if let Some(xsdt_addr) = rsdp.xsdt_address {
            log::info!("acpi: using XSDT at {:#X}", xsdt_addr);
            let xsdt = unsafe { Xsdt::from_address(xsdt_addr)? };

            for &entry_addr in &xsdt.entries {
                if entry_addr == 0 {
                    continue;
                }
                match unsafe { SdtHeader::from_address(entry_addr) } {
                    Ok(header) => {
                        let sig = String::from(header.signature_str());
                        let hdr_length = header.length;
                        log::info!(
                            "acpi: found table '{}' at {:#X} (len={})",
                            sig,
                            entry_addr,
                            hdr_length,
                        );
                        tables_out.tables.push((sig, entry_addr));
                    }
                    Err(e) => {
                        log::warn!("acpi: failed to read SDT header at {:#X}: {:?}", entry_addr, e);
                    }
                }
            }

            tables_out.xsdt = Some(xsdt);
        } else {
            let rsdt_addr = rsdp.rsdt_address as u64;
            log::info!("acpi: using RSDT at {:#X}", rsdt_addr);
            let rsdt = unsafe { Rsdt::from_address(rsdt_addr)? };

            for &entry_addr in &rsdt.entries {
                if entry_addr == 0 {
                    continue;
                }
                let entry_addr_64 = entry_addr as u64;
                match unsafe { SdtHeader::from_address(entry_addr_64) } {
                    Ok(header) => {
                        let sig = String::from(header.signature_str());
                        let hdr_length = header.length;
                        log::info!(
                            "acpi: found table '{}' at {:#X} (len={})",
                            sig,
                            entry_addr_64,
                            hdr_length,
                        );
                        tables_out.tables.push((sig, entry_addr_64));
                    }
                    Err(e) => {
                        log::warn!(
                            "acpi: failed to read SDT header at {:#X}: {:?}",
                            entry_addr_64,
                            e,
                        );
                    }
                }
            }

            tables_out.rsdt = Some(rsdt);
        }

        log::info!("acpi: discovered {} tables total", tables_out.tables.len());
        Ok(tables_out)
    }

    /// Find a table by its 4-byte ASCII signature (e.g., "FACP", "APIC", "MCFG").
    pub fn find_table(&self, signature: &str) -> Option<u64> {
        for (sig, addr) in &self.tables {
            if sig == signature {
                log::debug!("acpi: lookup '{}' -> {:#X}", signature, addr);
                return Some(*addr);
            }
        }
        log::debug!("acpi: lookup '{}' -> not found", signature);
        None
    }
}
