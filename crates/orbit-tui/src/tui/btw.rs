use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{Frame, layout::Rect};

use super::{
    app::{RunStatus, TranscriptItem},
    transcript_dialog::{Action, TranscriptDialog},
};

pub(super) fn question(prompt: &str) -> Option<&str> {
    let (name, text) = prompt
        .trim()
        .split_once(char::is_whitespace)
        .unwrap_or((prompt.trim(), ""));
    (name == "/btw").then_some(text.trim())
}

#[derive(Debug)]
struct Dialog {
    index: usize,
    view: TranscriptDialog,
    rendered_answer: usize,
    rendered_status: String,
}

#[derive(Default)]
pub(super) struct Questions {
    records: Vec<orbit_protocol::SideQuestion>,
    running: bool,
    cancel_request: Option<usize>,
    dialog: Option<Box<Dialog>>,
}

impl std::fmt::Debug for Questions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Questions")
            .field("count", &self.records.len())
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

impl Questions {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn receive(
        &mut self,
        records: Option<Vec<orbit_protocol::SideQuestion>>,
        running: bool,
        selected: Option<usize>,
    ) {
        if let Some(records) = records {
            self.records = records;
        }
        self.running = running;
        if self
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.index >= self.records.len())
        {
            self.dialog = None;
        }
        if let Some(index) = selected.filter(|index| *index < self.records.len()) {
            self.open(index);
        } else {
            self.refresh();
        }
    }

    pub fn follow_up_index(&self) -> Option<usize> {
        self.dialog.as_ref().map(|dialog| dialog.index)
    }

    pub fn take_cancel(&mut self) -> Option<usize> {
        self.cancel_request.take()
    }

    pub fn has_records(&self) -> bool {
        !self.records.is_empty()
    }

    pub fn visible(&self) -> bool {
        self.dialog.is_some()
    }

    fn open(&mut self, index: usize) {
        let mut view = TranscriptDialog::new(self.records[index].model.clone());
        view.enable_input();
        view.ui.follow_bottom = self.records[index].status == "running";
        self.dialog = Some(Box::new(Dialog {
            index,
            view,
            rendered_answer: usize::MAX,
            rendered_status: String::new(),
        }));
        self.refresh();
    }

    pub fn poll(&mut self) -> bool {
        self.refresh()
    }

    fn refresh(&mut self) -> bool {
        let Some(dialog) = &mut self.dialog else {
            return false;
        };
        let record = &self.records[dialog.index];
        if dialog.view.ui.selection.active()
            || (dialog.rendered_answer == record.answer.len()
                && dialog.rendered_status == record.status)
        {
            return false;
        }

        dialog.view.ui.transcript.clear();
        for record in self
            .records
            .as_slice()
            .iter()
            .filter(|item| item.thread_id == record.thread_id)
        {
            dialog
                .view
                .ui
                .transcript
                .push(TranscriptItem::User(record.question.clone()));
            dialog
                .view
                .ui
                .transcript
                .push(TranscriptItem::Assistant(record.answer.clone()));
            if let Some(error) = &record.error {
                dialog
                    .view
                    .ui
                    .transcript
                    .push(TranscriptItem::Error(error.clone()));
            }
        }

        dialog.view.block_submission(self.running);
        dialog.view.ui.status = match record.status.as_str() {
            "running" => RunStatus::Running,
            "completed" => RunStatus::Completed,
            "cancelled" => RunStatus::Cancelled,
            _ => RunStatus::Failed,
        };
        dialog.view.ui.invalidate_transcript(0);
        dialog.rendered_answer = record.answer.len();
        dialog.rendered_status = record.status.clone();
        true
    }

    pub fn tick(&mut self) -> bool {
        self.poll()
            | self
                .dialog
                .as_mut()
                .is_some_and(|dialog| dialog.view.tick())
    }

    pub fn handle(&mut self, event: &Event) -> std::io::Result<Action> {
        let Some(dialog) = &mut self.dialog else {
            return Ok(Action::None);
        };
        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
        {
            if key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && !dialog.view.ui.selection.active()
                && dialog.view.ui.input.selection().is_none()
                && self.running
            {
                self.cancel_request = Some(dialog.index);
                return Ok(Action::Redraw);
            }

            if key.modifiers == KeyModifiers::ALT
                && matches!(key.code, KeyCode::Left | KeyCode::Right)
            {
                let heads = self.thread_heads();
                let position = heads
                    .iter()
                    .position(|index| {
                        Some(*index) == self.dialog.as_ref().map(|dialog| dialog.index)
                    })
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Left {
                    position.saturating_sub(1)
                } else {
                    (position + 1).min(heads.len() - 1)
                };
                if position != next {
                    self.open(heads[next]);
                }
                return Ok(Action::Redraw);
            }
        }

        let action = dialog.view.handle(event)?;
        if matches!(action, Action::Close) {
            self.clear();
        }
        Ok(action)
    }

    pub fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        inner: Rect,
        media: super::media::Context,
    ) {
        self.refresh();
        let heads = self.thread_heads();
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        dialog.view.ui.media = media;
        let record = &self.records[dialog.index];
        let status = match record.status.as_str() {
            "running" => "生成中",
            "completed" => "完成",
            "cancelled" => "已取消",
            _ => "失败",
        };
        let title = format!(
            "旁路提问 · {status} · {}/{}",
            heads
                .iter()
                .position(|index| *index == dialog.index)
                .unwrap_or(0)
                + 1,
            heads.len()
        );
        let metadata = if heads.len() > 1 {
            format!("{} · Alt+←/→ 查看其他提问", record.model)
        } else {
            record.model.clone()
        };

        dialog
            .view
            .draw(&title, &metadata, None, frame, area, inner);
    }

    fn thread_heads(&self) -> Vec<usize> {
        let mut heads = Vec::<usize>::new();
        for (index, record) in self.records.iter().enumerate() {
            if let Some(head) = heads
                .iter_mut()
                .find(|head| self.records[**head].thread_id == record.thread_id)
            {
                *head = index;
            } else {
                heads.push(index);
            }
        }

        heads
    }

    pub fn cursor(&self) -> Option<ratatui::layout::Position> {
        self.dialog
            .as_ref()
            .and_then(|dialog| dialog.view.ui.ime_cursor)
    }

    pub fn submit_error(&mut self, error: &anyhow::Error) {
        if let Some(dialog) = &mut self.dialog {
            dialog.view.submit_error(error);
        }
    }

    pub fn link_error(&mut self, error: &anyhow::Error) {
        if let Some(dialog) = &mut self.dialog {
            dialog
                .view
                .ui
                .transcript
                .push(TranscriptItem::Error(format!("无法打开链接：{error}")));
        }
    }
}

#[cfg(test)]
mod tests;
