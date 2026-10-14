//! Agent-initiated questions and user-authored answers.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Single,
    Multiple,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionItem {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub kind: Kind,
    #[serde(default)]
    pub options: Vec<OptionItem>,
    #[serde(default = "yes")]
    pub allow_custom: bool,
    #[serde(default = "yes")]
    pub required: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Form {
    pub questions: Vec<Question>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub question_id: String,
    #[serde(default)]
    pub selected: Vec<String>,
    /// Free text for text questions or the custom choice.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Answered,
    Declined,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub call_id: String,
    pub source: Option<String>,
    pub form: Form,
    pub status: Status,
    pub answers: Vec<Answer>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub before: Option<i64>,
    pub records: Vec<Record>,
    pub next: Option<i64>,
}

impl Form {
    pub fn validate(&self) -> Result<(), String> {
        if self.questions.is_empty() || self.questions.len() > 8 {
            return Err("每组问答需要 1～8 道题".into());
        }
        let mut ids = HashSet::new();
        for q in &self.questions {
            if q.id.trim().is_empty()
                || q.id.len() > 64
                || !ids.insert(&q.id)
                || q.title.trim().is_empty()
                || q.title.chars().count() > 30
                || q.prompt.trim().is_empty()
                || q.prompt.len() > 4_096
            {
                return Err("题目需要唯一 ID、简短标题和完整问题".into());
            }
            if (q.kind == Kind::Text && !q.options.is_empty())
                || (q.kind != Kind::Text && !(2..=8).contains(&q.options.len()))
            {
                return Err("选择题需要 2～8 个选项，填空题无需选项".into());
            }
            let mut options = HashSet::new();
            for o in &q.options {
                if o.id.trim().is_empty()
                    || o.id.len() > 64
                    || !options.insert(&o.id)
                    || o.label.trim().is_empty()
                    || o.label.len() > 256
                    || o.description.len() > 1_024
                {
                    return Err("选项需要唯一 ID 和有效名称".into());
                }
            }
        }
        Ok(())
    }

    pub fn validate_answers(&self, answers: &[Answer]) -> Result<(), String> {
        if answers.len() != self.questions.len() {
            return Err("请检查每道题的答案".into());
        }
        let mut ids = HashSet::new();
        for a in answers {
            let q = self
                .questions
                .iter()
                .find(|q| q.id == a.question_id)
                .ok_or("答案包含未知题目")?;
            if !ids.insert(&a.question_id) || a.text.len() > 16_384 || a.note.len() > 8_192 {
                return Err("答案重复或文字过长".into());
            }
            let selected: HashSet<_> = a.selected.iter().collect();
            if selected.len() != a.selected.len()
                || a.selected
                    .iter()
                    .any(|id| !q.options.iter().any(|o| &o.id == id))
            {
                return Err("答案包含无效或重复选项".into());
            }
            let text = !a.text.trim().is_empty();
            if (q.kind == Kind::Text && !selected.is_empty())
                || (q.kind != Kind::Text && !q.allow_custom && text)
                || (q.kind == Kind::Single && selected.len() + usize::from(text) > 1)
            {
                return Err(format!("请检查「{}」的回答方式", q.title));
            }
            if q.required && selected.is_empty() && !text {
                return Err(format!("请回答「{}」", q.title));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn form(kind: Kind) -> Form {
        Form {
            questions: vec![Question {
                id: "q".into(),
                title: "目的地".into(),
                prompt: "选择目的地".into(),
                kind,
                options: if kind == Kind::Text {
                    vec![]
                } else {
                    vec![
                        OptionItem {
                            id: "a".into(),
                            label: "甲".into(),
                            description: String::new(),
                        },
                        OptionItem {
                            id: "b".into(),
                            label: "乙".into(),
                            description: String::new(),
                        },
                    ]
                },
                allow_custom: true,
                required: true,
            }],
        }
    }

    #[test]
    fn user_input_validates_selection_custom_text_and_optional_answers() {
        for kind in [Kind::Single, Kind::Multiple, Kind::Text] {
            form(kind).validate().unwrap();
        }
        let mut a = Answer {
            question_id: "q".into(),
            selected: vec!["a".into()],
            ..Default::default()
        };
        form(Kind::Single)
            .validate_answers(&[a.clone()])
            .unwrap();
        a.text = "其他目的地".into();
        assert!(
            form(Kind::Single)
                .validate_answers(&[a.clone()])
                .is_err()
        );
        form(Kind::Multiple)
            .validate_answers(&[a.clone()])
            .unwrap();
        assert!(
            form(Kind::Text)
                .validate_answers(&[a.clone()])
                .is_err()
        );
        a.selected.clear();
        form(Kind::Text)
            .validate_answers(&[a.clone()])
            .unwrap();
        a.text.clear();
        a.note = "备注不等于答案".into();
        assert!(
            form(Kind::Text)
                .validate_answers(&[a.clone()])
                .is_err()
        );
        let mut f = form(Kind::Text);
        f.questions[0].required = false;
        f.validate_answers(&[a]).unwrap();
    }

    #[test]
    fn user_input_rejects_unknown_ids_duplicates_and_invalid_schema() {
        let f = form(Kind::Multiple);
        for selected in [vec!["missing".into()], vec!["a".into(), "a".into()]] {
            assert!(
                f.validate_answers(&[Answer {
                    question_id: "q".into(),
                    selected,
                    ..Default::default()
                }])
                .is_err()
            );
        }
        let mut invalid = f.clone();
        invalid
            .questions
            .push(invalid.questions[0].clone());
        assert!(invalid.validate().is_err());
        invalid = f;
        invalid.questions[0].options[1].id = "a".into();
        assert!(invalid.validate().is_err());
    }
}
