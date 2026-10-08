use std::{io::Read, path::Path};

use orbit_agent::tool::{ToolError, ToolOutput};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{Arguments, PdfPages, ReadDocument, failed, invalid, worker};

const MAX_PAGES: usize = 4;
const MAX_EDGE: f32 = 2048.0;

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ImageMode {
    #[default]
    Auto,
    Always,
    Off,
}

#[derive(Serialize, Deserialize)]
struct RenderedPage {
    page: u32,
    error: Option<String>,
}

fn selected_pages(pdf: &PdfPages, args: &Arguments) -> Result<(Vec<u32>, Option<u32>), ToolError> {
    let start = args.image_start_page.unwrap_or(pdf.start_page);
    if start < pdf.start_page || start > pdf.end_page {
        return Err(invalid(
            "image_start_page must be within the requested PDF page range",
        ));
    }
    let mut pages = pdf
        .pages
        .iter()
        .filter(|page| {
            page.page >= start && (args.images == ImageMode::Always || page.needs_image())
        })
        .map(|page| page.page);
    let selected = pages.by_ref().take(MAX_PAGES).collect();
    Ok((selected, pages.next()))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn attach(
    tool: &ReadDocument,
    args: &Arguments,
    bytes: &[u8],
    pdf: Option<&PdfPages>,
    content: String,
    supports_vision: bool,
    cancellation: CancellationToken,
) -> Result<ToolOutput, ToolError> {
    let Some(pdf) = pdf else {
        if args.image_start_page.is_some() || args.images != ImageMode::Auto {
            return Err(invalid(
                "images/image_start_page are supported only for PDFs",
            ));
        }
        return Ok(ToolOutput::text(content));
    };
    let mut value: serde_json::Value = serde_json::from_str(&content).map_err(failed)?;
    let mut attachments = Vec::new();
    let mut vision = json!({"attached_pages": [], "failures": [], "next_image_start_page": null});
    if args.images == ImageMode::Off {
        vision["status"] = json!("disabled");
    } else {
        let (selected, next) = selected_pages(pdf, args)?;
        vision["requested_pages"] = json!(selected);
        if selected.is_empty() {
            vision["status"] = json!("not_needed");
        } else if !supports_vision {
            vision["status"] = json!("unavailable");
            vision["warning"] = json!(
                "Current model cannot receive images, or image capability is disabled. Only extracted text is available; switch to a vision model to read these pages."
            );
        } else if let (Some(executable), Some(assets)) = (&tool.worker, &tool.images) {
            let pages_arg = selected
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let result = worker::run(
                executable,
                bytes,
                "pdf",
                "--internal-pdf-render-worker",
                &[pages_arg],
                cancellation.clone(),
            )
            .await;
            match result {
                Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
                Err(error) => {
                    vision["status"] = json!("failed");
                    vision["warning"] = json!(format!(
                        "Page rendering failed; text extraction is preserved: {error}"
                    ));
                    vision["failures"] = json!(
                        selected
                            .iter()
                            .map(|page| json!({"page":page,"error":"render worker failed"}))
                            .collect::<Vec<_>>()
                    );
                }
                Ok(temporary_output) => {
                    let assets = assets.clone();
                    let name = Path::new(&args.path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let selected_copy = selected.clone();
                    let imported = tokio::task::spawn_blocking(move || {
                        import_pages(temporary_output.path(), &selected_copy, &assets, &name)
                    })
                    .await
                    .map_err(failed)
                    .and_then(|result| result);
                    match imported {
                        Ok((images, failures)) => {
                            attachments = images;
                            vision["failures"] = json!(failures);
                            vision["attached_pages"] = json!(
                                selected
                                    .iter()
                                    .filter(|page| !failures
                                        .iter()
                                        .any(|failure| failure.page == **page))
                                    .collect::<Vec<_>>()
                            );
                            vision["status"] = json!(if attachments.is_empty() {
                                "failed"
                            } else {
                                "attached"
                            });
                        }
                        Err(error) => {
                            vision["status"] = json!("failed");
                            vision["warning"] = json!(format!(
                                "Could not load rendered pages; text extraction is preserved: {error}"
                            ));
                        }
                    }
                }
            }
            // Failed pages are explicit and may be retried; this cursor covers the remaining batch.
            vision["next_image_start_page"] = json!(next);
            vision["note"] = json!(
                "Attachments are full physical PDF pages in ascending page order, named with their page numbers. Inspect the actual images to read diagrams and missing text. Text extraction warnings do not mean attached images are unreadable. Follow next_image_start_page with the SAME start_page/end_page to load remaining pages."
            );
        } else {
            vision["status"] = json!("unavailable");
            vision["warning"] = json!(
                "PDF image worker or attachment store is unavailable. Only extracted text is available."
            );
        }
    }
    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    value["pdf"]["vision"] = vision;
    Ok(ToolOutput::with_attachments(value.to_string(), attachments))
}

fn import_pages(
    directory: &Path,
    selected: &[u32],
    assets: &crate::image_assets::ImageAssets,
    name: &str,
) -> Result<(Vec<orbit_model::types::Attachment>, Vec<RenderedPage>), ToolError> {
    let file = std::fs::File::open(directory.join("result.json")).map_err(failed)?;
    let results: Vec<RenderedPage> =
        serde_json::from_reader(file.take(64 * 1024)).map_err(failed)?;
    let mut images = Vec::new();
    let mut failures = Vec::new();
    for &page in selected {
        let result = results.iter().find(|result| result.page == page);
        let error = match result {
            Some(RenderedPage { error: None, .. }) => {
                let path = directory.join(format!("page-{page}.png"));
                // Authorize a fixed basename beneath the private temporary_output directory; never trust
                // output paths returned by a document parser, including symlinks.
                let loaded = orbit_runtime::filesystem::Workspace::new(directory)
                    .and_then(|workspace| workspace.authorize_read(&path))
                    .map_err(failed)
                    .and_then(|file| {
                        let mut bytes = Vec::new();
                        file.take(crate::image_assets::MAX_IMAGE_BYTES as u64 + 1)
                            .read_to_end(&mut bytes)
                            .map_err(failed)?;
                        assets
                            .import_bytes(format!("{name} — page {page}.png"), bytes)
                            .map_err(failed)
                    });
                match loaded {
                    Ok(image) => {
                        images.push(image);
                        None
                    }
                    Err(error) => Some(error.to_string()),
                }
            }
            Some(result) => result.error.clone(),
            None => Some("renderer did not return this page".into()),
        };
        if let Some(error) = error {
            failures.push(RenderedPage {
                page,
                error: Some(error),
            });
        }
    }
    Ok((images, failures))
}

/// Isolated native PDF renderer. Only explicitly requested pages are rasterized.
pub fn render_worker(input: &Path, output: &Path, pages: &str) -> Result<(), String> {
    let pages: Vec<u32> = pages
        .split(',')
        .map(|page| page.parse().map_err(|_| "invalid render page".to_owned()))
        .collect::<Result<_, _>>()?;
    if pages.is_empty()
        || pages.len() > MAX_PAGES
        || pages.contains(&0)
        || pages.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err("render pages must be 1–4 increasing positive page numbers".into());
    }
    let file = std::fs::File::open(input).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(super::MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > super::MAX_FILE_BYTES {
        return Err("document exceeds 256 MiB".into());
    }
    let pdf =
        hayro::hayro_syntax::Pdf::new(bytes).map_err(|e| format!("cannot render PDF: {e:?}"))?;
    let directory = output.parent().ok_or("missing output directory")?;
    let mut results = Vec::new();
    for number in pages {
        let rendered =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
                let page = pdf
                    .pages()
                    .get(number as usize - 1)
                    .ok_or("render page exceeds PDF length")?;
                let (width, height) = page.render_dimensions();
                if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                    return Err("invalid PDF page dimensions".into());
                }
                let scale = (MAX_EDGE / width.max(height)).min(3.0);
                if width * scale < 1.0 || height * scale < 1.0 {
                    return Err("unsupported PDF page aspect ratio".into());
                }
                let settings = hayro::PixmapSettings {
                    x_scale: scale,
                    y_scale: scale,
                    bg_color: hayro::vello_cpu::color::palette::css::WHITE,
                };
                // Bound retained decoded images and glyphs to one page at a time.
                let image = hayro::render(
                    page,
                    &hayro::RenderCache::new(),
                    &Default::default(),
                    &Default::default(),
                    &settings,
                );
                let png = image.into_png().map_err(|e| e.to_string())?;
                if png.len() > crate::image_assets::MAX_IMAGE_BYTES {
                    return Err("rendered page exceeds 20 MiB".into());
                }
                std::fs::write(directory.join(format!("page-{number}.png")), png)
                    .map_err(|e| e.to_string())
            }));
        let error = match rendered {
            Ok(result) => result.err(),
            Err(_) => Some("PDF renderer could not process this page".into()),
        };
        results.push(RenderedPage {
            page: number,
            error,
        });
    }
    serde_json::to_writer(
        std::fs::File::create(output).map_err(|e| e.to_string())?,
        &results,
    )
    .map_err(|e| e.to_string())
}
