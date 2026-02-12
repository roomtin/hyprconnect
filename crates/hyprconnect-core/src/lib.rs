use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceState {
    pub id: String,
    pub name: String,
    pub reachable: bool,
    pub paired: bool,
    pub mounted: bool,
    pub mount_point: Option<String>,
    pub battery_percent: Option<u8>,
    pub charging: Option<bool>,
    pub signal_percent: Option<u8>,
    pub network_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DaemonState {
    pub devices: Vec<DeviceState>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub default_device: Option<String>,
    pub poll_interval_seconds: u64,
    pub battery_warn_percent: u8,
    pub battery_crit_percent: u8,
    pub notifications_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_device: None,
            poll_interval_seconds: 10,
            battery_warn_percent: 30,
            battery_crit_percent: 15,
            notifications_enabled: true,
        }
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let cfg_dir = dirs::config_dir().context("unable to resolve XDG config dir")?;
        Ok(cfg_dir.join("hyprconnect").join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("invalid config TOML: {}", path.display()))?;
        Ok(cfg)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcRequest {
    GetState,
    ShareFile {
        path: String,
        device: Option<String>,
    },
    ShareUrl {
        url: String,
        device: Option<String>,
    },
    ShareClipboard {
        device: Option<String>,
    },
    Ping {
        message: Option<String>,
        device: Option<String>,
    },
    Pair {
        device: String,
    },
    Unpair {
        device: String,
    },
    Find {
        device: Option<String>,
    },
    RefreshNetwork,
    Mount {
        device: Option<String>,
    },
    OpenMount {
        device: Option<String>,
    },
    ToggleMount {
        device: Option<String>,
    },
    Media {
        device: Option<String>,
        action: MediaAction,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MediaAction {
    Status,
    PlayPause,
    Next,
    Previous,
    Stop,
    Seek { ms: i32 },
    VolumeSet { value: u8 },
    PlayerList,
    PlayerSet { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    pub message: Option<String>,
    pub state: Option<DaemonState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaybarPayload {
    pub text: String,
    pub tooltip: String,
    pub class: String,
}

pub fn runtime_socket_path() -> Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    Ok(PathBuf::from(runtime_dir).join("hyprconnect.sock"))
}
