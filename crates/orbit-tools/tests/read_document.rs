use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::read_document::ReadDocument;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn mixed_pdf() -> Vec<u8> {
    let page_count = 7;
    let kids = (0..page_count)
        .map(|i| format!("{} 0 R", 5 + i * 2))
        .collect::<Vec<_>>()
        .join(" ");
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {page_count} >>"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB \
            /BitsPerComponent 8 /Length 3 >>\nstream\nabc\nendstream"
            .into(),
    ];
    for i in 0..page_count {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << \
            /Font << /F1 3 0 R >> /XObject << /Im 4 0 R >> >> /Contents {} 0 R >>",
            6 + i * 2
        ));
        let drawing = if i == 0 {
            ""
        } else {
            "q 200 0 0 200 50 300 cm /Im Do Q"
        };
        let text = if i == 1 {
            "".into()
        } else {
            format!(
                "BT /F1 12 Tf 50 700 Td (Page {} readable text.) Tj ET",
                i + 1
            )
        };
        let stream = format!("{drawing} {text}");
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
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

#[tokio::test]
async fn pdf_images_work_in_the_native_document_worker() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("mixed.pdf"), mixed_pdf()).unwrap();
    let tool = ReadDocument::new(Workspace::new(root.path()).unwrap())
        .with_worker(env!("CARGO_BIN_EXE_orbit-document-worker").into());
    let output = tool
        .execute(
            ToolCall::function(
                "doc",
                "ReadDocument",
                json!({
                    "path": "mixed.pdf",
                    "start_page": 2,
                    "end_page": 3
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        value["pdf"]["vision"]["attached_pages"],
        json!([2, 3]),
        "{value}"
    );
    assert_eq!(output.attachments.len(), 2);
    assert_eq!(
        std::fs::read_dir(root.path())
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn pdf_images_reach_attachments_with_page_numbers_and_bounded_continuation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("mixed.pdf"), mixed_pdf()).unwrap();
    let tool = ReadDocument::new(Workspace::new(root.path()).unwrap())
        .with_worker(env!("CARGO_BIN_EXE_orbit-document-worker").into());
    let call = |extra: Value| {
        let mut args = json!({
            "path": "mixed.pdf",
            "start_page": 1,
            "end_page": 7
        });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        ToolCall::function("document", "ReadDocument", args)
    };
    let output = tool
        .execute(call(json!({})), CancellationToken::new())
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        value["pdf"]["vision"]["attached_pages"],
        json!([2, 3, 4, 5])
    );
    assert_eq!(value["pdf"]["vision"]["next_image_start_page"], 6);
    assert!(
        value["content"]
            .as_str()
            .unwrap()
            .contains("Page 1 readable text")
    );
    assert_eq!(output.attachments.len(), 4);
    let attachment = &output.attachments[0];
    assert!(attachment.name().contains("page 2"));
    let temporary_path = attachment.path().to_owned();
    assert!(attachment.is_temporary());
    let image = image::load_from_memory(
        &orbit_model::types::Attachment::read_image_bytes(attachment.path()).unwrap(),
    )
    .unwrap()
    .to_rgb8();
    assert_eq!(image.width().max(image.height()), 2_048);
    assert!(
        image
            .pixels()
            .any(|pixel| pixel.0[0] < 150)
    );
    let message = orbit_model::types::ChatMessage::user_with_attachments(
        "document pages",
        output.attachments.clone(),
    );
    let saved = serde_json::to_string(&message).unwrap();
    assert!(!saved.contains(temporary_path.to_str().unwrap()));
    drop(message);
    drop(output);
    assert!(!temporary_path.exists());
    let next = tool
        .execute(
            call(json!({
                "image_start_page": 6
            })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&next).unwrap();
    assert_eq!(value["pdf"]["vision"]["attached_pages"], json!([6, 7]));
    assert!(value["pdf"]["vision"]["next_image_start_page"].is_null());
    assert_eq!(next.attachments.len(), 2);
    let text_only = tool
        .execute_with_context(
            call(json!({})),
            CancellationToken::new(),
            orbit_agent::tool::ToolContext {
                supports_vision: false,
            },
        )
        .await
        .unwrap();
    assert!(text_only.attachments.is_empty());
    assert!(text_only.contains("Current model cannot receive images"));
    let off = tool
        .execute(
            call(json!({
                "images": "off"
            })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(off.attachments.is_empty());
    let always = tool
        .execute(
            call(json!({
                "start_page": 1,
                "end_page": 1,
                "images": "always"
            })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(always.attachments.len(), 1);
    assert!(
        always.attachments[0]
            .name()
            .contains("page 1")
    );
    assert!(
        tool.execute(
            call(json!({
                "image_start_page": 8
            })),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        tool.execute(call(json!({})), cancelled)
            .await,
        Err(orbit_agent::tool::ToolError::Cancelled)
    ));
    assert_eq!(
        std::fs::read_dir(root.path())
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn render_failure_preserves_extracted_text() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("mixed.pdf"), mixed_pdf()).unwrap();
    let wrapper = data.path().join("worker.sh");
    let real = shlex::try_join([env!("CARGO_BIN_EXE_orbit-document-worker")]).unwrap();
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --internal-pdf-render-worker ]; \
        then exit 7; fi\nexec {real} \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tool = ReadDocument::new(Workspace::new(root.path()).unwrap()).with_worker(wrapper);
    let output = tool
        .execute(
            ToolCall::function(
                "doc",
                "ReadDocument",
                json!({
                    "path": "mixed.pdf"
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(output.attachments.is_empty());
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["pdf"]["vision"]["status"], "failed");
    assert_eq!(
        value["pdf"]["vision"]["failures"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(
        value["content"]
            .as_str()
            .unwrap()
            .contains("Page 1 readable text")
    );
}

#[tokio::test]
async fn standalone_worker_returns_paginated_content_and_invalidates_source_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("table.csv");
    std::fs::write(&path, "name,value\nalpha,1\nbeta,2\n").unwrap();
    let tool = ReadDocument::new(Workspace::new(dir.path()).unwrap())
        .with_worker(env!("CARGO_BIN_EXE_orbit-document-worker").into());
    let call = |arguments| ToolCall::function("document", "ReadDocument", arguments);
    let first: Value = serde_json::from_str(
        &tool
            .execute(
                call(json!({
                    "path": "table.csv",
                    "max_chars": 10
                })),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["next_start_char"], 10);
    assert!(first["pdf"].is_null());
    std::fs::write(&path, "name,value\nchanged,42\n").unwrap();
    let next = tool
        .execute(
            call(json!({
                "path": "table.csv"
            })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(next.contains("changed"));
    let invalid = tool
        .execute(
            call(json!({
                "path": "table.csv",
                "start_page": 2
            })),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        invalid
            .to_string()
            .contains("only for PDFs")
    );
    assert_eq!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .count(),
        1
    );
}
