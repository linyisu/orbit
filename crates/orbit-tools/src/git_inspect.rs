//! A bounded, read-only view of the Git repository that owns a workspace.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use orbit_runtime::filesystem::Workspace;

const MAX_OUTPUT_BYTES: usize = 256 * 1024;
/// A SWE prediction must contain the complete patch. Refuse a larger patch
/// instead of silently truncating it as the interactive inspection tool does.
const MAX_PATCH_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 500;
const MAX_OFFSET: usize = 20_000;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Arguments {
    view: View,
    path: Option<String>,
    staged: bool,
    offset: usize,
    limit: usize,
}

impl Default for Arguments {
    fn default() -> Self {
        Self {
            view: View::Status,
            path: None,
            staged: false,
            offset: 0,
            limit: 100,
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum View {
    Status,
    Diff,
    History,
}

pub struct GitInspect {
    workspace: Workspace,
}

/// Capture the initial Git state for a session. This is deliberately a small,
/// read-only snapshot: it distinguishes pre-existing worktree changes from
/// later Orbit write receipts without writing a repository-side marker.
pub fn capture_baseline(workspace: &Path) -> Option<Value> {
    if !workspace.join(".git").exists() {
        return None;
    }
    let workspace = Workspace::new(workspace).ok()?;
    let cancellation = CancellationToken::new();
    let root = repository_root(workspace.root(), &cancellation).ok()?;
    status(
        &root,
        &workspace,
        &Arguments {
            limit: MAX_ENTRIES,
            ..Arguments::default()
        },
        &cancellation,
    )
    .ok()
}

/// Export the complete patch from the repository baseline to the current
/// working tree. This is intentionally separate from the bounded
/// `git_inspect` tool response: benchmark runners must never receive a
/// truncated prediction. Untracked regular files are included as additions,
/// which is required for fixes that introduce a new source or test file.
pub fn working_tree_patch(workspace: &Path) -> Result<String, ToolError> {
    let workspace = Workspace::new(workspace).map_err(failed)?;
    let cancellation = CancellationToken::new();
    let root = repository_root(workspace.root(), &cancellation)?;
    let mut patch = git_export(
        &root,
        vec![
            "diff".into(),
            "--binary".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-renames".into(),
            "HEAD".into(),
        ],
        &cancellation,
        false,
    )?;
    let untracked = git_export(
        &root,
        vec![
            "ls-files".into(),
            "--others".into(),
            "--exclude-standard".into(),
            "-z".into(),
        ],
        &cancellation,
        false,
    )?;
    for path in untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = text(path)?;
        let relative = workspace
            .checked_relative(Path::new(&path))
            .map_err(failed)?
            .to_string_lossy()
            .into_owned();
        let addition = git_export(
            &root,
            vec![
                "diff".into(),
                "--no-index".into(),
                "--binary".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--".into(),
                "/dev/null".into(),
                relative,
            ],
            &cancellation,
            true,
        )?;
        if patch.len().saturating_add(addition.len()) > MAX_PATCH_BYTES {
            return Err(ToolError::ExecutionFailed(format!(
                "Git patch exceeds the {} MiB export limit",
                MAX_PATCH_BYTES / (1024 * 1024)
            )));
        }
        patch.extend(addition);
    }
    String::from_utf8(patch).map_err(|_| {
        ToolError::ExecutionFailed("Git patch contains non-UTF-8 paths or content".into())
    })
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
            "Inspect the Git repository that exactly matches the workspace root, without running a shell or changing the working tree. view=status (default) returns branch, HEAD baseline and paginated changed paths. view=diff returns a bounded unified diff; set staged=true for index-to-HEAD, otherwise working-tree-to-index. view=history returns recent commits. Optional path is workspace-relative and is used only for status/diff. Git hooks, external diff drivers, text conversion, user/global Git configuration, and Git environment overrides are disabled. This tool is read-only; it never stages, commits, resets, fetches or writes Git locks.",
            json!({"type":"object","properties":{
                "view":{"type":"string","enum":["status","diff","history"],"default":"status"},
                "path":{"type":"string","description":"Optional workspace-relative path for status or diff"},
                "staged":{"type":"boolean","default":false,"description":"For diff: inspect staged changes instead of unstaged changes"},
                "offset":{"type":"integer","minimum":0,"maximum":20000,"default":0,"description":"Entry offset for status/history, line offset for diff"},
                "limit":{"type":"integer","minimum":1,"maximum":500,"default":100,"description":"Entries for status/history, lines for diff"}
            },"required":[],"additionalProperties":false}),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;
            validate(&args)?;
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let workspace = self.workspace.clone();
            let token = cancellation.clone();
            let task = tokio::task::spawn_blocking(move || inspect(workspace, args, token));
            tokio::select! {
                _ = cancellation.cancelled() => Err(ToolError::Cancelled),
                result = task => result.map_err(failed)?,
            }
            .map(ToolOutput::text)
        })
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "git_inspect".into(),
        message: message.into(),
    }
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

fn validate(args: &Arguments) -> Result<(), ToolError> {
    if !(1..=MAX_ENTRIES).contains(&args.limit) || args.offset > MAX_OFFSET {
        return Err(invalid("limit must be 1–500 and offset must be 0–20000"));
    }
    if let Some(path) = &args.path
        && path.trim().is_empty()
    {
        return Err(invalid("path must not be empty"));
    }
    Ok(())
}

fn inspect(
    workspace: Workspace,
    args: Arguments,
    cancellation: CancellationToken,
) -> Result<String, ToolError> {
    let root = repository_root(workspace.root(), &cancellation)?;
    let value = match args.view {
        View::Status => status(&root, &workspace, &args, &cancellation)?,
        View::Diff => diff(&root, &workspace, &args, &cancellation)?,
        View::History => history(&root, &args, &cancellation)?,
    };
    serde_json::to_string(&value).map_err(failed)
}

fn repository_root(
    workspace: &Path,
    cancellation: &CancellationToken,
) -> Result<PathBuf, ToolError> {
    let output = git(workspace, ["rev-parse", "--show-toplevel"], cancellation)?;
    let root = PathBuf::from(text(&output)?.trim_end());
    let root = root.canonicalize().map_err(failed)?;
    if root != workspace {
        return Err(ToolError::ExecutionFailed(
            "Git repository root differs from the workspace root; start Orbit at the repository root to inspect Git safely".into(),
        ));
    }
    Ok(root)
}

fn status(
    root: &Path,
    workspace: &Workspace,
    args: &Arguments,
    cancellation: &CancellationToken,
) -> Result<Value, ToolError> {
    let output = git(
        root,
        ["status", "--porcelain=v2", "--branch", "-z"],
        cancellation,
    )?;
    let mut branch = json!({"head":null,"oid":null,"upstream":null,"ahead":0,"behind":0});
    let mut entries = Vec::new();
    let mut skipped = 0;
    let mut has_more = false;
    let path_filter = checked_path(workspace, args.path.as_deref())?;
    let records = output.split(|byte| *byte == 0).collect::<Vec<_>>();
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
        let (entry, consumed) = parse_status_entry(record, records.get(index).copied())?;
        index += consumed;
        let Some(entry) = entry else { continue };
        if path_filter.as_ref().is_some_and(|filter| {
            let path = entry
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            !(path == filter || path.starts_with(&format!("{filter}/")))
        }) {
            continue;
        }
        if skipped < args.offset {
            skipped += 1;
            continue;
        }
        if entries.len() == args.limit {
            has_more = true;
            break;
        }
        entries.push(entry);
    }
    let total_known = skipped + entries.len();
    Ok(json!({
        "repository_root": root,
        "branch": branch,
        "entries": entries,
        "offset": args.offset,
        "next_offset": has_more.then_some(args.offset + entries.len()),
        "has_more": has_more,
        "returned_entries": entries.len(),
        "scanned_entries": total_known,
    }))
}

fn diff(
    root: &Path,
    workspace: &Workspace,
    args: &Arguments,
    cancellation: &CancellationToken,
) -> Result<Value, ToolError> {
    let mut command = vec![
        "diff".to_owned(),
        "--no-ext-diff".to_owned(),
        "--no-textconv".to_owned(),
        "--unified=3".to_owned(),
    ];
    if args.staged {
        command.push("--cached".into());
    }
    if let Some(path) = checked_path(workspace, args.path.as_deref())? {
        command.push("--".into());
        command.push(path);
    }
    let output = git_owned(root, command, cancellation)?;
    let lines = String::from_utf8_lossy(&output)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let end = args.offset.saturating_add(args.limit).min(lines.len());
    Ok(json!({
        "repository_root": root,
        "staged": args.staged,
        "path": args.path,
        "offset": args.offset,
        "lines": lines.get(args.offset..end).unwrap_or_default(),
        "next_offset": (end < lines.len()).then_some(end),
        "has_more": end < lines.len(),
        "total_lines": lines.len(),
        "output_truncated": output.len() >= MAX_OUTPUT_BYTES,
    }))
}

fn history(
    root: &Path,
    args: &Arguments,
    cancellation: &CancellationToken,
) -> Result<Value, ToolError> {
    let count = args.limit.saturating_add(1);
    let output = git_owned(
        root,
        vec![
            "log".into(),
            format!("--max-count={count}"),
            format!("--skip={}", args.offset),
            // -z supplies the final field/record terminator. An extra %x00
            // here would shift every commit after the first by one field.
            "--format=%H%x00%h%x00%aI%x00%an%x00%s".into(),
            "-z".into(),
        ],
        cancellation,
    )?;
    let mut fields = output.split(|byte| *byte == 0).collect::<Vec<_>>();
    // Only consume terminated fields, never a partial record at the byte cap.
    fields.pop();
    let mut commits = Vec::new();
    let (records, _) = fields.as_chunks::<5>();
    for chunk in records.iter().take(args.limit) {
        commits.push(json!({
            "id": text(chunk[0])?,
            "short_id": text(chunk[1])?,
            "authored_at": text(chunk[2])?,
            "author": text(chunk[3])?,
            "subject": text(chunk[4])?,
        }));
    }
    let has_more = records.len() > args.limit;
    Ok(json!({
        "repository_root": root,
        "commits": commits,
        "offset": args.offset,
        "next_offset": has_more.then_some(args.offset + commits.len()),
        "has_more": has_more,
    }))
}

fn checked_path(workspace: &Workspace, path: Option<&str>) -> Result<Option<String>, ToolError> {
    path.map(|path| {
        workspace
            .checked_relative(Path::new(path))
            .map(|path| path.to_string_lossy().to_string())
            .map_err(failed)
    })
    .transpose()
}

fn parse_branch(branch: &mut Value, record: &[u8]) -> Result<(), ToolError> {
    let record = text(record)?;
    let Some((key, value)) = record[2..].split_once(' ') else {
        return Ok(());
    };
    match key {
        "branch.head" => branch["head"] = json!(value),
        "branch.oid" => branch["oid"] = json!(value),
        "branch.upstream" => branch["upstream"] = json!(value),
        "branch.ab" => {
            let mut fields = value.split_whitespace();
            branch["ahead"] = json!(
                fields
                    .next()
                    .and_then(|value| value.strip_prefix('+'))
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0)
            );
            branch["behind"] = json!(
                fields
                    .next()
                    .and_then(|value| value.strip_prefix('-'))
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0)
            );
        }
        _ => {}
    }
    Ok(())
}

fn parse_status_entry(
    record: &[u8],
    rename_from: Option<&[u8]>,
) -> Result<(Option<Value>, usize), ToolError> {
    let record = text(record)?;
    if let Some(rest) = record.strip_prefix("? ") {
        return Ok((Some(json!({"kind":"untracked","path":rest})), 0));
    }
    if let Some(rest) = record.strip_prefix("! ") {
        return Ok((Some(json!({"kind":"ignored","path":rest})), 0));
    }
    let tag = record
        .split_once(' ')
        .map(|(tag, _)| tag)
        .unwrap_or_default();
    if !matches!(tag, "1" | "2" | "u") {
        return Ok((None, 0));
    }
    let parts = if tag == "u" { 11 } else { 9 };
    let mut fields = record.splitn(parts, ' ');
    debug_assert_eq!(fields.next(), Some(tag));
    let xy = fields.next().unwrap_or_default();
    let fields_to_path = match tag {
        // After XY, ordinary records contain submodule, three modes and two
        // object IDs. Type-2 then carries score and path together in the rest.
        "1" | "2" => 6,
        // Unmerged records add a worktree mode and a third object ID.
        "u" => 8,
        _ => unreachable!(),
    };
    for _ in 0..fields_to_path {
        let _ = fields.next();
    }
    let rest = fields.next().unwrap_or_default();
    let (score, path) = if tag == "2" {
        let (score, path) = rest.split_once(' ').unwrap_or((rest, ""));
        (Some(score), path)
    } else {
        (None, rest)
    };
    let mut entry = json!({"kind":match tag { "1" => "changed", "2" => "renamed_or_copied", _ => "unmerged" },"xy":xy,"path":path});
    if tag == "2" {
        entry["score"] = json!(score);
        entry["original_path"] = json!(rename_from.map(text).transpose()?);
        return Ok((Some(entry), 1));
    }
    Ok((Some(entry), 0))
}

fn git<const N: usize>(
    root: &Path,
    args: [&str; N],
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, ToolError> {
    git_owned(
        root,
        args.into_iter().map(str::to_owned).collect(),
        cancellation,
    )
}

fn git_owned(
    root: &Path,
    args: Vec<String>,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, ToolError> {
    git_with_limit(root, args, cancellation, MAX_OUTPUT_BYTES, true, false)
}

fn git_export(
    root: &Path,
    args: Vec<String>,
    cancellation: &CancellationToken,
    allows_difference_status: bool,
) -> Result<Vec<u8>, ToolError> {
    git_with_limit(
        root,
        args,
        cancellation,
        MAX_PATCH_BYTES,
        false,
        allows_difference_status,
    )
}

fn git_with_limit(
    root: &Path,
    args: Vec<String>,
    cancellation: &CancellationToken,
    limit: usize,
    truncate: bool,
    allows_difference_status: bool,
) -> Result<Vec<u8>, ToolError> {
    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let mut command = Command::new("git");
    command
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .env("LC_ALL", "C")
        .arg("--no-optional-locks")
        .arg("--no-pager")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("diff.external=")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(failed)?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut output = Vec::new();
    let mut limited = (&mut stdout).take((limit + 1) as u64);
    limited.read_to_end(&mut output).map_err(failed)?;
    if output.len() > limit {
        let _ = child.kill();
        let _ = child.wait();
        if truncate {
            output.truncate(limit);
            return Ok(output);
        }
        return Err(ToolError::ExecutionFailed(format!(
            "Git output exceeds the {} MiB export limit",
            limit / (1024 * 1024)
        )));
    }
    let status = child.wait().map_err(failed)?;
    if !status.success() && !(allows_difference_status && status.code() == Some(1)) {
        return Err(ToolError::ExecutionFailed(
            "Git inspection failed; the workspace may not be a Git repository".into(),
        ));
    }
    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    Ok(output)
}

fn text(bytes: &[u8]) -> Result<String, ToolError> {
    std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| {
        ToolError::ExecutionFailed("Git returned a non-UTF-8 path or metadata field".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rename_and_unmerged_porcelain_v2_records() {
        let (rename, consumed) = parse_status_entry(
            b"2 R. N... 100644 100644 100644 aaaaaaa bbbbbbb R100 new-name.rs",
            Some(b"old-name.rs"),
        )
        .unwrap();
        let rename = rename.unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(rename["path"], "new-name.rs");
        assert_eq!(rename["original_path"], "old-name.rs");
        assert_eq!(rename["score"], "R100");

        let (unmerged, consumed) = parse_status_entry(
            b"u UU N... 100644 100644 100644 100644 aaaaaaa bbbbbbb ccccccc conflicted.rs",
            None,
        )
        .unwrap();
        assert_eq!(consumed, 0);
        assert_eq!(unmerged.unwrap()["path"], "conflicted.rs");
    }
}
