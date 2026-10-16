//! Reconnect in the background; running agents keep their registry snapshot.
use super::*;
use orbit_agent::tool::ToolRegistry;
use orbit_tools::mcp::{McpConfig, McpServerStatus};
use std::path::Path;
use tokio::sync::oneshot;
use tokio_util::task::AbortOnDropHandle;

#[cfg(test)]
mod tests;

pub(in crate::workspace) type Catalog = (ToolRegistry, Vec<McpServerStatus>);

/// Owned by the prepared workspace, so replacing or closing it cancels startup.
pub(in crate::workspace) struct Startup {
    pub task: AbortOnDropHandle<Catalog>,
    pub statuses: Vec<McpServerStatus>,
}

impl Startup {
    pub fn start(
        config: &McpConfig,
        directory: &Path,
        approvals: Option<orbit_tools::approval::ApprovalSender>,
        allow_start: bool,
    ) -> Option<Self> {
        if !allow_start
            || approvals.is_none()
            || !config
                .servers
                .values()
                .any(|s| s.enabled)
        {
            return None;
        }
        let statuses = config
            .servers
            .iter()
            .map(|(name, server)| McpServerStatus {
                name: name.clone(),
                connected: false,
                detail: if server.enabled {
                    "正在连接"
                } else {
                    "已禁用"
                }
                .into(),
                tool_count: 0,
                tools: Vec::new(),
            })
            .collect();
        let config = config.clone();
        let directory = directory.to_owned();
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            let mut tools = ToolRegistry::new();
            let statuses =
                orbit_tools::mcp::register(&mut tools, &config, &directory, approvals, allow_start)
                    .await;
            (tools, statuses)
        }));
        Some(Self { task, statuses })
    }

    pub fn finish(self, setup: &mut PreparedWorkspace, output: Result<Catalog>) {
        match output {
            Ok(catalog) => apply(setup, catalog),
            Err(error) => {
                for status in &mut setup.resources.budget.mcp_statuses {
                    if status.detail == "正在连接" {
                        status.detail = format!("MCP 启动中断：{error:#}");
                    }
                }
            }
        }
    }
}

fn apply(setup: &mut PreparedWorkspace, (tools, statuses): Catalog) {
    setup
        .resources
        .tools
        .replace_prefix("mcp__", &tools);
    super::super::setup::filter_configured_tools(
        &mut setup.resources.tools,
        &setup.resources.policy,
        &setup.resources.disabled_tools,
    );
    setup.resources.budget.mcp_statuses = statuses;
}

pub(in crate::workspace) struct Reload {
    pub task: AbortOnDropHandle<Result<Catalog>>,
    session: String,
    workspace: String,
    result: oneshot::Sender<Result<Snapshot>>,
}

impl Reload {
    pub fn start(
        setup: &PreparedWorkspace,
        config: serde_json::Value,
        allow_start: bool,
        result: oneshot::Sender<Result<Snapshot>>,
    ) -> Result<Self> {
        ensure!(
            !setup
                .resources
                .policy
                .disabled(Capability::Mcp),
            "MCP 工具已禁用"
        );
        ensure!(
            !setup.resources.policy.read_only,
            "只读会话不能重新加载 MCP 服务器"
        );
        let config: McpConfig = serde_json::from_value(config)?;
        config.validate()?;
        let workspace = setup
            .session
            .snapshot
            .session
            .workspace
            .clone();
        let directory = std::path::PathBuf::from(&workspace);
        let approvals = setup.resources.mcp_approvals.clone();
        let allow_start = allow_start || setup.resources.options.allow_mcp_start;
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            let mut tools = ToolRegistry::new();
            let statuses =
                orbit_tools::mcp::register(&mut tools, &config, &directory, approvals, allow_start)
                    .await;
            Ok((tools, statuses))
        }));
        Ok(Self {
            task,
            session: setup.session.id().into(),
            workspace,
            result,
        })
    }

    pub fn finish(self, setup: &mut PreparedWorkspace, output: Result<Catalog>) {
        let response = output.and_then(|(tools, statuses)| {
            ensure!(
                self.session == setup.session.id()
                    && self.workspace == setup.session.snapshot.session.workspace,
                "工作区已切换，请重新应用 MCP 配置"
            );
            ensure!(
                !setup.resources.policy.read_only
                    && !setup
                        .resources
                        .policy
                        .disabled(Capability::Mcp),
                "当前会话不允许 MCP 工具"
            );
            apply(setup, (tools, statuses));
            Ok(snapshot(setup))
        });
        let _ = self.result.send(response);
    }
}

pub(super) fn snapshot(setup: &PreparedWorkspace) -> Snapshot {
    let visible = setup
        .resources
        .tools
        .definitions()
        .into_iter()
        .map(|definition| definition.function.name)
        .collect::<std::collections::HashSet<_>>();
    Snapshot::Mcp(
        setup
            .resources
            .budget
            .mcp_statuses
            .iter()
            .map(|server| McpServer {
                name: server.name.clone(),
                connected: server.connected,
                detail: server.detail.clone(),
                tools: server
                    .tools
                    .iter()
                    .filter(|tool| visible.contains(&tool.name))
                    .map(|tool| McpTool {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                    })
                    .collect(),
            })
            .collect(),
    )
}
