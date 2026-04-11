//! Init system — configuration persistence and boot-time initialization.
//!
//! Loads configuration from QEMU fw_cfg (`opt/claudio/config`) or provides
//! sensible defaults. The config uses a simple `key=value` format similar to
//! `/etc/claudio.conf`.
//!
//! ## Config format
//!
//! ```text
//! hostname=claudio
//! log_level=info
//! auto_start_agents=1
//! default_model=claude-sonnet-4-6
//! ssh_enabled=true
//! ssh_port=22
//! auto_login=true
//! auto_mount=/dev/vda1:/mnt/data:fat32
//! startup_script=/etc/claudio/startup.sh
//! ```

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Log level enum
// ---------------------------------------------------------------------------

/// Supported log levels, mapping to the `log` crate's `LevelFilter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// Parse from a string (case-insensitive).
    pub fn from_str(s: &str) -> Option<LogLevel> {
        match s.to_ascii_lowercase().as_str() {
            "trace" => Some(LogLevel::Trace),
            "debug" => Some(LogLevel::Debug),
            "info" => Some(LogLevel::Info),
            "warn" | "warning" => Some(LogLevel::Warn),
            "error" => Some(LogLevel::Error),
            _ => None,
        }
    }

    /// Convert to `log::LevelFilter`.
    pub fn to_level_filter(self) -> log::LevelFilter {
        match self {
            LogLevel::Trace => log::LevelFilter::Trace,
            LogLevel::Debug => log::LevelFilter::Debug,
            LogLevel::Info => log::LevelFilter::Info,
            LogLevel::Warn => log::LevelFilter::Warn,
            LogLevel::Error => log::LevelFilter::Error,
        }
    }

    /// Serialize back to string.
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

// Implement lowercase conversion helper since we're no_std
trait AsciiLowerExt {
    fn to_ascii_lowercase(&self) -> String;
}

impl AsciiLowerExt for str {
    fn to_ascii_lowercase(&self) -> String {
        let mut s = String::with_capacity(self.len());
        for c in self.chars() {
            if c.is_ascii_uppercase() {
                s.push((c as u8 + 32) as char);
            } else {
                s.push(c);
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Mount entry
// ---------------------------------------------------------------------------

/// A filesystem mount entry: (device, mount_path, fstype).
#[derive(Debug, Clone)]
pub struct MountEntry {
    pub device: String,
    pub path: String,
    pub fstype: String,
}

impl MountEntry {
    /// Parse from `device:path:fstype` format.
    pub fn from_str(s: &str) -> Option<MountEntry> {
        let parts: Vec<&str> = s.splitn(3, ':').collect();
        if parts.len() == 3 {
            Some(MountEntry {
                device: String::from(parts[0]),
                path: String::from(parts[1]),
                fstype: String::from(parts[2]),
            })
        } else {
            None
        }
    }

    /// Serialize back to `device:path:fstype`.
    pub fn to_string(&self) -> String {
        alloc::format!("{}:{}:{}", self.device, self.path, self.fstype)
    }
}

// ---------------------------------------------------------------------------
// InitConfig
// ---------------------------------------------------------------------------

/// System configuration loaded at boot time.
#[derive(Debug, Clone)]
pub struct InitConfig {
    /// Number of agent panes to create at boot (0 = none).
    pub auto_start_agents: u8,
    /// Default Claude model for new agent sessions.
    pub default_model: String,
    /// Filesystem mounts to perform at boot.
    pub auto_mount: Vec<MountEntry>,
    /// Whether SSH server is enabled.
    pub ssh_enabled: bool,
    /// SSH listen port.
    pub ssh_port: u16,
    /// System hostname.
    pub hostname: String,
    /// Logging verbosity.
    pub log_level: LogLevel,
    /// Skip auth if a valid session cookie exists.
    pub auto_login: bool,
    /// Path to a shell script to run at boot.
    pub startup_script: Option<String>,
}

impl InitConfig {
    /// Create a default configuration — sensible out-of-the-box.
    pub fn default() -> Self {
        InitConfig {
            auto_start_agents: 1,
            default_model: String::from("claude-sonnet-4-6"),
            auto_mount: Vec::new(),
            ssh_enabled: true,
            ssh_port: 22,
            hostname: String::from("claudio"),
            log_level: LogLevel::Info,
            auto_login: true,
            startup_script: None,
        }
    }

    /// Parse configuration from a `key=value` text blob.
    ///
    /// Lines starting with `#` are comments. Empty lines are ignored.
    /// Unknown keys are logged and skipped.
    pub fn parse(text: &str) -> Self {
        let mut config = Self::default();

        for line in text.lines() {
            let line = line.trim();
            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Split on first '='
            let (key, value) = match line.find('=') {
                Some(pos) => (line[..pos].trim(), line[pos + 1..].trim()),
                None => {
                    log::warn!("[init] ignoring malformed config line: {}", line);
                    continue;
                }
            };

            match key {
                "auto_start_agents" => {
                    if let Ok(n) = value.parse::<u8>() {
                        config.auto_start_agents = n;
                    } else {
                        log::warn!("[init] invalid auto_start_agents: {}", value);
                    }
                }
                "default_model" => {
                    config.default_model = String::from(value);
                }
                "auto_mount" => {
                    if let Some(entry) = MountEntry::from_str(value) {
                        config.auto_mount.push(entry);
                    } else {
                        log::warn!("[init] invalid auto_mount (expected device:path:fstype): {}", value);
                    }
                }
                "ssh_enabled" => {
                    config.ssh_enabled = value == "true" || value == "1" || value == "yes";
                }
                "ssh_port" => {
                    if let Ok(p) = value.parse::<u16>() {
                        config.ssh_port = p;
                    } else {
                        log::warn!("[init] invalid ssh_port: {}", value);
                    }
                }
                "hostname" => {
                    config.hostname = String::from(value);
                }
                "log_level" => {
                    if let Some(level) = LogLevel::from_str(value) {
                        config.log_level = level;
                    } else {
                        log::warn!("[init] invalid log_level: {}", value);
                    }
                }
                "auto_login" => {
                    config.auto_login = value == "true" || value == "1" || value == "yes";
                }
                "startup_script" => {
                    if !value.is_empty() {
                        config.startup_script = Some(String::from(value));
                    }
                }
                _ => {
                    log::warn!("[init] unknown config key: {}", key);
                }
            }
        }

        config
    }

    /// Serialize the config back to `key=value` format.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str("# ClaudioOS configuration\n");
        out.push_str("# Generated by init system\n\n");

        out.push_str(&alloc::format!("hostname={}\n", self.hostname));
        out.push_str(&alloc::format!("log_level={}\n", self.log_level.as_str()));
        out.push_str(&alloc::format!("auto_start_agents={}\n", self.auto_start_agents));
        out.push_str(&alloc::format!("default_model={}\n", self.default_model));
        out.push_str(&alloc::format!("ssh_enabled={}\n", if self.ssh_enabled { "true" } else { "false" }));
        out.push_str(&alloc::format!("ssh_port={}\n", self.ssh_port));
        out.push_str(&alloc::format!("auto_login={}\n", if self.auto_login { "true" } else { "false" }));

        for mount in &self.auto_mount {
            out.push_str(&alloc::format!("auto_mount={}\n", mount.to_string()));
        }

        if let Some(ref script) = self.startup_script {
            out.push_str(&alloc::format!("startup_script={}\n", script));
        }

        out
    }
}

// ---------------------------------------------------------------------------
// fw_cfg helpers
// ---------------------------------------------------------------------------

/// Read a file from QEMU fw_cfg by name. Returns `None` if not found.
///
/// # Safety
/// Accesses I/O ports 0x510 (selector) and 0x511 (data).
unsafe fn fwcfg_read_file(name: &str) -> Option<Vec<u8>> {
    unsafe {
        let mut sel = x86_64::instructions::port::Port::<u16>::new(0x510);
        let mut data = x86_64::instructions::port::Port::<u8>::new(0x511);

        // Read file directory (selector 0x0019)
        sel.write(0x0019);

        // First 4 bytes = file count (big-endian)
        let count = ((data.read() as u32) << 24)
            | ((data.read() as u32) << 16)
            | ((data.read() as u32) << 8)
            | (data.read() as u32);

        let mut found_selector: Option<u16> = None;
        let mut found_size: u32 = 0;

        for _ in 0..count.min(128) {
            // Each entry: 4 bytes size, 2 bytes select, 2 bytes reserved, 56 bytes name
            let size = ((data.read() as u32) << 24)
                | ((data.read() as u32) << 16)
                | ((data.read() as u32) << 8)
                | (data.read() as u32);
            let select = ((data.read() as u16) << 8) | (data.read() as u16);
            let _reserved = ((data.read() as u16) << 8) | (data.read() as u16);
            let mut name_buf = [0u8; 56];
            for b in name_buf.iter_mut() {
                *b = data.read();
            }
            let name_len = name_buf.iter().position(|&b| b == 0).unwrap_or(56);
            let entry_name = core::str::from_utf8(&name_buf[..name_len]).unwrap_or("");

            if entry_name == name {
                found_selector = Some(select);
                found_size = size;
                break;
            }
        }

        if let Some(sel_val) = found_selector {
            sel.write(sel_val);
            let mut buf = Vec::with_capacity(found_size as usize);
            for _ in 0..found_size {
                buf.push(data.read());
            }
            Some(buf)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load the system configuration.
///
/// Tries fw_cfg (`opt/claudio/config`) first. Falls back to defaults.
pub fn load_config() -> InitConfig {
    log::info!("[init] loading system configuration...");

    // Try fw_cfg
    let config = unsafe {
        match fwcfg_read_file("opt/claudio/config") {
            Some(data) => {
                if let Ok(text) = core::str::from_utf8(&data) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        log::info!("[init] loaded config from fw_cfg ({} bytes)", data.len());
                        InitConfig::parse(trimmed)
                    } else {
                        log::info!("[init] fw_cfg config empty, using defaults");
                        InitConfig::default()
                    }
                } else {
                    log::warn!("[init] fw_cfg config not valid UTF-8, using defaults");
                    InitConfig::default()
                }
            }
            None => {
                log::info!("[init] no fw_cfg config found, using defaults");
                InitConfig::default()
            }
        }
    };

    log::info!("[init] hostname={}", config.hostname);
    log::info!("[init] log_level={}", config.log_level.as_str());
    log::info!("[init] auto_start_agents={}", config.auto_start_agents);
    log::info!("[init] default_model={}", config.default_model);
    log::info!("[init] ssh_enabled={} port={}", config.ssh_enabled, config.ssh_port);
    log::info!("[init] auto_login={}", config.auto_login);
    log::info!("[init] auto_mount entries: {}", config.auto_mount.len());
    if let Some(ref script) = config.startup_script {
        log::info!("[init] startup_script={}", script);
    }

    config
}

/// Apply the loaded configuration to the running system.
///
/// This executes the init sequence in order:
/// 1. Set hostname (stored globally for SSH banner, logs, etc.)
/// 2. Set log level
/// 3. Mount filesystems
/// 4. Start SSH if enabled (caller handles actual SSH start)
/// 5. Run startup script (if specified)
///
/// Returns the config for the caller to use (e.g. auto_start_agents count).
pub fn apply_config(config: &InitConfig) {
    log::info!("[init] applying configuration...");

    // 1. Set hostname
    set_hostname(&config.hostname);
    log::info!("[init] hostname set to '{}'", config.hostname);

    // 2. Set log level
    log::set_max_level(config.log_level.to_level_filter());
    log::info!("[init] log level set to {}", config.log_level.as_str());

    // 3. Mount filesystems
    for mount in &config.auto_mount {
        log::info!("[init] mounting {} -> {} ({})", mount.device, mount.path, mount.fstype);
        // Filesystem mounting is handled by vfs/fs-persist crates.
        // For now, log the intent — actual mount requires block device access.
        mount_filesystem(mount);
    }

    // 4. SSH — caller checks config.ssh_enabled and starts the server.
    if config.ssh_enabled {
        log::info!("[init] SSH server will start on port {}", config.ssh_port);
    } else {
        log::info!("[init] SSH server disabled by config");
    }

    // 5. Startup script
    if let Some(ref script) = config.startup_script {
        log::info!("[init] running startup script: {}", script);
        run_startup_script(script);
    }

    log::info!("[init] configuration applied successfully");
}

/// Save the config — serialize to key=value and write to fw_cfg marker
/// so the host can capture it.
pub fn save_config(config: &InitConfig) {
    let serialized = config.serialize();
    log::info!("[init] SAVE_CONFIG:{}", serialized.len());

    // Print each line to serial as a marker for the host to capture.
    // Format: [init] CONFIG_LINE:key=value
    for line in serialized.lines() {
        if !line.is_empty() && !line.starts_with('#') {
            log::info!("[init] CONFIG_LINE:{}", line);
        }
    }

    log::info!("[init] config saved ({} bytes)", serialized.len());
}

// ---------------------------------------------------------------------------
// Hostname storage
// ---------------------------------------------------------------------------

/// Global hostname — set by init, read by SSH banner and log prefixes.
static HOSTNAME: spin::Mutex<Option<String>> = spin::Mutex::new(None);

/// Set the system hostname.
fn set_hostname(name: &str) {
    let mut lock = HOSTNAME.lock();
    *lock = Some(String::from(name));
}

/// Get the current system hostname.
pub fn hostname() -> String {
    let lock = HOSTNAME.lock();
    lock.as_ref()
        .map(|s| s.clone())
        .unwrap_or_else(|| String::from("claudio"))
}

// ---------------------------------------------------------------------------
// Filesystem mounting
// ---------------------------------------------------------------------------

/// Legacy auto-mount entry point.
///
/// Historically this tried to walk a PCI-enumerated block device list and
/// mount real on-disk filesystems here. Since Phase 2b of kernel boot,
/// the root VFS (a MemFs at `/`) is initialized directly in
/// `crate::storage::init()` during `main.rs` startup, and additional mounts
/// are expected to be driven by the storage subsystem on real hardware. This
/// function now only logs the intent so that config-driven auto_mount entries
/// remain visible in the boot log.
fn mount_filesystem(entry: &MountEntry) {
    log::info!(
        "[init] VFS mounted by kernel boot phase 2b (storage::init); \
         auto_mount entry {} -> {} ({}) deferred to storage subsystem",
        entry.device, entry.path, entry.fstype
    );
}

// ---------------------------------------------------------------------------
// Startup script
// ---------------------------------------------------------------------------

/// Run a startup script. Reads the script from the VFS via `claudio_fs` and
/// logs its contents line-by-line. Actual shell execution is not yet wired —
/// when the shell crate is integrated, this will hand the lines off to it.
fn run_startup_script(path: &str) {
    log::info!("[init] reading startup script '{}' from VFS", path);
    match claudio_fs::read_file(path) {
        Ok(bytes) => {
            match core::str::from_utf8(&bytes) {
                Ok(text) => {
                    log::info!(
                        "[init] startup script '{}' loaded ({} bytes)",
                        path,
                        bytes.len()
                    );
                    for (i, line) in text.lines().enumerate() {
                        log::info!("[init] startup[{}]: {}", i + 1, line);
                    }
                    log::info!(
                        "[init] startup script '{}' — shell execution not yet wired",
                        path
                    );
                }
                Err(e) => {
                    log::warn!(
                        "[init] startup script '{}' is not valid UTF-8: {}",
                        path, e
                    );
                }
            }
        }
        Err(e) => {
            log::warn!("[init] startup script '{}' could not be read: {}", path, e);
        }
    }
}
