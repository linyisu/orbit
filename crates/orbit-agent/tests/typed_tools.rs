use orbit_agent::tool::{Tool, ToolCapabilities, ToolError, ToolOutput, ToolRegistry};
use orbit_model::types::ToolCall;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct InspectPolicy(std::sync::atomic::AtomicBool);

impl orbit_agent::tool::ToolPolicy for InspectPolicy {
    fn visible(&self, _: ToolCapabilities) -> bool {
        true
    }

    fn acquire(&self, call: &ToolCall, _: ToolCapabilities) -> Result<Box<dyn Send>, ToolError> {
        assert!(
            call.arguments_json()
                .unwrap()
                .get("_orbit_purpose")
                .is_none()
        );
        self.0
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(Box::new(()))
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Arguments {
    /// Number of generated entries to inspect.
    #[schemars(range(min = 1, max = 8))]
    count: usize,
    #[serde(default)]
    verbose: bool,
}

struct Inspect;

impl Tool for Inspect {
    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities::READ
    }

    /// Inspect generated entries without modifying them.
    #[orbit_agent::tool(name = "inspect_entries")]
    async fn execute(
        &self,
        arguments: Arguments,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        if cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        Ok(ToolOutput::text(format!(
            "{}:{}",
            arguments.count, arguments.verbose
        )))
    }
}

#[tokio::test]
async fn typed_contract_preserves_defaults_cancellation_and_executor_arguments() {
    let tool = Inspect;
    let definition = tool.definition();
    let schema = definition.function.parameters;
    assert_eq!(schema["required"], json!(["count"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["count"]["maximum"], 8);
    assert_eq!(schema["properties"]["verbose"]["default"], false);
    assert_eq!(tool.capabilities(), ToolCapabilities::READ);

    let call = ToolCall::function("fixture-1", "inspect_entries", json!({"count": 3}));
    assert_eq!(
        tool.execute(call.clone(), CancellationToken::new())
            .await
            .unwrap()
            .content,
        "3:false"
    );
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        tool.execute(call, token).await,
        Err(ToolError::Cancelled)
    ));

    let mut registry = ToolRegistry::default();
    registry.register(Inspect);
    let policy = std::sync::Arc::new(InspectPolicy(std::sync::atomic::AtomicBool::new(false)));
    registry.set_policy(policy.clone());
    let call = ToolCall::function(
        "fixture-2",
        "inspect_entries",
        json!({
            "count": 4, "_orbit_purpose": "Inspect generated entries"
        }),
    );
    let output = registry
        .execute(call, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.content, "4:false");
    assert!(
        policy
            .0
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    let call = ToolCall::function(
        "fixture-3",
        "inspect_entries",
        json!({"count": 4, "extra": true}),
    );
    assert!(matches!(
        registry
            .execute(call, CancellationToken::new())
            .await,
        Err(ToolError::InvalidArguments { .. })
    ));
}
