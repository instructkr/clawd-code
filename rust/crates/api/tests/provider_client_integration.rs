use std::ffi::OsString;
use std::sync::{Mutex, OnceLock};

use api::{read_xai_base_url, ApiError, AuthSource, ProviderClient, ProviderKind};

#[test]
fn provider_client_routes_grok_aliases_through_xai() {
    let _lock = env_lock();
    let _xai_api_key = EnvVarGuard::set("XAI_API_KEY", Some("xai-test-key"));

    let client = ProviderClient::from_model("grok-mini").expect("grok alias should resolve");

    assert_eq!(client.provider_kind(), ProviderKind::Xai);
}

#[test]
fn provider_client_reports_missing_xai_credentials_for_grok_models() {
    let _lock = env_lock();
    let _xai_api_key = EnvVarGuard::set("XAI_API_KEY", None);

    let error = ProviderClient::from_model("grok-3")
        .expect_err("grok requests without XAI_API_KEY should fail fast");

    match error {
        ApiError::MissingCredentials {
            provider, env_vars, ..
        } => {
            assert_eq!(provider, "xAI");
            assert_eq!(env_vars, &["XAI_API_KEY"]);
        }
        other => panic!("expected missing xAI credentials, got {other:?}"),
    }
}

#[test]
fn provider_client_uses_explicit_anthropic_auth_without_env_lookup() {
    let _lock = env_lock();
    let _anthropic_api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", None);
    let _anthropic_auth_token = EnvVarGuard::set("ANTHROPIC_AUTH_TOKEN", None);

    let client = ProviderClient::from_model_with_anthropic_auth(
        "claude-sonnet-4-6",
        Some(AuthSource::ApiKey("anthropic-test-key".to_string())),
    )
    .expect("explicit anthropic auth should avoid env lookup");

    assert_eq!(client.provider_kind(), ProviderKind::Anthropic);
}

#[test]
fn read_xai_base_url_prefers_env_override() {
    let _lock = env_lock();
    let _xai_base_url = EnvVarGuard::set("XAI_BASE_URL", Some("https://example.xai.test/v1"));

    assert_eq!(read_xai_base_url(), "https://example.xai.test/v1");
}

#[test]
fn provider_client_routes_deepseek_aliases_through_deepseek() {
    let _lock = env_lock();
    let _ds_key = EnvVarGuard::set("DEEPSEEK_API_KEY", Some("sk-test"));

    let client =
        ProviderClient::from_model("deepseek-chat").expect("deepseek-chat alias should resolve");

    assert_eq!(client.provider_kind(), ProviderKind::DeepSeek);
}

#[test]
fn provider_client_reports_missing_deepseek_credentials() {
    let _lock = env_lock();
    let _ds_key = EnvVarGuard::set("DEEPSEEK_API_KEY", None);
    let _anthropic = EnvVarGuard::set("ANTHROPIC_API_KEY", None);
    let _openai = EnvVarGuard::set("OPENAI_API_KEY", None);
    let _xai = EnvVarGuard::set("XAI_API_KEY", None);

    let error = ProviderClient::from_model("deepseek-chat")
        .expect_err("deepseek requests without DEEPSEEK_API_KEY should fail fast");

    match error {
        ApiError::MissingCredentials {
            provider, env_vars, ..
        } => {
            assert_eq!(provider, "DeepSeek");
            assert_eq!(env_vars, &["DEEPSEEK_API_KEY"]);
        }
        other => panic!("expected missing DeepSeek credentials, got {other:?}"),
    }
}

#[test]
fn provider_client_routes_ollama_prefix_to_ollama() {
    let _lock = env_lock();
    // No auth needed for Ollama - just set the base URL
    let _ollama_url = EnvVarGuard::set("OLLAMA_BASE_URL", Some("http://localhost:11434/v1"));
    let _anthropic = EnvVarGuard::set("ANTHROPIC_API_KEY", None);
    let _openai = EnvVarGuard::set("OPENAI_API_KEY", None);

    let client = ProviderClient::from_model("ollama/llama3.1:8b")
        .expect("ollama/ prefix should resolve without credentials");

    assert_eq!(client.provider_kind(), ProviderKind::Ollama);
}

#[test]
fn provider_client_routes_vllm_prefix_to_vllm() {
    let _lock = env_lock();
    let _vllm_url = EnvVarGuard::set("VLLM_BASE_URL", Some("http://localhost:8000/v1"));
    let _anthropic = EnvVarGuard::set("ANTHROPIC_API_KEY", None);
    let _ollama = EnvVarGuard::set("OLLAMA_BASE_URL", None);

    let client = ProviderClient::from_model("vllm/meta-llama/Llama-3.1-8B")
        .expect("vllm/ prefix should resolve without credentials");

    assert_eq!(client.provider_kind(), ProviderKind::Vllm);
}

#[test]
fn provider_client_routes_qwen_external_with_qwen_prefix() {
    let _lock = env_lock();
    let _qwen_key = EnvVarGuard::set("QWEN_API_KEY", Some("sk-qwen-test"));
    let _anthropic = EnvVarGuard::set("ANTHROPIC_API_KEY", None);
    let _openai = EnvVarGuard::set("OPENAI_API_KEY", None);

    let client = ProviderClient::from_model("qwen/qwen2.5-7b")
        .expect("qwen/ external prefix should resolve with QWEN_API_KEY");

    assert_eq!(client.provider_kind(), ProviderKind::Qwen);
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
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
        match &self.original {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
