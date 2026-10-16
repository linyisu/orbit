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
            unknown. Does not execute commands or change files.",
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
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
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
    let current = fingerprint_configured(workspace, token, limits.into()).await?;
    let (baseline, changes, commands) = tokio::try_join!(
        store.workspace_baseline(id),
        store.workspace_changes(id),
        store.verification_records(id)
    )?;

    let mut result = compare(&current, baseline.as_ref(), &changes, &commands);

    while orbit_agent::context::estimate_tokens(
        &[orbit_model::types::ChatMessage::tool(
            "CheckEvidence",
            result.to_string(),
        )],
        &[],
    ) > 6_000
    {
        let key = if result["changes"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            "changes"
        } else {
            "commands"
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
    }

    Ok(result)
}

fn compare(
    current: &WorkspaceFingerprint,
    baseline: Option<&WorkspaceFingerprint>,
    changes: &[WorkspaceChange],
    commands: &[VerificationRecord],
) -> Value {
    let records: Vec<_> = commands
        .iter()
        .rev()
        .take(20)
        .map(|record| {
            let input_state = match &record.evidence {
                None => "unknown",
                Some(e) => {
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

    let mut paths = std::collections::BTreeSet::new();

    for c in changes {
        paths.insert(c.change.path.clone());
    }

    if let Some(baseline) = baseline {
        for path in baseline
            .files
            .keys()
            .chain(current.files.keys())
        {
            if baseline.files.get(path) != current.files.get(path) {
                paths.insert(path.clone());
            }
        }
    }

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
                .files
                .get(&path)
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

            let origin = if last.is_some() && current_hash == last_expected {
                "content_matches_orbit_receipt"
            } else if last.is_some() {
                "content_modified_after_orbit"
            } else {
                "external_change"
            };

            let changed_from_baseline =
                baseline.map(|b| b.files.get(&path) != current.files.get(&path));
            let executable_changed = baseline.map(|b| {
                b.files.get(&path).map(|f| f.executable)
                    != current
                        .files
                        .get(&path)
                        .map(|f| f.executable)
            });
            let external_before_first_orbit_write = baseline
                .and(first)
                .map(|_| baseline_hash != first_before);

            json!({
                "path": path,
                "origin": origin,
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
        "snapshot_id": current.digest,
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
                "输入版本  最近 {} 条命令：输入未变 {} · 已过期 {} · 未记录 {}",
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
                    _ => "会话开始后发生的外部变更",
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
            &verified,
            Some(&baseline),
            std::slice::from_ref(&change),
            std::slice::from_ref(&command),
        );

        assert_eq!(same["commands"][0]["input_state"], "input_unchanged");

        let mut modified_ws = verified.clone();
        modified_ws.digest = "modified_digest".into();
        let command_with_output = VerificationRecord {
            run_id: 2,
            run_status: "completed".into(),
            command: "touch new_file".into(),
            exit_code: Some(0),
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
            &modified_ws,
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
            &current,
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
            compare(&current, None, &[], &[command])["commands"][0]["input_state"],
            "unknown"
        );
    }
}
