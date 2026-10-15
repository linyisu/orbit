use futures_util::StreamExt;

use crate::{error::ModelError, stream::StreamAccumulator, types::*};

/// SSE-only account endpoints also serve non-interactive and summary callers.
pub(crate) async fn completion(
    mut stream: ModelStream,
    model: &str,
) -> Result<ChatCompletion, ModelError> {
    let mut accumulator = StreamAccumulator::default();
    while let Some(event) = stream.next().await {
        accumulator.push(event?);
    }
    let mut completion = accumulator.finish()?;
    completion.model = Some(model.into());
    Ok(completion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn account_completion_retains_terminal_reason_before_usage_tail() {
        let stream = futures_util::stream::iter([
            Ok(StreamEvent::TextDelta("generated answer".into())),
            Ok(StreamEvent::Completed {
                finish_reason: Some("stop".into()),
                usage: Some(Usage {
                    prompt_tokens: Some(11),
                    ..Default::default()
                }),
            }),
            Ok(StreamEvent::Completed {
                finish_reason: None,
                usage: Some(Usage {
                    completion_tokens: Some(4),
                    ..Default::default()
                }),
            }),
        ]);
        let result = completion(Box::pin(stream), "synthetic-model")
            .await
            .unwrap();
        assert_eq!(result.model.as_deref(), Some("synthetic-model"));
        assert_eq!(result.finish_reason.as_deref(), Some("stop"));
        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(11));
        assert_eq!(usage.completion_tokens, Some(4));
    }
}
