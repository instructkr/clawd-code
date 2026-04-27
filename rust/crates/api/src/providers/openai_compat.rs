use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::http_client::build_http_client_or_default;
use crate::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent, MessageRequest,
    MessageResponse, MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent,
    ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};

use super::{preflight_message_request, Provider, ProviderFuture};

pub const DEFAULT_XAI_BASE_URL: &str = "https://api.x.ai/v1";
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_DASHSCOPE_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
const REQUEST_ID_HEADER: &str = "request-id";
const ALT_REQUEST_ID_HEADER: &str = "x-request-id";
const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(128);
const DEFAULT_MAX_RETRIES: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAiCompatConfig {
    pub provider_name: &'static str,
    pub api_key_env: &'static str,
    pub base_url_env: &'static str,
    pub default_base_url: &'static str,
    /// Maximum request body size in bytes. Provider-specific limits:
    /// - `DashScope`: 6MB (`6_291_456` bytes) - observed in dogfood testing
    /// - `OpenAI`: 100MB (`104_857_600` bytes)
    /// - `xAI`: 50MB (`52_428_800` bytes)
    pub max_request_body_bytes: usize,
}

const XAI_ENV_VARS: &[&str] = &["XAI_API_KEY"];
const OPENAI_ENV_VARS: &[&str] = &["OPENAI_API_KEY"];
const DASHSCOPE_ENV_VARS: &[&str] = &["DASHSCOPE_API_KEY"];

// Provider-specific request body size limits in bytes
const XAI_MAX_REQUEST_BODY_BYTES: usize = 52_428_800; // 50MB
const OPENAI_MAX_REQUEST_BODY_BYTES: usize = 104_857_600; // 100MB
const DASHSCOPE_MAX_REQUEST_BODY_BYTES: usize = 6_291_456; // 6MB (observed limit in dogfood)

impl OpenAiCompatConfig {
    #[must_use]
    pub const fn xai() -> Self {
        Self {
            provider_name: "xAI",
            api_key_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: DEFAULT_XAI_BASE_URL,
            max_request_body_bytes: XAI_MAX_REQUEST_BODY_BYTES,
        }
    }

    #[must_use]
    pub const fn openai() -> Self {
        Self {
            provider_name: "OpenAI",
            api_key_env: "OPENAI_API_KEY",
            base_url_env: "OPENAI_BASE_URL",
            default_base_url: DEFAULT_OPENAI_BASE_URL,
            max_request_body_bytes: OPENAI_MAX_REQUEST_BODY_BYTES,
        }
    }

    /// Alibaba `DashScope` compatible-mode endpoint (Qwen family models).
    /// Uses the OpenAI-compatible REST shape at /compatible-mode/v1.
    /// Requested via Discord #clawcode-get-help: native Alibaba API for
    /// higher rate limits than going through `OpenRouter`.
    #[must_use]
    pub const fn dashscope() -> Self {
        Self {
            provider_name: "DashScope",
            api_key_env: "DASHSCOPE_API_KEY",
            base_url_env: "DASHSCOPE_BASE_URL",
            default_base_url: DEFAULT_DASHSCOPE_BASE_URL,
            max_request_body_bytes: DASHSCOPE_MAX_REQUEST_BODY_BYTES,
        }
    }

    #[must_use]
    pub fn credential_env_vars(self) -> &'static [&'static str] {
        match self.provider_name {
            "xAI" => XAI_ENV_VARS,
            "OpenAI" => OPENAI_ENV_VARS,
            "DashScope" => DASHSCOPE_ENV_VARS,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatClient {
    http: reqwest::Client,
    api_key: String,
    config: OpenAiCompatConfig,
    base_url: String,
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl OpenAiCompatClient {
    const fn config(&self) -> OpenAiCompatConfig {
        self.config
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
    #[must_use]
    pub fn new(api_key: impl Into<String>, config: OpenAiCompatConfig) -> Self {
        Self {
            http: build_http_client_or_default(),
            api_key: api_key.into(),
            config,
            base_url: read_base_url(config),
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
        }
    }

    pub fn from_env(config: OpenAiCompatConfig) -> Result<Self, ApiError> {
        let Some(api_key) = read_env_non_empty(config.api_key_env)? else {
            return Err(ApiError::missing_credentials(
                config.provider_name,
                config.credential_env_vars(),
            ));
        };
        Ok(Self::new(api_key, config))
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    #[must_use]
    pub fn with_retry_policy(
        mut self,
        max_retries: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        self.max_retries = max_retries;
        self.initial_backoff = initial_backoff;
        self.max_backoff = max_backoff;
        self
    }

    /// Replace the internal HTTP client with one that respects the given
    /// timeout configuration.
    #[must_use]
    pub fn with_timeout(mut self, timeout: &crate::http_client::TimeoutConfig) -> Self {
        self.http = crate::http_client::build_http_client_with_opts(
            &crate::http_client::ProxyConfig::from_env(),
            timeout,
        )
        .unwrap_or_else(|_| reqwest::Client::new());
        self
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let request = MessageRequest {
            stream: false,
            ..request.clone()
        };
        preflight_message_request(&request)?;
        let response = self.send_with_retry(&request).await?;
        let request_id = request_id_from_headers(response.headers());
        let body = response.text().await.map_err(ApiError::from)?;
        // Some backends return {"error":{"message":"...","type":"...","code":...}}
        // instead of a valid completion object. Check for this before attempting
        // full deserialization so the user sees the actual error, not a cryptic
        // "missing field 'id'" parse failure.
        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(err_obj) = raw.get("error") {
                let msg = err_obj
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("provider returned an error")
                    .to_string();
                let code = err_obj
                    .get("code")
                    .and_then(serde_json::Value::as_u64)
                    .map(|c| c as u16);
                return Err(ApiError::Api {
                    status: reqwest::StatusCode::from_u16(code.unwrap_or(400))
                        .unwrap_or(reqwest::StatusCode::BAD_REQUEST),
                    error_type: err_obj
                        .get("type")
                        .and_then(|t| t.as_str())
                        .map(str::to_owned),
                    message: Some(msg),
                    request_id,
                    body,
                    retryable: false,
                    suggested_action: suggested_action_for_status(
                        reqwest::StatusCode::from_u16(code.unwrap_or(400))
                            .unwrap_or(reqwest::StatusCode::BAD_REQUEST),
                    ),
                    retry_after: None,
                });
            }
        }
        let payload = serde_json::from_str::<ChatCompletionResponse>(&body).map_err(|error| {
            ApiError::json_deserialize(self.config.provider_name, &request.model, &body, error)
        })?;
        let mut normalized = normalize_response(&request.model, payload)?;
        if normalized.request_id.is_none() {
            normalized.request_id = request_id;
        }
        Ok(normalized)
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        preflight_message_request(request)?;
        let response = self
            .send_with_retry(&request.clone().with_streaming())
            .await?;
        Ok(MessageStream {
            request_id: request_id_from_headers(response.headers()),
            response,
            parser: OpenAiSseParser::with_context(self.config.provider_name, request.model.clone()),
            pending: VecDeque::new(),
            done: false,
            state: StreamState::new(request.model.clone()),
        })
    }

    async fn send_with_retry(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        let mut attempts = 0;

        let last_error = loop {
            attempts += 1;
            let retryable_error = match self.send_raw_request(request).await {
                Ok(response) => match expect_success(response).await {
                    Ok(response) => return Ok(response),
                    Err(error) if error.is_retryable() && attempts <= self.max_retries + 1 => error,
                    Err(error) => return Err(error),
                },
                Err(error) if error.is_retryable() && attempts <= self.max_retries + 1 => error,
                Err(error) => return Err(error),
            };

            if attempts > self.max_retries {
                break retryable_error;
            }

            let delay = if let Some(retry_after) = retryable_error.retry_after() {
                retry_after
            } else {
                self.jittered_backoff_for_attempt(attempts)?
            };
            tokio::time::sleep(delay).await;
        };

        Err(ApiError::RetriesExhausted {
            attempts,
            last_error: Box::new(last_error),
        })
    }

    async fn send_raw_request(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        // Pre-flight check: verify request body size against provider limits
        check_request_body_size(request, self.config())?;

        let request_url = chat_completions_endpoint(&self.base_url);
        self.http
            .post(&request_url)
            .header("content-type", "application/json")
            .bearer_auth(&self.api_key)
            .json(&build_chat_completion_request(request, self.config()))
            .send()
            .await
            .map_err(ApiError::from)
    }

    fn backoff_for_attempt(&self, attempt: u32) -> Result<Duration, ApiError> {
        let Some(multiplier) = 1_u32.checked_shl(attempt.saturating_sub(1)) else {
            return Err(ApiError::BackoffOverflow {
                attempt,
                base_delay: self.initial_backoff,
            });
        };
        Ok(self
            .initial_backoff
            .checked_mul(multiplier)
            .map_or(self.max_backoff, |delay| delay.min(self.max_backoff)))
    }

    fn jittered_backoff_for_attempt(&self, attempt: u32) -> Result<Duration, ApiError> {
        let base = self.backoff_for_attempt(attempt)?;
        Ok(base + jitter_for_base(base))
    }
}

/// Process-wide counter that guarantees distinct jitter samples even when
/// the system clock resolution is coarser than consecutive retry sleeps.
static JITTER_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Returns a random additive jitter in `[0, base]` to decorrelate retries
/// Deserialize a JSON field as a `Vec<T>`, treating an explicit `null` value
/// the same as a missing field (i.e. as an empty vector).
/// Some OpenAI-compatible providers emit `"tool_calls": null` instead of
/// omitting the field or using `[]`, which serde's `#[serde(default)]` alone
/// does not tolerate — `default` only handles absent keys, not null values.
fn deserialize_null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// from multiple concurrent clients. Entropy is drawn from the nanosecond
/// wall clock mixed with a monotonic counter and run through a splitmix64
/// finalizer; adequate for retry jitter (no cryptographic requirement).
fn jitter_for_base(base: Duration) -> Duration {
    let base_nanos = u64::try_from(base.as_nanos()).unwrap_or(u64::MAX);
    if base_nanos == 0 {
        return Duration::ZERO;
    }
    let raw_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let tick = JITTER_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut mixed = raw_nanos
        .wrapping_add(tick)
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    mixed ^= mixed >> 31;
    let jitter_nanos = mixed % base_nanos.saturating_add(1);
    Duration::from_nanos(jitter_nanos)
}

impl Provider for OpenAiCompatClient {
    type Stream = MessageStream;

    fn send_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, MessageResponse> {
        Box::pin(async move { self.send_message(request).await })
    }

    fn stream_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, Self::Stream> {
        Box::pin(async move { self.stream_message(request).await })
    }
}

#[derive(Debug)]
pub struct MessageStream {
    request_id: Option<String>,
    response: reqwest::Response,
    parser: OpenAiSseParser,
    pending: VecDeque<StreamEvent>,
    done: bool,
    state: StreamState,
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }

            if self.done {
                self.pending.extend(self.state.finish()?);
                if let Some(event) = self.pending.pop_front() {
                    return Ok(Some(event));
                }
                return Ok(None);
            }

            match self.response.chunk().await? {
                Some(chunk) => {
                    for parsed in self.parser.push(&chunk)? {
                        self.pending.extend(self.state.ingest_chunk(parsed)?);
                    }
                }
                None => {
                    self.done = true;
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct OpenAiSseParser {
    buffer: Vec<u8>,
    provider: String,
    model: String,
}

impl OpenAiSseParser {
    fn with_context(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            buffer: Vec::new(),
            provider: provider.into(),
            model: model.into(),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<ChatCompletionChunk>, ApiError> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();

        while let Some(frame) = next_sse_frame(&mut self.buffer) {
            if let Some(event) = parse_sse_frame(&frame, &self.provider, &self.model)? {
                events.push(event);
            }
        }

        Ok(events)
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct StreamState {
    model: String,
    message_started: bool,
    text_started: bool,
    text_finished: bool,
    finished: bool,
    stop_reason: Option<String>,
    usage: Option<Usage>,
    tool_calls: BTreeMap<u32, ToolCallState>,
}

impl StreamState {
    fn new(model: String) -> Self {
        Self {
            model,
            message_started: false,
            text_started: false,
            text_finished: false,
            finished: false,
            stop_reason: None,
            usage: None,
            tool_calls: BTreeMap::new(),
        }
    }

    fn ingest_chunk(&mut self, chunk: ChatCompletionChunk) -> Result<Vec<StreamEvent>, ApiError> {
        let mut events = Vec::new();
        if !self.message_started {
            self.message_started = true;
            events.push(StreamEvent::MessageStart(MessageStartEvent {
                message: MessageResponse {
                    id: chunk.id.clone(),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: Vec::new(),
                    model: chunk.model.clone().unwrap_or_else(|| self.model.clone()),
                    stop_reason: None,
                    stop_sequence: None,
                    usage: Usage {
                        input_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        output_tokens: 0,
                    },
                    request_id: None,
                },
            }));
        }

        if let Some(usage) = chunk.usage {
            self.usage = Some(Usage {
                input_tokens: usage.prompt_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                output_tokens: usage.completion_tokens,
            });
        }

        for choice in chunk.choices {
            if let Some(content) = choice.delta.content.filter(|value| !value.is_empty()) {
                if !self.text_started {
                    self.text_started = true;
                    events.push(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                        index: 0,
                        content_block: OutputContentBlock::Text {
                            text: String::new(),
                        },
                    }));
                }
                events.push(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                    index: 0,
                    delta: ContentBlockDelta::TextDelta { text: content },
                }));
            }

            for tool_call in choice.delta.tool_calls {
                let state = self.tool_calls.entry(tool_call.index).or_default();
                state.apply(tool_call);
                let block_index = state.block_index();
                if !state.started {
                    if let Some(start_event) = state.start_event()? {
                        state.started = true;
                        events.push(StreamEvent::ContentBlockStart(start_event));
                    } else {
                        continue;
                    }
                }
                if let Some(delta_event) = state.delta_event() {
                    events.push(StreamEvent::ContentBlockDelta(delta_event));
                }
                if choice.finish_reason.as_deref() == Some("tool_calls") && !state.stopped {
                    state.stopped = true;
                    events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                        index: block_index,
                    }));
                }
            }

            if let Some(finish_reason) = choice.finish_reason {
                self.stop_reason = Some(normalize_finish_reason(&finish_reason));
                if finish_reason == "tool_calls" {
                    for state in self.tool_calls.values_mut() {
                        if state.started && !state.stopped {
                            state.stopped = true;
                            events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                                index: state.block_index(),
                            }));
                        }
                    }
                }
            }
        }

        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>, ApiError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;

        let mut events = Vec::new();
        if self.text_started && !self.text_finished {
            self.text_finished = true;
            events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                index: 0,
            }));
        }

        for state in self.tool_calls.values_mut() {
            if !state.started {
                if let Some(start_event) = state.start_event()? {
                    state.started = true;
                    events.push(StreamEvent::ContentBlockStart(start_event));
                    if let Some(delta_event) = state.delta_event() {
                        events.push(StreamEvent::ContentBlockDelta(delta_event));
                    }
                }
            }
            if state.started && !state.stopped {
                state.stopped = true;
                events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                    index: state.block_index(),
                }));
            }
        }

        if self.message_started {
            events.push(StreamEvent::MessageDelta(MessageDeltaEvent {
                delta: MessageDelta {
                    stop_reason: Some(
                        self.stop_reason
                            .clone()
                            .unwrap_or_else(|| "end_turn".to_string()),
                    ),
                    stop_sequence: None,
                },
                usage: self.usage.clone().unwrap_or(Usage {
                    input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    output_tokens: 0,
                }),
            }));
            events.push(StreamEvent::MessageStop(MessageStopEvent {}));
        }
        Ok(events)
    }
}

#[derive(Debug, Default)]
struct ToolCallState {
    openai_index: u32,
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    emitted_len: usize,
    started: bool,
    stopped: bool,
}

impl ToolCallState {
    fn apply(&mut self, tool_call: DeltaToolCall) {
        self.openai_index = tool_call.index;
        if let Some(id) = tool_call.id {
            self.id = Some(id);
        }
        if let Some(name) = tool_call.function.name {
            self.name = Some(name);
        }
        if let Some(arguments) = tool_call.function.arguments {
            self.arguments.push_str(&arguments);
        }
    }

    const fn block_index(&self) -> u32 {
        self.openai_index + 1
    }

    #[allow(clippy::unnecessary_wraps)]
    fn start_event(&self) -> Result<Option<ContentBlockStartEvent>, ApiError> {
        let Some(name) = self.name.clone() else {
            return Ok(None);
        };
        let id = self
            .id
            .clone()
            .unwrap_or_else(|| format!("tool_call_{}", self.openai_index));
        Ok(Some(ContentBlockStartEvent {
            index: self.block_index(),
            content_block: OutputContentBlock::ToolUse {
                id,
                name,
                input: json!({}),
            },
        }))
    }

    fn delta_event(&mut self) -> Option<ContentBlockDeltaEvent> {
        if self.emitted_len >= self.arguments.len() {
            return None;
        }
        let delta = self.arguments[self.emitted_len..].to_string();
        self.emitted_len = self.arguments.len();
        Some(ContentBlockDeltaEvent {
            index: self.block_index(),
            delta: ContentBlockDelta::InputJsonDelta {
                partial_json: delta,
            },
        })
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    id: String,
    model: String,
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ResponseToolCall>,
}

#[derive(Debug, Deserialize)]
struct ResponseToolCall {
    id: String,
    function: ResponseToolFunction,
}

#[derive(Debug, Deserialize)]
struct ResponseToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_as_empty_vec")]
    tool_calls: Vec<DeltaToolCall>,
}

#[derive(Debug, Deserialize)]
struct DeltaToolCall {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: DeltaFunction,
}

#[derive(Debug, Default, Deserialize)]
struct DeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(rename = "type")]
    error_type: Option<String>,
    message: Option<String>,
}

/// Returns true for models known to reject tuning parameters like temperature,
/// `top_p`, `frequency_penalty`, and `presence_penalty`. These are typically
/// reasoning/chain-of-thought models with fixed sampling.
/// Returns true for models known to reject tuning parameters like temperature,
/// `top_p`, `frequency_penalty`, and `presence_penalty`. These are typically
/// reasoning/chain-of-thought models with fixed sampling.
/// Public for benchmarking and testing purposes.
#[must_use]
pub fn is_reasoning_model(model: &str) -> bool {
    let lowered = model.to_ascii_lowercase();
    // Strip any provider/ prefix for the check (e.g. qwen/qwen-qwq -> qwen-qwq)
    let canonical = lowered.rsplit('/').next().unwrap_or(lowered.as_str());
    // OpenAI reasoning models
    canonical.starts_with("o1")
        || canonical.starts_with("o3")
        || canonical.starts_with("o4")
        // xAI reasoning: grok-3-mini always uses reasoning mode
        || canonical == "grok-3-mini"
        // Alibaba DashScope reasoning variants (QwQ + Qwen3-Thinking family)
        || canonical.starts_with("qwen-qwq")
        || canonical.starts_with("qwq")
        || canonical.contains("thinking")
}

/// Strip routing prefix (e.g., "openai/gpt-4" → "gpt-4") for the wire.
/// The prefix is used only to select transport; the backend expects the
/// bare model id.
fn strip_routing_prefix(model: &str) -> &str {
    if let Some(pos) = model.find('/') {
        let prefix = &model[..pos];
        // Only strip if the prefix before "/" is a known routing prefix,
        // not if "/" appears in the middle of the model name for other reasons.
        if matches!(prefix, "openai" | "xai" | "grok" | "qwen" | "kimi") {
            &model[pos + 1..]
        } else {
            model
        }
    } else {
        model
    }
}

/// Estimate the serialized JSON size of a request payload in bytes.
/// This is a pre-flight check to avoid hitting provider-specific size limits.
pub fn estimate_request_body_size(request: &MessageRequest, config: OpenAiCompatConfig) -> usize {
    let payload = build_chat_completion_request(request, config);
    // serde_json::to_vec gives us the exact byte size of the serialized JSON
    serde_json::to_vec(&payload).map_or(0, |v| v.len())
}

/// Pre-flight check for request body size against provider limits.
/// Returns Ok(()) if the request is within limits, or an error with
/// a clear message about the size limit being exceeded.
pub fn check_request_body_size(
    request: &MessageRequest,
    config: OpenAiCompatConfig,
) -> Result<(), ApiError> {
    let estimated_bytes = estimate_request_body_size(request, config);
    let max_bytes = config.max_request_body_bytes;

    if estimated_bytes > max_bytes {
        Err(ApiError::RequestBodySizeExceeded {
            estimated_bytes,
            max_bytes,
            provider: config.provider_name,
        })
    } else {
        Ok(())
    }
}

/// Builds a chat completion request payload from a `MessageRequest`.
/// Public for benchmarking purposes.
pub fn build_chat_completion_request(
    request: &MessageRequest,
    config: OpenAiCompatConfig,
) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = request.system.as_ref().filter(|value| !value.is_empty()) {
        messages.push(json!({
            "role": "system",
            "content": system,
        }));
    }
    // Strip routing prefix (e.g., "openai/gpt-4" → "gpt-4") for the wire.
    let wire_model = strip_routing_prefix(&request.model);
    for message in &request.messages {
        messages.extend(translate_message(message, wire_model));
    }
    // Sanitize: drop any `role:"tool"` message that does not have a valid
    // paired `role:"assistant"` with a `tool_calls` entry carrying the same
    // `id` immediately before it (directly or as part of a run of tool
    // results). OpenAI-compatible backends return 400 for orphaned tool
    // messages regardless of how they were produced (compaction, session
    // editing, resume, etc.). We drop rather than error so the request can
    // still proceed with the remaining history intact.
    messages = sanitize_tool_message_pairing(messages);

    // gpt-5* requires `max_completion_tokens`; older OpenAI models accept both.
    // We send the correct field based on the wire model name so gpt-5.x requests
    // don't fail with "unknown field max_tokens".
    let max_tokens_key = if wire_model.starts_with("gpt-5") {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };

    let mut payload = json!({
        "model": wire_model,
        max_tokens_key: request.max_tokens,
        "messages": messages,
        "stream": request.stream,
    });

    if request.stream && should_request_stream_usage(config) {
        payload["stream_options"] = json!({ "include_usage": true });
    }

    if let Some(tools) = &request.tools {
        payload["tools"] =
            Value::Array(tools.iter().map(openai_tool_definition).collect::<Vec<_>>());
    }
    if let Some(tool_choice) = &request.tool_choice {
        payload["tool_choice"] = openai_tool_choice(tool_choice);
    }

    // OpenAI-compatible tuning parameters — only included when explicitly set.
    // Reasoning models (o1/o3/o4/grok-3-mini) reject these params with 400;
    // silently strip them to avoid cryptic provider errors.
    if !is_reasoning_model(&request.model) {
        if let Some(temperature) = request.temperature {
            payload["temperature"] = json!(temperature);
        }
        if let Some(top_p) = request.top_p {
            payload["top_p"] = json!(top_p);
        }
        if let Some(frequency_penalty) = request.frequency_penalty {
            payload["frequency_penalty"] = json!(frequency_penalty);
        }
        if let Some(presence_penalty) = request.presence_penalty {
            payload["presence_penalty"] = json!(presence_penalty);
        }
    }
    // stop is generally safe for all providers
    if let Some(stop) = &request.stop {
        if !stop.is_empty() {
            payload["stop"] = json!(stop);
        }
    }
    // reasoning_effort for OpenAI-compatible reasoning models (o4-mini, o3, etc.)
    if let Some(effort) = &request.reasoning_effort {
        payload["reasoning_effort"] = json!(effort);
    }

    payload
}

/// Returns true for models that do NOT support the `is_error` field in tool results.
/// kimi models (via Moonshot AI/Dashscope) reject this field with 400 Bad Request.
/// Returns true for models that do NOT support the `is_error` field in tool results.
/// kimi models (via Moonshot AI/Dashscope) reject this field with 400 Bad Request.
/// Public for benchmarking and testing purposes.
#[must_use]
pub fn model_rejects_is_error_field(model: &str) -> bool {
    let lowered = model.to_ascii_lowercase();
    // Strip any provider/ prefix for the check
    let canonical = lowered.rsplit('/').next().unwrap_or(lowered.as_str());
    // kimi models (kimi-k2.5, kimi-k1.5, kimi-moonshot, etc.)
    canonical.starts_with("kimi")
}

/// Translates an `InputMessage` into OpenAI-compatible message format.
/// Public for benchmarking purposes.
#[must_use]
pub fn translate_message(message: &InputMessage, model: &str) -> Vec<Value> {
    let supports_is_error = !model_rejects_is_error_field(model);
    match message.role.as_str() {
        "assistant" => {
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            for block in &message.content {
                match block {
                    InputContentBlock::Text { text: value } => text.push_str(value),
                    InputContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": input.to_string(),
                        }
                    })),
                    InputContentBlock::ToolResult { .. } => {}
                }
            }
            if text.is_empty() && tool_calls.is_empty() {
                Vec::new()
            } else {
                let mut msg = serde_json::json!({
                    "role": "assistant",
                    "content": (!text.is_empty()).then_some(text),
                });
                // Only include tool_calls when non-empty: some providers reject
                // assistant messages with an explicit empty tool_calls array.
                if !tool_calls.is_empty() {
                    msg["tool_calls"] = json!(tool_calls);
                }
                vec![msg]
            }
        }
        _ => message
            .content
            .iter()
            .filter_map(|block| match block {
                InputContentBlock::Text { text } => Some(json!({
                    "role": "user",
                    "content": text,
                })),
                InputContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let mut msg = json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": flatten_tool_result_content(content),
                    });
                    // Only include is_error for models that support it.
                    // kimi models reject this field with 400 Bad Request.
                    if supports_is_error {
                        msg["is_error"] = json!(is_error);
                    }
                    Some(msg)
                }
                InputContentBlock::ToolUse { .. } => None,
            })
            .collect(),
    }
}

/// Remove `role:"tool"` messages from `messages` that have no valid paired
/// `role:"assistant"` message with a matching `tool_calls[].id` immediately
/// preceding them. This is a last-resort safety net at the request-building
/// layer — the compaction boundary fix (6e301c8) prevents the most common
/// producer path, but resume, session editing, or future compaction variants
/// could still create orphaned tool messages.
///
/// Algorithm: scan left-to-right. For each `role:"tool"` message, check the
/// immediately preceding non-tool message. If it's `role:"assistant"` with a
/// `tool_calls` array containing an entry whose `id` matches the tool
/// message's `tool_call_id`, the pair is valid and both are kept. Otherwise
/// the tool message is dropped.
/// Remove `role:"tool"` messages from `messages` that have no valid paired
/// `role:"assistant"` message with a matching `tool_calls[].id` immediately
/// preceding them. Public for benchmarking purposes.
pub fn sanitize_tool_message_pairing(messages: Vec<Value>) -> Vec<Value> {
    // Collect indices of tool messages that are orphaned.
    let mut drop_indices = std::collections::HashSet::new();
    for (i, msg) in messages.iter().enumerate() {
        if msg.get("role").and_then(|v| v.as_str()) != Some("tool") {
            continue;
        }
        let tool_call_id = msg
            .get("tool_call_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Find the nearest preceding non-tool message.
        let preceding = messages[..i]
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(|v| v.as_str()) != Some("tool"));
        // A tool message is considered paired when:
        // (a) the nearest preceding non-tool message is an assistant message
        //     whose `tool_calls` array contains an entry with the matching id, OR
        // (b) there's no clear preceding context (e.g. the message comes right
        //     after a user turn — this can happen with translated mixed-content
        //     user messages). In case (b) we allow the message through rather
        //     than silently dropping potentially valid history.
        let preceding_role = preceding
            .and_then(|m| m.get("role"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Only apply sanitization when the preceding message is an assistant
        // turn (the invariant is: assistant-with-tool_calls must precede tool).
        // If the preceding is something else (user, system) don't drop — it
        // may be a valid translation artifact or a path we don't understand.
        if preceding_role != "assistant" {
            continue;
        }
        let paired = preceding
            .and_then(|m| m.get("tool_calls").and_then(|tc| tc.as_array()))
            .is_some_and(|tool_calls| {
                tool_calls
                    .iter()
                    .any(|tc| tc.get("id").and_then(|v| v.as_str()) == Some(tool_call_id))
            });
        if !paired {
            drop_indices.insert(i);
        }
    }
    if drop_indices.is_empty() {
        return messages;
    }
    messages
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !drop_indices.contains(i))
        .map(|(_, m)| m)
        .collect()
}

/// Flattens tool result content blocks into a single string.
/// Optimized to pre-allocate capacity and avoid intermediate `Vec` construction.
#[must_use]
pub fn flatten_tool_result_content(content: &[ToolResultContentBlock]) -> String {
    // Pre-calculate total capacity needed to avoid reallocations
    let total_len: usize = content
        .iter()
        .map(|block| match block {
            ToolResultContentBlock::Text { text } => text.len(),
            ToolResultContentBlock::Json { value } => value.to_string().len(),
        })
        .sum();

    // Add capacity for newlines between blocks
    let capacity = total_len + content.len().saturating_sub(1);

    let mut result = String::with_capacity(capacity);
    for (i, block) in content.iter().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        match block {
            ToolResultContentBlock::Text { text } => result.push_str(text),
            ToolResultContentBlock::Json { value } => {
                // Use write! to append without creating intermediate String
                result.push_str(&value.to_string());
            }
        }
    }
    result
}

/// Recursively ensure every object-type node in a JSON Schema has
/// `"properties"` (at least `{}`) and `"additionalProperties": false`.
/// The `OpenAI` `/responses` endpoint validates schemas strictly and rejects
/// objects that omit these fields; `/chat/completions` is lenient but also
/// accepts them, so we normalise unconditionally.
fn normalize_object_schema(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        if obj.get("type").and_then(Value::as_str) == Some("object") {
            obj.entry("properties").or_insert_with(|| json!({}));
            obj.entry("additionalProperties")
                .or_insert(Value::Bool(false));
        }
        // Recurse into properties values
        if let Some(props) = obj.get_mut("properties") {
            if let Some(props_obj) = props.as_object_mut() {
                let keys: Vec<String> = props_obj.keys().cloned().collect();
                for k in keys {
                    if let Some(v) = props_obj.get_mut(&k) {
                        normalize_object_schema(v);
                    }
                }
            }
        }
        // Recurse into items (arrays)
        if let Some(items) = obj.get_mut("items") {
            normalize_object_schema(items);
        }
    }
}

fn openai_tool_definition(tool: &ToolDefinition) -> Value {
    let mut parameters = tool.input_schema.clone();
    normalize_object_schema(&mut parameters);
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": parameters,
        }
    })
}

fn openai_tool_choice(tool_choice: &ToolChoice) -> Value {
    match tool_choice {
        ToolChoice::Auto => Value::String("auto".to_string()),
        ToolChoice::Any => Value::String("required".to_string()),
        ToolChoice::Tool { name } => json!({
            "type": "function",
            "function": { "name": name },
        }),
    }
}

fn should_request_stream_usage(config: OpenAiCompatConfig) -> bool {
    matches!(config.provider_name, "OpenAI")
}

fn normalize_response(
    model: &str,
    response: ChatCompletionResponse,
) -> Result<MessageResponse, ApiError> {
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or(ApiError::InvalidSseFrame(
            "chat completion response missing choices",
        ))?;
    let mut content = Vec::new();
    if let Some(text) = choice.message.content.filter(|value| !value.is_empty()) {
        content.push(OutputContentBlock::Text { text });
    }
    for tool_call in choice.message.tool_calls {
        content.push(OutputContentBlock::ToolUse {
            id: tool_call.id,
            name: tool_call.function.name,
            input: parse_tool_arguments(&tool_call.function.arguments),
        });
    }

    Ok(MessageResponse {
        id: response.id,
        kind: "message".to_string(),
        role: choice.message.role,
        content,
        model: response.model.if_empty_then(model.to_string()),
        stop_reason: choice
            .finish_reason
            .map(|value| normalize_finish_reason(&value)),
        stop_sequence: None,
        usage: Usage {
            input_tokens: response
                .usage
                .as_ref()
                .map_or(0, |usage| usage.prompt_tokens),
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens: response
                .usage
                .as_ref()
                .map_or(0, |usage| usage.completion_tokens),
        },
        request_id: None,
    })
}

fn parse_tool_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({ "raw": arguments }))
}

fn next_sse_frame(buffer: &mut Vec<u8>) -> Option<String> {
    let separator = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2))
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| (position, 4))
        })?;

    let (position, separator_len) = separator;
    let frame = buffer.drain(..position + separator_len).collect::<Vec<_>>();
    let frame_len = frame.len().saturating_sub(separator_len);
    Some(String::from_utf8_lossy(&frame[..frame_len]).into_owned())
}

fn parse_sse_frame(
    frame: &str,
    provider: &str,
    model: &str,
) -> Result<Option<ChatCompletionChunk>, ApiError> {
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let mut data_lines = Vec::new();
    for line in trimmed.lines() {
        if line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let payload = data_lines.join("\n");
    if payload == "[DONE]" {
        return Ok(None);
    }
    // Some backends embed an error object in a data: frame instead of using an
    // HTTP error status. Surface the error message directly rather than letting
    // ChatCompletionChunk deserialization fail with a cryptic 'missing field' error.
    if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&payload) {
        if let Some(err_obj) = raw.get("error") {
            let msg = err_obj
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("provider returned an error in stream")
                .to_string();
            let code = err_obj
                .get("code")
                .and_then(serde_json::Value::as_u64)
                .map(|c| c as u16);
            let status = reqwest::StatusCode::from_u16(code.unwrap_or(400))
                .unwrap_or(reqwest::StatusCode::BAD_REQUEST);
            return Err(ApiError::Api {
                status,
                error_type: err_obj
                    .get("type")
                    .and_then(|t| t.as_str())
                    .map(str::to_owned),
                message: Some(msg),
                request_id: None,
                body: payload.clone(),
                retryable: false,
                suggested_action: suggested_action_for_status(status),
                retry_after: None,
            });
        }
    }
    serde_json::from_str::<ChatCompletionChunk>(&payload)
        .map(Some)
        .map_err(|error| ApiError::json_deserialize(provider, model, &payload, error))
}

fn read_env_non_empty(key: &str) -> Result<Option<String>, ApiError> {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(super::dotenv_value(key)),
        Err(error) => Err(ApiError::from(error)),
    }
}

#[must_use]
pub fn has_api_key(key: &str) -> bool {
    read_env_non_empty(key)
        .ok()
        .and_then(std::convert::identity)
        .is_some()
}

#[must_use]
pub fn read_base_url(config: OpenAiCompatConfig) -> String {
    std::env::var(config.base_url_env).unwrap_or_else(|_| config.default_base_url.to_string())
}

fn chat_completions_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/chat/completions")
    }
}

fn request_id_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(REQUEST_ID_HEADER)
        .or_else(|| headers.get(ALT_REQUEST_ID_HEADER))
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

async fn expect_success(response: reqwest::Response) -> Result<reqwest::Response, ApiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let headers = response.headers().clone();
    let request_id = request_id_from_headers(&headers);
    let body = response.text().await.unwrap_or_default();
    let parsed_error = serde_json::from_str::<ErrorEnvelope>(&body).ok();
    let retryable = is_retryable_status(status);
    let retry_after = parse_retry_after(&headers, status);

    let suggested_action = suggested_action_for_status(status);

    Err(ApiError::Api {
        status,
        error_type: parsed_error
            .as_ref()
            .and_then(|error| error.error.error_type.clone()),
        message: parsed_error
            .as_ref()
            .and_then(|error| error.error.message.clone()),
        request_id,
        body,
        retryable,
        suggested_action,
        retry_after,
    })
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap, status: reqwest::StatusCode) -> Option<std::time::Duration> {
    if status != reqwest::StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
}

const fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 429 | 500 | 502 | 503 | 504)
}

/// Generate a suggested user action based on the HTTP status code and error context.
/// This provides actionable guidance when API requests fail.
fn suggested_action_for_status(status: reqwest::StatusCode) -> Option<String> {
    match status.as_u16() {
        401 => Some("Check API key is set correctly and has not expired".to_string()),
        403 => Some("Verify API key has required permissions for this operation".to_string()),
        413 => Some("Reduce prompt size or context window before retrying".to_string()),
        429 => Some("Wait a moment before retrying; consider reducing request rate".to_string()),
        500 => Some("Provider server error - retry after a brief wait".to_string()),
        502..=504 => Some("Provider gateway error - retry after a brief wait".to_string()),
        _ => None,
    }
}

fn normalize_finish_reason(value: &str) -> String {
    match value {
        "stop" => "end_turn",
        "tool_calls" => "tool_use",
        other => other,
    }
    .to_string()
}

trait StringExt {
    fn if_empty_then(self, fallback: String) -> String;
}

impl StringExt for String {
    fn if_empty_then(self, fallback: String) -> String {
        if self.is_empty() {
            fallback
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_chat_completion_request, chat_completions_endpoint, is_reasoning_model,
        normalize_finish_reason, openai_tool_choice, parse_tool_arguments, OpenAiCompatClient,
        OpenAiCompatConfig,
    };
    use crate::error::ApiError;
    use crate::types::{
        InputContentBlock, InputMessage, MessageRequest, ToolChoice, ToolDefinition,
        ToolResultContentBlock,
    };
    use serde_json::json;
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn request_translation_uses_openai_compatible_shape() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "grok-3".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage {
                    role: "user".to_string(),
                    content: vec![
                        InputContentBlock::Text {
                            text: "hello".to_string(),
                        },
                        InputContentBlock::ToolResult {
                            tool_use_id: "tool_1".to_string(),
                            content: vec![ToolResultContentBlock::Json {
                                value: json!({"ok": true}),
                            }],
                            is_error: false,
                        },
                    ],
                }],
                system: Some("be helpful".to_string()),
                tools: Some(vec![ToolDefinition {
                    name: "weather".to_string(),
                    description: Some("Get weather".to_string()),
                    input_schema: json!({"type": "object"}),
                }]),
                tool_choice: Some(ToolChoice::Auto),
                stream: false,
                ..Default::default()
            },
            OpenAiCompatConfig::xai(),
        );

        assert_eq!(payload["messages"][0]["role"], json!("system"));
        assert_eq!(payload["messages"][1]["role"], json!("user"));
        assert_eq!(payload["messages"][2]["role"], json!("tool"));
        assert_eq!(payload["tools"][0]["type"], json!("function"));
        assert_eq!(payload["tool_choice"], json!("auto"));
    }

    #[test]
    fn tool_schema_object_gets_strict_fields_for_responses_endpoint() {
        // OpenAI /responses endpoint rejects object schemas missing
        // "properties" and "additionalProperties". Verify normalize_object_schema
        // fills them in so the request shape is strict-validator-safe.
        use super::normalize_object_schema;

        // Bare object — no properties at all
        let mut schema = json!({"type": "object"});
        normalize_object_schema(&mut schema);
        assert_eq!(schema["properties"], json!({}));
        assert_eq!(schema["additionalProperties"], json!(false));

        // Nested object inside properties
        let mut schema2 = json!({
            "type": "object",
            "properties": {
                "location": {"type": "object", "properties": {"lat": {"type": "number"}}}
            }
        });
        normalize_object_schema(&mut schema2);
        assert_eq!(schema2["additionalProperties"], json!(false));
        assert_eq!(
            schema2["properties"]["location"]["additionalProperties"],
            json!(false)
        );

        // Existing properties/additionalProperties should not be overwritten
        let mut schema3 = json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "additionalProperties": true
        });
        normalize_object_schema(&mut schema3);
        assert_eq!(
            schema3["additionalProperties"],
            json!(true),
            "must not overwrite existing"
        );
    }

    #[test]
    fn reasoning_effort_is_included_when_set() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "o4-mini".to_string(),
                max_tokens: 1024,
                messages: vec![InputMessage::user_text("think hard")],
                reasoning_effort: Some("high".to_string()),
                ..Default::default()
            },
            OpenAiCompatConfig::openai(),
        );
        assert_eq!(payload["reasoning_effort"], json!("high"));
    }

    #[test]
    fn reasoning_effort_omitted_when_not_set() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "gpt-4o".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage::user_text("hello")],
                ..Default::default()
            },
            OpenAiCompatConfig::openai(),
        );
        assert!(payload.get("reasoning_effort").is_none());
    }

    #[test]
    fn openai_streaming_requests_include_usage_opt_in() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "gpt-5".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage::user_text("hello")],
                system: None,
                tools: None,
                tool_choice: None,
                stream: true,
                ..Default::default()
            },
            OpenAiCompatConfig::openai(),
        );

        assert_eq!(payload["stream_options"], json!({"include_usage": true}));
    }

    #[test]
    fn xai_streaming_requests_skip_openai_specific_usage_opt_in() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "grok-3".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage::user_text("hello")],
                system: None,
                tools: None,
                tool_choice: None,
                stream: true,
                ..Default::default()
            },
            OpenAiCompatConfig::xai(),
        );

        assert!(payload.get("stream_options").is_none());
    }

    #[test]
    fn tool_choice_translation_supports_required_function() {
        assert_eq!(openai_tool_choice(&ToolChoice::Any), json!("required"));
        assert_eq!(
            openai_tool_choice(&ToolChoice::Tool {
                name: "weather".to_string(),
            }),
            json!({"type": "function", "function": {"name": "weather"}})
        );
    }

    #[test]
    fn parses_tool_arguments_fallback() {
        assert_eq!(
            parse_tool_arguments("{\"city\":\"Paris\"}"),
            json!({"city": "Paris"})
        );
        assert_eq!(parse_tool_arguments("not-json"), json!({"raw": "not-json"}));
    }

    #[test]
    fn missing_xai_api_key_is_provider_specific() {
        let _lock = env_lock();
        std::env::remove_var("XAI_API_KEY");
        let error = OpenAiCompatClient::from_env(OpenAiCompatConfig::xai())
            .expect_err("missing key should error");
        assert!(matches!(
            error,
            ApiError::MissingCredentials {
                provider: "xAI",
                ..
            }
        ));
    }

    #[test]
    fn endpoint_builder_accepts_base_urls_and_full_endpoints() {
        assert_eq!(
            chat_completions_endpoint("https://api.x.ai/v1"),
            "https://api.x.ai/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://api.x.ai/v1/"),
            "https://api.x.ai/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://api.x.ai/v1/chat/completions"),
            "https://api.x.ai/v1/chat/completions"
        );
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock")
    }

    #[test]
    fn normalizes_stop_reasons() {
        assert_eq!(normalize_finish_reason("stop"), "end_turn");
        assert_eq!(normalize_finish_reason("tool_calls"), "tool_use");
    }

    #[test]
    fn tuning_params_included_in_payload_when_set() {
        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 1024,
            messages: vec![],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.7),
            top_p: Some(0.9),
            frequency_penalty: Some(0.5),
            presence_penalty: Some(0.3),
            stop: Some(vec!["\n".to_string()]),
            reasoning_effort: None,
        };
        let payload = build_chat_completion_request(&request, OpenAiCompatConfig::openai());
        assert_eq!(payload["temperature"], 0.7);
        assert_eq!(payload["top_p"], 0.9);
        assert_eq!(payload["frequency_penalty"], 0.5);
        assert_eq!(payload["presence_penalty"], 0.3);
        assert_eq!(payload["stop"], json!(["\n"]));
    }

    #[test]
    fn reasoning_model_strips_tuning_params() {
        let request = MessageRequest {
            model: "o1-mini".to_string(),
            max_tokens: 1024,
            messages: vec![],
            stream: false,
            temperature: Some(0.7),
            top_p: Some(0.9),
            frequency_penalty: Some(0.5),
            presence_penalty: Some(0.3),
            stop: Some(vec!["\n".to_string()]),
            ..Default::default()
        };
        let payload = build_chat_completion_request(&request, OpenAiCompatConfig::openai());
        assert!(
            payload.get("temperature").is_none(),
            "reasoning model should strip temperature"
        );
        assert!(
            payload.get("top_p").is_none(),
            "reasoning model should strip top_p"
        );
        assert!(payload.get("frequency_penalty").is_none());
        assert!(payload.get("presence_penalty").is_none());
        // stop is safe for all providers
        assert_eq!(payload["stop"], json!(["\n"]));
    }

    #[test]
    fn grok_3_mini_is_reasoning_model() {
        assert!(is_reasoning_model("grok-3-mini"));
        assert!(is_reasoning_model("o1"));
        assert!(is_reasoning_model("o1-mini"));
        assert!(is_reasoning_model("o3-mini"));
        assert!(!is_reasoning_model("gpt-4o"));
        assert!(!is_reasoning_model("grok-3"));
        assert!(!is_reasoning_model("claude-sonnet-4-6"));
    }

    #[test]
    fn qwen_reasoning_variants_are_detected() {
        // QwQ reasoning model
        assert!(is_reasoning_model("qwen-qwq-32b"));
        assert!(is_reasoning_model("qwen/qwen-qwq-32b"));
        // Qwen3 thinking family
        assert!(is_reasoning_model("qwen3-30b-a3b-thinking"));
        assert!(is_reasoning_model("qwen/qwen3-30b-a3b-thinking"));
        // Bare qwq
        assert!(is_reasoning_model("qwq-plus"));
        // Regular Qwen models must NOT be classified as reasoning
        assert!(!is_reasoning_model("qwen-max"));
        assert!(!is_reasoning_model("qwen/qwen-plus"));
        assert!(!is_reasoning_model("qwen-turbo"));
    }

    #[test]
    fn tuning_params_omitted_from_payload_when_none() {
        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 1024,
            messages: vec![],
            stream: false,
            ..Default::default()
        };
        let payload = build_chat_completion_request(&request, OpenAiCompatConfig::openai());
        assert!(
            payload.get("temperature").is_none(),
            "temperature should be absent"
        );
        assert!(payload.get("top_p").is_none(), "top_p should be absent");
        assert!(payload.get("frequency_penalty").is_none());
        assert!(payload.get("presence_penalty").is_none());
        assert!(payload.get("stop").is_none());
    }

    #[test]
    fn gpt5_uses_max_completion_tokens_not_max_tokens() {
        // gpt-5* models require `max_completion_tokens`; legacy `max_tokens` causes
        // a request-validation failure. Verify the correct key is emitted.
        let request = MessageRequest {
            model: "gpt-5.2".to_string(),
            max_tokens: 512,
            messages: vec![],
            stream: false,
            ..Default::default()
        };
        let payload = build_chat_completion_request(&request, OpenAiCompatConfig::openai());
        assert_eq!(
            payload["max_completion_tokens"],
            json!(512),
            "gpt-5.2 should emit max_completion_tokens"
        );
        assert!(
            payload.get("max_tokens").is_none(),
            "gpt-5.2 must not emit max_tokens"
        );
    }

    /// Regression test: some OpenAI-compatible providers emit `"tool_calls": null`
    /// in stream delta chunks instead of omitting the field or using `[]`.
    /// Before the fix this produced: `invalid type: null, expected a sequence`.
    #[test]
    fn delta_with_null_tool_calls_deserializes_as_empty_vec() {
        use super::deserialize_null_as_empty_vec;

        #[allow(dead_code)]
        #[derive(serde::Deserialize, Debug)]
        struct Delta {
            content: Option<String>,
            #[serde(default, deserialize_with = "deserialize_null_as_empty_vec")]
            tool_calls: Vec<super::DeltaToolCall>,
        }

        // Simulate the exact shape observed in the wild (gaebal-gajae repro 2026-04-09)
        let json = r#"{
            "content": "",
            "function_call": null,
            "refusal": null,
            "role": "assistant",
            "tool_calls": null
        }"#;
        let delta: Delta = serde_json::from_str(json)
            .expect("delta with tool_calls:null must deserialize without error");
        assert!(
            delta.tool_calls.is_empty(),
            "tool_calls:null must produce an empty vec, not an error"
        );
    }

    /// Regression: when building a multi-turn request where a prior assistant
    /// turn has no tool calls, the serialized assistant message must NOT include
    /// `tool_calls: []`. Some providers reject requests that carry an empty
    /// `tool_calls` array on assistant turns (gaebal-gajae repro 2026-04-09).
    #[test]
    fn assistant_message_without_tool_calls_omits_tool_calls_field() {
        use crate::types::{InputContentBlock, InputMessage};

        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 100,
            messages: vec![InputMessage {
                role: "assistant".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "Hello".to_string(),
                }],
            }],
            stream: false,
            ..Default::default()
        };
        let payload = build_chat_completion_request(&request, OpenAiCompatConfig::openai());
        let messages = payload["messages"].as_array().unwrap();
        let assistant_msg = messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant message must be present");
        assert!(
            assistant_msg.get("tool_calls").is_none(),
            "assistant message without tool calls must omit tool_calls field: {assistant_msg:?}"
        );
    }

    /// Regression: assistant messages WITH tool calls must still include
    /// the `tool_calls` array (normal multi-turn tool-use flow).
    #[test]
    fn assistant_message_with_tool_calls_includes_tool_calls_field() {
        use crate::types::{InputContentBlock, InputMessage};

        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 100,
            messages: vec![InputMessage {
                role: "assistant".to_string(),
                content: vec![InputContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "/tmp/test"}),
                }],
            }],
            stream: false,
            ..Default::default()
        };
        let payload = build_chat_completion_request(&request, OpenAiCompatConfig::openai());
        let messages = payload["messages"].as_array().unwrap();
        let assistant_msg = messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant message must be present");
        let tool_calls = assistant_msg
            .get("tool_calls")
            .expect("assistant message with tool calls must include tool_calls field");
        assert!(tool_calls.is_array());
        assert_eq!(tool_calls.as_array().unwrap().len(), 1);
    }

    /// Orphaned tool messages (no preceding assistant `tool_calls`) must be
    /// dropped by the request-builder sanitizer. Regression for the second
    /// layer of the tool-pairing invariant fix (gaebal-gajae 2026-04-10).
    #[test]
    fn sanitize_drops_orphaned_tool_messages() {
        use super::sanitize_tool_message_pairing;

        // Valid pair: assistant with tool_calls → tool result
        let valid = vec![
            json!({"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "search", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": "call_1", "content": "result"}),
        ];
        let out = sanitize_tool_message_pairing(valid);
        assert_eq!(out.len(), 2, "valid pair must be preserved");

        // Orphaned tool message: no preceding assistant tool_calls
        let orphaned = vec![
            json!({"role": "assistant", "content": "hi"}),
            json!({"role": "tool", "tool_call_id": "call_2", "content": "orphaned"}),
        ];
        let out = sanitize_tool_message_pairing(orphaned);
        assert_eq!(out.len(), 1, "orphaned tool message must be dropped");
        assert_eq!(out[0]["role"], json!("assistant"));

        // Mismatched tool_call_id
        let mismatched = vec![
            json!({"role": "assistant", "content": null, "tool_calls": [{"id": "call_3", "type": "function", "function": {"name": "f", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": "call_WRONG", "content": "bad"}),
        ];
        let out = sanitize_tool_message_pairing(mismatched);
        assert_eq!(out.len(), 1, "tool message with wrong id must be dropped");

        // Two tool results both valid (same preceding assistant)
        let two_results = vec![
            json!({"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_a", "type": "function", "function": {"name": "fa", "arguments": "{}"}},
                {"id": "call_b", "type": "function", "function": {"name": "fb", "arguments": "{}"}}
            ]}),
            json!({"role": "tool", "tool_call_id": "call_a", "content": "ra"}),
            json!({"role": "tool", "tool_call_id": "call_b", "content": "rb"}),
        ];
        let out = sanitize_tool_message_pairing(two_results);
        assert_eq!(out.len(), 3, "both valid tool results must be preserved");
    }

    #[test]
    fn non_gpt5_uses_max_tokens() {
        // Older OpenAI models expect `max_tokens`; verify gpt-4o is unaffected.
        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 512,
            messages: vec![],
            stream: false,
            ..Default::default()
        };
        let payload = build_chat_completion_request(&request, OpenAiCompatConfig::openai());
        assert_eq!(payload["max_tokens"], json!(512));
        assert!(
            payload.get("max_completion_tokens").is_none(),
            "gpt-4o must not emit max_completion_tokens"
        );
    }

    // ============================================================================
    // US-009: kimi model compatibility tests
    // ============================================================================

    #[test]
    fn model_rejects_is_error_field_detects_kimi_models() {
        // kimi models (various formats) should be detected
        assert!(super::model_rejects_is_error_field("kimi-k2.5"));
        assert!(super::model_rejects_is_error_field("kimi-k1.5"));
        assert!(super::model_rejects_is_error_field("kimi-moonshot"));
        assert!(super::model_rejects_is_error_field("KIMI-K2.5")); // case insensitive
        assert!(super::model_rejects_is_error_field("dashscope/kimi-k2.5")); // with prefix
        assert!(super::model_rejects_is_error_field("moonshot/kimi-k2.5")); // different prefix

        // Non-kimi models should NOT be detected
        assert!(!super::model_rejects_is_error_field("gpt-4o"));
        assert!(!super::model_rejects_is_error_field("gpt-4"));
        assert!(!super::model_rejects_is_error_field("claude-sonnet-4-6"));
        assert!(!super::model_rejects_is_error_field("grok-3"));
        assert!(!super::model_rejects_is_error_field("grok-3-mini"));
        assert!(!super::model_rejects_is_error_field("xai/grok-3"));
        assert!(!super::model_rejects_is_error_field("qwen/qwen-plus"));
        assert!(!super::model_rejects_is_error_field("o1-mini"));
    }

    #[test]
    fn translate_message_includes_is_error_for_non_kimi_models() {
        use crate::types::{InputContentBlock, InputMessage, ToolResultContentBlock};

        // Test with gpt-4o (should include is_error)
        let message = InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: vec![ToolResultContentBlock::Text {
                    text: "Error occurred".to_string(),
                }],
                is_error: true,
            }],
        };

        let translated = super::translate_message(&message, "gpt-4o");
        assert_eq!(translated.len(), 1);
        let tool_msg = &translated[0];
        assert_eq!(tool_msg["role"], json!("tool"));
        assert_eq!(tool_msg["tool_call_id"], json!("call_1"));
        assert_eq!(tool_msg["content"], json!("Error occurred"));
        assert!(
            tool_msg.get("is_error").is_some(),
            "gpt-4o should include is_error field"
        );
        assert_eq!(tool_msg["is_error"], json!(true));

        // Test with grok-3 (should include is_error)
        let message2 = InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::ToolResult {
                tool_use_id: "call_2".to_string(),
                content: vec![ToolResultContentBlock::Text {
                    text: "Success".to_string(),
                }],
                is_error: false,
            }],
        };

        let translated2 = super::translate_message(&message2, "grok-3");
        assert!(
            translated2[0].get("is_error").is_some(),
            "grok-3 should include is_error field"
        );
        assert_eq!(translated2[0]["is_error"], json!(false));

        // Test with claude model (should include is_error)
        let translated3 = super::translate_message(&message, "claude-sonnet-4-6");
        assert!(
            translated3[0].get("is_error").is_some(),
            "claude should include is_error field"
        );
    }

    #[test]
    fn translate_message_excludes_is_error_for_kimi_models() {
        use crate::types::{InputContentBlock, InputMessage, ToolResultContentBlock};

        // Test with kimi-k2.5 (should EXCLUDE is_error)
        let message = InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: vec![ToolResultContentBlock::Text {
                    text: "Error occurred".to_string(),
                }],
                is_error: true,
            }],
        };

        let translated = super::translate_message(&message, "kimi-k2.5");
        assert_eq!(translated.len(), 1);
        let tool_msg = &translated[0];
        assert_eq!(tool_msg["role"], json!("tool"));
        assert_eq!(tool_msg["tool_call_id"], json!("call_1"));
        assert_eq!(tool_msg["content"], json!("Error occurred"));
        assert!(
            tool_msg.get("is_error").is_none(),
            "kimi-k2.5 must NOT include is_error field (would cause 400 Bad Request)"
        );

        // Test with kimi-k1.5
        let translated2 = super::translate_message(&message, "kimi-k1.5");
        assert!(
            translated2[0].get("is_error").is_none(),
            "kimi-k1.5 must NOT include is_error field"
        );

        // Test with dashscope/kimi-k2.5 (with provider prefix)
        let translated3 = super::translate_message(&message, "dashscope/kimi-k2.5");
        assert!(
            translated3[0].get("is_error").is_none(),
            "dashscope/kimi-k2.5 must NOT include is_error field"
        );
    }

    #[test]
    fn build_chat_completion_request_kimi_vs_non_kimi_tool_results() {
        use crate::types::{InputContentBlock, InputMessage, ToolResultContentBlock};

        // Helper to create a request with a tool result
        let make_request = |model: &str| MessageRequest {
            model: model.to_string(),
            max_tokens: 100,
            messages: vec![
                InputMessage {
                    role: "assistant".to_string(),
                    content: vec![InputContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({"path": "/tmp/test"}),
                    }],
                },
                InputMessage {
                    role: "user".to_string(),
                    content: vec![InputContentBlock::ToolResult {
                        tool_use_id: "call_1".to_string(),
                        content: vec![ToolResultContentBlock::Text {
                            text: "file contents".to_string(),
                        }],
                        is_error: false,
                    }],
                },
            ],
            stream: false,
            ..Default::default()
        };

        // Non-kimi model: should have is_error field
        let request_gpt = make_request("gpt-4o");
        let payload_gpt = build_chat_completion_request(&request_gpt, OpenAiCompatConfig::openai());
        let messages_gpt = payload_gpt["messages"].as_array().unwrap();
        let tool_msg_gpt = messages_gpt.iter().find(|m| m["role"] == "tool").unwrap();
        assert!(
            tool_msg_gpt.get("is_error").is_some(),
            "gpt-4o request should include is_error in tool result"
        );

        // kimi model: should NOT have is_error field
        let request_kimi = make_request("kimi-k2.5");
        let payload_kimi =
            build_chat_completion_request(&request_kimi, OpenAiCompatConfig::dashscope());
        let messages_kimi = payload_kimi["messages"].as_array().unwrap();
        let tool_msg_kimi = messages_kimi.iter().find(|m| m["role"] == "tool").unwrap();
        assert!(
            tool_msg_kimi.get("is_error").is_none(),
            "kimi-k2.5 request must NOT include is_error in tool result (would cause 400)"
        );

        // Verify both have the essential fields
        assert_eq!(tool_msg_gpt["tool_call_id"], json!("call_1"));
        assert_eq!(tool_msg_kimi["tool_call_id"], json!("call_1"));
        assert_eq!(tool_msg_gpt["content"], json!("file contents"));
        assert_eq!(tool_msg_kimi["content"], json!("file contents"));
    }

    // ============================================================================
    // US-021: Request body size pre-flight check tests
    // ============================================================================

    #[test]
    fn estimate_request_body_size_returns_reasonable_estimate() {
        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 100,
            messages: vec![InputMessage::user_text("Hello world".to_string())],
            stream: false,
            ..Default::default()
        };

        let size = super::estimate_request_body_size(&request, OpenAiCompatConfig::openai());
        // Should be non-zero and reasonable for a small request
        assert!(size > 0, "estimated size should be positive");
        assert!(size < 10_000, "small request should be under 10KB");
    }

    #[test]
    fn check_request_body_size_passes_for_small_requests() {
        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 100,
            messages: vec![InputMessage::user_text("Hello".to_string())],
            stream: false,
            ..Default::default()
        };

        // Should pass for all providers with a small request
        assert!(super::check_request_body_size(&request, OpenAiCompatConfig::openai()).is_ok());
        assert!(super::check_request_body_size(&request, OpenAiCompatConfig::xai()).is_ok());
        assert!(super::check_request_body_size(&request, OpenAiCompatConfig::dashscope()).is_ok());
    }

    #[test]
    fn check_request_body_size_fails_for_dashscope_when_exceeds_6mb() {
        // Create a request that exceeds DashScope's 6MB limit
        let large_content = "x".repeat(7_000_000); // 7MB of content
        let request = MessageRequest {
            model: "qwen-plus".to_string(),
            max_tokens: 100,
            messages: vec![InputMessage::user_text(large_content)],
            stream: false,
            ..Default::default()
        };

        let result = super::check_request_body_size(&request, OpenAiCompatConfig::dashscope());
        assert!(result.is_err(), "should fail for 7MB request to DashScope");

        let err = result.unwrap_err();
        match err {
            crate::error::ApiError::RequestBodySizeExceeded {
                estimated_bytes,
                max_bytes,
                provider,
            } => {
                assert_eq!(provider, "DashScope");
                assert_eq!(max_bytes, 6_291_456); // 6MB limit
                assert!(estimated_bytes > max_bytes);
            }
            _ => panic!("expected RequestBodySizeExceeded error, got {err:?}"),
        }
    }

    #[test]
    fn check_request_body_size_allows_large_requests_for_openai() {
        // Create a request that exceeds DashScope's limit but is under OpenAI's 100MB limit
        let large_content = "x".repeat(10_000_000); // 10MB of content
        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 100,
            messages: vec![InputMessage::user_text(large_content)],
            stream: false,
            ..Default::default()
        };

        // Should pass for OpenAI (100MB limit)
        assert!(
            super::check_request_body_size(&request, OpenAiCompatConfig::openai()).is_ok(),
            "10MB request should pass for OpenAI's 100MB limit"
        );

        // Should fail for DashScope (6MB limit)
        assert!(
            super::check_request_body_size(&request, OpenAiCompatConfig::dashscope()).is_err(),
            "10MB request should fail for DashScope's 6MB limit"
        );
    }

    #[test]
    fn provider_specific_size_limits_are_correct() {
        assert_eq!(OpenAiCompatConfig::dashscope().max_request_body_bytes, 6_291_456); // 6MB
        assert_eq!(OpenAiCompatConfig::openai().max_request_body_bytes, 104_857_600); // 100MB
        assert_eq!(OpenAiCompatConfig::xai().max_request_body_bytes, 52_428_800); // 50MB
    }

    #[test]
    fn strip_routing_prefix_strips_kimi_provider_prefix() {
        // US-023: kimi prefix should be stripped for wire format
        assert_eq!(super::strip_routing_prefix("kimi/kimi-k2.5"), "kimi-k2.5");
        assert_eq!(super::strip_routing_prefix("kimi-k2.5"), "kimi-k2.5"); // no prefix, unchanged
        assert_eq!(super::strip_routing_prefix("kimi/kimi-k1.5"), "kimi-k1.5");
    }
}
