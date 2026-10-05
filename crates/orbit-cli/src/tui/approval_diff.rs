//! Aligned, cached physical rows for side-by-side file approval.
use std::time::Duration;

use ratatui::{
    style::{Color, Style},
    text::Line,
};
use similar::{DiffTag, TextDiff};

use super::{code, render::fit_label};

const FOLDED_COLOR: Color = crate::tui::theme::THEME.secondary;

#[derive(Debug)]
pub(super) struct Cache {
    before: String,
    after: String,
    widths: (u16, u16),
    full: bool,
    path: String,
    pub left: Vec<Line<'static>>,
    pub right: Vec<Line<'static>>,
}

impl Cache {
    pub fn matches(
        &self,
        before: &str,
        after: &str,
        widths: (u16, u16),
        full: bool,
        path: &str,
    ) -> bool {
        self.before == before
            && self.after == after
            && self.widths == widths
            && self.full == full
            && self.path == path
    }

    pub fn new(before: &str, after: &str, widths: (u16, u16), full: bool, path: &str) -> Self {
        let mut cache = Self {
            before: before.into(),
            after: after.into(),
            widths,
            full,
            path: path.into(),
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
        let old_lines = super::code::file_lines(path, before);
        let new_lines = super::code::file_lines(path, after);

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
                    .then(|| (old.start + index + 1, &old_lines[old.start + index]));
                let right = (index < new.len())
                    .then(|| (new.start + index + 1, &new_lines[new.start + index]));
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
    content: Option<(usize, &Line<'static>)>,
    digits: usize,
    marker: char,
    width: u16,
) -> Vec<Line<'static>> {
    let Some((number, text)) = content else {
        return vec![Line::default()];
    };

    code::render_line(
        text,
        code::Options {
            gutter: code::Gutter::Diff {
                number: number as u64,
                digits,
                indent: 0,
            },
            width: Some(width),
            change: match marker {
                '-' => Some(code::Change::Removed),
                '+' => Some(code::Change::Added),
                _ => None,
            },
            ..Default::default()
        },
    )
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
            "orbit.txt",
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

        let deleted = Cache::new("a\nb\nc\n", "a\nc\n", (30, 30), true, "orbit.txt");

        assert_eq!(deleted.left[2].to_string().split("│ ").last(), Some("c"));
        assert_eq!(deleted.right[2].to_string().split("│ ").last(), Some("c"));
        assert!(deleted.right[1].to_string().is_empty());
    }

    #[test]
    fn folds_only_unchanged_lines_and_handles_new_empty_and_small_files() {
        let before = format!("{}old\n", "unchanged\n".repeat(100));
        let after = format!("{}new\n", "unchanged\n".repeat(100));
        let folded = Cache::new(&before, &after, (40, 40), false, "orbit.txt");
        let full = Cache::new(&before, &after, (40, 40), true, "orbit.txt");

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
        assert!(folded.matches(&before, &after, (40, 40), false, "orbit.txt"));
        assert!(!folded.matches(&before, &after, (30, 40), false, "orbit.txt"));
        assert!(!folded.matches(&before, &after, (40, 40), false, "orbit.rs"));

        let one_side_missing_newline =
            Cache::new("before\n", "after", (40, 40), false, "orbit.txt");

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
                let diff = Cache::new(a, b, (width, width), true, "orbit.txt");

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
