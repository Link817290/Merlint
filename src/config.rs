use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Persistent configuration stored at ~/.merlint/config.toml
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MerlintConfig {
    pub proxy: ProxyDefaults,
    pub monitor: MonitorDefaults,
    pub daemon: DaemonDefaults,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyDefaults {
    pub port: u16,
    pub target_url: String,
    pub optimize: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorDefaults {
    pub interval: u64,
    pub auto_optimize: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonDefaults {
    pub interval: u64,
    pub report: bool,
}

impl Default for ProxyDefaults {
    fn default() -> Self {
        Self {
            port: 8080,
            target_url: "https://api.openai.com".into(),
            optimize: false,
        }
    }
}

impl Default for MonitorDefaults {
    fn default() -> Self {
        Self {
            interval: 30,
            auto_optimize: true,
        }
    }
}

impl Default for DaemonDefaults {
    fn default() -> Self {
        Self {
            interval: 3600,
            report: false,
        }
    }
}

fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".merlint")
}

fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Load config from ~/.merlint/config.toml, falling back to defaults if not found.
pub fn load_config() -> MerlintConfig {
    let path = config_path();
    if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str::<MerlintConfig>(&content) {
                Ok(cfg) => return cfg,
                Err(e) => {
                    eprintln!("Warning: failed to parse {}: {}", path.display(), e);
                }
            },
            Err(e) => {
                eprintln!("Warning: failed to read {}: {}", path.display(), e);
            }
        }
    }
    MerlintConfig::default()
}

/// Save config to ~/.merlint/config.toml
pub fn save_config(config: &MerlintConfig) -> anyhow::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let content = toml::to_string_pretty(config)?;
    std::fs::write(config_path(), content)?;
    Ok(())
}

/// Initialize config file with defaults if it doesn't exist.
/// Returns true if a new file was created.
pub fn init_config() -> anyhow::Result<bool> {
    let path = config_path();
    if path.exists() {
        return Ok(false);
    }
    save_config(&MerlintConfig::default())?;
    Ok(true)
}
