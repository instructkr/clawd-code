use std::io::{self, IsTerminal, Write};

use runtime::{save_user_provider_settings, ConfigLoader, RuntimeProviderConfig};

const PROVIDERS: &[(&str, &str, &str)] = &[
    ("1", "Anthropic", "anthropic"),
    ("2", "xAI / Grok", "xai"),
    ("3", "OpenAI", "openai"),
    ("4", "DashScope (Qwen/Kimi)", "dashscope"),
    ("5", "Custom (OpenAI-compat)", "openai"),
];

const PROVIDER_MODELS: &[(&str, &[&str])] = &[
    ("anthropic", &["opus", "sonnet", "haiku"]),
    ("xai", &["grok", "grok-mini", "grok-2"]),
    ("openai", &["gpt-4.1", "gpt-4.1-mini", "gpt-4.1-nano"]),
    ("dashscope", &["qwen-plus", "qwen-max", "kimi"]),
];

const DEFAULT_BASE_URLS: &[(&str, &str)] = &[
    ("anthropic", "https://api.anthropic.com"),
    ("xai", "https://api.x.ai/v1"),
    ("openai", "https://api.openai.com/v1"),
    ("dashscope", "https://dashscope.aliyuncs.com/compatible-mode/v1"),
];

const API_KEY_ENV_VARS: &[(&str, &str)] = &[
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("xai", "XAI_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("dashscope", "DASHSCOPE_API_KEY"),
];

pub fn run_setup_wizard() -> Result<(), Box<dyn std::error::Error>> {
    if !io::stdin().is_terminal() {
        return Err("setup wizard requires an interactive terminal".into());
    }

    let current = load_current_provider_config();

    println!();
    println!("  \x1b[1mClaw Code Setup Wizard\x1b[0m");
    println!("  Configure your provider, API key, and model.");
    println!("  Press Enter to keep current value.\n");

    let kind = prompt_provider(&current)?;
    let api_key = prompt_api_key(&kind, &current)?;
    let base_url = prompt_base_url(&kind, &current)?;
    let model = prompt_model(&kind, &current)?;

    save_user_provider_settings(
        &kind,
        &api_key,
        base_url.as_deref(),
        model.as_deref(),
    )?;

    println!();
    println!("  \x1b[32mProvider saved to ~/.claw/settings.json\x1b[0m");
    println!("  Run \x1b[1m/model {}\x1b[0m or restart claw to activate.", model.as_deref().unwrap_or(&kind));
    println!();

    Ok(())
}

fn load_current_provider_config() -> RuntimeProviderConfig {
    let cwd = std::env::current_dir().unwrap_or_default();
    ConfigLoader::default_for(&cwd)
        .load()
        .map(|c| c.provider().clone())
        .unwrap_or_default()
}

fn prompt_provider(current: &RuntimeProviderConfig) -> Result<String, Box<dyn std::error::Error>> {
    let current_kind = current.kind().unwrap_or("anthropic");
    println!("  \x1b[1mProvider\x1b[0m");
    for (num, label, kind) in PROVIDERS {
        let marker = if *kind == current_kind { " (current)" } else { "" };
        println!("    [{num}] {label}{marker}");
    }
    let default = PROVIDERS
        .iter()
        .position(|(_, _, k)| *k == current_kind)
        .map_or_else(|| "1".to_string(), |i| (i + 1).to_string());

    let input = read_line(&format!("  Select provider [{default}]: "))?;
    let choice = if input.trim().is_empty() {
        default
    } else {
        input.trim().to_string()
    };

    let kind = PROVIDERS
        .iter()
        .find(|(num, _, _)| *num == choice)
        .map(|(_, _, kind)| *kind)
        .ok_or_else(|| format!("invalid provider choice: {choice}"))?;

    Ok(kind.to_string())
}

fn prompt_api_key(
    kind: &str,
    current: &RuntimeProviderConfig,
) -> Result<String, Box<dyn std::error::Error>> {
    let env_var = API_KEY_ENV_VARS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or("API_KEY", |(_, v)| *v);

    let current_key = current.api_key();
    let hint = match current_key {
        Some(key) if !key.is_empty() => {
            let masked = if key.len() > 4 {
                format!("****{}", &key[key.len() - 4..])
            } else {
                "****".to_string()
            };
            format!("[{masked}]")
        }
        _ => "(none)".to_string(),
    };

    // Check if env var is already set
    let env_set = std::env::var(env_var)
        .ok()
        .is_some_and(|v| !v.is_empty());
    if env_set {
        println!("  {env_var} is set in environment (will take priority over stored key)");
    }

    let input = read_line(&format!("  API key ({env_var}) {hint}: "))?;
    let key = if input.trim().is_empty() {
        current_key.unwrap_or("").to_string()
    } else {
        input.trim().to_string()
    };

    if key.is_empty() && !env_set {
        eprintln!("  \x1b[33mWarning: no API key configured. Set {env_var} or re-run setup.\x1b[0m");
    }

    Ok(key)
}

fn prompt_base_url(
    kind: &str,
    current: &RuntimeProviderConfig,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let default_url = DEFAULT_BASE_URLS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or("", |(_, v)| *v);

    let current_url = current.base_url().unwrap_or(default_url);
    let display = if current_url.is_empty() {
        default_url.to_string()
    } else {
        current_url.to_string()
    };

    // Check if the relevant env var is already set
    let env_var = match kind {
        "anthropic" => "ANTHROPIC_BASE_URL",
        "xai" => "XAI_BASE_URL",
        "openai" => "OPENAI_BASE_URL",
        "dashscope" => "DASHSCOPE_BASE_URL",
        _ => "BASE_URL",
    };
    let env_set = std::env::var(env_var)
        .ok()
        .is_some_and(|v| !v.is_empty());
    if env_set {
        println!("  {env_var} is set in environment (will take priority over stored URL)");
    }

    let input = read_line(&format!("  Base URL [{display}]: "))?;
    if input.trim().is_empty() {
        if current_url == default_url || current_url.is_empty() {
            Ok(None)
        } else {
            Ok(Some(current_url.to_string()))
        }
    } else {
        Ok(Some(input.trim().to_string()))
    }
}

fn prompt_model(
    kind: &str,
    current: &RuntimeProviderConfig,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let empty: &[&str] = &[];
    let aliases = PROVIDER_MODELS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(empty, |(_, models)| *models);

    let current_model = current.model().unwrap_or(aliases.first().copied().unwrap_or(""));

    println!("  \x1b[1mModel\x1b[0m");
    if !aliases.is_empty() {
        println!("    Common: {}", aliases.join(", "));
    }
    println!("    Or enter any model name (e.g. openai/gpt-4.1-mini for custom routing)");

    let input = read_line(&format!("  Model [{current_model}]: "))?;
    if input.trim().is_empty() {
        if current_model.is_empty() {
            Ok(None)
        } else {
            Ok(Some(current_model.to_string()))
        }
    } else {
        Ok(Some(input.trim().to_string()))
    }
}

fn read_line(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut stdout = io::stdout();
    write!(stdout, "{prompt}")?;
    stdout.flush()?;
    let mut buffer = String::new();
    io::stdin().read_line(&mut buffer)?;
    Ok(buffer)
}
