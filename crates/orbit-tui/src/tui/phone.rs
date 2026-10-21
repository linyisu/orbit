//! Phone management views; authorization and transport remain in the shared client and service.
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use futures_util::StreamExt;
use orbit_client::{
    phone::{self, HostAction, HostStatus, Permissions},
    transport::{Connection, Target},
};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use tokio_util::task::AbortOnDropHandle;

use super::{terminal, theme::THEME};

struct Choice {
    label: String,
    action: HostAction,
}

fn choices(status: &HostStatus, permissions: Permissions) -> Vec<Choice> {
    let mut items = vec![Choice {
        label: if status.enabled {
            "关闭手机连接"
        } else {
            "开启手机连接"
        }
        .into(),
        action: if status.enabled {
            HostAction::Disable
        } else {
            HostAction::Enable
        },
    }];
    if status.enabled {
        items.push(Choice {
            label: "生成配对二维码".into(),
            action: HostAction::Invite { permissions },
        });
    }
    for id in &status.pending {
        items.push(Choice {
            label: format!("允许配对 · {id}"),
            action: HostAction::Confirm {
                id: id.clone(),
                allow: true,
            },
        });
        items.push(Choice {
            label: format!("拒绝配对 · {id}"),
            action: HostAction::Confirm {
                id: id.clone(),
                allow: false,
            },
        });
    }
    for device in &status.devices {
        items.push(Choice {
            label: format!("撤销设备 · {device}"),
            action: HostAction::Revoke {
                device: device.clone(),
            },
        });
    }
    items
}

fn request(
    connection: Arc<Connection>,
    action: HostAction,
) -> AbortOnDropHandle<Result<HostStatus>> {
    AbortOnDropHandle::new(tokio::spawn(async move {
        phone::request_connection(&connection, action).await
    }))
}

fn qr_lines(invitation: &str) -> Result<Vec<String>> {
    let code = qrcode::QrCode::new(invitation)?;
    Ok(code
        .render::<qrcode::render::unicode::Dense1x2>()
        .build()
        .lines()
        .map(str::to_owned)
        .collect())
}

pub(super) async fn run(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    connection: Arc<Connection>,
) -> Result<()> {
    anyhow::ensure!(
        matches!(connection.target, Target::Local),
        "请在本机项目中管理手机连接"
    );
    let mut status = HostStatus::default();
    let mut permissions = Permissions {
        control: true,
        approve: true,
    };
    let mut selection = ListState::default().with_selected(Some(0));
    let mut pending = Some(request(connection.clone(), HostAction::Status));
    let mut error = String::new();
    let mut qr: Option<Vec<String>> = None;
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    loop {
        let items = choices(&status, permissions);
        let selected = selection
            .selected()
            .unwrap_or(0)
            .min(items.len() - 1);
        selection.select(Some(selected));
        terminal::draw_synchronized(terminal, |frame| {
            let area = frame.area();
            frame.render_widget(Clear, area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" 手机连接 · Esc 关闭 ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let sections = Layout::vertical([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(inner);
            let mode = match (permissions.control, permissions.approve) {
                (false, _) => "只读",
                (true, false) => "发送任务和附件",
                (true, true) => "发送任务、附件和审批",
            };
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(format!(
                        "{} · {}",
                        if status.enabled {
                            "已开启"
                        } else {
                            "未开启"
                        },
                        connection.workspace
                    )),
                    Line::from(format!("新配对权限：{mode}  ·  P 切换")),
                ]),
                sections[0],
            );
            if let Some(lines) = &qr {
                if lines.len() > usize::from(sections[1].height)
                    || lines
                        .first()
                        .is_some_and(|line| line.chars().count() > usize::from(sections[1].width))
                {
                    frame.render_widget(
                        Paragraph::new(concat!(
                            "终端空间不足以完整显示二维码，请放大窗口或缩小字体。\n",
                            "Q 返回管理，配对请求会自动显示。",
                        )),
                        sections[1],
                    );
                } else {
                    frame.render_widget(
                        Paragraph::new(lines.join("\n")).style(
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::White),
                        ),
                        sections[1],
                    );
                }
            } else {
                let list = List::new(
                    items
                        .iter()
                        .map(|item| ListItem::new(item.label.as_str())),
                )
                .highlight_symbol("› ")
                .highlight_style(Style::default().fg(THEME.accent));
                frame.render_stateful_widget(list, sections[1], &mut selection);
            }
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(if pending.is_some() {
                        "正在更新…"
                    } else {
                        "↑↓ 选择 · Enter 执行 · Q 二维码/管理 · R 刷新"
                    }),
                    Line::from(error.as_str()),
                    Line::from("关闭此窗口不会关闭远程连接；手机扫码即可完成配对。"),
                ]),
                sections[2],
            );
        })?;
        tokio::select! {
            event = events.next() => {
                let Some(event) = event else { return Ok(()) };
                if let Event::Key(key) = event? && key.kind != KeyEventKind::Release {
                    match key.code {
                        KeyCode::Esc => return Ok(()),
                        KeyCode::Char('q') => {
                            qr = if qr.is_some() { None } else {
                                status.invitation.as_deref().map(qr_lines).transpose()?
                            };
                        }
                        KeyCode::Up => selection.select(Some(selected.saturating_sub(1))),
                        KeyCode::Down => selection.select(Some((selected + 1).min(items.len() - 1))),
                        KeyCode::Char('p') if pending.is_none() => {
                            permissions = match (permissions.control, permissions.approve) {
                                (true, false) => Permissions { control: true, approve: true },
                                (true, true) => Permissions { control: false, approve: false },
                                _ => Permissions { control: true, approve: false },
                            };
                        }
                        KeyCode::Enter if pending.is_none() && qr.is_none() => {
                            let action = items.into_iter().nth(selected).unwrap().action;
                            pending = Some(request(connection.clone(), action));
                        }
                        KeyCode::Char('r') if pending.is_none() => {
                            pending = Some(request(connection.clone(), HostAction::Status));
                        }
                        _ => {}
                    }
                }
            }
            result = async { pending.as_mut().unwrap().await }, if pending.is_some() => {
                pending = None;
                match result? {
                    Ok(next) => {
                        error.clear();
                        if next.invitation != status.invitation {
                            qr = next.invitation.as_deref().map(qr_lines).transpose()?;
                        }
                        if !next.pending.is_empty() && next.pending != status.pending {
                            qr = None;
                            selection.select(Some(2));
                        }
                        status = next;
                    }
                    Err(failure) => error = failure.to_string(),
                }
            }
            _ = ticks.tick(), if pending.is_none() => {
                pending = Some(request(connection.clone(), HostAction::Status));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_qr_preserves_modules_and_white_quiet_zone() {
        let text = "orbit-test-generated-invitation";
        let code = qrcode::QrCode::new(text).unwrap();
        let lines = qr_lines(text).unwrap();
        let width = code.width() + 8;
        assert_eq!(lines.len(), width.div_ceil(2));
        for y in 0..width {
            for x in 0..width {
                let glyph = lines[y / 2].chars().nth(x).unwrap();
                let dark = match glyph {
                    '█' => true,
                    '▀' => y % 2 == 0,
                    '▄' => y % 2 == 1,
                    ' ' => false,
                    _ => panic!("unexpected QR glyph"),
                };
                let expected = x >= 4
                    && y >= 4
                    && x < width - 4
                    && y < width - 4
                    && code[(x - 4, y - 4)] == qrcode::Color::Dark;
                assert_eq!(dark, expected, "module {x},{y}");
            }
        }
    }

    #[test]
    fn pending_pairing_requires_explicit_choice_and_device_revocation_keeps_identity() {
        let status = HostStatus {
            enabled: true,
            pending: vec!["request-test".into()],
            devices: vec!["device-test".into()],
            ..Default::default()
        };
        let items = choices(
            &status,
            Permissions {
                control: false,
                approve: false,
            },
        );
        assert!(
            matches!(&items[1].action, HostAction::Invite { permissions } if !permissions.control && !permissions.approve)
        );
        assert!(
            matches!(&items[2].action, HostAction::Confirm { id, allow: true } if id == "request-test")
        );
        assert!(
            matches!(&items[3].action, HostAction::Confirm { id, allow: false } if id == "request-test")
        );
        assert!(
            matches!(&items[4].action, HostAction::Revoke { device } if device == "device-test")
        );
    }
}
