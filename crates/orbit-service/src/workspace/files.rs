//! File operations share the file tools' authorization and atomic commit implementation.
use anyhow::{Result, ensure};
use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_protocol::{
    Capability,
    files::{Action, Entry, Snapshot},
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

pub(super) async fn execute(
    setup: &mut crate::PreparedWorkspace,
    action: Action,
    busy: bool,
) -> Result<Snapshot> {
    execute_with(
        &setup.resources.workspace,
        &setup.resources.undo_writer,
        &setup.resources.policy,
        action,
        busy,
    )
    .await
}

pub(super) async fn browse(
    browsing: &super::session::browsing::Browsing,
    options: &crate::WorkspaceOptions,
    action: Action,
) -> Result<Snapshot> {
    let _lease = if matches!(action, Action::Save { .. }) && browsing.session.is_persisted() {
        Some(
            browsing
                .session
                .store
                .acquire(browsing.session.id())
                .await?,
        )
    } else {
        None
    };
    let (workspace, policy) = browsing_access(browsing, options).await?;
    let (approvals, _receiver) = orbit_tools::approval::channel();
    let writer = orbit_tools::write_file::WriteFile::new(workspace.clone(), approvals);
    execute_with(&workspace, &writer, &policy, action, false).await
}

pub(super) async fn browsing_access(
    browsing: &super::session::browsing::Browsing,
    options: &crate::WorkspaceOptions,
) -> Result<(
    orbit_runtime::filesystem::Workspace,
    orbit_protocol::SessionPolicy,
)> {
    if browsing.session.is_persisted() {
        browsing
            .session
            .validate_directory(&browsing.directory)
            .await?;
    }
    let config = browsing.config(options)?;
    let planning = browsing
        .session
        .store
        .planning(browsing.session.id())
        .await?;
    let policy = orbit_protocol::SessionPolicy {
        disabled: config.disabled_capabilities,
        read_only: browsing
            .plan_mode
            .unwrap_or(planning.mode)
            == orbit_protocol::plan::WorkMode::Plan,
    };
    ensure!(!policy.disabled(Capability::Files), "文件功能已禁用");
    let approvals = orbit_runtime::policy::ApprovalPolicy::default();
    approvals.set_mode(super::setup::approval_mode(browsing.mode));
    let workspace = crate::runtime::configured_workspace(
        &browsing.directory,
        Some(config.permissions),
        config.command.network,
        approvals,
    )?;
    Ok((workspace, policy))
}

async fn execute_with(
    workspace: &orbit_runtime::filesystem::Workspace,
    writer: &orbit_tools::write_file::WriteFile,
    policy: &orbit_protocol::SessionPolicy,
    action: Action,
    busy: bool,
) -> Result<Snapshot> {
    ensure!(!policy.disabled(Capability::Files), "文件功能已禁用");
    match action {
        Action::List {
            path,
            offset,
            include_hidden,
        } => {
            // ListFiles owns descriptor-based traversal, filtering, and bounded pagination.
            let tool = orbit_tools::list_files::ListFiles::new(workspace.clone());
            let output = tool
                .execute(
                    ToolCall::function(
                        orbit_protocol::request_id(),
                        "ListFiles",
                        serde_json::json!({
                            "path": path,
                            "offset": offset,
                            "depth": 1,
                            "limit": 200,
                            "include_hidden": include_hidden,
                        }),
                    ),
                    CancellationToken::new(),
                )
                .await?;
            #[derive(Deserialize)]
            struct Listing {
                entries: Vec<Entry>,
                next_offset: Option<usize>,
                scan_truncated: bool,
            }
            let listing: Listing = serde_json::from_str(&output.content)?;
            Ok(Snapshot::Directory {
                path,
                entries: listing.entries,
                next_offset: listing.next_offset,
                scan_truncated: listing.scan_truncated,
                readonly: policy.read_only || policy.disabled(Capability::Write),
            })
        }
        Action::Save {
            path,
            original,
            content,
        } => {
            ensure!(!busy, "请等待当前任务结束后保存文件");
            ensure!(!policy.read_only, "当前会话为只读，无法保存文件");
            ensure!(!policy.disabled(Capability::Write), "文件写入已禁用");
            writer
                .save_user_edit(
                    path.clone(),
                    original,
                    content.clone(),
                    CancellationToken::new(),
                )
                .await?;
            Ok(Snapshot::Saved { path, content })
        }
    }
}
