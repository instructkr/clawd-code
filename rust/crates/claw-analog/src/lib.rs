//! Lean agent harness: tool loop, optional streaming, optional `PermissionEnforcer`.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt::Write;
use std::io::{self, IsTerminal};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use api::{
    max_tokens_for_model, resolve_model_alias, ApiError, ContentBlockDelta, ContentBlockStartEvent,
    InputContentBlock, InputMessage, MessageDeltaEvent, MessageRequest, MessageResponse,
    MessageStartEvent, MessageStopEvent, OutputContentBlock, ProviderClient, StreamEvent,
    ToolChoice, ToolDefinition, ToolResultContentBlock,
};
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use runtime::permission_enforcer::{EnforcementResult, PermissionEnforcer};
pub use runtime::PermissionMode;
use runtime::PermissionPolicy;
use serde_json::{json, Value};

/// Refuses unrestricted permission modes in non-interactive runs unless explicitly opted in (same spirit as full `claw` CLI).
pub fn enforce_non_interactive_permission_rules(
    mode: PermissionMode,
    accept_danger_non_interactive: bool,
) -> Result<(), String> {
    enforce_non_interactive_permission_rules_with_tty(
        mode,
        accept_danger_non_interactive,
        io::stdin().is_terminal(),
    )
}

/// Same as [`enforce_non_interactive_permission_rules`] but with an explicit stdin-TTY flag (for tests and tooling).
pub fn enforce_non_interactive_permission_rules_with_tty(
    mode: PermissionMode,
    accept_danger_non_interactive: bool,
    stdin_is_tty: bool,
) -> Result<(), String> {
    if matches!(
        mode,
        PermissionMode::DangerFullAccess | PermissionMode::Allow
    ) && !stdin_is_tty
        && !accept_danger_non_interactive
    {
        return Err(
            "permission modes 'danger-full-access' and 'allow' are refused when stdin is not a TTY (non-interactive). \
             Use --permission read-only or workspace-write for CI/automation, or pass --accept-danger-non-interactive if you accept the risk."
                .into(),
        );
    }
    if mode == PermissionMode::Prompt && !stdin_is_tty {
        eprintln!(
            "[claw-analog] warning: 'prompt' without a TTY cannot confirm tool use; writes remain denied. For headless edits use --permission workspace-write."
        );
    }
    Ok(())
}

/// Assistant reply language hint (system prompt); does not switch the API model name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnalogLanguage {
    #[default]
    En,
    Ru,
}

impl AnalogLanguage {
    #[must_use]
    pub fn from_toml_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Some(Self::En),
            "ru" | "russian" => Some(Self::Ru),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ru => "ru",
        }
    }
}

fn language_system_hint(lang: AnalogLanguage) -> Option<&'static str> {
    match lang {
        AnalogLanguage::En => None,
        AnalogLanguage::Ru => Some(
            "Язык: отвечайте по-русски, когда пользователь пишет по-русски; пути к файлам, идентификаторы в коде и стандартные термины API можно оставлять на английском.",
        ),
    }
}

/// Human-readable text vs newline-delimited JSON events (for CI and agent pipelines).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Rich,
    Json,
}

/// Built-in behavior presets: system prompt bias + default permission when not overridden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    #[default]
    None,
    Audit,
    Explain,
    Implement,
}

impl Preset {
    #[must_use]
    pub fn from_toml_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "none" => Some(Self::None),
            "audit" => Some(Self::Audit),
            "explain" => Some(Self::Explain),
            "implement" => Some(Self::Implement),
            _ => None,
        }
    }

    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Audit => Some("audit"),
            Self::Explain => Some("explain"),
            Self::Implement => Some("implement"),
        }
    }

    fn extra_system(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Audit => Some(
                "Preset: audit — prioritize security, correctness, and suspicious patterns; cite file paths and evidence; prefer read-only investigation.",
            ),
            Self::Explain => Some(
                "Preset: explain — teach clearly; define terms; ground claims in repository content; avoid unnecessary jargon.",
            ),
            Self::Implement => Some(
                "Preset: implement — make focused edits; read before writing when unsure; keep changes small and explain what you changed.",
            ),
        }
    }
}

/// Infer a reasonable preset from the initial user prompt.
///
/// This is intentionally heuristic and conservative:
/// - Prefer `audit` when security/review intent is detected.
/// - Prefer `implement` when the user asks to change/fix/add/refactor something.
/// - Prefer `explain` for "why/how/explain" style questions.
/// - Fall back to `none`.
#[must_use]
pub fn infer_preset_from_prompt(prompt: &str) -> Preset {
    let p = prompt.trim().to_ascii_lowercase();
    if p.is_empty() {
        return Preset::None;
    }

    // High priority: audit / security review intent.
    let audit_hits = [
        "audit",
        "security",
        "secure",
        "vuln",
        "vulnerability",
        "threat",
        "review",
        "pentest",
        "опасн",
        "безопас",
        "уязв",
        "аудит",
        "ревью",
    ];
    if audit_hits.iter().any(|k| p.contains(k)) {
        return Preset::Audit;
    }

    // Next: implement intent (do work / change code).
    let implement_hits = [
        "implement",
        "add",
        "build",
        "create",
        "change",
        "update",
        "refactor",
        "optimize",
        "fix",
        "bug",
        "feature",
        "сделай",
        "сделать",
        "добав",
        "передел",
        "измен",
        "обнов",
        "рефактор",
        "оптимиз",
        "почин",
        "исправ",
        "баг",
        "фича",
    ];
    if implement_hits.iter().any(|k| p.contains(k)) {
        return Preset::Implement;
    }

    // Then: explain intent.
    let explain_hits = [
        "explain",
        "why",
        "how",
        "what is",
        "help me understand",
        "объясни",
        "объяснить",
        "почему",
        "как",
        "что такое",
        "разъясни",
    ];
    if explain_hits.iter().any(|k| p.contains(k)) {
        return Preset::Explain;
    }

    Preset::None
}

/// Stable NDJSON contract id for consumers. Bump [`NDJSON_FORMAT_VERSION`] when event shapes break compatibility.
pub const NDJSON_SCHEMA: &str = "claw-analog-ndjson";
/// Increment when NDJSON event types or required `run_start` fields change incompatibly.
pub const NDJSON_FORMAT_VERSION: u32 = 1;

/// Default `model` when CLI and TOML omit it.
pub const ANALOG_DEFAULT_MODEL: &str = "sonnet";

/// Map TOML / policy strings to [`PermissionMode`] (same rules as the main `claw-analog` CLI).
#[must_use]
pub fn permission_mode_from_toml_str(s: &str) -> Option<PermissionMode> {
    match s.to_ascii_lowercase().replace('_', "-").as_str() {
        "read-only" | "readonly" => Some(PermissionMode::ReadOnly),
        "workspace-write" | "write" => Some(PermissionMode::WorkspaceWrite),
        "prompt" => Some(PermissionMode::Prompt),
        "danger-full-access" | "danger" => Some(PermissionMode::DangerFullAccess),
        "allow" => Some(PermissionMode::Allow),
        _ => None,
    }
}

fn output_format_from_toml_str(s: &str) -> Option<OutputFormat> {
    match s.to_ascii_lowercase().as_str() {
        "json" => Some(OutputFormat::Json),
        "rich" => Some(OutputFormat::Rich),
        _ => None,
    }
}

/// How doctor (or tooling) overrides `stream` relative to TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamOverride {
    #[default]
    FromFile,
    ForceOn,
    ForceOff,
}

/// Optional CLI knobs for [`resolve_analog_options`] (subset of the real run CLI).
#[derive(Debug, Clone, Default)]
pub struct AnalogDoctorOverrides {
    pub model: Option<String>,
    pub permission: Option<PermissionMode>,
    pub preset: Option<Preset>,
    pub output_format: Option<OutputFormat>,
    pub stream: StreamOverride,
    pub no_runtime_enforcer: bool,
    pub accept_danger_non_interactive: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedAnalogOptions {
    pub model: String,
    pub permission_mode: PermissionMode,
    pub preset: Preset,
    pub output_format: OutputFormat,
    pub use_stream: bool,
    pub use_runtime_enforcer: bool,
    pub accept_danger_non_interactive: bool,
    /// One line per knob: human-readable provenance (`model ← CLI`, etc.).
    pub provenance: Vec<String>,
}

/// Effective options after merging `.claw-analog.toml` with optional CLI overrides (same precedence as `claw-analog` run).
#[must_use]
pub fn resolve_analog_options(
    file: &AnalogFileConfig,
    overrides: &AnalogDoctorOverrides,
) -> ResolvedAnalogOptions {
    let (model, m_src) = if let Some(ref m) = overrides.model {
        (m.trim().to_string(), "CLI")
    } else if let Some(ref fm) = file.model {
        let fm = fm.trim();
        if fm.is_empty() {
            (ANALOG_DEFAULT_MODEL.to_string(), "default (empty in TOML)")
        } else {
            (fm.to_string(), ".claw-analog.toml")
        }
    } else {
        (ANALOG_DEFAULT_MODEL.to_string(), "default")
    };

    let (preset, p_src) = if let Some(p) = overrides.preset {
        (p, "CLI")
    } else if let Some(s) = file.preset.as_deref().and_then(Preset::from_toml_str) {
        (s, ".claw-analog.toml")
    } else {
        (Preset::None, "default (none)")
    };

    let (permission_mode, perm_src) = if let Some(p) = overrides.permission {
        (p, "CLI")
    } else if let Some(s) = file
        .permission
        .as_deref()
        .and_then(permission_mode_from_toml_str)
    {
        (s, ".claw-analog.toml")
    } else {
        match preset {
            Preset::Implement => (
                PermissionMode::WorkspaceWrite,
                "default for preset implement",
            ),
            _ => (PermissionMode::ReadOnly, "default (read-only)"),
        }
    };

    let (output_format, of_src) = if let Some(o) = overrides.output_format {
        (o, "CLI")
    } else if let Some(s) = file
        .output_format
        .as_deref()
        .and_then(output_format_from_toml_str)
    {
        (s, ".claw-analog.toml")
    } else {
        (OutputFormat::Rich, "default (rich)")
    };

    let (use_stream, stream_src) = match overrides.stream {
        StreamOverride::ForceOn => (true, "CLI (--stream)"),
        StreamOverride::ForceOff => (false, "CLI (--no-stream)"),
        StreamOverride::FromFile => {
            if let Some(b) = file.stream {
                (b, ".claw-analog.toml")
            } else {
                (false, "default (off)")
            }
        }
    };

    let use_runtime_enforcer =
        !overrides.no_runtime_enforcer && !file.no_runtime_enforcer.unwrap_or(false);
    let re_src = if overrides.no_runtime_enforcer {
        "CLI (--no-runtime-enforcer)"
    } else if file.no_runtime_enforcer == Some(true) {
        ".claw-analog.toml"
    } else {
        "default (on)"
    };

    let accept_danger_non_interactive = overrides.accept_danger_non_interactive
        || file.accept_danger_non_interactive.unwrap_or(false);
    let ad_src = match (
        overrides.accept_danger_non_interactive,
        file.accept_danger_non_interactive.unwrap_or(false),
    ) {
        (true, true) => "CLI and .claw-analog.toml",
        (true, false) => "CLI",
        (false, true) => ".claw-analog.toml",
        (false, false) => "default (off)",
    };

    let provenance = vec![
        format!("model ← {m_src}"),
        format!("preset ← {p_src}"),
        format!("permission ← {perm_src}"),
        format!("output_format ← {of_src}"),
        format!("stream ← {stream_src}"),
        format!("runtime_enforcer ← {re_src}"),
        format!("accept_danger_non_interactive ← {ad_src}"),
    ];

    ResolvedAnalogOptions {
        model,
        permission_mode,
        preset,
        output_format,
        use_stream,
        use_runtime_enforcer,
        accept_danger_non_interactive,
        provenance,
    }
}

/// User home directory (`USERPROFILE` on Windows, `HOME` elsewhere).
#[must_use]
pub fn analog_user_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// Expand a leading `~/` path using [`analog_user_home_dir`].
#[must_use]
pub fn analog_expand_tilde_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        analog_user_home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else {
        PathBuf::from(raw)
    }
}

/// Match main CLI profile resolution: `--profile`, then TOML `profile`, then default `~/.claw-analog/profile.toml` if it exists.
#[must_use]
pub fn resolve_analog_profile_path(
    workspace: &Path,
    profile_cli: Option<PathBuf>,
    profile_from_toml: Option<&str>,
) -> Option<PathBuf> {
    if let Some(p) = profile_cli {
        return Some(if p.is_absolute() {
            p
        } else {
            workspace.join(&p)
        });
    }
    if let Some(s) = profile_from_toml {
        let p = analog_expand_tilde_path(s.trim());
        return Some(if p.is_absolute() {
            p
        } else {
            workspace.join(p)
        });
    }
    let def = analog_user_home_dir()?
        .join(".claw-analog")
        .join("profile.toml");
    if def.is_file() {
        Some(def)
    } else {
        None
    }
}

fn persist_conversation_sessions(
    config: &AnalogConfig,
    ws_str: &str,
    model: &str,
    messages: &[InputMessage],
) -> Result<(), String> {
    if let Some(p) = &config.session_path {
        session_save(p, ws_str, model, config.preset, messages)?;
    }
    if let Some(p) = &config.session_save_path {
        let duplicate = config.session_path.as_ref() == Some(p);
        if !duplicate {
            session_save(p, ws_str, model, config.preset, messages)?;
        }
    }
    Ok(())
}

/// Max bytes read from `profile.toml`; line is truncated to this many **Unicode scalars** after trim.
pub const PROFILE_FILE_MAX_BYTES: usize = 2048;
pub const PROFILE_LINE_MAX_CHARS: usize = 512;

const SESSION_FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SessionFile {
    version: u32,
    workspace: String,
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    messages: Vec<InputMessage>,
}

/// Load a session file without appending a new user prompt.
pub fn session_load_messages(path: &Path) -> Result<Vec<InputMessage>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file: SessionFile = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if file.version != SESSION_FILE_VERSION {
        return Err(format!(
            "session file version {} not supported (expected {SESSION_FILE_VERSION})",
            file.version
        ));
    }
    Ok(file.messages)
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let tmp = path.with_extension("tmp_session_write");
    std::fs::write(&tmp, contents).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Load prior turns (if the file exists), append `new_prompt` as a new user message.
pub fn session_bootstrap_messages(
    path: &Path,
    workspace: &str,
    model: &str,
    preset: Preset,
    new_prompt: &str,
) -> Result<Vec<InputMessage>, String> {
    if !path.exists() {
        return Ok(vec![InputMessage::user_text(new_prompt.to_string())]);
    }
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file: SessionFile = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if file.version != SESSION_FILE_VERSION {
        return Err(format!(
            "session file version {} not supported (expected {SESSION_FILE_VERSION})",
            file.version
        ));
    }
    if file.workspace != workspace {
        eprintln!(
            "[claw-analog] warning: session workspace differs (file: {}, current: {})",
            file.workspace, workspace
        );
    }
    if file.model != model {
        eprintln!(
            "[claw-analog] warning: session model differs (file: {}, current: {})",
            file.model, model
        );
    }
    let want_preset = preset.label().map(String::from);
    if want_preset.as_deref() != file.preset.as_deref() {
        eprintln!(
            "[claw-analog] warning: session preset {:?} vs current {:?}",
            file.preset, want_preset
        );
    }
    let mut messages = file.messages;
    messages.push(InputMessage::user_text(new_prompt.to_string()));
    Ok(messages)
}

pub fn session_save(
    path: &Path,
    workspace: &str,
    model: &str,
    preset: Preset,
    messages: &[InputMessage],
) -> Result<(), String> {
    let data = SessionFile {
        version: SESSION_FILE_VERSION,
        workspace: workspace.into(),
        model: model.into(),
        preset: preset.label().map(String::from),
        messages: messages.to_vec(),
    };
    let json = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
    atomic_write(path, json.as_bytes())?;
    Ok(())
}

fn session_warn_common() {
    eprintln!(
        "[claw-analog] session: files may contain secrets (tool output, pasted keys). Do not share. Large histories increase API cost."
    );
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileToml {
    line: Option<String>,
}

/// Read `~/.claw-analog/profile.toml`-style file: single `line` merged into system prompt.
pub fn load_profile_hint(path: &Path) -> Result<Option<String>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() as usize > PROFILE_FILE_MAX_BYTES {
        return Err(format!(
            "profile file too large ({} bytes; max {})",
            meta.len(),
            PROFILE_FILE_MAX_BYTES
        ));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let parsed: ProfileToml = toml::from_str(&raw).map_err(|e| e.to_string())?;
    let line = parsed.line.unwrap_or_default();
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let nchars = line.chars().count();
    if nchars > PROFILE_LINE_MAX_CHARS {
        eprintln!(
            "[claw-analog] warning: profile line truncated ({} chars; max {})",
            nchars, PROFILE_LINE_MAX_CHARS
        );
        let truncated: String = line.chars().take(PROFILE_LINE_MAX_CHARS).collect();
        return Ok(Some(truncated));
    }
    Ok(Some(line.to_string()))
}

#[derive(Debug, Clone)]
pub struct AnalogConfig {
    pub model: String,
    pub workspace: PathBuf,
    /// Active [`PermissionMode`] (read-only, workspace-write, prompt, danger-full-access, allow).
    pub permission_mode: PermissionMode,
    /// Allow `danger-full-access` / `allow` when stdin is not a TTY (automation opt-in).
    pub accept_danger_non_interactive: bool,
    pub use_stream: bool,
    pub output_format: OutputFormat,
    /// Gate tools with [`PermissionEnforcer`] (aligned with main CLI policy).
    pub use_runtime_enforcer: bool,
    pub max_read_bytes: u64,
    pub max_turns: u32,
    pub max_list_entries: usize,
    pub grep_max_lines: usize,
    /// Cap for `glob_workspace` and for `grep_workspace` when using `glob`.
    pub glob_max_paths: usize,
    /// `walkdir` max depth from the search root (prevents unbounded recursion).
    pub glob_max_depth: usize,
    pub preset: Preset,
    /// Bias assistant replies toward English or Russian (system prompt only).
    pub language: AnalogLanguage,
    /// When set, load/save turn history (resume with the same path). See session warnings in `how_to_run.md`.
    pub session_path: Option<PathBuf>,
    /// After each session snapshot, also write this path (export without resuming from `--session`, or copy of the same file).
    pub session_save_path: Option<PathBuf>,
    /// One short line from profile TOML, merged into the system prompt.
    pub profile_hint: Option<String>,
    pub prompt: String,
    /// When set (TOML `rag_base_url` or env `RAG_BASE_URL`), exposes `retrieve_context` and calls `POST {base}/v1/query`.
    pub rag_base_url: Option<String>,
    pub rag_http_timeout: Duration,
    /// Upper bound for `top_k` accepted from the model (default 32).
    pub rag_top_k_max: u32,
}

/// Optional defaults from `.claw-analog.toml` (see `load_analog_toml`).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalogFileConfig {
    pub model: Option<String>,
    pub stream: Option<bool>,
    pub output_format: Option<String>,
    pub permission: Option<String>,
    pub no_runtime_enforcer: Option<bool>,
    /// Acknowledge danger/allow mode in CI when stdin is not a TTY.
    pub accept_danger_non_interactive: Option<bool>,
    pub max_read_bytes: Option<u64>,
    pub max_turns: Option<u32>,
    pub max_list_entries: Option<usize>,
    pub grep_max_lines: Option<usize>,
    pub glob_max_paths: Option<usize>,
    pub glob_max_depth: Option<usize>,
    pub preset: Option<String>,
    /// `en` or `ru` — reply language hint in system prompt (not the API model id).
    pub language: Option<String>,
    /// Session file path (relative to workspace if not absolute).
    pub session: Option<String>,
    /// Profile snippet path (default `~/.claw-analog/profile.toml` when omitted; see `profile` CLI).
    pub profile: Option<String>,
    /// Override env `RAG_BASE_URL` when non-empty (HTTP root of `claw-rag-service`, no trailing `/v1` path).
    pub rag_base_url: Option<String>,
    /// Timeout for `retrieve_context` HTTP calls (seconds).
    pub rag_timeout_secs: Option<u64>,
    /// Max `top_k` the model may request (default 32, hard-capped at 256).
    pub rag_top_k_max: Option<u32>,
}

/// Read `.claw-analog.toml`; relative paths are the caller's responsibility.
pub fn load_analog_toml(path: &Path) -> Result<AnalogFileConfig, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    toml::from_str(&raw).map_err(|e| e.to_string())
}

/// Non-empty `rag_base_url` from TOML wins; otherwise `RAG_BASE_URL` from the environment.
#[must_use]
pub fn resolve_rag_base_url(file: &AnalogFileConfig) -> Option<String> {
    let from_file = file
        .rag_base_url
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    if from_file.is_some() {
        return from_file;
    }
    std::env::var("RAG_BASE_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

const MAX_JSON_TOOL_OUTPUT_BYTES: usize = 32 * 1024;
const RAG_QUERY_MAX_CHARS: usize = 12_000;

fn write_json_line(out: &mut impl std::io::Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *out, value).map_err(std::io::Error::other)?;
    writeln!(out)
}

fn truncate_for_json(s: &str) -> (String, bool) {
    let max = MAX_JSON_TOOL_OUTPUT_BYTES;
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &s[..end]), true)
}

fn assistant_plain_text(content: &[OutputContentBlock]) -> String {
    content
        .iter()
        .filter_map(|b| {
            if let OutputContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .concat()
}

fn tool_calls_for_json(content: &[OutputContentBlock]) -> Vec<Value> {
    content
        .iter()
        .filter_map(|b| {
            if let OutputContentBlock::ToolUse { id, name, input } = b {
                Some(json!({
                    "id": id,
                    "name": name,
                    "input": input,
                }))
            } else {
                None
            }
        })
        .collect()
}

fn git_gate_is_repo(workspace: &Path) -> Result<(), String> {
    let out = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace)
        .output()
        .map_err(|e| format!("git not available: {e}"))?;
    if !out.status.success() {
        return Err("not a git work tree".to_string());
    }
    Ok(())
}

fn is_safe_git_rev_range(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.len() > 200 {
        return false;
    }
    // Conservative allowlist: alnum, common ref/range punctuation.
    t.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '_' | '-' | '^' | '~' | ':' | '@')
    })
}

fn read_pipe_capped(r: impl std::io::Read, cap: usize) -> std::io::Result<(Vec<u8>, bool)> {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut limited = r.take(u64::try_from(cap.saturating_add(1)).unwrap_or(u64::MAX));
    limited.read_to_end(&mut buf)?;
    let truncated = buf.len() > cap;
    if truncated {
        buf.truncate(cap);
    }
    Ok((buf, truncated))
}

fn run_git_capped(workspace: &Path, args: &[String], cap: usize) -> Result<String, String> {
    git_gate_is_repo(workspace)?;
    let mut child = Command::new("git")
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git spawn failed: {e}"))?;

    let stdout = child.stdout.take().ok_or("git stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("git stderr unavailable")?;

    let out_handle = std::thread::spawn(move || read_pipe_capped(stdout, cap));
    let err_handle = std::thread::spawn(move || read_pipe_capped(stderr, cap));

    let status = child.wait().map_err(|e| format!("git wait failed: {e}"))?;
    let (out_bytes, out_trunc) = out_handle
        .join()
        .map_err(|_| "git stdout thread panicked".to_string())?
        .map_err(|e| format!("git stdout read failed: {e}"))?;
    let (err_bytes, err_trunc) = err_handle
        .join()
        .map_err(|_| "git stderr thread panicked".to_string())?
        .map_err(|e| format!("git stderr read failed: {e}"))?;

    let mut out = String::from_utf8_lossy(&out_bytes).into_owned();
    let err = String::from_utf8_lossy(&err_bytes).into_owned();
    if !err.trim().is_empty() {
        if !out.trim().is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(err.trim_end());
    }
    if out_trunc || err_trunc {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("… truncated to {cap} bytes"));
    }

    if status.success() {
        Ok(out)
    } else if out.trim().is_empty() {
        Err(format!("git failed (exit={})", status))
    } else {
        Err(out)
    }
}

fn build_policy(mode: PermissionMode) -> PermissionPolicy {
    PermissionPolicy::new(mode)
        .with_tool_requirement("read_file", PermissionMode::ReadOnly)
        .with_tool_requirement("list_dir", PermissionMode::ReadOnly)
        .with_tool_requirement("glob_workspace", PermissionMode::ReadOnly)
        .with_tool_requirement("grep_workspace", PermissionMode::ReadOnly)
        .with_tool_requirement("grep_search", PermissionMode::ReadOnly)
        .with_tool_requirement("git_diff", PermissionMode::ReadOnly)
        .with_tool_requirement("git_log", PermissionMode::ReadOnly)
        .with_tool_requirement("retrieve_context", PermissionMode::ReadOnly)
        .with_tool_requirement("write_file", PermissionMode::WorkspaceWrite)
}

fn tool_definitions(mode: PermissionMode, rag_base_url: Option<&str>) -> Vec<ToolDefinition> {
    let mut tools = vec![
        ToolDefinition {
            name: "read_file".to_string(),
            description: Some("Read a UTF-8 file under the workspace.".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path from workspace root" }
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "list_dir".to_string(),
            description: Some("Non-recursive directory listing (use `.` for root).".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative directory" }
                }
            }),
        },
        ToolDefinition {
            name: "glob_workspace".to_string(),
            description: Some(
                "List UTF-8 file paths under workspace matching a glob (relative to search root). Recursive depth and path count are capped. For Rust monorepos, crates often live under `rust/crates/<name>/`; use `root` `.` and patterns like `**/my-crate/**/*.rs` if a direct path is unknown.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "root": { "type": "string", "description": "Relative directory under workspace (default `.`)" },
                    "pattern": { "type": "string", "description": "Glob relative to root, use `/` e.g. `**/*.rs`" },
                    "max_paths": { "type": "integer", "description": "Max paths to return (capped by server)" }
                },
                "required": ["pattern"]
            }),
        },
        ToolDefinition {
            name: "grep_workspace".to_string(),
            description: Some(
                "Literal substring search per line (no regex, no shell). Pass `path`, or `paths`, or `glob` + optional `glob_root`.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Single relative file" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Several relative files" },
                    "glob": { "type": "string", "description": "Glob of files under workspace (same rules as glob_workspace)" },
                    "glob_root": { "type": "string", "description": "Directory for `glob` (default `.`)" },
                    "pattern": { "type": "string", "description": "Literal substring" },
                    "max_lines": { "type": "integer", "description": "Total max matching lines across all files (capped)" }
                },
                "required": ["pattern"]
            }),
        },
        ToolDefinition {
            name: "grep_search".to_string(),
            description: Some("Alias of `grep_workspace` (prompt compatibility). Same inputs.".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" } },
                    "glob": { "type": "string" },
                    "glob_root": { "type": "string" },
                    "pattern": { "type": "string" },
                    "max_lines": { "type": "integer" }
                },
                "required": ["pattern"]
            }),
        },
        ToolDefinition {
            name: "git_diff".to_string(),
            description: Some(
                "Read-only `git diff` from the workspace repo (no color). Optional `cached` for staged diff; optional `rev_range`; optional path filters.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cached": { "type": "boolean", "description": "Use --cached (staged diff)" },
                    "rev_range": { "type": "string", "description": "Revision range like `HEAD~3..HEAD` or `main...HEAD`" },
                    "context_lines": { "type": "integer", "description": "Unified diff context lines (passed as -U<n>)" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Relative paths to limit the diff" }
                }
            }),
        },
        ToolDefinition {
            name: "git_log".to_string(),
            description: Some(
                "Read-only `git log` from the workspace repo (no color). Supports `max_count`, optional `rev_range`, optional path filters.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "max_count": { "type": "integer", "description": "Max commits (default 20; capped by server)" },
                    "rev_range": { "type": "string", "description": "Revision range like `HEAD~20..HEAD`" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Relative paths to limit the log" }
                }
            }),
        },
    ];
    if rag_base_url.is_some() {
        tools.push(ToolDefinition {
            name: "retrieve_context".to_string(),
            description: Some(
                "Semantic search over the workspace RAG index (separate claw-rag-service). Returns paths and snippets.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural-language query" },
                    "top_k": { "type": "integer", "description": "Max hits (default 8; capped by server)" }
                },
                "required": ["query"]
            }),
        });
    }
    if matches!(
        mode,
        PermissionMode::WorkspaceWrite | PermissionMode::DangerFullAccess | PermissionMode::Allow
    ) {
        tools.push(ToolDefinition {
            name: "write_file".to_string(),
            description: Some(
                "Create or overwrite a UTF-8 file (parents created if needed).".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        });
    }
    tools
}

/// Nudge models away from answering “implementation” questions from ops wiring alone.
const SOURCE_GROUNDING_HINT: &str = "When asked where something is implemented or how an internal pipeline works, ground the answer in program source (e.g. crate modules, `main`/CLI entrypoints), not only deployment manifests (`docker-compose`, CI YAML, shell scripts) unless the question is explicitly about ops. Open the relevant service sources before concluding. If `list_dir`/`glob_workspace` under a short name (e.g. a service folder) returns empty, this repo is often a monorepo: try `glob_workspace` with `root` `.` and a broad `pattern` such as `**/claw-rag-service/**/*.rs` or `rust/crates/**/src/**/*.rs` before concluding the code is missing.";

fn system_prompt(
    mode: PermissionMode,
    root: &Path,
    preset: Preset,
    profile_hint: Option<&str>,
    language: AnalogLanguage,
    rag_enabled: bool,
) -> String {
    let root_s = root.display();
    let rag_blurb = if rag_enabled {
        ", `retrieve_context` (RAG over indexed workspace via HTTP)"
    } else {
        ""
    };
    let git_blurb = ", `git_diff`, `git_log` (read-only git context)";
    let base = match mode {
        PermissionMode::ReadOnly => format!(
            "You are a read-only coding assistant. Workspace root: {root_s}. \
             Tools: `read_file`, `list_dir`, `glob_workspace`, `grep_workspace` / `grep_search` (literal substring){git_blurb}{rag_blurb}. Paths relative; use `/`; no `..`."
        ),
        PermissionMode::WorkspaceWrite => format!(
            "You are a coding assistant with read/list/glob/grep/write{git_blurb}{rag_blurb}. Workspace root: {root_s}. \
             Relative paths only; no `..`."
        ),
        PermissionMode::Prompt => format!(
            "You are a coding assistant in prompt-style permission mode (workspace root: {root_s}). \
             Read/list/glob/grep{git_blurb}{rag_blurb} tools available; `write_file` is gated — in this harness writes require workspace-write or higher unless an interactive prompt is available (non-interactive runs deny writes per PolicyEnforcer)."
        ),
        PermissionMode::DangerFullAccess | PermissionMode::Allow => format!(
            "You are a coding assistant with read/list/glob/grep/write{git_blurb}{rag_blurb} and expanded permission mode '{}' (workspace root: {root_s}). \
             Still use only the provided tools; paths must stay under workspace.",
            mode.as_str()
        ),
    };
    let mut out = base;
    out.push('\n');
    out.push_str(SOURCE_GROUNDING_HINT);
    if let Some(x) = preset.extra_system() {
        out.push('\n');
        out.push_str(x);
    }
    if let Some(h) = profile_hint.filter(|s| !s.is_empty()) {
        out.push('\n');
        out.push_str("Learner hint: ");
        out.push_str(h);
    }
    if let Some(h) = language_system_hint(language) {
        out.push('\n');
        out.push_str(h);
    }
    out
}

/// Print effective tool names and policy summary (no network; for `--print-tools` dry-run).
pub fn print_tools_dry_run(
    permission_mode: PermissionMode,
    use_runtime_enforcer: bool,
    rag_base_url: Option<&str>,
    out: &mut impl io::Write,
) -> std::io::Result<()> {
    let tools = tool_definitions(permission_mode, rag_base_url);
    writeln!(out, "claw-analog — effective tools (dry-run, no API calls)")?;
    writeln!(
        out,
        "permission_mode: {}   runtime::PermissionEnforcer: {}",
        permission_mode.as_str(),
        if use_runtime_enforcer { "on" } else { "off" }
    )?;
    writeln!(out, "\nTools:")?;
    for t in tools {
        let desc = t.description.as_deref().unwrap_or("—");
        writeln!(out, "  - {} — {desc}", t.name)?;
    }
    Ok(())
}

#[derive(Debug)]
enum BlockKind {
    Text,
    Tool {
        id: String,
        name: String,
        json: String,
    },
}

pub(crate) fn format_rag_query_json_for_model(body: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let phase = v.get("phase").and_then(|x| x.as_str()).unwrap_or("unknown");
    let hits = v
        .get("hits")
        .and_then(|h| h.as_array())
        .ok_or_else(|| "missing hits array".to_string())?;
    let mut out = String::new();
    writeln!(&mut out, "phase: {phase}").map_err(|e| e.to_string())?;
    if hits.is_empty() {
        writeln!(&mut out, "(no hits)").map_err(|e| e.to_string())?;
        return Ok(out);
    }
    for (i, h) in hits.iter().enumerate() {
        let path = h.get("path").and_then(|x| x.as_str()).unwrap_or("");
        let snippet = h.get("snippet").and_then(|x| x.as_str()).unwrap_or("");
        let score = h.get("score").and_then(|x| x.as_f64());
        write!(&mut out, "{}. ", i + 1).map_err(|e| e.to_string())?;
        if let Some(s) = score {
            write!(&mut out, "score={s:.4} ").map_err(|e| e.to_string())?;
        }
        writeln!(&mut out, "path={path}").map_err(|e| e.to_string())?;
        let lines: Vec<&str> = snippet.lines().collect();
        for line in lines.iter().take(32) {
            writeln!(&mut out, "    {line}").map_err(|e| e.to_string())?;
        }
        if lines.len() > 32 {
            writeln!(&mut out, "    …").map_err(|e| e.to_string())?;
        }
        writeln!(&mut out).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

async fn retrieve_context_tool(
    http: &reqwest::Client,
    rag_base_url: &str,
    top_k_cap: u32,
    enforcer: Option<&PermissionEnforcer>,
    input: &Value,
) -> String {
    if let Err(e) = enforce_tool(enforcer, "retrieve_context", input) {
        return format!("error: permission denied: {e}");
    }
    let Some(q) = input.get("query").and_then(|v| v.as_str()) else {
        return "error: missing query".to_string();
    };
    let q = q.trim();
    if q.is_empty() {
        return "error: empty query".to_string();
    }
    if q.chars().count() > RAG_QUERY_MAX_CHARS {
        return format!("error: query too long (max {RAG_QUERY_MAX_CHARS} chars)");
    }
    let cap = top_k_cap.max(1);
    let top_k = input
        .get("top_k")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(8)
        .clamp(1, cap);
    let base = rag_base_url.trim_end_matches('/');
    let url = format!("{base}/v1/query");
    let body = json!({ "query": q, "top_k": top_k });
    let resp = match http.post(url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => return format!("error: RAG request failed: {e}"),
    };
    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return format!("error: RAG response body: {e}"),
    };
    if !status.is_success() {
        return format!("error: RAG HTTP {status}: {text}");
    }
    match format_rag_query_json_for_model(&text) {
        Ok(s) => s,
        Err(e) => format!("error: {e}\nraw: {text}"),
    }
}

/// Run the agent loop; assistant text is written to `out` (streaming deltas when `use_stream`).
pub async fn run(
    config: AnalogConfig,
    out: &mut impl std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workspace = config.workspace.canonicalize()?;
    enforce_non_interactive_permission_rules(
        config.permission_mode,
        config.accept_danger_non_interactive,
    )?;
    let model = resolve_model_alias(&config.model);
    let client = ProviderClient::from_model(model.as_str())?;
    let rag_enabled = config.rag_base_url.is_some();
    let rag_http = if rag_enabled {
        Some(
            reqwest::Client::builder()
                .timeout(config.rag_http_timeout)
                .build()?,
        )
    } else {
        None
    };
    let tools = tool_definitions(config.permission_mode, config.rag_base_url.as_deref());
    let profile_ref = config.profile_hint.as_deref();
    let system = system_prompt(
        config.permission_mode,
        &workspace,
        config.preset,
        profile_ref,
        config.language,
        rag_enabled,
    );
    let ws_str = workspace.display().to_string();

    if config.session_path.is_some() || config.session_save_path.is_some() {
        session_warn_common();
    }

    let mut messages = if let Some(ref sp) = config.session_path {
        session_bootstrap_messages(sp, &ws_str, model.as_str(), config.preset, &config.prompt)?
    } else {
        vec![InputMessage::user_text(config.prompt.clone())]
    };
    let max_tokens = max_tokens_for_model(model.as_str());
    let policy = build_policy(config.permission_mode);
    let enforcer = PermissionEnforcer::new(policy);

    if config.output_format == OutputFormat::Json {
        write_json_line(
            out,
            &json!({
                "type": "run_start",
                "schema": NDJSON_SCHEMA,
                "format_version": NDJSON_FORMAT_VERSION,
                "workspace": ws_str.clone(),
                "model": model.clone(),
                "stream": config.use_stream,
                "permission": config.permission_mode.as_str(),
                "preset": config.preset.label(),
                "session": config.session_path.as_ref().map(|p| p.display().to_string()),
                "session_save": config.session_save_path.as_ref().map(|p| p.display().to_string()),
                "rag_enabled": rag_enabled,
            }),
        )?;
    }

    for turn in 0..config.max_turns {
        if config.output_format == OutputFormat::Json {
            write_json_line(out, &json!({ "type": "turn_start", "turn": turn + 1 }))?;
        }

        let request = MessageRequest {
            model: model.clone(),
            max_tokens,
            messages: messages.clone(),
            system: Some(system.clone()),
            tools: Some(tools.clone()),
            tool_choice: Some(ToolChoice::Auto),
            ..Default::default()
        };

        let response = if config.use_stream {
            stream_to_message_response(&client, &request, out, config.output_format).await?
        } else {
            let r = client.send_message(&request).await?;
            if config.output_format == OutputFormat::Rich {
                for block in &r.content {
                    if let OutputContentBlock::Text { text } = block {
                        write!(out, "{text}")?;
                    }
                }
            }
            r
        };

        eprintln!(
            "[claw-analog] turn {} stop_reason={:?} tokens≈{}",
            turn + 1,
            response.stop_reason,
            response.total_tokens(),
        );

        if config.output_format == OutputFormat::Json {
            let text_full = assistant_plain_text(&response.content);
            write_json_line(
                out,
                &json!({
                    "type": "assistant_turn",
                    "turn": turn + 1,
                    "stop_reason": response.stop_reason,
                    "usage": {
                        "input_tokens": response.usage.input_tokens,
                        "output_tokens": response.usage.output_tokens,
                        "cache_creation_input_tokens": response.usage.cache_creation_input_tokens,
                        "cache_read_input_tokens": response.usage.cache_read_input_tokens,
                        "total_tokens": response.total_tokens(),
                    },
                    "text": text_full,
                    "tool_calls": tool_calls_for_json(&response.content),
                    "request_id": response.request_id,
                }),
            )?;
        }

        messages.push(InputMessage {
            role: "assistant".to_string(),
            content: output_to_input_blocks(&response.content),
        });

        let tool_uses = collect_tool_uses(&response.content);
        if tool_uses.is_empty() || response.stop_reason.as_deref() != Some("tool_use") {
            persist_conversation_sessions(&config, &ws_str, model.as_str(), &messages)?;
            break;
        }

        let mut results: Vec<InputContentBlock> = Vec::new();
        for tu in tool_uses {
            let text = if tu.name == "retrieve_context" {
                match (&rag_http, &config.rag_base_url) {
                    (Some(http), Some(base)) => {
                        retrieve_context_tool(
                            http,
                            base,
                            config.rag_top_k_max,
                            config.use_runtime_enforcer.then_some(&enforcer),
                            tu.input,
                        )
                        .await
                    }
                    _ => "error: retrieve_context is not configured (set RAG_BASE_URL or rag_base_url in .claw-analog.toml)".to_string(),
                }
            } else {
                dispatch_tool(
                    tu.name,
                    tu.input,
                    &workspace,
                    &ws_str,
                    config.permission_mode,
                    config.use_runtime_enforcer.then_some(&enforcer),
                    config.max_read_bytes,
                    config.max_list_entries,
                    config.grep_max_lines,
                    config.glob_max_paths,
                    config.glob_max_depth,
                )
            };
            let is_err = text.starts_with("error:");
            eprintln!(
                "[claw-analog] tool {} -> {} chars is_error={}",
                tu.name,
                text.len(),
                is_err,
            );
            if config.output_format == OutputFormat::Json {
                let (output, truncated) = truncate_for_json(&text);
                write_json_line(
                    out,
                    &json!({
                        "type": "tool_result",
                        "turn": turn + 1,
                        "tool_use_id": tu.id,
                        "name": tu.name,
                        "is_error": is_err,
                        "output": output,
                        "output_len_chars": text.chars().count(),
                        "truncated": truncated,
                    }),
                )?;
            }
            results.push(InputContentBlock::ToolResult {
                tool_use_id: tu.id.to_string(),
                content: vec![ToolResultContentBlock::Text { text }],
                is_error: is_err,
            });
        }
        messages.push(InputMessage {
            role: "user".to_string(),
            content: results,
        });
        persist_conversation_sessions(&config, &ws_str, model.as_str(), &messages)?;
    }

    if config.output_format == OutputFormat::Json {
        write_json_line(out, &json!({ "type": "run_end", "ok": true }))?;
    }

    Ok(())
}

async fn stream_to_message_response(
    client: &ProviderClient,
    request: &MessageRequest,
    out: &mut impl std::io::Write,
    output_format: OutputFormat,
) -> Result<MessageResponse, ApiError> {
    let mut stream = client.stream_message(request).await?;
    let mut block_kind: BTreeMap<u32, BlockKind> = BTreeMap::new();
    let mut text_buf: BTreeMap<u32, String> = BTreeMap::new();
    let mut message_id = String::from("stream");
    let mut message_model = request.model.clone();
    let mut stop_reason: Option<String> = None;
    let mut usage = api::Usage::default();
    let mut saw_stop = false;
    let mut finished: BTreeMap<u32, OutputContentBlock> = BTreeMap::new();

    while let Some(event) = stream.next_event().await? {
        match event {
            StreamEvent::MessageStart(MessageStartEvent { message }) => {
                message_id = message.id;
                message_model = message.model;
                for block in message.content {
                    if let OutputContentBlock::Text { text } = block {
                        if text.is_empty() {
                            continue;
                        }
                        match output_format {
                            OutputFormat::Rich => {
                                write!(out, "{text}").ok();
                            }
                            OutputFormat::Json => {
                                write_json_line(
                                    out,
                                    &json!({ "type": "assistant_text_delta", "text": text }),
                                )
                                .map_err(ApiError::from)?;
                            }
                        }
                    }
                }
            }
            StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                index,
                content_block,
            }) => match content_block {
                OutputContentBlock::Text { text } => {
                    block_kind.insert(index, BlockKind::Text);
                    text_buf.insert(index, text);
                }
                OutputContentBlock::ToolUse { id, name, input } => {
                    let json = if input.as_object().is_some_and(|m| m.is_empty()) {
                        String::new()
                    } else {
                        input.to_string()
                    };
                    block_kind.insert(index, BlockKind::Tool { id, name, json });
                }
                OutputContentBlock::Thinking { .. }
                | OutputContentBlock::RedactedThinking { .. } => {}
            },
            StreamEvent::ContentBlockDelta(delta) => match delta.delta {
                ContentBlockDelta::TextDelta { text } => {
                    if !text.is_empty() {
                        match output_format {
                            OutputFormat::Rich => {
                                write!(out, "{text}").ok();
                            }
                            OutputFormat::Json => {
                                write_json_line(
                                    out,
                                    &json!({ "type": "assistant_text_delta", "text": text }),
                                )
                                .map_err(ApiError::from)?;
                            }
                        }
                        text_buf.entry(delta.index).or_default().push_str(&text);
                    }
                }
                ContentBlockDelta::InputJsonDelta { partial_json } => {
                    if let Some(BlockKind::Tool { json, .. }) = block_kind.get_mut(&delta.index) {
                        json.push_str(&partial_json);
                    }
                }
                ContentBlockDelta::ThinkingDelta { .. }
                | ContentBlockDelta::SignatureDelta { .. } => {}
            },
            StreamEvent::ContentBlockStop(stop) => {
                let idx = stop.index;
                match block_kind.remove(&idx) {
                    Some(BlockKind::Text) => {
                        let t = text_buf.remove(&idx).unwrap_or_default();
                        if !t.is_empty() {
                            finished.insert(idx, OutputContentBlock::Text { text: t });
                        }
                    }
                    Some(BlockKind::Tool { id, name, json }) => {
                        let input = serde_json::from_str::<Value>(&json)
                            .unwrap_or_else(|_| json!({ "raw": json }));
                        finished.insert(idx, OutputContentBlock::ToolUse { id, name, input });
                    }
                    None => {}
                }
            }
            StreamEvent::MessageDelta(MessageDeltaEvent { delta, usage: u }) => {
                usage = u;
                stop_reason = delta.stop_reason.or(stop_reason);
            }
            StreamEvent::MessageStop(MessageStopEvent {}) => {
                saw_stop = true;
                break;
            }
        }
    }

    if !saw_stop {
        return client.send_message(request).await;
    }

    let content: Vec<OutputContentBlock> = finished.into_values().collect();
    if content.is_empty() {
        return client.send_message(request).await;
    }
    let has_tools = content
        .iter()
        .any(|b| matches!(b, OutputContentBlock::ToolUse { .. }));
    let stop_reason = stop_reason.or_else(|| {
        Some(if has_tools {
            "tool_use".to_string()
        } else {
            "end_turn".to_string()
        })
    });

    Ok(MessageResponse {
        id: message_id,
        kind: "message".to_string(),
        role: "assistant".to_string(),
        content,
        model: message_model,
        stop_reason,
        stop_sequence: None,
        usage,
        request_id: stream.request_id().map(ToString::to_string),
    })
}

struct ToolUse<'a> {
    id: &'a str,
    name: &'a str,
    input: &'a Value,
}

fn collect_tool_uses(content: &[OutputContentBlock]) -> Vec<ToolUse<'_>> {
    content
        .iter()
        .filter_map(|b| {
            if let OutputContentBlock::ToolUse { id, name, input } = b {
                Some(ToolUse {
                    id: id.as_str(),
                    name: name.as_str(),
                    input,
                })
            } else {
                None
            }
        })
        .collect()
}

fn output_to_input_blocks(blocks: &[OutputContentBlock]) -> Vec<InputContentBlock> {
    blocks
        .iter()
        .filter_map(|b| match b {
            OutputContentBlock::Text { text } => {
                Some(InputContentBlock::Text { text: text.clone() })
            }
            OutputContentBlock::ToolUse { id, name, input } => Some(InputContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            OutputContentBlock::Thinking { .. } | OutputContentBlock::RedactedThinking { .. } => {
                None
            }
        })
        .collect()
}

pub fn validate_rel_path(rel: &str) -> Result<(), String> {
    let p = Path::new(rel);
    for c in p.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err("path must be relative with no '..' or absolute segments".into());
            }
        }
    }
    Ok(())
}

fn join_under_root(root: &Path, rel: &str) -> Result<PathBuf, String> {
    validate_rel_path(rel)?;
    Ok(root.join(rel))
}

fn assert_workspace_path(root: &Path, path: &Path) -> Result<(), String> {
    let root_canon = root.canonicalize().map_err(|e| e.to_string())?;

    if path.exists() {
        let c = path.canonicalize().map_err(|e| e.to_string())?;
        return if c.starts_with(&root_canon) {
            Ok(())
        } else {
            Err("path escapes workspace".into())
        };
    }

    if let Some(parent) = path.parent() {
        if parent.as_os_str().is_empty() {
            return Ok(());
        }
        let mut cur = parent;
        loop {
            if cur == root {
                break;
            }
            if cur.exists() {
                let pc = cur.canonicalize().map_err(|e| e.to_string())?;
                if !pc.starts_with(&root_canon) {
                    return Err("path escapes workspace".into());
                }
                break;
            }
            cur = cur.parent().ok_or_else(|| "invalid path".to_string())?;
        }
    }
    Ok(())
}

fn enforce_tool(
    enforcer: Option<&PermissionEnforcer>,
    tool: &str,
    input: &Value,
) -> Result<(), String> {
    let Some(e) = enforcer else {
        return Ok(());
    };
    let payload = input.to_string();
    match e.check(tool, &payload) {
        EnforcementResult::Allowed => Ok(()),
        EnforcementResult::Denied { reason, .. } => Err(reason),
    }
}

fn assert_safe_glob_pattern(pattern: &str) -> Result<(), String> {
    if pattern.contains("..") {
        return Err("glob pattern must not contain '..'".into());
    }
    Ok(())
}

/// Returns workspace-relative paths using `/`, sorted; `truncated` if `max_paths` reached.
pub fn glob_workspace_collect(
    workspace: &Path,
    rel_root: &str,
    glob_pat: &str,
    max_depth: usize,
    max_paths: usize,
) -> Result<(Vec<String>, bool), String> {
    assert_safe_glob_pattern(glob_pat)?;
    if max_paths == 0 {
        return Ok((Vec::new(), false));
    }
    let root_path = join_under_root(workspace, rel_root)?;
    assert_workspace_path(workspace, &root_path)?;
    let g = Glob::new(glob_pat).map_err(|e| e.to_string())?;
    let mut b = GlobSetBuilder::new();
    b.add(g);
    let set: GlobSet = b.build().map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    let mut truncated = false;
    let depth = max_depth.max(1);
    let mut walker = WalkBuilder::new(&root_path);
    walker
        .follow_links(false)
        .max_depth(Some(depth))
        .git_ignore(true)
        .git_exclude(true)
        .ignore(true)
        .hidden(false)
        .add_custom_ignore_filename(".clawignore");
    for result in walker.build() {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let full = entry.path();
        let rel_search = full
            .strip_prefix(&root_path)
            .map_err(|_| "internal path prefix".to_string())?;
        let rel_s = rel_search.to_string_lossy().replace('\\', "/");
        if rel_s.is_empty() {
            continue;
        }
        if set.is_match(rel_s.as_str()) {
            let ws_rel = full
                .strip_prefix(workspace)
                .map_err(|_| "internal workspace prefix".to_string())?;
            let line = ws_rel.to_string_lossy().replace('\\', "/");
            out.push(line);
            if out.len() >= max_paths {
                truncated = true;
                break;
            }
        }
    }
    out.sort();
    out.dedup();
    Ok((out, truncated))
}

/// Literal substring per line; capped lines; no regex/shell.
pub fn grep_in_file(
    path: &Path,
    pattern: &str,
    max_file_bytes: u64,
    max_matching_lines: usize,
) -> Result<String, String> {
    let cap = max_matching_lines.max(1);
    let (s, _) = grep_in_file_labeled(path, pattern, max_file_bytes, cap, None)?;
    Ok(s)
}

fn grep_in_file_labeled(
    path: &Path,
    pattern: &str,
    max_file_bytes: u64,
    max_matching_lines: usize,
    path_label: Option<&str>,
) -> Result<(String, usize), String> {
    if max_matching_lines == 0 {
        return Ok((String::new(), 0));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.iter().take(8 * 1024).any(|b| *b == 0) {
        return Err("file looks binary (NUL byte)".into());
    }
    if bytes.len() as u64 > max_file_bytes {
        return Err(format!(
            "file too large ({} bytes; max {})",
            bytes.len(),
            max_file_bytes
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| "invalid UTF-8".to_string())?;
    let mut out = String::new();
    let mut count = 0usize;
    let cap = max_matching_lines;
    for (lineno, line) in text.lines().enumerate() {
        if line.contains(pattern) {
            count += 1;
            if out.len() < 256 * 1024 {
                match path_label {
                    Some(label) => {
                        let _ = writeln!(&mut out, "{label}:{}:{}", lineno + 1, line);
                    }
                    None => {
                        let _ = writeln!(&mut out, "{}:{}", lineno + 1, line);
                    }
                }
            }
            if count >= cap {
                let _ = writeln!(&mut out, "… truncated after {cap} matching lines");
                break;
            }
        }
    }
    if out.is_empty() {
        if path_label.is_none() {
            Ok(("(no matches)".into(), 0))
        } else {
            Ok((String::new(), 0))
        }
    } else {
        Ok((out, count))
    }
}

fn dispatch_grep_workspace(
    input: &Value,
    workspace: &Path,
    max_read: u64,
    grep_cap: usize,
    glob_max_paths: usize,
    glob_max_depth: usize,
) -> String {
    let Some(pattern) = input.get("pattern").and_then(|p| p.as_str()) else {
        return "error: missing pattern".to_string();
    };
    if pattern.is_empty() {
        return "error: empty pattern".to_string();
    }
    let max_lines_total = input
        .get("max_lines")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(grep_cap)
        .min(grep_cap.max(1));

    let path_single = input
        .get("path")
        .and_then(|p| p.as_str())
        .filter(|s| !s.is_empty());
    let paths_arr = input.get("paths").and_then(|p| p.as_array());
    let glob = input
        .get("glob")
        .and_then(|p| p.as_str())
        .filter(|s| !s.is_empty());

    let mut selector_count = 0u8;
    if path_single.is_some() {
        selector_count += 1;
    }
    if paths_arr.is_some_and(|a| !a.is_empty()) {
        selector_count += 1;
    }
    if glob.is_some() {
        selector_count += 1;
    }
    if selector_count > 1 {
        return "error: specify only one of path, paths, or glob".to_string();
    }
    if selector_count == 0 {
        return "error: provide path, paths, or glob".to_string();
    }

    let mut files: Vec<String> = Vec::new();
    if let Some(g) = glob {
        let glob_root = input
            .get("glob_root")
            .and_then(|p| p.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(".");
        match glob_workspace_collect(workspace, glob_root, g, glob_max_depth, glob_max_paths) {
            Ok((mut v, _)) => {
                if v.is_empty() {
                    return "(no matches)".into();
                }
                files.append(&mut v);
            }
            Err(e) => return format!("error: {e}"),
        }
    } else if let Some(arr) = paths_arr {
        files.reserve(arr.len());
        for p in arr {
            let Some(s) = p.as_str() else {
                return "error: paths must be strings".to_string();
            };
            if !s.is_empty() {
                files.push(s.to_string());
            }
        }
        if files.is_empty() {
            return "error: paths is empty".to_string();
        }
    } else if let Some(p) = path_single {
        files.push(p.to_string());
    }

    files.sort();
    files.dedup();

    let multi = files.len() > 1;
    let mut combined = String::new();
    let mut total_matches = 0usize;

    for rel in files {
        if total_matches >= max_lines_total {
            break;
        }
        let remaining = max_lines_total.saturating_sub(total_matches);
        if remaining == 0 {
            break;
        }
        let Ok(full) = join_under_root(workspace, &rel) else {
            return format!("error: invalid path {rel:?}");
        };
        if let Err(e) = assert_workspace_path(workspace, &full) {
            return format!("error: {e}");
        }
        let label = if multi { Some(rel.as_str()) } else { None };
        match grep_in_file_labeled(&full, pattern, max_read, remaining, label) {
            Ok((chunk, n)) => {
                if multi {
                    if n > 0 {
                        combined.push_str(&chunk);
                        total_matches += n;
                    }
                } else {
                    return chunk;
                }
            }
            Err(e) => return format!("error: {e}"),
        }
    }

    if combined.is_empty() {
        "(no matches)".into()
    } else {
        combined
    }
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch_tool(
    name: &str,
    input: &Value,
    workspace: &Path,
    workspace_str: &str,
    mode: PermissionMode,
    enforcer: Option<&PermissionEnforcer>,
    max_read: u64,
    max_list: usize,
    grep_cap: usize,
    glob_max_paths: usize,
    glob_max_depth: usize,
) -> String {
    match name {
        "read_file" => {
            if let Err(e) = enforce_tool(enforcer, name, input) {
                return format!("error: permission denied: {e}");
            }
            let Some(path_s) = input.get("path").and_then(|p| p.as_str()) else {
                return "error: missing path".to_string();
            };
            let Ok(full) = join_under_root(workspace, path_s) else {
                return format!("error: invalid path {path_s:?}");
            };
            if let Err(e) = assert_workspace_path(workspace, &full) {
                return format!("error: {e}");
            }
            match std::fs::read(&full) {
                Ok(bytes) => {
                    if bytes.iter().take(8 * 1024).any(|b| *b == 0) {
                        return "error: file looks binary (NUL byte)".to_string();
                    }
                    if bytes.len() as u64 > max_read {
                        return format!(
                            "error: file too large ({} bytes; max {})",
                            bytes.len(),
                            max_read
                        );
                    }
                    String::from_utf8_lossy(&bytes).into_owned()
                }
                Err(e) => format!("error: read failed: {e}"),
            }
        }
        "list_dir" => {
            if let Err(e) = enforce_tool(enforcer, "list_dir", input) {
                return format!("error: permission denied: {e}");
            }
            let path_s = input
                .get("path")
                .and_then(|p| p.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(".");
            let Ok(full) = join_under_root(workspace, path_s) else {
                return format!("error: invalid path {path_s:?}");
            };
            if let Err(e) = assert_workspace_path(workspace, &full) {
                return format!("error: {e}");
            }
            // Use ignore-aware walker to respect .gitignore/.clawignore.
            let mut walker = WalkBuilder::new(&full);
            walker
                .follow_links(false)
                .max_depth(Some(1))
                .git_ignore(true)
                .git_exclude(true)
                .ignore(true)
                .hidden(false)
                .add_custom_ignore_filename(".clawignore");
            let mut names: Vec<String> = walker
                .build()
                .filter_map(|r| r.ok())
                .filter_map(|e| {
                    let p = e.path();
                    if p == full {
                        return None;
                    }
                    p.file_name().map(|n| n.to_string_lossy().into_owned())
                })
                .take(max_list.saturating_add(1))
                .collect();
            names.sort();
            names.dedup();
            let truncated = names.len() > max_list;
            names.truncate(max_list);
            let body = names.join("\n");
            if truncated {
                format!("{body}\n… truncated to {max_list} entries")
            } else {
                body
            }
        }
        "glob_workspace" => {
            if let Err(e) = enforce_tool(enforcer, name, input) {
                return format!("error: permission denied: {e}");
            }
            let root = input
                .get("root")
                .and_then(|r| r.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(".");
            let Some(pat) = input.get("pattern").and_then(|p| p.as_str()) else {
                return "error: missing pattern".to_string();
            };
            if pat.is_empty() {
                return "error: empty pattern".to_string();
            }
            let cap = input
                .get("max_paths")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .map(|n| n.min(glob_max_paths))
                .unwrap_or(glob_max_paths);
            match glob_workspace_collect(workspace, root, pat, glob_max_depth, cap) {
                Ok((paths, truncated)) => {
                    if paths.is_empty() {
                        "(no matches)".into()
                    } else {
                        let body = paths.join("\n");
                        if truncated {
                            format!("{body}\n… truncated (max_paths={cap})")
                        } else {
                            body
                        }
                    }
                }
                Err(e) => format!("error: {e}"),
            }
        }
        "grep_workspace" => {
            if let Err(e) = enforce_tool(enforcer, name, input) {
                return format!("error: permission denied: {e}");
            }
            dispatch_grep_workspace(
                input,
                workspace,
                max_read,
                grep_cap,
                glob_max_paths,
                glob_max_depth,
            )
        }
        "grep_search" => {
            if let Err(e) = enforce_tool(enforcer, name, input) {
                return format!("error: permission denied: {e}");
            }
            dispatch_grep_workspace(
                input,
                workspace,
                max_read,
                grep_cap,
                glob_max_paths,
                glob_max_depth,
            )
        }
        "retrieve_context" => {
            "error: retrieve_context runs via async HTTP only (configure RAG_BASE_URL)".to_string()
        }
        "write_file" => {
            if !matches!(
                mode,
                PermissionMode::WorkspaceWrite
                    | PermissionMode::DangerFullAccess
                    | PermissionMode::Allow
            ) {
                return format!(
                    "error: write_file requires workspace-write, danger-full-access, or allow (current: {})",
                    mode.as_str()
                );
            }
            if let Err(e) = enforce_tool(enforcer, name, input) {
                return format!("error: permission denied: {e}");
            }
            let Some(path_s) = input.get("path").and_then(|p| p.as_str()) else {
                return "error: missing path".to_string();
            };
            let Some(content) = input.get("content").and_then(|p| p.as_str()) else {
                return "error: missing content".to_string();
            };
            let Ok(full) = join_under_root(workspace, path_s) else {
                return format!("error: invalid path {path_s:?}");
            };
            if let Err(e) = assert_workspace_path(workspace, &full) {
                return format!("error: {e}");
            }
            if let Some(e) = enforcer {
                match e.check_file_write(&full.display().to_string(), workspace_str) {
                    EnforcementResult::Allowed => {}
                    EnforcementResult::Denied { reason, .. } => {
                        return format!("error: permission denied: {reason}");
                    }
                }
            }
            if let Some(parent) = full.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return format!("error: mkdir: {e}");
                }
            }
            match std::fs::write(&full, content.as_bytes()) {
                Ok(()) => format!("wrote {} bytes to {}", content.len(), full.display()),
                Err(e) => format!("error: write failed: {e}"),
            }
        }
        "git_diff" => {
            if let Err(e) = enforce_tool(enforcer, name, input) {
                return format!("error: permission denied: {e}");
            }
            let cached = input
                .get("cached")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let context_lines = input.get("context_lines").and_then(|v| v.as_i64());
            let rev_range = input
                .get("rev_range")
                .and_then(|v| v.as_str())
                .map(str::trim);
            if let Some(rr) = rev_range {
                if !is_safe_git_rev_range(rr) {
                    return "error: invalid rev_range".to_string();
                }
            }

            let mut args: Vec<String> = vec![
                "diff".to_string(),
                "--no-color".to_string(),
                "--no-ext-diff".to_string(),
            ];
            if let Some(n) = context_lines {
                let n = n.clamp(0, 100);
                args.push(format!("-U{n}"));
            }
            if cached {
                args.push("--cached".to_string());
            }
            if let Some(rr) = rev_range {
                if !rr.is_empty() {
                    args.push(rr.to_string());
                }
            }
            if let Some(arr) = input.get("paths").and_then(|v| v.as_array()) {
                let mut paths: Vec<String> = Vec::new();
                for p in arr.iter().filter_map(|v| v.as_str()) {
                    if validate_rel_path(p).is_err() {
                        return format!("error: invalid path {p:?}");
                    }
                    paths.push(p.replace('\\', "/"));
                }
                if !paths.is_empty() {
                    args.push("--".to_string());
                    args.extend(paths);
                }
            }
            match run_git_capped(workspace, &args, max_read as usize) {
                Ok(s) => {
                    if s.trim().is_empty() {
                        "(no diff)".to_string()
                    } else {
                        s
                    }
                }
                Err(e) => format!("error: {e}"),
            }
        }
        "git_log" => {
            if let Err(e) = enforce_tool(enforcer, name, input) {
                return format!("error: permission denied: {e}");
            }
            let max_count = input
                .get("max_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .min(50);
            let rev_range = input
                .get("rev_range")
                .and_then(|v| v.as_str())
                .map(str::trim);
            if let Some(rr) = rev_range {
                if !is_safe_git_rev_range(rr) {
                    return "error: invalid rev_range".to_string();
                }
            }
            let mut args: Vec<String> = vec![
                "log".to_string(),
                "--no-color".to_string(),
                "--no-decorate".to_string(),
                format!("--max-count={max_count}"),
                "--pretty=format:%h %s".to_string(),
            ];
            if let Some(rr) = rev_range {
                if !rr.is_empty() {
                    args.push(rr.to_string());
                }
            }
            if let Some(arr) = input.get("paths").and_then(|v| v.as_array()) {
                let mut paths: Vec<String> = Vec::new();
                for p in arr.iter().filter_map(|v| v.as_str()) {
                    if validate_rel_path(p).is_err() {
                        return format!("error: invalid path {p:?}");
                    }
                    paths.push(p.replace('\\', "/"));
                }
                if !paths.is_empty() {
                    args.push("--".to_string());
                    args.extend(paths);
                }
            }
            match run_git_capped(workspace, &args, max_read as usize) {
                Ok(s) => {
                    if s.trim().is_empty() {
                        "(no commits)".to_string()
                    } else {
                        s
                    }
                }
                Err(e) => format!("error: {e}"),
            }
        }
        _ => {
            format!("error: unknown tool {name} (input {input})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn mock_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    async fn mock_env_lock_async() -> tokio::sync::MutexGuard<'static, ()> {
        use tokio::sync::Mutex as AsyncMutex;
        static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| AsyncMutex::new(())).lock().await
    }

    fn git(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git should run");
        if !out.status.success() {
            panic!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn validate_rel_rejects_dotdot() {
        assert!(validate_rel_path("..\\x").is_err());
        assert!(validate_rel_path("a/../../b").is_err());
        assert!(validate_rel_path("src/main.rs").is_ok());
    }

    #[test]
    fn resolve_analog_options_preset_implement_default_write() {
        let file = AnalogFileConfig {
            preset: Some("implement".into()),
            ..Default::default()
        };
        let r = resolve_analog_options(&file, &AnalogDoctorOverrides::default());
        assert_eq!(r.permission_mode, PermissionMode::WorkspaceWrite);
        assert!(r.provenance.iter().any(|s| s.contains("implement")));
    }

    #[test]
    fn resolve_analog_options_cli_beats_toml() {
        let file = AnalogFileConfig {
            model: Some("from-file".into()),
            ..Default::default()
        };
        let o = AnalogDoctorOverrides {
            model: Some("from-cli".into()),
            ..Default::default()
        };
        let r = resolve_analog_options(&file, &o);
        assert_eq!(r.model, "from-cli");
        assert!(r.provenance[0].contains("CLI"));
    }

    #[test]
    fn grep_finds_lines() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("t.txt");
        std::fs::write(&f, "alpha\nbeta parity\ngamma\nparity tail\n").unwrap();
        let s = grep_in_file(&f, "parity", 4096, 10).unwrap();
        assert!(s.contains("2:"));
        assert!(s.contains("4:"));
    }

    #[test]
    fn glob_workspace_respects_cap_and_depth() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::write(root.join("src/nested/foo.rs"), "").unwrap();
        std::fs::write(root.join("src/bar.txt"), "").unwrap();
        let (paths, trunc) = glob_workspace_collect(&root, ".", "**/*.rs", 32, 500).expect("glob");
        assert!(!trunc);
        assert!(paths.iter().any(|p| p.ends_with("foo.rs")));
        let (few, trunc2) = glob_workspace_collect(&root, ".", "**/*", 32, 1).expect("glob2");
        assert!(trunc2);
        assert_eq!(few.len(), 1);
    }

    #[test]
    fn glob_workspace_respects_gitignore_and_clawignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        // The ignore walker enables gitignore semantics more consistently when a repo root is present.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        std::fs::write(root.join(".clawignore"), "ignored_dir/\n").unwrap();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/kept.rs"), "").unwrap();

        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("node_modules/ignored.rs"), "").unwrap();

        std::fs::create_dir_all(root.join("ignored_dir")).unwrap();
        std::fs::write(root.join("ignored_dir/also_ignored.rs"), "").unwrap();

        let (paths, trunc) = glob_workspace_collect(&root, ".", "**/*.rs", 32, 500).expect("glob");
        assert!(!trunc);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("src/kept.rs"));
    }

    #[test]
    fn grep_paths_and_glob_and_grep_search_alias() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), "one xhere\n").unwrap();
        std::fs::write(root.join("b.txt"), "two xhere\n").unwrap();

        let out = dispatch_grep_workspace(
            &json!({ "paths": ["a.txt", "b.txt"], "pattern": "xhere" }),
            &root,
            4096,
            50,
            100,
            16,
        );
        assert!(out.contains("a.txt:"));
        assert!(out.contains("b.txt:"));

        let out_g = dispatch_grep_workspace(
            &json!({ "glob": "*.txt", "pattern": "xhere" }),
            &root,
            4096,
            50,
            100,
            16,
        );
        assert!(out_g.contains("a.txt:") || out_g.contains("b.txt:"));

        let alias = dispatch_tool(
            "grep_search",
            &json!({ "path": "a.txt", "pattern": "xhere" }),
            &root,
            &root.display().to_string(),
            PermissionMode::ReadOnly,
            None,
            4096,
            100,
            50,
            2000,
            32,
        );
        assert!(alias.contains("1:"));
    }

    #[test]
    fn session_save_and_resume_appends_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.json");
        let ws = dir.path().canonicalize().unwrap();
        let wss = ws.display().to_string();
        let m = "m1";
        session_save(
            &path,
            &wss,
            m,
            Preset::Audit,
            &[InputMessage::user_text("first")],
        )
        .expect("save");
        let msgs =
            session_bootstrap_messages(&path, &wss, m, Preset::Audit, "second").expect("boot");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].role, "user");
        let json = serde_json::to_value(&msgs[1]).expect("ser");
        assert_eq!(json["content"][0]["text"], "second");
    }

    #[test]
    fn profile_line_load_and_cap() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("profile.toml");
        let long = "x".repeat(PROFILE_LINE_MAX_CHARS + 20);
        std::fs::write(&p, format!("line = \"{long}\"\n")).unwrap();
        let h = load_profile_hint(&p).expect("ok");
        assert_eq!(
            h.as_ref().map(|s| s.chars().count()),
            Some(PROFILE_LINE_MAX_CHARS)
        );
    }

    #[test]
    fn system_prompt_includes_preset_and_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let s = system_prompt(
            PermissionMode::ReadOnly,
            &root,
            Preset::Explain,
            Some("keep answers short"),
            AnalogLanguage::En,
            false,
        );
        assert!(s.contains("Preset: explain"));
        assert!(s.contains("Learner hint: keep answers short"));
        assert!(s.contains("deployment manifests"));
        assert!(s.contains("monorepo"));
    }

    #[test]
    fn system_prompt_russian_language_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let s = system_prompt(
            PermissionMode::ReadOnly,
            &root,
            Preset::None,
            None,
            AnalogLanguage::Ru,
            false,
        );
        assert!(s.contains("Язык:"));
    }

    #[test]
    fn system_prompt_rag_lists_retrieve_context() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let s = system_prompt(
            PermissionMode::ReadOnly,
            &root,
            Preset::None,
            None,
            AnalogLanguage::En,
            true,
        );
        assert!(s.contains("retrieve_context"));
    }

    #[test]
    fn enforce_non_interactive_rejects_danger_when_not_tty() {
        assert!(enforce_non_interactive_permission_rules_with_tty(
            PermissionMode::Allow,
            false,
            false
        )
        .is_err());
    }

    #[test]
    fn enforce_non_interactive_accepts_danger_with_flag() {
        assert!(enforce_non_interactive_permission_rules_with_tty(
            PermissionMode::DangerFullAccess,
            true,
            false
        )
        .is_ok());
    }

    #[test]
    fn enforce_non_interactive_accepts_danger_when_tty() {
        assert!(enforce_non_interactive_permission_rules_with_tty(
            PermissionMode::Allow,
            false,
            true
        )
        .is_ok());
    }

    #[test]
    fn print_tools_dry_run_lists_read_only_tools() {
        let mut buf = Vec::new();
        print_tools_dry_run(PermissionMode::ReadOnly, true, None, &mut buf).unwrap();
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains("read_file"));
        assert!(!s.contains("write_file"));
        assert!(!s.contains("retrieve_context"));
        let mut buf2 = Vec::new();
        print_tools_dry_run(PermissionMode::WorkspaceWrite, true, None, &mut buf2).unwrap();
        let s2 = String::from_utf8_lossy(&buf2);
        assert!(s2.contains("write_file"));
        let mut buf3 = Vec::new();
        print_tools_dry_run(
            PermissionMode::ReadOnly,
            true,
            Some("http://127.0.0.1:8787"),
            &mut buf3,
        )
        .unwrap();
        let s3 = String::from_utf8_lossy(&buf3);
        assert!(s3.contains("retrieve_context"));
    }

    #[test]
    fn rag_response_formatting() {
        let out = format_rag_query_json_for_model(
            r#"{"hits":[{"path":"a.rs","snippet":"one\ntwo","score":0.5}],"phase":"1-sqlite"}"#,
        )
        .unwrap();
        assert!(out.contains("phase: 1-sqlite"));
        assert!(out.contains("a.rs"));
        assert!(out.contains("one"));
        assert!(out.contains("score="));
    }

    #[test]
    fn resolve_rag_base_url_toml_beats_env() {
        let _g = mock_env_lock();
        std::env::set_var("RAG_BASE_URL", "http://from-env");
        let file = AnalogFileConfig {
            rag_base_url: Some("http://from-toml".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_rag_base_url(&file).as_deref(),
            Some("http://from-toml")
        );
        std::env::remove_var("RAG_BASE_URL");
    }

    #[test]
    fn infer_preset_from_prompt_prefers_audit_over_others() {
        assert_eq!(
            infer_preset_from_prompt("please do a security review and audit this"),
            Preset::Audit
        );
        assert_eq!(
            infer_preset_from_prompt("Аудит безопасности"),
            Preset::Audit
        );
    }

    #[test]
    fn infer_preset_from_prompt_detects_implement() {
        assert_eq!(
            infer_preset_from_prompt("fix the bug in parser"),
            Preset::Implement
        );
        assert_eq!(infer_preset_from_prompt("добавь фичу"), Preset::Implement);
    }

    #[test]
    fn infer_preset_from_prompt_detects_explain() {
        assert_eq!(
            infer_preset_from_prompt("explain how this works"),
            Preset::Explain
        );
        assert_eq!(
            infer_preset_from_prompt("почему падает? объясни"),
            Preset::Explain
        );
    }

    #[test]
    fn load_analog_toml_parses() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".claw-analog.toml");
        std::fs::write(
            &p,
            r#"
model = "opus"
stream = true
output_format = "json"
permission = "read-only"
language = "ru"
glob_max_paths = 100
"#,
        )
        .unwrap();
        let c = load_analog_toml(&p).expect("toml");
        assert_eq!(c.model.as_deref(), Some("opus"));
        assert_eq!(c.stream, Some(true));
        assert_eq!(c.output_format.as_deref(), Some("json"));
        assert_eq!(c.language.as_deref(), Some("ru"));
        assert_eq!(c.glob_max_paths, Some(100));
    }

    #[test]
    fn git_tools_work_in_temp_repo() {
        let _g = mock_env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        git(root, &["init", "--quiet", "--initial-branch=main"]);
        git(root, &["config", "user.email", "tests@example.com"]);
        git(root, &["config", "user.name", "Claw Analog Tests"]);

        std::fs::write(root.join("a.txt"), "a\n").expect("write a");
        git(root, &["add", "a.txt"]);
        git(root, &["commit", "-m", "initial", "--quiet"]);
        std::fs::write(root.join("a.txt"), "a!\n").expect("modify a");

        let ws_str = root.display().to_string();
        let log_out = dispatch_tool(
            "git_log",
            &json!({"max_count": 5}),
            root,
            &ws_str,
            PermissionMode::ReadOnly,
            None,
            256 * 1024,
            200,
            200,
            1000,
            32,
        );
        assert!(log_out.contains("initial"), "log output was: {log_out}");

        let diff_out = dispatch_tool(
            "git_diff",
            &json!({}),
            root,
            &ws_str,
            PermissionMode::ReadOnly,
            None,
            256 * 1024,
            200,
            200,
            1000,
            32,
        );
        assert!(
            diff_out.contains("diff --git") || diff_out.contains("@@"),
            "diff output was: {diff_out}"
        );
    }

    #[tokio::test]
    async fn mock_read_file_roundtrip() {
        let _env = mock_env_lock_async().await;
        use mock_anthropic_service::MockAnthropicService;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("fixture.txt"), "hello parity fixture\n").unwrap();

        let mock = MockAnthropicService::spawn().await.expect("mock");
        let url = mock.base_url();

        let _g1 = EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-test-mock");
        let _g2 = EnvVarGuard::set("ANTHROPIC_BASE_URL", url.as_str());

        let config = AnalogConfig {
            model: "claude-sonnet-4-6".into(),
            workspace: root.clone(),
            permission_mode: PermissionMode::ReadOnly,
            accept_danger_non_interactive: false,
            use_stream: false,
            output_format: OutputFormat::Rich,
            use_runtime_enforcer: true,
            max_read_bytes: 1024 * 64,
            max_turns: 4,
            max_list_entries: 100,
            grep_max_lines: 50,
            glob_max_paths: 2000,
            glob_max_depth: 32,
            preset: Preset::None,
            language: AnalogLanguage::En,
            session_path: None,
            session_save_path: None,
            profile_hint: None,
            prompt: "PARITY_SCENARIO:read_file_roundtrip summarize".into(),
            rag_base_url: None,
            rag_http_timeout: Duration::from_secs(30),
            rag_top_k_max: 32,
        };

        let mut out = Vec::new();
        run(config, &mut out).await.expect("run");

        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("read_file roundtrip") || text.contains("fixture"),
            "unexpected model text: {text}"
        );
    }

    #[tokio::test]
    async fn mock_session_save_export_without_resume_path() {
        let _env = mock_env_lock_async().await;
        use mock_anthropic_service::MockAnthropicService;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("fixture.txt"), "hello parity fixture\n").unwrap();

        let mock = MockAnthropicService::spawn().await.expect("mock");
        let url = mock.base_url();

        let _g1 = EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-test-mock");
        let _g2 = EnvVarGuard::set("ANTHROPIC_BASE_URL", url.as_str());

        let export = dir.path().join("export-session.json");

        let config = AnalogConfig {
            model: "claude-sonnet-4-6".into(),
            workspace: root,
            permission_mode: PermissionMode::ReadOnly,
            accept_danger_non_interactive: false,
            use_stream: false,
            output_format: OutputFormat::Rich,
            use_runtime_enforcer: true,
            max_read_bytes: 1024 * 64,
            max_turns: 4,
            max_list_entries: 100,
            grep_max_lines: 50,
            glob_max_paths: 2000,
            glob_max_depth: 32,
            preset: Preset::None,
            language: AnalogLanguage::En,
            session_path: None,
            session_save_path: Some(export.clone()),
            profile_hint: None,
            prompt: "PARITY_SCENARIO:read_file_roundtrip summarize".into(),
            rag_base_url: None,
            rag_http_timeout: Duration::from_secs(30),
            rag_top_k_max: 32,
        };

        let mut out = Vec::new();
        run(config, &mut out).await.expect("run");

        let raw = std::fs::read_to_string(&export).expect("export file");
        let v: Value = serde_json::from_str(&raw).expect("session json");
        assert_eq!(v["version"], 1);
        let msgs = v["messages"].as_array().expect("messages");
        assert!(
            msgs.len() >= 2,
            "expected user+assistant, got {}",
            msgs.len()
        );
    }

    #[tokio::test]
    async fn mock_streaming_text_json() {
        let _env = mock_env_lock_async().await;
        use mock_anthropic_service::MockAnthropicService;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        let mock = MockAnthropicService::spawn().await.expect("mock");
        let url = mock.base_url();

        let _g1 = EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-test-mock");
        let _g2 = EnvVarGuard::set("ANTHROPIC_BASE_URL", url.as_str());

        let config = AnalogConfig {
            model: "claude-sonnet-4-6".into(),
            workspace: root,
            permission_mode: PermissionMode::ReadOnly,
            accept_danger_non_interactive: false,
            use_stream: true,
            output_format: OutputFormat::Json,
            use_runtime_enforcer: true,
            max_read_bytes: 1024 * 64,
            max_turns: 2,
            max_list_entries: 100,
            grep_max_lines: 50,
            glob_max_paths: 2000,
            glob_max_depth: 32,
            preset: Preset::None,
            language: AnalogLanguage::En,
            session_path: None,
            session_save_path: None,
            profile_hint: None,
            prompt: "PARITY_SCENARIO:streaming_text hello".into(),
            rag_base_url: None,
            rag_http_timeout: Duration::from_secs(30),
            rag_top_k_max: 32,
        };

        let mut buf = Vec::new();
        run(config, &mut buf).await.expect("run");

        let s = String::from_utf8_lossy(&buf);
        let lines: Vec<Value> = s
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<Value>(l).unwrap_or(Value::Null))
            .filter(|v| !v.is_null())
            .collect();

        let types: Vec<&str> = lines
            .iter()
            .filter_map(|v| v.get("type").and_then(|t| t.as_str()))
            .collect();

        assert!(types.contains(&"run_start"), "types={types:?}");
        let run_start = lines
            .iter()
            .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("run_start"))
            .expect("run_start");
        assert_eq!(
            run_start.get("schema").and_then(|v| v.as_str()),
            Some(NDJSON_SCHEMA)
        );
        assert_eq!(
            run_start.get("format_version").and_then(|v| v.as_u64()),
            Some(u64::from(NDJSON_FORMAT_VERSION))
        );
        assert!(
            types.contains(&"assistant_text_delta"),
            "expected NDJSON deltas, types={types:?}"
        );

        let turn = lines
            .iter()
            .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("assistant_turn"))
            .expect("assistant_turn line");
        let text = turn["text"].as_str().unwrap_or("");
        assert!(
            text.contains("Mock streaming") && text.contains("parity harness"),
            "rebuilt assistant text: {text:?}"
        );

        assert!(types.contains(&"run_end"), "types={types:?}");
    }

    struct EnvVarGuard {
        key: &'static str,
        old: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.old.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
