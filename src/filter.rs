use signal_smooth::OneEuroFilter;

/// Eye samples normally arrive many times per second. A half-second gap therefore indicates
/// tracking loss, suspension, or a producer restart, and retaining old history would add lag.
pub(crate) const MAX_FILTER_SAMPLE_GAP: f64 = 0.5;

#[derive(Clone, Copy, Debug)]
pub(crate) struct OneEuroConfig {
    pub(crate) min_cutoff: f32,
    pub(crate) beta: f32,
    pub(crate) d_cutoff: f32,
}

impl Default for OneEuroConfig {
    fn default() -> Self {
        Self {
            min_cutoff: 0.5,
            beta: 3.0,
            d_cutoff: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EyeValues {
    pub(crate) left: [f32; 2],
    pub(crate) right: [f32; 2],
    pub(crate) combined: [f32; 2],
    pub(crate) eyelids: [f32; 2],
}

pub(crate) struct EyeFilters {
    gaze: [OneEuroFilter; 6],
    eyelids: [OneEuroFilter; 2],
    previous_sample_time: Option<f64>,
}

impl EyeFilters {
    pub(crate) fn new(config: OneEuroConfig) -> Self {
        Self::with_configs(config, config)
    }

    fn with_configs(gaze_config: OneEuroConfig, eyelid_config: OneEuroConfig) -> Self {
        Self {
            gaze: std::array::from_fn(|_| {
                OneEuroFilter::with_d_cutoff(
                    gaze_config.min_cutoff,
                    gaze_config.beta,
                    gaze_config.d_cutoff,
                )
            }),
            eyelids: std::array::from_fn(|_| {
                OneEuroFilter::with_d_cutoff(
                    eyelid_config.min_cutoff,
                    eyelid_config.beta,
                    eyelid_config.d_cutoff,
                )
            }),
            previous_sample_time: None,
        }
    }

    pub(crate) fn filter(&mut self, sample_time: f64, values: EyeValues) -> EyeValues {
        if !sample_time.is_finite() {
            self.reset();
            return values;
        }

        let Some(previous_sample_time) = self.previous_sample_time else {
            self.previous_sample_time = Some(sample_time);
            return self.filter_values(values, f32::MIN_POSITIVE);
        };
        let dt = sample_time - previous_sample_time;
        if dt <= 0.0 || dt > MAX_FILTER_SAMPLE_GAP {
            self.reset();
            self.previous_sample_time = Some(sample_time);
            return self.filter_values(values, f32::MIN_POSITIVE);
        }
        self.previous_sample_time = Some(sample_time);
        self.filter_values(values, dt as f32)
    }

    fn filter_values(&mut self, values: EyeValues, dt: f32) -> EyeValues {
        let gaze = [
            values.left[0],
            values.left[1],
            values.right[0],
            values.right[1],
            values.combined[0],
            values.combined[1],
        ];
        let gaze: [f32; 6] = std::array::from_fn(|index| self.gaze[index].filter(gaze[index], dt));
        let eyelids =
            std::array::from_fn(|index| self.eyelids[index].filter(values.eyelids[index], dt));

        EyeValues {
            left: [gaze[0], gaze[1]],
            right: [gaze[2], gaze[3]],
            combined: [gaze[4], gaze[5]],
            eyelids,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.gaze.iter_mut().for_each(OneEuroFilter::reset);
        self.eyelids.iter_mut().for_each(OneEuroFilter::reset);
        self.previous_sample_time = None;
    }
}

pub(crate) struct OptionalEyeFilter(Option<EyeFilters>);

impl OptionalEyeFilter {
    pub(crate) fn new(enabled: bool, config: OneEuroConfig) -> Self {
        Self(enabled.then(|| EyeFilters::new(config)))
    }

    pub(crate) fn filter(&mut self, sample_time: f64, values: EyeValues) -> EyeValues {
        self.0
            .as_mut()
            .map_or(values, |filters| filters.filter(sample_time, values))
    }

    pub(crate) fn tracking_inactive(&mut self) {
        if let Some(filters) = &mut self.0 {
            filters.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(gaze: f32, eyelid: f32) -> EyeValues {
        EyeValues {
            left: [gaze, gaze],
            right: [gaze, gaze],
            combined: [gaze, gaze],
            eyelids: [eyelid, eyelid],
        }
    }

    #[test]
    fn first_sample_initializes_cleanly() {
        let mut filter = EyeFilters::new(OneEuroConfig::default());
        let values = EyeValues {
            left: [0.25, -0.5],
            right: [-0.25, 0.5],
            combined: [0.1, -0.1],
            eyelids: [0.75, 0.8],
        };

        assert_eq!(filter.filter(10.0, values), values);
    }

    #[test]
    fn repeated_stationary_gaze_jitter_is_reduced() {
        let mut filter = EyeFilters::new(OneEuroConfig::default());
        filter.filter(0.0, values(0.0, 0.5));
        let raw = [0.02, -0.02, 0.02, -0.02, 0.02, -0.02];
        let filtered: Vec<_> = raw
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                filter
                    .filter((index + 1) as f64 / 120.0, values(value, 0.5))
                    .left[0]
            })
            .collect();

        let raw_amplitude = raw.into_iter().map(f32::abs).fold(0.0, f32::max);
        let filtered_amplitude = filtered.into_iter().map(f32::abs).fold(0.0, f32::max);
        assert!(filtered_amplitude < raw_amplitude * 0.75);
    }

    #[test]
    fn fast_gaze_movement_is_more_responsive_with_beta() {
        let config = OneEuroConfig::default();
        let mut adaptive = EyeFilters::new(config);
        let mut fixed = EyeFilters::new(OneEuroConfig {
            beta: 0.0,
            ..config
        });
        adaptive.filter(0.0, values(0.0, 0.5));
        fixed.filter(0.0, values(0.0, 0.5));

        let adaptive_value = adaptive.filter(1.0 / 120.0, values(1.0, 0.5)).left[0];
        let fixed_value = fixed.filter(1.0 / 120.0, values(1.0, 0.5)).left[0];

        assert!(adaptive_value > fixed_value * 2.0);
        assert!(adaptive_value > 0.25);
    }

    #[test]
    fn reset_makes_next_sample_initialize_cleanly() {
        let mut filter = EyeFilters::new(OneEuroConfig::default());
        filter.filter(0.0, values(0.0, 0.0));
        filter.filter(1.0 / 120.0, values(1.0, 1.0));

        filter.reset();

        assert_eq!(filter.filter(1.0, values(-0.75, 0.25)), values(-0.75, 0.25));
    }

    #[test]
    fn non_monotonic_timestamp_resets_safely() {
        let mut filter = EyeFilters::new(OneEuroConfig::default());
        filter.filter(2.0, values(0.0, 0.0));
        filter.filter(2.01, values(1.0, 1.0));

        assert_eq!(filter.filter(1.0, values(-0.5, 0.25)), values(-0.5, 0.25));
    }

    #[test]
    fn zero_timestamp_delta_resets_safely() {
        let mut filter = EyeFilters::new(OneEuroConfig::default());
        filter.filter(2.0, values(0.0, 0.0));
        filter.filter(2.01, values(1.0, 1.0));

        assert_eq!(filter.filter(2.01, values(-0.5, 0.25)), values(-0.5, 0.25));
    }

    #[test]
    fn invalid_timestamp_resets_safely() {
        let mut filter = EyeFilters::new(OneEuroConfig::default());
        filter.filter(0.0, values(0.0, 0.0));
        filter.filter(0.01, values(1.0, 1.0));

        assert_eq!(
            filter.filter(f64::NAN, values(-0.5, 0.25)),
            values(-0.5, 0.25)
        );
        assert_eq!(
            filter.filter(f64::INFINITY, values(0.75, 0.8)),
            values(0.75, 0.8)
        );
        assert_eq!(filter.filter(1.0, values(-0.25, 0.6)), values(-0.25, 0.6));
    }

    #[test]
    fn large_timestamp_gap_resets_safely() {
        let mut filter = EyeFilters::new(OneEuroConfig::default());
        filter.filter(0.0, values(0.0, 0.0));
        filter.filter(0.01, values(1.0, 1.0));

        assert_eq!(filter.filter(1.0, values(-0.5, 0.25)), values(-0.5, 0.25));
    }

    #[test]
    fn tracking_inactivity_resets_filter_state() {
        let mut filter = OptionalEyeFilter::new(true, OneEuroConfig::default());
        filter.filter(0.0, values(0.0, 0.0));
        filter.filter(0.01, values(1.0, 1.0));

        filter.tracking_inactive();

        assert_eq!(filter.filter(5.0, values(-0.5, 0.25)), values(-0.5, 0.25));
    }

    #[test]
    fn disabled_filter_returns_exact_raw_values() {
        let mut filter = OptionalEyeFilter::new(false, OneEuroConfig::default());
        let raw = EyeValues {
            left: [0.123_456_7, -0.765_432_1],
            right: [-0.234_567_8, 0.876_543_2],
            combined: [0.345_678_9, -0.987_654_3],
            eyelids: [0.456_789, 0.654_321],
        };

        assert_eq!(filter.filter(f64::NAN, raw), raw);
    }

    #[test]
    fn eyelid_jitter_is_reduced() {
        let mut filter = EyeFilters::new(OneEuroConfig::default());
        filter.filter(0.0, values(0.0, 0.5));
        let raw = [0.52, 0.48, 0.52, 0.48, 0.52, 0.48];
        let filtered: Vec<_> = raw
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                filter
                    .filter((index + 1) as f64 / 120.0, values(0.0, value))
                    .eyelids[0]
            })
            .collect();

        let filtered_amplitude = filtered
            .into_iter()
            .map(|value| (value - 0.5).abs())
            .fold(0.0, f32::max);
        assert!(filtered_amplitude < 0.015);
    }

    #[test]
    fn fast_blink_like_transition_remains_responsive() {
        let config = OneEuroConfig::default();
        let mut adaptive = EyeFilters::new(config);
        let mut fixed = EyeFilters::new(OneEuroConfig {
            beta: 0.0,
            ..config
        });
        adaptive.filter(0.0, values(0.0, 1.0));
        fixed.filter(0.0, values(0.0, 1.0));

        let adaptive_value = adaptive.filter(1.0 / 120.0, values(0.0, 0.0)).eyelids[0];
        let fixed_value = fixed.filter(1.0 / 120.0, values(0.0, 0.0)).eyelids[0];

        assert!(adaptive_value < fixed_value * 0.8);
        assert!(adaptive_value < 0.75);
    }

    #[test]
    fn scalar_channels_have_independent_filter_state() {
        let mut filter = EyeFilters::new(OneEuroConfig::default());
        let baseline = values(0.0, 0.5);
        filter.filter(0.0, baseline);
        let changed = EyeValues {
            left: [1.0, baseline.left[1]],
            ..baseline
        };

        let output = filter.filter(1.0 / 120.0, changed);

        assert!(output.left[0] > 0.0);
        assert_eq!(output.left[1], baseline.left[1]);
        assert_eq!(output.right, baseline.right);
        assert_eq!(output.combined, baseline.combined);
        assert_eq!(output.eyelids, baseline.eyelids);
    }
}
