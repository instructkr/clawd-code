use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use api::models_file;

/// Global mutex to serialize e2e tests that manipulate the global models_file registry.
fn e2e_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn clear_and_setup() {
    models_file::clear_custom_models();
}

fn temp_dir() -> PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time should be after epoch")
        .as_millis();
    let pid = std::process::id();
    std::env::temp_dir().join(format!("models-e2e-{pid}-{millis}"))
}

fn write_models_file(dir: &Path, content: &str) -> PathBuf {
    let path = dir.join("models.json");
    fs::write(&path, content).expect("write models.json");
    path
}

#[test]
fn e2e_load_models_json_and_find_custom_model() {
    let _lock = e2e_lock();
    clear_and_setup();
    let dir = temp_dir();
    fs::create_dir_all(&dir).expect("create temp dir");

    let content = r#"{
        "providers": {
            "ollama": {
                "baseUrl": "http://localhost:11434/v1",
                "api": "openai-completions",
                "apiKey": "ollama",
                "models": [
                    { "id": "llama3.1:8b", "contextWindow": 128000, "maxTokens": 32768 },
                    { "id": "qwen2.5-coder:7b", "contextWindow": 32768, "maxTokens": 8192 }
                ]
            }
        }
    }"#;
    let path = write_models_file(&dir, content);

    let loaded = models_file::load_custom_models(&path)
        .expect("should load models file")
        .expect("should have content");
    assert_eq!(loaded.providers.len(), 1);

    let found =
        models_file::find_custom_model("ollama/llama3.1:8b").expect("should find by prefix");
    assert_eq!(found.model_id, "llama3.1:8b");
    assert_eq!(found.base_url, "http://localhost:11434/v1");
    assert_eq!(found.max_tokens, 32768);
    assert_eq!(found.context_window, 128000);

    let found2 =
        models_file::find_custom_model("qwen2.5-coder:7b").expect("should find by bare ID");
    assert_eq!(found2.provider_label, "ollama");

    let max_tokens = api::max_tokens_for_model("qwen2.5-coder:7b");
    assert_eq!(max_tokens, 8192);

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn e2e_custom_model_with_env_var_api_key() {
    let _lock = e2e_lock();
    clear_and_setup();
    let dir = temp_dir();
    fs::create_dir_all(&dir).expect("create temp dir");

    let content = r#"{
        "providers": {
            "local": {
                "baseUrl": "http://127.0.0.1:8080/v1",
                "api": "openai-completions",
                "apiKey": "LOCAL_LLM_KEY",
                "models": [
                    { "id": "my-model", "contextWindow": 4096 }
                ]
            }
        }
    }"#;
    let path = write_models_file(&dir, content);

    let loaded = models_file::load_custom_models(&path)
        .expect("should load")
        .expect("should have content");
    assert_eq!(loaded.providers.len(), 1);
    assert_eq!(loaded.providers["local"].models[0].context_window, 4096);

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn e2e_empty_models_file_returns_none() {
    let _lock = e2e_lock();
    clear_and_setup();
    let dir = temp_dir();
    fs::create_dir_all(&dir).expect("create temp dir");
    let path = write_models_file(&dir, "");
    let result = models_file::load_custom_models(&path).expect("should not error");
    assert!(result.is_none());
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn e2e_missing_models_file_returns_none() {
    let _lock = e2e_lock();
    clear_and_setup();
    let dir = temp_dir();
    fs::create_dir_all(&dir).expect("create temp dir");
    let missing = dir.join("nonexistent.json");
    let result = models_file::load_custom_models(&missing).expect("should not error");
    assert!(result.is_none());
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn e2e_invalid_models_file_returns_error() {
    let _lock = e2e_lock();
    clear_and_setup();
    let dir = temp_dir();
    fs::create_dir_all(&dir).expect("create temp dir");
    let path = write_models_file(&dir, "this is not valid json");
    let result = models_file::load_custom_models(&path);
    assert!(result.is_err(), "invalid JSON should error: {:?}", result);
    assert!(result.unwrap_err().contains("parse error"));
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn e2e_discover_and_load_from_standard_paths() {
    let _lock = e2e_lock();
    clear_and_setup();
    let dir = temp_dir();
    let config_home = dir.join(".claw");
    let cwd = dir.join("project");
    fs::create_dir_all(config_home.join("sessions")).expect("create config home");
    fs::create_dir_all(cwd.join(".claw")).expect("create project config");

    let project_models = cwd.join(".claw").join("models.json");
    fs::write(
        &project_models,
        r#"{"providers":{"local":{"baseUrl":"http://127.0.0.1:8080/v1","api":"openai-completions","apiKey":"test","models":[{"id":"test-model"}]}}}"#,
    )
    .expect("write project models.json");

    models_file::discover_and_load_models(&cwd, &config_home).expect("discover should succeed");

    let found = models_file::find_custom_model("test-model")
        .expect("should find test-model after discovery");
    assert_eq!(found.provider_label, "local");
    assert_eq!(found.base_url, "http://127.0.0.1:8080/v1");

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn e2e_discover_without_models_file_does_not_error() {
    let _lock = e2e_lock();
    clear_and_setup();
    let dir = temp_dir();
    let config_home = dir.join(".claw");
    let cwd = dir.join("project");
    fs::create_dir_all(&config_home).expect("create config home");
    fs::create_dir_all(&cwd).expect("create project dir");

    let result = models_file::discover_and_load_models(&cwd, &config_home);
    assert!(
        result.is_ok(),
        "discover without models file should not error: {:?}",
        result.err()
    );

    let all = models_file::all_custom_models();
    assert!(all.is_empty(), "no models should be registered");

    fs::remove_dir_all(&dir).expect("cleanup");
}
