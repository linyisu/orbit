use orbit_agent::{
    Agent, AgentConfig,
    events::AgentEvent,
    model::MockModel,
    tool::{Tool, ToolError, ToolOutcome, ToolOutput, ToolRegistry},
};
use orbit_model::types::{ChatCompletion, ChatMessage, ChatRequest, ToolCall};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Arguments {}

struct Receipt(Option<ToolOutcome>);

impl Tool for Receipt {
    /// Return an opaque receipt for a synthetic operation.
    #[orbit_agent::tool(name = "receipt")]
    async fn execute(
        &self,
        _arguments: Arguments,
        _cancellation: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        match self.0 {
            Some(outcome) => Ok(ToolOutput::text("opaque result").with_outcome(outcome)),
            None => Err(ToolError::Cancelled),
        }
    }
}

#[tokio::test]
async fn foreground_receipts_preserve_outcomes_without_parsing_result_text() {
    for (reported, expected) in [
        (Some(ToolOutcome::Completed), ToolOutcome::Completed),
        (Some(ToolOutcome::Failed), ToolOutcome::Failed),
        (Some(ToolOutcome::Cancelled), ToolOutcome::Cancelled),
        (None, ToolOutcome::Cancelled),
    ] {
        let mut tools = ToolRegistry::new();
        tools.register(Receipt(reported));
        let backend = MockModel::new(
            [
                ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                    "operation",
                    "receipt",
                    json!({}),
                )]),
                ChatMessage::assistant("done"),
            ]
            .into_iter()
            .map(|message| ChatCompletion {
                id: None,
                model: None,
                message,
                finish_reason: Some("stop".into()),
                usage: None,
            }),
        );
        let agent = Agent::new(backend, tools, AgentConfig::default());
        let (sender, mut events) = tokio::sync::mpsc::channel(64);
        agent
            .run_with_agent_events(
                ChatRequest::new(vec![ChatMessage::user("Perform the fixture operation")]),
                CancellationToken::new(),
                sender,
            )
            .await
            .unwrap();
        let mut receipt = None;
        while let Some(event) = events.recv().await {
            if let AgentEvent::ToolFinished {
                outcome, content, ..
            } = event
            {
                if reported.is_some() {
                    assert_eq!(content, "opaque result");
                } else {
                    let failure: serde_json::Value = serde_json::from_str(&content).unwrap();
                    assert_eq!(failure["error"]["code"], "cancelled");
                    assert_eq!(failure["error"]["recovery"], "stop");
                }
                receipt = outcome;
            }
        }
        assert_eq!(receipt, Some(expected));
    }
}
