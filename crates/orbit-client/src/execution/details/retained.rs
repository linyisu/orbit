//! Load complete retained evidence without changing model history or repeating tools.
use super::{Stream, ToolRun, ToolStatus};
use crate::{
    protocol::{Request, Response},
    transport::Connection,
};
use anyhow::{Result, bail, ensure};
use orbit_protocol::tool_output::{Source, Stream as OutputStream};

pub type Contents = Vec<Content>;

/// Decoded evidence and its opaque raw-byte cursor supplied by the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Content {
    pub stream: Stream,
    pub text: String,
    pub offset: Option<u64>,
}

/// Read-only viewer state keyed by the exact received result, never by a tool name alone.
#[derive(Default, Debug)]
pub enum Retained {
    #[default]
    Inline,
    Loading {
        output: String,
        sources: Vec<(Stream, Source)>,
        contents: Contents,
    },
    Loaded {
        output: String,
        sources: Vec<(Stream, Source)>,
        contents: Contents,
    },
    Failed {
        output: String,
        error: String,
    },
}

impl Retained {
    pub fn begin(&mut self, tool: &ToolRun) -> bool {
        let next_sources = sources(tool);
        if matches!(self, Self::Loading { sources, .. } if *sources == next_sources) {
            return false;
        }
        let output = revision(tool);
        let previous = match &*self {
            Self::Inline => None,
            Self::Loaded { output, .. } | Self::Failed { output, .. } => Some(output),
            Self::Loading { output, .. } => Some(output),
        };
        if previous == Some(&output) {
            return false;
        }
        if next_sources.is_empty() {
            *self = Self::Inline;
            return false;
        }
        let contents = match std::mem::take(self) {
            Self::Loaded {
                sources, contents, ..
            } if sources == next_sources => contents,
            _ => Vec::new(),
        };
        *self = Self::Loading {
            output,
            sources: next_sources,
            contents,
        };
        true
    }

    /// The requested revision and already loaded prefix for an incremental read.
    pub fn pending(&self) -> Option<(String, Contents)> {
        match self {
            Self::Loading {
                output, contents, ..
            } => Some((output.clone(), contents.clone())),
            _ => None,
        }
    }

    /// Reject late responses after the owning result changed or the viewer was replaced.
    pub fn receive(&mut self, output: &str, result: Result<Contents, String>) -> bool {
        let Self::Loading {
            output: current,
            sources,
            ..
        } = self
        else {
            return false;
        };
        if current != output {
            return false;
        }
        let sources = std::mem::take(sources);
        *self = match result {
            Ok(contents) => Self::Loaded {
                output: output.into(),
                sources,
                contents,
            },
            Err(error) => Self::Failed {
                output: output.into(),
                error,
            },
        };
        true
    }

    pub fn content(&self, tool: &ToolRun, stream: Stream) -> String {
        if !sources(tool)
            .iter()
            .any(|(page, _)| *page == stream)
        {
            return super::content(tool, stream);
        }
        match self {
            Self::Loading { contents, .. } | Self::Loaded { contents, .. } => contents
                .iter()
                .find(|page| page.stream == stream)
                .map(|page| page.text.clone())
                .unwrap_or_else(|| super::content(tool, stream)),
            Self::Failed { error, .. } => format!("完整结果读取失败\n\n{error}"),
            Self::Inline => super::content(tool, stream),
        }
    }
}

/// A revision identifies received evidence; elapsed-time ticks do not trigger log reads.
pub fn revision(tool: &ToolRun) -> String {
    if matches!(tool.status, ToolStatus::Running | ToolStatus::Background)
        && let Some(progress) = &tool.command_progress
    {
        return serde_json::to_string(progress).unwrap_or_default();
    }
    tool.output.clone().unwrap_or_else(|| {
        tool.command_progress
            .as_ref()
            .and_then(|progress| serde_json::to_string(progress).ok())
            .unwrap_or_default()
    })
}

fn sources(tool: &ToolRun) -> Vec<(Stream, Source)> {
    if tool.name == "Exec"
        && let Some(progress) = &tool.command_progress
        && matches!(tool.status, ToolStatus::Running | ToolStatus::Background)
    {
        return command_sources(&progress.output_id);
    }
    let Some(value) = tool
        .output
        .as_deref()
        .and_then(|output| serde_json::from_str::<serde_json::Value>(output).ok())
    else {
        return Vec::new();
    };
    if tool.name == "Exec"
        && let Some(result) = tool.command_result()
        && (result.output_truncated
            || result.retained_output_truncated
            || result.retained_output_error.is_some())
        && let Some(id) = value["output_id"].as_str()
    {
        return command_sources(id);
    }
    if tool.name == "CommandOutput"
        && let Some(id) = value["output_id"].as_str()
    {
        return vec![(
            Stream::Result,
            Source::Command {
                output_id: id.into(),
                stream: if value["stream"] == "stderr" {
                    OutputStream::Stderr
                } else {
                    OutputStream::Stdout
                },
            },
        )];
    }
    if tool.name == "WebFetch"
        && let Some(id) = value["snapshot_id"].as_str()
        && (value["has_more"] == true
            || value["truncated"] == true
            || value["start_char"]
                .as_u64()
                .is_some_and(|start| start > 0))
    {
        return vec![(
            Stream::Result,
            Source::Web {
                snapshot_id: id.into(),
            },
        )];
    }
    Vec::new()
}

fn command_sources(id: &str) -> Vec<(Stream, Source)> {
    [
        (Stream::Stdout, OutputStream::Stdout),
        (Stream::Stderr, OutputStream::Stderr),
    ]
    .into_iter()
    .map(|(page, stream)| {
        (
            page,
            Source::Command {
                output_id: id.into(),
                stream,
            },
        )
    })
    .collect()
}

/// Uses an independent transport connection so output reads cannot block approvals or cancellation.
pub async fn fetch(
    connection: &Connection,
    session: String,
    agent: Option<String>,
    tool: &ToolRun,
    mut previous: Contents,
) -> Result<Contents> {
    const MAX_CONTENT_BYTES: usize = 32 * 1_024 * 1_024;
    let mut client = connection.connect().await?;
    let mut contents = Vec::new();
    for (stream, source) in sources(tool) {
        let prefix = previous
            .iter_mut()
            .find(|page| page.stream == stream);
        let (mut content, mut offset) = prefix
            .and_then(|page| {
                page.offset
                    .map(|offset| (std::mem::take(&mut page.text), offset))
            })
            .unwrap_or_default();
        let end;
        loop {
            let Response::ToolOutput(page) = client
                .request(Request::ReadToolOutput {
                    session: session.clone(),
                    agent: agent.clone(),
                    source: source.clone(),
                    offset,
                })
                .await?
            else {
                bail!("工作区服务没有返回完整结果，请更新服务后重试");
            };
            ensure!(
                content
                    .len()
                    .saturating_add(page.content.len())
                    <= MAX_CONTENT_BYTES,
                "结果超出查看器容量"
            );
            content.push_str(&page.content);
            let Some(next) = page.next_offset else {
                ensure!(
                    page.read_offset
                        .is_none_or(|end| end >= offset),
                    "服务返回了倒退的输出游标"
                );
                end = page.read_offset;
                break;
            };
            ensure!(
                next > offset && !page.content.is_empty(),
                "服务返回了无效的输出游标"
            );
            offset = next;
        }
        contents.push(Content {
            stream,
            text: content,
            offset: end,
        });
    }
    Ok(contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::model::ToolCall;
    use serde_json::json;

    #[test]
    fn live_log_reads_keep_the_loaded_prefix_and_wait_for_the_pending_revision() {
        let mut tool = ToolRun::new(&ToolCall::function("live", "Exec", json!({})));
        tool.command_progress = Some(orbit_protocol::CommandProgress {
            output_id: "cmd-generated".into(),
            stdout_bytes: 4,
            stderr_bytes: 5,
        });
        let mut state = Retained::default();
        assert!(state.begin(&tool));
        let (key, previous) = state.pending().unwrap();
        assert!(previous.is_empty());
        tool.command_progress
            .as_mut()
            .unwrap()
            .stdout_bytes = 9;
        assert!(!state.begin(&tool));
        assert!(state.receive(
            &key,
            Ok(vec![
                Content {
                    stream: Stream::Stdout,
                    text: "one\n".into(),
                    offset: Some("one\n".len() as u64)
                },
                Content {
                    stream: Stream::Stderr,
                    text: "note\n".into(),
                    offset: Some("note\n".len() as u64)
                }
            ])
        ));
        assert!(state.begin(&tool));
        assert_eq!(state.content(&tool, Stream::Stdout), "one\n");
        let (next, previous) = state.pending().unwrap();
        assert_eq!(previous[0].text, "one\n");
        assert!(!state.receive(&key, Ok(Vec::new())));
        assert!(state.receive(
            &next,
            Ok(vec![Content {
                stream: Stream::Stdout,
                text: "one\ntwo\n".into(),
                offset: Some("one\ntwo\n".len() as u64)
            }])
        ));
        tool.elapsed_ms = Some(5_000);
        assert!(!state.begin(&tool));
        assert_eq!(state.content(&tool, Stream::Stdout), "one\ntwo\n");
    }

    #[test]
    fn retained_results_reject_stale_responses_and_never_replace_raw_evidence() {
        let mut record = ToolRun::new(&ToolCall::function(
            "sample",
            "WebFetch",
            json!({"url": "https://example.org"}),
        ));
        record.set_output(
            ToolStatus::Complete,
            json!({"snapshot_id": "web-one", "has_more": true, "content": "first page"})
                .to_string(),
        );
        let original = record.output.clone().unwrap();
        let mut state = Retained::default();
        assert!(state.begin(&record));
        assert!(!state.begin(&record));
        assert!(
            state
                .content(&record, Stream::Raw)
                .contains("first page")
        );
        record.output = Some(
            json!({"snapshot_id": "web-two", "has_more": true, "content": "different page"})
                .to_string(),
        );
        assert!(state.begin(&record));
        assert!(!state.receive(
            &original,
            Ok(vec![Content {
                stream: Stream::Result,
                text: "stale".into(),
                offset: Some("stale".len() as u64)
            }])
        ));
        assert!(state.receive(
            record.output.as_deref().unwrap(),
            Ok(vec![Content {
                stream: Stream::Result,
                text: "complete document".into(),
                offset: Some("complete document".len() as u64)
            }])
        ));
        assert_eq!(state.content(&record, Stream::Result), "complete document");
        assert!(
            state
                .content(&record, Stream::Raw)
                .contains("different page")
        );
    }
}
