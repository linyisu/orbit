use std::time::{Duration, Instant};

use orbit_agent::events::AgentEvent;
use orbit_store::types::Snapshot;

use super::app::{ToolStatus, TranscriptItem, UiState};

/// Replay saved events in bounded batches. This is local display work, not
/// another agent run; it never invokes a tool or appends a stored event.
#[derive(Default)]
pub struct HistoryReplay {
    run: usize,
    event: usize,
    run_start: Option<usize>,
}

impl HistoryReplay {
    /// Continue after events that this view has already displayed.
    pub fn following(snapshot: &Snapshot, state: &UiState) -> Self {
        if let Some(last) = snapshot.runs.last().filter(|run| run.status == "running") {
            Self {
                run: snapshot.runs.len() - 1,
                event: last.events.len(),
                run_start: Some(
                    state
                        .transcript
                        .iter()
                        .rposition(|item| {
                            matches!(
                                item,
                                TranscriptItem::User(_) | TranscriptItem::UserWithImages { .. }
                            )
                        })
                        .unwrap_or(0),
                ),
            }
        } else {
            Self {
                run: snapshot.runs.len(),
                ..Self::default()
            }
        }
    }

    pub fn completed_runs(&self) -> usize {
        self.run
    }

    pub fn advance(
        &mut self,
        snapshot: &Snapshot,
        state: &mut UiState,
        max_events: usize,
        budget: Duration,
    ) -> bool {
        let started = Instant::now();
        let mut processed = 0;
        state.workspace = snapshot.session.workspace.clone();

        while self.run < snapshot.runs.len() && processed < max_events && started.elapsed() < budget
        {
            let run = &snapshot.runs[self.run];

            if self.run_start.is_none() {
                self.run_start = Some(state.transcript.len());
                state.input.history.push(run.prompt.clone());

                let queued_input = run
                    .events
                    .iter()
                    .take_while(|event| !matches!(event, AgentEvent::TurnStarted { .. }))
                    .any(|event| matches!(event, AgentEvent::InputReceived { .. }));

                if run.prompt != "/compact" && !queued_input {
                    // Do not reset the reader's scroll position between runs.
                    let (scroll, follow) = (state.scroll, state.follow_bottom);
                    state.submit_with_images(run.prompt.clone(), run.images.clone());
                    state.scroll = scroll;
                    state.follow_bottom = follow;
                }
            }

            if let Some(event) = run.events.get(self.event) {
                // Receipt fingerprints and archived summaries can be large;
                // the transcript has no presentation for these metadata events.
                if !matches!(
                    event,
                    AgentEvent::CompactionRecorded { .. } | AgentEvent::VerificationRecorded { .. }
                ) {
                    state.apply_replayed(event.clone());
                }

                self.event += 1;
                processed += 1;
                continue;
            }

            // A live reader waits at the tail; it must not mark pending tools as failed.
            if run.status == "running" {
                return true;
            }

            match run.status.as_str() {
                "completed" => state.apply(AgentEvent::Completed {
                    turns: state.turn,
                    tool_calls: state.tool_calls,
                }),

                "cancelled" => state.apply(AgentEvent::Cancelled),
                _ => {
                    state.status = crate::tui::app::RunStatus::Failed;
                    state.finish_clock();
                }
            }

            if let Some(elapsed_ms) = run.elapsed_ms {
                state.elapsed = Duration::from_millis(elapsed_ms);
                state.started_at = None;
            }

            // Only scan the just-finished run, rather than all preceding runs.
            let first = self.run_start.take().unwrap_or_default();

            for item in &mut state.transcript[first..] {
                if let TranscriptItem::Tool(tool) = item
                    && tool.status == ToolStatus::Running
                {
                    tool.status = ToolStatus::Failed;
                    tool.output = "运行已结束或中断，未保存工具结果；不会自动重试。".into();
                }
            }

            state.invalidate_transcript(first);
            self.run += 1;
            self.event = 0;
            processed += 1;
        }

        self.run == snapshot.runs.len()
    }
}

#[cfg(test)]
mod tests;
