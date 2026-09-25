use std::{
    io::Read,
    path::{Path, PathBuf},
};

use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use orbit_runtime::filesystem::{Workspace, WorkspaceError};

const MAX_VISITED: usize = 20_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SCANNED_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
const MAX_LINE_BYTES: usize = 4 * 1024;
const MAX_QUERY_BYTES: usize = 4096;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Arguments {
    query: String,
    path: String,
    regex: bool,
    case_sensitive: bool,
    glob: Option<String>,
    context_lines: usize,
    offset: usize,
    limit: usize,
    include_hidden: bool,
}

impl Default for Arguments {
    fn default() -> Self {
        Self {
            query: String::new(),
            path: ".".into(),
            regex: false,
            case_sensitive: false,
            glob: None,
            context_lines: 0,
            offset: 0,
            limit: 100,
            include_hidden: false,
        }
    }
}

pub struct SearchText {
    workspace: Workspace,
}

impl SearchText {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

impl Tool for SearchText {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "search_text",
            concat!(
                "Search UTF-8 text files in the workspace (or outside paths granted via request_capability) without running a shell. ",
                "IMPORTANT: regex defaults to false. With regex=false or omitted, the entire query is literal: ",
                "a|b searches for the actual text a|b, NOT either a or b. ",
                "To search alternatives, send {\"query\":\"a|b\",\"regex\":true}; ",
                "to find the literal pipe expression, send {\"query\":\"a|b\",\"regex\":false}. ",
                "Set regex=true explicitly whenever using alternation, character classes, anchors or other regex syntax. ",
                "Regex uses Rust regex syntax and matches one line at a time; lookaround and backreferences are unsupported. ",
                "Both modes are case-insensitive unless case_sensitive=true. ",
                "If a query returns no matches, check the returned regex mode and query before concluding the text is absent. ",
                "Uses .gitignore and .ignore rules, never follows symlinks, and excludes protected metadata such as .git and .env. ",
                "Optionally narrow with a workspace-relative path, outside granted path, or glob such as **/*.rs. ",
                "Results have path, 1-based line/column, text and surrounding lines. Paginate matches with offset/next_offset. ",
                "Large, binary and unreadable files are skipped; use read_file for full content after locating a match."
            ),
            json!({"type":"object","properties":{
                "query":{"type":"string","minLength":1,"maxLength":4096,"description":"Search text. Literal unless regex=true: a|b means the literal pipe expression by default. For either term, use query=a|b AND regex=true. Do not add shell quotes or /regex/ delimiters."},
                "path":{"type":"string","default":".","description":"Workspace-relative directory or file, or outside path granted via request_capability"},
                "regex":{"type":"boolean","default":false,"description":"false or omitted: exact literal substring matching, including | . * [ ] ^ $. true: interpret query as a Rust regular expression. REQUIRED for alternatives such as foo|bar; never inferred from query content."},
                "case_sensitive":{"type":"boolean","default":false},
                "glob":{"type":"string","description":"Optional glob filter, for example **/*.rs"},
                "context_lines":{"type":"integer","minimum":0,"maximum":10,"default":0},
                "offset":{"type":"integer","minimum":0,"maximum":20000,"default":0},
                "limit":{"type":"integer","minimum":1,"maximum":500,"default":100},
                "include_hidden":{"type":"boolean","default":false}
            },"required":["query"],"additionalProperties":false}),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let arguments: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;
            validate(&arguments)?;
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let workspace = self.workspace.clone();
            let token = cancellation.clone();
            let task = tokio::task::spawn_blocking(move || search(workspace, arguments, token));
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
        tool: "search_text".into(),
        message: message.into(),
    }
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

fn validate(args: &Arguments) -> Result<(), ToolError> {
    if args.query.is_empty()
        || args.query.len() > MAX_QUERY_BYTES
        || args.path.trim().is_empty()
        || !(1..=500).contains(&args.limit)
        || args.context_lines > 10
        || args.offset > MAX_VISITED
    {
        return Err(invalid(
            "query must be 1–4096 bytes, path nonempty; limit 1–500, context_lines 0–10, offset 0–20000",
        ));
    }
    Ok(())
}

enum SearchScope {
    Workspace(PathBuf),
    External(PathBuf),
}

fn search(
    workspace: Workspace,
    args: Arguments,
    token: CancellationToken,
) -> Result<String, ToolError> {
    let search_scope = scope(&workspace, &args.path)?;
    let matcher = compile_pattern(&args)?;
    let glob = compile_glob(args.glob.as_deref())?;

    let (root, filter_scope, is_external) = match search_scope {
        SearchScope::Workspace(scope) => (workspace.root().to_owned(), scope, false),
        SearchScope::External(ext_root) => (ext_root, PathBuf::new(), true),
    };

    let filter_workspace = workspace.clone();
    let filter_scope_clone = filter_scope.clone();
    let mut walker = WalkBuilder::new(&root);
    walker
        .hidden(!args.include_hidden)
        .parents(false)
        .follow_links(false)
        .max_filesize(Some(MAX_FILE_BYTES))
        .sort_by_file_name(|left, right| left.cmp(right));

    if !is_external {
        walker.filter_entry(move |entry| {
            let Ok(relative) = entry.path().strip_prefix(filter_workspace.root()) else {
                return false;
            };
            let within_scope = filter_scope_clone.as_os_str().is_empty()
                || relative.starts_with(&filter_scope_clone)
                || filter_scope_clone.starts_with(relative);
            within_scope
                && (relative.as_os_str().is_empty()
                    || filter_workspace.checked_relative(relative).is_ok())
        });
    }

    let mut matches = Vec::new();
    let mut matched = 0;
    let mut visited = 0;
    let mut scanned_files = 0;
    let mut skipped_files = 0;
    let mut scanned_bytes = 0_u64;
    let mut output_bytes = 0;
    let mut has_more = false;
    let mut scan_truncated = false;

    'files: for entry in walker.build() {
        if token.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                skipped_files += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if visited >= MAX_VISITED {
            scan_truncated = true;
            break 'files;
        }
        visited += 1;

        let (relative_or_path, file_result) = if is_external {
            let path = entry.path().to_path_buf();
            let file = workspace.authorize_read(&path);
            (path, file)
        } else {
            let relative = match entry.path().strip_prefix(workspace.root()) {
                Ok(path) if workspace.checked_relative(path).is_ok() => path.to_path_buf(),
                _ => {
                    skipped_files += 1;
                    continue;
                }
            };
            if !filter_scope.as_os_str().is_empty() && !relative.starts_with(&filter_scope) {
                continue;
            }
            let file = workspace.open_read(&relative);
            (relative, file)
        };

        if glob
            .as_ref()
            .is_some_and(|matcher| !matcher.is_match(&relative_or_path))
        {
            continue;
        }

        let mut file = match file_result {
            Ok(file) => file,
            Err(_) => {
                skipped_files += 1;
                continue;
            }
        };
        let file_size = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => {
                skipped_files += 1;
                continue;
            }
        };
        if file_size > MAX_FILE_BYTES || scanned_bytes.saturating_add(file_size) > MAX_SCANNED_BYTES
        {
            scan_truncated = true;
            break;
        }
        let mut bytes = Vec::with_capacity(file_size as usize);
        if file.read_to_end(&mut bytes).is_err() {
            skipped_files += 1;
            continue;
        }
        scanned_bytes += bytes.len() as u64;
        scanned_files += 1;
        let text = match String::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => text,
            _ => {
                skipped_files += 1;
                continue;
            }
        };
        let lines: Vec<_> = text.lines().collect();
        for (line_index, line) in lines.iter().enumerate() {
            if token.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            for found in matcher.find_iter(line) {
                matched += 1;
                if matched <= args.offset {
                    continue;
                }
                let entry = match_entry(
                    &relative_or_path,
                    line_index,
                    line,
                    found.start(),
                    &lines,
                    &args,
                );
                let size = entry.to_string().len() + 1;
                if matches.len() == args.limit || output_bytes + size > MAX_OUTPUT_BYTES - 1024 {
                    has_more = true;
                    break 'files;
                }
                output_bytes += size;
                matches.push(entry);
            }
        }
    }

    Ok(json!({
        "query": args.query,
        "path": args.path,
        "regex": args.regex,
        "case_sensitive": args.case_sensitive,
        "matches": matches,
        "next_offset": has_more.then_some(args.offset + matches.len()),
        "has_more": has_more,
        "scan_truncated": scan_truncated,
        "scanned_files": scanned_files,
        "skipped_files": skipped_files,
    })
    .to_string())
}

fn scope(workspace: &Workspace, path: &str) -> Result<SearchScope, ToolError> {
    let target_path = Path::new(path);
    if target_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(invalid("use a workspace-relative path without '..'"));
    }

    if target_path
        .components()
        .all(|component| component == std::path::Component::CurDir)
    {
        return Ok(SearchScope::Workspace(PathBuf::new()));
    }

    match workspace.checked_relative(target_path) {
        Ok(relative) => {
            if workspace.open_directory(&relative).is_err()
                && workspace.open_read(&relative).is_err()
            {
                return Err(invalid(
                    "path must name a readable workspace file or directory",
                ));
            }
            Ok(SearchScope::Workspace(relative))
        }
        Err(WorkspaceError::ProtectedPath) => {
            Err(invalid(WorkspaceError::ProtectedPath.to_string()))
        }
        Err(_) => {
            let canonical = target_path.canonicalize().map_err(failed)?;
            match workspace.authorize_directory(&canonical) {
                Ok(_) => Ok(SearchScope::External(canonical)),
                Err(WorkspaceError::NotDirectory) => match workspace.authorize_read(&canonical) {
                    Ok(_) => Ok(SearchScope::External(canonical)),
                    Err(WorkspaceError::PermissionDenied(err)) => Err(failed(err)),
                    Err(_) => Err(invalid("path must name a readable file or directory")),
                },
                Err(WorkspaceError::PermissionDenied(err)) => Err(failed(err)),
                Err(_) => Err(invalid("path must name a readable file or directory")),
            }
        }
    }
}

fn compile_pattern(args: &Arguments) -> Result<Regex, ToolError> {
    let pattern = if args.regex {
        args.query.clone()
    } else {
        regex::escape(&args.query)
    };
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(!args.case_sensitive)
        .build()
        .map_err(|error| invalid(format!("invalid search pattern: {error}")))?;
    if regex.is_match("") {
        return Err(invalid("search pattern must not match an empty string"));
    }
    Ok(regex)
}

fn compile_glob(value: Option<&str>) -> Result<Option<GlobMatcher>, ToolError> {
    value
        .map(|value| {
            Glob::new(value)
                .map(|glob| glob.compile_matcher())
                .map_err(|error| invalid(format!("invalid glob: {error}")))
        })
        .transpose()
}

fn match_entry(
    path: &Path,
    line_index: usize,
    line: &str,
    byte_column: usize,
    lines: &[&str],
    args: &Arguments,
) -> Value {
    let start = line_index.saturating_sub(args.context_lines);
    let end = (line_index + args.context_lines + 1).min(lines.len());
    let surrounding = (start..end)
        .filter(|index| *index != line_index)
        .map(|index| json!({"line":index + 1,"text":bounded_line(lines[index])}))
        .collect::<Vec<_>>();
    json!({
        "path": path.to_string_lossy(),
        "line": line_index + 1,
        "column": line[..byte_column].chars().count() + 1,
        "text": bounded_line(line),
        "context": surrounding,
    })
}

fn bounded_line(text: &str) -> String {
    if text.len() <= MAX_LINE_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_LINE_BYTES.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
