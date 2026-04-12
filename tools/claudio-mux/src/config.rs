use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub keybindings: KeybindingsConfig,
    pub status_bar: StatusBarConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub shell: String,
    pub shell_args: Vec<String>,
    pub agent: String,
    pub agent_args: Vec<String>,
    pub require_windows_terminal: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct KeybindingsConfig {
    pub prefix: String,
    pub bindings: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct StatusBarConfig {
    pub enabled: bool,
    pub left: String,
    pub right: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            keybindings: KeybindingsConfig::default(),
            status_bar: StatusBarConfig::default(),
        }
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            shell: if which_shell("pwsh.exe") { "pwsh.exe" } else { "cmd.exe" }.into(),
            shell_args: vec![],
            agent: "claude".into(),
            agent_args: vec![],
            require_windows_terminal: true,
        }
    }
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            prefix: "Ctrl+b".into(),
            bindings: HashMap::new(),
        }
    }
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            left: " {session} \u{2502} panes:{pane_count} ".into(),
            right: " {focus} \u{2502} {time} ".into(),
        }
    }
}

pub fn load_config() -> anyhow::Result<Config> {
    let config_dir = config_dir();
    let config_path = config_dir.join("config.toml");
    if config_path.exists() {
        let text = std::fs::read_to_string(&config_path)?;
        let config: Config = toml::from_str(&text)?;
        Ok(config)
    } else {
        Ok(Config::default())
    }
}

pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("", "ridge-cell", "claudio-mux")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn log_dir() -> PathBuf {
    directories::ProjectDirs::from("", "ridge-cell", "claudio-mux")
        .map(|d| d.data_local_dir().join("logs"))
        .unwrap_or_else(|| PathBuf::from("logs"))
}

fn which_shell(name: &str) -> bool {
    std::process::Command::new("where")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
