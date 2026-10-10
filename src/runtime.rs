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
    pub input: InputStatus,
    pub tracking: Option<bool>,
    pub stream: StreamObservation,
    pub osc_target: Option<SocketAddr>,
    pub output: &'static str,
}
pub(crate) type RuntimeError = Box<dyn std::error::Error>;
// Consumed by the settings/IPC transaction layer in the next task.
#[allow(dead_code)]
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
    active: bool,
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
            active: false,
            last_refresh: Instant::now(),
            pending_inactive: None,
            errors: ErrorLog::default(),
        })
    }
    pub(crate) fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            settings: self.settings.clone(),
            input: self.input,
            tracking: self.tracking,
            stream: self.stream.clone(),
            osc_target: self.target,
            output: output_reason(self.input, self.target, self.tracking),
        }
    }
    #[allow(dead_code)] // Shared transaction API; standalone mode has no mutations.
    pub(crate) fn prepare_settings(
        &self,
        candidate: &Settings,
    ) -> Result<PreparedSettings, RuntimeError> {
        candidate.validate()?;
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
    #[allow(dead_code)] // Shared transaction API; standalone mode has no mutations.
    pub(crate) fn apply_settings(&mut self, prepared: PreparedSettings) {
        let previous = &self.settings;
        let next = &prepared.settings;
        if previous.one_euro != next.one_euro
            || previous.one_euro_min_cutoff != next.one_euro_min_cutoff
            || previous.one_euro_beta != next.one_euro_beta
            || previous.one_euro_d_cutoff != next.one_euro_d_cutoff
        {
            self.filter = OptionalEyeFilter::new(next.one_euro, next.filter_config());
        }
        if prepared.replace_source {
            self.source = prepared.source;
            self.input = InputStatus::Ready;
            self.tracking = None;
            self.active = false;
            self.filter.tracking_inactive();
        }
        self.settings = prepared.settings;
        self.target = prepared.target;
        self.socket = prepared.socket;
    }
    pub(crate) fn tick(&mut self) -> Result<(), RuntimeError> {
        if self.last_refresh.elapsed() >= Duration::from_secs(1) {
            self.last_refresh = Instant::now();
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
        let output = process_eye_update(update, &mut self.filter, &mut self.active);
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
                        cap_values(values, self.settings.eyelid_output_cap),
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
