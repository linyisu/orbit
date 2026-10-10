//! Bounded provider-usage calibration, distinct from an exact tokenizer.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TokenCalibration {
    samples: Vec<(usize, u64)>,
}

impl TokenCalibration {
    pub fn merge(&mut self, newer: &Self) {
        for (estimated, actual) in &newer.samples {
            self.observe(*estimated, Some(*actual));
        }
    }

    pub fn observe(&mut self, estimated: usize, actual: Option<u64>) {
        if estimated == 0 {
            return;
        }

        if let Some(actual) = actual.filter(|n| *n > 0) {
            self.samples.push((estimated, actual));

            if self.samples.len() > 9 {
                self.samples.remove(0);
            }
        }
    }

    pub fn samples(&self) -> usize {
        self.samples.len()
    }

    pub fn factor(&self) -> f64 {
        let mut ratios: Vec<_> = self
            .samples
            .iter()
            .map(|(local, actual)| *actual as f64 / *local as f64)
            .collect();

        ratios.sort_by(f64::total_cmp);

        ratios.get(ratios.len() / 2).copied().unwrap_or(1.0)
    }

    pub fn estimate(&self, local: usize) -> usize {
        (local as f64 * self.factor()).ceil() as usize
    }

    /// Translate the independently configured input allowance into local
    /// estimator units; output capacity remains unchanged.
    pub fn local_limit(&self, total: usize, output: usize) -> usize {
        ((total.saturating_sub(output) as f64 / self.factor()).floor() as usize)
            .saturating_add(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn median_resists_one_outlier_and_keeps_input_budget_independent() {
        let mut c = TokenCalibration::default();

        assert_eq!(c.estimate(100), 100);

        c.observe(100, Some(200));
        c.observe(100, Some(190));
        c.observe(100, Some(9000));

        assert_eq!(c.estimate(150), 300);
        assert_eq!(c.local_limit(256_000, 32_000), 144_000);

        c.observe(0, Some(10));
        c.observe(100, None);
        c.observe(100, Some(0));

        assert_eq!(c.samples(), 3);

        for _ in 0..15 {
            c.observe(100, Some(110));
        }

        assert_eq!(c.samples(), 9);
        assert_eq!(c.estimate(1000), 1100);
    }
}
