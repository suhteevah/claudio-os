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
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use spin::{Mutex, Once};

use claudio_vfs::{BlockDevice, MemFs, MountOptions, OpenFlags, Vfs, VfsError};
use claudio_vfs::adapters::Ext4FilesystemAdapter;
use claudio_vfs::device::{parse_gpt, GPT_GUID_LINUX_FS};
use claudio_vfs::fs_trait::Filesystem;
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

// ============================================================================
// Ext4 disk mounting
// ============================================================================

/// Owned adapter that implements `claudio_ext4::BlockDevice` over a boxed
/// VFS `BlockDevice`, with a partition byte offset.
///
/// `claudio_vfs::adapters::VfsToExt4BlockDevice` exists but borrows the
/// underlying device with a lifetime — Ext4Fs<D> needs `D: 'static`, so we
/// can't use it for a long-lived mount. This owned version takes the
/// boxed device and lives for the program lifetime.
struct OwnedExt4BlockDevice {
    device: Box<dyn BlockDevice + Send + Sync>,
    partition_offset: u64,
}

impl claudio_ext4::BlockDevice for OwnedExt4BlockDevice {
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

/// Walk the disk registry, parse GPT on each disk, and mount the first
/// Linux-filesystem partition on each disk as an ext4 filesystem at
/// `/disk<n>p<m>`. Must be called AFTER `crate::disks::init()`.
///
/// Returns the number of partitions successfully mounted.
pub fn mount_disks() -> usize {
    let disk_count = crate::disks::len();
    log::info!("[storage] mount_disks: scanning {} disk(s)", disk_count);

    // Snapshot disk metadata (label + sector size) up front so we don't hold
    // the disks lock while parsing GPT / opening filesystems.
    let snapshots: Vec<(usize, String, u32)> = crate::disks::with_disks(|disks| {
        disks
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.label.clone(), e.sector_size))
            .collect()
    });

    let mut mounted = 0usize;
    for (idx, label, sector_size) in snapshots {
        // Get a fresh block device adapter for GPT parsing.
        let bd = match crate::disks::as_block_device(idx) {
            Some(bd) => bd,
            None => continue,
        };

        let partitions = match parse_gpt(&*bd) {
            Ok(parts) => parts,
            Err(e) => {
                log::warn!(
                    "[storage] disk {} ({}): parse_gpt failed: {:?}",
                    idx, label, e,
                );
                continue;
            }
        };

        if partitions.is_empty() {
            log::info!("[storage] disk {} ({}): no GPT partitions", idx, label);
            continue;
        }

        log::info!(
            "[storage] disk {} ({}): {} partition(s) found",
            idx, label, partitions.len(),
        );

        for part in &partitions {
            if part.type_id != GPT_GUID_LINUX_FS {
                log::debug!(
                    "[storage]   p{}: non-Linux-FS partition, skipping",
                    part.index,
                );
                continue;
            }

            // ── Mount safety allowlist ────────────────────────────────────
            //
            // CRITICAL: ClaudioOS is not a production-ready OS. When it boots
            // on real hardware, `crate::disks::init` enumerates EVERY attached
            // mass-storage controller (AHCI/NVMe/USB). Without this filter,
            // `mount_disks` would happily mount any ext4 partition it finds
            // — including the host OS's root filesystem, a dual-boot Linux
            // install, a neighbour's data disk, etc. That's a footgun the
            // size of a filesystem.
            //
            // Policy: only auto-mount partitions whose GPT partition label
            // contains the substring "claudio" (case-insensitive). To prepare
            // a disk for use with ClaudioOS, label its data partition with
            // something like:
            //     sgdisk -c <N>:'claudio-data' /dev/sdX
            // or the parted equivalent. Any disk without such a label is
            // enumerated (visible via `df`, `lsblk`, etc.) but NOT mounted,
            // so writes through the VFS cannot reach it.
            //
            // This is deliberately coarse and conservative. Override options:
            //   - set a compile-time env `CLAUDIO_MOUNT_ANY=1` (not yet
            //     implemented, add when genuinely needed)
            //   - add a runtime shell command `mount <device> <mount_point>`
            //     that bypasses the allowlist (not yet implemented)
            //
            // Until those exist, the ONLY way to get a partition mounted is
            // to label it with "claudio" in its name.
            let label_lower = part.name.to_lowercase();
            if !label_lower.contains("claudio") {
                log::info!(
                    "[storage]   p{}: skipping — GPT label '{}' is not in the \
                     mount allowlist (must contain 'claudio')",
                    part.index, part.name,
                );
                continue;
            }
            log::info!(
                "[storage]   p{}: label '{}' matches allowlist — attempting ext4 mount",
                part.index, part.name,
            );

            // Get an owned block device adapter for this partition — each
            // ext4 mount consumes its own D, so we can't share with parse_gpt.
            let part_bd = match crate::disks::as_block_device(idx) {
                Some(bd) => bd,
                None => continue,
            };

            let partition_offset = part.start_offset(sector_size);
            let ext4_device = OwnedExt4BlockDevice {
                device: part_bd,
                partition_offset,
            };

            match claudio_ext4::Ext4Fs::mount(ext4_device) {
                Ok(fs) => {
                    let mount_point = format!("/disk{}p{}", idx, part.index);
                    // Box-leak the adapter to get a 'static reference (Vfs::mount
                    // requires `&'static dyn Filesystem`). We never unmount
                    // these in the current kernel, so the leak is acceptable.
                    let adapter: &'static dyn Filesystem = Box::leak(Box::new(
                        Ext4FilesystemAdapter::new(fs),
                    ));

                    let result = with_vfs(|vfs| {
                        let _ = vfs.mkdir(&mount_point);
                        vfs.mount(&mount_point, adapter, MountOptions::default())
                            .map_err(|e| format!("{}", e))?;
                        Ok(())
                    });

                    match result {
                        Ok(()) => {
                            log::info!(
                                "[storage]   p{}: mounted ext4 at {}",
                                part.index, mount_point,
                            );
                            mounted += 1;
                        }
                        Err(e) => {
                            log::warn!(
                                "[storage]   p{}: ext4 mount into VFS failed: {}",
                                part.index, e,
                            );
                        }
                    }
                }
                Err(e) => {
                    log::warn!(
                        "[storage]   p{}: ext4 Ext4Fs::mount failed: {:?}",
                        part.index, e,
                    );
                }
            }
        }
    }

    log::info!(
        "[storage] mount_disks: {} ext4 partition(s) mounted",
        mounted,
    );
    mounted
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
