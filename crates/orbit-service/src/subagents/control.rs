//! Typed native controls share subagent operations and retain the registry's live policy lease.
use super::*;

#[derive(Clone)]
pub struct Control(pub(super) Arc<Service>);

pub(super) enum LaunchResult {
    Started {
        workspace: String,
        agent: SubagentSession,
        invocation: i64,
        run_id: i64,
        job_id: String,
    },
    Rejected {
        agent_id: String,
        invocation: i64,
        run_id: i64,
        error: String,
        cancelled: bool,
    },
}

pub(super) enum SendResult {
    Queued {
        agent: SubagentSession,
        message_id: i64,
    },
    Launched(LaunchResult),
}

impl LaunchResult {
    pub(super) fn into_output(self) -> ToolOutput {
        match self {
            Self::Started {
                workspace,
                agent,
                invocation,
                run_id,
                job_id,
            } => {
                let mut output = ToolOutput::text(
                    json!({
                        "workspace": workspace,
                        "agent_id": agent.id,
                        "invocation_id": invocation,
                        "run_id": run_id,
                        "job_id": job_id,
                        "role": agent.role,
                        "model": agent.model,
                        "status": "running"
                    })
                    .to_string(),
                );
                output.background_job = Some(job_id);
                output
            }
            Self::Rejected {
                agent_id,
                invocation,
                run_id,
                error,
                cancelled,
            } => ToolOutput::text(
                json!({
                    "agent_id": agent_id,
                    "invocation_id": invocation,
                    "run_id": run_id,
                    "status": if cancelled { "cancelled" } else { "failed" },
                    "error": error
                })
                .to_string(),
            )
            .with_outcome(if cancelled {
                ToolOutcome::Cancelled
            } else {
                ToolOutcome::Failed
            }),
        }
    }
}

impl SendResult {
    pub(super) fn into_output(self) -> ToolOutput {
        match self {
            Self::Queued { agent, message_id } => ToolOutput::text(
                json!({
                    "agent_id": agent.id,
                    "role": agent.role,
                    "model": agent.model,
                    "status": "queued",
                    "message_id": message_id
                })
                .to_string(),
            ),
            Self::Launched(launch) => launch.into_output(),
        }
    }

    fn into_snapshot(self) -> Result<orbit_protocol::subagents::Snapshot> {
        use orbit_protocol::subagents::Snapshot;
        Ok(match self {
            Self::Queued { agent, message_id } => Snapshot::Sent {
                id: agent.id,
                status: "queued".into(),
                message_id: Some(message_id),
            },
            Self::Launched(LaunchResult::Started { agent, .. }) => Snapshot::Sent {
                id: agent.id,
                status: "running".into(),
                message_id: None,
            },
            Self::Launched(LaunchResult::Rejected { error, .. }) => {
                anyhow::bail!("subagent could not start: {error}")
            }
        })
    }
}

impl Control {
    pub(crate) fn set_parent_model(&self, model: crate::models::ModelOption) {
        *self.0.parent_model.write().unwrap() = model;
    }

    /// Acquire the same current visibility and policy lease as a model tool call.
    fn authorize(
        &self,
        tools: &ToolRegistry,
        action: Action,
        args: Value,
    ) -> Result<(ToolCall, Option<Box<dyn Send>>)> {
        ensure!(
            tools.contains(action.name()),
            "当前会话不能使用该子 Agent 操作"
        );
        let call = ToolCall::function(orbit_protocol::request_id(), action.name(), args);
        let lease = tools
            .policy()
            .map(|policy| policy.acquire(&call, tools.call_capabilities(&call)))
            .transpose()?;
        Ok((call, lease))
    }

    pub(crate) async fn send(
        &self,
        tools: &ToolRegistry,
        id: String,
        message: String,
    ) -> Result<orbit_protocol::subagents::Snapshot> {
        let (call, _lease) = self.authorize(
            tools,
            Action::Send,
            json!({
                "agent_id": id,
                "message": message
            }),
        )?;
        self.0
            .send(
                call,
                SendArgs {
                    agent_id: id,
                    message,
                },
                CancellationToken::new(),
            )
            .await?
            .into_snapshot()
    }

    pub(crate) async fn cancel(&self, tools: &ToolRegistry, id: String, run: i64) -> Result<()> {
        let (_call, _lease) = self.authorize(
            tools,
            Action::Cancel,
            json!({
                "agent_ids": [id],
                "expected_run_id": run
            }),
        )?;
        let ids = self
            .0
            .controlled_jobs(
                &IdsArgs {
                    agent_ids: vec![id],
                    all: false,
                    expected_run_id: Some(run),
                },
                true,
            )
            .await?;
        self.0.jobs.cancel(&ids)?;
        Ok(())
    }
}
