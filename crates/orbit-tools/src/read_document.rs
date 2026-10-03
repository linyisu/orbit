use std::{io::Read, path::Path};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use orbit_runtime::filesystem::Workspace;

const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const DEFAULT_CHARS: usize = 20_000;
const MAX_CHARS: usize = 60_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    #[serde(default)]
    start_char: usize,
    #[serde(default = "default_chars")]
    max_chars: usize,
}

fn default_chars() -> usize {
    DEFAULT_CHARS
}

const INDEX_STRIDE: usize = 4096;

struct Converted {
    text: String,
    total_chars: usize,
    offsets: Vec<usize>,
}
impl Converted {
    fn new(text: String) -> Self {
        let mut offsets = Vec::new();
        let mut total_chars = 0;
        for (index, (offset, _)) in text.char_indices().enumerate() {
            if index % INDEX_STRIDE == 0 {
                offsets.push(offset);
            }
            total_chars += 1;
        }
        Self {
            text,
            total_chars,
            offsets,
        }
    }
}

pub struct ReadDocument {
    workspace: Workspace,
    worker: Option<std::path::PathBuf>,
    cache:
        tokio::sync::Mutex<std::collections::VecDeque<(String, String, std::sync::Arc<Converted>)>>,
}

impl ReadDocument {
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            worker: None,
            cache: tokio::sync::Mutex::new(Default::default()),
        }
    }

    pub fn with_worker(mut self, worker: std::path::PathBuf) -> Self {
        self.worker = Some(worker);
        self
    }
}

impl Tool for ReadDocument {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "read_document",
            "Convert a document inside the workspace (or outside path granted via request_capability) to Markdown and read one Unicode-character page. Supports PDF, Word, PowerPoint, Excel, OpenDocument, RTF, EPUB and CSV. Use read_file for source code and ordinary text. Scanned/image-only PDFs require OCR and return an explicit error. Conversion is local and creates no project files. Continue with next_start_char while truncated.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the workspace root, or an outside path granted via request_capability" },
                    "start_char": { "type": "integer", "minimum": 0, "default": 0 },
                    "max_chars": { "type": "integer", "minimum": 1, "maximum": MAX_CHARS, "default": DEFAULT_CHARS }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let arguments: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;

            if arguments.path.trim().is_empty() || !(1..=MAX_CHARS).contains(&arguments.max_chars) {
                return Err(invalid(format!(
                    "path must be nonempty and max_chars must be between 1 and {MAX_CHARS}"
                )));
            }
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let workspace = self.workspace.clone();
            let token = cancellation.clone();
            let path = arguments.path.clone();
            let (bytes, extension) =
                tokio::task::spawn_blocking(move || read_source(workspace, &path, token))
                    .await
                    .map_err(failed)??;
            use std::os::unix::fs::MetadataExt;
            let worker_version = match &self.worker {
                Some(path) => {
                    let metadata = std::fs::metadata(path).map_err(failed)?;
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
                None => env!("CARGO_PKG_VERSION").to_owned(),
            };
            let key = format!(
                "{}:{}:{}",
                orbit_runtime::execution::content_hash(&bytes),
                extension,
                worker_version
            );
            let mut cache = tokio::select! {
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                guard = self.cache.lock() => guard,
            };
            let (format, markdown) =
                if let Some(index) = cache.iter().position(|(k, _, _)| k == &key) {
                    let entry = cache.remove(index).unwrap();
                    let result = (entry.1.clone(), entry.2.clone());
                    cache.push_back(entry);
                    result
                } else {
                    let format = anydoc::Format::from_bytes(&bytes)
                        .or_else(|| anydoc::Format::from_path(Path::new(&arguments.path)))
                        .ok_or_else(|| failed("unsupported document content and file extension"))?;
                    let text = if let Some(worker) = &self.worker {
                        convert_worker(worker, &bytes, &extension, cancellation.clone()).await?
                    } else {
                        // Only unit tests exercise the parser directly; production requires a worker.
                        #[cfg(test)]
                        {
                            anydoc::to_markdown_bytes(&bytes, format).map_err(failed)?
                        }
                        #[cfg(not(test))]
                        {
                            return Err(failed("document worker executable is unavailable"));
                        }
                    };
                    if text.len() > 64 * 1024 * 1024 {
                        return Err(failed("converted document exceeds 64 MiB"));
                    }
                    let format = format!("{format:?}").to_ascii_lowercase();
                    let markdown = std::sync::Arc::new(Converted::new(text));
                    while !cache.is_empty()
                        && (cache.len() >= 4
                            || cache
                                .iter()
                                .map(|(_, _, text)| text.text.len())
                                .sum::<usize>()
                                + markdown.text.len()
                                > 64 * 1024 * 1024)
                    {
                        cache.pop_front();
                    }
                    cache.push_back((key, format.clone(), markdown.clone()));
                    (format, markdown)
                };
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            page(arguments, &format, &markdown).map(ToolOutput::text)
        })
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "read_document".into(),
        message: message.into(),
    }
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

fn read_source(
    workspace: Workspace,
    path: &str,
    cancellation: CancellationToken,
) -> Result<(Vec<u8>, String), ToolError> {
    use std::os::unix::fs::MetadataExt;
    let mut file = workspace.authorize_read(Path::new(path)).map_err(failed)?;
    let before = file.metadata().map_err(failed)?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let count = file.read(&mut buffer).map_err(failed)?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > MAX_FILE_BYTES as usize {
            return Err(failed("document exceeds 32 MiB"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let after = file.metadata().map_err(failed)?;
    if (
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) {
        return Err(failed(
            "document changed during read; retry after writing finishes",
        ));
    }
    let extension = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !extension.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(invalid("invalid document extension"));
    }
    Ok((bytes, extension))
}

async fn convert_worker(
    worker: &Path,
    bytes: &[u8],
    extension: &str,
    cancellation: CancellationToken,
) -> Result<String, ToolError> {
    use orbit_runtime::execution::{
        BwrapBackend, ExecutionBackend, ExecutionEnvironment, ExecutionPolicy, ExecutionRequest,
        MountSpec, ResourceLimits,
    };
    let input = tempfile::tempdir().map_err(failed)?;
    let scratch = tempfile::tempdir().map_err(failed)?;
    std::fs::write(input.path().join(format!("source.{extension}")), bytes).map_err(failed)?;
    let request = ExecutionRequest::new(format!("/opt/orbit-document-worker --internal-document-worker /workspace/source.{extension} /scratch/document.md"), input.path(), std::time::Duration::from_secs(60)).with_scratch(scratch.path());
    let environment = ExecutionEnvironment::new(input.path(), input.path()).map_err(failed)?;
    let mut policy = ExecutionPolicy::read_only();
    policy.mounts = vec![MountSpec::read_only(worker, "/opt/orbit-document-worker")];
    policy.resources = ResourceLimits {
        max_memory_bytes: Some(512 * 1024 * 1024),
        max_file_size_bytes: Some(64 * 1024 * 1024),
        max_processes: Some(64),
        max_open_files: Some(128),
    };
    let result = BwrapBackend::new()
        .map_err(failed)?
        .execute(&request, &policy, &environment, None, cancellation)
        .await
        .map_err(failed)?;
    if result.cancelled {
        return Err(ToolError::Cancelled);
    }
    if result.timed_out {
        return Err(failed("document conversion timed out after 60 seconds"));
    }
    if result.exit_code != Some(0) {
        return Err(failed(format!(
            "document conversion failed: {}",
            result.stderr
        )));
    }
    std::fs::read_to_string(scratch.path().join("document.md")).map_err(failed)
}

/// Private CLI worker entry point, invoked only inside the conversion sandbox.
pub fn document_worker(input: &Path, output: &Path) -> Result<(), String> {
    let bytes = std::fs::read(input).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("document exceeds 32 MiB".into());
    }
    let format = anydoc::Format::from_bytes(&bytes)
        .or_else(|| anydoc::Format::from_path(input))
        .ok_or("unsupported document format")?;
    let markdown = anydoc::to_markdown_bytes(&bytes, format).map_err(|e| e.to_string())?;
    if markdown.len() > 64 * 1024 * 1024 {
        return Err("converted document exceeds 64 MiB".into());
    }
    std::fs::write(output, markdown).map_err(|e| e.to_string())
}

fn page(arguments: Arguments, format: &str, markdown: &Converted) -> Result<String, ToolError> {
    let total_chars = markdown.total_chars;

    if arguments.start_char > total_chars {
        return Err(invalid(format!(
            "start_char exceeds converted document length ({total_chars} characters)"
        )));
    }

    let offset = markdown
        .offsets
        .get(arguments.start_char / INDEX_STRIDE)
        .copied()
        .unwrap_or(markdown.text.len());
    let content: String = markdown.text[offset..]
        .chars()
        .skip(arguments.start_char % INDEX_STRIDE)
        .take(arguments.max_chars)
        .collect();
    let next = arguments.start_char + content.chars().count();

    Ok(json!({
        "path": arguments.path,
        "format": format,
        "content": content,
        "start_char": arguments.start_char,
        "total_chars": total_chars,
        "truncated": next < total_chars,
        "next_start_char": (next < total_chars).then_some(next),
        "source_kind": "local_document_markdown"
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn converts_and_pages_csv_without_writing_to_workspace() {
        let directory = tempdir().unwrap();
        std::fs::write(
            directory.path().join("data.csv"),
            "name,value\nalpha,1\nbeta,2\n",
        )
        .unwrap();
        let mut tool = ReadDocument::new(Workspace::new(directory.path()).unwrap());
        tool.worker = None;
        let call = ToolCall::function(
            "doc-1",
            "read_document",
            json!({ "path": "data.csv", "max_chars": 12 }),
        );
        let first: serde_json::Value =
            serde_json::from_str(&tool.execute(call, CancellationToken::new()).await.unwrap())
                .unwrap();

        assert_eq!(first["source_kind"], "local_document_markdown");
        assert_eq!(first["truncated"], true);
        assert!(first["next_start_char"].as_u64().unwrap() > 0);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    #[test]
    fn indexed_pages_preserve_unicode_across_sparse_index_boundaries() {
        let document = Converted::new("中a".repeat(10_000));
        for start in [0, 4095, 4096, 4097, 19990, 20000] {
            let output: serde_json::Value = serde_json::from_str(
                &page(
                    Arguments {
                        path: "sample.csv".into(),
                        start_char: start,
                        max_chars: 17,
                    },
                    "csv",
                    &document,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                output["content"],
                document
                    .text
                    .chars()
                    .skip(start)
                    .take(17)
                    .collect::<String>()
            );
            assert_eq!(output["total_chars"], 20_000);
        }
    }
}
