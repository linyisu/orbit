//! A bounded, read-only view of the Git repository that owns a workspace.

use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::filesystem::{Workspace, WorkspaceError};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub mod history;
mod index;
pub use index::{ReviewedFile, review_index_patch, update_review_index};

const MAX_OUTPUT_BYTES: usize = 256 * 1_024;
/// A SWE prediction must contain the complete patch. Refuse a larger patch
/// instead of silently truncating it as the interactive inspection tool does.
const MAX_PATCH_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_ENTRIES: usize = 500;
const MAX_OFFSET: usize = 20_000;
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Read-only initial Git baseline, separate from later write receipts.
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

/// The desktop uses the same bounded Git execution and path checks as the tool.
pub fn review_status(workspace: &Workspace, offset: usize) -> Result<Value, ToolError> {
    let cancellation = CancellationToken::new();
    let root = repository_root(workspace.root(), &cancellation)?;
    let args = Arguments {
        offset,
        limit: MAX_ENTRIES,
        ..Arguments::default()
    };
    validate(&args)?;
    status_inner(&root, workspace, &args, &cancellation, true)
}

/// Read one status page with its complete HEAD-to-worktree patch, including untracked files.
/// Every path keeps the review permission checks; aggregate output is bounded and never truncated.
pub fn review_changes(workspace: &Workspace, offset: usize) -> Result<Value, ToolError> {
    let mut listing = review_status(workspace, offset)?;
    let cancellation = CancellationToken::new();
    let root = repository_root(workspace.root(), &cancellation)?;
    let head = git_export(
        &root,
        vec![
            "rev-parse".into(),
            "--verify".into(),
            "--quiet".into(),
            "HEAD".into(),
        ],
        &cancellation,
        true,
    )?;
    let head = text(&head)?.trim().to_owned();
    let mut tracked = Vec::new();
    let mut additions = Vec::new();
    for entry in listing["entries"]
        .as_array()
        .ok_or_else(|| failed("Git status is missing entries"))?
    {
        let path = entry["path"]
            .as_str()
            .ok_or_else(|| failed("Git entry is missing a path"))?;
        let path = checked_review_path(workspace, path)?;
        if head.is_empty() || entry["kind"] == "untracked" {
            additions.push(path);
        } else {
            tracked.push(path);
        }
    }
    let mut patch = Vec::new();
    if !tracked.is_empty() {
        let mut args = vec![
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-renames".into(),
            "--unified=3".into(),
            head,
            "--".into(),
        ];
        args.extend(tracked.iter().cloned());
        patch = git_export(&root, args, &cancellation, false)?;
    }
    for path in &additions {
        // A staged addition removed from disk has no HEAD-to-worktree difference.
        match workspace.open_read(Path::new(path)) {
            Err(WorkspaceError::Io(error)) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(failed(error)),
            Ok(_) => {}
        }
        let next = git_export(
            &root,
            vec![
                "diff".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--no-index".into(),
                "--".into(),
                "/dev/null".into(),
                path.clone(),
            ],
            &cancellation,
            true,
        )?;
        if patch.len().saturating_add(next.len()) > MAX_PATCH_BYTES {
            return Err(failed("combined review patch exceeds the byte limit"));
        }
        patch.extend(next);
    }
    for path in tracked.iter().chain(&additions) {
        checked_review_path(workspace, path)?;
    }
    listing["patch"] = text(&patch)?.into();
    Ok(listing)
}

/// Complete, size-limited patch for one reviewed file. Never silently truncate.
pub fn review_patch(
    workspace: &Workspace,
    path: &str,
    staged: bool,
    untracked: bool,
) -> Result<String, ToolError> {
    let cancellation = CancellationToken::new();
    let root = repository_root(workspace.root(), &cancellation)?;
    let path = checked_review_path(workspace, path)?;
    let mut args = vec![
        "diff".into(),
        "--no-ext-diff".into(),
        "--no-textconv".into(),
        "--no-renames".into(),
        "--unified=3".into(),
    ];
    if untracked {
        let listing = status_inner(
            &root,
            workspace,
            &Arguments {
                path: Some(path.clone()),
                ..Arguments::default()
            },
            &cancellation,
            true,
        )?;
        if !listing["entries"]
            .as_array()
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry["path"] == path && entry["kind"] == "untracked")
            })
        {
            return Err(invalid(
                "the file is no longer untracked; refresh the review",
            ));
        }
        args.extend([
            "--no-index".into(),
            "--".into(),
            "/dev/null".into(),
            path.clone(),
        ]);
    } else {
        if staged {
            args.push("--cached".into());
        }
        args.extend(["--".into(), path.clone()]);
    }
    let patch = git_export(&root, args, &cancellation, untracked)?;
    checked_review_path(workspace, &path)?;
    String::from_utf8(patch).map_err(|_| failed("Git patch contains non-UTF-8 paths or content"))
}

/// Complete worktree patch, including untracked regular files.
/// Refuse oversized output instead of truncating a benchmark prediction.
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

        if patch
            .len()
            .saturating_add(addition.len())
            > MAX_PATCH_BYTES
        {
            return Err(ToolError::ExecutionFailed(format!(
                "Git patch exceeds the {} MiB export limit",
                MAX_PATCH_BYTES / (1_024 * 1_024)
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
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::READ
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "git_inspect",
            "Inspect the Git repository that exactly matches the workspace root, without running a \
                shell or changing the working tree. view=status (default) returns branch, HEAD \
                baseline and paginated changed paths. view=diff returns a bounded unified diff; \
                set staged=true for index-to-HEAD, otherwise working-tree-to-index. view=history \
                returns recent commits. Optional path is workspace-relative and is used only for \
                status/diff. Git hooks, external diff drivers, text conversion, user/global Git \
                configuration, and Git environment overrides are disabled. This tool is \
                read-only; it never stages, commits, resets, fetches or writes Git locks.",
            json!({
                "type": "object",
                "properties": {
                    "view": {
                        "type": "string",
                        "enum": ["status", "diff", "history"],
                        "default": "status"
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional workspace-relative path for status or diff"
                    },
                    "staged": {
                        "type": "boolean",
                        "default": false,
                        "description": "For diff: inspect staged changes instead of unstaged \
                            changes"
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 20_000,
                        "default": 0,
                        "description": "Entry offset for status/history, line offset for diff"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 500,
                        "default": 100,
                        "description": "Entries for status/history, lines for diff"
                    }
                },
                "required": [],
                "additionalProperties": false
            }),
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
            "Git repository root differs from the workspace root; start Orbit at the repository \
                root to inspect Git safely"
                .into(),
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
    status_inner(root, workspace, args, cancellation, false)
}

fn status_inner(
    root: &Path,
    workspace: &Workspace,
    args: &Arguments,
    cancellation: &CancellationToken,
    review: bool,
) -> Result<Value, ToolError> {
    let command = [
        "status",
        "--porcelain=v2",
        "--branch",
        "--untracked-files=all",
        "-z",
    ];
    let output = if review {
        // A desktop list must not report a truncated prefix as the complete final page.
        git_export(
            root,
            command
                .into_iter()
                .map(str::to_owned)
                .collect(),
            cancellation,
            false,
        )?
    } else {
        git(root, command, cancellation)?
    };
    let mut branch = json!({
        "head": null,
        "oid": null,
        "upstream": null,
        "ahead": 0,
        "behind": 0
    });
    let mut entries = Vec::new();
    let mut skipped = 0;
    let mut has_more = false;
    let path_filter = checked_path(workspace, args.path.as_deref())?;
    let records = output
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
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
        if review
            && ["path", "original_path"]
                .iter()
                .any(|key| {
                    entry
                        .get(key)
                        .and_then(Value::as_str)
                        .is_some_and(|path| checked_review_path(workspace, path).is_err())
                })
        {
            continue;
        }
        if path_filter
            .as_ref()
            .is_some_and(|filter| {
                let path = entry
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                !(path == filter || path.starts_with(&format!("{filter}/")))
            })
        {
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
        "scanned_entries": total_known
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
    let end = args
        .offset
        .saturating_add(args.limit)
        .min(lines.len());

    Ok(json!({
        "repository_root": root,
        "staged": args.staged,
        "path": args.path,
        "offset": args.offset,
        "lines": lines
            .get(args.offset..end)
            .unwrap_or_default(),
        "next_offset": (end < lines.len()).then_some(end),
        "has_more": end < lines.len(),
        "total_lines": lines.len(),
        "output_truncated": output.len() >= MAX_OUTPUT_BYTES
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
    let mut fields = output
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
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
            "subject": text(chunk[4])?
        }));
    }

    let has_more = records.len() > args.limit;

    Ok(json!({
        "repository_root": root,
        "commits": commits,
        "offset": args.offset,
        "next_offset": has_more.then_some(args.offset + commits.len()),
        "has_more": has_more
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

fn checked_review_path(workspace: &Workspace, path: &str) -> Result<String, ToolError> {
    let relative = workspace
        .checked_relative(Path::new(path))
        .map_err(failed)?;
    // Descriptor-based resolution checks protected inodes and symlink destinations as well
    // as the requested spelling. Deleted tracked files still have an inspectable Git blob.
    match workspace.open_read(&relative) {
        Ok(_) => {}
        Err(WorkspaceError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed(error)),
    }
    Ok(relative.to_string_lossy().into_owned())
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
        return Ok((
            Some(json!({
                "kind": "untracked",
                "path": rest
            })),
            0,
        ));
    }
    if let Some(rest) = record.strip_prefix("! ") {
        return Ok((
            Some(json!({
                "kind": "ignored",
                "path": rest
            })),
            0,
        ));
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
    let record_tag = fields.next();
    debug_assert_eq!(record_tag, Some(tag));
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
        let (score, path) = rest
            .split_once(' ')
            .unwrap_or((rest, ""));
        (Some(score), path)
    } else {
        (None, rest)
    };

    let kind = match tag {
        "1" => "changed",
        "2" => "renamed_or_copied",
        _ => "unmerged",
    };
    let mut entry = json!({
        "kind": kind,
        "xy": xy,
        "path": path
    });

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
        args.into_iter()
            .map(str::to_owned)
            .collect(),
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
    git_with_index(
        root,
        args,
        cancellation,
        limit,
        truncate,
        allows_difference_status,
        None,
        Instant::now() + GIT_TIMEOUT,
    )
}

#[allow(clippy::too_many_arguments)]
fn git_with_index(
    root: &Path,
    args: Vec<String>,
    cancellation: &CancellationToken,
    limit: usize,
    truncate: bool,
    allows_difference_status: bool,
    index: Option<&Path>,
    deadline: Instant,
) -> Result<Vec<u8>, ToolError> {
    git_with_objects(
        root,
        args,
        cancellation,
        limit,
        truncate,
        allows_difference_status,
        index,
        None,
        None,
        deadline,
    )
}

#[allow(clippy::too_many_arguments)]
/// --no-textconv does not disable clean/process filters. Enumerate configured driver names with a
/// non-converting Git builtin, then override every driver through argv. This also covers attributes
/// supplied by .git/info/attributes.
fn git_with_objects(
    root: &Path,
    args: Vec<String>,
    cancellation: &CancellationToken,
    limit: usize,
    truncate: bool,
    allows_difference_status: bool,
    index: Option<&Path>,
    objects: Option<&Path>,
    metadata: Option<&std::fs::File>,
    deadline: Instant,
) -> Result<Vec<u8>, ToolError> {
    let keys = run_git_index(
        root,
        vec![
            "config".into(),
            "--null".into(),
            "--name-only".into(),
            "--get-regexp".into(),
            r"^(filter\..*\.(clean|smudge|process|required)|remote\..*\.promisor|extensions\.partialclone)$".into(),
        ],
        cancellation,
        &BTreeSet::new(),
        GitLimits {
            bytes: MAX_OUTPUT_BYTES,
            truncate: false,
            // `git config` returns 1 when no key matches.
            allows_difference_status: true,
            deadline,
        },
        None,
        None,
        metadata,
    )?;
    let mut drivers = BTreeSet::new();
    for key in keys
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        let key = text(key)?;
        if !key.starts_with("filter.") {
            // Older Git versions may not understand GIT_NO_LAZY_FETCH. Refuse
            // partial repositories before any object-reading builtin can
            // invoke a configured transport or upload-pack executable.
            return Err(failed(
                "Partial/promisor repositories require external object fetching and cannot be \
                    inspected safely",
            ));
        }
        let (driver, _) = key
            .rsplit_once('.')
            .ok_or_else(|| failed("invalid Git filter key"))?;
        drivers.insert(driver.to_owned());
        if drivers.len() > 256 {
            return Err(failed(
                "Git repository configures more than 256 conversion filters",
            ));
        }
    }
    run_git_index(
        root,
        args,
        cancellation,
        &drivers,
        GitLimits {
            bytes: limit,
            truncate,
            allows_difference_status,
            deadline,
        },
        index,
        objects,
        metadata,
    )
}

struct GitLimits {
    bytes: usize,
    truncate: bool,
    allows_difference_status: bool,
    deadline: Instant,
}

#[cfg(test)]
fn run_git(
    root: &Path,
    args: Vec<String>,
    cancellation: &CancellationToken,
    drivers: &BTreeSet<String>,
    limits: GitLimits,
) -> Result<Vec<u8>, ToolError> {
    run_git_index(root, args, cancellation, drivers, limits, None, None, None)
}

#[allow(clippy::too_many_arguments)]
fn run_git_index(
    root: &Path,
    args: Vec<String>,
    cancellation: &CancellationToken,
    drivers: &BTreeSet<String>,
    limits: GitLimits,
    index: Option<&Path>,
    objects: Option<&Path>,
    metadata: Option<&std::fs::File>,
) -> Result<Vec<u8>, ToolError> {
    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled);
    }

    #[cfg(windows)]
    const NULL_DEVICE: &str = "NUL";
    #[cfg(unix)]
    const NULL_DEVICE: &str = "/dev/null";
    let mut command = Command::new("git");
    command
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| {
                if cfg!(windows) {
                    "".into()
                } else {
                    "/usr/bin:/bin".into()
                }
            }),
        )
        .env("LC_ALL", "C")
        .arg("--no-optional-locks")
        .arg("--no-pager")
        .arg("--literal-pathspecs")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg(format!("core.hooksPath={NULL_DEVICE}"))
        .arg("-c")
        .arg("diff.external=");
    for driver in drivers {
        for suffix in ["clean=", "smudge=", "process=", "required=false"] {
            command
                .arg("-c")
                .arg(format!("{driver}.{suffix}"));
        }
    }
    if let Some(metadata) = metadata {
        orbit_runtime::git::process::set_directory(&mut command, metadata).map_err(failed)?;
        // Index transactions supply captured blobs and an isolated index. Their builtins must
        // never consult the mutable worktree; both Git paths resolve from the pinned metadata.
        command
            .arg("--git-dir=.")
            .arg("--work-tree=.");
    } else {
        command.arg("-C").arg(root);
    }
    command
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    if let Some(objects) = objects {
        command.env("GIT_OBJECT_DIRECTORY", objects);
    }
    use orbit_runtime::git::process::{InspectionError, InspectionOutput, OutputLimit, inspect};
    let output = inspect(
        command,
        cancellation.clone(),
        limits.deadline,
        if limits.truncate {
            OutputLimit::Truncate(limits.bytes)
        } else {
            OutputLimit::Complete(limits.bytes)
        },
    )
    .map_err(|error| match error {
        InspectionError::Cancelled => ToolError::Cancelled,
        InspectionError::TimedOut => failed("Git inspection timed out after 10 seconds"),
        error => failed(error),
    })?;
    match output {
        InspectionOutput::Truncated(output) => Ok(output),
        InspectionOutput::Complete { stdout, status } => {
            if !status.success() && !(limits.allows_difference_status && status.code() == Some(1)) {
                return Err(failed(
                    "Git inspection failed; the workspace may not be a Git repository",
                ));
            }
            Ok(stdout)
        }
    }
}

fn text(bytes: &[u8]) -> Result<String, ToolError> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| {
            ToolError::ExecutionFailed("Git returned a non-UTF-8 path or metadata field".into())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(script: &str) -> Vec<String> {
        vec![
            "-c".into(),
            format!("alias.orbit-probe=!{script}"),
            "orbit-probe".into(),
        ]
    }

    #[test]
    fn git_deadline_stops_a_child_that_never_closes_stdout() {
        let directory = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let result = run_git(
            directory.path(),
            probe("exec sleep 30"),
            &CancellationToken::new(),
            &BTreeSet::new(),
            GitLimits {
                bytes: 1_024,
                truncate: false,
                allows_difference_status: false,
                deadline: started + Duration::from_millis(100),
            },
        );
        assert!(matches!(
            result,
            Err(ToolError::ExecutionFailed(error)) if error.contains("timed out")
        ));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn git_cancellation_stops_a_child_without_output() {
        let directory = tempfile::tempdir().unwrap();
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let marker = directory.path().join("started");
        let canceller = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !marker.exists() {
                assert!(Instant::now() < deadline, "Git probe never started");
                std::thread::sleep(Duration::from_millis(5));
            }
            token.cancel();
        });
        let started = Instant::now();
        let result = run_git(
            directory.path(),
            probe("printf started > started; exec sleep 30"),
            &cancellation,
            &BTreeSet::new(),
            GitLimits {
                bytes: 1_024,
                truncate: false,
                allows_difference_status: false,
                deadline: started + Duration::from_secs(3),
            },
        );
        canceller.join().unwrap();
        assert!(matches!(result, Err(ToolError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn git_output_limits_stop_the_process_and_preserve_strict_or_truncated_semantics() {
        let directory = tempfile::tempdir().unwrap();
        for truncate in [false, true] {
            let started = Instant::now();
            let result = run_git(
                directory.path(),
                probe("printf 123456789; exec sleep 30"),
                &CancellationToken::new(),
                &BTreeSet::new(),
                GitLimits {
                    bytes: 8,
                    truncate,
                    allows_difference_status: false,
                    deadline: started + Duration::from_secs(3),
                },
            );
            if truncate {
                assert_eq!(result.unwrap(), b"12345678");
            } else {
                assert!(matches!(
                    result,
                    Err(ToolError::ExecutionFailed(error)) if error.contains("8 byte")
                ));
            }
            assert!(started.elapsed() < Duration::from_secs(2));
        }
    }

    #[test]
    fn parses_rename_and_unmerged_porcelain_v2_records() {
        let (ordinary, consumed) = parse_status_entry(
            b"1 .M N... 100644 100644 100644 aaaaaaa bbbbbbb source/file with spaces.rs",
            None,
        )
        .unwrap();
        let ordinary = ordinary.unwrap();
        assert_eq!(consumed, 0);
        assert_eq!(ordinary["xy"], ".M");
        assert_eq!(ordinary["path"], "source/file with spaces.rs");
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
