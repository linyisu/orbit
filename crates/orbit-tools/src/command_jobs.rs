use orbit_agent::{
    background::BackgroundJobs,
    tool::{Tool, ToolError, ToolFuture, ToolOutput},
};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

const DEFAULT_WAIT_SECONDS: u64 = 30;
const MAX_WAIT_SECONDS: u64 = 60;

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
    timeout_seconds: Option<u64>,
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
            CommandJobAction::Status => "CommandStatus",
            CommandJobAction::Wait => "WaitCommands",
            CommandJobAction::Cancel => "StopCommands",
        }
    }
}

impl Tool for CommandJobs {
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        use orbit_agent::tool::{ToolCapabilities, ToolEffect};
        match self.action {
            CommandJobAction::Status => ToolCapabilities::READ,
            CommandJobAction::Wait => ToolCapabilities::exclusive(ToolEffect::Observation, true),
            CommandJobAction::Cancel => ToolCapabilities::COMMAND,
        }
    }

    fn definition(&self) -> ToolDefinition {
        let description = match self.action {
            CommandJobAction::Status => {
                "Inspect managed background commands in this session. Returns status, output_id \
                    and a bounded result preview. Omit job_ids to list this run's commands. \
                    Completion notifications are automatic: do not repeatedly poll this tool; use \
                    WaitCommands when no independent work remains."
            }
            CommandJobAction::Wait => {
                "Suspend without model requests until any or all selected background commands \
                    finish. Omit job_ids to wait for outstanding commands in this run. mode=any \
                    returns when one finishes; mode=all waits for all. Returns bounded results \
                    and output_ids for CommandOutput. This is event-driven and has no \
                    polling interval. timeout_seconds bounds this wait (default 30, maximum 60); \
                    returning early leaves commands running and includes wait_timed_out=true."
            }
            CommandJobAction::Cancel => {
                "Request cancellation of the selected background commands in this session, \
                    including their child processes. Requires explicit job_ids; completed jobs \
                    are unchanged. Returns cancelling until the process has actually stopped; use \
                    WaitCommands to await cleanup."
            }
        };
        let mut properties = json!({
            "job_ids": {
                "type": "array",
                "items": {
                    "type": "string",
                    "minLength": 1
                },
                "maxItems": 8,
                "uniqueItems": true,
                "description": "Opaque job IDs returned by Exec; private to this session"
            }
        });

        if matches!(self.action, CommandJobAction::Wait) {
            properties["mode"] = json!({
                "type": "string",
                "enum": ["any", "all"],
                "default": "any"
            });
            properties["timeout_seconds"] = json!({
                "type": "integer",
                "minimum": 0,
                "maximum": MAX_WAIT_SECONDS,
                "default": DEFAULT_WAIT_SECONDS
            });
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
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
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
                    .as_object()
                    .is_some_and(|arguments| {
                        arguments.contains_key("mode") || arguments.contains_key("timeout_seconds")
                    })
            {
                return Err(invalid(
                    "mode and timeout_seconds are only supported by WaitCommands".into(),
                ));
            }
            if args
                .timeout_seconds
                .is_some_and(|seconds| seconds > MAX_WAIT_SECONDS)
            {
                return Err(invalid("timeout_seconds must be 0..60".into()));
            }

            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let mut wait_timed_out = false;
            let snapshots = match self.action {
                CommandJobAction::Status => self
                    .jobs
                    .snapshots(&args.job_ids, !args.job_ids.is_empty())?,
                CommandJobAction::Wait => {
                    let waiting = self.jobs.wait(
                        &args.job_ids,
                        matches!(args.mode, WaitMode::All),
                        cancellation.clone(),
                    );
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(
                            args.timeout_seconds
                                .unwrap_or(DEFAULT_WAIT_SECONDS),
                        ),
                        waiting,
                    )
                    .await
                    {
                        Ok(result) => result?,
                        Err(_) => {
                            if cancellation.is_cancelled() {
                                return Err(ToolError::Cancelled);
                            }
                            wait_timed_out = true;
                            self.jobs
                                .snapshots(&args.job_ids, !args.job_ids.is_empty())?
                        }
                    }
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
            let mut result = json!({ "jobs": snapshots });
            if matches!(self.action, CommandJobAction::Wait) {
                result["wait_timed_out"] = json!(wait_timed_out);
            }
            Ok(ToolOutput::text(result.to_string()))
        })
    }
}
