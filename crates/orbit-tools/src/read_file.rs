use std::os::unix::fs::MetadataExt;
use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use orbit_agent::tool::{Tool, ToolError, ToolOutput};
use orbit_runtime::filesystem::Workspace;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::limits::{DEFAULT_FILE_SCAN_BYTES, MAX_TEXT_OUTPUT_BYTES};

const DEFAULT_PAGE_LINES: usize = 200;
const MAX_PAGE_LINES: usize = 500;

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    /// Path relative to the workspace root, or an outside path granted via RequestAccess.
    path: String,
    #[serde(default)]
    /// Pass next_cursor unchanged to resume this file version.
    #[schemars(with = "Cursor")]
    cursor: Option<Cursor>,
    #[serde(default = "first_line")]
    #[schemars(range(min = 1))]
    start_line: usize,
    #[serde(default)]
    start_char: usize,
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = MAX_PAGE_LINES))]
    max_lines: usize,
}

#[derive(serde::Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: String,
    offset: u64,
    #[schemars(range(min = 1))]
    line: usize,
    character: usize,
}

fn version(metadata: &std::fs::Metadata) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

fn first_line() -> usize {
    1
}

fn default_limit() -> usize {
    DEFAULT_PAGE_LINES
}

pub struct ReadFile {
    workspace: Workspace,
    scan_budget: u64,
}

impl ReadFile {
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            scan_budget: DEFAULT_FILE_SCAN_BYTES,
        }
    }

    pub fn with_scan_budget(mut self, bytes: u64) -> Self {
        self.scan_budget = bytes;
        self
    }
}

impl Tool for ReadFile {
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::READ
    }

    /// Read a UTF-8 file inside the workspace (or outside path granted via RequestAccess).
    /// Use a relative path or granted outside path, and optional 1-based start_line/max_lines.
    /// If a long line is truncated, resume with the returned next_start_line and
    /// next_start_char (Unicode character offset within that line). Returns JSON with numbered
    /// lines. Protected configuration files are rejected. Reads incrementally with a
    /// configurable scan budget per call (default 64 MiB) and 32 KiB content limit. Resume
    /// using next_cursor to avoid rescanning; cursors are rejected if the file changed.
    /// total_lines is null until EOF is reached.
    #[orbit_agent::tool(name = "ReadFile")]
    async fn execute(
        &self,
        arguments: ReadArguments,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        if arguments.path.trim().is_empty()
            || arguments.start_line == 0
            || !(1..=MAX_PAGE_LINES).contains(&arguments.max_lines)
        {
            return Err(invalid(
                "path must be nonempty, start_line >= 1, and max_lines between 1 and 500",
            ));
        }

        if cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        let workspace = self.workspace.clone();
        let token = cancellation.clone();
        let budget = self.scan_budget;
        let path = arguments.path.clone();
        let task = tokio::task::spawn_blocking(move || read(workspace, arguments, token, budget));

        tokio::select! {
            _ = cancellation.cancelled() => Err(ToolError::Cancelled),
            result = task => {
                result.map_err(|error| ToolError::ExecutionFailed(error.to_string()))?
            }
        }
        .map(|content| ToolOutput::text(content).with_read(path))
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "ReadFile".into(),
        message: message.into(),
    }
}

fn read(
    workspace: Workspace,
    arguments: ReadArguments,
    cancellation: CancellationToken,
    scan_budget: u64,
) -> Result<String, ToolError> {
    let mut file = workspace
        .authorize_read(Path::new(&arguments.path))
        .map_err(ToolError::from)?;
    let snapshot = version(&file.metadata().map_err(failed)?);
    let (mut line, mut character, offset) = if let Some(cursor) = &arguments.cursor {
        if cursor.version != snapshot
            || cursor.line == 0
            || cursor.offset > file.metadata().map_err(failed)?.len()
        {
            return Err(invalid(
                "cursor is invalid or file changed; read again without cursor",
            ));
        }

        (cursor.line, cursor.character, cursor.offset)
    } else {
        (1, 0, 0)
    };

    file.seek(SeekFrom::Start(offset))
        .map_err(failed)?;
    let mut reader = BufReader::new(file);
    let start_line = arguments
        .cursor
        .as_ref()
        .map_or(arguments.start_line, |c| c.line);
    let start_char = arguments
        .cursor
        .as_ref()
        .map_or(arguments.start_char, |c| c.character);
    let mut position = offset;
    let mut scanned = 0u64;
    let mut content = String::new();
    let mut end_line = start_line.saturating_sub(1);
    let mut prefix_written = false;
    let mut eof = false;
    loop {
        if cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if scan_budget != 0 && scanned >= scan_budget {
            return Err(failed(
                "read scan budget exceeded; request a nearer range or use a continuation cursor",
            ));
        }

        if reader
            .fill_buf()
            .map_err(failed)?
            .is_empty()
        {
            eof = true;
            break;
        }

        if line >= start_line.saturating_add(arguments.max_lines) {
            break;
        }
        let selected = line >= start_line && (line != start_line || character >= start_char);
        let prefix = if selected && !prefix_written {
            format!("{line}: ")
        } else {
            String::new()
        };
        // Reserve enough for any UTF-8 scalar before consuming it.
        if selected && MAX_TEXT_OUTPUT_BYTES - content.len() < prefix.len() + 4 {
            break;
        }
        let scalar = read_scalar(&mut reader)?;
        let bytes = scalar.len_utf8() as u64;
        scanned += bytes;
        position += bytes;
        if scalar == '\r'
            && reader
                .fill_buf()
                .map_err(failed)?
                .first()
                == Some(&b'\n')
        {
            continue;
        }
        if scalar == '\0' {
            return Err(failed("binary files are not supported"));
        }
        if selected {
            content.push_str(&prefix);
            prefix_written = true;
            end_line = line;
            if scalar == '\n' && content.ends_with('\r') {
                content.pop();
            }
            content.push(scalar);
        }

        if scalar == '\n' {
            if line == start_line && character < start_char {
                return Err(invalid("start_char exceeds the selected line length"));
            }
            line = line
                .checked_add(1)
                .ok_or_else(|| invalid("line offset overflow"))?;
            character = 0;
            prefix_written = false;
        } else {
            character = character
                .checked_add(1)
                .ok_or_else(|| invalid("character offset overflow"))?;
        }
    }

    let total_at_eof = if character == 0 { line - 1 } else { line };
    if eof && (start_line > total_at_eof.max(1) || line == start_line && character < start_char) {
        return Err(invalid("requested range exceeds file length"));
    }
    if eof && prefix_written && !content.ends_with('\n') && content.len() < MAX_TEXT_OUTPUT_BYTES {
        content.push('\n');
    }
    if snapshot
        != version(
            &reader
                .get_ref()
                .metadata()
                .map_err(failed)?,
        )
    {
        return Err(failed("file changed during read; retry"));
    }
    let next = (!eof).then_some(Cursor {
        version: snapshot,
        offset: position,
        line,
        character,
    });
    let total_lines = eof.then_some(if character == 0 { line - 1 } else { line });
    Ok(json!({
        "path": arguments.path,
        "start_line": start_line,
        "start_char": start_char,
        "end_line": end_line,
        "total_lines": total_lines,
        "truncated": !eof,
        "next_start_line": next.as_ref().map(|c| c.line),
        "next_start_char": next.as_ref().map(|c| c.character),
        "next_cursor": next,
        "content": content
    })
    .to_string())
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

fn read_scalar(reader: &mut impl Read) -> Result<char, ToolError> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes[..1])
        .map_err(failed)?;
    let length = match bytes[0] {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Err(failed("file is not UTF-8 text")),
    };

    reader
        .read_exact(&mut bytes[1..length])
        .map_err(failed)?;
    std::str::from_utf8(&bytes[..length])
        .map_err(|_| failed("file is not UTF-8 text"))?
        .chars()
        .next()
        .ok_or_else(|| failed("invalid UTF-8 scalar"))
}
