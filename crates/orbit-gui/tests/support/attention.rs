//! Pending MCP and command decisions for automatic-presentation checks.
use serde_json::{Value, json};

pub(super) fn approvals(workspace: &str) -> Vec<Value> {
    vec![
        json!({"id":71,"preview":{"Mcp":{"server":"fixture-tools","tool":"inspect_sample","arguments":"{\"sample_id\":\"fixture-item-7\",\"mode\":\"summary\"}"}}}),
        json!({"id":72,"preview":{"Command":{"command":format!("printf 'read-only diagnostics\\n'\n{}", "printf 'detail\\n'\n".repeat(60)),"cwd":workspace,"timeout_seconds":30,"workspace_access":"read","network_allowed":false,"external_capabilities":[],"external_write":false,"git":null}}}),
    ]
}

/// An insertion, deletion and enough surrounding context to exercise folding.
pub(super) fn file_approval(workspace: &str) -> Value {
    let before = (1..=80)
        .map(|line| format!("import fixture.Line{line};\n"))
        .collect::<String>();
    let after = before
        .replace(
            "import fixture.Line30;\n",
            "import fixture.Line30;\nimport fixture.GeneratedIndex;\n",
        )
        .replace("import fixture.Line40;\n", "");
    json!({"id":73,"preview":{"Write":{"path":format!("{workspace}/SampleCatalog.java"),"before":before,"after":after}}})
}
