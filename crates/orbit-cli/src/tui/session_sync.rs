//! Incremental event replay for sessions owned by another TUI process.
use super::app::UiState;
use crate::session::HistoryReplay;
use anyhow::Result;
use orbit_model::types::ChatMessage;
use orbit_store::{store::Store, tui_sessions::SessionRevision, types::Snapshot};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub(super) async fn sync_mode(store: &Store, id: &str, state: &mut UiState) -> Result<bool> {
    let mode = super::permissions::Mode::parse(&store.tui_mode(id).await?).unwrap_or_default();
    let grants = store.tui_file_grants(id).await?;
    let changed = state.permission_mode != mode || state.session_allowed_files != grants;
    state.permission_mode = mode;
    state.session_allowed_files = grants;
    Ok(changed)
}

pub(super) async fn sync_runtime(store: &Store, id: &str, state: &mut UiState) -> Result<bool> {
    if let Some(runtime) = store.tui_runtime(id).await? {
        state.apply_runtime(&runtime);
        return Ok(true);
    }
    Ok(false)
}

pub(super) async fn publish(
    session: &crate::session::SessionState,
    state: &UiState,
    run: Option<i64>,
) -> Result<()> {
    if let Some(run) = run {
        session
            .store
            .publish_tui_runtime(session.id(), run, state.runtime_snapshot())
            .await?;
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct SessionSync {
    deleted: HashSet<String>,
    readers: HashMap<String, (Option<SessionRevision>, HistoryReplay)>,
}

impl SessionSync {
    pub fn reset(&mut self, id: &str) {
        self.readers.remove(id);
    }

    pub async fn poll(
        &mut self,
        store: &Store,
        snapshot: &mut Snapshot,
        state: &mut UiState,
        history: &mut Vec<ChatMessage>,
    ) -> Result<bool> {
        let id = snapshot.session.id.clone();
        if self.deleted.contains(&id) {
            return Ok(false);
        }
        let Some(mut revision) = store.session_revision(&id).await? else {
            self.deleted.insert(id.clone());
            state.transcript.push(super::app::TranscriptItem::Notice(
                "会话已在其他窗口删除，请新建会话继续。".into(),
            ));
            state.invalidate_transcript(0);
            return Ok(true);
        };
        let (previous, replay) = self
            .readers
            .entry(id.clone())
            .or_insert_with(|| (None, HistoryReplay::following(snapshot, state)));
        // An exited writer releases its OS lease, even after a crash. Recover only then.
        if revision.status == "running"
            && let Ok(lease) = store.acquire(&id).await
        {
            drop(lease);
            revision = store.session_revision(&id).await?.unwrap_or(revision);
        }
        if previous.as_ref() == Some(&revision) {
            return Ok(false);
        }
        let incremental = previous.as_ref().is_some_and(|old| {
            old.run == revision.run && old.status == "running" && revision.status == "running"
        });
        if incremental {
            if let Some(run) = snapshot.runs.last_mut() {
                run.events.extend(
                    store
                        .run_events_after(&id, revision.run, run.events.len())
                        .await?,
                );
            }
            snapshot.session = store.session(&id).await?;
        } else {
            *snapshot = store.load(&id).await?;
        }
        let next = &*snapshot;
        // Keep local injected system instructions, editor drafts and scroll position.
        if !incremental {
            let systems: Vec<_> = history
                .iter()
                .filter(|message| matches!(message, ChatMessage::System { .. }))
                .cloned()
                .collect();
            *history = next
                .history
                .iter()
                .filter(|message| !matches!(message, ChatMessage::System { .. }))
                .cloned()
                .collect();
            history.splice(0..0, systems);
        }
        while !replay.advance(next, state, 2048, Duration::from_millis(4)) {
            tokio::task::yield_now().await;
        }
        *previous = Some(revision);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::app::{RunStatus, TranscriptItem};
    use super::*;
    use crate::session::SessionState;
    use orbit_agent::events::AgentEvent;
    use orbit_model::types::StreamEvent;

    #[tokio::test]
    async fn mode_context_and_clock_follow_the_owner_including_pauses_and_completion() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let mut owner = SessionState::open_shared_in(db.clone(), None, "mock".into(), dir.path())
            .await
            .unwrap();
        let mut peer =
            SessionState::open_shared_in(db, Some(owner.id().into()), "mock".into(), dir.path())
                .await
                .unwrap();
        peer.store.set_tui_mode(peer.id(), "YOLO").await.unwrap();
        let mut owner_ui = UiState::new("mock");
        assert!(
            sync_mode(&owner.store, owner.id(), &mut owner_ui)
                .await
                .unwrap()
        );
        assert_eq!(
            owner_ui.permission_mode,
            super::super::permissions::Mode::Yolo
        );
        peer.store
            .grant_tui_file(peer.id(), "src/lib.rs")
            .await
            .unwrap();
        sync_mode(&owner.store, owner.id(), &mut owner_ui)
            .await
            .unwrap();
        assert!(owner_ui.session_allowed_files.contains("src/lib.rs"));
        owner.begin("hello".into(), "mock".into()).await.unwrap();
        owner_ui.submit("hello".into());
        owner_ui.context_used_tokens = 45000;
        owner_ui.context_tokens = 186000;
        owner_ui.input_context_tokens = 160000;
        owner_ui.max_output_tokens = 26000;
        owner_ui.started_at = Some(std::time::Instant::now() - Duration::from_secs(8));
        owner_ui.turn = 3;
        owner_ui.tool_calls = 7;
        publish(&owner, &owner_ui, owner.active_run_id())
            .await
            .unwrap();
        let mut peer_ui = UiState::new("mock");
        peer_ui.input.set("draft".into());
        let mut sync = SessionSync::default();
        sync.poll(&peer.store, &mut peer.snapshot, &mut peer_ui, &mut vec![])
            .await
            .unwrap();
        sync_mode(&peer.store, peer.id(), &mut peer_ui)
            .await
            .unwrap();
        sync_runtime(&peer.store, peer.id(), &mut peer_ui)
            .await
            .unwrap();
        assert_eq!(peer_ui.permission_mode, owner_ui.permission_mode);
        assert_eq!(peer_ui.context_used_tokens, 45000);
        assert_eq!(peer_ui.context_tokens, 186000);
        assert_eq!(peer_ui.input_context_tokens, 160000);
        assert_eq!(peer_ui.max_output_tokens, 26000);
        assert_eq!((peer_ui.turn, peer_ui.tool_calls), (3, 7));
        assert!(
            peer_ui.active_elapsed().abs_diff(owner_ui.active_elapsed())
                < Duration::from_millis(30)
        );
        assert_eq!(peer_ui.input.text, "draft");
        owner_ui.pause_clock();
        let mut paused = owner_ui.runtime_snapshot();
        paused.sampled_at_ms = now_ms().saturating_sub(4000);
        owner
            .store
            .publish_tui_runtime(owner.id(), owner.active_run_id().unwrap(), paused)
            .await
            .unwrap();
        sync_runtime(&peer.store, peer.id(), &mut peer_ui)
            .await
            .unwrap();
        assert!(
            peer_ui.active_elapsed().abs_diff(owner_ui.active_elapsed()) < Duration::from_millis(2)
        );
        owner_ui.resume_clock();
        owner_ui.status = RunStatus::Cancelling;
        publish(&owner, &owner_ui, owner.active_run_id())
            .await
            .unwrap();
        sync_runtime(&peer.store, peer.id(), &mut peer_ui)
            .await
            .unwrap();
        assert_eq!(peer_ui.status, RunStatus::Cancelling);
        let run = owner.active_run_id();
        owner.finish("cancelled", None).await.unwrap();
        owner_ui.apply(AgentEvent::Cancelled);
        owner_ui.context_used_tokens = 47000;
        publish(&owner, &owner_ui, run).await.unwrap();
        sync.poll(&peer.store, &mut peer.snapshot, &mut peer_ui, &mut vec![])
            .await
            .unwrap();
        sync_runtime(&peer.store, peer.id(), &mut peer_ui)
            .await
            .unwrap();
        assert_eq!(peer_ui.status, RunStatus::Cancelled);
        assert_eq!(peer_ui.context_used_tokens, 47000);
        assert_eq!(
            peer_ui.active_elapsed(),
            Duration::from_millis(owner_ui.active_elapsed().as_millis() as u64)
        );
        // A late old owner cannot replace a new run's presentation.
        peer.begin("next".into(), "mock".into()).await.unwrap();
        publish(&owner, &owner_ui, run).await.unwrap();
        assert!(peer.store.tui_runtime(peer.id()).await.unwrap().is_none());
        peer.finish("cancelled", None).await.unwrap();
    }

    #[tokio::test]
    async fn an_exited_writer_is_interrupted_without_reexecuting_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let mut owner = SessionState::open_shared_in(db.clone(), None, "mock".into(), dir.path())
            .await
            .unwrap();
        owner.begin("hello".into(), "mock".into()).await.unwrap();
        let mut peer =
            SessionState::open_shared_in(db, Some(owner.id().into()), "mock".into(), dir.path())
                .await
                .unwrap();
        let mut state = UiState::new("mock");
        let mut replay = HistoryReplay::default();
        assert!(replay.advance(&peer.snapshot, &mut state, usize::MAX, Duration::MAX));
        assert_eq!(state.status, RunStatus::Running);
        drop(owner);
        let mut sync = SessionSync::default();
        sync.poll(&peer.store, &mut peer.snapshot, &mut state, &mut vec![])
            .await
            .unwrap();
        assert_eq!(state.status, RunStatus::Failed);
        assert_eq!(peer.snapshot.runs[0].status, "interrupted");
        assert_eq!(state.transcript.len(), 1);
    }

    #[tokio::test]
    async fn two_windows_share_live_events_without_losing_draft_or_replaying_twice() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let mut owner = SessionState::open_shared_in(db.clone(), None, "mock".into(), dir.path())
            .await
            .unwrap();
        let mut peer =
            SessionState::open_shared_in(db, Some(owner.id().into()), "mock".into(), dir.path())
                .await
                .unwrap();
        let mut state = UiState::new("mock");
        state.input.set("my draft".into());
        state.scroll = 7;
        state.follow_bottom = false;
        let mut history = vec![];
        let mut sync = SessionSync::default();
        owner.begin("hello".into(), "mock".into()).await.unwrap();
        let journal = owner.execution_journal().unwrap();
        journal.record(AgentEvent::Model(StreamEvent::TextDelta("hi".into())));
        owner.flush_events().await.unwrap();
        sync.poll(&peer.store, &mut peer.snapshot, &mut state, &mut history)
            .await
            .unwrap();
        assert_eq!(state.input.text, "my draft");
        assert_eq!(state.scroll, 7);
        assert!(!state.follow_bottom);
        assert_eq!(state.status, RunStatus::Running);
        assert!(matches!(&state.transcript[0], TranscriptItem::User(text) if text == "hello"));
        assert!(matches!(&state.transcript[1], TranscriptItem::Assistant(text) if text == "hi"));
        assert!(
            !sync
                .poll(&peer.store, &mut peer.snapshot, &mut state, &mut history)
                .await
                .unwrap()
        );
        assert!(peer.begin("collision".into(), "mock".into()).await.is_err());
        journal.record(AgentEvent::Model(StreamEvent::TextDelta(" there".into())));
        owner.flush_events().await.unwrap();
        sync.poll(&peer.store, &mut peer.snapshot, &mut state, &mut history)
            .await
            .unwrap();
        assert_eq!(state.transcript.len(), 2);
        assert!(
            matches!(&state.transcript[1], TranscriptItem::Assistant(text) if text == "hi there")
        );
        owner
            .finish("completed", Some(vec![ChatMessage::user("hello")]))
            .await
            .unwrap();
        sync.poll(&peer.store, &mut peer.snapshot, &mut state, &mut history)
            .await
            .unwrap();
        assert_eq!(state.status, RunStatus::Completed);
        peer.begin("next".into(), "mock".into()).await.unwrap();
        peer.finish("completed", Some(history)).await.unwrap();
    }

    #[tokio::test]
    async fn a_stale_window_cannot_overwrite_a_completed_turn() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let mut owner = SessionState::open_shared_in(db.clone(), None, "mock".into(), dir.path())
            .await
            .unwrap();
        let mut peer =
            SessionState::open_shared_in(db, Some(owner.id().into()), "mock".into(), dir.path())
                .await
                .unwrap();
        owner.begin("first".into(), "mock".into()).await.unwrap();
        owner
            .finish("completed", Some(vec![ChatMessage::user("first")]))
            .await
            .unwrap();
        assert!(
            peer.begin("stale".into(), "mock".into())
                .await
                .unwrap_err()
                .is::<crate::session::SessionRefreshRequired>()
        );
        assert_eq!(owner.store.load(owner.id()).await.unwrap().runs.len(), 1);
        assert!(owner.store.acquire(owner.id()).await.is_ok());
        let mut ui = UiState::new("mock");
        let mut history = Vec::new();
        SessionSync::default()
            .poll(&peer.store, &mut peer.snapshot, &mut ui, &mut history)
            .await
            .unwrap();
        peer.begin("stale".into(), "mock".into()).await.unwrap();
        peer.finish("completed", Some(history)).await.unwrap();
        assert_eq!(owner.store.load(owner.id()).await.unwrap().runs.len(), 2);
    }
}
