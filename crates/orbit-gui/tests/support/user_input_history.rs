//! Database-shaped Agent input history: opaque row cursors, scoped records,
//! and upserts that retain a request's original history position.
use orbit_protocol::user_input::{Answer, Form, Kind, OptionItem, Question, Record, Status};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path};

#[derive(Default)]
pub(super) struct FixtureUserInputs {
    records: HashMap<String, Vec<(i64, Record)>>,
    live: HashMap<String, String>,
    next_row: i64,
}

impl FixtureUserInputs {
    pub fn prepare(&mut self, workspace: &str, session: &str) {
        if session != "demo"
            || self.records.contains_key(session)
            || !Path::new(workspace)
                .join(".fixture-user-input-history")
                .exists()
        {
            return;
        }
        for index in 1..=45 {
            let mut record = question(false);
            record.id = format!("history-request-{index}");
            record.call_id = format!("history-call-{index}");
            record.form.questions[0].title = format!("第 {index} 次实现选择");
            record.form.questions[0].prompt = format!(
                "第 {index} 次检查：请选择实现方式。此记录用于验证加载更早记录后保留当前选择。"
            );
            record.status = match index % 5 {
                0 => Status::Declined,
                1 => Status::Answered,
                2 => Status::Cancelled,
                3 => Status::Interrupted,
                _ => Status::Answered,
            };
            if record.status == Status::Answered {
                record.answers.push(Answer {
                    question_id: "choice".into(),
                    selected: vec!["small".into()],
                    note: format!("第 {index} 次补充说明"),
                    ..Default::default()
                });
            }
            self.save(session, record);
        }
    }

    fn save(&mut self, session: &str, record: Record) {
        let records = self
            .records
            .entry(session.into())
            .or_default();
        if let Some((_, existing)) = records
            .iter_mut()
            .find(|(_, item)| item.id == record.id)
        {
            *existing = record;
        } else {
            // Deliberate gaps ensure consumers use the cursor, not a count or
            // the numeric suffix of a domain request ID.
            self.next_row += 7;
            records.push((self.next_row, record));
        }
    }

    pub fn start(&mut self, workspace: &str, session: &str, multiple: bool) {
        self.prepare(workspace, session);
        let record = question(multiple);
        self.live
            .insert(session.into(), record.id.clone());
        self.save(session, record);
    }

    pub fn live(&self, session: &str) -> Vec<Record> {
        self.records
            .get(session)
            .into_iter()
            .flatten()
            .filter(|(_, record)| self.live.get(session) == Some(&record.id))
            .map(|(_, record)| record.clone())
            .collect()
    }

    pub fn answer(&mut self, request: &Value) -> Value {
        let session = request["session"]
            .as_str()
            .unwrap_or_default();
        let id = request["request"]
            .as_str()
            .unwrap_or_default();
        let Some((_, record)) = self
            .records
            .get_mut(session)
            .into_iter()
            .flatten()
            .find(|(_, record)| record.id == id)
        else {
            return json!({
                "type": "error",
                "value": "Agent 问题不存在"
            });
        };
        if record.status != Status::Pending {
            return json!({
                "type": "error",
                "value": "Agent 问题已结束"
            });
        }
        if request["answers"].is_null() {
            record.status = Status::Declined;
            record.answers.clear();
        } else {
            let answers: Vec<Answer> = match serde_json::from_value(request["answers"].clone()) {
                Ok(answers) => answers,
                Err(error) => {
                    return json!({
                        "type": "error",
                        "value": error.to_string()
                    });
                }
            };
            if let Err(error) = record.form.validate_answers(&answers) {
                return json!({
                    "type": "error",
                    "value": error
                });
            }
            record.status = Status::Answered;
            record.answers = answers;
        }
        json!({
            "type": "ok"
        })
    }

    pub fn history(&mut self, workspace: &str, session: &str, before: Option<i64>) -> Value {
        self.prepare(workspace, session);
        if before.is_some()
            && std::fs::remove_file(
                Path::new(workspace).join(".fixture-user-input-history-fail-once"),
            )
            .is_ok()
        {
            return json!({
                "type": "error",
                "value": "测试历史页暂时不可用，请重试。"
            });
        }
        let mut records: Vec<_> = self
            .records
            .get(session)
            .into_iter()
            .flatten()
            .rev()
            .filter(|(row, _)| before.is_none_or(|before| *row < before))
            .take(21)
            .collect();
        let more = records.len() > 20;
        records.truncate(20);
        let next = more.then(|| records.last().unwrap().0);
        let records: Vec<_> = records
            .into_iter()
            .map(|(_, record)| record)
            .collect();
        json!({
            "type": "user_inputs",
            "value": {
                "before": before,
                "records": records,
                "next": next
            }
        })
    }
}

fn question(multiple: bool) -> Record {
    let mut questions = vec![Question {
        id: "choice".into(),
        title: "实现方式".into(),
        prompt: "选择实现方式".into(),
        kind: Kind::Single,
        options: vec![
            OptionItem {
                id: "small".into(),
                label: "小步迭代".into(),
                description: String::new(),
            },
            OptionItem {
                id: "full".into(),
                label: "完整迁移".into(),
                description: String::new(),
            },
        ],
        allow_custom: true,
        required: true,
    }];
    if multiple {
        questions.push(Question {
            id: "details".into(),
            title: "补充要求".into(),
            prompt: "还需要哪些内容？可选择多项，也可以补充说明。".into(),
            kind: Kind::Multiple,
            options: vec![
                OptionItem {
                    id: "docs".into(),
                    label: "说明文档".into(),
                    description: "记录使用方法与设计决定".into(),
                },
                OptionItem {
                    id: "tests".into(),
                    label: "回归验证".into(),
                    description: String::new(),
                },
            ],
            allow_custom: true,
            required: true,
        });
    }
    Record {
        id: "question-1".into(),
        call_id: "question-call".into(),
        source: None,
        status: Status::Pending,
        answers: vec![],
        form: Form { questions },
    }
}
