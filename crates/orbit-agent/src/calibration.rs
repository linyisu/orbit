//! Bounded calibration of the local token estimate against provider usage.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TokenCalibration {
    samples: Vec<(usize, u64)>,
}

impl TokenCalibration {
    pub fn observe(&mut self, estimated: usize, actual: Option<u64>) {
        let Some(actual) = actual.filter(|actual| *actual > 0) else {
            return;
        };
        if estimated == 0 {
            return;
        }

        self.samples.push((estimated, actual));
        if self.samples.len() > 9 {
            self.samples.remove(0);
        }
    }

    pub fn samples(&self) -> usize {
        self.samples.len()
    }

    pub fn factor(&self) -> f64 {
        let mut ratios = self
            .samples
            .iter()
            .map(|(estimated, actual)| *actual as f64 / *estimated as f64)
            .collect::<Vec<_>>();
        ratios.sort_by(f64::total_cmp);
        ratios
            .get(ratios.len() / 2)
            .copied()
            .unwrap_or(1.0)
            .clamp(0.5, 4.0)
    }

    pub fn estimate(&self, local: usize) -> usize {
        (local as f64 * self.factor()).ceil() as usize
    }

    pub fn local_input_budget(&self, window: usize, output_reserve: usize) -> usize {
        (window.saturating_sub(output_reserve) as f64 / self.factor()).floor() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_resists_outliers_and_only_uses_valid_provider_usage() {
        let mut calibration = TokenCalibration::default();
        calibration.observe(100, Some(200));
        calibration.observe(100, Some(190));
        calibration.observe(100, Some(9000));
        calibration.observe(0, Some(10));
        calibration.observe(100, None);

        assert_eq!(calibration.samples(), 3);
        assert_eq!(calibration.estimate(150), 300);
        assert_eq!(calibration.local_input_budget(256_000, 32_000), 112_000);
    }
}
