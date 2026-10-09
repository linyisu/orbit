use orbit_agent::{events::AgentEvent, tool::ToolRegistry};
use orbit_protocol::ToolProgress;
use std::collections::BTreeMap;

#[derive(Clone)]
struct Call {
    name: String,
    job: Option<String>,
}

#[derive(Default)]
pub(super) struct Progress {
    calls: BTreeMap<String, Call>,
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
                self.calls.insert(
                    call.id.clone(),
                    Call {
                        name: call.function.name.clone(),
                        job: None,
                    },
                );
            }
            AgentEvent::ToolBackgrounded { call, content, .. } => {
                if let Some(tracked) = self.calls.get_mut(&call.id) {
                    tracked.job = serde_json::from_str::<serde_json::Value>(content)
                        .ok()
                        .and_then(|value| value["job_id"].as_str().map(str::to_owned));
                }
            }
            AgentEvent::ToolFinished { call, .. } | AgentEvent::ToolFailed { call, .. } => {
                self.calls.remove(&call.id);
            }
            _ => {}
        }
    }
    pub fn clear(&mut self) {
        self.calls.clear();
    }
    pub async fn snapshot(
        &self,
        tools: &ToolRegistry,
        output: &orbit_runtime::execution::output::CommandOutputs,
    ) -> Vec<ToolProgress> {
        if self.calls.is_empty() {
            return Vec::new();
        }
        let calls = self.calls.clone();
        let tools = tools.clone();
        let lease = output.lease();
        tokio::task::spawn_blocking(move || {
            calls
                .into_iter()
                .filter_map(|(call_id, call)| {
                    let (stdout, elapsed_ms) = if call.name == "run_command" {
                        let jobs = tools.background_jobs();
                        let stdout = jobs
                            .output_id_for_call(&call_id)
                            .and_then(|id| {
                                crate::tools::command_output::read_command_stdout(
                                    lease.as_ref()?.path(),
                                    &id,
                                )
                            })
                            .filter(|text| !text.is_empty());
                        let elapsed = jobs
                            .snapshots(&[], false)
                            .ok()
                            .and_then(|jobs| jobs.into_iter().find(|job| job.call_id == call_id))
                            .map(|job| job.elapsed_ms);
                        (stdout, elapsed)
                    } else {
                        let elapsed = call
                            .job
                            .and_then(|id| {
                                tools.task_groups().skip(1).find_map(|jobs| {
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
                .collect()
        })
        .await
        .unwrap_or_default()
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
        let path = directory.path().join("cmd-orbit-preview.stdout");
        std::fs::write(&path, "first line").unwrap();
        assert_eq!(
            progress.snapshot(&tools, &output).await[0]
                .stdout
                .as_deref(),
            Some("first line")
        );
        std::fs::write(path, "first line\nsecond line").unwrap();
        assert_eq!(
            progress.snapshot(&tools, &output).await[0]
                .stdout
                .as_deref(),
            Some("first line\nsecond line")
        );
        progress.record(&AgentEvent::ToolFinished {
            call,
            content: "done".into(),
            changes: vec![],
        });
        assert!(progress.snapshot(&tools, &output).await.is_empty());
    }
}
