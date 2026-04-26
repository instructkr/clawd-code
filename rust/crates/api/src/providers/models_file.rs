//! Runtime-loaded model configuration from `models.json`.
//!
//! Users can define custom providers and models in `~/.claw/models.json` or
//! `.claw/models.json` (project-local). The file is loaded lazily on first
//! access and can be refreshed at any time.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{OnceLock, RwLock};

use serde::Deserialize;

use super::{ProviderKind, ProviderMetadata};

// ---------------------------------------------------------------------------
// JSON file schema
// ---------------------------------------------------------------------------

/// Top-level structure of the models.json file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsFile {
    #[serde(default)]
    pub providers: BTreeMap<String, CustomProviderEntry>,
}

/// A user-defined provider configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomProviderEntry {
    /// API base URL (e.g. "http://localhost:11434/v1")
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    /// API protocol family: "openai-completions" or "anthropic-messages"
    pub api: String,
    /// API key (literal value or environment variable name)
    #[serde(rename = "apiKey")]
    pub api_key: String,
    /// Optional custom HTTP headers
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Models available through this provider
    #[serde(default)]
    pub models: Vec<CustomModelEntry>,
}

/// A user-defined model under a custom provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomModelEntry {
    /// Model identifier passed to the API
    pub id: String,
    /// Human-readable label (defaults to `id`)
    #[serde(default)]
    pub name: Option<String>,
    /// Supports extended thinking
    #[serde(default)]
    pub reasoning: bool,
    /// Input types: "text" or "text" + "image"
    #[serde(default = "default_input_types")]
    pub input: Vec<String>,
    /// Context window size in tokens (default 128000)
    #[serde(rename = "contextWindow", default = "default_context_window")]
    pub context_window: u32,
    /// Maximum output tokens (default 16384)
    #[serde(rename = "maxTokens", default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_input_types() -> Vec<String> {
    vec!["text".to_string()]
}

const fn default_context_window() -> u32 {
    128_000
}

const fn default_max_tokens() -> u32 {
    16_384
}

// ---------------------------------------------------------------------------
// Resolved (flattened) custom model entry used by the provider router
// ---------------------------------------------------------------------------

/// A fully resolved custom model entry merged from a provider + model pair.
#[derive(Debug, Clone)]
pub struct ResolvedCustomModel {
    /// The fully-qualified provider label (e.g. "ollama")
    pub provider_label: String,
    /// Model ID as sent to the API
    pub model_id: String,
    /// Human-readable name
    pub name: String,
    /// Base URL of the provider
    pub base_url: String,
    /// API type ("openai-completions" or "anthropic-messages")
    pub api: String,
    /// API key value (resolved from literal or env var)
    pub api_key: String,
    /// Optional custom headers
    pub headers: BTreeMap<String, String>,
    /// Supports extended thinking
    pub reasoning: bool,
    /// Supports image input
    pub supports_images: bool,
    /// Context window size in tokens
    pub context_window: u32,
    /// Maximum output tokens
    pub max_tokens: u32,
}

// ---------------------------------------------------------------------------
// Global lazy-loaded registry
// ---------------------------------------------------------------------------

static CUSTOM_MODELS: OnceLock<RwLock<Option<ModelsFile>>> = OnceLock::new();

fn custom_models() -> &'static RwLock<Option<ModelsFile>> {
    CUSTOM_MODELS.get_or_init(|| RwLock::new(None))
}

/// Load custom models from a `models.json` file and replace the global registry.
///
/// Valid values for the `api` field in a custom provider entry.
const VALID_API_VALUES: &[&str] = &[
    "openai-completions",
    "anthropic-messages",
    "deepseek",
    "ollama",
    "qwen",
    "vllm",
];

/// Validate that every provider in the models file uses a recognized `api` value.
fn validate_providers(file: &ModelsFile, path: &Path) -> Result<(), String> {
    for (provider_label, provider) in &file.providers {
        if !VALID_API_VALUES.contains(&provider.api.as_str()) {
            return Err(format!(
                "invalid api value '{}' for provider '{}' in {}: must be one of {}",
                provider.api,
                provider_label,
                path.display(),
                VALID_API_VALUES.join(", ")
            ));
        }
    }
    Ok(())
}

/// The file is parsed and stored in a global `RwLock`. Returns `None` when the
/// file does not exist, or an error description on parse failure.
pub fn load_custom_models(path: &Path) -> Result<Option<ModelsFile>, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };

    if content.trim().is_empty() {
        return Ok(None);
    }

    let parsed: ModelsFile = serde_json::from_str(&content)
        .map_err(|e| format!("parse error in {}: {e}", path.display()))?;

    validate_providers(&parsed, path)?;

    let mut registry = custom_models().write().map_err(|e| e.to_string())?;
    *registry = Some(parsed.clone());
    Ok(Some(parsed))
}

/// Load custom models and merge into the global registry (project-level
/// provider entries override user-level entries with the same key, but
/// user-level entries with different keys are preserved).
fn load_and_merge_custom_models(path: &Path) -> Result<Option<ModelsFile>, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };

    if content.trim().is_empty() {
        return Ok(None);
    }

    let parsed: ModelsFile = serde_json::from_str(&content)
        .map_err(|e| format!("parse error in {}: {e}", path.display()))?;

    validate_providers(&parsed, path)?;

    let mut registry = custom_models().write().map_err(|e| e.to_string())?;
    match registry.as_mut() {
        Some(existing) => {
            // Merge: project-level entries override user-level with same key
            for (key, value) in parsed.providers {
                existing.providers.insert(key, value);
            }
        }
        None => {
            *registry = Some(parsed.clone());
        }
    }
    Ok(registry.clone())
}

/// Discover and load models.json from standard config paths.
///
/// Checks `~/.claw/models.json` and `.claw/models.json` (project-local).
/// Project-level provider entries override user-level entries with the same
/// key, but user-level entries with different keys are preserved.
pub fn discover_and_load_models(cwd: &Path, config_home: &Path) -> Result<(), String> {
    // User-level: ~/.claw/models.json
    let user_path = config_home.join("models.json");
    let _user_loaded = load_custom_models(&user_path)?;

    // Project-level: .claw/models.json (merges into user-level; same-key entries override)
    let project_path = cwd.join(".claw").join("models.json");
    let _project_loaded = load_and_merge_custom_models(&project_path)?;

    Ok(())
}

/// Return all resolved custom models from the global registry.
pub fn all_custom_models() -> Vec<ResolvedCustomModel> {
    let guard = match custom_models().read() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    let Some(file) = guard.as_ref() else {
        return Vec::new();
    };

    let mut results = Vec::new();
    for (provider_label, provider) in &file.providers {
        // Resolve API key: if it matches an env var name, read it; otherwise use literal
        let api_key = resolve_api_key(&provider.api_key);

        for model in &provider.models {
            let name = model.name.clone().unwrap_or_else(|| model.id.clone());
            let supports_images = model.input.iter().any(|t| t == "image");
            results.push(ResolvedCustomModel {
                provider_label: provider_label.clone(),
                model_id: model.id.clone(),
                name,
                base_url: provider.base_url.clone(),
                api: provider.api.clone(),
                api_key: api_key.clone(),
                headers: provider.headers.clone(),
                reasoning: model.reasoning,
                supports_images,
                context_window: model.context_window,
                max_tokens: model.max_tokens,
            });
        }
    }
    results
}

/// Check whether any custom model in the registry matches the given model ID.
/// Returns the resolved entry if found.
///
/// When called with a bare model ID (no `provider/` prefix), the first match
/// by provider iteration order is returned. To disambiguate models with the
/// same name across providers, use the `provider/` prefix form (e.g.
/// `"ollama/llama3.1:8b"`). Provider-prefixed lookups take priority and are
/// checked before bare ID lookups.
pub fn find_custom_model(model: &str) -> Option<ResolvedCustomModel> {
    let guard = custom_models().read().ok()?;
    let file = guard.as_ref()?;

    // First pass: prefer provider-prefixed matches (unambiguous)
    for (provider_label, provider) in &file.providers {
        if let Some(model_part) = model.strip_prefix(&format!("{provider_label}/")) {
            if let Some(entry) = provider.models.iter().find(|m| m.id == model_part) {
                return Some(build_resolved(provider_label, provider, entry));
            }
        }
    }

    // Second pass: bare model ID — returns the first match by provider order.
    // Project-level providers override user-level in the loading order.
    for (provider_label, provider) in &file.providers {
        if let Some(entry) = provider.models.iter().find(|m| m.id == model) {
            return Some(build_resolved(provider_label, provider, entry));
        }
    }
    None
}

/// Return a `ProviderMetadata` for a custom model, if one matches.
/// This lets the existing `metadata_for_model()` and `detect_provider_kind()`
/// functions seamlessly route through custom providers.
pub fn custom_metadata_for_model(model: &str) -> Option<ProviderMetadata> {
    let resolved = find_custom_model(model)?;
    let (provider_kind, auth_env) = match resolved.api.as_str() {
        "anthropic-messages" => (ProviderKind::Anthropic, "ANTHROPIC_API_KEY"),
        "openai-completions" => (ProviderKind::OpenAi, "OPENAI_API_KEY"),
        "deepseek" => (ProviderKind::DeepSeek, "DEEPSEEK_API_KEY"),
        "ollama" => (ProviderKind::Ollama, ""),
        "qwen" => (ProviderKind::Qwen, "QWEN_API_KEY"),
        "vllm" => (ProviderKind::Vllm, ""),
        _ => (ProviderKind::OpenAi, "OPENAI_API_KEY"),
    };
    Some(ProviderMetadata {
        provider: provider_kind,
        auth_env,
        base_url_env: "", // Custom providers always use the configured base_url directly
        default_base_url: "", // Not used for custom providers -- we pass the configured URL
    })
}

/// Get the max tokens override for a custom model, if registered.
pub fn custom_max_tokens(model: &str) -> Option<u32> {
    find_custom_model(model).map(|m| m.max_tokens)
}

/// Get the context window size for a custom model, if registered.
pub fn custom_context_window(model: &str) -> Option<u32> {
    find_custom_model(model).map(|m| m.context_window)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_resolved(
    provider_label: &str,
    provider: &CustomProviderEntry,
    model: &CustomModelEntry,
) -> ResolvedCustomModel {
    let api_key = resolve_api_key(&provider.api_key);
    let supports_images = model.input.iter().any(|t| t == "image");
    ResolvedCustomModel {
        provider_label: provider_label.to_string(),
        model_id: model.id.clone(),
        name: model.name.clone().unwrap_or_else(|| model.id.clone()),
        base_url: provider.base_url.clone(),
        api: provider.api.clone(),
        api_key,
        headers: provider.headers.clone(),
        reasoning: model.reasoning,
        supports_images,
        context_window: model.context_window,
        max_tokens: model.max_tokens,
    }
}

/// Resolve an API key string: if it matches a set env var, use env value;
/// otherwise use the literal string.
fn resolve_api_key(key: &str) -> String {
    // If it's an environment variable name (uppercase with underscores), try to read it
    if key
        .chars()
        .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
    {
        std::env::var(key).unwrap_or_else(|_| key.to_string())
    } else {
        key.to_string()
    }
}

/// Clear the global custom models registry. Useful for test isolation.
pub fn clear_custom_models() {
    if let Ok(mut guard) = custom_models().write() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn models_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn parses_basic_models_file() {
        let json = r#"{
            "providers": {
                "ollama": {
                    "baseUrl": "http://localhost:11434/v1",
                    "api": "openai-completions",
                    "apiKey": "ollama",
                    "models": [
                        { "id": "llama3.1:8b" },
                        { "id": "qwen2.5-coder:7b", "contextWindow": 32768, "maxTokens": 8192 }
                    ]
                }
            }
        }"#;
        let parsed: ModelsFile = serde_json::from_str(json).expect("should parse");
        let ollama = parsed.providers.get("ollama").expect("ollama provider");
        assert_eq!(ollama.base_url, "http://localhost:11434/v1");
        assert_eq!(ollama.api, "openai-completions");
        assert_eq!(ollama.api_key, "ollama");
        assert_eq!(ollama.models.len(), 2);
        assert_eq!(ollama.models[0].id, "llama3.1:8b");
        assert_eq!(ollama.models[0].context_window, 128_000); // default
        assert_eq!(ollama.models[0].max_tokens, 16_384); // default
        assert_eq!(ollama.models[1].id, "qwen2.5-coder:7b");
        assert_eq!(ollama.models[1].context_window, 32_768);
        assert_eq!(ollama.models[1].max_tokens, 8_192);
    }

    #[test]
    fn finds_custom_model_by_provider_prefix() {
        let _lock = models_lock();
        clear_custom_models();

        // Manually set custom models
        let file = ModelsFile {
            providers: [(
                "ollama".to_string(),
                CustomProviderEntry {
                    base_url: "http://localhost:11434/v1".to_string(),
                    api: "openai-completions".to_string(),
                    api_key: "ollama".to_string(),
                    headers: BTreeMap::new(),
                    models: vec![CustomModelEntry {
                        id: "llama3.1:8b".to_string(),
                        name: None,
                        reasoning: false,
                        input: vec!["text".to_string()],
                        context_window: 128_000,
                        max_tokens: 16_384,
                    }],
                },
            )]
            .into_iter()
            .collect(),
        };
        if let Ok(mut guard) = custom_models().write() {
            *guard = Some(file);
        }

        // Find by provider prefix
        let found =
            find_custom_model("ollama/llama3.1:8b").expect("should find by provider prefix");
        assert_eq!(found.model_id, "llama3.1:8b");
        assert_eq!(found.provider_label, "ollama");
        assert_eq!(found.base_url, "http://localhost:11434/v1");

        // Find by bare model ID
        let found2 = find_custom_model("llama3.1:8b").expect("should find by bare ID");
        assert_eq!(found2.model_id, "llama3.1:8b");

        clear_custom_models();
    }

    #[test]
    fn custom_model_not_found_returns_none() {
        let _lock = models_lock();
        clear_custom_models();

        // No models loaded
        assert!(find_custom_model("nonexistent").is_none());
        assert!(custom_max_tokens("nonexistent").is_none());
        assert!(custom_context_window("nonexistent").is_none());
        assert!(custom_metadata_for_model("nonexistent").is_none());
    }

    #[test]
    fn load_from_missing_file_returns_none() {
        let missing = Path::new("/tmp/__nonexistent_models_file__");
        let result = load_custom_models(missing).expect("missing file should not error");
        assert!(result.is_none());
    }

    #[test]
    fn discover_merges_user_and_project_providers() {
        let _lock = models_lock();
        clear_custom_models();

        let dir = std::env::temp_dir().join(format!(
            "models-merge-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        let config_home = dir.join("home").join(".claw");
        let cwd = dir.join("project");
        std::fs::create_dir_all(&config_home).expect("create config home");
        std::fs::create_dir_all(cwd.join(".claw")).expect("create project .claw");

        // User-level: defines "ollama" provider
        std::fs::write(
            config_home.join("models.json"),
            r#"{"providers":{"ollama":{"baseUrl":"http://localhost:11434/v1","api":"openai-completions","apiKey":"ollama","models":[{"id":"llama3.1:8b"}]}}}"#,
        )
        .expect("write user models.json");

        // Project-level: defines "local" provider (different key)
        std::fs::write(
            cwd.join(".claw").join("models.json"),
            r#"{"providers":{"local":{"baseUrl":"http://127.0.0.1:8080/v1","api":"openai-completions","apiKey":"local","models":[{"id":"my-model"}]}}}"#,
        )
        .expect("write project models.json");

        discover_and_load_models(&cwd, &config_home).expect("discover should succeed");

        // Both providers should be present after merge
        let ollama = find_custom_model("ollama/llama3.1:8b");
        assert!(
            ollama.is_some(),
            "user-level ollama provider should survive after project merge"
        );
        assert_eq!(ollama.unwrap().model_id, "llama3.1:8b");

        let local = find_custom_model("local/my-model");
        assert!(
            local.is_some(),
            "project-level local provider should be present"
        );
        assert_eq!(local.unwrap().model_id, "my-model");

        let all = all_custom_models();
        assert_eq!(
            all.len(),
            2,
            "should have exactly 2 models from 2 providers"
        );

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn project_provider_overrides_same_key_user_provider() {
        let _lock = models_lock();
        clear_custom_models();

        let dir = std::env::temp_dir().join(format!(
            "models-override-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        let config_home = dir.join("home").join(".claw");
        let cwd = dir.join("project");
        std::fs::create_dir_all(&config_home).expect("create config home");
        std::fs::create_dir_all(cwd.join(".claw")).expect("create project .claw");

        // User-level: defines "ollama" with llama3.1:8b
        std::fs::write(
            config_home.join("models.json"),
            r#"{"providers":{"ollama":{"baseUrl":"http://localhost:11434/v1","api":"openai-completions","apiKey":"ollama","models":[{"id":"llama3.1:8b"}]}}}"#,
        )
        .expect("write user models.json");

        // Project-level: same key "ollama" but different base URL (override)
        std::fs::write(
            cwd.join(".claw").join("models.json"),
            r#"{"providers":{"ollama":{"baseUrl":"http://custom-ollama:11434/v1","api":"openai-completions","apiKey":"project-key","models":[{"id":"qwen2.5:7b"}]}}}"#,
        )
        .expect("write project models.json");

        discover_and_load_models(&cwd, &config_home).expect("discover should succeed");

        // Project-level "ollama" should override user-level
        let found = find_custom_model("ollama/qwen2.5:7b");
        assert!(found.is_some(), "project ollama model should be present");
        assert_eq!(found.unwrap().base_url, "http://custom-ollama:11434/v1");

        // User-level llama3.1:8b should NOT be present (provider was replaced)
        let old = find_custom_model("ollama/llama3.1:8b");
        assert!(
            old.is_none(),
            "user-level ollama models should be replaced by project override"
        );

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn validates_api_field_rejects_unknown_value() {
        let _lock = models_lock();
        clear_custom_models();

        let dir = std::env::temp_dir().join(format!(
            "models-validate-api-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let models_path = dir.join("models.json");
        std::fs::write(
            &models_path,
            r#"{"providers":{"bad":{"baseUrl":"http://localhost:11434/v1","api":"invalid-api-value","apiKey":"k","models":[{"id":"m"}]}}}"#,
        )
        .expect("write models.json");

        let result = super::load_custom_models(&models_path);
        match result {
            Err(msg) => {
                assert!(
                    msg.contains("invalid api value"),
                    "error should mention invalid api value, got: {msg}"
                );
                assert!(
                    msg.contains("invalid-api-value"),
                    "error should include the bad value, got: {msg}"
                );
                assert!(
                    msg.contains("bad"),
                    "error should name the provider, got: {msg}"
                );
            }
            other => panic!("expected validation error, got {other:?}"),
        }

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn validates_api_field_accepts_all_valid_values() {
        let _lock = models_lock();
        clear_custom_models();

        for valid_api in &[
            "openai-completions",
            "anthropic-messages",
            "deepseek",
            "ollama",
            "qwen",
            "vllm",
        ] {
            let dir = std::env::temp_dir().join(format!(
                "models-validate-ok-{valid_api}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            let models_path = dir.join("models.json");
            let json = format!(
                r#"{{"providers":{{"p":{{"baseUrl":"http://localhost:11434/v1","api":"{valid_api}","apiKey":"k","models":[{{"id":"m"}}]}}}}}}"#
            );
            std::fs::write(&models_path, &json).expect("write models.json");

            let result = super::load_custom_models(&models_path);
            assert!(
                result.is_ok(),
                "api='{valid_api}' should be accepted, got: {result:?}"
            );

            std::fs::remove_dir_all(&dir).expect("cleanup");
        }
    }
}
