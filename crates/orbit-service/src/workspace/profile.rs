use anyhow::{Result, ensure};
use orbit_model::{client::ModelClient, types::ReasoningEffort};

use super::protocol::SessionInfo;
use crate::{PreparedWorkspace, WorkspaceResources};

pub(super) fn describe(setup: &PreparedWorkspace) -> SessionInfo {
    let resources = &setup.resources;
    let budget = &resources.budget;
    SessionInfo {
        persisted: setup.session.is_persisted(),
        mode: resources.mode,
        mode_revision: resources.mode_revision,
        branch: None,
        home: resources
            .options
            .home
            .as_ref()
            .map(|home| home.to_string_lossy().into_owned()),
        cursor_trail: resources.cursor_trail,
        auto_compact: budget.auto_compact,
        skills: budget.skills.completions(),
        id: setup.session.id().into(),
        title: setup
            .session
            .snapshot
            .session
            .title
            .clone(),
        workspace: setup
            .session
            .snapshot
            .session
            .workspace
            .clone(),
        model: resources.model_name.clone(),
        model_catalog: budget
            .model_options
            .iter()
            .map(|model| orbit_protocol::ModelSummary {
                provider: model.provider.clone(),
                model: model.model.clone(),
                supports_vision: model.supports_vision,
                reasoning_effort: model
                    .config
                    .reasoning_effort
                    .map(|effort| effort.as_str().into()),
                reasoning_efforts: model
                    .reasoning_efforts
                    .iter()
                    .map(|effort| effort.as_str().into())
                    .collect(),
            })
            .collect(),
        models: budget
            .model_options
            .iter()
            .map(crate::models::ModelOption::qualified_name)
            .collect(),
        supports_vision: resources.model.supports_vision(),
        reasoning_effort: resources
            .model
            .config()
            .reasoning_effort
            .map(|effort| effort.as_str().to_owned()),
        reasoning_efforts: budget
            .model_options
            .iter()
            .find(|model| model.qualified_name() == resources.model_name)
            .map(|model| {
                model
                    .reasoning_efforts
                    .iter()
                    .map(|effort| effort.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default(),
        context_tokens: budget.context_tokens,
        input_context_tokens: budget.input_context_tokens,
        max_output_tokens: budget.max_output_tokens,
        context_used_tokens: Some(
            setup
                .session
                .calibration(&resources.model_name)
                .estimate(orbit_agent::context::estimate_tokens(
                    &setup.session.snapshot.history,
                    &resources.tools.definitions(),
                )),
        ),
    }
}

pub(super) fn apply(
    resources: &mut WorkspaceResources,
    name: &str,
    effort: Option<ReasoningEffort>,
) -> Result<()> {
    let candidates = resources
        .budget
        .model_options
        .iter()
        .filter(|option| {
            option.qualified_name() == name || (!name.contains("::") && option.model == name)
        })
        .collect::<Vec<_>>();
    ensure!(
        candidates.len() == 1,
        "model is missing or ambiguous; use provider::model: {name}"
    );
    let selected = candidates[0];
    ensure!(
        effort.is_none_or(|effort| selected
            .reasoning_efforts
            .contains(&effort)),
        "reasoning effort is not supported by this model"
    );
    let mut config = selected.config.clone();
    if let Some(effort) = effort {
        config.reasoning_effort = Some(effort);
    }
    resources.model = ModelClient::new(config.clone())?;
    if let Some(subagents) = &resources.subagents {
        let mut inherited = selected.clone();
        inherited.config = config;
        subagents.set_parent_model(inherited);
    }
    resources.model_name = selected.qualified_name();
    resources.budget.context_tokens = selected.context_tokens;
    resources.budget.input_context_tokens = selected.input_context_tokens;
    resources.budget.max_output_tokens = selected.max_output_tokens;
    Ok(())
}

pub(super) async fn select(
    setup: &mut PreparedWorkspace,
    name: &str,
    effort: Option<&str>,
) -> Result<()> {
    let effort = effort
        .map(str::parse)
        .transpose()
        .map_err(|()| anyhow::anyhow!("unknown reasoning effort"))?;
    apply(&mut setup.resources, name, effort)?;
    setup
        .session
        .set_model_profile(
            setup.resources.model_name.clone(),
            setup
                .resources
                .model
                .config()
                .reasoning_effort,
        )
        .await
}
