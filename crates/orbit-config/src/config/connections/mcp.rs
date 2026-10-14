//! Edit only MCP configuration while preserving other settings and comments.
pub mod environment;
use anyhow::{Result, ensure};
pub use orbit_tools::mcp::{McpConfig, McpServerConfig};
use std::{collections::BTreeMap, path::Path};

/// Editable stdio fields; argument lines are literal and environment values use dotenv quoting.
pub struct ServerInput<'a> {
    pub command: &'a str,
    pub args: &'a str,
    pub env: &'a str,
    pub timeout_seconds: &'a str,
    pub tools: &'a str,
    pub enabled: bool,
}

impl ServerInput<'_> {
    /// Parse the server fields without launching a process or changing configuration.
    pub fn parse(&self) -> Result<McpServerConfig> {
        let timeout_seconds = self
            .timeout_seconds
            .trim()
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("超时应为正整数（秒）。"))?;
        ensure!(timeout_seconds > 0, "超时应为正整数（秒）。");
        Ok(McpServerConfig {
            transport: "stdio".into(),
            command: self.command.trim().into(),
            args: self
                .args
                .lines()
                .map(str::to_owned)
                .collect(),
            env: environment::parse(self.env)?,
            timeout_seconds,
            tools: self
                .tools
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
            enabled: self.enabled,
        })
    }
}

pub struct McpDraft {
    source: String,
    servers: BTreeMap<String, McpServerConfig>,
}

impl McpDraft {
    pub fn load(path: &Path) -> Result<Self> {
        let source = super::read_config(path)?;
        let config: crate::config::FileConfig = toml::from_str(&source)?;
        Ok(Self {
            source,
            servers: config.mcp.servers,
        })
    }

    pub fn servers(&self) -> &BTreeMap<String, McpServerConfig> {
        &self.servers
    }

    /// Add a new server without replacing an existing declaration with the same name.
    pub fn create(&self, path: &Path, name: &str, server: McpServerConfig) -> Result<Self> {
        ensure!(
            !self.servers.contains_key(name),
            "服务器名称已存在，请使用其他名称。"
        );
        self.save(path, name, Some(server))
    }

    pub fn save(&self, path: &Path, name: &str, server: Option<McpServerConfig>) -> Result<Self> {
        let mut servers = self.servers.clone();
        if let Some(server) = server {
            servers.insert(name.into(), server);
        } else {
            servers.remove(name);
        }
        McpConfig {
            servers: servers.clone(),
        }
        .validate()?;
        ensure!(
            super::read_config(path)? == self.source,
            "配置已变更，请刷新后重试"
        );
        let mut document = self
            .source
            .parse::<toml_edit::DocumentMut>()?;
        if let Some(server) = servers.get(name) {
            if document.get("mcp").is_none() {
                document["mcp"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            if document["mcp"].get("servers").is_none() {
                document["mcp"]["servers"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let serialized = toml::to_string(server)?.parse::<toml_edit::DocumentMut>()?;
            document["mcp"]["servers"][name] =
                toml_edit::Item::Table(serialized.as_table().clone());
        } else if let Some(table) = document
            .get_mut("mcp")
            .and_then(|v| v.get_mut("servers"))
            .and_then(|v| v.as_table_like_mut())
        {
            table.remove(name);
        }
        let source = document.to_string();
        super::persist(path, &self.source, &source)?;
        Ok(Self { source, servers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_editor_saves_the_actual_path_and_reopens_without_adding_quotes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, "# preserved\n[providers]\n").unwrap();
        let mut server: McpServerConfig = toml::from_str("command='test'\nenabled=false").unwrap();
        server.env = environment::parse("DEMO_TOOL_HOME= '/opt/sample-tool'").unwrap();
        McpDraft::load(&path)
            .unwrap()
            .save(&path, "fixture-tools", Some(server))
            .unwrap();
        let draft = McpDraft::load(&path).unwrap();
        let env = &draft.servers()["fixture-tools"].env;
        assert_eq!(env["DEMO_TOOL_HOME"], "/opt/sample-tool");
        assert_eq!(environment::format(env), "DEMO_TOOL_HOME=/opt/sample-tool");
        assert_eq!(environment::parse(&environment::format(env)).unwrap(), *env);
        #[cfg(unix)]
        {
            let output = std::process::Command::new("/bin/sh")
                .args(["-c", "printf '%s' \"$DEMO_TOOL_HOME\""])
                .envs(env)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"/opt/sample-tool");
        }
    }

    #[test]
    fn save_preserves_other_sections_and_rejects_stale_draft() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# keep me\n[providers]\n").unwrap();
        let draft = McpDraft::load(&path).unwrap();
        let server: McpServerConfig = toml::from_str("command='test'\nenabled=false").unwrap();
        draft
            .save(&path, "demo", Some(server.clone()))
            .unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("# keep me")
        );
        assert!(
            draft
                .save(&path, "other", Some(server))
                .is_err()
        );
        let draft = McpDraft::load(&path).unwrap();
        assert_eq!(draft.servers().len(), 1);
        assert!(
            draft
                .save(&path, "demo", None)
                .unwrap()
                .servers()
                .is_empty()
        );
    }
}
