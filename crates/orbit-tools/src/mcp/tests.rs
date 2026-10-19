use super::*;

#[tokio::test]
async fn failed_start_reports_the_error_chain_and_stderr() {
    let workspace = tempfile::tempdir().unwrap();
    let config = lifecycle::config("startup_error");
    let (approvals, _receiver) = crate::approval::channel();
    let statuses = register(
        &mut ToolRegistry::new(),
        &McpConfig {
            servers: BTreeMap::from([("broken".into(), config)]),
        },
        workspace.path(),
        Some(approvals),
        true,
    )
    .await;
    assert!(!statuses[0].connected);
    assert!(
        statuses[0]
            .detail
            .contains("MCP initialize failed"),
        "{}",
        statuses[0].detail
    );
    assert!(
        statuses[0]
            .detail
            .contains("missing MCP subcommand: use --mcp"),
        "{}",
        statuses[0].detail
    );
}

#[tokio::test]
async fn rejects_an_unterminated_response_at_the_protocol_limit() {
    let bytes = vec![b'x'; MAX_MESSAGE_BYTES + 1];
    let mut reader = BufReader::new(std::io::Cursor::new(bytes));
    let mut pending = Vec::new();

    let error = read_protocol_line(&mut reader, &mut pending)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("4 MiB"));
    assert!(pending.len() <= MAX_MESSAGE_BYTES);
}

#[tokio::test]
async fn interrupted_read_keeps_the_partial_json_rpc_line() {
    let (mut writer, reader) = tokio::io::duplex(64);
    let mut reader = BufReader::new(reader);
    let mut pending = Vec::new();
    writer
        .write_all(b"{\"id\":")
        .await
        .unwrap();

    let timed_out = time::timeout(
        Duration::from_millis(10),
        read_protocol_line(&mut reader, &mut pending),
    )
    .await;

    assert!(timed_out.is_err());
    assert_eq!(pending, b"{\"id\":");

    writer.write_all(b"1}\n").await.unwrap();
    let value = read_protocol_line(&mut reader, &mut pending)
        .await
        .unwrap();

    assert_eq!(value["id"], 1);
}

mod lifecycle;

use orbit_model::types::ToolCall;

use crate::approval::ApprovalPreview;

#[test]
fn stdio_configuration_defaults_to_disabled() {
    let config: McpConfig = toml::from_str(
        r#"
            [servers.files]
            command = "npx"
            args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
            tools = ["read_file"]
            "#,
    )
    .unwrap();

    config.validate().unwrap();

    assert!(!config.servers["files"].enabled);
}

#[test]
fn rejects_remote_transport_until_credential_boundary_exists() {
    let config: McpConfig = toml::from_str(
        r#"
            [servers.remote]
            type = "http"
            command = "unused"
            "#,
    )
    .unwrap();

    assert!(
        config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("stdio")
    );
}

#[tokio::test]
async fn discovers_and_calls_a_stdio_server() {
    for mode in [
        crate::approval::ApprovalMode::Ask,
        crate::approval::ApprovalMode::Auto,
        crate::approval::ApprovalMode::Yolo,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let config = McpConfig {
            servers: BTreeMap::from([("fixture".into(), lifecycle::config("echo"))]),
        };
        let mut registry = ToolRegistry::new();
        let (approvals, mut receiver) = crate::approval::channel();
        approvals.policy().set_mode(mode);
        let approval = if mode == crate::approval::ApprovalMode::Ask {
            Some(tokio::spawn(async move {
                let request = receiver
                    .recv()
                    .await
                    .expect("MCP call requests approval");
                let ApprovalPreview::Mcp(preview) = request.preview else {
                    panic!("expected MCP approval");
                };

                assert_eq!(preview.server, "fixture");
                assert_eq!(preview.tool, "echo");

                request.decision.send(true).unwrap();
            }))
        } else {
            None
        };
        let statuses = register(
            &mut registry,
            &config,
            directory.path(),
            Some(approvals),
            true,
        )
        .await;

        assert_eq!(statuses[0].tool_count, 1, "{statuses:?}");
        assert_eq!(
            statuses[0].tools,
            vec![McpToolStatus {
                name: "MCP_Fixture_Echo".into(),
                description: "Echo input".into(),
            }]
        );

        let output = registry
            .execute(
                ToolCall::function(
                    "call",
                    "MCP_Fixture_Echo",
                    json!({
                        "message": "hello"
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        if let Some(approval) = approval {
            approval.await.unwrap();
        }

        assert!(
            output
                .content
                .contains("\"text\":\"ok\"")
        );
    }
}

#[tokio::test]
async fn does_not_start_enabled_servers_without_an_approval_channel() {
    let config: McpConfig = toml::from_str(
        r#"
            [servers.files]
            command = "definitely-not-a-real-mcp-server"
            enabled = true
            "#,
    )
    .unwrap();
    let mut registry = ToolRegistry::new();
    let statuses = register(&mut registry, &config, Path::new("/tmp"), None, false).await;

    assert_eq!(statuses[0].detail, "需要审批通道；未启动");
    assert!(registry.definitions().is_empty());
}

#[tokio::test]
async fn configured_server_requires_startup_authorization() {
    let config: McpConfig = toml::from_str(
        r#"
            [servers.files]
            command = "definitely-not-a-real-mcp-server"
            enabled = true
            "#,
    )
    .unwrap();
    let mut registry = ToolRegistry::new();
    let (approvals, _receiver) = crate::approval::channel();

    let statuses = register(
        &mut registry,
        &config,
        Path::new("/tmp"),
        Some(approvals),
        false,
    )
    .await;

    assert!(
        statuses[0]
            .detail
            .contains("--allow-mcp-start")
    );
    assert!(statuses[0].tools.is_empty());
    assert!(registry.definitions().is_empty());
}

#[test]
fn rejects_mcp_environment_overrides_of_runtime_isolation() {
    let config: McpConfig = toml::from_str(
        r#"
            [servers.files]
            command = "npx"
            [servers.files.env]
            PATH = "/tmp/untrusted"
            "#,
    )
    .unwrap();

    assert!(
        config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("PATH")
    );
}
