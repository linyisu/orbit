use super::*;

pub(super) fn queue_approval(
    request: orbit_tools::approval::ApprovalRequest,
    state: &mut UiState,
    session: &SessionState,
    pending: &mut Option<orbit_tools::approval::ApprovalRequest>,
) {
    if request.decision.is_closed() {
        return;
    }
    let auto_allowed = match &request.preview {
        orbit_tools::approval::ApprovalPreview::Write(p) => orbit_tools::approval::file_grant_key(
            &p.path,
            std::path::Path::new(&session.snapshot.session.workspace),
        )
        .is_some_and(|key| state.session_allowed_files.contains(&key)),

        orbit_tools::approval::ApprovalPreview::Delete(p) => orbit_tools::approval::file_grant_key(
            &p.path,
            std::path::Path::new(&session.snapshot.session.workspace),
        )
        .is_some_and(|key| state.session_allowed_files.contains(&key)),

        orbit_tools::approval::ApprovalPreview::Command(cmd) => {
            if let Some(git) = &cmd.git {
                match git.kind {
                    orbit_runtime::git::GitOperationKind::LocalWrite { .. }
                    | orbit_runtime::git::GitOperationKind::RemoteWrite { .. }
                    | orbit_runtime::git::GitOperationKind::ReadOnlyQuery { .. } => cmd
                        .git_grant_key(std::path::Path::new(&session.snapshot.session.workspace))
                        .is_some_and(|key| state.session_git_commands.contains(&key)),

                    _ => false,
                }
            } else {
                state.session_commands.contains(
                    &cmd.request_grant_key(std::path::Path::new(
                        &session.snapshot.session.workspace,
                    )),
                )
            }
        }

        _ => false,
    };

    if auto_allowed {
        let _ = request.decision.send(true);
        return;
    }

    if state.permission_mode.allows(
        &request.preview,
        std::path::Path::new(&session.snapshot.session.workspace),
    ) {
        let _ = request.decision.send(true);
        return;
    }

    state.selection = Default::default();
    state.approval = Some(request.preview.clone());
    state.approval_scroll = 0;
    state.approval_shown = false;
    state.approval_choice = 0;
    *pending = Some(request);
    state.pause_clock();
}

pub(super) fn apply_live_event(event: AgentEvent, state: &mut UiState) {
    // The joined task is authoritative: persist terminal status with its context atomically.
    if matches!(
        event,
        AgentEvent::Completed { .. } | AgentEvent::Failed { .. } | AgentEvent::Cancelled
    ) {
        return;
    }

    state.apply(event);
}

pub(super) fn refresh_context_usage(
    state: &mut UiState,
    history: &[ChatMessage],
    tools: &ToolRegistry,
) {
    // While a request is live, ContextUsage/RequestMetrics describe the actual
    // model input. The completed-history buffer can still describe an older turn.
    if matches!(
        state.status,
        app::RunStatus::Running | app::RunStatus::Cancelling
    ) {
        return;
    }

    state.context_used_tokens =
        state
            .calibration()
            .estimate(orbit_agent::context::estimate_tokens(
                history,
                &tools.definitions(),
            ));
}

pub(super) fn tool_context_tokens(history: &[ChatMessage], name: &str) -> usize {
    let ids: std::collections::HashSet<_> = history
        .iter()
        .flat_map(|message| match message {
            ChatMessage::Assistant { tool_calls, .. } => tool_calls.as_slice(),
            _ => &[],
        })
        .filter(|call| call.function.name == name)
        .map(|call| call.id.as_str())
        .collect();
    let messages: Vec<_> = history.iter().filter(|message| {
        matches!(message, ChatMessage::Tool { tool_call_id, .. } if ids.contains(tool_call_id.as_str()))
    }).cloned().collect();

    if messages.is_empty() {
        0
    } else {
        orbit_agent::context::estimate_tokens(&messages, &[])
    }
}

pub(super) fn format_undo_result(result: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(result) else {
        return result.into();
    };

    let path = value["path"].as_str().unwrap_or("文件");

    if value["created_file_removed"].as_bool() == Some(true) {
        format!("已回退 {path}：已删除由 Orbit 创建的文件。")
    } else if value["restored"].as_bool() == Some(true) {
        format!("已回退 {path}：已恢复到 Orbit 写入前的内容。")
    } else {
        result.into()
    }
}

pub(super) fn steering_history(
    history: &[ChatMessage],
    interrupted_prompt: Option<ChatMessage>,
) -> Vec<ChatMessage> {
    let mut history = history.to_vec();

    if let Some(prompt) = interrupted_prompt {
        history.push(prompt);
    }

    history
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn start_agent_run(
    prompt: String,
    attachments: Vec<Attachment>,
    compacting: bool,
    state: &mut UiState,
    session: &mut SessionState,
    history: &[ChatMessage],
    model: &ModelClient,
    tools: &ToolRegistry,
    context_tokens: usize,
    auto_compact: bool,
    max_turns: usize,
    max_tool_calls: usize,
    compaction: &orbit_agent::compaction::CompactionRuntime,
) -> Result<StartedRun> {
    let display_prompt = if attachments.is_empty() {
        prompt.clone()
    } else {
        let labels = attachments
            .iter()
            .map(|attachment| format!("[图片：{}]", attachment.name()))
            .collect::<Vec<_>>()
            .join(" ");
        if prompt.is_empty() {
            labels
        } else {
            format!("{prompt}\n{labels}")
        }
    };
    let started = crate::execution::start(
        session,
        prompt,
        attachments,
        compacting,
        history,
        model,
        tools,
        crate::execution::RunOptions {
            model_name: state.model.clone(),
            generate_title: true,
            max_output_tokens: state.max_output_tokens,
            config: AgentConfig {
                context_tokens,
                auto_compact,
                max_turns,
                max_tool_calls,
            },
            calibration: state.calibration(),
            compaction: compaction.clone(),
        },
    )
    .await?;
    if compacting {
        state.begin_run();
    } else {
        state.submit(display_prompt);
    }
    state.context_used_tokens = started.context_used_tokens;
    Ok(started)
}
