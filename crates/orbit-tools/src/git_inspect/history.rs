//! Complete, bounded Git history reads using the existing inspection process policy.
use super::*;

const HISTORY_PAGE_COMMITS: usize = 50;
const REVISION_BYTES: usize = 1_024;
const HISTORY_BRANCHES: usize = 500;

fn revision(root: &Path, value: &str, token: &CancellationToken) -> Result<String, ToolError> {
    if value.is_empty() || value.len() > REVISION_BYTES || value.contains(['\0', '\n', '\r']) {
        return Err(invalid("invalid Git revision"));
    }
    let output = git_owned(
        root,
        vec![
            "rev-parse".into(),
            "--verify".into(),
            "--end-of-options".into(),
            format!("{value}^{{commit}}"),
        ],
        token,
    )?;
    let id = text(&output)?.trim().to_owned();
    if !matches!(id.len(), 40 | 64)
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(failed("Git returned an invalid commit identity"));
    }
    Ok(id)
}

fn git_path(path: &str) -> String {
    if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    }
}

fn commits(
    root: &Path,
    id: &str,
    offset: usize,
    token: &CancellationToken,
) -> Result<Vec<Value>, ToolError> {
    let bytes = git_export(
        root,
        vec![
            "log".into(),
            "--no-show-signature".into(),
            format!("--max-count={}", HISTORY_PAGE_COMMITS + 1),
            format!("--skip={offset}"),
            "--format=%H%x00%h%x00%aI%x00%an%x00%s".into(),
            "-z".into(),
            id.into(),
            "--".into(),
        ],
        token,
        false,
    )?;
    let mut fields: Vec<_> = bytes.split(|byte| *byte == 0).collect();
    fields.pop();
    let (records, remainder) = fields.as_chunks::<5>();
    if !remainder.is_empty() {
        return Err(failed("Git returned incomplete history records"));
    }
    records
        .iter()
        .map(|record| {
            Ok(json!({
                "id": text(record[0])?,
                "short_id": text(record[1])?,
                "authored_at": text(record[2])?,
                "author": text(record[3])?,
                "subject": text(record[4])?
            }))
        })
        .collect()
}

fn publication(
    root: &Path,
    id: &str,
    count: usize,
    entries: &mut [Value],
    token: &CancellationToken,
) -> Result<(), ToolError> {
    let refs = git_export(
        root,
        vec![
            "for-each-ref".into(),
            "--count=1".into(),
            "--format=%(objectname)".into(),
            "refs/remotes".into(),
        ],
        token,
        false,
    )?;
    if refs.is_empty() {
        return Ok(());
    }
    let local = git_export(
        root,
        vec![
            "rev-list".into(),
            format!("--max-count={count}"),
            id.into(),
            "--not".into(),
            "--remotes".into(),
            "--".into(),
        ],
        token,
        false,
    )?;
    let local = text(&local)?;
    let ids = local
        .lines()
        .collect::<std::collections::HashSet<_>>();
    for entry in entries {
        entry["publication"] = if ids.contains(entry["id"].as_str().unwrap_or_default()) {
            "unpushed"
        } else {
            "published"
        }
        .into();
    }
    Ok(())
}

/// Pin the requested branch to an object ID so later pages cannot move with new commits.
pub fn list(workspace: &Workspace, requested: &str, offset: usize) -> Result<Value, ToolError> {
    if offset > MAX_OFFSET {
        return Err(invalid("history offset exceeds the inspection limit"));
    }
    let token = CancellationToken::new();
    let root = repository_root(workspace.root(), &token)?;
    let id = revision(&root, requested, &token)?;
    let mut entries = commits(&root, &id, offset, &token)?;
    let more = entries.len() > HISTORY_PAGE_COMMITS;
    entries.truncate(HISTORY_PAGE_COMMITS);
    publication(
        &root,
        &id,
        offset + HISTORY_PAGE_COMMITS,
        &mut entries,
        &token,
    )?;
    let branches = git_export(
        &root,
        vec![
            "for-each-ref".into(),
            format!("--count={HISTORY_BRANCHES}"),
            "--format=%(refname:short)".into(),
            "refs/heads".into(),
            "refs/remotes".into(),
        ],
        &token,
        false,
    )?;
    Ok(json!({
        "revision": id,
        "next": more.then_some(offset + entries.len()),
        "commits": entries,
        "branches": text(&branches)?.lines().collect::<Vec<_>>()
    }))
}

fn parent(root: &Path, id: &str, token: &CancellationToken) -> Result<Option<String>, ToolError> {
    let output = git(root, ["rev-list", "--parents", "-n", "1", id, "--"], token)?;
    Ok(text(&output)?
        .split_whitespace()
        .nth(1)
        .map(str::to_owned))
}

fn diff_args(id: &str, parent: Option<String>, patch: bool) -> Vec<String> {
    let mut args = if let Some(parent) = parent {
        vec!["diff".into(), parent, id.into()]
    } else {
        vec![
            "diff-tree".into(),
            "--root".into(),
            "--no-commit-id".into(),
            "-r".into(),
            id.into(),
        ]
    };
    args.extend([
        "--no-ext-diff".into(),
        "--no-textconv".into(),
        "--no-renames".into(),
    ]);
    if patch {
        args.push("--patch".into());
    } else {
        args.extend(["--name-only".into(), "-z".into()]);
    }
    args.push("--".into());
    args
}

/// Read metadata and paths for the authorized repository root, including merge commits.
pub fn detail(workspace: &Workspace, requested: &str) -> Result<Value, ToolError> {
    let token = CancellationToken::new();
    let root = repository_root(workspace.root(), &token)?;
    let id = revision(&root, requested, &token)?;
    let mut commit = commits(&root, &id, 0, &token)?
        .into_iter()
        .next()
        .ok_or_else(|| failed("commit not found"))?;
    publication(&root, &id, 1, std::slice::from_mut(&mut commit), &token)?;
    let message = git_export(
        &root,
        vec![
            "show".into(),
            "--no-show-signature".into(),
            "-s".into(),
            "--format=%B".into(),
            id.clone(),
            "--".into(),
        ],
        &token,
        false,
    )?;
    let mut args = diff_args(&id, parent(&root, &id, &token)?, false);
    args.push(".".into());
    let paths = git_export(&root, args, &token, false)?;
    let mut files = Vec::new();
    for path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = text(path)?;
        let path = path.as_str();
        // Validate lexical scope; historical blobs never follow current symlinks.
        workspace
            .checked_relative(Path::new(path))
            .map_err(failed)?;
        files.push(path.to_owned());
        if files.len() > MAX_ENTRIES {
            return Err(failed("commit exceeds the file inspection limit"));
        }
    }
    Ok(json!({
        "commit": commit,
        "message": text(&message)?,
        "files": files
    }))
}

/// Read historical content and its first-parent patch without applying worktree filters.
pub fn file(workspace: &Workspace, requested: &str, path: &str) -> Result<Value, ToolError> {
    let relative = checked_review_path(workspace, path)?;
    let token = CancellationToken::new();
    let root = repository_root(workspace.root(), &token)?;
    let id = revision(&root, requested, &token)?;
    let relative = git_path(&relative);
    let target = relative.clone();
    let mut args = diff_args(&id, parent(&root, &id, &token)?, true);
    args.push(target.clone());
    let patch = git_export(&root, args, &token, false)?;
    let tree = git_export(
        &root,
        vec![
            "ls-tree".into(),
            "-z".into(),
            id.clone(),
            "--".into(),
            target.clone(),
        ],
        &token,
        false,
    )?;
    let content = if tree.is_empty() {
        None
    } else {
        let metadata = tree
            .split(|byte| *byte == b'\t')
            .next()
            .unwrap_or_default();
        if !text(metadata)?
            .split_whitespace()
            .nth(1)
            .is_some_and(|kind| kind == "blob")
        {
            return Err(invalid("selected Git entry is not a file"));
        }
        let bytes = git_export(
            &root,
            vec!["cat-file".into(), "blob".into(), format!("{id}:{target}")],
            &token,
            false,
        )?;
        if bytes.contains(&0) {
            None
        } else {
            String::from_utf8(bytes).ok()
        }
    };
    Ok(json!({
        "path": relative,
        "patch": text(&patch)?,
        "content": content
    }))
}
