//! Session-owned question lifecycle, shared by the parent and its subagents.
use anyhow::{Result, ensure};
use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput, ToolRegistry};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_protocol::user_input::{Answer, Form, Record, Status};
use orbit_store::store::Store;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

struct Entry {
    queued: std::time::Instant,
    record: Record,
    reply: oneshot::Sender<Record>,
}
#[derive(Clone)]
pub struct Questions(Arc<Inner>);
struct Inner {
    store: Store,
    session: String,
    enabled: AtomicBool,
    pending: Mutex<BTreeMap<String, Entry>>,
}

impl Questions {
    pub async fn new(store: Store, session: String) -> Result<Self> {
        store.interrupt_user_inputs(&session).await?;
        Ok(Self(Arc::new(Inner {
            store,
            session,
            enabled: AtomicBool::new(false),
            pending: Mutex::new(BTreeMap::new()),
        })))
    }
    pub fn enable(&self) {
        self.0.enabled.store(true, Ordering::Release);
    }
    pub fn register(&self, tools: &mut ToolRegistry, source: Option<String>) {
        tools.register(AskUser {
            questions: self.clone(),
            source,
        });
    }
    pub async fn pending(&self) -> Vec<Record> {
        let pending = self.0.pending.lock().await;
        let mut entries: Vec<_> = pending.values().collect();
        entries.sort_by_key(|e| e.queued);
        entries.into_iter().map(|e| e.record.clone()).collect()
    }
    pub async fn answer(&self, id: &str, answers: Option<Vec<Answer>>) -> Result<()> {
        let mut pending = self.0.pending.lock().await;
        let status = if answers.is_some() {
            Status::Answered
        } else {
            Status::Declined
        };
        let answers = answers.unwrap_or_default();
        let Some(entry) = pending.get(id) else {
            let existing = self.0.store.user_input(&self.0.session, id).await?;
            ensure!(
                existing.is_some_and(|r| r.status == status && r.answers == answers),
                "该问答已结束或已在其他终端回答，请查看最新结果"
            );
            return Ok(());
        };
        if status == Status::Answered {
            entry
                .record
                .form
                .validate_answers(&answers)
                .map_err(anyhow::Error::msg)?;
        }
        let mut record = entry.record.clone();
        record.status = status;
        record.answers = answers;
        // Persist before releasing the waiting tool or acknowledging the client.
        self.0
            .store
            .save_user_input(&self.0.session, &record)
            .await?;
        let entry = pending.remove(id).unwrap();
        let _ = entry.reply.send(record);
        Ok(())
    }
    async fn ask(
        &self,
        call_id: String,
        source: Option<String>,
        form: Form,
        token: CancellationToken,
    ) -> Result<Record> {
        ensure!(
            self.0.enabled.load(Ordering::Acquire),
            "当前客户端不支持交互问答，请通过对话向用户提问"
        );
        form.validate().map_err(anyhow::Error::msg)?;
        let record = Record {
            id: orbit_protocol::request_id(),
            call_id,
            source,
            form,
            status: Status::Pending,
            answers: Vec::new(),
        };
        let (reply, response) = oneshot::channel();
        {
            let mut pending = self.0.pending.lock().await;
            ensure!(pending.len() < 8, "待回答问答已达上限，请等待已有问题完成");
            self.0
                .store
                .save_user_input(&self.0.session, &record)
                .await?;
            pending.insert(
                record.id.clone(),
                Entry {
                    queued: std::time::Instant::now(),
                    record: record.clone(),
                    reply,
                },
            );
        }
        let _guard = PendingGuard {
            questions: self.clone(),
            id: record.id.clone(),
        };
        tokio::select! {
            biased;
            result = response => Ok(result?),
            _ = token.cancelled() => {
                let mut pending = self.0.pending.lock().await;
                if let Some(entry) = pending.get(&record.id) {
                    let mut cancelled = entry.record.clone();
                    cancelled.status = Status::Cancelled;
                    self.0.store.save_user_input(&self.0.session,&cancelled).await?;
                    pending.remove(&record.id);
                    Ok(cancelled)
                } else {
                    self.0.store.user_input(&self.0.session,&record.id).await?.ok_or_else(||anyhow::anyhow!("问答记录丢失"))
                }
            }
        }
    }
}

struct PendingGuard {
    questions: Questions,
    id: String,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        let questions = self.questions.clone();
        let id = self.id.clone();
        tokio::spawn(async move {
            let mut pending = questions.0.pending.lock().await;
            if let Some(mut entry) = pending.remove(&id) {
                entry.record.status = Status::Cancelled;
                if let Err(error) = questions
                    .0
                    .store
                    .save_user_input(&questions.0.session, &entry.record)
                    .await
                {
                    eprintln!("failed to record cancelled user input: {error}");
                }
            }
        });
    }
}

struct AskUser {
    questions: Questions,
    source: Option<String>,
}
impl Tool for AskUser {
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::INTERACTION
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "ask_user",
            "Ask the user a group of related clarification questions. Prefer 1–4 questions. Supports single choice, multiple choice and free text. The UI supplies a custom-answer option; do not add an Other option yourself. No answer is preselected. Waits for explicit submission or cancellation, including in auto/YOLO mode. Use only when missing user information materially affects the task. This tool does not grant execution permissions.",
            serde_json::json!({
                "type":"object","required":["questions"],"properties":{"questions":{"type":"array","minItems":1,"maxItems":8,"items":{
                    "type":"object","required":["id","title","prompt","kind"],"properties":{
                        "id":{"type":"string"},"title":{"type":"string","description":"Short question tab title"},"prompt":{"type":"string"},
                        "kind":{"type":"string","enum":["single","multiple","text"]},"required":{"type":"boolean","default":true},"allow_custom":{"type":"boolean","default":true},
                        "options":{"type":"array","items":{"type":"object","required":["id","label"],"properties":{"id":{"type":"string"},"label":{"type":"string"},"description":{"type":"string"}}}}
                    }
                }}}
            }),
        )
    }
    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let form: Form = serde_json::from_value(call.arguments_json().map_err(|e| {
                ToolError::InvalidArguments {
                    tool: "ask_user".into(),
                    message: e.to_string(),
                }
            })?)
            .map_err(|e| ToolError::InvalidArguments {
                tool: "ask_user".into(),
                message: e.to_string(),
            })?;
            let result = self
                .questions
                .ask(call.id, self.source.clone(), form, cancellation)
                .await
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
            Ok(ToolOutput::text(
                serde_json::to_string(&result)
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_protocol::user_input::{Kind, Question};
    fn form() -> Form {
        Form {
            questions: vec![Question {
                id: "q".into(),
                title: "要求".into(),
                prompt: "需要哪些内容？".into(),
                kind: Kind::Text,
                options: vec![],
                allow_custom: true,
                required: true,
            }],
        }
    }
    async fn fixture() -> (
        tempfile::TempDir,
        Questions,
        orbit_store::store::SessionWriter,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("test.sqlite")).await.unwrap();
        let writer = store
            .create(dir.path().display().to_string(), "test".into())
            .await
            .unwrap();
        let questions = Questions::new(store, writer.id().into()).await.unwrap();
        questions.enable();
        (dir, questions, writer)
    }
    async fn wait(questions: &Questions) -> Record {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(record) = questions.pending().await.first() {
                    return record.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn user_input_two_clients_submit_once_and_results_survive_reopen() {
        let (_dir, q, writer) = fixture().await;
        let caller = q.clone();
        let task = tokio::spawn(async move {
            caller
                .ask("call".into(), None, form(), CancellationToken::new())
                .await
                .unwrap()
        });
        let pending = wait(&q).await;
        let answer = vec![Answer {
            question_id: "q".into(),
            text: "完整说明".into(),
            ..Default::default()
        }];
        let (a, b) = tokio::join!(
            q.answer(&pending.id, Some(answer.clone())),
            q.answer(&pending.id, Some(answer.clone()))
        );
        a.unwrap();
        b.unwrap();
        assert!(q.pending().await.is_empty());
        assert_eq!(task.await.unwrap().answers, answer);
        assert!(q.answer(&pending.id, None).await.is_err());
        let store = Store::open(q.0.store.path()).await.unwrap();
        assert_eq!(
            store
                .user_input_history(writer.id(), None)
                .await
                .unwrap()
                .records[0]
                .status,
            Status::Answered
        );
    }
    #[tokio::test]
    async fn user_input_validation_preserves_pending_and_cancellation_is_recorded() {
        let (_dir, q, _writer) = fixture().await;
        let token = CancellationToken::new();
        let caller = q.clone();
        let cancellation = token.clone();
        let task = tokio::spawn(async move {
            caller
                .ask("call".into(), Some("child".into()), form(), cancellation)
                .await
                .unwrap()
        });
        let r = wait(&q).await;
        assert_eq!(r.source.as_deref(), Some("child"));
        assert!(q.answer(&r.id, Some(vec![])).await.is_err());
        assert_eq!(q.pending().await.len(), 1);
        token.cancel();
        assert_eq!(task.await.unwrap().status, Status::Cancelled);
        assert!(q.pending().await.is_empty());
        assert_eq!(
            q.0.store
                .user_input(&q.0.session, &r.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            Status::Cancelled
        );
    }
    #[tokio::test]
    async fn user_input_headless_rejects_and_restart_marks_interrupted() {
        let (_dir, q, _writer) = fixture().await;
        let fresh = Questions::new(q.0.store.clone(), q.0.session.clone())
            .await
            .unwrap();
        assert!(
            fresh
                .ask("headless".into(), None, form(), CancellationToken::new())
                .await
                .is_err()
        );
        let record = Record {
            id: "pending".into(),
            call_id: "call".into(),
            source: None,
            form: form(),
            status: Status::Pending,
            answers: vec![],
        };
        q.0.store
            .save_user_input(&q.0.session, &record)
            .await
            .unwrap();
        Questions::new(q.0.store.clone(), q.0.session.clone())
            .await
            .unwrap();
        assert_eq!(
            q.0.store
                .user_input(&q.0.session, "pending")
                .await
                .unwrap()
                .unwrap()
                .status,
            Status::Interrupted
        );
    }
    #[tokio::test]
    async fn user_input_history_pages_and_session_boundaries() {
        let (_dir, q, _writer) = fixture().await;
        for i in 0..23 {
            q.0.store
                .save_user_input(
                    &q.0.session,
                    &Record {
                        id: i.to_string(),
                        call_id: "call".into(),
                        source: None,
                        form: form(),
                        status: Status::Declined,
                        answers: vec![],
                    },
                )
                .await
                .unwrap();
        }
        let first =
            q.0.store
                .user_input_history(&q.0.session, None)
                .await
                .unwrap();
        assert_eq!(first.records.len(), 20);
        let second =
            q.0.store
                .user_input_history(&q.0.session, first.next)
                .await
                .unwrap();
        assert_eq!(second.records.len(), 3);
        assert!(second.next.is_none());
        assert!(q.0.store.user_input("other", "0").await.unwrap().is_none());
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[tokio::test]
    async fn user_input_aborted_tool_releases_request() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("db")).await.unwrap();
        let writer = store
            .create(dir.path().display().to_string(), "test".into())
            .await
            .unwrap();
        let q = Questions::new(store.clone(), writer.id().into())
            .await
            .unwrap();
        q.enable();
        let form=serde_json::from_value(serde_json::json!({"questions":[{"id":"q","title":"要求","prompt":"需要什么？","kind":"text"}]})).unwrap();
        let caller = q.clone();
        let task = tokio::spawn(async move {
            caller
                .ask("call".into(), None, form, CancellationToken::new())
                .await
        });
        let id = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(r) = q.pending().await.first() {
                    break r.id.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !q.pending().await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            store
                .user_input(writer.id(), &id)
                .await
                .unwrap()
                .unwrap()
                .status,
            Status::Cancelled
        );
    }
}
