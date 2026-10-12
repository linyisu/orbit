//! A viewer-owned, append-only side question fixture with actual revisions.
use orbit_protocol::{Questions, SideQuestion};
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct FixtureQuestions {
    records: Vec<SideQuestion>,
    revision: u64,
    viewer: bool,
    pending: Option<Pending>,
}

struct Pending {
    index: usize,
    reads: usize,
    hold: bool,
    fail: bool,
    cancelled: bool,
}

impl FixtureQuestions {
    pub(super) fn running(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn execute(&mut self, method: &str, request: &Value, model: &str) -> Value {
        match method {
            "ask" => {
                let question = request["question"].as_str().unwrap_or_default().trim();
                if question.is_empty() || question.chars().count() > 32_768 {
                    return error("请输入有效的旁路问题");
                }
                if self.pending.is_some() {
                    return error("旁路问题仍在生成，请先结束当前请求。");
                }
                let follow_up = request["follow_up"].as_u64().map(|index| index as usize);
                let thread_id = if let Some(index) = follow_up {
                    if !self.viewer {
                        return error("旁路提问已关闭。请重新提问。");
                    }
                    let Some(record) = self.records.get(index) else {
                        return error("旁路问题记录不存在。");
                    };
                    record.thread_id.clone()
                } else {
                    format!("thread-{}", self.records.len())
                };
                let index = self.records.len();
                self.records.push(SideQuestion {
                    thread_id,
                    model: model.into(),
                    question: question.into(),
                    answer: String::new(),
                    status: "running".into(),
                    error: None,
                });
                self.pending = Some(Pending {
                    index,
                    reads: 0,
                    hold: question.contains("hold-side") || question.contains("慢速"),
                    fail: question.contains("fail-side") || question.contains("失败"),
                    cancelled: false,
                });
                self.viewer = true;
                self.revision += 1;
                self.snapshot(None, Some(index))
            }
            "open_questions" => {
                if self.records.is_empty() {
                    return error("没有正在查看的旁路对话，请先提出一个旁路问题。");
                }
                self.viewer = true;
                self.snapshot(None, self.records.len().checked_sub(1))
            }
            "read_questions" => {
                if !self.viewer {
                    return error("旁路提问已关闭。");
                }
                self.poll();
                self.snapshot(request["after"].as_u64(), None)
            }
            "cancel_question" => {
                if self.viewer
                    && let Some(pending) = &mut self.pending
                    && request["index"].as_u64() == Some(pending.index as u64)
                {
                    pending.cancelled = true;
                }
                json!({"type":"ok"})
            }
            "close_questions" => {
                self.viewer = false;
                self.records.clear();
                self.pending = None;
                self.revision += 1;
                json!({"type":"ok"})
            }
            _ => unreachable!(),
        }
    }

    fn snapshot(&self, after: Option<u64>, selected: Option<usize>) -> Value {
        json!({"type":"questions","value":Questions {
            revision: self.revision,
            selected,
            running: self.pending.is_some(),
            records: (after != Some(self.revision)).then(|| self.records.clone()),
        }})
    }

    fn poll(&mut self) {
        let Some(pending) = &mut self.pending else {
            return;
        };
        let record = &mut self.records[pending.index];
        if pending.cancelled {
            record.status = "cancelled".into();
            self.pending = None;
            self.revision += 1;
        } else if pending.reads == 0 {
            record.answer = if pending.hold {
                "正在生成，等待停止"
            } else {
                "旁路回答"
            }
            .into();
            pending.reads += 1;
            self.revision += 1;
        } else if !pending.hold {
            if pending.fail {
                record.status = "failed".into();
                record.error = Some("旁路回答连接中断，已保留生成内容。".into());
            } else {
                record.answer = "旁路回答已完成".into();
                record.status = "completed".into();
            }
            self.pending = None;
            self.revision += 1;
        }
    }
}

fn error(message: &str) -> Value {
    json!({"type":"error","value":message})
}
