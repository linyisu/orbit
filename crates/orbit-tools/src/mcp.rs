//! Strict local stdio MCP client; external tools use the regular approval boundary.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutcome, ToolOutput, ToolRegistry};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::execution::process::ProcessGroup;
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
mod diagnostics;

const MAX_MESSAGE_BYTES: usize = 4 * 1_024 * 1_024;
const PROTOCOL_VERSION: &str = "2025-11-25";
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const CANCELLATION_WRITE_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Default, Deserialize, serde::Serialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct McpServerConfig {
    #[serde(rename = "type", default = "stdio_transport")]
    pub transport: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Absolute budget for writing and receiving each request, including tool execution.
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
                    "MCP server name {name:?} must use ASCII letters, digits, _ or - and be at \
                        most 48 characters"
                );
            }

            if server.transport != "stdio" {
                bail!(
                    "MCP server {name:?}: only local stdio is supported currently; remote \
                        transports require explicit credential and OAuth handling"
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
pub struct McpToolStatus {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerStatus {
    pub name: String,
    pub detail: String,
    pub tool_count: usize,
    pub connected: bool,
    pub tools: Vec<McpToolStatus>,
}

/// Register namespaced tools; report optional server failures without aborting the TUI.
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
                tools: Vec::new(),
            });
            continue;
        }

        let Some(approvals) = approvals.clone() else {
            statuses.push(McpServerStatus {
                name: name.clone(),
                detail: "需要审批通道；未启动".into(),
                tool_count: 0,
                connected: false,
                tools: Vec::new(),
            });
            continue;
        };

        if !allow_start {
            statuses.push(McpServerStatus {
                name: name.clone(),
                detail: "启动需显式授权：添加 --allow-mcp-start".into(),
                tool_count: 0,
                connected: false,
                tools: Vec::new(),
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
                let tools = filtered
                    .iter()
                    .map(|definition| McpToolStatus {
                        name: exposed_name(name, &definition.name),
                        description: definition.description.clone(),
                    })
                    .collect();
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
                    tools,
                });
            }

            Err(error) => statuses.push(McpServerStatus {
                name: name.clone(),
                detail: format!("{error:#}"),
                tool_count: 0,
                connected: false,
                tools: Vec::new(),
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
                "MCP server {} · {} External process: each invocation is subject to the selected \
                    approval mode; inspect arguments before allowing it. Transport recovery starts \
                    a fresh server session; previous session handles may be invalid. Never replay \
                    timed-out operations automatically.",
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
                result = self.approvals.send(request) => result
                    .map_err(|_| ToolError::ExecutionFailed("MCP approval unavailable".into()))?,
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

            let mut client = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                client = self.client.lock() => client,
            };
            let result = client
                .call_tool(&self.definition.name, arguments, cancellation.clone())
                .await
                .map_err(|error| {
                    if cancellation.is_cancelled() {
                        ToolError::Cancelled
                    } else {
                        ToolError::ExecutionFailed(format!("MCP {}: {error:#}", self.server))
                    }
                })?;

            Ok(result)
        })
    }
}

struct StdioClient {
    group: ProcessGroup,
    _child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    pending_line: Vec<u8>,
    poisoned: bool,
    next_id: u64,
    timeout: Duration,
    workspace: PathBuf,
    diagnostics: diagnostics::Stderr,
    name: String,
    config: McpServerConfig,
    tools: Vec<RemoteTool>,
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
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        command.envs(&config.env);
        let (mut child, group) = ProcessGroup::spawn(&mut command)
            .with_context(|| format!("failed to start MCP server {name:?}"))?;
        let stdin = child
            .stdin
            .take()
            .context("MCP server did not expose stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("MCP server did not expose stdout")?;
        let diagnostics = diagnostics::Stderr::capture(
            child
                .stderr
                .take()
                .context("MCP server did not expose stderr")?,
        );
        let mut client = Self {
            group,
            _child: child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            pending_line: Vec::new(),
            poisoned: false,
            next_id: 1,
            timeout: Duration::from_secs(config.timeout_seconds),
            workspace: workspace.to_path_buf(),
            diagnostics,
            name: name.to_owned(),
            config: config.clone(),
            tools: Vec::new(),
        };
        let tools = match client.initialize().await {
            Ok(tools) => tools,
            Err(error) => {
                client.shutdown().await;
                let exit = client._child.try_wait();
                let stderr = client.diagnostics.finish().await;
                return Err(error.context(format!(
                    "MCP server {name:?} failed{}{}",
                    match exit {
                        Ok(Some(status)) => format!(" ({status})"),
                        _ => String::new(),
                    },
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!("; stderr: {stderr}")
                    },
                )));
            }
        };
        client.tools = tools.clone();
        Ok((client, tools))
    }

    async fn initialize(&mut self) -> Result<Vec<RemoteTool>> {
        let initialized = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {
                        "roots": {
                            "listChanged": false
                        }
                    },
                    "clientInfo": {
                        "name": "orbit",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
                CancellationToken::new(),
            )
            .await
            .context("MCP initialize failed")?;

        if initialized
            .get("capabilities")
            .is_none()
        {
            bail!("MCP initialize response has no capabilities");
        }

        time::timeout(
            self.timeout,
            self.notify("notifications/initialized", json!({})),
        )
        .await
        .context("MCP initialized notification timed out")??;
        let listed = self
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

        Ok(tools)
    }

    /// Reconnect only for a new invocation; failed operations are never replayed here.
    async fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        if cancellation.is_cancelled() {
            bail!("cancelled");
        }
        let reconnected = self.poisoned;
        if reconnected {
            self.shutdown().await;
            let (replacement, tools) = tokio::select! {
                biased;
                _ = cancellation.cancelled() => bail!("cancelled"),
                result = Self::start(&self.name, &self.config, &self.workspace) => {
                    result.context("MCP reconnect failed; previous call was not replayed")?
                }
            };
            if !self.tools.iter().all(|old| {
                tools
                    .iter()
                    .any(|new| old.name == new.name && old.input_schema == new.input_schema)
            }) {
                bail!("MCP tool changed during reconnect; apply MCP configuration again");
            }
            *self = replacement;
        }
        let result = self
            .request(
                "tools/call",
                json!({
                    "name": name,
                    "arguments": arguments
                }),
                cancellation,
            )
            .await?;
        let content = result
            .get("content")
            .cloned()
            .unwrap_or_else(|| json!([]));

        let failed = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let content = json!({
            "server_result": content,
            "is_error": failed,
            "source_kind": "untrusted_mcp_tool_result",
            "connection_restarted": reconnected
        });
        Ok(
            ToolOutput::text(content.to_string()).with_outcome(if failed {
                ToolOutcome::Failed
            } else {
                ToolOutcome::Completed
            }),
        )
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
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

        if cancellation.is_cancelled() {
            bail!("cancelled");
        }
        let id = self.next_id;
        self.next_id += 1;
        let encoded = encode_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        self.poisoned = true;
        let timeout = self.timeout;
        let mut issued = false;
        let response = async {
            self.write_encoded(&encoded).await?;
            issued = true;
            loop {
                let message = self.read().await?;
                if message
                    .get("jsonrpc")
                    .and_then(Value::as_str)
                    != Some("2.0")
                {
                    bail!("server emitted an invalid JSON-RPC version");
                }

                if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                    if message.get("result").is_some() == message.get("error").is_some() {
                        bail!("response must have exactly one result or error");
                    }
                    return Ok(message);
                }

                if message
                    .get("method")
                    .and_then(Value::as_str)
                    == Some("roots/list")
                    && let Some(request_id) = message.get("id").cloned()
                {
                    self.write(json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": {
                            "roots": [{
                                "uri": file_uri(&self.workspace),
                                "name": self
                                    .workspace
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("workspace")
                            }]
                        }
                    }))
                    .await?;
                }
            }
        };
        let outcome = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow::anyhow!("cancelled")),
            result = time::timeout(timeout, response) => result
                .map_err(|_| anyhow::anyhow!(
                    "MCP {method} timed out after {} seconds (timeout_seconds); \
                     operation outcome is unknown; do not replay automatically",
                    timeout.as_secs()
                ))
                .and_then(|response| response),
        };

        match outcome {
            Ok(message) => {
                self.poisoned = false;
                if let Some(error) = message.get("error") {
                    bail!("server returned {error}");
                }
                Ok(message["result"].clone())
            }
            Err(error) => {
                if issued && method != "initialize" {
                    let _ = time::timeout(
                        CANCELLATION_WRITE_TIMEOUT,
                        self.notify(
                            "notifications/cancelled",
                            json!({
                                "requestId": id,
                                "reason": error.to_string()
                            }),
                        ),
                    )
                    .await;
                }
                self.shutdown().await;
                let stderr = self.diagnostics.finish().await;
                if stderr.is_empty() {
                    Err(error)
                } else {
                    Err(error.context(format!("MCP transport failed; stderr: {stderr}")))
                }
            }
        }
    }

    /// Close stdin before terminating the owned group, then reap the server with bounded waits.
    async fn shutdown(&mut self) {
        self.poisoned = true;
        self.stdin.take();
        if time::timeout(SHUTDOWN_GRACE, self._child.wait())
            .await
            .is_err()
        {
            self.group.request_termination();
            let _ = time::timeout(SHUTDOWN_GRACE, self._child.wait()).await;
        }
        self.group.kill();
        let _ = self._child.start_kill();
        let _ = time::timeout(SHUTDOWN_GRACE, self._child.wait()).await;
    }

    async fn write(&mut self, value: Value) -> Result<()> {
        self.write_encoded(&encode_message(value)?)
            .await
    }

    async fn write_encoded(&mut self, encoded: &[u8]) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .context("MCP stdin is closed")?;
        stdin.write_all(encoded).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        Ok(())
    }

    async fn read(&mut self) -> Result<Value> {
        read_protocol_line(&mut self.stdout, &mut self.pending_line).await
    }
}

/// Validate a complete outgoing frame before changing the transport state.
fn encode_message(value: Value) -> Result<Vec<u8>> {
    let encoded = serde_json::to_vec(&value)?;
    if encoded.len() > MAX_MESSAGE_BYTES {
        bail!("MCP request exceeds the 4 MiB protocol limit");
    }
    Ok(encoded)
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
        .unwrap_or_else(|| {
            json!({
                "type": "object"
            })
        });

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
mod tests;
