//! Edit only MCP configuration while preserving other settings and comments.
use anyhow::{Result, ensure};
use orbit_tools::mcp::{McpConfig, McpServerConfig};
use std::{collections::BTreeMap, path::Path};

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
        let mut document = self.source.parse::<toml_edit::DocumentMut>()?;
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
    fn save_preserves_other_sections_and_rejects_stale_draft() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# keep me\n[providers]\n").unwrap();
        let draft = McpDraft::load(&path).unwrap();
        let server: McpServerConfig = toml::from_str("command='test'\nenabled=false").unwrap();
        draft.save(&path, "demo", Some(server.clone())).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("# keep me")
        );
        assert!(draft.save(&path, "other", Some(server)).is_err());
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
