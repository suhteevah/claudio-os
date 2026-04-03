//! Storage-driver and filesystem adapter layers.
//!
//! This module bridges the different `BlockDevice` and filesystem traits used by
//! the various ClaudioOS crates into the unified VFS interfaces.
//!
//! ## Storage driver adapters
//!
//! - [`AhciBlockDeviceAdapter`] — wraps `AhciDisk` + `HbaRegs` for the VFS `BlockDevice` trait.
//! - [`NvmeBlockDeviceAdapter`] — wraps `NvmeBlockDevice` for the VFS `BlockDevice` trait.
//!
//! ## Filesystem adapters
//!
//! - [`Ext4FilesystemAdapter`] — wraps `Ext4Fs<D>` and implements VFS `Filesystem`.
//! - [`BtrfsFilesystemAdapter`] — wraps `BtrFs<D>` and implements VFS `Filesystem`.
//! - [`NtfsFilesystemAdapter`] — wraps `NtfsFs<D>` and implements VFS `Filesystem`.
//!
//! ## Auto-detection
//!
//! - [`FsAutoDetect`] / [`detect_filesystem`] — reads the first few sectors of a partition
//!   and identifies the filesystem type from magic numbers.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use spin::Mutex;

use crate::device::{BlockDevice, DeviceError};
use crate::dir::DirEntry as VfsDirEntry;
use crate::file::{FileInfo, FileType};
use crate::fs_trait::{Filesystem, FsError, FsType};

// ============================================================================
// Storage driver adapters — bridge AHCI/NVMe to VFS BlockDevice
// ============================================================================

/// Adapter that wraps an `AhciDisk` + `HbaRegs` reference behind the VFS
/// `BlockDevice` trait.
///
/// AHCI requires the HBA register handle for every I/O operation, so we store
/// both in a `Mutex` to satisfy `Send + Sync`.
pub struct AhciBlockDeviceAdapter {
    /// Interior-mutable handle: (AhciDisk, HbaRegs pointer).
    inner: Mutex<AhciBlockDeviceInner>,
    sector_size: u32,
    total_bytes: u64,
}

struct AhciBlockDeviceInner {
    /// Pointer to the AhciDisk. Held as a raw pointer because `AhciDisk` is
    /// typically owned by the `AhciController` and we need a long-lived ref.
    disk: *mut claudio_ahci::AhciDisk,
    hba: *const claudio_ahci::hba::HbaRegs,
}

// SAFETY: AHCI MMIO registers are accessed through volatile reads/writes and
// the Mutex serializes access.
unsafe impl Send for AhciBlockDeviceInner {}
unsafe impl Sync for AhciBlockDeviceInner {}

impl AhciBlockDeviceAdapter {
    /// Create a new adapter.
    ///
    /// # Safety
    ///
    /// The `disk` and `hba` pointers must remain valid for the lifetime of
    /// this adapter. The caller must ensure no other code mutates the disk
    /// concurrently (the internal Mutex handles our own serialization).
    pub unsafe fn new(
        disk: *mut claudio_ahci::AhciDisk,
        hba: *const claudio_ahci::hba::HbaRegs,
    ) -> Self {
        let disk_ref = unsafe { &*disk };
        Self {
            sector_size: disk_ref.sector_size,
            total_bytes: disk_ref.sector_count * disk_ref.sector_size as u64,
            inner: Mutex::new(AhciBlockDeviceInner { disk, hba }),
        }
    }
}

impl BlockDevice for AhciBlockDeviceAdapter {
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        let inner = self.inner.lock();
        let disk = unsafe { &*inner.disk };
        let hba = unsafe { &*inner.hba };
        disk.read_bytes(hba, offset, buf)
            .map(|()| buf.len())
            .map_err(|_| DeviceError::IoError)
    }

    fn write_bytes(&self, offset: u64, data: &[u8]) -> Result<usize, DeviceError> {
        let inner = self.inner.lock();
        let disk = unsafe { &*inner.disk };
        let hba = unsafe { &*inner.hba };
        disk.write_bytes(hba, offset, data)
            .map(|()| data.len())
            .map_err(|_| DeviceError::IoError)
    }

    fn flush(&self) -> Result<(), DeviceError> {
        let inner = self.inner.lock();
        let disk = unsafe { &*inner.disk };
        let hba = unsafe { &*inner.hba };
        disk.flush_bytes(hba)
            .map_err(|_| DeviceError::IoError)
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_size(&self) -> u64 {
        self.total_bytes
    }
}

/// Adapter that wraps an `NvmeBlockDevice` (which needs `&mut self`) behind
/// the VFS `BlockDevice` trait using a `Mutex` for interior mutability.
pub struct NvmeBlockDeviceAdapter {
    /// Interior-mutable handle to the NVMe controller + namespace info.
    inner: Mutex<NvmeBlockDeviceInner>,
    sector_size: u32,
    total_bytes: u64,
}

struct NvmeBlockDeviceInner {
    ctrl: *mut claudio_nvme::NvmeController,
    nsid: u32,
    sector_size: u32,
}

// SAFETY: NVMe MMIO registers use volatile access and the Mutex serializes.
unsafe impl Send for NvmeBlockDeviceInner {}
unsafe impl Sync for NvmeBlockDeviceInner {}

impl NvmeBlockDeviceAdapter {
    /// Create a new adapter.
    ///
    /// # Safety
    ///
    /// The `ctrl` pointer must remain valid for the lifetime of this adapter.
    /// `sector_count` and `sector_size` describe the namespace geometry.
    pub unsafe fn new(
        ctrl: *mut claudio_nvme::NvmeController,
        nsid: u32,
        sector_count: u64,
        sector_size: u32,
    ) -> Self {
        Self {
            sector_size,
            total_bytes: sector_count * sector_size as u64,
            inner: Mutex::new(NvmeBlockDeviceInner {
                ctrl,
                nsid,
                sector_size,
            }),
        }
    }
}

impl BlockDevice for NvmeBlockDeviceAdapter {
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        let mut inner = self.inner.lock();
        let ctrl = unsafe { &mut *inner.ctrl };
        // Create a temporary NvmeBlockDevice for the call.
        let mut blk = claudio_nvme::driver::NvmeBlockDevice::new(
            ctrl,
            inner.nsid,
            self.total_bytes / inner.sector_size as u64,
            inner.sector_size,
        );
        blk.read_bytes(offset, buf)
            .map(|()| buf.len())
            .map_err(|_| DeviceError::IoError)
    }

    fn write_bytes(&self, offset: u64, data: &[u8]) -> Result<usize, DeviceError> {
        let mut inner = self.inner.lock();
        let ctrl = unsafe { &mut *inner.ctrl };
        let mut blk = claudio_nvme::driver::NvmeBlockDevice::new(
            ctrl,
            inner.nsid,
            self.total_bytes / inner.sector_size as u64,
            inner.sector_size,
        );
        blk.write_bytes(offset, data)
            .map(|()| data.len())
            .map_err(|_| DeviceError::IoError)
    }

    fn flush(&self) -> Result<(), DeviceError> {
        let mut inner = self.inner.lock();
        let ctrl = unsafe { &mut *inner.ctrl };
        let mut blk = claudio_nvme::driver::NvmeBlockDevice::new(
            ctrl,
            inner.nsid,
            self.total_bytes / inner.sector_size as u64,
            inner.sector_size,
        );
        blk.flush()
            .map_err(|_| DeviceError::IoError)
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_size(&self) -> u64 {
        self.total_bytes
    }
}

// ============================================================================
// Partition-scoped block device adapters for filesystem crates
//
// Each filesystem crate defines its own `BlockDevice` trait with its own error
// type. These adapters wrap the VFS `BlockDevice` trait behind the FS-specific
// `BlockDevice` traits.
// ============================================================================

/// Wraps a VFS `&dyn BlockDevice` (or a `Partition` view) to satisfy the
/// ext4 crate's `BlockDevice` trait.
pub struct VfsToExt4BlockDevice<'a> {
    /// The underlying VFS block device.
    pub device: &'a dyn BlockDevice,
    /// Byte offset of the partition start (0 for whole-device).
    pub partition_offset: u64,
    /// Size of the partition in bytes (used for bounds checking).
    pub partition_size: u64,
}

impl<'a> claudio_ext4::BlockDevice for VfsToExt4BlockDevice<'a> {
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<(), claudio_ext4::Ext4Error> {
        self.device
            .read_bytes(self.partition_offset + offset, buf)
            .map(|_| ())
            .map_err(|_| claudio_ext4::Ext4Error::IoError)
    }

    fn write_bytes(&self, offset: u64, buf: &[u8]) -> Result<(), claudio_ext4::Ext4Error> {
        self.device
            .write_bytes(self.partition_offset + offset, buf)
            .map(|_| ())
            .map_err(|_| claudio_ext4::Ext4Error::IoError)
    }
}

/// Wraps a VFS `&dyn BlockDevice` to satisfy the btrfs crate's `BlockDevice` trait.
pub struct VfsToBtrfsBlockDevice<'a> {
    pub device: &'a dyn BlockDevice,
    pub partition_offset: u64,
    pub partition_size: u64,
}

impl<'a> claudio_btrfs::BlockDevice for VfsToBtrfsBlockDevice<'a> {
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<(), claudio_btrfs::BtrfsError> {
        self.device
            .read_bytes(self.partition_offset + offset, buf)
            .map(|_| ())
            .map_err(|_| claudio_btrfs::BtrfsError::IoError)
    }

    fn write_bytes(&self, offset: u64, buf: &[u8]) -> Result<(), claudio_btrfs::BtrfsError> {
        self.device
            .write_bytes(self.partition_offset + offset, buf)
            .map(|_| ())
            .map_err(|_| claudio_btrfs::BtrfsError::IoError)
    }
}

/// Wraps a VFS `&dyn BlockDevice` to satisfy the NTFS crate's `BlockDevice` trait.
pub struct VfsToNtfsBlockDevice<'a> {
    pub device: &'a dyn BlockDevice,
    pub partition_offset: u64,
    pub partition_size: u64,
}

impl<'a> claudio_ntfs::BlockDevice for VfsToNtfsBlockDevice<'a> {
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<(), claudio_ntfs::NtfsError> {
        self.device
            .read_bytes(self.partition_offset + offset, buf)
            .map(|_| ())
            .map_err(|_| claudio_ntfs::NtfsError::IoError)
    }

    fn write_bytes(&self, offset: u64, buf: &[u8]) -> Result<(), claudio_ntfs::NtfsError> {
        self.device
            .write_bytes(self.partition_offset + offset, buf)
            .map(|_| ())
            .map_err(|_| claudio_ntfs::NtfsError::IoError)
    }
}

// ============================================================================
// Filesystem adapters — bridge ext4/btrfs/NTFS FS types to VFS Filesystem trait
// ============================================================================

/// Adapts `Ext4Fs<D>` to the VFS `Filesystem` trait.
///
/// Uses a `Mutex` because ext4 write operations need `&mut self`.
pub struct Ext4FilesystemAdapter<D: claudio_ext4::BlockDevice + Send + Sync> {
    inner: Mutex<claudio_ext4::Ext4Fs<D>>,
    label: Option<String>,
}

impl<D: claudio_ext4::BlockDevice + Send + Sync> Ext4FilesystemAdapter<D> {
    /// Create a new adapter from a mounted ext4 filesystem.
    pub fn new(fs: claudio_ext4::Ext4Fs<D>) -> Self {
        let vol = fs.sb.volume_name_str();
        let label = if vol.is_empty() { None } else { Some(String::from(vol)) };
        Self {
            inner: Mutex::new(fs),
            label,
        }
    }
}

impl<D: claudio_ext4::BlockDevice + Send + Sync + 'static> Filesystem for Ext4FilesystemAdapter<D> {
    fn fs_type(&self) -> FsType {
        FsType::Ext4
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn read_file(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let fs = self.inner.lock();
        let data = fs.read_file(path.as_bytes()).map_err(ext4_err_to_vfs)?;
        let start = offset as usize;
        if start >= data.len() {
            return Ok(0);
        }
        let end = core::cmp::min(start + buf.len(), data.len());
        let n = end - start;
        buf[..n].copy_from_slice(&data[start..end]);
        Ok(n)
    }

    fn write_file(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let mut fs = self.inner.lock();
        // ext4 write_file replaces the entire file. For offset-based writes, we need
        // to read-modify-write.
        if offset == 0 {
            fs.write_file(path.as_bytes(), data).map_err(ext4_err_to_vfs)?;
            Ok(data.len())
        } else {
            // Read existing content, splice in new data, write back.
            let existing = fs.read_file(path.as_bytes()).unwrap_or_default();
            let off = offset as usize;
            let new_len = core::cmp::max(existing.len(), off + data.len());
            let mut combined = vec![0u8; new_len];
            let copy_len = core::cmp::min(existing.len(), new_len);
            combined[..copy_len].copy_from_slice(&existing[..copy_len]);
            combined[off..off + data.len()].copy_from_slice(data);
            fs.write_file(path.as_bytes(), &combined).map_err(ext4_err_to_vfs)?;
            Ok(data.len())
        }
    }

    fn create_file(&self, path: &str) -> Result<(), FsError> {
        let mut fs = self.inner.lock();
        // Create an empty file by writing zero bytes.
        fs.write_file(path.as_bytes(), &[]).map_err(ext4_err_to_vfs)
    }

    fn delete_file(&self, _path: &str) -> Result<(), FsError> {
        // ext4 crate does not expose delete yet.
        Err(FsError::Unsupported)
    }

    fn mkdir(&self, path: &str) -> Result<(), FsError> {
        let mut fs = self.inner.lock();
        fs.mkdir(path.as_bytes()).map(|_| ()).map_err(ext4_err_to_vfs)
    }

    fn rmdir(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }

    fn stat(&self, path: &str) -> Result<FileInfo, FsError> {
        let fs = self.inner.lock();
        let (ino, inode) = fs.lookup_path(path.as_bytes()).map_err(ext4_err_to_vfs)?;
        let file_type = if inode.is_dir() {
            FileType::Directory
        } else {
            FileType::File
        };
        Ok(FileInfo {
            size: inode.size(),
            file_type,
            permissions: (inode.mode & 0o7777) as u32,
            created: inode.ctime as u64,
            modified: inode.mtime as u64,
            accessed: inode.atime as u64,
            inode: ino as u64,
            nlinks: inode.links_count as u32,
            uid: inode.uid as u32,
            gid: inode.gid as u32,
        })
    }

    fn readdir(&self, path: &str) -> Result<Vec<VfsDirEntry>, FsError> {
        let fs = self.inner.lock();
        let entries = fs.list_dir(path.as_bytes()).map_err(ext4_err_to_vfs)?;
        Ok(entries
            .into_iter()
            .filter(|e| {
                let name = e.name_str();
                name != "." && name != ".."
            })
            .map(|e| {
                let ft = match e.file_type {
                    claudio_ext4::dir::FT_DIR => FileType::Directory,
                    claudio_ext4::dir::FT_SYMLINK => FileType::Symlink,
                    _ => FileType::File,
                };
                VfsDirEntry {
                    name: String::from(e.name_str()),
                    file_type: ft,
                    size: 0, // dir entries don't carry size; use stat for that
                    inode: e.inode as u64,
                }
            })
            .collect())
    }

    fn rename(&self, _old: &str, _new: &str) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }

    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        if size == 0 {
            let mut fs = self.inner.lock();
            fs.write_file(path.as_bytes(), &[]).map_err(ext4_err_to_vfs)
        } else {
            // Partial truncate not supported by ext4 crate API.
            Err(FsError::Unsupported)
        }
    }

    fn sync(&self) -> Result<(), FsError> {
        // ext4 crate has no explicit sync; writes are immediate.
        Ok(())
    }
}

/// Adapts `BtrFs<D>` to the VFS `Filesystem` trait.
pub struct BtrfsFilesystemAdapter<D: claudio_btrfs::BlockDevice + Send + Sync> {
    inner: Mutex<claudio_btrfs::BtrFs<D>>,
    label: Option<String>,
}

impl<D: claudio_btrfs::BlockDevice + Send + Sync> BtrfsFilesystemAdapter<D> {
    /// Create a new adapter from a mounted btrfs filesystem.
    pub fn new(fs: claudio_btrfs::BtrFs<D>) -> Self {
        let lbl = fs.sb.label_str();
        let label = if lbl.is_empty() { None } else { Some(String::from(lbl)) };
        Self {
            inner: Mutex::new(fs),
            label,
        }
    }
}

impl<D: claudio_btrfs::BlockDevice + Send + Sync + 'static> Filesystem for BtrfsFilesystemAdapter<D> {
    fn fs_type(&self) -> FsType {
        FsType::Btrfs
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn read_file(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let fs = self.inner.lock();
        let data = fs.read_file(path.as_bytes()).map_err(btrfs_err_to_vfs)?;
        let start = offset as usize;
        if start >= data.len() {
            return Ok(0);
        }
        let end = core::cmp::min(start + buf.len(), data.len());
        let n = end - start;
        buf[..n].copy_from_slice(&data[start..end]);
        Ok(n)
    }

    fn write_file(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let mut fs = self.inner.lock();
        if offset == 0 {
            fs.write_file(path.as_bytes(), data).map_err(btrfs_err_to_vfs)?;
            Ok(data.len())
        } else {
            let existing = fs.read_file(path.as_bytes()).unwrap_or_default();
            let off = offset as usize;
            let new_len = core::cmp::max(existing.len(), off + data.len());
            let mut combined = vec![0u8; new_len];
            let copy_len = core::cmp::min(existing.len(), new_len);
            combined[..copy_len].copy_from_slice(&existing[..copy_len]);
            combined[off..off + data.len()].copy_from_slice(data);
            fs.write_file(path.as_bytes(), &combined).map_err(btrfs_err_to_vfs)?;
            Ok(data.len())
        }
    }

    fn create_file(&self, path: &str) -> Result<(), FsError> {
        let mut fs = self.inner.lock();
        fs.write_file(path.as_bytes(), &[]).map_err(btrfs_err_to_vfs)
    }

    fn delete_file(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }

    fn mkdir(&self, path: &str) -> Result<(), FsError> {
        let mut fs = self.inner.lock();
        fs.mkdir(path.as_bytes()).map_err(btrfs_err_to_vfs)
    }

    fn rmdir(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }

    fn stat(&self, path: &str) -> Result<FileInfo, FsError> {
        // btrfs crate doesn't expose a direct stat/lookup_path.
        // Try list_dir on the parent to find the entry, or try read_file for size.
        let fs = self.inner.lock();

        // Check if it's the root directory.
        if path == "/" {
            return Ok(FileInfo::simple_dir());
        }

        // Split path into parent and basename.
        let (parent, basename) = split_parent_basename(path);

        let entries = fs.list_dir(parent.as_bytes()).map_err(btrfs_err_to_vfs)?;
        for entry in &entries {
            if entry.name == basename {
                let file_type = match entry.file_type {
                    claudio_btrfs::dir::dir_type::DIR => FileType::Directory,
                    claudio_btrfs::dir::dir_type::SYMLINK => FileType::Symlink,
                    _ => FileType::File,
                };

                if file_type == FileType::Directory {
                    return Ok(FileInfo {
                        size: 0,
                        file_type,
                        permissions: 0o755,
                        created: 0,
                        modified: 0,
                        accessed: 0,
                        inode: entry.inode,
                        nlinks: 2,
                        uid: 0,
                        gid: 0,
                    });
                } else {
                    // Get the file size by reading it (expensive but correct).
                    let data = fs.read_file(path.as_bytes()).unwrap_or_default();
                    return Ok(FileInfo {
                        size: data.len() as u64,
                        file_type,
                        permissions: 0o644,
                        created: 0,
                        modified: 0,
                        accessed: 0,
                        inode: entry.inode,
                        nlinks: 1,
                        uid: 0,
                        gid: 0,
                    });
                }
            }
        }

        Err(FsError::NotFound)
    }

    fn readdir(&self, path: &str) -> Result<Vec<VfsDirEntry>, FsError> {
        let fs = self.inner.lock();
        let entries = fs.list_dir(path.as_bytes()).map_err(btrfs_err_to_vfs)?;
        Ok(entries
            .into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .map(|e| {
                let ft = match e.file_type {
                    claudio_btrfs::dir::dir_type::DIR => FileType::Directory,
                    claudio_btrfs::dir::dir_type::SYMLINK => FileType::Symlink,
                    _ => FileType::File,
                };
                VfsDirEntry {
                    name: e.name.clone(),
                    file_type: ft,
                    size: 0,
                    inode: e.inode,
                }
            })
            .collect())
    }

    fn rename(&self, _old: &str, _new: &str) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }

    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        if size == 0 {
            let mut fs = self.inner.lock();
            fs.write_file(path.as_bytes(), &[]).map_err(btrfs_err_to_vfs)
        } else {
            Err(FsError::Unsupported)
        }
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

/// Adapts `NtfsFs<D>` to the VFS `Filesystem` trait.
pub struct NtfsFilesystemAdapter<D: claudio_ntfs::BlockDevice + Send + Sync> {
    inner: Mutex<claudio_ntfs::NtfsFs<D>>,
}

impl<D: claudio_ntfs::BlockDevice + Send + Sync> NtfsFilesystemAdapter<D> {
    /// Create a new adapter from a mounted NTFS filesystem.
    pub fn new(fs: claudio_ntfs::NtfsFs<D>) -> Self {
        Self {
            inner: Mutex::new(fs),
        }
    }
}

impl<D: claudio_ntfs::BlockDevice + Send + Sync + 'static> Filesystem for NtfsFilesystemAdapter<D> {
    fn fs_type(&self) -> FsType {
        FsType::Ntfs
    }

    fn label(&self) -> Option<&str> {
        None // NTFS volume label requires reading $Volume MFT entry; defer.
    }

    fn read_file(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let fs = self.inner.lock();
        let data = fs.read_file(path.as_bytes()).map_err(ntfs_err_to_vfs)?;
        let start = offset as usize;
        if start >= data.len() {
            return Ok(0);
        }
        let end = core::cmp::min(start + buf.len(), data.len());
        let n = end - start;
        buf[..n].copy_from_slice(&data[start..end]);
        Ok(n)
    }

    fn write_file(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let fs = self.inner.lock();
        if offset == 0 {
            fs.write_file(path.as_bytes(), data).map_err(ntfs_err_to_vfs)?;
            Ok(data.len())
        } else {
            let existing = fs.read_file(path.as_bytes()).unwrap_or_default();
            let off = offset as usize;
            let new_len = core::cmp::max(existing.len(), off + data.len());
            let mut combined = vec![0u8; new_len];
            let copy_len = core::cmp::min(existing.len(), new_len);
            combined[..copy_len].copy_from_slice(&existing[..copy_len]);
            combined[off..off + data.len()].copy_from_slice(data);
            fs.write_file(path.as_bytes(), &combined).map_err(ntfs_err_to_vfs)?;
            Ok(data.len())
        }
    }

    fn create_file(&self, path: &str) -> Result<(), FsError> {
        let fs = self.inner.lock();
        fs.write_file(path.as_bytes(), &[]).map_err(ntfs_err_to_vfs)
    }

    fn delete_file(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }

    fn mkdir(&self, path: &str) -> Result<(), FsError> {
        let fs = self.inner.lock();
        fs.mkdir(path.as_bytes()).map_err(ntfs_err_to_vfs)
    }

    fn rmdir(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }

    fn stat(&self, path: &str) -> Result<FileInfo, FsError> {
        let fs = self.inner.lock();

        if path == "/" {
            return Ok(FileInfo::simple_dir());
        }

        // Try listing the parent directory to find the entry.
        let (parent, basename) = split_parent_basename(path);
        let entries = fs.list_dir(parent.as_bytes()).map_err(ntfs_err_to_vfs)?;

        for entry in &entries {
            if entry.name == basename {
                let file_type = if entry.is_directory {
                    FileType::Directory
                } else {
                    FileType::File
                };
                return Ok(FileInfo {
                    size: entry.size,
                    file_type,
                    permissions: if entry.is_directory { 0o755 } else { 0o644 },
                    created: entry.creation_time,
                    modified: entry.modification_time,
                    accessed: 0,
                    inode: entry.mft_entry,
                    nlinks: 1,
                    uid: 0,
                    gid: 0,
                });
            }
        }

        Err(FsError::NotFound)
    }

    fn readdir(&self, path: &str) -> Result<Vec<VfsDirEntry>, FsError> {
        let fs = self.inner.lock();
        let entries = fs.list_dir(path.as_bytes()).map_err(ntfs_err_to_vfs)?;
        Ok(entries
            .into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .map(|e| {
                let ft = if e.is_directory {
                    FileType::Directory
                } else {
                    FileType::File
                };
                VfsDirEntry {
                    name: e.name.clone(),
                    file_type: ft,
                    size: e.size,
                    inode: e.mft_entry,
                }
            })
            .collect())
    }

    fn rename(&self, _old: &str, _new: &str) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }

    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        if size == 0 {
            let fs = self.inner.lock();
            fs.write_file(path.as_bytes(), &[]).map_err(ntfs_err_to_vfs)
        } else {
            Err(FsError::Unsupported)
        }
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

// ============================================================================
// Filesystem auto-detection
// ============================================================================

/// Result of filesystem auto-detection on a partition or device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsAutoDetect {
    /// ext4 filesystem detected (magic 0xEF53 at superblock offset 0x438).
    Ext4,
    /// btrfs filesystem detected (magic "_BHRfS_M" at offset 0x10040).
    Btrfs,
    /// NTFS filesystem detected (magic "NTFS    " at offset 3 of boot sector).
    Ntfs,
    /// FAT32 filesystem detected (various FAT signatures in boot sector).
    Fat32,
    /// No recognized filesystem signature found.
    Unknown,
}

impl FsAutoDetect {
    /// Convert to the VFS `FsType` enum.
    pub fn to_fs_type(self) -> FsType {
        match self {
            FsAutoDetect::Ext4 => FsType::Ext4,
            FsAutoDetect::Btrfs => FsType::Btrfs,
            FsAutoDetect::Ntfs => FsType::Ntfs,
            FsAutoDetect::Fat32 => FsType::Fat32,
            FsAutoDetect::Unknown => FsType::Unknown,
        }
    }
}

/// Detect the filesystem type on a block device or partition.
///
/// Reads the first few sectors and checks for well-known magic numbers:
/// - **ext4**: magic `0xEF53` at byte offset 1080 (0x438) in the superblock.
/// - **btrfs**: magic `_BHRfS_M` at byte offset 0x10040.
/// - **NTFS**: magic `NTFS    ` (with trailing spaces) at byte offset 3.
/// - **FAT32**: OEM name check + FAT32-specific BPB fields.
pub fn detect_filesystem(device: &dyn BlockDevice) -> FsAutoDetect {
    log::info!("[vfs::adapters] auto-detecting filesystem type");

    // We need to read up to offset 0x10048 for btrfs detection.
    // Read the first 512 bytes for NTFS/FAT32.
    let mut sector0 = vec![0u8; 512];
    if device.read_bytes(0, &mut sector0).is_err() {
        log::warn!("[vfs::adapters] failed to read sector 0");
        return FsAutoDetect::Unknown;
    }

    // ---- Check NTFS: "NTFS    " at offset 3 ----
    if sector0.len() >= 11 && &sector0[3..11] == b"NTFS    " {
        log::info!("[vfs::adapters] detected NTFS (magic at offset 3)");
        return FsAutoDetect::Ntfs;
    }

    // ---- Check FAT32: various signatures ----
    // FAT32 has "FAT32   " at offset 82 in the boot sector (BS_FilSysType).
    if sector0.len() >= 90 && &sector0[82..90] == b"FAT32   " {
        log::info!("[vfs::adapters] detected FAT32 (BS_FilSysType at offset 82)");
        return FsAutoDetect::Fat32;
    }
    // Also check the older location at offset 54 for FAT12/16.
    // FAT32 BPB: bytes_per_sector (offset 11, u16), sectors_per_cluster (offset 13, u8),
    // BPB_FATSz16=0 at offset 22 means FAT32.
    if sector0.len() >= 62 {
        let bps = u16::from_le_bytes([sector0[11], sector0[12]]);
        let fat_sz16 = u16::from_le_bytes([sector0[22], sector0[23]]);
        let fat_sz32 = u32::from_le_bytes([sector0[36], sector0[37], sector0[38], sector0[39]]);
        if (bps == 512 || bps == 1024 || bps == 2048 || bps == 4096)
            && fat_sz16 == 0
            && fat_sz32 != 0
        {
            log::info!("[vfs::adapters] detected FAT32 (BPB heuristic: FATSz16=0, FATSz32={})", fat_sz32);
            return FsAutoDetect::Fat32;
        }
    }

    // ---- Check ext4: magic 0xEF53 at offset 1080 (0x438) ----
    // The ext4 superblock starts at byte 1024. Magic is at offset 0x38 within it.
    let mut sb_buf = vec![0u8; 2];
    if device.read_bytes(1080, &mut sb_buf).is_ok() {
        let magic = u16::from_le_bytes([sb_buf[0], sb_buf[1]]);
        if magic == 0xEF53 {
            log::info!("[vfs::adapters] detected ext4 (magic 0xEF53 at offset 1080)");
            return FsAutoDetect::Ext4;
        }
    }

    // ---- Check btrfs: "_BHRfS_M" at offset 0x10040 ----
    let mut btrfs_buf = vec![0u8; 8];
    if device.read_bytes(0x10040, &mut btrfs_buf).is_ok() {
        if &btrfs_buf == b"_BHRfS_M" {
            log::info!("[vfs::adapters] detected btrfs (magic at offset 0x10040)");
            return FsAutoDetect::Btrfs;
        }
    }

    log::info!("[vfs::adapters] no recognized filesystem signature found");
    FsAutoDetect::Unknown
}

// ============================================================================
// Error conversion helpers
// ============================================================================

fn ext4_err_to_vfs(e: claudio_ext4::Ext4Error) -> FsError {
    match e {
        claudio_ext4::Ext4Error::NotFound => FsError::NotFound,
        claudio_ext4::Ext4Error::AlreadyExists => FsError::AlreadyExists,
        claudio_ext4::Ext4Error::IoError => FsError::IoError,
        claudio_ext4::Ext4Error::NoFreeBlocks | claudio_ext4::Ext4Error::NoFreeInodes => FsError::NoSpace,
        claudio_ext4::Ext4Error::Corrupt(_) | claudio_ext4::Ext4Error::InvalidSuperblock => FsError::Corrupt,
        claudio_ext4::Ext4Error::IsADirectory => FsError::WrongType,
        claudio_ext4::Ext4Error::IsNotADirectory | claudio_ext4::Ext4Error::NotADirectory => FsError::WrongType,
        claudio_ext4::Ext4Error::DirectoryNotEmpty => FsError::NotEmpty,
        claudio_ext4::Ext4Error::InvalidPath | claudio_ext4::Ext4Error::NameTooLong => FsError::NotFound,
        claudio_ext4::Ext4Error::UnsupportedFeature(_) => FsError::Unsupported,
    }
}

fn btrfs_err_to_vfs(e: claudio_btrfs::BtrfsError) -> FsError {
    match e {
        claudio_btrfs::BtrfsError::NotFound => FsError::NotFound,
        claudio_btrfs::BtrfsError::AlreadyExists => FsError::AlreadyExists,
        claudio_btrfs::BtrfsError::IoError => FsError::IoError,
        claudio_btrfs::BtrfsError::NoFreeSpace => FsError::NoSpace,
        claudio_btrfs::BtrfsError::Corrupt(_) | claudio_btrfs::BtrfsError::InvalidSuperblock => FsError::Corrupt,
        claudio_btrfs::BtrfsError::IsADirectory => FsError::WrongType,
        claudio_btrfs::BtrfsError::IsNotADirectory | claudio_btrfs::BtrfsError::NotADirectory => FsError::WrongType,
        claudio_btrfs::BtrfsError::InvalidPath | claudio_btrfs::BtrfsError::NameTooLong => FsError::NotFound,
        claudio_btrfs::BtrfsError::UnsupportedFeature(_) | claudio_btrfs::BtrfsError::CompressedExtent => FsError::Unsupported,
        claudio_btrfs::BtrfsError::UnmappedLogical(_) => FsError::IoError,
        claudio_btrfs::BtrfsError::DecompressError(_) => FsError::IoError,
    }
}

fn ntfs_err_to_vfs(e: claudio_ntfs::NtfsError) -> FsError {
    match e {
        claudio_ntfs::NtfsError::NotFound => FsError::NotFound,
        claudio_ntfs::NtfsError::AlreadyExists => FsError::AlreadyExists,
        claudio_ntfs::NtfsError::IoError => FsError::IoError,
        claudio_ntfs::NtfsError::NoFreeMftEntries | claudio_ntfs::NtfsError::NoFreeClusters => FsError::NoSpace,
        claudio_ntfs::NtfsError::Corrupt(_) | claudio_ntfs::NtfsError::InvalidBootSector
            | claudio_ntfs::NtfsError::CorruptMftEntry(_) => FsError::Corrupt,
        claudio_ntfs::NtfsError::IsADirectory => FsError::WrongType,
        claudio_ntfs::NtfsError::IsNotADirectory | claudio_ntfs::NtfsError::NotADirectory => FsError::WrongType,
        claudio_ntfs::NtfsError::InvalidPath | claudio_ntfs::NtfsError::NameTooLong => FsError::NotFound,
        claudio_ntfs::NtfsError::Unsupported(_) | claudio_ntfs::NtfsError::AttributeNotFound(_) => FsError::Unsupported,
    }
}

// ============================================================================
// Path helpers
// ============================================================================

/// Split a path into (parent, basename).
///
/// `/foo/bar/baz` -> (`/foo/bar`, `baz`)
/// `/foo` -> (`/`, `foo`)
/// `/` -> (`/`, ``)
fn split_parent_basename(path: &str) -> (&str, &str) {
    let path = path.trim_end_matches('/');
    if path.is_empty() || path == "/" {
        return ("/", "");
    }
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => ("/", path),
    }
}
