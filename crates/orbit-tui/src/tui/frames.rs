//! Coalesce redraw requests without putting input behind the stream refresh tick.
//! Design reference: openai/codex, tui/src/tui/frame_requester.rs.
use std::time::Duration;

use tokio::time::Instant;

const MIN_FRAME_INTERVAL: Duration = Duration::from_micros(8_334);

#[derive(Default)]
pub(super) struct Frames {
    pending: Option<Instant>,
    last_draw: Option<Instant>,
}

impl Frames {
    pub fn request(&mut self, now: Instant) {
        let target = self
            .last_draw
            .map_or(now, |last| now.max(last + MIN_FRAME_INTERVAL));
        self.pending = Some(
            self.pending
                .map_or(target, |previous| previous.min(target)),
        );
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn deadline(&self) -> Instant {
        self.pending
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400))
    }

    pub fn drawn(&mut self, now: Instant) {
        self.last_draw = Some(now);
        self.pending = None;
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

        for micros in 1..1_000 {
            frames.request(now + Duration::from_micros(micros));
        }

        assert_eq!(frames.deadline(), now + MIN_FRAME_INTERVAL);

        frames.drawn(now + MIN_FRAME_INTERVAL);

        assert!(!frames.pending());

        frames.request(now + Duration::from_millis(20));

        assert_eq!(frames.deadline(), now + Duration::from_millis(20));
    }
}
