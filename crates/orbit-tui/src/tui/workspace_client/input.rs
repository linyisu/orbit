use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

use super::*;
use crate::tui::{self, approval, render, selection};

impl Tab {
    pub fn handle(&mut self, event: Event) -> Result<Action> {
        tui::overlay::sync(&mut self.state);
        if matches!(event, Event::Resize(..)) {
            tui::overlay::invalidate(&mut self.state);
        }
        if self.state.overlay_needs_layout && matches!(event, Event::Mouse(_)) {
            return Ok(Action::Redraw);
        }
        let action = self.handle_event(event);
        tui::overlay::sync(&mut self.state);
        action
    }

    fn handle_event(&mut self, mut event: Event) -> Result<Action> {
        let layer = tui::overlay::active(&self.state);
        if let Some(prompt) = &mut self.state.ssh_prompt {
            if prompt.handle(&event) {
                self.cancel_connect();
            }
            return Ok(Action::Redraw);
        }
        if let Some(wizard) = &mut self.state.workspace_wizard {
            match wizard.handle(&event) {
                super::wizard::Action::Start(spec) => self.start(*spec),
                super::wizard::Action::Browse(path) => self.browse(path),
                super::wizard::Action::Open(path) => self.open_directory(path),
                super::wizard::Action::Cancel => return Ok(Action::Close),
                super::wizard::Action::Back => {
                    if let Some(spec) = self.spec.clone() {
                        self.state.model = "配置连接".into();
                        self.state.workspace_wizard = Some(super::wizard::Wizard::new(Some(spec)));
                    }
                }
                super::wizard::Action::Retry => {
                    if self.id.is_some() {
                        self.reconnect();
                    } else if let Some(spec) = self.spec.clone() {
                        self.start(spec);
                    }
                }
                super::wizard::Action::None => {}
            }
            return Ok(Action::Redraw);
        }
        if (self.connecting.is_some() || self.reconnect_at.is_some())
            && matches!(&event, Event::Key(key) if key.kind != KeyEventKind::Release && (key.code == KeyCode::Esc || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))))
        {
            self.cancel_connect();
            return Ok(Action::Redraw);
        }
        if let Some(preview) = &mut self.state.media_preview {
            let (_, close) = preview.handle(&event);
            if close {
                self.state.media_preview = None;
            }
            return Ok(Action::None);
        }
        if layer == Some(tui::overlay::Layer::Plan) {
            if let Some(action) = self.state.planning.handle(&event) {
                self.submit_with_receipt(
                    Request::Plan {
                        session: self.id.clone().unwrap_or_default(),
                        action,
                    },
                    submissions::Draft::Plan,
                );
            }
            return Ok(Action::Redraw);
        }
        if !self.state.dialog_open() && self.state.approval.is_none() {
            self.state.planning.handle(&event);
            if self.state.planning.visible() {
                return Ok(Action::Redraw);
            }
        }
        if layer == Some(tui::overlay::Layer::Questions) {
            match self.state.user_input.handle(&event) {
                tui::user_input::Action::Answer { id, answers } => self.submit_with_receipt(
                    Request::AnswerUserInput {
                        session: self.id.clone().unwrap_or_default(),
                        request: id.clone(),
                        answers,
                    },
                    submissions::Draft::UserInput(id),
                ),
                tui::user_input::Action::History(before) => {
                    self.send(Request::UserInputHistory {
                        session: self.id.clone().unwrap_or_default(),
                        before,
                    });
                }
                tui::user_input::Action::CancelTask => {
                    self.send(Request::Cancel {
                        session: self.id.clone().unwrap_or_default(),
                    });
                }
                tui::user_input::Action::None => {}
            }
            return Ok(Action::Redraw);
        }
        if !self.state.dialog_open() && self.state.approval.is_none() {
            self.state.user_input.handle(&event);
            if self.state.user_input.visible() {
                return Ok(Action::Redraw);
            }
        }
        if layer == Some(tui::overlay::Layer::Restore)
            && tui::restore::handle(&mut self.state, &event)
        {
            return Ok(Action::Redraw);
        }
        if self.state.btw.visible() {
            use tui::transcript_dialog::Action as DialogAction;
            match self.state.btw.handle(&event)? {
                DialogAction::Submit(question) => {
                    self.ask(question, self.state.btw.follow_up_index());
                }
                DialogAction::Link(url) => {
                    if let Err(error) = tui::open_link_in_browser(&url) {
                        self.state.btw.link_error(&error);
                    }
                }
                DialogAction::None | DialogAction::Redraw | DialogAction::Close => {}
            }
            if let Some(index) = self.state.btw.take_cancel() {
                self.send(Request::CancelQuestion {
                    session: self.id.clone().unwrap_or_default(),
                    index,
                });
            }
            return Ok(Action::Redraw);
        }
        if let Some(viewer) = &mut self.state.subagent_dialog {
            match viewer.handle(&event)? {
                tui::subagents::ViewerAction::Close => {
                    self.state.subagent_dialog = None;
                    self.subagent = None;
                }
                tui::subagents::ViewerAction::Link(url) => {
                    if let Err(error) = tui::open_link_in_browser(&url) {
                        viewer.link_error(&error);
                    }
                }
                tui::subagents::ViewerAction::Submit(message) => self.send_subagent(message),
                tui::subagents::ViewerAction::None | tui::subagents::ViewerAction::Redraw => {}
            }
            return Ok(Action::Redraw);
        }
        if tui::subagents::handle_parent(&mut self.state, &event).is_some() {
            if let Some(parent) = self.state.resume_request.take() {
                return Ok(Action::SameTarget(Some(parent)));
            }
            if !matches!(&event, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved) {
                return Ok(Action::Redraw);
            }
        }
        if self.state.dialog_open() {
            if let Some(id) = tui::dialogs::handle(&mut self.state, event) {
                self.state.picker = None;
                return Ok(Action::SameTarget(Some(id)));
            }
            let plugin_action = if let Some(source) = self.state.pending_marketplace_source.take() {
                Some(orbit_protocol::plugins::Action::Add { source })
            } else {
                self.state
                    .pending_project_plugin_change
                    .take()
                    .map(|change| match change {
                        tui::dialogs::ProjectPluginChange::Enable(plugin) => {
                            orbit_protocol::plugins::Action::Enable { plugin }
                        }
                        tui::dialogs::ProjectPluginChange::Disable {
                            marketplace_source,
                            plugin_name,
                            ..
                        } => orbit_protocol::plugins::Action::Disable {
                            marketplace_source,
                            plugin_name,
                        },
                    })
            };
            if let Some(action) = plugin_action {
                self.send(Request::Plugins {
                    session: self.id.clone().unwrap_or_default(),
                    action,
                });
            }
            if let Some(id) = self.state.delete_request.take() {
                self.send(Request::Admin {
                    action: orbit_protocol::admin::Action::DeleteSession { id },
                });
                self.send(Request::ListSessions {
                    session: self.id.clone().unwrap_or_default(),
                });
            }
            if let Some(model) = self.state.chosen_model.take() {
                let effort = self
                    .state
                    .chosen_effort
                    .take()
                    .map(|effort| effort.as_str().into());
                self.submit_with_receipt(
                    Request::SetModel {
                        session: self.id.clone().unwrap_or_default(),
                        model: model.clone(),
                        effort,
                    },
                    submissions::Draft::Model(model),
                );
            }
            return Ok(Action::Redraw);
        }
        if let Some(approval) = self.approval_id
            && approval::captures(&self.state, &event)
        {
            let action = approval::handle(&mut self.state, event);
            let session = self.id.clone().unwrap_or_default();
            match action {
                approval::Action::Approve
                | approval::Action::AlwaysAllow
                | approval::Action::Deny => {
                    self.send(Request::Approve {
                        session,
                        approval,
                        allow: action != approval::Action::Deny,
                        remember: action == approval::Action::AlwaysAllow,
                    });
                }
                approval::Action::Cancel => {
                    self.send(Request::Cancel { session });
                }
                approval::Action::None => {}
            }
            return Ok(Action::None);
        }
        if let Event::Mouse(mouse) = &event
            && self
                .state
                .menu_area
                .contains((mouse.column, mouse.row).into())
            && !self.state.menu().is_empty()
        {
            tui::completion::select_with_mouse(&mut self.state, mouse);
            let clicked = mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && self
                    .state
                    .menu_rows
                    .iter()
                    .any(|(area, _)| area.contains((mouse.column, mouse.row).into()));
            if !clicked {
                return Ok(Action::Redraw);
            }
            let code = if self.state.menu()[self.state.menu_index]
                .name
                .starts_with("/cd ")
            {
                KeyCode::Tab
            } else {
                KeyCode::Enter
            };
            event = Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE));
        }
        if !self.state.selection.dragging() {
            render::handle_context_hover(&mut self.state, &event);
            tui::handle_list_hover(&mut self.state, &event);
            render::handle_link_hover(&mut self.state, &event);
            render::handle_transcript_hover(&mut self.state, &event);
            if tui::navigation::handle(&mut self.state, &event).is_some() {
                return Ok(Action::Redraw);
            }
        }
        if matches!(&event, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved)
            || tui::handle_input_mouse(&mut self.state, &event)
        {
            return Ok(Action::None);
        }
        if let Event::Mouse(mouse) = &event
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && mouse.modifiers.contains(KeyModifiers::CONTROL)
        {
            let offset = self.state.max_scroll.saturating_sub(self.state.scroll);
            if let Some(link) = self
                .state
                .transcript_layout
                .link_at(
                    self.state.selection.viewport,
                    offset,
                    (mouse.column, mouse.row),
                )
                .map(str::to_owned)
            {
                if tui::markdown::footnotes::is_target(&link) {
                    tui::jump_footnote_at(&mut self.state, (mouse.column, mouse.row));
                } else if tui::subagent_link_id(&link).is_some() {
                    tui::open_subagent_at(&mut self.state, (mouse.column, mouse.row));
                } else if let Err(error) = tui::open_link_in_browser(&link) {
                    self.state
                        .report_local(TranscriptItem::Error(error.to_string()));
                }
                return Ok(Action::None);
            }
        }
        if let Event::Mouse(mouse) = &event
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && tui::attachments::focus_attachment_at(&mut self.state, mouse.column, mouse.row)
        {
            self.state.selection.clear();
            return Ok(Action::Redraw);
        }
        if let Some(point) = self.state.selection.click(&event)
            && (tui::jump_footnote_at(&mut self.state, point)
                || tui::open_subagent_at(&mut self.state, point)
                || tui::toggle_transcript_at(&mut self.state, point))
        {
            return Ok(Action::Redraw);
        }
        if self.state.selection.handle(&event)? {
            return Ok(Action::None);
        }
        if let Event::Paste(text) = event {
            if !text.is_empty() {
                if let Some(paths) =
                    tui::attachments::pasted_image_paths(&text, &self.local_directory()?)
                {
                    self.attach(paths);
                } else {
                    self.state.input.insert(&text);
                }
                return Ok(Action::None);
            }
            return self.paste();
        }
        if matches!(&event, Event::Key(key) if key.code == KeyCode::BackTab && key.kind != KeyEventKind::Release)
        {
            self.send(Request::Plan {
                session: self.id.clone().unwrap_or_default(),
                action: if self.state.planning.mode() == orbit_protocol::plan::WorkMode::Plan {
                    orbit_protocol::plan::Action::Leave
                } else {
                    orbit_protocol::plan::Action::Enter
                },
            });
            return Ok(Action::Redraw);
        }
        let busy = self.busy || self.starting.is_some();
        match tui::keyboard_action(&mut self.state, event, busy) {
            tui::Action::Quit => Ok(Action::Quit),
            tui::Action::Redraw => Ok(Action::Redraw),
            tui::Action::Submit(prompt) => self.submit(prompt),
            tui::Action::Steer(prompt) => {
                let prompt = prompt.trim().to_owned();
                self.submit_with_receipt(
                    Request::Steer {
                        session: self.id.clone().unwrap_or_default(),
                        message: prompt.clone(),
                    },
                    submissions::Draft::Composer(prompt),
                );
                Ok(Action::None)
            }
            tui::Action::Cancel => {
                if let Some(session) = &self.id {
                    self.send(Request::Cancel {
                        session: session.clone(),
                    });
                }
                Ok(Action::None)
            }
            tui::Action::CopyInput(text) => {
                selection::copy_text(&text)?;
                Ok(Action::None)
            }
            tui::Action::PasteClipboard => self.paste(),
            tui::Action::AttachPaths(paths) => {
                self.attach(paths);
                Ok(Action::None)
            }
            tui::Action::None => Ok(Action::None),
        }
    }

    fn set_mode(&mut self, mode: tui::permissions::Mode) {
        let mode = match mode {
            tui::permissions::Mode::Ask => Mode::Ask,
            tui::permissions::Mode::Auto => Mode::Auto,
            tui::permissions::Mode::Yolo => Mode::Yolo,
        };
        if self.send(Request::SetMode {
            session: self.id.clone().unwrap_or_default(),
            mode,
        }) {
            self.mode = mode;
            self.state.permission_mode = match mode {
                Mode::Ask => tui::permissions::Mode::Ask,
                Mode::Auto => tui::permissions::Mode::Auto,
                Mode::Yolo => tui::permissions::Mode::Yolo,
            };
        }
    }

    fn inspect(&mut self, section: orbit_protocol::Inspection) {
        self.send(Request::SessionAction {
            session: self.id.clone().unwrap_or_default(),
            action: orbit_protocol::SessionAction::Inspect { section },
        });
    }

    fn attach(&mut self, paths: Vec<std::path::PathBuf>) {
        if !self.state.supports_vision || self.attachment_task.is_some() {
            self.state.report_local(TranscriptItem::Error(
                "当前模型不支持图片，或上一批图片尚在读取。".into(),
            ));
            return;
        }
        self.attachment_task = Some(AbortOnDropHandle::new(tokio::task::spawn_blocking(
            move || workspace::attachments::paths(paths),
        )));
    }

    fn paste(&mut self) -> Result<Action> {
        let payload = match tui::attachments::read_clipboard() {
            Ok(payload) => payload,
            Err(error) => {
                self.state
                    .report_local(TranscriptItem::Error(error.to_string()));
                return Ok(Action::None);
            }
        };
        self.paste_payload(payload)
    }

    pub(super) fn paste_payload(
        &mut self,
        payload: tui::attachments::ClipboardPayload,
    ) -> Result<Action> {
        use tui::attachments::ClipboardPayload;
        match payload {
            ClipboardPayload::Image {
                width,
                height,
                bytes,
            } => {
                if self.state.supports_vision && self.attachment_task.is_none() {
                    self.attachment_task =
                        Some(AbortOnDropHandle::new(tokio::task::spawn_blocking(
                            move || workspace::attachments::rgba(width, height, bytes),
                        )));
                } else {
                    self.state.report_local(TranscriptItem::Error(
                        "当前模型不支持图片，或上一批图片尚在读取。".into(),
                    ));
                }
            }
            ClipboardPayload::Files(paths) => {
                let (images, files): (Vec<_>, Vec<_>) = paths
                    .into_iter()
                    .partition(|path| tui::attachments::is_image_path(path));
                if !files.is_empty() {
                    let references = files
                        .iter()
                        .map(|path| format!("[文件：{}]", path.display()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.state.input.insert(&references);
                }
                if !images.is_empty() {
                    self.attach(images);
                }
            }
            ClipboardPayload::Text(text) => {
                if let Some(paths) =
                    tui::attachments::pasted_image_paths(&text, &self.local_directory()?)
                {
                    self.attach(paths);
                } else {
                    self.state.input.insert(&text);
                }
            }
        }
        Ok(Action::None)
    }

    pub fn submit(&mut self, prompt: String) -> Result<Action> {
        let prompt = prompt.trim().to_owned();
        let command = prompt.trim();
        if Spec::is_command(command) {
            match Spec::parse(command) {
                Ok(mut spec) => {
                    if let Some(configuration) = self
                        .connection
                        .as_ref()
                        .and_then(|connection| connection.local_configuration.as_ref())
                    {
                        spec.inherit(&configuration.config, &configuration.auth, None);
                    }
                    return Ok(Action::Open(Box::new(spec)));
                }
                Err(error) => {
                    self.state.input.set(prompt);
                    self.state
                        .report_local(TranscriptItem::Error(error.to_string()));
                    return Ok(Action::None);
                }
            }
        }
        let session = self.id.clone().unwrap_or_default();
        if let Some(question) = tui::btw::question(command) {
            self.ask(question.into(), None);
            return Ok(Action::Redraw);
        }
        let (name, argument) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        if matches!(name, "/tasks" | "/memory") {
            match project::parse(name, argument) {
                Ok(action) => {
                    self.send(Request::SessionAction { session, action });
                }
                Err(error) => self
                    .state
                    .report_local(TranscriptItem::CommandError(error.to_string())),
            }
            return Ok(Action::Redraw);
        }
        match command {
            "/quit" | "/exit" => return Ok(Action::Quit),
            "/close" => return Ok(Action::Close),
            "/local" => return Ok(Action::Local),
            "/new" => return Ok(Action::SameTarget(None)),
            "/reconnect" => self.reconnect(),
            "/restore" => return Ok(Action::Restore),
            "/connect" => return Ok(Action::Connect),
            "/editor" => return Ok(Action::Editor),
            "/model refresh" => return Ok(Action::RefreshModels),
            "/answers" => {
                self.state.user_input.history();
                self.send(Request::UserInputHistory {
                    session: self.id.clone().unwrap_or_default(),
                    before: None,
                });
            }
            "/help" => {
                self.state.selection = Default::default();
                self.state.show_help = true;
                self.state.help_keys = false;
                self.state.help_scroll = 0;
            }
            text if text == "/undo" || text.starts_with("/undo ") => {
                let path = text
                    .strip_prefix("/undo ")
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned);
                self.send(Request::Undo { session, path });
            }
            "/plugins" => {
                self.send(Request::Plugins {
                    session,
                    action: orbit_protocol::plugins::Action::List,
                });
            }
            text if text.starts_with("/cd ") => {
                self.send(Request::SessionAction {
                    session,
                    action: orbit_protocol::SessionAction::ChangeDirectory {
                        path: text[4..].trim().into(),
                    },
                });
            }
            "/status" => self.inspect(orbit_protocol::Inspection::Status),
            "/context" => self.inspect(orbit_protocol::Inspection::Context),
            "/skills" => self.inspect(orbit_protocol::Inspection::Skills),
            "/mcp" => self.inspect(orbit_protocol::Inspection::Mcp),
            "/diagnostics" => self.inspect(orbit_protocol::Inspection::Diagnostics),
            "/changes" => self.inspect(orbit_protocol::Inspection::Changes),
            "/diff" => self.inspect(orbit_protocol::Inspection::Diff { path: None }),
            text if text.starts_with("/diff ") => self.inspect(orbit_protocol::Inspection::Diff {
                path: Some(text[6..].trim().into()),
            }),
            text if text.starts_with("/rename ") => {
                self.send(Request::SessionAction {
                    session,
                    action: orbit_protocol::SessionAction::Rename {
                        title: text[8..].trim().into(),
                    },
                });
            }
            text if text.starts_with("/skill ") => {
                self.send(Request::SessionAction {
                    session,
                    action: orbit_protocol::SessionAction::LoadSkill {
                        name: text[7..].trim().into(),
                    },
                });
            }
            "/sessions" | "/resume" => {
                self.send(Request::ListSessions {
                    session: self.id.clone().unwrap_or_default(),
                });
            }
            text if text.starts_with("/resume ") => {
                return Ok(Action::SameTarget(Some(text[8..].trim().into())));
            }
            "/model" => {
                self.state.model_picker = Some(tui::dialogs::ModelPicker::new(
                    self.model_catalog.clone(),
                    self.state.model.clone(),
                    self.state.reasoning_effort,
                ));
            }
            text if text.starts_with("/model ") => {
                let requested = text[7..].trim();
                let qualified = if requested.contains("::") {
                    requested.to_owned()
                } else {
                    self.state.model.split_once("::").map_or_else(
                        || requested.to_owned(),
                        |(provider, _)| format!("{provider}::{requested}"),
                    )
                };
                if self.state.model_completions.contains(&qualified) {
                    let model = qualified.as_str();
                    self.submit_with_receipt(
                        Request::SetModel {
                            session: session.clone(),
                            model: model.into(),
                            effort: None,
                        },
                        submissions::Draft::Model(model.into()),
                    );
                } else {
                    self.state.report_local(TranscriptItem::Error(
                        "模型未配置；用 /model 查看目标环境的模型。".into(),
                    ));
                }
            }
            "/reasoning" => self.state.input.set("/reasoning ".into()),
            text if text.starts_with("/reasoning ") => {
                self.send(Request::SetModel {
                    session: session.clone(),
                    model: self.state.model.clone(),
                    effort: Some(text[11..].trim().into()),
                });
            }
            "/plan" | "/mode plan" => {
                if self.state.planning.mode() != orbit_protocol::plan::WorkMode::Plan {
                    self.send(Request::Plan {
                        session: session.clone(),
                        action: orbit_protocol::plan::Action::Enter,
                    });
                } else if self.state.planning.snapshot.plan.is_some() {
                    self.state.planning.open();
                }
            }
            "/plan view" => self.state.planning.open(),
            "/build" | "/mode build" => {
                self.send(Request::Plan {
                    session: session.clone(),
                    action: orbit_protocol::plan::Action::Leave,
                });
            }
            "/mode" => {
                self.state.show_output("工作模式", format!("当前：{}\n/plan 只读规划 · /build 返回执行模式（不会批准或自动执行计划）\n/plan view 查看计划 · /permissions 管理执行审批", self.state.planning.mode().label()));
            }
            "/permissions" => {
                self.state
                    .show_output("审批模式", self.state.permission_mode.description().into());
            }
            _ if matches!(name, "/mode" | "/permissions") && !argument.trim().is_empty() => {
                match tui::permissions::Mode::parse(argument.trim()) {
                    Some(mode) => self.set_mode(mode),
                    None => self
                        .state
                        .report_local(TranscriptItem::CommandError("模式：auto、ask、yolo".into())),
                }
            }
            text if tui::commands::is_command_input(text) && text != "/compact" => {
                self.state.report_local(TranscriptItem::CommandError(
                    tui::commands::suggestion(text).map_or_else(
                        || "未知命令；输入 / 查看可用命令。".into(),
                        |command| format!("是否想输入 {}？", command.name),
                    ),
                ))
            }
            _ if self.busy
                || self.starting.is_some()
                || self.commands.is_none()
                || self.attachment_task.is_some() =>
            {
                self.state.input.set(prompt);
                self.state.report_local(TranscriptItem::Notice(
                    "连接尚未就绪或任务仍在执行，输入已保留。".into(),
                ));
            }
            _ => {
                let compact = command == "/compact";
                if !compact && !self.state.supports_vision && !self.state.attachments.is_empty() {
                    self.state.input.set(prompt);
                    self.state.report_local(TranscriptItem::Error(
                        "当前模型不支持图片输入，文字和图片草稿已保留。请切换支持图片的模型，或移除图片后发送。".into(),
                    ));
                    return Ok(Action::None);
                }
                let images = match workspace::attachments::uploads(if compact {
                    &[]
                } else {
                    &self.state.attachments
                }) {
                    Ok(images) => images,
                    Err(error) => {
                        self.state.input.set(prompt);
                        self.state
                            .report_local(TranscriptItem::Error(error.to_string()));
                        return Ok(Action::None);
                    }
                };
                if self.send(Request::Start {
                    images,
                    compact,
                    session,
                    request_id: workspace::request_id(),
                    prompt: prompt.clone(),
                    mode: self.mode,
                    model: None,
                }) {
                    self.starting = Some(prompt);
                    if !compact {
                        self.starting_attachments = std::mem::take(&mut self.state.attachments);
                        self.state.attachment_cursor = None;
                    }
                } else {
                    self.state.input.set(prompt);
                }
            }
        }
        Ok(Action::None)
    }
}
