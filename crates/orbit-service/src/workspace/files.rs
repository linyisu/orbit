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
    ensure!(
        !setup
            .resources
            .policy
            .disabled(Capability::Files),
        "文件功能已禁用"
    );
    match action {
        Action::List {
            path,
            offset,
            include_hidden,
        } => {
            // ListFiles owns descriptor-based traversal, filtering, and bounded pagination.
            let tool = orbit_tools::list_files::ListFiles::new(setup.resources.workspace.clone());
            let output = tool
                .execute(
                    ToolCall::function(
                        orbit_protocol::request_id(),
                        "list_files",
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
                readonly: setup.resources.policy.read_only
                    || setup
                        .resources
                        .policy
                        .disabled(Capability::Write),
            })
        }
        Action::Save {
            path,
            original,
            content,
        } => {
            ensure!(!busy, "请等待当前任务结束后保存文件");
            ensure!(
                !setup.resources.policy.read_only,
                "当前会话为只读，无法保存文件"
            );
            ensure!(
                !setup
                    .resources
                    .policy
                    .disabled(Capability::Write),
                "文件写入已禁用"
            );
            setup
                .resources
                .undo_writer
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
