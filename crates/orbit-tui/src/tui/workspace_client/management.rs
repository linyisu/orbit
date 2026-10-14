//! All management requests use the same local/SSH transport and bound receipts.
use super::*;
use crate::tui::{management, selection};

impl Tab {
    pub(super) fn shortcut(&mut self, event: &crossterm::event::Event) -> Option<Result<Action>> {
        use crate::tui::keymap::Command;
        use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
        let Event::Key(key) = event else {
            return None;
        };
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let command = self
            .keymap
            .as_ref()?
            .lock()
            .ok()?
            .resolve(key);
        if command == Some(Command::Help)
            && key.code == KeyCode::Char('?')
            && (!self.state.input.text.is_empty() || !self.state.attachments.is_empty())
        {
            return None;
        }
        let Some(command) = command else {
            // A disabled or rebound default must not fall through to the old
            // hardcoded route. Ordinary '?' remains usable as prompt text.
            if key.modifiers == KeyModifiers::CONTROL
                && matches!(key.code, KeyCode::Up | KeyCode::Down)
            {
                return Some(Ok(Action::Redraw));
            }
            if key.code == KeyCode::Char('?')
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && self.state.input.text.is_empty()
                && self.state.attachments.is_empty()
            {
                self.state.input.insert("?");
                return Some(Ok(Action::Redraw));
            }
            return None;
        };
        Some(match command {
            Command::NewTab => Ok(Action::SameTarget(None)),
            Command::CloseTab => Ok(Action::Close),
            Command::Help => self.submit("/help".into()),
            Command::Search => self.submit("/resume".into()),
            Command::Plugins => self.submit("/plugins".into()),
            Command::Files => self.submit("/file".into()),
            Command::Review => self.submit("/review".into()),
            Command::Queue => self.submit("/queue".into()),
            Command::Settings => self.submit("/settings".into()),
            Command::Stop => {
                self.send(Request::Cancel {
                    session: self.id.clone().unwrap_or_default(),
                });
                Ok(Action::Redraw)
            }
            Command::HistoryPrevious | Command::HistoryNext => {
                if self.state.attachments.is_empty() {
                    self.state.attachment_cursor = None;
                    if command == Command::HistoryPrevious {
                        self.state.input.history_up();
                    } else {
                        self.state.input.history_down();
                    }
                    self.state.menu_dismissed = true;
                }
                Ok(Action::Redraw)
            }
        })
    }

    pub(super) fn open_management(&mut self, command: &str) -> bool {
        let Some(session) = self.id.as_deref() else {
            return false;
        };
        if command == "/keys" {
            let Some(keymap) = &self.keymap else {
                self.state
                    .report_local(TranscriptItem::CommandError(
                        "此连接未提供本机快捷键配置路径。".into(),
                    ));
                return true;
            };
            let state = management::local_keys(
                session.into(),
                self.state.workspace.clone(),
                keymap.clone(),
            );
            self.state.management.activate(state);
            return true;
        }
        if matches!(command, "/settings" | "/mcp") {
            let Some(local) = self
                .connection
                .as_ref()
                .and_then(|connection| connection.local_configuration.as_ref())
            else {
                self.state
                    .report_local(TranscriptItem::CommandError(
                        "此连接未提供本机配置路径；/mcp status 可查看服务器连接状态。".into(),
                    ));
                return true;
            };
            let state = management::local_settings(
                session.into(),
                self.state.workspace.clone(),
                local.config.clone(),
                local.auth.clone(),
                command == "/mcp",
            );
            self.state.management.activate(state);
            return true;
        }
        let Some((panel, request)) = management::open(command, session, &self.state.workspace)
        else {
            return false;
        };
        let id = panel.id.clone();
        if self.state.management.activate(panel) {
            self.management_action(&id, management::Action::Request(request));
        }
        true
    }

    pub(super) fn management_action(&mut self, id: &str, action: management::Action) {
        match action {
            management::Action::None => {}
            management::Action::Close => {
                if self
                    .state
                    .management
                    .active
                    .as_ref()
                    .is_some_and(|panel| panel.id == id)
                {
                    self.state.management.hide();
                }
            }
            management::Action::Request(request) => {
                let Some(panel) = self.state.management.get_mut(id) else {
                    return;
                };
                if self.id.as_deref() != Some(&panel.session)
                    || self.state.workspace != panel.workspace
                {
                    panel
                        .panel
                        .error("会话或工作区已变化；请返回原会话继续，草稿已保留".into());
                    return;
                }
                self.submit_with_receipt(request, submissions::Draft::Management(id.into()));
            }
            management::Action::Draft(text) => {
                if !self.state.input.text.is_empty() {
                    self.state.input.insert("\n");
                }
                self.state.input.insert(&text);
                if self
                    .state
                    .management
                    .active
                    .as_ref()
                    .is_some_and(|panel| panel.id == id)
                {
                    self.state.management.hide();
                }
            }
            management::Action::Copy(text) => {
                if let Err(error) = selection::copy_text(&text) {
                    self.state
                        .report_local(TranscriptItem::Error(format!("复制失败：{error}")));
                }
            }
        }
    }

    pub(super) fn management_response(&mut self, id: &str, response: Result<Response>) {
        let Some(panel) = self.state.management.get_mut(id) else {
            return;
        };
        if self.id.as_deref() != Some(&panel.session) || self.state.workspace != panel.workspace {
            panel
                .panel
                .error("工作区已变化；忽略旧响应，草稿已保留".into());
            return;
        }
        let action = match response {
            Ok(response) => panel.panel.receive(response),
            Err(error) => {
                panel.panel.error(error.to_string());
                management::Action::None
            }
        };
        self.management_action(id, action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use orbit_protocol::input_queue::{Entry, Snapshot, State};

    fn tab() -> (Tab, mpsc::Receiver<Call>) {
        let mut tab = Tab::empty("local".into(), "/project".into(), media::Context::default());
        tab.id = Some("session".into());
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        tab.commands = Some(Arc::new(sender));
        (tab, receiver)
    }

    #[tokio::test]
    async fn late_response_settles_its_hidden_panel_without_replacing_active_editor() {
        let (mut tab, mut requests) = tab();
        assert!(tab.open_management("/queue"));
        let queue = tab
            .state
            .management
            .active
            .as_ref()
            .unwrap()
            .id
            .clone();
        let call = requests.try_recv().unwrap();
        assert!(matches!(call.request, Request::InputQueue { .. }));
        assert!(tab.open_management("/file"));
        let file = tab
            .state
            .management
            .active
            .as_ref()
            .unwrap()
            .id
            .clone();
        call.reply
            .unwrap()
            .send(Ok(Response::InputQueue(Snapshot::List {
                entries: vec![Entry {
                    images: vec![],
                    id: 3,
                    content: "original".into(),
                    state: State::Queued,
                }],
                next: None,
                run: Some(9),
            })))
            .unwrap();
        assert!(tab.poll_submissions());
        assert_eq!(
            tab.state
                .management
                .active
                .as_ref()
                .unwrap()
                .id,
            file
        );
        assert!(tab.open_management("/queue"));
        assert_eq!(
            tab.state
                .management
                .active
                .as_ref()
                .unwrap()
                .id,
            queue
        );
        let panel = &mut tab
            .state
            .management
            .active
            .as_mut()
            .unwrap()
            .panel;
        panel.handle(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let action = panel.handle(&Event::Key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        )));
        assert!(matches!(
            action,
            management::Action::Request(Request::InputQueue {
                action: orbit_protocol::input_queue::Action::Edit { run: Some(9), .. },
                ..
            })
        ));
    }

    #[tokio::test]
    async fn workspace_changes_block_hidden_panel_mutations_and_stale_receipts() {
        let (mut tab, mut requests) = tab();
        tab.open_management("/queue");
        let id = tab
            .state
            .management
            .active
            .as_ref()
            .unwrap()
            .id
            .clone();
        let call = requests.try_recv().unwrap();
        tab.state.workspace = "/other".into();
        tab.management_action(
            &id,
            management::Action::Request(Request::Cancel {
                session: "session".into(),
            }),
        );
        assert!(requests.try_recv().is_err());
        call.reply
            .unwrap()
            .send(Ok(Response::InputQueue(Snapshot::List {
                entries: vec![],
                next: None,
                run: None,
            })))
            .unwrap();
        tab.poll_submissions();
        assert_eq!(
            tab.state
                .management
                .active
                .as_ref()
                .unwrap()
                .id,
            id
        );
        assert_eq!(
            tab.state
                .management
                .active
                .as_ref()
                .unwrap()
                .workspace,
            "/project"
        );
    }

    #[tokio::test]
    async fn panels_open_during_execution_and_capture_wheel_without_steering_chat() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let (mut tab, mut requests) = tab();
        tab.busy = true;
        tab.state
            .input
            .set("unsent draft".into());
        tab.state.scroll = 4;
        tab.submit("/queue".into()).unwrap();
        let call = requests.try_recv().unwrap();
        assert!(matches!(call.request, Request::InputQueue { .. }));
        call.reply
            .unwrap()
            .send(Ok(Response::InputQueue(Snapshot::List {
                entries: (1..40)
                    .map(|id| Entry {
                        images: vec![],
                        id,
                        content: format!("queued {id}"),
                        state: State::Queued,
                    })
                    .collect(),
                next: None,
                run: Some(5),
            })))
            .unwrap();
        tab.poll_submissions();
        for (width, height) in [(120, 32), (40, 12), (18, 7)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::tui::render::draw_frame(frame, &mut tab.state))
                .unwrap();
        }
        tab.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }))
        .unwrap();
        assert_eq!(tab.state.scroll, 4);
        assert_eq!(tab.state.input.text, "unsent draft");
        assert!(requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn disabling_default_history_and_rebinding_commands_changes_actual_event_routing() {
        use crate::tui::keymap::{self, Command};
        let directory = tempfile::tempdir().unwrap();
        let keymap = keymap::shared(&directory.path().join("config.toml"));
        keymap
            .lock()
            .unwrap()
            .remove(Command::HistoryPrevious)
            .unwrap();
        keymap
            .lock()
            .unwrap()
            .capture(
                Command::Queue,
                &KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
            )
            .unwrap();
        let (mut tab, mut requests) = tab();
        tab.keymap = Some(keymap);
        tab.state
            .input
            .history
            .push("history".into());
        tab.state.input.set("draft".into());
        tab.handle(Event::Key(KeyEvent::new(
            KeyCode::Up,
            KeyModifiers::CONTROL,
        )))
        .unwrap();
        assert_eq!(tab.state.input.text, "draft");
        tab.handle(Event::Key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)))
            .unwrap();
        assert!(tab.state.management.active.is_some());
        assert!(matches!(
            requests.try_recv().unwrap().request,
            Request::InputQueue { .. }
        ));
        tab.handle(Event::Key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)))
            .unwrap();
        assert!(requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn plugin_receipts_cannot_replace_a_newly_opened_dialog() {
        let (mut tab, mut requests) = tab();
        let catalog = || orbit_protocol::plugins::Catalog {
            marketplaces: vec![],
            project_id: "project".into(),
            enabled: vec![],
            inspections: vec![],
        };
        tab.state.marketplace_dialog = Some(crate::tui::dialogs::MarketplaceDialog::from_catalog(
            catalog(),
        ));
        let old_id = tab
            .state
            .marketplace_dialog
            .as_ref()
            .unwrap()
            .identity()
            .to_owned();
        tab.submit_with_receipt(
            Request::Plugins {
                session: "session".into(),
                expected_workspace: Some("/project".into()),
                action: orbit_protocol::plugins::Action::List,
            },
            submissions::Draft::Plugins {
                workspace: "/project".into(),
                id: old_id,
            },
        );
        let call = requests.try_recv().unwrap();
        tab.state.marketplace_dialog = Some(crate::tui::dialogs::MarketplaceDialog::from_catalog(
            catalog(),
        ));
        let current_id = tab
            .state
            .marketplace_dialog
            .as_ref()
            .unwrap()
            .identity()
            .to_owned();
        call.reply
            .unwrap()
            .send(Ok(Response::Plugins(catalog())))
            .unwrap();
        tab.poll_submissions();
        assert_eq!(
            tab.state
                .marketplace_dialog
                .as_ref()
                .unwrap()
                .identity(),
            current_id
        );
    }

    #[tokio::test]
    async fn cancel_aborts_independent_writes_waiting_to_send() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (mut tab, mut requests) = tab();
        let (ready, wait) = tokio::sync::oneshot::channel();
        let sent = Arc::new(AtomicBool::new(false));
        let observed = sent.clone();
        tab.independent_requests
            .spawn(async move {
                let _ = wait.await;
                observed.store(true, Ordering::SeqCst);
            });
        tokio::task::yield_now().await;
        assert!(tab.send(Request::Cancel {
            session: "session".into(),
        }));
        assert!(matches!(
            requests.try_recv().unwrap().request,
            Request::Cancel { .. }
        ));
        let _ = ready.send(());
        let result = tab
            .independent_requests
            .join_next()
            .await
            .unwrap();
        assert!(result.unwrap_err().is_cancelled());
        assert!(!sent.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn review_writes_leave_the_main_channel_available_for_cancel_and_polling() {
        let (mut tab, mut requests) = tab();
        tab.connection = Some(Arc::new(Connection {
            local_configuration: None,
            target: workspace::transport::Target::Local,
            workspace: "/project".into(),
            executable: "/missing-orbit-parity-fixture".into(),
            config: "/unused-config.toml".into(),
            database: None,
            auth_dir: None,
            allow_mcp_start: false,
            unattended: true,
            interactive: None,
        }));
        tab.open_management("/review");
        let initial = requests.try_recv().unwrap();
        initial
            .reply
            .unwrap()
            .send(Ok(Response::Review(
                orbit_protocol::review::Snapshot::List {
                    entries: vec![],
                    next: None,
                    branch: None,
                },
            )))
            .unwrap();
        tab.poll_submissions();
        let id = tab
            .state
            .management
            .active
            .as_ref()
            .unwrap()
            .id
            .clone();
        tab.management_action(
            &id,
            management::Action::Request(Request::Review {
                session: "session".into(),
                expected_workspace: "/project".into(),
                action: orbit_protocol::review::Action::Undo {
                    path: "file.rs".into(),
                    run_id: 7,
                    call_id: "call".into(),
                },
            }),
        );
        assert!(requests.try_recv().is_err());
        assert_eq!(tab.independent_requests.len(), 1);
        tab.send(Request::Cancel {
            session: "session".into(),
        });
        assert!(matches!(
            requests.try_recv().unwrap().request,
            Request::Cancel { .. }
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !tab.submissions.is_empty() {
                tokio::task::yield_now().await;
                tab.poll_submissions();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            tab.state
                .management
                .active
                .as_ref()
                .unwrap()
                .id,
            id
        );
        assert!(!tab.offline);
    }

    #[tokio::test]
    async fn approval_temporarily_yields_the_management_panel_and_restores_its_identity() {
        use crate::tui::overlay;
        let (mut tab, mut requests) = tab();
        tab.open_management("/queue");
        let _initial = requests.try_recv().unwrap();
        let id = tab
            .state
            .management
            .active
            .as_ref()
            .unwrap()
            .id
            .clone();
        tab.approval_id = Some(8);
        tab.state.approval = Some(orbit_tools::approval::ApprovalPreview::Capability(
            orbit_tools::approval::CapabilityPreview {
                environment: orbit_runtime::capability::EnvironmentId::Local,
                path: "/external".into(),
                access: orbit_runtime::capability::FilesystemAccess::Read,
                reason: "test".into(),
            },
        ));
        overlay::sync(&mut tab.state);
        assert!(tab.state.management.active.is_none());
        tab.state.approval_shown = true;
        tab.handle(Event::Key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::CONTROL,
        )))
        .unwrap();
        assert!(matches!(
            requests.try_recv().unwrap().request,
            Request::Approve {
                approval: 8,
                allow: true,
                ..
            }
        ));
        tab.state.approval = None;
        overlay::sync(&mut tab.state);
        assert_eq!(
            tab.state
                .management
                .active
                .as_ref()
                .unwrap()
                .id,
            id
        );
    }
}
