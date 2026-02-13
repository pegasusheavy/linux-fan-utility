// Copyright (c) 2026 Pegasus Heavy Industries LLC
// Licensed under the MIT License

//! Configuration file handling.
//!
//! Persists fan assignments and curve definitions to TOML.
//! Default path: `/etc/fanctl/config.toml`

use crate::curve::{self, FanCurve};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Default config file location.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/fanctl/config.toml";

/// Default daemon socket path.
pub const DEFAULT_SOCKET_PATH: &str = "/run/fanctl.sock";

/// Default poll interval in milliseconds.
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 2000;

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// Top-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Daemon settings.
    #[serde(default)]
    pub daemon: DaemonConfig,

    /// Named fan curves.
    #[serde(default)]
    pub curves: Vec<FanCurve>,

    /// Per-fan assignments, keyed by fan id (e.g. "hwmon3/pwm1").
    #[serde(default)]
    pub fans: HashMap<String, FanAssignment>,
}

/// Daemon-specific settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Poll interval for the curve engine, in milliseconds.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,

    /// Path for the Unix domain socket.
    #[serde(default = "default_socket_path")]
    pub socket_path: String,

    /// Whether to restore fans to automatic on daemon exit.
    #[serde(default = "default_true")]
    pub restore_on_exit: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            socket_path: DEFAULT_SOCKET_PATH.to_string(),
            restore_on_exit: true,
        }
    }
}

/// How a fan should be controlled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode")]
pub enum FanAssignment {
    /// Automatic (BIOS) control -- daemon doesn't touch this fan.
    #[serde(rename = "auto")]
    Auto,

    /// Fixed manual PWM value.
    #[serde(rename = "manual")]
    Manual {
        /// PWM duty 0-255
        pwm: u8,
    },

    /// Controlled by a named curve tracking a specific temp sensor.
    #[serde(rename = "curve")]
    Curve {
        /// Name of the curve (must match a curve in `Config::curves`)
        curve_name: String,
        /// Id of the temp sensor to read (e.g. "hwmon3/temp1")
        temp_sensor_id: String,
    },
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig::default(),
            curves: vec![
                curve::default_silent_curve(),
                curve::default_performance_curve(),
            ],
            fans: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Load / Save
// ---------------------------------------------------------------------------

/// Load config from a TOML file, or return the default if the file doesn't exist.
pub fn load_config(path: &Path) -> io::Result<Config> {
    if !path.exists() {
        log::info!("No config file at {}, using defaults", path.display());
        return Ok(Config::default());
    }

    let contents = fs::read_to_string(path)?;
    let config: Config = toml::from_str(&contents).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to parse config: {e}"),
        )
    })?;

    log::info!("Loaded config from {}", path.display());
    Ok(config)
}

/// Save config to a TOML file, creating parent directories if needed.
pub fn save_config(path: &Path, config: &Config) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let contents = toml::to_string_pretty(config).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to serialize config: {e}"),
        )
    })?;

    fs::write(path, contents)?;
    log::info!("Saved config to {}", path.display());
    Ok(())
}

/// Resolve the config file path from CLI arg or default.
pub fn resolve_config_path(cli_path: Option<&str>) -> PathBuf {
    cli_path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_poll_interval() -> u64 {
    DEFAULT_POLL_INTERVAL_MS
}

fn default_socket_path() -> String {
    DEFAULT_SOCKET_PATH.to_string()
}

fn default_true() -> bool {
    true
}
