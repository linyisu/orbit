use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

use super::*;
use crate::tui::{self, approval, render, selection};

impl Tab {
    pub fn handle(&mut self, event: Event) -> Result<Action> {
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
        if self.connecting.is_some()
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
        if self.state.dialog_open() {
            if let Some(id) = tui::dialogs::handle(&mut self.state, event) {
                self.state.picker = None;
                return Ok(Action::SameTarget(Some(id)));
            }
            return Ok(Action::Redraw);
        }
        if matches!(&event, Event::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) && self.state.input.text.is_empty())
        {
            return Ok(Action::Quit);
        }
        if let Some(approval) = self.approval_id {
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
        if !self.state.selection.dragging() {
            render::handle_link_hover(&mut self.state, &event);
            render::handle_transcript_hover(&mut self.state, &event);
            tui::navigation::handle(&mut self.state, &event);
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
        if let Some(point) = self.state.selection.click(&event)
            && (tui::jump_footnote_at(&mut self.state, point)
                || tui::open_subagent_at(&mut self.state, point)
                || tui::toggle_transcript_at(&mut self.state, point))
        {
            return Ok(Action::None);
        }
        if self.state.selection.handle(&event)? {
            return Ok(Action::None);
        }
        if let Event::Paste(text) = event {
            if !text.is_empty() {
                if let Some(paths) =
                    tui::attachments::pasted_image_paths(&text, &std::env::current_dir()?)
                {
                    self.attach(paths);
                } else {
                    self.state.input.insert(&text);
                }
                return Ok(Action::None);
            }
            return self.paste();
        }
        let busy = self.busy || self.starting.is_some();
        match tui::keyboard_action(&mut self.state, event, busy) {
            tui::Action::Quit => Ok(Action::Quit),
            tui::Action::Redraw => Ok(Action::Redraw),
            tui::Action::Submit(prompt) => self.submit(prompt),
            tui::Action::Steer(prompt) => {
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
        use tui::attachments::ClipboardPayload;
        let payload = match tui::attachments::read_clipboard() {
            Ok(payload) => payload,
            Err(error) => {
                self.state
                    .report_local(TranscriptItem::Error(error.to_string()));
                return Ok(Action::None);
            }
        };
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
            ClipboardPayload::Files(paths) => self.attach(paths),
            ClipboardPayload::Text(text) => {
                if let Some(paths) =
                    tui::attachments::pasted_image_paths(&text, &std::env::current_dir()?)
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
        let command = prompt.trim();
        if Spec::is_command(command) {
            match Spec::parse(command) {
                Ok(mut spec) => {
                    if let Some(configuration) = self
                        .connection
                        .as_ref()
                        .and_then(|connection| connection.local_configuration.as_ref())
                    {
                        spec.inherit(
                            &configuration.config,
                            &configuration.auth,
                            Some(&self.state.model),
                        );
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
        match command {
            "/quit" | "/exit" => return Ok(Action::Quit),
            "/close" => return Ok(Action::Close),
            "/local" => return Ok(Action::Local),
            "/new" => return Ok(Action::SameTarget(None)),
            "/reconnect" => self.reconnect(),
            "/help" => self.state.report_local(TranscriptItem::Notice(HELP.into())),
            text if text == "/undo" || text.starts_with("/undo ") => {
                let path = text
                    .strip_prefix("/undo ")
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned);
                self.send(Request::Undo { session, path });
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
                self.send(Request::Sessions);
            }
            text if text.starts_with("/resume ") => {
                return Ok(Action::SameTarget(Some(text[8..].trim().into())));
            }
            "/model" => self.state.report_local(TranscriptItem::CommandResult(
                self.state.model_completions.join("\n"),
            )),
            text if text.starts_with("/model ") => {
                let model = text[7..].trim();
                if self
                    .state
                    .model_completions
                    .iter()
                    .any(|choice| choice == model)
                {
                    self.send(Request::SetModel {
                        session: session.clone(),
                        model: model.into(),
                        effort: None,
                    });
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
            text if text.starts_with("/mode ") => {
                match text[6..].trim().to_ascii_lowercase().as_str() {
                    "auto" => {
                        self.mode = Mode::Auto;
                        self.state.permission_mode = tui::permissions::Mode::Auto;
                    }
                    "ask" => {
                        self.mode = Mode::Ask;
                        self.state.permission_mode = tui::permissions::Mode::Ask;
                    }
                    "yolo" => {
                        self.mode = Mode::Yolo;
                        self.state.permission_mode = tui::permissions::Mode::Yolo;
                    }
                    _ => self
                        .state
                        .report_local(TranscriptItem::Error("模式：auto、ask、yolo".into())),
                }
            }
            text if text.starts_with('/') && text != "/compact" => self.state.report_local(
                TranscriptItem::Error("此命令尚未接入工作区服务；/help 查看支持的命令。".into()),
            ),
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
                let images = match workspace::attachments::uploads(&self.state.attachments) {
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
                    compact: command == "/compact",
                    session,
                    request_id: workspace::request_id(),
                    prompt: prompt.clone(),
                    mode: self.mode,
                    model: None,
                }) {
                    self.starting = Some(prompt);
                } else {
                    self.state.input.set(prompt);
                }
            }
        }
        Ok(Action::None)
    }
}
