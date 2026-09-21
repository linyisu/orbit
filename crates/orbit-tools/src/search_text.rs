use std::{
    io::Read,
    path::{Path, PathBuf},
};

use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::workspace::Workspace;

const MAX_VISITED: usize = 20_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SCANNED_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
const MAX_LINE_BYTES: usize = 4 * 1024;
const DEFAULT_CONTEXT_LINES: usize = 0;
const DEFAULT_OFFSET: usize = 0;
const DEFAULT_LIMIT: usize = 100;

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
            context_lines: DEFAULT_CONTEXT_LINES,
            offset: DEFAULT_OFFSET,
            limit: DEFAULT_LIMIT,
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
            "Search UTF-8 workspace files for query by regex or literal match without running grep in a shell. Paths are relative to the workspace root. Respects .gitignore, skips hidden files by default, skips binary/symlinks/large files (>2 MiB), and enforces read-only access with the same root protection as read_file. Default: case-insensitive literal substring. Output includes 1-based line/column, matching lines, and optional context_lines (0–10). Results are capped at 32 KiB, 500 matches, 20000 visited entries, and 16 MiB scanned content; narrow path, glob, or query if scan_truncated is true.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search pattern (literal substring or regex)" },
                    "path": { "type": "string", "default": ".", "description": "Subdirectory or file relative to workspace root" },
                    "regex": { "type": "boolean", "default": false, "description": "Interpret query as a regular expression" },
                    "case_sensitive": { "type": "boolean", "default": false },
                    "glob": { "type": "string", "description": "Optional glob pattern to filter relative paths (e.g. '*.rs')" },
                    "context_lines": { "type": "integer", "minimum": 0, "maximum": 10, "default": DEFAULT_CONTEXT_LINES },
                    "offset": { "type": "integer", "minimum": 0, "maximum": 20000, "default": DEFAULT_OFFSET },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": DEFAULT_LIMIT },
                    "include_hidden": { "type": "boolean", "default": false }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
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
        || args.path.trim().is_empty()
        || !(1..=500).contains(&args.limit)
        || args.context_lines > 10
        || args.offset > MAX_VISITED
    {
        return Err(invalid(
            "query and path must be nonempty; limit 1–500, context_lines 0–10, offset 0–20000",
        ));
    }

    Ok(())
}

fn search(
    workspace: Workspace,
    args: Arguments,
    token: CancellationToken,
) -> Result<String, ToolError> {
    let scope = scope(&workspace, &args.path)?;
    let matcher = compile_pattern(&args)?;
    let glob = compile_glob(args.glob.as_deref())?;

    let root = workspace.root().to_owned();
    let filter_workspace = workspace.clone();
    let filter_scope = scope.clone();

    let mut walker = WalkBuilder::new(&root);
    walker
        .hidden(!args.include_hidden)
        .parents(false)
        .follow_links(false)
        .max_filesize(Some(MAX_FILE_BYTES))
        .sort_by_file_name(|left, right| left.cmp(right))
        .filter_entry(move |entry| {
            let Ok(relative) = entry.path().strip_prefix(filter_workspace.root()) else {
                return false;
            };

            let within_scope = filter_scope.as_os_str().is_empty()
                || relative.starts_with(&filter_scope)
                || filter_scope.starts_with(relative);

            within_scope
                && (relative.as_os_str().is_empty()
                    || filter_workspace.checked_relative(relative).is_ok())
        });

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
            break;
        }
        visited += 1;

        let relative = match entry.path().strip_prefix(workspace.root()) {
            Ok(path) if workspace.checked_relative(path).is_ok() => path,
            _ => {
                skipped_files += 1;
                continue;
            }
        };
        if !scope.as_os_str().is_empty() && !relative.starts_with(&scope) {
            continue;
        }
        if glob
            .as_ref()
            .is_some_and(|matcher| !matcher.is_match(relative))
        {
            continue;
        }

        let mut file = match workspace.open_read(relative) {
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

                let entry = match_entry(relative, line_index, line, found.start(), &lines, &args);
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

fn scope(workspace: &Workspace, path: &str) -> Result<PathBuf, ToolError> {
    if Path::new(path)
        .components()
        .all(|component| component == std::path::Component::CurDir)
    {
        return Ok(PathBuf::new());
    }

    let relative = workspace
        .checked_relative(Path::new(path))
        .map_err(|error| invalid(error.to_string()))?;

    if workspace.open_directory(&relative).is_err() && workspace.open_read(&relative).is_err() {
        return Err(invalid(
            "path must name a readable workspace file or directory",
        ));
    }

    Ok(relative)
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
        .map(|index| {
            json!({
                "line": index + 1,
                "text": bounded_line(lines[index])
            })
        })
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
