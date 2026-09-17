//! Bound terminal wheel-event bursts without delaying the first movement.
use crossterm::event::{Event, MouseEventKind};
use std::time::{Duration, Instant};

pub(super) const LINES: usize = 8;
const INTERVAL: Duration = Duration::from_millis(40);

#[derive(Default)]
pub(super) struct Wheel {
    last: Option<(MouseEventKind, Instant)>,
}

impl Wheel {
    pub fn accept(&mut self, event: &Event, now: Instant) -> bool {
        let Event::Mouse(mouse) = event else {
            self.last = None;
            return true;
        };
        if !matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            return true;
        }
        if self.last.is_some_and(|(direction, last)| {
            direction == mouse.kind && now.saturating_duration_since(last) < INTERVAL
        }) {
            // Do not accumulate a backlog: stopping the wheel stops the viewport.
            return false;
        }
        self.last = Some((mouse.kind, now));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};

    fn wheel(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn dense_bursts_move_eight_lines_and_reversal_is_immediate() {
        let mut filter = Wheel::default();
        let now = Instant::now();
        let up = wheel(MouseEventKind::ScrollUp);
        let mut moved = 0;
        for millis in 0..40 {
            if filter.accept(&up, now + Duration::from_millis(millis)) {
                moved += LINES;
            }
        }
        assert_eq!(moved, 8);
        assert!(filter.accept(
            &wheel(MouseEventKind::ScrollDown),
            now + Duration::from_millis(39)
        ));
        assert!(!filter.accept(
            &wheel(MouseEventKind::ScrollDown),
            now + Duration::from_millis(40)
        ));
        assert!(filter.accept(
            &wheel(MouseEventKind::ScrollDown),
            now + Duration::from_millis(79)
        ));
    }

    #[test]
    fn separate_ticks_and_keyboard_input_remain_responsive() {
        let mut filter = Wheel::default();
        let now = Instant::now();
        let up = wheel(MouseEventKind::ScrollUp);
        for tick in 0..5 {
            assert!(filter.accept(&up, now + Duration::from_millis(tick * 80)));
        }
        let key = Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(filter.accept(&key, now + Duration::from_millis(321)));
        assert!(filter.accept(&up, now + Duration::from_millis(322)));
    }
}
