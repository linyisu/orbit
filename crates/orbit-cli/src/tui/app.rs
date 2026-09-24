//! UI state derived from canonical history and the Agent event stream.

use std::collections::HashMap;

use orbit_agent::events::AgentEvent;
use orbit_model::types::{Attachment, ChatMessage, ReasoningEffort, StreamEvent, ToolCall};
use orbit_store::Session;
use orbit_tools::approval::ApprovalRequest;
use ratatui::text::Line;

use super::input::Input;
use crate::approval::ApprovalMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    User,
    Assistant,
    Thinking,
    Tool,
    Notice,
    Error,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub kind: EntryKind,
    pub text: String,
    pub tool_id: Option<String>,
    pub tool_name: Option<String>,
    pub output: String,
    pub preview: String,
    pub expanded: bool,
    pub finished: bool,
    pub failed: bool,
    pub revision: u64,
}

impl Entry {
    pub(super) fn new(kind: EntryKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            tool_id: None,
            tool_name: None,
            output: String::new(),
            preview: String::new(),
            expanded: false,
            finished: false,
            failed: false,
            revision: 0,
        }
    }
}

pub(crate) struct CachedEntry {
    pub width: usize,
    pub revision: u64,
    pub lines: Vec<Line<'static>>,
}

pub struct Picker {
    pub sessions: Vec<Session>,
    pub selected: usize,
}

pub struct ModelPicker {
    pub models: Vec<String>,
    pub selected: usize,
}

pub struct App {
    pub input: Input,
    pub entries: Vec<Entry>,
    pub session_id: String,
    pub workspace: String,
    pub model: String,
    pub reasoning: Option<ReasoningEffort>,
    pub pending_images: Vec<Attachment>,
    pub context_tokens: usize,
    pub context_used_tokens: usize,
    pub running: bool,
    pub status: String,
    pub approval_mode: ApprovalMode,
    pub scroll: usize,
    pub follow_bottom: bool,
    pub picker: Option<Picker>,
    pub model_picker: Option<ModelPicker>,
    pub approval: Option<ApprovalRequest>,
    pub approval_scroll: usize,
    pub selected_tool: Option<usize>,
    pub command_selection: usize,
    pub restoring: bool,
    pub(crate) render_cache: Vec<Option<CachedEntry>>,
    active_assistant: Option<usize>,
    active_thinking: Option<usize>,
    tool_entries: HashMap<String, usize>,
}

impl App {
    pub fn new(
        session_id: String,
        workspace: String,
        model: String,
        context_tokens: usize,
    ) -> Self {
        Self {
            input: Input::default(),
            entries: Vec::new(),
            session_id,
            workspace,
            model,
            reasoning: None,
            pending_images: Vec::new(),
            context_tokens,
            context_used_tokens: 0,
            running: false,
            status: "Ready".to_owned(),
            approval_mode: ApprovalMode::Auto,
            scroll: 0,
            follow_bottom: true,
            picker: None,
            model_picker: None,
            approval: None,
            approval_scroll: 0,
            selected_tool: None,
            command_selection: 0,
            restoring: false,
            render_cache: Vec::new(),
            active_assistant: None,
            active_thinking: None,
            tool_entries: HashMap::new(),
        }
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.entries.push(Entry::new(EntryKind::Notice, text));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.entries.push(Entry::new(EntryKind::Error, text));
    }

    pub fn submit(&mut self, prompt: &str, attachments: &[Attachment]) {
        self.entries.push(Entry::new(
            EntryKind::User,
            user_display(prompt, attachments),
        ));
        self.running = true;
        self.status = "Thinking".to_owned();
        self.active_assistant = None;
        self.active_thinking = None;
        self.follow_bottom = true;
    }

    pub fn begin_restore(&mut self) {
        self.pending_images.clear();
        self.entries.clear();
        self.render_cache.clear();
        self.active_assistant = None;
        self.active_thinking = None;
        self.tool_entries.clear();
        self.selected_tool = None;
        self.restoring = true;
        self.status = "Restoring session".to_owned();
    }

    pub fn restore_page(&mut self, history: &[ChatMessage]) {
        for message in history {
            match message {
                ChatMessage::User {
                    content,
                    attachments,
                } => {
                    self.entries.push(Entry::new(
                        EntryKind::User,
                        user_display(content, attachments),
                    ));
                }
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                } => {
                    if let Some(content) = content.as_ref().filter(|text| !text.is_empty()) {
                        self.entries.push(Entry::new(EntryKind::Assistant, content));
                    }
                    for call in tool_calls {
                        self.add_tool(call);
                    }
                }
                ChatMessage::Tool {
                    tool_call_id,
                    content,
                } => {
                    self.finish_tool(tool_call_id, content, false);
                }
                ChatMessage::System { .. } => {}
            }
        }
        self.follow_bottom = true;
    }

    pub fn finish_restore(&mut self) {
        for entry in &mut self.entries {
            if entry.kind == EntryKind::Tool && !entry.finished {
                entry.output = "Tool outcome unknown after an interrupted run".to_owned();
                entry.preview = entry.output.clone();
                entry.failed = true;
                entry.finished = true;
                entry.revision += 1;
            }
        }
        self.restoring = false;
        self.status = "Ready".to_owned();
    }

    pub fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::ModelRequestPrepared {
                estimated_input_tokens,
            } => {
                self.context_used_tokens = estimated_input_tokens;
            }
            AgentEvent::ModelUsageObserved {
                estimated_input_tokens,
                actual_input_tokens,
                ..
            } => {
                self.context_used_tokens = actual_input_tokens
                    .and_then(|actual| usize::try_from(actual).ok())
                    .unwrap_or(estimated_input_tokens);
            }
            AgentEvent::TurnStarted { .. } => {
                self.active_assistant = None;
                self.active_thinking = None;
                self.status = "Thinking".to_owned();
            }
            AgentEvent::Model(StreamEvent::TextDelta(text)) => {
                let index = self.active_assistant.unwrap_or_else(|| {
                    self.entries.push(Entry::new(EntryKind::Assistant, ""));
                    let index = self.entries.len() - 1;
                    self.active_assistant = Some(index);
                    index
                });
                self.entries[index].text.push_str(&text);
                self.entries[index].revision += 1;
                self.status = "Responding".to_owned();
            }
            AgentEvent::Model(StreamEvent::ReasoningDelta(text)) => {
                let index = self.active_thinking.unwrap_or_else(|| {
                    self.entries.push(Entry::new(EntryKind::Thinking, ""));
                    let index = self.entries.len() - 1;
                    self.active_thinking = Some(index);
                    index
                });
                self.entries[index].text.push_str(&text);
                self.entries[index].revision += 1;
            }
            AgentEvent::MessageCommitted {
                message: ChatMessage::Assistant { content, .. },
            } => {
                if let Some(content) = content.filter(|text| !text.is_empty()) {
                    if let Some(index) = self.active_assistant {
                        self.entries[index].text = content;
                        self.entries[index].revision += 1;
                    } else {
                        self.entries.push(Entry::new(EntryKind::Assistant, content));
                    }
                }
            }
            AgentEvent::ToolStarted { call } => {
                self.add_tool(&call);
                self.active_assistant = None;
                self.status = "Using tools".to_owned();
            }
            AgentEvent::ToolFinished { call, content } => {
                self.finish_tool(&call.id, &content, false);
            }
            AgentEvent::ToolFailed { call, error } => {
                self.finish_tool(&call.id, &error, true);
            }
            AgentEvent::ContextCheckpoint { .. } => {
                self.notice("Context checkpoint saved");
            }
            AgentEvent::Completed { .. } => {
                self.running = false;
                self.status = "Ready".to_owned();
            }
            AgentEvent::Failed { error } => {
                self.running = false;
                self.status = "Failed".to_owned();
                self.error(error);
            }
            AgentEvent::Cancelled => {
                self.running = false;
                self.status = "Cancelled".to_owned();
            }
            AgentEvent::Model(StreamEvent::ToolCallDelta(_) | StreamEvent::Completed { .. })
            | AgentEvent::MessageCommitted { .. } => {}
        }
    }

    pub fn select_next_tool(&mut self) {
        let indices = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| (entry.kind == EntryKind::Tool).then_some(index))
            .collect::<Vec<_>>();
        if indices.is_empty() {
            return;
        }
        let next = self
            .selected_tool
            .and_then(|selected| indices.iter().position(|index| *index == selected))
            .map_or(0, |position| (position + 1) % indices.len());
        if let Some(selected) = self.selected_tool
            && let Some(entry) = self.entries.get_mut(selected)
        {
            entry.revision += 1;
        }
        self.selected_tool = Some(indices[next]);
        self.entries[indices[next]].revision += 1;
    }

    pub fn toggle_tool(&mut self) {
        if let Some(index) = self.selected_tool
            && let Some(entry) = self.entries.get_mut(index)
            && entry.kind == EntryKind::Tool
        {
            entry.expanded = !entry.expanded;
            entry.revision += 1;
        }
    }

    pub fn clear_tool_selection(&mut self) {
        if let Some(index) = self.selected_tool.take()
            && let Some(entry) = self.entries.get_mut(index)
        {
            entry.revision += 1;
        }
    }

    fn finish_tool(&mut self, id: &str, content: &str, failed: bool) {
        if let Some(entry) = self
            .tool_entries
            .get(id)
            .and_then(|index| self.entries.get_mut(*index))
        {
            let (output, preview) =
                super::tool_display::display(entry.tool_name.as_deref().unwrap_or(""), content);
            entry.output = output;
            entry.preview = preview;
            entry.finished = true;
            entry.failed = failed;
            entry.revision += 1;
        }
    }

    fn add_tool(&mut self, call: &ToolCall) {
        let mut entry = Entry::new(EntryKind::Tool, tool_label(call));
        entry.tool_id = Some(call.id.clone());
        entry.tool_name = Some(call.function.name.clone());
        self.tool_entries
            .insert(call.id.clone(), self.entries.len());
        self.entries.push(entry);
    }
}

fn user_display(content: &str, attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return content.to_owned();
    }
    let names = attachments
        .iter()
        .map(Attachment::name)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{content}\n[images: {names}]")
}

fn tool_label(call: &ToolCall) -> String {
    let arguments = call.arguments_json().ok();
    if call.function.name == "search_text"
        && let Some(arguments) = &arguments
        && let Some(query) = arguments.get("query").and_then(|value| value.as_str())
    {
        let path = arguments
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        return format!(
            "search_text  \"{}\"  {}",
            truncate(&query.replace('\n', " "), 60),
            truncate(path, 60)
        )
        .trim_end()
        .to_owned();
    }
    let target = arguments.and_then(|arguments| {
        ["query", "path", "command", "url", "title"]
            .into_iter()
            .find_map(|key| arguments.get(key).and_then(|value| value.as_str()))
            .map(|value| value.replace('\n', " "))
    });
    match target {
        Some(target) => format!("{}  {}", call.function.name, truncate(&target, 90)),
        None => call.function.name.clone(),
    }
}

fn truncate(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_model::types::ToolCall;
    use serde_json::json;

    #[test]
    fn streaming_events_replace_the_final_text_without_duplication() {
        let mut app = App::new(
            "session".into(),
            "workspace".into(),
            "model".into(),
            128_000,
        );
        app.submit("hello", &[]);
        app.apply(AgentEvent::TurnStarted { turn: 1 });
        app.apply(AgentEvent::Model(StreamEvent::TextDelta("hel".into())));
        app.apply(AgentEvent::Model(StreamEvent::TextDelta("lo".into())));
        app.apply(AgentEvent::MessageCommitted {
            message: ChatMessage::assistant("hello"),
        });
        assert_eq!(app.entries[1].text, "hello");

        let call = ToolCall::function("tool-1", "read_file", json!({ "path": "main.rs" }));
        app.apply(AgentEvent::ToolStarted { call: call.clone() });
        app.apply(AgentEvent::ToolFinished {
            call,
            content: "ok".into(),
        });
        assert_eq!(app.entries[2].output, "ok");
    }

    #[test]
    fn paged_restore_matches_tool_results_across_page_boundaries() {
        let mut app = App::new(
            "session".into(),
            "workspace".into(),
            "model".into(),
            128_000,
        );
        let call = ToolCall::function("tool-1", "read_file", json!({ "path": "main.rs" }));

        app.begin_restore();
        app.restore_page(&[ChatMessage::assistant_tool_calls(vec![call])]);
        assert!(app.restoring);
        app.restore_page(&[ChatMessage::tool("tool-1", "file content")]);
        app.finish_restore();

        assert!(!app.restoring);
        assert!(app.entries[0].finished);
        assert_eq!(app.entries[0].output, "file content");
        assert!(!app.entries[0].failed);
    }

    #[test]
    fn tool_cards_show_the_target_and_a_readable_collapsed_result() {
        let mut app = App::new(
            "session".into(),
            "workspace".into(),
            "model".into(),
            128_000,
        );
        let call = ToolCall::function(
            "tool-1",
            "search_text",
            json!({ "query": "fallback", "path": "src" }),
        );

        app.apply(AgentEvent::ToolStarted { call: call.clone() });
        app.apply(AgentEvent::ToolFinished {
            call,
            content: json!({
                "matches": [
                    { "path": "src/a.rs", "line": 1, "text": "fallback one" },
                    { "path": "src/a.rs", "line": 2, "text": "fallback two" }
                ],
                "has_more": false,
                "scan_truncated": false
            })
            .to_string(),
        });

        assert_eq!(app.entries[0].text, "search_text  \"fallback\"  src");
        assert!(app.entries[0].preview.starts_with("2 matches in 1 file"));
        assert!(app.entries[0].output.contains("> 1 │ fallback one"));
    }

    #[test]
    fn context_usage_moves_from_estimate_to_provider_reported_usage() {
        let mut app = App::new("session".into(), "workspace".into(), "model".into(), 32_000);

        app.apply(AgentEvent::ModelRequestPrepared {
            estimated_input_tokens: 1_400,
        });
        assert_eq!(app.context_used_tokens, 1_400);

        app.apply(AgentEvent::ModelUsageObserved {
            local_estimate: 1_400,
            estimated_input_tokens: 1_400,
            actual_input_tokens: Some(2_000),
        });
        assert_eq!(app.context_used_tokens, 2_000);
    }
}
