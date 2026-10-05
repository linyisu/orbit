//! Render complete source lines incrementally; never reparse the conversation on scroll.
//! CommonMark inline syntax is handled by pulldown-cmark, not regex replacements.
pub(super) mod blocks;
mod css;
mod html;
mod inline;

pub(super) use inline::State as InlineState;

use pulldown_cmark::{BrokenLink, BrokenLinkCallback, CowStr, Event, Options, Parser, Tag};
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

pub(super) const BODY_MARGIN: u16 = 2;

pub(super) fn body_width(width: u16) -> u16 {
    width.saturating_sub(BODY_MARGIN * 2)
}

pub(super) fn rule_line(width: u16) -> Line<'static> {
    let margin = BODY_MARGIN.min(width / 2);
    Line::from(vec![
        Span::raw(" ".repeat(usize::from(margin))),
        Span::styled(
            "─".repeat(usize::from(width - margin * 2)),
            Style::default().fg(super::theme::THEME.md_rule),
        ),
        Span::raw(" ".repeat(usize::from(margin))),
    ])
}

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

#[derive(Clone, Default)]
pub(super) struct LinkedLine {
    pub line: Line<'static>,
    pub links: Vec<LinkTarget>,
    // UTF-8 byte ranges of generated borders, labels and padding.
    pub decorations: Vec<std::ops::Range<usize>>,
    // A whole row of UI chrome; unlike an empty body row it adds no copied newline.
    pub chrome: bool,
    pub folds: Vec<Disclosure>,
    pub disclosure: Option<Disclosure>,
    pub media: Vec<super::media::Target>,
    pub graphics: Vec<super::media::Fragment>,
    pub copy: Vec<super::media::CopyText>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Disclosure {
    pub id: String,
    pub open: bool,
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
    /// Keep semantic line breaks; Ratatui's Display implementation removes them.
    pub fn text(&self) -> String {
        self.line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// Project alignment to real cells so selection and link hit-testing share the
    /// exact displayed coordinates rather than leaving alignment to Paragraph.
    pub fn align_to(mut self, width: u16) -> Self {
        let spare = usize::from(width).saturating_sub(self.line.width());
        let left = match self.line.alignment {
            Some(ratatui::layout::Alignment::Center) => spare / 2,
            Some(ratatui::layout::Alignment::Right) => spare,
            _ => 0,
        };
        self.line.alignment = None;
        if left > 0 {
            self = self.framed(vec![Span::raw(" ".repeat(left))], Vec::new());
        }
        self
    }
    pub fn plain(mut line: Line<'static>) -> Self {
        super::render::sanitize_line(&mut line);
        let links = plain_links(&line.to_string());
        Self {
            line,
            links,
            decorations: Vec::new(),
            chrome: false,
            ..Default::default()
        }
    }

    pub fn strip_body_margin(&mut self) {
        if let Some(first) = self.line.spans.first_mut()
            && let Some(rest) = first.content.strip_prefix("  ")
        {
            first.content = rest.to_owned().into();
            self.shift_offsets(-2);
        }
    }

    pub fn decorate(mut self, spans: &[usize]) -> Self {
        let mut offset = 0;
        for (index, span) in self.line.spans.iter().enumerate() {
            let end = offset + span.content.len();
            if spans.contains(&index) {
                self.decorations.push(offset..end);
            }
            offset = end;
        }
        self
    }

    pub fn chrome(line: Line<'static>) -> Self {
        let end = line.to_string().len();
        Self {
            line,
            links: Vec::new(),
            decorations: std::iter::once(0..end).collect(),
            chrome: true,
            ..Default::default()
        }
    }

    pub fn shift_offsets(&mut self, amount: isize) {
        for target in &mut self.media {
            target.range.start = target.range.start.saturating_add_signed(amount);
            target.range.end = target.range.end.saturating_add_signed(amount);
        }
        self.links.retain_mut(|link| {
            link.start = link.start.saturating_add_signed(amount);
            link.end = link.end.saturating_add_signed(amount);
            link.start < link.end
        });
        self.decorations.retain_mut(|range| {
            range.start = range.start.saturating_add_signed(amount);
            range.end = range.end.saturating_add_signed(amount);
            range.start < range.end
        });
    }

    pub fn framed(mut self, leading: Vec<Span<'static>>, trailing: Vec<Span<'static>>) -> Self {
        let columns = leading.iter().map(Span::width).sum::<usize>();
        for graphic in &mut self.graphics {
            graphic.x = graphic.x.saturating_add(columns as u16);
        }
        for copy in &mut self.copy {
            copy.columns.start += columns;
            copy.columns.end += columns;
        }
        let start = leading.len();
        let end = start + self.line.spans.len();
        for span in &mut self.line.spans {
            span.style = self.line.style.patch(span.style);
        }
        self.shift_offsets(leading.iter().map(|span| span.content.len()).sum::<usize>() as isize);
        self.line.spans.splice(0..0, leading);
        self.line.spans.extend(trailing);
        self.line.style = Style::default();
        let decorations = (0..start)
            .chain(end..self.line.spans.len())
            .collect::<Vec<_>>();
        self.decorate(&decorations)
    }

    pub fn wrap(self, width: u16) -> Vec<Self> {
        super::transcript::wrap_linked_line(self, width)
    }
}

pub(super) fn slice_decorations(
    ranges: &[std::ops::Range<usize>],
    slice: std::ops::Range<usize>,
    offset: usize,
) -> Vec<std::ops::Range<usize>> {
    ranges
        .iter()
        .filter_map(|range| {
            let start = range.start.max(slice.start);
            let end = range.end.min(slice.end);
            (start < end).then(|| start - slice.start + offset..end - slice.start + offset)
        })
        .collect()
}

#[derive(Default)]
pub(super) struct Context<'a> {
    parser: Option<Parser<'a>>,
}

impl<'a> Context<'a> {
    pub(super) fn math_candidate(source: &str) -> bool {
        source.trim_start_matches(' ').starts_with("$$")
            && !source.starts_with("    ")
            && !source.starts_with('\t')
    }

    pub(super) fn math_block(&self, source: &str) -> Option<(usize, Vec<LinkedLine>)> {
        // The parser owns delimiter matching, escaping and multiline math.
        // The inexpensive prefix check avoids parsing the rest of a transcript
        // for each ordinary text line.
        if !Self::math_candidate(source) {
            return None;
        }
        let mut parser = self.parse(source, Options::ENABLE_MATH).into_offset_iter();
        let (event, range) = parser.next()?;
        let (event, range) = if matches!(event, Event::Start(Tag::Paragraph)) {
            parser.next()?
        } else {
            (event, range)
        };
        let Event::DisplayMath(tex) = event else {
            return None;
        };
        if !source[..range.start].trim().is_empty() {
            return None;
        }
        let bytes = range.end
            + if source[range.end..].starts_with("\r\n") {
                2
            } else {
                usize::from(source[range.end..].starts_with('\n'))
            };
        Some((
            bytes,
            inline::render(
                std::iter::once(Event::DisplayMath(tex)),
                source[..bytes].trim(),
                "  ".into(),
                &mut InlineState::default(),
            ),
        ))
    }
    pub fn new(source: &'a str) -> Self {
        Self {
            parser: source
                .contains('[')
                .then(|| Parser::new_ext(source, Options::ENABLE_TABLES)),
        }
    }

    pub fn definitions(&self) -> Vec<(String, String, Option<String>)> {
        let mut definitions = self.parser.as_ref().map_or_else(Vec::new, |parser| {
            parser
                .reference_definitions()
                .iter()
                .map(|(label, definition)| {
                    (
                        label.to_owned(),
                        definition.dest.to_string(),
                        definition.title.as_ref().map(ToString::to_string),
                    )
                })
                .collect::<Vec<_>>()
        });
        definitions.sort();
        definitions
    }

    fn parse<'s>(
        &self,
        source: &'s str,
        options: Options,
    ) -> Parser<'s, impl BrokenLinkCallback<'s> + '_> {
        Parser::new_with_broken_link_callback(
            source,
            options,
            Some(move |link: BrokenLink<'s>| {
                let definition = self
                    .parser
                    .as_ref()?
                    .reference_definitions()
                    .get(&link.reference)?;
                Some((
                    CowStr::from(definition.dest.to_string()),
                    CowStr::from(
                        definition
                            .title
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    ),
                ))
            }),
        )
    }
}

#[cfg(test)]
pub(super) fn linked_line(source: &str, index: usize, fence: &mut Fence) -> LinkedLine {
    Context::default().linked_line(source, index, fence)
}

impl Context<'_> {
    #[cfg(test)]
    pub fn linked_line(&self, source: &str, index: usize, fence: &mut Fence) -> LinkedLine {
        self.linked_lines(source, index, fence, &mut InlineState::default())
            .remove(0)
    }

    /// Render one source line into physical lines using shared semantics and link metadata.
    pub fn linked_lines(
        &self,
        source: &str,
        _index: usize,
        fence: &mut Fence,
        state: &mut InlineState,
    ) -> Vec<LinkedLine> {
        let was_code = fence.is_some();
        if let Some(mut line) = self.code_line(source, fence) {
            super::render::sanitize_line(&mut line);
            let header = !was_code && fence.is_some();
            let chrome = header || (was_code && fence.is_none());
            let decorations = if header {
                (0..line.spans.len()).collect()
            } else {
                vec![0]
            };
            let links = plain_links(&line.to_string());
            return vec![
                LinkedLine {
                    line,
                    links,
                    decorations: Vec::new(),
                    chrome,
                    ..Default::default()
                }
                .decorate(&decorations),
            ];
        }
        let trimmed = source.trim_start();
        let indent = source.len() - trimmed.len();
        inline::render(
            self.parse(
                trimmed,
                Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_MATH,
            ),
            trimmed,
            format!("  {}", &source[..indent]),
            state,
        )
    }
}

pub(super) fn document(source: &str, width: u16) -> Vec<LinkedLine> {
    document_at(source, width, "body", 0)
}

fn document_at(source: &str, width: u16, path: &str, depth: usize) -> Vec<LinkedLine> {
    let context = Context::new(source);
    let mut out = Vec::new();
    let mut remaining = source;
    let mut fence = None;
    let mut inline = InlineState::default();
    let mut index = 0;
    while !remaining.is_empty() {
        if fence.is_none()
            && let Some((bytes, lines)) = context.math_block(remaining)
        {
            out.extend(lines);
            index += remaining[..bytes].lines().count();
            remaining = &remaining[bytes..];
            continue;
        }
        if fence.is_none()
            && let Some(block) = blocks::render(
                remaining,
                width,
                &format!("{path}/{}", source.len() - remaining.len()),
                depth,
            )
        {
            out.extend(block.lines);
            index += remaining[..block.bytes].lines().count();
            remaining = &remaining[block.bytes..];
            continue;
        }
        if fence.is_none()
            && let Some((bytes, lines)) = context.table(remaining, width)
        {
            out.extend(lines);
            index += remaining[..bytes].lines().count();
            remaining = &remaining[bytes..];
            continue;
        }

        let part = remaining.split_inclusive('\n').next().unwrap();
        let source = part.trim_end_matches(['\r', '\n']);
        out.extend(context.linked_lines(source, index, &mut fence, &mut inline));
        remaining = &remaining[part.len()..];
        index += 1;
    }
    out
}

pub(super) fn expand_rule(line: &mut Line<'static>, width: u16) -> bool {
    let mut visible = line
        .spans
        .iter()
        .filter(|span| !span.content.trim().is_empty());
    if let Some(rule) = visible.next()
        && visible.next().is_none()
        && rule.style.fg == Some(THEME.md_rule)
        && rule.content.chars().all(|ch| ch == '─')
    {
        *line = rule_line(width);
        return true;
    }
    false
}

use super::theme::THEME;

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
                            '<' | '>' | '"' | '\'' | '`' | '，' | '。' | '！' | '？' | '；' | '：'
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

#[cfg(test)]
fn links(source: &str, rendered: &Line<'_>) -> Vec<LinkTarget> {
    Context::default().links(source, rendered)
}

impl Context<'_> {
    #[cfg(test)]
    pub fn links(&self, source: &str, _rendered: &Line<'_>) -> Vec<LinkTarget> {
        self.linked_line(source, 0, &mut None).links
    }
}

#[cfg(test)]
fn table(source: &str, width: u16) -> Option<(usize, Vec<LinkedLine>)> {
    Context::default().table(source, width)
}

impl Context<'_> {
    /// Parse only a possible table at the current source position. The caller keeps
    /// an open table provisional so new rows can adjust column widths during streaming.
    pub fn table(&self, source: &str, width: u16) -> Option<(usize, Vec<LinkedLine>)> {
        if !source.lines().next()?.contains('|') {
            return None;
        }

        let mut events = self
            .parse(source, Options::ENABLE_TABLES)
            .into_offset_iter();
        let (Event::Start(Tag::Table(alignments)), _) = events.next()? else {
            return None;
        };

        let mut rows: Vec<Vec<Cell>> = Vec::new();
        let mut end = 0;

        for (event, range) in events {
            match event {
                Event::Start(Tag::TableHead | Tag::TableRow) => rows.push(Vec::new()),
                Event::Start(Tag::TableCell) => {
                    let cell_source = source[range].trim().replace(r"\|", "|");
                    let mut lines =
                        self.linked_lines(&cell_source, 1, &mut None, &mut InlineState::default());
                    for line in &mut lines {
                        let removed = line.line.spans.remove(0).content.len();
                        line.shift_offsets(-(removed as isize));
                    }
                    let header = rows.len() == 1;
                    rows.last_mut()?.push(Cell {
                        lines,
                        colspan: 1,
                        alignment: None,
                        header,
                    });
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

        Some((end, render_table(&rows, &alignments, width)))
    }
}

fn render_table(
    rows: &[Vec<Cell>],
    alignments: &[pulldown_cmark::Alignment],
    width: u16,
) -> Vec<LinkedLine> {
    let cols = alignments.len();
    if cols == 0 || rows.is_empty() {
        return Vec::new();
    }
    let available = usize::from(width).saturating_sub(3 * cols + 3);

    // On narrow terminals use labelled cells rather than losing columns/content.
    if available < cols * 3 {
        return labelled_table_rows(rows);
    }
    let normalized = rows
        .iter()
        .map(|row| {
            let mut row = row.clone();
            let occupied = row.iter().map(|cell| cell.colspan).sum::<usize>();
            for _ in occupied..cols {
                row.push(Cell {
                    lines: vec![LinkedLine::default()],
                    colspan: 1,
                    alignment: None,
                    header: false,
                });
            }
            row
        })
        .collect::<Vec<_>>();
    let rows = normalized.as_slice();

    let mut widths = vec![3usize; cols];
    for row in rows {
        let mut column = 0;
        for cell in row {
            let count = cell.colspan.min(cols - column);
            let desired = cell
                .lines
                .iter()
                .map(|line| line.line.width())
                .max()
                .unwrap_or(0);
            let available =
                widths[column..column + count].iter().sum::<usize>() + 3 * count.saturating_sub(1);
            if desired > available {
                widths[column + count - 1] += desired - available;
            }
            column += count;
            if column >= cols {
                break;
            }
        }
    }

    while widths.iter().sum::<usize>() > available {
        let Some(i) = (0..cols).max_by_key(|&i| widths[i]) else {
            break;
        };

        widths[i] -= 1;
    }

    let boundaries = |row: &[Cell]| {
        let mut column = 0;
        row.iter()
            .map(|cell| {
                column += cell.colspan;
                column
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let border = |left: &str, above: Option<&[Cell]>, below: Option<&[Cell]>, right: &str| {
        let above = above.map(boundaries).unwrap_or_default();
        let below = below.map(boundaries).unwrap_or_default();
        let mut body = String::new();
        for (i, w) in widths.iter().enumerate() {
            body.push_str(&"─".repeat(w + 2));
            if i + 1 < cols {
                body.push(match (above.contains(&(i + 1)), below.contains(&(i + 1))) {
                    (true, true) => '┼',
                    (true, false) => '┴',
                    (false, true) => '┬',
                    (false, false) => '─',
                });
            }
        }
        LinkedLine::chrome(Line::styled(
            format!("  {left}{body}{right}"),
            Style::default().fg(THEME.md_table_border),
        ))
    };
    let mut lines = vec![border("┌", None, rows.first().map(Vec::as_slice), "┐")];

    for (r, row) in rows.iter().enumerate() {
        let mut column = 0;
        let cells: Vec<_> = row
            .iter()
            .map(|cell| {
                let count = cell.colspan.min(cols.saturating_sub(column));
                let span_width = widths[column..column + count].iter().sum::<usize>()
                    + 3 * count.saturating_sub(1);
                column += count;
                (
                    span_width,
                    cell.lines
                        .iter()
                        .flat_map(|line| wrap_cell(line, span_width))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();

        for n in 0..cells
            .iter()
            .map(|(_, lines)| lines.len())
            .max()
            .unwrap_or(1)
        {
            let mut spans = vec![Span::styled(
                "  │",
                Style::default().fg(THEME.md_table_border),
            )];
            let mut links = Vec::new();
            let mut decorations = vec![0];
            let mut byte_offset = spans[0].content.len();

            let mut column = 0;
            for (i, (cell_width, rendered)) in cells.iter().enumerate() {
                let cell = rendered.get(n);
                let definition = &row[i];
                let padding = cell_width.saturating_sub(cell.map_or(0, |cell| cell.line.width()));
                let alignment = definition.alignment.unwrap_or(match alignments[column] {
                    pulldown_cmark::Alignment::Right => ratatui::layout::Alignment::Right,
                    pulldown_cmark::Alignment::Center => ratatui::layout::Alignment::Center,
                    _ => ratatui::layout::Alignment::Left,
                });
                let left = match alignment {
                    ratatui::layout::Alignment::Right => padding,
                    ratatui::layout::Alignment::Center => padding / 2,
                    _ => 0,
                };
                decorations.push(spans.len());
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
                        if definition.header {
                            span.style = span
                                .style
                                .add_modifier(Modifier::BOLD)
                                .fg(THEME.md_table_header);
                        }
                        span
                    }));
                }
                decorations.extend([spans.len(), spans.len() + 1]);
                spans.push(Span::raw(" ".repeat(padding - left + 1)));
                spans.push(Span::styled(
                    "│",
                    Style::default().fg(THEME.md_table_border),
                ));
                byte_offset += padding - left + 1 + "│".len();
                column += definition.colspan;
            }

            lines.push(
                LinkedLine {
                    line: Line::from(spans),
                    links,
                    decorations: Vec::new(),
                    chrome: false,
                    ..Default::default()
                }
                .decorate(&decorations),
            );
        }

        if r + 1 < rows.len() {
            let mut separator = border("├", Some(row), Some(&rows[r + 1]), "┤");

            if r > 0 {
                separator.line.style = Style::default().fg(THEME.md_table_separator);
            }

            lines.push(separator);
        }
    }

    lines.push(border("└", rows.last().map(Vec::as_slice), None, "┘"));

    lines
}

#[derive(Clone)]
struct Cell {
    lines: Vec<LinkedLine>,
    colspan: usize,
    alignment: Option<ratatui::layout::Alignment>,
    header: bool,
}

impl Cell {
    fn heading(&self) -> LinkedLine {
        let mut spans = Vec::new();
        let mut links = Vec::new();
        let mut offset = 0;
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw(" "));
                offset += 1;
            }
            links.extend(line.links.iter().cloned().map(|mut link| {
                link.start += offset;
                link.end += offset;
                link
            }));
            offset += line.line.to_string().len();
            spans.extend(line.line.spans.iter().cloned());
        }
        LinkedLine {
            line: Line::from(spans),
            links,
            decorations: Vec::new(),
            chrome: false,
            ..Default::default()
        }
    }
}

fn labelled_table_rows(rows: &[Vec<Cell>]) -> Vec<LinkedLine> {
    let mut lines = Vec::new();
    let has_header = rows[0].iter().any(|cell| cell.header);
    for row in rows.iter().skip(usize::from(has_header)) {
        let mut column = 0;
        for cell in row {
            let start = column;
            column += cell.colspan;
            let mut heading_column = 0;
            let headings = rows[0]
                .iter()
                .filter_map(|header| {
                    let begin = heading_column;
                    heading_column += header.colspan;
                    (has_header && begin < column && heading_column > start)
                        .then(|| header.heading())
                })
                .collect::<Vec<_>>();
            let mut header = LinkedLine::default();
            for heading in headings {
                if !header.line.spans.is_empty() {
                    header.line.spans.push(Span::raw(" / "));
                }
                let offset = header.line.to_string().len();
                header
                    .links
                    .extend(heading.links.into_iter().map(|mut link| {
                        link.start += offset;
                        link.end += offset;
                        link
                    }));
                header.line.spans.extend(heading.line.spans);
            }
            if header.line.spans.is_empty() {
                header.line = Line::from(if cell.colspan == 1 {
                    format!("第 {} 列", start + 1)
                } else {
                    format!("第 {}–{} 列", start + 1, column)
                });
            }
            let label = format!("  {}: ", header.line);
            for (index, line) in cell.lines.iter().enumerate() {
                let prefix = if index == 0 {
                    label.clone()
                } else {
                    " ".repeat(label.width())
                };
                let offset = prefix.len();
                let mut links: Vec<_> = line
                    .links
                    .iter()
                    .cloned()
                    .map(|mut link| {
                        link.start += offset;
                        link.end += offset;
                        link
                    })
                    .collect();
                if index == 0 {
                    links.extend(header.links.iter().cloned().map(|mut link| {
                        link.start += 2;
                        link.end += 2;
                        link
                    }));
                }
                let mut spans = vec![Span::styled(
                    prefix,
                    Style::default().fg(THEME.md_table_header),
                )];
                spans.extend(line.line.spans.iter().cloned());
                lines.push(LinkedLine {
                    line: Line::from(spans),
                    links,
                    decorations: Vec::new(),
                    chrome: false,
                    ..Default::default()
                });
            }
        }
        lines.push(LinkedLine::plain(Line::from("")));
    }
    if lines.is_empty() {
        lines.extend(rows[0].iter().map(Cell::heading));
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
            let decorations = slice_decorations(&cell.decorations, offset..end, 0);
            offset = end;
            LinkedLine {
                line,
                links,
                decorations,
                chrome: cell.chrome,
                folds: cell.folds.clone(),
                disclosure: cell.disclosure.clone(),
                ..Default::default()
            }
        })
        .collect()
}

fn is_fence_opening(marker: char, count: usize, trimmed: &str) -> bool {
    matches!(marker, '`' | '~')
        && count >= 3
        && !(marker == '`' && trimmed[count..].contains('`'))
        && !trimmed[count..].trim().is_empty()
}

#[cfg(test)]
pub(super) fn line(source: &str, index: usize, fence: &mut Fence) -> Line<'static> {
    Context::default().line(source, index, fence)
}

impl Context<'_> {
    #[cfg(test)]
    pub fn line(&self, source: &str, index: usize, fence: &mut Fence) -> Line<'static> {
        self.linked_line(source, index, fence).line
    }

    fn code_line(&self, source: &str, fence: &mut Fence) -> Option<Line<'static>> {
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
                    if marker == delimiter && count >= length && trimmed[count..].trim().is_empty()
                    {
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
                return Some(Line::styled(
                    format!("  {indent}╰─"),
                    Style::default().fg(color),
                ));
            }

            let indent = " ".repeat(code.indent);
            let code_line = source.strip_prefix(&indent).unwrap_or(source);
            return Some(
                code.renderer
                    .render(
                        code_line,
                        super::code::Options {
                            gutter: super::code::Gutter::Rail {
                                indent: code.indent + 2,
                                color: code.color,
                            },
                            ..Default::default()
                        },
                    )
                    .remove(0),
            );
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
            return Some(Line::from(vec![
                Span::styled(format!("  {indent_str}╭─"), Style::default().fg(color)),
                Span::styled(
                    format!(" {label} "),
                    Style::default()
                        .fg(THEME.on_accent)
                        .bg(color)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }

        None
    }
}

#[cfg(test)]
mod tests;
