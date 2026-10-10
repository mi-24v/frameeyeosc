use crate::{
    EyeOutput,
    filter::{EyeValues, OptionalEyeFilter},
    input::{EyeSource, EyeUpdate, InputStatus},
    process_eye_update,
    settings::{Settings, TargetMode},
    target::{StreamObservation, detect_stream},
};
use std::{
    collections::HashMap,
    io,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RuntimeSnapshot {
    pub settings: Settings,
    pub profile: serde_json::Value,
    pub calibration: serde_json::Value,
    pub input: InputStatus,
    pub tracking: Option<bool>,
    pub stream: StreamObservation,
    pub osc_target: Option<SocketAddr>,
    pub output: &'static str,
}
pub(crate) type RuntimeError = Box<dyn std::error::Error>;
pub(crate) struct PreparedSettings {
    settings: Settings,
    target: Option<SocketAddr>,
    socket: Option<UdpSocket>,
    source: Option<EyeSource>,
    replace_source: bool,
}
pub(crate) struct Runtime {
    settings: Settings,
    source: Option<EyeSource>,
    source_path: PathBuf,
    proc_root: PathBuf,
    input: InputStatus,
    tracking: Option<bool>,
    stream: StreamObservation,
    target: Option<SocketAddr>,
    socket: Option<UdpSocket>,
    filter: OptionalEyeFilter,
    wide_filter: OptionalEyeFilter,
    calibration: Option<crate::calibration::Calibration>,
    active: bool,
    profile_revision: u64,
    profile_cause: serde_json::Value,
    last_refresh: Instant,
    pending_inactive: Option<(SocketAddr, String)>,
    errors: ErrorLog,
}
fn cap_values(mut values: EyeValues, cap: f32) -> EyeValues {
    values.eyelids = values.eyelids.map(|x| x.clamp(0.0, cap));
    values
}
fn output_reason(
    input: InputStatus,
    target: Option<SocketAddr>,
    tracking: Option<bool>,
) -> &'static str {
    match input {
        InputStatus::Unavailable => "input_unavailable",
        InputStatus::AbiMismatch => "abi_mismatch",
        _ if target.is_none() => "no_target",
        InputStatus::InvalidSample => "invalid_sample",
        _ => match tracking {
            Some(true) => "sending",
            Some(false) => "tracking_inactive",
            None => "tracking_unknown",
        },
    }
}
fn classify(error: &io::Error) -> InputStatus {
    if error.kind() == io::ErrorKind::InvalidData {
        InputStatus::AbiMismatch
    } else {
        InputStatus::Unavailable
    }
}
fn resolve(settings: &Settings, stream: &StreamObservation) -> io::Result<Option<SocketAddr>> {
    if settings.target_mode == TargetMode::Auto {
        Ok(stream
            .peer
            .map(|peer| SocketAddr::new(peer.ip(), settings.target_port)))
    } else {
        (settings.target_host.as_str(), settings.target_port)
            .to_socket_addrs()?
            .next()
            .map(Some)
            .ok_or_else(|| io::Error::other("target did not resolve"))
    }
}
fn socket(target: Option<SocketAddr>) -> io::Result<Option<UdpSocket>> {
    target
        .map(|target| {
            let socket = UdpSocket::bind(if target.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            })?;
            socket.connect(target)?;
            socket.set_nonblocking(true)?;
            Ok(socket)
        })
        .transpose()
}
impl Runtime {
    pub(crate) fn load_profile(&mut self, config: &Path) {
        if self.calibration.is_none() {
            self.calibration = Some(crate::calibration::Calibration::new(
                crate::calibration::profile_path(config),
                &self.settings,
            ));
            self.reset_filters();
        }
    }
    fn reset_filters(&mut self) {
        self.filter.reset_eyelids();
        self.wide_filter.tracking_inactive();
    }
    pub(crate) fn calibration_active(&self) -> bool {
        self.calibration.as_ref().is_some_and(|c| c.active())
    }
    pub(crate) fn calibration_command(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cause: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let calibration = self.calibration.as_mut().ok_or("not_initialized")?;
        let previous = calibration.profile.clone();
        let result = calibration.command(method, params, cause, Instant::now(), &self.settings);
        if calibration.profile != previous || (method == "calibration.skip_wide" && result.is_ok())
        {
            self.reset_filters();
        }
        result
    }
    pub(crate) fn calibration_disconnect(&mut self, connection: &str) {
        if let Some(c) = &mut self.calibration {
            c.disconnect(connection);
        }
    }
    pub(crate) fn advance_calibration(
        &mut self,
        now: Instant,
        observation: Option<crate::input::Observation>,
    ) {
        if let Some(c) = &mut self.calibration {
            let previous = c.profile.clone();
            let was_active = c.active();
            c.advance(now, observation, &self.settings);
            if c.profile != previous || (was_active && c.snapshot()["state"] == "completed") {
                self.profile_revision = self.profile_revision.wrapping_add(1);
                self.profile_cause = c.snapshot()["caused_by"].clone();
                self.reset_filters();
            }
        }
    }
    pub(crate) fn profile_change(&self) -> (u64, &serde_json::Value) {
        (self.profile_revision, &self.profile_cause)
    }
    fn calibrated_output(&mut self, update: EyeUpdate) -> EyeOutput {
        if let EyeUpdate::Data(data) = &update
            && crate::valid_eye_data(data)
            && let Some(profile) = self.calibration.as_ref().and_then(|c| c.profile.as_ref())
        {
            let (base, wide) = profile.normalize(data.openness, &self.settings);
            let mut values = crate::normalized_eye_values(data);
            values.eyelids = base;
            let mut values = self.filter.filter(data.sample_time, values);
            let wide_values = self.wide_filter.filter(
                data.sample_time,
                EyeValues {
                    eyelids: wide,
                    ..values
                },
            );
            let available = profile.wide_available(&self.settings);
            values.eyelids = std::array::from_fn(|i| {
                0.75 * values.eyelids[i]
                    + if self.settings.eye_widening && available[i] {
                        0.25 * wide_values.eyelids[i]
                    } else {
                        0.0
                    }
            });
            self.active = true;
            return EyeOutput::Data(values);
        }
        if matches!(update, EyeUpdate::Inactive | EyeUpdate::Invalid)
            || matches!(&update,EyeUpdate::Data(data) if !crate::valid_eye_data(data))
        {
            self.wide_filter.tracking_inactive();
        }
        process_eye_update(update, &mut self.filter, &mut self.active)
    }

    pub(crate) fn new(settings: Settings) -> Result<Self, RuntimeError> {
        Self::with_paths(
            settings,
            Path::new("/dev/shm/eye-server.mmap"),
            Path::new("/proc"),
        )
    }
    pub(crate) fn with_paths(
        settings: Settings,
        source_path: &Path,
        proc_root: &Path,
    ) -> Result<Self, RuntimeError> {
        settings.validate()?;
        let stream = detect_stream(proc_root);
        let (target, socket) =
            match resolve(&settings, &stream).and_then(|target| Ok((target, socket(target)?))) {
                Ok(destination) => destination,
                Err(error) => {
                    eprintln!("OSC destination unavailable: {error}");
                    (None, None)
                }
            };
        let (source, input) = match EyeSource::open(source_path, settings.shm_version) {
            Ok(source) => (Some(source), InputStatus::Ready),
            Err(error) => (None, classify(&error)),
        };
        let filter = OptionalEyeFilter::new(settings.one_euro, settings.filter_config());
        let wide_filter = OptionalEyeFilter::new(settings.one_euro, settings.filter_config());
        Ok(Self {
            settings,
            source,
            source_path: source_path.into(),
            proc_root: proc_root.into(),
            input,
            tracking: None,
            stream,
            target,
            socket,
            filter,
            wide_filter,
            calibration: None,
            active: false,
            profile_revision: 0,
            profile_cause: serde_json::Value::Null,
            last_refresh: Instant::now(),
            pending_inactive: None,
            errors: ErrorLog::default(),
        })
    }
    pub(crate) fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            settings: self.settings.clone(),
            profile: self
                .calibration
                .as_ref()
                .and_then(|c| c.profile.as_ref())
                .map_or(serde_json::Value::Null, |p| p.snapshot(&self.settings)),
            calibration: self
                .calibration
                .as_ref()
                .map_or(serde_json::Value::Null, |c| c.snapshot()),
            input: self.input,
            tracking: self.tracking,
            stream: self.stream.clone(),
            osc_target: self.target,
            output: output_reason(self.input, self.target, self.tracking),
        }
    }
    pub(crate) fn prepare_settings(
        &self,
        candidate: &Settings,
    ) -> Result<PreparedSettings, RuntimeError> {
        candidate.validate()?;
        if let Some(calibration) = &self.calibration {
            calibration.validate_candidate_settings(candidate)?;
        }
        if let Some(profile) = self.calibration.as_ref().and_then(|c| c.profile.as_ref()) {
            profile.validate(candidate)?;
        }
        if self.calibration_active() && candidate.shm_version != self.settings.shm_version {
            return Err("busy".into());
        }
        let target = if candidate.target_mode == self.settings.target_mode
            && candidate.target_host == self.settings.target_host
            && candidate.target_port == self.settings.target_port
        {
            self.target
        } else {
            resolve(candidate, &self.stream)?
        };
        let socket = socket(target)?;
        let replace_source = candidate.shm_version != self.settings.shm_version;
        let source = if replace_source {
            Some(EyeSource::open(&self.source_path, candidate.shm_version)?)
        } else {
            // Keep the current source for unrelated settings; tick revalidates it before use.
            None
        };
        Ok(PreparedSettings {
            settings: candidate.clone(),
            target,
            socket,
            source,
            replace_source,
        })
    }
    pub(crate) fn apply_settings(&mut self, prepared: PreparedSettings) {
        let previous = &self.settings;
        let next = &prepared.settings;
        if previous.one_euro != next.one_euro
            || previous.one_euro_min_cutoff != next.one_euro_min_cutoff
            || previous.one_euro_beta != next.one_euro_beta
            || previous.one_euro_d_cutoff != next.one_euro_d_cutoff
        {
            self.filter = OptionalEyeFilter::new(next.one_euro, next.filter_config());
            self.wide_filter = OptionalEyeFilter::new(next.one_euro, next.filter_config());
        }
        if previous.eye_widening_margin_left != next.eye_widening_margin_left
            || previous.eye_widening_margin_right != next.eye_widening_margin_right
            || previous.min_base_width != next.min_base_width
            || previous.min_wide_width != next.min_wide_width
        {
            self.filter.reset_eyelids();
            self.wide_filter.tracking_inactive();
        }
        if prepared.replace_source {
            self.source = prepared.source;
            self.input = InputStatus::Ready;
            self.tracking = None;
            self.filter.tracking_inactive();
        }
        self.settings = prepared.settings;
        self.target = prepared.target;
        self.socket = prepared.socket;
    }
    /// Revalidate the retained mapping after a rejected ABI transaction, without
    /// consuming a sample or sending output. Tick still validates every read.
    pub(crate) fn revalidate_input(&mut self) {
        if let Some(source) = &self.source
            && let Err(error) = source.validate()
        {
            self.input = classify(&error);
            self.tracking = Some(false);
            self.filter.tracking_inactive();
        }
    }
    pub(crate) fn tick(&mut self) -> Result<(), RuntimeError> {
        self.tick_with_clock(Instant::now)
    }
    fn tick_with_clock(&mut self, clock: impl Fn() -> Instant) -> Result<(), RuntimeError> {
        let now = clock();
        if now.saturating_duration_since(self.last_refresh) >= Duration::from_secs(1) {
            self.last_refresh = now;
            self.stream = detect_stream(&self.proc_root);
            if self.settings.target_mode == TargetMode::Auto || self.target.is_none() {
                match resolve(&self.settings, &self.stream) {
                    Ok(target) => {
                        if target != self.target {
                            match socket(target) {
                                Ok(socket) => {
                                    self.socket = socket;
                                    self.target = target;
                                }
                                Err(error) => {
                                    self.report_error("OSC destination unavailable", &error);
                                    self.socket = None;
                                    self.target = None;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        self.report_error("OSC destination unavailable", &error);
                        self.socket = None;
                        self.target = None;
                    }
                }
            }
            if self.source.is_none() {
                match EyeSource::open(&self.source_path, self.settings.shm_version) {
                    Ok(source) => {
                        self.source = Some(source);
                        self.input = InputStatus::Ready;
                    }
                    Err(error) => self.input = classify(&error),
                }
            }
        }
        let update = match self.source.as_mut() {
            Some(source) => match source.next(Duration::from_millis(10)) {
                Ok(update) => {
                    self.input = if matches!(update, EyeUpdate::Invalid)
                        || (matches!(update, EyeUpdate::NoUpdate)
                            && self.input == InputStatus::InvalidSample)
                    {
                        InputStatus::InvalidSample
                    } else {
                        InputStatus::Ready
                    };
                    update
                }
                Err(error) => {
                    self.input = classify(&error);
                    self.source = None;
                    if let Some(c) = &mut self.calibration {
                        c.invalidate_epoch();
                    }
                    EyeUpdate::Inactive
                }
            },
            None => {
                std::thread::sleep(Duration::from_millis(10));
                EyeUpdate::Inactive
            }
        };
        match &update {
            EyeUpdate::Data(_) => self.tracking = Some(true),
            EyeUpdate::Inactive | EyeUpdate::Invalid => self.tracking = Some(false),
            EyeUpdate::NoUpdate => {}
        }
        let observation = self.source.as_mut().and_then(EyeSource::take_observation);
        // Read the stage clock after the bounded producer wait, preserving the
        // live sampling boundary while allowing deterministic fixture clocks.
        self.advance_calibration(clock(), observation);
        let output = self.calibrated_output(update);
        // A failed stop belongs to exactly its destination/prefix, never to a replacement.
        if self
            .pending_inactive
            .as_ref()
            .is_some_and(|(target, prefix)| {
                Some(*target) != self.target || *prefix != self.settings.prefix
            })
        {
            self.pending_inactive = None;
        }
        match output {
            EyeOutput::Data(values) => {
                self.pending_inactive = None;
                if let Some(socket) = &self.socket
                    && let Err(error) = crate::send_eye_data(
                        socket,
                        &self.settings.prefix,
                        cap_values(values, self.settings.eyelid_output_cap as f32),
                    )
                {
                    let kind = error
                        .downcast_ref::<io::Error>()
                        .map_or(io::ErrorKind::Other, io::Error::kind);
                    if self.errors.allow(kind, Instant::now()) {
                        eprintln!("OSC send failed: {error}");
                    }
                }
            }
            EyeOutput::Inactive => {
                if let Some(target) = self.target {
                    self.pending_inactive = Some((target, self.settings.prefix.clone()));
                }
            }
            EyeOutput::NoUpdate => {}
        }
        if self.pending_inactive.is_some()
            && let Some(socket) = &self.socket
        {
            match crate::send_inactive(socket, &self.settings.prefix) {
                Ok(()) => self.pending_inactive = None,
                Err(error) => {
                    let kind = error
                        .downcast_ref::<io::Error>()
                        .map_or(io::ErrorKind::Other, io::Error::kind);
                    if self.errors.allow(kind, Instant::now()) {
                        eprintln!("OSC stop notification failed: {error}");
                    }
                }
            }
        }
        Ok(())
    }
    fn report_error(&mut self, context: &str, error: &io::Error) {
        if self.errors.allow(error.kind(), Instant::now()) {
            eprintln!("{context}: {error}");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cap_after_filter() {
        let values = EyeValues {
            left: [0.0; 2],
            right: [0.0; 2],
            combined: [0.0; 2],
            eyelids: [0.8, 1.0],
        };
        for cap in [0.0, 0.7, 1.0] {
            assert_eq!(cap_values(values, cap).eyelids, [0.8f32.min(cap), cap]);
        }
    }
    #[test]
    fn priorities() {
        let target = Some("127.0.0.1:9000".parse().unwrap());
        assert_eq!(
            output_reason(InputStatus::Ready, target, Some(true)),
            "sending"
        );
        assert_eq!(
            output_reason(InputStatus::Ready, target, Some(false)),
            "tracking_inactive"
        );
        assert_eq!(
            output_reason(InputStatus::Ready, target, None),
            "tracking_unknown"
        );
        assert_eq!(
            output_reason(InputStatus::Unavailable, None, Some(false)),
            "input_unavailable"
        );
        assert_eq!(
            output_reason(InputStatus::AbiMismatch, None, Some(false)),
            "abi_mismatch"
        );
        assert_eq!(
            output_reason(InputStatus::InvalidSample, None, Some(false)),
            "no_target"
        );
        assert_eq!(
            output_reason(
                InputStatus::InvalidSample,
                Some("127.0.0.1:9000".parse().unwrap()),
                Some(false)
            ),
            "invalid_sample"
        );
    }
}
#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::input::{ShmVersion, fixtures::Fixture};
    #[test]
    fn input_recovery_and_transaction_rollback() {
        let fixture = Fixture::new(4);
        let mut runtime =
            Runtime::with_paths(Settings::default(), &fixture.path, &fixture.dir).unwrap();
        fixture.update(1, 1.0, [0.5; 2]);
        runtime.tick().unwrap();
        assert_eq!(runtime.snapshot().output, "sending");
        let mut candidate = runtime.snapshot().settings;
        candidate.shm_version = ShmVersion::V5;
        assert!(runtime.prepare_settings(&candidate).is_err());
        assert_eq!(runtime.snapshot().settings.shm_version, ShmVersion::Auto);
        fixture.update(2, 1.01, [0.6; 2]);
        runtime.tick().unwrap();
        assert_eq!(runtime.snapshot().output, "sending");
        std::fs::remove_file(&fixture.path).unwrap();
        runtime.tick().unwrap();
        assert_eq!(runtime.snapshot().output, "input_unavailable");
        std::fs::copy(
            format!(
                "/proc/self/fd/{}",
                std::os::fd::AsRawFd::as_raw_fd(&fixture.file)
            ),
            &fixture.path,
        )
        .unwrap();
        runtime.last_refresh = Instant::now() - Duration::from_secs(2);
        runtime.tick().unwrap();
        assert_eq!(runtime.snapshot().input, InputStatus::Ready);
        use std::os::unix::fs::FileExt;
        let reopened = std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.path)
            .unwrap();
        reopened
            .write_all_at(&1.02f64.to_le_bytes(), (fixture.offset + 5) as u64)
            .unwrap();
        reopened.write_all_at(&3u32.to_le_bytes(), 0x38).unwrap();
        runtime.tick().unwrap();
        assert_eq!(runtime.snapshot().output, "sending");
    }
    #[test]
    fn actual_osc_cap_and_switch_without_old_stop() {
        let fixture = Fixture::new(4);
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let settings = Settings {
            target_port: receiver.local_addr().unwrap().port(),
            one_euro: true,
            eyelid_output_cap: 0.7,
            ..Settings::default()
        };
        let mut runtime = Runtime::with_paths(settings, &fixture.path, &fixture.dir).unwrap();
        fixture.update(1, 1.0, [1.0; 2]);
        runtime.tick().unwrap();
        let mut buf = [0; 1024];
        let mut lids = 0;
        for _ in 0..9 {
            let n = receiver.recv(&mut buf).unwrap();
            if buf[..n].windows(6).any(|w| w == b"EyeLid") {
                assert_eq!(f32::from_be_bytes(buf[n - 4..n].try_into().unwrap()), 0.7);
                lids += 1;
            }
        }
        assert_eq!(lids, 2);
        let receiver2 = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut candidate = runtime.snapshot().settings;
        candidate.target_port = receiver2.local_addr().unwrap().port();
        let prepared = runtime.prepare_settings(&candidate).unwrap();
        runtime.apply_settings(prepared);
        receiver.set_nonblocking(true).unwrap();
        assert_eq!(
            receiver.recv(&mut buf).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    #[test]
    fn unrelated_settings_can_change_while_input_restarts() {
        let fixture = Fixture::new(4);
        let mut runtime =
            Runtime::with_paths(Settings::default(), &fixture.path, &fixture.dir).unwrap();
        std::fs::remove_file(&fixture.path).unwrap();
        let mut candidate = runtime.snapshot().settings;
        candidate.eyelid_output_cap = 0.7;
        let prepared = runtime.prepare_settings(&candidate).unwrap();
        runtime.apply_settings(prepared);
        runtime.tick().unwrap();
        assert_eq!(runtime.snapshot().settings.eyelid_output_cap, 0.7);
        assert_eq!(runtime.snapshot().input, InputStatus::Unavailable);
        candidate.shm_version = ShmVersion::V5;
        assert!(runtime.prepare_settings(&candidate).is_err());
    }
    #[test]
    fn manual_refresh_keeps_resolved_destination() {
        let fixture = Fixture::new(4);
        let mut runtime =
            Runtime::with_paths(Settings::default(), &fixture.path, &fixture.dir).unwrap();
        crate::target::detector_tests::process(
            &fixture.dir,
            "9272",
            "159857",
            include_str!("../docs/research/fixtures/steamlink-adapter-v4/udp-streaming.txt"),
        );
        let old_target = runtime.snapshot().osc_target;
        // Simulate a hostname that can no longer resolve after the socket was prepared.
        runtime.settings.target_host = "invalid.invalid".into();
        runtime.last_refresh = Instant::now() - Duration::from_secs(2);
        runtime.tick().unwrap();
        assert_eq!(runtime.snapshot().osc_target, old_target);
        assert_eq!(
            runtime.snapshot().stream.peer.unwrap().to_string(),
            "192.0.2.20:10400"
        );
    }
    #[test]
    fn auto_without_peer_is_accepted_and_uses_configured_port() {
        let fixture = Fixture::new(4);
        let settings = Settings {
            target_mode: TargetMode::Auto,
            ..Settings::default()
        };
        let runtime = Runtime::with_paths(settings.clone(), &fixture.path, &fixture.dir).unwrap();
        assert!(runtime.prepare_settings(&settings).is_ok());
        assert_eq!(runtime.snapshot().osc_target, None);
        let stream = StreamObservation {
            state: crate::target::StreamState::Detected,
            peer: Some("192.0.2.20:10400".parse().unwrap()),
        };
        assert_eq!(resolve(&settings, &stream).unwrap().unwrap().port(), 9000);
        let manual = Settings::default();
        assert_eq!(
            resolve(&manual, &stream).unwrap().unwrap().to_string(),
            "127.0.0.1:9000"
        );
    }
}
#[cfg(test)]
mod review_runtime_tests {
    use super::*;
    use crate::input::fixtures::Fixture;
    use crate::target::detector_tests::process;
    use std::os::unix::fs::FileExt;
    #[test]
    fn unresolved_manual_host_waits_and_retries() {
        let fixture = Fixture::new(4);
        let settings = Settings {
            target_host: "invalid.invalid".into(),
            ..Settings::default()
        };
        let mut runtime = Runtime::with_paths(settings, &fixture.path, &fixture.dir).unwrap();
        assert_eq!(runtime.snapshot().osc_target, None);
        assert_eq!(runtime.snapshot().output, "no_target");
        runtime.settings.target_host = "127.0.0.1".into();
        runtime.last_refresh = Instant::now() - Duration::from_secs(2);
        runtime.tick().unwrap();
        assert_eq!(
            runtime.snapshot().osc_target.unwrap().ip().to_string(),
            "127.0.0.1"
        );
    }
    #[test]
    fn failed_inactive_notification_is_retried() {
        let fixture = Fixture::new(4);
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let settings = Settings {
            target_port: receiver.local_addr().unwrap().port(),
            ..Settings::default()
        };
        let mut runtime = Runtime::with_paths(settings, &fixture.path, &fixture.dir).unwrap();
        runtime.socket = Some(UdpSocket::bind("127.0.0.1:0").unwrap());
        fixture.update(1, 1.0, [0.7; 2]);
        runtime.tick().unwrap();
        std::fs::remove_file(&fixture.path).unwrap();
        runtime.tick().unwrap();
        runtime.socket = socket(runtime.target).unwrap();
        runtime.tick().unwrap();
        let mut bytes = [0; 512];
        let n = receiver.recv(&mut bytes).unwrap();
        assert!(bytes[..n].windows(3).any(|w| w == b",F\0"));
        receiver.set_nonblocking(true).unwrap();
        runtime.tick().unwrap();
        assert_eq!(
            receiver.recv(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    #[test]
    fn real_reopened_input_sends_new_data() {
        let fixture = Fixture::new(4);
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let settings = Settings {
            target_port: receiver.local_addr().unwrap().port(),
            ..Settings::default()
        };
        let mut runtime = Runtime::with_paths(settings, &fixture.path, &fixture.dir).unwrap();
        std::fs::remove_file(&fixture.path).unwrap();
        runtime.tick().unwrap();
        std::fs::copy(
            format!(
                "/proc/self/fd/{}",
                std::os::fd::AsRawFd::as_raw_fd(&fixture.file)
            ),
            &fixture.path,
        )
        .unwrap();
        runtime.last_refresh = Instant::now() - Duration::from_secs(2);
        runtime.tick().unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.path)
            .unwrap();
        file.write_all_at(&1u32.to_le_bytes(), fixture.offset as u64)
            .unwrap();
        file.write_all_at(&2.0f64.to_le_bytes(), (fixture.offset + 5) as u64)
            .unwrap();
        file.write_all_at(&1u32.to_le_bytes(), 0x38).unwrap();
        runtime.tick().unwrap();
        assert_eq!(runtime.snapshot().output, "sending");
        let mut bytes = [0; 512];
        let n = receiver.recv(&mut bytes).unwrap();
        assert!(bytes[..n].windows(3).any(|w| w == b",T\0"));
    }
    #[test]
    fn runtime_manual_and_auto_observe_stop_restart() {
        let fixture = Fixture::new(4);
        let row = include_str!("../docs/research/fixtures/steamlink-adapter-v4/udp-streaming.txt");
        process(&fixture.dir, "9272", "159857", row);
        let mut manual =
            Runtime::with_paths(Settings::default(), &fixture.path, &fixture.dir).unwrap();
        assert_eq!(
            manual.snapshot().stream.peer.unwrap().to_string(),
            "192.0.2.20:10400"
        );
        assert_eq!(
            manual.snapshot().osc_target.unwrap().to_string(),
            "127.0.0.1:9000"
        );
        let mut auto = Runtime::with_paths(
            Settings {
                target_mode: TargetMode::Auto,
                ..Settings::default()
            },
            &fixture.path,
            &fixture.dir,
        )
        .unwrap();
        assert_eq!(
            auto.snapshot().osc_target.unwrap().to_string(),
            "192.0.2.20:9000"
        );
        std::fs::remove_dir_all(fixture.dir.join("9272")).unwrap();
        auto.last_refresh = Instant::now() - Duration::from_secs(2);
        auto.tick().unwrap();
        assert_eq!(auto.snapshot().osc_target, None);
        manual.last_refresh = Instant::now() - Duration::from_secs(2);
        manual.tick().unwrap();
        assert_eq!(manual.snapshot().stream.peer, None);
        assert_eq!(
            manual.snapshot().osc_target.unwrap().to_string(),
            "127.0.0.1:9000"
        );
        let row = include_str!("../docs/research/fixtures/steamlink-adapter-v4/udp-restarted.txt");
        process(&fixture.dir, "9683", "160449", row);
        auto.last_refresh = Instant::now() - Duration::from_secs(2);
        auto.tick().unwrap();
        assert_eq!(
            auto.snapshot().osc_target.unwrap().to_string(),
            "192.0.2.20:9000"
        );
        assert!(auto.socket.is_some());
        manual.last_refresh = Instant::now() - Duration::from_secs(2);
        manual.tick().unwrap();
        assert_eq!(
            manual.snapshot().stream.peer.unwrap().to_string(),
            "192.0.2.20:10400"
        );
        assert_eq!(
            manual.snapshot().osc_target.unwrap().to_string(),
            "127.0.0.1:9000"
        );
    }
}
#[derive(Default)]
struct ErrorLog {
    last: HashMap<io::ErrorKind, Instant>,
}
impl ErrorLog {
    fn allow(&mut self, kind: io::ErrorKind, now: Instant) -> bool {
        if self
            .last
            .get(&kind)
            .is_some_and(|last| now.saturating_duration_since(*last) < Duration::from_secs(1))
        {
            return false;
        }
        self.last.insert(kind, now);
        true
    }
}
#[cfg(test)]
mod log_tests {
    use super::*;
    #[test]
    fn repeated_errors_are_limited_per_kind() {
        let mut log = ErrorLog::default();
        let now = Instant::now();
        assert!(log.allow(io::ErrorKind::ConnectionRefused, now));
        assert!(!log.allow(
            io::ErrorKind::ConnectionRefused,
            now + Duration::from_millis(10)
        ));
        assert!(log.allow(io::ErrorKind::WouldBlock, now));
        assert!(log.allow(
            io::ErrorKind::ConnectionRefused,
            now + Duration::from_secs(1)
        ));
    }
}
#[cfg(test)]
mod raw_send_tests {
    use super::*;
    use crate::input::fixtures::Fixture;
    #[test]
    fn finite_raw_excursions_are_clamped_over_udp() {
        let fixture = Fixture::new(4);
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let settings = Settings {
            target_port: receiver.local_addr().unwrap().port(),
            ..Settings::default()
        };
        let mut runtime = Runtime::with_paths(settings, &fixture.path, &fixture.dir).unwrap();
        fixture.update(1, 1.0, [1.0001, -0.0001]);
        runtime.tick().unwrap();
        assert_eq!(runtime.snapshot().output, "sending");
        let mut bytes = [0; 512];
        let mut lids = Vec::new();
        for _ in 0..9 {
            let n = receiver.recv(&mut bytes).unwrap();
            if bytes[..n].windows(6).any(|w| w == b"EyeLid") {
                lids.push(f32::from_be_bytes(bytes[n - 4..n].try_into().unwrap()));
            }
        }
        assert_eq!(lids, [1.0, 0.0]);
    }
}
#[cfg(test)]
mod stop_switch_tests {
    use super::*;
    use crate::input::fixtures::Fixture;
    #[test]
    fn pending_stop_is_discarded_on_target_or_prefix_switch() {
        for change_target in [false, true] {
            let fixture = Fixture::new(4);
            let old = UdpSocket::bind("127.0.0.1:0").unwrap();
            let new = UdpSocket::bind("127.0.0.1:0").unwrap();
            let settings = Settings {
                target_port: old.local_addr().unwrap().port(),
                ..Settings::default()
            };
            let mut runtime = Runtime::with_paths(settings, &fixture.path, &fixture.dir).unwrap();
            runtime.socket = Some(UdpSocket::bind("127.0.0.1:0").unwrap());
            fixture.update(1, 1.0, [0.7; 2]);
            runtime.tick().unwrap();
            fixture.update(2, 1.01, [0.7; 2]);
            fixture.write(fixture.offset as u64, &0u32.to_le_bytes());
            runtime.tick().unwrap();
            assert!(runtime.pending_inactive.is_some());
            let mut candidate = runtime.snapshot().settings;
            if change_target {
                candidate.target_port = new.local_addr().unwrap().port();
            } else {
                candidate.prefix = "/New".into();
            }
            let prepared = runtime.prepare_settings(&candidate).unwrap();
            runtime.apply_settings(prepared);
            runtime.tick().unwrap();
            assert!(runtime.pending_inactive.is_none());
            old.set_nonblocking(true).unwrap();
            new.set_nonblocking(true).unwrap();
            let mut bytes = [0; 512];
            assert_eq!(
                old.recv(&mut bytes).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            assert_eq!(
                new.recv(&mut bytes).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
    }
}

#[cfg(test)]
mod calibrated_output_tests {
    use super::*;
    use crate::input::fixtures::Fixture;
    #[test]
    fn calibrated_udp_natural_wide_toggle_and_immediate_cap() {
        let fixture = Fixture::new(4);
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let settings = Settings {
            target_port: receiver.local_addr().unwrap().port(),
            ..Settings::default()
        };
        let mut runtime = Runtime::with_paths(settings, &fixture.path, &fixture.dir).unwrap();
        let config = fixture.dir.join("settings.json");
        let profile = crate::calibration::profile_path(&config);
        std::fs::write(profile,r#"{"left":{"closed":0.0,"relaxed":0.5,"relaxed_max":0.5,"wide":1.0},"right":{"closed":0.0,"relaxed":0.5,"relaxed_max":0.5,"wide":1.0}}"#).unwrap();
        runtime.load_profile(&config);
        fn lids(receiver: &UdpSocket) -> [f32; 2] {
            let mut values = Vec::new();
            let mut buf = [0; 1024];
            for _ in 0..9 {
                let n = receiver.recv(&mut buf).unwrap();
                if buf[..n].windows(6).any(|x| x == b"EyeLid") {
                    values.push(f32::from_be_bytes(buf[n - 4..n].try_into().unwrap()));
                }
            }
            values.try_into().unwrap()
        }
        fixture.update(1, 1.0, [0.5; 2]);
        runtime.tick().unwrap();
        assert_eq!(lids(&receiver), [0.75; 2]);
        fixture.update(2, 1.01, [1.0; 2]);
        runtime.tick().unwrap();
        assert_eq!(lids(&receiver), [1.0; 2]);
        let mut candidate = runtime.snapshot().settings;
        candidate.eye_widening = false;
        runtime.apply_settings(runtime.prepare_settings(&candidate).unwrap());
        fixture.update(3, 1.02, [1.0; 2]);
        runtime.tick().unwrap();
        assert_eq!(lids(&receiver), [0.75; 2]);
        candidate.eyelid_output_cap = 0.4;
        candidate.one_euro = true;
        runtime.apply_settings(runtime.prepare_settings(&candidate).unwrap());
        fixture.update(4, 1.03, [1.0; 2]);
        runtime.tick().unwrap();
        assert_eq!(lids(&receiver), [0.4; 2]);
        candidate.min_base_width = 0.6;
        assert!(runtime.prepare_settings(&candidate).is_err());
        assert_eq!(runtime.snapshot().settings.min_base_width, 0.1);
    }
}

#[cfg(test)]
mod calibrated_filter_tests {
    use super::*;
    use crate::input::{Observation, fixtures::Fixture};
    use serde_json::{Value, json};
    fn profile_file(config: &Path) {
        std::fs::write(crate::calibration::profile_path(config),r#"{"left":{"closed":0.0,"relaxed":0.5,"relaxed_max":0.5,"wide":null},"right":{"closed":0.0,"relaxed":0.5,"relaxed_max":0.5,"wide":null}}"#).unwrap();
    }
    #[test]
    fn widening_disabled_still_updates_independent_wide_filters() {
        let f = Fixture::new(4);
        let settings = Settings {
            one_euro: true,
            ..Settings::default()
        };
        let mut r = Runtime::with_paths(settings, &f.path, &f.dir).unwrap();
        let config = f.dir.join("settings.json");
        std::fs::write(crate::calibration::profile_path(&config),r#"{"left":{"closed":0.0,"relaxed":0.5,"relaxed_max":0.5,"wide":1.0},"right":{"closed":0.0,"relaxed":0.5,"relaxed_max":0.5,"wide":1.0}}"#).unwrap();
        r.load_profile(&config);
        let make = |time, raw| {
            EyeUpdate::Data(crate::EyeData {
                sample_time: time,
                gaze: [[0.0, 0.0, -1.0]; 2],
                fixation_point: [0.0, 0.0, -1.0],
                openness: raw,
            })
        };
        let EyeOutput::Data(values) = r.calibrated_output(make(1.0, [1.0, 0.5])) else {
            panic!()
        };
        assert_eq!(values.eyelids, [1.0, 0.75]);
        let mut settings = r.settings.clone();
        settings.eye_widening = false;
        r.apply_settings(r.prepare_settings(&settings).unwrap());
        for i in 1..=200 {
            r.calibrated_output(make(1.0 + i as f64 * 0.01, [0.5; 2]));
        }
        settings.eye_widening = true;
        r.apply_settings(r.prepare_settings(&settings).unwrap());
        let EyeOutput::Data(values) = r.calibrated_output(make(3.01, [0.5; 2])) else {
            panic!()
        };
        assert!((values.eyelids[0] - 0.75).abs() < 0.001);
        assert_eq!(values.eyelids[1], 0.75);
    }
    #[test]
    fn calibrated_nonfinite_eye_stops_both_channels_and_resets_history() {
        let f = Fixture::new(4);
        let settings = Settings {
            one_euro: true,
            ..Settings::default()
        };
        let mut r = Runtime::with_paths(settings, &f.path, &f.dir).unwrap();
        let config = f.dir.join("settings.json");
        profile_file(&config);
        r.load_profile(&config);
        let make = |time, raw| {
            EyeUpdate::Data(crate::EyeData {
                sample_time: time,
                gaze: [[0.0, 0.0, -1.0]; 2],
                fixation_point: [0.0, 0.0, -1.0],
                openness: raw,
            })
        };
        r.calibrated_output(make(1.0, [0.0; 2]));
        assert!(matches!(
            r.calibrated_output(make(1.01, [f32::NAN, 0.5])),
            EyeOutput::Inactive
        ));
        assert!(!r.active);
        let EyeOutput::Data(values) = r.calibrated_output(make(1.02, [0.5; 2])) else {
            panic!()
        };
        assert_eq!(values.eyelids, [0.75; 2]);
    }
    #[test]
    fn identical_recalibration_resets_eyelid_history() {
        let f = Fixture::new(4);
        let settings = Settings {
            one_euro: true,
            ..Settings::default()
        };
        let mut r = Runtime::with_paths(settings.clone(), &f.path, &f.dir).unwrap();
        let config = f.dir.join("settings.json");
        profile_file(&config);
        r.load_profile(&config);
        let make = |time, raw| {
            EyeUpdate::Data(crate::EyeData {
                sample_time: time,
                gaze: [[0.0, 0.0, -1.0]; 2],
                fixation_point: [0.0, 0.0, -1.0],
                openness: [raw; 2],
            })
        };
        r.calibrated_output(make(1.0, 0.0));
        let EyeOutput::Data(v) = r.calibrated_output(make(1.01, 0.5)) else {
            panic!()
        };
        assert!(v.eyelids[0] < 0.75);
        r.calibration_command("calibration.start", &json!({}), Value::Null)
            .unwrap();
        let now = Instant::now();
        let c = r.calibration.as_mut().unwrap();
        let id = c.snapshot()["session_id"].clone();
        for (stage, raw, offset) in [("closed", 0.0, 0), ("relaxed", 0.5, 8)] {
            let began = now + Duration::from_secs(offset);
            c.command(
                "calibration.begin_stage",
                &json!({"session_id":id,"stage":stage}),
                Value::Null,
                began,
                &settings,
            )
            .unwrap();
            for i in 0..100 {
                c.advance(
                    began + Duration::from_millis(2000 + i * 50),
                    Some(Observation {
                        sequence: i as u32,
                        timestamp: 10.0 + i as f64 * 0.05,
                        openness: Some([raw; 2]),
                    }),
                    &settings,
                );
            }
            c.advance(began + Duration::from_secs(7), None, &settings);
        }
        r.calibration_command(
            "calibration.skip_wide",
            &json!({"session_id":id}),
            Value::Null,
        )
        .unwrap();
        let EyeOutput::Data(v) = r.calibrated_output(make(1.02, 0.5)) else {
            panic!()
        };
        assert_eq!(v.eyelids, [0.75; 2]);
    }
}

#[cfg(test)]
mod full_tick_calibration_tests {
    use super::*;
    use crate::input::fixtures::Fixture;
    use serde_json::{Value, json};
    fn begin(runtime: &mut Runtime, stage: &str, now: Instant) {
        let id = runtime.snapshot().calibration["session_id"].clone();
        let settings = runtime.settings.clone();
        runtime
            .calibration
            .as_mut()
            .unwrap()
            .command(
                "calibration.begin_stage",
                &json!({"session_id":id,"stage":stage}),
                json!({"connection_id":"p1","request_id":stage}),
                now,
                &settings,
            )
            .unwrap();
    }
    fn eyelids(receiver: &UdpSocket) -> [f32; 2] {
        let mut values = Vec::new();
        let mut buffer = [0; 1024];
        for _ in 0..9 {
            let n = receiver.recv(&mut buffer).unwrap();
            if buffer[..n].windows(6).any(|x| x == b"EyeLid") {
                values.push(f32::from_be_bytes(buffer[n - 4..n].try_into().unwrap()));
            }
        }
        values.try_into().unwrap()
    }
    fn drive(
        runtime: &mut Runtime,
        fixture: &Fixture,
        stage: &str,
        now: Instant,
        samples: [[f32; 2]; 2],
        sequence: &mut u32,
        receiver: Option<&UdpSocket>,
    ) -> Option<[f32; 2]> {
        let [raw, last] = samples;
        begin(runtime, stage, now);
        *sequence += 1;
        fixture.update(*sequence, 100.0 + f64::from(*sequence) / 90.0, [1.0; 2]);
        runtime
            .tick_with_clock(|| now + Duration::from_secs(1))
            .unwrap();
        if let Some(r) = receiver {
            eyelids(r);
        }
        for i in 0..450 {
            *sequence += 1;
            fixture.update(
                *sequence,
                100.0 + f64::from(*sequence) / 90.0,
                if stage == "relaxed" && i == 449 {
                    [0.75, 0.875]
                } else {
                    raw
                },
            );
            runtime
                .tick_with_clock(|| now + Duration::from_secs_f64(2.0 + f64::from(i) / 90.0))
                .unwrap();
            if let Some(r) = receiver {
                eyelids(r);
            }
        }
        *sequence += 1;
        fixture.update(*sequence, 100.0 + f64::from(*sequence) / 90.0, last);
        runtime
            .tick_with_clock(|| now + Duration::from_secs(7))
            .unwrap();
        receiver.map(eyelids)
    }
    #[test]
    fn full_tick_uses_post_read_clock_for_stage_boundaries() {
        let f = Fixture::new(4);
        let settings = Settings {
            target_mode: TargetMode::Auto,
            ..Settings::default()
        };
        let mut r = Runtime::with_paths(settings, &f.path, &f.dir).unwrap();
        r.load_profile(&f.dir.join("settings.json"));
        r.calibration_command("calibration.start", &json!({}), Value::Null)
            .unwrap();
        let now = Instant::now();
        begin(&mut r, "closed", now);
        f.update(1, 100.0, [0.0; 2]);
        let calls = std::cell::Cell::new(0);
        r.tick_with_clock(|| {
            let call = calls.get();
            calls.set(call + 1);
            now + Duration::from_millis(if call == 0 { 1990 } else { 2000 })
        })
        .unwrap();
        assert_eq!(r.snapshot().calibration["state"], "measuring");
        assert_eq!(r.snapshot().calibration["remaining_ms"], 5000);
    }
    #[test]
    fn full_tick_collects_unique_ninety_hz_samples_and_excludes_preparation() {
        let f = Fixture::new(4);
        let settings = Settings {
            target_mode: TargetMode::Auto,
            ..Settings::default()
        };
        let mut r = Runtime::with_paths(settings, &f.path, &f.dir).unwrap();
        r.load_profile(&f.dir.join("settings.json"));
        r.calibration_command("calibration.start", &json!({}), Value::Null)
            .unwrap();
        let now = Instant::now();
        let mut sequence = 0;
        drive(
            &mut r,
            &f,
            "closed",
            now,
            [[0.0; 2], [1.0; 2]],
            &mut sequence,
            None,
        );
        assert_eq!(r.snapshot().calibration["stage"], "relaxed");
        drive(
            &mut r,
            &f,
            "relaxed",
            now + Duration::from_secs(8),
            [[0.5, 0.625], [1.0; 2]],
            &mut sequence,
            None,
        );
        assert_eq!(r.snapshot().calibration["stage"], "wide");
        let id = r.snapshot().calibration["session_id"].clone();
        r.calibration_command(
            "calibration.skip_wide",
            &json!({"session_id":id}),
            Value::Null,
        )
        .unwrap();
        assert_eq!(r.snapshot().profile["left"]["closed"], 0.0);
        assert_eq!(r.snapshot().profile["left"]["relaxed"], 0.5);
        assert_eq!(r.snapshot().profile["right"]["relaxed"], 0.625);
        assert_eq!(r.snapshot().profile["left"]["relaxed_max"], 0.75);
        assert_eq!(r.snapshot().profile["right"]["relaxed_max"], 0.875);
    }
    #[test]
    fn full_tick_producer_sequence_restart_fails_current_stage() {
        let f = Fixture::new(4);
        let settings = Settings {
            target_mode: TargetMode::Auto,
            ..Settings::default()
        };
        let mut r = Runtime::with_paths(settings, &f.path, &f.dir).unwrap();
        r.load_profile(&f.dir.join("settings.json"));
        r.calibration_command("calibration.start", &json!({}), Value::Null)
            .unwrap();
        let now = Instant::now();
        begin(&mut r, "closed", now);
        for i in 0..450 {
            let sequence = if i < 100 { 1000 + i } else { i - 99 };
            f.update(sequence, 100.0 + f64::from(i) / 90.0, [0.0; 2]);
            r.tick_with_clock(|| now + Duration::from_secs_f64(2.0 + f64::from(i) / 90.0))
                .unwrap();
        }
        r.tick_with_clock(|| now + Duration::from_secs(7)).unwrap();
        assert_eq!(r.snapshot().calibration["stage"], "closed");
        assert_eq!(r.snapshot().calibration["state"], "waiting");
        assert_eq!(
            r.snapshot().calibration["result"]["reason"],
            "insufficient_samples"
        );
    }
    #[test]
    fn full_tick_mmap_replacement_fails_stage_despite_recovered_valid_coverage() {
        use std::os::{fd::AsRawFd, unix::fs::FileExt};
        let f = Fixture::new(4);
        let settings = Settings {
            target_mode: TargetMode::Auto,
            ..Settings::default()
        };
        let mut r = Runtime::with_paths(settings, &f.path, &f.dir).unwrap();
        r.load_profile(&f.dir.join("settings.json"));
        r.calibration_command("calibration.start", &json!({}), Value::Null)
            .unwrap();
        let now = Instant::now();
        begin(&mut r, "closed", now);
        for i in 0..90 {
            f.update(1000 + i, 100.0 + f64::from(i) / 90.0, [0.0; 2]);
            r.tick_with_clock(|| now + Duration::from_secs_f64(2.0 + f64::from(i) / 90.0))
                .unwrap();
        }
        std::fs::remove_file(&f.path).unwrap();
        r.tick_with_clock(|| now + Duration::from_secs(3)).unwrap();
        assert_eq!(r.snapshot().input, InputStatus::Unavailable);
        std::fs::copy(format!("/proc/self/fd/{}", f.file.as_raw_fd()), &f.path).unwrap();
        r.tick_with_clock(|| now + Duration::from_secs(4)).unwrap();
        assert_eq!(r.snapshot().input, InputStatus::Ready);
        let replacement = std::fs::OpenOptions::new()
            .write(true)
            .open(&f.path)
            .unwrap();
        for i in 0..400 {
            replacement
                .write_all_at(&1u32.to_le_bytes(), f.offset as u64)
                .unwrap();
            replacement
                .write_all_at(
                    &(101.0 + f64::from(i) / 90.0).to_le_bytes(),
                    (f.offset + 5) as u64,
                )
                .unwrap();
            replacement
                .write_all_at(&(1090u32 + i).to_le_bytes(), 0x38)
                .unwrap();
            r.tick_with_clock(|| now + Duration::from_secs_f64(4.0 + f64::from(i) * 3.0 / 400.0))
                .unwrap();
        }
        r.tick_with_clock(|| now + Duration::from_secs(7)).unwrap();
        assert_eq!(r.snapshot().calibration["state"], "waiting");
        assert_eq!(r.snapshot().calibration["stage"], "closed");
        assert_eq!(
            r.snapshot().calibration["result"]["reason"],
            "insufficient_samples"
        );
    }
    #[test]
    fn full_tick_async_wide_save_failure_retains_profile_filter_history_and_ipc_cause() {
        // Root bypasses directory write permissions, so it cannot exercise this failure.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        use std::{
            io::{Read, Write},
            os::unix::{fs::PermissionsExt, net::UnixStream},
        };
        let f = Fixture::new(4);
        std::fs::set_permissions(&f.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let savedir = f.dir.join("saved");
        std::fs::create_dir(&savedir).unwrap();
        let config = savedir.join("settings.json");
        std::fs::write(&config, b"{}").unwrap();
        let profile = crate::calibration::profile_path(&config);
        let old_bytes=br#"{"left":{"closed":0.0,"relaxed":1.0,"relaxed_max":1.0,"wide":null},"right":{"closed":0.0,"relaxed":1.0,"relaxed_max":1.0,"wide":null}}"#;
        std::fs::write(&profile, old_bytes).unwrap();
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let settings = Settings {
            one_euro: true,
            target_port: receiver.local_addr().unwrap().port(),
            ..Settings::default()
        };
        let mut r = Runtime::with_paths(settings, &f.path, &f.dir).unwrap();
        r.load_profile(&config);
        let old = r.snapshot().profile;
        let (store, _) =
            crate::settings::SettingsStore::load(config.clone(), &serde_json::Map::new(), false)
                .unwrap();
        let socket = f.dir.join("control.sock");
        let mut server = crate::ipc::IpcServer::bind(socket.clone(), store).unwrap();
        let mut client = UnixStream::connect(socket).unwrap();
        client.set_nonblocking(true).unwrap();
        client.write_all(b"{\"v\":1,\"type\":\"request\",\"id\":\"get\",\"method\":\"state.get\",\"params\":{}}\n").unwrap();
        fn notices(
            server: &mut crate::ipc::IpcServer,
            r: &mut Runtime,
            client: &mut UnixStream,
        ) -> Vec<Value> {
            for _ in 0..5 {
                server.poll(r).unwrap();
            }
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                match client.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => panic!("{e}"),
                }
            }
            bytes
                .split(|b| *b == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice(line).unwrap())
                .collect()
        }
        notices(&mut server, &mut r, &mut client);
        r.calibration_command(
            "calibration.start",
            &json!({}),
            json!({"connection_id":"p1","request_id":"start"}),
        )
        .unwrap();
        let now = Instant::now();
        let mut sequence = 0;
        drive(
            &mut r,
            &f,
            "closed",
            now,
            [[0.0; 2], [0.0; 2]],
            &mut sequence,
            Some(&receiver),
        );
        drive(
            &mut r,
            &f,
            "relaxed",
            now + Duration::from_secs(8),
            [[0.5, 0.625], [0.5, 0.625]],
            &mut sequence,
            Some(&receiver),
        );
        notices(&mut server, &mut r, &mut client);
        std::fs::set_permissions(&savedir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let values = drive(
            &mut r,
            &f,
            "wide",
            now + Duration::from_secs(16),
            [[1.0; 2], [0.0; 2]],
            &mut sequence,
            Some(&receiver),
        )
        .unwrap();
        std::fs::set_permissions(&savedir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(r.snapshot().calibration["state"], "failed");
        assert_eq!(r.snapshot().calibration["result"]["reason"], "save_failed");
        assert_eq!(r.snapshot().profile, old);
        assert_eq!(std::fs::read(&profile).unwrap(), old_bytes);
        assert_eq!(std::fs::read(config).unwrap(), b"{}");
        assert!(
            values.iter().all(|value| *value > 0.0 && *value < 0.75),
            "old One Euro history must remain: {values:?}"
        );
        assert_eq!(r.profile_change().0, 0);
        let messages = notices(&mut server, &mut r, &mut client);
        assert!(!messages.iter().any(|v| v["event"] == "state.changed"));
        let failed = messages
            .iter()
            .find(|v| v["event"] == "calibration.changed")
            .unwrap();
        assert_eq!(failed["data"]["calibration"]["state"], "failed");
        assert_eq!(
            failed["data"]["calibration"]["result"]["reason"],
            "save_failed"
        );
        assert_eq!(
            failed["caused_by"],
            json!({"connection_id":"p1","request_id":"wide"})
        );
    }
    #[test]
    fn full_tick_async_wide_completion_resets_one_euro_before_udp_output() {
        let f = Fixture::new(4);
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let settings = Settings {
            one_euro: true,
            target_port: receiver.local_addr().unwrap().port(),
            ..Settings::default()
        };
        let mut r = Runtime::with_paths(settings, &f.path, &f.dir).unwrap();
        let config = f.dir.join("settings.json");
        std::fs::write(crate::calibration::profile_path(&config),r#"{"left":{"closed":0.0,"relaxed":1.0,"relaxed_max":1.0,"wide":null},"right":{"closed":0.0,"relaxed":1.0,"relaxed_max":1.0,"wide":null}}"#).unwrap();
        r.load_profile(&config);
        r.calibration_command("calibration.start", &json!({}), Value::Null)
            .unwrap();
        let now = Instant::now();
        let mut sequence = 0;
        drive(
            &mut r,
            &f,
            "closed",
            now,
            [[0.0; 2], [0.0; 2]],
            &mut sequence,
            Some(&receiver),
        );
        drive(
            &mut r,
            &f,
            "relaxed",
            now + Duration::from_secs(8),
            [[0.5, 0.625], [0.5, 0.625]],
            &mut sequence,
            Some(&receiver),
        );
        let values = drive(
            &mut r,
            &f,
            "wide",
            now + Duration::from_secs(16),
            [[1.0; 2], [0.0; 2]],
            &mut sequence,
            Some(&receiver),
        )
        .unwrap();
        assert_eq!(r.snapshot().calibration["state"], "completed");
        assert_eq!(values, [0.0; 2]);
        assert_eq!(r.snapshot().profile["left"]["wide"], 1.0);
    }
}
