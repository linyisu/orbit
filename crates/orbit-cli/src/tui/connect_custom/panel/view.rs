use ratatui::{
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use super::super::form::{self, ACCENT};
use super::{Action, Panel, Target};
use crate::tui::overlay;

pub(super) fn draw(
    panel: &mut Panel,
    frame: &mut ratatui::Frame<'_>,
    loading: bool,
) -> (Rect, Vec<(Rect, Target)>, usize) {
    let area = form::area(frame.area(), 100, frame.area().height.saturating_sub(4));
    overlay::clear(frame, area);
    let block = Block::default()
        .title(" 连接服务 ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let viewport = Rect::new(
        inner.x + 2,
        inner.y + 1,
        inner.width.saturating_sub(4),
        inner.height.saturating_sub(5),
    );
    let rows = panel.rows();
    let max_offset = rows.len().saturating_sub(viewport.height as usize);
    panel.offset = panel.offset.min(max_offset);
    let mut hits = Vec::new();
    for (index, row) in rows
        .iter()
        .enumerate()
        .skip(panel.offset)
        .take(viewport.height as usize)
    {
        let rect = Rect::new(
            viewport.x + row.indent,
            viewport.y + (index - panel.offset) as u16,
            viewport.width.saturating_sub(row.indent),
            1,
        );
        if row.target == Some(Target::Action(Action::Load)) {
            let mut x = rect.x;
            for (label, action) in [
                ("读取模型", Action::Load),
                ("添加模型", Action::Add),
                ("全选 / 取消全选", Action::All),
            ] {
                let label = format!(" {label} ");
                let button = Rect::new(
                    x,
                    rect.y,
                    (label.width() as u16).min(rect.right().saturating_sub(x)),
                    1,
                );
                let target = Target::Action(action);
                frame.render_widget(
                    Paragraph::new(label).style(button_style(panel, button, target, false)),
                    button,
                );
                hits.push((button, target));
                x = button.right().saturating_add(2).min(rect.right());
            }
        } else if let Some(Target::Action(Action::Effort(i, 1))) = row.target {
            frame.render_widget(
                Paragraph::new("思考强度")
                    .style(Style::default().fg(crate::tui::theme::THEME.foreground)),
                Rect::new(rect.x, rect.y, 18.min(rect.width), 1),
            );
            let mut x = rect.x + 20.min(rect.width);
            for j in 1..=7 {
                let field = &panel.models[i].reasoning[j];
                let label = format!(" {} ", field.label);
                let button = Rect::new(
                    x,
                    rect.y,
                    (label.width() as u16).min(rect.right().saturating_sub(x)),
                    1,
                );
                let target = Target::Action(Action::Effort(i, j));
                let style = button_style(panel, button, target, field.value() == "启用");
                frame.render_widget(Paragraph::new(label).style(style), button);
                hits.push((button, target));
                x = button.right();
            }
        } else if let Some(Target::Field(id)) = row.target {
            let field = panel.field(id);
            let focused = !field.readonly
                && (panel.focus == Target::Field(id)
                    || matches!(panel.focus, Target::Choice(field_id, _) if field_id == id));
            let label_width = 20.min(rect.width);
            frame.render_widget(
                Paragraph::new(field.label.clone()).style(Style::default().fg(if focused {
                    ACCENT
                } else {
                    crate::tui::theme::THEME.foreground
                })),
                Rect::new(rect.x, rect.y, label_width, 1),
            );
            let rect = Rect::new(
                rect.x + label_width,
                rect.y,
                rect.width.saturating_sub(label_width),
                1,
            );
            if !field.choices.is_empty() && !field.readonly {
                let mut x = rect.x;
                for (index, value) in field.choices.iter().enumerate() {
                    if !panel.choice_visible(id, index) {
                        continue;
                    }
                    let label = match value.as_str() {
                        "" => "未设置",
                        "OpenAI Chat Completions" => "Chat Completions",
                        "OpenAI Responses" => "Responses",
                        "Anthropic Messages" => "Messages",
                        other => other,
                    };
                    let label = format!(" {label} ");
                    let button = Rect::new(
                        x,
                        rect.y,
                        (label.width() as u16).min(rect.right().saturating_sub(x)),
                        1,
                    );
                    let target = Target::Choice(id, index);
                    frame.render_widget(
                        Paragraph::new(label).style(button_style(
                            panel,
                            button,
                            target,
                            field.value() == value,
                        )),
                        button,
                    );
                    hits.push((button, target));
                    x = button.right().saturating_add(1).min(rect.right());
                }
            } else {
                let line = form::input_line(
                    &field.input,
                    rect.width as usize,
                    field.secret,
                    focused,
                    &field.hint,
                );
                frame.render_widget(Paragraph::new(line), rect);
                if !field.readonly {
                    hits.push((rect, Target::Field(id)));
                }
            }
        } else {
            let width = if matches!(row.target, Some(Target::Action(Action::Toggle(_)))) {
                rect.width.saturating_sub(18)
            } else {
                rect.width
            };
            let hit = Rect::new(rect.x, rect.y, width.min(row.label.width() as u16), 1);
            let hover = panel.pointer.is_some_and(|p| hit.contains(p));
            let active = row.target.is_some()
                && (hover || panel.pointer.is_none() && row.target == Some(panel.focus));
            frame.render_widget(
                Paragraph::new(row.label.clone()).style(if active {
                    Style::default()
                        .fg(crate::tui::theme::THEME.on_accent)
                        .bg(ACCENT)
                } else {
                    Style::default().fg(crate::tui::theme::THEME.foreground)
                }),
                hit,
            );
            if let Some(target) = row.target {
                hits.push((hit, target));
            }
            if let Some(Target::Action(Action::Toggle(i))) = row.target {
                for (label, action, back) in [
                    ("设置", Action::Expand(i), 16),
                    ("默认", Action::Default(i), 8),
                ] {
                    let button = Rect::new(
                        rect.right().saturating_sub(back),
                        rect.y,
                        4.min(rect.width),
                        1,
                    );
                    let active = panel.pointer.is_some_and(|p| button.contains(p))
                        || panel.pointer.is_none() && panel.focus == Target::Action(action);
                    frame.render_widget(
                        Paragraph::new(label).style(if active {
                            Style::default()
                                .fg(crate::tui::theme::THEME.on_accent)
                                .bg(ACCENT)
                        } else {
                            Style::default().fg(ACCENT)
                        }),
                        button,
                    );
                    hits.push((button, Target::Action(action)));
                }
            }
        }
    }
    let status = if !panel.message.is_empty() {
        panel.message.clone()
    } else if loading {
        "正在读取模型…".into()
    } else {
        format!(
            "{} 个模型 · 已启用 {} 个",
            panel.models.len(),
            panel.models.iter().filter(|m| m.entry.enabled).count()
        )
    };
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(ACCENT)),
        Rect::new(
            viewport.x,
            inner.bottom().saturating_sub(3),
            viewport.width,
            1,
        ),
    );
    for (label, action, x) in [
        (" 保存 ", Action::Save, viewport.x),
        (" 取消 ", Action::Cancel, viewport.x + 10),
    ] {
        let rect = Rect::new(x, inner.bottom().saturating_sub(1), label.width() as u16, 1);
        let active = panel.pointer.is_some_and(|p| rect.contains(p))
            || panel.pointer.is_none() && panel.focus == Target::Action(action);
        frame.render_widget(
            Paragraph::new(label).style(if active {
                Style::default()
                    .fg(crate::tui::theme::THEME.on_accent)
                    .bg(ACCENT)
            } else {
                Style::default().fg(ACCENT)
            }),
            rect,
        );
        hits.push((rect, Target::Action(action)));
    }
    (viewport, hits, max_offset)
}

fn button_style(panel: &Panel, rect: Rect, target: Target, selected: bool) -> Style {
    let hovered = panel.pointer.is_some_and(|p| rect.contains(p))
        || panel.pointer.is_none() && panel.focus == target;
    let mut style = if selected {
        Style::default()
            .fg(ACCENT)
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        Style::default().fg(crate::tui::theme::THEME.foreground)
    };
    if hovered {
        style = style.add_modifier(ratatui::style::Modifier::UNDERLINED);
    }
    style
}
