//! Size short execution records from their laid-out rows, including spacing.
use gpui_kit::{Pixels, Size};

#[derive(Default)]
pub(super) struct ProcessHeight {
    revision: u64,
    width: Option<Pixels>,
    rows: Vec<Option<Pixels>>,
}

impl ProcessHeight {
    pub(super) fn invalidate(&mut self, count: usize) {
        self.refresh();
        self.rows = vec![None; count];
    }

    /// Keep the last measured extent until replacement measurements arrive, rather than bouncing
    /// short groups up to the cap on every text delta.
    pub(super) fn refresh(&mut self) {
        self.revision += 1;
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn record(&mut self, revision: u64, index: usize, size: Size<Pixels>) {
        if revision != self.revision || index >= self.rows.len() {
            return;
        }
        if self.width != Some(size.width) {
            self.width = Some(size.width);
            self.rows.fill(None);
        }
        self.rows[index] = Some(size.height);
    }

    /// Start with enough space to measure short groups. Large groups remain virtualized at the cap;
    /// they need no full-history measurement pass.
    pub(super) fn height(&self, limit: Pixels) -> Pixels {
        let total: Option<Pixels> = self.rows.iter().copied().sum();
        total.map_or(limit, |height| height.ceil().min(limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{px, size};

    #[test]
    fn short_rows_include_all_padding_instead_of_a_line_count_estimate() {
        let mut layout = ProcessHeight::default();
        layout.invalidate(4);
        for (index, height) in [39., 39., 39., 47.]
            .into_iter()
            .enumerate()
        {
            layout.record(layout.revision(), index, size(px(800.), px(height)));
        }
        assert_eq!(layout.height(px(300.)), px(164.));
        layout.refresh();
        assert_eq!(layout.height(px(300.)), px(164.));
        // Fractional layout heights must never be clipped by rounding down.
        layout.record(layout.revision(), 3, size(px(800.), px(47.25)));
        assert_eq!(layout.height(px(300.)), px(165.));
    }

    #[test]
    fn expansion_and_collapse_use_measured_content_and_keep_the_height_cap() {
        let mut layout = ProcessHeight::default();
        layout.invalidate(1);
        layout.record(layout.revision(), 0, size(px(800.), px(120.)));
        assert_eq!(layout.height(px(300.)), px(120.));
        layout.record(layout.revision(), 0, size(px(800.), px(900.)));
        assert_eq!(layout.height(px(300.)), px(300.));
        layout.record(layout.revision(), 0, size(px(800.), px(40.)));
        assert_eq!(layout.height(px(300.)), px(40.));
    }

    #[test]
    fn stale_rows_and_old_widths_cannot_shrink_a_new_layout() {
        let mut layout = ProcessHeight::default();
        layout.invalidate(2);
        let old = layout.revision();
        for index in 0..2 {
            layout.record(old, index, size(px(800.), px(40.)));
        }
        layout.record(old, 0, size(px(400.), px(80.)));
        assert_eq!(layout.height(px(300.)), px(300.));
        layout.record(old, 1, size(px(400.), px(80.)));
        assert_eq!(layout.height(px(300.)), px(160.));
        layout.invalidate(3);
        layout.record(old, 2, size(px(400.), px(1.)));
        assert_eq!(layout.height(px(300.)), px(300.));
    }
}
