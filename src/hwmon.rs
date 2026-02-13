// Copyright (c) 2026 Pegasus Heavy Industries LLC
// Licensed under the MIT License

//! hwmon sysfs discovery and control.
//!
//! Scans `/sys/class/hwmon/` for fan and temperature sensor entries,
//! and provides read/write access to PWM and sensor values.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const HWMON_ROOT: &str = "/sys/class/hwmon";

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A discovered fan (PWM output + optional tachometer input).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fan {
    /// Unique identifier, e.g. "hwmon3/pwm1"
    pub id: String,
    /// Human-readable label if available
    pub label: Option<String>,
    /// Absolute path to the `pwmN` file
    pub pwm_path: PathBuf,
    /// Absolute path to the `pwmN_enable` file
    pub pwm_enable_path: PathBuf,
    /// Absolute path to the `fanN_input` file (RPM), if present
    pub rpm_path: Option<PathBuf>,
    /// Name of the parent hwmon device
    pub hwmon_name: String,
}

/// A discovered temperature sensor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempSensor {
    /// Unique identifier, e.g. "hwmon3/temp1"
    pub id: String,
    /// Human-readable label if available
    pub label: Option<String>,
    /// Absolute path to the `tempN_input` file (millidegrees C)
    pub input_path: PathBuf,
    /// Name of the parent hwmon device
    pub hwmon_name: String,
}

/// Live readings for a fan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanStatus {
    pub id: String,
    pub label: Option<String>,
    pub hwmon_name: String,
    /// Current PWM value 0-255
    pub pwm: Option<u8>,
    /// Current PWM enable mode: 0=off, 1=manual, 2=auto
    pub pwm_enable: Option<u8>,
    /// Current fan speed in RPM
    pub rpm: Option<u32>,
}

/// Live reading for a temperature sensor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempStatus {
    pub id: String,
    pub label: Option<String>,
    pub hwmon_name: String,
    /// Temperature in degrees Celsius
    pub temp_c: Option<f64>,
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Scan `/sys/class/hwmon` and return all discovered fans.
pub fn discover_fans() -> io::Result<Vec<Fan>> {
    let mut fans = Vec::new();

    for entry in fs::read_dir(HWMON_ROOT)? {
        let entry = entry?;
        let hwmon_dir = entry.path();
        let hwmon_name = read_trimmed(&hwmon_dir.join("name")).unwrap_or_default();
        let hwmon_basename = entry.file_name().to_string_lossy().to_string();

        // Look for pwmN files (N = 1, 2, 3, ...)
        for n in 1..=16 {
            let pwm_path = hwmon_dir.join(format!("pwm{n}"));
            let pwm_enable_path = hwmon_dir.join(format!("pwm{n}_enable"));

            if !pwm_path.exists() {
                break;
            }

            let id = format!("{hwmon_basename}/pwm{n}");
            let label = read_trimmed(&hwmon_dir.join(format!("fan{n}_label")));
            let rpm_path = {
                let p = hwmon_dir.join(format!("fan{n}_input"));
                if p.exists() { Some(p) } else { None }
            };

            fans.push(Fan {
                id,
                label,
                pwm_path,
                pwm_enable_path,
                rpm_path,
                hwmon_name: hwmon_name.clone(),
            });
        }
    }

    fans.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(fans)
}

/// Scan `/sys/class/hwmon` and return all discovered temperature sensors.
pub fn discover_temp_sensors() -> io::Result<Vec<TempSensor>> {
    let mut sensors = Vec::new();

    for entry in fs::read_dir(HWMON_ROOT)? {
        let entry = entry?;
        let hwmon_dir = entry.path();
        let hwmon_name = read_trimmed(&hwmon_dir.join("name")).unwrap_or_default();
        let hwmon_basename = entry.file_name().to_string_lossy().to_string();

        for n in 1..=32 {
            let input_path = hwmon_dir.join(format!("temp{n}_input"));

            if !input_path.exists() {
                break;
            }

            let id = format!("{hwmon_basename}/temp{n}");
            let label = read_trimmed(&hwmon_dir.join(format!("temp{n}_label")));

            sensors.push(TempSensor {
                id,
                label,
                input_path,
                hwmon_name: hwmon_name.clone(),
            });
        }
    }

    sensors.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(sensors)
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Read current status for a fan.
pub fn read_fan_status(fan: &Fan) -> FanStatus {
    let pwm = read_trimmed(&fan.pwm_path)
        .and_then(|s| s.parse::<u8>().ok());
    let pwm_enable = read_trimmed(&fan.pwm_enable_path)
        .and_then(|s| s.parse::<u8>().ok());
    let rpm = fan.rpm_path.as_ref().and_then(|p| {
        read_trimmed(p).and_then(|s| s.parse::<u32>().ok())
    });

    FanStatus {
        id: fan.id.clone(),
        label: fan.label.clone(),
        hwmon_name: fan.hwmon_name.clone(),
        pwm,
        pwm_enable,
        rpm,
    }
}

/// Read current status for a temperature sensor.
pub fn read_temp_status(sensor: &TempSensor) -> TempStatus {
    let temp_c = read_trimmed(&sensor.input_path)
        .and_then(|s| s.parse::<i64>().ok())
        .map(|millic| millic as f64 / 1000.0);

    TempStatus {
        id: sensor.id.clone(),
        label: sensor.label.clone(),
        hwmon_name: sensor.hwmon_name.clone(),
        temp_c,
    }
}

/// Read all fan statuses.
pub fn read_all_fan_statuses(fans: &[Fan]) -> Vec<FanStatus> {
    fans.iter().map(read_fan_status).collect()
}

/// Read all temp statuses.
pub fn read_all_temp_statuses(sensors: &[TempSensor]) -> Vec<TempStatus> {
    sensors.iter().map(read_temp_status).collect()
}

/// Build a map of sensor id -> current temp for quick lookup by the curve engine.
pub fn read_temp_map(sensors: &[TempSensor]) -> HashMap<String, f64> {
    let mut map = HashMap::new();
    for s in sensors {
        if let Some(t) = read_temp_status(s).temp_c {
            map.insert(s.id.clone(), t);
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Set PWM enable mode for a fan.
///   0 = fan off (full speed on some systems)
///   1 = manual PWM control
///   2 = automatic (BIOS/firmware)
pub fn set_pwm_enable(fan: &Fan, mode: u8) -> io::Result<()> {
    fs::write(&fan.pwm_enable_path, format!("{mode}"))
}

/// Set the PWM duty value (0-255) for a fan. The fan must already be in
/// manual mode (`pwm_enable = 1`).
pub fn set_pwm(fan: &Fan, value: u8) -> io::Result<()> {
    fs::write(&fan.pwm_path, format!("{value}"))
}

/// Put a fan into manual mode and set a specific PWM value.
pub fn set_manual_pwm(fan: &Fan, value: u8) -> io::Result<()> {
    set_pwm_enable(fan, 1)?;
    set_pwm(fan, value)
}

/// Restore a fan to automatic (BIOS) control.
pub fn restore_automatic(fan: &Fan) -> io::Result<()> {
    set_pwm_enable(fan, 2)
}

/// Restore all fans to automatic control (safety fallback).
pub fn restore_all_automatic(fans: &[Fan]) {
    for fan in fans {
        if let Err(e) = restore_automatic(fan) {
            log::warn!("Failed to restore automatic control for {}: {e}", fan.id);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}
