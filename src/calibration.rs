use crate::settings::Settings;
use serde_json::{Value, json};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_mapping_and_wide_disable_preserve_base() {
        let p = Profile::from_value(&json!({"left":{"closed":0.1,"relaxed":0.5,"relaxed_max":0.55,"wide":0.9},"right":{"closed":0.2,"relaxed":0.6,"relaxed_max":0.65,"wide":1.0}}), &Settings::default()).unwrap();
        let base = p.normalize([0.1, 0.6], &Settings::default()).0;
        assert!(base[0].abs() < 1e-6);
        assert_eq!(base[1], 1.0);
        let (base, wide) = p.normalize([0.9, 1.0], &Settings::default());
        assert_eq!(base, [1.0; 2]);
        assert!(wide.iter().all(|x| (*x - 1.0).abs() < 1e-6));
        let mut s = Settings {
            eye_widening_margin_left: 0.4,
            ..Settings::default()
        };
        assert_eq!(p.normalize([0.9, 1.0], &s).1, [0.0, 1.0]);
        assert_eq!(p.snapshot(&s)["left"]["wide"], 0.9);
        assert_eq!(
            p.snapshot(&s)["left"]["wide_unavailable_reason"],
            "insufficient_interval"
        );
        s.min_base_width = 0.5;
        assert!(p.validate(&s).is_err());
    }
    #[test]
    fn loading_invalid_saved_wide_preserves_both_base_intervals_and_other_eye_wide() {
        let f = crate::input::fixtures::Fixture::new(4);
        let settings = Settings::default();
        let path = f.dir.join("profile.json");
        for wide in [json!(1.5), json!("x")] {
            let saved = json!({"left":{"closed":0.0,"relaxed":0.5,"relaxed_max":0.5,"wide":wide},"right":{"closed":0.0,"relaxed":0.625,"relaxed_max":0.625,"wide":1.0}});
            std::fs::write(&path, saved.to_string()).unwrap();
            let c = Calibration::new(path.clone(), &settings);
            assert!(c.profile.is_some());
            let p = c.profile.unwrap();
            assert_eq!(p.normalize([1.0; 2], &settings), ([1.0; 2], [0.0, 1.0]));
            assert_eq!(p.normalize([0.5, 0.625], &settings).0, [1.0; 2]);
            let snapshot = p.snapshot(&settings);
            assert!(snapshot["left"]["wide"].is_null());
            assert_eq!(snapshot["left"]["wide_unavailable_reason"], "invalid_wide");
            assert_eq!(snapshot["right"]["wide_available"], true);
            assert_eq!(snapshot["right"]["wide"], 1.0);
            assert_eq!(p.measurements()["left"].as_object().unwrap().len(), 4);
        }
    }
    #[test]
    fn saturated_relaxed_max_disables_only_its_wide() {
        let p=Profile::from_value(&json!({"left":{"closed":0.0,"relaxed":0.5,"relaxed_max":1.0,"wide":1.0},"right":{"closed":0.0,"relaxed":0.5,"relaxed_max":0.5,"wide":1.0}}),&Settings::default()).unwrap();
        assert_eq!(p.normalize([1.0; 2], &Settings::default()).1, [0.0, 1.0]);
        for bad in [
            json!({"left":{}}),
            json!({"left":{"closed":0.5,"relaxed":0.5,"relaxed_max":0.5,"wide":null},"right":{"closed":0.0,"relaxed":0.5,"relaxed_max":0.5,"wide":null}}),
        ] {
            assert!(Profile::from_value(&bad, &Settings::default()).is_err());
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EyeProfile {
    pub closed: f64,
    pub relaxed: f64,
    pub relaxed_max: f64,
    pub wide: Option<f64>,
    invalid_wide: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Profile(pub [EyeProfile; 2]);
impl Profile {
    pub(crate) fn from_value(value: &Value, settings: &Settings) -> Result<Self, String> {
        let object = value.as_object().ok_or("profile must be an object")?;
        if object.len() != 2 || !object.contains_key("left") || !object.contains_key("right") {
            return Err("profile requires left and right measurements only".into());
        }
        let mut eyes = Vec::new();
        for key in ["left", "right"] {
            let eye = value[key]
                .as_object()
                .ok_or("eye profile must be an object")?;
            if eye.len() != 4
                || !["closed", "relaxed", "relaxed_max", "wide"]
                    .iter()
                    .all(|k| eye.contains_key(*k))
            {
                return Err("eye profile requires measurements only".into());
            }
            let number = |key: &str| {
                eye[key]
                    .as_f64()
                    .filter(|x| x.is_finite() && (0.0..=1.0).contains(x))
                    .ok_or_else(|| format!("invalid profile {key}"))
            };
            let wide = number("wide").ok();
            let invalid_wide = !eye["wide"].is_null() && wide.is_none();
            eyes.push(EyeProfile {
                closed: number("closed")?,
                relaxed: number("relaxed")?,
                relaxed_max: number("relaxed_max")?,
                wide,
                invalid_wide,
            });
        }
        let profile = Self(eyes.try_into().unwrap());
        profile.validate(settings)?;
        Ok(profile)
    }
    pub(crate) fn validate(&self, settings: &Settings) -> Result<(), String> {
        for eye in &self.0 {
            if ![eye.closed, eye.relaxed, eye.relaxed_max]
                .iter()
                .all(|x| x.is_finite() && (0.0..=1.0).contains(x))
                || eye.relaxed <= eye.closed
                || eye.relaxed - eye.closed < settings.min_base_width
                || eye.relaxed_max < eye.relaxed
                || eye
                    .wide
                    .is_some_and(|x| !x.is_finite() || !(0.0..=1.0).contains(&x))
            {
                return Err("invalid base interval or measurements".into());
            }
        }
        Ok(())
    }
    fn margin(settings: &Settings, index: usize) -> f64 {
        if index == 0 {
            settings.eye_widening_margin_left
        } else {
            settings.eye_widening_margin_right
        }
    }
    pub(crate) fn wide_available(&self, settings: &Settings) -> [bool; 2] {
        std::array::from_fn(|i| {
            self.0[i].wide.is_some_and(|w| {
                w - self.0[i].relaxed_max - Self::margin(settings, i) >= settings.min_wide_width
            })
        })
    }
    pub(crate) fn normalize(&self, raw: [f32; 2], settings: &Settings) -> ([f32; 2], [f32; 2]) {
        let available = self.wide_available(settings);
        let base = std::array::from_fn(|i| {
            ((f64::from(raw[i]) - self.0[i].closed) / (self.0[i].relaxed - self.0[i].closed))
                .clamp(0.0, 1.0) as f32
        });
        let wide = std::array::from_fn(|i| {
            if available[i] {
                let start = self.0[i].relaxed_max + Self::margin(settings, i);
                ((f64::from(raw[i]) - start) / (self.0[i].wide.unwrap() - start)).clamp(0.0, 1.0)
                    as f32
            } else {
                0.0
            }
        });
        (base, wide)
    }
    pub(crate) fn measurements(&self) -> Value {
        json!({"left":self.eye_measurements(0),"right":self.eye_measurements(1)})
    }
    fn eye_measurements(&self, i: usize) -> Value {
        let e = &self.0[i];
        json!({"closed":e.closed,"relaxed":e.relaxed,"relaxed_max":e.relaxed_max,"wide":e.wide})
    }
    pub(crate) fn snapshot(&self, settings: &Settings) -> Value {
        let available = self.wide_available(settings);
        let eyes: [Value; 2] = std::array::from_fn(|i| {
            let mut eye = self.eye_measurements(i);
            let start = self.0[i].relaxed_max + Self::margin(settings, i);
            eye["d"] = json!(start - self.0[i].relaxed);
            eye["wide_start"] = json!(start);
            eye["wide_available"] = json!(available[i]);
            eye["wide_unavailable_reason"] = if available[i] {
                Value::Null
            } else {
                json!(if self.0[i].invalid_wide {
                    "invalid_wide"
                } else if self.0[i].wide.is_none() {
                    "not_measured"
                } else {
                    "insufficient_interval"
                })
            };
            eye
        });
        json!({"left":eyes[0],"right":eyes[1]})
    }
}

#[cfg(test)]
mod measurement_tests {
    use super::*;
    use crate::input::Observation;
    fn collect(valid: usize, period: f64) -> Measurement {
        let mut m = Measurement::default();
        for i in 0..(5.0 / period).round() as usize {
            m.add(Observation {
                sequence: i as u32,
                timestamp: 10.0 + i as f64 * period,
                openness: (i < valid).then_some([0.0, 1.0]),
            });
        }
        m
    }
    #[test]
    fn observed_cadence_coverage_accepts_eighty_percent_without_fixed_count() {
        assert!(collect(80, 0.05).finish().is_ok());
        assert!(collect(79, 0.05).finish().is_err());
        assert!(collect(8, 0.5).finish().is_err()); // cadence requires ten differences
        assert!(collect(40, 0.1).finish().is_ok()); // no arbitrary 10 Hz minimum
    }
    #[test]
    fn duplicate_time_and_epoch_changes_cannot_inflate_quality() {
        let mut m = collect(100, 0.05);
        m.add(Observation {
            sequence: 101,
            timestamp: 10.0,
            openness: Some([0.5; 2]),
        });
        assert_eq!(m.finish().unwrap().0, [0.0, 1.0]);
        m.add(Observation {
            sequence: 102,
            timestamp: 9.0,
            openness: Some([0.5; 2]),
        });
        assert!(m.finish().is_err());
        let mut m = collect(100, 0.05);
        m.add(Observation {
            sequence: 1,
            timestamp: 16.0,
            openness: Some([0.5; 2]),
        });
        assert!(m.finish().is_err());
    }
}

#[derive(Default)]
struct Measurement {
    previous_sequence: Option<u32>,
    previous_time: Option<f64>,
    seen: std::collections::HashSet<u64>,
    deltas: Vec<f64>,
    eyes: [Vec<f64>; 2],
    epoch_changed: bool,
}
fn median(values: &[f64]) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n.is_multiple_of(2) {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    }
}
fn ulp(value: f64) -> f64 {
    let value = value.abs();
    if value == 0.0 {
        f64::from_bits(1)
    } else {
        f64::from_bits(value.to_bits() + 1) - value
    }
}
impl Measurement {
    fn add(&mut self, observation: crate::input::Observation) {
        if self.previous_sequence == Some(observation.sequence) {
            return;
        }
        if let Some(previous) = self.previous_sequence
            && observation.sequence < previous
            && !(previous > 0xf0000000 && observation.sequence < 0x10000000)
        {
            self.epoch_changed = true;
        }
        self.previous_sequence = Some(observation.sequence);
        let timestamp = observation.timestamp;
        if !timestamp.is_finite()
            || !self.seen.insert(if timestamp == 0.0 {
                0
            } else {
                timestamp.to_bits()
            })
        {
            return;
        }
        if let Some(previous) = self.previous_time {
            let delta = timestamp - previous;
            if delta < 0.0 {
                self.epoch_changed = true;
                return;
            }
            if delta > 0.0 {
                self.deltas.push(delta);
            }
        }
        self.previous_time = Some(timestamp);
        if let Some(raw) = observation.openness
            && raw.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x))
        {
            for (i, value) in raw.into_iter().enumerate() {
                self.eyes[i].push(f64::from(value));
            }
        }
    }
    fn finish(&self) -> Result<([f64; 2], [f64; 2]), String> {
        if self.epoch_changed || self.deltas.len() < 10 || self.eyes[0].is_empty() {
            return Err("insufficient_samples".into());
        }
        let period = median(&self.deltas);
        let expected = 5.0 / period;
        let rate = self.eyes[0].len() as f64 / expected;
        let float_quantum = self
            .seen
            .iter()
            .map(|x| ulp(f64::from_bits(*x)))
            .fold(0.0, f64::max);
        let mut steps: Vec<_> = self
            .deltas
            .iter()
            .copied()
            .filter(|d| *d < 1.5 * period)
            .collect();
        steps.sort_by(f64::total_cmp);
        steps.dedup();
        let grain = steps
            .windows(2)
            .map(|x| x[1] - x[0])
            .filter(|x| *x > 2.0 * float_quantum)
            .fold(f64::INFINITY, f64::min);
        let quantum = float_quantum.max(if grain.is_finite() { grain } else { 0.0 });
        let uncertainty = 1e-12 + rate * quantum / period;
        let resolution_ok = uncertainty < 0.5 / expected;
        if rate + uncertainty < 0.8 || (rate - uncertainty < 0.8 && !resolution_ok) {
            return Err("insufficient_samples".into());
        }
        Ok((
            std::array::from_fn(|i| median(&self.eyes[i])),
            std::array::from_fn(|i| self.eyes[i].iter().copied().fold(0.0, f64::max)),
        ))
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::input::Observation;
    use std::time::{Duration, Instant};
    fn stage(c: &mut Calibration, name: &str, raw: [f32; 2], now: Instant, settings: &Settings) {
        let id = c.snapshot()["session_id"].as_str().unwrap().to_owned();
        c.command(
            "calibration.begin_stage",
            &json!({"session_id":id,"stage":name}),
            json!({"connection_id":"p1","request_id":name}),
            now,
            settings,
        )
        .unwrap();
        c.advance(now + Duration::from_secs(2), None, settings);
        for i in 0..100 {
            c.advance(
                now + Duration::from_millis(2000 + i * 50),
                Some(Observation {
                    sequence: i as u32,
                    timestamp: 20.0 + i as f64 * 0.05,
                    openness: Some(raw),
                }),
                settings,
            );
        }
        c.advance(now + Duration::from_secs(7), None, settings);
    }
    #[test]
    fn explicit_stages_save_measurements_only_and_cancel_preserves_profile() {
        let fixture = crate::input::fixtures::Fixture::new(4);
        let path = fixture.dir.join("config.profile.json");
        let s = Settings::default();
        let now = Instant::now();
        let mut c = Calibration::new(path.clone(), &s);
        c.command(
            "calibration.start",
            &json!({}),
            json!({"connection_id":"p1","request_id":"start"}),
            now,
            &s,
        )
        .unwrap();
        assert_eq!(c.snapshot()["stage"], "closed");
        stage(&mut c, "closed", [0.0; 2], now, &s);
        assert_eq!(c.snapshot()["stage"], "relaxed");
        assert!(c.profile.is_none());
        stage(
            &mut c,
            "relaxed",
            [0.5; 2],
            now + Duration::from_secs(8),
            &s,
        );
        assert_eq!(c.snapshot()["stage"], "wide");
        let id = c.snapshot()["session_id"].clone();
        c.command(
            "calibration.skip_wide",
            &json!({"session_id":id}),
            Value::Null,
            now,
            &s,
        )
        .unwrap();
        assert_eq!(c.snapshot()["state"], "completed");
        let saved = crate::ipc::parse_json(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["left"]["relaxed"], 0.5);
        assert_eq!(saved["left"].as_object().unwrap().len(), 4);
        assert!(saved.get("eye_widening").is_none());
        let old = c.profile.clone();
        c.command("calibration.start", &json!({}), Value::Null, now, &s)
            .unwrap();
        let id = c.snapshot()["session_id"].clone();
        c.command(
            "calibration.cancel",
            &json!({"session_id":id}),
            Value::Null,
            now,
            &s,
        )
        .unwrap();
        assert_eq!(c.profile, old);
    }
    #[test]
    fn preparation_restart_does_not_invalidate_later_measurement() {
        let fixture = crate::input::fixtures::Fixture::new(4);
        let settings = Settings::default();
        let now = Instant::now();
        let mut c = Calibration::new(fixture.dir.join("p.json"), &settings);
        c.command("calibration.start", &json!({}), Value::Null, now, &settings)
            .unwrap();
        let id = c.snapshot()["session_id"].clone();
        c.command(
            "calibration.begin_stage",
            &json!({"session_id":id,"stage":"closed"}),
            Value::Null,
            now,
            &settings,
        )
        .unwrap();
        c.advance(
            now + Duration::from_secs(1),
            Some(Observation {
                sequence: 5000,
                timestamp: 100.0,
                openness: Some([0.8; 2]),
            }),
            &settings,
        );
        c.invalidate_epoch();
        for i in 0..100 {
            c.advance(
                now + Duration::from_millis(2000 + i * 50),
                Some(Observation {
                    sequence: i as u32 + 1,
                    timestamp: 1.0 + i as f64 * 0.05,
                    openness: Some([0.1, 0.2]),
                }),
                &settings,
            );
        }
        c.advance(now + Duration::from_secs(7), None, &settings);
        assert_eq!(c.snapshot()["stage"], "relaxed");
        assert_eq!(c.snapshot()["state"], "waiting");
        assert_eq!(c.snapshot()["result"]["kind"], "stage_completed");
        assert_eq!(c.snapshot()["result"]["reason"], Value::Null);
    }
    #[test]
    fn relaxed_max_uses_only_unique_valid_measurement_samples_and_preserves_begin_cause() {
        let f = crate::input::fixtures::Fixture::new(4);
        let settings = Settings::default();
        let now = Instant::now();
        let mut c = Calibration::new(f.dir.join("p.json"), &settings);
        c.command("calibration.start", &json!({}), Value::Null, now, &settings)
            .unwrap();
        stage(&mut c, "closed", [0.0; 2], now, &settings);
        let id = c.snapshot()["session_id"].clone();
        let began = now + Duration::from_secs(8);
        let cause = json!({"connection_id":"p1","request_id":"relaxed-second"});
        c.command(
            "calibration.begin_stage",
            &json!({"session_id":id,"stage":"relaxed"}),
            cause.clone(),
            began,
            &settings,
        )
        .unwrap();
        c.advance(
            began + Duration::from_secs(1),
            Some(Observation {
                sequence: 1000,
                timestamp: 10.0,
                openness: Some([1.0; 2]),
            }),
            &settings,
        );
        c.invalidate_epoch(); // preparation restart is outside the measurement window
        for i in 0..100 {
            let raw = if i == 99 { [0.75, 1.0] } else { [0.5; 2] };
            c.advance(
                began + Duration::from_millis(2000 + i * 50),
                Some(Observation {
                    sequence: 1001 + i as u32,
                    timestamp: 10.05 + i as f64 * 0.05,
                    openness: Some(raw),
                }),
                &settings,
            );
        }
        c.advance(
            began + Duration::from_secs(7),
            Some(Observation {
                sequence: 1101,
                timestamp: 15.1,
                openness: Some([1.0; 2]),
            }),
            &settings,
        );
        assert_eq!(c.snapshot()["stage"], "wide");
        assert_eq!(c.snapshot()["caused_by"], cause);
        assert_eq!(c.snapshot()["result"]["caused_by"], cause);
        let p = c.candidate(None);
        assert_eq!(p.0[0].relaxed, 0.5);
        assert_eq!(p.0[0].relaxed_max, 0.75);
        assert_eq!(p.0[1].relaxed_max, 1.0);
    }
    #[test]
    fn wide_saturated_interval_is_saved_and_margin_changes_can_reenable_it() {
        let f = crate::input::fixtures::Fixture::new(4);
        let settings = Settings::default();
        let now = Instant::now();
        let path = f.dir.join("p.json");
        let mut c = Calibration::new(path.clone(), &settings);
        c.command("calibration.start", &json!({}), Value::Null, now, &settings)
            .unwrap();
        stage(&mut c, "closed", [0.0; 2], now, &settings);
        stage(
            &mut c,
            "relaxed",
            [0.85, 0.5],
            now + Duration::from_secs(8),
            &settings,
        );
        stage(
            &mut c,
            "wide",
            [0.95, 1.0],
            now + Duration::from_secs(16),
            &settings,
        );
        assert_eq!(c.snapshot()["state"], "completed");
        assert_eq!(
            c.snapshot()["result"]["wide_available"],
            json!([false, true])
        );
        let p = c.profile.as_ref().unwrap();
        assert!(p.0[0].wide.is_some());
        let changed = Settings {
            eye_widening_margin_left: 0.0,
            ..settings.clone()
        };
        assert_eq!(p.wide_available(&changed), [true, true]);
        let loaded = Calibration::new(path, &changed);
        assert_eq!(loaded.profile, c.profile);
    }
    #[test]
    fn malformed_profile_allows_recalibration_and_save_failure_preserves_previous_profile() {
        let f = crate::input::fixtures::Fixture::new(4);
        let settings = Settings::default();
        let now = Instant::now();
        let path = f.dir.join("p.json");
        std::fs::write(&path, b"{invalid").unwrap();
        let mut c = Calibration::new(path.clone(), &settings);
        assert!(c.profile.is_none());
        c.command("calibration.start", &json!({}), Value::Null, now, &settings)
            .unwrap();
        stage(&mut c, "closed", [0.0; 2], now, &settings);
        stage(
            &mut c,
            "relaxed",
            [0.5; 2],
            now + Duration::from_secs(8),
            &settings,
        );
        let id = c.snapshot()["session_id"].clone();
        c.command(
            "calibration.skip_wide",
            &json!({"session_id":id}),
            Value::Null,
            now,
            &settings,
        )
        .unwrap();
        let old = c.profile.clone();
        let original = std::fs::read(&path).unwrap();
        c.command("calibration.start", &json!({}), Value::Null, now, &settings)
            .unwrap();
        stage(&mut c, "closed", [0.0; 2], now, &settings);
        stage(
            &mut c,
            "relaxed",
            [0.75; 2],
            now + Duration::from_secs(8),
            &settings,
        );
        let id = c.snapshot()["session_id"].clone();
        c.path = f.dir.clone();
        assert_eq!(
            c.command(
                "calibration.skip_wide",
                &json!({"session_id":id}),
                Value::Null,
                now,
                &settings
            )
            .unwrap_err(),
            "save_failed"
        );
        assert_eq!(c.snapshot()["state"], "failed");
        assert_eq!(c.snapshot()["result"]["reason"], "save_failed");
        assert_eq!(c.profile, old);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
    #[test]
    fn measurement_epoch_loss_fails_only_current_stage_and_retry_uses_new_samples() {
        let f = crate::input::fixtures::Fixture::new(4);
        let settings = Settings::default();
        let now = Instant::now();
        let mut c = Calibration::new(f.dir.join("p.json"), &settings);
        c.command("calibration.start", &json!({}), Value::Null, now, &settings)
            .unwrap();
        let id = c.snapshot()["session_id"].clone();
        c.command(
            "calibration.begin_stage",
            &json!({"session_id":id,"stage":"closed"}),
            Value::Null,
            now,
            &settings,
        )
        .unwrap();
        c.advance(now + Duration::from_secs(2), None, &settings);
        c.invalidate_epoch();
        for i in 0..100 {
            c.advance(
                now + Duration::from_millis(2000 + i * 50),
                Some(Observation {
                    sequence: i as u32,
                    timestamp: 10.0 + i as f64 * 0.05,
                    openness: Some([0.9; 2]),
                }),
                &settings,
            );
        }
        c.advance(now + Duration::from_secs(7), None, &settings);
        assert_eq!(c.snapshot()["result"]["reason"], "insufficient_samples");
        assert_eq!(c.snapshot()["stage"], "closed");
        stage(
            &mut c,
            "closed",
            [0.0; 2],
            now + Duration::from_secs(8),
            &settings,
        );
        assert_eq!(c.session.as_ref().unwrap().closed, Some([0.0; 2]));
    }
    #[test]
    fn wrong_state_operations_have_invalid_state() {
        let fixture = crate::input::fixtures::Fixture::new(4);
        let s = Settings::default();
        let now = Instant::now();
        let mut c = Calibration::new(fixture.dir.join("p.json"), &s);
        c.command("calibration.start", &json!({}), Value::Null, now, &s)
            .unwrap();
        let id = c.snapshot()["session_id"].clone();
        assert_eq!(
            c.command(
                "calibration.begin_stage",
                &json!({"session_id":id,"stage":"relaxed"}),
                Value::Null,
                now,
                &s
            )
            .unwrap_err(),
            "invalid_state"
        );
        assert_eq!(
            c.command(
                "calibration.skip_wide",
                &json!({"session_id":id}),
                Value::Null,
                now,
                &s
            )
            .unwrap_err(),
            "invalid_state"
        );
    }
    #[test]
    fn minimum_base_update_rejects_completed_candidate_interval() {
        let fixture = crate::input::fixtures::Fixture::new(4);
        let s = Settings::default();
        let now = Instant::now();
        let mut c = Calibration::new(fixture.dir.join("p.json"), &s);
        c.command("calibration.start", &json!({}), Value::Null, now, &s)
            .unwrap();
        stage(&mut c, "closed", [0.0; 2], now, &s);
        stage(
            &mut c,
            "relaxed",
            [0.5; 2],
            now + Duration::from_secs(8),
            &s,
        );
        assert!(
            c.validate_candidate_settings(&Settings {
                min_base_width: 0.6,
                ..s.clone()
            })
            .is_err()
        );
        assert!(
            c.validate_candidate_settings(&Settings {
                min_wide_width: 0.8,
                ..s
            })
            .is_ok()
        );
        assert_eq!(c.snapshot()["stage"], "wide");
    }
    #[test]
    fn failed_stage_retries_and_base_invalid_fails_relaxed() {
        let fixture = crate::input::fixtures::Fixture::new(4);
        let s = Settings::default();
        let now = Instant::now();
        let mut c = Calibration::new(fixture.dir.join("p.json"), &s);
        c.command("calibration.start", &json!({}), Value::Null, now, &s)
            .unwrap();
        stage(&mut c, "closed", [0.5; 2], now, &s);
        stage(
            &mut c,
            "relaxed",
            [0.5; 2],
            now + Duration::from_secs(8),
            &s,
        );
        assert_eq!(c.snapshot()["state"], "waiting");
        assert_eq!(c.snapshot()["stage"], "relaxed");
        assert_eq!(c.snapshot()["result"]["reason"], "invalid_interval");
        stage(
            &mut c,
            "relaxed",
            [0.8; 2],
            now + Duration::from_secs(16),
            &s,
        );
        assert_eq!(c.snapshot()["stage"], "wide");
    }
}

use std::{
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub(crate) struct Calibration {
    pub(crate) profile: Option<Profile>,
    path: PathBuf,
    session: Option<Session>,
}
struct Session {
    id: String,
    owner: Option<String>,
    state: &'static str,
    baseline: Option<crate::input::Observation>,
    stage: Option<&'static str>,
    remaining_ms: u64,
    cause: Value,
    result: Value,
    started: Instant,
    measurement: Measurement,
    closed: Option<[f64; 2]>,
    relaxed: Option<([f64; 2], [f64; 2])>,
}
pub(crate) fn profile_path(config: &Path) -> PathBuf {
    let mut path = config.as_os_str().to_os_string();
    path.push(".profile.json");
    PathBuf::from(path)
}
impl Calibration {
    pub(crate) fn validate_candidate_settings(&self, settings: &Settings) -> Result<(), String> {
        if self.active() && self.session.as_ref().is_some_and(|s| s.relaxed.is_some()) {
            self.candidate(None).validate(settings)?;
        }
        Ok(())
    }

    pub(crate) fn new(path: PathBuf, settings: &Settings) -> Self {
        let profile = (|| -> Result<Option<Profile>, String> {
            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.to_string()),
            };
            let mut bytes = Vec::new();
            file.take((crate::settings::CONFIG_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > crate::settings::CONFIG_LIMIT {
                return Err("profile exceeds 64 KiB".into());
            }
            let value = crate::ipc::parse_json(&bytes).map_err(|e| e.to_string())?;
            Profile::from_value(&value, settings).map(Some)
        })()
        .unwrap_or_else(|error| {
            eprintln!("Calibration profile unavailable; using raw output: {error}");
            None
        });
        Self {
            profile,
            path,
            session: None,
        }
    }
    pub(crate) fn active(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| matches!(s.state, "waiting" | "preparing" | "measuring"))
    }
    pub(crate) fn snapshot(&self) -> Value {
        self.session.as_ref().map_or(Value::Null,|s|json!({"session_id":s.id,"state":s.state,"stage":s.stage,"remaining_ms":s.remaining_ms,"caused_by":s.cause,"result":s.result}))
    }
    fn response(&self, save: &str) -> Value {
        json!({"calibration":self.snapshot(),"save":save})
    }
    fn set_result(
        &mut self,
        kind: &str,
        stage: Option<&str>,
        reason: Option<&str>,
        wide: Option<[bool; 2]>,
    ) {
        let s = self.session.as_mut().unwrap();
        s.result = json!({"kind":kind,"stage":stage,"reason":reason,"wide_available":wide,"caused_by":s.cause});
    }
    pub(crate) fn command(
        &mut self,
        method: &str,
        params: &Value,
        cause: Value,
        now: Instant,
        settings: &Settings,
    ) -> Result<Value, String> {
        if method == "calibration.start" {
            if self.active() {
                return Err("busy".into());
            }
            self.session = Some(Session {
                id: crate::ipc::unique_id().map_err(|_| "start_failed")?,
                owner: cause["connection_id"].as_str().map(str::to_owned),
                state: "waiting",
                baseline: None,
                stage: Some("closed"),
                remaining_ms: 0,
                cause,
                result: Value::Null,
                started: now,
                measurement: Measurement::default(),
                closed: None,
                relaxed: None,
            });
            return Ok(self.response("not_requested"));
        }
        let s = self.session.as_ref().ok_or("unknown_session")?;
        if params["session_id"].as_str() != Some(s.id.as_str()) {
            return Err("unknown_session".into());
        }
        if !self.active() {
            return Err("already_terminal".into());
        }
        match method {
            "calibration.begin_stage" => {
                let stage = match params["stage"].as_str() {
                    Some("closed") => "closed",
                    Some("relaxed") => "relaxed",
                    Some("wide") => "wide",
                    _ => return Err("invalid_stage".into()),
                };
                if s.state != "waiting" || s.stage != Some(stage) {
                    return Err("invalid_state".into());
                }
                let s = self.session.as_mut().unwrap();
                s.state = "preparing";
                s.remaining_ms = 2000;
                s.started = now;
                s.cause = cause;
                s.measurement = Measurement::default();
                s.baseline = None;
                Ok(self.response("not_requested"))
            }
            "calibration.skip_wide" => {
                if s.state != "waiting" || s.stage != Some("wide") {
                    return Err("invalid_state".into());
                }
                self.session.as_mut().unwrap().cause = cause;
                self.complete(None, settings)?;
                Ok(self.response("saved"))
            }
            "calibration.cancel" => {
                self.cancel(cause, None);
                Ok(self.response("not_requested"))
            }
            _ => Err("unknown_method".into()),
        }
    }
    fn candidate(&self, wide: Option<[f64; 2]>) -> Profile {
        let s = self.session.as_ref().unwrap();
        let closed = s.closed.unwrap();
        let (relaxed, max) = s.relaxed.unwrap();
        Profile(std::array::from_fn(|i| EyeProfile {
            closed: closed[i],
            relaxed: relaxed[i],
            relaxed_max: max[i],
            wide: wide.map(|w| w[i]),
            invalid_wide: false,
        }))
    }
    fn complete(&mut self, wide: Option<[f64; 2]>, settings: &Settings) -> Result<(), String> {
        let profile = self.candidate(wide);
        if profile.validate(settings).is_err() {
            self.stage_failed("invalid_interval");
            return Err("invalid_interval".into());
        }
        if let Err(error) =
            crate::settings::atomic_save(&self.path, profile.measurements().as_object().unwrap())
        {
            eprintln!("Calibration profile save failed: {error}");
            let s = self.session.as_mut().unwrap();
            s.state = "failed";
            s.stage = None;
            s.remaining_ms = 0;
            s.closed = None;
            s.relaxed = None;
            s.measurement = Measurement::default();
            self.set_result("failed", None, Some("save_failed"), None);
            return Err("save_failed".into());
        }
        let available = profile.wide_available(settings);
        self.profile = Some(profile);
        let s = self.session.as_mut().unwrap();
        s.state = "completed";
        s.stage = None;
        s.remaining_ms = 0;
        s.closed = None;
        s.relaxed = None;
        s.measurement = Measurement::default();
        self.set_result("completed", None, None, Some(available));
        Ok(())
    }
    fn stage_failed(&mut self, reason: &str) {
        let s = self.session.as_mut().unwrap();
        let stage = s.stage;
        s.state = "waiting";
        s.remaining_ms = 0;
        s.measurement = Measurement::default();
        self.set_result("stage_failed", stage, Some(reason), None);
    }
    fn cancel(&mut self, cause: Value, reason: Option<&str>) {
        if !self.active() {
            return;
        }
        let s = self.session.as_mut().unwrap();
        s.cause = cause;
        s.state = "cancelled";
        s.stage = None;
        s.remaining_ms = 0;
        s.measurement = Measurement::default();
        s.closed = None;
        s.relaxed = None;
        self.set_result("cancelled", None, reason, None);
    }
    pub(crate) fn disconnect(&mut self, connection: &str) {
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.owner.as_deref() == Some(connection))
        {
            self.cancel(Value::Null, Some("connection_lost"));
        }
    }
    pub(crate) fn invalidate_epoch(&mut self) {
        if let Some(s) = &mut self.session {
            match s.state {
                "preparing" => s.baseline = None,
                "measuring" => s.measurement.epoch_changed = true,
                _ => {}
            }
        }
    }
    pub(crate) fn advance(
        &mut self,
        now: Instant,
        observation: Option<crate::input::Observation>,
        settings: &Settings,
    ) {
        let Some(s) = &mut self.session else { return };
        if !matches!(s.state, "preparing" | "measuring") {
            return;
        }
        let elapsed = now.saturating_duration_since(s.started);
        if elapsed < Duration::from_secs(2) {
            if observation.is_some() {
                s.baseline = observation;
            }
            s.remaining_ms = ((2000 - elapsed.as_millis() as u64).div_ceil(100)) * 100;
            return;
        }
        if s.state == "preparing"
            && let Some(baseline) = s.baseline.take()
        {
            s.measurement.previous_sequence = Some(baseline.sequence);
            if baseline.timestamp.is_finite() {
                s.measurement.previous_time = Some(baseline.timestamp);
                s.measurement.seen.insert(if baseline.timestamp == 0.0 {
                    0
                } else {
                    baseline.timestamp.to_bits()
                });
            }
        }
        s.state = "measuring";
        if elapsed < Duration::from_secs(7) {
            s.remaining_ms = ((7000 - elapsed.as_millis() as u64).div_ceil(100)) * 100;
            if let Some(observation) = observation {
                s.measurement.add(observation);
            }
            return;
        }
        let stage = s.stage.unwrap();
        let (median, max) = match s.measurement.finish() {
            Ok(values) => values,
            Err(_) => {
                self.stage_failed("insufficient_samples");
                return;
            }
        };
        match stage {
            "closed" => {
                let s = self.session.as_mut().unwrap();
                s.closed = Some(median);
                s.stage = Some("relaxed");
                s.state = "waiting";
                s.remaining_ms = 0;
                s.measurement = Measurement::default();
                self.set_result("stage_completed", Some(stage), None, None);
            }
            "relaxed" => {
                self.session.as_mut().unwrap().relaxed = Some((median, max));
                if self.candidate(None).validate(settings).is_err() {
                    self.session.as_mut().unwrap().relaxed = None;
                    self.stage_failed("invalid_interval");
                    return;
                }
                let s = self.session.as_mut().unwrap();
                s.stage = Some("wide");
                s.state = "waiting";
                s.remaining_ms = 0;
                s.measurement = Measurement::default();
                self.set_result("stage_completed", Some(stage), None, None);
            }
            "wide" => {
                let _ = self.complete(Some(median), settings);
            }
            _ => unreachable!(),
        }
    }
}
