//! Render complete source lines incrementally; never reparse the conversation on scroll.
//! CommonMark inline syntax is handled by pulldown-cmark, not regex replacements.
use pulldown_cmark::{Event, Options, Parser, Tag};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::{
    code::Renderer,
    icons,
    languages::{is_markdown_label, label_key},
    theme::code_label_color,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CodeFence {
    indent: usize,
    delimiter: char,
    length: usize,
    markdown_source: bool,
    nested_fences: Vec<(char, usize)>,
    renderer: Renderer,
    color: Color,
}

pub(super) type Fence = Option<CodeFence>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LinkTarget {
    pub start: usize,
    pub end: usize,
    pub url: String,
}

pub(super) struct LinkedLine {
    pub line: Line<'static>,
    pub links: Vec<LinkTarget>,
}

impl From<LinkedLine> for Line<'static> {
    fn from(linked: LinkedLine) -> Self {
        linked.line
    }
}

impl LinkTarget {
    pub fn slice(&self, range: &std::ops::Range<usize>, offset: usize) -> Option<Self> {
        let start = self.start.max(range.start);
        let end = self.end.min(range.end);
        (start < end).then(|| Self {
            start: start - range.start + offset,
            end: end - range.start + offset,
            url: self.url.clone(),
        })
    }
}

impl LinkedLine {
    pub fn plain(mut line: Line<'static>) -> Self {
        super::render::sanitize_line(&mut line);
        let links = plain_links(&line.to_string());
        Self { line, links }
    }

    pub fn shift_offsets(&mut self, amount: isize) {
        self.links.retain_mut(|link| {
            link.start = link.start.saturating_add_signed(amount);
            link.end = link.end.saturating_add_signed(amount);
            link.start < link.end
        });
    }

    pub fn wrap(self, width: u16) -> Vec<Self> {
        super::transcript::wrap_linked_line(self, width)
    }
}

pub(super) fn document(source: &str, width: u16) -> Vec<LinkedLine> {
    let mut out = Vec::new();
    let mut remaining = source;
    let mut fence = None;
    let mut index = 0;
    while !remaining.is_empty() {
        if fence.is_none()
            && let Some((bytes, lines)) = table(remaining, width)
        {
            out.extend(lines);
            index += remaining[..bytes].lines().count();
            remaining = &remaining[bytes..];
            continue;
        }

        let part = remaining.split_inclusive('\n').next().unwrap();
        let source = part.trim_end_matches(['\r', '\n']);
        let was_code = fence.is_some();
        let mut line = line(source, index, &mut fence);
        super::render::sanitize_line(&mut line);
        let links = if was_code || fence.is_some() {
            Vec::new()
        } else {
            links(source, &line)
        };
        out.push(LinkedLine { line, links });
        remaining = &remaining[part.len()..];
        index += 1;
    }
    out
}

pub(super) fn expand_rule(line: &mut Line<'static>, width: u16) {
    let mut visible = line
        .spans
        .iter()
        .filter(|span| !span.content.trim().is_empty());
    if let Some(rule) = visible.next()
        && visible.next().is_none()
        && rule.style.fg == Some(THEME.md_rule)
        && rule.content.chars().all(|ch| ch == '─')
    {
        *line = Line::from(Span::styled("─".repeat(usize::from(width)), rule.style));
    }
}

use super::theme::THEME;

const SECONDARY: Color = THEME.secondary;

pub(super) fn mark_style(style: Style) -> Style {
    style.fg(THEME.md_mark).add_modifier(Modifier::UNDERLINED)
}

/// Byte ranges and normalized targets for visible, bare web addresses.
/// Deliberately accept only HTTP(S) and `www.` forms; paths, e-mail addresses
/// and prose such as `thing.example` should not unexpectedly become actions.
fn bare_urls(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut found: Vec<(std::ops::Range<usize>, String)> = Vec::new();

    for (start, _) in text.match_indices("http").chain(text.match_indices("www.")) {
        if start > 0
            && text[..start]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        {
            continue;
        }

        let remainder = &text[start..];
        let prefixed = remainder.starts_with("www.");

        if !remainder.starts_with("http://") && !remainder.starts_with("https://") && !prefixed {
            continue;
        }

        let mut end = start
            + remainder
                .find(|ch: char| {
                    ch.is_whitespace()
                        || matches!(
                            ch,
                            '<' | '>' | '"' | '\'' | '，' | '。' | '！' | '？' | '；' | '：'
                        )
                })
                .unwrap_or(remainder.len());

        while text[..end].chars().next_back().is_some_and(|ch| {
            matches!(
                ch,
                '.' | ',' | ';' | ':' | '!' | '?' | '，' | '。' | '！' | '？' | '；' | '：'
            )
        }) {
            end -= text[..end].chars().next_back().unwrap().len_utf8();
        }

        while text[..end].ends_with(')')
            && text[start..end].chars().filter(|&ch| ch == ')').count()
                > text[start..end].chars().filter(|&ch| ch == '(').count()
        {
            end -= 1;
        }

        let visible = &text[start..end];
        let target = if prefixed {
            format!("https://{visible}")
        } else {
            visible.to_owned()
        };

        if url::Url::parse(&target).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        }) && !found
            .iter()
            .any(|(range, _)| range.start <= start && start < range.end)
        {
            found.push((start..end, target));
        }
    }

    found.sort_by_key(|(range, _)| range.start);
    found.dedup_by(|left, right| left.0 == right.0);

    found
}

pub(super) fn plain_links(text: &str) -> Vec<LinkTarget> {
    bare_urls(text)
        .into_iter()
        .map(|(range, url)| LinkTarget {
            start: range.start,
            end: range.end,
            url,
        })
        .collect()
}

fn push_marked_text(spans: &mut Vec<Span<'static>>, text: &str, style: Style, recover: bool) {
    if !recover || !text.contains("==") {
        spans.push(Span::styled(text.to_owned(), style));
        return;
    }

    let mut rest = text;

    while let Some(open) = rest.find("==") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("==") else {
            break;
        };

        let content = &after_open[..close];

        if content.is_empty()
            || content.starts_with(char::is_whitespace)
            || content.ends_with(char::is_whitespace)
        {
            let split = open + 2;
            spans.push(Span::styled(rest[..split].to_owned(), style));
            rest = &rest[split..];
            continue;
        }

        if open > 0 {
            spans.push(Span::styled(rest[..open].to_owned(), style));
        }

        spans.push(Span::styled(content.to_owned(), mark_style(style)));
        rest = &after_open[close + 2..];
    }

    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_owned(), style));
    }
}

fn push_text_with_bare_links(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    style: Style,
    recover_mark: bool,
) {
    let mut cursor = 0;

    for (range, _) in bare_urls(text) {
        if range.start < cursor {
            continue;
        }

        push_marked_text(spans, &text[cursor..range.start], style, recover_mark);
        spans.push(Span::styled(
            text[range.clone()].to_owned(),
            style
                .fg(crate::tui::theme::THEME.md_link)
                .add_modifier(Modifier::UNDERLINED),
        ));
        cursor = range.end;
    }

    push_marked_text(spans, &text[cursor..], style, recover_mark);
}

fn push_text_with_cjk_strong(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    style: Style,
    recover: bool,
    recover_mark: bool,
) {
    if !recover || !text.contains("**") {
        push_text_with_bare_links(spans, text, style, recover_mark);
        return;
    }

    let mut rest = text;

    while let Some(open) = rest.find("**") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("**") else {
            break;
        };

        let content = &after_open[..close];

        if content.is_empty()
            || content.starts_with(char::is_whitespace)
            || content.ends_with(char::is_whitespace)
        {
            let split = open + 2;
            push_text_with_bare_links(spans, &rest[..split], style, recover_mark);
            rest = &rest[split..];
            continue;
        }

        if open > 0 {
            push_text_with_bare_links(spans, &rest[..open], style, recover_mark);
        }

        let strong_fg = if style.add_modifier.contains(Modifier::ITALIC) {
            THEME.md_bold_italic
        } else {
            THEME.md_bold
        };
        push_text_with_bare_links(
            spans,
            content,
            style.fg(strong_fg).add_modifier(Modifier::BOLD),
            recover_mark,
        );
        rest = &after_open[close + 2..];
    }

    if !rest.is_empty() {
        push_text_with_bare_links(spans, rest, style, recover_mark);
    }
}

fn recover_split_strong(spans: &mut Vec<Span<'static>>, start: usize, recover: bool) {
    if !recover {
        return;
    }

    let mut open = start;

    while open + 1 < spans.len() {
        if spans[open].content != "*" || spans[open + 1].content != "*" {
            open += 1;
            continue;
        }
        let Some(close) = (open + 2..spans.len().saturating_sub(1))
            .find(|&index| spans[index].content == "*" && spans[index + 1].content == "*")
        else {
            break;
        };

        let first = spans[open + 2..close]
            .iter()
            .find_map(|span| span.content.chars().next());
        let last = spans[open + 2..close]
            .iter()
            .rev()
            .find_map(|span| span.content.chars().next_back());

        if first.is_none_or(char::is_whitespace) || last.is_none_or(char::is_whitespace) {
            open += 2;
            continue;
        }

        spans.drain(close..close + 2);
        spans.drain(open..open + 2);

        for span in &mut spans[open..close - 2] {
            let strong_fg = if span.style.add_modifier.contains(Modifier::ITALIC) {
                THEME.md_bold_italic
            } else {
                THEME.md_bold
            };
            span.style = span.style.fg(strong_fg).add_modifier(Modifier::BOLD);
        }

        open = close - 2;
    }
}

/// Map visible Markdown labels and destinations to byte ranges before wrapping.
pub(super) fn links(source: &str, rendered: &Line<'_>) -> Vec<LinkTarget> {
    let mut active: Option<(String, String)> = None;
    let mut parsed = Vec::new();

    for event in Parser::new_ext(
        source,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ) {
        match event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                active = Some((dest_url.into_string(), String::new()));
            }
            Event::Text(text) | Event::Code(text) if active.is_some() => {
                active.as_mut().unwrap().1.push_str(&text);
            }
            Event::Text(text) => {
                parsed.extend(
                    bare_urls(&text)
                        .into_iter()
                        .map(|(range, url)| (text[range].to_owned(), url)),
                );
            }
            Event::End(pulldown_cmark::TagEnd::Link | pulldown_cmark::TagEnd::Image) => {
                if let Some((url, label)) = active.take()
                    && !label.is_empty()
                {
                    parsed.push((label, url));
                }
            }
            _ => {}
        }
    }

    let text = rendered.to_string();
    let mut cursor = 0;

    let mut styled_ranges = Vec::new();
    let mut offset = 0;
    for span in &rendered.spans {
        let end = offset + span.content.len();
        if span.style.add_modifier.contains(Modifier::UNDERLINED) {
            styled_ranges.push(offset..end);
        }
        offset = end;
    }
    let mut targets: Vec<_> = parsed
        .into_iter()
        .filter_map(|(label, url)| {
            let start = text[cursor..]
                .match_indices(&label)
                .map(|(start, _)| start + cursor)
                .find(|start| styled_ranges.iter().any(|range| range.contains(start)))?;
            let end = start + label.len();
            cursor = end;
            Some(LinkTarget { start, end, url })
        })
        .collect();
    // Destinations printed beside Markdown labels are actionable too.
    offset = 0;
    for span in &rendered.spans {
        if span.style.fg == Some(SECONDARY) && span.content.starts_with(" (") {
            targets.extend(plain_links(&span.content).into_iter().map(|mut target| {
                target.start += offset;
                target.end += offset;
                target
            }));
        }
        offset += span.content.len();
    }
    targets
}

/// Parse only a possible table at the current source position. The caller keeps
/// an open table provisional so new rows can adjust column widths during streaming.
pub(super) fn table(source: &str, width: u16) -> Option<(usize, Vec<LinkedLine>)> {
    if !source.lines().next()?.contains('|') {
        return None;
    }

    let mut events = Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter();
    let (Event::Start(Tag::Table(alignments)), _) = events.next()? else {
        return None;
    };

    let mut rows: Vec<Vec<LinkedLine>> = Vec::new();
    let mut end = 0;

    for (event, range) in events {
        match event {
            Event::Start(Tag::TableHead | Tag::TableRow) => rows.push(Vec::new()),
            Event::Start(Tag::TableCell) => {
                let cell_source = source[range].trim().replace(r"\|", "|");
                let mut cell = line(&cell_source, 1, &mut None);
                cell.spans.remove(0);
                super::render::sanitize_line(&mut cell);
                let links = links(&cell_source, &cell);
                rows.last_mut()?.push(LinkedLine { line: cell, links });
            }
            Event::End(pulldown_cmark::TagEnd::Table) => {
                end = range.end;
                break;
            }
            _ => {}
        }
    }

    if end == 0 || rows.is_empty() {
        return None;
    }

    let cols = alignments.len();
    let available = usize::from(width).saturating_sub(3 * cols + 3);

    // On narrow terminals use labelled cells rather than losing columns/content.
    if available < cols * 3 {
        return Some((end, labelled_table_rows(&rows)));
    }

    let mut widths: Vec<usize> = (0..cols)
        .map(|i| {
            rows.iter()
                .filter_map(|r| r.get(i))
                .map(|cell| cell.line.width())
                .max()
                .unwrap_or(1)
                .max(3)
        })
        .collect();

    while widths.iter().sum::<usize>() > available {
        let Some(i) = (0..cols).max_by_key(|&i| widths[i]) else {
            break;
        };

        widths[i] -= 1;
    }

    let border = |left: &str, join: &str, right: &str| LinkedLine {
        line: Line::styled(
            format!(
                "  {left}{}{right}",
                widths
                    .iter()
                    .map(|w| "─".repeat(w + 2))
                    .collect::<Vec<_>>()
                    .join(join)
            ),
            Style::default().fg(THEME.md_table_border),
        ),
        links: Vec::new(),
    };
    let mut lines = vec![border("┌", "┬", "┐")];

    for (r, row) in rows.iter().enumerate() {
        let cells: Vec<_> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| wrap_cell(cell, *w))
            .collect();

        for n in 0..cells.iter().map(Vec::len).max().unwrap_or(1) {
            let mut spans = vec![Span::styled(
                "  │",
                Style::default().fg(THEME.md_table_border),
            )];
            let mut links = Vec::new();
            let mut byte_offset = spans[0].content.len();

            for i in 0..cols {
                let cell = cells.get(i).and_then(|c| c.get(n));
                let padding = widths[i].saturating_sub(cell.map_or(0, |cell| cell.line.width()));
                let left = match alignments[i] {
                    pulldown_cmark::Alignment::Right => padding,
                    pulldown_cmark::Alignment::Center => padding / 2,
                    _ => 0,
                };

                spans.push(Span::raw(" ".repeat(left + 1)));
                byte_offset += left + 1;
                if let Some(cell) = cell {
                    links.extend(cell.links.iter().cloned().map(|mut link| {
                        link.start += byte_offset;
                        link.end += byte_offset;
                        link
                    }));
                    byte_offset += cell.line.to_string().len();
                    spans.extend(cell.line.spans.iter().cloned().map(|mut span| {
                        if r == 0 {
                            span.style = span
                                .style
                                .add_modifier(Modifier::BOLD)
                                .fg(THEME.md_table_header);
                        }

                        span
                    }));
                }
                spans.push(Span::raw(" ".repeat(padding - left + 1)));
                spans.push(Span::styled(
                    "│",
                    Style::default().fg(THEME.md_table_border),
                ));
                byte_offset += padding - left + 1 + "│".len();
            }

            lines.push(LinkedLine {
                line: Line::from(spans),
                links,
            });
        }

        if r + 1 < rows.len() {
            let mut separator = border("├", "┼", "┤");

            if r > 0 {
                separator.line.style = Style::default().fg(THEME.md_table_separator);
            }

            lines.push(separator);
        }
    }

    lines.push(border("└", "┴", "┘"));

    Some((end, lines))
}

fn labelled_table_rows(rows: &[Vec<LinkedLine>]) -> Vec<LinkedLine> {
    let mut lines = Vec::new();

    for row in rows.iter().skip(1) {
        for (i, cell) in row.iter().enumerate() {
            let heading = rows[0]
                .get(i)
                .map(|cell| cell.line.to_string())
                .unwrap_or_else(|| format!("第 {} 列", i + 1));
            let mut spans = vec![Span::styled(
                format!("  {heading}: "),
                Style::default().fg(THEME.md_table_header),
            )];
            let offset = spans[0].content.len();
            let mut links: Vec<_> = cell
                .links
                .iter()
                .cloned()
                .map(|mut link| {
                    link.start += offset;
                    link.end += offset;
                    link
                })
                .collect();
            if let Some(header) = rows[0].get(i) {
                links.extend(header.links.iter().cloned().map(|mut link| {
                    link.start += 2;
                    link.end += 2;
                    link
                }));
            }
            spans.extend(cell.line.spans.clone());
            lines.push(LinkedLine {
                line: Line::from(spans),
                links,
            });
        }

        lines.push(LinkedLine {
            line: Line::from(""),
            links: Vec::new(),
        });
    }

    if lines.is_empty() {
        lines.extend(rows[0].iter().map(|cell| LinkedLine {
            line: cell.line.clone(),
            links: cell.links.clone(),
        }));
    }

    lines
}

fn wrap_cell(cell: &LinkedLine, width: usize) -> Vec<LinkedLine> {
    let mut lines = vec![Line::default()];
    let mut used = 0;

    for span in &cell.line.spans {
        for grapheme in span.content.graphemes(true) {
            let size = grapheme.width();

            if used + size > width && used > 0 {
                lines.push(Line::default());
                used = 0;
            }

            lines
                .last_mut()
                .unwrap()
                .spans
                .push(Span::styled(grapheme.to_owned(), span.style));
            used += size;
        }
    }

    let mut offset = 0;
    lines
        .into_iter()
        .map(|line| {
            let end = offset + line.to_string().len();
            let links = cell
                .links
                .iter()
                .filter_map(|link| link.slice(&(offset..end), 0))
                .collect();
            offset = end;
            LinkedLine { line, links }
        })
        .collect()
}

fn is_fence_opening(marker: char, count: usize, trimmed: &str) -> bool {
    matches!(marker, '`' | '~')
        && count >= 3
        && !(marker == '`' && trimmed[count..].contains('`'))
        && !trimmed[count..].trim().is_empty()
}

pub(super) fn line(source: &str, _index: usize, fence: &mut Fence) -> Line<'static> {
    let trimmed = source.trim_start();
    let marker = trimmed.chars().next().unwrap_or(' ');
    let count = trimmed.chars().take_while(|c| *c == marker).count();

    if let Some(code) = fence {
        // Models frequently show a Markdown document inside a Markdown fence,
        // using ordinary triple fences rather than a longer outer delimiter.
        // Preserve those nested fences as source so their closing marker cannot
        // accidentally terminate the surrounding example.
        let mut closed_nested_fence = false;

        if code.markdown_source {
            if let Some(&(delimiter, length)) = code.nested_fences.last() {
                if marker == delimiter && count >= length && trimmed[count..].trim().is_empty() {
                    code.nested_fences.pop();
                    closed_nested_fence = true;
                }
            } else if is_fence_opening(marker, count, trimmed) {
                code.nested_fences.push((marker, count));
            }
        }

        if !closed_nested_fence
            && marker == code.delimiter
            && count >= code.length
            && trimmed[count..].trim().is_empty()
            && code.nested_fences.is_empty()
        {
            let indent = " ".repeat(code.indent);
            let color = code.color;
            *fence = None;
            return Line::styled(format!("  {indent}╰─"), Style::default().fg(color));
        }

        let indent = " ".repeat(code.indent);
        let code_line = source.strip_prefix(&indent).unwrap_or(source);
        return code
            .renderer
            .render(
                code_line,
                super::code::Options {
                    gutter: super::code::Gutter::Rail {
                        indent: 2 + code.indent,
                        color: code.color,
                    },
                    ..Default::default()
                },
            )
            .remove(0);
    }
    if matches!(marker, '`' | '~')
        && count >= 3
        && !(marker == '`' && trimmed[count..].contains('`'))
    {
        let indent = source.len() - trimmed.len();
        let label = trimmed[count..].trim();
        let color = code_label_color(&label_key(label));
        *fence = Some(CodeFence {
            indent,
            delimiter: marker,
            length: count,
            markdown_source: is_markdown_label(label),
            nested_fences: Vec::new(),
            renderer: Renderer::new(label),
            color,
        });
        let indent_str = " ".repeat(indent);
        let label = if label.is_empty() { "text" } else { label };
        return Line::from(vec![
            Span::styled(format!("  {indent_str}╭─"), Style::default().fg(color)),
            Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(THEME.on_accent)
                    .bg(color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
    }

    let mut spans = vec![Span::raw("  ")];
    let indent = source.len() - trimmed.len();

    if indent > 0 {
        spans.push(Span::raw(source[..indent].to_owned()));
    }

    let beginning = spans.len();
    let mut style = Style::default();
    let mut stack = Vec::new();
    let mut ordered = None;
    let mut links: Vec<(usize, String)> = Vec::new();
    let recover_cjk_strong = !trimmed.contains(r"\**") && !trimmed.contains(r"\*\*");
    let recover_mark = !trimmed.contains(r"\==");

    for event in Parser::new_ext(
        trimmed,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ) {
        match event {
            Event::Start(tag) => {
                stack.push(style);

                match tag {
                    Tag::Heading { level, .. } => {
                        let color = match level {
                            pulldown_cmark::HeadingLevel::H1 => THEME.md_h1,
                            pulldown_cmark::HeadingLevel::H2 => THEME.md_h2,
                            pulldown_cmark::HeadingLevel::H3 => THEME.md_h3,
                            _ => THEME.md_h4_plus,
                        };
                        style = style.fg(color).add_modifier(Modifier::BOLD);
                    }
                    Tag::Strong => {
                        let fg = if style.add_modifier.contains(Modifier::ITALIC) {
                            THEME.md_bold_italic
                        } else {
                            THEME.md_bold
                        };
                        style = style.fg(fg).add_modifier(Modifier::BOLD);
                    }
                    Tag::Emphasis => {
                        let fg = if style.add_modifier.contains(Modifier::BOLD) {
                            THEME.md_bold_italic
                        } else {
                            THEME.md_italic
                        };
                        style = style.fg(fg).add_modifier(Modifier::ITALIC);
                    }
                    Tag::Strikethrough => {
                        style = style
                            .fg(THEME.md_strikethrough)
                            .add_modifier(Modifier::CROSSED_OUT);
                    }
                    Tag::BlockQuote(_) => {
                        spans.push(Span::styled("▎ ", Style::default().fg(THEME.md_quote_rail)));
                        style = style.fg(THEME.md_quote_text);
                    }
                    Tag::List(start) => ordered = start,
                    Tag::Item => spans.push(Span::styled(
                        ordered.map_or("• ".into(), |n| format!("{n}. ")),
                        Style::default().fg(if ordered.is_some() {
                            THEME.md_ordered_bullet
                        } else {
                            THEME.md_unordered_bullet
                        }),
                    )),
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                        links.push((spans.len(), dest_url.into_string()));
                        style = style.fg(THEME.md_link).add_modifier(Modifier::UNDERLINED);
                    }
                    _ => {}
                }
            }

            Event::End(tag) => {
                style = stack.pop().unwrap_or_default();

                if matches!(
                    tag,
                    pulldown_cmark::TagEnd::Link | pulldown_cmark::TagEnd::Image
                ) && let Some((start, url)) = links.pop()
                {
                    let label: String = spans[start..].iter().map(|s| s.content.as_ref()).collect();

                    if !url.is_empty() && label != url {
                        spans.push(Span::styled(
                            format!(" ({url})"),
                            Style::default().fg(SECONDARY),
                        ));
                    }
                }
            }

            Event::Text(text) => {
                push_text_with_cjk_strong(
                    &mut spans,
                    &text,
                    style,
                    recover_cjk_strong,
                    recover_mark,
                );
            }

            Event::Html(text) | Event::InlineHtml(text) => {
                spans.push(Span::styled(text.into_string(), style))
            }
            Event::Code(text) => spans.push(Span::styled(
                text.into_string(),
                style.fg(THEME.md_inline_code).add_modifier(Modifier::BOLD),
            )),
            Event::Rule => spans.push(Span::styled(
                "────────────────────────",
                Style::default().fg(THEME.md_rule),
            )),

            Event::TaskListMarker(done) => {
                let checkbox = format!("{} ", icons::checkbox(done));

                if let Some(marker) = spans.iter_mut().rev().find(|span| {
                    span.content == "• "
                        || span.content.strip_suffix(". ").is_some_and(|number| {
                            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                        })
                }) {
                    marker.content = checkbox.into();
                    marker.style = Style::default().fg(if done {
                        THEME.md_task_done
                    } else {
                        THEME.secondary
                    });
                } else {
                    spans.push(Span::styled(
                        checkbox,
                        Style::default().fg(if done {
                            THEME.md_task_done
                        } else {
                            THEME.secondary
                        }),
                    ));
                }
            }

            Event::SoftBreak | Event::HardBreak => {}
            _ => {}
        }
    }

    recover_split_strong(&mut spans, beginning, recover_cjk_strong);

    // Reference definitions and incomplete constructs must never disappear.
    if spans.len() == beginning && !trimmed.is_empty() {
        spans.push(Span::raw(trimmed.to_owned()));
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::languages::syntax_for_label;

    #[test]
    fn indented_code_preserves_unicode_and_shorter_indentation() {
        let mut fence = None;
        line("    ```rust", 0, &mut fence);
        assert_eq!(line("中文", 1, &mut fence).to_string(), "      │ 中文");
        assert_eq!(
            line("  orbit", 2, &mut fence).to_string(),
            "      │   orbit"
        );
        assert_eq!(
            line("        orbit", 3, &mut fence).to_string(),
            "      │     orbit"
        );
    }

    #[test]
    fn resolves_real_model_labels_aliases_attributes_and_filename_extensions() {
        for label in [
            "csharp",
            "C#",
            "cs",
            "C Sharp",
            "{.csharp}",
            "language-csharp",
            "Program.cs",
            "csharp title=demo",
        ] {
            assert_eq!(syntax_for_label(label).unwrap().name, "C#", "{label}");
        }

        for label in [
            "vb",
            "VB.NET",
            "vbnet",
            "Visual Basic",
            "Visual Basic .NET",
            "Module.vb",
        ] {
            assert_eq!(
                syntax_for_label(label).unwrap().name,
                "Visual Basic .NET",
                "{label}"
            );
        }

        for label in ["typescript", "ts", "tsx", "toml", "Dockerfile", "python3"] {
            assert!(syntax_for_label(label).is_some(), "{label}");
        }

        assert!(syntax_for_label(".NET").is_none());
        assert!(syntax_for_label("invented-language").is_none());

        for (label, sample) in [
            (
                "csharp",
                "public static int Fact(int n) { return 42; } // note",
            ),
            ("vb", "Public Function Fact(n As Integer) As Long ' note"),
        ] {
            let mut fence = None;
            line(&format!("```{label}"), 0, &mut fence);
            let rendered = line(sample, 1, &mut fence);
            let mut colors = Vec::new();

            for span in rendered.spans.iter().skip(1) {
                if !colors.contains(&span.style.fg) {
                    colors.push(span.style.fg);
                }
            }

            assert!(colors.len() >= 3, "{label}: {rendered:?}");
            assert_eq!(rendered.to_string(), format!("  │ {sample}"));
        }
    }

    #[test]
    fn indented_code_fence_inside_list_aligns_border_and_strips_leading_indent() {
        let mut fence = None;
        let open = line("    ```rust", 0, &mut fence);
        assert_eq!(open.to_string(), "      ╭─ rust ");

        let code = line("    let x = 42;", 1, &mut fence);
        assert_eq!(code.to_string(), "      │ let x = 42;");

        let nested_code = line("        let y = x + 1;", 2, &mut fence);
        assert_eq!(nested_code.to_string(), "      │     let y = x + 1;");

        let close = line("    ```", 3, &mut fence);
        assert_eq!(close.to_string(), "      ╰─");
    }

    #[test]
    fn code_badges_share_alias_colors_and_keep_unsupported_code_unchanged() {
        let mut colors = Vec::new();
        for (label, alias) in [
            ("rust", "RS"),
            ("r", "R"),
            ("powershell", "pwsh"),
            ("python", "py"),
        ] {
            let header = line(&format!("```{label}"), 0, &mut None);
            let badge = &header.spans[1];
            let alias_header = line(&format!("```{alias}"), 0, &mut None);
            assert_eq!(badge.style.bg, alias_header.spans[1].style.bg);
            assert_eq!(header.spans[0].style.fg, badge.style.bg);
            assert_eq!(badge.style.fg, Some(THEME.on_accent));
            assert!(badge.style.add_modifier.contains(Modifier::BOLD));
            assert!(!colors.contains(&badge.style.bg), "{label}");
            colors.push(badge.style.bg);
        }

        for (label, title) in [
            ("powershell", "powershell"),
            ("orbit-script", "orbit-script"),
            ("", "text"),
        ] {
            let mut fence = None;
            let header = line(&format!("```{label}"), 0, &mut fence);
            assert_eq!(header.to_string(), format!("  ╭─ {title} "));
            let color = header.spans[1].style.bg;
            let source = r".\orbit\Scripts\orbit.exe --version # 中文";
            let body = line(source, 1, &mut fence);
            assert_eq!(body.to_string(), format!("  │ {source}"));
            assert_eq!(body.spans[0].style.fg, color);
            let closing = line("```", 2, &mut fence);
            assert_eq!(closing.style.fg, color);
            assert!(fence.is_none());
        }
    }

    #[test]
    fn tables_align_unicode_wrap_cells_and_preserve_inline_content() {
        let source = "| 名称 | 值 |\n| :--- | ---: |\n| **中文** | `a\\|b` |\n| 长文本测试长文本 | 123 |\n\n后文";
        let (end, lines) = table(source, 28).unwrap();
        let rendered = lines
            .iter()
            .map(|line| line.line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("中文"), "{rendered}");
        assert!(rendered.contains("a|b"), "{rendered}");
        assert!(!rendered.contains("**") && !rendered.contains('`'));
        assert!(source[end..].contains("后文"));
        assert!(lines.iter().all(|line| line.line.width() <= 28));

        let (_, narrow) = table(source, 10).unwrap();

        assert!(narrow.iter().any(|l| l.line.to_string().contains("名称:")));
        assert!(table("normal | text\nnot a separator", 80).is_none());
    }

    #[test]
    fn narrow_table_tolerates_a_body_wider_than_its_header() {
        let cell = |text: &'static str| LinkedLine {
            line: Line::from(text),
            links: Vec::new(),
        };
        let rows = vec![vec![cell("名称")], vec![cell("一"), cell("二")]];

        let lines = labelled_table_rows(&rows);

        assert!(
            lines
                .iter()
                .any(|line| line.line.to_string().contains("第 2 列: 二"))
        );
    }

    #[test]
    fn code_highlighting_keeps_multiline_state_and_unknown_languages_readable() {
        let mut fence = None;
        line("```rust", 0, &mut fence);
        let code = line("let answer = 42; // comment", 1, &mut fence);

        assert_eq!(code.to_string(), "  │ let answer = 42; // comment");
        assert!(
            code.spans
                .windows(2)
                .any(|s| s[0].style.fg != s[1].style.fg)
        );

        line("/* comment", 2, &mut fence);
        let middle = line("still a comment", 3, &mut fence);
        let mut fresh = None;
        line("```rust", 0, &mut fresh);

        assert_ne!(middle, line("still a comment", 1, &mut fresh));

        line("```", 4, &mut fence);
        line("```unknown_language", 5, &mut fence);

        assert_eq!(
            line("中文 **raw**", 6, &mut fence).to_string(),
            "  │ 中文 **raw**"
        );
    }
    fn render(source: &str) -> Line<'static> {
        line(source, 1, &mut None)
    }

    #[test]
    fn formats_inline_styles_without_modifying_code_or_urls() {
        let value = render("**重点**，*斜体*，~~删除~~，`**原样**` [文档](https://example.com)");

        assert_eq!(
            value.to_string(),
            "  重点，斜体，删除，**原样** 文档 (https://example.com)"
        );
        assert!(
            value
                .spans
                .iter()
                .any(|s| s.content == "重点" && s.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            value
                .spans
                .iter()
                .any(|s| s.content == "斜体" && s.style.add_modifier.contains(Modifier::ITALIC))
        );
        assert_eq!(
            render(r"\*literal\* and snake_case_name").to_string(),
            "  *literal* and snake_case_name"
        );
        assert!(
            render("[1]: https://example.com")
                .to_string()
                .contains("https://example.com")
        );
    }

    #[test]
    fn renders_mark_extension_without_touching_code_or_escaped_source() {
        let value = render("正常 ==重点== 与 `==代码==`");

        assert_eq!(value.to_string(), "  正常 重点 与 ==代码==");
        assert!(value.spans.iter().any(|span| {
            span.content == "重点"
                && span.style.fg == Some(THEME.md_mark)
                && span.style.add_modifier.contains(Modifier::UNDERLINED)
        }));
        assert!(value.spans.iter().any(|span| {
            span.content == "==代码=="
                && span.style.fg == Some(THEME.md_inline_code)
                && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert_eq!(render(r"保留 \==字面量==").to_string(), "  保留 ==字面量==");
    }

    #[test]
    fn recognizes_and_styles_bare_safe_web_links() {
        let source = "访问 https://example.com/docs?from=orbit，或 www.rust-lang.org/learn。`https://code.invalid`";
        let rendered = render(source);

        assert_eq!(
            rendered
                .spans
                .iter()
                .filter(|span| span.style.fg == Some(crate::tui::theme::THEME.md_link))
                .count(),
            2
        );
        assert_eq!(
            links(source, &rendered)
                .into_iter()
                .map(|target| target.url)
                .collect::<Vec<_>>(),
            vec![
                "https://example.com/docs?from=orbit".to_owned(),
                "https://www.rust-lang.org/learn".to_owned(),
            ]
        );
    }

    #[test]
    fn repeated_prose_does_not_steal_link_label_hits_and_destinations_are_actionable() {
        let source = "文档之前的文档 [文档](https://example.com/first) 与 [**文档**](https://example.com/second)";
        let rendered = render(source);
        let text = rendered.to_string();
        let targets = links(source, &rendered);
        assert_eq!(targets.len(), 4);
        for (label, target) in [
            (
                "文档 (https://example.com/first)",
                "https://example.com/first",
            ),
            (
                "文档 (https://example.com/second)",
                "https://example.com/second",
            ),
        ] {
            let start = text.find(label).unwrap();
            assert!(
                targets
                    .iter()
                    .any(|link| link.start == start && link.url == target)
            );
            assert!(
                targets
                    .iter()
                    .any(|link| &text[link.start..link.end] == target && link.url == target)
            );
        }
        assert!(
            targets
                .iter()
                .all(|link| link.start >= "  文档之前的文档 ".len())
        );
    }

    #[test]
    fn task_list_replaces_the_list_bullet_with_its_checkbox() {
        assert_eq!(render("- [x] 已完成任务").to_string(), "  󰄵 已完成任务");
        assert_eq!(render("- [ ] 未完成任务").to_string(), "  󰄱 未完成任务");
    }

    #[test]
    fn recovers_model_style_strong_markers_next_to_cjk_text() {
        let value = render(
            r#"4. **Orbit 的测试数据需要"可重复"**：这里使用**"虚构的示例"**验证文本渲染。"#,
        );

        assert!(!value.to_string().contains("**"), "{value:?}");
        assert!(value.spans.iter().any(|span| {
            span.content == "\"虚构的示例\"" && span.style.add_modifier.contains(Modifier::BOLD)
        }));

        let escaped = render(r"保留 \*\*字面量\*\*");

        assert_eq!(escaped.to_string(), "  保留 **字面量**");
        assert!(
            escaped
                .spans
                .iter()
                .all(|span| !span.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn formats_blocks_and_preserves_indentation() {
        assert_eq!(line("普通段落", 0, &mut None).to_string(), "  普通段落");
        assert_eq!(render("## 标题").to_string(), "  标题");
        assert_eq!(render("  - **项目**").to_string(), "    • 项目");
        assert_eq!(render("3. 第三项").to_string(), "  3. 第三项");
        assert_eq!(render("> 引用").to_string(), "  ▎ 引用");
        assert!(render("- [x] 完成").to_string().contains("󰄵 完成"));
        assert!(render("#hashtag").to_string().contains("#hashtag"));
    }

    #[test]
    fn code_fences_require_matching_delimiter_and_length() {
        let mut fence = None;

        assert_eq!(line("````rust", 0, &mut fence).to_string(), "  ╭─ rust ");
        assert_eq!(line("```", 1, &mut fence).to_string(), "  │ ```");
        assert_eq!(line("**code**", 2, &mut fence).to_string(), "  │ **code**");

        line("````", 3, &mut fence);

        assert_eq!(fence, None);

        line("~~~python", 4, &mut fence);

        assert_eq!(
            fence.as_ref().map(|f| (f.delimiter, f.length)),
            Some(('~', 3))
        );

        line("~~~~", 5, &mut fence);

        assert_eq!(fence, None);
    }

    #[test]
    fn outer_fence_keeps_nested_markdown_as_source() {
        let mut fence = None;

        assert_eq!(
            line("````markdown", 0, &mut fence).to_string(),
            "  ╭─ markdown "
        );
        assert_eq!(line("# 标题", 1, &mut fence).to_string(), "  │ # 标题");
        assert_eq!(
            line("```python", 2, &mut fence).to_string(),
            "  │ ```python"
        );
        assert_eq!(
            line("**不应加粗**", 3, &mut fence).to_string(),
            "  │ **不应加粗**"
        );
        assert_eq!(line("```", 4, &mut fence).to_string(), "  │ ```");
        assert!(fence.is_some());
        assert_eq!(line("````", 5, &mut fence).to_string(), "  ╰─");
        assert!(fence.is_none());
    }

    #[test]
    fn markdown_examples_recover_nested_triple_fences() {
        let mut fence = None;
        line("```markdown", 0, &mut fence);

        assert_eq!(
            line("```python", 1, &mut fence).to_string(),
            "  │ ```python"
        );
        assert_eq!(
            line("print('raw')", 2, &mut fence).to_string(),
            "  │ print('raw')"
        );
        assert_eq!(line("```", 3, &mut fence).to_string(), "  │ ```");
        assert!(
            fence.is_some(),
            "inner close must not end outer Markdown example"
        );
        assert_eq!(
            line("# still source", 4, &mut fence).to_string(),
            "  │ # still source"
        );
        assert_eq!(line("```", 5, &mut fence).to_string(), "  ╰─");
        assert!(fence.is_none());
    }
}
