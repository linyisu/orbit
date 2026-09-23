//! Aligned, cached physical rows for side-by-side file approval.
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use similar::{DiffTag, TextDiff};
use std::time::Duration;

use super::{
    render::{fit_label, safe_output},
    transcript::wrap_line,
};

const FOLDED_COLOR: Color = Color::Rgb(170, 170, 170);

#[derive(Debug)]
pub(super) struct Cache {
    before: String,
    after: String,
    widths: (u16, u16),
    full: bool,
    pub left: Vec<Line<'static>>,
    pub right: Vec<Line<'static>>,
}

impl Cache {
    pub fn matches(&self, before: &str, after: &str, widths: (u16, u16), full: bool) -> bool {
        self.before == before && self.after == after && self.widths == widths && self.full == full
    }

    pub fn new(before: &str, after: &str, widths: (u16, u16), full: bool) -> Self {
        let mut cache = Self {
            before: before.into(),
            after: after.into(),
            widths,
            full,
            left: vec![],
            right: vec![],
        };
        let diff = TextDiff::configure()
            .timeout(Duration::from_millis(250))
            .diff_lines(before, after);
        let digits = diff
            .old_slices()
            .len()
            .max(diff.new_slices().len())
            .to_string()
            .len()
            .max(4);
        for op in diff.ops() {
            let old = op.old_range();
            let new = op.new_range();
            let equal = op.tag() == DiffTag::Equal;
            let len = old.len().max(new.len());
            for index in 0..len {
                if equal && !full && len > 8 && (3..len - 3).contains(&index) {
                    if index == 3 {
                        let text = format!("… {} 行未改动", len - 6);
                        cache.left.push(Line::styled(
                            fit_label(&text, widths.0 as usize, false),
                            Style::default().fg(FOLDED_COLOR),
                        ));
                        cache.right.push(Line::styled(
                            fit_label(&text, widths.1 as usize, false),
                            Style::default().fg(FOLDED_COLOR),
                        ));
                    }
                    continue;
                }
                let left = (index < old.len())
                    .then(|| (old.start + index + 1, diff.old_slices()[old.start + index]));
                let right = (index < new.len())
                    .then(|| (new.start + index + 1, diff.new_slices()[new.start + index]));
                let mut left = side(left, digits, if equal { ' ' } else { '-' }, widths.0);
                let mut right = side(right, digits, if equal { ' ' } else { '+' }, widths.1);
                let height = left.len().max(right.len());
                left.resize(height, Line::default());
                right.resize(height, Line::default());
                cache.left.extend(left);
                cache.right.extend(right);
            }
        }
        cache
    }
}

fn side(
    content: Option<(usize, &str)>,
    digits: usize,
    marker: char,
    width: u16,
) -> Vec<Line<'static>> {
    let Some((number, text)) = content else {
        return vec![Line::default()];
    };
    let text = safe_output(text.strip_suffix('\n').unwrap_or(text));
    let style = Style::default().fg(match marker {
        '-' => Color::Rgb(218, 139, 139),
        '+' => Color::Rgb(145, 195, 155),
        _ => Color::Reset,
    });
    let prefix = format!("{marker}{number:>digits$} │ ");
    let prefix_width = digits + 4;
    if usize::from(width) <= prefix_width {
        return vec![Line::styled(
            fit_label(&format!("{prefix}{text}"), width as usize, false),
            style,
        )];
    }
    let (wrapped, _) = wrap_line(&Line::styled(text, style), width - prefix_width as u16);
    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, mut line)| {
            // A single wide glyph may exceed a one-cell pane; never cross its border.
            if line.width() > usize::from(width) - prefix_width {
                line = Line::styled(
                    fit_label(&line.to_string(), usize::from(width) - prefix_width, false),
                    style,
                );
            }
            line.spans.insert(
                0,
                Span::styled(
                    if index == 0 {
                        prefix.clone()
                    } else {
                        format!(" {} │ ", " ".repeat(digits))
                    },
                    style,
                ),
            );
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_delete_and_long_replacement_keep_both_sides_aligned() {
        let diff = Cache::new(
            "same\nold\ntail\n",
            "same\n这是一段很长的替换文本abcdefghijk\ninserted\ntail\n",
            (20, 20),
            true,
        );
        assert_eq!(diff.left.len(), diff.right.len());
        let position = |rows: &[Line<'static>]| {
            rows.iter()
                .position(|line| line.to_string().contains("tail"))
                .unwrap()
        };
        assert_eq!(position(&diff.left), position(&diff.right));
        assert!(
            diff.left
                .iter()
                .chain(&diff.right)
                .all(|line| line.width() <= 20)
        );
        assert!(
            diff.left
                .iter()
                .any(|line| line.to_string().starts_with("-   2 │ "))
        );
        assert!(
            diff.right
                .iter()
                .any(|line| line.to_string().starts_with("+   3 │ "))
        );
        let deleted = Cache::new("a\nb\nc\n", "a\nc\n", (30, 30), true);
        assert_eq!(deleted.left[2].to_string().split("│ ").last(), Some("c"));
        assert_eq!(deleted.right[2].to_string().split("│ ").last(), Some("c"));
        assert!(deleted.right[1].to_string().is_empty());
    }

    #[test]
    fn folds_only_unchanged_lines_and_handles_new_empty_and_small_files() {
        let before = format!("{}old\n", "unchanged\n".repeat(100));
        let after = format!("{}new\n", "unchanged\n".repeat(100));
        let folded = Cache::new(&before, &after, (40, 40), false);
        let full = Cache::new(&before, &after, (40, 40), true);
        assert!(folded.left.len() < full.left.len());
        let folded_row = folded
            .left
            .iter()
            .find(|line| line.to_string().contains("行未改动"))
            .unwrap();
        assert_eq!(folded_row.style.fg, Some(FOLDED_COLOR));
        assert!(
            !folded
                .left
                .iter()
                .any(|line| line.to_string().contains("末尾"))
        );
        assert_eq!(folded.left.len(), folded.right.len());
        assert!(
            folded
                .left
                .iter()
                .any(|line| line.to_string().contains("old"))
        );
        assert!(
            folded
                .right
                .iter()
                .any(|line| line.to_string().contains("new"))
        );
        assert!(folded.matches(&before, &after, (40, 40), false));
        assert!(!folded.matches(&before, &after, (30, 40), false));
        let one_side_missing_newline = Cache::new("before\n", "after", (40, 40), false);
        assert_eq!(
            one_side_missing_newline.left.len(),
            one_side_missing_newline.right.len()
        );
        assert!(
            !one_side_missing_newline
                .right
                .iter()
                .any(|line| line.to_string().contains("末尾"))
        );
        for width in [0, 1, 8, 9, 20] {
            for (a, b) in [("", "新内容\u{1b}\t\n"), ("old", ""), ("", "")] {
                let diff = Cache::new(a, b, (width, width), true);
                assert_eq!(diff.left.len(), diff.right.len());
                assert!(
                    diff.left
                        .iter()
                        .chain(&diff.right)
                        .all(|line| line.width() <= width as usize)
                );
            }
        }
    }
}
