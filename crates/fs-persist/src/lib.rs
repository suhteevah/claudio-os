//! Filesystem persistence: credentials, config, agent state, logs.
//!
//! Partition layout: /claudio/{config.json, credentials.json, agents/, logs/}
//!
//! This crate is backend-agnostic. The kernel (which owns the VFS singleton)
//! installs a concrete [`FsBackend`] at boot via [`set_backend`]; all
//! reads/writes from this crate are routed through that backend.

#![no_std]
extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum FsError {
    /// No filesystem backend has been installed yet.
    NotMounted,
    /// Path does not exist.
    NotFound,
    /// Write failed (I/O error, disk full, etc.).
    WriteFailed,
    /// File contents could not be parsed.
    CorruptedData,
    /// Path was not a valid absolute path.
    InvalidPath,
    /// Backend-reported I/O error.
    Io,
    /// Operation not supported by the backend.
    Unsupported,
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsError::NotMounted => write!(f, "no filesystem backend installed"),
            FsError::NotFound => write!(f, "file or directory not found"),
            FsError::WriteFailed => write!(f, "write failed"),
            FsError::CorruptedData => write!(f, "corrupted data"),
            FsError::InvalidPath => write!(f, "invalid path"),
            FsError::Io => write!(f, "i/o error"),
            FsError::Unsupported => write!(f, "operation not supported by backend"),
        }
    }
}

// ---------------------------------------------------------------------------
// Backend trait
// ---------------------------------------------------------------------------

/// Pluggable filesystem backend. The kernel implements this by delegating to
/// its global VFS singleton; test harnesses can implement it with an in-memory
/// map. All paths are absolute (starting with `/`).
pub trait FsBackend: Send + Sync {
    fn read_file(&self, path: &str) -> Result<Vec<u8>, FsError>;
    fn write_file(&self, path: &str, data: &[u8]) -> Result<(), FsError>;
    fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError>;
    fn mkdir(&self, path: &str) -> Result<(), FsError>;
}

static FS_BACKEND: spin::Once<&'static dyn FsBackend> = spin::Once::new();

/// Install the global filesystem backend. First caller wins; subsequent calls
/// are silently ignored (this is intentional — there is exactly one VFS).
pub fn set_backend(backend: &'static dyn FsBackend) {
    FS_BACKEND.call_once(|| backend);
    log::info!("[fs] backend installed");
}

fn backend() -> Result<&'static dyn FsBackend, FsError> {
    FS_BACKEND.get().copied().ok_or(FsError::NotMounted)
}

// ---------------------------------------------------------------------------
// Generic file operations (thin wrappers around the backend)
// ---------------------------------------------------------------------------

pub fn read_file(path: &str) -> Result<Vec<u8>, FsError> {
    log::debug!("[fs] read_file {}", path);
    backend()?.read_file(path)
}

pub fn write_file(path: &str, data: &[u8]) -> Result<(), FsError> {
    log::debug!("[fs] write_file {} ({} bytes)", path, data.len());
    backend()?.write_file(path, data)
}

pub fn list_dir(path: &str) -> Result<Vec<String>, FsError> {
    log::debug!("[fs] list_dir {}", path);
    backend()?.list_dir(path)
}

pub fn mkdir(path: &str) -> Result<(), FsError> {
    log::debug!("[fs] mkdir {}", path);
    backend()?.mkdir(path)
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

const CONFIG_PATH: &str = "/claudio/config.json";
const CREDENTIALS_PATH: &str = "/claudio/credentials.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub log_level: String,
    pub default_model: String,
    pub max_agents: usize,
    pub auto_start_agents: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            log_level: String::from("info"),
            default_model: String::from("claude-sonnet-4-20250514"),
            max_agents: 8,
            auto_start_agents: 1,
        }
    }
}

pub fn read_config() -> Result<Config, FsError> {
    log::debug!("[fs] reading config from {}", CONFIG_PATH);
    let data = backend()?.read_file(CONFIG_PATH)?;
    serde_json::from_slice::<Config>(&data).map_err(|e| {
        log::error!("[fs] failed to parse config: {}", e);
        FsError::CorruptedData
    })
}

pub fn write_config(config: &Config) -> Result<(), FsError> {
    log::debug!("[fs] writing config to {}", CONFIG_PATH);
    let data = serde_json::to_vec(config).map_err(|e| {
        log::error!("[fs] failed to serialize config: {}", e);
        FsError::WriteFailed
    })?;
    backend()?.write_file(CONFIG_PATH, &data)
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

pub fn read_credentials() -> Option<claudio_auth::Credentials> {
    log::debug!("[fs] reading credentials from {}", CREDENTIALS_PATH);
    let data = match backend() {
        Ok(b) => match b.read_file(CREDENTIALS_PATH) {
            Ok(d) => d,
            Err(e) => {
                log::debug!("[fs] credentials not available: {}", e);
                return None;
            }
        },
        Err(e) => {
            log::debug!("[fs] credentials read skipped: {}", e);
            return None;
        }
    };
    match claudio_auth::credentials_from_json(&data) {
        Ok(c) => {
            log::info!("[fs] credentials loaded from persist");
            Some(c)
        }
        Err(_) => {
            log::warn!("[fs] credentials file present but failed to parse");
            None
        }
    }
}

pub fn write_credentials(creds: &claudio_auth::Credentials) -> Result<(), FsError> {
    log::debug!("[fs] writing credentials to {}", CREDENTIALS_PATH);
    let data = claudio_auth::credentials_to_json(creds);
    if data.is_empty() {
        log::error!("[fs] credentials serialized to empty buffer — refusing to write");
        return Err(FsError::WriteFailed);
    }
    // Best-effort ensure the parent directory exists.
    let _ = backend()?.mkdir("/claudio");
    backend()?.write_file(CREDENTIALS_PATH, &data)
}
