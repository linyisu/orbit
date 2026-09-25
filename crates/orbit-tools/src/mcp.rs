//! A small, strict MCP stdio client for registering external tools in Orbit.
//!
//! The first transport is deliberately local stdio. It is the interoperable
//! configuration format used by Claude Code and Claude Desktop, and unlike a
//! remote connector it neither needs an OAuth token store nor silently sends
//! user credentials to a third party.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput, ToolRegistry};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, oneshot},
    time,
};
use tokio_util::sync::CancellationToken;

use crate::approval::{ApprovalPreview, ApprovalRequest, ApprovalSender, McpPreview};

const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Clone, Debug, Default, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpServerConfig {
    #[serde(rename = "type", default = "stdio_transport")]
    pub transport: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    /// Empty means all server tools. A nonempty list names the only tools Orbit
    /// may expose and call for this server.
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn stdio_transport() -> String {
    "stdio".into()
}

fn default_timeout_seconds() -> u64 {
    120
}

fn default_enabled() -> bool {
    false
}

impl McpConfig {
    pub fn validate(&self) -> Result<()> {
        for (name, server) in &self.servers {
            if !valid_component(name) {
                bail!(
                    "MCP server name {name:?} must use ASCII letters, digits, _ or - and be at most 48 characters"
                );
            }
            if server.transport != "stdio" {
                bail!(
                    "MCP server {name:?}: only local stdio is supported currently; remote transports require explicit credential and OAuth handling"
                );
            }
            if server.command.trim().is_empty() || server.timeout_seconds == 0 {
                bail!("MCP server {name:?} must set a command and a positive timeout_seconds");
            }
            let mut names = std::collections::BTreeSet::new();
            for key in server.env.keys() {
                if matches!(
                    key.as_str(),
                    "PATH" | "ORBIT_PROJECT_DIR" | "LD_PRELOAD" | "LD_LIBRARY_PATH"
                ) || key.starts_with("DYLD_")
                {
                    bail!(
                        "MCP server {name:?} cannot override protected environment variable {key:?}"
                    );
                }
            }
            for tool in &server.tools {
                if tool.trim().is_empty() || !names.insert(tool) {
                    bail!("MCP server {name:?} has an empty or duplicate tool allowlist entry");
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerStatus {
    pub name: String,
    pub detail: String,
    pub tool_count: usize,
    pub connected: bool,
}

/// Start configured stdio servers, discover their tools, and add namespaced
/// adapters to the regular Orbit tool registry. A broken optional integration
/// is recorded as a status instead of making the TUI unusable.
pub async fn register(
    registry: &mut ToolRegistry,
    config: &McpConfig,
    workspace: &Path,
    approvals: Option<ApprovalSender>,
    allow_start: bool,
) -> Vec<McpServerStatus> {
    let mut statuses = Vec::new();
    for (name, config) in &config.servers {
        if !config.enabled {
            statuses.push(McpServerStatus {
                name: name.clone(),
                detail: "已禁用".into(),
                tool_count: 0,
                connected: false,
            });
            continue;
        }
        let Some(approvals) = approvals.clone() else {
            statuses.push(McpServerStatus {
                name: name.clone(),
                detail: "需要审批通道；未启动".into(),
                tool_count: 0,
                connected: false,
            });
            continue;
        };
        if !allow_start {
            statuses.push(McpServerStatus {
                name: name.clone(),
                detail: "启动需显式授权：添加 --allow-mcp-start".into(),
                tool_count: 0,
                connected: false,
            });
            continue;
        }
        match StdioClient::start(name, config, workspace).await {
            Ok((client, definitions)) => {
                let filtered = definitions
                    .into_iter()
                    .filter(|definition| {
                        config.tools.is_empty()
                            || config
                                .tools
                                .iter()
                                .any(|allowed| allowed == &definition.name)
                    })
                    .collect::<Vec<_>>();
                let count = filtered.len();
                let client = Arc::new(Mutex::new(client));
                for definition in filtered {
                    registry.register(McpTool {
                        exposed_name: exposed_name(name, &definition.name),
                        server: name.clone(),
                        definition,
                        client: client.clone(),
                        approvals: approvals.clone(),
                    });
                }
                statuses.push(McpServerStatus {
                    name: name.clone(),
                    detail: if config.tools.is_empty() {
                        "已连接".into()
                    } else {
                        "已连接，已按白名单筛选".into()
                    },
                    tool_count: count,
                    connected: true,
                });
            }
            Err(error) => statuses.push(McpServerStatus {
                name: name.clone(),
                detail: error.to_string(),
                tool_count: 0,
                connected: false,
            }),
        }
    }
    statuses
}

#[derive(Clone, Debug)]
struct RemoteTool {
    name: String,
    description: String,
    input_schema: Value,
}

struct McpTool {
    exposed_name: String,
    server: String,
    definition: RemoteTool,
    client: Arc<Mutex<StdioClient>>,
    approvals: ApprovalSender,
}

impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.exposed_name.clone(),
            format!(
                "MCP server {} · {} External process: each invocation is subject to the selected approval mode; inspect arguments before allowing it.",
                self.server,
                self.definition.description.trim()
            ),
            self.definition.input_schema.clone(),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let arguments = call
                .arguments_json()
                .map_err(|error| ToolError::InvalidArguments {
                    tool: self.exposed_name.clone(),
                    message: error.to_string(),
                })?;
            let (sender, receiver) = oneshot::channel();
            let request = ApprovalRequest {
                preview: ApprovalPreview::Mcp(McpPreview {
                    server: self.server.clone(),
                    tool: self.definition.name.clone(),
                    arguments: arguments.to_string(),
                }),
                decision: sender,
            };
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                result = self.approvals.send(request) => result.map_err(|_| ToolError::ExecutionFailed("MCP approval unavailable".into()))?,
            }
            let approved = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                answer = receiver => answer.unwrap_or(false),
            };
            if !approved {
                return Err(ToolError::ExecutionFailed(
                    "MCP tool call was not approved; do not retry automatically".into(),
                ));
            }
            let mut client = self.client.lock().await;
            let result = client
                .call_tool(&self.definition.name, arguments, cancellation)
                .await
                .map_err(|error| {
                    ToolError::ExecutionFailed(format!("MCP {}: {error}", self.server))
                })?;
            Ok(ToolOutput::text(result.to_string()))
        })
    }
}

struct StdioClient {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pending_line: Vec<u8>,
    poisoned: bool,
    next_id: u64,
    timeout: Duration,
    workspace: PathBuf,
}

impl StdioClient {
    async fn start(
        name: &str,
        config: &McpServerConfig,
        workspace: &Path,
    ) -> Result<(Self, Vec<RemoteTool>)> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .current_dir(workspace)
            // MCP servers are arbitrary local programs. Do not pass through
            // host credentials, proxy settings, Git configuration, or HOME;
            // a user may explicitly grant a value in the server's `env` map.
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("ORBIT_PROJECT_DIR", workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        command.envs(&config.env);
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start MCP server {name:?}"))?;
        let stdin = child
            .stdin
            .take()
            .context("MCP server did not expose stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("MCP server did not expose stdout")?;
        let mut client = Self {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
            pending_line: Vec::new(),
            poisoned: false,
            next_id: 1,
            timeout: Duration::from_secs(config.timeout_seconds),
            workspace: workspace.to_path_buf(),
        };
        let initialized = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "roots": { "listChanged": false } },
                    "clientInfo": { "name": "orbit", "version": env!("CARGO_PKG_VERSION") }
                }),
                CancellationToken::new(),
            )
            .await
            .context("MCP initialize failed")?;
        if initialized.get("capabilities").is_none() {
            bail!("MCP initialize response has no capabilities");
        }
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        let listed = client
            .request("tools/list", json!({}), CancellationToken::new())
            .await
            .context("MCP tools/list failed")?;
        let tools = listed
            .get("tools")
            .and_then(Value::as_array)
            .context("MCP tools/list response has no tools array")?
            .iter()
            .filter_map(parse_tool)
            .collect::<Vec<_>>();
        Ok((client, tools))
    }

    async fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                cancellation,
            )
            .await?;
        let content = result.get("content").cloned().unwrap_or_else(|| json!([]));
        Ok(json!({
            "server_result": content,
            "is_error": result.get("isError").and_then(Value::as_bool).unwrap_or(false),
            "source_kind": "untrusted_mcp_tool_result"
        }))
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        if self.poisoned {
            bail!("MCP connection is unusable after a previous protocol or timeout error");
        }
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        let timeout = self.timeout;
        let response = async {
            loop {
                let message = self.read().await?;
                if message.get("id") == Some(&json!(id)) {
                    if let Some(error) = message.get("error") {
                        bail!("server returned {error}");
                    }
                    return message
                        .get("result")
                        .cloned()
                        .context("response has no result");
                }
                if message.get("method").and_then(Value::as_str) == Some("roots/list")
                    && let Some(request_id) = message.get("id").cloned()
                {
                    self.write(json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": { "roots": [{ "uri": file_uri(&self.workspace), "name": self.workspace.file_name().and_then(|name| name.to_str()).unwrap_or("workspace") }] }
                    })).await?;
                }
            }
        };
        let outcome = tokio::select! {
            _ = cancellation.cancelled() => Err(anyhow::anyhow!("cancelled")),
            result = time::timeout(timeout, response) => {
                result.map_err(|_| anyhow::anyhow!("MCP request timed out"))
                    .and_then(|response| response)
            },
        };
        if outcome.is_err() {
            self.poisoned = true;
            let _ = self._child.start_kill();
        }
        outcome
    }

    async fn write(&mut self, value: Value) -> Result<()> {
        let encoded = serde_json::to_vec(&value)?;
        if encoded.len() > MAX_MESSAGE_BYTES {
            bail!("MCP request exceeds the 4 MiB protocol limit");
        }
        self.stdin.write_all(&encoded).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read(&mut self) -> Result<Value> {
        read_protocol_line(&mut self.stdout, &mut self.pending_line).await
    }
}

async fn read_protocol_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    pending: &mut Vec<u8>,
) -> Result<Value> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            bail!("server closed its stdout");
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if pending.len().saturating_add(end) > MAX_MESSAGE_BYTES {
            bail!("MCP response exceeds the 4 MiB protocol limit");
        }

        let complete = available[end - 1] == b'\n';
        pending.extend_from_slice(&available[..end]);
        reader.consume(end);
        if complete {
            let line = std::mem::take(pending);
            return serde_json::from_slice(&line)
                .context("server emitted invalid JSON-RPC on stdout");
        }
    }
}

fn parse_tool(value: &Value) -> Option<RemoteTool> {
    let name = value.get("name")?.as_str()?.trim();
    if !valid_component(name) {
        return None;
    }
    let schema = value
        .get("inputSchema")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object" }));
    (schema.is_object()).then(|| RemoteTool {
        name: name.to_owned(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("No description provided by the MCP server.")
            .to_owned(),
        input_schema: schema,
    })
}

fn exposed_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn file_uri(path: &Path) -> String {
    url::Url::from_directory_path(path)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| "file:///".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_an_unterminated_response_at_the_protocol_limit() {
        let bytes = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        let mut reader = BufReader::new(std::io::Cursor::new(bytes));
        let mut pending = Vec::new();

        let error = read_protocol_line(&mut reader, &mut pending)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("4 MiB"));
        assert!(pending.len() <= MAX_MESSAGE_BYTES);
    }

    #[tokio::test]
    async fn interrupted_read_keeps_the_partial_json_rpc_line() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let mut reader = BufReader::new(reader);
        let mut pending = Vec::new();
        writer.write_all(b"{\"id\":").await.unwrap();

        let timed_out = time::timeout(
            Duration::from_millis(10),
            read_protocol_line(&mut reader, &mut pending),
        )
        .await;
        assert!(timed_out.is_err());
        assert_eq!(pending, b"{\"id\":");

        writer.write_all(b"1}\n").await.unwrap();
        let value = read_protocol_line(&mut reader, &mut pending).await.unwrap();

        assert_eq!(value["id"], 1);
    }

    #[tokio::test]
    async fn timed_out_request_invalidates_its_stdio_connection() {
        let workspace = tempfile::tempdir().unwrap();
        let config = McpServerConfig {
            transport: "stdio".into(),
            command: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                r#"while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}' ;;
    *'"method":"tools/list"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}' ;;
    *'"method":"tools/call"'*) sleep 3 ;;
  esac
done"#
                    .into(),
            ],
            env: BTreeMap::new(),
            timeout_seconds: 1,
            tools: Vec::new(),
            enabled: true,
        };
        let (mut client, _) = StdioClient::start("fixture", &config, workspace.path())
            .await
            .unwrap();

        let first = client
            .call_tool("slow", json!({}), CancellationToken::new())
            .await
            .unwrap_err();
        let second = client
            .call_tool("slow", json!({}), CancellationToken::new())
            .await
            .unwrap_err();

        assert!(first.to_string().contains("timed out"), "{first}");
        assert!(second.to_string().contains("unusable"), "{second}");
    }
    use crate::approval::ApprovalPreview;
    use orbit_model::types::ToolCall;

    #[test]
    fn validates_stdio_configuration_and_namespaces_tools() {
        let config: McpConfig = toml::from_str(
            r#"
            [servers.files]
            command = "npx"
            args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
            tools = ["read_file"]
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert!(!config.servers["files"].enabled);
        assert_eq!(exposed_name("files", "read_file"), "mcp__files__read_file");
    }

    #[test]
    fn rejects_remote_transport_until_credential_boundary_exists() {
        let config: McpConfig = toml::from_str(
            r#"
            [servers.remote]
            type = "http"
            command = "unused"
            "#,
        )
        .unwrap();
        assert!(config.validate().unwrap_err().to_string().contains("stdio"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discovers_and_calls_a_stdio_server() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("server.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"tools":{}}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object"}}]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"ok"}]}}'
      ;;
  esac
done
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions).unwrap();
        let config = McpConfig {
            servers: BTreeMap::from([(
                "fixture".into(),
                McpServerConfig {
                    transport: "stdio".into(),
                    command: script.to_string_lossy().into_owned(),
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    timeout_seconds: 2,
                    tools: Vec::new(),
                    enabled: true,
                },
            )]),
        };
        let mut registry = ToolRegistry::new();
        let (approvals, mut receiver) = crate::approval::channel();
        let approval = tokio::spawn(async move {
            let request = receiver.recv().await.expect("MCP call requests approval");
            let ApprovalPreview::Mcp(preview) = request.preview else {
                panic!("expected MCP approval");
            };
            assert_eq!(preview.server, "fixture");
            assert_eq!(preview.tool, "echo");
            request.decision.send(true).unwrap();
        });
        let statuses = register(
            &mut registry,
            &config,
            directory.path(),
            Some(approvals),
            true,
        )
        .await;
        assert_eq!(statuses[0].tool_count, 1, "{statuses:?}");
        let output = registry
            .execute(
                ToolCall::function("call", "mcp__fixture__echo", json!({ "message": "hello" })),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        approval.await.unwrap();
        assert!(output.content.contains("\"text\":\"ok\""));
    }

    #[tokio::test]
    async fn does_not_start_enabled_servers_without_an_approval_channel() {
        let config: McpConfig = toml::from_str(
            r#"
            [servers.files]
            command = "definitely-not-a-real-mcp-server"
            enabled = true
            "#,
        )
        .unwrap();
        let mut registry = ToolRegistry::new();
        let statuses = register(&mut registry, &config, Path::new("/tmp"), None, false).await;
        assert_eq!(statuses[0].detail, "需要审批通道；未启动");
        assert!(registry.definitions().is_empty());
    }

    #[tokio::test]
    async fn configured_server_requires_startup_authorization() {
        let config: McpConfig = toml::from_str(
            r#"
            [servers.files]
            command = "definitely-not-a-real-mcp-server"
            enabled = true
            "#,
        )
        .unwrap();
        let mut registry = ToolRegistry::new();
        let (approvals, _receiver) = crate::approval::channel();

        let statuses = register(
            &mut registry,
            &config,
            Path::new("/tmp"),
            Some(approvals),
            false,
        )
        .await;

        assert!(statuses[0].detail.contains("--allow-mcp-start"));
        assert!(registry.definitions().is_empty());
    }

    #[test]
    fn rejects_mcp_environment_overrides_of_runtime_isolation() {
        let config: McpConfig = toml::from_str(
            r#"
            [servers.files]
            command = "npx"
            [servers.files.env]
            PATH = "/tmp/untrusted"
            "#,
        )
        .unwrap();

        assert!(config.validate().unwrap_err().to_string().contains("PATH"));
    }
}
