use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use runtime::format_usd;
use runtime::{
    load_oauth_credentials, save_oauth_credentials, OAuthConfig, OAuthRefreshRequest,
    OAuthTokenExchangeRequest,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use telemetry::{AnalyticsEvent, AnthropicRequestProfile, ClientIdentity, SessionTracer};

use crate::error::ApiError;
use crate::http_client::build_http_client_or_default;
use crate::prompt_cache::{PromptCache, PromptCacheRecord, PromptCacheStats};

use super::{
    anthropic_missing_credentials, model_token_limit, resolve_model_alias, Provider, ProviderFuture,
};
use crate::sse::SseParser;
use crate::types::{MessageDeltaEvent, MessageRequest, MessageResponse, StreamEvent, Usage};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const REQUEST_ID_HEADER: &str = "request-id";
const ALT_REQUEST_ID_HEADER: &str = "x-request-id";
const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(128);
const DEFAULT_MAX_RETRIES: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthSource {
    None,
    ApiKey(String),
    BearerToken(String),
    ApiKeyAndBearer {
        api_key: String,
        bearer_token: String,
    },
}

impl AuthSource {
    pub fn from_env() -> Result<Self, ApiError> {
        let api_key = read_env_non_empty("ANTHROPIC_API_KEY")?;
        let auth_token = read_env_non_empty("ANTHROPIC_AUTH_TOKEN")?;
        match (api_key, auth_token) {
            (Some(api_key), Some(bearer_token)) => Ok(Self::ApiKeyAndBearer {
                api_key,
                bearer_token,
            }),
            (Some(api_key), None) => Ok(Self::ApiKey(api_key)),
            (None, Some(bearer_token)) => Ok(Self::BearerToken(bearer_token)),
            (None, None) => Err(anthropic_missing_credentials()),
        }
    }

    #[must_use]
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey(api_key) | Self::ApiKeyAndBearer { api_key, .. } => Some(api_key),
            Self::None | Self::BearerToken(_) => None,
        }
    }

    #[must_use]
    pub fn bearer_token(&self) -> Option<&str> {
        match self {
            Self::BearerToken(token)
            | Self::ApiKeyAndBearer {
                bearer_token: token,
                ..
            } => Some(token),
            Self::None | Self::ApiKey(_) => None,
        }
    }

    #[must_use]
    pub fn masked_authorization_header(&self) -> &'static str {
        if self.bearer_token().is_some() {
            "Bearer [REDACTED]"
        } else {
            "<absent>"
        }
    }

    pub fn apply(&self, mut request_builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(api_key) = self.api_key() {
            request_builder = request_builder.header("x-api-key", api_key);
        }
        if let Some(token) = self.bearer_token() {
            request_builder = request_builder.bearer_auth(token);
        }
        request_builder
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OAuthTokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl From<OAuthTokenSet> for AuthSource {
    fn from(value: OAuthTokenSet) -> Self {
        Self::BearerToken(value.access_token)
    }
}

#[derive(Debug, Clone)]
pub struct AnthropicClient {
    http: reqwest::Client,
    auth: AuthSource,
    base_url: String,
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
    request_profile: AnthropicRequestProfile,
    session_tracer: Option<SessionTracer>,
    prompt_cache: Option<PromptCache>,
    last_prompt_cache_record: Arc<Mutex<Option<PromptCacheRecord>>>,
}

impl AnthropicClient {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: build_http_client_or_default(),
            auth: AuthSource::ApiKey(api_key.into()),
            base_url: DEFAULT_BASE_URL.to_string(),
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
            request_profile: AnthropicRequestProfile::default(),
            session_tracer: None,
            prompt_cache: None,
            last_prompt_cache_record: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn from_auth(auth: AuthSource) -> Self {
        Self {
            http: build_http_client_or_default(),
            auth,
            base_url: DEFAULT_BASE_URL.to_string(),
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
            request_profile: AnthropicRequestProfile::default(),
            session_tracer: None,
            prompt_cache: None,
            last_prompt_cache_record: Arc::new(Mutex::new(None)),
        }
    }

    pub fn from_env() -> Result<Self, ApiError> {
        Ok(Self::from_auth(AuthSource::from_env_or_saved()?).with_base_url(read_base_url()))
    }

    #[must_use]
    pub fn with_auth_source(mut self, auth: AuthSource) -> Self {
        self.auth = auth;
        self
    }

    #[must_use]
    pub fn with_auth_token(mut self, auth_token: Option<String>) -> Self {
        match (
            self.auth.api_key().map(ToOwned::to_owned),
            auth_token.filter(|token| !token.is_empty()),
        ) {
            (Some(api_key), Some(bearer_token)) => {
                self.auth = AuthSource::ApiKeyAndBearer {
                    api_key,
                    bearer_token,
                };
            }
            (Some(api_key), None) => {
                self.auth = AuthSource::ApiKey(api_key);
            }
            (None, Some(bearer_token)) => {
                self.auth = AuthSource::BearerToken(bearer_token);
            }
            (None, None) => {
                self.auth = AuthSource::None;
            }
        }
        self
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
    /// timeout configuration. This controls connect and request-level
    /// timeouts for all outbound API calls.
    #[must_use]
    pub fn with_timeout(mut self, timeout: &crate::http_client::TimeoutConfig) -> Self {
        self.http = crate::http_client::build_http_client_with_opts(
            &crate::http_client::ProxyConfig::from_env(),
            timeout,
        )
        .unwrap_or_else(|_| reqwest::Client::new());
        self
    }

    #[must_use]
    pub fn with_session_tracer(mut self, session_tracer: SessionTracer) -> Self {
        self.session_tracer = Some(session_tracer);
        self
    }

    #[must_use]
    pub fn with_client_identity(mut self, client_identity: ClientIdentity) -> Self {
        self.request_profile.client_identity = client_identity;
        self
    }

    #[must_use]
    pub fn with_beta(mut self, beta: impl Into<String>) -> Self {
        self.request_profile = self.request_profile.with_beta(beta);
        self
    }

    #[must_use]
    pub fn with_extra_body_param(mut self, key: impl Into<String>, value: Value) -> Self {
        self.request_profile = self.request_profile.with_extra_body(key, value);
        self
    }

    #[must_use]
    pub fn with_prompt_cache(mut self, prompt_cache: PromptCache) -> Self {
        self.prompt_cache = Some(prompt_cache);
        self
    }

    #[must_use]
    pub fn prompt_cache_stats(&self) -> Option<PromptCacheStats> {
        self.prompt_cache.as_ref().map(PromptCache::stats)
    }

    #[must_use]
    pub fn request_profile(&self) -> &AnthropicRequestProfile {
        &self.request_profile
    }

    #[must_use]
    pub fn session_tracer(&self) -> Option<&SessionTracer> {
        self.session_tracer.as_ref()
    }

    #[must_use]
    pub fn prompt_cache(&self) -> Option<&PromptCache> {
        self.prompt_cache.as_ref()
    }

    #[must_use]
    pub fn take_last_prompt_cache_record(&self) -> Option<PromptCacheRecord> {
        self.last_prompt_cache_record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    #[must_use]
    pub fn with_request_profile(mut self, request_profile: AnthropicRequestProfile) -> Self {
        self.request_profile = request_profile;
        self
    }

    #[must_use]
    pub fn auth_source(&self) -> &AuthSource {
        &self.auth
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let request = MessageRequest {
            stream: false,
            ..request.clone()
        };

        if let Some(prompt_cache) = &self.prompt_cache {
            if let Some(response) = prompt_cache.lookup_completion(&request) {
                return Ok(response);
            }
        }

        self.preflight_message_request(&request).await?;

        let http_response = self.send_with_retry(&request).await?;
        let request_id = request_id_from_headers(http_response.headers());
        let body = http_response.text().await.map_err(ApiError::from)?;
        let mut response = serde_json::from_str::<MessageResponse>(&body).map_err(|error| {
            ApiError::json_deserialize("Anthropic", &request.model, &body, error)
        })?;
        if response.request_id.is_none() {
            response.request_id = request_id;
        }

        if let Some(prompt_cache) = &self.prompt_cache {
            let record = prompt_cache.record_response(&request, &response);
            self.store_last_prompt_cache_record(record);
        }
        if let Some(session_tracer) = &self.session_tracer {
            session_tracer.record_analytics(
                AnalyticsEvent::new("api", "message_usage")
                    .with_property(
                        "request_id",
                        response
                            .request_id
                            .clone()
                            .map_or(Value::Null, Value::String),
                    )
                    .with_property("total_tokens", Value::from(response.total_tokens()))
                    .with_property(
                        "estimated_cost_usd",
                        Value::String(format_usd(
                            response
                                .usage
                                .estimated_cost_usd(&response.model)
                                .total_cost_usd(),
                        )),
                    ),
            );
        }
        Ok(response)
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        self.preflight_message_request(request).await?;
        let response = self
            .send_with_retry(&request.clone().with_streaming())
            .await?;
        Ok(MessageStream {
            request_id: request_id_from_headers(response.headers()),
            response,
            parser: SseParser::new().with_context("Anthropic", request.model.clone()),
            pending: VecDeque::new(),
            done: false,
            request: request.clone(),
            prompt_cache: self.prompt_cache.clone(),
            latest_usage: None,
            usage_recorded: false,
            last_prompt_cache_record: Arc::clone(&self.last_prompt_cache_record),
        })
    }

    pub async fn exchange_oauth_code(
        &self,
        config: &OAuthConfig,
        request: &OAuthTokenExchangeRequest,
    ) -> Result<OAuthTokenSet, ApiError> {
        let response = self
            .http
            .post(&config.token_url)
            .header("content-type", "application/x-www-form-urlencoded")
            .form(&request.form_params())
            .send()
            .await
            .map_err(ApiError::from)?;
        let response = expect_success(response).await?;
        let body = response.text().await.map_err(ApiError::from)?;
        serde_json::from_str::<OAuthTokenSet>(&body).map_err(|error| {
            ApiError::json_deserialize("Anthropic OAuth (exchange)", "n/a", &body, error)
        })
    }

    pub async fn refresh_oauth_token(
        &self,
        config: &OAuthConfig,
        request: &OAuthRefreshRequest,
    ) -> Result<OAuthTokenSet, ApiError> {
        let response = self
            .http
            .post(&config.token_url)
            .header("content-type", "application/x-www-form-urlencoded")
            .form(&request.form_params())
            .send()
            .await
            .map_err(ApiError::from)?;
        let response = expect_success(response).await?;
        let body = response.text().await.map_err(ApiError::from)?;
        serde_json::from_str::<OAuthTokenSet>(&body).map_err(|error| {
            ApiError::json_deserialize("Anthropic OAuth (refresh)", "n/a", &body, error)
        })
    }

    async fn send_with_retry(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        let mut attempts = 0;
        let mut last_error: Option<ApiError>;

        loop {
            attempts += 1;
            if let Some(session_tracer) = &self.session_tracer {
                session_tracer.record_http_request_started(
                    attempts,
                    "POST",
                    "/v1/messages",
                    Map::new(),
                );
            }
            match self.send_raw_request(request).await {
                Ok(response) => match expect_success(response).await {
                    Ok(response) => {
                        if let Some(session_tracer) = &self.session_tracer {
                            session_tracer.record_http_request_succeeded(
                                attempts,
                                "POST",
                                "/v1/messages",
                                response.status().as_u16(),
                                request_id_from_headers(response.headers()),
                                Map::new(),
                            );
                        }
                        return Ok(response);
                    }
                    Err(error) if error.is_retryable() && attempts <= self.max_retries + 1 => {
                        self.record_request_failure(attempts, &error);
                        last_error = Some(error);
                    }
                    Err(error) => {
                        let error = enrich_bearer_auth_error(error, &self.auth);
                        self.record_request_failure(attempts, &error);
                        return Err(error);
                    }
                },
                Err(error) if error.is_retryable() && attempts <= self.max_retries + 1 => {
                    self.record_request_failure(attempts, &error);
                    last_error = Some(error);
                }
                Err(error) => {
                    self.record_request_failure(attempts, &error);
                    return Err(error);
                }
            }

            if attempts > self.max_retries {
                break;
            }

            let delay = if let Some(retry_after) = last_error.as_ref().and_then(|e| e.retry_after()) {
                retry_after
            } else {
                self.jittered_backoff_for_attempt(attempts)?
            };
            tokio::time::sleep(delay).await;
        }

        Err(ApiError::RetriesExhausted {
            attempts,
            last_error: Box::new(last_error.expect("retry loop must capture an error")),
        })
    }

    async fn send_raw_request(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        let request_url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut request_body = self.request_profile.render_json_body(request)?;
        strip_unsupported_beta_body_fields(&mut request_body);
        let request_builder = self.build_request(&request_url).json(&request_body);
        request_builder.send().await.map_err(ApiError::from)
    }

    fn build_request(&self, request_url: &str) -> reqwest::RequestBuilder {
        let request_builder = self
            .http
            .post(request_url)
            .header("content-type", "application/json");
        let mut request_builder = self.auth.apply(request_builder);
        for (header_name, header_value) in self.request_profile.header_pairs() {
            request_builder = request_builder.header(header_name, header_value);
        }
        request_builder
    }

    async fn preflight_message_request(&self, request: &MessageRequest) -> Result<(), ApiError> {
        // Always run the local byte-estimate guard first. This catches
        // oversized requests even if the remote count_tokens endpoint is
        // unreachable, misconfigured, or unimplemented (e.g., third-party
        // Anthropic-compatible gateways). If byte estimation already flags
        // the request as oversized, reject immediately without a network
        // round trip.
        super::preflight_message_request(request)?;

        let Some(limit) = model_token_limit(&request.model) else {
            return Ok(());
        };

        // Best-effort refinement using the Anthropic count_tokens endpoint.
        // On any failure (network, parse, auth), fall back to the local
        // byte-estimate result which already passed above.
        let Ok(counted_input_tokens) = self.count_tokens(request).await else {
            return Ok(());
        };
        let estimated_total_tokens = counted_input_tokens.saturating_add(request.max_tokens);
        if estimated_total_tokens > limit.context_window_tokens {
            return Err(ApiError::ContextWindowExceeded {
                model: resolve_model_alias(&request.model),
                estimated_input_tokens: counted_input_tokens,
                requested_output_tokens: request.max_tokens,
                estimated_total_tokens,
                context_window_tokens: limit.context_window_tokens,
            });
        }

        Ok(())
    }

    async fn count_tokens(&self, request: &MessageRequest) -> Result<u32, ApiError> {
        #[derive(serde::Deserialize)]
        struct CountTokensResponse {
            input_tokens: u32,
        }

        let request_url = format!(
            "{}/v1/messages/count_tokens",
            self.base_url.trim_end_matches('/')
        );
        let mut request_body = self.request_profile.render_json_body(request)?;
        strip_unsupported_beta_body_fields(&mut request_body);
        let response = self
            .build_request(&request_url)
            .json(&request_body)
            .send()
            .await
            .map_err(ApiError::from)?;

        let response = expect_success(response).await?;
        let body = response.text().await.map_err(ApiError::from)?;
        let parsed = serde_json::from_str::<CountTokensResponse>(&body).map_err(|error| {
            ApiError::json_deserialize("Anthropic count_tokens", &request.model, &body, error)
        })?;
        Ok(parsed.input_tokens)
    }

    fn record_request_failure(&self, attempt: u32, error: &ApiError) {
        if let Some(session_tracer) = &self.session_tracer {
            session_tracer.record_http_request_failed(
                attempt,
                "POST",
                "/v1/messages",
                error.to_string(),
                error.is_retryable(),
                Map::new(),
            );
        }
    }

    fn store_last_prompt_cache_record(&self, record: PromptCacheRecord) {
        *self
            .last_prompt_cache_record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(record);
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
    // splitmix64 finalizer — mixes the low bits so large bases still see
    // jitter across their full range instead of being clamped to subsec nanos.
    let mut mixed = raw_nanos
        .wrapping_add(tick)
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    mixed ^= mixed >> 31;
    // Inclusive upper bound: jitter may equal `base`, matching "up to base".
    let jitter_nanos = mixed % base_nanos.saturating_add(1);
    Duration::from_nanos(jitter_nanos)
}

impl AuthSource {
    pub fn from_env_or_saved() -> Result<Self, ApiError> {
        if let Some(api_key) = read_env_non_empty("ANTHROPIC_API_KEY")? {
            return match read_env_non_empty("ANTHROPIC_AUTH_TOKEN")? {
                Some(bearer_token) => Ok(Self::ApiKeyAndBearer {
                    api_key,
                    bearer_token,
                }),
                None => Ok(Self::ApiKey(api_key)),
            };
        }
        if let Some(bearer_token) = read_env_non_empty("ANTHROPIC_AUTH_TOKEN")? {
            return Ok(Self::BearerToken(bearer_token));
        }
        Err(anthropic_missing_credentials())
    }
}

#[must_use]
pub fn oauth_token_is_expired(token_set: &OAuthTokenSet) -> bool {
    token_set
        .expires_at
        .is_some_and(|expires_at| expires_at <= now_unix_timestamp())
}

pub fn resolve_saved_oauth_token(config: &OAuthConfig) -> Result<Option<OAuthTokenSet>, ApiError> {
    let Some(token_set) = load_saved_oauth_token()? else {
        return Ok(None);
    };
    resolve_saved_oauth_token_set(config, token_set).map(Some)
}

pub fn has_auth_from_env_or_saved() -> Result<bool, ApiError> {
    Ok(read_env_non_empty("ANTHROPIC_API_KEY")?.is_some()
        || read_env_non_empty("ANTHROPIC_AUTH_TOKEN")?.is_some())
}

pub fn resolve_startup_auth_source<F>(load_oauth_config: F) -> Result<AuthSource, ApiError>
where
    F: FnOnce() -> Result<Option<OAuthConfig>, ApiError>,
{
    let _ = load_oauth_config;
    if let Some(api_key) = read_env_non_empty("ANTHROPIC_API_KEY")? {
        return match read_env_non_empty("ANTHROPIC_AUTH_TOKEN")? {
            Some(bearer_token) => Ok(AuthSource::ApiKeyAndBearer {
                api_key,
                bearer_token,
            }),
            None => Ok(AuthSource::ApiKey(api_key)),
        };
    }
    if let Some(bearer_token) = read_env_non_empty("ANTHROPIC_AUTH_TOKEN")? {
        return Ok(AuthSource::BearerToken(bearer_token));
    }
    Err(anthropic_missing_credentials())
}

fn resolve_saved_oauth_token_set(
    config: &OAuthConfig,
    token_set: OAuthTokenSet,
) -> Result<OAuthTokenSet, ApiError> {
    if !oauth_token_is_expired(&token_set) {
        return Ok(token_set);
    }
    let Some(refresh_token) = token_set.refresh_token.clone() else {
        return Err(ApiError::ExpiredOAuthToken);
    };
    let client = AnthropicClient::from_auth(AuthSource::None).with_base_url(read_base_url());
    let refreshed = client_runtime_block_on(async {
        client
            .refresh_oauth_token(
                config,
                &OAuthRefreshRequest::from_config(
                    config,
                    refresh_token,
                    Some(token_set.scopes.clone()),
                ),
            )
            .await
    })?;
    let resolved = OAuthTokenSet {
        access_token: refreshed.access_token,
        refresh_token: refreshed.refresh_token.or(token_set.refresh_token),
        expires_at: refreshed.expires_at,
        scopes: refreshed.scopes,
    };
    save_oauth_credentials(&runtime::OAuthTokenSet {
        access_token: resolved.access_token.clone(),
        refresh_token: resolved.refresh_token.clone(),
        expires_at: resolved.expires_at,
        scopes: resolved.scopes.clone(),
    })
    .map_err(ApiError::from)?;
    Ok(resolved)
}

fn client_runtime_block_on<F, T>(future: F) -> Result<T, ApiError>
where
    F: std::future::Future<Output = Result<T, ApiError>>,
{
    tokio::runtime::Runtime::new()
        .map_err(ApiError::from)?
        .block_on(future)
}

fn load_saved_oauth_token() -> Result<Option<OAuthTokenSet>, ApiError> {
    let token_set = load_oauth_credentials().map_err(ApiError::from)?;
    Ok(token_set.map(|token_set| OAuthTokenSet {
        access_token: token_set.access_token,
        refresh_token: token_set.refresh_token,
        expires_at: token_set.expires_at,
        scopes: token_set.scopes,
    }))
}

fn now_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn read_env_non_empty(key: &str) -> Result<Option<String>, ApiError> {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(super::dotenv_value(key)),
        Err(error) => Err(ApiError::from(error)),
    }
}

#[cfg(test)]
fn read_api_key() -> Result<String, ApiError> {
    let auth = AuthSource::from_env_or_saved()?;
    auth.api_key()
        .or_else(|| auth.bearer_token())
        .map(ToOwned::to_owned)
        .ok_or_else(anthropic_missing_credentials)
}

#[cfg(test)]
fn read_auth_token() -> Option<String> {
    read_env_non_empty("ANTHROPIC_AUTH_TOKEN")
        .ok()
        .and_then(std::convert::identity)
}

#[must_use]
pub fn read_base_url() -> String {
    std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn request_id_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(REQUEST_ID_HEADER)
        .or_else(|| headers.get(ALT_REQUEST_ID_HEADER))
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

impl Provider for AnthropicClient {
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
    parser: SseParser,
    pending: VecDeque<StreamEvent>,
    done: bool,
    request: MessageRequest,
    prompt_cache: Option<PromptCache>,
    latest_usage: Option<Usage>,
    usage_recorded: bool,
    last_prompt_cache_record: Arc<Mutex<Option<PromptCacheRecord>>>,
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                self.observe_event(&event);
                return Ok(Some(event));
            }

            if self.done {
                let remaining = self.parser.finish()?;
                self.pending.extend(remaining);
                if let Some(event) = self.pending.pop_front() {
                    return Ok(Some(event));
                }
                return Ok(None);
            }

            match self.response.chunk().await? {
                Some(chunk) => {
                    self.pending.extend(self.parser.push(&chunk)?);
                }
                None => {
                    self.done = true;
                }
            }
        }
    }

    fn observe_event(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::MessageDelta(MessageDeltaEvent { usage, .. }) => {
                self.latest_usage = Some(usage.clone());
            }
            StreamEvent::MessageStop(_) => {
                if !self.usage_recorded {
                    if let (Some(prompt_cache), Some(usage)) =
                        (&self.prompt_cache, self.latest_usage.as_ref())
                    {
                        let record = prompt_cache.record_usage(&self.request, usage);
                        *self
                            .last_prompt_cache_record
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(record);
                    }
                    self.usage_recorded = true;
                }
            }
            _ => {}
        }
    }
}

async fn expect_success(response: reqwest::Response) -> Result<reqwest::Response, ApiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let headers = response.headers().clone();
    let request_id = request_id_from_headers(&headers);
    let body = response.text().await.unwrap_or_else(|_| String::new());
    let parsed_error = serde_json::from_str::<AnthropicErrorEnvelope>(&body).ok();
    let retryable = is_retryable_status(status);
    let retry_after = parse_retry_after(&headers, status);

    Err(ApiError::Api {
        status,
        error_type: parsed_error
            .as_ref()
            .map(|error| error.error.error_type.clone()),
        message: parsed_error
            .as_ref()
            .map(|error| error.error.message.clone()),
        request_id,
        body,
        retryable,
        suggested_action: None,
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

/// Some providers return HTTP 400 with an unparseable body when a gateway
/// or proxy flakes (e.g. "HTTP 400 from backend (no parseable body)").
/// These are transient network blips, not actual bad requests, and should
/// be retried. We detect them by checking the body for known gateway error
/// phrases.
fn is_retryable_400(status: reqwest::StatusCode, body: &str) -> bool {
    if status != reqwest::StatusCode::BAD_REQUEST {
        return false;
    }
    let lowered = body.to_ascii_lowercase();
    lowered.contains("no parseable body")
        || lowered.contains("connection reset")
        || lowered.contains("broken pipe")
        || lowered.contains("empty reply from server")
}

/// Anthropic API keys (`sk-ant-*`) are accepted over the `x-api-key` header
/// and rejected with HTTP 401 "Invalid bearer token" when sent as a Bearer
/// token via `ANTHROPIC_AUTH_TOKEN`. This happens often enough in the wild
/// (users copy-paste an `sk-ant-...` key into `ANTHROPIC_AUTH_TOKEN` because
/// the env var name sounds auth-related) that a bare 401 error is useless.
/// When we detect this exact shape, append a hint to the error message that
/// points the user at the one-line fix.
const SK_ANT_BEARER_HINT: &str = "sk-ant-* keys go in ANTHROPIC_API_KEY (x-api-key header), not ANTHROPIC_AUTH_TOKEN (Bearer header). Move your key to ANTHROPIC_API_KEY.";

fn enrich_bearer_auth_error(error: ApiError, auth: &AuthSource) -> ApiError {
    let ApiError::Api {
        status,
        error_type,
        message,
        request_id,
        body,
        retryable,
        suggested_action,
        retry_after,
        ..
    } = error
    else {
        return error;
    };
    if status.as_u16() != 401 {
        return ApiError::Api {
            status,
            error_type,
            message,
            request_id,
            body,
            retryable,
            suggested_action,
            retry_after,
        };
    }
    let Some(bearer_token) = auth.bearer_token() else {
        return ApiError::Api {
            status,
            error_type,
            message,
            request_id,
            body,
            retryable,
            suggested_action,
            retry_after,
        };
    };
    if !bearer_token.starts_with("sk-ant-") {
        return ApiError::Api {
            status,
            error_type,
            message,
            request_id,
            body,
            retryable,
            suggested_action,
            retry_after,
        };
    }
    // Only append the hint when the AuthSource is pure BearerToken. If both
    // api_key and bearer_token are present (`ApiKeyAndBearer`), the x-api-key
    // header is already being sent alongside the Bearer header and the 401
    // is coming from a different cause — adding the hint would be misleading.
    if auth.api_key().is_some() {
        return ApiError::Api {
            status,
            error_type,
            message,
            request_id,
            body,
            retryable,
            suggested_action,
            retry_after,
        };
    }
    let enriched_message = match message {
        Some(existing) => Some(format!("{existing} — hint: {SK_ANT_BEARER_HINT}")),
        None => Some(format!("hint: {SK_ANT_BEARER_HINT}")),
    };
    ApiError::Api {
        status,
        error_type,
        message: enriched_message,
        request_id,
        body,
        retryable,
        suggested_action,
        retry_after,
    }
}

/// Remove beta-only body fields that the standard `/v1/messages` and
/// `/v1/messages/count_tokens` endpoints reject as `Extra inputs are not
/// permitted`. The `betas` opt-in is communicated via the `anthropic-beta`
/// HTTP header on these endpoints, never as a JSON body field.
fn strip_unsupported_beta_body_fields(body: &mut Value) {
    if let Some(object) = body.as_object_mut() {
        object.remove("betas");
        // These fields are OpenAI-compatible only; Anthropic rejects them.
        object.remove("frequency_penalty");
        object.remove("presence_penalty");
        // Anthropic uses "stop_sequences" not "stop". Convert if present.
        if let Some(stop_val) = object.remove("stop") {
            if stop_val.as_array().is_some_and(|a| !a.is_empty()) {
                object.insert("stop_sequences".to_string(), stop_val);
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorEnvelope {
    error: AnthropicErrorBody,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorBody {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::{ALT_REQUEST_ID_HEADER, REQUEST_ID_HEADER};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use runtime::{clear_oauth_credentials, save_oauth_credentials, OAuthConfig};

    use super::{
        now_unix_timestamp, oauth_token_is_expired, resolve_saved_oauth_token,
        resolve_startup_auth_source, AnthropicClient, AuthSource, OAuthTokenSet,
    };
    use crate::types::{ContentBlockDelta, MessageRequest};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn temp_config_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "api-oauth-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ))
    }

    fn cleanup_temp_config_home(config_home: &std::path::Path) {
        match std::fs::remove_dir_all(config_home) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cleanup temp dir: {error}"),
        }
    }

    fn sample_oauth_config(token_url: String) -> OAuthConfig {
        OAuthConfig {
            client_id: "runtime-client".to_string(),
            authorize_url: "https://console.test/oauth/authorize".to_string(),
            token_url,
            callback_port: Some(4545),
            manual_redirect_url: Some("https://console.test/oauth/callback".to_string()),
            scopes: vec!["org:read".to_string(), "user:write".to_string()],
        }
    }

    fn spawn_token_server(response_body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let address = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer).expect("read request");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        format!("http://{address}/oauth/token")
    }

    #[test]
    fn read_api_key_requires_presence() {
        let _guard = env_lock();
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("CLAW_CONFIG_HOME");
        let error = super::read_api_key().expect_err("missing key should error");
        assert!(matches!(
            error,
            crate::error::ApiError::MissingCredentials { .. }
        ));
    }

    #[test]
    fn read_api_key_requires_non_empty_value() {
        let _guard = env_lock();
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "");
        std::env::remove_var("ANTHROPIC_API_KEY");
        let error = super::read_api_key().expect_err("empty key should error");
        assert!(matches!(
            error,
            crate::error::ApiError::MissingCredentials { .. }
        ));
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
    }

    #[test]
    fn read_api_key_prefers_api_key_env() {
        let _guard = env_lock();
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "auth-token");
        std::env::set_var("ANTHROPIC_API_KEY", "legacy-key");
        assert_eq!(
            super::read_api_key().expect("api key should load"),
            "legacy-key"
        );
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn read_auth_token_reads_auth_token_env() {
        let _guard = env_lock();
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "auth-token");
        assert_eq!(super::read_auth_token().as_deref(), Some("auth-token"));
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
    }

    #[test]
    fn oauth_token_maps_to_bearer_auth_source() {
        let auth = AuthSource::from(OAuthTokenSet {
            access_token: "access-token".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(123),
            scopes: vec!["scope:a".to_string()],
        });
        assert_eq!(auth.bearer_token(), Some("access-token"));
        assert_eq!(auth.api_key(), None);
    }

    #[test]
    fn auth_source_from_env_combines_api_key_and_bearer_token() {
        let _guard = env_lock();
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "auth-token");
        std::env::set_var("ANTHROPIC_API_KEY", "legacy-key");
        let auth = AuthSource::from_env().expect("env auth");
        assert_eq!(auth.api_key(), Some("legacy-key"));
        assert_eq!(auth.bearer_token(), Some("auth-token"));
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn auth_source_from_env_or_saved_ignores_saved_oauth_when_env_absent() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::remove_var("ANTHROPIC_API_KEY");
        save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: "saved-access-token".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(now_unix_timestamp() + 300),
            scopes: vec!["scope:a".to_string()],
        })
        .expect("save oauth credentials");

        let error = AuthSource::from_env_or_saved().expect_err("saved oauth should be ignored");
        assert!(error.to_string().contains("ANTHROPIC_API_KEY"));

        clear_oauth_credentials().expect("clear credentials");
        std::env::remove_var("CLAW_CONFIG_HOME");
        cleanup_temp_config_home(&config_home);
    }

    #[test]
    fn oauth_token_expiry_uses_expires_at_timestamp() {
        assert!(oauth_token_is_expired(&OAuthTokenSet {
            access_token: "access-token".to_string(),
            refresh_token: None,
            expires_at: Some(1),
            scopes: Vec::new(),
        }));
        assert!(!oauth_token_is_expired(&OAuthTokenSet {
            access_token: "access-token".to_string(),
            refresh_token: None,
            expires_at: Some(now_unix_timestamp() + 60),
            scopes: Vec::new(),
        }));
    }

    #[test]
    fn resolve_saved_oauth_token_refreshes_expired_credentials() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::remove_var("ANTHROPIC_API_KEY");
        save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: "expired-access-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            expires_at: Some(1),
            scopes: vec!["scope:a".to_string()],
        })
        .expect("save expired oauth credentials");

        let token_url = spawn_token_server(
            "{\"access_token\":\"refreshed-token\",\"refresh_token\":\"fresh-refresh\",\"expires_at\":9999999999,\"scopes\":[\"scope:a\"]}",
        );
        let resolved = resolve_saved_oauth_token(&sample_oauth_config(token_url))
            .expect("resolve refreshed token")
            .expect("token set present");
        assert_eq!(resolved.access_token, "refreshed-token");
        let stored = runtime::load_oauth_credentials()
            .expect("load stored credentials")
            .expect("stored token set");
        assert_eq!(stored.access_token, "refreshed-token");

        clear_oauth_credentials().expect("clear credentials");
        std::env::remove_var("CLAW_CONFIG_HOME");
        cleanup_temp_config_home(&config_home);
    }

    #[test]
    fn resolve_startup_auth_source_ignores_saved_oauth_without_loading_config() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::remove_var("ANTHROPIC_API_KEY");
        save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: "saved-access-token".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(now_unix_timestamp() + 300),
            scopes: vec!["scope:a".to_string()],
        })
        .expect("save oauth credentials");

        let error = resolve_startup_auth_source(|| panic!("config should not be loaded"))
            .expect_err("saved oauth should be ignored");
        assert!(error.to_string().contains("ANTHROPIC_API_KEY"));

        clear_oauth_credentials().expect("clear credentials");
        std::env::remove_var("CLAW_CONFIG_HOME");
        cleanup_temp_config_home(&config_home);
    }

    #[test]
    fn resolve_saved_oauth_token_preserves_refresh_token_when_refresh_response_omits_it() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::remove_var("ANTHROPIC_API_KEY");
        save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: "expired-access-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            expires_at: Some(1),
            scopes: vec!["scope:a".to_string()],
        })
        .expect("save expired oauth credentials");

        let token_url = spawn_token_server(
            "{\"access_token\":\"refreshed-token\",\"expires_at\":9999999999,\"scopes\":[\"scope:a\"]}",
        );
        let resolved = resolve_saved_oauth_token(&sample_oauth_config(token_url))
            .expect("resolve refreshed token")
            .expect("token set present");
        assert_eq!(resolved.access_token, "refreshed-token");
        assert_eq!(resolved.refresh_token.as_deref(), Some("refresh-token"));
        let stored = runtime::load_oauth_credentials()
            .expect("load stored credentials")
            .expect("stored token set");
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-token"));

        clear_oauth_credentials().expect("clear credentials");
        std::env::remove_var("CLAW_CONFIG_HOME");
        cleanup_temp_config_home(&config_home);
    }

    #[test]
    fn message_request_stream_helper_sets_stream_true() {
        let request = MessageRequest {
            model: "claude-opus-4-6".to_string(),
            max_tokens: 64,
            messages: vec![],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            ..Default::default()
        };

        assert!(request.with_streaming().stream);
    }

    #[test]
    fn backoff_doubles_until_maximum() {
        let client = AnthropicClient::new("test-key").with_retry_policy(
            3,
            Duration::from_millis(10),
            Duration::from_millis(25),
        );
        assert_eq!(
            client.backoff_for_attempt(1).expect("attempt 1"),
            Duration::from_millis(10)
        );
        assert_eq!(
            client.backoff_for_attempt(2).expect("attempt 2"),
            Duration::from_millis(20)
        );
        assert_eq!(
            client.backoff_for_attempt(3).expect("attempt 3"),
            Duration::from_millis(25)
        );
    }

    #[test]
    fn jittered_backoff_stays_within_additive_bounds_and_varies() {
        let client = AnthropicClient::new("test-key").with_retry_policy(
            8,
            Duration::from_secs(1),
            Duration::from_secs(128),
        );
        let mut samples = Vec::with_capacity(64);
        for _ in 0..64 {
            let base = client.backoff_for_attempt(3).expect("base attempt 3");
            let jittered = client
                .jittered_backoff_for_attempt(3)
                .expect("jittered attempt 3");
            assert!(
                jittered >= base,
                "jittered delay {jittered:?} must be at least the base {base:?}"
            );
            assert!(
                jittered <= base * 2,
                "jittered delay {jittered:?} must not exceed base*2 {:?}",
                base * 2
            );
            samples.push(jittered);
        }
        let distinct: std::collections::HashSet<_> = samples.iter().collect();
        assert!(
            distinct.len() > 1,
            "jitter should produce varied delays across samples, got {samples:?}"
        );
    }

    #[test]
    fn default_retry_policy_matches_exponential_schedule() {
        let client = AnthropicClient::new("test-key");
        assert_eq!(
            client.backoff_for_attempt(1).expect("attempt 1"),
            Duration::from_secs(1)
        );
        assert_eq!(
            client.backoff_for_attempt(2).expect("attempt 2"),
            Duration::from_secs(2)
        );
        assert_eq!(
            client.backoff_for_attempt(3).expect("attempt 3"),
            Duration::from_secs(4)
        );
        assert_eq!(
            client.backoff_for_attempt(8).expect("attempt 8"),
            Duration::from_secs(128)
        );
    }

    #[test]
    fn retryable_statuses_are_detected() {
        assert!(super::is_retryable_status(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(super::is_retryable_status(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(!super::is_retryable_status(
            reqwest::StatusCode::UNAUTHORIZED
        ));
    }

    #[test]
    fn tool_delta_variant_round_trips() {
        let delta = ContentBlockDelta::InputJsonDelta {
            partial_json: "{\"city\":\"Paris\"}".to_string(),
        };
        let encoded = serde_json::to_string(&delta).expect("delta should serialize");
        let decoded: ContentBlockDelta =
            serde_json::from_str(&encoded).expect("delta should deserialize");
        assert_eq!(decoded, delta);
    }

    #[test]
    fn request_id_uses_primary_or_fallback_header() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, "req_primary".parse().expect("header"));
        assert_eq!(
            super::request_id_from_headers(&headers).as_deref(),
            Some("req_primary")
        );

        headers.clear();
        headers.insert(
            ALT_REQUEST_ID_HEADER,
            "req_fallback".parse().expect("header"),
        );
        assert_eq!(
            super::request_id_from_headers(&headers).as_deref(),
            Some("req_fallback")
        );
    }

    #[test]
    fn auth_source_applies_headers() {
        let auth = AuthSource::ApiKeyAndBearer {
            api_key: "test-key".to_string(),
            bearer_token: "proxy-token".to_string(),
        };
        let request = auth
            .apply(reqwest::Client::new().post("https://example.test"))
            .build()
            .expect("request build");
        let headers = request.headers();
        assert_eq!(
            headers.get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("test-key")
        );
        assert_eq!(
            headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer proxy-token")
        );
    }

    #[test]
    fn strip_unsupported_beta_body_fields_removes_betas_array() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 1024,
            "betas": ["claude-code-20250219", "prompt-caching-scope-2026-01-05"],
            "metadata": {"source": "test"},
        });

        super::strip_unsupported_beta_body_fields(&mut body);

        assert!(
            body.get("betas").is_none(),
            "betas body field must be stripped before sending to /v1/messages"
        );
        assert_eq!(
            body.get("model").and_then(serde_json::Value::as_str),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(body["max_tokens"], serde_json::json!(1024));
        assert_eq!(body["metadata"]["source"], serde_json::json!("test"));
    }

    #[test]
    fn strip_unsupported_beta_body_fields_is_a_noop_when_betas_absent() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 1024,
        });
        let original = body.clone();

        super::strip_unsupported_beta_body_fields(&mut body);

        assert_eq!(body, original);
    }

    #[test]
    fn strip_removes_openai_only_fields_and_converts_stop() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 1024,
            "temperature": 0.7,
            "frequency_penalty": 0.5,
            "presence_penalty": 0.3,
            "stop": ["\n"],
        });

        super::strip_unsupported_beta_body_fields(&mut body);

        // temperature is kept (Anthropic supports it)
        assert_eq!(body["temperature"], serde_json::json!(0.7));
        // frequency_penalty and presence_penalty are removed
        assert!(
            body.get("frequency_penalty").is_none(),
            "frequency_penalty must be stripped for Anthropic"
        );
        assert!(
            body.get("presence_penalty").is_none(),
            "presence_penalty must be stripped for Anthropic"
        );
        // stop is renamed to stop_sequences
        assert!(body.get("stop").is_none(), "stop must be renamed");
        assert_eq!(body["stop_sequences"], serde_json::json!(["\n"]));
    }

    #[test]
    fn strip_does_not_add_empty_stop_sequences() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 1024,
            "stop": [],
        });

        super::strip_unsupported_beta_body_fields(&mut body);

        assert!(body.get("stop").is_none());
        assert!(
            body.get("stop_sequences").is_none(),
            "empty stop should not produce stop_sequences"
        );
    }

    #[test]
    fn rendered_request_body_strips_betas_for_standard_messages_endpoint() {
        let client = AnthropicClient::new("test-key").with_beta("tools-2026-04-01");
        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 64,
            messages: vec![],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            ..Default::default()
        };

        let mut rendered = client
            .request_profile()
            .render_json_body(&request)
            .expect("body should render");
        assert!(
            rendered.get("betas").is_some(),
            "render_json_body still emits betas; the strip helper guards the wire format",
        );
        super::strip_unsupported_beta_body_fields(&mut rendered);

        assert!(
            rendered.get("betas").is_none(),
            "betas must not appear in /v1/messages request bodies"
        );
        assert_eq!(
            rendered.get("model").and_then(serde_json::Value::as_str),
            Some("claude-sonnet-4-6")
        );
    }

    #[test]
    fn enrich_bearer_auth_error_appends_sk_ant_hint_on_401_with_pure_bearer_token() {
        // given
        let auth = AuthSource::BearerToken("sk-ant-api03-deadbeef".to_string());
        let error = crate::error::ApiError::Api {
            status: reqwest::StatusCode::UNAUTHORIZED,
            error_type: Some("authentication_error".to_string()),
            message: Some("Invalid bearer token".to_string()),
            request_id: Some("req_varleg_001".to_string()),
            body: String::new(),
            retryable: false,
            suggested_action: None,
            retry_after: None,
        };

        // when
        let enriched = super::enrich_bearer_auth_error(error, &auth);

        // then
        let rendered = enriched.to_string();
        assert!(
            rendered.contains("Invalid bearer token"),
            "existing provider message should be preserved: {rendered}"
        );
        assert!(
            rendered.contains(
                "sk-ant-* keys go in ANTHROPIC_API_KEY (x-api-key header), not ANTHROPIC_AUTH_TOKEN (Bearer header). Move your key to ANTHROPIC_API_KEY."
            ),
            "rendered error should include the sk-ant-* hint: {rendered}"
        );
        assert!(
            rendered.contains("[trace req_varleg_001]"),
            "request id should still flow through the enriched error: {rendered}"
        );
        match enriched {
            crate::error::ApiError::Api { status, .. } => {
                assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
            }
            other => panic!("expected Api variant, got {other:?}"),
        }
    }

    #[test]
    fn enrich_bearer_auth_error_leaves_non_401_errors_unchanged() {
        // given
        let auth = AuthSource::BearerToken("sk-ant-api03-deadbeef".to_string());
        let error = crate::error::ApiError::Api {
            status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            error_type: Some("api_error".to_string()),
            message: Some("internal server error".to_string()),
            request_id: None,
            body: String::new(),
            retryable: true,
            suggested_action: None,
            retry_after: None,
        };

        // when
        let enriched = super::enrich_bearer_auth_error(error, &auth);

        // then
        let rendered = enriched.to_string();
        assert!(
            !rendered.contains("sk-ant-*"),
            "non-401 errors must not be annotated with the bearer hint: {rendered}"
        );
        assert!(
            rendered.contains("internal server error"),
            "original message must be preserved verbatim: {rendered}"
        );
    }

    #[test]
    fn enrich_bearer_auth_error_ignores_401_when_bearer_token_is_not_sk_ant() {
        // given
        let auth = AuthSource::BearerToken("oauth-access-token-opaque".to_string());
        let error = crate::error::ApiError::Api {
            status: reqwest::StatusCode::UNAUTHORIZED,
            error_type: Some("authentication_error".to_string()),
            message: Some("Invalid bearer token".to_string()),
            request_id: None,
            body: String::new(),
            retryable: false,
            suggested_action: None,
            retry_after: None,
        };

        // when
        let enriched = super::enrich_bearer_auth_error(error, &auth);

        // then
        let rendered = enriched.to_string();
        assert!(
            !rendered.contains("sk-ant-*"),
            "oauth-style bearer tokens must not trigger the sk-ant-* hint: {rendered}"
        );
    }

    #[test]
    fn enrich_bearer_auth_error_skips_hint_when_api_key_header_is_also_present() {
        // given
        let auth = AuthSource::ApiKeyAndBearer {
            api_key: "sk-ant-api03-legitimate".to_string(),
            bearer_token: "sk-ant-api03-deadbeef".to_string(),
        };
        let error = crate::error::ApiError::Api {
            status: reqwest::StatusCode::UNAUTHORIZED,
            error_type: Some("authentication_error".to_string()),
            message: Some("Invalid bearer token".to_string()),
            request_id: None,
            body: String::new(),
            retryable: false,
            suggested_action: None,
            retry_after: None,
        };

        // when
        let enriched = super::enrich_bearer_auth_error(error, &auth);

        // then
        let rendered = enriched.to_string();
        assert!(
            !rendered.contains("sk-ant-*"),
            "hint should be suppressed when x-api-key header is already being sent: {rendered}"
        );
    }

    #[test]
    fn enrich_bearer_auth_error_ignores_401_when_auth_source_has_no_bearer() {
        // given
        let auth = AuthSource::ApiKey("sk-ant-api03-legitimate".to_string());
        let error = crate::error::ApiError::Api {
            status: reqwest::StatusCode::UNAUTHORIZED,
            error_type: Some("authentication_error".to_string()),
            message: Some("Invalid x-api-key".to_string()),
            request_id: None,
            body: String::new(),
            retryable: false,
            suggested_action: None,
            retry_after: None,
        };

        // when
        let enriched = super::enrich_bearer_auth_error(error, &auth);

        // then
        let rendered = enriched.to_string();
        assert!(
            !rendered.contains("sk-ant-*"),
            "bearer hint must not apply when AuthSource is ApiKey-only: {rendered}"
        );
    }

    #[test]
    fn enrich_bearer_auth_error_passes_non_api_errors_through_unchanged() {
        // given
        let auth = AuthSource::BearerToken("sk-ant-api03-deadbeef".to_string());
        let error = crate::error::ApiError::InvalidSseFrame("unterminated event");

        // when
        let enriched = super::enrich_bearer_auth_error(error, &auth);

        // then
        assert!(matches!(
            enriched,
            crate::error::ApiError::InvalidSseFrame(_)
        ));
    }
}
