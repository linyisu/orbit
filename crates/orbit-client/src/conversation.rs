//! Project transport events into user turns, process records, and conclusions.
//! Model request boundaries are internal to a turn; only user input starts one.
use crate::protocol::Update;
use crate::protocol::events::AgentEvent;
use crate::protocol::model::StreamEvent;
use crate::protocol::model::ToolCall;
use std::collections::HashMap;
pub mod layout;
use crate::execution::{ToolRun, ToolStatus};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Thought,
    Tool,
    Notice,
}

pub struct Part {
    pub kind: Kind,
    pub title: String,
    pub content: String,
    pub running: bool,
    pub tool: Option<ToolRun>,
}

#[derive(Default)]
pub struct Turn {
    pub prompt: Option<String>,
    pub images: Vec<String>,
    pub files: Vec<orbit_protocol::MessageImage>,
    pub parts: Vec<Part>,
    pub status: String,
    pub completed: bool,
    text: Option<usize>,
    thought: Option<usize>,
}

impl Turn {
    /// Keep confirmed write receipts with their original calls for historical change navigation.
    pub fn changed_tools(&self) -> Vec<ToolRun> {
        self.parts
            .iter()
            .filter_map(|part| part.tool.as_ref())
            .filter(|tool| !tool.changes.is_empty())
            .cloned()
            .collect()
    }

    /// Projects source parts without reordering or hiding intermediate prose.
    pub fn segments(&self) -> Vec<layout::Segment> {
        let roles = self
            .parts
            .iter()
            .map(|part| match part.kind {
                Kind::Text => layout::Role::Text,
                Kind::Thought => layout::Role::Thought,
                Kind::Tool => layout::Role::Tool,
                Kind::Notice => layout::Role::Notice,
            })
            .collect::<Vec<_>>();
        layout::partition(&roles).segments
    }

    pub fn reply(&self) -> String {
        (0..self.parts.len())
            .filter(|index| self.parts[*index].kind == Kind::Text)
            .map(|index| self.parts[index].content.as_str())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Reasoning and reports may arrive after prose in the same stream. Only another tool call
    /// moves that answer into the process history.
    pub fn conclusion(&self) -> Option<usize> {
        layout::conclusion(
            self.parts
                .iter()
                .enumerate()
                .map(|(index, part)| {
                    let role = match part.kind {
                        Kind::Text => layout::Role::Text,
                        Kind::Thought => layout::Role::Thought,
                        Kind::Tool => layout::Role::Tool,
                        Kind::Notice => layout::Role::Notice,
                    };
                    (index, role)
                }),
        )
    }

    pub fn process_indices(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.parts.len()).filter(|index| self.parts[*index].kind == Kind::Tool)
    }

    pub fn conclusions(&self) -> impl Iterator<Item = usize> + '_ {
        let start = self.answer_start();
        (start..self.parts.len()).filter(|index| self.parts[*index].kind == Kind::Text)
    }

    fn answer_start(&self) -> usize {
        layout::answer_start(
            self.parts
                .iter()
                .enumerate()
                .map(|(index, part)| {
                    (
                        index,
                        if part.kind == Kind::Tool {
                            layout::Role::Tool
                        } else {
                            layout::Role::Notice
                        },
                    )
                }),
        )
    }

    pub fn process_count(&self) -> usize {
        self.process_indices().count()
    }

    pub fn thoughts(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.parts.len())
            .filter(|index| matches!(self.parts[*index].kind, Kind::Thought | Kind::Notice))
    }

    pub fn tools_running(&self) -> bool {
        self.parts
            .iter()
            .any(|part| part.kind == Kind::Tool && part.running)
    }

    /// Plain progress remains available independently of reasoning and tool output.
    pub fn progress(&self) -> impl Iterator<Item = &str> {
        let start = self.answer_start();
        self.parts
            .iter()
            .enumerate()
            .filter_map(move |(index, part)| {
                (part.kind == Kind::Text && index < start).then_some(part.content.as_str())
            })
    }

    pub fn label(&self) -> String {
        crate::execution::batch_label(self.parts.iter().filter_map(|part| {
            part.tool
                .as_ref()
                .map(|tool| tool.status)
        }))
    }
}

#[derive(Default)]
pub struct Transcript {
    pub turns: Vec<Turn>,
    active: Option<usize>,
    tools: HashMap<String, (usize, usize)>,
}
pub struct Change {
    pub turn: usize,
    pub part: Option<usize>,
}

impl Transcript {
    fn current(&mut self) -> usize {
        if let Some(index) = self.active {
            return index;
        }
        self.begin(None, vec![])
    }

    fn begin(&mut self, prompt: Option<String>, images: Vec<String>) -> usize {
        let index = self.turns.len();
        self.turns.push(Turn {
            prompt,
            images,
            status: "正在处理".into(),
            ..Default::default()
        });
        self.active = Some(index);
        index
    }

    pub fn apply(&mut self, update: Update) -> Option<Change> {
        match update {
            Update::Started {
                prompt,
                presentation,
            } => {
                if presentation.compact || (presentation.queued_input && prompt.is_empty()) {
                    return None;
                }
                let turn = self.begin(
                    Some(prompt),
                    presentation
                        .image_assets
                        .into_iter()
                        .map(|a| a.path)
                        .collect(),
                );
                self.turns[turn].files = presentation.file_assets;
                Some(Change { turn, part: None })
            }
            Update::Agent(event) => self.agent(*event),
            Update::Finished { status, .. } => {
                let turn = self.active?;
                let run = &mut self.turns[turn];
                run.completed = status == "completed";
                run.status = match status.as_str() {
                    "completed" => "已完成",
                    "cancelled" => "已停止",
                    _ => "执行失败",
                }
                .into();
                for part in &mut run.parts {
                    if part.running {
                        part.running = false;
                        part.title = part
                            .title
                            .replacen("执行中", &run.status, 1);
                    }
                    if let Some(tool) = &mut part.tool {
                        tool.run_finished = true;
                        if tool.status == ToolStatus::Running {
                            tool.status = ToolStatus::Stopped;
                        }
                    }
                }
                self.active = None;
                Some(Change { turn, part: None })
            }
            Update::Report(report) => {
                let turn = self.current();
                let part = self.push(turn, Kind::Notice, "提示".into(), report.text, false);
                Some(Change {
                    turn,
                    part: Some(part),
                })
            }
            Update::Profile(_) => None,
        }
    }

    fn push(
        &mut self,
        turn: usize,
        kind: Kind,
        title: String,
        content: String,
        running: bool,
    ) -> usize {
        let index = self.turns[turn].parts.len();
        self.turns[turn].parts.push(Part {
            kind,
            title,
            content,
            running,
            tool: None,
        });
        index
    }

    fn agent(&mut self, event: AgentEvent) -> Option<Change> {
        match event {
            AgentEvent::InputReceived {
                content,
                attachments,
                ..
            } => {
                let images = attachments
                    .iter()
                    .filter(|a| a.is_image())
                    .map(|image| {
                        image
                            .path()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect();
                let turn = self.begin(Some(content), images);
                self.turns[turn].files = attachments
                    .iter()
                    .filter(|a| !a.is_image())
                    .map(|a| orbit_protocol::MessageImage {
                        name: a.name().into(),
                        path: a.path().to_string_lossy().into_owned(),
                    })
                    .collect();
                Some(Change { turn, part: None })
            }
            AgentEvent::TurnStarted { .. } => {
                let turn = self.current();
                self.finish_reasoning(turn);
                self.turns[turn].text = None;
                Some(Change { turn, part: None })
            }
            AgentEvent::Model(StreamEvent::TextDelta(text)) => self.delta(text, false),
            AgentEvent::Model(StreamEvent::ReasoningDelta(text)) => self.delta(text, true),
            AgentEvent::ReasoningFinished { .. } => {
                let turn = self.active?;
                let part = self.turns[turn].thought.take()?;
                self.turns[turn].parts[part].running = false;
                Some(Change {
                    turn,
                    part: Some(part),
                })
            }
            AgentEvent::ToolStarted { call } => {
                let turn = self.current();
                self.finish_reasoning(turn);
                self.turns[turn].text = None;
                self.turns[turn].status = "执行中".into();
                let part = self.push(
                    turn,
                    Kind::Tool,
                    format!("执行中 · {}", call.function.name),
                    String::new(),
                    true,
                );
                self.turns[turn].parts[part].tool = Some(ToolRun::new(&call));
                self.tools.insert(call.id, (turn, part));
                Some(Change { turn, part: None })
            }
            AgentEvent::ToolFinished {
                call,
                content,
                changes,
                outcome,
                ..
            } => {
                let change = self.finish(call, content, ToolStatus::Complete, outcome)?;
                if let Some(part) = change.part
                    && let Some(tool) = &mut self.turns[change.turn].parts[part].tool
                {
                    tool.changes = changes;
                }
                Some(change)
            }
            AgentEvent::ToolBackgrounded { call, content, .. } => {
                self.finish(call, content, ToolStatus::Background, None)
            }
            AgentEvent::ToolFailed { call, error } => {
                self.finish(call, error, ToolStatus::Failed, None)
            }
            _ => None,
        }
    }

    fn delta(&mut self, text: String, thinking: bool) -> Option<Change> {
        if text.is_empty() || (self.active.is_none() && text.trim().is_empty()) {
            return None;
        }
        let turn = self.current();
        let closed_thought = !thinking && self.finish_reasoning(turn);
        let current = if thinking {
            self.turns[turn].thought
        } else {
            self.turns[turn].text
        }
        .filter(|index| *index + 1 == self.turns[turn].parts.len());
        if text.trim().is_empty() && current.is_none() {
            return None;
        }
        let part = if let Some(part) = current {
            self.turns[turn].parts[part]
                .content
                .push_str(&text);
            part
        } else {
            let part = self.push(
                turn,
                if thinking { Kind::Thought } else { Kind::Text },
                if thinking { "思考" } else { "过程说明" }.into(),
                text,
                thinking,
            );
            if thinking {
                self.turns[turn].thought = Some(part);
            } else {
                self.turns[turn].text = Some(part);
            }
            part
        };
        Some(Change {
            turn,
            part: (!closed_thought).then_some(part),
        })
    }

    fn finish_reasoning(&mut self, turn: usize) -> bool {
        if let Some(index) = self.turns[turn].thought.take() {
            self.turns[turn].parts[index].running = false;
            true
        } else {
            false
        }
    }

    /// Keep the owning turn even when a background tool completes after new input.
    fn finish(
        &mut self,
        call: ToolCall,
        text: String,
        status: ToolStatus,
        outcome: Option<orbit_agent::tool::ToolOutcome>,
    ) -> Option<Change> {
        let (turn, part) = if let Some(location) = self.tools.get(&call.id) {
            *location
        } else {
            let turn = self.current();
            let part = self.push(turn, Kind::Tool, String::new(), String::new(), false);
            self.tools
                .insert(call.id.clone(), (turn, part));
            (turn, part)
        };
        let record = &mut self.turns[turn].parts[part];
        record.running = false;
        let tool = record
            .tool
            .get_or_insert_with(|| ToolRun::new(&call));
        tool.set_receipt(status, text, outcome);
        record.title = format!("{} · {}", tool.status.label(), tool.name);
        Some(Change {
            turn,
            part: Some(part),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn apply(transcript: &mut Transcript, event: Value) {
        transcript.apply(serde_json::from_value(event).unwrap());
    }

    fn call(id: &str) -> Value {
        json!({
            "id": id,
            "function": {
                "name": "ReadFile",
                "arguments": "{}"
            }
        })
    }

    #[test]
    fn historical_changes_keep_each_turns_original_receipts() {
        let mut transcript = Transcript::default();
        for (id, before, after) in [("write-a", "old", "first"), ("write-b", "first", "second")] {
            apply(
                &mut transcript,
                json!({"Started": {"prompt": "Generated request"}}),
            );
            let call = ToolCall::function(id, "WriteFile", json!({"path": "sample.txt"}));
            transcript.agent(AgentEvent::ToolStarted { call: call.clone() });
            assert!(
                transcript
                    .turns
                    .last()
                    .unwrap()
                    .changed_tools()
                    .is_empty()
            );
            transcript.agent(AgentEvent::ToolFinished {
                outcome: None,
                call,
                content: "saved".into(),
                files: None,
                changes: vec![crate::execution::FileChange {
                    path: "sample.txt".into(),
                    before: Some(before.into()),
                    after: after.into(),
                }],
            });
            apply(
                &mut transcript,
                json!({"Finished": {"status": "completed"}}),
            );
        }
        let first = transcript.turns[0].changed_tools();
        let second = transcript.turns[1].changed_tools();
        assert_eq!(first[0].id, "write-a");
        assert_eq!(first[0].changes[0].after, "first");
        assert_eq!(second[0].id, "write-b");
        assert_eq!(second[0].changes[0].before.as_deref(), Some("first"));
    }

    #[test]
    fn reasoning_and_tool_batches_have_independent_lifetimes() {
        let mut transcript = Transcript::default();
        transcript.agent(AgentEvent::Model(StreamEvent::ReasoningDelta(
            "Generated analysis".into(),
        )));
        assert!(transcript.turns[0].parts[0].running);
        transcript.agent(AgentEvent::ReasoningFinished { elapsed_ms: 7 });
        assert!(!transcript.turns[0].parts[0].running);
        let call = ToolCall::function("fixture-call", "inspect", json!({"path": "generated.rs"}));
        transcript.agent(AgentEvent::ToolStarted { call: call.clone() });
        assert!(transcript.turns[0].tools_running());
        transcript.agent(AgentEvent::ToolBackgrounded {
            call: call.clone(),
            job_id: "fixture-job".into(),
            content: "scheduled".into(),
        });
        assert!(!transcript.turns[0].tools_running());
        transcript.agent(AgentEvent::ToolFinished {
            outcome: None,
            call,
            content: "done".into(),
            changes: vec![],
            files: None,
        });
        let tool = transcript.turns[0].parts[1]
            .tool
            .as_ref()
            .unwrap();
        assert_eq!(tool.status, ToolStatus::Complete);
        assert_eq!(tool.output.as_deref(), Some("done"));
        assert_eq!(
            transcript.turns[0]
                .thoughts()
                .collect::<Vec<_>>(),
            [0]
        );
    }

    #[test]
    fn queued_continuation_has_no_empty_or_duplicate_user_turn() {
        let mut transcript = Transcript::default();
        apply(
            &mut transcript,
            json!({ "Started": { "prompt": "generated request" } }),
        );
        apply(
            &mut transcript,
            json!({ "Finished": { "status": "cancelled" } }),
        );
        apply(
            &mut transcript,
            json!({
                "Started": { "prompt": "", "presentation": { "queued_input": true } }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": { "InputReceived": {
                    "message_id": 73, "content": "generated follow-up", "attachments": []
                } }
            }),
        );
        assert_eq!(transcript.turns.len(), 2);
        assert_eq!(
            transcript.turns[1].prompt.as_deref(),
            Some("generated follow-up")
        );
    }

    #[test]
    fn copied_reply_preserves_all_prose_and_markdown_without_process_records() {
        let mut turn = Turn::default();
        for (kind, content) in [
            (Kind::Text, "**first**\n```rust\nlet x = 1;\n```"),
            (Kind::Thought, "reasoning"),
            (Kind::Tool, "tool output"),
            (Kind::Text, "final answer"),
        ] {
            turn.parts.push(Part {
                kind,
                title: String::new(),
                content: content.into(),
                running: false,
                tool: None,
            });
        }
        assert_eq!(
            turn.reply(),
            "**first**\n```rust\nlet x = 1;\n```\n\nfinal answer"
        );
    }

    #[test]
    fn request_boundaries_and_empty_deltas_do_not_split_a_user_turn() {
        let mut transcript = Transcript::default();
        apply(
            &mut transcript,
            json!({
                "Started": {
                    "prompt": "检查项目"
                }
            }),
        );
        for index in 0..12 {
            apply(
                &mut transcript,
                json!({
                    "Agent": {
                        "TurnStarted": {
                            "turn": index
                        }
                    }
                }),
            );
            apply(
                &mut transcript,
                json!({
                    "Agent": {
                        "Model": {
                            "TextDelta": ""
                        }
                    }
                }),
            );
            apply(
                &mut transcript,
                json!({
                    "Agent": {
                        "Model": {
                            "TextDelta": "  \n"
                        }
                    }
                }),
            );
            apply(
                &mut transcript,
                json!({
                    "Agent": {
                        "Model": {
                            "ReasoningDelta": "检查依赖"
                        }
                    }
                }),
            );
            let call = call(&index.to_string());
            apply(
                &mut transcript,
                json!({
                    "Agent": {
                        "ToolStarted": {
                            "call": call
                        }
                    }
                }),
            );
            apply(
                &mut transcript,
                json!({
                    "Agent": {
                        "ToolFinished": {
                            "call": call,
                            "content": "文件内容",
                            "changes": []
                        }
                    }
                }),
            );
        }
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "Model": {
                        "TextDelta": "检查完成。"
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Finished": {
                    "status": "completed"
                }
            }),
        );
        assert_eq!(transcript.turns.len(), 1);
        let turn = &transcript.turns[0];
        assert_eq!(turn.process_count(), 12);
        assert_eq!(turn.parts[turn.conclusion().unwrap()].content, "检查完成。");
        assert!(
            turn.parts
                .iter()
                .all(|part| part.tool.is_some() || !part.content.trim().is_empty())
        );
        assert_eq!(turn.label(), "工具调用 · 12 次");
    }

    #[test]
    fn commentary_moves_into_process_when_tools_start_and_conclusion_streams() {
        let mut transcript = Transcript::default();
        apply(
            &mut transcript,
            json!({
                "Started": {
                    "prompt": "修复问题"
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "Model": {
                        "TextDelta": "先检查源码。"
                    }
                }
            }),
        );
        assert_eq!(transcript.turns[0].conclusion(), Some(0));
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "ToolStarted": {
                        "call": call("1")
                    }
                }
            }),
        );
        assert_eq!(transcript.turns[0].conclusion(), None);
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "Model": {
                        "TextDelta": "修复"
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "Model": {
                        "TextDelta": "完成。"
                    }
                }
            }),
        );
        let turn = &transcript.turns[0];
        assert_eq!(turn.process_count(), 1);
        assert_eq!(turn.parts[turn.conclusion().unwrap()].content, "修复完成。");
    }

    #[test]
    fn reasoning_between_prose_keeps_distinct_chronological_segments() {
        let mut transcript = Transcript::default();
        apply(
            &mut transcript,
            json!({
                "Started": {
                    "prompt": "检查项目"
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "ToolStarted": {
                        "call": call("first")
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "ToolFinished": {
                        "call": call("first"),
                        "content": "文件内容",
                        "changes": []
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "Model": {
                        "TextDelta": "检查"
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "Model": {
                        "ReasoningDelta": "补充思考"
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "Model": {
                        "TextDelta": "完成。"
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Report": {
                    "title": "提示",
                    "text": "补充提示"
                }
            }),
        );
        let turn = &transcript.turns[0];
        assert_eq!(turn.conclusion(), Some(3));
        assert_eq!(turn.parts[1].content, "检查");
        assert_eq!(turn.parts[3].content, "完成。");
        assert_eq!(
            turn.process_indices()
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(turn.process_count(), 1);

        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "ToolStarted": {
                        "call": call("second")
                    }
                }
            }),
        );
        let turn = &transcript.turns[0];
        assert_eq!(turn.conclusion(), None);
        assert_eq!(
            turn.process_indices()
                .collect::<Vec<_>>(),
            vec![0, 5]
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "Model": {
                        "TextDelta": "再次检查完成。"
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "Model": {
                        "ReasoningDelta": "后续思考"
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "ToolFinished": {
                        "call": call("second"),
                        "content": "检查结果",
                        "changes": []
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Finished": {
                    "status": "completed"
                }
            }),
        );
        let turn = &transcript.turns[0];
        assert_eq!(turn.conclusion(), Some(6));
        assert_eq!(turn.parts[6].content, "再次检查完成。");
        assert_eq!(
            turn.process_indices()
                .collect::<Vec<_>>(),
            vec![0, 5]
        );
        assert_eq!(turn.process_count(), 2);
    }

    #[test]
    fn background_completion_stays_with_its_original_user_turn() {
        let mut transcript = Transcript::default();
        apply(
            &mut transcript,
            json!({
                "Started": {
                    "prompt": "第一轮"
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "ToolStarted": {
                        "call": call("a")
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "ToolStarted": {
                        "call": call("b")
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "ToolBackgrounded": {
                        "call": call("a"),
                        "job_id": "job",
                        "content": "后台检查"
                    }
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Finished": {
                    "status": "cancelled"
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Started": {
                    "prompt": "第二轮"
                }
            }),
        );
        apply(
            &mut transcript,
            json!({
                "Agent": {
                    "ToolFinished": {
                        "call": call("a"),
                        "content": "检查成功",
                        "changes": []
                    }
                }
            }),
        );
        assert_eq!(transcript.turns.len(), 2);
        assert!(transcript.turns[1].parts.is_empty());
        assert_eq!(transcript.turns[0].parts[0].title, "已返回 · ReadFile");
        assert_eq!(
            transcript.turns[0].parts[0]
                .tool
                .as_ref()
                .unwrap()
                .output
                .as_deref(),
            Some("检查成功")
        );
        assert_eq!(transcript.turns[0].parts[1].title, "已停止 · ReadFile");
    }
}
