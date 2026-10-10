//! Steam Frame eye bridge with resilient private ABI input.
mod filter;
mod input;
mod ipc;
mod runtime;
mod settings;
mod target;
use clap::Parser;
use filter::{EyeValues, OptionalEyeFilter};
use input::{EyeData, EyeUpdate, ShmVersion};
use rosc::{OscMessage, OscPacket, OscType, encoder};
use serde_json::{Map, Value, json};
#[cfg(test)]
use settings::Settings;
use settings::TargetMode;
use std::path::PathBuf;
use std::{error::Error, net::UdpSocket};
#[derive(Parser)]
#[command(about = "Send Steam Frame eye tracking from shared memory over OSC")]
struct Args {
    /// Saved settings file (default: XDG_CONFIG_HOME/frameeyeosc/settings.json).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Control socket in a private 0700 directory (default: XDG_RUNTIME_DIR/frameeyeosc/control.sock).
    #[arg(long)]
    control_socket: Option<PathBuf>,
    /// Manual OSC destination as HOST:PORT (default: 127.0.0.1:9000).
    #[arg(long, conflicts_with_all=["target_host", "target_port"])]
    target: Option<String>,
    /// Select manual destination or infer the streaming peer IP.
    #[arg(long, value_enum)]
    target_mode: Option<TargetMode>,
    /// Manual OSC destination host; selects manual mode.
    #[arg(long)]
    target_host: Option<String>,
    /// OSC port, independent of the detected streaming port (default: 9000).
    #[arg(long)]
    target_port: Option<u16>,
    /// Select the private shared-memory ABI, or infer it from the header.
    #[arg(long, value_enum)]
    shm_version: Option<ShmVersion>,
    /// Final eyelid clamp after smoothing, in [0, 1].
    #[arg(long, value_parser=parse_cap)]
    eyelid_output_cap: Option<f64>,
    /// Default to auto target mode unless --target-mode, --target, --target-host, or a saved target mode is set.
    #[arg(long)]
    panel_mode: bool,
    /// OSC avatar-parameter prefix (default: /FT).
    #[arg(long)]
    prefix: Option<String>,
    /// Enable One Euro smoothing for gaze and eyelid output.
    #[arg(long, num_args=0..=1, require_equals=true, default_missing_value="true")]
    one_euro: Option<bool>,
    /// Minimum cutoff in Hz (lower is smoother at rest; Baballonia default: 0.5).
    #[arg(long, value_parser = parse_positive_f64)]
    one_euro_min_cutoff: Option<f64>,
    /// Speed coefficient (higher is more responsive; Baballonia default: 3.0).
    #[arg(long, value_parser = parse_nonnegative_f64)]
    one_euro_beta: Option<f64>,
    /// Derivative low-pass cutoff in Hz (canonical/Baballonia default: 1.0).
    #[arg(long, value_parser = parse_positive_f64)]
    one_euro_d_cutoff: Option<f64>,
}

fn parse_positive_f64(value: &str) -> Result<f64, String> {
    let value = value
        .parse::<f64>()
        .map_err(|_| format!("expected a finite number greater than zero, got {value:?}"))?;
    if value.is_finite() && value > 0.0 && (value as f32).is_finite() && (value as f32) > 0.0 {
        Ok(value)
    } else {
        Err(format!(
            "expected a finite number greater than zero, got {value}"
        ))
    }
}

fn parse_nonnegative_f64(value: &str) -> Result<f64, String> {
    let value = value
        .parse::<f64>()
        .map_err(|_| format!("expected a finite non-negative number, got {value:?}"))?;
    if value.is_finite() && value >= 0.0 && (value as f32).is_finite() {
        Ok(value)
    } else {
        Err(format!(
            "expected a finite non-negative number, got {value}"
        ))
    }
}

enum EyeOutput {
    Data(EyeValues),
    Inactive,
    NoUpdate,
}

fn process_eye_update(
    update: EyeUpdate,
    filter: &mut OptionalEyeFilter,
    active: &mut bool,
) -> EyeOutput {
    match update {
        EyeUpdate::Data(data)
            if data.sample_time.is_finite()
                && data.gaze.iter().flatten().all(|value| value.is_finite())
                && data.fixation_point.iter().all(|value| value.is_finite())
                && data.openness.iter().all(|value| value.is_finite()) =>
        {
            let values = filter.filter(data.sample_time, normalized_eye_values(&data));
            *active = true;
            EyeOutput::Data(values)
        }
        EyeUpdate::NoUpdate => EyeOutput::NoUpdate,
        _ => {
            filter.tracking_inactive();
            if std::mem::replace(active, false) {
                EyeOutput::Inactive
            } else {
                EyeOutput::NoUpdate
            }
        }
    }
}

fn send(socket: &UdpSocket, addr: String, args: Vec<OscType>) -> Result<(), Box<dyn Error>> {
    let packet = OscPacket::Message(OscMessage { addr, args });
    socket.send(&encoder::encode(&packet)?)?;
    Ok(())
}

// Like Steam Link's OSC sender, ±45° maps to ±1; +Y is up (VRCFT convention, unverified on hardware).
fn gaze_angles([x, y, z]: [f32; 3]) -> [f32; 2] {
    let scale = 4.0 / std::f32::consts::PI;
    [
        (x.atan2(-z) * scale).clamp(-1.0, 1.0),
        (y.atan2(-z) * scale).clamp(-1.0, 1.0),
    ]
}

fn normalized_eye_values(data: &EyeData) -> EyeValues {
    EyeValues {
        left: gaze_angles(data.gaze[0]),
        right: gaze_angles(data.gaze[1]),
        combined: gaze_angles(data.fixation_point),
        eyelids: [
            data.openness[0].clamp(0.0, 1.0),
            data.openness[1].clamp(0.0, 1.0),
        ],
    }
}

fn send_eye_data(
    socket: &UdpSocket,
    prefix: &str,
    values: EyeValues,
) -> Result<(), Box<dyn Error>> {
    let prefix = format!("/avatar/parameters{}", prefix.trim_end_matches('/'));
    send(
        socket,
        format!("{prefix}/frame/EyeTrackingActive"),
        vec![OscType::Bool(true)],
    )?;
    for (suffix, value) in [
        ("EyeLeftX", values.left[0]),
        ("EyeLeftY", values.left[1]),
        ("EyeRightX", values.right[0]),
        ("EyeRightY", values.right[1]),
        ("EyeLidLeft", values.eyelids[0]),
        ("EyeLidRight", values.eyelids[1]),
        ("EyeX", values.combined[0]),
        ("EyeY", values.combined[1]),
    ] {
        send(
            socket,
            format!("{prefix}/v2/{suffix}"),
            vec![OscType::Float(value)],
        )?;
    }
    Ok(())
}

fn send_inactive(socket: &UdpSocket, prefix: &str) -> Result<(), Box<dyn Error>> {
    send(
        socket,
        format!(
            "/avatar/parameters{}/frame/EyeTrackingActive",
            prefix.trim_end_matches('/')
        ),
        vec![OscType::Bool(false)],
    )
}

fn parse_cap(value: &str) -> Result<f64, String> {
    let value = value
        .parse::<f64>()
        .map_err(|_| "cap must be a finite number in [0, 1]".to_owned())?;
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err("cap must be a finite number in [0, 1]".into())
    }
}
impl Args {
    fn cli_patch(&self) -> Result<Map<String, Value>, Box<dyn Error>> {
        let explicit_target = self.target.is_some() || self.target_host.is_some();
        if explicit_target && self.target_mode == Some(TargetMode::Auto) {
            return Err("explicit target conflicts with auto mode".into());
        }
        let mut patch = Map::new();
        macro_rules! add {
            ($field:ident) => {
                if let Some(value) = &self.$field {
                    patch.insert(stringify!($field).into(), json!(value));
                }
            };
        }
        add!(prefix);
        add!(one_euro);
        add!(one_euro_min_cutoff);
        add!(one_euro_beta);
        add!(one_euro_d_cutoff);
        add!(eyelid_output_cap);
        add!(target_host);
        add!(target_port);
        if let Some(mode) = self.target_mode {
            patch.insert(
                "target_mode".into(),
                json!(match mode {
                    TargetMode::Auto => "auto",
                    TargetMode::Manual => "manual",
                }),
            );
        }
        if explicit_target {
            patch.insert("target_mode".into(), json!("manual"));
        }
        if let Some(version) = self.shm_version {
            patch.insert(
                "shm_version".into(),
                json!(match version {
                    ShmVersion::Auto => "auto",
                    ShmVersion::V4 => "4",
                    ShmVersion::V5 => "5",
                }),
            );
        }
        if let Some(target) = &self.target {
            let (host, port) = target.rsplit_once(':').ok_or("target must be HOST:PORT")?;
            let host = if let Some(bracketed) = host.strip_prefix('[') {
                let host = bracketed
                    .strip_suffix(']')
                    .ok_or("unbalanced IPv6 target brackets")?;
                host.split('%')
                    .next()
                    .unwrap_or(host)
                    .parse::<std::net::Ipv6Addr>()
                    .map_err(|_| "bracketed target must be IPv6")?;
                host
            } else {
                if host.contains([':', '[', ']']) {
                    return Err("IPv6 target must use [HOST]:PORT".into());
                }
                host
            };
            patch.insert("target_host".into(), json!(host));
            patch.insert("target_port".into(), json!(port.parse::<u16>()?));
        }
        Ok(patch)
    }
    #[cfg(test)]
    fn settings(&self) -> Result<Settings, Box<dyn Error>> {
        Ok(settings::SettingsStore::merge(&Map::new(), &self.cli_patch()?, self.panel_mode)?.0)
    }
    fn config_path(&self) -> Result<PathBuf, Box<dyn Error>> {
        if let Some(path) = &self.config {
            return Ok(path.clone());
        }
        let root = if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty())
        {
            PathBuf::from(dir)
        } else {
            PathBuf::from(
                std::env::var_os("HOME")
                    .filter(|s| !s.is_empty())
                    .ok_or("HOME or --config is required")?,
            )
            .join(".config")
        };
        Ok(root.join("frameeyeosc/settings.json"))
    }
    fn socket_path(&self) -> Result<PathBuf, Box<dyn Error>> {
        if let Some(path) = &self.control_socket {
            return Ok(path.clone());
        }
        let root = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|s| !s.is_empty())
            .ok_or("XDG_RUNTIME_DIR or --control-socket is required")?;
        Ok(PathBuf::from(root).join("frameeyeosc/control.sock"))
    }
}
fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let (store, settings) =
        settings::SettingsStore::load(args.config_path()?, &args.cli_patch()?, args.panel_mode)?;
    let mut server = ipc::IpcServer::bind(args.socket_path()?, store)?;
    let mut runtime = runtime::Runtime::new(settings)?;
    eprintln!("Runtime started: {}", runtime.snapshot().output);
    loop {
        server.poll(&mut runtime)?;
        runtime.tick()?;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::OneEuroConfig;

    fn sample(time: f64, openness: f32) -> EyeData {
        EyeData {
            sample_time: time,
            gaze: [[0.0, 0.0, -1.0]; 2],
            fixation_point: [0.0, 0.0, -1.0],
            openness: [openness; 2],
        }
    }

    #[test]
    fn transient_no_update_preserves_tracking_and_filter_history() {
        let mut filter = OptionalEyeFilter::new(true, OneEuroConfig::default());
        let mut active = false;
        process_eye_update(EyeUpdate::Data(sample(0.0, 1.0)), &mut filter, &mut active);
        assert!(matches!(
            process_eye_update(EyeUpdate::NoUpdate, &mut filter, &mut active),
            EyeOutput::NoUpdate
        ));
        assert!(active);
        let EyeOutput::Data(values) =
            process_eye_update(EyeUpdate::Data(sample(0.01, 0.0)), &mut filter, &mut active)
        else {
            panic!("expected eye values")
        };
        assert!(
            values.eyelids[0] > 0.0,
            "history should smooth the transition"
        );
    }

    #[test]
    fn confirmed_inactivity_resets_history() {
        let mut filter = OptionalEyeFilter::new(true, OneEuroConfig::default());
        let mut active = false;
        process_eye_update(EyeUpdate::Data(sample(0.0, 1.0)), &mut filter, &mut active);
        assert!(matches!(
            process_eye_update(EyeUpdate::Inactive, &mut filter, &mut active),
            EyeOutput::Inactive
        ));
        assert!(!active);
        let EyeOutput::Data(values) =
            process_eye_update(EyeUpdate::Data(sample(0.01, 0.0)), &mut filter, &mut active)
        else {
            panic!("expected eye values")
        };
        assert_eq!(values.eyelids, [0.0; 2]);
    }

    #[test]
    fn one_euro_cli_defaults_match_baballonia() {
        let args = Args::try_parse_from(["frameeyeosc", "--one-euro"]).unwrap();

        assert_eq!(args.one_euro, Some(true));
        let current = args.settings().unwrap();
        assert_eq!(current.one_euro_min_cutoff, 0.5);
        assert_eq!(current.one_euro_beta, 3.0);
        assert_eq!(current.one_euro_d_cutoff, 1.0);
    }

    #[test]
    fn one_euro_cli_rejects_invalid_tuning_values() {
        for arguments in [
            ["--one-euro-min-cutoff", "0"],
            ["--one-euro-min-cutoff", "NaN"],
            ["--one-euro-beta", "-0.1"],
            ["--one-euro-beta", "inf"],
            ["--one-euro-d-cutoff", "0"],
            ["--one-euro-d-cutoff", "NaN"],
        ] {
            let result = Args::try_parse_from(["frameeyeosc", arguments[0], arguments[1]]);
            assert!(result.is_err(), "accepted invalid arguments: {arguments:?}");
        }
    }
}
#[cfg(test)]
mod cli_tests {
    use super::*;
    fn settings(arguments: &[&str]) -> Result<Settings, Box<dyn Error>> {
        Args::try_parse_from(std::iter::once("frameeyeosc").chain(arguments.iter().copied()))?
            .settings()
    }
    #[test]
    fn defaults_and_explicit_manual() {
        assert_eq!(settings(&[]).unwrap().target_mode, TargetMode::Manual);
        assert_eq!(
            settings(&["--panel-mode"]).unwrap().target_mode,
            TargetMode::Auto
        );
        assert_eq!(
            settings(&["--panel-mode", "--target", "127.0.0.1:9001"])
                .unwrap()
                .target_mode,
            TargetMode::Manual
        );
        assert_eq!(
            settings(&["--panel-mode", "--target-host", "localhost"])
                .unwrap()
                .target_mode,
            TargetMode::Manual
        );
        assert_eq!(
            settings(&["--target-mode", "auto", "--target-port", "9001"])
                .unwrap()
                .target_port,
            9001
        );
        assert!(settings(&["--target-mode", "auto", "--target", "localhost:9000"]).is_err());
        assert_eq!(
            settings(&["--target", "[::1]:9001"]).unwrap().target_host,
            "::1"
        );
    }
    #[test]
    fn cap_rejects_invalid_numbers() {
        for value in ["NaN", "inf", "-0.1", "1.1"] {
            assert!(settings(&["--eyelid-output-cap", value]).is_err());
        }
        assert_eq!(settings(&[]).unwrap().eyelid_output_cap, 1.0);
    }
}
#[cfg(test)]
mod target_syntax_tests {
    use super::*;
    #[test]
    fn malformed_target_brackets_are_rejected() {
        for target in [
            "[127.0.0.1:9000",
            "[[::1]]:9000",
            "::1:9000",
            "[localhost]:9000",
        ] {
            let args = Args::try_parse_from(["frameeyeosc", "--target", target]).unwrap();
            assert!(args.settings().is_err(), "{target}");
        }
    }
}
#[cfg(test)]
mod config_cli_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn explicit_false_and_saved_manual_survive_panel_mode() {
        let args =
            Args::try_parse_from(["frameeyeosc", "--panel-mode", "--one-euro=false"]).unwrap();
        let saved = json!({"one_euro":true,"target_mode":"manual"});
        let (settings, sources) = settings::SettingsStore::merge(
            saved.as_object().unwrap(),
            &args.cli_patch().unwrap(),
            args.panel_mode,
        )
        .unwrap();
        assert!(!settings.one_euro);
        assert_eq!(settings.target_mode, TargetMode::Manual);
        assert_eq!(sources["one_euro"], "cli");
    }
}
#[cfg(test)]
mod numeric_cli_tests {
    use super::*;
    #[test]
    fn cli_rejects_out_of_range_before_narrowing() {
        for (option, value) in [
            ("--eyelid-output-cap", "1.0000000001"),
            ("--eyelid-output-cap", "-1e-50"),
            ("--one-euro-beta", "-1e-50"),
        ] {
            assert!(Args::try_parse_from(["frameeyeosc", option, value]).is_err());
        }
    }
}
#[cfg(test)]
mod decimal_cli_tests {
    use super::*;
    #[test]
    fn cli_decimal_values_remain_exact_in_current_snapshot() {
        let args = Args::try_parse_from([
            "frameeyeosc",
            "--eyelid-output-cap",
            "0.7",
            "--one-euro-beta",
            "0.1",
        ])
        .unwrap();
        let values = args.settings().unwrap().values();
        assert_eq!(values["eyelid_output_cap"], json!(0.7));
        assert_eq!(values["one_euro_beta"], json!(0.1));
    }
}
