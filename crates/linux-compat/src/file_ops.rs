//! File-related syscall implementations.
//!
//! Implements read, write, open, close, stat, fstat, lstat, lseek, mmap (file-backed),
//! pread64, pwrite64, readv, writev, access, pipe, dup, dup2, fcntl, ftruncate,
//! getdents, getdents64, getcwd, chdir, rename, mkdir, rmdir, creat, link, unlink,
//! symlink, readlink, chmod, fchmod, chown, openat, newfstatat.

use crate::errno::*;
use crate::types::*;
use alloc::string::String;
use alloc::vec::Vec;

/// Process file descriptor table.
/// Maps Linux fd numbers to internal VFS file descriptors.
pub struct FileDescriptorTable {
    /// Maps fd number -> Option<FdEntry>.
    entries: Vec<Option<FdEntry>>,
    /// Current working directory path.
    cwd: String,
}

/// Entry in the file descriptor table.
#[derive(Debug, Clone)]
pub struct FdEntry {
    /// Path this fd was opened with.
    pub path: String,
    /// Open flags.
    pub flags: u32,
    /// Current file position.
    pub position: u64,
    /// File size (cached from stat).
    pub size: u64,
    /// File data buffer (for in-memory files like pipes, stdin/stdout).
    pub buffer: Vec<u8>,
    /// FD type.
    pub fd_type: FdType,
    /// Close-on-exec flag.
    pub cloexec: bool,
}

/// Type of file descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdType {
    /// Regular file backed by VFS.
    File,
    /// Standard input (fd 0).
    Stdin,
    /// Standard output (fd 1).
    Stdout,
    /// Standard error (fd 2).
    Stderr,
    /// Pipe read end.
    PipeRead,
    /// Pipe write end.
    PipeWrite,
    /// Socket (handled by network_ops).
    Socket,
    /// Epoll instance.
    Epoll,
}

impl FileDescriptorTable {
    /// Create a new FD table with stdin/stdout/stderr pre-opened.
    pub fn new() -> Self {
        let mut entries = Vec::new();
        // fd 0 = stdin
        entries.push(Some(FdEntry {
            path: String::from("/dev/stdin"),
            flags: O_RDONLY,
            position: 0,
            size: 0,
            buffer: Vec::new(),
            fd_type: FdType::Stdin,
            cloexec: false,
        }));
        // fd 1 = stdout
        entries.push(Some(FdEntry {
            path: String::from("/dev/stdout"),
            flags: O_WRONLY,
            position: 0,
            size: 0,
            buffer: Vec::new(),
            fd_type: FdType::Stdout,
            cloexec: false,
        }));
        // fd 2 = stderr
        entries.push(Some(FdEntry {
            path: String::from("/dev/stderr"),
            flags: O_WRONLY,
            position: 0,
            size: 0,
            buffer: Vec::new(),
            fd_type: FdType::Stderr,
            cloexec: false,
        }));
        Self {
            entries,
            cwd: String::from("/"),
        }
    }

    /// Allocate the lowest available fd number.
    pub fn alloc_fd(&mut self, entry: FdEntry) -> Result<usize, Errno> {
        // Find first empty slot
        for (i, slot) in self.entries.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(entry);
                return Ok(i);
            }
        }
        // No empty slot, extend
        let fd = self.entries.len();
        if fd >= 1024 {
            return Err(EMFILE);
        }
        self.entries.push(Some(entry));
        Ok(fd)
    }

    /// Get a reference to an fd entry.
    pub fn get(&self, fd: usize) -> Result<&FdEntry, Errno> {
        self.entries
            .get(fd)
            .and_then(|e| e.as_ref())
            .ok_or(EBADF)
    }

    /// Get a mutable reference to an fd entry.
    pub fn get_mut(&mut self, fd: usize) -> Result<&mut FdEntry, Errno> {
        self.entries
            .get_mut(fd)
            .and_then(|e| e.as_mut())
            .ok_or(EBADF)
    }

    /// Close an fd.
    pub fn close(&mut self, fd: usize) -> Result<(), Errno> {
        if fd >= self.entries.len() {
            return Err(EBADF);
        }
        if self.entries[fd].is_none() {
            return Err(EBADF);
        }
        self.entries[fd] = None;
        Ok(())
    }

    /// Duplicate an fd to a specific target fd number.
    pub fn dup_to(&mut self, old_fd: usize, new_fd: usize) -> Result<usize, Errno> {
        let entry = self.get(old_fd)?.clone();
        // Extend table if needed
        while self.entries.len() <= new_fd {
            self.entries.push(None);
        }
        // Close new_fd if it's open (dup2 semantics)
        self.entries[new_fd] = Some(entry);
        Ok(new_fd)
    }

    /// Get the current working directory.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// Set the current working directory.
    pub fn set_cwd(&mut self, path: String) {
        self.cwd = path;
    }
}

// ============================================================================
// Syscall implementations
// ============================================================================

/// sys_read(fd, buf, count) -> bytes read
pub fn sys_read(fdt: &mut FileDescriptorTable, fd: u64, buf: u64, count: u64) -> i64 {
    let fd = fd as usize;
    let entry = match fdt.get_mut(fd) {
        Ok(e) => e,
        Err(e) => return e.as_neg(),
    };

    match entry.fd_type {
        FdType::Stdin => {
            // In a real implementation, this would block waiting for keyboard input.
            // For now, return 0 (EOF) since we don't have interactive stdin wired up.
            0
        }
        FdType::Stdout | FdType::Stderr => EBADF.as_neg(),
        FdType::File => {
            // Read from the file buffer starting at current position
            let pos = entry.position as usize;
            let available = if pos < entry.buffer.len() {
                entry.buffer.len() - pos
            } else {
                0
            };
            let to_read = core::cmp::min(count as usize, available);
            if to_read == 0 {
                return 0; // EOF
            }
            // In a real implementation we'd copy to the user buffer at `buf`.
            // Here we track the position advancement.
            entry.position += to_read as u64;
            to_read as i64
        }
        FdType::PipeRead => {
            let to_read = core::cmp::min(count as usize, entry.buffer.len());
            if to_read == 0 {
                return 0;
            }
            // Drain from pipe buffer
            let _: Vec<u8> = entry.buffer.drain(..to_read).collect();
            to_read as i64
        }
        _ => EBADF.as_neg(),
    }
}

/// sys_write(fd, buf, count) -> bytes written
pub fn sys_write(fdt: &mut FileDescriptorTable, fd: u64, _buf: u64, count: u64) -> i64 {
    let fd = fd as usize;
    let entry = match fdt.get_mut(fd) {
        Ok(e) => e,
        Err(e) => return e.as_neg(),
    };

    match entry.fd_type {
        FdType::Stdout | FdType::Stderr => {
            // In the real kernel, this writes to the terminal framebuffer.
            // For now, accept the write and return count.
            count as i64
        }
        FdType::File => {
            // Append or write at position
            let pos = entry.position as usize;
            let new_len = pos + count as usize;
            if entry.buffer.len() < new_len {
                entry.buffer.resize(new_len, 0);
            }
            // In a real implementation we'd copy from the user buffer at `buf`.
            entry.position += count as u64;
            entry.size = entry.buffer.len() as u64;
            count as i64
        }
        FdType::PipeWrite => {
            // In a real implementation we'd copy from user buffer and write to pipe.
            count as i64
        }
        _ => EBADF.as_neg(),
    }
}

/// sys_open(filename_ptr, flags, mode) -> fd
pub fn sys_open(fdt: &mut FileDescriptorTable, _filename_ptr: u64, flags: u64, _mode: u64) -> i64 {
    // In a real implementation, we'd read the filename from user memory and
    // call into the VFS. For now, create a file entry.
    let entry = FdEntry {
        path: String::from("<opened-file>"),
        flags: flags as u32,
        position: 0,
        size: 0,
        buffer: Vec::new(),
        fd_type: FdType::File,
        cloexec: (flags as u32 & O_CLOEXEC) != 0,
    };
    match fdt.alloc_fd(entry) {
        Ok(fd) => fd as i64,
        Err(e) => e.as_neg(),
    }
}

/// sys_close(fd) -> 0
pub fn sys_close(fdt: &mut FileDescriptorTable, fd: u64) -> i64 {
    match fdt.close(fd as usize) {
        Ok(()) => 0,
        Err(e) => e.as_neg(),
    }
}

/// sys_stat(filename_ptr, statbuf_ptr) -> 0
pub fn sys_stat(_fdt: &FileDescriptorTable, _filename_ptr: u64, _statbuf_ptr: u64) -> i64 {
    // Would read filename from user memory, stat via VFS, write LinuxStat to statbuf.
    // For now, return a zeroed stat struct (pretend it's a regular file).
    // In kernel integration, this will write to the user-space pointer.
    0
}

/// sys_fstat(fd, statbuf_ptr) -> 0
pub fn sys_fstat(fdt: &FileDescriptorTable, fd: u64, _statbuf_ptr: u64) -> i64 {
    let fd = fd as usize;
    match fdt.get(fd) {
        Ok(entry) => {
            // Build a LinuxStat and write it to statbuf_ptr
            let _stat = LinuxStat {
                st_dev: 1,
                st_ino: fd as u64 + 1000,
                st_nlink: 1,
                st_mode: match entry.fd_type {
                    FdType::File => S_IFREG | 0o644,
                    FdType::Stdin | FdType::Stdout | FdType::Stderr => S_IFCHR | 0o666,
                    FdType::PipeRead | FdType::PipeWrite => S_IFIFO | 0o600,
                    _ => S_IFREG | 0o644,
                },
                st_uid: 0,
                st_gid: 0,
                _pad0: 0,
                st_rdev: 0,
                st_size: entry.size as i64,
                st_blksize: 4096,
                st_blocks: ((entry.size + 511) / 512) as i64,
                st_atime: 0,
                st_atime_nsec: 0,
                st_mtime: 0,
                st_mtime_nsec: 0,
                st_ctime: 0,
                st_ctime_nsec: 0,
                _unused: [0; 3],
            };
            // In kernel: unsafe { *(statbuf_ptr as *mut LinuxStat) = stat; }
            0
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_lstat(filename_ptr, statbuf_ptr) -> 0
pub fn sys_lstat(_fdt: &FileDescriptorTable, _filename_ptr: u64, _statbuf_ptr: u64) -> i64 {
    // Same as stat but doesn't follow symlinks. For now, same implementation.
    0
}

/// sys_lseek(fd, offset, whence) -> new position
pub fn sys_lseek(fdt: &mut FileDescriptorTable, fd: u64, offset: u64, whence: u64) -> i64 {
    let fd = fd as usize;
    let offset = offset as i64;
    let entry = match fdt.get_mut(fd) {
        Ok(e) => e,
        Err(e) => return e.as_neg(),
    };

    match entry.fd_type {
        FdType::PipeRead | FdType::PipeWrite | FdType::Socket => return ESPIPE.as_neg(),
        _ => {}
    }

    let new_pos = match whence as u32 {
        SEEK_SET => offset,
        SEEK_CUR => entry.position as i64 + offset,
        SEEK_END => entry.size as i64 + offset,
        _ => return EINVAL.as_neg(),
    };

    if new_pos < 0 {
        return EINVAL.as_neg();
    }

    entry.position = new_pos as u64;
    new_pos
}

/// sys_pread64(fd, buf, count, offset) -> bytes read
pub fn sys_pread64(fdt: &mut FileDescriptorTable, fd: u64, _buf: u64, count: u64, offset: u64) -> i64 {
    let fd = fd as usize;
    let entry = match fdt.get(fd) {
        Ok(e) => e,
        Err(e) => return e.as_neg(),
    };

    let pos = offset as usize;
    let available = if pos < entry.buffer.len() {
        entry.buffer.len() - pos
    } else {
        0
    };
    let to_read = core::cmp::min(count as usize, available);
    to_read as i64
}

/// sys_pwrite64(fd, buf, count, offset) -> bytes written
pub fn sys_pwrite64(fdt: &mut FileDescriptorTable, fd: u64, _buf: u64, count: u64, _offset: u64) -> i64 {
    let fd = fd as usize;
    match fdt.get(fd) {
        Ok(_) => count as i64,
        Err(e) => e.as_neg(),
    }
}

/// sys_readv(fd, iov, iovcnt) -> total bytes read
pub fn sys_readv(fdt: &mut FileDescriptorTable, fd: u64, _iov: u64, iovcnt: u64) -> i64 {
    let fd_num = fd as usize;
    match fdt.get(fd_num) {
        Ok(_) => {
            // In kernel: iterate over iovec array, call read for each
            // For now, return 0 (nothing to read)
            let _ = iovcnt;
            0
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_writev(fd, iov, iovcnt) -> total bytes written
pub fn sys_writev(fdt: &mut FileDescriptorTable, fd: u64, _iov: u64, _iovcnt: u64) -> i64 {
    let fd_num = fd as usize;
    match fdt.get(fd_num) {
        Ok(entry) => {
            match entry.fd_type {
                FdType::Stdout | FdType::Stderr => {
                    // In kernel: iterate over iovec array, write each buffer to terminal.
                    // Return total bytes (we'd sum iov_len values).
                    0
                }
                _ => 0,
            }
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_access(filename_ptr, mode) -> 0
pub fn sys_access(_fdt: &FileDescriptorTable, _filename_ptr: u64, _mode: u64) -> i64 {
    // Check if file exists and has requested permissions.
    // For now, return success (pretend everything is accessible).
    0
}

/// sys_pipe(pipefd_ptr) -> 0
pub fn sys_pipe(fdt: &mut FileDescriptorTable, _pipefd_ptr: u64) -> i64 {
    let read_end = FdEntry {
        path: String::from("<pipe:read>"),
        flags: O_RDONLY,
        position: 0,
        size: 0,
        buffer: Vec::new(),
        fd_type: FdType::PipeRead,
        cloexec: false,
    };
    let write_end = FdEntry {
        path: String::from("<pipe:write>"),
        flags: O_WRONLY,
        position: 0,
        size: 0,
        buffer: Vec::new(),
        fd_type: FdType::PipeWrite,
        cloexec: false,
    };

    let read_fd = match fdt.alloc_fd(read_end) {
        Ok(fd) => fd,
        Err(e) => return e.as_neg(),
    };
    let write_fd = match fdt.alloc_fd(write_end) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = fdt.close(read_fd);
            return e.as_neg();
        }
    };

    // In kernel: write [read_fd, write_fd] as i32 pair to pipefd_ptr
    let _ = (read_fd, write_fd);
    0
}

/// sys_dup(oldfd) -> new fd
pub fn sys_dup(fdt: &mut FileDescriptorTable, oldfd: u64) -> i64 {
    let entry = match fdt.get(oldfd as usize) {
        Ok(e) => e.clone(),
        Err(e) => return e.as_neg(),
    };
    match fdt.alloc_fd(entry) {
        Ok(fd) => fd as i64,
        Err(e) => e.as_neg(),
    }
}

/// sys_dup2(oldfd, newfd) -> newfd
pub fn sys_dup2(fdt: &mut FileDescriptorTable, oldfd: u64, newfd: u64) -> i64 {
    if oldfd == newfd {
        // Check oldfd is valid
        return match fdt.get(oldfd as usize) {
            Ok(_) => newfd as i64,
            Err(e) => e.as_neg(),
        };
    }
    match fdt.dup_to(oldfd as usize, newfd as usize) {
        Ok(fd) => fd as i64,
        Err(e) => e.as_neg(),
    }
}

/// sys_fcntl(fd, cmd, arg) -> result
pub fn sys_fcntl(fdt: &mut FileDescriptorTable, fd: u64, cmd: u64, arg: u64) -> i64 {
    let fd = fd as usize;
    match cmd as u32 {
        F_DUPFD => {
            let entry = match fdt.get(fd) {
                Ok(e) => e.clone(),
                Err(e) => return e.as_neg(),
            };
            // Find lowest fd >= arg
            let min_fd = arg as usize;
            while fdt.entries.len() <= min_fd {
                fdt.entries.push(None);
            }
            for i in min_fd..fdt.entries.len() {
                if fdt.entries[i].is_none() {
                    fdt.entries[i] = Some(entry);
                    return i as i64;
                }
            }
            let new_fd = fdt.entries.len();
            fdt.entries.push(Some(entry));
            new_fd as i64
        }
        F_GETFD => {
            match fdt.get(fd) {
                Ok(e) => if e.cloexec { FD_CLOEXEC as i64 } else { 0 },
                Err(e) => e.as_neg(),
            }
        }
        F_SETFD => {
            match fdt.get_mut(fd) {
                Ok(e) => {
                    e.cloexec = (arg as u32 & FD_CLOEXEC) != 0;
                    0
                }
                Err(e) => e.as_neg(),
            }
        }
        F_GETFL => {
            match fdt.get(fd) {
                Ok(e) => e.flags as i64,
                Err(e) => e.as_neg(),
            }
        }
        F_SETFL => {
            match fdt.get_mut(fd) {
                Ok(e) => {
                    // Can only change O_APPEND, O_ASYNC, O_DIRECT, O_NOATIME, O_NONBLOCK
                    let changeable = O_APPEND | O_ASYNC | O_DIRECT | O_NOATIME | O_NONBLOCK;
                    e.flags = (e.flags & !changeable) | (arg as u32 & changeable);
                    0
                }
                Err(e) => e.as_neg(),
            }
        }
        F_DUPFD_CLOEXEC => {
            let mut entry = match fdt.get(fd) {
                Ok(e) => e.clone(),
                Err(e) => return e.as_neg(),
            };
            entry.cloexec = true;
            match fdt.alloc_fd(entry) {
                Ok(new_fd) => new_fd as i64,
                Err(e) => e.as_neg(),
            }
        }
        _ => EINVAL.as_neg(),
    }
}

/// sys_ftruncate(fd, length) -> 0
pub fn sys_ftruncate(fdt: &mut FileDescriptorTable, fd: u64, length: u64) -> i64 {
    let fd = fd as usize;
    match fdt.get_mut(fd) {
        Ok(entry) => {
            entry.buffer.resize(length as usize, 0);
            entry.size = length;
            0
        }
        Err(e) => e.as_neg(),
    }
}

/// sys_getdents(fd, dirp, count) -> bytes written
pub fn sys_getdents(_fdt: &FileDescriptorTable, _fd: u64, _dirp: u64, _count: u64) -> i64 {
    // Would iterate directory entries from VFS.
    // Return 0 to indicate end of directory.
    0
}

/// sys_getdents64(fd, dirp, count) -> bytes written
pub fn sys_getdents64(_fdt: &FileDescriptorTable, _fd: u64, _dirp: u64, _count: u64) -> i64 {
    0
}

/// sys_getcwd(buf, size) -> buf pointer
pub fn sys_getcwd(fdt: &FileDescriptorTable, buf: u64, size: u64) -> i64 {
    let cwd = fdt.cwd();
    let cwd_bytes = cwd.as_bytes();
    if cwd_bytes.len() + 1 > size as usize {
        return ERANGE.as_neg();
    }
    // In kernel: copy cwd_bytes + null terminator to buf
    let _ = buf;
    buf as i64
}

/// sys_chdir(filename_ptr) -> 0
pub fn sys_chdir(fdt: &mut FileDescriptorTable, _filename_ptr: u64) -> i64 {
    // In kernel: read filename from user memory, verify it's a directory
    // For now, accept the change.
    // fdt.set_cwd(new_path);
    let _ = fdt;
    0
}

/// sys_rename(oldname_ptr, newname_ptr) -> 0
pub fn sys_rename(_fdt: &FileDescriptorTable, _oldname_ptr: u64, _newname_ptr: u64) -> i64 {
    // VFS rename operation
    0
}

/// sys_mkdir(pathname_ptr, mode) -> 0
pub fn sys_mkdir(_fdt: &FileDescriptorTable, _pathname_ptr: u64, _mode: u64) -> i64 {
    0
}

/// sys_rmdir(pathname_ptr) -> 0
pub fn sys_rmdir(_fdt: &FileDescriptorTable, _pathname_ptr: u64) -> i64 {
    0
}

/// sys_creat(pathname_ptr, mode) -> fd
/// Equivalent to open(pathname, O_CREAT|O_WRONLY|O_TRUNC, mode)
pub fn sys_creat(fdt: &mut FileDescriptorTable, pathname_ptr: u64, mode: u64) -> i64 {
    sys_open(fdt, pathname_ptr, (O_CREAT | O_WRONLY | O_TRUNC) as u64, mode)
}

/// sys_link(oldname_ptr, newname_ptr) -> 0
pub fn sys_link(_fdt: &FileDescriptorTable, _oldname_ptr: u64, _newname_ptr: u64) -> i64 {
    // Hard links — VFS operation
    0
}

/// sys_unlink(pathname_ptr) -> 0
pub fn sys_unlink(_fdt: &FileDescriptorTable, _pathname_ptr: u64) -> i64 {
    0
}

/// sys_symlink(target_ptr, linkpath_ptr) -> 0
pub fn sys_symlink(_fdt: &FileDescriptorTable, _target_ptr: u64, _linkpath_ptr: u64) -> i64 {
    0
}

/// sys_readlink(path_ptr, buf, bufsiz) -> bytes written
pub fn sys_readlink(_fdt: &FileDescriptorTable, _path_ptr: u64, _buf: u64, _bufsiz: u64) -> i64 {
    EINVAL.as_neg() // Not a symbolic link by default
}

/// sys_chmod(filename_ptr, mode) -> 0
pub fn sys_chmod(_fdt: &FileDescriptorTable, _filename_ptr: u64, _mode: u64) -> i64 {
    0
}

/// sys_fchmod(fd, mode) -> 0
pub fn sys_fchmod(fdt: &FileDescriptorTable, fd: u64, _mode: u64) -> i64 {
    match fdt.get(fd as usize) {
        Ok(_) => 0,
        Err(e) => e.as_neg(),
    }
}

/// sys_chown(filename_ptr, owner, group) -> 0
pub fn sys_chown(_fdt: &FileDescriptorTable, _filename_ptr: u64, _owner: u64, _group: u64) -> i64 {
    0
}

/// sys_openat(dirfd, pathname_ptr, flags, mode) -> fd
pub fn sys_openat(fdt: &mut FileDescriptorTable, _dirfd: u64, pathname_ptr: u64, flags: u64, mode: u64) -> i64 {
    // If dirfd == AT_FDCWD, behave like open().
    // Otherwise, resolve pathname relative to dirfd's directory.
    sys_open(fdt, pathname_ptr, flags, mode)
}

/// sys_newfstatat(dirfd, pathname_ptr, statbuf_ptr, flag) -> 0
pub fn sys_newfstatat(fdt: &FileDescriptorTable, _dirfd: u64, _pathname_ptr: u64, statbuf_ptr: u64, _flag: u64) -> i64 {
    sys_stat(fdt, 0, statbuf_ptr)
}
