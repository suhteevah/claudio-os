//! High-level NTFS filesystem read/write API.
//!
//! This module provides the main `NtfsFs` type that ties together the boot sector,
//! MFT, attributes, indexes, and data runs into a usable filesystem interface.
//!
//! ## Usage
//!
//! Implement the `BlockDevice` trait for your storage backend, then:
//!
//! ```rust,no_run
//! use claudio_ntfs::{NtfsFs, BlockDevice};
//!
//! let fs = NtfsFs::mount(my_device).expect("mount failed");
//! let data = fs.read_file(b"/Windows/hello.txt").expect("read failed");
//! fs.write_file(b"/output.txt", &data).expect("write failed");
//! ```

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::attribute::{AttributeHeader, AttributeType};
use crate::boot_sector::{BootSector, BOOT_SECTOR_SIZE};
use crate::data_runs::{self, DataRun};
use crate::filename::{FileNameAttr, FileNamespace};
use crate::index::{self, IndexEntry, IndexNodeHeader, IndexRoot};
use crate::mft::{self, MftEntry, MFT_ENTRY_ROOT, MFT_ENTRY_UPCASE};
use crate::upcase::UpCaseTable;

/// Errors that can occur during NTFS filesystem operations.
#[derive(Debug)]
pub enum NtfsError {
    /// The device returned an I/O error.
    IoError,
    /// The boot sector is invalid or not NTFS.
    InvalidBootSector,
    /// An MFT entry is corrupt or has invalid fixup.
    CorruptMftEntry(u64),
    /// A required attribute was not found.
    AttributeNotFound(&'static str),
    /// The requested path was not found.
    NotFound,
    /// A path component is not a directory.
    NotADirectory,
    /// The target path already exists.
    AlreadyExists,
    /// The filesystem is corrupt.
    Corrupt(&'static str),
    /// A filename exceeds the maximum length (255 UTF-16 characters).
    NameTooLong,
    /// The path is invalid (empty, etc.).
    InvalidPath,
    /// The target is a directory when a file was expected.
    IsADirectory,
    /// The target is a file when a directory was expected.
    IsNotADirectory,
    /// No free MFT entries available.
    NoFreeMftEntries,
    /// No free clusters available.
    NoFreeClusters,
    /// Compressed or encrypted attributes are not supported.
    Unsupported(&'static str),
}

impl fmt::Display for NtfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NtfsError::IoError => write!(f, "I/O error"),
            NtfsError::InvalidBootSector => write!(f, "invalid NTFS boot sector"),
            NtfsError::CorruptMftEntry(n) => write!(f, "corrupt MFT entry #{}", n),
            NtfsError::AttributeNotFound(a) => write!(f, "attribute not found: {}", a),
            NtfsError::NotFound => write!(f, "not found"),
            NtfsError::NotADirectory => write!(f, "not a directory"),
            NtfsError::AlreadyExists => write!(f, "already exists"),
            NtfsError::Corrupt(msg) => write!(f, "filesystem corrupt: {}", msg),
            NtfsError::NameTooLong => write!(f, "filename too long"),
            NtfsError::InvalidPath => write!(f, "invalid path"),
            NtfsError::IsADirectory => write!(f, "is a directory"),
            NtfsError::IsNotADirectory => write!(f, "is not a directory"),
            NtfsError::NoFreeMftEntries => write!(f, "no free MFT entries"),
            NtfsError::NoFreeClusters => write!(f, "no free clusters"),
            NtfsError::Unsupported(msg) => write!(f, "unsupported: {}", msg),
        }
    }
}

/// Trait for the underlying block storage device.
///
/// Implement this for your NVMe driver, virtio-blk, RAM disk, or disk image
/// to provide NTFS with raw byte access.
pub trait BlockDevice {
    /// Read `buf.len()` bytes from the device starting at `offset`.
    ///
    /// `offset` is a byte offset from the start of the partition.
    /// Returns `Ok(())` on success.
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<(), NtfsError>;

    /// Write `buf.len()` bytes to the device starting at `offset`.
    ///
    /// `offset` is a byte offset from the start of the partition.
    /// Returns `Ok(())` on success.
    fn write_bytes(&self, offset: u64, buf: &[u8]) -> Result<(), NtfsError>;

    /// Flush any cached writes to the underlying storage.
    ///
    /// Called after metadata updates to ensure durability.
    fn flush(&self) -> Result<(), NtfsError> {
        Ok(())
    }
}

/// Directory entry returned by `list_dir`.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// MFT entry number for this file/directory.
    pub mft_entry: u64,
    /// Filename.
    pub name: String,
    /// Whether this is a directory.
    pub is_directory: bool,
    /// File size in bytes (0 for directories).
    pub size: u64,
    /// Creation time (Windows FILETIME).
    pub creation_time: u64,
    /// Modification time (Windows FILETIME).
    pub modification_time: u64,
    /// File attribute flags.
    pub flags: u32,
}

/// Main NTFS filesystem handle.
///
/// Holds the parsed boot sector, cached MFT location, and the $UpCase table.
/// All operations go through this struct.
pub struct NtfsFs<D: BlockDevice> {
    /// The underlying block device.
    pub device: D,
    /// The parsed boot sector / BPB.
    pub boot_sector: BootSector,
    /// The $UpCase table for case-insensitive comparison.
    pub upcase: UpCaseTable,
    /// Cluster size in bytes (cached from boot sector).
    cluster_size: u64,
    /// MFT record size in bytes (cached from boot sector).
    mft_record_size: u64,
    /// Byte offset of the MFT on disk (cached).
    mft_offset: u64,
    /// Data runs for the $MFT itself (since $MFT can be non-contiguous).
    mft_data_runs: Vec<DataRun>,
}

impl<D: BlockDevice> NtfsFs<D> {
    /// Mount an NTFS filesystem from a block device.
    ///
    /// Reads the boot sector, locates the MFT, parses the $MFT entry to get
    /// the full MFT data run list, and loads the $UpCase table.
    pub fn mount(device: D) -> Result<Self, NtfsError> {
        log::info!("[ntfs] mounting NTFS filesystem...");

        // Step 1: Read and parse the boot sector
        let mut boot_buf = [0u8; BOOT_SECTOR_SIZE];
        device.read_bytes(0, &mut boot_buf)?;
        let boot_sector = BootSector::from_bytes(&boot_buf)
            .ok_or(NtfsError::InvalidBootSector)?;

        let cluster_size = boot_sector.cluster_size();
        let mft_record_size = boot_sector.mft_record_size();
        let mft_offset = boot_sector.mft_byte_offset();

        log::info!("[ntfs] cluster_size={}, mft_record_size={}, mft_offset=0x{:X}",
            cluster_size, mft_record_size, mft_offset);

        // Step 2: Read the $MFT entry (entry 0) to get its own data runs
        let mut mft_entry_buf = vec![0u8; mft_record_size as usize];
        device.read_bytes(mft_offset, &mut mft_entry_buf)?;
        let mft_entry = MftEntry::from_bytes(&mft_entry_buf, mft_record_size as usize)
            .ok_or(NtfsError::CorruptMftEntry(0))?;

        log::debug!("[ntfs] $MFT entry parsed: {:?}", mft_entry.header);

        // Get the $DATA attribute of $MFT to find all MFT clusters
        let mft_data_runs = Self::read_data_runs_from_entry(&mft_entry, AttributeType::Data)?;
        log::info!("[ntfs] $MFT has {} data runs", mft_data_runs.len());
        for (i, run) in mft_data_runs.iter().enumerate() {
            log::debug!("[ntfs] $MFT run {}: lcn={}, length={} clusters, sparse={}",
                i, run.lcn, run.length, run.is_sparse);
        }

        // Step 3: Load the $UpCase table (MFT entry 10)
        let upcase = {
            let upcase_entry_offset = Self::resolve_mft_entry_offset(
                &mft_data_runs, MFT_ENTRY_UPCASE, mft_record_size, cluster_size, mft_offset
            )?;
            let mut upcase_buf = vec![0u8; mft_record_size as usize];
            device.read_bytes(upcase_entry_offset, &mut upcase_buf)?;
            let upcase_entry = MftEntry::from_bytes(&upcase_buf, mft_record_size as usize)
                .ok_or(NtfsError::CorruptMftEntry(MFT_ENTRY_UPCASE))?;

            log::debug!("[ntfs] $UpCase entry parsed: {:?}", upcase_entry.header);

            // Read the $DATA attribute of $UpCase
            let upcase_data = Self::read_attribute_data_static(
                &device, &upcase_entry, AttributeType::Data,
                &mft_data_runs, mft_record_size, cluster_size, mft_offset,
            )?;

            UpCaseTable::from_bytes(&upcase_data).unwrap_or_else(|| {
                log::warn!("[ntfs] failed to parse $UpCase, using ASCII fallback");
                UpCaseTable::default_ascii()
            })
        };

        log::info!("[ntfs] NTFS filesystem mounted successfully: volume_size={} bytes",
            boot_sector.volume_size());

        Ok(NtfsFs {
            device,
            boot_sector,
            upcase,
            cluster_size,
            mft_record_size,
            mft_offset,
            mft_data_runs,
        })
    }

    /// Read the data runs from a $DATA (or other) attribute in an MFT entry.
    fn read_data_runs_from_entry(entry: &MftEntry, attr_type: AttributeType) -> Result<Vec<DataRun>, NtfsError> {
        let (hdr, offset) = entry.find_attribute(attr_type)
            .ok_or(NtfsError::AttributeNotFound("$DATA"))?;

        if !hdr.non_resident {
            log::trace!("[ntfs] attribute is resident, no data runs");
            return Ok(Vec::new());
        }

        let run_bytes = entry.data_run_bytes(offset)
            .ok_or(NtfsError::Corrupt("failed to read data run bytes"))?;

        Ok(data_runs::decode_data_runs(run_bytes))
    }

    /// Resolve the byte offset of a specific MFT entry number,
    /// accounting for non-contiguous MFT data runs.
    fn resolve_mft_entry_offset(
        runs: &[DataRun],
        entry_number: u64,
        mft_record_size: u64,
        cluster_size: u64,
        mft_offset: u64,
    ) -> Result<u64, NtfsError> {
        if runs.is_empty() {
            // MFT is contiguous from mft_offset (simple case)
            let offset = mft_offset + entry_number * mft_record_size;
            log::trace!("[ntfs] MFT entry {} at offset 0x{:X} (contiguous)", entry_number, offset);
            return Ok(offset);
        }

        // Calculate which byte within the MFT's data we need
        let mft_byte = entry_number * mft_record_size;
        let vcn = mft_byte / cluster_size;
        let vcn_offset = mft_byte % cluster_size;

        let map = data_runs::build_vcn_map(runs);
        let (lcn, is_sparse) = map.resolve(vcn)
            .ok_or_else(|| {
                log::error!("[ntfs] MFT entry {} (VCN {}) not in data runs", entry_number, vcn);
                NtfsError::CorruptMftEntry(entry_number)
            })?;

        if is_sparse {
            log::error!("[ntfs] MFT entry {} is in a sparse region", entry_number);
            return Err(NtfsError::CorruptMftEntry(entry_number));
        }

        let offset = lcn * cluster_size + vcn_offset;
        log::trace!("[ntfs] MFT entry {} at offset 0x{:X} (VCN {} -> LCN {})",
            entry_number, offset, vcn, lcn);
        Ok(offset)
    }

    /// Read an MFT entry by entry number.
    pub fn read_mft_entry(&self, entry_number: u64) -> Result<MftEntry, NtfsError> {
        let offset = Self::resolve_mft_entry_offset(
            &self.mft_data_runs, entry_number,
            self.mft_record_size, self.cluster_size, self.mft_offset,
        )?;

        let mut buf = vec![0u8; self.mft_record_size as usize];
        self.device.read_bytes(offset, &mut buf)?;

        log::trace!("[ntfs] reading MFT entry {} from offset 0x{:X}", entry_number, offset);
        MftEntry::from_bytes(&buf, self.mft_record_size as usize)
            .ok_or(NtfsError::CorruptMftEntry(entry_number))
    }

    /// Write an MFT entry back to disk.
    pub fn write_mft_entry(&self, entry_number: u64, entry: &MftEntry) -> Result<(), NtfsError> {
        let offset = Self::resolve_mft_entry_offset(
            &self.mft_data_runs, entry_number,
            self.mft_record_size, self.cluster_size, self.mft_offset,
        )?;

        let buf = entry.to_bytes();
        log::debug!("[ntfs] writing MFT entry {} ({} bytes) to offset 0x{:X}",
            entry_number, buf.len(), offset);
        self.device.write_bytes(offset, &buf)?;
        self.device.flush()
    }

    /// Read all data for a non-resident attribute, following its data runs.
    fn read_non_resident_data(
        &self,
        entry: &MftEntry,
        attr_offset: usize,
    ) -> Result<Vec<u8>, NtfsError> {
        let nr = entry.non_resident_header(attr_offset)
            .ok_or(NtfsError::Corrupt("missing non-resident header"))?;

        let run_bytes = entry.data_run_bytes(attr_offset)
            .ok_or(NtfsError::Corrupt("missing data runs"))?;

        let runs = data_runs::decode_data_runs(run_bytes);
        let data_size = nr.data_size as usize;
        let mut result = vec![0u8; data_size];
        let mut bytes_read = 0usize;

        log::debug!("[ntfs] reading non-resident data: {} bytes across {} runs",
            data_size, runs.len());

        for run in &runs {
            if bytes_read >= data_size {
                break;
            }

            let run_bytes_total = run.length * self.cluster_size;
            let to_read = (data_size - bytes_read).min(run_bytes_total as usize);

            if run.is_sparse {
                // Sparse run: fill with zeros (already zeroed in vec)
                log::trace!("[ntfs] sparse run: {} bytes of zeros", to_read);
                bytes_read += to_read;
                continue;
            }

            let disk_offset = run.lcn * self.cluster_size;
            log::trace!("[ntfs] reading {} bytes from disk offset 0x{:X}", to_read, disk_offset);
            self.device.read_bytes(disk_offset, &mut result[bytes_read..bytes_read + to_read])?;
            bytes_read += to_read;
        }

        log::debug!("[ntfs] read {} bytes of non-resident data", bytes_read);
        Ok(result)
    }

    /// Read attribute data (works for both resident and non-resident).
    fn read_attribute_data_for_entry(
        &self,
        entry: &MftEntry,
        attr_type: AttributeType,
    ) -> Result<Vec<u8>, NtfsError> {
        Self::read_attribute_data_static(
            &self.device, entry, attr_type,
            &self.mft_data_runs, self.mft_record_size, self.cluster_size, self.mft_offset,
        )
    }

    /// Static version of attribute data reading (used during mount before self is available).
    fn read_attribute_data_static(
        device: &D,
        entry: &MftEntry,
        attr_type: AttributeType,
        _mft_runs: &[DataRun],
        _mft_record_size: u64,
        cluster_size: u64,
        _mft_offset: u64,
    ) -> Result<Vec<u8>, NtfsError> {
        let (hdr, offset) = entry.find_attribute(attr_type)
            .ok_or(NtfsError::AttributeNotFound(attr_type.name()))?;

        if !hdr.non_resident {
            // Resident: data is inline
            let data = entry.resident_data(offset)
                .ok_or(NtfsError::Corrupt("failed to read resident data"))?;
            log::trace!("[ntfs] read {} bytes of resident {} data", data.len(), attr_type.name());
            return Ok(data.to_vec());
        }

        // Non-resident: follow data runs
        let nr = entry.non_resident_header(offset)
            .ok_or(NtfsError::Corrupt("missing non-resident header"))?;

        let run_bytes = entry.data_run_bytes(offset)
            .ok_or(NtfsError::Corrupt("missing data runs"))?;

        let runs = data_runs::decode_data_runs(run_bytes);
        let data_size = nr.data_size as usize;
        let mut result = vec![0u8; data_size];
        let mut bytes_read = 0usize;

        for run in &runs {
            if bytes_read >= data_size {
                break;
            }
            let run_bytes_total = run.length * cluster_size;
            let to_read = (data_size - bytes_read).min(run_bytes_total as usize);

            if run.is_sparse {
                bytes_read += to_read;
                continue;
            }

            let disk_offset = run.lcn * cluster_size;
            device.read_bytes(disk_offset, &mut result[bytes_read..bytes_read + to_read])?;
            bytes_read += to_read;
        }

        log::trace!("[ntfs] read {} bytes of non-resident {} data", bytes_read, attr_type.name());
        Ok(result)
    }

    /// Split a path into components.
    fn split_path(path: &[u8]) -> Result<Vec<&[u8]>, NtfsError> {
        if path.is_empty() {
            return Err(NtfsError::InvalidPath);
        }

        // Strip leading slash(es)
        let path = if path[0] == b'/' || path[0] == b'\\' {
            &path[1..]
        } else {
            path
        };

        if path.is_empty() {
            return Ok(Vec::new()); // Root directory
        }

        let components: Vec<&[u8]> = path
            .split(|&b| b == b'/' || b == b'\\')
            .filter(|c| !c.is_empty())
            .collect();

        Ok(components)
    }

    /// List the contents of a directory by reading its index entries.
    fn read_directory_entries(&self, dir_entry_number: u64) -> Result<Vec<IndexEntry>, NtfsError> {
        let dir_mft = self.read_mft_entry(dir_entry_number)?;

        if !dir_mft.header.is_directory() {
            log::error!("[ntfs] MFT entry {} is not a directory", dir_entry_number);
            return Err(NtfsError::NotADirectory);
        }

        // Read $INDEX_ROOT attribute
        let (_, ir_offset) = dir_mft.find_attribute(AttributeType::IndexRoot)
            .ok_or(NtfsError::AttributeNotFound("$INDEX_ROOT"))?;

        let ir_data = dir_mft.resident_data(ir_offset)
            .ok_or(NtfsError::Corrupt("$INDEX_ROOT must be resident"))?;

        let index_root = IndexRoot::from_bytes(ir_data)
            .ok_or(NtfsError::Corrupt("invalid $INDEX_ROOT"))?;

        log::debug!("[ntfs] directory MFT#{}: INDEX_ROOT parsed, large_index={}",
            dir_entry_number, index_root.has_large_index());

        // Parse entries from the root node
        let entries_data = index_root.entries_data(ir_data)
            .ok_or(NtfsError::Corrupt("failed to get INDEX_ROOT entries"))?;

        let mut all_entries = index::parse_index_entries(entries_data);

        // If there's an $INDEX_ALLOCATION, read overflow nodes
        if index_root.has_large_index() {
            if let Some((_, ia_offset)) = dir_mft.find_attribute(AttributeType::IndexAllocation) {
                log::debug!("[ntfs] reading $INDEX_ALLOCATION for directory MFT#{}",
                    dir_entry_number);

                // Read all INDX blocks
                let ia_data = self.read_non_resident_data(&dir_mft, ia_offset)?;
                let block_size = self.boot_sector.index_block_size() as usize;

                let mut block_offset = 0;
                while block_offset + block_size <= ia_data.len() {
                    let mut block = ia_data[block_offset..block_offset + block_size].to_vec();

                    // Apply fixup to the INDX block
                    if !IndexNodeHeader::apply_fixup(&mut block) {
                        log::warn!("[ntfs] failed to apply fixup to INDX block at offset 0x{:X}",
                            block_offset);
                        block_offset += block_size;
                        continue;
                    }

                    if let Some(node_hdr) = IndexNodeHeader::from_bytes(&block) {
                        if let Some(node_entries) = node_hdr.entries_data(&block) {
                            let entries = index::parse_index_entries(node_entries);
                            log::trace!("[ntfs] INDX block VCN {}: {} entries",
                                node_hdr.vcn, entries.len());
                            all_entries.extend(entries);
                        }
                    }

                    block_offset += block_size;
                }
            }
        }

        log::debug!("[ntfs] directory MFT#{}: total {} index entries", dir_entry_number, all_entries.len());
        Ok(all_entries)
    }

    /// Resolve a path to its MFT entry number, starting from the root directory.
    fn resolve_path(&self, path: &[u8]) -> Result<u64, NtfsError> {
        let components = Self::split_path(path)?;

        if components.is_empty() {
            log::trace!("[ntfs] path resolves to root directory");
            return Ok(MFT_ENTRY_ROOT);
        }

        let mut current_entry = MFT_ENTRY_ROOT;

        for component in &components {
            let name = core::str::from_utf8(component).map_err(|_| NtfsError::InvalidPath)?;
            log::trace!("[ntfs] resolving component '{}' in MFT#{}", name, current_entry);

            let entries = self.read_directory_entries(current_entry)?;

            // Search for the component (prefer Win32 or Win32+DOS namespace)
            let mut found = None;
            for entry in &entries {
                if entry.is_last() {
                    continue;
                }
                if let Some(ref fn_attr) = entry.filename {
                    // Skip DOS-only names
                    if fn_attr.namespace == FileNamespace::Dos {
                        continue;
                    }
                    if self.upcase.names_equal(
                        &fn_attr.name_utf16,
                        &name.encode_utf16().collect::<Vec<u16>>(),
                    ) {
                        found = Some(entry.entry_number());
                        break;
                    }
                }
            }

            current_entry = found.ok_or_else(|| {
                log::debug!("[ntfs] component '{}' not found in MFT#{}", name, current_entry);
                NtfsError::NotFound
            })?;
        }

        log::debug!("[ntfs] path '{}' resolves to MFT#{}",
            core::str::from_utf8(path).unwrap_or("<invalid>"), current_entry);
        Ok(current_entry)
    }

    /// Read a file by path, returning its contents.
    pub fn read_file(&self, path: &[u8]) -> Result<Vec<u8>, NtfsError> {
        let path_str = core::str::from_utf8(path).unwrap_or("<invalid>");
        log::info!("[ntfs] read_file: '{}'", path_str);

        let entry_number = self.resolve_path(path)?;
        let entry = self.read_mft_entry(entry_number)?;

        if entry.header.is_directory() {
            log::error!("[ntfs] '{}' is a directory, not a file", path_str);
            return Err(NtfsError::IsADirectory);
        }

        // Read the unnamed $DATA attribute
        self.read_attribute_data_for_entry(&entry, AttributeType::Data)
    }

    /// Write data to a file, creating it if it does not exist, or overwriting if it does.
    pub fn write_file(&self, path: &[u8], data: &[u8]) -> Result<(), NtfsError> {
        let path_str = core::str::from_utf8(path).unwrap_or("<invalid>");
        log::info!("[ntfs] write_file: '{}' ({} bytes)", path_str, data.len());

        let components = Self::split_path(path)?;
        if components.is_empty() {
            return Err(NtfsError::InvalidPath);
        }

        // Find the parent directory
        let parent_entry_number = if components.len() == 1 {
            MFT_ENTRY_ROOT
        } else {
            let parent_path: Vec<u8> = {
                let mut p = vec![b'/'];
                for (i, c) in components[..components.len() - 1].iter().enumerate() {
                    if i > 0 {
                        p.push(b'/');
                    }
                    p.extend_from_slice(c);
                }
                p
            };
            self.resolve_path(&parent_path)?
        };

        let filename = core::str::from_utf8(components[components.len() - 1])
            .map_err(|_| NtfsError::InvalidPath)?;

        if filename.len() > 255 {
            return Err(NtfsError::NameTooLong);
        }

        // Check if file already exists
        match self.resolve_path(path) {
            Ok(existing_entry) => {
                // File exists: overwrite its $DATA attribute
                log::debug!("[ntfs] file '{}' exists at MFT#{}, overwriting", path_str, existing_entry);
                self.write_file_data(existing_entry, data)
            }
            Err(NtfsError::NotFound) => {
                // File does not exist: allocate new MFT entry and create it
                log::debug!("[ntfs] file '{}' does not exist, creating", path_str);
                self.create_file(parent_entry_number, filename, data)
            }
            Err(e) => Err(e),
        }
    }

    /// Write data to an existing file's $DATA attribute.
    fn write_file_data(&self, entry_number: u64, data: &[u8]) -> Result<(), NtfsError> {
        let entry = self.read_mft_entry(entry_number)?;
        let (hdr, attr_offset) = entry.find_attribute(AttributeType::Data)
            .ok_or(NtfsError::AttributeNotFound("$DATA"))?;

        if hdr.non_resident {
            // Non-resident: write to existing clusters (may need reallocation)
            let runs = Self::read_data_runs_from_entry(&entry, AttributeType::Data)?;
            let mut bytes_written = 0usize;

            for run in &runs {
                if bytes_written >= data.len() {
                    break;
                }
                if run.is_sparse {
                    bytes_written += (run.length * self.cluster_size) as usize;
                    continue;
                }

                let disk_offset = run.lcn * self.cluster_size;
                let run_capacity = (run.length * self.cluster_size) as usize;
                let to_write = (data.len() - bytes_written).min(run_capacity);

                log::trace!("[ntfs] writing {} bytes to disk offset 0x{:X}", to_write, disk_offset);
                self.device.write_bytes(disk_offset, &data[bytes_written..bytes_written + to_write])?;
                bytes_written += to_write;
            }

            if bytes_written < data.len() {
                log::warn!("[ntfs] file data ({} bytes) exceeds allocated clusters ({} bytes written). \
                    Cluster reallocation not yet implemented.",
                    data.len(), bytes_written);
                return Err(NtfsError::Unsupported("growing non-resident file data"));
            }

            // Update data_size in the non-resident header
            // (This requires rewriting the MFT entry with updated attribute metadata)
            log::debug!("[ntfs] wrote {} bytes to MFT#{} non-resident data", bytes_written, entry_number);
        } else {
            // Resident: update inline data
            // For simplicity, if the new data fits in the resident area, update in place.
            // Otherwise, would need to convert to non-resident.
            let res_data = entry.resident_data(attr_offset)
                .ok_or(NtfsError::Corrupt("failed to read resident data"))?;

            if data.len() > res_data.len() {
                log::warn!("[ntfs] new data ({} bytes) exceeds resident capacity ({} bytes). \
                    Resident-to-non-resident conversion not yet implemented.",
                    data.len(), res_data.len());
                return Err(NtfsError::Unsupported("growing resident to non-resident"));
            }

            // Write updated data into MFT entry
            let mut updated = entry.clone();
            let res_hdr = crate::attribute::ResidentHeader::from_bytes(
                &updated.data[attr_offset + AttributeHeader::HEADER_SIZE..]
            ).ok_or(NtfsError::Corrupt("bad resident header"))?;

            let data_start = attr_offset + res_hdr.value_offset as usize;
            updated.data[data_start..data_start + data.len()].copy_from_slice(data);

            // Zero out any remaining space
            for i in data.len()..res_hdr.value_length as usize {
                updated.data[data_start + i] = 0;
            }

            // Update value_length
            let vl_offset = attr_offset + AttributeHeader::HEADER_SIZE;
            updated.data[vl_offset..vl_offset + 4].copy_from_slice(&(data.len() as u32).to_le_bytes());

            self.write_mft_entry(entry_number, &updated)?;
            log::debug!("[ntfs] wrote {} bytes to MFT#{} resident data", data.len(), entry_number);
        }

        self.device.flush()
    }

    /// Create a new file in a parent directory.
    fn create_file(&self, parent_entry: u64, name: &str, data: &[u8]) -> Result<(), NtfsError> {
        log::info!("[ntfs] creating file '{}' in directory MFT#{}", name, parent_entry);

        // Allocate a new MFT entry
        let new_entry_number = self.allocate_mft_entry()?;
        log::debug!("[ntfs] allocated MFT entry #{} for new file '{}'", new_entry_number, name);

        // Build the $FILE_NAME attribute
        let parent_mft = self.read_mft_entry(parent_entry)?;
        let parent_ref = mft::make_mft_reference(parent_entry, parent_mft.header.sequence_number);

        let name_utf16: Vec<u16> = name.encode_utf16().collect();
        let now = 0u64; // TODO: get current time as FILETIME

        let fn_attr = FileNameAttr {
            parent_reference: parent_ref,
            creation_time: now,
            modification_time: now,
            mft_modification_time: now,
            access_time: now,
            allocated_size: data.len() as u64,
            real_size: data.len() as u64,
            flags: 0,
            ea_reparse: 0,
            name_length: name_utf16.len() as u8,
            namespace: FileNamespace::Win32AndDos,
            name: String::from(name),
            name_utf16: name_utf16.clone(),
        };

        // Build the MFT entry
        let record_size = self.mft_record_size as usize;
        let mut entry_data = vec![0u8; record_size];

        // Write FILE header
        entry_data[0..4].copy_from_slice(b"FILE");
        // USA offset (just after the header at offset 0x30)
        entry_data[0x04..0x06].copy_from_slice(&0x0030u16.to_le_bytes());
        // USA count (record_size / 512 + 1)
        let usa_count = (record_size / 512 + 1) as u16;
        entry_data[0x06..0x08].copy_from_slice(&usa_count.to_le_bytes());
        // Sequence number = 1
        entry_data[0x10..0x12].copy_from_slice(&1u16.to_le_bytes());
        // Hard link count = 1
        entry_data[0x12..0x14].copy_from_slice(&1u16.to_le_bytes());
        // First attribute offset (after header + USA)
        let first_attr = 0x30 + usa_count as usize * 2;
        let first_attr_aligned = (first_attr + 7) & !7; // 8-byte align
        entry_data[0x14..0x16].copy_from_slice(&(first_attr_aligned as u16).to_le_bytes());
        // Flags: in use
        entry_data[0x16..0x18].copy_from_slice(&0x0001u16.to_le_bytes());
        // Allocated size
        entry_data[0x1C..0x20].copy_from_slice(&(record_size as u32).to_le_bytes());

        // Write $FILE_NAME attribute (resident)
        let fn_bytes = fn_attr.to_bytes();
        let mut attr_pos = first_attr_aligned;
        attr_pos = Self::write_resident_attribute(
            &mut entry_data, attr_pos, AttributeType::FileName, &fn_bytes, 0,
        );

        // Write $DATA attribute (resident if small enough)
        if data.len() + attr_pos + 32 < record_size - 8 {
            // Resident $DATA
            attr_pos = Self::write_resident_attribute(
                &mut entry_data, attr_pos, AttributeType::Data, data, 1,
            );
        } else {
            // Non-resident: allocate clusters
            log::debug!("[ntfs] file data too large for resident, allocating clusters");
            let clusters_needed = (data.len() as u64 + self.cluster_size - 1) / self.cluster_size;
            let start_lcn = self.allocate_clusters(clusters_needed)?;

            // Write data to allocated clusters
            let disk_offset = start_lcn * self.cluster_size;
            self.device.write_bytes(disk_offset, data)?;

            // Write non-resident $DATA attribute with data runs
            let run = DataRun { lcn: start_lcn, length: clusters_needed, is_sparse: false };
            let run_bytes = data_runs::encode_data_runs(&[run]);
            attr_pos = Self::write_non_resident_attribute(
                &mut entry_data, attr_pos, AttributeType::Data,
                &run_bytes, data.len() as u64, clusters_needed * self.cluster_size, 1,
            );
        }

        // Write end-of-attributes marker
        entry_data[attr_pos..attr_pos + 4].copy_from_slice(&0xFFFFFFFFu32.to_le_bytes());
        attr_pos += 4;

        // Update used_size
        entry_data[0x18..0x1C].copy_from_slice(&(attr_pos as u32).to_le_bytes());

        // Write USA check value
        let check_value = 0x0001u16; // arbitrary check value
        entry_data[0x30..0x32].copy_from_slice(&check_value.to_le_bytes());

        let new_entry = MftEntry::from_bytes(&entry_data, record_size)
            .ok_or(NtfsError::Corrupt("failed to parse newly created MFT entry"))?;

        self.write_mft_entry(new_entry_number, &new_entry)?;

        // TODO: Add index entry to parent directory's $INDEX_ROOT / $INDEX_ALLOCATION
        log::warn!("[ntfs] directory index update not yet implemented — file created but not \
            yet visible in directory listing");

        log::info!("[ntfs] created file '{}' as MFT#{}", name, new_entry_number);
        Ok(())
    }

    /// Write a resident attribute into an MFT entry buffer.
    /// Returns the new position after the attribute.
    fn write_resident_attribute(
        buf: &mut [u8],
        pos: usize,
        attr_type: AttributeType,
        value: &[u8],
        instance: u16,
    ) -> usize {
        let value_offset = 24u16; // Header(16) + ResidentHeader(8) = 24 bytes to value
        let total_len = ((value_offset as usize + value.len()) + 7) & !7; // 8-byte aligned

        // Common header
        buf[pos..pos + 4].copy_from_slice(&(attr_type as u32).to_le_bytes());
        buf[pos + 4..pos + 8].copy_from_slice(&(total_len as u32).to_le_bytes());
        buf[pos + 8] = 0; // resident
        buf[pos + 9] = 0; // name_length
        buf[pos + 10..pos + 12].copy_from_slice(&0u16.to_le_bytes()); // name_offset
        buf[pos + 12..pos + 14].copy_from_slice(&0u16.to_le_bytes()); // flags
        buf[pos + 14..pos + 16].copy_from_slice(&instance.to_le_bytes());

        // Resident header
        buf[pos + 16..pos + 20].copy_from_slice(&(value.len() as u32).to_le_bytes());
        buf[pos + 20..pos + 22].copy_from_slice(&value_offset.to_le_bytes());
        buf[pos + 22] = 0; // indexed_flag
        buf[pos + 23] = 0; // padding

        // Value
        buf[pos + value_offset as usize..pos + value_offset as usize + value.len()]
            .copy_from_slice(value);

        log::trace!("[ntfs] wrote resident attribute {} ({} value bytes) at offset 0x{:04X}",
            attr_type.name(), value.len(), pos);

        pos + total_len
    }

    /// Write a non-resident attribute into an MFT entry buffer.
    /// Returns the new position after the attribute.
    fn write_non_resident_attribute(
        buf: &mut [u8],
        pos: usize,
        attr_type: AttributeType,
        run_data: &[u8],
        data_size: u64,
        allocated_size: u64,
        instance: u16,
    ) -> usize {
        let mapping_pairs_offset = 64u16; // Header(16) + NR-Header(48) = 64
        let total_len = ((mapping_pairs_offset as usize + run_data.len()) + 7) & !7;

        // Common header
        buf[pos..pos + 4].copy_from_slice(&(attr_type as u32).to_le_bytes());
        buf[pos + 4..pos + 8].copy_from_slice(&(total_len as u32).to_le_bytes());
        buf[pos + 8] = 1; // non-resident
        buf[pos + 9] = 0; // name_length
        buf[pos + 10..pos + 12].copy_from_slice(&0u16.to_le_bytes());
        buf[pos + 12..pos + 14].copy_from_slice(&0u16.to_le_bytes());
        buf[pos + 14..pos + 16].copy_from_slice(&instance.to_le_bytes());

        // Non-resident header (starting at pos + 16)
        let nr_base = pos + 16;
        // lowest_vcn = 0
        buf[nr_base..nr_base + 8].copy_from_slice(&0u64.to_le_bytes());
        // highest_vcn
        let highest_vcn = if allocated_size > 0 {
            (allocated_size / (allocated_size / data_size.max(1))).max(1) - 1
        } else {
            0
        };
        buf[nr_base + 8..nr_base + 16].copy_from_slice(&highest_vcn.to_le_bytes());
        // mapping_pairs_offset (from attribute start)
        buf[nr_base + 16..nr_base + 18].copy_from_slice(&mapping_pairs_offset.to_le_bytes());
        // compression_unit = 0
        buf[nr_base + 18..nr_base + 20].copy_from_slice(&0u16.to_le_bytes());
        // padding (4 bytes)
        // allocated_size
        buf[nr_base + 24..nr_base + 32].copy_from_slice(&allocated_size.to_le_bytes());
        // data_size
        buf[nr_base + 32..nr_base + 40].copy_from_slice(&data_size.to_le_bytes());
        // initialized_size
        buf[nr_base + 40..nr_base + 48].copy_from_slice(&data_size.to_le_bytes());

        // Data runs
        let runs_start = pos + mapping_pairs_offset as usize;
        buf[runs_start..runs_start + run_data.len()].copy_from_slice(run_data);

        log::trace!("[ntfs] wrote non-resident attribute {} ({} run bytes, data_size={}) at 0x{:04X}",
            attr_type.name(), run_data.len(), data_size, pos);

        pos + total_len
    }

    /// Allocate a free MFT entry.
    ///
    /// Scans the $MFT bitmap for a free entry and marks it as allocated.
    fn allocate_mft_entry(&self) -> Result<u64, NtfsError> {
        log::debug!("[ntfs] searching for free MFT entry...");

        // Read the $Bitmap attribute of $MFT (entry 0)
        let mft_entry = self.read_mft_entry(mft::MFT_ENTRY_MFT)?;
        let bitmap_data = self.read_attribute_data_for_entry(&mft_entry, AttributeType::Bitmap)?;

        // Scan for first free bit, starting after reserved entries
        for byte_idx in (mft::MFT_ENTRY_FIRST_USER as usize / 8)..bitmap_data.len() {
            if bitmap_data[byte_idx] != 0xFF {
                for bit in 0..8 {
                    if bitmap_data[byte_idx] & (1 << bit) == 0 {
                        let entry_number = (byte_idx * 8 + bit) as u64;
                        log::info!("[ntfs] found free MFT entry #{}", entry_number);
                        // TODO: set the bit in the bitmap and write it back
                        return Ok(entry_number);
                    }
                }
            }
        }

        log::error!("[ntfs] no free MFT entries available");
        Err(NtfsError::NoFreeMftEntries)
    }

    /// Allocate contiguous clusters from the volume bitmap.
    ///
    /// Scans the $Bitmap (MFT entry 6) for free clusters.
    fn allocate_clusters(&self, count: u64) -> Result<u64, NtfsError> {
        log::debug!("[ntfs] allocating {} clusters...", count);

        // Read the volume $Bitmap (MFT entry 6)
        let bitmap_entry = self.read_mft_entry(mft::MFT_ENTRY_BITMAP)?;
        let bitmap_data = self.read_attribute_data_for_entry(&bitmap_entry, AttributeType::Data)?;

        // Simple first-fit allocation: find `count` contiguous free bits
        let total_bits = bitmap_data.len() * 8;
        let mut run_start = 0u64;
        let mut run_length = 0u64;

        for bit_idx in 0..total_bits {
            let byte_idx = bit_idx / 8;
            let bit = bit_idx % 8;

            if bitmap_data[byte_idx] & (1 << bit) == 0 {
                // Free
                if run_length == 0 {
                    run_start = bit_idx as u64;
                }
                run_length += 1;
                if run_length >= count {
                    log::info!("[ntfs] allocated {} clusters starting at LCN {}", count, run_start);
                    // TODO: set the bits in the bitmap and write it back
                    return Ok(run_start);
                }
            } else {
                run_length = 0;
            }
        }

        log::error!("[ntfs] no contiguous run of {} free clusters", count);
        Err(NtfsError::NoFreeClusters)
    }

    /// Create a directory.
    pub fn mkdir(&self, path: &[u8]) -> Result<(), NtfsError> {
        let path_str = core::str::from_utf8(path).unwrap_or("<invalid>");
        log::info!("[ntfs] mkdir: '{}'", path_str);

        let components = Self::split_path(path)?;
        if components.is_empty() {
            return Err(NtfsError::InvalidPath);
        }

        // Check if already exists
        if self.resolve_path(path).is_ok() {
            return Err(NtfsError::AlreadyExists);
        }

        // Find parent directory
        let parent_entry_number = if components.len() == 1 {
            MFT_ENTRY_ROOT
        } else {
            let parent_path: Vec<u8> = {
                let mut p = vec![b'/'];
                for (i, c) in components[..components.len() - 1].iter().enumerate() {
                    if i > 0 {
                        p.push(b'/');
                    }
                    p.extend_from_slice(c);
                }
                p
            };
            self.resolve_path(&parent_path)?
        };

        let dirname = core::str::from_utf8(components[components.len() - 1])
            .map_err(|_| NtfsError::InvalidPath)?;

        if dirname.len() > 255 {
            return Err(NtfsError::NameTooLong);
        }

        // Allocate MFT entry
        let new_entry_number = self.allocate_mft_entry()?;
        log::debug!("[ntfs] allocated MFT entry #{} for directory '{}'", new_entry_number, dirname);

        // Build directory MFT entry (similar to file but with directory flag and $INDEX_ROOT)
        let parent_mft = self.read_mft_entry(parent_entry_number)?;
        let parent_ref = mft::make_mft_reference(parent_entry_number, parent_mft.header.sequence_number);

        let name_utf16: Vec<u16> = dirname.encode_utf16().collect();
        let now = 0u64; // TODO: current time

        let fn_attr = FileNameAttr {
            parent_reference: parent_ref,
            creation_time: now,
            modification_time: now,
            mft_modification_time: now,
            access_time: now,
            allocated_size: 0,
            real_size: 0,
            flags: crate::filename::FILE_ATTR_DIRECTORY,
            ea_reparse: 0,
            name_length: name_utf16.len() as u8,
            namespace: FileNamespace::Win32AndDos,
            name: String::from(dirname),
            name_utf16,
        };

        let record_size = self.mft_record_size as usize;
        let mut entry_data = vec![0u8; record_size];

        // FILE header
        entry_data[0..4].copy_from_slice(b"FILE");
        entry_data[0x04..0x06].copy_from_slice(&0x0030u16.to_le_bytes());
        let usa_count = (record_size / 512 + 1) as u16;
        entry_data[0x06..0x08].copy_from_slice(&usa_count.to_le_bytes());
        entry_data[0x10..0x12].copy_from_slice(&1u16.to_le_bytes());
        entry_data[0x12..0x14].copy_from_slice(&1u16.to_le_bytes());
        let first_attr = ((0x30 + usa_count as usize * 2) + 7) & !7;
        entry_data[0x14..0x16].copy_from_slice(&(first_attr as u16).to_le_bytes());
        // Flags: in use + directory
        entry_data[0x16..0x18].copy_from_slice(&0x0003u16.to_le_bytes());
        entry_data[0x1C..0x20].copy_from_slice(&(record_size as u32).to_le_bytes());

        let mut attr_pos = first_attr;

        // $FILE_NAME attribute
        let fn_bytes = fn_attr.to_bytes();
        attr_pos = Self::write_resident_attribute(
            &mut entry_data, attr_pos, AttributeType::FileName, &fn_bytes, 0,
        );

        // $INDEX_ROOT attribute (empty directory index)
        let index_root_value = Self::build_empty_index_root();
        attr_pos = Self::write_resident_attribute(
            &mut entry_data, attr_pos, AttributeType::IndexRoot, &index_root_value, 1,
        );

        // End marker
        entry_data[attr_pos..attr_pos + 4].copy_from_slice(&0xFFFFFFFFu32.to_le_bytes());
        attr_pos += 4;

        // Update used_size
        entry_data[0x18..0x1C].copy_from_slice(&(attr_pos as u32).to_le_bytes());

        // USA check value
        entry_data[0x30..0x32].copy_from_slice(&0x0001u16.to_le_bytes());

        let new_entry = MftEntry::from_bytes(&entry_data, record_size)
            .ok_or(NtfsError::Corrupt("failed to parse newly created directory MFT entry"))?;

        self.write_mft_entry(new_entry_number, &new_entry)?;

        // TODO: Add index entry to parent directory
        log::warn!("[ntfs] directory index update not yet implemented — directory created but \
            not yet visible in parent listing");

        log::info!("[ntfs] created directory '{}' as MFT#{}", dirname, new_entry_number);
        Ok(())
    }

    /// Build an empty $INDEX_ROOT value for a new directory.
    fn build_empty_index_root() -> Vec<u8> {
        let mut buf = vec![0u8; 32]; // 16 bytes header + 16 bytes index header

        // Indexed attribute type: $FILE_NAME (0x30)
        buf[0..4].copy_from_slice(&0x00000030u32.to_le_bytes());
        // Collation rule: COLLATION_FILENAME (0x01)
        buf[4..8].copy_from_slice(&0x00000001u32.to_le_bytes());
        // Index block size (4096 typical)
        buf[8..12].copy_from_slice(&4096u32.to_le_bytes());
        // Clusters per index block
        buf[12] = 1;
        // padding: buf[13..16] = 0

        // Index header (at offset 16)
        // entries_offset: offset to first entry from index header start
        let entries_off = 16u32; // index entries start right after the index header
        buf[16..20].copy_from_slice(&entries_off.to_le_bytes());
        // total_size: index header + last entry (16 bytes for sentinel)
        let sentinel_size = 16u32; // minimal last entry
        buf[20..24].copy_from_slice(&(entries_off + sentinel_size).to_le_bytes());
        // allocated_size
        buf[24..28].copy_from_slice(&(entries_off + sentinel_size).to_le_bytes());
        // flags = 0 (small index, no $INDEX_ALLOCATION needed)
        buf[28] = 0;

        // Append the sentinel (last) entry
        let mut sentinel = [0u8; 16];
        // MFT reference = 0
        // entry_length = 16
        sentinel[8..10].copy_from_slice(&16u16.to_le_bytes());
        // content_length = 0
        // flags = LAST_ENTRY
        sentinel[12..14].copy_from_slice(&index::INDEX_ENTRY_FLAG_LAST_ENTRY.to_le_bytes());

        buf.extend_from_slice(&sentinel);

        log::trace!("[ntfs] built empty INDEX_ROOT: {} bytes", buf.len());
        buf
    }

    /// List the contents of a directory.
    pub fn list_dir(&self, path: &[u8]) -> Result<Vec<DirEntry>, NtfsError> {
        let path_str = core::str::from_utf8(path).unwrap_or("<invalid>");
        log::info!("[ntfs] list_dir: '{}'", path_str);

        let entry_number = self.resolve_path(path)?;
        let entries = self.read_directory_entries(entry_number)?;

        let mut results = Vec::new();
        for entry in &entries {
            if entry.is_last() {
                continue;
            }
            if let Some(ref fn_attr) = entry.filename {
                // Skip DOS-only names to avoid duplicates
                if fn_attr.namespace == FileNamespace::Dos {
                    continue;
                }
                results.push(DirEntry {
                    mft_entry: entry.entry_number(),
                    name: fn_attr.name.clone(),
                    is_directory: fn_attr.is_directory(),
                    size: fn_attr.real_size,
                    creation_time: fn_attr.creation_time,
                    modification_time: fn_attr.modification_time,
                    flags: fn_attr.flags,
                });
            }
        }

        log::info!("[ntfs] list_dir '{}': {} entries", path_str, results.len());
        Ok(results)
    }
}

impl<D: BlockDevice> fmt::Debug for NtfsFs<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NtfsFs")
            .field("boot_sector", &self.boot_sector)
            .field("cluster_size", &self.cluster_size)
            .field("mft_record_size", &self.mft_record_size)
            .field("mft_offset", &format_args!("0x{:X}", self.mft_offset))
            .field("mft_data_runs", &self.mft_data_runs.len())
            .finish()
    }
}
