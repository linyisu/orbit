use std::time::{Duration, Instant};

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::Color,
};
use unicode_width::UnicodeWidthStr;

use super::theme::{THEME, color_rgb};

#[derive(Clone, Copy, Debug)]
struct Trail {
    from: Position,
    to: Position,
    started: Instant,
    duration: Duration,
}

#[derive(Debug, Default)]
pub(super) struct CursorMotion {
    pub enabled: bool,
    previous: Option<(Rect, Rect, Position)>,
    trail: Option<Trail>,
}

impl CursorMotion {
    pub fn clear(&mut self) {
        self.previous = None;
        self.trail = None;
    }

    pub fn update(&mut self, screen: Rect, area: Rect, position: Option<Position>, now: Instant) {
        let Some(position) =
            position.filter(|p| self.enabled && screen.contains(*p) && area.contains(*p))
        else {
            self.clear();
            return;
        };
        let previous = self
            .previous
            .replace((screen, area, position));
        if self
            .trail
            .is_some_and(|trail| now.duration_since(trail.started) >= trail.duration)
        {
            self.trail = None;
        }

        if let Some((old_screen, old_area, from)) = previous {
            if old_screen != screen || old_area != area || from.y != position.y {
                self.trail = None;
            } else if from != position {
                self.trail = Some(Trail {
                    from,
                    to: position,
                    started: now,
                    duration: Duration::from_millis(if from.x.abs_diff(position.x) <= 2 {
                        40
                    } else {
                        100
                    }),
                });
            }
        }
    }

    pub fn active(&self, now: Instant) -> bool {
        self.trail
            .is_some_and(|trail| now.duration_since(trail.started) < trail.duration)
    }

    // Animate only the tail; the native caret and IME position never lag behind editing.
    pub fn paint(&self, buffer: &mut Buffer, now: Instant) {
        let Some(trail) = self.trail else {
            return;
        };
        let Some((_, area, _)) = self.previous else {
            return;
        };
        let area = area.intersection(buffer.area);
        if !area.contains(trail.to) {
            return;
        }
        let progress = now
            .duration_since(trail.started)
            .as_secs_f64()
            / trail.duration.as_secs_f64();
        if progress >= 1.0 {
            return;
        }
        let eased = 1.0 - (1.0 - progress).powi(3);
        let tail = (f64::from(trail.from.x)
            + (f64::from(trail.to.x) - f64::from(trail.from.x)) * eased)
            .round() as u16;
        let columns = if tail < trail.to.x {
            tail.max(trail.to.x.saturating_sub(3))..trail.to.x
        } else {
            trail.to.x.saturating_add(1)
                ..tail
                    .saturating_add(1)
                    .min(trail.to.x.saturating_add(4))
        };
        let mut x = area.x;
        while x < area.right() {
            let width = buffer[(x, trail.to.y)]
                .symbol()
                .width()
                .max(1) as u16;
            let end = x
                .saturating_add(width)
                .min(area.right());
            if columns.start < end && columns.end > x && !(x..end).contains(&trail.to.x) {
                let base = color_rgb(buffer[(x, trail.to.y)].bg)
                    .or_else(|| color_rgb(THEME.border_muted))
                    .unwrap();
                let target = color_rgb(THEME.caret).unwrap();
                let strength = (1.0 - progress) * 0.35;
                let mix = |base: u8, target: u8| {
                    (f64::from(base) + (f64::from(target) - f64::from(base)) * strength).round()
                        as u8
                };
                let color = Color::Rgb(
                    mix(base.0, target.0),
                    mix(base.1, target.1),
                    mix(base.2, target.2),
                );
                for column in x..end {
                    buffer[(column, trail.to.y)].set_bg(color);
                }
            }

            x = end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_keeps_the_head_immediate_and_stops_requesting_frames_at_rest() {
        let area = Rect::new(0, 0, 20, 2);
        let now = Instant::now();
        let mut motion = CursorMotion {
            enabled: true,
            ..Default::default()
        };
        motion.update(area, area, Some((2, 0).into()), now);
        motion.update(area, area, Some((8, 0).into()), now);
        assert_eq!(motion.previous.unwrap().2, Position::new(8, 0));
        assert!(motion.active(now + Duration::from_millis(99)));
        assert!(!motion.active(now + Duration::from_millis(100)));
        motion.update(
            area,
            area,
            Some((8, 0).into()),
            now + Duration::from_millis(100),
        );
        assert!(motion.trail.is_none());
        motion.update(
            area,
            area,
            Some((9, 0).into()),
            now + Duration::from_millis(101),
        );
        assert!(!motion.active(now + Duration::from_millis(141)));
    }

    #[test]
    fn tail_preserves_glyphs_colors_and_wide_continuation_cells() {
        let area = Rect::new(0, 0, 20, 2);
        let now = Instant::now();
        let mut motion = CursorMotion {
            enabled: true,
            ..Default::default()
        };
        let mut buffer = Buffer::with_lines(["Orbit你好👩‍💻"]);
        buffer.resize(area);
        buffer[(5, 0)].set_fg(Color::Green);
        let before = buffer.clone();
        motion.update(area, area, Some((4, 0).into()), now);
        motion.update(area, area, Some((9, 0).into()), now);
        motion.paint(&mut buffer, now);
        for (old, new) in before
            .content
            .iter()
            .zip(&buffer.content)
        {
            assert_eq!(old.symbol(), new.symbol());
            assert_eq!(old.fg, new.fg);
        }

        assert_eq!(buffer[(5, 0)].bg, buffer[(6, 0)].bg);
        assert_eq!(buffer[(7, 0)].bg, buffer[(8, 0)].bg);
        assert_eq!(buffer[(9, 0)].bg, Color::Reset);
        assert!(
            buffer
                .content
                .iter()
                .filter(|cell| cell.bg != Color::Reset)
                .count()
                <= 4
        );
    }

    #[test]
    fn focus_layout_changes_and_disabled_motion_remove_the_tail() {
        let area = Rect::new(0, 0, 20, 2);
        let now = Instant::now();
        let mut motion = CursorMotion {
            enabled: true,
            ..Default::default()
        };

        for reset in 0..4 {
            motion.clear();
            motion.update(area, area, Some((1, 0).into()), now);
            motion.update(area, area, Some((5, 0).into()), now);
            match reset {
                0 => motion.update(area, area, None, now),
                1 => motion.update(Rect::new(0, 0, 30, 2), area, Some((5, 0).into()), now),
                2 => motion.update(area, area, Some((5, 1).into()), now),
                _ => {
                    motion.enabled = false;
                    motion.update(area, area, Some((5, 0).into()), now);
                }
            }

            assert!(!motion.active(now));
        }
    }
}
