use std::collections::BTreeMap;

use pdf_inspector::types::{ItemType, TextItem};
use serde::{Deserialize, Serialize};

use super::{Document, PdfPages, page_range};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct PageInfo {
    pub(super) page: u32,
    status: Status,
    has_images: bool,
    unreadable_fragments: usize,
}

impl PageInfo {
    pub(super) fn needs_image(&self) -> bool {
        self.has_images || self.status != Status::Text
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Text,
    Partial,
    Unreadable,
    ImageOnly,
    NoText,
}

pub(super) fn convert(
    bytes: &[u8],
    start: Option<u32>,
    end: Option<u32>,
) -> Result<Document, String> {
    let (start, end) = page_range(start, end)?;
    // Classification provides metadata only. It must not suppress a mixed document's
    // usable text or turn one scanned/undecodable page into a range-wide failure.
    let info = pdf_inspector::process_pdf_mem_with_options(
        bytes,
        pdf_inspector::PdfOptions::detect_only().detection(pdf_inspector::DetectionConfig {
            strategy: pdf_inspector::ScanStrategy::Pages((start..=end).collect()),
            ..Default::default()
        }),
    )
    .map_err(|e| e.to_string())?;
    if start > info.page_count {
        return Err(format!(
            "start_page exceeds PDF length ({} pages)",
            info.page_count
        ));
    }
    let end = end.min(info.page_count);
    let selected = (start..=end).collect();
    let items = pdf_inspector::extract_text_with_positions_mem_in_frame(
        bytes,
        Some(&selected),
        pdf_inspector::PositionFrame::Sheet,
    )
    .map_err(|e| e.to_string())?;
    Ok(render_pages(items, start, end, info.page_count))
}

fn render_pages(items: Vec<TextItem>, start: u32, end: u32, total: u32) -> Document {
    let mut grouped: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
    for item in items {
        if (start..=end).contains(&item.page) {
            grouped
                .entry(item.page)
                .or_default()
                .push(item);
        }
    }
    let mut text = String::new();
    let mut pages = Vec::new();
    let mut pages_needing_ocr = Vec::new();
    let mut has_encoding_issues = false;
    let mut partial = false;
    for number in start..=end {
        let mut items = grouped
            .remove(&number)
            .unwrap_or_default();
        let has_images = items
            .iter()
            .any(|item| matches!(item.item_type, ItemType::Image));
        let mut unreadable_fragments = 0;
        let mut text_fragments = 0;
        let mut readable = false;
        for item in &mut items {
            if matches!(item.item_type, ItemType::Text) {
                if !item.text.trim().is_empty() {
                    text_fragments += 1;
                }
                if item.text.contains('\u{fffd}') {
                    // Keep the gap visible at its original location instead of silently
                    // joining the surrounding text into a seemingly complete sentence.
                    unreadable_fragments += 1;
                    item.text = "[Unreadable text fragment]".into();
                } else if !item.text.trim().is_empty() {
                    readable = true;
                }
            }
        }
        // A few surviving glyphs are not reliable text when the surrounding font
        // decoding has failed. Suppress the whole damaged text layer, including
        // plausible-looking garbage, and let vision read the original page.
        let omit_text = unreadable_fragments > 0
            && (!readable
                || (unreadable_fragments >= 5 && unreadable_fragments * 4 >= text_fragments));
        let status = if unreadable_fragments > 0 {
            has_encoding_issues = true;
            pages_needing_ocr.push(number);
            if !omit_text {
                Status::Partial
            } else {
                Status::Unreadable
            }
        } else if readable {
            Status::Text
        } else if has_images {
            pages_needing_ocr.push(number);
            Status::ImageOnly
        } else {
            Status::NoText
        };
        partial |= status != Status::Text || has_images;
        text.push_str(&format!("## Page {number}\n\n"));
        if omit_text {
            text.push_str(
                "[Text layer is unreliable and has been omitted. Read this page's image \
                instead; see pdf.vision for whether it is attached or still needs loading. Do not \
                infer the page's content from text extraction.]\n\n",
            );
        } else if unreadable_fragments > 0 {
            text.push_str(&format!(
                "[Partial extraction: {unreadable_fragments} text fragments \
                could not be decoded. Their positions are marked below; verify this page with OCR \
                or page-image recognition.]\n\n"
            ));
        }
        if has_images && !omit_text {
            text.push_str(
                "[Text extraction excludes image content. Read the page image; see pdf.vision for \
                    attachment status.]\n\n",
            );
        }
        if !readable && unreadable_fragments == 0 {
            text.push_str(if has_images {
                "[No selectable text was extracted. Read the page image or use OCR.]\n\n"
            } else {
                "[No text was extracted. This page may be blank or contain graphics/unsupported \
                    content; inspect the page if needed.]\n\n"
            });
        }
        if !omit_text {
            let markdown = pdf_inspector::to_markdown_from_items(items, Default::default());
            text.push_str(&markdown);
        }
        text.push_str("\n\n");
        pages.push(PageInfo {
            page: number,
            status,
            has_images,
            unreadable_fragments,
        });
    }
    Document {
        text,
        pdf: Some(PdfPages {
            start_page: start,
            end_page: end,
            total_pages: total,
            pages_needing_ocr,
            has_encoding_issues,
            extraction: "per_page_text".into(),
            partial,
            pages,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severely_damaged_page_omits_placeholders_and_surviving_garbage() {
        let mut items =
            pdf_inspector::extract_text_with_positions_mem(&super::super::tests::pdf()).unwrap();
        let template = items
            .iter()
            .find(|item| item.page == 1)
            .unwrap()
            .clone();
        for item in items
            .iter_mut()
            .filter(|item| item.page == 1)
        {
            item.text = "5L @0 +X+X".into();
        }
        for _ in 0..69 {
            let mut item = template.clone();
            item.text = "\u{fffd}".into();
            items.push(item);
        }
        let result = render_pages(items, 1, 2, 2);
        assert!(
            result
                .text
                .contains("Text layer is unreliable and has been omitted")
        );
        assert!(
            !result
                .text
                .contains("Unreadable text fragment")
        );
        assert!(!result.text.contains("5L @0"));
        assert!(result.text.contains("Second chapter"));
        assert_eq!(result.pdf.unwrap().pages[0].status, Status::Unreadable);
    }

    #[test]
    fn undecodable_fragment_does_not_discard_the_rest_of_the_page_or_range() {
        let mut items =
            pdf_inspector::extract_text_with_positions_mem(&super::super::tests::pdf()).unwrap();
        let mut broken = items
            .iter()
            .find(|item| item.page == 1)
            .unwrap()
            .clone();
        broken.text = "broken \u{fffd} font".into();
        broken.y -= 20.0;
        items.push(broken);
        let result = render_pages(items, 1, 3, 3);
        assert!(result.text.contains("First chapter"));
        assert!(result.text.contains("Second chapter"));
        assert!(
            result
                .text
                .contains("Unreadable text fragment")
        );
        assert!(!result.text.contains('\u{fffd}'));
        assert!(result.text.contains("## Page 3"));
        let metadata = result.pdf.unwrap();
        assert_eq!(metadata.pages[0].status, Status::Partial);
        assert_eq!(metadata.pages[1].status, Status::Text);
        assert_eq!(metadata.pages[2].status, Status::NoText);
        assert_eq!(metadata.pages_needing_ocr, vec![1]);
    }
}
