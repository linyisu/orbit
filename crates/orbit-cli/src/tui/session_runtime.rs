//! Session ownership and Agent execution for the interactive client.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use orbit_agent::{
    agent::{Agent, AgentConfig, AgentResult},
    context::{ContextCheckpoint, ContextConfig, ContextRuntime},
    error::AgentError,
    events::AgentEvent,
    model::ModelClientBackend,
    tool::ToolRegistry,
};
use orbit_model::{
    client::ModelClient,
    types::{ChatMessage, ReasoningEffort},
};
use orbit_store::{SessionWriter, Store};
use orbit_tools::{InstructionSet, WriteFile, approval::ApprovalSender};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    FileConfig, ModelOption, TuiArgs, coding_request, open_store_and_instructions,
    persist_agent_event, project_state, result_status, runtime, token_calibration,
};

use super::app::App;

pub(super) type RunResult = Result<AgentResult, AgentError>;

pub(super) enum RunOutcome {
    Agent {
        writer: SessionWriter,
        result: RunResult,
        finish: Result<()>,
    },
    Compaction(Result<CompactionProgress>),
    Undo(Result<String>),
}

pub(super) struct CompactionProgress {
    pub(super) batches: usize,
    pub(super) degraded: bool,
    pub(super) complete: bool,
}

pub(super) struct ActiveRun {
    pub(super) events: mpsc::Receiver<AgentEvent>,
    pub(super) task: JoinHandle<RunOutcome>,
    pub(super) cancellation: CancellationToken,
}

pub(super) struct Runtime {
    config_path: PathBuf,
    pub(super) workspace: PathBuf,
    pub(super) config: FileConfig,
    pub(super) store: Store,
    client: ModelClient,
    selected: ModelOption,
    reasoning: Option<ReasoningEffort>,
    instructions: InstructionSet,
    approval_sender: ApprovalSender,
    pub(super) writer: Option<SessionWriter>,
    tools: ToolRegistry,
    undo_writer: WriteFile,
    max_turns: usize,
    max_tool_calls: usize,
}

impl Runtime {
    pub(super) async fn open(
        config_path: PathBuf,
        database: PathBuf,
        args: &TuiArgs,
        approval_sender: ApprovalSender,
    ) -> Result<Self> {
        ensure!(
            (1..=256).contains(&args.max_turns),
            "max_turns must be 1–256"
        );
        ensure!(
            (1..=1024).contains(&args.max_tool_calls),
            "max_tool_calls must be 1–1024"
        );
        let workspace = std::env::current_dir()?.canonicalize()?;
        let config = FileConfig::load(&config_path)?;
        let (store, instructions) = open_store_and_instructions(
            &workspace,
            &config_path,
            &database,
            config.instruction_home.as_deref(),
        )
        .await?;
        let mut selected = config.select_model(args.model.as_deref())?;
        let mut saved_effort = None;
        let writer = if let Some(id) = args.resume.as_deref() {
            let session = store.session(id).await?;
            ensure!(
                session.workspace == workspace.to_string_lossy(),
                "session belongs to a different workspace"
            );
            if args.model.is_none() {
                selected = config.select_model(Some(&session.model))?;
            }
            if args.model.is_none() || selected.name == session.model {
                saved_effort = session.reasoning_effort;
            }
            store.acquire(id).await?
        } else {
            store.create(&workspace, &selected.name).await?
        };
        let reasoning = selected.select_reasoning(args.reasoning, saved_effort.as_deref())?;
        let client = ModelClient::new(selected.config.clone())?;
        let tools = tools_for(
            &workspace,
            &config_path,
            &store,
            writer.id(),
            approval_sender.clone(),
            &config,
        )?;
        let undo_writer = runtime::workspace_writer(
            &workspace,
            &config_path,
            store.path(),
            approval_sender.clone(),
        )?;
        store
            .set_session_profile(
                writer.id(),
                &selected.name,
                reasoning.map(ReasoningEffort::as_str),
            )
            .await?;

        Ok(Self {
            config_path,
            workspace,
            config,
            store,
            client,
            selected,
            reasoning,
            instructions,
            approval_sender,
            writer: Some(writer),
            tools,
            undo_writer,
            max_turns: args.max_turns,
            max_tool_calls: args.max_tool_calls,
        })
    }

    pub(super) fn session_id(&self) -> &str {
        self.writer
            .as_ref()
            .expect("session writer present while idle")
            .id()
    }

    pub(super) fn selected_model(&self) -> &ModelOption {
        &self.selected
    }

    pub(super) fn reasoning(&self) -> Option<ReasoningEffort> {
        self.reasoning
    }

    pub(super) fn model_options(&self) -> Result<Vec<ModelOption>> {
        self.config.model_options()
    }

    pub(super) async fn select_model(&mut self, name: &str, app: &mut App) -> Result<()> {
        let selected = self.config.select_model(Some(name))?;
        let client = ModelClient::new(selected.config.clone())?;
        let saved = (selected.name == self.selected.name)
            .then(|| self.reasoning.map(ReasoningEffort::as_str))
            .flatten();
        let reasoning = selected.select_reasoning(None, saved)?;
        self.store
            .set_session_profile(
                self.session_id(),
                &selected.name,
                reasoning.map(ReasoningEffort::as_str),
            )
            .await?;
        self.client = client;
        self.selected = selected;
        self.reasoning = reasoning;
        if !self.selected.supports_vision && !app.pending_images.is_empty() {
            app.pending_images.clear();
            app.notice("Pending images cleared because this model does not support image input");
        }
        app.model = self.selected.name.clone();
        app.context_tokens = self.selected.context_tokens;
        app.reasoning = reasoning;
        app.context_used_tokens = self.context_usage().await?.0;
        app.notice(format!("Model: {}", app.model));
        Ok(())
    }

    pub(super) async fn select_reasoning(
        &mut self,
        effort: ReasoningEffort,
        app: &mut App,
    ) -> Result<()> {
        let reasoning = self.selected.select_reasoning(Some(effort), None)?;
        self.store
            .set_session_profile(
                self.session_id(),
                &self.selected.name,
                reasoning.map(ReasoningEffort::as_str),
            )
            .await?;
        self.reasoning = reasoning;
        app.reasoning = reasoning;
        app.notice(format!("Reasoning: {}", effort.as_str()));
        Ok(())
    }

    pub(super) async fn attach_image(&self, path: &Path, app: &mut App) -> Result<()> {
        ensure!(
            self.selected.supports_vision,
            "selected model does not support image input"
        );
        ensure!(
            app.pending_images.len() < 4,
            "at most four images per message"
        );
        let attachment = self.store.save_image(self.session_id(), path).await?;
        app.notice(format!(
            "Image attached for next message: {}",
            attachment.name()
        ));
        app.pending_images.push(attachment);
        Ok(())
    }

    pub(super) async fn switch_session(&mut self, id: Option<&str>, app: &mut App) -> Result<()> {
        if id.is_some_and(|id| id == self.session_id()) {
            app.notice("This session is already open.");
            return Ok(());
        }
        let (selected, saved_effort) = if let Some(id) = id {
            let session = self.store.session(id).await?;
            ensure!(
                session.workspace == self.workspace.to_string_lossy(),
                "session belongs to a different workspace"
            );
            (
                self.config.select_model(Some(&session.model))?,
                session.reasoning_effort,
            )
        } else {
            (self.config.select_model(None)?, None)
        };
        let reasoning = selected.select_reasoning(None, saved_effort.as_deref())?;
        let next = if let Some(id) = id {
            self.store.acquire(id).await?
        } else {
            self.store.create(&self.workspace, &selected.name).await?
        };
        let next_client = ModelClient::new(selected.config.clone())?;
        let next_tools = tools_for(
            &self.workspace,
            &self.config_path,
            &self.store,
            next.id(),
            self.approval_sender.clone(),
            &self.config,
        )?;
        self.store
            .set_session_profile(
                next.id(),
                &selected.name,
                reasoning.map(ReasoningEffort::as_str),
            )
            .await?;

        self.writer = Some(next);
        self.tools = next_tools;
        self.client = next_client;
        self.selected = selected;
        self.reasoning = reasoning;
        app.session_id = self.session_id().to_owned();
        app.model = self.selected.name.clone();
        app.context_tokens = self.selected.context_tokens;
        app.reasoning = reasoning;
        app.scroll = 0;
        app.begin_restore();
        app.context_used_tokens = self.context_usage().await?.0;
        Ok(())
    }

    pub(super) async fn start_run(&mut self, prompt: String, app: &mut App) -> Result<ActiveRun> {
        ensure!(
            app.pending_images.is_empty() || self.selected.supports_vision,
            "selected model does not support image input"
        );
        let session_id = self.session_id().to_owned();
        let snapshot = self.store.load(&session_id).await?;
        let state =
            project_state::projection(&self.store, &self.workspace.to_string_lossy(), &session_id)
                .await?;
        let mut history = snapshot.projected_history();
        let user = ChatMessage::user_with_attachments(prompt.clone(), app.pending_images.clone());
        history.push(user.clone());
        let mut request = coding_request(history, &self.instructions, state);
        request.max_tokens = Some(
            u32::try_from(self.selected.max_output_tokens)
                .context("max_output_tokens exceeds the model request limit")?,
        );
        request.reasoning_effort = self.reasoning;
        let checkpoint = self
            .store
            .load_context_checkpoint(&session_id)
            .await?
            .and_then(|payload| serde_json::from_str::<ContextCheckpoint>(&payload).ok());
        let calibration = token_calibration(&self.store, &session_id, &self.selected.name).await?;
        let context = ContextRuntime::new(
            ContextConfig {
                window_tokens: self.selected.context_tokens,
                output_reserve: self.selected.max_output_tokens,
                force_compact: false,
            },
            checkpoint,
        )
        .map_err(|message| anyhow::anyhow!(message))?
        .with_calibration(calibration);
        let mut writer = self.writer.take().context("session is already running")?;
        if let Err(error) = writer.begin_user(user, self.selected.name.clone()).await {
            self.writer = Some(writer);
            return Err(error);
        }
        let agent = Agent::new(
            ModelClientBackend::new(self.client.clone()),
            self.tools.clone(),
            AgentConfig {
                max_turns: self.max_turns,
                max_tool_calls: self.max_tool_calls,
            },
        )
        .with_context(context);
        let (sender, events) = mpsc::channel(256);
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let store = self.store.clone();
        let model_name = self.selected.name.clone();
        let task = tokio::spawn(async move {
            let result = agent
                .run_with_agent_events(request, token, |event| {
                    let writer = &writer;
                    let store = &store;
                    let sender = &sender;
                    let model_name = &model_name;
                    async move {
                        persist_agent_event(writer, store, model_name, &event).await?;
                        let _ = sender.send(event).await;
                        Ok(())
                    }
                })
                .await;
            let status = result_status(&result);
            let finish = writer.finish(status).await;
            RunOutcome::Agent {
                writer,
                result,
                finish,
            }
        });
        let images = std::mem::take(&mut app.pending_images);
        app.submit(&prompt, &images);
        Ok(ActiveRun {
            events,
            task,
            cancellation,
        })
    }

    pub(super) async fn context_usage(&self) -> Result<(usize, usize, bool)> {
        let session_id = self.session_id();
        let snapshot = self.store.load(session_id).await?;
        let state =
            project_state::projection(&self.store, &self.workspace.to_string_lossy(), session_id)
                .await?;
        let request = coding_request(snapshot.projected_history(), &self.instructions, state);
        let checkpoint = self
            .store
            .load_context_checkpoint(session_id)
            .await?
            .and_then(|payload| serde_json::from_str::<ContextCheckpoint>(&payload).ok());
        let calibration = token_calibration(&self.store, session_id, &self.selected.name).await?;
        let context = ContextRuntime::new(
            ContextConfig {
                window_tokens: self.selected.context_tokens,
                output_reserve: self.selected.max_output_tokens,
                force_compact: false,
            },
            checkpoint.clone(),
        )
        .map_err(|message| anyhow::anyhow!(message))?
        .with_calibration(calibration);
        let definitions = self
            .tools
            .definitions()
            .into_iter()
            .filter(|definition| {
                self.selected.supports_vision || definition.function.name != "read_image"
            })
            .collect::<Vec<_>>();
        let used = context.estimate_provider_tokens(
            &request.messages,
            &definitions,
            self.selected.supports_vision,
        );
        let active_checkpoint = context.has_valid_checkpoint(&request.messages);
        Ok((used, self.selected.context_tokens, active_checkpoint))
    }

    pub(super) async fn start_compaction(&self, app: &mut App) -> Result<ActiveRun> {
        let session_id = self.session_id().to_owned();
        let snapshot = self.store.load(&session_id).await?;
        let state =
            project_state::projection(&self.store, &self.workspace.to_string_lossy(), &session_id)
                .await?;
        let request = coding_request(snapshot.projected_history(), &self.instructions, state);
        let checkpoint = self
            .store
            .load_context_checkpoint(&session_id)
            .await?
            .and_then(|payload| serde_json::from_str::<ContextCheckpoint>(&payload).ok());
        let calibration = token_calibration(&self.store, &session_id, &self.selected.name).await?;
        let mut context = ContextRuntime::new(
            ContextConfig {
                window_tokens: self.selected.context_tokens,
                output_reserve: self.selected.max_output_tokens,
                force_compact: true,
            },
            checkpoint,
        )
        .map_err(|message| anyhow::anyhow!(message))?
        .with_calibration(calibration);
        let backend = ModelClientBackend::new(self.client.clone());
        let definitions = self.tools.definitions();
        let store = self.store.clone();
        let (sender, events) = mpsc::channel(64);
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let task = tokio::spawn(async move {
            let mut progress = CompactionProgress {
                batches: 0,
                degraded: false,
                complete: false,
            };
            let result = async {
                for _ in 0..64 {
                    let step = context
                        .prepare_step(&backend, &request.messages, &definitions, token.clone())
                        .await
                        .map_err(anyhow::Error::new)?;
                    if let Some(checkpoint) = step.checkpoint {
                        progress.degraded |= checkpoint.summary_degraded;
                        let payload = serde_json::to_string(&checkpoint)?;
                        store
                            .save_context_checkpoint(
                                &session_id,
                                checkpoint.through,
                                &checkpoint.prefix_sha256,
                                &payload,
                            )
                            .await?;
                        progress.batches += 1;
                        let _ = sender
                            .send(AgentEvent::ContextCheckpoint { checkpoint })
                            .await;
                    }
                    if !step.needs_more {
                        progress.complete = true;
                        break;
                    }
                }
                Ok(progress)
            }
            .await;
            RunOutcome::Compaction(result)
        });
        app.running = true;
        app.status = "Compacting context".to_owned();
        app.notice("/compact");
        Ok(ActiveRun {
            events,
            task,
            cancellation,
        })
    }

    pub(super) async fn start_undo(&self, path: Option<&str>, app: &mut App) -> Result<ActiveRun> {
        let changes = self.store.workspace_changes(self.session_id()).await?;
        let record = changes
            .into_iter()
            .rev()
            .find(|record| !record.undone && path.is_none_or(|path| record.change.path == path))
            .context("No matching file change to undo in this session")?;
        let session_id = self.session_id().to_owned();
        let store = self.store.clone();
        let writer = self.undo_writer.clone();
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let (_, events) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            let result = async {
                let path = record.change.path.clone();
                let receipt = writer.restore(record.change, token).await?;
                store
                    .record_workspace_undo(&session_id, record.id)
                    .await
                    .with_context(|| {
                        format!(
                            "file was restored, but its write receipt could not be marked undone: {receipt}"
                        )
                    })?;
                Ok(path)
            }
            .await;
            RunOutcome::Undo(result)
        });
        app.running = true;
        app.status = "Restoring file".to_owned();

        Ok(ActiveRun {
            events,
            task,
            cancellation,
        })
    }
}

fn tools_for(
    workspace: &std::path::Path,
    config_path: &std::path::Path,
    store: &Store,
    session_id: &str,
    approvals: ApprovalSender,
    config: &FileConfig,
) -> Result<ToolRegistry> {
    runtime::workspace_tools(
        workspace,
        config_path,
        store.path(),
        store.clone(),
        session_id,
        approvals,
        runtime::WorkspaceToolOptions {
            rust_toolchain: config.command.rust_toolchain.clone(),
            web: config.web.clone(),
            web_enabled: true,
        },
    )
}
