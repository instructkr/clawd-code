use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::sync::{Mutex as StdMutex, OnceLock};

use api::{
    ApiError, ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent,
    ContentBlockStopEvent, InputContentBlock, InputMessage, MessageDeltaEvent, MessageRequest,
    OpenAiCompatClient, OpenAiCompatConfig, OutputContentBlock, ProviderClient, StreamEvent,
    ToolChoice, ToolDefinition,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[tokio::test]
async fn send_message_uses_openai_compatible_endpoint_and_auth() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let body = concat!(
        "{",
        "\"id\":\"chatcmpl_test\",",
        "\"model\":\"grok-3\",",
        "\"choices\":[{",
        "\"message\":{\"role\":\"assistant\",\"content\":\"Hello from Grok\",\"tool_calls\":[]},",
        "\"finish_reason\":\"stop\"",
        "}],",
        "\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":5}",
        "}"
    );
    let server = spawn_server(
        state.clone(),
        vec![http_response("200 OK", "application/json", body)],
    )
    .await;

    let client = OpenAiCompatClient::new("xai-test-key", OpenAiCompatConfig::xai())
        .with_base_url(server.base_url());
    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("request should succeed");

    assert_eq!(response.model, "grok-3");
    assert_eq!(response.total_tokens(), 16);
    assert_eq!(
        response.content,
        vec![OutputContentBlock::Text {
            text: "Hello from Grok".to_string(),
        }]
    );

    let captured = state.lock().await;
    let request = captured.first().expect("server should capture request");
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer xai-test-key")
    );
    let body: serde_json::Value = serde_json::from_str(&request.body).expect("json body");
    assert_eq!(body["model"], json!("grok-3"));
    assert_eq!(body["messages"][0]["role"], json!("system"));
    assert_eq!(body["tools"][0]["type"], json!("function"));
}

#[tokio::test]
async fn send_message_blocks_oversized_xai_requests_before_the_http_call() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![http_response("200 OK", "application/json", "{}")],
    )
    .await;

    let client = OpenAiCompatClient::new("xai-test-key", OpenAiCompatConfig::xai())
        .with_base_url(server.base_url());
    let error = client
        .send_message(&MessageRequest {
            model: "grok-3".to_string(),
            max_tokens: 64_000,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "x".repeat(300_000),
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
async fn send_message_accepts_full_chat_completions_endpoint_override() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let body = concat!(
        "{",
        "\"id\":\"chatcmpl_full_endpoint\",",
        "\"model\":\"grok-3\",",
        "\"choices\":[{",
        "\"message\":{\"role\":\"assistant\",\"content\":\"Endpoint override works\",\"tool_calls\":[]},",
        "\"finish_reason\":\"stop\"",
        "}],",
        "\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}",
        "}"
    );
    let server = spawn_server(
        state.clone(),
        vec![http_response("200 OK", "application/json", body)],
    )
    .await;

    let endpoint_url = format!("{}/chat/completions", server.base_url());
    let client = OpenAiCompatClient::new("xai-test-key", OpenAiCompatConfig::xai())
        .with_base_url(endpoint_url);
    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("request should succeed");

    assert_eq!(response.total_tokens(), 10);

    let captured = state.lock().await;
    let request = captured.first().expect("server should capture request");
    assert_eq!(request.path, "/chat/completions");
}

#[tokio::test]
async fn stream_message_normalizes_text_and_multiple_tool_calls() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let sse = concat!(
        "data: {\"id\":\"chatcmpl_stream\",\"model\":\"grok-3\",\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl_stream\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"weather\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}},{\"index\":1,\"id\":\"call_2\",\"function\":{\"name\":\"clock\",\"arguments\":\"{\\\"zone\\\":\\\"UTC\\\"}\"}}]}}]}\n\n",
        "data: {\"id\":\"chatcmpl_stream\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let server = spawn_server(
        state.clone(),
        vec![http_response_with_headers(
            "200 OK",
            "text/event-stream",
            sse,
            &[("x-request-id", "req_grok_stream")],
        )],
    )
    .await;

    let client = OpenAiCompatClient::new("xai-test-key", OpenAiCompatConfig::xai())
        .with_base_url(server.base_url());
    let mut stream = client
        .stream_message(&sample_request(false))
        .await
        .expect("stream should start");

    assert_eq!(stream.request_id(), Some("req_grok_stream"));

    let mut events = Vec::new();
    while let Some(event) = stream.next_event().await.expect("event should parse") {
        events.push(event);
    }

    assert!(matches!(events[0], StreamEvent::MessageStart(_)));
    assert!(matches!(
        events[1],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            content_block: OutputContentBlock::Text { .. },
            ..
        })
    ));
    assert!(matches!(
        events[2],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            delta: ContentBlockDelta::TextDelta { .. },
            ..
        })
    ));
    assert!(matches!(
        events[3],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: 1,
            content_block: OutputContentBlock::ToolUse { .. },
        })
    ));
    assert!(matches!(
        events[4],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: 1,
            delta: ContentBlockDelta::InputJsonDelta { .. },
        })
    ));
    assert!(matches!(
        events[5],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: 2,
            content_block: OutputContentBlock::ToolUse { .. },
        })
    ));
    assert!(matches!(
        events[6],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: 2,
            delta: ContentBlockDelta::InputJsonDelta { .. },
        })
    ));
    assert!(matches!(
        events[7],
        StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 1 })
    ));
    assert!(matches!(
        events[8],
        StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 2 })
    ));
    assert!(matches!(
        events[9],
        StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 0 })
    ));
    assert!(matches!(events[10], StreamEvent::MessageDelta(_)));
    assert!(matches!(events[11], StreamEvent::MessageStop(_)));

    let captured = state.lock().await;
    let request = captured.first().expect("captured request");
    assert_eq!(request.path, "/chat/completions");
    assert!(request.body.contains("\"stream\":true"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn stream_message_emits_thinking_blocks_for_reasoning_content() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let sse = concat!(
        "data: {\"id\":\"chatcmpl_reasoning\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Let me think\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl_reasoning\",\"choices\":[{\"delta\":{\"reasoning_content\":\" about this...\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl_reasoning\",\"choices\":[{\"delta\":{\"content\":\"Hello!\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl_reasoning\",\"choices\":[{\"delta\":{\"content\":\" The answer is 42.\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl_reasoning\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let server = spawn_server(
        state.clone(),
        vec![http_response("200 OK", "text/event-stream", sse)],
    )
    .await;

    let client = OpenAiCompatClient::new("ds-test-key", OpenAiCompatConfig::deepseek())
        .with_base_url(server.base_url());
    let mut stream = client
        .stream_message(&sample_request(false))
        .await
        .expect("stream should start");

    let mut events = Vec::new();
    while let Some(event) = stream.next_event().await.expect("event should parse") {
        events.push(event);
    }

    // Expected event sequence: MessageStart, ThinkingStart, ThinkingDelta x2,
    // ThinkingStop, TextStart, TextDelta x2, TextStop, MessageDelta, MessageStop
    assert!(matches!(events[0], StreamEvent::MessageStart(_)));

    // Thinking block at index 0
    assert!(matches!(
        events[1],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: 0,
            content_block: OutputContentBlock::Thinking { .. },
        })
    ));
    assert!(matches!(
        events[2],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: 0,
            delta: ContentBlockDelta::ThinkingDelta { .. },
        })
    ));
    assert!(matches!(
        events[3],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: 0,
            delta: ContentBlockDelta::ThinkingDelta { .. },
        })
    ));
    // Thinking block closes before text starts
    assert!(matches!(
        events[4],
        StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 0 })
    ));

    // Text block at index 1 (shifted because thinking was present)
    assert!(matches!(
        events[5],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: 1,
            content_block: OutputContentBlock::Text { .. },
        })
    ));
    assert!(matches!(
        events[6],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: 1,
            delta: ContentBlockDelta::TextDelta { .. },
        })
    ));
    assert!(matches!(
        events[7],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: 1,
            delta: ContentBlockDelta::TextDelta { .. },
        })
    ));
    assert!(matches!(
        events[8],
        StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 1 })
    ));

    assert!(matches!(events[9], StreamEvent::MessageDelta(_)));
    assert!(matches!(events[10], StreamEvent::MessageStop(_)));

    assert_eq!(events.len(), 11);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn stream_message_emits_thinking_then_tools_without_text() {
    // Reasoning-only stream: reasoning_content → tool_calls → finish, no text.
    // Thinking block should be index 0, tools should start at index 2
    // (1+thinking_offset).
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let sse = concat!(
        "data: {\"id\":\"chatcmpl_reasoning_tool\",\"choices\":[{\"delta\":{\"reasoning_content\":\"I need to use a tool.\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl_reasoning_tool\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"weather\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}}]}}]}\n\n",
        "data: {\"id\":\"chatcmpl_reasoning_tool\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let server = spawn_server(
        state.clone(),
        vec![http_response("200 OK", "text/event-stream", sse)],
    )
    .await;

    let client = OpenAiCompatClient::new("ds-test-key", OpenAiCompatConfig::deepseek())
        .with_base_url(server.base_url());
    let mut stream = client
        .stream_message(&sample_request(false))
        .await
        .expect("stream should start");

    let mut events = Vec::new();
    while let Some(event) = stream.next_event().await.expect("event should parse") {
        events.push(event);
    }

    // Thinking block stays open through tool calls; closes in finish().
    // Expected: MessageStart, ThinkingStart(idx 0), ThinkingDelta,
    // ToolStart(idx 2), ToolDelta, ToolStop(idx 2),
    // ThinkingStop(idx 0) [from finish], MessageDelta, MessageStop
    assert!(matches!(events[0], StreamEvent::MessageStart(_)));

    // Thinking block at index 0
    assert!(matches!(
        events[1],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: 0,
            content_block: OutputContentBlock::Thinking { .. },
        })
    ));
    assert!(matches!(
        events[2],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: 0,
            delta: ContentBlockDelta::ThinkingDelta { .. },
        })
    ));

    // Tool call at index 2 (shifted: thinking_offset=1, so 0 + 1 + 1 = 2)
    assert!(matches!(
        events[3],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: 2,
            content_block: OutputContentBlock::ToolUse { .. },
        })
    ));
    assert!(matches!(
        events[4],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: 2,
            delta: ContentBlockDelta::InputJsonDelta { .. },
        })
    ));
    assert!(matches!(
        events[5],
        StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 2 })
    ));

    // Thinking block closes in finish(), after tool calls
    assert!(matches!(
        events[6],
        StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 0 })
    ));

    assert!(matches!(events[7], StreamEvent::MessageDelta(_)));
    assert!(matches!(events[8], StreamEvent::MessageStop(_)));

    assert_eq!(events.len(), 9);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn openai_streaming_requests_opt_into_usage_chunks() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let sse = concat!(
        "data: {\"id\":\"chatcmpl_openai_stream\",\"model\":\"gpt-5\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl_openai_stream\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"chatcmpl_openai_stream\",\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":4}}\n\n",
        "data: [DONE]\n\n"
    );
    let server = spawn_server(
        state.clone(),
        vec![http_response_with_headers(
            "200 OK",
            "text/event-stream",
            sse,
            &[("x-request-id", "req_openai_stream")],
        )],
    )
    .await;

    let client = OpenAiCompatClient::new("openai-test-key", OpenAiCompatConfig::openai())
        .with_base_url(server.base_url());
    let mut stream = client
        .stream_message(&sample_request(false))
        .await
        .expect("stream should start");

    assert_eq!(stream.request_id(), Some("req_openai_stream"));

    let mut events = Vec::new();
    while let Some(event) = stream.next_event().await.expect("event should parse") {
        events.push(event);
    }

    assert!(matches!(events[0], StreamEvent::MessageStart(_)));
    assert!(matches!(
        events[1],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            content_block: OutputContentBlock::Text { .. },
            ..
        })
    ));
    assert!(matches!(
        events[2],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            delta: ContentBlockDelta::TextDelta { .. },
            ..
        })
    ));
    assert!(matches!(
        events[3],
        StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 0 })
    ));
    assert!(matches!(
        events[4],
        StreamEvent::MessageDelta(MessageDeltaEvent { .. })
    ));
    assert!(matches!(events[5], StreamEvent::MessageStop(_)));

    match &events[4] {
        StreamEvent::MessageDelta(MessageDeltaEvent { usage, .. }) => {
            assert_eq!(usage.input_tokens, 9);
            assert_eq!(usage.output_tokens, 4);
        }
        other => panic!("expected message delta, got {other:?}"),
    }

    let captured = state.lock().await;
    let request = captured.first().expect("captured request");
    assert_eq!(request.path, "/chat/completions");
    let body: serde_json::Value = serde_json::from_str(&request.body).expect("json body");
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn provider_client_dispatches_xai_requests_from_env() {
    let _lock = env_lock();
    let _api_key = ScopedEnvVar::set("XAI_API_KEY", "xai-test-key");

    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![http_response(
            "200 OK",
            "application/json",
            "{\"id\":\"chatcmpl_provider\",\"model\":\"grok-3\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"Through provider client\",\"tool_calls\":[]},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":4}}",
        )],
    )
    .await;
    let _base_url = ScopedEnvVar::set("XAI_BASE_URL", server.base_url());

    let client =
        ProviderClient::from_model("grok").expect("xAI provider client should be constructed");
    assert!(matches!(client, ProviderClient::Xai(_)));

    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("provider-dispatched request should succeed");

    assert_eq!(response.total_tokens(), 13);

    let captured = state.lock().await;
    let request = captured.first().expect("captured request");
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer xai-test-key")
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedRequest {
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
    let address = listener.local_addr().expect("listener addr");
    let join_handle = tokio::spawn(async move {
        for response in responses {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buffer = Vec::new();
            let mut header_end = None;
            loop {
                let mut chunk = [0_u8; 1024];
                let read = socket.read(&mut chunk).await.expect("read request");
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(position) = find_header_end(&buffer) {
                    header_end = Some(position);
                    break;
                }
            }

            let header_end = header_end.expect("headers should exist");
            let (header_bytes, remaining) = buffer.split_at(header_end);
            let header_text = String::from_utf8(header_bytes.to_vec()).expect("utf8 headers");
            let mut lines = header_text.split("\r\n");
            let request_line = lines.next().expect("request line");
            let path = request_line
                .split_whitespace()
                .nth(1)
                .expect("path")
                .to_string();
            let mut headers = HashMap::new();
            let mut content_length = 0_usize;
            for line in lines {
                if line.is_empty() {
                    continue;
                }
                let (name, value) = line.split_once(':').expect("header");
                let value = value.trim().to_string();
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.parse().expect("content length");
                }
                headers.insert(name.to_ascii_lowercase(), value);
            }

            let mut body = remaining[4..].to_vec();
            while body.len() < content_length {
                let mut chunk = vec![0_u8; content_length - body.len()];
                let read = socket.read(&mut chunk).await.expect("read body");
                if read == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..read]);
            }

            state.lock().await.push(CapturedRequest {
                path,
                headers,
                body: String::from_utf8(body).expect("utf8 body"),
            });

            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
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
        write!(&mut extra_headers, "{name}: {value}\r\n").expect("header write");
    }
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn sample_request(stream: bool) -> MessageRequest {
    MessageRequest {
        model: "grok-3".to_string(),
        max_tokens: 64,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text {
                text: "Say hello".to_string(),
            }],
        }],
        system: Some("Use tools when needed".to_string()),
        tools: Some(vec![ToolDefinition {
            name: "weather".to_string(),
            description: Some("Fetches weather".to_string()),
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

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<OsString>,
}

impl ScopedEnvVar {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
