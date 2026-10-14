use orbit_agent::{events::AgentEvent, tool::ToolRegistry};
use orbit_protocol::ToolProgress;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Deliveries(BTreeMap<u64, BTreeMap<String, [u8; 32]>>);

impl Deliveries {
    pub fn reset_started_tools(&mut self, viewer: u64, updates: &[super::Update]) {
        if let Some(seen) = self.0.get_mut(&viewer) {
            for update in updates {
                if let super::Update::Agent(event) = update
                    && let AgentEvent::ToolStarted { call } = event.as_ref()
                {
                    seen.remove(&call.id);
                }
            }
        }
    }

    pub fn filter(&mut self, viewer: u64, updates: &mut [ToolProgress]) {
        use sha2::{Digest, Sha256};
        let seen = self.0.entry(viewer).or_default();
        seen.retain(|id, _| {
            updates
                .iter()
                .any(|update| &update.call_id == id)
        });
        for update in updates {
            if let Some(stdout) = &update.stdout {
                let digest: [u8; 32] = Sha256::digest(stdout.as_bytes()).into();
                if seen.insert(update.call_id.clone(), digest) == Some(digest) {
                    update.stdout = None;
                }
            }
        }
    }

    pub fn forget(&mut self, viewer: u64) {
        self.0.remove(&viewer);
    }
}

#[derive(Clone)]
struct Call {
    name: String,
    job: Option<String>,
    generation: u64,
}

#[derive(Default)]
pub(super) struct Progress {
    calls: BTreeMap<String, Call>,
    previews: BTreeMap<String, crate::tools::command_output::Preview>,
    generation: u64,
}

pub(super) struct Snapshot {
    updates: Vec<ToolProgress>,
    previews: BTreeMap<String, crate::tools::command_output::Preview>,
    generations: BTreeMap<String, u64>,
}

impl Progress {
    pub fn record(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ToolStarted { call }
                if matches!(
                    call.function.name.as_str(),
                    "run_command" | "spawn_agent" | "send_agent_message"
                ) =>
            {
                self.generation += 1;
                self.previews.remove(&call.id);
                self.calls.insert(
                    call.id.clone(),
                    Call {
                        name: call.function.name.clone(),
                        job: None,
                        generation: self.generation,
                    },
                );
            }
            AgentEvent::ToolBackgrounded { call, content, .. } => {
                if let Some(tracked) = self.calls.get_mut(&call.id) {
                    tracked.job = serde_json::from_str::<serde_json::Value>(content)
                        .ok()
                        .and_then(|value| {
                            value["job_id"]
                                .as_str()
                                .map(str::to_owned)
                        });
                }
            }
            AgentEvent::ToolFinished { call, .. } | AgentEvent::ToolFailed { call, .. } => {
                self.calls.remove(&call.id);
                self.previews.remove(&call.id);
            }
            _ => {}
        }
    }

    pub fn clear(&mut self) {
        self.calls.clear();
        self.previews.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    pub fn start_snapshot(
        &mut self,
        tools: &ToolRegistry,
        output: &orbit_runtime::execution::output::CommandOutputs,
    ) -> tokio_util::task::AbortOnDropHandle<Snapshot> {
        let calls = self.calls.clone();
        let generations = calls
            .iter()
            .map(|(id, call)| (id.clone(), call.generation))
            .collect();
        let tools = tools.clone();
        let lease = output.lease();
        let mut previews = std::mem::take(&mut self.previews);
        tokio_util::task::AbortOnDropHandle::new(tokio::task::spawn_blocking(move || {
            let jobs = tools.background_jobs();
            let snapshots = jobs
                .snapshots(&[], false)
                .unwrap_or_default();
            let updates = calls
                .into_iter()
                .filter_map(|(call_id, call)| {
                    let (stdout, elapsed_ms) = if call.name == "run_command" {
                        let stdout = jobs
                            .output_id_for_call(&call_id)
                            .and_then(|id| {
                                crate::tools::command_output::read_preview(
                                    lease.as_ref()?.path(),
                                    &id,
                                    previews.remove(&call_id),
                                )
                            })
                            .and_then(|preview| {
                                let text =
                                    (!preview.text.is_empty()).then(|| preview.text.to_string());
                                previews.insert(call_id.clone(), preview);
                                text
                            });
                        let elapsed = snapshots
                            .iter()
                            .find(|job| job.call_id == call_id)
                            .map(|job| job.elapsed_ms);
                        (stdout, elapsed)
                    } else {
                        let elapsed = call
                            .job
                            .and_then(|id| {
                                tools
                                    .task_groups()
                                    .skip(1)
                                    .find_map(|jobs| {
                                        jobs.snapshots(std::slice::from_ref(&id), false)
                                            .ok()?
                                            .into_iter()
                                            .next()
                                    })
                            })
                            .map(|job| job.elapsed_ms);
                        (None, elapsed)
                    };
                    (stdout.is_some() || elapsed_ms.is_some()).then_some(ToolProgress {
                        call_id,
                        stdout,
                        elapsed_ms,
                    })
                })
                .collect();
            Snapshot {
                updates,
                previews,
                generations,
            }
        }))
    }

    /// A tool may finish while its file is being read. Never resurrect its progress, or let that
    /// reader publish into the next run.
    pub fn accept(&mut self, mut snapshot: Snapshot) -> Vec<ToolProgress> {
        snapshot.previews.retain(|id, _| {
            self.calls
                .get(id)
                .is_some_and(|call| snapshot.generations.get(id) == Some(&call.generation))
        });
        snapshot.updates.retain(|update| {
            self.calls
                .get(&update.call_id)
                .is_some_and(|call| {
                    snapshot
                        .generations
                        .get(&update.call_id)
                        == Some(&call.generation)
                })
        });
        self.previews = snapshot.previews;
        snapshot.updates
    }

    #[cfg(test)]
    async fn snapshot(
        &mut self,
        tools: &ToolRegistry,
        output: &orbit_runtime::execution::output::CommandOutputs,
    ) -> Vec<ToolProgress> {
        let snapshot = self
            .start_snapshot(tools, output)
            .await
            .unwrap();
        self.accept(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn command_progress_follows_current_stdout_and_stops_when_the_call_finishes() {
        let output = orbit_runtime::execution::output::CommandOutputs::default();
        let _task = output.begin_run().unwrap();
        let _log = output.create_log().await.unwrap();
        let directory = output.lease().unwrap();
        let tools = ToolRegistry::default();
        tools
            .background_jobs()
            .track_output("active-call", "cmd-orbit-preview");
        let call = orbit_model::types::ToolCall::function(
            "active-call",
            "run_command",
            serde_json::json!({}),
        );
        let mut progress = Progress::default();
        progress.record(&AgentEvent::ToolStarted { call: call.clone() });
        let path = directory
            .path()
            .join("cmd-orbit-preview.stdout");
        std::fs::write(&path, "first line").unwrap();
        assert_eq!(
            progress.snapshot(&tools, &output).await[0]
                .stdout
                .as_deref(),
            Some("first line")
        );
        let text = progress.previews["active-call"]
            .text
            .clone();
        assert_eq!(
            progress.snapshot(&tools, &output).await[0]
                .stdout
                .as_deref(),
            Some("first line")
        );
        assert!(std::sync::Arc::ptr_eq(
            &text,
            &progress.previews["active-call"].text
        ));
        std::fs::write(path, "first line\nsecond line").unwrap();
        assert_eq!(
            progress.snapshot(&tools, &output).await[0]
                .stdout
                .as_deref(),
            Some("first line\nsecond line")
        );
        let in_flight = progress.start_snapshot(&tools, &output);
        progress.record(&AgentEvent::ToolFinished {
            call: call.clone(),
            content: "done".into(),
            changes: vec![],
        });
        // Providers may reuse a call ID in a later turn. Its old reader must
        // not publish into the new tool instance either.
        progress.record(&AgentEvent::ToolStarted { call: call.clone() });
        assert!(
            progress
                .accept(in_flight.await.unwrap())
                .is_empty()
        );
        assert!(progress.previews.is_empty());
        progress.record(&AgentEvent::ToolFailed {
            call,
            error: "cancelled".into(),
        });
        assert!(
            progress
                .snapshot(&tools, &output)
                .await
                .is_empty()
        );
    }

    #[test]
    fn output_delivery_is_incremental_per_viewer_and_reconnect_receives_current_tail() {
        let mut delivery = Deliveries::default();
        let update = |text: &str| ToolProgress {
            call_id: "command".into(),
            stdout: Some(text.into()),
            elapsed_ms: Some(100),
        };
        for viewer in [1, 2] {
            let mut first = [update("first line")];
            delivery.filter(viewer, &mut first);
            assert_eq!(first[0].stdout.as_deref(), Some("first line"));
            let mut same = [update("first line")];
            delivery.filter(viewer, &mut same);
            assert!(same[0].stdout.is_none());
            assert_eq!(same[0].elapsed_ms, Some(100));
        }
        let mut changed = [update("first line\nsecond line")];
        delivery.filter(1, &mut changed);
        assert!(changed[0].stdout.is_some());
        delivery.forget(1);
        delivery.filter(1, &mut changed);
        assert!(changed[0].stdout.is_some());
        delivery.reset_started_tools(
            1,
            &[super::super::Update::agent(AgentEvent::ToolStarted {
                call: orbit_model::types::ToolCall::function(
                    "command",
                    "run_command",
                    serde_json::json!({}),
                ),
            })],
        );
        delivery.filter(1, &mut changed);
        assert!(changed[0].stdout.is_some());
        delivery.filter(1, &mut []);
        assert!(delivery.0[&1].is_empty());
    }
}
