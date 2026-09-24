//! Aligned physical rows for the write-approval preview.

use std::time::Duration;

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use similar::{DiffTag, TextDiff};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const REMOVED: Color = Color::Rgb(218, 139, 139);
const ADDED: Color = Color::Rgb(145, 195, 155);
const UNCHANGED: Color = Color::Rgb(190, 190, 190);

pub struct ApprovalDiff {
    pub widths: (u16, u16),
    pub left: Vec<Line<'static>>,
    pub right: Vec<Line<'static>>,
}

impl ApprovalDiff {
    pub fn new(before: &str, after: &str, widths: (u16, u16), full: bool) -> Self {
        let diff = TextDiff::configure()
            .timeout(Duration::from_millis(250))
            .diff_lines(before, after);
        let digits = diff
            .old_slices()
            .len()
            .max(diff.new_slices().len())
            .to_string()
            .len()
            .max(2);
        let mut result = Self {
            widths,
            left: Vec::new(),
            right: Vec::new(),
        };

        for operation in diff.ops() {
            let old = operation.old_range();
            let new = operation.new_range();
            let equal = operation.tag() == DiffTag::Equal;
            let length = old.len().max(new.len());
            for offset in 0..length {
                if equal && !full && length > 10 && (3..length - 3).contains(&offset) {
                    if offset == 3 {
                        let label = format!("… {} unchanged lines", length - 6);
                        result.left.push(folded(&label, widths.0));
                        result.right.push(folded(&label, widths.1));
                    }
                    continue;
                }

                let before = (offset < old.len()).then(|| {
                    (
                        old.start + offset + 1,
                        diff.old_slices()[old.start + offset],
                    )
                });
                let after = (offset < new.len()).then(|| {
                    (
                        new.start + offset + 1,
                        diff.new_slices()[new.start + offset],
                    )
                });
                let mut left = side(before, digits, if equal { ' ' } else { '-' }, widths.0);
                let mut right = side(after, digits, if equal { ' ' } else { '+' }, widths.1);
                let height = left.len().max(right.len());
                left.resize(height, Line::default());
                right.resize(height, Line::default());
                result.left.extend(left);
                result.right.extend(right);
            }
        }

        result
    }
}

fn folded(label: &str, width: u16) -> Line<'static> {
    Line::styled(fit(label, width as usize), Style::default().fg(UNCHANGED))
}

fn side(
    content: Option<(usize, &str)>,
    digits: usize,
    marker: char,
    width: u16,
) -> Vec<Line<'static>> {
    let Some((number, source)) = content else {
        return vec![Line::default()];
    };
    let style = Style::default().fg(match marker {
        '-' => REMOVED,
        '+' => ADDED,
        _ => Color::Reset,
    });
    let prefix = format!("{marker}{number:>digits$} │ ");
    let continuation = format!(" {} │ ", " ".repeat(digits));
    let prefix_width = prefix.width();
    let width = width as usize;
    if width <= prefix_width {
        return vec![Line::styled(fit(&prefix, width), style)];
    }

    let text = visible(source.strip_suffix('\n').unwrap_or(source));
    let available = width - prefix_width;
    let mut chunks = vec![String::new()];
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let size = grapheme.width();
        if size > available {
            continue;
        }
        if used + size > available && used > 0 {
            chunks.push(String::new());
            used = 0;
        }
        chunks.last_mut().unwrap().push_str(grapheme);
        used += size;
    }
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            Line::from(vec![
                Span::styled(
                    if index == 0 { &prefix } else { &continuation }.to_owned(),
                    style,
                ),
                Span::styled(chunk, style),
            ])
        })
        .collect()
}

fn visible(text: &str) -> String {
    let mut output = String::new();
    for character in text.chars() {
        match character {
            '\t' => output.push_str("    "),
            '\r' => output.push_str("\\r"),
            '\x1b' => output.push_str("\\x1b"),
            c if c.is_control() => output.push('?'),
            c => output.push(c),
        }
    }
    output
}

fn fit(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let size = grapheme.width();
        if used + size > width {
            break;
        }
        output.push_str(grapheme);
        used += size;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_and_wrapped_rows_stay_aligned_with_shared_scroll_indices() {
        let diff = ApprovalDiff::new(
            "same\nold\ntail\n",
            "same\n这是一段很长的替换文本abcdefghijk\ninserted\ntail\n",
            (20, 20),
            false,
        );

        assert_eq!(diff.left.len(), diff.right.len());
        let tail_row = |rows: &[Line<'static>]| {
            rows.iter()
                .position(|line| line.to_string().contains("tail"))
                .unwrap()
        };
        assert_eq!(tail_row(&diff.left), tail_row(&diff.right));
        assert!(
            diff.left
                .iter()
                .chain(&diff.right)
                .all(|line| line.width() <= 20)
        );
    }

    #[test]
    fn long_unchanged_runs_fold_and_control_characters_remain_visible() {
        let before = format!("{}old\n", "same\n".repeat(100));
        let after = format!("{}new\x1b\t\n", "same\n".repeat(100));
        let diff = ApprovalDiff::new(&before, &after, (32, 32), false);
        let full = ApprovalDiff::new(&before, &after, (32, 32), true);

        assert!(
            diff.left
                .iter()
                .any(|line| line.to_string().contains("unchanged lines"))
        );
        assert!(
            diff.right
                .iter()
                .any(|line| line.to_string().contains("\\x1b"))
        );
        assert_eq!(diff.left.len(), diff.right.len());
        assert!(diff.left.len() < full.left.len());
    }

    #[test]
    fn new_files_and_tiny_panes_never_cross_their_borders() {
        for width in [0, 1, 8, 20] {
            let diff = ApprovalDiff::new("", "👩‍💻\tnew\n", (width, width), false);

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
