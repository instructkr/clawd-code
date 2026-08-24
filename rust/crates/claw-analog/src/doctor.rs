//! `claw-analog doctor` — environment and Cargo sanity checks.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clap::ValueEnum;
use claw_analog::{
    load_analog_toml, load_profile_hint, resolve_analog_options, AnalogDoctorOverrides,
    AnalogFileConfig, OutputFormat, PermissionMode, Preset, StreamOverride, NDJSON_FORMAT_VERSION,
    NDJSON_SCHEMA,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

const ENV_CHECK: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "XAI_API_KEY",
    "RAG_BASE_URL",
];

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum DoctorPermissionArg {
    ReadOnly,
    WorkspaceWrite,
    Prompt,
    #[value(name = "danger-full-access")]
    DangerFullAccess,
    Allow,
}

impl From<DoctorPermissionArg> for PermissionMode {
    fn from(p: DoctorPermissionArg) -> Self {
        match p {
            DoctorPermissionArg::ReadOnly => PermissionMode::ReadOnly,
            DoctorPermissionArg::WorkspaceWrite => PermissionMode::WorkspaceWrite,
            DoctorPermissionArg::Prompt => PermissionMode::Prompt,
            DoctorPermissionArg::DangerFullAccess => PermissionMode::DangerFullAccess,
            DoctorPermissionArg::Allow => PermissionMode::Allow,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum DoctorOutputArg {
    Rich,
    Json,
}

impl From<DoctorOutputArg> for OutputFormat {
    fn from(o: DoctorOutputArg) -> Self {
        match o {
            DoctorOutputArg::Rich => OutputFormat::Rich,
            DoctorOutputArg::Json => OutputFormat::Json,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum DoctorPresetCli {
    None,
    Audit,
    Explain,
    Implement,
}

impl From<DoctorPresetCli> for Preset {
    fn from(p: DoctorPresetCli) -> Self {
        match p {
            DoctorPresetCli::None => Preset::None,
            DoctorPresetCli::Audit => Preset::Audit,
            DoctorPresetCli::Explain => Preset::Explain,
            DoctorPresetCli::Implement => Preset::Implement,
        }
    }
}

#[derive(Debug, clap::Args)]
pub struct DoctorCli {
    /// Workspace root (same as `claw-analog -w`; config defaults to `<workspace>/.claw-analog.toml`).
    #[arg(short = 'w', long, default_value = ".", value_name = "DIR")]
    pub workspace: PathBuf,
    /// Config path (default: `<workspace>/.claw-analog.toml`).
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Override model (same precedence as main CLI).
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long, value_enum)]
    pub permission: Option<DoctorPermissionArg>,
    #[arg(long, value_enum)]
    pub preset: Option<DoctorPresetCli>,
    #[arg(long, value_enum)]
    pub output_format: Option<DoctorOutputArg>,
    #[arg(long, default_value_t = false, conflicts_with = "no_stream")]
    pub stream: bool,
    #[arg(long, default_value_t = false, conflicts_with = "stream")]
    pub no_stream: bool,
    /// Disable `runtime::PermissionEnforcer` (same as main CLI).
    #[arg(
        long = "no-runtime-enforcer",
        default_value_t = false,
        action = clap::ArgAction::SetTrue
    )]
    pub no_runtime_enforcer: bool,
    #[arg(
        long = "accept-danger-non-interactive",
        default_value_t = false,
        action = clap::ArgAction::SetTrue
    )]
    pub accept_danger_non_interactive: bool,
    /// Profile TOML path (optional; if omitted, uses TOML `profile` or default `~/.claw-analog/profile.toml`).
    #[arg(long, value_name = "PATH")]
    pub profile: Option<PathBuf>,
    /// TCP connect to host:port from `ANTHROPIC_BASE_URL` (or default API URL); not a full HTTP check.
    #[arg(long, visible_alias = "mock")]
    pub tcp_ping: bool,
    /// Skip HTTPS/TLS + auth + quota header checks against configured providers.
    #[arg(long, default_value_t = false)]
    pub no_http_check: bool,
    /// Also probe the embeddings endpoint for OpenAI-compatible providers (may incur minimal cost).
    #[arg(long, default_value_t = false)]
    pub embeddings_check: bool,
    /// Skip compile check (`cargo check` / `build --release`).
    #[arg(long)]
    pub no_build: bool,
    /// Run `cargo build --release -p claw-analog` (writes `target/release/…`, safe while `cargo run` holds `target/debug/…` on Windows).
    #[arg(long, conflicts_with = "no_build")]
    pub release_build: bool,
    /// Directory containing the repo workspace `Cargo.toml` (default: search upward from cwd).
    #[arg(long, value_name = "DIR")]
    pub manifest_dir: Option<PathBuf>,
}

pub fn run_doctor(args: DoctorCli) -> i32 {
    println!("claw-analog doctor — environment and build checks\n");

    let workspace = args.workspace.clone();
    let canon_ws = std::fs::canonicalize(&workspace).unwrap_or_else(|_| workspace.clone());
    let cfg_path = args
        .config
        .clone()
        .unwrap_or_else(|| workspace.join(".claw-analog.toml"));
    let (file_cfg, cfg_note) = if cfg_path.is_file() {
        match load_analog_toml(&cfg_path) {
            Ok(c) => (c, "loaded"),
            Err(e) => {
                eprintln!(
                    "[claw-analog] doctor: failed to parse {}: {e} (using empty TOML defaults)",
                    cfg_path.display()
                );
                (AnalogFileConfig::default(), "parse error (defaults)")
            }
        }
    } else {
        (AnalogFileConfig::default(), "file missing (defaults only)")
    };

    let stream_ov = if args.no_stream {
        StreamOverride::ForceOff
    } else if args.stream {
        StreamOverride::ForceOn
    } else {
        StreamOverride::FromFile
    };
    let overrides = AnalogDoctorOverrides {
        model: args.model.clone(),
        permission: args.permission.map(Into::into),
        preset: args.preset.map(Into::into),
        output_format: args.output_format.map(Into::into),
        stream: stream_ov,
        no_runtime_enforcer: args.no_runtime_enforcer,
        accept_danger_non_interactive: args.accept_danger_non_interactive,
    };
    let resolved = resolve_analog_options(&file_cfg, &overrides);

    println!("NDJSON contract (for `--output-format json` runs):");
    println!("  schema: {NDJSON_SCHEMA}");
    println!("  format_version: {NDJSON_FORMAT_VERSION}\n");

    println!("Effective config (merge of `.claw-analog.toml` + flags below):");
    println!("  workspace: {}", canon_ws.display());
    println!("  config: {} ({cfg_note})", cfg_path.display());
    println!("  model: {}", resolved.model);
    println!("  permission: {}", resolved.permission_mode.as_str());
    println!("  preset: {}", resolved.preset.label().unwrap_or("none"));
    println!(
        "  output_format: {}",
        match resolved.output_format {
            OutputFormat::Rich => "rich",
            OutputFormat::Json => "json",
        }
    );
    println!("  stream: {}", resolved.use_stream);
    println!(
        "  runtime_enforcer: {}",
        if resolved.use_runtime_enforcer {
            "on"
        } else {
            "off"
        }
    );
    println!(
        "  accept_danger_non_interactive: {}",
        resolved.accept_danger_non_interactive
    );
    println!("  Provenance (which side won src ← …):");
    for line in &resolved.provenance {
        println!("    - {line}");
    }
    println!();

    let prof = resolve_profile_path_doctor(
        args.profile.as_ref(),
        file_cfg.profile.as_deref(),
        &workspace,
    );
    print_profile_hint_section(&prof);
    println!();

    check_env();
    println!();
    let build_ok = if args.no_build {
        println!("cargo: skipped (--no-build)");
        true
    } else if args.release_build {
        run_cargo_release_build(args.manifest_dir.as_deref())
    } else {
        run_cargo_check(args.manifest_dir.as_deref())
    };
    println!();
    if args.tcp_ping {
        ping_print();
        println!();
    }
    if !args.no_http_check {
        http_checks_print(args.embeddings_check);
        println!();
    }
    if build_ok {
        0
    } else {
        1
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn expand_user_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else {
        PathBuf::from(raw)
    }
}

fn resolve_profile_path_doctor(
    cli: Option<&PathBuf>,
    file: Option<&str>,
    workspace: &Path,
) -> Option<PathBuf> {
    if let Some(p) = cli {
        return Some(if p.is_absolute() {
            p.clone()
        } else {
            workspace.join(p)
        });
    }
    if let Some(s) = file {
        let p = expand_user_path(s.trim());
        return Some(if p.is_absolute() {
            p
        } else {
            workspace.join(p)
        });
    }
    let def = home_dir()?.join(".claw-analog").join("profile.toml");
    if def.is_file() {
        Some(def)
    } else {
        None
    }
}

fn print_profile_hint_section(path: &Option<PathBuf>) {
    println!("Profile (system prompt snippet):");
    match path {
        None => println!("  (none — no --profile, no `profile` in TOML, default file absent)"),
        Some(p) => {
            print!("  path: {}", p.display());
            match load_profile_hint(p) {
                Ok(Some(h)) => println!(" — loaded, {} chars", h.chars().count()),
                Ok(None) => println!(" — file ok, empty `line`"),
                Err(e) => println!(" — error: {e}"),
            }
        }
    }
}

fn mask_env_line(name: &str) {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => {
            println!("  {name}: set ({} chars)", v.chars().count());
        }
        Ok(_) => println!("  {name}: set but empty"),
        Err(_) => println!("  {name}: unset"),
    }
}

fn check_env() {
    println!("Environment (values are not printed):");
    for name in ENV_CHECK {
        mask_env_line(name);
    }
    let anthro_ok = std::env::var("ANTHROPIC_API_KEY")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
        || std::env::var("ANTHROPIC_AUTH_TOKEN")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
    let openai_ok = std::env::var("OPENAI_API_KEY")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    println!();
    if anthro_ok {
        println!("Anthropic credentials: OK (API key and/or auth token).");
    } else {
        println!("Anthropic credentials: not set — needed for default Claude/Anthropic models.");
    }
    if openai_ok {
        println!("OpenAI API key: set — use `openai/...` model prefix for that provider.");
    } else {
        println!("OpenAI API key: unset — only relevant for `openai/` models.");
    }
    if !anthro_ok && !openai_ok {
        println!("\nNote: neither Anthropic nor OpenAI keys are set; live runs will fail until you export credentials (see USAGE.md).");
    }
}

/// Walk upward from `start` for a `Cargo.toml` that defines `[workspace]`.
pub fn discover_cargo_workspace(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    for _ in 0..32 {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            if let Ok(txt) = std::fs::read_to_string(&manifest) {
                if txt.contains("[workspace]") {
                    return Some(dir);
                }
            }
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

fn workspace_root_or_eprint(manifest_dir: Option<&Path>) -> Option<PathBuf> {
    let start = manifest_dir
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    discover_cargo_workspace(&start).or_else(|| {
        eprintln!(
            "cargo: could not find a [workspace] Cargo.toml above {}.\n      Pass --manifest-dir pointing at the `rust` folder of claw-code.",
            start.display()
        );
        None
    })
}

/// `cargo check` does not replace `target/debug/claw-analog.exe`, so `cargo run … doctor` works on Windows.
fn run_cargo_check(manifest_dir: Option<&Path>) -> bool {
    let Some(root) = workspace_root_or_eprint(manifest_dir) else {
        return false;
    };
    println!("cargo check -p claw-analog (workspace {})", root.display());
    println!("  (compile-only; avoids “access denied” replacing the running debug exe on Windows)");
    let status = Command::new("cargo")
        .args(["check", "-p", "claw-analog"])
        .current_dir(&root)
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo check: OK");
            true
        }
        Ok(s) => {
            eprintln!("cargo check: failed ({s})");
            false
        }
        Err(e) => {
            eprintln!("cargo check: could not run `cargo` ({e}). Is Rust/Cargo on PATH?");
            false
        }
    }
}

fn run_cargo_release_build(manifest_dir: Option<&Path>) -> bool {
    let Some(root) = workspace_root_or_eprint(manifest_dir) else {
        return false;
    };
    println!(
        "cargo build --release -p claw-analog (workspace {})",
        root.display()
    );
    println!("  (output in target/release/; does not overwrite a running target/debug/ binary)");
    let status = Command::new("cargo")
        .args(["build", "--release", "-p", "claw-analog"])
        .current_dir(&root)
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo build --release: OK");
            true
        }
        Ok(s) => {
            eprintln!("cargo build --release: failed ({s})");
            false
        }
        Err(e) => {
            eprintln!("cargo build --release: could not run `cargo` ({e}). Is Rust/Cargo on PATH?");
            false
        }
    }
}

fn default_anthropic_base() -> String {
    std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".into())
}

fn parse_host_port(url: &str) -> Result<(String, u16), String> {
    let url = url.trim().trim_end_matches('/');
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = url.strip_prefix("http://") {
        ("http", r)
    } else {
        return Err("URL must start with http:// or https://".into());
    };
    let host_part = rest
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing host".to_string())?;
    if let Some((host, port_s)) = host_part.rsplit_once(':') {
        if let Ok(p) = port_s.parse::<u16>() {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            return Ok((host.to_string(), p));
        }
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    Ok((host_part.to_string(), default_port))
}

fn ping_print() {
    let url = default_anthropic_base();
    println!("TCP check for ANTHROPIC_BASE_URL (default if unset): {url}");
    match parse_host_port(&url) {
        Ok((host, port)) => match tcp_ping(&host, port) {
            Ok(()) => println!("  reachability: OK ({host}:{port})"),
            Err(e) => println!("  reachability: FAIL ({host}:{port}) — {e}"),
        },
        Err(e) => println!("  could not parse URL: {e}"),
    }
    println!("  (HTTP/TLS application data is not validated; this is connect() only.)");
}

fn tcp_ping(host: &str, port: u16) -> Result<(), String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| "no resolved addresses".to_string())?;
    TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    Ok(())
}

fn http_checks_print(embeddings_check: bool) {
    println!("HTTP/TLS checks (auth + TLS validation + quota headers when available):");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let Ok(rt) = rt else {
        println!("  runtime: FAIL (could not build tokio runtime)");
        return;
    };

    rt.block_on(async {
        // OpenAI-compatible providers (OPENAI_BASE_URL, OPENAI_API_KEY)
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            if !key.trim().is_empty() {
                let base = std::env::var("OPENAI_BASE_URL")
                    .ok()
                    .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                let url = openai_models_url(base.as_str());
                let mut headers = HeaderMap::new();
                if let Ok(v) = HeaderValue::from_str(format!("Bearer {}", key.trim()).as_str()) {
                    headers.insert(reqwest::header::AUTHORIZATION, v);
                }
                let _ = http_check_and_print("openai", url.as_str(), headers).await;

                if embeddings_check {
                    let model = std::env::var("OPENAI_EMBEDDING_MODEL")
                        .ok()
                        .or_else(|| std::env::var("CLAW_RAG_EMBEDDING_MODEL").ok())
                        .unwrap_or_else(|| "text-embedding-3-small".to_string());
                    let eurl = openai_embeddings_url(base.as_str());
                    let mut eheaders = HeaderMap::new();
                    if let Ok(v) = HeaderValue::from_str(format!("Bearer {}", key.trim()).as_str())
                    {
                        eheaders.insert(reqwest::header::AUTHORIZATION, v);
                    }
                    let _ = openai_embeddings_probe(
                        "openai embeddings",
                        eurl.as_str(),
                        &model,
                        eheaders,
                    )
                    .await;
                } else {
                    println!("  openai embeddings: skipped (pass --embeddings-check to enable)");
                }
            } else {
                println!("  openai: skipped (OPENAI_API_KEY empty)");
            }
        } else {
            println!("  openai: skipped (OPENAI_API_KEY unset)");
        }

        // Anthropic (ANTHROPIC_BASE_URL, ANTHROPIC_API_KEY/AUTH_TOKEN)
        let a_key = std::env::var("ANTHROPIC_API_KEY").ok();
        let a_tok = std::env::var("ANTHROPIC_AUTH_TOKEN").ok();
        let a_base = std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .unwrap_or_else(|| "https://api.anthropic.com".to_string());
        if a_key.as_deref().is_some_and(|s| !s.trim().is_empty())
            || a_tok.as_deref().is_some_and(|s| !s.trim().is_empty())
        {
            let url = anthropic_models_url(a_base.as_str());
            let mut headers = HeaderMap::new();
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static("2023-06-01"),
            );
            if let Some(k) = a_key.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                if let Ok(v) = HeaderValue::from_str(k) {
                    headers.insert(HeaderName::from_static("x-api-key"), v);
                }
            } else if let Some(t) = a_tok.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                if let Ok(v) = HeaderValue::from_str(format!("Bearer {t}").as_str()) {
                    headers.insert(reqwest::header::AUTHORIZATION, v);
                }
            }
            let _ = http_check_and_print("anthropic", url.as_str(), headers).await;
        } else {
            println!("  anthropic: skipped (no API key/token)");
        }

        // RAG service (RAG_BASE_URL) — just basic health + stats.
        if let Ok(base) = std::env::var("RAG_BASE_URL") {
            let base = base.trim().trim_end_matches('/');
            if !base.is_empty() {
                let headers = HeaderMap::new();
                let _ =
                    http_check_and_print("rag health", &format!("{base}/health"), headers.clone())
                        .await;
                let _ =
                    http_check_and_print("rag stats", &format!("{base}/v1/stats"), headers).await;
            }
        }
    });

    println!("  (TLS validation is performed by the HTTP client; certificate errors surface as request failures.)");
}

fn openai_models_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/v1") {
        format!("{b}/models")
    } else {
        format!("{b}/v1/models")
    }
}

fn openai_embeddings_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/v1") {
        format!("{b}/embeddings")
    } else {
        format!("{b}/v1/embeddings")
    }
}

fn anthropic_models_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    format!("{b}/v1/models?limit=1")
}

async fn http_check_and_print(label: &str, url: &str, headers: HeaderMap) -> Result<(), ()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build();
    let Ok(client) = client else {
        println!("  {label}: FAIL (client build)");
        return Err(());
    };

    let resp = client.get(url).headers(headers).send().await;
    match resp {
        Ok(r) => {
            let status = r.status();
            println!("  {label}: {status} ({url})");
            print_quota_headers(r.headers());
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.to_ascii_lowercase().contains("certificate")
                || msg.to_ascii_lowercase().contains("tls")
            {
                println!("  {label}: FAIL (TLS/cert) ({url}) — {msg}");
            } else {
                println!("  {label}: FAIL ({url}) — {msg}");
            }
            Err(())
        }
    }
}

fn print_quota_headers(headers: &HeaderMap) {
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in headers.iter() {
        let name = k.as_str().to_ascii_lowercase();
        if name.contains("ratelimit") || name.contains("quota") {
            if let Ok(s) = v.to_str() {
                out.push((k.as_str().to_string(), s.to_string()));
            }
        }
        // OpenAI-compatible common headers:
        if name.starts_with("x-ratelimit-") {
            if let Ok(s) = v.to_str() {
                out.push((k.as_str().to_string(), s.to_string()));
            }
        }
    }
    out.sort();
    out.dedup();
    for (k, v) in out {
        println!("    {k}: {v}");
    }
}

async fn openai_embeddings_probe(
    label: &str,
    url: &str,
    model: &str,
    headers: HeaderMap,
) -> Result<(), ()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build();
    let Ok(client) = client else {
        println!("  {label}: FAIL (client build)");
        return Err(());
    };

    // Minimal request: one short string. We don't parse the embedding content.
    let body = serde_json::json!({
        "model": model,
        "input": ["ping"]
    });

    let resp = client.post(url).headers(headers).json(&body).send().await;
    match resp {
        Ok(r) => {
            let status = r.status();
            println!("  {label}: {status} ({url}) model={model}");
            print_quota_headers(r.headers());
            if !status.is_success() {
                let t = r.text().await.unwrap_or_default();
                if !t.trim().is_empty() {
                    println!("    body: {}", t.chars().take(400).collect::<String>());
                }
                return Err(());
            }
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.to_ascii_lowercase().contains("certificate")
                || msg.to_ascii_lowercase().contains("tls")
            {
                println!("  {label}: FAIL (TLS/cert) ({url}) — {msg}");
            } else {
                println!("  {label}: FAIL ({url}) — {msg}");
            }
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base_url_host_port() {
        assert_eq!(
            parse_host_port("http://127.0.0.1:8080/v1").unwrap(),
            ("127.0.0.1".into(), 8080)
        );
        assert_eq!(
            parse_host_port("https://api.anthropic.com").unwrap(),
            ("api.anthropic.com".into(), 443)
        );
    }
}
