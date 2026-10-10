use crate::{filter::OneEuroConfig, input::ShmVersion};
use clap::ValueEnum;
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum TargetMode {
    Auto,
    Manual,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Settings {
    pub eyelid_output_cap: f32,
    pub prefix: String,
    pub one_euro: bool,
    pub one_euro_min_cutoff: f32,
    pub one_euro_beta: f32,
    pub one_euro_d_cutoff: f32,
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
            .all(|x| x.is_finite() && *x > 0.0)
            || !self.one_euro_beta.is_finite()
            || self.one_euro_beta < 0.0
        {
            return Err(SettingsError("invalid One Euro coefficients".into()));
        }
        Ok(())
    }
    pub fn filter_config(&self) -> OneEuroConfig {
        OneEuroConfig {
            min_cutoff: self.one_euro_min_cutoff,
            beta: self.one_euro_beta,
            d_cutoff: self.one_euro_d_cutoff,
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
