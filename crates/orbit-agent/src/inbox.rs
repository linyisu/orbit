use std::{collections::VecDeque, future::Future, sync::Arc};

use tokio::sync::{Mutex, Notify};

use crate::tool::ToolError;

#[derive(Clone, Debug)]
pub struct InboxMessage {
    pub id: i64,
    pub content: String,
    pub attachments: Vec<orbit_model::types::Attachment>,
}

#[derive(Clone, Debug)]
pub struct InboxSnapshot {
    pub messages: Vec<InboxMessage>,
    pub closed: bool,
}

#[derive(Default)]
struct State {
    messages: VecDeque<InboxMessage>,
    closed: bool,
}

#[derive(Clone, Default)]
pub struct AgentInbox {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
}

impl AgentInbox {
    pub async fn push(&self, message: InboxMessage) -> Result<(), ToolError> {
        self.persist_and_push(|| std::future::ready(Ok(message)))
            .await
    }

    /// Reserve the input boundary while persisting, so finalization cannot
    /// accept a message durably and then silently miss its in-memory delivery.
    pub async fn persist_and_push<F, Fut>(&self, persist: F) -> Result<(), ToolError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<InboxMessage, ToolError>>,
    {
        let mut state = self.state.lock().await;
        if state.closed {
            return Err(ToolError::ExecutionFailed(
                "Agent is finishing; wait for its result before continuing it".into(),
            ));
        }

        if state.messages.len() >= 32 {
            return Err(ToolError::ExecutionFailed(
                "Agent inbox is full; wait for queued messages to be received".into(),
            ));
        }

        state
            .messages
            .push_back(persist().await?);
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }

    pub(crate) async fn drain(&self) -> Vec<InboxMessage> {
        self.state
            .lock()
            .await
            .messages
            .drain(..)
            .collect()
    }

    pub async fn snapshot(&self) -> InboxSnapshot {
        let state = self.state.lock().await;
        InboxSnapshot {
            messages: state.messages.iter().cloned().collect(),
            closed: state.closed,
        }
    }

    /// Reserve the same boundary as drain and finalization. The service owns
    /// this future independently of the requesting client's connection, and
    /// replaces memory only after durable compare-and-swap has succeeded.
    pub async fn persist_edit<F, Fut>(
        &self,
        id: i64,
        expected: &str,
        content: String,
        persist: F,
    ) -> Result<(), ToolError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), ToolError>>,
    {
        let mut state = self.state.lock().await;
        let index = editable_index(&state, id, expected)?;
        persist().await?;
        state.messages[index].content = content;
        Ok(())
    }

    /// Deleting only the database row cannot revoke input already claimed by
    /// drain. Absence from this deque therefore rejects before persistence.
    pub async fn persist_remove<F, Fut>(
        &self,
        id: i64,
        expected: &str,
        persist: F,
    ) -> Result<(), ToolError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), ToolError>>,
    {
        let mut state = self.state.lock().await;
        let index = editable_index(&state, id, expected)?;
        persist().await?;
        state.messages.remove(index);
        Ok(())
    }

    pub(crate) async fn close_if_empty(&self) -> bool {
        let mut state = self.state.lock().await;
        if !state.messages.is_empty() {
            return false;
        }
        state.closed = true;
        true
    }

    pub(crate) async fn close(&self) {
        self.state.lock().await.closed = true;
    }

    pub async fn ready(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self
                .state
                .lock()
                .await
                .messages
                .is_empty()
            {
                return;
            }
            notified.await;
        }
    }
}

fn editable_index(state: &State, id: i64, expected: &str) -> Result<usize, ToolError> {
    if state.closed {
        return Err(ToolError::ExecutionFailed(
            "Agent 正在结束，请等待任务结束后刷新输入队列".into(),
        ));
    }
    let index = state
        .messages
        .iter()
        .position(|input| input.id == id)
        .ok_or_else(|| {
            ToolError::ExecutionFailed("输入已被 Agent 领取或已撤销，无法再修改；请刷新队列".into())
        })?;
    if state.messages[index].content != expected {
        return Err(ToolError::ExecutionFailed(
            "输入内容已改变，请刷新后重新编辑".into(),
        ));
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persistence_reservation_prevents_finalization_from_losing_input() {
        let inbox = AgentInbox::default();
        let (reserved, ready) = tokio::sync::oneshot::channel();
        let (release, gate) = tokio::sync::oneshot::channel();
        let queue = inbox.clone();
        let push = tokio::spawn(async move {
            queue
                .persist_and_push(|| async {
                    reserved.send(()).unwrap();
                    gate.await.unwrap();
                    Ok(InboxMessage {
                        attachments: vec![],
                        id: 1,
                        content: "orbit context".into(),
                    })
                })
                .await
        });

        ready.await.unwrap();
        let finishing = inbox.clone();
        let close = tokio::spawn(async move { finishing.close_if_empty().await });
        tokio::task::yield_now().await;
        assert!(!close.is_finished());
        release.send(()).unwrap();
        push.await.unwrap().unwrap();
        assert!(!close.await.unwrap());
        assert_eq!(inbox.drain().await[0].content, "orbit context");
        assert!(inbox.close_if_empty().await);
        assert!(
            inbox
                .push(InboxMessage {
                    attachments: vec![],
                    id: 2,
                    content: "late".into()
                })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_persistence_leaves_no_phantom_message() {
        let inbox = AgentInbox::default();
        assert!(
            inbox
                .persist_and_push(|| async {
                    Err(ToolError::ExecutionFailed("database unavailable".into()))
                })
                .await
                .is_err()
        );
        assert!(inbox.drain().await.is_empty());
        assert!(inbox.close_if_empty().await);
    }

    async fn three_messages() -> AgentInbox {
        let inbox = AgentInbox::default();
        for (id, content) in [(1, "first"), (2, "original"), (3, "last")] {
            inbox
                .push(InboxMessage {
                    attachments: vec![],
                    id,
                    content: content.into(),
                })
                .await
                .unwrap();
        }
        inbox
    }

    #[tokio::test]
    async fn edit_reservation_blocks_drain_and_consumes_only_committed_content_in_original_order() {
        let inbox = three_messages().await;
        let (reserved, waiting) = tokio::sync::oneshot::channel();
        let (release, gate) = tokio::sync::oneshot::channel();
        let editing = inbox.clone();
        let edit = tokio::spawn(async move {
            editing
                .persist_edit(2, "original", "edited".into(), || async {
                    reserved.send(()).unwrap();
                    gate.await.unwrap();
                    Ok(())
                })
                .await
        });
        waiting.await.unwrap();
        let consuming = inbox.clone();
        let drain = tokio::spawn(async move { consuming.drain().await });
        tokio::task::yield_now().await;
        assert!(!drain.is_finished());
        release.send(()).unwrap();
        edit.await.unwrap().unwrap();
        let consumed = drain.await.unwrap();
        assert_eq!(
            consumed
                .iter()
                .map(|input| (input.id, input.content.as_str()))
                .collect::<Vec<_>>(),
            [(1, "first"), (2, "edited"), (3, "last")]
        );
    }

    #[tokio::test]
    async fn removal_reservation_blocks_drain_and_never_delivers_removed_input() {
        let inbox = three_messages().await;
        let (reserved, waiting) = tokio::sync::oneshot::channel();
        let (release, gate) = tokio::sync::oneshot::channel();
        let removing = inbox.clone();
        let remove = tokio::spawn(async move {
            removing
                .persist_remove(2, "original", || async {
                    reserved.send(()).unwrap();
                    gate.await.unwrap();
                    Ok(())
                })
                .await
        });
        waiting.await.unwrap();
        let consuming = inbox.clone();
        let drain = tokio::spawn(async move { consuming.drain().await });
        tokio::task::yield_now().await;
        assert!(!drain.is_finished());
        release.send(()).unwrap();
        remove.await.unwrap().unwrap();
        assert_eq!(
            drain
                .await
                .unwrap()
                .iter()
                .map(|input| input.id)
                .collect::<Vec<_>>(),
            [1, 3]
        );
    }

    #[tokio::test]
    async fn drain_wins_before_checkpoint_so_mutations_reject_without_calling_persistence() {
        let inbox = three_messages().await;
        assert_eq!(inbox.drain().await.len(), 3);
        let error = inbox
            .persist_edit(2, "original", "late".into(), || async {
                panic!("a claimed input must never be edited in persistence")
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("领取"));
        assert!(
            inbox
                .persist_remove(2, "original", || async {
                    panic!("a claimed input must never be deleted in persistence")
                })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn stale_content_failed_persistence_and_closed_inbox_preserve_memory() {
        let inbox = three_messages().await;
        assert!(
            inbox
                .persist_edit(2, "different", "late".into(), || async {
                    panic!("stale memory CAS must reject before persistence")
                })
                .await
                .is_err()
        );
        assert!(
            inbox
                .persist_edit(2, "original", "failed".into(), || async {
                    Err(ToolError::ExecutionFailed("database unavailable".into()))
                })
                .await
                .is_err()
        );
        assert!(
            inbox
                .persist_remove(2, "original", || async {
                    Err(ToolError::ExecutionFailed("database unavailable".into()))
                })
                .await
                .is_err()
        );
        let snapshot = inbox.snapshot().await;
        assert_eq!(snapshot.messages[1].content, "original");
        assert!(!snapshot.closed);
        inbox.close().await;
        assert!(
            inbox
                .persist_remove(2, "original", || async {
                    panic!("closed inbox must not mutate persistence")
                })
                .await
                .is_err()
        );
        assert_eq!(inbox.snapshot().await.messages[1].content, "original");
    }

    #[tokio::test]
    async fn actual_agent_receives_edited_text_and_never_receives_removed_text() {
        use crate::{
            agent::{Agent, AgentConfig},
            events::AgentEvent,
            model::MockModel,
            tool::ToolRegistry,
        };
        use orbit_model::types::{ChatCompletion, ChatMessage, ChatRequest};
        let inbox = three_messages().await;
        inbox
            .persist_edit(2, "original", "edited".into(), || async { Ok(()) })
            .await
            .unwrap();
        inbox
            .persist_remove(1, "first", || async { Ok(()) })
            .await
            .unwrap();
        let image = orbit_model::types::Attachment::Image {
            name: "queued.png".into(),
            mime_type: "image/png".into(),
            path: "/queued.png".into(),
            lease: None,
        };
        inbox
            .push(InboxMessage {
                id: 4,
                content: String::new(),
                attachments: vec![image],
            })
            .await
            .unwrap();
        let model = MockModel::new([ChatCompletion {
            id: None,
            model: None,
            message: ChatMessage::assistant("done"),
            finish_reason: Some("stop".into()),
            usage: None,
        }])
        .with_vision(true);
        let (sender, mut events) = tokio::sync::mpsc::channel(128);
        Agent::new(model.clone(), ToolRegistry::new(), AgentConfig::default())
            .with_inbox(inbox)
            .run_with_agent_events(
                ChatRequest::new(vec![ChatMessage::user("task")]),
                tokio_util::sync::CancellationToken::new(),
                sender,
            )
            .await
            .unwrap();
        let requests = model.requests();
        assert!(
            requests[0]
                .messages
                .iter()
                .any(|message| matches!(
                    message,
                    ChatMessage::User {
                        content,
                        attachments,
                    } if content.is_empty()
                        && attachments.len() == 1
                        && attachments[0].name() == "queued.png"
                ))
        );

        assert!(
            requests[0]
                .messages
                .iter()
                .any(|message| matches!(
                    message,
                    ChatMessage::User { content, .. } if content == "edited"
                ))
        );
        assert!(
            !requests[0]
                .messages
                .iter()
                .any(|message| matches!(
                    message,
                    ChatMessage::User { content, .. }
                        if content == "original" || content == "first"
                ))
        );
        let mut received = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let AgentEvent::InputReceived {
                message_id,
                content,
                ..
            } = event
            {
                received.push((message_id, content));
            }
        }
        assert_eq!(
            received,
            [(2, "edited".into()), (3, "last".into()), (4, String::new())]
        );
    }
}
