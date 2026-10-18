//! Session-scoped retained evidence; reads never re-execute an operation.
use anyhow::{Context, Result, ensure};
use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_protocol::tool_output::{Page, Source, Stream};
use orbit_runtime::execution::output::CommandOutputs;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

pub(super) async fn read(directory: PathBuf, source: Source, offset: u64) -> Result<Page> {
    match source {
        Source::Command { output_id, stream } => {
            let reader = orbit_tools::read_command_output::ReadCommandOutput::new(
                CommandOutputs::persistent(directory),
            );
            let arguments = serde_json::json!({
                "output_id": output_id,
                "stream": match stream {
                    Stream::Stdout => "stdout",
                    Stream::Stderr => "stderr",
                },
                "offset": offset,
            });
            let result = reader
                .execute(
                    ToolCall::function("inspect", "CommandOutput", arguments),
                    CancellationToken::new(),
                )
                .await?;
            let value: serde_json::Value = serde_json::from_str(&result.content)?;
            ensure!(
                value["capture_complete"] != false,
                "完整输出读取失败：原始日志未被完整保存"
            );
            Ok(Page {
                content: value["content"]
                    .as_str()
                    .context("invalid retained output")?
                    .to_owned(),
                next_offset: (value["has_more"] == true)
                    .then(|| value["next_offset"].as_u64())
                    .flatten(),
                read_offset: value["next_offset"].as_u64(),
            })
        }
        Source::Web { snapshot_id } => {
            tokio::task::spawn_blocking(move || {
                const PAGE_BYTES: usize = 32 * 1_024;
                let snapshot = orbit_tools::web_fetch::retained_snapshot(&directory, &snapshot_id)?;
                let value: serde_json::Value = serde_json::from_str(&snapshot)?;
                let text = value["content"]
                    .as_str()
                    .context("invalid web snapshot")?;
                let start = usize::try_from(offset)?;
                ensure!(
                    start <= text.len() && text.is_char_boundary(start),
                    "invalid output offset"
                );
                let mut end = start
                    .saturating_add(PAGE_BYTES)
                    .min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                Ok(Page {
                    content: text[start..end].to_owned(),
                    next_offset: (end < text.len()).then_some(end as u64),
                    read_offset: Some(end as u64),
                })
            })
            .await?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_complete_unicode_logs_and_rejects_missing_or_incomplete_capture() {
        let root = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir_in(root.path()).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let text = "generated 甲乙\n".repeat(8_000);
        std::fs::write(directory.path().join("cmd-test.stdout"), &text).unwrap();
        let receipt = |truncated| {
            serde_json::json!({
                "stdout_bytes": text.len(),
                "stderr_bytes": 0,
                "truncated": truncated,
                "error": null
            })
            .to_string()
        };
        std::fs::write(directory.path().join("cmd-test.json"), receipt(false)).unwrap();
        let source = Source::Command {
            output_id: "cmd-test".into(),
            stream: Stream::Stdout,
        };
        let mut actual = String::new();
        let mut offset = 0;
        loop {
            let page = read(directory.path().into(), source.clone(), offset)
                .await
                .unwrap();
            actual.push_str(&page.content);
            let Some(next) = page.next_offset else {
                break;
            };
            assert!(next > offset);
            offset = next;
        }
        assert_eq!(actual, text);
        std::fs::write(directory.path().join("cmd-test.json"), receipt(true)).unwrap();
        assert!(
            read(directory.path().into(), source.clone(), 0)
                .await
                .is_err()
        );
        let other = tempfile::tempdir().unwrap();
        assert!(
            read(other.path().into(), source, 0)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn live_log_cursor_uses_raw_bytes_even_when_decoding_replaces_invalid_text() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir_in(root.path()).unwrap();
        for path in [root.path(), directory.path()] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = directory.path().join("cmd-live.stderr");
        std::fs::write(&path, b"first\xff\n").unwrap();
        let source = Source::Command {
            output_id: "cmd-live".into(),
            stream: Stream::Stderr,
        };
        let first = read(directory.path().into(), source.clone(), 0)
            .await
            .unwrap();
        assert_eq!(first.content, "first\u{fffd}\n");
        assert_eq!(first.read_offset, Some(7));
        assert!(first.next_offset.is_none());
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"second\n")
            .unwrap();
        let second = read(directory.path().into(), source, first.read_offset.unwrap())
            .await
            .unwrap();
        assert_eq!(second.content, "second\n");
        assert_eq!(second.read_offset, Some(14));
    }

    #[tokio::test]
    async fn reads_web_snapshot_from_start_to_end_without_network() {
        let directory = tempfile::tempdir().unwrap();
        let id = format!("web-{}", "a".repeat(64));
        let text = "甲乙\n".repeat(12_000);
        std::fs::write(
            directory
                .path()
                .join(format!("{id}.json")),
            serde_json::json!({"content": text}).to_string(),
        )
        .unwrap();
        let source = Source::Web { snapshot_id: id };
        let mut offset = 0;
        let mut actual = String::new();
        loop {
            let page = read(directory.path().into(), source.clone(), offset)
                .await
                .unwrap();
            actual.push_str(&page.content);
            let Some(next) = page.next_offset else {
                break;
            };
            offset = next;
        }
        assert_eq!(actual, text);
    }
}
