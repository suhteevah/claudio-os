//! In-memory filesystem (`MemFs`).
//!
//! A pure-RAM implementation of the [`Filesystem`] trait. Used as the initial
//! root filesystem (`/`) so ClaudioOS can boot and agents can read/write files
//! without any block device being available yet. Disk-backed filesystems like
//! ext4-on-AHCI are expected to be mounted later at paths like `/disk`.
//!
//! Internally the tree is stored as a flat `BTreeMap<String, FsNode>` keyed by
//! absolute paths (e.g. `"/"`, `"/claudio"`, `"/claudio/test.txt"`). This keeps
//! lookup, readdir, and rename operations straightforward without requiring a
//! linked tree of inodes.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use spin::Mutex;

use crate::dir::DirEntry;
use crate::file::{FileInfo, FileType};
use crate::fs_trait::{Filesystem, FsError, FsType};

/// A node in the in-memory filesystem tree.
enum FsNode {
    /// A regular file containing the given byte contents.
    File(Vec<u8>),
    /// A directory. Contents are discovered by scanning keys in the outer map.
    Dir,
}

/// Interior mutable state, guarded by a [`spin::Mutex`].
struct Inner {
    nodes: BTreeMap<String, FsNode>,
}

/// Pure in-memory filesystem implementation.
pub struct MemFs {
    inner: Mutex<Inner>,
}

impl MemFs {
    /// Create a new, empty in-memory filesystem with just the root directory.
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert("/".to_string(), FsNode::Dir);
        log::debug!("memfs: initialized new in-memory filesystem");
        Self {
            inner: Mutex::new(Inner { nodes }),
        }
    }
}

impl Default for MemFs {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalize a path: ensure it starts with `/`, strip any trailing slash
/// except for the root itself.
fn normalize(path: &str) -> Result<String, FsError> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(FsError::NotFound);
    }
    if path == "/" {
        return Ok("/".to_string());
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        Ok("/".to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

/// Return the parent directory path for the given normalized path.
/// For `"/foo"` returns `"/"`. For `"/"` returns `None`.
fn parent_of(path: &str) -> Option<String> {
    if path == "/" {
        return None;
    }
    match path.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(idx) => Some(path[..idx].to_string()),
        None => None,
    }
}

/// Extract the basename (last path component) from a normalized path.
fn basename_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[idx + 1..],
        None => path,
    }
}

impl Inner {
    /// Check if `path` is a direct child of `parent` (same depth, one level
    /// below). Both must be normalized.
    fn is_direct_child(parent: &str, candidate: &str) -> bool {
        if parent == candidate {
            return false;
        }
        let prefix = if parent == "/" {
            "/".to_string()
        } else {
            format!("{}/", parent)
        };
        if !candidate.starts_with(&prefix) {
            return false;
        }
        let rest = &candidate[prefix.len()..];
        !rest.is_empty() && !rest.contains('/')
    }

    /// Require that a parent directory exists for the given path (i.e. the
    /// path itself — we check path's parent).
    fn require_parent_dir(&self, path: &str) -> Result<(), FsError> {
        let parent = match parent_of(path) {
            Some(p) => p,
            None => return Ok(()), // root has no parent
        };
        match self.nodes.get(&parent) {
            Some(FsNode::Dir) => Ok(()),
            Some(FsNode::File(_)) => Err(FsError::WrongType),
            None => Err(FsError::NotFound),
        }
    }
}

impl Filesystem for MemFs {
    fn fs_type(&self) -> FsType {
        FsType::Unknown
    }

    fn label(&self) -> Option<&str> {
        Some("memfs")
    }

    fn read_file(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let path = normalize(path)?;
        let inner = self.inner.lock();
        match inner.nodes.get(&path) {
            Some(FsNode::File(data)) => {
                let offset = offset as usize;
                if offset >= data.len() {
                    return Ok(0);
                }
                let available = data.len() - offset;
                let n = core::cmp::min(buf.len(), available);
                buf[..n].copy_from_slice(&data[offset..offset + n]);
                log::trace!("memfs: read {} bytes from {} @ {}", n, path, offset);
                Ok(n)
            }
            Some(FsNode::Dir) => Err(FsError::WrongType),
            None => Err(FsError::NotFound),
        }
    }

    fn write_file(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let path = normalize(path)?;
        let mut inner = self.inner.lock();
        match inner.nodes.get_mut(&path) {
            Some(FsNode::File(contents)) => {
                let offset = offset as usize;
                let end = offset + data.len();
                if contents.len() < end {
                    contents.resize(end, 0);
                }
                contents[offset..end].copy_from_slice(data);
                log::trace!("memfs: wrote {} bytes to {} @ {}", data.len(), path, offset);
                Ok(data.len())
            }
            Some(FsNode::Dir) => Err(FsError::WrongType),
            None => Err(FsError::NotFound),
        }
    }

    fn create_file(&self, path: &str) -> Result<(), FsError> {
        let path = normalize(path)?;
        if path == "/" {
            return Err(FsError::AlreadyExists);
        }
        let mut inner = self.inner.lock();
        if inner.nodes.contains_key(&path) {
            return Err(FsError::AlreadyExists);
        }
        inner.require_parent_dir(&path)?;
        inner.nodes.insert(path.clone(), FsNode::File(Vec::new()));
        log::debug!("memfs: created file {}", path);
        Ok(())
    }

    fn delete_file(&self, path: &str) -> Result<(), FsError> {
        let path = normalize(path)?;
        let mut inner = self.inner.lock();
        match inner.nodes.get(&path) {
            Some(FsNode::File(_)) => {
                inner.nodes.remove(&path);
                log::debug!("memfs: deleted file {}", path);
                Ok(())
            }
            Some(FsNode::Dir) => Err(FsError::WrongType),
            None => Err(FsError::NotFound),
        }
    }

    fn mkdir(&self, path: &str) -> Result<(), FsError> {
        let path = normalize(path)?;
        if path == "/" {
            return Err(FsError::AlreadyExists);
        }
        let mut inner = self.inner.lock();
        if inner.nodes.contains_key(&path) {
            return Err(FsError::AlreadyExists);
        }
        inner.require_parent_dir(&path)?;
        inner.nodes.insert(path.clone(), FsNode::Dir);
        log::debug!("memfs: created directory {}", path);
        Ok(())
    }

    fn rmdir(&self, path: &str) -> Result<(), FsError> {
        let path = normalize(path)?;
        if path == "/" {
            return Err(FsError::PermissionDenied);
        }
        let mut inner = self.inner.lock();
        match inner.nodes.get(&path) {
            Some(FsNode::Dir) => {
                // Check emptiness: no direct children exist.
                let has_children = inner
                    .nodes
                    .keys()
                    .any(|k| Inner::is_direct_child(&path, k));
                if has_children {
                    return Err(FsError::NotEmpty);
                }
                inner.nodes.remove(&path);
                log::debug!("memfs: removed directory {}", path);
                Ok(())
            }
            Some(FsNode::File(_)) => Err(FsError::WrongType),
            None => Err(FsError::NotFound),
        }
    }

    fn stat(&self, path: &str) -> Result<FileInfo, FsError> {
        let path = normalize(path)?;
        let inner = self.inner.lock();
        match inner.nodes.get(&path) {
            Some(FsNode::File(data)) => Ok(FileInfo::simple_file(data.len() as u64)),
            Some(FsNode::Dir) => Ok(FileInfo::simple_dir()),
            None => Err(FsError::NotFound),
        }
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let path = normalize(path)?;
        let inner = self.inner.lock();
        match inner.nodes.get(&path) {
            Some(FsNode::Dir) => {
                let mut out = Vec::new();
                for (key, node) in inner.nodes.iter() {
                    if !Inner::is_direct_child(&path, key) {
                        continue;
                    }
                    let name = basename_of(key);
                    let entry = match node {
                        FsNode::File(data) => {
                            DirEntry::new(name, FileType::File, data.len() as u64)
                        }
                        FsNode::Dir => DirEntry::new(name, FileType::Directory, 0),
                    };
                    out.push(entry);
                }
                Ok(out)
            }
            Some(FsNode::File(_)) => Err(FsError::WrongType),
            None => Err(FsError::NotFound),
        }
    }

    fn rename(&self, old: &str, new: &str) -> Result<(), FsError> {
        let old = normalize(old)?;
        let new = normalize(new)?;
        if old == "/" {
            return Err(FsError::PermissionDenied);
        }
        if old == new {
            return Ok(());
        }
        let mut inner = self.inner.lock();
        if !inner.nodes.contains_key(&old) {
            return Err(FsError::NotFound);
        }
        if inner.nodes.contains_key(&new) {
            return Err(FsError::AlreadyExists);
        }
        inner.require_parent_dir(&new)?;

        // Disallow moving a directory into itself or a descendant.
        let old_prefix = if old == "/" {
            "/".to_string()
        } else {
            format!("{}/", old)
        };
        if new == old || new.starts_with(&old_prefix) {
            return Err(FsError::PermissionDenied);
        }

        // Collect all keys to move (the entry itself plus any descendants
        // if it's a directory).
        let keys_to_move: Vec<String> = inner
            .nodes
            .keys()
            .filter(|k| k.as_str() == old || k.starts_with(&old_prefix))
            .cloned()
            .collect();

        for key in keys_to_move {
            let node = inner.nodes.remove(&key).unwrap();
            let new_key = if key == old {
                new.clone()
            } else {
                format!("{}{}", new, &key[old.len()..])
            };
            inner.nodes.insert(new_key, node);
        }
        log::debug!("memfs: renamed {} -> {}", old, new);
        Ok(())
    }

    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        let path = normalize(path)?;
        let mut inner = self.inner.lock();
        match inner.nodes.get_mut(&path) {
            Some(FsNode::File(data)) => {
                data.resize(size as usize, 0);
                log::trace!("memfs: truncated {} to {} bytes", path, size);
                Ok(())
            }
            Some(FsNode::Dir) => Err(FsError::WrongType),
            None => Err(FsError::NotFound),
        }
    }

    fn sync(&self) -> Result<(), FsError> {
        // Nothing to flush — everything is already in RAM.
        Ok(())
    }
}
