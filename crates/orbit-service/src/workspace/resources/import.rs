//! Imports own their background work; the session actor must keep serving chat.
use anyhow::{Context, Result, ensure};
use orbit_protocol::{
    Capability,
    resources::{Action, Snapshot},
};
use tokio::sync::oneshot;
use tokio_util::task::AbortOnDropHandle;

use crate::{PreparedWorkspace, workspace::mcp_import::Scan};

pub(in crate::workspace) struct Import {
    pub task: AbortOnDropHandle<Result<(Snapshot, Option<Scan>)>>,
    workspace: String,
    result: oneshot::Sender<Result<Snapshot>>,
}

impl Import {
    pub fn start(
        setup: &mut PreparedWorkspace,
        action: Action,
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
            !setup.resources.policy.read_only
                && !setup
                    .resources
                    .policy
                    .disabled(Capability::Write),
            "只读会话或禁用写入时不能导入 MCP 服务器"
        );
        let workspace = setup
            .session
            .snapshot
            .session
            .workspace
            .clone();
        let directory = std::path::PathBuf::from(&workspace);
        let session = setup.session.id().to_owned();
        let home = setup.resources.options.home.clone();
        let scan = setup.resources.mcp_import.take();
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            match action {
                Action::ScanMcp { source } => {
                    drop(scan);
                    let scan = Scan::acquire(session, directory, source, home).await?;
                    Ok((scan.snapshot(), Some(scan)))
                }
                Action::InstallMcp { token, id } => {
                    let snapshot = scan
                        .context("MCP 来源已过期，请重新扫描")?
                        .install(&token, &id, &session, &directory)
                        .await?;
                    Ok((snapshot, None))
                }
                _ => unreachable!("only MCP imports run in this task"),
            }
        }));
        Ok(Self {
            task,
            workspace,
            result,
        })
    }

    pub fn finish(self, setup: &mut PreparedWorkspace, output: Result<(Snapshot, Option<Scan>)>) {
        let response = output.and_then(|(snapshot, scan)| {
            ensure!(
                self.workspace == setup.session.snapshot.session.workspace,
                "工作区已切换，请重新打开 MCP 导入"
            );
            setup.resources.mcp_import = scan;
            Ok(snapshot)
        });
        let _ = self.result.send(response);
    }
}
