use std::env::VarError;
use std::fmt::{Display, Formatter};
use std::time::Duration;

const GENERIC_FATAL_WRAPPER_MARKERS: &[&str] = &[
    "something went wrong while processing your request",
    "please try again, or use /new to start a fresh session",
];

const CONTEXT_WINDOW_ERROR_MARKERS: &[&str] = &[
    "maximum context length",
    "context window",
    "context length",
    "too many tokens",
    "prompt is too long",
    "input is too long",
    "request is too large",
];

#[derive(Debug)]
pub enum ApiError {
    MissingCredentials {
        provider: &'static str,
        env_vars: &'static [&'static str],
        /// Optional, runtime-computed hint appended to the error Display
        /// output. Populated when the provider resolver can infer what the
        /// user probably intended (e.g. an `OpenAI` key is set but Anthropic
        /// was selected because no Anthropic credentials exist).
        hint: Option<String>,
    },
    ContextWindowExceeded {
        model: String,
        estimated_input_tokens: u32,
        requested_output_tokens: u32,
        estimated_total_tokens: u32,
        context_window_tokens: u32,
    },
    ExpiredOAuthToken,
    Auth(String),
    InvalidApiKeyEnv(VarError),
    Http(reqwest::Error),
    Io(std::io::Error),
    Json {
        provider: String,
        model: String,
        body_snippet: String,
        source: serde_json::Error,
    },
    Api {
        status: reqwest::StatusCode,
        error_type: Option<String>,
        message: Option<String>,
        request_id: Option<String>,
        body: String,
        retryable: bool,
        /// Suggested user action based on error type (e.g., "Reduce prompt size" for 413)
        suggested_action: Option<String>,
        /// Parsed Retry-After header value (seconds) for 429 responses.
        /// When present, overrides the exponential backoff delay.
        retry_after: Option<Duration>,
    },
    RetriesExhausted {
        attempts: u32,
        last_error: Box<ApiError>,
    },
    InvalidSseFrame(&'static str),
    BackoffOverflow {
        attempt: u32,
        base_delay: Duration,
    },
    RequestBodySizeExceeded {
        estimated_bytes: usize,
        max_bytes: usize,
        provider: &'static str,
    },
}

impl ApiError {
    #[must_use]
    pub const fn missing_credentials(
        provider: &'static str,
        env_vars: &'static [&'static str],
    ) -> Self {
        Self::MissingCredentials {
            provider,
            env_vars,
            hint: None,
        }
    }

    /// Build a `MissingCredentials` error carrying an extra, runtime-computed
    /// hint string that the Display impl appends after the canonical "missing
    /// <provider> credentials" message. Used by the provider resolver to
    /// suggest the likely fix when the user has credentials for a different
    /// provider already in the environment.
    #[must_use]
    pub fn missing_credentials_with_hint(
        provider: &'static str,
        env_vars: &'static [&'static str],
        hint: impl Into<String>,
    ) -> Self {
        Self::MissingCredentials {
            provider,
            env_vars,
            hint: Some(hint.into()),
        }
    }

    /// Build a `Self::Json` enriched with the provider name, the model that
    /// was requested, and the first 200 characters of the raw response body so
    /// that callers can diagnose deserialization failures without re-running
    /// the request.
    #[must_use]
    pub fn json_deserialize(
        provider: impl Into<String>,
        model: impl Into<String>,
        body: &str,
        source: serde_json::Error,
    ) -> Self {
        Self::Json {
            provider: provider.into(),
            model: model.into(),
            body_snippet: truncate_body_snippet(body, 200),
            source,
        }
    }

    #[must_use]
    /// Return the `Retry-After` delay if this error came from a 429 response
    /// that included a `retry-after` header. Callers should prefer this value
    /// over the computed backoff delay when it exists.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Api { retry_after, .. } => *retry_after,
            Self::RetriesExhausted { last_error, .. } => last_error.retry_after(),
            _ => None,
        }
    }

    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http(error) => error.is_connect() || error.is_timeout() || error.is_request(),
            Self::Api { retryable, .. } => *retryable,
            Self::RetriesExhausted { last_error, .. } => last_error.is_retryable(),
            Self::MissingCredentials { .. }
            | Self::ContextWindowExceeded { .. }
            | Self::ExpiredOAuthToken
            | Self::Auth(_)
            | Self::InvalidApiKeyEnv(_)
            | Self::Io(_)
            | Self::Json { .. }
            | Self::InvalidSseFrame(_)
            | Self::BackoffOverflow { .. }
            | Self::RequestBodySizeExceeded { .. } => false,
        }
    }

    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Api { request_id, .. } => request_id.as_deref(),
            Self::RetriesExhausted { last_error, .. } => last_error.request_id(),
            Self::MissingCredentials { .. }
            | Self::ContextWindowExceeded { .. }
            | Self::ExpiredOAuthToken
            | Self::Auth(_)
            | Self::InvalidApiKeyEnv(_)
            | Self::Http(_)
            | Self::Io(_)
            | Self::Json { .. }
            | Self::InvalidSseFrame(_)
            | Self::BackoffOverflow { .. }
            | Self::RequestBodySizeExceeded { .. } => None,
        }
    }

    #[must_use]
    pub fn safe_failure_class(&self) -> &'static str {
        match self {
            Self::RetriesExhausted { .. } if self.is_context_window_failure() => "context_window",
            Self::RetriesExhausted { .. } if self.is_generic_fatal_wrapper() => {
                "provider_retry_exhausted"
            }
            Self::RetriesExhausted { last_error, .. } => last_error.safe_failure_class(),
            Self::MissingCredentials { .. } | Self::ExpiredOAuthToken | Self::Auth(_) => {
                "provider_auth"
            }
            Self::Api { status, .. } if matches!(status.as_u16(), 401 | 403) => "provider_auth",
            Self::ContextWindowExceeded { .. } => "context_window",
            Self::Api { .. } if self.is_context_window_failure() => "context_window",
            Self::Api { status, .. } if status.as_u16() == 429 => "provider_rate_limit",
            Self::Api { .. } if self.is_generic_fatal_wrapper() => "provider_internal",
            Self::Api { .. } => "provider_error",
            Self::Http(_) | Self::InvalidSseFrame(_) | Self::BackoffOverflow { .. } => {
                "provider_transport"
            }
            Self::InvalidApiKeyEnv(_) | Self::Io(_) | Self::Json { .. } => "runtime_io",
            Self::RequestBodySizeExceeded { .. } => "request_size",
        }
    }

    #[must_use]
    pub fn is_generic_fatal_wrapper(&self) -> bool {
        match self {
            Self::Api { message, body, .. } => {
                message
                    .as_deref()
                    .is_some_and(looks_like_generic_fatal_wrapper)
                    || looks_like_generic_fatal_wrapper(body)
            }
            Self::RetriesExhausted { last_error, .. } => last_error.is_generic_fatal_wrapper(),
            Self::MissingCredentials { .. }
            | Self::ContextWindowExceeded { .. }
            | Self::ExpiredOAuthToken
            | Self::Auth(_)
            | Self::InvalidApiKeyEnv(_)
            | Self::Http(_)
            | Self::Io(_)
            | Self::Json { .. }
            | Self::InvalidSseFrame(_)
            | Self::BackoffOverflow { .. }
            | Self::RequestBodySizeExceeded { .. } => false,
        }
    }

    #[must_use]
    pub fn is_context_window_failure(&self) -> bool {
        match self {
            Self::ContextWindowExceeded { .. } => true,
            Self::Api {
                status,
                message,
                body,
                ..
            } => {
                matches!(status.as_u16(), 400 | 413 | 422)
                    && (message
                        .as_deref()
                        .is_some_and(looks_like_context_window_error)
                        || looks_like_context_window_error(body))
            }
            Self::RetriesExhausted { last_error, .. } => last_error.is_context_window_failure(),
            Self::MissingCredentials { .. }
            | Self::ExpiredOAuthToken
            | Self::Auth(_)
            | Self::InvalidApiKeyEnv(_)
            | Self::Http(_)
            | Self::Io(_)
            | Self::Json { .. }
            | Self::InvalidSseFrame(_)
            | Self::BackoffOverflow { .. }
            | Self::RequestBodySizeExceeded { .. } => false,
        }
    }
}

impl Display for ApiError {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials {
                provider,
                env_vars,
                hint,
            } => {
                write!(
                    f,
                    "missing {provider} credentials; export {} before calling the {provider} API",
                    env_vars.join(" or ")
                )?;
                if cfg!(target_os = "windows") {
                    if let Some(primary) = env_vars.first() {
                        write!(
                            f,
                            " (on Windows, environment variables set in PowerShell only persist for the current session; use `setx {primary} <value>` to make it permanent, then open a new terminal, or place a `.env` file containing `{primary}=<value>` in the current working directory)"
                        )?;
                    } else {
                        write!(
                            f,
                            " (on Windows, environment variables set in PowerShell only persist for the current session; use `setx` to make them permanent, then open a new terminal, or place a `.env` file in the current working directory)"
                        )?;
                    }
                }
                if let Some(hint) = hint {
                    write!(f, " — hint: {hint}")?;
                }
                Ok(())
            }
            Self::ContextWindowExceeded {
                model,
                estimated_input_tokens,
                requested_output_tokens,
                estimated_total_tokens,
                context_window_tokens,
            } => write!(
                f,
                "context_window_blocked for {model}: estimated input {estimated_input_tokens} + requested output {requested_output_tokens} = {estimated_total_tokens} tokens exceeds the {context_window_tokens}-token context window; compact the session or reduce request size before retrying"
            ),
            Self::ExpiredOAuthToken => {
                write!(
                    f,
                    "saved OAuth token is expired and no refresh token is available"
                )
            }
            Self::Auth(message) => write!(f, "auth error: {message}"),
            Self::InvalidApiKeyEnv(error) => {
                write!(f, "failed to read credential environment variable: {error}")
            }
            Self::Http(error) => write!(f, "http error: {error}"),
            Self::Io(error) => write!(f, "io error: {error}"),
            Self::Json {
                provider,
                model,
                body_snippet,
                source,
            } => write!(
                f,
                "failed to parse {provider} response for model {model}: {source}; first 200 chars of body: {body_snippet}"
            ),
            Self::Api {
                status,
                error_type,
                message,
                request_id,
                body,
                ..
            } => {
                if let (Some(error_type), Some(message)) = (error_type, message) {
                    write!(f, "api returned {status} ({error_type})")?;
                    if let Some(request_id) = request_id {
                        write!(f, " [trace {request_id}]")?;
                    }
                    write!(f, ": {message}")
                } else {
                    write!(f, "api returned {status}")?;
                    if let Some(request_id) = request_id {
                        write!(f, " [trace {request_id}]")?;
                    }
                    write!(f, ": {body}")
                }
            }
            Self::RetriesExhausted {
                attempts,
                last_error,
            } => write!(f, "api failed after {attempts} attempts: {last_error}"),
            Self::InvalidSseFrame(message) => write!(f, "invalid sse frame: {message}"),
            Self::BackoffOverflow {
                attempt,
                base_delay,
            } => write!(
                f,
                "retry backoff overflowed on attempt {attempt} with base delay {base_delay:?}"
            ),
            Self::RequestBodySizeExceeded {
                estimated_bytes,
                max_bytes,
                provider,
            } => write!(
                f,
                "request body size ({estimated_bytes} bytes) exceeds {provider} limit ({max_bytes} bytes); reduce prompt length or context before retrying"
            ),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<reqwest::Error> for ApiError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value)
    }
}

impl From<std::io::Error> for ApiError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json {
            provider: "unknown".to_string(),
            model: "unknown".to_string(),
            body_snippet: String::new(),
            source: value,
        }
    }
}

impl From<VarError> for ApiError {
    fn from(value: VarError) -> Self {
        Self::InvalidApiKeyEnv(value)
    }
}

fn looks_like_generic_fatal_wrapper(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    GENERIC_FATAL_WRAPPER_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

fn looks_like_context_window_error(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    CONTEXT_WINDOW_ERROR_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

/// Truncate `body` so the resulting snippet contains at most `max_chars`
/// characters (counted by Unicode scalar values, not bytes), preserving the
/// leading slice of the body that the caller most often needs to inspect.
fn truncate_body_snippet(body: &str, max_chars: usize) -> String {
    let mut taken_chars = 0;
    let mut byte_end = 0;
    for (offset, character) in body.char_indices() {
        if taken_chars >= max_chars {
            break;
        }
        taken_chars += 1;
        byte_end = offset + character.len_utf8();
    }
    if taken_chars >= max_chars && byte_end < body.len() {
        format!("{}…", &body[..byte_end])
    } else {
        body[..byte_end].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{truncate_body_snippet, ApiError};

    #[test]
    fn json_deserialize_error_includes_provider_model_and_truncated_body_snippet() {
        let raw_body = format!("{}{}", "x".repeat(190), "_TAIL_PAST_200_CHARS_MARKER_");
        let source = serde_json::from_str::<serde_json::Value>("{not json")
            .expect_err("invalid json should fail to parse");

        let error = ApiError::json_deserialize("Anthropic", "claude-opus-4-6", &raw_body, source);
        let rendered = error.to_string();

        assert!(
            rendered.starts_with("failed to parse Anthropic response for model claude-opus-4-6: "),
            "rendered error should lead with provider and model: {rendered}"
        );
        assert!(
            rendered.contains("first 200 chars of body: "),
            "rendered error should label the body snippet: {rendered}"
        );
        let snippet = rendered
            .split("first 200 chars of body: ")
            .nth(1)
            .expect("snippet section should be present");
        assert!(
            snippet.starts_with(&"x".repeat(190)),
            "snippet should preserve the leading characters of the body: {snippet}"
        );
        assert!(
            snippet.ends_with('…'),
            "snippet should signal truncation with an ellipsis: {snippet}"
        );
        assert!(
            !snippet.contains("_TAIL_PAST_200_CHARS_MARKER_"),
            "snippet should drop characters past the 200-char cap: {snippet}"
        );
        assert_eq!(error.safe_failure_class(), "runtime_io");
        assert_eq!(error.request_id(), None);
        assert!(!error.is_retryable());
    }

    #[test]
    fn truncate_body_snippet_keeps_short_bodies_intact() {
        assert_eq!(truncate_body_snippet("hello", 200), "hello");
        assert_eq!(truncate_body_snippet("", 200), "");
    }

    #[test]
    fn truncate_body_snippet_caps_long_bodies_at_max_chars() {
        let body = "a".repeat(250);
        let snippet = truncate_body_snippet(&body, 200);
        assert_eq!(snippet.chars().count(), 201, "200 chars + ellipsis");
        assert!(snippet.ends_with('…'));
        assert!(snippet.starts_with(&"a".repeat(200)));
    }

    #[test]
    fn truncate_body_snippet_does_not_split_multibyte_characters() {
        let body = "한글한글한글한글한글한글";
        let snippet = truncate_body_snippet(body, 4);
        assert_eq!(snippet, "한글한글…");
    }

    #[test]
    fn detects_generic_fatal_wrapper_and_classifies_it_as_provider_internal() {
        let error = ApiError::Api {
            status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            error_type: Some("api_error".to_string()),
            message: Some(
                "Something went wrong while processing your request. Please try again, or use /new to start a fresh session."
                    .to_string(),
            ),
            request_id: Some("req_jobdori_123".to_string()),
            body: String::new(),
            retryable: true,
            suggested_action: None,
        retry_after: None,
        };

        assert!(error.is_generic_fatal_wrapper());
        assert_eq!(error.safe_failure_class(), "provider_internal");
        assert_eq!(error.request_id(), Some("req_jobdori_123"));
        assert!(error.to_string().contains("[trace req_jobdori_123]"));
    }

    #[test]
    fn retries_exhausted_preserves_nested_request_id_and_failure_class() {
        let error = ApiError::RetriesExhausted {
            attempts: 3,
            last_error: Box::new(ApiError::Api {
                status: reqwest::StatusCode::BAD_GATEWAY,
                error_type: Some("api_error".to_string()),
                message: Some(
                    "Something went wrong while processing your request. Please try again, or use /new to start a fresh session."
                        .to_string(),
                ),
                request_id: Some("req_nested_456".to_string()),
                body: String::new(),
                retryable: true,
                suggested_action: None,
            retry_after: None,
            }),
        };

        assert!(error.is_generic_fatal_wrapper());
        assert_eq!(error.safe_failure_class(), "provider_retry_exhausted");
        assert_eq!(error.request_id(), Some("req_nested_456"));
    }

    #[test]
    fn classifies_provider_context_window_errors() {
        let error = ApiError::Api {
            status: reqwest::StatusCode::BAD_REQUEST,
            error_type: Some("invalid_request_error".to_string()),
            message: Some(
                "This model's maximum context length is 200000 tokens, but your request used 230000 tokens."
                    .to_string(),
            ),
            request_id: Some("req_ctx_123".to_string()),
            body: String::new(),
            retryable: false,
            suggested_action: None,
        retry_after: None,
        };

        assert!(error.is_context_window_failure());
        assert_eq!(error.safe_failure_class(), "context_window");
        assert_eq!(error.request_id(), Some("req_ctx_123"));
    }

    #[test]
    fn missing_credentials_without_hint_renders_the_canonical_message() {
        // given
        let error = ApiError::missing_credentials(
            "Anthropic",
            &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
        );

        // when
        let rendered = error.to_string();

        // then
        assert!(
            rendered.starts_with(
                "missing Anthropic credentials; export ANTHROPIC_AUTH_TOKEN or ANTHROPIC_API_KEY before calling the Anthropic API"
            ),
            "rendered error should lead with the canonical missing-credential message: {rendered}"
        );
        assert!(
            !rendered.contains(" — hint: "),
            "no hint should be appended when none is supplied: {rendered}"
        );
    }

    #[test]
    fn missing_credentials_with_hint_appends_the_hint_after_base_message() {
        // given
        let error = ApiError::missing_credentials_with_hint(
            "Anthropic",
            &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
            "I see OPENAI_API_KEY is set — if you meant to use the OpenAI-compat provider, prefix your model name with `openai/` so prefix routing selects it.",
        );

        // when
        let rendered = error.to_string();

        // then
        assert!(
            rendered.starts_with("missing Anthropic credentials;"),
            "hint should be appended, not replace the base message: {rendered}"
        );
        let hint_marker = " — hint: I see OPENAI_API_KEY is set — if you meant to use the OpenAI-compat provider, prefix your model name with `openai/` so prefix routing selects it.";
        assert!(
            rendered.ends_with(hint_marker),
            "rendered error should end with the hint: {rendered}"
        );
        // Classification semantics are unaffected by the presence of a hint.
        assert_eq!(error.safe_failure_class(), "provider_auth");
        assert!(!error.is_retryable());
        assert_eq!(error.request_id(), None);
    }
}
