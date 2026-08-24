use std::collections::HashMap;
use std::sync::Arc;
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::Duration;

use api::{
    AnthropicClient, ApiClient, ApiError, AuthSource, ContentBlockDelta, ContentBlockDeltaEvent,
    ContentBlockStartEvent, InputContentBlock, InputMessage, MessageDeltaEvent, MessageRequest,
    OutputContentBlock, PromptCache, PromptCacheConfig, ProviderClient, StreamEvent, ToolChoice,
    ToolDefinition,
};
use serde_json::json;
use telemetry::{ClientIdentity, MemoryTelemetrySink, SessionTracer, TelemetryEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[tokio::test]
async fn send_message_posts_json_and_parses_response() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let body = concat!(
        "{",
        "\"id\":\"msg_test\",",
        "\"type\":\"message\",",
        "\"role\":\"assistant\",",
        "\"content\":[{\"type\":\"text\",\"text\":\"Hello from Claude\"}],",
        "\"model\":\"claude-3-7-sonnet-latest\",",
        "\"stop_reason\":\"end_turn\",",
        "\"stop_sequence\":null,",
        "\"usage\":{\"input_tokens\":12,\"output_tokens\":4},",
        "\"request_id\":\"req_body_123\"",
        "}"
    );
    let server = spawn_server(
        state.clone(),
        vec![http_response("200 OK", "application/json", body)],
    )
    .await;

    let client = ApiClient::new("test-key")
        .with_auth_token(Some("proxy-token".to_string()))
        .with_base_url(server.base_url());
    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("request should succeed");

    assert_eq!(response.id, "msg_test");
    assert_eq!(response.total_tokens(), 16);
    assert_eq!(response.request_id.as_deref(), Some("req_body_123"));
    assert_eq!(response.usage.cache_creation_input_tokens, 0);
    assert_eq!(response.usage.cache_read_input_tokens, 0);
    assert_eq!(
        response.content,
        vec![OutputContentBlock::Text {
            text: "Hello from Claude".to_string(),
        }]
    );

    let captured = state.lock().await;
    let request = captured.first().expect("server should capture request");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/messages");
    assert_eq!(
        request.headers.get("x-api-key").map(String::as_str),
        Some("test-key")
    );
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer proxy-token")
    );
    assert_eq!(
        request.headers.get("anthropic-version").map(String::as_str),
        Some("2023-06-01")
    );
    assert_eq!(
        request.headers.get("user-agent").map(String::as_str),
        Some("claude-code/0.1.3")
    );
    assert_eq!(
        request.headers.get("anthropic-beta").map(String::as_str),
        Some("claude-code-20250219,prompt-caching-scope-2026-01-05")
    );
    let body: serde_json::Value =
        serde_json::from_str(&request.body).expect("request body should be json");
    assert_eq!(
        body.get("model").and_then(serde_json::Value::as_str),
        Some("claude-3-7-sonnet-latest")
    );
    assert!(body.get("stream").is_none());
    assert_eq!(body["tools"][0]["name"], json!("get_weather"));
    assert_eq!(body["tool_choice"]["type"], json!("auto"));
    assert!(
        body.get("betas").is_none(),
        "betas must travel via the anthropic-beta header, not the request body"
    );
}

#[tokio::test]
async fn send_message_strips_anthropic_routing_prefix_on_wire() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![
            http_response("200 OK", "application/json", "{\"input_tokens\":1}"),
            http_response(
                "200 OK",
                "application/json",
                concat!(
                    "{",
                    "\"id\":\"msg_prefixed\",",
                    "\"type\":\"message\",",
                    "\"role\":\"assistant\",",
                    "\"content\":[{\"type\":\"text\",\"text\":\"ok\"}],",
                    "\"model\":\"claude-opus-4-6\",",
                    "\"stop_reason\":\"end_turn\",",
                    "\"stop_sequence\":null,",
                    "\"usage\":{\"input_tokens\":1,\"output_tokens\":1}",
                    "}"
                ),
            ),
        ],
    )
    .await;

    let client = AnthropicClient::new("test-key").with_base_url(server.base_url());
    client
        .send_message(&MessageRequest {
            model: "anthropic/claude-opus-4-6".to_string(),
            ..sample_request(false)
        })
        .await
        .expect("request should succeed");

    let captured = state.lock().await;
    assert_eq!(
        captured.len(),
        2,
        "count_tokens and messages requests should be captured"
    );
    let count_tokens_body: serde_json::Value =
        serde_json::from_str(&captured[0].body).expect("count_tokens body should be json");
    let messages_body: serde_json::Value =
        serde_json::from_str(&captured[1].body).expect("request body should be json");
    assert_eq!(captured[0].path, "/v1/messages/count_tokens");
    assert_eq!(captured[1].path, "/v1/messages");
    assert_eq!(count_tokens_body["model"], json!("claude-opus-4-6"));
    assert_eq!(messages_body["model"], json!("claude-opus-4-6"));
}

#[tokio::test]
async fn send_message_blocks_oversized_requests_before_the_http_call() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![http_response("200 OK", "application/json", "{}")],
    )
    .await;

    let client = AnthropicClient::new("test-key").with_base_url(server.base_url());
    let error = client
        .send_message(&MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 64_000,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "x".repeat(600_000),
                }],
            }],
            system: Some("Keep the answer short.".to_string()),
            tools: None,
            tool_choice: None,
            stream: false,
            ..Default::default()
        })
        .await
        .expect_err("oversized request should fail local context-window preflight");

    assert!(matches!(error, ApiError::ContextWindowExceeded { .. }));
    assert!(
        state.lock().await.is_empty(),
        "preflight failure should avoid any upstream HTTP request"
    );
}

#[tokio::test]
async fn send_message_applies_request_profile_and_records_telemetry() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![http_response_with_headers(
            "200 OK",
            "application/json",
            concat!(
                "{",
                "\"id\":\"msg_profile\",",
                "\"type\":\"message\",",
                "\"role\":\"assistant\",",
                "\"content\":[{\"type\":\"text\",\"text\":\"ok\"}],",
                "\"model\":\"claude-3-7-sonnet-latest\",",
                "\"stop_reason\":\"end_turn\",",
                "\"stop_sequence\":null,",
                "\"usage\":{\"input_tokens\":1,\"cache_creation_input_tokens\":2,\"cache_read_input_tokens\":3,\"output_tokens\":1}",
                "}"
            ),
            &[("request-id", "req_profile_123")],
        )],
    )
    .await;
    let sink = Arc::new(MemoryTelemetrySink::default());

    let client = AnthropicClient::new("test-key")
        .with_base_url(server.base_url())
        .with_client_identity(ClientIdentity::new("claude-code", "9.9.9").with_runtime("rust-cli"))
        .with_beta("tools-2026-04-01")
        .with_extra_body_param("metadata", json!({"source": "clawd-code"}))
        .with_session_tracer(SessionTracer::new("session-telemetry", sink.clone()));

    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("request should succeed");

    assert_eq!(response.request_id.as_deref(), Some("req_profile_123"));

    let captured = state.lock().await;
    let request = captured.first().expect("server should capture request");
    assert_eq!(
        request.headers.get("anthropic-beta").map(String::as_str),
        Some("claude-code-20250219,prompt-caching-scope-2026-01-05,tools-2026-04-01")
    );
    assert_eq!(
        request.headers.get("user-agent").map(String::as_str),
        Some("claude-code/9.9.9")
    );
    let body: serde_json::Value =
        serde_json::from_str(&request.body).expect("request body should be json");
    assert_eq!(body["metadata"]["source"], json!("clawd-code"));
    assert!(
        body.get("betas").is_none(),
        "betas must travel via the anthropic-beta header, not the request body"
    );

    let events = sink.events();
    assert_eq!(events.len(), 6);
    assert!(matches!(
        &events[0],
        TelemetryEvent::HttpRequestStarted {
            session_id,
            attempt: 1,
            method,
            path,
            ..
        } if session_id == "session-telemetry" && method == "POST" && path == "/v1/messages"
    ));
    assert!(matches!(
        &events[1],
        TelemetryEvent::SessionTrace(trace) if trace.name == "http_request_started"
    ));
    assert!(matches!(
        &events[2],
        TelemetryEvent::HttpRequestSucceeded {
            request_id,
            status: 200,
            ..
        } if request_id.as_deref() == Some("req_profile_123")
    ));
    assert!(matches!(
        &events[3],
        TelemetryEvent::SessionTrace(trace) if trace.name == "http_request_succeeded"
    ));
    assert!(matches!(
        &events[4],
        TelemetryEvent::Analytics(event)
            if event.namespace == "api"
                && event.action == "message_usage"
                && event.properties.get("request_id") == Some(&json!("req_profile_123"))
                && event.properties.get("total_tokens") == Some(&json!(7))
                && event.properties.get("estimated_cost_usd") == Some(&json!("$0.0001"))
    ));
    assert!(matches!(
        &events[5],
        TelemetryEvent::SessionTrace(trace) if trace.name == "analytics"
    ));
}

#[tokio::test]
async fn send_message_parses_prompt_cache_token_usage_from_response() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let body = concat!(
        "{",
        "\"id\":\"msg_cache_tokens\",",
        "\"type\":\"message\",",
        "\"role\":\"assistant\",",
        "\"content\":[{\"type\":\"text\",\"text\":\"Cache tokens\"}],",
        "\"model\":\"claude-3-7-sonnet-latest\",",
        "\"stop_reason\":\"end_turn\",",
        "\"stop_sequence\":null,",
        "\"usage\":{\"input_tokens\":12,\"cache_creation_input_tokens\":321,\"cache_read_input_tokens\":654,\"output_tokens\":4}",
        "}"
    );
    let server = spawn_server(
        state,
        vec![http_response("200 OK", "application/json", body)],
    )
    .await;

    let client = AnthropicClient::new("test-key").with_base_url(server.base_url());
    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("request should succeed");

    assert_eq!(response.usage.input_tokens, 12);
    assert_eq!(response.usage.cache_creation_input_tokens, 321);
    assert_eq!(response.usage.cache_read_input_tokens, 654);
    assert_eq!(response.usage.output_tokens, 4);
}

#[tokio::test]
async fn given_empty_usage_object_when_send_message_parses_response_then_usage_defaults_to_zero() {
    // given
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let body = concat!(
        "{",
        "\"id\":\"msg_empty_usage\",",
        "\"type\":\"message\",",
        "\"role\":\"assistant\",",
        "\"content\":[{\"type\":\"text\",\"text\":\"Hello from Claude\"}],",
        "\"model\":\"claude-3-7-sonnet-latest\",",
        "\"stop_reason\":\"end_turn\",",
        "\"stop_sequence\":null,",
        "\"usage\":{}",
        "}"
    );
    let server = spawn_server(
        state,
        vec![http_response("200 OK", "application/json", body)],
    )
    .await;
    let client = AnthropicClient::new("test-key").with_base_url(server.base_url());

    // when
    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("response with empty usage object should still parse");

    // then
    assert_eq!(response.id, "msg_empty_usage");
    assert_eq!(response.total_tokens(), 0);
    assert_eq!(response.usage.input_tokens, 0);
    assert_eq!(response.usage.cache_creation_input_tokens, 0);
    assert_eq!(response.usage.cache_read_input_tokens, 0);
    assert_eq!(response.usage.output_tokens, 0);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn stream_message_parses_sse_events_with_tool_use() {
    let _guard = env_lock();
    let temp_root = std::env::temp_dir().join(format!(
        "api-stream-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::env::set_var("CLAUDE_CONFIG_HOME", &temp_root);
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_stream\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-3-7-sonnet-latest\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":8,\"cache_creation_input_tokens\":13,\"cache_read_input_tokens\":21,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_123\",\"name\":\"get_weather\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\\\"Paris\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":8,\"cache_creation_input_tokens\":34,\"cache_read_input_tokens\":55,\"output_tokens\":1}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
        "data: [DONE]\n\n"
    );
    let server = spawn_server(
        state.clone(),
        vec![http_response_with_headers(
            "200 OK",
            "text/event-stream",
            sse,
            &[("request-id", "req_stream_456")],
        )],
    )
    .await;

    let client = ApiClient::new("test-key")
        .with_auth_token(Some("proxy-token".to_string()))
        .with_base_url(server.base_url())
        .with_prompt_cache(PromptCache::new("stream-session"));
    let mut stream = client
        .stream_message(&sample_request(false))
        .await
        .expect("stream should start");

    assert_eq!(stream.request_id(), Some("req_stream_456"));

    let mut events = Vec::new();
    while let Some(event) = stream
        .next_event()
        .await
        .expect("stream event should parse")
    {
        events.push(event);
    }

    assert_eq!(events.len(), 6);
    assert!(matches!(events[0], StreamEvent::MessageStart(_)));
    assert!(matches!(
        events[1],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            content_block: OutputContentBlock::ToolUse { .. },
            ..
        })
    ));
    assert!(matches!(
        events[2],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            delta: ContentBlockDelta::InputJsonDelta { .. },
            ..
        })
    ));
    assert!(matches!(events[3], StreamEvent::ContentBlockStop(_)));
    assert!(matches!(
        events[4],
        StreamEvent::MessageDelta(MessageDeltaEvent { .. })
    ));
    assert!(matches!(events[5], StreamEvent::MessageStop(_)));

    match &events[1] {
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            content_block: OutputContentBlock::ToolUse { name, input, .. },
            ..
        }) => {
            assert_eq!(name, "get_weather");
            assert_eq!(input, &json!({}));
        }
        other => panic!("expected tool_use block, got {other:?}"),
    }

    let captured = state.lock().await;
    let request = captured.first().expect("server should capture request");
    assert!(request.body.contains("\"stream\":true"));

    let cache_stats = client
        .prompt_cache_stats()
        .expect("prompt cache stats should exist");
    assert_eq!(cache_stats.tracked_requests, 1);
    assert_eq!(cache_stats.last_cache_creation_input_tokens, Some(34));
    assert_eq!(cache_stats.last_cache_read_input_tokens, Some(55));
    assert_eq!(
        cache_stats.last_cache_source.as_deref(),
        Some("api-response")
    );

    std::fs::remove_dir_all(temp_root).expect("cleanup temp root");
    std::env::remove_var("CLAUDE_CONFIG_HOME");
}

#[tokio::test]
async fn retries_retryable_failures_before_succeeding() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![
            http_response(
                "429 Too Many Requests",
                "application/json",
                "{\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"slow down\"}}",
            ),
            http_response(
                "200 OK",
                "application/json",
                "{\"id\":\"msg_retry\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Recovered\"}],\"model\":\"claude-3-7-sonnet-latest\",\"stop_reason\":\"end_turn\",\"stop_sequence\":null,\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}",
            ),
        ],
    )
    .await;

    let client = ApiClient::new("test-key")
        .with_base_url(server.base_url())
        .with_retry_policy(2, Duration::from_millis(1), Duration::from_millis(2));

    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("retry should eventually succeed");

    assert_eq!(response.total_tokens(), 5);
    assert_eq!(state.lock().await.len(), 2);
}

#[tokio::test]
async fn provider_client_dispatches_anthropic_requests() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![http_response(
            "200 OK",
            "application/json",
            "{\"id\":\"msg_provider\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Dispatched\"}],\"model\":\"claude-3-7-sonnet-latest\",\"stop_reason\":\"end_turn\",\"stop_sequence\":null,\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}",
        )],
    )
    .await;

    let client = ProviderClient::from_model_with_anthropic_auth(
        "claude-sonnet-4-6",
        Some(AuthSource::ApiKey("test-key".to_string())),
    )
    .expect("anthropic provider client should be constructed");
    let client = match client {
        ProviderClient::Anthropic(client) => {
            ProviderClient::Anthropic(client.with_base_url(server.base_url()))
        }
        other => panic!("expected anthropic provider, got {other:?}"),
    };

    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("provider-dispatched request should succeed");

    assert_eq!(response.total_tokens(), 5);

    let captured = state.lock().await;
    let request = captured.first().expect("server should capture request");
    assert_eq!(request.path, "/v1/messages");
    assert_eq!(
        request.headers.get("x-api-key").map(String::as_str),
        Some("test-key")
    );
}

#[tokio::test]
async fn surfaces_retry_exhaustion_for_persistent_retryable_errors() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![
            http_response(
                "503 Service Unavailable",
                "application/json",
                "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}",
            ),
            http_response(
                "503 Service Unavailable",
                "application/json",
                "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"still busy\"}}",
            ),
        ],
    )
    .await;

    let client = ApiClient::new("test-key")
        .with_base_url(server.base_url())
        .with_retry_policy(1, Duration::from_millis(1), Duration::from_millis(2));

    let error = client
        .send_message(&sample_request(false))
        .await
        .expect_err("persistent 503 should fail");

    match error {
        ApiError::RetriesExhausted {
            attempts,
            last_error,
        } => {
            assert_eq!(attempts, 2);
            assert!(matches!(
                *last_error,
                ApiError::Api {
                    status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                    retryable: true,
                    ..
                }
            ));
        }
        other => panic!("expected retries exhausted, got {other:?}"),
    }
}

#[tokio::test]
async fn retries_multiple_retryable_failures_with_exponential_backoff_and_jitter() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![
            http_response(
                "429 Too Many Requests",
                "application/json",
                "{\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"slow down\"}}",
            ),
            http_response(
                "500 Internal Server Error",
                "application/json",
                "{\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"boom\"}}",
            ),
            http_response(
                "503 Service Unavailable",
                "application/json",
                "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}",
            ),
            http_response(
                "429 Too Many Requests",
                "application/json",
                "{\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"slow down again\"}}",
            ),
            http_response(
                "503 Service Unavailable",
                "application/json",
                "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"still busy\"}}",
            ),
            http_response(
                "200 OK",
                "application/json",
                "{\"id\":\"msg_exp_retry\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Recovered after 5\"}],\"model\":\"claude-3-7-sonnet-latest\",\"stop_reason\":\"end_turn\",\"stop_sequence\":null,\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}",
            ),
        ],
    )
    .await;

    let client = ApiClient::new("test-key")
        .with_base_url(server.base_url())
        .with_retry_policy(8, Duration::from_millis(1), Duration::from_millis(4));
    let started_at = std::time::Instant::now();

    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("8-retry policy should absorb 5 retryable failures");

    let elapsed = started_at.elapsed();
    assert_eq!(response.total_tokens(), 5);
    assert_eq!(
        state.lock().await.len(),
        6,
        "client should issue 1 original + 5 retry requests before the 200"
    );
    // Jittered sleeps are bounded by 2 * max_backoff per retry (base + jitter),
    // so 5 sleeps fit comfortably below this upper bound with generous slack.
    assert!(
        elapsed < Duration::from_secs(5),
        "retries should complete promptly, took {elapsed:?}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn send_message_reuses_recent_completion_cache_entries() {
    let _guard = env_lock();
    let temp_root = std::env::temp_dir().join(format!(
        "api-prompt-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::env::set_var("CLAUDE_CONFIG_HOME", &temp_root);

    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![http_response(
            "200 OK",
            "application/json",
            "{\"id\":\"msg_cached\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Cached once\"}],\"model\":\"claude-3-7-sonnet-latest\",\"stop_reason\":\"end_turn\",\"stop_sequence\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":5,\"cache_read_input_tokens\":4000,\"output_tokens\":2}}",
        )],
    )
    .await;

    let client = AnthropicClient::new("test-key")
        .with_base_url(server.base_url())
        .with_prompt_cache(PromptCache::new("integration-session"));

    let first = client
        .send_message(&sample_request(false))
        .await
        .expect("first request should succeed");
    let second = client
        .send_message(&sample_request(false))
        .await
        .expect("second request should reuse cache");

    assert_eq!(first.content, second.content);
    assert_eq!(state.lock().await.len(), 1);

    let cache_stats = client
        .prompt_cache_stats()
        .expect("prompt cache stats should exist");
    assert_eq!(cache_stats.completion_cache_hits, 1);
    assert_eq!(cache_stats.completion_cache_misses, 1);
    assert_eq!(cache_stats.completion_cache_writes, 1);

    std::fs::remove_dir_all(temp_root).expect("cleanup temp root");
    std::env::remove_var("CLAUDE_CONFIG_HOME");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn send_message_tracks_unexpected_prompt_cache_breaks() {
    let _guard = env_lock();
    let temp_root = std::env::temp_dir().join(format!(
        "api-prompt-break-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::env::set_var("CLAUDE_CONFIG_HOME", &temp_root);

    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state,
        vec![
            http_response(
                "200 OK",
                "application/json",
                "{\"id\":\"msg_one\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"One\"}],\"model\":\"claude-3-7-sonnet-latest\",\"stop_reason\":\"end_turn\",\"stop_sequence\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":5,\"cache_read_input_tokens\":6000,\"output_tokens\":2}}",
            ),
            http_response(
                "200 OK",
                "application/json",
                "{\"id\":\"msg_two\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Two\"}],\"model\":\"claude-3-7-sonnet-latest\",\"stop_reason\":\"end_turn\",\"stop_sequence\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":1000,\"output_tokens\":2}}",
            ),
        ],
    )
    .await;

    let request = sample_request(false);
    let client = AnthropicClient::new("test-key")
        .with_base_url(server.base_url())
        .with_prompt_cache(PromptCache::with_config(PromptCacheConfig {
            session_id: "break-session".to_string(),
            completion_ttl: Duration::from_secs(0),
            ..PromptCacheConfig::default()
        }));

    client
        .send_message(&request)
        .await
        .expect("first response should succeed");
    client
        .send_message(&request)
        .await
        .expect("second response should succeed");

    let cache_stats = client
        .prompt_cache_stats()
        .expect("prompt cache stats should exist");
    assert_eq!(cache_stats.unexpected_cache_breaks, 1);
    assert_eq!(
        cache_stats.last_break_reason.as_deref(),
        Some("cache read tokens dropped while prompt fingerprint remained stable")
    );

    std::fs::remove_dir_all(temp_root).expect("cleanup temp root");
    std::env::remove_var("CLAUDE_CONFIG_HOME");
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY and network access"]
async fn live_stream_smoke_test() {
    let client = ApiClient::from_env().expect("ANTHROPIC_API_KEY must be set");
    let mut stream = client
        .stream_message(&MessageRequest {
            model: std::env::var("ANTHROPIC_MODEL")
                .unwrap_or_else(|_| "claude-3-7-sonnet-latest".to_string()),
            max_tokens: 32,
            messages: vec![InputMessage::user_text(
                "Reply with exactly: hello from rust",
            )],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            ..Default::default()
        })
        .await
        .expect("live stream should start");

    while let Some(_event) = stream
        .next_event()
        .await
        .expect("live stream should yield events")
    {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

struct TestServer {
    base_url: String,
    join_handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    fn base_url(&self) -> String {
        self.base_url.clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.join_handle.abort();
    }
}

async fn spawn_server(
    state: Arc<Mutex<Vec<CapturedRequest>>>,
    responses: Vec<String>,
) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should have local addr");
    let join_handle = tokio::spawn(async move {
        for response in responses {
            let (mut socket, _) = listener.accept().await.expect("server should accept");
            let mut buffer = Vec::new();
            let mut header_end = None;

            loop {
                let mut chunk = [0_u8; 1024];
                let read = socket
                    .read(&mut chunk)
                    .await
                    .expect("request read should succeed");
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(position) = find_header_end(&buffer) {
                    header_end = Some(position);
                    break;
                }
            }

            let header_end = header_end.expect("request should include headers");
            let (header_bytes, remaining) = buffer.split_at(header_end);
            let header_text =
                String::from_utf8(header_bytes.to_vec()).expect("headers should be utf8");
            let mut lines = header_text.split("\r\n");
            let request_line = lines.next().expect("request line should exist");
            let mut parts = request_line.split_whitespace();
            let method = parts.next().expect("method should exist").to_string();
            let path = parts.next().expect("path should exist").to_string();
            let mut headers = HashMap::new();
            let mut content_length = 0_usize;
            for line in lines {
                if line.is_empty() {
                    continue;
                }
                let (name, value) = line.split_once(':').expect("header should have colon");
                let value = value.trim().to_string();
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.parse().expect("content length should parse");
                }
                headers.insert(name.to_ascii_lowercase(), value);
            }

            let mut body = remaining[4..].to_vec();
            while body.len() < content_length {
                let mut chunk = vec![0_u8; content_length - body.len()];
                let read = socket
                    .read(&mut chunk)
                    .await
                    .expect("body read should succeed");
                if read == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..read]);
            }

            state.lock().await.push(CapturedRequest {
                method,
                path,
                headers,
                body: String::from_utf8(body).expect("body should be utf8"),
            });

            socket
                .write_all(response.as_bytes())
                .await
                .expect("response write should succeed");
        }
    });

    TestServer {
        base_url: format!("http://{address}"),
        join_handle,
    }
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn http_response(status: &str, content_type: &str, body: &str) -> String {
    http_response_with_headers(status, content_type, body, &[])
}

fn http_response_with_headers(
    status: &str,
    content_type: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> String {
    let mut extra_headers = String::new();
    for (name, value) in headers {
        use std::fmt::Write as _;
        write!(&mut extra_headers, "{name}: {value}\r\n").expect("header write should succeed");
    }
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn sample_request(stream: bool) -> MessageRequest {
    MessageRequest {
        model: "claude-3-7-sonnet-latest".to_string(),
        max_tokens: 64,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![
                InputContentBlock::Text {
                    text: "Say hello".to_string(),
                },
                InputContentBlock::ToolResult {
                    tool_use_id: "toolu_prev".to_string(),
                    content: vec![api::ToolResultContentBlock::Json {
                        value: json!({"forecast": "sunny"}),
                    }],
                    is_error: false,
                },
            ],
        }],
        system: Some("Use tools when needed".to_string()),
        tools: Some(vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: Some("Fetches the weather".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
        }]),
        tool_choice: Some(ToolChoice::Auto),
        stream,
        ..Default::default()
    }
}
