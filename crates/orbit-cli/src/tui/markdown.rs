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
mod tests {
    use super::*;
    use crate::tui::languages::syntax_for_label;

    #[test]
    fn basic_html_and_markdown_share_nested_semantic_styles() {
        for (html, markdown) in [
            ("<b>粗体</b>", "**粗体**"),
            ("<strong>粗体</strong>", "**粗体**"),
            ("<em>斜体</em>", "*斜体*"),
            ("<i>斜体</i>", "*斜体*"),
            ("<del>删除</del>", "~~删除~~"),
            ("<s>删除</s>", "~~删除~~"),
            ("<mark>高亮</mark>", "==高亮=="),
            ("<code>代码</code>", "`代码`"),
            ("<strong><em>粗斜体</em></strong>", "***粗斜体***"),
            ("<b>*粗斜体*</b>", "***粗斜体***"),
        ] {
            let html = document(html, 80);
            let markdown = document(markdown, 80);
            assert_eq!(html[0].line, markdown[0].line);
        }
        let value = render("<u>下划线</u> 正常");
        assert!(value.spans.iter().any(
            |span| span.content == "下划线" && span.style.add_modifier == Modifier::UNDERLINED
        ));
        assert!(
            value
                .spans
                .iter()
                .any(|span| span.content.contains("正常") && span.style.add_modifier.is_empty())
        );
    }

    #[test]
    fn html_tokenizer_handles_case_attributes_entities_and_retains_unsupported_source() {
        let value = render("<STRONG title='a > b' style='color:red'>甲 &amp; 乙</STRONG> 正常");
        assert_eq!(value.to_string(), "  甲 & 乙 正常");
        assert!(
            value
                .spans
                .iter()
                .filter(|span| span.content.contains("甲"))
                .all(|span| span.style.fg == Some(Color::Rgb(255, 0, 0)))
        );
        assert_eq!(render("<span>原文</span>").to_string(), "  原文");
        for source in [
            "<div>原文</div>",
            "<!-- 注释 -->",
            "<script>alert('x')</script>",
            "<style>b{color:red}</style>",
        ] {
            assert_eq!(render(source).to_string(), format!("  {source}"));
        }
        assert_eq!(
            render("`<strong>原文</strong>`").to_string(),
            "  <strong>原文</strong>"
        );
        for label in ["html", "markdown", "text"] {
            let mut fence = None;
            line(&format!("```{label}"), 0, &mut fence);
            assert_eq!(
                line("<b>原文</b><br>下一行", 1, &mut fence).to_string(),
                "  │ <b>原文</b><br>下一行"
            );
        }
    }

    #[test]
    fn html_breaks_preserve_styles_links_and_physical_line_offsets() {
        let url = "https://orbit.example/?a=1&b=2";
        let source =
            "- <a href='https://orbit.example/?a=1&amp;b=2'><b>中文<br />下一行</b></a> 正常";
        let rows = document(source, 80);
        assert_eq!(
            rows.iter()
                .map(|row| row.line.to_string())
                .collect::<Vec<_>>(),
            ["  • 中文", "  下一行 正常"]
        );
        for (row, label) in rows.iter().zip(["中文", "下一行"]) {
            assert_eq!(row.links.len(), 1);
            assert_eq!(row.links[0].url, url);
            assert_eq!(
                &row.line.to_string()[row.links[0].start..row.links[0].end],
                label
            );
            assert!(row.line.spans.iter().any(|span| {
                span.content == label
                    && span
                        .style
                        .add_modifier
                        .contains(Modifier::BOLD | Modifier::UNDERLINED)
            }));
        }
        assert_eq!(
            document("甲<BR>乙<br/>丙", 80)
                .iter()
                .map(|row| row.line.to_string())
                .collect::<Vec<_>>(),
            ["  甲", "  乙", "  丙"]
        );
        let rows = document("\t<a href='https://orbit.example'>链接</a>", 80);
        let link = &rows[0].links[0];
        assert_eq!(&rows[0].line.to_string()[link.start..link.end], "链接");
        assert!(!rows[0].line.to_string().contains('\t'));
    }

    #[test]
    fn html_scripts_convert_complete_groups_and_keep_link_targets_after_conversion() {
        let source = "H<sub>2</sub>O x<sup>2</sup> y<sup>任意文字</sup> z<sub>ABC</sub>";
        assert_eq!(render(source).to_string(), "  H₂O x² y^(任意文字) z_(ABC)");
        let rows = document(
            "<a href='https://orbit.example'>x<sup><b>12</b></sup>水<sub>AB</sub></a>",
            80,
        );
        assert_eq!(rows[0].line.to_string(), "  x¹²水_(AB)");
        assert_eq!(rows[0].links.len(), 1);
        let link = &rows[0].links[0];
        assert_eq!(
            &rows[0].line.to_string()[link.start..link.end],
            "x¹²水_(AB)"
        );
        assert_eq!(link.url, "https://orbit.example");
    }

    #[test]
    fn html_styles_and_links_survive_source_lines_and_close_before_following_text() {
        let rows = document(
            "<a href='https://orbit.example'><strong>\n第一行\n第二行</strong></a> 正常",
            80,
        );
        for row in &rows[1..] {
            assert!(row.line.spans.iter().any(|span| {
                span.content.starts_with('第')
                    && span
                        .style
                        .add_modifier
                        .contains(Modifier::BOLD | Modifier::UNDERLINED)
            }));
            assert_eq!(row.links.len(), 1);
        }
        assert!(
            rows[2]
                .line
                .spans
                .iter()
                .any(|span| span.content.contains("正常") && span.style.add_modifier.is_empty())
        );
    }

    #[test]
    fn html_table_cells_break_into_rows_and_keep_links_on_narrow_and_wide_layouts() {
        let source = "| 名称 | 内容 |\n| --- | --- |\n| 示例 | <a href='https://orbit.example'>第一<br>第二</a> |";
        for width in [14, 16, 80] {
            let rows = document(source, width);
            let labels: String = rows
                .iter()
                .flat_map(|row| {
                    row.links.iter().map(|link| {
                        assert_eq!(link.url, "https://orbit.example");
                        row.line.to_string()[link.start..link.end].to_owned()
                    })
                })
                .collect();
            assert_eq!(labels, "第一第二");
            assert!(rows.iter().filter(|row| !row.links.is_empty()).count() >= 2);
            assert!(!rows.iter().any(|row| row.line.to_string().contains("<br>")));
        }
    }

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
        let cell = |text: &'static str| Cell {
            lines: vec![LinkedLine::plain(Line::from(text))],
            colspan: 1,
            alignment: None,
            header: true,
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

        assert_eq!(value.to_string(), "  重点，斜体，删除，**原样** 文档");
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
        assert!(value.spans.iter().any(|span| {
            span.content == "删除"
                && span.style.add_modifier.contains(Modifier::CROSSED_OUT)
                && span.style.fg == Some(THEME.md_strikethrough)
        }));
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
                "https://code.invalid".to_owned(),
            ]
        );
    }

    #[test]
    fn repeated_prose_does_not_steal_link_label_hits_when_destinations_are_hidden() {
        let source = "文档之前的文档 [文档](https://orbit.example/first) 与 [**文档**](https://orbit.example/second)";
        let rendered = render(source);
        let text = rendered.to_string();
        let targets = links(source, &rendered);
        assert_eq!(text, "  文档之前的文档 文档 与 文档");
        assert_eq!(targets.len(), 2);
        for (link, target) in targets.iter().zip(["first", "second"]) {
            assert_eq!(&text[link.start..link.end], "文档");
            assert_eq!(link.url, format!("https://orbit.example/{target}"));
            assert!(link.start >= "  文档之前的文档 ".len());
        }
    }

    #[test]
    fn reference_links_images_and_bare_definitions_keep_targets_without_printing_destinations() {
        let source = "[Orbit 文档][docs] 与 ![Orbit 图片](https://orbit.example/image)\n\n| 项 | 地址 |\n|---|---|\n| 文档 | [Orbit][DOCS] |\n\n[docs]: https://orbit.example/docs/ \"Orbit Guide\"\n";
        let lines = document(source, 80);
        assert_eq!(lines[0].line.to_string(), "  Orbit 文档 与 Orbit 图片");
        let targets: Vec<_> = lines
            .iter()
            .flat_map(|line| {
                line.links.iter().map(move |link| {
                    let text = line.line.to_string();
                    (text[link.start..link.end].to_owned(), link.url.as_str())
                })
            })
            .collect();
        for (label, target) in [
            ("Orbit 文档", "https://orbit.example/docs/"),
            ("Orbit 图片", "https://orbit.example/image"),
            ("Orbit", "https://orbit.example/docs/"),
            ("https://orbit.example/docs/", "https://orbit.example/docs/"),
        ] {
            assert!(targets.contains(&(label.to_owned(), target)), "{targets:?}");
        }
        let definition = lines
            .iter()
            .find(|line| line.line.to_string().contains("[docs]:"))
            .unwrap();
        assert!(
            definition
                .line
                .spans
                .iter()
                .any(|span| span.style.fg == Some(THEME.md_link))
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
