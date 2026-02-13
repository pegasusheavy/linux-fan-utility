// Copyright (c) 2026 Pegasus Heavy Industries LLC
// Licensed under the MIT License

//! fanctl-daemon: system service that owns hwmon writes, runs fan curves,
//! and accepts commands from TUI clients over a Unix domain socket.

use clap::Parser;
use linux_fan_utility::config::{self, Config, FanAssignment};
use linux_fan_utility::curve::FanCurve;
use linux_fan_utility::hwmon::{self, Fan, TempSensor};
use linux_fan_utility::protocol::{self, FanAssignmentInfo, Request, Response};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify};
use tokio::time::{self, Duration};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "fanctl-daemon", about = "Linux fan control daemon")]
struct Cli {
    /// Path to the configuration file.
    #[arg(short, long, default_value = config::DEFAULT_CONFIG_PATH)]
    config: String,

    /// Override the socket path.
    #[arg(short, long)]
    socket: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared daemon state
// ---------------------------------------------------------------------------

struct DaemonState {
    config: Config,
    fans: Vec<Fan>,
    sensors: Vec<TempSensor>,
    config_path: PathBuf,
}

type SharedState = Arc<Mutex<DaemonState>>;

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();
    let config_path = config::resolve_config_path(Some(&cli.config));
    let cfg = config::load_config(&config_path).unwrap_or_else(|e| {
        log::warn!("Could not load config: {e}, using defaults");
        Config::default()
    });

    let socket_path = cli
        .socket
        .clone()
        .unwrap_or_else(|| cfg.daemon.socket_path.clone());

    // Discover hardware
    let fans = hwmon::discover_fans().unwrap_or_else(|e| {
        log::error!("Failed to discover fans: {e}");
        Vec::new()
    });
    let sensors = hwmon::discover_temp_sensors().unwrap_or_else(|e| {
        log::error!("Failed to discover temp sensors: {e}");
        Vec::new()
    });

    log::info!(
        "Discovered {} fan(s) and {} temp sensor(s)",
        fans.len(),
        sensors.len()
    );

    // Apply initial config
    apply_assignments(&fans, &sensors, &cfg);

    let restore_on_exit = cfg.daemon.restore_on_exit;
    let poll_interval = cfg.daemon.poll_interval_ms;
    let state: SharedState = Arc::new(Mutex::new(DaemonState {
        config: cfg,
        fans,
        sensors,
        config_path,
    }));

    // Clean up old socket file
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;

    // Make socket accessible to non-root users
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o666);
        std::fs::set_permissions(&socket_path, perms)?;
    }

    log::info!("Listening on {socket_path}");

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();

    // Signal handler
    let state_for_signal = state.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        log::info!("Received shutdown signal");
        if restore_on_exit {
            let st = state_for_signal.lock().await;
            hwmon::restore_all_automatic(&st.fans);
            log::info!("Restored all fans to automatic control");
        }
        shutdown_signal.notify_waiters();
    });

    // Curve engine loop
    let state_for_curve = state.clone();
    let shutdown_for_curve = shutdown.clone();
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_millis(poll_interval));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let st = state_for_curve.lock().await;
                    run_curve_engine(&st);
                }
                _ = shutdown_for_curve.notified() => {
                    break;
                }
            }
        }
    });

    // Accept client connections
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let state_clone = state.clone();
                        tokio::spawn(handle_client(stream, state_clone));
                    }
                    Err(e) => {
                        log::error!("Failed to accept connection: {e}");
                    }
                }
            }
            _ = shutdown.notified() => {
                log::info!("Daemon shutting down");
                break;
            }
        }
    }

    // Cleanup socket
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

// ---------------------------------------------------------------------------
// Client connection handler
// ---------------------------------------------------------------------------

async fn handle_client(stream: UnixStream, state: SharedState) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let response = match protocol::decode::<Request>(&line) {
            Ok(req) => process_request(req, &state).await,
            Err(e) => Response::Error {
                message: format!("Invalid request: {e}"),
            },
        };

        let encoded = match protocol::encode(&response) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Failed to encode response: {e}");
                continue;
            }
        };

        if writer.write_all(encoded.as_bytes()).await.is_err() {
            break; // Client disconnected
        }
    }
}

async fn process_request(req: Request, state: &SharedState) -> Response {
    let mut st = state.lock().await;

    match req {
        Request::GetStatus => {
            let fans = hwmon::read_all_fan_statuses(&st.fans);
            let temps = hwmon::read_all_temp_statuses(&st.sensors);
            let assignments = st
                .config
                .fans
                .iter()
                .map(|(fan_id, a)| FanAssignmentInfo {
                    fan_id: fan_id.clone(),
                    assignment: a.clone(),
                })
                .collect();

            Response::Status {
                fans,
                temps,
                assignments,
            }
        }

        Request::SetManual { fan_id, pwm } => {
            if let Some(fan) = st.fans.iter().find(|f| f.id == fan_id) {
                match hwmon::set_manual_pwm(fan, pwm) {
                    Ok(()) => {
                        st.config
                            .fans
                            .insert(fan_id.clone(), FanAssignment::Manual { pwm });
                        Response::Ok {
                            message: format!("Set {fan_id} to manual PWM {pwm}"),
                        }
                    }
                    Err(e) => Response::Error {
                        message: format!("Failed to set PWM: {e}"),
                    },
                }
            } else {
                Response::Error {
                    message: format!("Unknown fan: {fan_id}"),
                }
            }
        }

        Request::SetCurve {
            fan_id,
            curve_name,
            temp_sensor_id,
        } => {
            // Validate curve exists
            if !st.config.curves.iter().any(|c| c.name == curve_name) {
                return Response::Error {
                    message: format!("Unknown curve: {curve_name}"),
                };
            }
            // Validate sensor exists
            if !st.sensors.iter().any(|s| s.id == temp_sensor_id) {
                return Response::Error {
                    message: format!("Unknown temp sensor: {temp_sensor_id}"),
                };
            }
            // Put fan in manual mode (curves write PWM via manual mode)
            if let Some(fan) = st.fans.iter().find(|f| f.id == fan_id) {
                if let Err(e) = hwmon::set_pwm_enable(fan, 1) {
                    return Response::Error {
                        message: format!("Failed to enable manual mode: {e}"),
                    };
                }
            } else {
                return Response::Error {
                    message: format!("Unknown fan: {fan_id}"),
                };
            }

            st.config.fans.insert(
                fan_id.clone(),
                FanAssignment::Curve {
                    curve_name: curve_name.clone(),
                    temp_sensor_id,
                },
            );
            Response::Ok {
                message: format!("Assigned curve '{curve_name}' to {fan_id}"),
            }
        }

        Request::SetAuto { fan_id } => {
            if let Some(fan) = st.fans.iter().find(|f| f.id == fan_id) {
                match hwmon::restore_automatic(fan) {
                    Ok(()) => {
                        st.config.fans.insert(fan_id.clone(), FanAssignment::Auto);
                        Response::Ok {
                            message: format!("Restored {fan_id} to automatic control"),
                        }
                    }
                    Err(e) => Response::Error {
                        message: format!("Failed to restore auto: {e}"),
                    },
                }
            } else {
                Response::Error {
                    message: format!("Unknown fan: {fan_id}"),
                }
            }
        }

        Request::ListCurves => Response::Curves {
            curves: st.config.curves.clone(),
        },

        Request::UpsertCurve { name, points } => {
            let curve = FanCurve::new(name.clone(), points);
            if let Err(e) = curve.validate() {
                return Response::Error { message: e };
            }

            // Replace existing or push new
            if let Some(existing) = st.config.curves.iter_mut().find(|c| c.name == name) {
                *existing = curve;
            } else {
                st.config.curves.push(curve);
            }

            Response::Ok {
                message: format!("Curve '{name}' saved"),
            }
        }

        Request::DeleteCurve { name } => {
            let before = st.config.curves.len();
            st.config.curves.retain(|c| c.name != name);
            if st.config.curves.len() < before {
                Response::Ok {
                    message: format!("Deleted curve '{name}'"),
                }
            } else {
                Response::Error {
                    message: format!("Curve '{name}' not found"),
                }
            }
        }

        Request::SaveConfig => match config::save_config(&st.config_path, &st.config) {
            Ok(()) => Response::Ok {
                message: format!("Config saved to {}", st.config_path.display()),
            },
            Err(e) => Response::Error {
                message: format!("Failed to save config: {e}"),
            },
        },

        Request::ReloadConfig => match config::load_config(&st.config_path) {
            Ok(cfg) => {
                apply_assignments(&st.fans, &st.sensors, &cfg);
                st.config = cfg;
                Response::Ok {
                    message: "Config reloaded".to_string(),
                }
            }
            Err(e) => Response::Error {
                message: format!("Failed to reload config: {e}"),
            },
        },

        Request::Subscribe | Request::Unsubscribe => {
            // Subscription is handled at the connection level in a full
            // implementation. For now, status polling via GetStatus works.
            Response::Ok {
                message: "Acknowledged".to_string(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Curve engine
// ---------------------------------------------------------------------------

fn run_curve_engine(st: &DaemonState) {
    let temp_map = hwmon::read_temp_map(&st.sensors);

    for (fan_id, assignment) in &st.config.fans {
        if let FanAssignment::Curve {
            curve_name,
            temp_sensor_id,
        } = assignment
        {
            let Some(curve) = st.config.curves.iter().find(|c| &c.name == curve_name) else {
                log::warn!("Fan {fan_id}: curve '{curve_name}' not found, skipping");
                continue;
            };
            let Some(&temp) = temp_map.get(temp_sensor_id) else {
                log::warn!("Fan {fan_id}: sensor '{temp_sensor_id}' has no reading, skipping");
                continue;
            };
            let pwm = curve.interpolate(temp);

            if let Some(fan) = st.fans.iter().find(|f| &f.id == fan_id) {
                if let Err(e) = hwmon::set_pwm(fan, pwm) {
                    log::error!("Failed to write PWM for {fan_id}: {e}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Apply assignments from config on startup/reload
// ---------------------------------------------------------------------------

fn apply_assignments(fans: &[Fan], sensors: &[TempSensor], config: &Config) {
    let temp_map = hwmon::read_temp_map(sensors);

    for (fan_id, assignment) in &config.fans {
        let Some(fan) = fans.iter().find(|f| &f.id == fan_id) else {
            log::warn!("Config references unknown fan: {fan_id}");
            continue;
        };

        match assignment {
            FanAssignment::Auto => {
                if let Err(e) = hwmon::restore_automatic(fan) {
                    log::error!("Failed to set {fan_id} to auto: {e}");
                }
            }
            FanAssignment::Manual { pwm } => {
                if let Err(e) = hwmon::set_manual_pwm(fan, *pwm) {
                    log::error!("Failed to set {fan_id} to manual PWM {pwm}: {e}");
                }
            }
            FanAssignment::Curve {
                curve_name,
                temp_sensor_id,
            } => {
                // Enable manual mode so the curve engine can write PWM values
                if let Err(e) = hwmon::set_pwm_enable(fan, 1) {
                    log::error!("Failed to enable manual mode for {fan_id}: {e}");
                    continue;
                }
                // Apply initial value from curve
                if let Some(curve) = config.curves.iter().find(|c| &c.name == curve_name) {
                    if let Some(&temp) = temp_map.get(temp_sensor_id) {
                        let pwm = curve.interpolate(temp);
                        if let Err(e) = hwmon::set_pwm(fan, pwm) {
                            log::error!("Failed to write initial curve PWM for {fan_id}: {e}");
                        }
                    }
                }
            }
        }
    }
}
