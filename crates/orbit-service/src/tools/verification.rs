//! Compare execution evidence to the current filtered workspace, without
//! claiming that a zero exit status proves a test suite or reproduces an OS.
use anyhow::Result;
use orbit_agent::tool::{ClosureTool, ToolError, ToolRegistry};
use orbit_runtime::{
    execution::{content_hash, fingerprint_configured},
    filesystem::Workspace,
    verification::WorkspaceFingerprint,
};
use orbit_store::{
    store::Store,
    types::{VerificationRecord, WorkspaceChange},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::project_state::MemoryScope;

pub fn register(
    tools: &mut ToolRegistry,
    store: Store,
    scope: MemoryScope,
    workspace: Workspace,
    limits: orbit_tools::run_command::CommandLimits,
) {
    tools.register(ClosureTool::new(
        "CheckEvidence",
        "Inspect this session's actual command evidence and compare input file fingerprints with \
            the current filtered workspace. Call after final edits before reporting verification. \
            A zero exit code is not proof of tests; input_unchanged means the current workspace \
            matches the recorded input or post-command output fingerprint; it does not verify \
            mutable system tools, caches, or external state. Also classifies per-path changes \
            against the session baseline and Orbit receipts. Old sessions without evidence remain \
            unknown. If workspace scanning is unavailable or exceeds its budget, returns existing \
            records with unknown current input state and workspace_scan.complete=false; do not \
            retry the same scan or claim the files are unchanged. Does not execute commands or \
            change files.",
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        move |args, token| {
            let store = store.clone();
            let id = scope.session_id();
            let workspace = workspace.clone();
            let limits = limits.clone();

            async move {
                if !args.as_object().is_some_and(|a| a.is_empty()) {
                    return Err(ToolError::InvalidArguments {
                        tool: "CheckEvidence".into(),
                        message: "no arguments expected".into(),
                    });
                }

                report(&store, &id, workspace, token, limits)
                    .await
                    .map(|r| r.to_string())
                    .map_err(ToolError::from_anyhow)
            }
        },
    ).with_capabilities(orbit_agent::tool::ToolCapabilities::READ));
}

pub async fn report(
    store: &Store,
    id: &str,
    workspace: Workspace,
    token: CancellationToken,
    limits: orbit_tools::run_command::CommandLimits,
) -> Result<Value> {
    let (current, scan_error) =
        match fingerprint_configured(workspace.clone(), token.clone(), limits.into()).await {
            Ok(current) => (Some(current), None),
            Err(orbit_runtime::error::RuntimeError::Cancelled) => {
                return Err(orbit_runtime::error::RuntimeError::Cancelled.into());
            }
            Err(error) => (None, Some(error.to_string())),
        };
    let (baseline, changes, commands) = tokio::try_join!(
        store.workspace_baseline(id),
        store.workspace_changes(id),
        store.verification_records(id)
    )?;

    let mut result = compare(current.as_ref(), baseline.as_ref(), &changes, &commands);
    let candidates = orbit_tools::project_overview::ProjectOverview::new(workspace)
        .verification_candidates(
            changed_paths(current.as_ref(), baseline.as_ref(), &changes)
                .into_iter()
                .collect(),
            token,
        )
        .await?;
    result["verification_candidates"] = json!(candidates.candidates);
    result["verification_candidates_incomplete"] =
        json!(candidates.incomplete || current.is_none());
    result["workspace_scan"] = json!({
        "complete": current.is_some(),
        "error": scan_error,
    });

    while orbit_agent::context::estimate_tokens(
        &[orbit_model::types::ChatMessage::tool(
            "CheckEvidence",
            result.to_string(),
        )],
        &[],
    ) > 6_000
    {
        let Some(key) = ["verification_candidates", "changes", "commands"]
            .into_iter()
            .find(|key| {
                result[*key]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
            })
        else {
            break;
        };

        if result[key]
            .as_array_mut()
            .unwrap()
            .pop()
            .is_none()
        {
            break;
        }

        let omitted = format!("{key}_omitted");
        result[&omitted] = json!(result[&omitted].as_u64().unwrap_or(0) + 1);
        if key == "verification_candidates" {
            result["verification_candidates_incomplete"] = json!(true);
        }
    }

    Ok(result)
}

fn changed_paths(
    current: Option<&WorkspaceFingerprint>,
    baseline: Option<&WorkspaceFingerprint>,
    changes: &[WorkspaceChange],
) -> std::collections::BTreeSet<String> {
    let mut paths = std::collections::BTreeSet::new();
    paths.extend(
        changes
            .iter()
            .map(|change| change.change.path.clone()),
    );
    if let (Some(baseline), Some(current)) = (baseline, current) {
        paths.extend(
            baseline
                .files
                .keys()
                .chain(current.files.keys())
                .filter(|path| baseline.files.get(*path) != current.files.get(*path))
                .cloned(),
        );
    }
    paths
}

fn compare(
    current: Option<&WorkspaceFingerprint>,
    baseline: Option<&WorkspaceFingerprint>,
    changes: &[WorkspaceChange],
    commands: &[VerificationRecord],
) -> Value {
    let records: Vec<_> = commands
        .iter()
        .rev()
        .take(20)
        .map(|record| {
            let input_state = match (current, &record.evidence) {
                (Some(current), Some(e)) => {
                    let matches_output = e
                        .output
                        .as_ref()
                        .is_some_and(|o| o.digest == current.digest);
                    let matches_input = e.input.digest == current.digest;

                    if matches_output || matches_input {
                        "input_unchanged"
                    } else {
                        "stale"
                    }
                }
                _ => "unknown",
            };

            let snapshot_id = record.evidence.as_ref().map(|e| {
                e.output
                    .as_ref()
                    .unwrap_or(&e.input)
                    .digest
                    .as_str()
            });

            json!({
                "run": record.run_id,
                "command": record
                    .command
                    .chars()
                    .take(512)
                    .collect::<String>(),
                "command_truncated": record.command.chars().count() > 512,
                "exit_code": record.exit_code,
                "signal": record.signal,
                "core_dumped": record.core_dumped,
                "timed_out": record.timed_out,
                "cancelled": record.cancelled,
                "elapsed_ms": record.elapsed_ms,
                "input_state": input_state,
                "snapshot_id": snapshot_id,
                "environment": record.evidence.as_ref().map(|e| json!(
                    {
                        "platform": e.platform,
                        "isolation": e.isolation,
                    }
                ))
            })
        })
        .collect();

    let paths = changed_paths(current, baseline, changes);

    let total = paths.len();
    let paths: Vec<_> = paths
        .into_iter()
        .take(100)
        .map(|path| {
            let first = changes
                .iter()
                .find(|c| c.change.path == path);
            let last = changes
                .iter()
                .rev()
                .find(|c| c.change.path == path);
            let current_hash = current
                .and_then(|current| current.files.get(&path))
                .map(|f| f.sha256.clone());
            let baseline_hash = baseline
                .and_then(|b| b.files.get(&path))
                .map(|f| f.sha256.clone());
            let first_before = first.and_then(|c| {
                c.change
                    .before
                    .as_ref()
                    .map(|s| content_hash(s.as_bytes()))
            });
            let last_expected = last.and_then(|c| {
                if c.undone {
                    c.change
                        .before
                        .as_ref()
                        .map(|s| content_hash(s.as_bytes()))
                } else {
                    Some(content_hash(c.change.after.as_bytes()))
                }
            });

            let origin = if current.is_none() {
                "unknown"
            } else if last.is_some() && current_hash == last_expected {
                "content_matches_orbit_receipt"
            } else if last.is_some() {
                "content_modified_after_orbit"
            } else {
                "unattributed_change"
            };

            let changed_from_baseline = baseline
                .zip(current)
                .map(|(b, current)| b.files.get(&path) != current.files.get(&path));
            let executable_changed = baseline
                .zip(current)
                .and_then(|(b, current)| {
                    Some(b.files.get(&path)?.executable != current.files.get(&path)?.executable)
                });
            let change_kind = baseline
                .zip(current)
                .map(
                    |(b, current)| match (b.files.get(&path), current.files.get(&path)) {
                        (None, Some(_)) => "created",
                        (Some(_), None) => "deleted",
                        (Some(before), Some(after)) if before == after => "unchanged",
                        _ => "modified",
                    },
                );
            let external_before_first_orbit_write = baseline
                .and(first)
                .map(|_| baseline_hash != first_before);

            json!({
                "path": path,
                "origin": origin,
                "change_kind": change_kind,
                "baseline_available": baseline.is_some(),
                "changed_from_baseline": changed_from_baseline,
                "executable_changed": executable_changed,
                "external_before_first_orbit_write": external_before_first_orbit_write,
                "baseline_sha256": baseline_hash,
                "first_orbit_before_sha256": first_before,
                "last_orbit_expected_sha256": last_expected,
                "current_sha256": current_hash
            })
        })
        .collect();

    json!({
        "snapshot_id": current.map(|current| &current.digest),
        "changes_incomplete": current.is_none(),
        "baseline_available": baseline.is_some(),
        "command_count": commands.len(),
        "commands": records,
        "commands_omitted": commands.len().saturating_sub(20),
        "changes": paths,
        "changes_omitted": total.saturating_sub(100),
        "limitations": "Source fingerprints cover the same filtered workspace as Exec, not \
            ignored runtime directories or external system state. Commands execute in the real \
            workspace with the target user environment. Inspect the command and its output before \
            claiming tests passed; stale or unknown evidence does not verify current edits."
    })
}

pub async fn summary(session: &crate::session::SessionState) -> String {
    let Some(workspace) = session.audit_workspace.clone() else {
        return "输入版本  未记录".into();
    };

    match report(
        &session.store,
        session.id(),
        workspace,
        CancellationToken::new(),
        session.audit_limits.clone(),
    )
    .await
    {
        Ok(value) => {
            let entries = value["commands"].as_array().unwrap();
            let unchanged = entries
                .iter()
                .filter(|r| r["input_state"] == "input_unchanged")
                .count();
            let stale = entries
                .iter()
                .filter(|r| r["input_state"] == "stale")
                .count();

            format!(
                "输入版本  最近 {} 条命令：输入未变 {} · 已过期 {} · 未确认 {}",
                entries.len(),
                unchanged,
                stale,
                entries.len() - unchanged - stale
            )
        }

        Err(error) => format!("输入版本  无法核对：{error}"),
    }
}

pub async fn changes_text(session: &crate::session::SessionState) -> String {
    let Some(workspace) = session.audit_workspace.clone() else {
        return String::new();
    };

    match report(
        &session.store,
        session.id(),
        workspace,
        CancellationToken::new(),
        session.audit_limits.clone(),
    )
    .await
    {
        Ok(value) => {
            use std::fmt::Write;

            let mut text = String::from("\n当前文件核对\n");

            if value["workspace_scan"]["complete"] == false {
                text.push_str("当前文件扫描未完成；以下仅为已记录的路径，不能确认当前内容。\n");
            }

            if value["baseline_available"] != true {
                text.push_str("旧会话未保存开始时的文件基线。\n");
            }

            for row in value["changes"].as_array().unwrap() {
                let state = match row["origin"]
                    .as_str()
                    .unwrap_or_default()
                {
                    "content_matches_orbit_receipt" => "内容与 Orbit 检查点一致",
                    "content_modified_after_orbit" => "内容已偏离 Orbit 检查点",
                    "external_change" | "unattributed_change" => "变更来源未确认",
                    _ => "变更来源未知",
                };

                let prior = if row["external_before_first_orbit_write"] == true {
                    "；首次 Orbit 写入前已有外部改动"
                } else {
                    ""
                };

                let _ = writeln!(
                    text,
                    "{}  {}{}",
                    row["path"].as_str().unwrap_or_default(),
                    state,
                    prior
                );
            }

            if value["changes_omitted"]
                .as_u64()
                .unwrap_or(0)
                > 0
            {
                text.push_str("部分路径未列出，请缩小检查范围。\n");
            }

            text
        }

        Err(error) => format!("\n当前文件无法核对：{error}"),
    }
}

#[cfg(test)]
mod tests {
    use orbit_agent::tool::FileChange;
    use orbit_runtime::verification::{FileFingerprint, VerificationEvidence};

    use super::*;

    #[tokio::test]
    async fn changed_files_get_manifest_suggestions_without_becoming_verification_receipts() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(project.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();
        let workspace = Workspace::new(&project).unwrap();
        let limits = orbit_tools::run_command::CommandLimits::default();
        let baseline = fingerprint_configured(
            workspace.clone(),
            CancellationToken::new(),
            limits.clone().into(),
        )
        .await
        .unwrap();
        let store = Store::open(directory.path().join("sessions.db"))
            .await
            .unwrap();
        let writer = store
            .create(project.display().to_string(), "fixture".into())
            .await
            .unwrap();
        store
            .save_workspace_baseline(writer.id(), &baseline)
            .await
            .unwrap();
        std::fs::write(project.join("src/lib.rs"), "pub fn value() -> u8 { 2 }\n").unwrap();
        let result = report(
            &store,
            writer.id(),
            workspace,
            CancellationToken::new(),
            limits,
        )
        .await
        .unwrap();
        assert_eq!(result["command_count"], 0);
        assert_eq!(result["verification_candidates_incomplete"], false);
        assert_eq!(
            result["verification_candidates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(
            result["verification_candidates"]
                .as_array()
                .unwrap()
                .iter()
                .all(|candidate| { candidate["affected_paths"] == json!(["src/lib.rs"]) })
        );
        assert_eq!(result["changes"][0]["origin"], "unattributed_change");
    }

    #[tokio::test]
    async fn scan_budget_exhaustion_keeps_evidence_available_but_never_confirms_current_files() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let large = std::fs::File::create(project.join("generated.bin")).unwrap();
        large
            .set_len(2 * 1_024 * 1_024)
            .unwrap();
        let workspace = Workspace::new(&project).unwrap();
        let store = Store::open(directory.path().join("sessions.db"))
            .await
            .unwrap();
        let writer = store
            .create(project.display().to_string(), "fixture".into())
            .await
            .unwrap();
        let limits = orbit_tools::run_command::CommandLimits {
            max_scan_mib: 1,
            ..Default::default()
        };
        let result = report(
            &store,
            writer.id(),
            workspace.clone(),
            CancellationToken::new(),
            limits.clone(),
        )
        .await
        .unwrap();
        assert_eq!(result["workspace_scan"]["complete"], false);
        assert!(
            result["workspace_scan"]["error"]
                .as_str()
                .unwrap()
                .contains("1 MiB")
        );
        assert_eq!(result["changes_incomplete"], true);
        assert!(result["snapshot_id"].is_null());
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            report(&store, writer.id(), workspace, cancelled, limits)
                .await
                .is_err()
        );
    }

    #[test]
    fn file_presence_and_execute_bit_changes_are_distinct_from_attribution() {
        let baseline = input("base");
        let mut created = baseline.clone();
        created.files.insert(
            "new.rs".into(),
            FileFingerprint {
                sha256: content_hash(b"new"),
                executable: false,
            },
        );
        let result = compare(Some(&created), Some(&baseline), &[], &[]);
        assert_eq!(result["changes"][0]["change_kind"], "created");
        assert_eq!(result["changes"][0]["origin"], "unattributed_change");
        assert!(result["changes"][0]["executable_changed"].is_null());
        let deleted = WorkspaceFingerprint::default();
        let result = compare(Some(&deleted), Some(&baseline), &[], &[]);
        assert_eq!(result["changes"][0]["change_kind"], "deleted");
        assert!(result["changes"][0]["executable_changed"].is_null());
        let mut executable = baseline.clone();
        executable
            .files
            .get_mut("a.rs")
            .unwrap()
            .executable = true;
        let result = compare(Some(&executable), Some(&baseline), &[], &[]);
        assert_eq!(result["changes"][0]["change_kind"], "modified");
        assert_eq!(result["changes"][0]["executable_changed"], true);
    }
    fn input(content: &str) -> WorkspaceFingerprint {
        WorkspaceFingerprint {
            digest: content_hash(content.as_bytes()),
            files: [(
                "a.rs".into(),
                FileFingerprint {
                    sha256: content_hash(content.as_bytes()),
                    executable: false,
                },
            )]
            .into(),
            ..Default::default()
        }
    }

    #[test]
    fn edits_stale_checks_and_distinguish_external_edits_without_claiming_test_success() {
        let baseline = input("user start");
        let verified = input("orbit edit");
        let current = input("external later");
        let change = WorkspaceChange {
            run_id: 1,
            run_status: "completed".into(),
            call_id: "edit".into(),
            tool: "EditFile".into(),
            undone: false,
            change: FileChange {
                path: "a.rs".into(),
                before: Some("external before orbit".into()),
                after: "orbit edit".into(),
            },
        };
        let mut command = VerificationRecord {
            run_id: 1,
            run_status: "completed".into(),
            command: "echo no tests".into(),
            exit_code: Some(0),
            signal: None,
            core_dumped: false,
            timed_out: false,
            cancelled: false,
            elapsed_ms: 1,
            evidence: Some(VerificationEvidence {
                input: verified.clone(),
                output: None,
                platform: "linux".into(),
                isolation: "native".into(),
            }),
        };
        let same = compare(
            Some(&verified),
            Some(&baseline),
            std::slice::from_ref(&change),
            std::slice::from_ref(&command),
        );

        assert_eq!(same["commands"][0]["input_state"], "input_unchanged");
        let unavailable = compare(
            None,
            Some(&baseline),
            std::slice::from_ref(&change),
            std::slice::from_ref(&command),
        );
        assert_eq!(unavailable["commands"][0]["input_state"], "unknown");
        assert_eq!(unavailable["changes"][0]["origin"], "unknown");
        assert!(unavailable["changes"][0]["change_kind"].is_null());
        assert!(unavailable["changes"][0]["changed_from_baseline"].is_null());

        let mut modified_ws = verified.clone();
        modified_ws.digest = "modified_digest".into();
        let command_with_output = VerificationRecord {
            run_id: 2,
            run_status: "completed".into(),
            command: "touch new_file".into(),
            exit_code: Some(0),
            signal: None,
            core_dumped: false,
            timed_out: false,
            cancelled: false,
            elapsed_ms: 5,
            evidence: Some(VerificationEvidence {
                input: verified.clone(),
                output: Some(modified_ws.clone()),
                platform: "linux".into(),
                isolation: "native".into(),
            }),
        };
        let compared = compare(
            Some(&modified_ws),
            Some(&baseline),
            &[],
            std::slice::from_ref(&command_with_output),
        );

        assert_eq!(compared["commands"][0]["input_state"], "input_unchanged");
        assert_eq!(compared["commands"][0]["snapshot_id"], "modified_digest");
        assert_eq!(
            same["changes"][0]["external_before_first_orbit_write"],
            true
        );

        let changed = compare(
            Some(&current),
            Some(&baseline),
            &[change],
            std::slice::from_ref(&command),
        );

        assert_eq!(changed["commands"][0]["input_state"], "stale");
        assert_eq!(
            changed["changes"][0]["origin"],
            "content_modified_after_orbit"
        );

        command.evidence = None;

        assert_eq!(
            compare(Some(&current), None, &[], &[command])["commands"][0]["input_state"],
            "unknown"
        );
    }
}
