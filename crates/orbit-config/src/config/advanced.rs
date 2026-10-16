//! A reviewed configuration snapshot for native advanced Agent settings.
use super::FileConfig;
use anyhow::{Result, ensure};
use orbit_protocol::Capability;
use std::{collections::BTreeSet, path::Path};

#[derive(Clone)]
pub struct AgentSettings {
    pub max_turns: usize,
    pub max_tool_calls: usize,
    pub auto_compact: bool,
    pub command: orbit_tools::run_command::CommandLimits,
    pub subagents: crate::subagents::SubagentConfig,
    pub disabled_capabilities: Vec<Capability>,
    pub disabled_tools: Vec<String>,
}

impl From<&FileConfig> for AgentSettings {
    fn from(config: &FileConfig) -> Self {
        Self {
            max_turns: config.max_turns,
            max_tool_calls: config.max_tool_calls,
            auto_compact: config.auto_compact,
            command: config.command.clone(),
            subagents: config.subagents.clone(),
            disabled_capabilities: config.disabled_capabilities.clone(),
            disabled_tools: config.disabled_tools.clone(),
        }
    }
}

impl AgentSettings {
    /// Apply textual limit overrides and validate them before any configuration write.
    pub fn with_limits(&self, values: [&str; 11]) -> Result<Self> {
        let mut settings = self.clone();
        settings.max_turns = number(values[0], "会话轮数上限")?;
        settings.max_tool_calls = number(values[1], "会话工具调用上限")?;
        settings.command.default_timeout_seconds = number(values[2], "默认命令超时")?;
        settings.command.max_timeout_seconds = number(values[3], "最长命令超时")?;
        settings.command.read_file_scan_bytes = number(values[4], "文件扫描上限")?;
        settings.subagents.max_concurrent = number(values[5], "并行子 Agent 上限")?;
        settings.subagents.max_turns = number(values[6], "子 Agent 轮数上限")?;
        settings.subagents.max_tool_calls = number(values[7], "子 Agent 工具调用上限")?;
        settings.subagents.timeout_seconds = number(values[8], "子 Agent 进度提醒间隔")?;
        let optional = |value: &str| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        settings.subagents.explore_model = optional(values[9]);
        settings.subagents.review_model = optional(values[10]);
        settings.subagents.validate()?;
        settings
            .command
            .validate()
            .map_err(anyhow::Error::msg)?;
        Ok(settings)
    }

    pub fn set_network_allowed(&mut self, allowed: bool) {
        self.command.network = if allowed {
            orbit_runtime::capability::NetworkPolicy::Allow
        } else {
            orbit_runtime::capability::NetworkPolicy::Deny
        };
    }
}

fn number<T: std::str::FromStr>(value: &str, label: &str) -> Result<T> {
    value
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("{label}必须为非负整数。"))
}

pub(super) fn validate_disabled(capabilities: &[Capability], tools: &[String]) -> Result<()> {
    let mut seen = BTreeSet::new();
    ensure!(
        capabilities
            .iter()
            .all(|capability| seen.insert(capability)),
        "禁用能力列表不能重复"
    );
    let mut seen = BTreeSet::new();
    for name in tools {
        ensure!(
            !name.is_empty()
                && name.len() <= 256
                && !name
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control()),
            "禁用工具名称必须为不含空白的完整名称，且不超过 256 字节"
        );
        ensure!(seen.insert(name), "禁用工具“{name}”重复");
    }
    Ok(())
}

#[derive(Clone)]
pub struct AdvancedDraft {
    source: String,
    settings: AgentSettings,
}

impl AdvancedDraft {
    pub fn load(path: &Path, auth: &Path) -> Result<Self> {
        let source = super::connections::read_config(path)?;
        let config = FileConfig::parse(&source, auth)?;
        Ok(Self {
            source,
            settings: AgentSettings::from(&config),
        })
    }

    pub fn settings(&self) -> &AgentSettings {
        &self.settings
    }

    pub fn save(&self, path: &Path, auth: &Path, settings: AgentSettings) -> Result<Self> {
        validate_disabled(&settings.disabled_capabilities, &settings.disabled_tools)?;
        settings.subagents.validate()?;
        settings
            .command
            .validate()
            .map_err(anyhow::Error::msg)?;
        let mut document = self
            .source
            .parse::<toml_edit::DocumentMut>()?;
        set(&mut document, "max_turns", &settings.max_turns)?;
        set(&mut document, "max_tool_calls", &settings.max_tool_calls)?;
        set(&mut document, "auto_compact", &settings.auto_compact)?;
        set(
            &mut document,
            "disabled_capabilities",
            &settings.disabled_capabilities,
        )?;
        set(&mut document, "disabled_tools", &settings.disabled_tools)?;
        set_section(
            &mut document,
            "subagents",
            &settings.subagents,
            &[
                "enabled",
                "max_concurrent",
                "max_turns",
                "max_tool_calls",
                "timeout_seconds",
                "explore_model",
                "review_model",
            ],
        )?;
        set_section(
            &mut document,
            "command",
            &settings.command,
            &[
                "network",
                "default_timeout_seconds",
                "max_timeout_seconds",
                "read_file_scan_bytes",
            ],
        )?;
        let source = document.to_string();
        let config = FileConfig::parse(&source, auth)?;
        for model in [
            &config.subagents.explore_model,
            &config.subagents.review_model,
        ]
        .into_iter()
        .flatten()
        {
            config
                .select_model(model)
                .map_err(|error| anyhow::anyhow!("子 Agent 模型“{model}”不可用：{error}"))?;
        }
        super::connections::persist(path, &self.source, &source)?;
        Ok(Self { source, settings })
    }
}

fn set(
    document: &mut toml_edit::DocumentMut,
    name: &str,
    value: &impl serde::Serialize,
) -> Result<()> {
    let mut values = toml::map::Map::new();
    values.insert(name.to_string(), toml::Value::try_from(value)?);
    let encoded = toml::to_string(&values)?.parse::<toml_edit::DocumentMut>()?;
    if let Some(existing) = document.get_mut(name) {
        let decor = existing
            .as_value()
            .map(|value| value.decor().clone());
        *existing = encoded[name].clone();
        if let (Some(decor), Some(value)) = (decor, existing.as_value_mut()) {
            *value.decor_mut() = decor;
        }
    } else {
        document[name] = encoded[name].clone();
    }
    Ok(())
}

fn set_section(
    document: &mut toml_edit::DocumentMut,
    name: &str,
    value: &impl serde::Serialize,
    fields: &[&str],
) -> Result<()> {
    let encoded = toml::to_string(value)?.parse::<toml_edit::DocumentMut>()?;
    if document.get(name).is_none() {
        document[name] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let destination = document[name]
        .as_table_like_mut()
        .ok_or_else(|| anyhow::anyhow!("{name} 配置不是表格"))?;
    for &field in fields {
        let Some(value) = encoded.get(field) else {
            destination.remove(field);
            continue;
        };
        let decor = destination
            .get(field)
            .and_then(toml_edit::Item::as_value)
            .map(|value| value.decor().clone());
        let mut value = value.clone();
        if let (Some(decor), Some(value)) = (decor, value.as_value_mut()) {
            *value.decor_mut() = decor;
        }
        destination.insert(field, value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_ceilings_parse_with_defaults_and_reject_invalid_entries() {
        let auth = tempfile::tempdir().unwrap();
        let defaults = FileConfig::parse("[providers]\n", auth.path()).unwrap();
        assert!(
            defaults
                .disabled_capabilities
                .is_empty()
        );
        assert!(defaults.disabled_tools.is_empty());
        let configured = FileConfig::parse(
            "disabled_capabilities = ['all', 'write']\ndisabled_tools = \
                ['MCP_Server_Tool']\n[providers]\n",
            auth.path(),
        )
        .unwrap();
        assert_eq!(
            configured.disabled_capabilities,
            [Capability::All, Capability::Write]
        );
        assert_eq!(configured.disabled_tools, ["MCP_Server_Tool"]);
        for invalid in [
            "disabled_capabilities = ['web', 'web']",
            "disabled_tools = ['ReadFile', 'ReadFile']",
            "disabled_tools = ['']",
            "disabled_tools = ['read file']",
            "disabled_capabilities = ['unknown']",
        ] {
            assert!(
                FileConfig::parse(&format!("{invalid}\n[providers]\n"), auth.path()).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn saves_real_agent_limits_and_preserves_other_settings_and_comments() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let auth = directory.path().join("auth");
        std::fs::write(
            &path,
            "# retain this\nmax_turns = 0 # retain \
            inline\n[providers.demo]\nbase_url='http://localhost/v1'\nmodels=['model']\nauto_refre\
            sh_models=false\n[command]\nmax_scan_entries=1234\n[command.verification_commands]\nch\
            eck='cargo check'\n[permissions]\nread_templates=['.env.example']\n",
        )
        .unwrap();
        let draft = AdvancedDraft::load(&path, &auth).unwrap();
        let mut settings = draft.settings().clone();
        settings.max_turns = 25;
        settings.max_tool_calls = 500;
        settings.disabled_capabilities = vec![Capability::Mcp, Capability::Write];
        settings.disabled_tools = vec!["ReadFile".into(), "MCP_Server_Tool".into()];
        settings.subagents.max_concurrent = 3;
        settings.subagents.max_tool_calls = 80;
        settings.subagents.explore_model = Some("demo::model".into());
        settings.command.default_timeout_seconds = 30;
        let saved = draft
            .save(&path, &auth, settings)
            .unwrap();
        let config = FileConfig::load(&path, &auth).unwrap();
        assert_eq!(config.agent_config().max_turns, 25);
        assert_eq!(config.agent_config().max_tool_calls, 500);
        assert_eq!(config.subagents.max_concurrent, 3);
        assert_eq!(config.command.default_timeout_seconds, 30);
        assert_eq!(config.command.max_scan_entries, 1_234);
        assert_eq!(config.command.verification_commands["check"], "cargo check");
        assert_eq!(
            config.permissions.read_templates,
            vec![std::path::PathBuf::from(".env.example")]
        );
        assert_eq!(config.disabled_tools, ["ReadFile", "MCP_Server_Tool"]);
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(source.starts_with("# retain this"));
        assert!(source.contains("# retain inline"));
        let mut cleared = saved.settings().clone();
        cleared.subagents.explore_model = None;
        cleared.disabled_capabilities.clear();
        cleared.disabled_tools.clear();
        saved
            .save(&path, &auth, cleared)
            .unwrap();
        let config = FileConfig::load(&path, &auth).unwrap();
        assert!(config.disabled_capabilities.is_empty());
        assert!(config.disabled_tools.is_empty());
        assert!(config.subagents.explore_model.is_none());
        assert_eq!(config.providers.len(), 1);
    }

    #[test]
    fn invalid_or_stale_values_never_replace_the_saved_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let auth = directory.path().join("auth");
        let original = "[providers]\n";
        std::fs::write(&path, original).unwrap();
        let draft = AdvancedDraft::load(&path, &auth).unwrap();
        let mut values = draft.settings().clone();
        values.subagents.max_concurrent = 0;
        assert!(
            draft
                .save(&path, &auth, values)
                .is_err()
        );
        let mut values = draft.settings().clone();
        values.command.default_timeout_seconds = values.command.max_timeout_seconds + 1;
        assert!(
            draft
                .save(&path, &auth, values)
                .is_err()
        );
        let mut values = draft.settings().clone();
        values.disabled_tools = vec!["ReadFile".into(), "ReadFile".into()];
        assert!(
            draft
                .save(&path, &auth, values)
                .is_err()
        );
        let mut values = draft.settings().clone();
        values.subagents.review_model = Some("missing::model".into());
        assert!(
            draft
                .save(&path, &auth, values)
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        std::fs::write(&path, "# external edit\n[providers]\n").unwrap();
        assert!(
            draft
                .save(&path, &auth, draft.settings().clone())
                .is_err()
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("# external edit")
        );
    }
}
