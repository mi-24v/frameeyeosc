use crate::{filter::OneEuroConfig, input::ShmVersion};
use clap::ValueEnum;
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum TargetMode {
    Auto,
    Manual,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Settings {
    pub eyelid_output_cap: f64,
    pub eye_widening: bool,
    pub eye_widening_margin_left: f64,
    pub eye_widening_margin_right: f64,
    pub min_base_width: f64,
    pub min_wide_width: f64,
    pub prefix: String,
    pub one_euro: bool,
    pub one_euro_min_cutoff: f64,
    pub one_euro_beta: f64,
    pub one_euro_d_cutoff: f64,
    pub target_mode: TargetMode,
    pub target_host: String,
    pub target_port: u16,
    pub shm_version: ShmVersion,
}
#[derive(Debug)]
pub(crate) struct SettingsError(pub String);
impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SettingsError {}
impl Default for Settings {
    fn default() -> Self {
        Self {
            eyelid_output_cap: 1.0,
            eye_widening: true,
            eye_widening_margin_left: 0.10,
            eye_widening_margin_right: 0.10,
            min_base_width: 0.10,
            min_wide_width: 0.05,
            prefix: "/FT".into(),
            one_euro: false,
            one_euro_min_cutoff: 0.5,
            one_euro_beta: 3.0,
            one_euro_d_cutoff: 1.0,
            target_mode: TargetMode::Manual,
            target_host: "127.0.0.1".into(),
            target_port: 9000,
            shm_version: ShmVersion::Auto,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        if !self.eyelid_output_cap.is_finite() || !(0.0..=1.0).contains(&self.eyelid_output_cap) {
            return Err(SettingsError("cap must be finite and in [0, 1]".into()));
        }
        if ![
            self.eye_widening_margin_left,
            self.eye_widening_margin_right,
        ]
        .iter()
        .all(|x| x.is_finite() && *x >= 0.0)
            || ![self.min_base_width, self.min_wide_width]
                .iter()
                .all(|x| x.is_finite() && *x > 0.0 && *x <= 1.0)
        {
            return Err(SettingsError(
                "invalid calibration margins or minimum widths".into(),
            ));
        }
        if !self.prefix.starts_with('/')
            || self
                .prefix
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(SettingsError("prefix must be an OSC path".into()));
        }
        if self.target_host.trim().is_empty() || self.target_port == 0 {
            return Err(SettingsError(
                "target host and nonzero port are required".into(),
            ));
        }
        let host = &self.target_host;
        let ip = host.parse::<std::net::IpAddr>().is_ok();
        let scoped = host.split_once('%').is_some_and(|(address, scope)| {
            address.parse::<std::net::Ipv6Addr>().is_ok()
                && !scope.is_empty()
                && scope
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
        });
        let domain = host.len() <= 253
            && !host.chars().all(|c| c.is_ascii_digit() || c == '.')
            && host.trim_end_matches('.').split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            });
        if !ip && !scoped && !domain {
            return Err(SettingsError("invalid target host syntax".into()));
        }
        if ![self.one_euro_min_cutoff, self.one_euro_d_cutoff]
            .iter()
            .all(|x| x.is_finite() && *x > 0.0 && (*x as f32).is_finite() && (*x as f32) > 0.0)
            || !self.one_euro_beta.is_finite()
            || self.one_euro_beta < 0.0
            || !(self.one_euro_beta as f32).is_finite()
        {
            return Err(SettingsError("invalid One Euro coefficients".into()));
        }
        Ok(())
    }
    pub fn filter_config(&self) -> OneEuroConfig {
        OneEuroConfig {
            min_cutoff: self.one_euro_min_cutoff as f32,
            beta: self.one_euro_beta as f32,
            d_cutoff: self.one_euro_d_cutoff as f32,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_hosts_are_rejected_before_resolution() {
        for host in ["127.0.0.999", "bad host", "[broken", "::gg"] {
            assert!(
                Settings {
                    target_host: host.into(),
                    ..Settings::default()
                }
                .validate()
                .is_err(),
                "{host}"
            );
        }
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn merge_precedence_and_explicit_false() {
        let saved = json!({"one_euro":true,"target_mode":"manual","eyelid_output_cap":0.4,"future":{"x":1}});
        let cli = json!({"one_euro":false,"eyelid_output_cap":0.8});
        let (current, sources) =
            SettingsStore::merge(saved.as_object().unwrap(), cli.as_object().unwrap(), true)
                .unwrap();
        assert!(!current.one_euro);
        assert_eq!(current.eyelid_output_cap, 0.8);
        assert_eq!(current.target_mode, TargetMode::Manual);
        assert_eq!(sources["one_euro"], "cli");
        assert_eq!(sources["target_mode"], "saved");
        assert_eq!(sources["prefix"], "default");
    }
    #[test]
    fn strict_patch_rejects_types_ranges_and_unknown_fields() {
        for patch in [
            json!({"one_euro":1}),
            json!({"target_port":1.5}),
            json!({"target_port":0}),
            json!({"target_port":65536}),
            json!({"eyelid_output_cap":1.1}),
            json!({"one_euro_beta":-1}),
            json!({"one_euro_min_cutoff":0}),
            json!({"shm_version":4}),
            json!({"unknown_setting":false}),
        ] {
            assert!(
                Settings::default()
                    .patched(patch.as_object().unwrap())
                    .is_err(),
                "{patch}"
            );
        }
    }
}

use serde_json::{Map, Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

impl Settings {
    pub fn values(&self) -> Value {
        json!({"eye_widening":self.eye_widening,"eye_widening_margin_left":self.eye_widening_margin_left,"eye_widening_margin_right":self.eye_widening_margin_right,"min_base_width":self.min_base_width,"min_wide_width":self.min_wide_width,"eyelid_output_cap":self.eyelid_output_cap,"prefix":self.prefix,"one_euro":self.one_euro,
            "one_euro_min_cutoff":self.one_euro_min_cutoff,"one_euro_beta":self.one_euro_beta,
            "one_euro_d_cutoff":self.one_euro_d_cutoff,"target_mode":match self.target_mode {TargetMode::Auto=>"auto",TargetMode::Manual=>"manual"},
            "target_host":self.target_host,"target_port":self.target_port,"shm_version":match self.shm_version {ShmVersion::Auto=>"auto",ShmVersion::V4=>"4",ShmVersion::V5=>"5"}})
    }
    pub fn patched(&self, patch: &Map<String, Value>) -> Result<Self, SettingsError> {
        let mut next = self.clone();
        for (key, value) in patch {
            let invalid = || SettingsError(format!("invalid value for {key}"));
            let number = || {
                value
                    .as_f64()
                    .filter(|x| {
                        x.is_finite()
                            && match key.as_str() {
                                "eyelid_output_cap" => (0.0..=1.0).contains(x),
                                "one_euro_beta"
                                | "eye_widening_margin_left"
                                | "eye_widening_margin_right" => *x >= 0.0,
                                _ => *x > 0.0,
                            }
                    })
                    .filter(|x| x.is_finite())
                    .ok_or_else(invalid)
            };
            let string = || value.as_str().map(str::to_owned).ok_or_else(invalid);
            match key.as_str() {
                "eyelid_output_cap" => next.eyelid_output_cap = number()?,
                "eye_widening" => next.eye_widening = value.as_bool().ok_or_else(invalid)?,
                "eye_widening_margin_left" => next.eye_widening_margin_left = number()?,
                "eye_widening_margin_right" => next.eye_widening_margin_right = number()?,
                "min_base_width" => next.min_base_width = number()?,
                "min_wide_width" => next.min_wide_width = number()?,
                "prefix" => next.prefix = string()?,
                "one_euro" => next.one_euro = value.as_bool().ok_or_else(invalid)?,
                "one_euro_min_cutoff" => next.one_euro_min_cutoff = number()?,
                "one_euro_beta" => next.one_euro_beta = number()?,
                "one_euro_d_cutoff" => next.one_euro_d_cutoff = number()?,
                "target_host" => next.target_host = string()?,
                "target_port" => {
                    next.target_port = value
                        .as_u64()
                        .and_then(|x| u16::try_from(x).ok())
                        .filter(|x| *x > 0)
                        .ok_or_else(invalid)?
                }
                "target_mode" => {
                    next.target_mode = match value.as_str() {
                        Some("auto") => TargetMode::Auto,
                        Some("manual") => TargetMode::Manual,
                        _ => return Err(invalid()),
                    }
                }
                "shm_version" => {
                    next.shm_version = match value.as_str() {
                        Some("auto") => ShmVersion::Auto,
                        Some("4") => ShmVersion::V4,
                        Some("5") => ShmVersion::V5,
                        _ => return Err(invalid()),
                    }
                }
                _ => return Err(SettingsError(format!("unknown setting: {key}"))),
            }
        }
        next.validate()?;
        Ok(next)
    }
}

pub(crate) const CONFIG_LIMIT: usize = 64 * 1024;
pub(crate) struct SettingsStore {
    pub(crate) path: PathBuf,
    pub saved: Map<String, Value>,
    pub sources: Map<String, Value>,
}
impl SettingsStore {
    pub fn merge(
        saved: &Map<String, Value>,
        cli: &Map<String, Value>,
        panel: bool,
    ) -> Result<(Settings, Map<String, Value>), SettingsError> {
        let mut current = Settings::default();
        if panel {
            current.target_mode = TargetMode::Auto;
        }
        let mut sources: Map<String, Value> = current
            .values()
            .as_object()
            .unwrap()
            .keys()
            .map(|k| (k.clone(), json!("default")))
            .collect();
        let known: Map<String, Value> = saved
            .iter()
            .filter(|(key, _)| sources.contains_key(*key))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        current = current.patched(&known)?;
        for key in known.keys() {
            sources.insert(key.clone(), json!("saved"));
        }
        current = current.patched(cli)?;
        for key in cli.keys() {
            sources.insert(key.clone(), json!("cli"));
        }
        Ok((current, sources))
    }
    pub fn load(
        path: PathBuf,
        cli: &Map<String, Value>,
        panel: bool,
    ) -> Result<(Self, Settings), SettingsError> {
        let saved = match std::fs::File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take((CONFIG_LIMIT + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|e| SettingsError(e.to_string()))?;
                if bytes.len() > CONFIG_LIMIT {
                    return Err(SettingsError("settings file exceeds 64 KiB".into()));
                }
                crate::ipc::parse_json(&bytes)
                    .map_err(|e| SettingsError(e.to_string()))?
                    .as_object()
                    .cloned()
                    .ok_or_else(|| SettingsError("settings must be an object".into()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
            Err(e) => return Err(SettingsError(e.to_string())),
        };
        let (current, sources) = Self::merge(&saved, cli, panel)?;
        Ok((
            Self {
                path,
                saved,
                sources,
            },
            current,
        ))
    }
    pub fn known_saved(&self) -> Map<String, Value> {
        self.saved
            .iter()
            .filter(|(key, _)| self.sources.contains_key(*key))
            .map(|(k, v)| (k.clone(), normalized_numeric_setting(k, v)))
            .collect()
    }
    pub fn save_patch(&mut self, patch: &Map<String, Value>) -> Result<(), SettingsError> {
        let mut next = self.saved.clone();
        next.extend(
            patch
                .iter()
                .map(|(k, v)| (k.clone(), normalized_numeric_setting(k, v))),
        );
        atomic_save(&self.path, &next).map_err(|e| SettingsError(e.to_string()))?;
        self.saved = next;
        Ok(())
    }
    pub fn panel_sources(&mut self, patch: &Map<String, Value>) {
        for key in patch.keys() {
            self.sources.insert(key.clone(), json!("panel"));
        }
    }
}
// Normalize only the schema's floating settings. Keep target_port integer and
// leave unknown saved values (including large integers) exactly as parsed.
fn normalized_numeric_setting(key: &str, value: &Value) -> Value {
    if matches!(
        key,
        "eye_widening_margin_left"
            | "eye_widening_margin_right"
            | "min_base_width"
            | "min_wide_width"
            | "eyelid_output_cap"
            | "one_euro_min_cutoff"
            | "one_euro_beta"
            | "one_euro_d_cutoff"
    ) {
        value
            .as_f64()
            .map_or_else(|| value.clone(), |number| json!(number))
    } else {
        value.clone()
    }
}
pub(crate) fn atomic_save(path: &Path, saved: &Map<String, Value>) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(saved)?;
    bytes.push(b'\n');
    if bytes.len() > CONFIG_LIMIT {
        return Err(std::io::Error::other("settings file exceeds 64 KiB"));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = None;
    for _ in 0..16 {
        let temp = parent.join(format!(".frameeyeosc-{}.tmp", crate::ipc::unique_id()?));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
        {
            Ok(file) => {
                temporary = Some((temp, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    let (temp, mut file) = temporary
        .ok_or_else(|| std::io::Error::other("could not create settings temporary file"))?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        // Rename is the commit boundary. No fallible work follows it: reporting failure
        // after replacement would falsely promise the old saved state was retained.
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(test)]
mod calibration_settings_tests {
    use super::*;
    #[test]
    fn widening_settings_are_strict_and_merge_explicit_false() {
        let patch = json!({"eye_widening":false,"eye_widening_margin_left":0.0,"eye_widening_margin_right":0.2,"min_base_width":0.1,"min_wide_width":0.05});
        let result = Settings::default().patched(patch.as_object().unwrap());
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(result.unwrap().values()["eye_widening"], false);
        for patch in [
            json!({"eye_widening":1}),
            json!({"eye_widening_margin_left":-0.1}),
            json!({"min_base_width":0}),
            json!({"min_wide_width":1.1}),
        ] {
            assert!(
                Settings::default()
                    .patched(patch.as_object().unwrap())
                    .is_err()
            );
        }
    }
}
