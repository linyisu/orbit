use std::time::Duration;

use futures_util::{StreamExt, stream};
use orbit_agent::model::{CompletionFuture, ModelBackend, StreamFuture};
use orbit_model::types::{
    ChatCompletion, ChatMessage, ChatRequest, ModelStream, StreamEvent, ToolCall, ToolCallDelta,
};
use tokio_util::sync::CancellationToken;

pub struct DemoModel;

fn response(request: &ChatRequest) -> ChatMessage {
    let turns = request
        .messages
        .iter()
        .filter(|message| matches!(message, ChatMessage::User { .. }))
        .count();
    if let Some(ChatMessage::Tool { content, .. }) = request.messages.last() {
        if content.starts_with("tool error:") {
            return ChatMessage::assistant(format!(
                "读取未成功：{content}\n\n这是实际工具返回的错误。可以换一个工作区内的相对路径重试。"
            ));
        }
        let value: serde_json::Value = serde_json::from_str(content).unwrap_or_default();
        return ChatMessage::assistant(format!(
            "已通过真实 read_file 工具读取 {}，文件共 {} 行。\n\n{}\n{}\n\n这段内容来自本机文件；回答由演示流程生成，没有调用模型 API。",
            value["path"].as_str().unwrap_or_default(),
            value["total_lines"],
            value["content"].as_str().unwrap_or_default(),
            if value["truncated"] == true {
                "这里只展示了部分内容。"
            } else {
                "读取完成。"
            },
        ));
    }
    if let Some(ChatMessage::User { content }) = request.messages.last()
        && let Some(path) = content
            .strip_prefix("read ")
            .or_else(|| content.strip_prefix("读取 "))
    {
        return ChatMessage::Assistant {
            content: Some("正在调用 read_file 读取工作区文件。".into()),
            tool_calls: vec![ToolCall::function(
                format!("demo-read-{turns}"),
                "read_file",
                serde_json::json!({"path": path.trim(), "max_lines": 40}),
            )],
        };
    }
    ChatMessage::assistant(format!(
        "这是 Orbit 的演示模式，没有调用真实模型。\n\n你正在进行第 {turns} 次对话。\n\n输入 read Cargo.toml，可以体验完整流程：\n• Agent 发起 read_file 调用\n• 工具实际读取工作区文件\n• 工具结果回到 Agent，再生成演示回答\n\n也可以输入 read ../outside.txt 测试路径边界，或 read missing.txt 查看失败状态。\n\n/ 查看命令 · Ctrl+J 换行 · Esc 取消 · Ctrl+O 展开工具结果。"
    ))
}

impl ModelBackend for DemoModel {
    fn complete<'a>(&'a self, request: ChatRequest, _: CancellationToken) -> CompletionFuture<'a> {
        Box::pin(async move {
            Ok(ChatCompletion {
                id: None,
                model: Some("demo".into()),
                message: response(&request),
                finish_reason: Some("stop".into()),
                usage: None,
            })
        })
    }

    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> StreamFuture<'a> {
        Box::pin(async move {
            let ChatMessage::Assistant {
                content,
                tool_calls,
            } = response(&request)
            else {
                unreachable!()
            };
            let text = content.unwrap_or_default();
            let chars = text.chars().collect::<Vec<_>>();
            let mut events = chars
                .chunks(12)
                .map(|chunk| StreamEvent::TextDelta(chunk.iter().collect()))
                .collect::<Vec<_>>();
            for (index, call) in tool_calls.into_iter().enumerate() {
                events.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                    index,
                    id: Some(call.id),
                    name: Some(call.function.name),
                    arguments: call.function.arguments,
                }));
            }
            events.push(StreamEvent::Completed {
                finish_reason: Some("stop".into()),
                usage: None,
            });
            let stream = stream::iter(events).then(move |event| {
                let token = cancellation.clone();
                async move {
                    tokio::select! {
                        _ = token.cancelled() => Err(orbit_model::error::ModelError::Cancelled),
                        _ = tokio::time::sleep(Duration::from_millis(35)) => Ok(event),
                    }
                }
            });
            Ok(Box::pin(stream) as ModelStream)
        })
    }
}
