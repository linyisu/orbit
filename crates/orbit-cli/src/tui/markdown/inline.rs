//! One semantic renderer for Markdown events and tokenized HTML fragments.
use pulldown_cmark::{Event, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::{LinkTarget, LinkedLine, THEME, html, icons};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Formatting {
    Bold,
    Italic,
    Strike,
    Underline,
    Mark,
    Code,
    Heading(pulldown_cmark::HeadingLevel),
    Quote,
    Link(String),
    Image(String),
    Group(Vec<Formatting>),
    Custom(Style),
}

impl Formatting {
    fn hyperlink(&self) -> Option<&str> {
        match self {
            Self::Link(url) if !url.is_empty() => Some(url),
            Self::Group(items) => items.iter().find_map(Self::hyperlink),
            _ => None,
        }
    }

    fn image_link(&self) -> Option<&str> {
        match self {
            Self::Image(url) if super::super::browser_url(url).is_ok() => Some(url),
            Self::Group(items) => items.iter().find_map(Self::image_link),
            _ => None,
        }
    }

    fn code(&self) -> bool {
        matches!(self, Self::Code)
            || matches!(self, Self::Group(items) if items.iter().any(Self::code))
    }

    fn style(&self, style: Style) -> Style {
        match self {
            Self::Bold => style
                .fg(if style.add_modifier.contains(Modifier::ITALIC) {
                    THEME.md_bold_italic
                } else {
                    THEME.md_bold
                })
                .add_modifier(Modifier::BOLD),

            Self::Italic => style
                .fg(if style.add_modifier.contains(Modifier::BOLD) {
                    THEME.md_bold_italic
                } else {
                    THEME.md_italic
                })
                .add_modifier(Modifier::ITALIC),

            Self::Strike => style
                .fg(THEME.md_strikethrough)
                .add_modifier(Modifier::CROSSED_OUT),
            Self::Underline => style.add_modifier(Modifier::UNDERLINED),
            Self::Mark => super::mark_style(style),
            Self::Code => style.fg(THEME.md_inline_code).add_modifier(Modifier::BOLD),
            Self::Heading(level) => style
                .fg(match level {
                    pulldown_cmark::HeadingLevel::H1 => THEME.md_h1,
                    pulldown_cmark::HeadingLevel::H2 => THEME.md_h2,
                    pulldown_cmark::HeadingLevel::H3 => THEME.md_h3,
                    _ => THEME.md_h4_plus,
                })
                .add_modifier(Modifier::BOLD),

            Self::Quote => style.fg(THEME.md_quote_text),
            Self::Link(_) => style.fg(THEME.md_link).add_modifier(Modifier::UNDERLINED),
            Self::Image(_) => style,
            Self::Group(items) => items.iter().fold(style, |style, item| item.style(style)),
            Self::Custom(custom) => style.patch(*custom),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Markdown(TagEnd),
    Html(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    key: Key,
    formatting: Option<Formatting>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct State {
    frames: Vec<Frame>,
}

#[derive(Clone)]
struct Atom {
    span: Span<'static>,
    url: Option<String>,
    media: Option<crate::tui::media::Source>,
}

pub(super) fn render<'a>(
    events: impl Iterator<Item = Event<'a>>,
    source: &str,
    prefix: String,
    state: &mut State,
) -> Vec<LinkedLine> {
    let mut renderer = Renderer {
        frames: state.frames.clone(),
        rows: vec![Vec::new()],
        ordered: None,
        recover_strong: !source.contains(r"\**") && !source.contains(r"\*\*"),
        recover_mark: !source.contains(r"\=="),
        semantic_html: false,
        image_starts: Vec::new(),
    };
    for event in events {
        renderer.event(event);
    }
    if renderer.rows.iter().all(Vec::is_empty)
        && !source.trim().is_empty()
        && !renderer.semantic_html
    {
        let mut spans = Vec::new();
        super::push_text_with_bare_links(&mut spans, source, renderer.style(), false);
        for span in spans {
            renderer.push(span);
        }
    }

    state.frames = renderer
        .frames
        .iter()
        .filter(|frame| matches!(frame.key, Key::Html(_)))
        .cloned()
        .collect();
    renderer.finish(prefix)
}

struct Renderer {
    frames: Vec<Frame>,
    rows: Vec<Vec<Atom>>,
    ordered: Option<u64>,
    recover_strong: bool,
    recover_mark: bool,
    semantic_html: bool,
    image_starts: Vec<usize>,
}

impl Renderer {
    fn style(&self) -> Style {
        self.frames
            .iter()
            .filter_map(|frame| frame.formatting.as_ref())
            .fold(Style::default(), |style, formatting| {
                formatting.style(style)
            })
    }

    fn push(&mut self, span: Span<'static>) {
        let url = self
            .frames
            .iter()
            .rev()
            .find_map(|frame| {
                frame
                    .formatting
                    .as_ref()
                    .and_then(Formatting::hyperlink)
                    .map(str::to_owned)
            })
            .or_else(|| {
                self.frames.iter().rev().find_map(|frame| {
                    frame
                        .formatting
                        .as_ref()
                        .and_then(Formatting::image_link)
                        .map(str::to_owned)
                })
            });

        self.rows.last_mut().unwrap().push(Atom {
            span,
            url,
            media: self.frames.iter().rev().find_map(|f| match &f.formatting {
                Some(Formatting::Image(url)) => Some(crate::tui::media::Source::Image(url.clone())),
                _ => None,
            }),
        });
    }

    fn text(&mut self, text: &str, markdown: bool) {
        let style = self.style();
        let literal = self
            .frames
            .iter()
            .any(|frame| frame.formatting.as_ref().is_some_and(Formatting::code));

        if literal || !markdown {
            self.push(Span::styled(text.to_owned(), style));
        } else {
            let mut spans = Vec::new();
            super::push_text_with_cjk_strong(
                &mut spans,
                text,
                style,
                self.recover_strong,
                self.recover_mark,
            );
            for span in spans {
                self.push(span);
            }
        }
    }

    fn event(&mut self, event: Event<'_>) {
        let display = matches!(&event, Event::DisplayMath(_));
        match event {
            Event::Start(tag) => {
                let formatting = match &tag {
                    Tag::Heading { level, .. } => Some(Formatting::Heading(*level)),
                    Tag::Strong => Some(Formatting::Bold),
                    Tag::Emphasis => Some(Formatting::Italic),
                    Tag::Strikethrough => Some(Formatting::Strike),
                    Tag::BlockQuote(_) => {
                        self.push(Span::styled("▎ ", Style::default().fg(THEME.md_quote_rail)));
                        Some(Formatting::Quote)
                    }

                    Tag::List(start) => {
                        self.ordered = *start;
                        None
                    }

                    Tag::Item => {
                        self.push(Span::styled(
                            self.ordered.map_or("• ".into(), |n| format!("{n}. ")),
                            Style::default().fg(if self.ordered.is_some() {
                                THEME.md_ordered_bullet
                            } else {
                                THEME.md_unordered_bullet
                            }),
                        ));
                        None
                    }

                    Tag::Link { dest_url, .. } => Some(Formatting::Link(dest_url.to_string())),
                    Tag::Image { dest_url, .. } => {
                        self.image_starts.push(self.rows.iter().map(Vec::len).sum());
                        Some(Formatting::Image(dest_url.to_string()))
                    }

                    _ => None,
                };

                self.frames.push(Frame {
                    key: Key::Markdown(tag.to_end()),
                    formatting,
                });
            }

            Event::End(tag) => {
                if tag == TagEnd::Image
                    && self.image_starts.pop() == Some(self.rows.iter().map(Vec::len).sum())
                {
                    self.text("图片", false);
                }

                if let Some(index) = self
                    .frames
                    .iter()
                    .rposition(|frame| frame.key == Key::Markdown(tag))
                {
                    self.frames.remove(index);
                }
            }

            Event::Text(text) => self.text(&text, true),

            Event::Code(text) => {
                let style = Formatting::Code.style(self.style());
                self.push(Span::styled(text.into_string(), style));
            }

            Event::InlineMath(tex) | Event::DisplayMath(tex) => {
                let source = crate::tui::media::Source::Math {
                    tex: tex.to_string(),
                    display,
                };
                self.push(Span::styled(
                    format!(
                        "{}{}{}",
                        if display { "$$" } else { "$" },
                        tex,
                        if display { "$$" } else { "$" }
                    ),
                    Style::default().fg(THEME.md_inline_code),
                ));
                self.rows.last_mut().unwrap().last_mut().unwrap().media = Some(source);
            }

            Event::Html(text) | Event::InlineHtml(text) => {
                if let Some(events) = html::events(&text) {
                    self.semantic_html = true;
                    for event in events {
                        match event {
                            html::Event::Start(name, formatting) => self.frames.push(Frame {
                                key: Key::Html(name),
                                formatting: Some(formatting),
                            }),

                            html::Event::End(name) => {
                                if let Some(index) = self
                                    .frames
                                    .iter()
                                    .rposition(|frame| frame.key == Key::Html(name.clone()))
                                {
                                    self.frames.truncate(index);
                                }
                            }

                            html::Event::Text(text) => self.text(&text, false),
                            html::Event::Break => self.rows.push(Vec::new()),
                            html::Event::Image(label, url) => {
                                self.frames.push(Frame {
                                    key: Key::Html("img".into()),
                                    formatting: Some(Formatting::Image(url)),
                                });
                                self.text(&label, false);
                                self.frames.pop();
                            }
                        }
                    }
                } else {
                    self.text(&text, false);
                }
            }

            Event::Rule => self.push(Span::styled(
                "────────────────────────",
                Style::default().fg(THEME.md_rule),
            )),

            Event::TaskListMarker(done) => {
                let checkbox = format!("{} ", icons::checkbox(done));
                let style = Style::default().fg(if done {
                    THEME.md_task_done
                } else {
                    THEME.secondary
                });
                let marker =
                    self.rows.last_mut().unwrap().iter_mut().rev().find(|atom| {
                        atom.span.content == "• "
                            || atom.span.content.strip_suffix(". ").is_some_and(|n| {
                                !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
                            })
                    });

                if let Some(marker) = marker {
                    marker.span = Span::styled(checkbox, style);
                } else {
                    self.push(Span::styled(checkbox, style));
                }
            }

            Event::HardBreak => self.rows.push(Vec::new()),
            Event::SoftBreak => self.text(" ", false),
            _ => {}
        }
    }

    fn finish(self, prefix: String) -> Vec<LinkedLine> {
        let prefix = super::super::render::safe_output(&prefix);
        self.rows
            .into_iter()
            .map(|mut atoms| {
                // Repair parser-split CJK emphasis within a single semantic context.
                let mut start = 0;
                while start < atoms.len() {
                    let mut end = start + 1;
                    while end < atoms.len()
                        && atoms[end].url == atoms[start].url
                        && atoms[end].media == atoms[start].media
                    {
                        end += 1;
                    }

                    let mut spans: Vec<_> = atoms[start..end]
                        .iter()
                        .map(|atom| atom.span.clone())
                        .collect();
                    super::recover_split_strong(&mut spans, 0, self.recover_strong);
                    let url = atoms[start].url.clone();
                    let media = atoms[start].media.clone();
                    let replacement: Vec<_> = spans
                        .into_iter()
                        .map(|span| Atom {
                            span,
                            url: url.clone(),
                            media: media.clone(),
                        })
                        .collect();
                    let next = start + replacement.len();
                    atoms.splice(start..end, replacement);
                    start = next;
                }

                let mut spans = vec![Span::raw(prefix.clone())];
                let mut links: Vec<LinkTarget> = Vec::new();
                let mut media: Vec<crate::tui::media::Target> = Vec::new();
                let mut offset = prefix.len();
                for mut atom in atoms {
                    atom.span.content =
                        super::super::render::safe_multiline_output(&atom.span.content).into();
                    let end = offset + atom.span.content.len();
                    if let Some(source) = atom.media {
                        if let Some(last) = media.last_mut()
                            && last.source == source
                            && last.range.end == offset
                        {
                            last.range.end = end;
                        } else {
                            media.push(crate::tui::media::Target {
                                range: offset..end,
                                source,
                            });
                        }
                    }

                    if let Some(url) = atom.url
                        && offset < end
                    {
                        if let Some(last) = links.last_mut()
                            && last.url == url
                            && last.end == offset
                        {
                            last.end = end;
                        } else {
                            links.push(LinkTarget {
                                start: offset,
                                end,
                                url,
                            });
                        }
                    }

                    offset = end;
                    spans.push(atom.span);
                }

                let line = Line::from(spans);
                for link in super::plain_links(&line.to_string()) {
                    if !links
                        .iter()
                        .any(|target| target.start < link.end && target.end > link.start)
                        && !media.iter().any(|target| {
                            target.range.start < link.end && target.range.end > link.start
                        })
                    {
                        links.push(link);
                    }
                }

                let chrome = line
                    .spans
                    .iter()
                    .any(|span| span.style.fg == Some(THEME.md_rule));
                let decorations = line
                    .spans
                    .iter()
                    .enumerate()
                    .filter_map(|(index, span)| {
                        (span.style.fg == Some(THEME.md_rule)
                            || (span.content == "▎ " && span.style.fg == Some(THEME.md_quote_rail)))
                        .then_some(index)
                    })
                    .collect::<Vec<_>>();
                LinkedLine {
                    line,
                    links,
                    decorations: Vec::new(),
                    chrome,
                    media,
                    ..Default::default()
                }
                .decorate(&decorations)
            })
            .collect()
    }
}
