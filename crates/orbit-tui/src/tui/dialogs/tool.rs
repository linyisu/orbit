//! Terminal interaction and layout for shared tool evidence.
use super::*;
use orbit_client::execution::{ToolRun, details};
use std::time::{Duration, Instant};

const COPY_FEEDBACK: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    Parameters,
    Result,
}

#[derive(Debug)]
pub(super) struct Header {
    record: ToolRun,
    retained: details::retained::Retained,
    pending: Option<
        tokio_util::task::AbortOnDropHandle<(String, Result<details::retained::Contents, String>)>,
    >,
    request: details::Request,
    status: String,
    streams: &'static [details::Stream],
    manual_stream: bool,
    active: Pane,
    area: Rect,
    scroll: usize,
    max_scroll: usize,
    copy_parameters: Rect,
    copy_result: Rect,
    copied: Option<(Pane, Instant)>,
}

impl Header {
    pub(super) fn content(&self, index: usize) -> String {
        self.retained
            .content(&self.record, self.streams[index])
    }

    pub(super) fn select_result(&mut self) {
        self.active = Pane::Result;
        self.manual_stream = true;
        self.copied = None;
    }

    fn icon(&self, pane: Pane) -> &'static str {
        if self
            .copied
            .is_some_and(|(copied, _)| copied == pane)
        {
            icons::CHECK
        } else {
            icons::CLIPBOARD_TEXT_OUTLINE
        }
    }
}

impl InfoDialog {
    pub(crate) fn tool(record: &ToolRun, workspace: &str) -> Self {
        let mut dialog = Self::new(&record.name, String::new());
        dialog.source = Some(ReportSource::Tool(record.id.clone()));
        dialog.tool_header = Some(Header {
            record: record.clone(),
            retained: details::retained::Retained::default(),
            pending: None,
            request: details::Request::new(record, workspace),
            status: String::new(),
            streams: &[],
            manual_stream: false,
            active: Pane::Result,
            area: Rect::default(),
            scroll: 0,
            max_scroll: 0,
            copy_parameters: Rect::default(),
            copy_result: Rect::default(),
            copied: None,
        });
        dialog.update_tool(record);
        dialog
    }

    pub(crate) fn update_tool(&mut self, record: &ToolRun) {
        let Some(header) = &mut self.tool_header else {
            return;
        };
        let previous = header
            .streams
            .get(self.selected_page)
            .copied();
        let follow_log = record.name == "Exec"
            && matches!(
                header.record.status,
                orbit_client::execution::ToolStatus::Running
                    | orbit_client::execution::ToolStatus::Background
            )
            && self.scroll >= self.max_scroll;
        header.record = record.clone();
        if !record.changes.is_empty() {
            header.request = details::Request::new(record, "");
            header.active = Pane::Result;
        }
        header.streams = details::streams(record);
        header.status = details::status(record);
        let selected = previous
            .filter(|stream| header.manual_stream && header.streams.contains(stream))
            .unwrap_or_else(|| details::default_stream(record));
        self.selected_page = header
            .streams
            .iter()
            .position(|stream| *stream == selected)
            .unwrap_or(0);
        if previous != Some(selected) {
            self.scroll = 0;
            header.copied = None;
        }
        if follow_log {
            self.scroll = usize::MAX;
        }
        let streams = header.streams;
        self.title = record.name.clone();
        self.target = details::target(record);
        self.content = header
            .retained
            .content(record, selected);
        self.pages = streams
            .iter()
            .map(|stream| ReportPage {
                label: stream.label().into(),
                content: Text::from(header.retained.content(record, *stream)),
            })
            .collect();
    }

    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        let Some(header) = &mut self.tool_header else {
            return false;
        };
        if header
            .copied
            .is_some_and(|(_, since)| now.saturating_duration_since(since) >= COPY_FEEDBACK)
        {
            header.copied = None;
            true
        } else {
            false
        }
    }

    pub(crate) async fn poll_output(
        &mut self,
        connection: &std::sync::Arc<orbit_client::transport::Connection>,
        session: &str,
        agent: Option<String>,
    ) -> bool {
        let Some(header) = &mut self.tool_header else {
            return false;
        };
        let mut changed = false;
        if header.retained.begin(&header.record) {
            let record = header.record.clone();
            let connection = connection.clone();
            let session = session.to_owned();
            let (revision, previous) = header.retained.pending().unwrap();
            header.pending = Some(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
                async move {
                    let result =
                        details::retained::fetch(&connection, session, agent, &record, previous)
                            .await
                            .map_err(|error| format!("{error:#}"));
                    (revision, result)
                },
            )));
            changed = true;
        }
        if header
            .pending
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let result = header.pending.take().unwrap().await;
            let (output, result) = result.unwrap_or_else(|error| {
                (
                    header
                        .record
                        .output
                        .clone()
                        .unwrap_or_default(),
                    Err(error.to_string()),
                )
            });
            changed |= header.retained.receive(&output, result);
        }
        if changed {
            let record = header.record.clone();
            self.update_tool(&record);
        }
        changed
    }

    pub(super) fn copy_request(&mut self, event: &Event) -> Option<&str> {
        let header = self.tool_header.as_mut()?;
        match event {
            Event::Key(key)
                if key.kind != KeyEventKind::Release && key.code == KeyCode::Char('y') => {}
            Event::Mouse(mouse)
                if mouse.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left) =>
            {
                let point = (mouse.column, mouse.row).into();
                header.active = if header.copy_parameters.contains(point) {
                    Pane::Parameters
                } else if header.copy_result.contains(point) {
                    Pane::Result
                } else {
                    return None;
                };
            }
            _ => return None,
        }
        Some(match header.active {
            Pane::Parameters => header.request.content(),
            Pane::Result => &self.content,
        })
    }

    pub(super) fn copied(&mut self, now: Instant) {
        if let Some(header) = &mut self.tool_header {
            header.copied = Some((header.active, now));
        }
    }

    pub(super) fn handle_parameters(&mut self, event: &Event) -> bool {
        let Some(header) = &mut self.tool_header else {
            return false;
        };
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if matches!(key.code, KeyCode::Char('r' | 'R') | KeyCode::F(5)) {
                    header.pending = None;
                    header.retained = details::retained::Retained::default();
                    return true;
                }
                if key.code == KeyCode::Char('p')
                    && header.request.layout() == details::RequestLayout::Body
                {
                    header.active = if header.active == Pane::Parameters {
                        Pane::Result
                    } else {
                        Pane::Parameters
                    };
                    return true;
                }
                if header.active != Pane::Parameters {
                    return false;
                }
                match key.code {
                    KeyCode::Up => header.scroll = header.scroll.saturating_sub(1),
                    KeyCode::Down => header.scroll = header.scroll.saturating_add(1),
                    KeyCode::PageUp => {
                        header.scroll = header
                            .scroll
                            .saturating_sub(header.area.height as usize)
                    }
                    KeyCode::PageDown => {
                        header.scroll = header
                            .scroll
                            .saturating_add(header.area.height as usize)
                    }
                    KeyCode::Home => header.scroll = 0,
                    KeyCode::End => header.scroll = header.max_scroll,
                    _ => return false,
                }
            }
            Event::Mouse(mouse) => {
                if !header
                    .area
                    .contains((mouse.column, mouse.row).into())
                {
                    if matches!(
                        mouse.kind,
                        MouseEventKind::Down(_)
                            | MouseEventKind::ScrollUp
                            | MouseEventKind::ScrollDown
                    ) {
                        header.active = Pane::Result;
                    }
                    return false;
                }
                header.active = Pane::Parameters;
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        header.scroll = header
                            .scroll
                            .saturating_sub(super::super::wheel::LINES)
                    }
                    MouseEventKind::ScrollDown => {
                        header.scroll = header
                            .scroll
                            .saturating_add(super::super::wheel::LINES)
                    }
                    _ => {}
                }
            }
            _ => return false,
        }
        header.scroll = header.scroll.min(header.max_scroll);
        true
    }

    pub(super) fn draw_tool(&mut self, frame: &mut Frame<'_>, inner: Rect) -> bool {
        let Some(header) = &mut self.tool_header else {
            return false;
        };
        use ratatui::layout::{Constraint, Layout};
        let area = inner.inner(ratatui::layout::Margin::new(1, 0));
        let request_body = header.request.layout() == details::RequestLayout::Body;
        let hidden = header.request.layout() == details::RequestLayout::Hidden;
        let [title, label, parameters, context, tabs, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(u16::from(!hidden)),
            Constraint::Length(if request_body {
                (area.height / 4).clamp(1, 5)
            } else {
                0
            }),
            Constraint::Length(u16::from(!hidden && !header.request.context().is_empty())),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        frame.render_widget(
            Paragraph::new(super::super::render::safe_output(&self.title))
                .style(super::super::components::accent()),
            title,
        );
        let active = if header.active == Pane::Parameters {
            " ◂"
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(format!(
                "{}{active}  {}",
                if header.request.layout() == details::RequestLayout::Summary {
                    header.request.content()
                } else {
                    header.request.heading()
                },
                header.status
            ))
            .style(super::super::components::muted()),
            label,
        );
        header.copy_parameters = Rect::new(
            parameters
                .right()
                .saturating_sub(3)
                .max(parameters.x),
            parameters.y,
            parameters
                .width
                .saturating_sub(1)
                .min(2),
            parameters.height.min(1),
        );
        header.area = parameters;
        let parameter_text = super::super::render::safe_multiline_output(header.request.content());
        header.max_scroll = draw_content(
            frame,
            parameters,
            &parameter_text,
            &mut header.scroll,
            false,
        );
        frame.render_widget(
            Paragraph::new(header.icon(Pane::Parameters)).style(super::super::components::accent()),
            header.copy_parameters,
        );
        frame.render_widget(
            Paragraph::new(super::super::render::fit_label(
                &super::super::render::safe_output(header.request.context()),
                context.width as usize,
                false,
            ))
            .style(super::super::components::muted()),
            context,
        );
        self.page_areas.clear();
        let mut x = tabs.x;
        for (index, page) in self.pages.iter().enumerate() {
            let label = format!(" {} ", page.label);
            let width = (unicode_width::UnicodeWidthStr::width(label.as_str()) as u16).min(
                tabs.right()
                    .saturating_sub(2)
                    .saturating_sub(x),
            );
            let area = Rect::new(x, tabs.y, width, tabs.height);
            frame.render_widget(
                Paragraph::new(label).style(if index == self.selected_page {
                    super::super::components::highlighted()
                } else {
                    super::super::components::muted()
                }),
                area,
            );
            self.page_areas.push((area, index));
            x = x
                .saturating_add(width + 1)
                .min(tabs.right());
        }
        header.copy_result = Rect::new(
            body.right()
                .saturating_sub(3)
                .max(body.x),
            body.y,
            body.width.saturating_sub(1).min(2),
            body.height.min(1),
        );
        self.page_rows = usize::from(body.height).max(1);
        self.max_scroll = draw_content(
            frame,
            body,
            &super::super::render::safe_multiline_output(&self.content),
            &mut self.scroll,
            header.streams[self.selected_page] == details::Stream::Changes,
        );
        frame.render_widget(
            Paragraph::new(header.icon(Pane::Result)).style(super::super::components::accent()),
            header.copy_result,
        );
        let mut actions = "Tab 切页 · Y ⧉ · Alt+←/→ 切换调用 · Esc 关闭".to_owned();
        if request_body {
            actions = format!("P 调用/结果 · {actions}");
        }
        if let Some(target) = &self.target {
            actions = format!("A {} · {actions}", target.label());
        }
        frame.render_widget(
            Paragraph::new(super::super::render::fit_label(
                &actions,
                footer.width as usize,
                false,
            ))
            .style(super::super::components::muted()),
            footer,
        );
        true
    }
}

fn draw_content(
    frame: &mut Frame<'_>,
    area: Rect,
    content: &str,
    scroll: &mut usize,
    diff: bool,
) -> usize {
    let body = Rect::new(area.x, area.y, area.width.saturating_sub(4), area.height);
    let paragraph = Paragraph::new(content).wrap(Wrap { trim: false });
    let max_scroll = paragraph
        .line_count(body.width)
        .saturating_sub(body.height as usize);
    *scroll = (*scroll).min(max_scroll);
    // Paragraph's scroll offset is u16. Advance whole logical lines first so
    // large retained logs can still reach their final wrapped line.
    let mut remaining = *scroll;
    let mut visible = content;
    if remaining > u16::MAX as usize && body.width > 0 {
        for line in content.split_inclusive('\n') {
            let rows = Paragraph::new(line.trim_end_matches('\n'))
                .wrap(Wrap { trim: false })
                .line_count(body.width)
                .max(1);
            if rows > remaining {
                break;
            }
            remaining -= rows;
            visible = &visible[line.len()..];
            if remaining <= u16::MAX as usize {
                break;
            }
        }
    }
    frame.render_widget(
        Paragraph::new(Text::from(
            visible
                .lines()
                .map(|line| {
                    let style = if diff && line.starts_with('+') && !line.starts_with("+++") {
                        Style::default()
                            .fg(super::super::theme::THEME.diff_added)
                            .bg(super::super::theme::THEME.diff_added_bg)
                    } else if diff && line.starts_with('-') && !line.starts_with("---") {
                        Style::default()
                            .fg(super::super::theme::THEME.diff_removed)
                            .bg(super::super::theme::THEME.diff_removed_bg)
                    } else {
                        Style::default()
                    };
                    Line::styled(line.to_owned(), style)
                })
                .collect::<Vec<_>>(),
        ))
        .wrap(Wrap { trim: false })
        .scroll((remaining.min(u16::MAX as usize) as u16, 0)),
        body,
    );
    if max_scroll > 0 && !area.is_empty() {
        let mut scrollbar = ScrollbarState::new(max_scroll + 1)
            .position(*scroll)
            .viewport_content_length(body.height as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(super::super::components::accent())
                .track_style(super::super::components::muted()),
            Rect::new(area.right().saturating_sub(1), area.y, 1, area.height),
            &mut scrollbar,
        );
    }
    max_scroll
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use orbit_client::{execution::ToolStatus, protocol::model::ToolCall};
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    fn record() -> ToolRun {
        let mut record = ToolRun::new(&ToolCall::function(
            "generated",
            "Exec",
            json!({
                "command": (0..30).map(|line| format!("echo sample-{line}\n")).collect::<String>()
            }),
        ));
        record.set_output(
            ToolStatus::Complete,
            json!({
                "exit_code": 0, "elapsed": {"secs": 1, "nanos": 0},
                "stdout": "first\n".repeat(100), "stderr": "error\n",
                "stdout_total_bytes": 600, "stderr_total_bytes": 6,
                "timed_out": false, "cancelled": false, "output_truncated": false
            })
            .to_string(),
        );
        record
    }

    fn draw(dialog: &mut InfoDialog, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                dialog.draw_tool(frame, frame.area());
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn equal_result_viewports_keep_parameter_scroll_and_copy_feedback_separate() {
        let mut dialog = InfoDialog::tool(&record(), "/generated");
        let text = draw(&mut dialog, 90, 30);
        assert!(!text.contains("已返回") && !text.contains("复制") && !text.contains("仍有后续"));
        assert!(!text.contains("参数") && !text.contains("command:"));
        let copy = dialog
            .tool_header
            .as_ref()
            .unwrap()
            .copy_result;
        assert!(copy.y > dialog.page_areas[0].0.y);
        let rows = dialog.page_rows;
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        dialog.handle(&key(KeyCode::PageDown));
        let result_scroll = dialog.scroll;
        draw(&mut dialog, 90, 30);
        assert_eq!(
            dialog
                .tool_header
                .as_ref()
                .unwrap()
                .copy_result,
            copy
        );
        dialog.handle(&key(KeyCode::Char('p')));
        dialog.handle(&key(KeyCode::PageDown));
        assert_eq!(dialog.scroll, result_scroll);
        assert!(
            dialog
                .tool_header
                .as_ref()
                .unwrap()
                .scroll
                > 0
        );
        assert!(
            dialog
                .copy_request(&key(KeyCode::Char('y')))
                .unwrap()
                .contains("echo sample-29")
        );
        let now = Instant::now();
        dialog.copied(now);
        assert_eq!(
            dialog
                .tool_header
                .as_ref()
                .unwrap()
                .icon(Pane::Parameters),
            icons::CHECK
        );
        assert!(!dialog.tick(now + Duration::from_secs(1)));
        assert!(dialog.tick(now + COPY_FEEDBACK));
        dialog.handle(&key(KeyCode::Tab));
        draw(&mut dialog, 90, 30);
        assert_eq!(dialog.page_rows, rows);
        assert_eq!(
            dialog.copy_request(&key(KeyCode::Char('y'))),
            Some("error\n")
        );
        for (width, height) in [(40, 12), (16, 5), (1, 1), (140, 40)] {
            draw(&mut dialog, width, height);
        }
    }

    #[test]
    fn live_completion_preserves_selected_page_by_identity() {
        let mut record = record();
        record.status = ToolStatus::Running;
        record.output = None;
        let mut dialog = InfoDialog::tool(&record, "");
        dialog.select_page(1);
        record = self::record();
        dialog.update_tool(&record);
        assert_eq!(
            dialog.pages[dialog.selected_page].label,
            details::Stream::Stderr.label()
        );
        assert_eq!(dialog.content, "error\n");
    }

    #[test]
    fn retained_log_can_scroll_past_the_paragraph_offset_limit() {
        let mut dialog = InfoDialog::tool(&record(), "");
        dialog.content = "generated line\n".repeat(70_000) + "final generated line";
        dialog.scroll = usize::MAX;
        let rendered = draw(&mut dialog, 90, 30);
        assert!(dialog.scroll > u16::MAX as usize);
        assert!(rendered.contains("final generated line"));
    }
}
