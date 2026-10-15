mod request;

use request::read_request;

#[tokio::test]
async fn account_providers_authenticate_and_collect_sse_for_nonstream_callers() {
    use serde_json::json;

    for (format, kind, base, payload) in [
        (
            ApiFormat::ChatgptCodex,
            orbit_auth::CredentialKind::ChatgptOAuth,
            "https://chatgpt.com/backend-api/codex",
            concat!(
                "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\
                    \":\"function_call\",\"id\":\"item-one\",\"call_id\":\"call-one\",\"name\":\"t\
                    est\",\"arguments\":\"\"}}\n\n",
                "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"it\
                    em_id\":\"item-one\",\"delta\":\"{}\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"ou\
                    tput\":[],\"usage\":{\"input_tokens\":10,\"output_tokens\":3,\"total_tokens\":\
                    13}}}\n\n"
            ),
        ),
        (
            ApiFormat::GoogleCodeAssist,
            orbit_auth::CredentialKind::GoogleOAuth,
            "https://cloudcode-pa.googleapis.com",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"nam\
                e\":\"test\",\"args\":{}},\"thoughtSignature\":\"opaque-signature\"}]},\"finishRea\
                son\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\
                \":1,\"thoughtsTokenCount\":2,\"totalTokenCount\":13}}}\n\n",
        ),
        (
            ApiFormat::GoogleCodeAssist,
            orbit_auth::CredentialKind::AntigravityOAuth,
            "https://daily-cloudcode-pa.googleapis.com",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"nam\
                e\":\"test\",\"args\":{}},\"thoughtSignature\":\"opaque-signature\"}]},\"finishRea\
                son\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\
                \":1,\"thoughtsTokenCount\":2,\"totalTokenCount\":13}}}\n\n",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.json");
        let credential = json!({
            "version": 1,
            "kind": kind,
            "endpoint": format!("{base}/"),
            "access": "fake-account-token",
            "refresh": "fake-refresh-token",
            "expires_at": u64::MAX,
            "account_id": "test-account",
            "project_id": "test-project"
        });

        std::fs::write(&path, credential.to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let content_type = if format == ApiFormat::ChatgptCodex {
            ""
        } else {
            "content-type: text/event-stream\r\n"
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\n{content_type}content-length: {}\r\n\r\n{payload}",
            payload.len()
        );
        let (transport, captured) = test_server(response).await;
        let config = ModelConfig {
            api_format: format,
            base_url: base.into(),
            model: if kind == orbit_auth::CredentialKind::AntigravityOAuth {
                "claude-sonnet-4-6"
            } else {
                "test-model"
            }
            .into(),
            credential: Some(orbit_auth::CredentialRef {
                directory: directory.path().into(),
                name: "test".into(),
            }),
            timeout: Duration::from_secs(5),
            max_retries: 0,
            ..Default::default()
        };
        let mut model = ModelClient::new(config).unwrap();
        // The credential scope remains the real service; only the test HTTP transport is
        // redirected.
        model.request_endpoint = Some(transport.parse().unwrap());
        let mut request = ChatRequest::new(vec![ChatMessage::user("hello")]);
        request.max_tokens = Some(10);
        request.temperature = Some(0.5);

        let completion = model.complete(request).await.unwrap();
        let wire = captured.await;

        assert!(wire.contains("authorization: bearer fake-account-token"));
        assert_eq!(completion.usage.unwrap().total_tokens, Some(13));
        let ChatMessage::Assistant { tool_calls, .. } = completion.message else {
            panic!("assistant response expected")
        };
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.arguments, "{}");
        if kind == orbit_auth::CredentialKind::AntigravityOAuth {
            assert!(wire.contains(
                "user-agent: antigravity/hub/2.8.0 (aidev_client; \
                os_type=darwin; arch=arm64; cl=963137146)"
            ));
            assert!(wire.contains("\"useragent\":\"antigravity\""));
            assert!(wire.contains("anthropic-beta: interleaved-thinking-2025-05-14"));
            assert!(wire.contains("\"requesttype\":\"agent\""));
            assert!(wire.contains("\"requestid\":\"agent/"));
        }

        if format == ApiFormat::ChatgptCodex {
            assert_eq!(tool_calls[0].id, "call-one");
            assert!(wire.contains("chatgpt-account-id: test-account"));
            assert!(wire.contains("\"store\":false"));
            assert!(wire.contains("\"stream\":true"));
            assert!(!wire.contains("max_output_tokens"));
            assert!(!wire.contains("temperature"));
        } else {
            assert!(wire.contains("\"project\":\"test-project\""));
            assert_eq!(
                tool_calls[0]
                    .metadata
                    .as_ref()
                    .unwrap()
                    .google
                    .thought_signature
                    .as_deref(),
                Some("opaque-signature")
            );
        }
    }
}

#[tokio::test]
async fn saved_credentials_are_resolved_for_requests_and_logout_takes_effect() {
    let root = tempfile::tempdir().unwrap();
    let store = orbit_auth::AuthStore::new(root.path().join("auth")).unwrap();
    let (base_url, request) = test_server(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"choices\":[{\"message\":{\
                \"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}",
    )
    .await;
    store
        .set_api_key("test", "stored-secret".into(), &base_url)
        .await
        .unwrap();
    let mut config = client(&base_url, "openai_chat_completions", "mock", None).config;
    config.credential = Some(orbit_auth::CredentialRef {
        directory: store.directory().to_owned(),
        name: "test".into(),
    });
    let model = super::ModelClient::new(config).unwrap();
    let message = crate::types::ChatRequest::new(vec![crate::types::ChatMessage::user("hello")]);

    model
        .complete(message.clone())
        .await
        .unwrap();

    assert!(
        request
            .await
            .contains("authorization: bearer stored-secret")
    );
    assert!(
        !serde_json::to_string(model.config())
            .unwrap()
            .contains("stored-secret")
    );

    store.remove("test").await.unwrap();
    let error = model
        .complete(message)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        crate::error::ModelError::Authentication(orbit_auth::AuthError::Missing)
    ));
    assert!(!error.is_retryable());
}

#[test]
fn accepts_custom_http_endpoints_with_api_keys() {
    for base_url in ["http://example.com/v1", "http://192.168.1.42:8080/v1"] {
        let config = crate::types::ModelConfig {
            base_url: base_url.into(),
            api_key: Some("secret".into()),
            ..Default::default()
        };

        let client = super::ModelClient::new(config).unwrap();

        assert_eq!(client.config().base_url, base_url);
    }
}

#[tokio::test]
async fn does_not_follow_a_model_endpoint_redirect() {
    let (base_url, request) = test_server(
        "HTTP/1.1 302 Found\r\nlocation: https://example.com/receive\r\ncontent-length: 0\r\n\r\n",
    )
    .await;
    let client = client(&base_url, "openai_chat_completions", "mock", Some("secret"));

    let error = client
        .complete(crate::types::ChatRequest::new(vec![
            crate::types::ChatMessage::user("hello"),
        ]))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        crate::error::ModelError::Http { status: 302, .. }
    ));
    assert!(request.await.contains("secret"));
}

#[test]
fn recognizes_context_limit_before_truncating_a_long_error_body() {
    let body = serde_json::json!({
        "error": {
            "code": "context_length_exceeded",
            "message": "x".repeat(4_000)
        }
    })
    .to_string();
    let error = crate::error::ModelError::Http {
        status: 400,
        request_id: String::new(),
        body: super::truncate_body(&body, 2_000),
        retryable: false,
        context_limit: crate::error::ModelError::has_context_limit_code(&body),
    };

    assert!(error.is_context_limit());
}

#[tokio::test]
async fn model_config_reasoning_effort_applies_when_request_has_no_override() {
    let (base_url, request) = test_server(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"choices\":[{\"message\":{\
                \"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}",
    )
    .await;
    let mut config = client(&base_url, "openai_chat_completions", "mock", None).config;
    config.reasoning_adapter = Some(crate::types::ReasoningAdapter::OpenaiChat);
    config.reasoning_effort = Some(crate::types::ReasoningEffort::High);
    let client = super::ModelClient::new(config).unwrap();

    client
        .complete(crate::types::ChatRequest::new(vec![
            crate::types::ChatMessage::user("think"),
        ]))
        .await
        .unwrap();

    assert!(
        request
            .await
            .contains("\"reasoning_effort\":\"high\"")
    );
}

#[test]
fn rejects_credentials_used_as_service_addresses_without_echoing_them() {
    for address in [
        "http://sk-not-a-real-secret-123456789",
        "https://name:private-password@example.com/v1",
    ] {
        let config = crate::types::ModelConfig {
            base_url: address.into(),
            ..Default::default()
        };
        let error = super::ModelClient::new(config)
            .unwrap_err()
            .to_string();

        assert!(error.contains("base URL is invalid"));
        assert!(!error.contains("sk-not-a-real-secret"));
        assert!(!error.contains("private-password"));
    }
}

use std::sync::Arc;

use futures_util::StreamExt;
use tokio::{io::AsyncWriteExt, net::TcpListener};

use super::*;

#[test]
fn parses_all_supported_api_formats() {
    assert_eq!(
        ApiFormat::parse("openai_chat_completions").unwrap(),
        ApiFormat::OpenaiChatCompletions
    );
    assert_eq!(
        ApiFormat::parse("openai_responses").unwrap(),
        ApiFormat::OpenaiResponses
    );
    assert_eq!(
        ApiFormat::parse("anthropic_messages").unwrap(),
        ApiFormat::AnthropicMessages
    );
    assert!(matches!(
        ApiFormat::parse("unknown"),
        Err(ModelError::UnsupportedApiFormat(_))
    ));
}

#[test]
fn model_config_debug_redacts_api_key() {
    let config = ModelConfig {
        api_key: Some("secret-value".to_owned()),
        ..ModelConfig::default()
    };
    let debug = format!("{config:?}");

    assert!(!debug.contains("secret-value"));
    assert!(debug.contains("[redacted]"));
}

#[tokio::test]
async fn openai_chat_uses_chat_completions_and_bearer_auth() {
    let (base_url, request) = test_server(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"id\":\"chat-1\",\"model\":\
                \"mock\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"hello\",\
                \"tool_calls\":null},\"finish_reason\":\"stop\"}]}",
    )
    .await;
    let client = client(
        &base_url,
        "openai_chat_completions",
        "mock-model",
        Some("openai-key"),
    );
    let completion = client
        .complete(ChatRequest::new(vec![ChatMessage::user("hello")]))
        .await
        .expect("completion succeeds");

    assert_eq!(completion.message, ChatMessage::assistant("hello"));

    let request = request.await;

    assert!(request.starts_with("post /chat/completions http/1.1"));
    assert!(request.contains("authorization: bearer openai-key"));
    assert!(request.contains("\"model\":\"mock-model\""));
}

#[tokio::test]
async fn openai_responses_maps_output_text_and_uses_responses_path() {
    let (base_url, request) = test_server(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"id\":\"resp-1\",\"model\":\
                \"mock\",\"output_text\":\"hello from responses\",\"output\":[],\"status\":\"compl\
                eted\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"total_tokens\":5}}",
    )
    .await;
    let client = client(
        &base_url,
        "openai_responses",
        "mock-responses",
        Some("responses-key"),
    );
    let completion = client
        .complete(ChatRequest::new(vec![
            ChatMessage::system("be concise"),
            ChatMessage::user("hello"),
        ]))
        .await
        .expect("completion succeeds");

    assert_eq!(
        completion.message,
        ChatMessage::assistant("hello from responses")
    );
    assert_eq!(completion.finish_reason.as_deref(), Some("stop"));

    let request = request.await;

    assert!(request.starts_with("post /responses http/1.1"));
    assert!(request.contains("authorization: bearer responses-key"));
    assert!(request.contains("\"instructions\":\"be concise\""));
    assert!(request.contains("\"input\""));
}

#[tokio::test]
async fn anthropic_thinking_and_top_level_usage_survive_token_exhaustion() {
    let (base_url, request) = test_server(concat!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":321}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thin\
                king\",\"thinking\":\"first \"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_del\
                ta\",\"thinking\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\"\
                :{\"output_tokens\":16384}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    ))
    .await;
    let client = client(&base_url, "anthropic_messages", "fixture", None);
    let mut req = ChatRequest::new(vec![ChatMessage::user("think")]);
    req.max_tokens = Some(16_384);
    let mut stream = client.stream(req).await.unwrap();
    let mut thinking = String::new();
    let mut input_tokens = None;
    let mut finished = false;

    while let Some(event) = stream.next().await {
        match event.unwrap() {
            StreamEvent::ReasoningDelta(text) => thinking.push_str(&text),
            StreamEvent::Completed {
                finish_reason,
                usage,
            } => {
                let usage = usage.unwrap();

                if finish_reason.is_none() {
                    input_tokens = usage.prompt_tokens;
                } else {
                    assert_eq!(finish_reason.as_deref(), Some("max_tokens"));
                    assert_eq!(usage.completion_tokens, Some(16_384));

                    finished = true;
                }
            }

            _ => panic!("unexpected event"),
        }
    }

    assert_eq!(thinking, "first reasoning");
    assert_eq!(input_tokens, Some(321));
    assert!(finished);
    assert!(
        request
            .await
            .contains("\"max_tokens\":16384")
    );
}

#[tokio::test]
async fn anthropic_messages_maps_text_and_uses_anthropic_headers() {
    let (base_url, request) = test_server(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"id\":\"msg-1\",\"model\":\
                \"claude\",\"content\":[{\"type\":\"text\",\"text\":\"hello from \
                claude\"}],\"stop_reason\":\"end_turn\",\"usage\":{\"input_tokens\":4,\"output_tok\
                ens\":5}}",
    )
    .await;
    let client = client(
        &base_url,
        "anthropic_messages",
        "claude-sonnet",
        Some("anthropic-key"),
    );
    let completion = client
        .complete(ChatRequest::new(vec![
            ChatMessage::system("be concise"),
            ChatMessage::user("hello"),
        ]))
        .await
        .expect("completion succeeds");

    assert_eq!(
        completion.message,
        ChatMessage::assistant("hello from claude")
    );

    let request = request.await;

    assert!(request.starts_with("post /messages http/1.1"));
    assert!(request.contains("x-api-key: anthropic-key"));
    assert!(request.contains("anthropic-version: 2023-06-01"));
    assert!(request.contains("\"system\":\"be concise\""));
}

#[tokio::test]
async fn stream_accepts_done_frame_without_trailing_separator() {
    let (base_url, _) = test_server(concat!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\",\"tool_calls\":null},\"finish_re\
                ason\":null}]}\n\n",
        "data: [DONE]",
    ))
    .await;
    let client = client(&base_url, "openai_chat_completions", "mock-chat", None);
    let mut stream = client
        .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
        .await
        .expect("stream starts");
    let mut accumulator = crate::stream::StreamAccumulator::default();

    while let Some(event) = stream.next().await {
        accumulator.push(event.expect("stream event succeeds"));
    }

    assert_eq!(
        accumulator.finish().unwrap().message,
        ChatMessage::assistant("hello")
    );
}

#[test]
fn explicit_sse_terminators_supply_completion_without_overwriting_stop_reasons() {
    for (data, reason) in [
        ("[DONE]", "stop"),
        (r#"{"type":"message_stop"}"#, "stop"),
        (r#"{"type":"response.completed"}"#, "stop"),
        (r#"{"type":"response.incomplete"}"#, "max_tokens"),
    ] {
        let mut completed = false;
        let (events, terminal) =
            decode_sse_data(data, &mut completed, &mut |_| Ok(Vec::new())).unwrap();
        assert!(completed && terminal);
        assert!(matches!(
            &events[..],
            [StreamEvent::Completed { finish_reason: Some(actual), usage: None }]
                if actual == reason
        ));
        let (events, _) = decode_sse_data(data, &mut completed, &mut |_| Ok(Vec::new())).unwrap();
        assert!(events.is_empty());
    }
}

#[tokio::test]
async fn responses_stream_maps_text_and_completion() {
    let (base_url, _) = test_server(concat!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\
                \":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n",
    ))
    .await;
    let client = client(&base_url, "openai_responses", "mock-responses", None);
    let mut stream = client
        .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
        .await
        .expect("stream starts");
    let mut text = String::new();
    let mut completed = false;

    while let Some(event) = stream.next().await {
        match event.expect("stream event succeeds") {
            StreamEvent::TextDelta(delta) => text.push_str(&delta),
            StreamEvent::Completed { .. } => completed = true,
            _ => {}
        }
    }

    assert_eq!(text, "hello");
    assert!(completed);
}

#[test]
fn reports_error_envelopes_in_all_stream_formats() {
    for value in [
        serde_json::json!({
            "error": {
                "message": "overloaded",
                "code": "upstream_error"
            }
        }),
        serde_json::json!({
            "type": "error",
            "error": {
                "type": "overloaded_error",
                "message": "overloaded"
            }
        }),
        serde_json::json!({
            "type": "response.failed",
            "response": {
                "id": "resp-test",
                "error": {
                    "code": "upstream_error",
                    "message": "overloaded"
                }
            }
        }),
    ] {
        let error = decode_sse_data(&value.to_string(), &mut false, &mut |_| {
            panic!("failed responses must not reach the normal decoder")
        })
        .unwrap_err();

        assert!(matches!(
            error,
            ModelError::Upstream { message, .. } if message.contains("overloaded")
        ));
    }
}

#[tokio::test]
async fn responses_completion_does_not_wait_for_connection_close() {
    let response = concat!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
    );
    let (url, _) =
        test_server_held(vec![response.as_bytes().to_vec()], Duration::from_secs(2)).await;
    let model = client(&url, "openai_responses", "mock", None);
    let mut events = model
        .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
        .await
        .unwrap();
    time::timeout(Duration::from_millis(300), async {
        let mut completed = false;

        while let Some(event) = events.next().await {
            completed |= matches!(event.unwrap(), StreamEvent::Completed { .. });
        }

        assert!(completed);
    })
    .await
    .expect("terminal SSE event ends the stream immediately");
}

#[tokio::test]
async fn json_body_timeout_is_bounded_and_identifies_the_stage() {
    let (url, _) = test_server_held(
        vec![
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n"
                .to_vec(),
        ],
        Duration::from_secs(2),
    )
    .await;
    let model = ModelClient::new(ModelConfig {
        base_url: url,
        timeout: Duration::from_millis(100),
        ..ModelConfig::default()
    })
    .unwrap();
    let error = time::timeout(
        Duration::from_secs(1),
        model.complete(ChatRequest::new(vec![ChatMessage::user("hello")])),
    )
    .await
    .unwrap()
    .unwrap_err();

    assert!(matches!(
        error,
        ModelError::Timeout { stage, .. } if stage.contains("JSON body")
    ));
}

#[tokio::test]
async fn active_stream_can_run_longer_than_the_idle_timeout() {
    let mut chunks = vec![b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n".to_vec()];

    for _ in 0..30 {
        chunks.push(
            b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"},\"finish_\
            reason\":null}]}\n\n"
                .to_vec(),
        );
    }

    chunks.push(b"data: [DONE]\n\n".to_vec());
    let (url, _) = test_server_chunks(chunks).await;
    let timeout = Duration::from_millis(200);
    let model = ModelClient::new(ModelConfig {
        base_url: url,
        timeout,
        ..ModelConfig::default()
    })
    .unwrap();
    let mut events = model
        .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
        .await
        .unwrap();
    let started = time::Instant::now();
    let mut count = 0;
    let mut completed = false;

    while let Some(event) = events.next().await {
        assert!(!completed, "received another event after stream completion");
        match event.unwrap() {
            StreamEvent::ReasoningDelta(text) => {
                assert_eq!(text, "thinking");
                count += 1;
            }
            StreamEvent::Completed {
                finish_reason,
                usage,
            } => {
                assert_eq!(finish_reason.as_deref(), Some("stop"));
                assert!(usage.is_none());
                completed = true;
            }
            event => panic!("unexpected stream event: {event:?}"),
        }
    }

    assert_eq!(count, 30);
    assert!(completed);
    assert!(started.elapsed() > timeout);
}

#[tokio::test]
async fn heartbeat_only_stream_times_out_with_received_byte_count() {
    let (url, _) = test_server_held(
        vec![b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n: PING\n\n".to_vec()],
        Duration::from_secs(2),
    )
    .await;
    let model = ModelClient::new(ModelConfig {
        base_url: url,
        timeout: Duration::from_millis(100),
        ..ModelConfig::default()
    })
    .unwrap();
    let mut events = model
        .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
        .await
        .unwrap();
    let error = time::timeout(Duration::from_secs(1), events.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();

    assert!(
        matches!(error, ModelError::Timeout { stage, .. } if stage.contains("8 body bytes received \
            previously, 0 model events"))
    );
}

#[tokio::test]
async fn preserves_unicode_split_across_network_chunks() {
    let response = concat!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"中文\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    );
    let split = response.find('中').unwrap() + 1;
    let (base_url, _) = test_server_chunks(vec![
        response.as_bytes()[..split].to_vec(),
        response.as_bytes()[split..].to_vec(),
    ])
    .await;
    let mut stream = client(&base_url, "openai_chat_completions", "mock", None)
        .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
        .await
        .unwrap();
    let mut text = String::new();

    while let Some(event) = stream.next().await {
        if let StreamEvent::TextDelta(delta) = event.unwrap() {
            text.push_str(&delta);
        }
    }

    assert_eq!(text, "中文");
}

#[tokio::test]
async fn retries_a_transient_http_failure_before_streaming_begins() {
    let (base_url, requests) = test_server_responses(vec![
        b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 10\r\n\r\ntry again!".to_vec(),
        concat!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"},\"finish_reason\":null}\
                    ]}\n\n",
            "data: [DONE]\n\n",
        )
        .as_bytes()
        .to_vec(),
    ])
    .await;
    let model = ModelClient::new(ModelConfig {
        base_url,
        max_retries: 1,
        ..ModelConfig::default()
    })
    .unwrap();

    let (mut stream, retries) = model
        .stream_with_retries(
            ChatRequest::new(vec![ChatMessage::user("hello")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(retries, 1);

    let mut text = String::new();

    while let Some(event) = stream.next().await {
        if let StreamEvent::TextDelta(delta) = event.unwrap() {
            text.push_str(&delta);
        }
    }

    assert_eq!(text, "recovered");
    assert_eq!(requests.await.expect("request count"), 2);
}

#[tokio::test]
async fn codex_missing_content_type_does_not_accept_non_sse_bodies() {
    for payload in [
        "<html>proxy error</html>",
        "{\"error\":\"unavailable\"}",
        "data: not-json\n\n",
    ] {
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{payload}",
            payload.len()
        );
        let (url, _) = test_server(response).await;
        let mut model = client(&url, "openai_responses", "mock", None);
        model.config.api_format = ApiFormat::ChatgptCodex;

        let result = model
            .complete(ChatRequest::new(vec![ChatMessage::user("hello")]))
            .await;

        assert!(result.is_err(), "unexpected success for {payload:?}");
    }
}

fn client(base_url: &str, api_format: &str, model: &str, api_key: Option<&str>) -> ModelClient {
    ModelClient::new(ModelConfig {
        api_format: ApiFormat::parse(api_format).unwrap(),
        base_url: base_url.to_owned(),
        api_key: api_key.map(str::to_owned),
        credential: None,
        model: model.to_owned(),
        timeout: Duration::from_secs(5),
        max_retries: 0,
        reasoning_effort: None,
        reasoning_adapter: None,
        reasoning_extra: serde_json::json!({}),
        supports_vision: true,
    })
    .expect("client builds")
}

async fn test_server(
    response: impl Into<String>,
) -> (String, impl std::future::Future<Output = String>) {
    test_server_chunks(vec![response.into().into_bytes()]).await
}

async fn test_server_chunks(
    chunks: Vec<Vec<u8>>,
) -> (String, impl std::future::Future<Output = String>) {
    test_server_held(chunks, Duration::ZERO).await
}

async fn test_server_responses(
    responses: Vec<Vec<u8>>,
) -> (
    String,
    impl std::future::Future<Output = Result<usize, tokio::sync::oneshot::error::RecvError>>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind test server");
    let address = listener
        .local_addr()
        .expect("local address");
    let (sender, receiver) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let mut requests = 0;

        for response in responses {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("accept request");
            read_request(&mut socket).await;

            requests += 1;
            socket
                .write_all(&response)
                .await
                .expect("write response");
            let _ = socket.shutdown().await;
        }

        let _ = sender.send(requests);
    });

    (format!("http://{address}"), receiver)
}

async fn test_server_held(
    chunks: Vec<Vec<u8>>,
    hold: Duration,
) -> (String, impl std::future::Future<Output = String>) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind test server");
    let address = listener
        .local_addr()
        .expect("local address");
    let chunks = Arc::new(chunks);
    let (sender, receiver) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener
            .accept()
            .await
            .expect("accept request");
        let bytes = read_request(&mut socket).await;

        for chunk in chunks.iter() {
            socket
                .write_all(chunk)
                .await
                .expect("write response");
            time::sleep(Duration::from_millis(10)).await;
        }

        time::sleep(hold).await;
        let _ = socket.shutdown().await;
        let _ = sender.send(String::from_utf8_lossy(&bytes).to_ascii_lowercase());
    });

    (format!("http://{address}"), async move {
        receiver
            .await
            .expect("request captured")
    })
}
