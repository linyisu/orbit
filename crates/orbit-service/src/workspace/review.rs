use std::{collections::HashSet, time::Duration};

use anyhow::{Context, Result, ensure};
use orbit_protocol::review::{Action, Checkpoint, Entry, Kind, Snapshot};

use crate::PreparedWorkspace;
use orbit_protocol::Capability;

pub(super) async fn execute(setup: &PreparedWorkspace, action: Action) -> Result<Snapshot> {
    execute_session(
        &setup.session,
        &setup.resources.workspace,
        &setup.resources.policy,
        action,
    )
    .await
}

pub(super) async fn browse(
    browsing: &super::session::browsing::Browsing,
    options: &crate::WorkspaceOptions,
    action: Action,
) -> Result<Snapshot> {
    let (workspace, policy) = super::files::browsing_access(browsing, options).await?;
    let _lease = if matches!(
        action,
        Action::Stage { .. } | Action::Unstage { .. } | Action::Undo { .. }
    ) && browsing.session.is_persisted()
    {
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
    execute_session(&browsing.session, &workspace, &policy, action).await
}

/// Read Git data without holding the session actor while subprocesses complete.
pub(super) async fn git_read(
    workspace: &orbit_runtime::filesystem::Workspace,
    policy: &orbit_protocol::SessionPolicy,
    action: Action,
) -> Result<Snapshot> {
    ensure!(!policy.disabled(Capability::Files), "文件功能已禁用");
    ensure!(!policy.disabled(Capability::Git), "Git 功能已禁用");
    match action {
        Action::Status {
            offset,
            include_patch,
        } => {
            let workspace = workspace.clone();
            let listing = tokio::task::spawn_blocking(move || {
                if include_patch {
                    orbit_tools::git_inspect::review_changes(&workspace, offset)
                } else {
                    orbit_tools::git_inspect::review_status(&workspace, offset)
                }
            })
            .await??;
            Ok(status_snapshot(listing)?)
        }
        action @ (Action::History { .. } | Action::Commit { .. } | Action::CommitFile { .. }) => {
            let view = match &action {
                Action::History { .. } => "history",
                Action::Commit { .. } => "commit",
                Action::CommitFile { .. } => "commit_file",
                _ => unreachable!(),
            };
            let workspace = workspace.clone();
            let value = tokio::task::spawn_blocking(move || match action {
                Action::History { revision, offset } => {
                    orbit_tools::git_inspect::history::list(&workspace, &revision, offset)
                }
                Action::Commit {
                    revision,
                    include_patch,
                } => {
                    orbit_tools::git_inspect::history::commit(&workspace, &revision, include_patch)
                }
                Action::CommitFile { revision, path } => {
                    orbit_tools::git_inspect::history::file(&workspace, &revision, &path)
                }
                _ => unreachable!(),
            })
            .await??;
            let mut value = value;
            value["view"] = view.into();
            Ok(serde_json::from_value(value)?)
        }
        _ => anyhow::bail!("expected a read-only Git browsing action"),
    }
}

async fn execute_session(
    session: &crate::session::SessionState,
    workspace: &orbit_runtime::filesystem::Workspace,
    policy: &orbit_protocol::SessionPolicy,
    action: Action,
) -> Result<Snapshot> {
    ensure!(!policy.disabled(Capability::Files), "文件功能已禁用");
    if matches!(
        action,
        Action::List { kind, .. } | Action::Diff { kind, .. } if kind != Kind::Checkpoint
    ) || matches!(
        action,
        Action::Stage { .. }
            | Action::Unstage { .. }
            | Action::History { .. }
            | Action::Commit { .. }
            | Action::CommitFile { .. }
            | Action::Status { .. }
    ) {
        ensure!(!policy.disabled(Capability::Git), "Git 功能已禁用");
    }
    match action {
        action @ (Action::Status { .. }
        | Action::History { .. }
        | Action::Commit { .. }
        | Action::CommitFile { .. }) => git_read(workspace, policy, action).await,
        Action::List {
            kind: Kind::Checkpoint,
            offset,
        } => {
            ensure!(offset <= 20_000, "检查点页码超出限制");
            let mut paths = HashSet::new();
            let entries: Vec<_> = session
                .store
                .workspace_changes(session.id())
                .await?
                .into_iter()
                .rev()
                .filter(|record| paths.insert(record.change.path.clone()))
                .map(|record| Entry {
                    path: record.change.path,
                    kind: Kind::Checkpoint,
                    status: record.run_status,
                    undone: record.undone,
                })
                .collect();
            let end = offset
                .saturating_add(500)
                .min(entries.len());
            Ok(Snapshot::List {
                entries: entries
                    .get(offset..end)
                    .unwrap_or_default()
                    .to_vec(),
                next: (end < entries.len()).then_some(end),
                branch: None,
            })
        }
        Action::List { kind, offset } => {
            let workspace = workspace.clone();
            let listing = tokio::task::spawn_blocking(move || {
                orbit_tools::git_inspect::review_status(&workspace, offset)
            })
            .await??;
            let entries = listing["entries"]
                .as_array()
                .context("Git status is missing entries")?
                .iter()
                .filter_map(|entry| {
                    let xy = entry["xy"]
                        .as_str()
                        .unwrap_or("..")
                        .as_bytes();
                    let include = match kind {
                        Kind::Untracked => entry["kind"] == "untracked",
                        Kind::Staged => xy
                            .first()
                            .is_some_and(|value| !matches!(value, b'.' | b' ')),
                        Kind::Unstaged => xy
                            .get(1)
                            .is_some_and(|value| !matches!(value, b'.' | b' ')),
                        Kind::Checkpoint => false,
                    };
                    include.then(|| Entry {
                        path: entry["path"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                        kind,
                        status: entry["xy"]
                            .as_str()
                            .unwrap_or("新增")
                            .to_owned(),
                        undone: false,
                    })
                })
                .collect();
            Ok(Snapshot::List {
                entries,
                next: listing["next_offset"]
                    .as_u64()
                    .map(|value| value as usize),
                branch: listing["branch"]["head"]
                    .as_str()
                    .map(str::to_owned),
            })
        }
        Action::Diff {
            kind: Kind::Checkpoint,
            path,
        } => {
            let changes = session
                .store
                .workspace_changes(session.id())
                .await?;
            let record = changes
                .iter()
                .rev()
                .find(|record| record.change.path == path)
                .context("该文件的写入检查点已不存在，请刷新")?;
            let before = record
                .change
                .before
                .as_deref()
                .unwrap_or_default();
            ensure!(
                before
                    .len()
                    .saturating_add(record.change.after.len())
                    <= 8 * 1_024 * 1_024,
                "检查点过大，无法在桌面差异视图中完整显示"
            );
            let patch = similar::TextDiff::configure()
                .timeout(Duration::from_millis(250))
                .diff_lines(before, &record.change.after)
                .unified_diff()
                .header(&format!("a/{path}"), &format!("b/{path}"))
                .to_string();
            Ok(Snapshot::Diff {
                path,
                patch,
                note: format!(
                    "Orbit 写入检查点 · {} · run {} · 当前文件可能已继续修改",
                    record.tool, record.run_id
                ),
                checkpoint: Some(Checkpoint {
                    run_id: record.run_id,
                    call_id: record.call_id.clone(),
                    undone: record.undone,
                }),
                index_receipt: None,
            })
        }
        Action::Diff { kind, path } => {
            let workspace = workspace.clone();
            let target = path.clone();
            let reviewed = tokio::task::spawn_blocking(move || {
                orbit_tools::git_inspect::review_index_patch(
                    &workspace,
                    &target,
                    kind == Kind::Staged,
                    kind == Kind::Untracked,
                )
            })
            .await??;
            Ok(Snapshot::Diff {
                path,
                patch: reviewed.patch,
                checkpoint: None,
                index_receipt: reviewed.index_receipt,
                note: reviewed.index_note.unwrap_or_else(|| {
                    match kind {
                        Kind::Staged => "Git 暂存区相对于 HEAD",
                        Kind::Unstaged => "Git 工作区相对于暂存区",
                        _ => "Git 未跟踪文件",
                    }
                    .into()
                }),
            })
        }
        action @ (Action::Stage { .. } | Action::Unstage { .. }) => {
            ensure!(!policy.read_only, "只读会话不能修改暂存区");
            ensure!(!policy.disabled(Capability::Write), "写入功能已禁用");
            let staged = matches!(action, Action::Stage { .. });
            let (path, receipt) = match action {
                Action::Stage { path, receipt } | Action::Unstage { path, receipt } => {
                    (path, receipt)
                }
                _ => unreachable!(),
            };
            let workspace = workspace.clone();
            let target = path.clone();
            tokio::task::spawn_blocking(move || {
                orbit_tools::git_inspect::update_review_index(
                    &workspace,
                    &target,
                    &receipt,
                    staged,
                    tokio_util::sync::CancellationToken::new(),
                )
            })
            .await??;
            Ok(Snapshot::IndexUpdated { path, staged })
        }
        Action::Undo { .. } => anyhow::bail!("撤销必须由会话执行器处理"),
    }
}

fn status_snapshot(listing: serde_json::Value) -> Result<Snapshot> {
    let entries = listing["entries"]
        .as_array()
        .context("Git status is missing entries")?
        .iter()
        .map(|entry| {
            let status = entry["xy"]
                .as_str()
                .unwrap_or("??")
                .to_owned();
            let kind = if entry["kind"] == "untracked" {
                Kind::Untracked
            } else if status
                .as_bytes()
                .get(1)
                .is_some_and(|byte| !matches!(byte, b'.' | b' '))
            {
                Kind::Unstaged
            } else {
                Kind::Staged
            };
            Ok(Entry {
                path: entry["path"]
                    .as_str()
                    .context("Git entry is missing a path")?
                    .into(),
                kind,
                status,
                undone: false,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Snapshot::Status {
        patch: listing["patch"]
            .as_str()
            .map(str::to_owned),
        entries,
        next: listing["next_offset"]
            .as_u64()
            .map(|offset| offset as usize),
        branch: listing["branch"]["head"]
            .as_str()
            .map(str::to_owned),
    })
}
