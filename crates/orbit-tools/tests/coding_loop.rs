//! End-to-end execution fixtures. Scripted model choices isolate runtime
//! correctness; these tests do not measure a real model's coding intelligence.
use orbit_agent::{
    agent::{Agent, AgentConfig},
    events::AgentEvent,
    model::MockModel,
    tool::ToolRegistry,
};
use orbit_model::types::{ChatCompletion, ChatMessage, ChatRequest, ToolCall};
use orbit_runtime::filesystem::Workspace;
use orbit_tools::{
    approval,
    edit_file::EditFile,
    read_file::ReadFile,
    run_command::{CommandLimits, RunCommand},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn call(id: &str, name: &str, args: serde_json::Value) -> ChatCompletion {
    ChatCompletion {
        id: None,
        model: None,
        message: ChatMessage::assistant_tool_calls(vec![ToolCall::function(id, name, args)]),
        finish_reason: Some("tool_calls".into()),
        usage: None,
    }
}

#[tokio::test]
#[ignore = "requires Linux user namespaces, Python, Node and ORBIT_TEST_RUST_TOOLCHAIN"]
async fn rust_python_node_fail_edit_retest_in_real_sandboxes() {
    let rust =
        std::env::var_os("ORBIT_TEST_RUST_TOOLCHAIN").expect("set ORBIT_TEST_RUST_TOOLCHAIN");
    let cases = [
        (
            "lib.rs",
            "pub fn parse(s:&str)->Option<char> { Some(s.chars().next().unwrap()) }\n#[test] fn empty() {assert_eq!(parse(\"\"),None);}\n",
            "Some(s.chars().next().unwrap())",
            "s.chars().next()",
            "rustc --test lib.rs -o /tmp/tests && /tmp/tests",
        ),
        (
            "parser.py",
            "def parse(s):\n    return s[0]\n\nimport unittest\nclass TestParser(unittest.TestCase):\n    def test_empty(self): self.assertIsNone(parse(''))\nif __name__ == '__main__': unittest.main()\n",
            "return s[0]",
            "return s[0] if s else None",
            "python3 parser.py",
        ),
        (
            "parser.test.cjs",
            "const test=require('node:test');const assert=require('node:assert/strict');\nfunction parse(s) {return s[0].toUpperCase();}\ntest('empty input',()=>assert.equal(parse(''),null));\n",
            "return s[0].toUpperCase();",
            "return s.length ? s[0].toUpperCase() : null;",
            "node --test parser.test.cjs",
        ),
    ];

    for (path, source, old, new, command) in cases {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(path), source).unwrap();
        let workspace = Workspace::new(temp.path()).unwrap();
        let (tx, mut rx) = approval::channel();
        let approvals = tokio::spawn(async move {
            while let Some(request) = rx.recv().await {
                request.decision.send(true).unwrap();
            }
        });
        let mut tools = ToolRegistry::new();
        tools.register(ReadFile::new(workspace.clone()));
        tools.register(EditFile::new(workspace.clone(), tx.clone()));
        tools.register(RunCommand::with_limits(
            workspace,
            tx,
            CommandLimits {
                rust_toolchain: Some(rust.clone().into()),
                ..Default::default()
            },
        ));
        let backend = MockModel::new(vec![
            call("fail", "run_command", json!({ "command": command })),
            call("read", "read_file", json!({ "path": path })),
            call(
                "edit",
                "edit_file",
                json!({ "path": path, "old_text": old, "new_text": new }),
            ),
            call("pass", "run_command", json!({ "command": command })),
            ChatCompletion {
                id: None,
                model: None,
                message: ChatMessage::assistant("verified"),
                finish_reason: Some("stop".into()),
                usage: None,
            },
        ]);
        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(256);
        let result = Agent::new(backend.clone(), tools, AgentConfig::default())
            .run_with_agent_events(
                ChatRequest::new(vec![ChatMessage::user(
                    "Fix empty input panic; verify the fix",
                )]),
                CancellationToken::new(),
                events_tx,
            )
            .await
            .unwrap();

        assert_eq!(result.tool_calls, 4);

        let mut codes = Vec::new();
        let mut snapshots = Vec::new();

        while let Ok(event) = events_rx.try_recv() {
            match event {
                AgentEvent::ToolFinished { call, content, .. }
                    if call.function.name == "run_command" =>
                {
                    codes.push(
                        serde_json::from_str::<serde_json::Value>(&content).unwrap()["exit_code"]
                            .as_i64(),
                    )
                }

                AgentEvent::VerificationRecorded { evidence, .. } => {
                    snapshots.push(evidence.input.digest)
                }
                AgentEvent::ToolFailed { error, .. } => panic!("{path}: {error}"),
                _ => {}
            }
        }

        assert_ne!(codes[0], Some(0), "{path} must fail before edit");
        assert_eq!(codes[1], Some(0), "{path} must pass after edit");
        assert_ne!(snapshots[0], snapshots[1]);
        assert_eq!(
            std::fs::read_to_string(temp.path().join(path)).unwrap(),
            source.replace(old, new)
        );
        assert_eq!(
            std::fs::read_dir(temp.path()).unwrap().count(),
            1,
            "build products must stay out of host project"
        );

        approvals.await.unwrap();
    }
}
