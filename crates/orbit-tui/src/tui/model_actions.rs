use super::*;

pub(super) fn needs_model_switch_compaction(state: &UiState) -> bool {
    state.auto_compact
        && !state.is_home()
        && state.context_used_tokens >= state.input_context_tokens.saturating_mul(85) / 100
}

pub(super) async fn select_model(
    qualified_name: &str,
    selected_effort: Option<orbit_model::types::ReasoningEffort>,
    state: &mut UiState,
    model: &mut ModelClient,
    model_options: &[ModelOption],
    session: &mut SessionState,
) -> Result<()> {
    let Some(choice) = model_options
        .iter()
        .find(|choice| choice.qualified_name() == qualified_name)
    else {
        state.report_local(TranscriptItem::Error("所选模型已不在当前配置中。".into()));
        return Ok(());
    };

    let mut config = choice.config.clone();

    if let Some(effort) = selected_effort {
        config.reasoning_effort = choice.reasoning_efforts.contains(&effort).then_some(effort);
    }

    match ModelClient::new(config) {
        Ok(client) => {
            let previous_model = state.model.clone();
            let model_name = choice.qualified_name();
            let reasoning_effort = client.config().reasoning_effort;
            session
                .set_model_profile(model_name.clone(), reasoning_effort)
                .await?;
            *model = client;
            state.model = model_name;
            state.compact_after_model_switch |=
                state.auto_compact && choice.input_context_tokens < state.input_context_tokens;
            state.max_output_tokens = choice.max_output_tokens;
            state.context_tokens = choice.context_tokens;
            state.input_context_tokens = choice.input_context_tokens;
            state.reasoning_effort = reasoning_effort;
            state.reasoning_efforts = choice.reasoning_efforts.clone();
            state.supports_vision = choice.supports_vision;

            if previous_model != state.model {
                state.report_local(TranscriptItem::CommandResult(format!(
                    "{previous_model} → {}",
                    state.model
                )));
            }

            if !choice.supports_vision && !state.attachments.is_empty() {
                state.attachments.clear();
                state.report_local(TranscriptItem::Notice(
                    "所选模型不支持图片输入，已清除待发送的图片附件。".into(),
                ));
            }
        }

        Err(error) => state.report_local(TranscriptItem::Error(error.to_string())),
    }

    Ok(())
}
