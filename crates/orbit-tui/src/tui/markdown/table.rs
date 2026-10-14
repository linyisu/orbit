//! Shared Markdown and HTML table layout, with media laid out inside each cell.
use std::sync::Arc;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{LinkedLine, THEME, slice_decorations};
use crate::tui::media;

#[derive(Clone)]
pub(in crate::tui) struct Part {
    table: Arc<Table>,
    first: bool,
    pub(super) leading: Vec<Span<'static>>,
    pub(super) trailing: Vec<Span<'static>>,
    pub(super) strip_margin: bool,
    pub(in crate::tui) style: Style,
    pub(in crate::tui) thinking: bool,
}

struct Table {
    rows: Vec<Vec<Cell>>,
    alignments: Vec<pulldown_cmark::Alignment>,
}

impl Part {
    pub(super) fn strip_body_margin(&mut self) {
        if let Some(first) = self.leading.first_mut()
            && let Some(rest) = first.content.strip_prefix("  ")
        {
            first.content = rest.to_owned().into();
            if first.content.is_empty() {
                self.leading.remove(0);
            }
        } else {
            self.strip_margin = true;
        }
    }

    pub(in crate::tui) fn layout(
        &self,
        context: &media::Context,
        width: u16,
        template: &LinkedLine,
    ) -> Vec<LinkedLine> {
        if !self.first {
            return Vec::new();
        }
        let frame_width = self
            .leading
            .iter()
            .chain(&self.trailing)
            .map(Span::width)
            .sum::<usize>();
        let width = width
            .saturating_sub(frame_width as u16)
            .saturating_add(if self.strip_margin {
                super::BODY_MARGIN
            } else {
                0
            });
        render(
            &self.table.rows,
            &self.table.alignments,
            width,
            Some(context),
        )
        .into_iter()
        .map(|mut line| {
            for span in &mut line.line.spans {
                span.style = self.style.patch(span.style);
                if self.thinking {
                    span.style = line.line.style.patch(span.style);
                    span.style = span
                        .style
                        .fg(crate::tui::theme::desaturate_for_thinking(span.style.fg));
                    if let Some(background) = span.style.bg {
                        span.style = span
                            .style
                            .bg(crate::tui::theme::desaturate_for_thinking(Some(background)));
                    }
                }
            }

            if self.thinking {
                line.line.style = Style::default();
            }
            if self.strip_margin {
                line.strip_body_margin();
            }
            if !self.leading.is_empty() || !self.trailing.is_empty() {
                let style = line.line.style;
                for span in &mut line.line.spans {
                    span.style = style.patch(span.style);
                }
                line = line.framed(self.leading.clone(), self.trailing.clone());
            }

            line.folds
                .extend(template.folds.clone());
            line.anchors
                .extend(template.anchors.clone());
            line
        })
        .collect()
    }
}

pub(super) fn render_table(
    rows: &[Vec<Cell>],
    alignments: &[pulldown_cmark::Alignment],
    width: u16,
) -> Vec<LinkedLine> {
    let mut lines = render(rows, alignments, width, None);
    if rows
        .iter()
        .flatten()
        .flat_map(|cell| &cell.lines)
        .any(|line| !line.media.is_empty() || line.table.is_some())
    {
        let table = Arc::new(Table {
            rows: rows.to_vec(),
            alignments: alignments.to_vec(),
        });

        for (index, line) in lines.iter_mut().enumerate() {
            line.table = Some(Part {
                table: table.clone(),
                first: index == 0,
                leading: Vec::new(),
                trailing: Vec::new(),
                strip_margin: false,
                style: Style::default(),
                thinking: false,
            });
        }
    }

    lines
}

fn render(
    rows: &[Vec<Cell>],
    alignments: &[pulldown_cmark::Alignment],
    width: u16,
    context: Option<&media::Context>,
) -> Vec<LinkedLine> {
    let cols = alignments.len();
    if cols == 0 || rows.is_empty() {
        return Vec::new();
    }
    let available = usize::from(width).saturating_sub(3 * cols + 3);

    // On narrow terminals use labelled cells rather than losing columns/content.
    if available < cols * 3 {
        let lines = labelled_table_rows(rows);
        return match context {
            None => lines,
            Some(context) => lines
                .into_iter()
                .flat_map(|line| {
                    let requests = media::requests(&line, width);
                    media::layout(line, context, width)
                        .into_iter()
                        .flat_map(|line| {
                            if line.graphics.is_empty() && line.media.is_empty() {
                                line.wrap(width)
                            } else {
                                vec![line]
                            }
                        })
                        .map(move |mut line| {
                            line.media_requests
                                .extend(requests.clone());
                            line
                        })
                })
                .collect(),
        };
    }

    let normalized = rows
        .iter()
        .map(|row| {
            let mut row = row.clone();
            let occupied = row
                .iter()
                .map(|cell| cell.colspan)
                .sum::<usize>();
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
            let available = widths[column..column + count]
                .iter()
                .sum::<usize>()
                + 3 * count.saturating_sub(1);
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
        let above = above
            .map(boundaries)
            .unwrap_or_default();
        let below = below
            .map(boundaries)
            .unwrap_or_default();
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
                let count = cell
                    .colspan
                    .min(cols.saturating_sub(column));
                let span_width = widths[column..column + count]
                    .iter()
                    .sum::<usize>()
                    + 3 * count.saturating_sub(1);
                column += count;
                (
                    span_width,
                    cell.lines
                        .iter()
                        .flat_map(|line| {
                            if let Some(context) = context {
                                let mut line = line.clone();
                                crate::tui::render::sanitize_linked_line(&mut line);
                                let requests = media::requests_in_cell(&line, span_width as u16);
                                media::layout_in_cell(line, context, span_width as u16)
                                    .into_iter()
                                    .flat_map(|line| {
                                        if line.line.width() <= span_width {
                                            vec![line]
                                        } else {
                                            wrap_cell(&line, span_width)
                                        }
                                    })
                                    .map(move |mut line| {
                                        line.media_requests
                                            .extend(requests.clone());
                                        line
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                wrap_cell(line, span_width)
                            }
                        })
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
            let mut media = Vec::new();
            let mut graphics = Vec::new();
            let mut copy = Vec::new();
            let mut requests = Vec::new();
            let mut decorations = vec![0];
            let mut byte_offset = spans[0].content.len();

            let mut column = 0;
            for (i, (cell_width, rendered)) in cells.iter().enumerate() {
                let cell = rendered.get(n);
                let definition = &row[i];
                let padding = cell_width.saturating_sub(cell.map_or(0, |cell| cell.line.width()));
                let alignment = definition
                    .alignment
                    .unwrap_or(match alignments[column] {
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
                    let column_offset = spans
                        .iter()
                        .map(Span::width)
                        .sum::<usize>();
                    media.extend(
                        cell.media
                            .iter()
                            .cloned()
                            .map(|mut target| {
                                target.range.start += byte_offset;
                                target.range.end += byte_offset;
                                target
                            }),
                    );
                    graphics.extend(
                        cell.graphics
                            .iter()
                            .cloned()
                            .map(|mut graphic| {
                                graphic.x += column_offset as u16;
                                graphic
                            }),
                    );
                    copy.extend(
                        cell.copy
                            .iter()
                            .cloned()
                            .map(|mut copy| {
                                copy.columns.start += column_offset;
                                copy.columns.end += column_offset;
                                copy
                            }),
                    );
                    requests.extend(cell.media_requests.clone());
                    links.extend(
                        cell.links
                            .iter()
                            .cloned()
                            .map(|mut link| {
                                link.start += byte_offset;
                                link.end += byte_offset;
                                link
                            }),
                    );
                    byte_offset += cell.line.to_string().len();
                    spans.extend(
                        cell.line
                            .spans
                            .iter()
                            .cloned()
                            .map(|mut span| {
                                if definition.header {
                                    span.style = span
                                        .style
                                        .add_modifier(Modifier::BOLD)
                                        .fg(THEME.md_table_header);
                                }

                                span
                            }),
                    );
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
                    media,
                    graphics,
                    copy,
                    media_requests: requests,
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
pub(super) struct Cell {
    pub(super) lines: Vec<LinkedLine>,
    pub(super) colspan: usize,
    pub(super) alignment: Option<ratatui::layout::Alignment>,
    pub(super) header: bool,
}

impl Cell {
    fn heading(&self) -> LinkedLine {
        let mut spans = Vec::new();
        let mut links = Vec::new();
        let mut media = Vec::new();
        let mut offset = 0;
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw(" "));
                offset += 1;
            }

            links.extend(
                line.links
                    .iter()
                    .cloned()
                    .map(|mut link| {
                        link.start += offset;
                        link.end += offset;
                        link
                    }),
            );
            media.extend(
                line.media
                    .iter()
                    .cloned()
                    .map(|mut target| {
                        target.range.start += offset;
                        target.range.end += offset;
                        target
                    }),
            );
            offset += line.line.to_string().len();
            spans.extend(line.line.spans.iter().cloned());
        }

        LinkedLine {
            line: Line::from(spans),
            links,
            media,
            decorations: Vec::new(),
            chrome: false,
            ..Default::default()
        }
    }
}

pub(super) fn labelled_table_rows(rows: &[Vec<Cell>]) -> Vec<LinkedLine> {
    let mut lines = Vec::new();
    let has_header = rows[0].iter().any(|cell| cell.header);
    for row in rows
        .iter()
        .skip(usize::from(has_header))
    {
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
                header.links.extend(
                    heading
                        .links
                        .into_iter()
                        .map(|mut link| {
                            link.start += offset;
                            link.end += offset;
                            link
                        }),
                );
                header.media.extend(
                    heading
                        .media
                        .into_iter()
                        .map(|mut target| {
                            target.range.start += offset;
                            target.range.end += offset;
                            target
                        }),
                );
                header
                    .line
                    .spans
                    .extend(heading.line.spans);
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
                let mut media = line
                    .media
                    .iter()
                    .cloned()
                    .map(|mut target| {
                        target.range.start += offset;
                        target.range.end += offset;
                        target
                    })
                    .collect::<Vec<_>>();

                if index == 0 {
                    links.extend(
                        header
                            .links
                            .iter()
                            .cloned()
                            .map(|mut link| {
                                link.start += 2;
                                link.end += 2;
                                link
                            }),
                    );
                    media.extend(
                        header
                            .media
                            .iter()
                            .cloned()
                            .map(|mut target| {
                                target.range.start += 2;
                                target.range.end += 2;
                                target
                            }),
                    );
                }

                media.sort_by_key(|target| target.range.start);
                let mut spans = vec![Span::styled(
                    prefix,
                    Style::default().fg(THEME.md_table_header),
                )];
                spans.extend(line.line.spans.iter().cloned());
                lines.push(LinkedLine {
                    line: Line::from(spans),
                    links,
                    media,
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
            let media = cell
                .media
                .iter()
                .filter_map(|target| {
                    let start = target.range.start.max(offset);
                    let finish = target.range.end.min(end);
                    (start < finish).then(|| media::Target {
                        range: start - offset..finish - offset,
                        source: target.source.clone(),
                    })
                })
                .collect();

            offset = end;
            LinkedLine {
                line,
                links,
                media,
                decorations,
                chrome: cell.chrome,
                folds: cell.folds.clone(),
                disclosure: cell.disclosure.clone(),
                ..Default::default()
            }
        })
        .collect()
}
