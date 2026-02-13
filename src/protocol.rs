// Copyright (c) 2026 Pegasus Heavy Industries LLC
// Licensed under the MIT License

//! Client-daemon protocol over Unix domain sockets.
//!
//! Messages are newline-delimited JSON. The client sends a [`Request`]
//! and the daemon replies with a [`Response`].

use crate::config::FanAssignment;
use crate::curve::{CurvePoint, FanCurve};
use crate::hwmon::{FanStatus, TempStatus};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Requests (TUI -> Daemon)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    /// Request current status of all fans and temps.
    #[serde(rename = "get_status")]
    GetStatus,

    /// Set a fan to manual PWM.
    #[serde(rename = "set_manual")]
    SetManual { fan_id: String, pwm: u8 },

    /// Assign a curve to a fan.
    #[serde(rename = "set_curve")]
    SetCurve {
        fan_id: String,
        curve_name: String,
        temp_sensor_id: String,
    },

    /// Set a fan to automatic (BIOS) control.
    #[serde(rename = "set_auto")]
    SetAuto { fan_id: String },

    /// List all configured curves.
    #[serde(rename = "list_curves")]
    ListCurves,

    /// Create or update a curve.
    #[serde(rename = "upsert_curve")]
    UpsertCurve {
        name: String,
        points: Vec<CurvePoint>,
    },

    /// Delete a curve by name.
    #[serde(rename = "delete_curve")]
    DeleteCurve { name: String },

    /// Save current configuration to disk.
    #[serde(rename = "save_config")]
    SaveConfig,

    /// Reload configuration from disk.
    #[serde(rename = "reload_config")]
    ReloadConfig,

    /// Request the daemon to push periodic status updates.
    #[serde(rename = "subscribe")]
    Subscribe,

    /// Stop receiving periodic status updates.
    #[serde(rename = "unsubscribe")]
    Unsubscribe,
}

// ---------------------------------------------------------------------------
// Responses (Daemon -> TUI)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    /// Current system status.
    #[serde(rename = "status")]
    Status {
        fans: Vec<FanStatus>,
        temps: Vec<TempStatus>,
        assignments: Vec<FanAssignmentInfo>,
    },

    /// List of configured curves.
    #[serde(rename = "curves")]
    Curves { curves: Vec<FanCurve> },

    /// Operation succeeded.
    #[serde(rename = "ok")]
    Ok { message: String },

    /// Operation failed.
    #[serde(rename = "error")]
    Error { message: String },
}

/// Fan assignment info sent in status messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanAssignmentInfo {
    pub fan_id: String,
    pub assignment: FanAssignment,
}

// ---------------------------------------------------------------------------
// Serialization helpers
// ---------------------------------------------------------------------------

/// Encode a message as a newline-delimited JSON string.
pub fn encode<T: Serialize>(msg: &T) -> Result<String, serde_json::Error> {
    let mut s = serde_json::to_string(msg)?;
    s.push('\n');
    Ok(s)
}

/// Decode a message from a JSON string (newline-trimmed).
pub fn decode<'a, T: Deserialize<'a>>(s: &'a str) -> Result<T, serde_json::Error> {
    serde_json::from_str(s.trim())
}
