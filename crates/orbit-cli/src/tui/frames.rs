//! Coalesce redraw requests without putting input behind the stream refresh tick.
//! Design reference: openai/codex, tui/src/tui/frame_requester.rs.
use std::time::Duration;
use tokio::time::Instant;

const MIN_FRAME_INTERVAL: Duration = Duration::from_micros(8_334);
const RESIZE_QUIET_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Default)]
pub(super) struct Frames {
    pending: Option<Instant>,
    last_draw: Option<Instant>,
    resize_barrier: Option<Instant>,
}

impl Frames {
    pub fn request(&mut self, now: Instant) {
        let target = self
            .last_draw
            .map_or(now, |last| now.max(last + MIN_FRAME_INTERVAL));
        self.pending = Some(self.pending.map_or(target, |previous| previous.min(target)));
    }

    /// Wait until a burst of terminal resize events settles. Ratatui must clear
    /// its fullscreen viewport whenever its dimensions change, so drawing every
    /// intermediate size exposes repeated full repaints while a window is dragged.
    pub fn request_resize(&mut self, now: Instant) {
        self.request(now);
        self.resize_barrier = Some(now + RESIZE_QUIET_INTERVAL);
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn deadline(&self) -> Instant {
        let pending = self
            .pending
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(86400));
        self.resize_barrier
            .map_or(pending, |barrier| pending.max(barrier))
    }

    pub fn drawn(&mut self, now: Instant) {
        self.last_draw = Some(now);
        self.pending = None;
        self.resize_barrier = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_immediate_when_idle_and_bursts_share_the_earliest_frame() {
        let mut frames = Frames::default();
        let now = Instant::now();
        frames.request(now);
        assert_eq!(frames.deadline(), now);
        frames.drawn(now);
        for micros in 1..1000 {
            frames.request(now + Duration::from_micros(micros));
        }
        assert_eq!(frames.deadline(), now + MIN_FRAME_INTERVAL);
        frames.drawn(now + MIN_FRAME_INTERVAL);
        assert!(!frames.pending());
        frames.request(now + Duration::from_millis(20));
        assert_eq!(frames.deadline(), now + Duration::from_millis(20));
    }

    #[test]
    fn resize_bursts_wait_until_the_latest_size_settles() {
        let mut frames = Frames::default();
        let now = Instant::now();
        frames.request_resize(now);
        assert_eq!(frames.deadline(), now + RESIZE_QUIET_INTERVAL);

        frames.request(now + Duration::from_millis(10));
        assert_eq!(frames.deadline(), now + RESIZE_QUIET_INTERVAL);

        frames.request_resize(now + Duration::from_millis(30));
        assert_eq!(
            frames.deadline(),
            now + Duration::from_millis(30) + RESIZE_QUIET_INTERVAL
        );
        frames.drawn(now + Duration::from_millis(80));
        assert!(!frames.pending());
    }
}
