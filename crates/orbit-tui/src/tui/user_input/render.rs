use super::*;
use unicode_width::UnicodeWidthStr;

fn selected() -> Style {
    Style::default()
        .fg(THEME.on_accent)
        .bg(THEME.accent)
        .add_modifier(Modifier::BOLD)
}
fn normal() -> Style {
    Style::default().fg(THEME.foreground)
}
fn muted() -> Style {
    Style::default().fg(THEME.secondary)
}
fn accent() -> Style {
    Style::default().fg(THEME.accent)
}
fn choice_mark(kind: Kind, chosen: bool) -> &'static str {
    if kind == Kind::Multiple {
        crate::tui::icons::checkbox(chosen)
    } else if chosen {
        crate::tui::icons::CHECK
    } else {
        " "
    }
}
fn status(status: Status) -> &'static str {
    match status {
        Status::Pending => "待回答",
        Status::Answered => "已回答",
        Status::Declined => "已取消问答",
        Status::Cancelled => "任务已取消",
        Status::Interrupted => "任务已中断",
    }
}

struct Row {
    line: Line<'static>,
    focus: Option<Focus>,
}
fn push(rows: &mut Vec<Row>, text: String, style: Style, focus: Option<Focus>, width: u16) {
    // Sanitize model-controlled text before terminal rendering.
    for text in text.split('\n') {
        let line = Line::styled(crate::tui::render::safe_output(text), style);
        for line in crate::tui::transcript::wrap_line(&line, width.max(1)).0 {
            rows.push(Row { line, focus });
        }
    }
}
impl State {
    pub fn draw_badge(&mut self, frame: &mut Frame<'_>) {
        self.badge = Rect::default();
        if self.pending.is_empty() {
            return;
        }
        let area = frame.area();
        if area.height == 0 {
            return;
        }
        let text = format!("待回答 · {} 组问题  查看", self.pending.len());
        self.badge = Rect::new(
            area.x,
            area.bottom() - 1,
            (text.width() as u16).min(area.width.saturating_sub(12)),
            1,
        );
        frame.render_widget(
            Paragraph::new(text).style(accent().add_modifier(Modifier::BOLD)),
            self.badge,
        );
    }
    pub fn draw(&mut self, frame: &mut Frame<'_>) -> Option<Position> {
        self.hits.clear();
        self.editor_area = Rect::default();
        self.editor_rows.clear();
        let dialog = self.dialog.as_ref()?;
        let desired = match dialog {
            Dialog::History { .. } => (self.records.len() + 8).clamp(12, 25),
            Dialog::Form(_) => 25,
        };
        let area = crate::tui::dialogs::centered_size(frame.area(), 92, desired as u16);
        crate::tui::overlay::clear(frame, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(accent());
        let mut inner = block.inner(area);
        frame.render_widget(block, area);
        inner.x = inner.x.saturating_add(1);
        inner.width = inner.width.saturating_sub(2);
        if inner.height < 8 || inner.width < 16 {
            frame.render_widget(
                Paragraph::new("请扩大窗口 · Esc 收起").style(muted()),
                inner,
            );
            return None;
        }
        if let Dialog::History { cursor, offset } = &mut self.dialog.as_mut().unwrap() {
            frame.render_widget(
                Paragraph::new("问答记录").style(accent().add_modifier(Modifier::BOLD)),
                Rect::new(inner.x, inner.y, inner.width, 1),
            );
            let height = inner.height.saturating_sub(5) as usize;
            *cursor = (*cursor).min(self.records.len().saturating_sub(1));
            *offset = (*offset).min(self.records.len().saturating_sub(height));
            if *cursor >= *offset + height {
                *offset = cursor.saturating_sub(height - 1);
            }
            for (row, (i, r)) in self
                .records
                .iter()
                .enumerate()
                .skip(*offset)
                .take(height)
                .enumerate()
            {
                let rect = Rect::new(inner.x, inner.y + 2 + row as u16, inner.width, 1);
                let title = r
                    .form
                    .questions
                    .iter()
                    .map(|q| q.title.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ");
                let label = format!("{}  {}", status(r.status), title);
                frame.render_widget(
                    Paragraph::new(label).style(if i == *cursor { selected() } else { normal() }),
                    rect,
                );
                self.hits.push((rect, Focus::History(i)));
            }
            if self.records.is_empty() {
                frame.render_widget(
                    Paragraph::new("当前会话还没有问答记录").style(muted()),
                    Rect::new(inner.x, inner.y + 2, inner.width, 1),
                );
            }
            if self.next.is_some() {
                let rect = Rect::new(inner.x, inner.bottom() - 3, inner.width, 1);
                frame.render_widget(Paragraph::new("加载更早的问答  N").style(accent()), rect);
                self.hits.push((rect, Focus::More));
            }
            frame.render_widget(
                Paragraph::new("↑↓ 选择 · Enter 查看 · Esc 返回").style(muted()),
                Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            );
            return None;
        }
        let Some(Dialog::Form(v)) = &mut self.dialog else {
            return None;
        };
        let highlight = if self.pointer_active {
            self.hovered
        } else {
            Some(v.focus)
        };
        let r = self.records.iter().find(|r| r.id == v.id)?;
        let editable = r.status == Status::Pending && self.pending.contains(&v.id);
        let count = r.form.questions.len();
        let page = v.page.min(count);
        v.page = page;
        let title = if editable {
            format!(
                "需要你回答 · {}",
                if page < count {
                    format!("{}/{}", page + 1, count)
                } else {
                    "检查答案".into()
                }
            )
        } else {
            format!("问答记录 · {}", status(r.status))
        };
        frame.render_widget(
            Paragraph::new(title).style(accent().add_modifier(Modifier::BOLD)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        if count > 1 {
            let labels = r
                .form
                .questions
                .iter()
                .map(|q| q.title.as_str())
                .chain(std::iter::once("检查答案"))
                .enumerate()
                .map(|(i, label)| {
                    let answered = i < count && {
                        let d = &self.drafts[&r.id][i];
                        !d.selected.is_empty() || !d.text.text.trim().is_empty()
                    };
                    crate::tui::render::fit_label(
                        &format!(
                            " {}{} ",
                            if answered {
                                crate::tui::icons::CHECK
                            } else {
                                ""
                            },
                            label
                        ),
                        inner.width as usize,
                        false,
                    )
                })
                .collect::<Vec<_>>();
            let focused = if let Focus::Tab(i) = v.focus {
                i.min(count)
            } else {
                page
            };
            let mut start = 0;
            while start < focused
                && labels[start..=focused]
                    .iter()
                    .map(|s| s.width() + 1)
                    .sum::<usize>()
                    > inner.width as usize
            {
                start += 1;
            }
            let mut x = inner.x;
            for (i, text) in labels.iter().enumerate().skip(start) {
                let width = text.width() as u16;
                if x + width > inner.right() {
                    break;
                }
                let rect = Rect::new(x, inner.y + 1, width, 1);
                frame.render_widget(
                    Paragraph::new(text.as_str()).style(
                        if i == page || highlight == Some(Focus::Tab(i)) {
                            selected()
                        } else {
                            muted()
                        },
                    ),
                    rect,
                );
                self.hits.push((rect, Focus::Tab(i)));
                x += width + 1;
            }
        }
        let mut rows = Vec::new();
        if let Some(source) = &r.source {
            push(
                &mut rows,
                format!("来自子代理 · {source}"),
                muted(),
                None,
                inner.width,
            );
        }
        let mut caret = None;
        let mut editor_ranges = Vec::new();
        if page < count {
            let q = &r.form.questions[page];
            let draft = &self.drafts[&r.id][page];
            push(
                &mut rows,
                format!("{}{}", q.prompt, if q.required { "" } else { "（可选）" }),
                normal().add_modifier(Modifier::BOLD),
                None,
                inner.width,
            );
            push(&mut rows, String::new(), normal(), None, inner.width);
            for (i, o) in q.options.iter().enumerate() {
                let chosen = draft.selected.contains(&o.id);
                let mark = choice_mark(q.kind, chosen);
                let focus = Some(Focus::Option(i));
                push(
                    &mut rows,
                    format!("{mark} {}", o.label),
                    if highlight == Some(Focus::Option(i)) {
                        selected()
                    } else if chosen {
                        accent()
                    } else {
                        normal()
                    },
                    focus,
                    inner.width,
                );
                if !o.description.is_empty() {
                    push(
                        &mut rows,
                        format!("  {}", o.description),
                        if highlight == Some(Focus::Option(i)) {
                            selected()
                        } else {
                            muted()
                        },
                        focus,
                        inner.width,
                    );
                }
                push(&mut rows, String::new(), normal(), None, inner.width);
            }
            let custom = q.kind != Kind::Text && q.allow_custom;
            if custom && !draft.custom {
                let focus = Focus::Option(q.options.len());
                push(
                    &mut rows,
                    format!("{} 自行填写…", choice_mark(q.kind, false)),
                    if highlight == Some(focus) {
                        selected()
                    } else {
                        normal()
                    },
                    Some(focus),
                    inner.width,
                );
            }
            if q.kind == Kind::Text || (custom && draft.custom) {
                // The custom choice becomes its editor in place. Continuation
                // rows are created only by actual wrapping or entered newlines.
                let prefix = if custom {
                    format!("{} ", choice_mark(q.kind, true))
                } else {
                    String::new()
                };
                let prefix_width = prefix.width() as u16;
                let width = inner.width.saturating_sub(prefix_width).max(1);
                let (lines, pos) = draft.text.visual_lines(width as usize);
                let start = rows.len();
                for (index, text) in lines.into_iter().enumerate() {
                    let empty = draft.text.text.is_empty();
                    let text = if empty {
                        if custom {
                            "自行填写…".into()
                        } else {
                            "填写答案…".into()
                        }
                    } else {
                        text
                    };
                    let line = Line::from(vec![
                        ratatui::text::Span::styled(
                            if index == 0 {
                                prefix.clone()
                            } else {
                                " ".repeat(prefix_width as usize)
                            },
                            accent(),
                        ),
                        ratatui::text::Span::styled(text, if empty { muted() } else { normal() }),
                    ]);
                    rows.push(Row {
                        line,
                        focus: editable.then_some(Focus::Text),
                    });
                }
                if editable && !v.sending {
                    editor_ranges.push((start, rows.len(), prefix_width));
                    if v.focus == Focus::Text {
                        caret = Some((prefix_width + pos.0 as u16, start + pos.1));
                    }
                }
            }
        } else {
            for (i, (q, d)) in r.form.questions.iter().zip(&self.drafts[&r.id]).enumerate() {
                push(
                    &mut rows,
                    format!("{}. {}", i + 1, q.prompt),
                    accent(),
                    Some(Focus::Tab(i)),
                    inner.width,
                );
                let mut answer = q
                    .options
                    .iter()
                    .filter(|o| d.selected.contains(&o.id))
                    .map(|o| o.label.clone())
                    .collect::<Vec<_>>();
                if (q.kind == Kind::Text || d.custom) && !d.text.text.is_empty() {
                    answer.push(d.text.text.clone());
                }
                push(
                    &mut rows,
                    if answer.is_empty() {
                        "尚未填写".into()
                    } else {
                        answer.join("、")
                    },
                    normal(),
                    Some(Focus::Tab(i)),
                    inner.width,
                );
                if let Some(answer) = r
                    .answers
                    .iter()
                    .find(|a| a.question_id == q.id && !a.note.is_empty())
                {
                    push(
                        &mut rows,
                        format!("补充：{}", answer.note),
                        muted(),
                        Some(Focus::Tab(i)),
                        inner.width,
                    );
                }
                push(&mut rows, String::new(), normal(), None, inner.width);
            }
        }
        let body = Rect::new(inner.x, inner.y + 3, inner.width, inner.height - 8);
        let height = body.height as usize;
        v.offset = v.offset.min(rows.len().saturating_sub(height) as u16);
        if v.follow_focus {
            let row = caret
                .map(|(_, row)| row)
                .or_else(|| rows.iter().position(|r| r.focus == Some(v.focus)));
            if let Some(row) = row {
                if v.focus == Focus::Option(0) && row < height {
                    v.offset = 0;
                } else if row < v.offset as usize {
                    v.offset = row as u16;
                } else if row >= v.offset as usize + height {
                    v.offset = (row + 1 - height) as u16;
                }
            }
            v.follow_focus = false;
        }
        for (i, row) in rows.iter().skip(v.offset as usize).take(height).enumerate() {
            let rect = Rect::new(body.x, body.y + i as u16, body.width, 1);
            frame.render_widget(Paragraph::new(row.line.clone()).style(row.line.style), rect);
            let absolute = i + v.offset as usize;
            if let Some((start, _, prefix_width)) = editor_ranges
                .iter()
                .find(|(start, end, _)| (*start..*end).contains(&absolute))
            {
                let editor = Rect::new(
                    rect.x + *prefix_width,
                    rect.y,
                    rect.width.saturating_sub(*prefix_width),
                    1,
                );
                self.editor_area = editor;
                if absolute == *start && *prefix_width > 0 {
                    self.hits.push((
                        Rect::new(rect.x, rect.y, *prefix_width, 1),
                        Focus::Option(r.form.questions[page].options.len()),
                    ));
                }
                self.editor_rows
                    .push((editor, Focus::Text, absolute - start));
            }
            if let Some(focus) = row.focus {
                self.hits.push((rect, focus));
            }
        }
        let labels = [
            (Focus::Previous, "上一题"),
            (
                Focus::Next,
                if page < count {
                    "下一题"
                } else if editable {
                    "提交答案"
                } else {
                    "关闭"
                },
            ),
            (Focus::Later, if editable { "收起" } else { "关闭" }),
            (Focus::Decline, "取消问答"),
        ];
        let mut x = inner.x;
        let mut y = inner.bottom() - 4;
        for (focus, label) in labels {
            if focus == Focus::Decline && !editable {
                continue;
            }
            if focus == Focus::Next && page == count && !editable {
                continue;
            }
            let disabled = focus == Focus::Previous && page == 0;
            let width = label.width() as u16 + 2;
            if x + width > inner.right() {
                x = inner.x;
                y += 1;
            }
            if y > inner.bottom() - 3 {
                break;
            }
            let rect = Rect::new(x, y, width.min(inner.width), 1);
            frame.render_widget(
                Paragraph::new(format!(" {label} ")).style(if disabled {
                    muted()
                } else if highlight == Some(focus) {
                    selected()
                } else {
                    accent()
                }),
                rect,
            );
            if !disabled {
                self.hits.push((rect, focus));
            }
            x += width + 2;
        }
        let message = if v.sending {
            "正在确认提交结果…"
        } else {
            v.error.as_deref().unwrap_or("")
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(THEME.tool_failed)),
            Rect::new(inner.x, inner.bottom() - 2, inner.width, 1),
        );
        let help = if !editable {
            "↑↓ 滚动 · Tab 下一题 · Shift+Tab 上一题 · Esc 关闭"
        } else if page == count {
            "Enter 提交答案 · Shift+Tab 返回修改 · Esc 收起"
        } else if matches!(v.focus, Focus::Text) {
            "Enter 下一题 · Alt+Enter 换行 · Tab 切题 · Esc 收起"
        } else if r.form.questions[page].kind == Kind::Multiple {
            "↑↓ 移动 · 空格勾选 · Enter 下一题 · Tab 切题 · Esc 收起"
        } else {
            "↑↓ 选择 · Enter 确认并继续 · Tab 切题 · Esc 收起"
        };
        frame.render_widget(
            Paragraph::new(help).style(muted()),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
        caret.and_then(|(col, row)| {
            let y = row.checked_sub(v.offset as usize)?;
            (y < height).then_some(Position::new(
                body.x + col.min(body.width - 1),
                body.y + y as u16,
            ))
        })
    }
}
