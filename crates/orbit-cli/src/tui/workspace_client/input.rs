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
                } else if let Err(error) = tui::open_link_in_browser(&link) {
                    self.state
                        .report_local(TranscriptItem::Error(error.to_string()));
                }
                return Ok(Action::None);
            }
        }
        if let Some(point) = self.state.selection.click(&event)
            && (tui::jump_footnote_at(&mut self.state, point)
                || tui::toggle_transcript_at(&mut self.state, point))
        {
            return Ok(Action::None);
        }
        if self.state.selection.handle(&event)? {
            return Ok(Action::None);
        }
        // Pasted target paths are text until remote attachment upload is implemented.
        if let Event::Paste(text) = event {
            if !text.is_empty() {
                self.state.input.insert(&text);
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
                self.state.input.set(prompt);
                self.state.report_local(TranscriptItem::Notice(
                    "任务仍在执行；可先取消，输入已保留。".into(),
                ));
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
                self.state.input.insert(
                    &paths
                        .iter()
                        .map(|p| p.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                Ok(Action::None)
            }
            tui::Action::None => Ok(Action::None),
        }
    }

    fn paste(&mut self) -> Result<Action> {
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            Ok(text) => self.state.input.insert(&text),
            Err(error) => self.state.report_local(TranscriptItem::Error(format!(
                "剪贴板文本读取失败：{error}；当前连接尚不支持上传图片附件。"
            ))),
        }
        Ok(Action::None)
    }

    pub fn submit(&mut self, prompt: String) -> Result<Action> {
        let command = prompt.trim();
        if Spec::is_command(command) {
            match Spec::parse(command) {
                Ok(spec) => return Ok(Action::Open(Box::new(spec))),
                Err(error) => {
                    self.state.input.set(prompt);
                    self.state
                        .report_local(TranscriptItem::Error(error.to_string()));
                    return Ok(Action::None);
                }
            }
        }
        let session = self.id.clone().unwrap_or_default();
        match command {
            "/quit" | "/exit" => return Ok(Action::Quit),
            "/close" => return Ok(Action::Close),
            "/local" => return Ok(Action::Local),
            "/new" => return Ok(Action::SameTarget(None)),
            "/reconnect" => self.reconnect(),
            "/help" => self.state.report_local(TranscriptItem::Notice(HELP.into())),
            "/status" => self
                .state
                .report_local(TranscriptItem::CommandResult(format!(
                    "{}\n{}\n会话：{}\n{}",
                    self.state.connection_label.as_deref().unwrap_or_default(),
                    self.state.workspace,
                    session,
                    if self.offline {
                        "已断开"
                    } else if self.connecting.is_some() {
                        "连接中"
                    } else {
                        "已连接"
                    }
                ))),
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
                    self.selected_model = Some(model.to_owned());
                    self.state.model = model.into();
                } else {
                    self.state.report_local(TranscriptItem::Error(
                        "模型未配置；用 /model 查看目标环境的模型。".into(),
                    ));
                }
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
            text if text.starts_with('/') => self.state.report_local(TranscriptItem::Error(
                "此命令尚未接入工作区服务；/help 查看支持的命令。".into(),
            )),
            _ if self.busy || self.starting.is_some() || self.commands.is_none() => {
                self.state.input.set(prompt);
                self.state.report_local(TranscriptItem::Notice(
                    "连接尚未就绪或任务仍在执行，输入已保留。".into(),
                ));
            }
            _ => {
                if self.send(Request::Start {
                    session,
                    request_id: workspace::request_id(),
                    prompt: prompt.clone(),
                    mode: self.mode,
                    model: self.selected_model.clone(),
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
