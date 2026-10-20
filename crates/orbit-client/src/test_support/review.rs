use orbit_protocol::review::{Action, Checkpoint, Entry, Kind, Snapshot};
use serde_json::{Value, json};

pub fn execute(workspace: &str, request: &Value) -> Value {
    let result = (|| -> anyhow::Result<Snapshot> {
        anyhow::ensure!(
            request["expected_workspace"] == workspace,
            "工作区已切换，请重新打开差异审查"
        );
        let root = std::path::Path::new(workspace);
        let filesystem = orbit_runtime::filesystem::Workspace::new(root)?;
        let action: Action = serde_json::from_value(request["action"].clone())?;
        let undone = root.join(".fixture-review-undone");
        match action {
            Action::Status { offset } => {
                let listing = orbit_tools::git_inspect::review_status(&filesystem, offset)?;
                let entries = listing["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|entry| Entry {
                        path: entry["path"]
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                        kind: if entry["kind"] == "untracked" {
                            Kind::Untracked
                        } else if entry["xy"]
                            .as_str()
                            .and_then(|xy| xy.as_bytes().get(1))
                            .is_some_and(|value| !matches!(value, b'.' | b' '))
                        {
                            Kind::Unstaged
                        } else {
                            Kind::Staged
                        },
                        status: entry["xy"]
                            .as_str()
                            .unwrap_or("??")
                            .into(),
                        undone: false,
                    })
                    .collect();
                Ok(Snapshot::Status {
                    entries,
                    next: listing["next_offset"]
                        .as_u64()
                        .map(|offset| offset as usize),
                    branch: listing["branch"]["head"]
                        .as_str()
                        .map(str::to_owned),
                })
            }
            Action::History { revision, offset } => {
                let mut value =
                    orbit_tools::git_inspect::history::list(&filesystem, &revision, offset)?;
                value["view"] = "history".into();
                Ok(serde_json::from_value(value)?)
            }
            Action::Commit {
                revision,
                include_patch,
            } => {
                let mut value = orbit_tools::git_inspect::history::commit(
                    &filesystem,
                    &revision,
                    include_patch,
                )?;
                value["view"] = "commit".into();
                Ok(serde_json::from_value(value)?)
            }
            Action::CommitFile { revision, path } => {
                let mut value =
                    orbit_tools::git_inspect::history::file(&filesystem, &revision, &path)?;
                value["view"] = "commit_file".into();
                Ok(serde_json::from_value(value)?)
            }
            Action::List {
                kind: Kind::Checkpoint,
                ..
            } => Ok(Snapshot::List {
                entries: vec![Entry {
                    path: "src/main.rs".into(),
                    kind: Kind::Checkpoint,
                    status: "completed".into(),
                    undone: undone.exists(),
                }],
                next: None,
                branch: None,
            }),
            Action::Diff {
                kind: Kind::Checkpoint,
                path,
            } => Ok(Snapshot::Diff {
                path: path.clone(),
                patch: format!(
                    "--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-fn main() {{}}\n+fn main() {{ \
                        println!(\"Hello Orbit\"); }}\n"
                ),
                note: "Orbit 写入检查点 · WriteFile · run 7".into(),
                checkpoint: Some(Checkpoint {
                    run_id: 7,
                    call_id: "fixture-write".into(),
                    undone: undone.exists(),
                }),
                index_receipt: None,
            }),
            Action::Undo {
                path,
                run_id,
                call_id,
            } => {
                anyhow::ensure!(
                    !undone.exists() && run_id == 7 && call_id == "fixture-write",
                    "检查点已变更，请重新审查后撤销"
                );
                std::fs::write(undone, b"undone")?;
                Ok(Snapshot::Undone {
                    path,
                    detail: "已撤销检查点".into(),
                })
            }
            Action::List { kind, offset } => {
                let listing = orbit_tools::git_inspect::review_status(&filesystem, offset)?;
                let entries = listing["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| {
                        let xy = entry["xy"]
                            .as_str()
                            .unwrap_or("..")
                            .as_bytes();
                        let include = match kind {
                            Kind::Untracked => entry["kind"] == "untracked",
                            Kind::Staged => xy
                                .first()
                                .is_some_and(|v| !matches!(v, b'.' | b' ')),
                            Kind::Unstaged => xy
                                .get(1)
                                .is_some_and(|v| !matches!(v, b'.' | b' ')),
                            _ => false,
                        };
                        include.then(|| Entry {
                            path: entry["path"]
                                .as_str()
                                .unwrap_or_default()
                                .into(),
                            kind,
                            status: entry["xy"]
                                .as_str()
                                .unwrap_or("新增")
                                .into(),
                            undone: false,
                        })
                    })
                    .collect();
                Ok(Snapshot::List {
                    entries,
                    next: listing["next_offset"]
                        .as_u64()
                        .map(|v| v as usize),
                    branch: listing["branch"]["head"]
                        .as_str()
                        .map(str::to_owned),
                })
            }
            Action::Diff { kind, path } => {
                let reviewed = orbit_tools::git_inspect::review_index_patch(
                    &filesystem,
                    &path,
                    kind == Kind::Staged,
                    kind == Kind::Untracked,
                )?;
                Ok(Snapshot::Diff {
                    patch: reviewed.patch,
                    path,
                    note: reviewed
                        .index_note
                        .unwrap_or_else(|| "Git 文件差异".into()),
                    checkpoint: None,
                    index_receipt: reviewed.index_receipt,
                })
            }
            action @ (Action::Stage { .. } | Action::Unstage { .. }) => {
                let staged = matches!(action, Action::Stage { .. });
                let (path, receipt) = match action {
                    Action::Stage { path, receipt } | Action::Unstage { path, receipt } => {
                        (path, receipt)
                    }
                    _ => unreachable!(),
                };
                orbit_tools::git_inspect::update_review_index(
                    &filesystem,
                    &path,
                    &receipt,
                    staged,
                    Default::default(),
                )?;
                Ok(Snapshot::IndexUpdated { path, staged })
            }
        }
    })();
    match result {
        Ok(snapshot) => json!({
            "type": "review",
            "value": snapshot
        }),
        Err(error) => json!({
            "type": "error",
            "value": format!("{error:#}")
        }),
    }
}
