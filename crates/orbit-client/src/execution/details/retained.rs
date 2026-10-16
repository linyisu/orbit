//! Load complete retained evidence without changing model history or repeating tools.
use super::{Stream, ToolRun, ToolStatus};
use crate::{
    protocol::{Request, Response},
    transport::Connection,
};
use anyhow::{Result, bail, ensure};
use orbit_protocol::tool_output::{Source, Stream as OutputStream};

pub type Contents = Vec<(Stream, String)>;

/// Read-only viewer state keyed by the exact received result, never by a tool name alone.
#[derive(Default, Debug)]
pub enum Retained {
    #[default]
    Inline,
    Loading(String),
    Loaded {
        output: String,
        contents: Contents,
    },
    Failed {
        output: String,
        error: String,
    },
}

impl Retained {
    pub fn begin(&mut self, tool: &ToolRun) -> bool {
        let Some(output) = &tool.output else {
            return false;
        };
        let previous = match &*self {
            Self::Inline => None,
            Self::Loading(output) | Self::Loaded { output, .. } | Self::Failed { output, .. } => {
                Some(output)
            }
        };
        if previous == Some(output) {
            return false;
        }
        if sources(tool).is_empty() {
            *self = Self::Inline;
            return false;
        }
        *self = Self::Loading(output.clone());
        true
    }

    /// Reject late responses after the owning result changed or the viewer was replaced.
    pub fn receive(&mut self, output: &str, result: Result<Contents, String>) -> bool {
        if !matches!(self, Self::Loading(current) if current == output) {
            return false;
        }
        *self = match result {
            Ok(contents) => Self::Loaded {
                output: output.into(),
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
            Self::Loading(_) => "正在读取完整结果…".into(),
            Self::Loaded { contents, .. } => contents
                .iter()
                .find(|(page, _)| *page == stream)
                .map(|(_, content)| content.clone())
                .unwrap_or_default(),
            Self::Failed { error, .. } => format!("完整结果读取失败\n\n{error}"),
            Self::Inline => super::content(tool, stream),
        }
    }
}

fn sources(tool: &ToolRun) -> Vec<(Stream, Source)> {
    if matches!(tool.status, ToolStatus::Running | ToolStatus::Background) {
        return Vec::new();
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
        return [
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
        .collect();
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

/// Uses an independent transport connection so output reads cannot block approvals or cancellation.
pub async fn fetch(
    connection: &Connection,
    session: String,
    agent: Option<String>,
    tool: &ToolRun,
) -> Result<Contents> {
    const MAX_CONTENT_BYTES: usize = 32 * 1_024 * 1_024;
    let mut client = connection.connect().await?;
    let mut contents = Vec::new();
    for (stream, source) in sources(tool) {
        let mut offset = 0;
        let mut content = String::new();
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
                break;
            };
            ensure!(
                next > offset && !page.content.is_empty(),
                "服务返回了无效的输出游标"
            );
            offset = next;
        }
        contents.push((stream, content));
    }
    Ok(contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::model::ToolCall;
    use serde_json::json;

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
        assert!(!state.receive(&original, Ok(vec![(Stream::Result, "stale".into())])));
        assert!(state.receive(
            record.output.as_deref().unwrap(),
            Ok(vec![(Stream::Result, "complete document".into())])
        ));
        assert_eq!(state.content(&record, Stream::Result), "complete document");
        assert!(
            state
                .content(&record, Stream::Raw)
                .contains("different page")
        );
    }
}
