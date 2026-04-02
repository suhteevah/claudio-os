//! Top-level VFS — the single entry point that ties together mounts, file
//! operations, directory operations, path resolution, and working directory state.
//!
//! Provides a POSIX-like API: open, read, write, close, stat, mkdir, readdir,
//! rm, cp, mv, pwd, cd.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::device::{BlockDevice, PartitionEntry, detect_partition_scheme, parse_gpt, parse_mbr, PartitionScheme};
use crate::dir::DirEntry;
use crate::file::{FileDescriptor, FileInfo, FileType, OpenFile, OpenFlags, SeekWhence};
use crate::fs_trait::{Filesystem, FsError};
use crate::mount::{MountOptions, MountTable, MountError};
use crate::path::Path;

/// Maximum number of simultaneously open files.
const MAX_OPEN_FILES: usize = 256;

/// VFS error type, wrapping filesystem and mount errors.
#[derive(Debug)]
pub enum VfsError {
    /// A filesystem-level error.
    Fs(FsError),
    /// A mount-table error.
    Mount(MountError),
    /// The file descriptor is invalid (not open, or already closed).
    BadFd,
    /// Too many files are open.
    TooManyOpen,
    /// The operation requires write access but the mount is read-only.
    ReadOnly,
    /// The operation is invalid for the given file state (e.g. read on write-only).
    InvalidOp,
    /// A path argument is invalid.
    InvalidPath,
}

impl From<FsError> for VfsError {
    fn from(e: FsError) -> Self {
        VfsError::Fs(e)
    }
}

impl From<MountError> for VfsError {
    fn from(e: MountError) -> Self {
        VfsError::Mount(e)
    }
}

impl core::fmt::Display for VfsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            VfsError::Fs(e) => write!(f, "filesystem error: {}", e),
            VfsError::Mount(e) => write!(f, "mount error: {}", e),
            VfsError::BadFd => write!(f, "bad file descriptor"),
            VfsError::TooManyOpen => write!(f, "too many open files"),
            VfsError::ReadOnly => write!(f, "read-only filesystem"),
            VfsError::InvalidOp => write!(f, "invalid operation"),
            VfsError::InvalidPath => write!(f, "invalid path"),
        }
    }
}

/// The top-level Virtual Filesystem.
///
/// Owns the mount table, the open-file table, and the current working directory.
pub struct Vfs {
    /// Mount table for path->filesystem resolution.
    mount_table: MountTable,
    /// Open file table, indexed by file descriptor number.
    open_files: Vec<Option<OpenFile>>,
    /// Current working directory (always absolute).
    cwd: String,
}

impl Vfs {
    /// Create a new VFS instance with an empty mount table and cwd at `/`.
    pub fn new() -> Self {
        log::info!("vfs: initializing virtual filesystem");
        Self {
            mount_table: MountTable::new(),
            open_files: Vec::new(),
            cwd: String::from("/"),
        }
    }

    // ── Mount operations ─────────────────────────────────────────────────

    /// Mount a filesystem at the given absolute path.
    pub fn mount(
        &mut self,
        path: &str,
        filesystem: &'static dyn Filesystem,
        options: MountOptions,
    ) -> Result<(), VfsError> {
        log::info!("vfs: mounting {} at {:?}", filesystem.fs_type(), path);
        self.mount_table.mount(path, filesystem, options)?;
        Ok(())
    }

    /// Unmount the filesystem at the given path.
    pub fn umount(&mut self, path: &str) -> Result<(), VfsError> {
        log::info!("vfs: unmounting {:?}", path);
        // Close any open files on this mount
        let normalized = if path.len() > 1 {
            path.trim_end_matches('/')
        } else {
            path
        };
        let mut closed = 0usize;
        for slot in self.open_files.iter_mut() {
            if let Some(open_file) = slot.as_ref() {
                if open_file.path.starts_with(normalized) {
                    *slot = None;
                    closed += 1;
                }
            }
        }
        if closed > 0 {
            log::warn!("vfs: force-closed {} open file(s) on unmounted filesystem", closed);
        }
        self.mount_table.umount(path)?;
        Ok(())
    }

    /// List all current mount points.
    pub fn mounts(&self) -> &[crate::mount::MountPoint] {
        self.mount_table.list()
    }

    /// Auto-detect partitions and their filesystem types on a block device.
    ///
    /// Returns partition entries with `fs_type_hint` populated. The caller
    /// is responsible for mounting appropriate filesystem implementations.
    pub fn detect_partitions(
        &self,
        device: &dyn BlockDevice,
    ) -> Result<Vec<PartitionEntry>, VfsError> {
        log::info!("vfs: detecting partitions on device (size={} bytes)", device.total_size());

        let scheme = detect_partition_scheme(device);
        match scheme {
            PartitionScheme::Gpt => {
                parse_gpt(device).map_err(|_| VfsError::Fs(FsError::IoError))
            }
            PartitionScheme::Mbr => {
                parse_mbr(device).map_err(|_| VfsError::Fs(FsError::IoError))
            }
            PartitionScheme::None => {
                log::info!("vfs: no partition table found — device may contain a raw filesystem");
                Ok(Vec::new())
            }
        }
    }

    // ── Working directory ────────────────────────────────────────────────

    /// Return the current working directory.
    pub fn pwd(&self) -> &str {
        &self.cwd
    }

    /// Change the working directory.
    pub fn cd(&mut self, path: &str) -> Result<(), VfsError> {
        let abs = self.resolve_path(path);
        log::info!("vfs: cd {:?} -> {:?}", path, abs);

        // Verify the target is a directory
        let resolved = self.mount_table.resolve(abs.as_str())?;
        let info = resolved.filesystem.stat(resolved.relative_path.as_str())?;
        if info.file_type != FileType::Directory {
            log::error!("vfs: cd target {:?} is not a directory", abs);
            return Err(VfsError::Fs(FsError::WrongType));
        }

        self.cwd = abs;
        log::debug!("vfs: cwd is now {:?}", self.cwd);
        Ok(())
    }

    // ── File operations ──────────────────────────────────────────────────

    /// Open a file, returning a file descriptor.
    pub fn open(&mut self, path: &str, flags: OpenFlags) -> Result<FileDescriptor, VfsError> {
        let abs = self.resolve_path(path);
        log::info!("vfs: open {:?} (flags=0x{:x})", abs, flags.bits());

        let resolved = self.mount_table.resolve(abs.as_str())?;

        // Enforce read-only mounts
        if resolved.read_only && (flags.has(OpenFlags::WRITE) || flags.has(OpenFlags::CREATE) || flags.has(OpenFlags::TRUNCATE)) {
            log::error!("vfs: write operation on read-only mount for {:?}", abs);
            return Err(VfsError::ReadOnly);
        }

        // Create file if needed
        if flags.has(OpenFlags::CREATE) {
            match resolved.filesystem.stat(resolved.relative_path.as_str()) {
                Ok(_) => {
                    log::trace!("vfs: file already exists, skipping create");
                }
                Err(FsError::NotFound) => {
                    log::debug!("vfs: creating file {:?}", resolved.relative_path.as_str());
                    resolved.filesystem.create_file(resolved.relative_path.as_str())?;
                }
                Err(e) => return Err(VfsError::Fs(e)),
            }
        }

        // Truncate if requested
        if flags.has(OpenFlags::TRUNCATE) {
            log::debug!("vfs: truncating file {:?}", resolved.relative_path.as_str());
            resolved.filesystem.truncate(resolved.relative_path.as_str(), 0)?;
        }

        // Get file info for initial position and size
        let info = resolved.filesystem.stat(resolved.relative_path.as_str())?;
        let position = if flags.has(OpenFlags::APPEND) {
            info.size
        } else {
            0
        };

        // Find the mount index for this path
        let mount_index = self.find_mount_index(abs.as_str());

        // Allocate a file descriptor
        let fd = self.alloc_fd()?;
        let of = OpenFile {
            path: abs.clone(),
            mount_index,
            relative_path: String::from(resolved.relative_path.as_str()),
            position,
            flags,
            size: info.size,
        };

        // Store in the open-file table
        if fd.0 >= self.open_files.len() {
            self.open_files.resize_with(fd.0 + 1, || None);
        }
        self.open_files[fd.0] = Some(of);

        log::debug!("vfs: opened {:?} as fd={}", abs, fd.0);
        Ok(fd)
    }

    /// Read from an open file into `buf`. Returns the number of bytes read.
    pub fn read(&mut self, fd: FileDescriptor, buf: &mut [u8]) -> Result<usize, VfsError> {
        let of = self.open_files.get(fd.0).and_then(|o| o.as_ref()).ok_or(VfsError::BadFd)?;
        if !of.flags.has(OpenFlags::READ) {
            log::error!("vfs: read on fd={} not opened for reading", fd.0);
            return Err(VfsError::InvalidOp);
        }

        let rel_path = of.relative_path.clone();
        let position = of.position;
        let abs_path = of.path.clone();

        let resolved = self.mount_table.resolve(abs_path.as_str())?;
        let n = resolved.filesystem.read_file(rel_path.as_str(), position, buf)?;

        // Update position
        if let Some(Some(open_file)) = self.open_files.get_mut(fd.0) {
            open_file.position += n as u64;
        }

        log::trace!("vfs: read fd={}: {} bytes at offset {}", fd.0, n, position);
        Ok(n)
    }

    /// Write to an open file from `data`. Returns the number of bytes written.
    pub fn write(&mut self, fd: FileDescriptor, data: &[u8]) -> Result<usize, VfsError> {
        let of = self.open_files.get(fd.0).and_then(|o| o.as_ref()).ok_or(VfsError::BadFd)?;
        if !of.flags.has(OpenFlags::WRITE) {
            log::error!("vfs: write on fd={} not opened for writing", fd.0);
            return Err(VfsError::InvalidOp);
        }

        let rel_path = of.relative_path.clone();
        let position = of.position;
        let abs_path = of.path.clone();

        let resolved = self.mount_table.resolve(abs_path.as_str())?;
        if resolved.read_only {
            return Err(VfsError::ReadOnly);
        }

        let n = resolved.filesystem.write_file(rel_path.as_str(), position, data)?;

        // Update position and cached size
        if let Some(Some(open_file)) = self.open_files.get_mut(fd.0) {
            open_file.position += n as u64;
            if open_file.position > open_file.size {
                open_file.size = open_file.position;
            }
        }

        log::trace!("vfs: write fd={}: {} bytes at offset {}", fd.0, n, position);
        Ok(n)
    }

    /// Seek within an open file. Returns the new absolute position.
    pub fn seek(&mut self, fd: FileDescriptor, offset: i64, whence: SeekWhence) -> Result<u64, VfsError> {
        let of = self.open_files.get_mut(fd.0).and_then(|o| o.as_mut()).ok_or(VfsError::BadFd)?;

        let new_pos = match whence {
            SeekWhence::Start => {
                if offset < 0 {
                    return Err(VfsError::Fs(FsError::OutOfRange));
                }
                offset as u64
            }
            SeekWhence::Current => {
                let cur = of.position as i64;
                let result = cur + offset;
                if result < 0 {
                    return Err(VfsError::Fs(FsError::OutOfRange));
                }
                result as u64
            }
            SeekWhence::End => {
                let end = of.size as i64;
                let result = end + offset;
                if result < 0 {
                    return Err(VfsError::Fs(FsError::OutOfRange));
                }
                result as u64
            }
        };

        of.position = new_pos;
        log::trace!("vfs: seek fd={}: new position={}", fd.0, new_pos);
        Ok(new_pos)
    }

    /// Close an open file descriptor.
    pub fn close(&mut self, fd: FileDescriptor) -> Result<(), VfsError> {
        if fd.0 >= self.open_files.len() {
            return Err(VfsError::BadFd);
        }
        if self.open_files[fd.0].is_none() {
            return Err(VfsError::BadFd);
        }
        log::debug!("vfs: closing fd={}", fd.0);
        self.open_files[fd.0] = None;
        Ok(())
    }

    /// Get metadata for a path.
    pub fn stat(&self, path: &str) -> Result<FileInfo, VfsError> {
        let abs = self.resolve_path(path);
        log::trace!("vfs: stat {:?}", abs);
        let resolved = self.mount_table.resolve(abs.as_str())?;
        let info = resolved.filesystem.stat(resolved.relative_path.as_str())?;
        Ok(info)
    }

    // ── Directory operations ─────────────────────────────────────────────

    /// List entries in a directory.
    pub fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, VfsError> {
        let abs = self.resolve_path(path);
        log::debug!("vfs: readdir {:?}", abs);
        let resolved = self.mount_table.resolve(abs.as_str())?;
        let entries = resolved.filesystem.readdir(resolved.relative_path.as_str())?;
        log::debug!("vfs: readdir {:?}: {} entries", abs, entries.len());
        Ok(entries)
    }

    /// Create a directory.
    pub fn mkdir(&self, path: &str) -> Result<(), VfsError> {
        let abs = self.resolve_path(path);
        log::info!("vfs: mkdir {:?}", abs);
        let resolved = self.mount_table.resolve(abs.as_str())?;
        if resolved.read_only {
            return Err(VfsError::ReadOnly);
        }
        resolved.filesystem.mkdir(resolved.relative_path.as_str())?;
        Ok(())
    }

    /// Remove an empty directory.
    pub fn rmdir(&self, path: &str) -> Result<(), VfsError> {
        let abs = self.resolve_path(path);
        log::info!("vfs: rmdir {:?}", abs);
        let resolved = self.mount_table.resolve(abs.as_str())?;
        if resolved.read_only {
            return Err(VfsError::ReadOnly);
        }
        resolved.filesystem.rmdir(resolved.relative_path.as_str())?;
        Ok(())
    }

    // ── High-level convenience operations ────────────────────────────────

    /// Remove a file.
    pub fn rm(&self, path: &str) -> Result<(), VfsError> {
        let abs = self.resolve_path(path);
        log::info!("vfs: rm {:?}", abs);
        let resolved = self.mount_table.resolve(abs.as_str())?;
        if resolved.read_only {
            return Err(VfsError::ReadOnly);
        }
        resolved.filesystem.delete_file(resolved.relative_path.as_str())?;
        Ok(())
    }

    /// Copy a file from `src` to `dst`.
    ///
    /// Reads the entire source file into memory and writes it to the destination.
    /// The destination is created (or truncated) automatically.
    pub fn cp(&mut self, src: &str, dst: &str) -> Result<(), VfsError> {
        let src_abs = self.resolve_path(src);
        let dst_abs = self.resolve_path(dst);
        log::info!("vfs: cp {:?} -> {:?}", src_abs, dst_abs);

        // Read source
        let src_resolved = self.mount_table.resolve(src_abs.as_str())?;
        let info = src_resolved.filesystem.stat(src_resolved.relative_path.as_str())?;
        if info.file_type != FileType::File {
            log::error!("vfs: cp source {:?} is not a regular file", src_abs);
            return Err(VfsError::Fs(FsError::WrongType));
        }

        let mut data = vec![0u8; info.size as usize];
        src_resolved.filesystem.read_file(
            src_resolved.relative_path.as_str(),
            0,
            &mut data,
        )?;

        // Write destination
        let dst_resolved = self.mount_table.resolve(dst_abs.as_str())?;
        if dst_resolved.read_only {
            return Err(VfsError::ReadOnly);
        }

        // Create destination file if it doesn't exist
        match dst_resolved.filesystem.stat(dst_resolved.relative_path.as_str()) {
            Ok(_) => {
                dst_resolved.filesystem.truncate(dst_resolved.relative_path.as_str(), 0)?;
            }
            Err(FsError::NotFound) => {
                dst_resolved.filesystem.create_file(dst_resolved.relative_path.as_str())?;
            }
            Err(e) => return Err(VfsError::Fs(e)),
        }

        dst_resolved.filesystem.write_file(
            dst_resolved.relative_path.as_str(),
            0,
            &data,
        )?;

        log::info!("vfs: cp complete: {} bytes", data.len());
        Ok(())
    }

    /// Move (rename) a file or directory from `src` to `dst`.
    ///
    /// If both paths are on the same mount, uses the filesystem's rename.
    /// Otherwise falls back to copy + delete.
    pub fn mv(&mut self, src: &str, dst: &str) -> Result<(), VfsError> {
        let src_abs = self.resolve_path(src);
        let dst_abs = self.resolve_path(dst);
        log::info!("vfs: mv {:?} -> {:?}", src_abs, dst_abs);

        let src_mount = self.find_mount_index(src_abs.as_str());
        let dst_mount = self.find_mount_index(dst_abs.as_str());

        if src_mount == dst_mount {
            // Same filesystem — use rename
            let resolved = self.mount_table.resolve(src_abs.as_str())?;
            if resolved.read_only {
                return Err(VfsError::ReadOnly);
            }
            let dst_resolved = self.mount_table.resolve(dst_abs.as_str())?;
            resolved.filesystem.rename(
                resolved.relative_path.as_str(),
                dst_resolved.relative_path.as_str(),
            )?;
            log::info!("vfs: mv (rename) complete");
        } else {
            // Cross-filesystem move: copy then delete
            log::debug!("vfs: mv across filesystems, using cp + rm");
            self.cp(src, dst)?;
            self.rm(src)?;
            log::info!("vfs: mv (cross-fs copy+delete) complete");
        }

        Ok(())
    }

    /// Rename a file or directory (alias for `mv`).
    pub fn rename(&mut self, old: &str, new: &str) -> Result<(), VfsError> {
        self.mv(old, new)
    }

    /// Sync all mounted filesystems (flush pending writes).
    pub fn sync_all(&self) {
        log::info!("vfs: syncing all mounted filesystems");
        for mount in self.mount_table.list() {
            if let Err(e) = mount.filesystem.sync() {
                log::error!("vfs: sync failed for mount {:?}: {}", mount.path, e);
            } else {
                log::debug!("vfs: synced mount {:?}", mount.path);
            }
        }
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    /// Resolve a possibly-relative path to an absolute path using the cwd.
    fn resolve_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            let p = Path::new(path).normalize();
            String::from(p.as_str())
        } else {
            let base = Path::new(&self.cwd);
            let joined = base.join(path);
            let normalized = joined.normalize();
            String::from(normalized.as_str())
        }
    }

    /// Find the mount-table index for a given absolute path.
    ///
    /// Returns a synthetic index based on position in the mount list.
    fn find_mount_index(&self, path: &str) -> usize {
        for (i, mount) in self.mount_table.list().iter().enumerate() {
            let prefix = mount.path.as_str();
            if path.starts_with(prefix) {
                let rest = &path[prefix.len()..];
                if rest.is_empty() || rest.starts_with('/') || prefix == "/" {
                    return i;
                }
            }
        }
        usize::MAX
    }

    /// Allocate the lowest available file descriptor.
    fn alloc_fd(&self) -> Result<FileDescriptor, VfsError> {
        // Reuse a closed slot
        for (i, slot) in self.open_files.iter().enumerate() {
            if slot.is_none() {
                return Ok(FileDescriptor(i));
            }
        }
        // Allocate a new slot
        let next = self.open_files.len();
        if next >= MAX_OPEN_FILES {
            log::error!("vfs: too many open files (max={})", MAX_OPEN_FILES);
            return Err(VfsError::TooManyOpen);
        }
        Ok(FileDescriptor(next))
    }
}
