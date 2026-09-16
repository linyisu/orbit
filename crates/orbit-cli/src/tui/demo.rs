use std::time::Duration;

use futures_util::{StreamExt, stream};
use orbit_agent::model::{CompletionFuture, ModelBackend, StreamFuture};
use orbit_model::types::{
    ChatCompletion, ChatMessage, ChatRequest, ModelStream, StreamEvent, ToolCallDelta,
};
use tokio_util::sync::CancellationToken;

pub struct DemoModel;

fn response(request: &ChatRequest) -> ChatMessage {
    let turns = request
        .messages
        .iter()
        .filter(|message| matches!(message, ChatMessage::User { .. }))
        .count();
    ChatMessage::assistant(format!(
        "这是 Orbit 的界面演示，没有调用真实模型。\n\n你正在进行第 {turns} 次对话。\n\n• 输入 / 打开命令菜单，↑↓ 选择，Tab 补全。\n• 输入 /clear 开始新对话。\n• Ctrl+J 换行，Esc 取消正在生成的回答。\n\n退出后移除 --demo，即可使用 config.toml 中的真实 API。"
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
