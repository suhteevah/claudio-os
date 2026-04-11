//! Global VFS singleton for ClaudioOS.
//!
//! This module owns the kernel's single `Vfs` instance, mounts a `MemFs` at
//! `/` during boot, creates the standard ClaudioOS directory tree, and wires
//! the `claudio-fs` persistence crate to route all reads/writes through the
//! VFS via the [`VfsBackend`] adapter.
//!
//! Other kernel modules access the VFS via [`with_vfs`], which acquires the
//! singleton's mutex and hands the caller a `&mut Vfs` closure-scoped.
//!
//! # Boot ordering
//!
//! [`init`] must be called **after** the heap allocator is online (it uses
//! `Box` and `Vec`) and **before** any code that reads credentials/config or
//! otherwise relies on `claudio-fs` having a backend installed.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use spin::{Mutex, Once};

use claudio_vfs::{MemFs, MountOptions, OpenFlags, Vfs, VfsError};
use claudio_fs::{FsBackend, FsError};

/// The kernel's single VFS instance. Populated by [`init`] on boot.
static VFS: Once<Mutex<Vfs>> = Once::new();

/// Initialize the kernel VFS: mount a `MemFs` at `/`, create the standard
/// ClaudioOS directory tree, and install [`VfsBackend`] as the `claudio-fs`
/// backend.
///
/// Safe to call more than once — subsequent calls are no-ops because `Once`
/// guarantees one-shot initialization.
pub fn init() {
    if VFS.get().is_some() {
        log::warn!("[storage] init called twice — ignoring");
        return;
    }

    let mut vfs = Vfs::new();

    // Leak a MemFs so it has 'static lifetime (Vfs::mount requires
    // `&'static dyn Filesystem`). The kernel never unmounts the root FS,
    // so a one-time leak is appropriate and costs us one allocation for
    // the entire kernel lifetime.
    let memfs: &'static MemFs = Box::leak(Box::new(MemFs::new()));

    vfs.mount("/", memfs, MountOptions::default())
        .expect("mount MemFs at /");

    // Standard ClaudioOS directory tree. Errors are ignored — if any of
    // these already exist (e.g. the MemFs preseeds them) that's fine, and
    // any other failure will surface on the first real operation.
    let _ = vfs.mkdir("/claudio");
    let _ = vfs.mkdir("/claudio/agents");
    let _ = vfs.mkdir("/claudio/logs");

    VFS.call_once(|| Mutex::new(vfs));

    // Wire fs-persist to use our VFS.
    static BACKEND: VfsBackend = VfsBackend;
    claudio_fs::set_backend(&BACKEND);

    log::info!("[storage] VFS initialized with MemFs at /");
}

/// Borrow the kernel VFS for the duration of a closure.
///
/// Returns `Err` if [`init`] has not yet been called. The mutex is held for
/// the duration of `f`, so callers should do their work and return promptly.
pub fn with_vfs<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&mut Vfs) -> Result<R, String>,
{
    let vfs = VFS
        .get()
        .ok_or_else(|| "VFS not initialized".to_string())?;
    let mut guard = vfs.lock();
    f(&mut guard)
}

/// Adapter that exposes the kernel VFS to `claudio-fs` via its `FsBackend`
/// trait. Every method re-acquires the VFS mutex, performs the op, and
/// releases the lock before returning.
pub struct VfsBackend;

impl FsBackend for VfsBackend {
    fn read_file(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let vfs_mutex = VFS.get().ok_or(FsError::NotMounted)?;
        let mut vfs = vfs_mutex.lock();

        // Stat first so we can size the buffer exactly.
        let info = vfs.stat(path).map_err(vfs_err_to_fs_err)?;
        let size = info.size as usize;

        let fd = vfs
            .open(path, OpenFlags::read_only())
            .map_err(vfs_err_to_fs_err)?;

        let mut out: Vec<u8> = vec![0u8; size];
        let mut filled = 0usize;
        while filled < size {
            match vfs.read(fd, &mut out[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => {
                    let _ = vfs.close(fd);
                    return Err(vfs_err_to_fs_err(e));
                }
            }
        }
        out.truncate(filled);

        let _ = vfs.close(fd);
        Ok(out)
    }

    fn write_file(&self, path: &str, data: &[u8]) -> Result<(), FsError> {
        let vfs_mutex = VFS.get().ok_or(FsError::NotMounted)?;
        let mut vfs = vfs_mutex.lock();

        let fd = vfs
            .open(path, OpenFlags::create_truncate())
            .map_err(vfs_err_to_fs_err)?;

        let mut written = 0usize;
        while written < data.len() {
            match vfs.write(fd, &data[written..]) {
                Ok(0) => {
                    let _ = vfs.close(fd);
                    return Err(FsError::WriteFailed);
                }
                Ok(n) => written += n,
                Err(e) => {
                    let _ = vfs.close(fd);
                    return Err(vfs_err_to_fs_err(e));
                }
            }
        }

        let _ = vfs.close(fd);
        Ok(())
    }

    fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError> {
        let vfs_mutex = VFS.get().ok_or(FsError::NotMounted)?;
        let vfs = vfs_mutex.lock();

        let entries = vfs.readdir(path).map_err(vfs_err_to_fs_err)?;
        Ok(entries.into_iter().map(|e| e.name).collect())
    }

    fn mkdir(&self, path: &str) -> Result<(), FsError> {
        let vfs_mutex = VFS.get().ok_or(FsError::NotMounted)?;
        let vfs = vfs_mutex.lock();
        vfs.mkdir(path).map_err(vfs_err_to_fs_err)
    }
}

/// Map a `VfsError` into the `claudio-fs` `FsError` space.
fn vfs_err_to_fs_err(e: VfsError) -> FsError {
    use claudio_vfs::fs_trait::FsError as VfsFsError;
    match e {
        VfsError::Fs(fse) => match fse {
            VfsFsError::NotFound => FsError::NotFound,
            VfsFsError::AlreadyExists => FsError::WriteFailed,
            VfsFsError::PermissionDenied => FsError::WriteFailed,
            VfsFsError::NoSpace => FsError::WriteFailed,
            VfsFsError::Unsupported => FsError::Unsupported,
            _ => FsError::Io,
        },
        VfsError::Mount(_) => FsError::NotMounted,
        VfsError::ReadOnly => FsError::Unsupported,
        VfsError::InvalidPath => FsError::InvalidPath,
        VfsError::BadFd | VfsError::TooManyOpen | VfsError::InvalidOp => FsError::Io,
    }
}
