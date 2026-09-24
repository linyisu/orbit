use std::{path::Path, process::Stdio, time::Duration};

use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

use crate::workspace::Workspace;

const MAX_GIT_BYTES: usize = 1024 * 1024;
const MAX_ERROR_BYTES: usize = 8 * 1024;
const MAX_ENTRIES: usize = 200;
const MAX_OFFSET: usize = 10_000;
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum View {
    #[default]
    Status,
    Diff,
    History,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Arguments {
    view: View,
    path: Option<String>,
    staged: bool,
    offset: usize,
    limit: Option<usize>,
}

pub struct GitInspect {
    workspace: Workspace,
}

impl GitInspect {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

impl Tool for GitInspect {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "git_inspect",
            "Inspect Git status, tracked-file diffs, or recent commits for the repository rooted at this workspace. This tool is read-only. status includes untracked paths; diff compares the worktree to the index, or the index to HEAD when staged=true. Use read_file for untracked content. Output is bounded and protected workspace files are excluded. Use path to narrow large results. No shell, hooks, external diff, global Git config, or optional Git locks are used.",
            json!({
                "type": "object",
                "properties": {
                    "view": { "type": "string", "enum": ["status", "diff", "history"], "default": "status" },
                    "path": { "type": "string", "description": "Optional workspace-relative file or directory for status/diff" },
                    "staged": { "type": "boolean", "default": false, "description": "For diff: compare the index to HEAD" },
                    "offset": { "type": "integer", "minimum": 0, "maximum": MAX_OFFSET, "default": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_ENTRIES, "default": 100 }
                },
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;
            let limit = args.limit.unwrap_or(100);
            if !(1..=MAX_ENTRIES).contains(&limit) || args.offset > MAX_OFFSET {
                return Err(invalid("limit must be 1–200 and offset must be 0–10000"));
            }
            if matches!(args.view, View::History) && (args.path.is_some() || args.staged) {
                return Err(invalid("history does not accept path or staged"));
            }
            if !matches!(args.view, View::Diff) && args.staged {
                return Err(invalid("staged is only valid for diff"));
            }
            let path = match args.path.as_deref() {
                Some(path) if path.trim().is_empty() => {
                    return Err(invalid("path must not be empty"));
                }
                Some(path) => Some(
                    safe_path(&self.workspace, path)?
                        .to_string_lossy()
                        .into_owned(),
                ),
                None => None,
            };
            let root = self.workspace.root();
            let actual_root = git(root, &["rev-parse", "--show-toplevel"], &cancellation).await?;
            let actual_root = std::str::from_utf8(&actual_root)
                .map_err(|_| failed("Git returned a non-UTF-8 repository root"))?
                .trim_end();
            if Path::new(actual_root).canonicalize().map_err(failed)? != root {
                return Err(failed(
                    "Git repository root differs from the workspace root",
                ));
            }

            let output = match args.view {
                View::Status => {
                    status(
                        &self.workspace,
                        path.as_deref(),
                        args.offset,
                        limit,
                        &cancellation,
                    )
                    .await?
                }
                View::Diff => {
                    diff(
                        &self.workspace,
                        path.as_deref(),
                        args.staged,
                        args.offset,
                        limit,
                        &cancellation,
                    )
                    .await?
                }
                View::History => history(root, args.offset, limit, &cancellation).await?,
            };
            serde_json::to_string(&output).map_err(failed)
        })
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "git_inspect".to_owned(),
        message: message.into(),
    }
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

fn safe_path(workspace: &Workspace, path: &str) -> Result<std::path::PathBuf, ToolError> {
    let relative = workspace
        .checked_relative(Path::new(path))
        .map_err(failed)?;
    let actual = workspace.root().join(&relative);
    match std::fs::symlink_metadata(&actual) {
        Ok(metadata) if metadata.is_dir() => {
            workspace.open_directory(&relative).map_err(failed)?;
        }
        Ok(metadata) if metadata.is_file() => {
            workspace.open_read(&relative).map_err(failed)?;
        }
        Ok(_) => return Err(failed("Git path is not a regular file or directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed(error)),
    }
    Ok(relative)
}

fn allowed_status_path(workspace: &Workspace, path: &str) -> bool {
    safe_path(workspace, path).is_ok()
}

async fn status(
    workspace: &Workspace,
    path: Option<&str>,
    offset: usize,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Value, ToolError> {
    let mut args = vec![
        "status",
        "--porcelain=v2",
        "--branch",
        "-z",
        "--untracked-files=all",
    ];
    if let Some(path) = path {
        args.extend(["--", path]);
    }
    let output = git(workspace.root(), &args, cancellation).await?;
    let records = output.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut branch = json!({ "head": null, "oid": null, "upstream": null });
    let mut entries = Vec::new();
    let mut index = 0;

    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.is_empty() {
            continue;
        }
        if record.starts_with(b"# ") {
            parse_branch(&mut branch, record)?;
            continue;
        }
        let (entry, extra) = parse_status(record, records.get(index).copied())?;
        index += extra;
        if let Some(entry) = entry {
            let current = entry["path"].as_str().unwrap_or_default();
            let original = entry["original_path"].as_str();
            if allowed_status_path(workspace, current)
                && original.is_none_or(|path| allowed_status_path(workspace, path))
            {
                entries.push(entry);
            }
        }
    }

    let total = entries.len();
    let end = offset.saturating_add(limit).min(total);
    let page = entries.get(offset..end).unwrap_or_default();
    Ok(json!({
        "repository_root": workspace.root(),
        "branch": branch,
        "entries": page,
        "offset": offset,
        "next_offset": (end < total).then_some(end),
        "has_more": end < total,
        "total_entries": total
    }))
}

fn parse_branch(branch: &mut Value, record: &[u8]) -> Result<(), ToolError> {
    let record = std::str::from_utf8(record).map_err(failed)?;
    if let Some((key, value)) = record[2..].split_once(' ') {
        match key {
            "branch.head" => branch["head"] = json!(value),
            "branch.oid" => branch["oid"] = json!(value),
            "branch.upstream" => branch["upstream"] = json!(value),
            _ => {}
        }
    }
    Ok(())
}

fn parse_status(
    record: &[u8],
    original: Option<&[u8]>,
) -> Result<(Option<Value>, usize), ToolError> {
    let record = std::str::from_utf8(record).map_err(failed)?;
    if let Some(path) = record.strip_prefix("? ") {
        return Ok((Some(json!({ "kind": "untracked", "path": path })), 0));
    }
    let tag = record
        .split_once(' ')
        .map(|(tag, _)| tag)
        .unwrap_or_default();
    if !matches!(tag, "1" | "2" | "u") {
        return Ok((None, 0));
    }
    let mut fields = record.splitn(if tag == "u" { 11 } else { 9 }, ' ');
    let _ = fields.next();
    let state = fields.next().unwrap_or_default();
    let skip = if tag == "u" { 8 } else { 6 };
    for _ in 0..skip {
        fields.next();
    }
    let tail = fields.next().unwrap_or_default();
    let (score, path) = if tag == "2" {
        let (score, path) = tail.split_once(' ').unwrap_or(("", tail));
        (Some(score), path)
    } else {
        (None, tail)
    };
    let kind = match tag {
        "1" => "changed",
        "2" => "renamed_or_copied",
        _ => "unmerged",
    };
    let mut entry = json!({ "kind": kind, "state": state, "path": path });
    if tag == "2" {
        entry["score"] = json!(score);
        entry["original_path"] = json!(
            original
                .map(std::str::from_utf8)
                .transpose()
                .map_err(failed)?
        );
        return Ok((Some(entry), 1));
    }
    Ok((Some(entry), 0))
}

async fn diff(
    workspace: &Workspace,
    path: Option<&str>,
    staged: bool,
    offset: usize,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Value, ToolError> {
    let mut names = vec![
        "diff",
        "--name-only",
        "-z",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
    ];
    if staged {
        names.push("--cached");
    }
    if let Some(path) = path {
        names.extend(["--", path]);
    }
    let output = git(workspace.root(), &names, cancellation).await?;
    let paths = output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| std::str::from_utf8(path).map_err(failed))
        .collect::<Result<Vec<_>, _>>()?;
    let safe = paths
        .into_iter()
        .filter(|path| allowed_status_path(workspace, path))
        .collect::<Vec<_>>();
    if safe.len() > 500 {
        return Err(failed(
            "too many changed files for one diff; narrow the path",
        ));
    }
    if safe.is_empty() {
        return Ok(json!({
            "repository_root": workspace.root(),
            "staged": staged,
            "path": path,
            "lines": [],
            "offset": offset,
            "next_offset": null,
            "has_more": false,
            "total_lines": 0
        }));
    }

    let mut args = vec![
        "diff",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
        "--unified=3",
    ];
    if staged {
        args.push("--cached");
    }
    args.push("--");
    args.extend(safe);
    let output = git(workspace.root(), &args, cancellation).await?;
    let text = String::from_utf8(output).map_err(failed)?;
    let lines = text.lines().collect::<Vec<_>>();
    let end = offset.saturating_add(limit).min(lines.len());
    Ok(json!({
        "repository_root": workspace.root(),
        "staged": staged,
        "path": path,
        "lines": lines.get(offset..end).unwrap_or_default(),
        "offset": offset,
        "next_offset": (end < lines.len()).then_some(end),
        "has_more": end < lines.len(),
        "total_lines": lines.len()
    }))
}

async fn history(
    root: &Path,
    offset: usize,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Value, ToolError> {
    let count = format!("--max-count={}", limit + 1);
    let skip = format!("--skip={offset}");
    let output = git(
        root,
        &[
            "log",
            &count,
            &skip,
            "--format=%H%x00%h%x00%aI%x00%an%x00%s",
            "-z",
        ],
        cancellation,
    )
    .await?;
    let mut fields = output.split(|byte| *byte == 0).collect::<Vec<_>>();
    fields.pop();
    let mut commits = Vec::new();
    let (records, _) = fields.as_chunks::<5>();
    for record in records.iter().take(limit) {
        commits.push(json!({
            "id": std::str::from_utf8(record[0]).map_err(failed)?,
            "short_id": std::str::from_utf8(record[1]).map_err(failed)?,
            "authored_at": std::str::from_utf8(record[2]).map_err(failed)?,
            "author": std::str::from_utf8(record[3]).map_err(failed)?,
            "subject": std::str::from_utf8(record[4]).map_err(failed)?
        }));
    }
    let has_more = records.len() > limit;
    Ok(json!({
        "repository_root": root,
        "commits": commits,
        "offset": offset,
        "next_offset": has_more.then_some(offset + commits.len()),
        "has_more": has_more
    }))
}

async fn read_limited<R: tokio::io::AsyncRead + Unpin>(
    mut stream: R,
    limit: usize,
) -> Result<(Vec<u8>, bool), ToolError> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut overflow = false;
    loop {
        let size = stream.read(&mut buffer).await.map_err(failed)?;
        if size == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..size.min(remaining)]);
        overflow |= size > remaining;
    }
    Ok((output, overflow))
}

async fn git(
    root: &Path,
    args: &[&str],
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, ToolError> {
    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let mut command = Command::new("/usr/bin/git");
    command
        .current_dir(root)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "--no-pager",
            "--no-optional-locks",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "diff.external=",
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(failed)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| failed("Git stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| failed("Git stderr unavailable"))?;
    let stdout_task = tokio::spawn(read_limited(stdout, MAX_GIT_BYTES));
    let stderr_task = tokio::spawn(read_limited(stderr, MAX_ERROR_BYTES));
    let status = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            child.start_kill().map_err(failed)?;
            child.wait().await.map_err(failed)?;
            return Err(ToolError::Cancelled);
        }
        _ = tokio::time::sleep(GIT_TIMEOUT) => {
            child.start_kill().map_err(failed)?;
            child.wait().await.map_err(failed)?;
            return Err(failed("Git inspection timed out after 10 seconds"));
        }
        status = child.wait() => status.map_err(failed)?,
    };
    let (stdout, overflow) = stdout_task.await.map_err(failed)??;
    let (stderr, _) = stderr_task.await.map_err(failed)??;
    if !status.success() {
        let message = String::from_utf8_lossy(&stderr);
        return Err(failed(format!("Git inspection failed: {}", message.trim())));
    }
    if overflow {
        return Err(failed("Git output exceeds 1 MiB; narrow the path"));
    }
    Ok(stdout)
}
