use std::{collections::VecDeque, future::Future, sync::Arc};

use tokio::sync::{Mutex, Notify};

use crate::tool::ToolError;

#[derive(Clone, Debug)]
pub struct InboxMessage {
    pub id: i64,
    pub content: String,
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
        state.messages.push_back(persist().await?);
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }

    pub(crate) async fn drain(&self) -> Vec<InboxMessage> {
        self.state.lock().await.messages.drain(..).collect()
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
            if !self.state.lock().await.messages.is_empty() {
                return;
            }
            notified.await;
        }
    }
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
}
