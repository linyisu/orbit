//! Cache Unicode-aware physical-line layout, and hand only visible lines to
//! Ratatui. Scrolling never asks a Paragraph to wrap and skip the entire
//! conversation from its beginning.
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use std::{collections::HashSet, ops::Range};
use unicode_linebreak::linebreaks;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{app::TranscriptItem, render};

#[derive(Debug, Clone, Default)]
pub(super) struct Decorations {
    // Terminal column ranges, after wrapping and adding repeated gutters.
    pub ranges: Vec<Range<usize>>,
    pub chrome: bool,
    pub copy: Vec<super::media::CopyText>,
}

#[derive(Debug)]
struct Row {
    line: Line<'static>,
    wrapped: Vec<Line<'static>>,
    ranges: Vec<Range<usize>>,
    links: Vec<super::markdown::LinkTarget>,
    html_disclosure: Option<String>,
    decorations: Vec<Decorations>,
    gutter: Option<Gutter>,
    start: usize,
    height: usize,
    graphics: Vec<super::media::Fragment>,
    media: Vec<super::media::Target>,
    media_requests: Vec<super::media::Request>,
}

#[derive(Debug)]
struct Gutter {
    first: Line<'static>,
    continuation: Line<'static>,
    width: u16,
}

fn list_marker(text: &str) -> bool {
    matches!(
        text.strip_suffix(' '),
        Some("•" | super::icons::CHECKBOX_MARKED_OUTLINE | super::icons::CHECKBOX_BLANK_OUTLINE)
    ) || text
        .strip_suffix(". ")
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

fn is_rail_gutter(content: &str) -> bool {
    if let Some(prefix) = content
        .strip_suffix("┊ │ ")
        .or_else(|| content.strip_suffix("│ "))
        .or_else(|| content.strip_suffix("┊ "))
    {
        prefix.chars().all(|c| c == ' ')
    } else {
        false
    }
}

fn take_gutter(line: &mut Line<'static>) -> Option<Gutter> {
    if line
        .spans
        .first()
        .is_some_and(|span| is_rail_gutter(span.content.as_ref()))
    {
        let count = line
            .spans
            .iter()
            .take_while(|span| is_rail_gutter(&span.content))
            .count();
        let gutter = Line::from(line.spans.drain(..count).collect::<Vec<_>>());
        let width = gutter.width() as u16;
        return Some(Gutter {
            first: gutter.clone(),
            continuation: gutter,
            width,
        });
    }

    let mut quote_end = None;

    for (index, span) in line.spans.iter().enumerate() {
        if span.content == "▎ " {
            quote_end = Some(index);
        } else if !span.content.chars().all(char::is_whitespace) {
            break;
        }
    }

    if let Some(end) = quote_end {
        let prefix = line.spans.drain(..=end).collect::<Vec<_>>();
        let width = prefix.iter().map(Span::width).sum::<usize>() as u16;
        let gutter = Line::from(prefix);
        return Some(Gutter {
            first: gutter.clone(),
            continuation: gutter,
            width,
        });
    }

    let marker = line
        .spans
        .iter()
        .enumerate()
        .find(|(index, span)| {
            list_marker(&span.content)
                && line.spans[..*index]
                    .iter()
                    .all(|prefix| prefix.content.chars().all(char::is_whitespace))
        })
        .map(|(index, _)| index);
    let end = marker.or_else(|| {
        line.spans
            .iter()
            .take_while(|span| {
                !span.content.is_empty() && span.content.chars().all(char::is_whitespace)
            })
            .count()
            .checked_sub(1)
    })?;
    let prefix = line.spans.drain(..=end).collect::<Vec<_>>();
    let width = prefix.iter().map(Span::width).sum::<usize>() as u16;

    Some(Gutter {
        first: Line::from(prefix),
        continuation: Line::from(" ".repeat(width as usize)),
        width,
    })
}

fn take_gutter_before(line: &mut Line<'static>, column: u16) -> Option<Gutter> {
    let mut width = 0;
    let end = line
        .spans
        .iter()
        .take_while(|span| {
            width += span.width();
            width <= usize::from(column)
        })
        .count();
    let mut body = line.spans.split_off(end);
    let gutter = take_gutter(line);
    line.spans.append(&mut body);
    gutter
}

fn slice_line(line: &Line<'static>, range: Range<usize>) -> Line<'static> {
    let mut spans = Vec::new();
    let mut offset = 0;

    for span in &line.spans {
        let start = offset;
        let end = start + span.content.len();
        offset = end;
        let from = range.start.max(start);
        let to = range.end.min(end);

        if from < to {
            spans.push(Span::styled(
                span.content[from - start..to - start].to_owned(),
                span.style,
            ));
        }
    }

    Line {
        style: line.style,
        alignment: line.alignment,
        spans,
    }
}

fn wrap_ranges(text: &str, width: u16) -> Vec<Range<usize>> {
    wrap_ranges_from(text, width, width)
}

/// The same Unicode breaker also lays out prose following an inline picture.
pub(super) fn wrap_ranges_from(text: &str, width: u16, first_width: u16) -> Vec<Range<usize>> {
    if text.is_empty() || width == 0 {
        return std::iter::once_with(|| 0..0).collect();
    }

    let opportunities = linebreaks(text)
        .map(|(index, _)| index)
        .collect::<HashSet<_>>();
    let mut ranges = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let capacity = if ranges.is_empty() {
            first_width
        } else {
            width
        };
        let mut used = 0;
        let mut hard_end = start;
        let mut allowed = None;
        let mut overflowed = false;

        for (relative, grapheme) in text[start..].grapheme_indices(true) {
            let end = start + relative + grapheme.len();
            let size = grapheme.width();

            if used + size > usize::from(capacity) && hard_end > start {
                overflowed = true;
                break;
            }

            used += size;
            hard_end = end;

            if opportunities.contains(&end) {
                allowed = Some(end);
            }
        }

        if !overflowed || hard_end == text.len() {
            ranges.push(start..text.len());
            break;
        }

        let end = allowed.filter(|end| *end > start).unwrap_or(hard_end);
        ranges.push(start..end);
        start = end;
    }

    ranges
}

pub(super) fn wrap_line(
    line: &Line<'static>,
    width: u16,
) -> (Vec<Line<'static>>, Vec<Range<usize>>) {
    let text = line.to_string();
    let ranges = wrap_ranges(&text, width);
    let lines = ranges
        .iter()
        .cloned()
        .map(|range| slice_line(line, range))
        .collect();

    (lines, ranges)
}

pub(super) fn wrap_linked_line(
    mut linked: super::markdown::LinkedLine,
    width: u16,
) -> Vec<super::markdown::LinkedLine> {
    // Mixed media must be wrapped once, after images have their cell sizes.
    // Wrapping here would split formulas and lose their complete target range.
    if !linked.media.is_empty() {
        return vec![linked];
    }
    let original = linked.line.clone();
    let gutter = take_gutter(&mut linked.line).filter(|gutter| gutter.width < width);
    if gutter.is_none() {
        linked.line = original.clone();
    }
    let removed = original.to_string().len() - linked.line.to_string().len();
    let available = width.saturating_sub(gutter.as_ref().map_or(0, |gutter| gutter.width));
    let (lines, ranges) = wrap_line(&linked.line, available);
    lines
        .into_iter()
        .zip(ranges)
        .enumerate()
        .map(|(index, (mut line, range))| {
            let prefix = gutter.as_ref().map(|gutter| {
                if index == 0 {
                    &gutter.first
                } else {
                    &gutter.continuation
                }
            });
            let offset = prefix.map_or(0, |prefix| prefix.to_string().len());
            let links = linked
                .links
                .iter()
                .filter_map(|link| {
                    link.slice(&(range.start + removed..range.end + removed), offset)
                })
                .collect();
            let mut decorations =
                super::markdown::slice_decorations(&linked.decorations, 0..removed, 0);
            decorations.extend(super::markdown::slice_decorations(
                &linked.decorations,
                range.start + removed..range.end + removed,
                offset,
            ));
            if let Some(prefix) = prefix {
                line.spans.splice(0..0, prefix.spans.iter().cloned());
            }
            super::markdown::LinkedLine {
                line,
                links,
                decorations,
                chrome: linked.chrome,
                folds: linked.folds.clone(),
                disclosure: linked.disclosure.clone(),
                media: linked
                    .media
                    .iter()
                    .filter_map(|target| {
                        let slice = range.start + removed..range.end + removed;
                        (target.range.start >= slice.start && target.range.end <= slice.end).then(
                            || super::media::Target {
                                range: target.range.start - slice.start + offset
                                    ..target.range.end - slice.start + offset,
                                source: target.source.clone(),
                            },
                        )
                    })
                    .collect(),
                ..Default::default()
            }
        })
        .collect()
}

fn byte_at_column(text: &str, range: &Range<usize>, column: u16) -> Option<usize> {
    let mut used = 0;

    for (relative, grapheme) in text.get(range.clone())?.grapheme_indices(true) {
        let width = grapheme.width() as u16;

        if column < used + width {
            return Some(range.start + relative);
        }

        used += width;
    }

    None
}

#[derive(Debug, Default)]
struct Entry {
    media: super::media::Context,
    rows: Vec<Row>,
    disclosures: Vec<usize>,
    tool_group: Option<Range<usize>>,
    group_header: Option<usize>,
    height: usize,
    end: usize,
    spacing: bool,
    // Reuse completed lines for append-only deltas. Edits discard the entry.
    stable_rows: usize,
    parsed_bytes: usize,
    logical_index: usize,
    code: super::markdown::Fence,
    inline: super::markdown::InlineState,
    observed_bytes: Option<usize>,
    references: Vec<(String, String, Option<String>)>,
    html_toggles: std::collections::BTreeSet<String>,
}

impl Entry {
    fn clear(&mut self) {
        let toggles = std::mem::take(&mut self.html_toggles);
        let media = self.media.clone();
        *self = Self::default();
        self.html_toggles = toggles;
        self.media = media;
    }
    fn remove_spacing(&mut self) {
        if self.spacing {
            if let Some(row) = self.rows.pop() {
                self.height -= row.height;
            }
            self.spacing = false;
        }
    }

    fn push(&mut self, mut line: Line<'static>, width: u16) {
        render::sanitize_line(&mut line);
        let links = super::markdown::plain_links(&line.to_string());
        self.push_with_links(line, width, links);
    }

    fn push_with_links(
        &mut self,
        line: Line<'static>,
        width: u16,
        links: Vec<super::markdown::LinkTarget>,
    ) {
        self.push_linked(
            super::markdown::LinkedLine {
                line,
                links,
                decorations: Vec::new(),
                chrome: false,
                ..Default::default()
            },
            width,
        );
    }

    fn push_linked(&mut self, mut linked: super::markdown::LinkedLine, width: u16) {
        render::sanitize_linked_line(&mut linked);
        let requests = super::media::requests(&linked, width);
        for line in super::media::layout(linked, &self.media, width) {
            self.push_physical(line, width, requests.clone());
        }
    }

    fn push_physical(
        &mut self,
        mut linked: super::markdown::LinkedLine,
        width: u16,
        requests: Vec<super::media::Request>,
    ) {
        if linked
            .folds
            .iter()
            .any(|fold| !(fold.open ^ self.html_toggles.contains(&fold.id)))
        {
            return;
        }
        if let Some(fold) = &linked.disclosure {
            let open = fold.open ^ self.html_toggles.contains(&fold.id);
            for span in &mut linked.line.spans {
                if span.content == format!("{} ", super::icons::CHEVRON_RIGHT) {
                    span.content = format!(
                        "{} ",
                        if open {
                            super::icons::ARROW_DOWN
                        } else {
                            super::icons::CHEVRON_RIGHT
                        }
                    )
                    .into();
                    break;
                }
            }
        }
        let line = &mut linked.line;
        // Markdown and HTML rules share the body's symmetric margins.
        if super::markdown::expand_rule(line, width) {
            linked.decorations = std::iter::once(0..line.to_string().len()).collect();
            linked.chrome = true;
        }
        render::sanitize_line(line);
        let original = line.to_string();
        let gutter = if let Some(column) = linked.graphics.iter().map(|g| g.x).min() {
            take_gutter_before(line, column)
        } else {
            take_gutter(line)
        };
        let removed = original.len().saturating_sub(line.to_string().len());
        linked.links.retain_mut(|link| {
            if link.end <= removed {
                return false;
            }

            link.start = link.start.saturating_sub(removed);
            link.end = link.end.saturating_sub(removed);

            true
        });
        for target in &mut linked.media {
            target.range.start = target.range.start.saturating_sub(removed);
            target.range.end = target.range.end.saturating_sub(removed);
        }
        let content_width = width.saturating_sub(gutter.as_ref().map_or(0, |gutter| gutter.width));
        let (wrapped, ranges) = wrap_line(line, content_width);
        let decorations = ranges
            .iter()
            .map(|range| {
                let mut excluded = Vec::new();
                let prefix = &original[..removed];
                for slice in super::markdown::slice_decorations(&linked.decorations, 0..removed, 0)
                {
                    if let (Some(start), Some(end)) =
                        (prefix.get(..slice.start), prefix.get(..slice.end))
                    {
                        excluded.push(start.width()..end.width());
                    }
                }
                let prefix_width = gutter
                    .as_ref()
                    .map_or(0, |gutter| usize::from(gutter.width));
                let text = &original[removed + range.start..removed + range.end];
                for slice in super::markdown::slice_decorations(
                    &linked.decorations,
                    removed + range.start..removed + range.end,
                    0,
                ) {
                    if let (Some(start), Some(end)) =
                        (text.get(..slice.start), text.get(..slice.end))
                    {
                        excluded.push(prefix_width + start.width()..prefix_width + end.width());
                    }
                }
                Decorations {
                    ranges: excluded,
                    chrome: linked.chrome,
                    copy: linked.copy.clone(),
                }
            })
            .collect();
        let height = wrapped.len();
        for graphic in &mut linked.graphics {
            graphic.x = graphic
                .x
                .saturating_sub(gutter.as_ref().map_or(0, |g| g.width));
        }
        self.rows.push(Row {
            gutter,
            line: linked.line,
            wrapped,
            ranges,
            links: linked.links,
            html_disclosure: linked.disclosure.map(|fold| fold.id),
            decorations,
            start: self.height,
            height,
            graphics: linked.graphics,
            media: linked.media,
            media_requests: requests,
        });
        self.height += height;
    }

    fn stream(&mut self, text: &str, thinking: bool, width: u16) {
        if self.observed_bytes.is_some_and(|len| text.len() < len)
            || !text.is_char_boundary(self.parsed_bytes)
        {
            self.clear();
        }

        if self.observed_bytes == Some(text.len()) {
            return;
        }

        let context = if text[self.parsed_bytes..].contains('[') {
            let context = super::markdown::Context::new(text);
            let definitions = context.definitions();
            if definitions != self.references {
                self.clear();
                self.references = definitions;
            }
            context
        } else {
            super::markdown::Context::default()
        };

        self.rows.truncate(self.stable_rows);
        self.height = self.rows.last().map_or(0, |row| row.start + row.height);
        let mut code = self.code.clone();
        let mut inline = self.inline.clone();
        let mut index = self.logical_index;
        let mut consumed = self.parsed_bytes;
        let mut can_commit = true;

        while consumed < text.len() {
            if code.is_none()
                && let Some((bytes, lines)) = context.math_block(&text[consumed..])
            {
                for line in lines {
                    self.push_linked(
                        if thinking {
                            render::thinking_linked(line)
                        } else {
                            line
                        },
                        width,
                    );
                }
                index += text[consumed..consumed + bytes].lines().count();
                consumed += bytes;
                // Display math is provisional until its terminating newline is
                // received, just like tables and HTML fragments.
                if can_commit && text[..consumed].ends_with('\n') {
                    self.stable_rows = self.rows.len();
                    self.parsed_bytes = consumed;
                    self.logical_index = index;
                    self.code = code.clone();
                    self.inline = inline.clone();
                }
                continue;
            }
            if code.is_none() && super::markdown::Context::math_candidate(&text[consumed..]) {
                // An opening delimiter can become display math when later
                // chunks arrive. Keep its paragraph provisional meanwhile.
                can_commit = false;
            }
            if code.is_none()
                && let Some(block) = super::markdown::blocks::render(
                    &text[consumed..],
                    if thinking {
                        width.saturating_sub(2)
                    } else {
                        width
                    },
                    &format!("body/{consumed}"),
                    0,
                )
            {
                for line in block.lines {
                    self.push_linked(
                        if thinking {
                            render::thinking_linked(line)
                        } else {
                            line
                        },
                        width,
                    );
                }
                index += text[consumed..consumed + block.bytes].lines().count();
                consumed += block.bytes;
                if can_commit && block.complete && text[..consumed].ends_with('\n') {
                    self.stable_rows = self.rows.len();
                    self.parsed_bytes = consumed;
                    self.logical_index = index;
                    self.code = code.clone();
                    self.inline = inline.clone();
                }
                continue;
            }
            if !thinking
                && code.is_none()
                && let Some((bytes, lines)) = context.table(&text[consumed..], width)
            {
                for line in lines {
                    self.push_linked(line, width);
                }

                index += text[consumed..consumed + bytes].lines().count();
                consumed += bytes;

                // A partial next row (even just "| ") can temporarily end the parser's
                // table. Do not freeze it until a complete terminating line arrives.
                if can_commit
                    && text[consumed..]
                        .split_inclusive('\n')
                        .next()
                        .is_some_and(|line| {
                            line.ends_with('\n') && !line.trim_start().starts_with('|')
                        })
                {
                    self.stable_rows = self.rows.len();
                    self.parsed_bytes = consumed;
                    self.logical_index = index;
                    self.code = code.clone();
                    self.inline = inline.clone();
                }

                continue;
            }

            let part = text[consumed..].split_inclusive('\n').next().unwrap();
            let complete = part.ends_with('\n');
            let line = if complete {
                part.trim_end_matches('\n')
                    .strip_suffix('\r')
                    .unwrap_or(part.trim_end_matches('\n'))
            } else {
                part
            };

            for rendered in context.linked_lines(line, index, &mut code, &mut inline) {
                let rendered = if thinking {
                    render::thinking_linked(rendered)
                } else {
                    rendered
                };
                self.push_linked(rendered, width);
            }
            index += 1;
            consumed += part.len();

            if can_commit && complete && (thinking || code.is_some() || !line.contains('|')) {
                self.stable_rows = self.rows.len();
                self.parsed_bytes = consumed;
                self.logical_index = index;
                self.code = code.clone();
                self.inline = inline.clone();
            }
        }

        self.observed_bytes = Some(text.len());
    }

    fn user(&mut self, text: &str, width: u16) {
        self.framed_input(text, width, super::icons::PROMPT);
    }

    fn framed_input(&mut self, text: &str, width: u16, icon: &str) {
        // A complete frame lets user turns remain visually distinct after their
        // contents wrap. Keep a plain fallback for terminals too narrow to
        // draw the two side rails without stealing all useful text width.
        if width < 9 {
            for (index, line) in text.lines().enumerate() {
                self.push(
                    Line::from(vec![
                        Span::styled(
                            if index == 0 {
                                format!("{icon} ")
                            } else {
                                "  ".into()
                            },
                            ratatui::style::Style::default().fg(crate::tui::theme::THEME.accent),
                        ),
                        Span::styled(
                            line.to_owned(),
                            ratatui::style::Style::default()
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                    ]),
                    width,
                );
            }

            return;
        }

        let border = ratatui::style::Style::default().fg(crate::tui::theme::THEME.accent);
        let top_label = format!("╭─{icon}─");
        let trailing = "─".repeat(usize::from(width) - top_label.width() - 1);
        self.push_linked(
            super::markdown::LinkedLine::chrome(Line::from(vec![
                Span::styled("╭─", border),
                Span::styled(
                    icon.to_owned(),
                    border.add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(format!("─{trailing}╮"), border),
            ])),
            width,
        );
        let inner_width = width - 4;

        for source in text.split('\n') {
            let content = super::markdown::LinkedLine::plain(Line::from(Span::styled(
                source.to_owned(),
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
            )));

            for line in content.wrap(inner_width) {
                let padding = usize::from(inner_width).saturating_sub(line.line.width());
                self.push_linked(
                    line.framed(
                        vec![Span::styled("│ ", border)],
                        vec![Span::raw(" ".repeat(padding)), Span::styled(" │", border)],
                    ),
                    width,
                );
            }
        }

        self.push_linked(
            super::markdown::LinkedLine::chrome(Line::from(Span::styled(
                format!("╰{}╯", "─".repeat(usize::from(width) - 2)),
                border,
            ))),
            width,
        );
    }
}

#[derive(Debug, Default)]
pub(super) struct TranscriptLayout {
    media: super::media::Context,
    media_revision: u64,
    entries: Vec<Entry>,
    width: u16,
    expanded: std::collections::BTreeSet<usize>,
    expanded_groups: std::collections::BTreeSet<usize>,
    dirty_from: usize,
    html_toggles: std::collections::BTreeMap<usize, std::collections::BTreeSet<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TranscriptAnchor {
    entry: usize,
    row: usize,
    byte: usize,
}

impl TranscriptLayout {
    pub(super) fn set_media(&mut self, media: super::media::Context) {
        let revision = media.revision();
        if !self.media.same_cache(&media) || self.media_revision != revision {
            self.entries.clear();
            self.dirty_from = 0;
        }
        self.media = media;
        self.media_revision = revision;
    }

    pub(super) fn media_at(
        &self,
        area: Rect,
        offset: usize,
        point: (u16, u16),
    ) -> Option<super::media::Source> {
        if !self.media.enabled() {
            return None;
        }
        if !area.contains(point.into()) {
            return None;
        }
        let absolute = offset + usize::from(point.1 - area.y);
        let entry = self
            .entries
            .get(self.entries.partition_point(|e| e.end <= absolute))?;
        let relative = absolute - (entry.end - entry.height);
        let row = entry
            .rows
            .iter()
            .find(|r| r.start <= relative && relative < r.start + r.height)?;
        let column = point
            .0
            .saturating_sub(area.x + row.gutter.as_ref().map_or(0, |g| g.width));
        row.graphics
            .iter()
            .find(|f| column >= f.x && column < f.x + f.picture.size.width)
            .map(|f| f.picture.source.clone())
            .or_else(|| {
                let byte = byte_at_column(
                    &row.line.to_string(),
                    row.ranges.get(relative - row.start)?,
                    column,
                )?;
                row.media
                    .iter()
                    .find(|t| t.range.contains(&byte))
                    .map(|t| t.source.clone())
            })
    }
    pub(super) fn html_disclosure_at(
        &self,
        area: Rect,
        offset: usize,
        point: (u16, u16),
    ) -> Option<(usize, String)> {
        if !area.contains(point.into()) {
            return None;
        }
        let absolute = offset + usize::from(point.1 - area.y);
        let index = self.entries.partition_point(|entry| entry.end <= absolute);
        let entry = self.entries.get(index)?;
        let relative = absolute - (entry.end - entry.height);
        let row = entry
            .rows
            .iter()
            .find(|row| row.start <= relative && relative < row.start + row.height)?;
        let id = row.html_disclosure.as_ref()?;
        let width = row.wrapped.get(relative - row.start)?.width()
            + row
                .gutter
                .as_ref()
                .map_or(0, |gutter| gutter.width as usize);
        (usize::from(point.0 - area.x) < width).then(|| (index, id.clone()))
    }

    pub(super) fn toggle_html(&mut self, index: usize, id: String) {
        let toggles = self.html_toggles.entry(index).or_default();
        if !toggles.remove(&id) {
            toggles.insert(id);
        }
        self.reset_rows_from(index);
    }
    pub(super) fn anchor_at(&self, offset: usize) -> Option<TranscriptAnchor> {
        let entry_index = self.entries.partition_point(|entry| entry.end <= offset);
        let entry = self.entries.get(entry_index)?;
        let entry_start = entry.end.saturating_sub(entry.height);
        let relative = offset.saturating_sub(entry_start);
        let row_index = entry
            .rows
            .partition_point(|row| row.start + row.height <= relative)
            .min(entry.rows.len().saturating_sub(1));
        let row = entry.rows.get(row_index)?;
        let wrapped = relative
            .saturating_sub(row.start)
            .min(row.ranges.len().saturating_sub(1));

        Some(TranscriptAnchor {
            entry: entry_index,
            row: row_index,
            byte: row.ranges.get(wrapped)?.start,
        })
    }

    pub(super) fn offset_for_anchor(&self, anchor: TranscriptAnchor) -> Option<usize> {
        let entry = self.entries.get(anchor.entry)?;
        if entry.height == 0
            && let Some(group) = &entry.tool_group
        {
            return self.item_offset(group.start);
        }
        let row = entry.rows.get(anchor.row)?;
        let wrapped = row
            .ranges
            .partition_point(|range| range.start <= anchor.byte)
            .saturating_sub(1)
            .min(row.ranges.len().saturating_sub(1));

        Some(entry.end.saturating_sub(entry.height) + row.start + wrapped)
    }

    pub fn invalidate_from(&mut self, index: usize) {
        self.dirty_from = self.dirty_from.min(index);
    }

    /// Discard cached source offsets when text is edited or entries are removed.
    pub(super) fn reset_from(&mut self, index: usize) {
        self.html_toggles.retain(|item, _| *item < index);
        self.reset_rows_from(index);
    }

    fn reset_rows_from(&mut self, index: usize) {
        let index = self
            .entries
            .get(index)
            .and_then(|entry| entry.tool_group.as_ref())
            .map_or(index, |group| group.start);
        self.entries.truncate(index);
        self.invalidate_from(index);
    }

    #[cfg(test)]
    pub fn update(&mut self, items: &[TranscriptItem], expanded: Option<usize>, width: u16) {
        self.update_expanded(
            items,
            &expanded.into_iter().collect(),
            &Default::default(),
            width,
        );
    }

    pub fn update_expanded(
        &mut self,
        items: &[TranscriptItem],
        expanded: &std::collections::BTreeSet<usize>,
        expanded_groups: &std::collections::BTreeSet<usize>,
        width: u16,
    ) {
        if self.width != width || items.len() < self.entries.len() {
            self.entries.clear();
            self.dirty_from = 0;
            self.width = width;
        }

        if &self.expanded != expanded {
            if let Some(index) = self.expanded.symmetric_difference(expanded).next().copied() {
                self.invalidate_from(index);
            }
            self.expanded = expanded.clone();
        }

        if &self.expanded_groups != expanded_groups {
            if let Some(index) = self
                .expanded_groups
                .symmetric_difference(expanded_groups)
                .next()
                .copied()
            {
                self.invalidate_from(index);
            }
            self.expanded_groups = expanded_groups.clone();
        }

        if width == 0 {
            return;
        }

        let first = self.dirty_from.min(self.entries.len());
        // A changed block can also change the spacing before it.
        let first = if first < items.len() {
            first.saturating_sub(1)
        } else {
            first
        };
        let first = self
            .entries
            .get(first)
            .and_then(|entry| entry.tool_group.as_ref())
            .map_or(first, |group| group.start);
        let first = super::tool_groups::start(items, first);
        self.entries.resize_with(items.len(), Entry::default);
        let mut end = first.checked_sub(1).map_or(0, |i| self.entries[i].end);
        let mut group = first..first;

        for (relative, (entry, item)) in self.entries[first..]
            .iter_mut()
            .zip(&items[first..])
            .enumerate()
        {
            let item_index = first + relative;
            entry.media = self.media.clone();
            entry.html_toggles = self
                .html_toggles
                .get(&item_index)
                .cloned()
                .unwrap_or_default();
            if item_index >= group.end {
                group = super::tool_groups::range(items, item_index);
            }
            entry.remove_spacing();
            let mut next = item_index + 1;
            let mut framed = false;
            let mut block_end = true;
            match item {
                TranscriptItem::Assistant(text) => entry.stream(text, false, width),
                TranscriptItem::Thinking(row) if !row.finished => {
                    entry.stream(row.content.trim_end(), true, width)
                }
                TranscriptItem::Thinking(row) => {
                    entry.clear();
                    let is_expanded = expanded.contains(&(first + relative));
                    let mut lines = render::thinking_card_at_width(row, is_expanded, width);
                    if lines
                        .last()
                        .is_some_and(|line| line.line.to_string().is_empty())
                    {
                        lines.pop();
                    }
                    for line in lines {
                        if !line.line.to_string().trim().is_empty() {
                            entry.disclosures.push(entry.rows.len());
                        }
                        entry.push_linked(line, width);
                    }
                }
                TranscriptItem::User(text) => {
                    entry.clear();
                    entry.user(text, width);
                }
                TranscriptItem::Tool(row) => {
                    entry.clear();
                    let is_expanded = expanded.contains(&item_index);
                    let grouped = group.len() > 1;
                    let group_open = expanded_groups.contains(&group.start);
                    if grouped {
                        next = group.end;
                        block_end = if group_open {
                            item_index + 1 == group.end
                        } else {
                            item_index == group.start
                        };
                    }
                    framed = width >= 10 && if grouped { group_open } else { is_expanded };
                    if grouped {
                        entry.tool_group = Some(group.clone());
                        if item_index == group.start {
                            if framed {
                                entry.push_linked(
                                    render::tool_border_line(width, ('╭', '╮')),
                                    width,
                                );
                            }
                            entry.group_header = Some(entry.rows.len());
                            entry.push_linked(
                                super::tool_groups::header(&items[group.clone()], width, framed),
                                width,
                            );
                        }
                    }
                    if !grouped || group_open {
                        let (lines, header, body) = if grouped {
                            let section = super::tool_groups::section(
                                row,
                                is_expanded,
                                width,
                                item_index == group.start,
                                item_index + 1 == group.end,
                            );
                            (section.lines, Some(section.header), section.body)
                        } else {
                            (
                                render::tool_card_with_links(row, is_expanded, width),
                                None,
                                0..0,
                            )
                        };
                        for (index, line) in lines.into_iter().enumerate() {
                            if header.map_or_else(
                                || !line.line.to_string().is_empty(),
                                |header| header == index || body.contains(&index),
                            ) {
                                entry.disclosures.push(entry.rows.len());
                            }
                            entry.push_linked(line, width);
                        }
                    }
                }
                _ => {
                    entry.clear();
                    let collapsible = matches!(item, TranscriptItem::Thinking(_));
                    let toggle = collapsible.then(|| {
                        super::tool_display::toggle_line(
                            expanded.contains(&(first + relative)),
                            width,
                        )
                    });

                    let mut lines = render::transcript_lines_at_width(
                        std::slice::from_ref(item),
                        expanded.contains(&(first + relative)),
                        width,
                    );
                    if lines.last().is_some_and(|line| line.to_string().is_empty()) {
                        lines.pop();
                    }
                    for line in lines {
                        if (collapsible && entry.rows.is_empty()) || toggle.as_ref() == Some(&line)
                        {
                            entry.disclosures.push(entry.rows.len());
                        }

                        entry.push(line, width);
                    }
                }
            }

            if entry.height > 0 && block_end && !framed {
                let next_grouped = super::tool_groups::range(items, next).len() > 1;
                let next_framed = width >= 10
                    && matches!(items.get(next), Some(TranscriptItem::Tool(_)))
                    && if next_grouped {
                        expanded_groups.contains(&next)
                    } else {
                        expanded.contains(&next)
                    };
                if !next_framed {
                    entry.push(Line::default(), width);
                    entry.spacing = true;
                }
            }
            end += entry.height;
            entry.end = end;
        }

        self.dirty_from = items.len();
    }

    pub fn visible_collapsible(
        &self,
        items: &[TranscriptItem],
        offset: usize,
        height: usize,
    ) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, entry)| {
                (entry.end > offset
                    && entry.end - entry.height < offset + height
                    && !entry.disclosures.is_empty()
                    && (matches!(items.get(index), Some(TranscriptItem::Tool(_)))
                        || matches!(items.get(index), Some(TranscriptItem::Thinking(row)) if row.finished)))
                .then_some(index)
            })
    }

    pub(super) fn item_offset(&self, index: usize) -> Option<usize> {
        self.entries
            .get(index)
            .map(|entry| entry.end - entry.height)
    }

    pub(super) fn tool_offset(&self, index: usize) -> Option<usize> {
        let entry = self.entries.get(index)?;
        let row = entry.rows.get(*entry.disclosures.first()?)?;
        Some(entry.end - entry.height + row.start)
    }

    pub(super) fn group_range(&self, index: usize) -> Option<Range<usize>> {
        self.entries.get(index)?.tool_group.clone()
    }

    pub(super) fn group_offset(&self, index: usize) -> Option<usize> {
        let entry = self.entries.get(index)?;
        let row = entry.rows.get(entry.group_header?)?;
        Some(entry.end - entry.height + row.start)
    }

    pub(super) fn visible_group(&self, offset: usize, height: usize) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, entry)| {
                (entry.group_header.is_some()
                    && entry.end > offset
                    && entry.end - entry.height < offset + height)
                    .then_some(index)
            })
    }

    pub(super) fn group_at(&self, area: Rect, offset: usize, point: (u16, u16)) -> Option<usize> {
        if !area.contains(point.into()) {
            return None;
        }
        let absolute = offset + usize::from(point.1 - area.y);
        let index = self.entries.partition_point(|entry| entry.end <= absolute);
        let entry = self.entries.get(index)?;
        let row = entry.rows.get(entry.group_header?)?;
        let relative = absolute - (entry.end - entry.height);
        let column = usize::from(point.0 - area.x);
        let gutter = row
            .gutter
            .as_ref()
            .map_or(0, |gutter| usize::from(gutter.width));
        (row.start <= relative
            && relative < row.start + row.height
            && column < gutter + row.line.width()
            && !row.decorations[relative - row.start]
                .ranges
                .iter()
                .any(|range| range.contains(&column)))
        .then_some(index)
    }

    pub(super) fn disclosure_at(
        &self,
        items: &[TranscriptItem],
        area: Rect,
        offset: usize,
        point: (u16, u16),
    ) -> Option<usize> {
        if !area.contains(point.into()) {
            return None;
        }

        let absolute = offset + usize::from(point.1 - area.y);
        let index = self.entries.partition_point(|entry| entry.end <= absolute);
        let entry = self.entries.get(index)?;

        if !matches!(
            items.get(index),
            Some(TranscriptItem::Tool(_))
                | Some(TranscriptItem::Thinking(super::app::ThinkingRow {
                    finished: true,
                    ..
                }))
        ) {
            return None;
        }

        let relative = absolute - (entry.end - entry.height);

        let row = entry
            .disclosures
            .iter()
            .filter_map(|i| entry.rows.get(*i))
            .find(|row| row.start <= relative && relative < row.start + row.height)?;
        let column = usize::from(point.0 - area.x);
        let gutter_width = row
            .gutter
            .as_ref()
            .map_or(0, |gutter| gutter.width as usize);

        (column < gutter_width + row.line.width()).then_some(index)
    }

    pub fn height(&self) -> usize {
        self.entries.last().map_or(0, |entry| entry.end)
    }

    pub(super) fn decorations(&self, absolute_row: usize) -> &[Range<usize>] {
        self.selection_metadata(absolute_row)
            .map_or(&[], |metadata| metadata.ranges.as_slice())
    }

    pub(super) fn selection_metadata(&self, absolute_row: usize) -> Option<&Decorations> {
        let entry = self.entries.get(
            self.entries
                .partition_point(|entry| entry.end <= absolute_row),
        )?;
        let relative = absolute_row - (entry.end - entry.height);
        let row = entry.rows.get(
            entry
                .rows
                .partition_point(|row| row.start + row.height <= relative),
        )?;
        row.decorations.get(relative - row.start)
    }

    pub(super) fn link_at(&self, area: Rect, offset: usize, point: (u16, u16)) -> Option<&str> {
        if !area.contains(point.into()) {
            return None;
        }

        let absolute_row = offset + (point.1 - area.y) as usize;
        let entry = self.entries.iter().find(|entry| entry.end > absolute_row)?;
        let entry_start = entry.end - entry.height;
        let row = entry.rows.iter().find(|row| {
            let start = entry_start + row.start;
            absolute_row >= start && absolute_row < start + row.height
        })?;
        let gutter_width = row.gutter.as_ref().map_or(0, |gutter| gutter.width);
        let column = point.0.checked_sub(area.x + gutter_width)?;
        let wrapped = absolute_row - entry_start - row.start;
        let range = row.ranges.get(wrapped)?;
        let byte = byte_at_column(&row.line.to_string(), range, column)?;

        row.links
            .iter()
            .find(|link| link.start <= byte && byte < link.end)
            .map(|link| link.url.as_str())
    }

    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect, offset: usize) {
        self.draw_buffer(frame.buffer_mut(), area, offset);
        let first = self.entries.partition_point(|entry| entry.end <= offset);
        for entry in self.entries[first..]
            .iter()
            .take_while(|entry| entry.end - entry.height < offset + usize::from(area.height))
        {
            let start = entry.end - entry.height;
            for row in &entry.rows {
                let absolute = start + row.start;
                if absolute + row.height <= offset || absolute >= offset + usize::from(area.height)
                {
                    continue;
                }
                for request in &row.media_requests {
                    self.media
                        .picture(&request.source, request.width, request.height, false);
                }
                if absolute < offset {
                    continue;
                }
                let gutter = row.gutter.as_ref().map_or(0, |g| g.width);
                for fragment in &row.graphics {
                    let x = area.x + gutter + fragment.x;
                    let image_area = Rect::new(
                        x,
                        area.y + (absolute - offset) as u16,
                        fragment
                            .picture
                            .size
                            .width
                            .min(area.right().saturating_sub(x)),
                        1,
                    );
                    super::media::schedule(fragment, image_area);
                }
            }
        }
    }

    pub(super) fn draw_buffer(
        &self,
        buffer: &mut ratatui::buffer::Buffer,
        area: Rect,
        offset: usize,
    ) {
        let mut y = area.y;
        let first = self.entries.partition_point(|entry| entry.end <= offset);

        for entry in &self.entries[first..] {
            let start = entry.end - entry.height;
            let relative = offset.saturating_sub(start);
            let first_row = entry
                .rows
                .partition_point(|row| row.start + row.height <= relative);

            for row in &entry.rows[first_row..] {
                if y >= area.bottom() {
                    return;
                }

                let skip = relative.saturating_sub(row.start);
                let height = (row.height - skip).min((area.bottom() - y) as usize) as u16;
                let gutter_width = row.gutter.as_ref().map_or(0, |gutter| gutter.width);

                if let Some(gutter) = &row.gutter {
                    for dy in 0..height {
                        let first = skip == 0 && dy == 0;
                        Paragraph::new(
                            if first {
                                gutter.first.clone()
                            } else {
                                gutter.continuation.clone()
                            }
                            .style(row.line.style),
                        )
                        .render(Rect::new(area.x, y + dy, gutter_width, 1), buffer);
                    }
                }

                for (dy, line) in row
                    .wrapped
                    .iter()
                    .skip(skip)
                    .take(height as usize)
                    .enumerate()
                {
                    Paragraph::new(line.clone()).render(
                        Rect::new(
                            area.x + gutter_width,
                            y + dy as u16,
                            area.width.saturating_sub(gutter_width),
                            1,
                        ),
                        buffer,
                    );
                }

                y += height;
            }
        }
    }
}

#[cfg(test)]
mod tests;
