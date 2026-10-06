use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

use anyhow::Result;
use orbit_model::client::ModelClient;

use super::{ModelOption, ModelRefreshOutput, ModelRefreshTask, app::UiState, background, tabs};

const CHECK_INTERVAL: Duration = Duration::from_secs(1);
const REFRESH_INTERVAL: Duration = Duration::from_secs(900);

pub(super) struct Watcher {
    path: PathBuf,
    auth_directory: PathBuf,
    stamp: Option<(SystemTime, u64)>,
    checked: Instant,
    refreshed: Instant,
    task: Option<ModelRefreshTask>,
}

impl Watcher {
    pub fn new(path: PathBuf, auth_directory: PathBuf) -> Self {
        Self {
            stamp: stamp(&path),
            path,
            auth_directory,
            checked: Instant::now(),
            refreshed: Instant::now(),
            task: None,
        }
    }

    pub fn resynchronize(&mut self) {
        self.task = None;
        self.stamp = None;
        self.checked = Instant::now() - CHECK_INTERVAL;
        self.refreshed = Instant::now();
    }

    pub async fn poll(&mut self) -> Option<Result<ModelRefreshOutput>> {
        if self.checked.elapsed() >= CHECK_INTERVAL {
            self.checked = Instant::now();
            let current = stamp(&self.path);
            if current != self.stamp
                || (self.task.is_none() && self.refreshed.elapsed() >= REFRESH_INTERVAL)
            {
                self.stamp = current;
                self.refreshed = Instant::now();
                self.task = Some(refresh(
                    self.path.clone(),
                    self.auth_directory.clone(),
                    false,
                ));
            }
        }

        if !self.task.as_ref().is_some_and(|task| task.is_finished()) {
            return None;
        }
        let result = self.task.take().unwrap().await;
        // A slow discovery must not overwrite a configuration edited during the request.
        if stamp(&self.path) != self.stamp {
            return None;
        }
        Some(
            result
                .map_err(anyhow::Error::from)
                .and_then(|result| result),
        )
    }
}

fn stamp(path: &std::path::Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

pub(super) fn refresh(path: PathBuf, auth_directory: PathBuf, force: bool) -> ModelRefreshTask {
    tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let version = stamp(&path);
        let mut config = crate::config::FileConfig::load(&path, &auth_directory)?;
        let diagnostics = crate::account_models::refresh(&mut config, force).await;
        anyhow::ensure!(
            stamp(&path) == version,
            "配置文件已更改，请重新刷新模型目录"
        );
        Ok((config.model_options()?, diagnostics))
    }))
}

pub(super) fn publish(
    models: Vec<ModelOption>,
    state: &mut UiState,
    options: &mut Vec<ModelOption>,
    runs: &mut background::Runs,
    tabs: &mut tabs::Tabs,
) {
    state.model_completions = models.iter().map(ModelOption::qualified_name).collect();
    publish_background(models, options, runs, tabs);
}

pub(super) fn publish_background(
    models: Vec<ModelOption>,
    options: &mut Vec<ModelOption>,
    runs: &mut background::Runs,
    tabs: &mut tabs::Tabs,
) {
    let completions = models
        .iter()
        .map(ModelOption::qualified_name)
        .collect::<Vec<_>>();

    for (id, run) in runs {
        run.model_options = models.clone();
        if let Some(tab) = tabs.parked_mut(id) {
            tab.state.model_completions = completions.clone();
        }
    }

    *options = models;
}

/// Apply catalog changes only between runs so a reply keeps its original client and budget.
pub(super) fn reconcile(
    state: &mut UiState,
    model: &mut ModelClient,
    options: &[ModelOption],
) -> bool {
    let Some(choice) = options
        .iter()
        .find(|choice| choice.qualified_name() == state.model)
    else {
        return false;
    };
    let mut config = choice.config.clone();
    if let Some(effort) = state
        .reasoning_effort
        .filter(|effort| choice.reasoning_efforts.contains(effort))
    {
        config.reasoning_effort = Some(effort);
    }

    if model.config() == &config
        && state.max_output_tokens == choice.max_output_tokens
        && state.context_tokens == choice.context_tokens
        && state.input_context_tokens == choice.input_context_tokens
        && state.reasoning_efforts == choice.reasoning_efforts
        && state.reasoning_effort == config.reasoning_effort
        && state.supports_vision == choice.supports_vision
    {
        return false;
    }

    let Ok(client) = ModelClient::new(config) else {
        return false;
    };
    *model = client;
    state.compact_after_model_switch |=
        state.auto_compact && choice.input_context_tokens < state.input_context_tokens;
    state.max_output_tokens = choice.max_output_tokens;
    state.context_tokens = choice.context_tokens;
    state.input_context_tokens = choice.input_context_tokens;
    state.reasoning_efforts = choice.reasoning_efforts.clone();
    state.reasoning_effort = model.config().reasoning_effort;
    state.supports_vision = choice.supports_vision;
    true
}

#[cfg(test)]
mod tests {
    use orbit_model::types::{ReasoningAdapter, ReasoningEffort};

    use super::*;

    fn config(reasoning: &str) -> String {
        format!(
            r#"default_provider = "orbit"
[providers.orbit]
base_url = "http://127.0.0.1:9/v1"
api_key = "orbit-fixture"
managed_models = true
auto_refresh_models = false
[[providers.orbit.models]]
id = "orbit-thinking"
{reasoning}
"#
        )
    }

    fn options(reasoning: &str) -> Vec<ModelOption> {
        crate::config::FileConfig::parse(
            &config(reasoning),
            std::path::Path::new("/tmp/orbit-fixture-auth"),
        )
        .unwrap()
        .model_options()
        .unwrap()
    }

    const REASONING: &str = r#"reasoning = { adapter = "openai_chat", efforts = ["none", "low", "high", "max"], default = "high" }"#;

    #[test]
    fn refresh_updates_client_and_display_without_losing_selected_effort() {
        let old = options("");
        let mut client = ModelClient::new(old[0].config.clone()).unwrap();
        let mut state = UiState::new("orbit::orbit-thinking");
        let mut updated = options(REASONING);
        assert!(reconcile(&mut state, &mut client, &updated));
        assert_eq!(state.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(
            client.config().reasoning_adapter,
            Some(ReasoningAdapter::OpenaiChat)
        );
        assert_eq!(state.reasoning_efforts, updated[0].reasoning_efforts);

        state.reasoning_effort = Some(ReasoningEffort::None);
        updated[0].config.api_key = Some("orbit-rotated-fixture".into());
        assert!(reconcile(&mut state, &mut client, &updated));
        assert_eq!(
            client.config().reasoning_effort,
            Some(ReasoningEffort::None)
        );
        assert_eq!(
            client.config().api_key.as_deref(),
            Some("orbit-rotated-fixture")
        );
        assert!(!reconcile(&mut state, &mut client, &updated));

        updated[0].reasoning_efforts = vec![ReasoningEffort::Low, ReasoningEffort::High];
        updated[0].input_context_tokens = state.input_context_tokens / 2;
        state.auto_compact = true;
        assert!(reconcile(&mut state, &mut client, &updated));
        assert_eq!(state.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(client.config().reasoning_effort, state.reasoning_effort);
        assert!(state.compact_after_model_switch);
    }

    #[test]
    fn missing_model_does_not_switch_provider_implicitly() {
        let choices = options(REASONING);
        let mut state = UiState::new("orbit-retired::orbit-thinking");
        let mut client = ModelClient::new(choices[0].config.clone()).unwrap();
        assert!(!reconcile(&mut state, &mut client, &choices));
        assert_eq!(state.model, "orbit-retired::orbit-thinking");
        assert!(state.reasoning_efforts.is_empty());
    }

    async fn next(watcher: &mut Watcher) -> Result<ModelRefreshOutput> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(result) = watcher.poll().await {
                    break result;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn edited_configuration_refreshes_and_invalid_edits_are_reported_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, config("")).unwrap();
        let mut watcher = Watcher::new(path.clone(), directory.path().join("auth"));
        assert!(watcher.poll().await.is_none());
        std::fs::write(&path, config(REASONING)).unwrap();
        watcher.checked -= CHECK_INTERVAL;
        let (choices, diagnostics) = next(&mut watcher).await.unwrap();
        assert!(diagnostics.is_empty());
        assert_eq!(
            choices[0].config.reasoning_effort,
            Some(ReasoningEffort::High)
        );

        std::fs::write(&path, "invalid configuration").unwrap();
        watcher.checked -= CHECK_INTERVAL;
        assert!(next(&mut watcher).await.is_err());
        watcher.checked -= CHECK_INTERVAL;
        assert!(watcher.poll().await.is_none());

        std::fs::write(&path, config(REASONING)).unwrap();
        watcher.checked -= CHECK_INTERVAL;
        assert!(next(&mut watcher).await.is_ok());
    }

    #[tokio::test]
    async fn periodic_refresh_runs_without_a_configuration_edit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, config(REASONING)).unwrap();
        let mut watcher = Watcher::new(path, directory.path().join("auth"));
        watcher.checked -= CHECK_INTERVAL;
        watcher.refreshed -= REFRESH_INTERVAL;
        assert_eq!(
            next(&mut watcher).await.unwrap().0[0]
                .reasoning_efforts
                .len(),
            4
        );
    }

    #[tokio::test]
    async fn stale_discovery_result_does_not_replace_a_newer_edit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, config("")).unwrap();
        let mut watcher = Watcher::new(path.clone(), directory.path().join("auth"));
        let task = tokio::spawn(async { Ok((options(""), Vec::new())) });
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
        watcher.task = Some(tokio_util::task::AbortOnDropHandle::new(task));
        std::fs::write(&path, config(REASONING)).unwrap();
        assert!(watcher.poll().await.is_none());
        watcher.checked -= CHECK_INTERVAL;
        assert_eq!(
            next(&mut watcher).await.unwrap().0[0]
                .config
                .reasoning_effort,
            Some(ReasoningEffort::High)
        );
    }

    #[tokio::test]
    async fn manual_refresh_discards_pending_automatic_results() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, config(REASONING)).unwrap();
        let mut watcher = Watcher::new(path, directory.path().join("auth"));
        let task = tokio::spawn(async { Ok((options(""), Vec::new())) });
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
        watcher.task = Some(tokio_util::task::AbortOnDropHandle::new(task));
        watcher.resynchronize();
        let (choices, _) = next(&mut watcher).await.unwrap();
        assert_eq!(
            choices[0].config.reasoning_effort,
            Some(ReasoningEffort::High)
        );
    }
}
