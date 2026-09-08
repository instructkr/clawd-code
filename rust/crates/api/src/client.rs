use crate::error::ApiError;
use crate::prompt_cache::{PromptCache, PromptCacheRecord, PromptCacheStats};
use crate::providers::anthropic::{self, AnthropicClient, AuthSource};
use crate::providers::openai_compat::{self, OpenAiCompatClient, OpenAiCompatConfig};
use crate::providers::{self, ProviderKind};
use crate::types::{MessageRequest, MessageResponse, StreamEvent};

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ProviderClient {
    Anthropic(AnthropicClient),
    Xai(OpenAiCompatClient),
    OpenAi(OpenAiCompatClient),
}

/// Provider selected explicitly by persisted/runtime configuration.
///
/// This is intentionally separate from `ProviderKind`: DashScope speaks the
/// OpenAI-compatible wire protocol but remains a distinct configured provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderConfigKind {
    Anthropic,
    Xai,
    OpenAi,
    DashScope,
}

/// Explicit provider configuration supplied by the runtime layer.
///
/// This keeps the API crate independent of runtime configuration types.
/// Credentials are redacted from Debug output.
#[derive(Clone)]
pub struct ProviderConfig {
    pub kind: ProviderConfigKind,
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("kind", &self.kind)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url)
            .finish()
    }
}

fn read_env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

impl ProviderClient {
    /// Construct a client from an explicit provider configuration.
    ///
    /// The persisted provider kind is authoritative for provider selection,
    /// while environment credentials and base URLs retain precedence over
    /// persisted values.
    pub fn from_config(config: &ProviderConfig) -> Result<Self, ApiError> {
        match config.kind {
            ProviderConfigKind::Anthropic => {
                let api_key = read_env_non_empty("ANTHROPIC_API_KEY")
                    .or_else(|| config.api_key.clone())
                    .ok_or_else(|| {
                        ApiError::missing_credentials(
                            "Anthropic",
                            &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
                        )
                    })?;

                let mut client = AnthropicClient::new(api_key);
                let base_url = read_env_non_empty("ANTHROPIC_BASE_URL")
                    .or_else(|| config.base_url.clone())
                    .unwrap_or_else(anthropic::read_base_url);
                client = client.with_base_url(base_url);
                Ok(Self::Anthropic(client))
            }
            ProviderConfigKind::Xai => {
                let compat = OpenAiCompatConfig::xai();
                let api_key =
                    read_env_non_empty(compat.api_key_env).or_else(|| config.api_key.clone());

                let mut client = match api_key {
                    Some(api_key) => OpenAiCompatClient::new(api_key, compat),
                    None => OpenAiCompatClient::from_env(compat)?,
                };

                let base_url = read_env_non_empty(compat.base_url_env)
                    .or_else(|| config.base_url.clone())
                    .unwrap_or_else(|| compat.default_base_url.to_string());
                client = client.with_base_url(base_url);

                Ok(Self::Xai(client))
            }
            ProviderConfigKind::OpenAi | ProviderConfigKind::DashScope => {
                let compat = match config.kind {
                    ProviderConfigKind::OpenAi => OpenAiCompatConfig::openai(),
                    ProviderConfigKind::DashScope => OpenAiCompatConfig::dashscope(),
                    _ => unreachable!("non-OpenAI provider reached compatibility path"),
                };

                if config.kind == ProviderConfigKind::OpenAi
                    && read_env_non_empty("OLLAMA_HOST").is_some()
                    && config.api_key.is_none()
                    && config.base_url.is_none()
                {
                    return Ok(Self::OpenAi(
                        openai_compat::OpenAiCompatClient::from_ollama_env()
                            .expect("from_ollama_env always returns Some"),
                    ));
                }

                let api_key =
                    read_env_non_empty(compat.api_key_env).or_else(|| config.api_key.clone());
                let persisted_base_url = config.base_url.clone();
                let mut client = match api_key {
                    Some(api_key) => OpenAiCompatClient::new(api_key, compat),
                    None if config.kind == ProviderConfigKind::OpenAi
                        && persisted_base_url
                            .as_deref()
                            .is_some_and(openai_compat::is_local_openai_compatible_base_url) =>
                    {
                        OpenAiCompatClient::new("local-dev-token", compat)
                    }
                    None => OpenAiCompatClient::from_env(compat)?,
                };
                let base_url = read_env_non_empty(compat.base_url_env)
                    .or(persisted_base_url)
                    .unwrap_or_else(|| compat.default_base_url.to_string());
                client = client.with_base_url(base_url);

                Ok(Self::OpenAi(client))
            }
        }
    }
    pub fn from_model(model: &str) -> Result<Self, ApiError> {
        Self::from_model_with_anthropic_auth(model, None)
    }

    pub fn from_model_with_anthropic_auth(
        model: &str,
        anthropic_auth: Option<AuthSource>,
    ) -> Result<Self, ApiError> {
        let resolved_model = providers::resolve_model_alias(model);
        match providers::detect_provider_kind(&resolved_model) {
            ProviderKind::Anthropic => Ok(Self::Anthropic(match anthropic_auth {
                Some(auth) => AnthropicClient::from_auth(auth),
                None => AnthropicClient::from_env()?,
            })),
            ProviderKind::Xai => Ok(Self::Xai(OpenAiCompatClient::from_env(
                OpenAiCompatConfig::xai(),
            )?)),
            ProviderKind::OpenAi => {
                // OLLAMA_HOST takes priority: local Ollama needs no API key
                // and ignores DashScope/OpenAI env-based dispatch.
                if read_env_non_empty("OLLAMA_HOST").is_some() {
                    Ok(Self::OpenAi(
                        openai_compat::OpenAiCompatClient::from_ollama_env()
                            .expect("from_ollama_env always returns Some"),
                    ))
                } else {
                    // DashScope models (qwen-*) also return ProviderKind::OpenAi because they
                    // speak the OpenAI wire format, but they need the DashScope config which
                    // reads DASHSCOPE_API_KEY and points at dashscope.aliyuncs.com.
                    let config = match providers::metadata_for_model(&resolved_model) {
                        Some(meta) if meta.auth_env == "DASHSCOPE_API_KEY" => {
                            OpenAiCompatConfig::dashscope()
                        }
                        _ => OpenAiCompatConfig::openai(),
                    };
                    Ok(Self::OpenAi(OpenAiCompatClient::from_env(config)?))
                }
            }
        }
    }

    #[must_use]
    pub const fn provider_kind(&self) -> ProviderKind {
        match self {
            Self::Anthropic(_) => ProviderKind::Anthropic,
            Self::Xai(_) => ProviderKind::Xai,
            Self::OpenAi(_) => ProviderKind::OpenAi,
        }
    }

    #[must_use]
    pub fn with_prompt_cache(self, prompt_cache: PromptCache) -> Self {
        match self {
            Self::Anthropic(client) => Self::Anthropic(client.with_prompt_cache(prompt_cache)),
            other => other,
        }
    }

    #[must_use]
    pub fn prompt_cache_stats(&self) -> Option<PromptCacheStats> {
        match self {
            Self::Anthropic(client) => client.prompt_cache_stats(),
            Self::Xai(_) | Self::OpenAi(_) => None,
        }
    }

    #[must_use]
    pub fn take_last_prompt_cache_record(&self) -> Option<PromptCacheRecord> {
        match self {
            Self::Anthropic(client) => client.take_last_prompt_cache_record(),
            Self::Xai(_) | Self::OpenAi(_) => None,
        }
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        match self {
            Self::Anthropic(client) => client.send_message(request).await,
            Self::Xai(client) | Self::OpenAi(client) => client.send_message(request).await,
        }
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        match self {
            Self::Anthropic(client) => client
                .stream_message(request)
                .await
                .map(MessageStream::Anthropic),
            Self::Xai(client) | Self::OpenAi(client) => client
                .stream_message(request)
                .await
                .map(MessageStream::OpenAiCompat),
        }
    }
}

#[derive(Debug)]
pub enum MessageStream {
    Anthropic(anthropic::MessageStream),
    OpenAiCompat(openai_compat::MessageStream),
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Anthropic(stream) => stream.request_id(),
            Self::OpenAiCompat(stream) => stream.request_id(),
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        match self {
            Self::Anthropic(stream) => stream.next_event().await,
            Self::OpenAiCompat(stream) => stream.next_event().await,
        }
    }
}

pub use anthropic::{
    oauth_token_is_expired, resolve_saved_oauth_token, resolve_startup_auth_source, OAuthTokenSet,
};
#[must_use]
pub fn read_base_url() -> String {
    anthropic::read_base_url()
}

#[must_use]
pub fn read_xai_base_url() -> String {
    openai_compat::read_base_url(OpenAiCompatConfig::xai())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::{ProviderClient, ProviderConfig, ProviderConfigKind};
    use crate::providers::{detect_provider_kind, resolve_model_alias, ProviderKind};

    /// Serializes every test in this module that mutates process-wide
    /// environment variables so concurrent test threads cannot observe
    /// each other's partially-applied state.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn resolves_existing_and_grok_aliases() {
        assert_eq!(resolve_model_alias("opus"), "claude-opus-4-7");
        assert_eq!(resolve_model_alias("grok"), "grok-3");
        assert_eq!(resolve_model_alias("grok-mini"), "grok-3-mini");
    }

    #[test]
    fn provider_detection_prefers_model_family() {
        assert_eq!(detect_provider_kind("grok-3"), ProviderKind::Xai);
        assert_eq!(
            detect_provider_kind("claude-sonnet-4-6"),
            ProviderKind::Anthropic
        );
    }

    /// Snapshot-restore guard for a single environment variable. Mirrors
    /// the pattern used in `providers/mod.rs` tests: captures the original
    /// value on construction, applies the override, and restores on drop so
    /// tests leave the process env untouched even when they panic.
    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn persisted_provider_kind_overrides_model_provider_detection() {
        let _lock = env_lock();
        let _anthropic = EnvVarGuard::set("ANTHROPIC_API_KEY", Some("test-anthropic-key"));
        let _xai = EnvVarGuard::set("XAI_API_KEY", Some("test-xai-key"));

        let config = ProviderConfig {
            kind: ProviderConfigKind::Xai,
            model: "claude-sonnet-4-6".to_string(),
            api_key: None,
            base_url: None,
        };

        match ProviderClient::from_config(&config).expect("explicit xAI config should succeed") {
            ProviderClient::Xai(client) => {
                assert!(client.base_url().contains("api.x.ai"));
            }
            other => panic!("Expected explicit xAI provider, got: {other:?}"),
        }
    }

    #[test]
    fn persisted_dashscope_kind_overrides_qwen_model_routing() {
        let _lock = env_lock();
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", Some("test-dashscope-key"));
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", Some("test-openai-key"));

        let config = ProviderConfig {
            kind: ProviderConfigKind::DashScope,
            model: "gpt-5".to_string(),
            api_key: None,
            base_url: None,
        };

        match ProviderClient::from_config(&config)
            .expect("explicit DashScope config should succeed")
        {
            ProviderClient::OpenAi(client) => {
                assert!(client.base_url().contains("dashscope.aliyuncs.com"));
            }
            other => panic!("Expected explicit DashScope provider, got: {other:?}"),
        }
    }

    #[test]
    fn persisted_openai_kind_does_not_become_dashscope_from_model() {
        let _lock = env_lock();
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", Some("test-openai-key"));
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", Some("test-dashscope-key"));

        let config = ProviderConfig {
            kind: ProviderConfigKind::OpenAi,
            model: "qwen-plus".to_string(),
            api_key: None,
            base_url: None,
        };

        match ProviderClient::from_config(&config).expect("explicit OpenAI config should succeed") {
            ProviderClient::OpenAi(client) => {
                assert!(client.base_url().contains("api.openai.com"));
                assert!(!client.base_url().contains("dashscope.aliyuncs.com"));
            }
            other => panic!("Expected explicit OpenAI provider, got: {other:?}"),
        }
    }

    #[test]
    fn dashscope_model_uses_dashscope_config_not_openai() {
        // Regression: qwen-plus was being routed to OpenAiCompatConfig::openai()
        // which reads OPENAI_API_KEY and points at api.openai.com, when it should
        // use OpenAiCompatConfig::dashscope() which reads DASHSCOPE_API_KEY and
        // points at dashscope.aliyuncs.com.
        let _lock = env_lock();
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", Some("test-dashscope-key"));
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", None);

        let client = ProviderClient::from_model("qwen-plus");

        // Must succeed (not fail with "missing OPENAI_API_KEY")
        assert!(
            client.is_ok(),
            "qwen-plus with DASHSCOPE_API_KEY set should build successfully, got: {:?}",
            client.err()
        );

        // Verify it's the OpenAi variant pointed at the DashScope base URL.
        match client.unwrap() {
            ProviderClient::OpenAi(openai_client) => {
                assert!(
                    openai_client.base_url().contains("dashscope.aliyuncs.com"),
                    "qwen-plus should route to DashScope base URL (contains 'dashscope.aliyuncs.com'), got: {}",
                    openai_client.base_url()
                );
            }
            other => panic!("Expected ProviderClient::OpenAi for qwen-plus, got: {other:?}"),
        }
    }

    #[test]
    fn local_openai_base_url_routes_authless_ollama_models() {
        let _lock = env_lock();
        let _base_url = EnvVarGuard::set("OPENAI_BASE_URL", Some("http://127.0.0.1:11434/v1"));
        let _openai_key = EnvVarGuard::set("OPENAI_API_KEY", None);
        let _anthropic_key = EnvVarGuard::set("ANTHROPIC_API_KEY", Some("test-anthropic-key"));
        let _anthropic_token = EnvVarGuard::set("ANTHROPIC_AUTH_TOKEN", None);

        let client = ProviderClient::from_model("qwen2.5-coder:7b")
            .expect("local model should route to OpenAI-compatible client without auth");
        match client {
            ProviderClient::OpenAi(openai_client) => {
                assert_eq!(openai_client.base_url(), "http://127.0.0.1:11434/v1")
            }
            other => panic!("Expected ProviderClient::OpenAi for local model, got: {other:?}"),
        }
    }
}
