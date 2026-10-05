//! Cell layout for mixed prose and pictures. Graphics are indivisible inline
//! boxes; normal documents still use the cached Unicode line breaker.
use std::ops::Range;
use std::sync::Arc;

use ratatui::text::{Line, Span};

use super::{Context, CopyText, Fragment, Prepared, Request, Source};
use crate::tui::markdown::{LinkTarget, LinkedLine, slice_decorations};

pub(in crate::tui) fn layout(linked: LinkedLine, context: &Context, width: u16) -> Vec<LinkedLine> {
    layout_with_margin(linked, context, width, super::super::markdown::BODY_MARGIN)
}

pub(in crate::tui) fn layout_in_cell(
    linked: LinkedLine,
    context: &Context,
    width: u16,
) -> Vec<LinkedLine> {
    layout_with_margin(linked, context, width, 0)
}

fn layout_with_margin(
    linked: LinkedLine,
    context: &Context,
    width: u16,
    margin: u16,
) -> Vec<LinkedLine> {
    if context.enabled()
        && let Some(table) = &linked.table
    {
        return table.layout(context, width, &linked);
    }

    if linked.media.is_empty() || !context.enabled() || width < 4 {
        return plain_rows(linked, width);
    }
    let source = linked.text();
    let bounds = Bounds::new(&linked, width, margin);
    let prefix = slice(&linked, 0..bounds.prefix);
    let ready = linked
        .media
        .iter()
        .map(|target| {
            context.cached_picture(&target.source, bounds.available, height(&target.source))
        })
        .collect::<Vec<_>>();
    if ready.iter().all(Option::is_none) {
        return plain_rows(linked, width);
    }
    let mut flow = Flow {
        margin,
        width: width.saturating_sub(bounds.suffix.as_ref().map_or(0, |s| s.width() as u16)),
        suffix: bounds.suffix,
        template: linked.clone(),
        prefix,
        current: LinkedLine::default(),
        bytes: 0,
        columns: 0,
        has_content: false,
        boxes: Vec::new(),
        output: Vec::new(),
    };

    flow.reset();
    let mut start = bounds.prefix;
    for (target, ready) in linked.media.iter().zip(ready) {
        if target.range.start < start || target.range.end > bounds.end {
            continue;
        }
        flow.text(slice(&linked, start..target.range.start));
        if let Some(ready) = ready {
            let block = matches!(
                target.source,
                Source::Image(_) | Source::Math { display: true, .. }
            );
            let copy = match &target.source {
                Source::Math { tex, display } => {
                    let delimiter = if *display { "$$" } else { "$" };
                    format!("{delimiter}{tex}{delimiter}")
                }

                Source::Image(_) => source[target.range.clone()].to_owned(),
            };
            let url = linked
                .links
                .iter()
                .find(|link| link.start <= target.range.start && link.end >= target.range.end)
                .map(|link| link.url.clone());

            flow.picture(ready, copy, block, url);
        } else {
            flow.text(slice(&linked, target.range.clone()));
        }

        start = target.range.end;
    }

    flow.text(slice(&linked, start..bounds.end));
    flow.flush();
    flow.output
}

fn plain_rows(linked: LinkedLine, width: u16) -> Vec<LinkedLine> {
    let source = linked.text();
    if !source.contains('\n') {
        return vec![linked];
    }
    let bounds = Bounds::new(&linked, width, super::super::markdown::BODY_MARGIN);
    let prefix = slice(&linked, 0..bounds.prefix);
    let mut start = bounds.prefix;
    source[bounds.prefix..bounds.end]
        .split('\n')
        .map(|text| {
            let mut row = slice(&linked, start..start + text.len());
            row.anchors = linked.anchors.clone();
            row = row.framed(prefix.line.spans.clone(), Vec::new());
            if let Some(suffix) = &bounds.suffix {
                let padding = usize::from(width).saturating_sub(row.line.width() + suffix.width());
                row = row.framed(
                    Vec::new(),
                    vec![Span::raw(" ".repeat(padding)), suffix.clone()],
                );
            }

            start += text.len() + 1;
            row
        })
        .collect()
}

fn height(source: &Source) -> u16 {
    if matches!(source, Source::Image(_)) {
        12
    } else {
        10
    }
}

pub(in crate::tui) fn requests(linked: &LinkedLine, width: u16) -> Vec<Request> {
    requests_with_margin(linked, width, super::super::markdown::BODY_MARGIN)
}

pub(in crate::tui) fn requests_in_cell(linked: &LinkedLine, width: u16) -> Vec<Request> {
    requests_with_margin(linked, width, 0)
}

fn requests_with_margin(linked: &LinkedLine, width: u16, margin: u16) -> Vec<Request> {
    if linked.media.is_empty() {
        return Vec::new();
    }
    let width = Bounds::new(linked, width, margin).available;
    linked
        .media
        .iter()
        .map(|t| Request {
            source: t.source.clone(),
            width,
            height: height(&t.source),
        })
        .collect()
}

struct Bounds {
    prefix: usize,
    suffix: Option<Span<'static>>,
    end: usize,
    available: u16,
}

impl Bounds {
    fn new(linked: &LinkedLine, width: u16, margin: u16) -> Self {
        let prefix = linked
            .line
            .spans
            .iter()
            .take_while(|s| {
                s.content.chars().all(char::is_whitespace)
                    || matches!(s.content.as_ref(), "┊ " | "│ " | "▎ ")
            })
            .collect::<Vec<_>>();
        let bytes = linked
            .line
            .spans
            .iter()
            .map(|s| s.content.len())
            .sum::<usize>();
        let suffix = linked
            .line
            .spans
            .last()
            .filter(|s| {
                matches!(s.content.as_ref(), " │" | "│")
                    && linked.decorations.iter().any(|r| r.end == bytes)
            })
            .cloned();
        let padding = if suffix.is_some() {
            linked
                .line
                .spans
                .iter()
                .rev()
                .take_while(|s| {
                    s.content.chars().all(char::is_whitespace)
                        || matches!(s.content.as_ref(), " │" | "│")
                })
                .map(|s| s.content.len())
                .sum()
        } else {
            0
        };
        Self {
            prefix: prefix.iter().map(|s| s.content.len()).sum(),
            end: bytes.saturating_sub(padding),
            available: width
                .saturating_sub(
                    (prefix.iter().map(|s| s.width()).sum::<usize>()
                        + suffix.as_ref().map_or(0, Span::width)) as u16
                        + margin,
                )
                .min(80),
            suffix,
        }
    }
}

fn slice(linked: &LinkedLine, range: Range<usize>) -> LinkedLine {
    let mut result = LinkedLine {
        folds: linked.folds.clone(),
        disclosure: linked.disclosure.clone(),
        anchors: linked.anchors.clone(),
        chrome: linked.chrome,
        ..Default::default()
    };

    result.line.style = linked.line.style;
    let mut offset = 0;
    for span in &linked.line.spans {
        let end = offset + span.content.len();
        let a = range.start.max(offset);
        let b = range.end.min(end);
        if a < b {
            result.line.spans.push(Span::styled(
                span.content[a - offset..b - offset].to_owned(),
                span.style,
            ));
        }

        offset = end;
    }

    result.links = linked
        .links
        .iter()
        .filter_map(|l| l.slice(&range, 0))
        .collect();
    result.media = linked
        .media
        .iter()
        .filter_map(|t| {
            let start = t.range.start.max(range.start);
            let end = t.range.end.min(range.end);
            (start < end).then(|| super::Target {
                range: start - range.start..end - range.start,
                source: t.source.clone(),
            })
        })
        .collect();
    result.decorations = slice_decorations(&linked.decorations, range, 0);
    result
}

struct BoxAt {
    x: u16,
    picture: Arc<Prepared>,
    text: Arc<str>,
    url: Option<String>,
}

struct Flow {
    margin: u16,
    width: u16,
    suffix: Option<Span<'static>>,
    template: LinkedLine,
    prefix: LinkedLine,
    current: LinkedLine,
    bytes: usize,
    columns: u16,
    has_content: bool,
    boxes: Vec<BoxAt>,
    output: Vec<LinkedLine>,
}

impl Flow {
    fn reset(&mut self) {
        self.current = self.prefix.clone();
        self.current.folds = self.template.folds.clone();
        self.current.disclosure = self.template.disclosure.clone();
        self.current.line.style = self.template.line.style;
        self.bytes = self.prefix.line.spans.iter().map(|s| s.content.len()).sum();
        self.columns = self.prefix.line.width() as u16;
        self.has_content = false;
    }

    fn append(&mut self, mut part: LinkedLine) {
        self.has_content |= !part.line.spans.is_empty();
        part.shift_offsets(self.bytes as isize);
        self.bytes += part
            .line
            .spans
            .iter()
            .map(|s| s.content.len())
            .sum::<usize>();
        self.columns += part.line.width() as u16;
        self.current.line.spans.extend(part.line.spans);
        self.current.links.extend(part.links);
        self.current.decorations.extend(part.decorations);
        self.current.media.extend(part.media);
    }

    fn text(&mut self, part: LinkedLine) {
        let source = part.text();
        let mut start = 0;
        for chunk in source.split_inclusive('\n') {
            let line = chunk.strip_suffix('\n').unwrap_or(chunk);
            self.text_row(slice(&part, start..start + line.len()));
            if chunk.ends_with('\n') {
                self.flush();
            }
            start += chunk.len();
        }
    }

    fn text_row(&mut self, part: LinkedLine) {
        let source = part.text();
        if self.columns >= self.width {
            self.flush();
        }
        let ranges = crate::tui::transcript::wrap_ranges_from(
            &source,
            self.width.saturating_sub(self.prefix.line.width() as u16),
            self.width.saturating_sub(self.columns),
        );

        for (i, range) in ranges.into_iter().enumerate() {
            if i > 0 {
                self.flush();
            }
            self.append(slice(&part, range));
        }
    }

    fn picture(&mut self, picture: Arc<Prepared>, text: String, block: bool, url: Option<String>) {
        if block || self.columns + picture.size.width > self.width {
            self.flush();
        }
        if block {
            let spare = self
                .width
                .saturating_sub(self.columns + self.margin + picture.size.width);
            self.append(LinkedLine::chrome(Line::from(Span::raw(
                " ".repeat(usize::from(spare / 2)),
            ))));
        }

        let x = self.columns;
        self.current
            .line
            .spans
            .push(Span::raw(" ".repeat(picture.size.width as usize)));
        if let Some(url) = &url {
            self.current.links.push(LinkTarget {
                start: self.bytes,
                end: self.bytes + usize::from(picture.size.width),
                url: url.clone(),
            });
        }

        self.bytes += usize::from(picture.size.width);
        self.columns += picture.size.width;
        self.has_content = true;
        self.boxes.push(BoxAt {
            x,
            picture,
            text: text.into(),
            url,
        });
        if block {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if !self.has_content {
            self.reset();
            return;
        }

        let baseline = self
            .boxes
            .iter()
            .map(|b| b.picture.baseline)
            .max()
            .unwrap_or(0);
        let depth = self
            .boxes
            .iter()
            .map(|b| b.picture.size.height.saturating_sub(b.picture.baseline + 1))
            .max()
            .unwrap_or(0);
        let height = baseline + depth + 1;
        for y in 0..height {
            let mut line = if y == baseline {
                self.current.clone()
            } else {
                let mut line = self.prefix.clone();
                line.line.spans.push(Span::raw(
                    " ".repeat(
                        self.current
                            .line
                            .width()
                            .saturating_sub(self.prefix.line.width()),
                    ),
                ));
                line.chrome = true;
                line.folds = self.template.folds.clone();
                line.disclosure = None;
                line
            };

            for b in &self.boxes {
                let top = baseline - b.picture.baseline;
                if y >= top && y < top + b.picture.size.height {
                    line.graphics.push(Fragment {
                        picture: b.picture.clone(),
                        x: b.x,
                        row: y - top,
                    });
                    line.copy.push(CopyText {
                        columns: usize::from(b.x)..usize::from(b.x + b.picture.size.width),
                        row: y - top,
                        height: b.picture.size.height,
                        baseline: b.picture.baseline,
                        text: b.text.clone(),
                    });
                    if y != baseline
                        && let Some(url) = &b.url
                    {
                        let start = self.prefix.text().len()
                            + usize::from(b.x).saturating_sub(self.prefix.line.width());
                        line.links.push(LinkTarget {
                            start,
                            end: start + usize::from(b.picture.size.width),
                            url: url.clone(),
                        });
                    }
                }
            }

            if let Some(suffix) = &self.suffix {
                let padding = usize::from(self.width).saturating_sub(line.line.width());
                line = line.framed(
                    Vec::new(),
                    vec![Span::raw(" ".repeat(padding)), suffix.clone()],
                );
            }

            self.output.push(line);
        }

        self.boxes.clear();
        self.reset();
    }
}
