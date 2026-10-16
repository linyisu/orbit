//! Compact model-facing names; dispatch always retains the upstream identifier.
use super::RemoteTool;
use anyhow::{Result, ensure};
use orbit_agent::tool::ToolRegistry;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};

const MAX_NAME_BYTES: usize = 64;
const HASH_BYTES: usize = 8;

/// Resolves normalized collisions together, so catalog order cannot choose a winner.
pub(super) fn catalog(
    server: &str,
    tools: &[RemoteTool],
    registry: &ToolRegistry,
    servers: &[&str],
) -> Result<Vec<String>> {
    let server_component = component(server);
    let server_collision = servers
        .iter()
        .filter(|name| component(name) == server_component)
        .count()
        > 1;
    let bases: Vec<_> = tools
        .iter()
        .map(|tool| format!("MCP_{server_component}_{}", component(&tool.name)))
        .collect();
    let mut counts = BTreeMap::new();
    for base in &bases {
        *counts.entry(base).or_insert(0) += 1;
    }
    let mut used: HashSet<_> = registry
        .names()
        .map(str::to_owned)
        .collect();
    tools
        .iter()
        .zip(&bases)
        .map(|(tool, base)| {
            let mut name = base.clone();
            if name.len() > MAX_NAME_BYTES
                || counts[base] > 1
                || used.contains(base)
                || server_collision
            {
                let digest = Sha256::digest(format!("{server}\0{}", tool.name).as_bytes());
                name.truncate(MAX_NAME_BYTES - HASH_BYTES * 2 - 1);
                name.push('_');
                for byte in &digest[..HASH_BYTES] {
                    use std::fmt::Write;
                    write!(name, "{byte:02x}").expect("writing to a String cannot fail");
                }
            }
            ensure!(
                used.insert(name.clone()),
                "MCP 工具名称冲突：{server}/{}",
                tool.name
            );
            Ok(name)
        })
        .collect()
}

fn component(value: &str) -> String {
    let mut result = String::new();
    for word in value
        .split(['_', '-', '.'])
        .filter(|word| !word.is_empty())
    {
        let mut bytes = word.bytes();
        if let Some(first) = bytes.next() {
            result.push(first.to_ascii_uppercase() as char);
            result.extend(bytes.map(char::from));
        }
    }
    if result.is_empty() || result.as_bytes()[0].is_ascii_digit() {
        result.insert_str(0, "Mcp");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tools(names: &[&str]) -> Vec<RemoteTool> {
        names
            .iter()
            .map(|name| RemoteTool {
                name: (*name).into(),
                description: String::new(),
                input_schema: json!({}),
                output_schema: None,
            })
            .collect()
    }

    #[test]
    fn names_are_compact_bounded_and_distinct_after_normalization() {
        let registry = ToolRegistry::new();
        assert_eq!(
            catalog("files", &tools(&["read_file"]), &registry, &["files"]).unwrap(),
            ["MCP_Files_ReadFile"]
        );
        let definitions = tools(&["read_file", "read-file", "readFile", "READ_FILE"]);
        let names = catalog("files", &definitions, &registry, &["files"]).unwrap();
        assert_eq!(
            names
                .iter()
                .collect::<HashSet<_>>()
                .len(),
            definitions.len()
        );
        let reversed: Vec<_> = definitions.into_iter().rev().collect();
        assert_eq!(
            catalog("files", &reversed, &registry, &["files"]).unwrap(),
            names
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
        );
        for server in ["1files", "Files", "long_server_name_with_many_components"] {
            let names = catalog(
                server,
                &tools(&["a_very_long_tool_name_with_many_more_components"]),
                &registry,
                &[server],
            )
            .unwrap();
            let name = &names[0];
            assert!(name.len() <= MAX_NAME_BYTES);
            assert!(name.as_bytes()[0].is_ascii_uppercase());
            assert!(
                name.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            );
        }
        let definitions = tools(&["read_file"]);
        let servers = ["files", "Files"];
        let lower = catalog("files", &definitions, &registry, &servers).unwrap();
        let upper = catalog("Files", &definitions, &registry, &servers).unwrap();
        assert_ne!(lower, upper);
    }
}
