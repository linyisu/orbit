//! Project transport events into user turns, process records, and conclusions.
//! Model request boundaries are internal to a turn; only user input starts one.
use orbit_client::protocol::Update;
use orbit_client::protocol::events::AgentEvent;
use orbit_client::protocol::model::StreamEvent;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Text,
    Thought,
    Tool,
}

pub(super) struct Part {
    pub(super) kind: Kind,
    pub(super) title: String,
    pub(super) content: String,
    pub(super) running: bool,
}

#[derive(Default)]
pub(super) struct Turn {
    pub(super) prompt: Option<String>,
    pub(super) images: Vec<String>,
    pub(super) files: Vec<orbit_protocol::MessageImage>,
    pub(super) parts: Vec<Part>,
    pub(super) status: String,
    pub(super) completed: bool,
    text: Option<usize>,
    thought: Option<usize>,
}

impl Turn {
    pub(super) fn reply(&self) -> String {
        self.parts
            .iter()
            .filter(|part| part.kind == Kind::Text && !part.content.is_empty())
            .map(|part| part.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Reasoning and reports may arrive after prose in the same stream. Only another tool call
    /// moves that answer into the process history.
    pub(super) fn conclusion(&self) -> Option<usize> {
        self.parts
            .iter()
            .rposition(|part| matches!(part.kind, Kind::Text | Kind::Tool))
            .filter(|index| self.parts[*index].kind == Kind::Text)
    }

    pub(super) fn process_indices(&self) -> impl Iterator<Item = usize> + '_ {
        let conclusion = self.conclusion();
        (0..self.parts.len()).filter(move |index| Some(*index) != conclusion)
    }

    pub(super) fn process_count(&self) -> usize {
        self.parts.len() - usize::from(self.conclusion().is_some())
    }

    pub(super) fn label(&self) -> String {
        let calls = self
            .parts
            .iter()
            .filter(|p| p.kind == Kind::Tool)
            .count();
        if calls == 0 {
            format!("{} · 思考过程", self.status)
        } else {
            format!("{} · {calls} 次工具调用", self.status)
        }
    }
}

#[derive(Default)]
pub(super) struct Transcript {
    pub(super) turns: Vec<Turn>,
    active: Option<usize>,
    tools: HashMap<String, (usize, usize)>,
}
pub(super) struct Change {
    pub(super) turn: usize,
    pub(super) part: Option<usize>,
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

    pub(super) fn apply(&mut self, update: Update) -> Option<Change> {
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
                }
                self.active = None;
                Some(Change { turn, part: None })
            }
            Update::Report(report) => {
                let turn = self.current();
                let part = self.push(turn, Kind::Thought, "提示".into(), report.text, false);
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
                self.turns[turn].text = None;
                self.turns[turn].thought = None;
                None
            }
            AgentEvent::Model(StreamEvent::TextDelta(text)) => self.delta(text, false),
            AgentEvent::Model(StreamEvent::ReasoningDelta(text)) => self.delta(text, true),
            AgentEvent::ToolStarted { call } => {
                let turn = self.current();
                self.turns[turn].text = None;
                self.turns[turn].thought = None;
                self.turns[turn].status = "执行中".into();
                let part = self.push(
                    turn,
                    Kind::Tool,
                    format!("执行中 · {}", call.function.name),
                    format!("```json\n{}\n```", call.function.arguments),
                    true,
                );
                self.tools.insert(call.id, (turn, part));
                Some(Change {
                    turn,
                    part: Some(part),
                })
            }
            AgentEvent::ToolFinished { call, content, .. } => {
                self.finish(call.id, call.function.name, content, "完成")
            }
            AgentEvent::ToolBackgrounded { call, content, .. } => {
                self.finish(call.id, call.function.name, content, "后台运行")
            }
            AgentEvent::ToolFailed { call, error } => {
                self.finish(call.id, call.function.name, error, "失败")
            }
            _ => None,
        }
    }

    fn delta(&mut self, text: String, thinking: bool) -> Option<Change> {
        if text.is_empty() || (self.active.is_none() && text.trim().is_empty()) {
            return None;
        }
        let turn = self.current();
        let current = if thinking {
            self.turns[turn].thought
        } else {
            self.turns[turn].text
        };
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
                false,
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
            part: Some(part),
        })
    }

    /// Keep the owning turn even when a background tool completes after new input.
    fn finish(&mut self, id: String, name: String, text: String, status: &str) -> Option<Change> {
        let (turn, part) = if let Some(location) = self.tools.get(&id) {
            *location
        } else {
            let turn = self.current();
            let part = self.push(turn, Kind::Tool, String::new(), String::new(), false);
            self.tools.insert(id, (turn, part));
            (turn, part)
        };
        let record = &mut self.turns[turn].parts[part];
        record.title = format!("{status} · {name}");
        record.running = false;
        record
            .content
            .push_str(&format!("\n\n```text\n{text}\n```"));
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
                "name": "read_file",
                "arguments": "{}"
            }
        })
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
        assert_eq!(turn.process_count(), 24);
        assert_eq!(turn.parts[turn.conclusion().unwrap()].content, "检查完成。");
        assert!(
            turn.parts
                .iter()
                .all(|part| !part.content.trim().is_empty())
        );
        assert_eq!(turn.label(), "已完成 · 12 次工具调用");
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
        assert_eq!(turn.process_count(), 2);
        assert_eq!(turn.parts[turn.conclusion().unwrap()].content, "修复完成。");
    }

    #[test]
    fn mixed_reasoning_keeps_the_answer_visible_until_execution_resumes() {
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
        assert_eq!(turn.conclusion(), Some(1));
        assert_eq!(turn.parts[1].content, "检查完成。");
        assert_eq!(
            turn.process_indices()
                .collect::<Vec<_>>(),
            vec![0, 2, 3]
        );
        assert_eq!(turn.process_count(), 3);

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
            vec![0, 1, 2, 3, 4]
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
        assert_eq!(turn.conclusion(), Some(5));
        assert_eq!(turn.parts[5].content, "再次检查完成。");
        assert_eq!(
            turn.process_indices()
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 6]
        );
        assert_eq!(turn.process_count(), 6);
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
        assert_eq!(transcript.turns[0].parts[0].title, "完成 · read_file");
        assert!(
            transcript.turns[0].parts[0]
                .content
                .contains("后台检查")
        );
        assert!(
            transcript.turns[0].parts[0]
                .content
                .contains("检查成功")
        );
        assert_eq!(transcript.turns[0].parts[1].title, "已停止 · read_file");
    }
}
