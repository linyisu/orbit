use std::{io::Read, path::Path};

use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::filesystem::Workspace;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

mod pdf;
mod vision;
mod worker;

pub use vision::render_worker;

const MAX_FILE_BYTES: u64 = 256 * 1_024 * 1_024;
const DEFAULT_PAGE_CHARS: usize = 20_000;
const MAX_PAGE_CHARS: usize = 60_000;
const CHAR_INDEX_STRIDE: usize = 4_096;
const MAX_CACHE_ENTRIES: usize = 4;
const MAX_CACHE_BYTES: usize = 64 * 1_024 * 1_024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    #[serde(default)]
    start_char: usize,
    #[serde(default = "default_chars")]
    max_chars: usize,
    start_page: Option<u32>,
    end_page: Option<u32>,
    #[serde(default)]
    images: vision::ImageMode,
    image_start_page: Option<u32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Document {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pdf: Option<PdfPages>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PdfPages {
    start_page: u32,
    end_page: u32,
    total_pages: u32,
    pages_needing_ocr: Vec<u32>,
    has_encoding_issues: bool,
    extraction: String,
    #[serde(default)]
    partial: bool,
    #[serde(default)]
    pages: Vec<pdf::PageInfo>,
}

fn page_range(start: Option<u32>, end: Option<u32>) -> Result<(u32, u32), String> {
    let start = start.unwrap_or(1);
    let end = end.unwrap_or_else(|| start.saturating_add(19));
    if start == 0 || end < start || end - start >= 100 {
        return Err(
            "PDF pages are 1-based; end_page must be >= start_page, with at most 100 pages \
            per call"
                .into(),
        );
    }
    Ok((start, end))
}

fn default_chars() -> usize {
    DEFAULT_PAGE_CHARS
}

struct Converted {
    pdf: Option<PdfPages>,
    text: String,
    total_chars: usize,
    offsets: Vec<usize>,
}

impl Converted {
    fn new(document: Document) -> Self {
        let Document { text, pdf } = document;
        let mut offsets = Vec::new();
        let mut total_chars = 0;
        for (index, (offset, _)) in text.char_indices().enumerate() {
            if index % CHAR_INDEX_STRIDE == 0 {
                offsets.push(offset);
            }
            total_chars += 1;
        }

        Self {
            pdf,
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
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::READ
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "read_document",
            "Preferred tool for reading local PDFs and office documents: use this before shell \
                commands, pdftotext, Python packages or tool discovery. Reads documents up to 256 \
                MiB inside the workspace (or outside paths granted via request_capability). PDFs \
                use 1-based start_page/end_page (default first 20 pages, at most 100 per call); \
                extraction is restricted to that range. Supports PDF, Word, PowerPoint, Excel, \
                OpenDocument, RTF, EPUB and CSV. Use read_file for source code and ordinary text. \
                Mixed text/image PDFs return available text with physical page headings and \
                per-page status in pdf.pages. Image-only, empty and undecodable portions are \
                marked in place instead of failing the whole range. pdf.partial describes \
                incomplete TEXT extraction. With a vision model, images=auto attaches up to four \
                rendered physical pages containing images, undecodable text or no extracted text. \
                Read those image attachments directly. Use images=always for diagrams/layout \
                missed by text detection, or off for text only. pdf.vision lists attached pages, \
                failures, and next_image_start_page: continue with image_start_page in the SAME \
                page range until it is null. OCR is not performed; rendering alone does not mean \
                the page has been understood. Do not treat missing text or an unreadable fragment \
                as evidence that the original page lacks that information. Conversion is local \
                and creates no project files. Continue with next_start_char and the SAME page \
                range while truncated; then use next_start_page for the next PDF range. \
                start_char/max_chars paginate the converted text within that range.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path relative to the workspace root, or an outside path \
                            granted via request_capability"
                    },
                    "start_page": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "PDF only: first physical page (1-based), default 1"
                    },
                    "end_page": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "PDF only: last physical page inclusive, default start_page \
                            + 19; at most 100 pages"
                    },
                    "images": {
                        "type": "string",
                        "enum": ["auto", "always", "off"],
                        "default": "auto",
                        "description": "PDF page images: auto for images or missing/undecodable \
                            text; always for every requested page (including vector diagrams); \
                            off for text only. Requires vision model."
                    },
                    "image_start_page": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Resume page images using pdf.vision.next_image_start_page, \
                            keeping start_page/end_page unchanged; at most 4 images per call."
                    },
                    "start_char": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 0
                    },
                    "max_chars": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_PAGE_CHARS,
                        "default": DEFAULT_PAGE_CHARS
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        self.execute_with_context(
            call,
            cancellation,
            orbit_agent::tool::ToolContext {
                supports_vision: true,
            },
        )
    }

    fn execute_with_context<'a>(
        &'a self,
        call: ToolCall,
        cancellation: CancellationToken,
        context: orbit_agent::tool::ToolContext,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let arguments: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;

            if arguments.path.trim().is_empty()
                || !(1..=MAX_PAGE_CHARS).contains(&arguments.max_chars)
            {
                return Err(invalid(format!(
                    "path must be nonempty and max_chars must be between 1 and {MAX_PAGE_CHARS}"
                )));
            }

            page_range(arguments.start_page, arguments.end_page).map_err(invalid)?;

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
                "{}:{}:{}:{:?}:{:?}",
                orbit_runtime::execution::content_hash(&bytes),
                extension,
                worker_version,
                arguments.start_page,
                arguments.end_page,
            );
            let mut cache = tokio::select! {
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                guard = self.cache.lock() => guard,
            };
            let (format, markdown) = if let Some(index) = cache
                .iter()
                .position(|(k, _, _)| k == &key)
            {
                let entry = cache.remove(index).unwrap();
                let result = (entry.1.clone(), entry.2.clone());
                cache.push_back(entry);
                result
            } else {
                let format = anydoc::Format::from_bytes(&bytes)
                    .or_else(|| anydoc::Format::from_path(Path::new(&arguments.path)))
                    .ok_or_else(|| failed("unsupported document content and file extension"))?;
                if format != anydoc::Format::Pdf
                    && (arguments.start_page.is_some() || arguments.end_page.is_some())
                {
                    return Err(invalid(
                        "start_page/end_page are supported only for PDFs; use start_char/max_chars \
                            for other documents",
                    ));
                }
                let document = if let Some(worker) = &self.worker {
                    convert_worker(
                        worker,
                        &bytes,
                        &extension,
                        (arguments.start_page, arguments.end_page),
                        cancellation.clone(),
                    )
                    .await?
                } else {
                    // Only unit tests exercise the parser directly; production requires a worker.
                    #[cfg(test)]
                    {
                        convert(&bytes, format, arguments.start_page, arguments.end_page)
                            .map_err(failed)?
                    }

                    #[cfg(not(test))]
                    {
                        return Err(failed("document worker executable is unavailable"));
                    }
                };
                if document.text.len() > 64 * 1_024 * 1_024 {
                    return Err(failed("converted document exceeds 64 MiB"));
                }
                let format = format!("{format:?}").to_ascii_lowercase();
                let markdown = std::sync::Arc::new(Converted::new(document));
                while !cache.is_empty()
                    && (cache.len() >= MAX_CACHE_ENTRIES
                        || cache
                            .iter()
                            .map(|(_, _, text)| text.text.len())
                            .sum::<usize>()
                            + markdown.text.len()
                            > MAX_CACHE_BYTES)
                {
                    cache.pop_front();
                }

                cache.push_back((key, format.clone(), markdown.clone()));
                (format, markdown)
            };
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            drop(cache);
            let content = page(&arguments, &format, &markdown)?;
            vision::attach(
                self,
                &arguments,
                &bytes,
                markdown.pdf.as_ref(),
                content,
                context.supports_vision,
                cancellation,
            )
            .await
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
    let mut file = workspace
        .authorize_read(Path::new(path))
        .map_err(failed)?;
    let before = file.metadata().map_err(failed)?;
    if before.len() > MAX_FILE_BYTES {
        return Err(failed("document exceeds 256 MiB"));
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1_024];
    loop {
        if cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let count = file.read(&mut buffer).map_err(failed)?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > MAX_FILE_BYTES as usize {
            return Err(failed("document exceeds 256 MiB"));
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
    if !extension
        .bytes()
        .all(|b| b.is_ascii_alphanumeric())
    {
        return Err(invalid("invalid document extension"));
    }
    Ok((bytes, extension))
}

async fn convert_worker(
    worker: &Path,
    bytes: &[u8],
    extension: &str,
    pages: (Option<u32>, Option<u32>),
    cancellation: CancellationToken,
) -> Result<Document, ToolError> {
    let arguments = [pages.0, pages.1].map(|page| {
        page.map(|p| p.to_string())
            .unwrap_or_else(|| "-".into())
    });
    let temporary_output = worker::run(
        worker,
        bytes,
        extension,
        "--internal-document-worker",
        &arguments,
        cancellation,
    )
    .await?;
    let file = std::fs::File::open(
        temporary_output
            .path()
            .join("result.json"),
    )
    .map_err(failed)?;
    if file.metadata().map_err(failed)?.len() > 64 * 1_024 * 1_024 {
        return Err(failed("document worker output exceeds 64 MiB"));
    }
    serde_json::from_reader(file).map_err(failed)
}

/// Worker entry point for the host to invoke in a separate conversion process.
pub fn document_worker(
    input: &Path,
    output: &Path,
    start: Option<u32>,
    end: Option<u32>,
) -> Result<(), String> {
    if std::fs::metadata(input)
        .map_err(|e| e.to_string())?
        .len()
        > MAX_FILE_BYTES
    {
        return Err("document exceeds 256 MiB".into());
    }
    let bytes = std::fs::read(input).map_err(|e| e.to_string())?;
    let format = anydoc::Format::from_bytes(&bytes)
        .or_else(|| anydoc::Format::from_path(input))
        .ok_or("unsupported document format")?;
    let document = convert(&bytes, format, start, end)?;
    if document.text.len() > 64 * 1_024 * 1_024 {
        return Err("converted document exceeds 64 MiB".into());
    }
    let output_file = std::fs::File::create(output).map_err(|e| e.to_string())?;
    serde_json::to_writer(output_file, &document).map_err(|e| e.to_string())
}

fn convert(
    bytes: &[u8],
    format: anydoc::Format,
    start: Option<u32>,
    end: Option<u32>,
) -> Result<Document, String> {
    if format != anydoc::Format::Pdf {
        if start.is_some() || end.is_some() {
            return Err("page ranges are supported only for PDFs".into());
        }
        return anydoc::to_markdown_bytes(bytes, format)
            .map(|text| Document { text, pdf: None })
            .map_err(|e| e.to_string());
    }
    pdf::convert(bytes, start, end)
}

fn page(arguments: &Arguments, format: &str, markdown: &Converted) -> Result<String, ToolError> {
    let total_chars = markdown.total_chars;

    if arguments.start_char > total_chars {
        return Err(invalid(format!(
            "start_char exceeds converted document length ({total_chars} characters)"
        )));
    }

    let offset = markdown
        .offsets
        .get(arguments.start_char / CHAR_INDEX_STRIDE)
        .copied()
        .unwrap_or(markdown.text.len());
    let content: String = markdown.text[offset..]
        .chars()
        .skip(arguments.start_char % CHAR_INDEX_STRIDE)
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
        "pdf": markdown.pdf,
        "next_start_page": markdown
            .pdf
            .as_ref()
            .and_then(
                |pdf| (next >= total_chars && pdf.end_page < pdf.total_pages)
                    .then_some(pdf.end_page + 1)
            ),
        "source_kind": "local_document_markdown"
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    pub(super) fn pdf() -> Vec<u8> {
        pdf_fixture(false)
    }

    fn pdf_fixture(images: bool) -> Vec<u8> {
        let stream = |text: &str| {
            let text = format!("BT /F1 12 Tf 50 700 Td ({text}) Tj ET");
            format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len())
        };
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R \
                >> >> /Contents 6 0 R >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R \
                >> >> /Contents 7 0 R >>".into(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            stream("First chapter: compiler structure and syntax analysis."),
            stream("Second chapter: language grammar and parsing techniques."),
        ];
        if images {
            objects[2] =
                objects[2].replace("/Resources <<", "/Resources << /XObject << /Im 8 0 R >>");
            objects[3] =
                objects[3].replace("/Resources <<", "/Resources << /XObject << /Im 8 0 R >>");
            let drawing = "q 200 0 0 200 50 300 cm /Im Do Q";
            objects[5] = format!(
                "<< /Length {} >>\nstream\n{drawing}\nendstream",
                drawing.len()
            );
            let mixed = format!(
                "{drawing} BT /F1 12 Tf 50 700 Td (Second chapter: readable text beside an image.) \
                    Tj ET"
            );
            objects[6] = format!("<< /Length {} >>\nstream\n{mixed}\nendstream", mixed.len());
            objects.push(
                "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace \
                /DeviceRGB /BitsPerComponent 8 /Length 3 >>\nstream\nabc\nendstream"
                    .into(),
            );
        }
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn mixed_pdf_keeps_text_and_image_only_pages_do_not_fail() {
        let result = convert(&pdf_fixture(true), anydoc::Format::Pdf, Some(1), Some(2)).unwrap();
        assert!(
            result
                .text
                .contains("Second chapter: readable text beside an image.")
        );
        assert!(result.text.contains("## Page 1"));
        assert!(result.text.contains("## Page 2"));
        let metadata = serde_json::to_value(result.pdf.unwrap()).unwrap();
        assert_eq!(metadata["pages"][0]["status"], "image_only");
        assert_eq!(metadata["pages"][1]["status"], "text");
        assert_eq!(metadata["pages"][1]["has_images"], true);
        assert_eq!(metadata["pages_needing_ocr"], json!([1]));
        assert_eq!(metadata["has_encoding_issues"], false);
        let image_only =
            convert(&pdf_fixture(true), anydoc::Format::Pdf, Some(1), Some(1)).unwrap();
        assert!(
            image_only
                .text
                .contains("No selectable text")
        );
        assert!(
            !image_only
                .text
                .contains("Second chapter")
        );
    }

    #[test]
    fn pdf_ranges_extract_only_selected_pages_and_expose_continuation() {
        let first = convert(&pdf(), anydoc::Format::Pdf, Some(1), Some(1)).unwrap();
        assert!(first.text.contains("First chapter"));
        assert!(!first.text.contains("Second chapter"));
        assert_eq!(first.pdf.as_ref().unwrap().total_pages, 2);
        let converted = Converted::new(first);
        let args = |max_chars| Arguments {
            path: "book.pdf".into(),
            start_char: 0,
            max_chars,
            start_page: Some(1),
            end_page: Some(1),
            images: Default::default(),
            image_start_page: None,
        };
        let short: serde_json::Value =
            serde_json::from_str(&page(&args(5), "pdf", &converted).unwrap()).unwrap();
        assert!(short["next_start_page"].is_null());
        assert_eq!(short["next_start_char"], 5);
        let complete: serde_json::Value =
            serde_json::from_str(&page(&args(MAX_PAGE_CHARS), "pdf", &converted).unwrap()).unwrap();
        assert_eq!(complete["next_start_page"], 2);
        let second = convert(&pdf(), anydoc::Format::Pdf, Some(2), Some(20)).unwrap();
        assert!(second.text.contains("Second chapter"));
        assert!(!second.text.contains("First chapter"));
        assert_eq!(second.pdf.unwrap().end_page, 2);
        assert!(
            convert(&pdf(), anydoc::Format::Pdf, Some(3), Some(3))
                .unwrap_err()
                .contains("exceeds PDF length")
        );
    }

    #[test]
    fn large_sources_and_invalid_ranges_are_checked() {
        assert!(page_range(Some(0), None).is_err());
        assert!(page_range(Some(4), Some(3)).is_err());
        assert!(page_range(Some(1), Some(101)).is_err());
        assert_eq!(page_range(None, None).unwrap(), (1, 20));
        let dir = tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("large.pdf")).unwrap();
        file.set_len(33 * 1_024 * 1_024)
            .unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        assert_eq!(
            read_source(workspace.clone(), "large.pdf", CancellationToken::new())
                .unwrap()
                .0
                .len(),
            33 * 1_024 * 1_024
        );
        file.set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(
            read_source(workspace, "large.pdf", CancellationToken::new())
                .unwrap_err()
                .to_string()
                .contains("256 MiB")
        );
    }

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
            json!({
                "path": "data.csv",
                "max_chars": 12
            }),
        );
        let first: serde_json::Value = serde_json::from_str(
            &tool
                .execute(call, CancellationToken::new())
                .await
                .unwrap(),
        )
        .unwrap();

        assert_eq!(first["source_kind"], "local_document_markdown");
        assert_eq!(first["truncated"], true);
        assert!(
            first["next_start_char"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn indexed_pages_preserve_unicode_across_sparse_index_boundaries() {
        let document = Converted::new(Document {
            text: "中a".repeat(10_000),
            pdf: None,
        });
        for start in [0, 4_095, 4_096, 4_097, 19_990, 20_000] {
            let output: serde_json::Value = serde_json::from_str(
                &page(
                    &Arguments {
                        path: "sample.csv".into(),
                        start_char: start,
                        max_chars: 17,
                        start_page: None,
                        end_page: None,
                        images: Default::default(),
                        image_start_page: None,
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
