//! A tool's own physical-row viewport, shared by standalone and grouped cards.
//! Scrolling reuses parsed rows, including their link, syntax and media metadata.
use super::*;

pub(in crate::tui) const MAX_TOOL_BODY_ROWS: usize = 15;

#[derive(Debug)]
pub(super) struct ToolViewport {
    source: Vec<Row>,
    pub(super) rows: Range<usize>,
    total: usize,
    offset: usize,
}

impl Entry {
    pub(super) fn limit_tool_body(&mut self, rows: Range<usize>, offset: usize, width: u16) {
        if rows.is_empty() {
            return;
        }
        let top = self.rows[rows.start].start;
        let total = self.rows[rows.end - 1].start + self.rows[rows.end - 1].height - top;
        if total <= MAX_TOOL_BODY_ROWS {
            return;
        }
        let mut source = self.rows[rows.clone()].to_vec();
        for row in &mut source {
            row.start -= top;
        }
        self.set_tool_offset(
            ToolViewport {
                source,
                rows,
                total,
                offset: 0,
            },
            offset,
            width,
        );
    }

    fn set_tool_offset(&mut self, mut view: ToolViewport, offset: usize, width: u16) {
        view.offset = offset.min(
            view.total
                .saturating_sub(MAX_TOOL_BODY_ROWS),
        );
        let bottom = (view.offset + MAX_TOOL_BODY_ROWS).min(view.total);
        let mut visible = Vec::new();
        let first = view
            .source
            .partition_point(|row| row.start + row.height <= view.offset);
        for row in view.source[first..]
            .iter()
            .take_while(|row| row.start < bottom)
        {
            let start = row.start.max(view.offset);
            let end = (row.start + row.height).min(bottom);
            if start >= end {
                continue;
            }
            let skip = start - row.start;
            let count = end - start;
            let mut row = row.clone();
            row.wrapped = row.wrapped[skip..skip + count].to_vec();
            row.ranges = row.ranges[skip..skip + count].to_vec();
            row.decorations = row.decorations[skip..skip + count].to_vec();
            row.height = count;
            if skip > 0
                && let Some(gutter) = &mut row.gutter
            {
                gutter.first = gutter.continuation.clone();
            }
            visible.push(row);
        }
        let old = view.rows.clone();
        let new_end = old.start + visible.len();
        let body_disclosure = self
            .disclosures
            .iter()
            .any(|row| old.contains(row));
        self.disclosures = self
            .disclosures
            .iter()
            .filter_map(|&row| {
                if row < old.start {
                    Some(row)
                } else if row >= old.end {
                    Some(row - old.end + new_end)
                } else {
                    None
                }
            })
            .collect();
        if body_disclosure {
            self.disclosures
                .extend(old.start..new_end);
        }
        self.rows.splice(old, visible);
        view.rows.end = new_end;

        // Put the position in the existing lower border without consuming
        // any of the content rows reserved by MAX_TOOL_BODY_ROWS.
        if view.total > MAX_TOOL_BODY_ROWS
            && let Some(border) = self.rows.get_mut(new_end)
        {
            let text = border.line.to_string();
            let ends = (
                text.chars().next().unwrap_or('╰'),
                text.chars().next_back().unwrap_or('╯'),
            );
            let label = render::fit_label(
                &format!(" {}–{} / {} ", view.offset + 1, bottom, view.total),
                width.saturating_sub(2) as usize,
                false,
            );
            let line = Line::from(vec![
                Span::styled(
                    format!(
                        "{}{}",
                        ends.0,
                        "─".repeat(
                            (width.saturating_sub(2) as usize).saturating_sub(label.width())
                        )
                    ),
                    Style::default().fg(THEME.tool_border),
                ),
                Span::styled(label, Style::default().fg(THEME.tool_summary)),
                Span::styled(ends.1.to_string(), Style::default().fg(THEME.tool_border)),
            ]);
            let mut footer = Entry::default();
            footer.push_linked(super::super::markdown::LinkedLine::chrome(line), width);
            if let Some(row) = footer.rows.pop() {
                *border = row;
            }
        }
        let previous_height = self.height;
        self.height = 0;
        for row in &mut self.rows {
            row.start = self.height;
            self.height += row.height;
        }
        self.end = self
            .end
            .saturating_add_signed(self.height as isize - previous_height as isize);
        self.tool_body = Some(view);
    }
}

impl TranscriptLayout {
    /// Consume wheel events only over an overflowing tool body. At its limits
    /// keep the outer transcript still, rather than chaining scroll containers.
    pub(in crate::tui) fn scroll_tool_body(
        &mut self,
        area: Rect,
        offset: usize,
        point: (u16, u16),
        lines: isize,
    ) -> bool {
        if !area.contains(point.into())
            || point.0 < area.x.saturating_add(2)
            || point.0 >= area.right().saturating_sub(2)
        {
            return false;
        }
        let absolute = offset + usize::from(point.1 - area.y);
        let index = self
            .entries
            .partition_point(|entry| entry.end <= absolute);
        let Some(entry) = self.entries.get_mut(index) else {
            return false;
        };
        let Some(view) = &entry.tool_body else {
            return false;
        };
        let start = entry.end - entry.height;
        let top = entry.rows[view.rows.start].start;
        let bottom = entry
            .rows
            .get(view.rows.end)
            .map_or(entry.height, |row| row.start + row.height);
        if view.total <= MAX_TOOL_BODY_ROWS || !(start + top..start + bottom).contains(&absolute) {
            return false;
        }
        let next = view
            .offset
            .saturating_add_signed(lines)
            .min(
                view.total
                    .saturating_sub(MAX_TOOL_BODY_ROWS),
            );
        if next != view.offset {
            let view = entry.tool_body.take().unwrap();
            entry.set_tool_offset(view, next, self.width);
            self.tool_scroll.insert(index, next);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{ToolRow, ToolStatus};

    fn tool(output: String) -> TranscriptItem {
        TranscriptItem::Tool(ToolRow {
            id: "viewport".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({
                "path": "viewport.txt"
            })
            .to_string(),
            status: ToolStatus::Finished,
            output,
        })
    }

    fn body(layout: &TranscriptLayout) -> Vec<String> {
        let entry = &layout.entries[0];
        let view = entry.tool_body.as_ref().unwrap();
        entry.rows[view.rows.clone()]
            .iter()
            .flat_map(|row| {
                row.wrapped
                    .iter()
                    .map(ToString::to_string)
            })
            .collect()
    }

    #[test]
    fn web_links_keep_absolute_hit_targets_while_scrolling_grouped_and_standalone_bodies() {
        for grouped in [false, true] {
            let items = (0..if grouped { 2 } else { 1 })
                .map(|i| {
                    TranscriptItem::Tool(ToolRow {
                        id: format!("fetch-{i}"),
                        name: "web_fetch".into(),
                        arguments: serde_json::json!({
                            "url": "https://docs.orbit.example/topic/"
                        })
                        .to_string(),
                        status: ToolStatus::Finished,
                        output: serde_json::json!({
                            "url": "https://docs.orbit.example/topic/",
                            "content": (1..=16)
                                .map(|n| format!("* [很长的文档链接 {n}][{n}]\n"))
                                .collect::<String>(),
                            "links": (1..=16)
                                .map(|n| serde_json::json!({
                                    "n": n,
                                    "url": format!(
                                        "https://docs.orbit.example/page/{n}"
                                    )
                                }))
                                .collect::<Vec<_>>()
                        })
                        .to_string(),
                    })
                })
                .collect::<Vec<_>>();
            for width in [24, 80, 40] {
                let mut layout = TranscriptLayout::default();
                layout.update_expanded(
                    &items,
                    &(0..items.len()).collect(),
                    &if grouped {
                        [0].into()
                    } else {
                        Default::default()
                    },
                    width,
                );
                let area = Rect::new(3, 2, width, layout.height() as u16);
                for index in 0..items.len() {
                    let entry = &layout.entries[index];
                    let view = entry.tool_body.as_ref().unwrap();
                    let point = (
                        area.x + 3,
                        area.y
                            + (entry.end - entry.height + entry.rows[view.rows.start].start) as u16,
                    );
                    let mut targets = std::collections::BTreeSet::new();
                    loop {
                        for y in area.y..area.bottom() {
                            for x in area.x..area.right() {
                                if let Some(url) = layout.link_at(area, 0, (x, y)) {
                                    targets.insert(url.to_owned());
                                }
                            }
                        }
                        let before = layout.tool_scroll.get(&index).copied();
                        assert!(layout.scroll_tool_body(area, 0, point, 1));
                        if before == layout.tool_scroll.get(&index).copied() {
                            break;
                        }
                    }
                    for n in 1..=16 {
                        assert!(
                            targets.contains(&format!("https://docs.orbit.example/page/{n}")),
                            "grouped={grouped} width={width} page={n}"
                        );
                    }
                    assert!(
                        targets
                            .iter()
                            .all(|url| crate::tui::browser_url(url).is_ok())
                    );
                }
            }
        }
    }

    #[test]
    fn tool_viewport_counts_wrapped_rows_and_keeps_position_across_redraw_and_resize() {
        let mut items = [tool(
            (0..20)
                .map(|i| format!("{i:02} 中文长行{}\n", "内容".repeat(20)))
                .collect(),
        )];
        let mut layout = TranscriptLayout::default();
        layout.update(&items, Some(0), 24);
        assert_eq!(body(&layout).len(), MAX_TOOL_BODY_ROWS);
        let entry = &layout.entries[0];
        let view = entry.tool_body.as_ref().unwrap();
        assert!(
            view.total > 20,
            "wrapped continuations count toward the limit"
        );
        let point = (3, entry.rows[view.rows.start].start as u16);
        let area = Rect::new(0, 0, 24, layout.height() as u16);
        let initial = body(&layout);
        let height = layout.height();
        assert!(layout.scroll_tool_body(area, 0, point, 3));
        assert_ne!(body(&layout), initial);
        assert_eq!(body(&layout).len(), MAX_TOOL_BODY_ROWS);
        assert_eq!(layout.height(), height);
        let scrolled = body(&layout);
        layout.update(&items, Some(0), 24);
        assert_eq!(body(&layout), scrolled);
        layout.update(&items, None, 24);
        layout.update(&items, Some(0), 24);
        assert_eq!(body(&layout), scrolled);
        layout.update(&items, Some(0), 80);
        assert_eq!(body(&layout).len(), MAX_TOOL_BODY_ROWS);
        assert_eq!(
            layout.entries[0]
                .tool_body
                .as_ref()
                .unwrap()
                .offset,
            3
        );

        let area = Rect::new(0, 0, 80, layout.height() as u16);
        layout.scroll_tool_body(area, 0, point, isize::MAX);
        let at_end = body(&layout);
        assert!(
            layout.scroll_tool_body(area, 0, point, 3),
            "the boundary still consumes wheel events"
        );
        assert_eq!(body(&layout), at_end);
        items[0] = tool("short result".into());
        layout.invalidate_from(0);
        layout.update(&items, Some(0), 80);
        assert!(layout.entries[0].tool_body.is_none());
        assert!(layout.height() < height);
        assert!(!layout.scroll_tool_body(area, 0, point, 3));
    }
}
