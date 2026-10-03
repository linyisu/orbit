use orbit_agent::{
    background::BackgroundJobs,
    tool::{Tool, ToolError, ToolFuture, ToolOutput},
};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
pub enum CommandJobAction {
    Status,
    Wait,
    Cancel,
}

pub struct CommandJobs {
    jobs: BackgroundJobs,
    action: CommandJobAction,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Arguments {
    job_ids: Vec<String>,
    mode: WaitMode,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WaitMode {
    #[default]
    Any,
    All,
}

impl CommandJobs {
    pub fn new(jobs: BackgroundJobs, action: CommandJobAction) -> Self {
        Self { jobs, action }
    }

    fn name(&self) -> &'static str {
        match self.action {
            CommandJobAction::Status => "get_command_status",
            CommandJobAction::Wait => "wait_commands",
            CommandJobAction::Cancel => "cancel_commands",
        }
    }
}

impl Tool for CommandJobs {
    fn definition(&self) -> ToolDefinition {
        let description = match self.action {
            CommandJobAction::Status => {
                "Inspect managed background commands in this session. Returns status, output_id and a bounded result preview. Omit job_ids to list this run's commands. Completion notifications are automatic: do not repeatedly poll this tool; use wait_commands when no independent work remains."
            }
            CommandJobAction::Wait => {
                "Suspend without model requests until any or all selected background commands finish. Omit job_ids to wait for outstanding commands in this run. mode=any returns when one finishes; mode=all waits for all. Returns bounded results and output_ids for read_command_output. This is event-driven and has no polling interval; execution deadlines still apply."
            }
            CommandJobAction::Cancel => {
                "Request cancellation of the selected background commands in this session, including their child processes. Requires explicit job_ids; completed jobs are unchanged. Returns cancelling until the process has actually stopped; use wait_commands to await cleanup."
            }
        };
        let mut properties = json!({
            "job_ids": {
                "type": "array", "items": { "type": "string", "minLength": 1 },
                "maxItems": 8, "uniqueItems": true,
                "description": "Opaque job IDs returned by run_command; private to this session"
            }
        });
        if matches!(self.action, CommandJobAction::Wait) {
            properties["mode"] =
                json!({ "type": "string", "enum": ["any", "all"], "default": "any" });
        }
        let required = if matches!(self.action, CommandJobAction::Cancel) {
            vec!["job_ids"]
        } else {
            vec![]
        };
        ToolDefinition::function(
            self.name(),
            description,
            json!({
                "type": "object", "properties": properties, "required": required, "additionalProperties": false,
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let invalid = |message: String| ToolError::InvalidArguments {
                tool: self.name().into(),
                message,
            };
            let args: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;
            if !matches!(self.action, CommandJobAction::Wait)
                && call
                    .arguments_json()
                    .map_err(|error| invalid(error.to_string()))?
                    .get("mode")
                    .is_some()
            {
                return Err(invalid("mode is only supported by wait_commands".into()));
            }
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let snapshots = match self.action {
                CommandJobAction::Status => self
                    .jobs
                    .snapshots(&args.job_ids, !args.job_ids.is_empty())?,
                CommandJobAction::Wait => {
                    self.jobs
                        .wait(
                            &args.job_ids,
                            matches!(args.mode, WaitMode::All),
                            cancellation,
                        )
                        .await?
                }
                CommandJobAction::Cancel => {
                    if args.job_ids.is_empty() {
                        return Err(invalid(
                            "job_ids must contain at least one command to cancel".into(),
                        ));
                    }
                    self.jobs.cancel(&args.job_ids)?
                }
            };
            Ok(ToolOutput::text(json!({ "jobs": snapshots }).to_string()))
        })
    }
}
