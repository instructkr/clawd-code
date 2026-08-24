#![recursion_limit = "256"]
#![allow(
    dead_code,
    unused_imports,
    unused_variables,
    clippy::doc_markdown,
    clippy::len_zero,
    clippy::manual_string_new,
    clippy::match_same_arms,
    clippy::result_large_err,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unneeded_struct_pattern,
    clippy::unnecessary_wraps,
    clippy::unused_self
)]
mod init;
mod input;
mod render;
mod setup_wizard;

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::net::TcpListener;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, UNIX_EPOCH};

use log::debug;

use api::{
    detect_provider_kind, model_family_identity_for, resolve_startup_auth_source, AnthropicClient,
    AuthSource, ContentBlockDelta, InputContentBlock, InputMessage, MessageRequest,
    MessageResponse, OutputContentBlock, PromptCache, ProviderClient as ApiProviderClient,
    ProviderKind, StreamEvent as ApiStreamEvent, ToolChoice, ToolDefinition,
    ToolResultContentBlock,
};

use commands::{
    classify_skills_slash_command, handle_agents_slash_command, handle_agents_slash_command_json,
    handle_mcp_slash_command, handle_mcp_slash_command_json, handle_plugins_slash_command,
    handle_skills_slash_command, handle_skills_slash_command_json, render_slash_command_help,
    render_slash_command_help_filtered, resolve_skill_invocation, resume_supported_slash_commands,
    slash_command_specs, validate_slash_command_input, PluginsCommandResult, SkillSlashDispatch,
    SlashCommand,
};
use init::initialize_repo;
use plugins::{PluginHooks, PluginManager, PluginManagerConfig, PluginRegistry};
use render::{MarkdownStreamState, Spinner, TerminalRenderer};
use runtime::{
    check_base_commit, format_stale_base_warning, format_usd, load_oauth_credentials,
    load_system_prompt, load_system_prompt_with_context, pricing_for_model, resolve_expected_base,
    resolve_sandbox_status, ApiClient, ApiRequest, AssistantEvent, BaseCommitState,
    CompactionConfig, ConfigFileReport, ConfigLoader, ConfigSource, ContentBlock, ContextFile,
    ConversationMessage, ConversationRuntime, McpConfigCollection, McpInvalidServerConfig,
    McpServer, McpServerManager, McpServerSpec, McpTool, MessageRole, ModelPricing, PermissionMode,
    PermissionPolicy, ProjectContext, PromptCacheEvent, ResolvedPermissionMode, RuntimeError,
    RuntimeInvalidHookConfig, Session, TokenUsage, ToolError, ToolExecutor, UsageTracker,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tools::{
    canonical_allowed_tool_name, execute_tool, mvp_tool_specs, GlobalToolRegistry,
    RuntimeToolDefinition, ToolSearchOutput,
};

const DEFAULT_MODEL: &str = "anthropic/claude-opus-4-7";

/// #148: Model provenance for `claw status` JSON/text output. Records where
/// the resolved model string came from so claws don't have to re-read argv
/// to audit whether their `--model` flag was honored vs falling back to env
/// or config or default.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelSource {
    /// Explicit `--model` / `--model=` CLI flag.
    Flag,
    /// Runtime model environment variable (when no flag was passed).
    Env,
    /// `model` key in `.claw.json` / `.claw/settings.json` (when neither
    /// flag nor env set it).
    Config,
    /// Compiled-in `DEFAULT_MODEL` fallback.
    Default,
}

impl ModelSource {
    fn as_str(&self) -> &'static str {
        match self {
            ModelSource::Flag => "flag",
            ModelSource::Env => "env",
            ModelSource::Config => "config",
            ModelSource::Default => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelProvenance {
    /// Resolved model string (after alias expansion).
    resolved: String,
    /// Raw user input before alias resolution. None when source is Default.
    raw: Option<String>,
    /// Where the resolved model string originated.
    source: ModelSource,
    /// Alias-expanded target when `raw` differs from `resolved`.
    alias_resolved_to: Option<String>,
    /// Environment variable that supplied the model, when source is Env.
    env_var: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermissionModeSource {
    Flag,
    Env,
    Config,
    Default,
}

impl PermissionModeSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Env => "env",
            Self::Config => "config",
            Self::Default => "default",
        }
    }

    fn is_explicit(self) -> bool {
        !matches!(self, Self::Default)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PermissionModeProvenance {
    mode: PermissionMode,
    source: PermissionModeSource,
    env_var: Option<&'static str>,
}

impl PermissionModeProvenance {
    fn from_flag(mode: PermissionMode) -> Self {
        Self {
            mode,
            source: PermissionModeSource::Flag,
            env_var: None,
        }
    }

    fn default_fallback() -> Self {
        Self {
            mode: PermissionMode::WorkspaceWrite,
            source: PermissionModeSource::Default,
            env_var: None,
        }
    }
}

struct EnvModel {
    name: &'static str,
    value: String,
}

impl ModelProvenance {
    fn default_fallback() -> Self {
        Self {
            resolved: DEFAULT_MODEL.to_string(),
            raw: None,
            source: ModelSource::Default,
            alias_resolved_to: None,
            env_var: None,
        }
    }

    fn from_flag(raw: &str, resolved: &str) -> Self {
        Self::from_resolved(raw, resolved, ModelSource::Flag, None)
    }

    fn from_raw(raw: &str, source: ModelSource, env_var: Option<&str>) -> Self {
        let resolved = resolve_model_alias_with_config(raw);
        Self::from_resolved(raw, &resolved, source, env_var)
    }

    fn from_resolved(
        raw: &str,
        resolved: &str,
        source: ModelSource,
        env_var: Option<&str>,
    ) -> Self {
        let raw_trimmed = raw.trim();
        let alias_resolved_to = (raw_trimmed != resolved).then(|| resolved.to_string());
        Self {
            resolved: resolved.to_string(),
            raw: Some(raw.to_string()),
            source,
            alias_resolved_to,
            env_var: env_var.map(str::to_string),
        }
    }

    fn from_env_or_config_or_default(cli_model: &str) -> Result<Self, String> {
        // Only called when no --model flag was passed. Probe env first,
        // then config, else fall back to default. Mirrors the logic in
        // resolve_repl_model() but captures the source.
        if cli_model != DEFAULT_MODEL {
            let provenance = Self::from_resolved(cli_model, cli_model, ModelSource::Flag, None);
            provenance.validate()?;
            return Ok(provenance);
        }
        if let Some(env_model) = env_model_for_runtime() {
            let provenance =
                Self::from_raw(&env_model.value, ModelSource::Env, Some(env_model.name));
            provenance.validate()?;
            return Ok(provenance);
        }
        if let Some(config_model) = config_model_for_current_dir() {
            let provenance = Self::from_raw(&config_model, ModelSource::Config, None);
            provenance.validate()?;
            return Ok(provenance);
        }
        Ok(Self::default_fallback())
    }

    fn validate(&self) -> Result<(), String> {
        validate_model_syntax(&self.resolved).map_err(|error| {
            let source = match self.source {
                ModelSource::Flag => "--model",
                ModelSource::Env => self.env_var.as_deref().unwrap_or("environment"),
                ModelSource::Config => "config model",
                ModelSource::Default => "default model",
            };
            if let Some(raw) = &self.raw {
                format!(
                    "invalid_model: {source} model `{raw}` is invalid after alias resolution to `{}`.\n{error}",
                    self.resolved
                )
            } else {
                error
            }
        })
    }
}

fn env_model_for_runtime() -> Option<EnvModel> {
    ["CLAW_MODEL", "ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_MODEL"]
        .into_iter()
        .find_map(|name| {
            env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(|value| EnvModel { name, value })
        })
}

fn max_tokens_for_model(model: &str) -> u32 {
    api::max_tokens_for_model(model)
}
// Build-time constants injected by build.rs (fall back to static values when
// build.rs hasn't run, e.g. in doc-test or unusual toolchain environments).
const DEFAULT_DATE: &str = match option_env!("BUILD_DATE") {
    Some(d) => d,
    None => "unknown",
};
const DEFAULT_OAUTH_CALLBACK_PORT: u16 = 4545;
const VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_TARGET: Option<&str> = option_env!("TARGET");
const GIT_SHA: Option<&str> = option_env!("GIT_SHA");
const GIT_SHA_SHORT: Option<&str> = option_env!("GIT_SHA_SHORT");
const GIT_DIRTY: Option<&str> = option_env!("GIT_DIRTY");
const GIT_BRANCH: Option<&str> = option_env!("GIT_BRANCH");
const GIT_COMMIT_DATE: Option<&str> = option_env!("GIT_COMMIT_DATE");
const GIT_COMMIT_TIMESTAMP: Option<&str> = option_env!("GIT_COMMIT_TIMESTAMP");
const RUSTC_VERSION: Option<&str> = option_env!("RUSTC_VERSION");
const INTERNAL_PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);
const POST_TOOL_STALL_TIMEOUT: Duration = Duration::from_secs(10);
const PRIMARY_SESSION_EXTENSION: &str = "jsonl";
const LEGACY_SESSION_EXTENSION: &str = "json";
const OFFICIAL_REPO_URL: &str = "https://github.com/ultraworkers/claw-code";
const OFFICIAL_REPO_SLUG: &str = "ultraworkers/claw-code";
const DEPRECATED_INSTALL_COMMAND: &str = "cargo install claw-code";
const LATEST_SESSION_REFERENCE: &str = "latest";
const SESSION_REFERENCE_ALIASES: &[&str] = &[LATEST_SESSION_REFERENCE, "last", "recent"];
const CLI_OPTION_SUGGESTIONS: &[&str] = &[
    "--help",
    "-h",
    "--version",
    "-V",
    "--model",
    "--output-format",
    "--permission-mode",
    "--cwd",
    "--directory",
    "-C",
    "--skip-permissions",
    "--dangerously-skip-permissions",
    "--allowedTools",
    "--allowed-tools",
    "--resume",
    "--acp",
    "-acp",
    "--print",
    "--compact",
    "--base-commit",
    "-p",
];

fn is_registered_cli_flag_token(value: &str) -> bool {
    let flag = value.split_once('=').map_or(value, |(flag, _)| flag);
    CLI_OPTION_SUGGESTIONS.contains(&flag)
}

fn should_reject_unknown_option_like(value: &str) -> bool {
    is_registered_cli_flag_token(value)
        || (value.starts_with("--")
            && suggest_closest_term(value, CLI_OPTION_SUGGESTIONS).is_some())
}

type AllowedToolSet = BTreeSet<String>;
type RuntimePluginStateBuildOutput = (
    Option<Arc<Mutex<RuntimeMcpState>>>,
    Vec<RuntimeToolDefinition>,
);

fn main() {
    if let Err(error) = run() {
        let message = error.to_string();
        // When --output-format json is active, emit errors as JSON so downstream
        // tools can parse failures the same way they parse successes (ROADMAP #42).
        let argv: Vec<String> = std::env::args().collect();
        let json_output = raw_args_request_json_output(&argv[1..]);
        if json_output {
            // #77/#696: classify error by prefix so downstream claws can route
            // without regex-scraping prose. Keep the legacy `type`/`kind`
            // fields and add the stable status/error_kind/action contract used
            // by non-interactive command guards.
            let kind = classify_error_kind(&message);
            let (short_reason, inline_hint) = split_error_hint(&message);
            // #781: fall back to a kind-derived hint when the message has no \n-delimited hint
            let hint = inline_hint.or_else(|| fallback_hint_for_error_kind(kind).map(String::from));
            let mut error_json = serde_json::json!({
                "type": "error",
                "kind": kind,
                "status": "error",
                "error_kind": kind,
                "error": short_reason,
                "message": short_reason,
                "action": "abort",
                "hint": hint,
                "exit_code": 1,
            });
            if kind == "invalid_cwd" {
                if let Some(error) = error.downcast_ref::<InvalidCwdError>() {
                    if let Some(object) = error_json.as_object_mut() {
                        object.insert("path".to_string(), serde_json::json!(&error.path));
                        object.insert(
                            "reason".to_string(),
                            serde_json::json!(error.reason.as_str()),
                        );
                    }
                }
            } else if kind == "invalid_output_path" {
                if let Some(error) = error.downcast_ref::<InvalidOutputPathError>() {
                    if let Some(object) = error_json.as_object_mut() {
                        object.insert("path".to_string(), serde_json::json!(&error.path));
                        object.insert(
                            "reason".to_string(),
                            serde_json::json!(error.reason.as_str()),
                        );
                    }
                }
            } else if kind == "invalid_output_format" {
                if let Some(object) = error_json.as_object_mut() {
                    object.insert(
                        "value".to_string(),
                        serde_json::json!(invalid_output_format_value(&message)),
                    );
                    object.insert("expected".to_string(), serde_json::json!(["text", "json"]));
                }
            } else if kind == "invalid_tool_name" {
                let (tool_name, available, aliases) = invalid_tool_name_details(&message);
                if let Some(object) = error_json.as_object_mut() {
                    if let Some(tool_name) = tool_name {
                        object.insert("tool_name".to_string(), serde_json::json!(tool_name));
                    }
                    object.insert("available".to_string(), serde_json::json!(available));
                    object.insert("tool_aliases".to_string(), aliases);
                }
            } else if kind == "missing_argument" {
                if let Some(object) = error_json.as_object_mut() {
                    if message.contains("--allowedTools") {
                        object.insert("argument".to_string(), serde_json::json!("--allowedTools"));
                    } else if message.contains("prompt or subcommand") {
                        object.insert(
                            "argument".to_string(),
                            serde_json::json!("prompt or subcommand"),
                        );
                    }
                }
            }
            // #819/#820/#823: JSON mode error envelopes must go to stdout so machine
            // consumers can parse failures from stdout byte 0 (parity with all
            // non-interactive command guards that already use println! / to_stdout).
            println!("{}", error_json);
        } else {
            // #156: Add machine-readable error kind to text output so stderr observers
            // don't need to regex-scrape the prose.
            let kind = classify_error_kind(&message);
            if message.contains("`claw --help`") {
                eprintln!(
                    "[error-kind: {kind}]
error: {message}"
                );
            } else {
                eprintln!(
                    "[error-kind: {kind}]
error: {message}

Run `claw --help` for usage."
                );
            }
        }
        std::process::exit(1);
    }
}

/// #77: Classify a stringified error message into a machine-readable kind.
///
/// Returns a `snake_case` token that downstream consumers can switch on instead
/// of regex-scraping the prose. The classification is best-effort prefix/keyword
/// matching against the error messages produced throughout the CLI surface.
fn classify_error_kind(message: &str) -> &'static str {
    // Check specific patterns first (more specific before generic)
    if message.starts_with("unknown_slash_command:") {
        "unknown_slash_command"
    } else if message.starts_with("command_not_found:") {
        "command_not_found"
    } else if message.contains("missing Anthropic credentials") {
        "missing_credentials"
    } else if message.contains("Manifest source files are missing")
        || message.starts_with("missing_manifests:")
    {
        "missing_manifests"
    } else if message.contains("no worker state file found") {
        "missing_worker_state"
    } else if message.contains("session not found") {
        "session_not_found"
    } else if message.contains("no managed sessions found") {
        "no_managed_sessions"
    } else if message.contains("legacy session is missing workspace binding") {
        // #780: must precede the generic "failed to restore session" arm — the full
        // error message is "failed to restore session: legacy session is missing workspace
        // binding: ...", so the specific arm must be checked first.
        "legacy_session_no_workspace_binding"
    } else if message.contains("Is a directory") || message.contains("os error 21") {
        // #787: --resume given a directory path instead of a .jsonl file
        "session_path_is_directory"
    } else if message.contains("failed to restore session") {
        "session_load_failed"
    } else if message.contains("unsupported ACP invocation") {
        "unsupported_acp_invocation"
    } else if message.starts_with("missing_argument:") {
        "missing_argument"
    } else if message.contains("unsupported skills action") {
        "unsupported_skills_action"
    } else if message.starts_with("invalid_install_source:") {
        "invalid_install_source"
    } else if message.starts_with("invalid_cwd:") {
        "invalid_cwd"
    } else if message.starts_with("invalid_output_path:") {
        "invalid_output_path"
    } else if message.starts_with("invalid_output_format:") {
        "invalid_output_format"
    } else if message.starts_with("invalid_tool_name:") {
        "invalid_tool_name"
    } else if message.contains("unrecognized argument") || message.contains("unknown option") {
        "cli_parse"
    } else if message.starts_with("missing_flag_value:") {
        "missing_flag_value"
    } else if message.starts_with("invalid_permission_mode:") {
        "invalid_permission_mode"
    } else if message.starts_with("invalid_flag_value:") {
        "invalid_flag_value"
    } else if message.starts_with("invalid_model:") {
        "invalid_model"
    } else if message.contains("invalid model syntax") {
        "invalid_model_syntax"
    } else if message.contains("is not yet implemented") {
        "unsupported_command"
    } else if message.contains("unsupported resumed command") {
        "unsupported_resumed_command"
    } else if message.contains("confirmation required") {
        "confirmation_required"
    } else if (message.contains("api failed") || message.contains("api returned"))
        && (message.contains("401")
            || message.contains("Unauthorized")
            || message.contains("authentication_error"))
    {
        // #781: sub-classify auth failures so wrappers can distinguish from rate-limit / server errors
        "api_auth_error"
    } else if (message.contains("api failed") || message.contains("api returned"))
        && (message.contains("429")
            || message.contains("rate_limit")
            || message.contains("rate limit"))
    {
        // #781: sub-classify rate-limit failures
        "api_rate_limit_error"
    } else if message.contains("api failed") || message.contains("api returned") {
        "api_http_error"
    } else if message.contains("mcpServers") {
        "malformed_mcp_config"
    } else if message.contains(".claw/settings.json") || message.contains(".claw.json") {
        // #763: config file JSON parse / validation errors (e.g. unterminated string, type mismatch)
        "config_parse_error"
    } else if message.starts_with("empty prompt") {
        "empty_prompt"
    } else if message.starts_with("interactive_only:") || message.contains("stdin is not a TTY") {
        "interactive_only"
    } else if message.starts_with("unknown agents subcommand:") {
        "unknown_agents_subcommand"
    } else if message.starts_with("agent not found:") {
        "agent_not_found"
    } else if message.contains("is not installed") || message.starts_with("plugin_not_found:") {
        "plugin_not_found"
    } else if message.contains("plugin source") && message.contains("was not found") {
        // #794: `plugins install /nonexistent/path` → "plugin source ... was not found"
        "plugin_source_not_found"
    } else if (message.contains("skill source") && message.contains("not found"))
        || message.starts_with("skill '")
    {
        "skill_not_found"
    } else if message.contains("Unsupported config section") {
        "unsupported_config_section"
    } else if message.contains("unknown_plugins_action") {
        "unknown_plugins_action"
    } else if message.starts_with("invalid_history_count:") || message.contains("invalid count") {
        "invalid_history_count"
    } else if message.starts_with("missing_prompt:") {
        "missing_prompt"
    } else if message.contains("has been removed.") {
        // #765: removed subcommands (login, logout) — hint contains migration guidance
        "removed_subcommand"
    } else if message.starts_with("unknown subcommand:") {
        // #785/#825: typo/unknown top-level subcommand (e.g. `claw dump` → did you mean dump-manifests?)
        // Unified under command_not_found in #825.
        "command_not_found"
    } else if message.starts_with("unexpected extra arguments")
        || message.starts_with("unexpected_extra_args:")
    {
        // #766: extra positionals after commands that take no arguments (e.g. claw diff)
        // #784: export extra-positional errors use the typed prefix form
        "unexpected_extra_args"
    } else if message.starts_with("invalid_resume_argument:") {
        // #768: --resume trailing arg is not a slash command
        "invalid_resume_argument"
    } else if message.starts_with("unknown_option:") {
        "unknown_option"
    } else if message.contains("is a slash command")
        || message.starts_with("interactive_only:")
        // #735: "slash command /X is interactive-only" emitted by interactive-only guard
        || (message.starts_with("slash command") && message.contains("interactive-only"))
    {
        "interactive_only"
    } else {
        "unknown"
    }
}

/// #77: Split a multi-line error message into (`short_reason`, `optional_hint`).
///
/// The `short_reason` is the first line (up to the first newline), and the hint
/// is the remaining text or `None` if there's no newline. This prevents the
/// runbook prose from being stuffed into the `error` field that downstream
/// parsers expect to be the short reason alone.
fn split_error_hint(message: &str) -> (String, Option<String>) {
    match message.split_once('\n') {
        Some((short, hint)) => (short.to_string(), Some(hint.trim().to_string())),
        None => (message.to_string(), None),
    }
}

fn invalid_tool_name_details(message: &str) -> (Option<String>, Vec<String>, Value) {
    let tool_name = message
        .strip_prefix("invalid_tool_name: unsupported tool in --allowedTools:")
        .and_then(|rest| rest.lines().next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let available = message
        .lines()
        .find_map(|line| line.strip_prefix("Available:"))
        .map(|line| {
            line.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let aliases = message
        .lines()
        .find_map(|line| line.strip_prefix("Aliases:"))
        .map(|line| {
            line.split(',')
                .filter_map(|entry| entry.trim().split_once('='))
                .map(|(alias, canonical)| {
                    (
                        alias.trim().to_string(),
                        Value::String(canonical.trim().to_string()),
                    )
                })
                .collect::<Map<_, _>>()
        })
        .unwrap_or_default();
    (tool_name, available, Value::Object(aliases))
}

fn invalid_output_format_value(message: &str) -> Option<String> {
    message
        .strip_prefix("invalid_output_format: unsupported value for --output-format:")
        .and_then(|rest| rest.lines().next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// #781: derive a stable fallback hint from a classified error kind when the error
/// message itself has no `\n`-delimited hint. Returns `None` for kinds where the
/// message is self-explanatory or no canonical remediation exists.
fn fallback_hint_for_error_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "api_auth_error" => {
            Some("Check that ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN is set and valid.")
        }
        "api_rate_limit_error" => {
            Some("You have hit the API rate limit. Wait and retry, or reduce request frequency.")
        }
        "missing_credentials" => {
            Some("Set ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN before running claw.")
        }
        "config_parse_error" => Some(
            "Fix the JSON syntax or schema in the referenced .claw/settings.json or .claw.json file, then rerun the command.",
        ),
        // #787: session load failures have no \n-delimited hint from the OS error path
        "session_load_failed" => Some(
            "Pass a path to a .jsonl session file, not a directory. Managed sessions live in .claw/sessions/.",
        ),
        "session_path_is_directory" => Some(
            "--resume expects a .jsonl session file path, not a directory. Run `claw --output-format json /session list` to list managed sessions.",
        ),
        // #793: plugins uninstall/enable/disable of non-existing plugin propagates through
        // the ? operator with no \n delimiter, so split_error_hint returns None.
        "plugin_not_found" => Some("Run `claw plugins list` to see installed plugins."),
        // #794: plugins install with a path that doesn't exist
        "plugin_source_not_found" => Some(
            "Check that the path or URL is correct. Use a local directory or a valid registry id.",
        ),
        // #795: skills install/show of a non-existing skill path or name
        "skill_not_found" => Some(
            "Run `claw skills list` to see available skills, or `claw skills install <path>` to install a new one.",
        ),
        // #795/#431: unsupported/invalid skills lifecycle input should include actionable local guidance.
        "unsupported_skills_action" => Some(
            "Supported: list, show <name>, install <path>, uninstall <name>, help. Run `claw skills help` for details.",
        ),
        "invalid_install_source" => Some(
            "Pass a local skill directory containing SKILL.md or a standalone markdown file.",
        ),
        "invalid_tool_name" => Some(
            "Use canonical snake_case tool names from `available` or documented aliases from `tool_aliases`.",
        ),
        "invalid_output_format" => Some("Use --output-format text or --output-format json."),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvalidCwdReason {
    Empty,
    NotFound,
    NotADirectory,
}

impl InvalidCwdReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::NotFound => "not_found",
            Self::NotADirectory => "not_a_directory",
        }
    }
}

#[derive(Debug)]
struct InvalidCwdError {
    path: String,
    reason: InvalidCwdReason,
}

impl InvalidCwdError {
    fn new(path: impl Into<String>, reason: InvalidCwdReason) -> Self {
        Self {
            path: path.into(),
            reason,
        }
    }
}

impl std::fmt::Display for InvalidCwdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid_cwd: {}: `{}`\nUsage: --cwd <path>, -C <path>, or --directory <path>",
            self.reason.as_str(),
            self.path
        )
    }
}

impl std::error::Error for InvalidCwdError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvalidOutputPathReason {
    Empty,
    ParentNotFound,
    ParentNotADirectory,
    PathIsDirectory,
}

impl InvalidOutputPathReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::ParentNotFound => "parent_not_found",
            Self::ParentNotADirectory => "parent_not_a_directory",
            Self::PathIsDirectory => "path_is_directory",
        }
    }
}

#[derive(Debug)]
struct InvalidOutputPathError {
    path: String,
    reason: InvalidOutputPathReason,
}

impl InvalidOutputPathError {
    fn new(path: impl Into<String>, reason: InvalidOutputPathReason) -> Self {
        Self {
            path: path.into(),
            reason,
        }
    }
}

impl std::fmt::Display for InvalidOutputPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid_output_path: {}: `{}`\nUsage: claw export [PATH] [--session SESSION] [--output PATH]",
            self.reason.as_str(),
            self.path
        )
    }
}

impl std::error::Error for InvalidOutputPathError {}

fn split_global_cwd_args(
    args: &[String],
) -> Result<(Vec<String>, Option<PathBuf>), Box<dyn std::error::Error>> {
    let mut filtered = Vec::with_capacity(args.len());
    let mut cwd = None;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--cwd" | "-C" | "--directory" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "missing_flag_value: missing value for --cwd.\nUsage: --cwd <path>, -C <path>, or --directory <path>",
                    )
                })?;
                cwd = Some(validate_global_cwd(value)?);
                index += 2;
            }
            flag if flag.starts_with("--cwd=") => {
                let value = &flag[6..];
                cwd = Some(validate_global_cwd(value)?);
                index += 1;
            }
            flag if flag.starts_with("--directory=") => {
                let value = &flag[12..];
                cwd = Some(validate_global_cwd(value)?);
                index += 1;
            }
            flag if global_flag_takes_value(flag) => {
                filtered.push(arg.clone());
                if let Some(value) = args.get(index + 1) {
                    filtered.push(value.clone());
                    index += 2;
                } else {
                    index += 1;
                }
            }
            flag if global_flag_is_value_inline(flag) => {
                filtered.push(arg.clone());
                index += 1;
            }
            flag if global_flag_without_value(flag) => {
                filtered.push(arg.clone());
                index += 1;
            }
            "--" => {
                filtered.extend(args[index..].iter().cloned());
                break;
            }
            other if other.starts_with('-') => {
                filtered.push(arg.clone());
                index += 1;
            }
            _ => {
                filtered.extend(args[index..].iter().cloned());
                break;
            }
        }
    }

    Ok((filtered, cwd))
}

fn global_flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--model"
            | "--output-format"
            | "--permission-mode"
            | "--base-commit"
            | "--reasoning-effort"
            | "--allowedTools"
            | "--allowed-tools"
    )
}

fn global_flag_is_value_inline(flag: &str) -> bool {
    flag.starts_with("--model=")
        || flag.starts_with("--output-format=")
        || flag.starts_with("--permission-mode=")
        || flag.starts_with("--base-commit=")
        || flag.starts_with("--reasoning-effort=")
        || flag.starts_with("--allowedTools=")
        || flag.starts_with("--allowed-tools=")
}

fn global_flag_without_value(flag: &str) -> bool {
    matches!(
        flag,
        "--help"
            | "-h"
            | "--version"
            | "-V"
            | "--dangerously-skip-permissions"
            | "--skip-permissions"
            | "--compact"
            | "--allow-broad-cwd"
            | "--print"
            | "--acp"
            | "-acp"
    )
}

fn validate_global_cwd(value: &str) -> Result<PathBuf, InvalidCwdError> {
    if value.trim().is_empty() {
        return Err(InvalidCwdError::new(value, InvalidCwdReason::Empty));
    }
    let path = PathBuf::from(value);
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Ok(path),
        Ok(_) => Err(InvalidCwdError::new(value, InvalidCwdReason::NotADirectory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(InvalidCwdError::new(value, InvalidCwdReason::NotFound))
        }
        Err(_) => Err(InvalidCwdError::new(value, InvalidCwdReason::NotFound)),
    }
}

fn apply_global_cwd(cwd: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(cwd) = cwd {
        env::set_current_dir(cwd)?;
    }
    Ok(())
}

/// Read piped stdin content when stdin is not a terminal.
///
/// Returns `None` when stdin is attached to a terminal (interactive REPL use),
/// when reading fails, or when the piped content is empty after trimming.
/// Returns `Some(raw_content)` when a pipe delivered non-empty content.
fn read_piped_stdin() -> Option<String> {
    if io::stdin().is_terminal() {
        return None;
    }
    let mut buffer = String::new();
    if io::stdin().read_to_string(&mut buffer).is_err() {
        return None;
    }
    if buffer.trim().is_empty() {
        return None;
    }
    Some(buffer)
}

/// Merge a piped stdin payload into a prompt argument.
///
/// When `stdin_content` is `None` or empty after trimming, the prompt is
/// returned unchanged. Otherwise the trimmed stdin content is appended to the
/// prompt separated by a blank line so the model sees the prompt first and the
/// piped context immediately after it.
fn merge_prompt_with_stdin(prompt: &str, stdin_content: Option<&str>) -> String {
    let Some(raw) = stdin_content else {
        return prompt.to_string();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return prompt.to_string();
    }
    if prompt.is_empty() {
        return trimmed.to_string();
    }
    format!("{prompt}\n\n{trimmed}")
}

fn plugin_command_json(
    action: &str,
    target: Option<&str>,
    result: &commands::PluginsCommandResult,
    report: &plugins::PluginRegistryReport,
) -> Value {
    let failures = report.failures();
    json!({
        "kind": "plugin",
        "action": action,
        "target": target,
        "status": if failures.is_empty() { "ok" } else { "degraded" },
        "message": result.message,
        "reload_runtime": result.reload_runtime,
        "plugins": report.summaries().iter().map(plugin_summary_json).collect::<Vec<_>>(),
        "load_failures": failures.iter().map(plugin_load_failure_json).collect::<Vec<_>>(),
    })
}

fn plugin_summary_json(plugin: &plugins::PluginSummary) -> Value {
    json!({
        "id": &plugin.metadata.id,
        "name": &plugin.metadata.name,
        "version": &plugin.metadata.version,
        "description": &plugin.metadata.description,
        "kind": plugin.metadata.kind.to_string(),
        "source": &plugin.metadata.source,
        // #730: path parity with agents (#728) and skills (#729)
        "path": plugin.metadata.root.as_ref().map(|p| p.display().to_string()),
        "enabled": plugin.enabled,
        "lifecycle_state": plugin.lifecycle_state(),
        "lifecycle": {
            "configured": !plugin.lifecycle.is_empty(),
            "init": {
                "configured": !plugin.lifecycle.init.is_empty(),
                "command_count": plugin.lifecycle.init.len(),
            },
            "shutdown": {
                "configured": !plugin.lifecycle.shutdown.is_empty(),
                "command_count": plugin.lifecycle.shutdown.len(),
            },
        },
    })
}

fn plugin_load_failure_json(failure: &plugins::PluginLoadFailure) -> Value {
    json!({
        "plugin_root": failure.plugin_root.display().to_string(),
        "kind": failure.kind.to_string(),
        "source": &failure.source,
        "lifecycle_state": "load_failed",
        "error": failure.error().to_string(),
    })
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    // #824: suppress config deprecation prose warnings to stderr when JSON
    // output mode is active.  Scan the raw argv before parse_args so the
    // suppression is in place before any settings file is loaded.
    let json_mode = raw_args_request_json_output(&args);
    if json_mode {
        runtime::suppress_config_warnings_for_json_mode();
    }
    let (args, cwd) = split_global_cwd_args(&args)?;
    apply_global_cwd(cwd)?;
    match parse_args(&args)? {
        CliAction::DumpManifests {
            output_format,
            manifests_dir,
        } => dump_manifests(manifests_dir.as_deref(), output_format)?,
        CliAction::BootstrapPlan { output_format } => print_bootstrap_plan(output_format)?,
        CliAction::Agents {
            args,
            output_format,
        } => LiveCli::print_agents(args.as_deref(), output_format)?,
        CliAction::Mcp {
            args,
            output_format,
        } => LiveCli::print_mcp(args.as_deref(), output_format)?,
        CliAction::Skills {
            args,
            output_format,
        } => LiveCli::print_skills(args.as_deref(), output_format)?,
        CliAction::Plugins {
            action,
            target,
            output_format,
        } => LiveCli::print_plugins(action.as_deref(), target.as_deref(), output_format)?,
        CliAction::PrintSystemPrompt {
            cwd,
            date,
            model,
            output_format,
        } => print_system_prompt(cwd, date, &model, output_format)?,
        CliAction::Version { output_format } => print_version(output_format)?,
        CliAction::ResumeSession {
            session_path,
            commands,
            output_format,
            allow_broad_cwd,
        } => {
            enforce_broad_cwd_policy(allow_broad_cwd, output_format)?;
            resume_session(&session_path, &commands, output_format)
        }
        CliAction::Status {
            model,
            model_flag_raw,
            permission_mode,
            output_format,
            allowed_tools,
        } => print_status_snapshot(
            &model,
            model_flag_raw.as_deref(),
            permission_mode,
            output_format,
            allowed_tools.as_ref(),
        )?,
        CliAction::Sandbox { output_format } => print_sandbox_status_snapshot(output_format)?,
        CliAction::Prompt {
            prompt,
            model,
            output_format,
            allowed_tools,
            permission_mode,
            compact,
            base_commit,
            reasoning_effort,
            allow_broad_cwd,
        } => {
            enforce_broad_cwd_policy(allow_broad_cwd, output_format)?;
            run_stale_base_preflight(base_commit.as_deref());
            // Only consume piped stdin as prompt context when the permission
            // mode is fully unattended. In modes where the permission
            // prompter may invoke CliPermissionPrompter::decide(), stdin
            // must remain available for interactive approval; otherwise the
            // prompter's read_line() would hit EOF and deny every request.
            let stdin_context = if matches!(permission_mode, PermissionMode::DangerFullAccess) {
                read_piped_stdin()
            } else {
                None
            };
            let effective_prompt = merge_prompt_with_stdin(&prompt, stdin_context.as_deref());
            let resolved_model = resolve_repl_model(model)?;
            let mut cli = LiveCli::new(resolved_model, true, allowed_tools, permission_mode)?;
            cli.set_reasoning_effort(reasoning_effort);
            cli.run_turn_with_output(&effective_prompt, output_format, compact)?;
        }
        CliAction::Doctor {
            output_format,
            permission_mode,
        } => run_doctor(output_format, permission_mode)?,
        CliAction::Acp { output_format } => {
            print_acp_status(output_format)?;
            std::process::exit(2);
        }
        CliAction::SessionList { output_format } => run_session_list(output_format)?,
        CliAction::State { output_format } => run_worker_state(output_format)?,
        CliAction::Init { output_format } => run_init(output_format)?,
        CliAction::Setup { output_format: _ } => run_setup()?,
        // #146: dispatch pure-local introspection. Text mode uses existing
        // render_config_report/render_diff_report; JSON mode uses the
        // corresponding _json helpers already exposed for resume sessions.
        CliAction::Config {
            section,
            output_format,
        } => match output_format {
            CliOutputFormat::Text => {
                println!("{}", render_config_report(section.as_deref())?);
            }
            CliOutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&render_config_json(section.as_deref())?)?
                );
            }
        },
        CliAction::Models {
            action,
            output_format,
        } => print_models(action.as_deref(), output_format)?,
        CliAction::Diff { output_format } => match output_format {
            CliOutputFormat::Text => {
                println!("{}", render_diff_report()?);
            }
            CliOutputFormat::Json => {
                let cwd = friendly_cwd(env::current_dir()?);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&render_diff_json_for(&cwd)?)?
                );
            }
        },
        CliAction::Export {
            session_reference,
            output_path,
            output_format,
        } => run_export(&session_reference, output_path.as_deref(), output_format)?,
        CliAction::Repl {
            model,
            allowed_tools,
            permission_mode,
            base_commit,
            reasoning_effort,
            allow_broad_cwd,
        } => run_repl(
            model,
            allowed_tools,
            permission_mode,
            base_commit,
            reasoning_effort,
            allow_broad_cwd,
        )?,
        CliAction::HelpTopic {
            topic,
            output_format,
        } => print_help_topic(topic, output_format)?,
        CliAction::Help { output_format } => print_help(output_format)?,
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliAction {
    DumpManifests {
        output_format: CliOutputFormat,
        manifests_dir: Option<PathBuf>,
    },
    BootstrapPlan {
        output_format: CliOutputFormat,
    },
    Agents {
        args: Option<String>,
        output_format: CliOutputFormat,
    },
    Mcp {
        args: Option<String>,
        output_format: CliOutputFormat,
    },
    Skills {
        args: Option<String>,
        output_format: CliOutputFormat,
    },
    Plugins {
        action: Option<String>,
        target: Option<String>,
        output_format: CliOutputFormat,
    },
    PrintSystemPrompt {
        cwd: PathBuf,
        date: String,
        model: String,
        output_format: CliOutputFormat,
    },
    Version {
        output_format: CliOutputFormat,
    },
    SessionList {
        output_format: CliOutputFormat,
    },
    ResumeSession {
        session_path: PathBuf,
        commands: Vec<String>,
        output_format: CliOutputFormat,
        allow_broad_cwd: bool,
    },
    Status {
        model: String,
        // #148: raw `--model` flag input (pre-alias-resolution), if any.
        // None means no flag was supplied; env/config/default fallback is
        // resolved inside `print_status_snapshot`.
        model_flag_raw: Option<String>,
        permission_mode: PermissionModeProvenance,
        output_format: CliOutputFormat,
        allowed_tools: Option<AllowedToolSet>,
    },
    Sandbox {
        output_format: CliOutputFormat,
    },
    Prompt {
        prompt: String,
        model: String,
        output_format: CliOutputFormat,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
        compact: bool,
        base_commit: Option<String>,
        reasoning_effort: Option<String>,
        allow_broad_cwd: bool,
    },
    Doctor {
        output_format: CliOutputFormat,
        permission_mode: PermissionModeProvenance,
    },
    Acp {
        output_format: CliOutputFormat,
    },
    State {
        output_format: CliOutputFormat,
    },
    Init {
        output_format: CliOutputFormat,
    },
    Setup {
        output_format: CliOutputFormat,
    },
    // #146: `claw config` and `claw diff` are pure-local read-only
    // introspection commands; wire them as standalone CLI subcommands.
    Config {
        section: Option<String>,
        output_format: CliOutputFormat,
    },
    Models {
        action: Option<String>,
        output_format: CliOutputFormat,
    },
    Diff {
        output_format: CliOutputFormat,
    },
    Export {
        session_reference: String,
        output_path: Option<PathBuf>,
        output_format: CliOutputFormat,
    },
    Repl {
        model: String,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
        base_commit: Option<String>,
        reasoning_effort: Option<String>,
        allow_broad_cwd: bool,
    },
    HelpTopic {
        topic: LocalHelpTopic,
        output_format: CliOutputFormat,
    },
    // prompt-mode formatting is only supported for non-interactive runs
    Help {
        output_format: CliOutputFormat,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalHelpTopic {
    Status,
    Sandbox,
    Doctor,
    Acp,
    // #141: extend the local-help pattern to every subcommand so
    // `claw <subcommand> --help` has one consistent contract.
    Init,
    State,
    Resume,
    Session,
    Compact,
    Export,
    Version,
    SystemPrompt,
    DumpManifests,
    BootstrapPlan,
    // #720: subsystem help topics so `claw help agents` etc. route to usage JSON
    Agents,
    Skills,
    Plugins,
    Mcp,
    Config,
    Model,
    Settings,
    Diff,
    Setup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliOutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormatSource {
    Default,
    Env,
    Flag,
}

impl OutputFormatSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Env => "env",
            Self::Flag => "flag",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputFormatSelection {
    format: CliOutputFormat,
    source: OutputFormatSource,
    raw: Option<String>,
    overridden: Vec<String>,
}

impl Default for OutputFormatSelection {
    fn default() -> Self {
        Self {
            format: CliOutputFormat::Text,
            source: OutputFormatSource::Default,
            raw: None,
            overridden: Vec::new(),
        }
    }
}

static OUTPUT_FORMAT_SELECTION: OnceLock<Mutex<OutputFormatSelection>> = OnceLock::new();
// #468: duplicate global flag occurrences for provenance reporting
static DUPLICATE_FLAGS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn output_format_selection_cell() -> &'static Mutex<OutputFormatSelection> {
    OUTPUT_FORMAT_SELECTION.get_or_init(|| Mutex::new(OutputFormatSelection::default()))
}

fn duplicate_flags_cell() -> &'static Mutex<Vec<String>> {
    DUPLICATE_FLAGS.get_or_init(|| Mutex::new(Vec::new()))
}

fn push_duplicate_flag(flag: &str) {
    if let Ok(mut flags) = duplicate_flags_cell().lock() {
        flags.push(flag.to_string());
    }
}

fn take_duplicate_flags() -> Vec<String> {
    duplicate_flags_cell()
        .lock()
        .map(|mut flags| std::mem::take(&mut *flags))
        .unwrap_or_default()
}

fn set_current_output_format_selection(selection: &OutputFormatSelection) {
    *output_format_selection_cell()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = selection.clone();
}

fn current_output_format_selection() -> OutputFormatSelection {
    output_format_selection_cell()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn cli_has_output_format_flag(args: &[String]) -> bool {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--output-format" || arg.starts_with("--output-format="))
}

fn raw_args_request_json_output(args: &[String]) -> bool {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            break;
        }
        if arg == "--output-format" {
            if let Some(value) = args.get(index + 1) {
                values.push(value.as_str());
            }
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--output-format=") {
            values.push(value);
        }
        index += 1;
    }
    if let Some(value) = values.last() {
        let value = value.trim();
        return !value.eq_ignore_ascii_case("text");
    }
    env::var("CLAW_OUTPUT_FORMAT").ok().is_some_and(|value| {
        let value = value.trim();
        !value.is_empty() && !value.eq_ignore_ascii_case("text")
    })
}

fn output_format_selection_from_env() -> Result<OutputFormatSelection, String> {
    match env::var("CLAW_OUTPUT_FORMAT") {
        Ok(raw) if !raw.trim().is_empty() => Ok(OutputFormatSelection {
            format: CliOutputFormat::parse(&raw)?,
            source: OutputFormatSource::Env,
            raw: Some(raw),
            overridden: Vec::new(),
        }),
        _ => Ok(OutputFormatSelection::default()),
    }
}

fn apply_output_format_flag(
    selection: &mut OutputFormatSelection,
    value: &str,
) -> Result<CliOutputFormat, String> {
    let parsed = CliOutputFormat::parse(value)?;
    if selection.source == OutputFormatSource::Flag {
        let previous = selection
            .raw
            .clone()
            .unwrap_or_else(|| selection.format.as_str().to_string());
        eprintln!("warning: --output-format specified multiple times; using last value '{value}'");
        selection.overridden.push(previous);
    }
    selection.format = parsed;
    selection.source = OutputFormatSource::Flag;
    selection.raw = Some(value.to_string());
    set_current_output_format_selection(selection);
    Ok(parsed)
}
impl CliOutputFormat {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            value if value.eq_ignore_ascii_case("text") => Ok(Self::Text),
            value if value.eq_ignore_ascii_case("json") => Ok(Self::Json),
            other => Err(format!(
                "invalid_output_format: unsupported value for --output-format: {other}\nExpected: text, json\nHint: Use --output-format text or --output-format json."
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

#[allow(clippy::too_many_lines)]
fn parse_args(args: &[String]) -> Result<CliAction, String> {
    let mut model = DEFAULT_MODEL.to_string();
    // #148: when user passes --model/--model=, capture the raw input so we
    // can attribute source: "flag" later. None means no flag was supplied.
    let mut model_flag_raw: Option<String> = None;
    let mut output_format_selection = if cli_has_output_format_flag(args) {
        OutputFormatSelection::default()
    } else {
        output_format_selection_from_env()?
    };
    set_current_output_format_selection(&output_format_selection);
    let mut output_format = output_format_selection.format;
    let mut permission_mode_override = None;
    let mut wants_help = false;
    let mut wants_version = false;
    let mut allowed_tool_values = Vec::new();
    let mut compact = false;
    let mut base_commit: Option<String> = None;
    let mut reasoning_effort: Option<String> = None;
    let mut allow_broad_cwd = false;

    // #755: -p prompt text captured as single token; remaining args continue
    // flag parsing. None until `-p <text>` is seen.
    let mut short_p_prompt: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut positional_after_separator = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--help" | "-h" if rest.is_empty() => {
                wants_help = true;
                index += 1;
            }
            "--help" | "-h"
                if !rest.is_empty()
                    && matches!(rest[0].as_str(), "prompt" | "commit" | "pr" | "issue") =>
            {
                // `--help` following a subcommand that would otherwise forward
                // the arg to the API (e.g. `claw prompt --help`) should show
                // top-level help instead. Subcommands that consume their own
                // args (agents, mcp, plugins, skills) and local help-topic
                // subcommands (status, sandbox, doctor, init, state, export,
                // version, system-prompt, dump-manifests, bootstrap-plan) must
                // NOT be intercepted here — they handle --help in their own
                // dispatch paths via parse_local_help_action(). See #141.
                wants_help = true;
                index += 1;
            }
            "--version" | "-V" => {
                wants_version = true;
                index += 1;
            }
            "--model" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing_flag_value: missing value for --model.\nUsage: --model <provider/model>  e.g. --model anthropic/claude-opus-4-7".to_string())?;
                // #468: track duplicate --model flags
                if model_flag_raw.is_some() {
                    push_duplicate_flag(&format!(
                        "--model (previous: {}, new: {})",
                        model_flag_raw.as_deref().unwrap_or(""),
                        value
                    ));
                }
                let resolved = resolve_model_alias_with_config(value);
                debug!("Resolved --model '{}' -> '{}'", value, resolved);
                validate_model_syntax(&resolved)?;
                model = resolved;
                model_flag_raw = Some(value.clone()); // #148
                index += 2;
            }

            flag if flag.starts_with("--model=") => {
                let value = &flag[8..];
                let resolved = resolve_model_alias_with_config(value);
                debug!("Resolved --model='{}' -> '{}'", value, resolved);
                validate_model_syntax(&resolved)?;
                model = resolved;
                model_flag_raw = Some(value.to_string()); // #148
                index += 1;
            }
            "--output-format" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing_flag_value: missing value for --output-format.\nUsage: --output-format text  or  --output-format json".to_string())?;
                // #468: track duplicate --output-format flags
                if output_format != CliOutputFormat::Text
                    || output_format_selection.format != CliOutputFormat::Text
                {
                    push_duplicate_flag("--output-format (overwriting previous value)");
                }
                output_format = apply_output_format_flag(&mut output_format_selection, value)?;
                index += 2;
            }
            "--permission-mode" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing_flag_value: missing value for --permission-mode.\nUsage: --permission-mode read-only|workspace-write|danger-full-access".to_string())?;
                // #468: track duplicate --permission-mode flags
                if permission_mode_override.is_some() {
                    push_duplicate_flag("--permission-mode (overwriting previous value)");
                }
                permission_mode_override = Some(parse_permission_mode_arg(value)?);
                index += 2;
            }

            flag if flag.starts_with("--output-format=") => {
                output_format =
                    apply_output_format_flag(&mut output_format_selection, &flag[16..])?;
                index += 1;
            }
            flag if flag.starts_with("--permission-mode=") => {
                permission_mode_override = Some(parse_permission_mode_arg(&flag[18..])?);
                index += 1;
            }
            "--dangerously-skip-permissions" | "--skip-permissions" => {
                permission_mode_override = Some(PermissionMode::DangerFullAccess);
                index += 1;
            }
            "--compact" => {
                compact = true;
                index += 1;
            }
            "--base-commit" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing_flag_value: missing value for --base-commit.\nUsage: --base-commit <git-sha>".to_string())?;
                // #122: validate that base-commit looks like a git SHA (hex, 7-64 chars)
                if value.len() < 7
                    || value.len() > 64
                    || !value.chars().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(format!(
                        "invalid_flag_value: --base-commit expects a hex SHA (7-64 chars), got '{}'.\nUsage: --base-commit <git-sha>",
                        value
                    ));
                }
                base_commit = Some(value.clone());
                index += 2;
            }
            flag if flag.starts_with("--base-commit=") => {
                base_commit = Some(flag[14..].to_string());
                index += 1;
            }
            "--reasoning-effort" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing_flag_value: missing value for --reasoning-effort.\nUsage: --reasoning-effort low|medium|high".to_string())?;
                if !matches!(value.as_str(), "low" | "medium" | "high") {
                    return Err(format!(
                        "invalid_flag_value: invalid value for --reasoning-effort: '{value}'.\nUsage: --reasoning-effort low|medium|high"
                    ));
                }
                reasoning_effort = Some(value.clone());
                index += 2;
            }
            flag if flag.starts_with("--reasoning-effort=") => {
                let value = &flag[19..];
                if !matches!(value, "low" | "medium" | "high") {
                    return Err(format!(
                        "invalid_flag_value: invalid value for --reasoning-effort: '{value}'.\nUsage: --reasoning-effort low|medium|high"
                    ));
                }
                reasoning_effort = Some(value.to_string());
                index += 1;
            }
            "--allow-broad-cwd" => {
                allow_broad_cwd = true;
                index += 1;
            }
            "--" => {
                if rest.is_empty() {
                    positional_after_separator = true;
                    rest.extend(args[index + 1..].iter().cloned());
                } else {
                    rest.push("--".to_string());
                    rest.extend(args[index + 1..].iter().cloned());
                }
                break;
            }
            "-p" => {
                // Claw Code compat: -p "prompt" = one-shot prompt.
                // #755: consume exactly one token so subsequent flags like
                // --model/--output-format are parsed normally instead of
                // being swallowed into the prompt string (#117).
                let next = args.get(index + 1).map(|s| s.as_str());
                match next {
                    None | Some("") => {
                        return Err("missing_prompt: -p requires a prompt string.\nUsage: claw -p <text>  or  claw prompt <text>".to_string());
                    }
                    Some(tok) if tok.starts_with('-') && tok != "--" => {
                        // Looks like a flag, not a prompt. Reject so the user
                        // knows to quote the literal text or use `--`.
                        return Err(format!(
                            "missing_prompt: -p requires a prompt string before flags; got `{tok}`.\nUsage: claw -p <text> --model sonnet  or  claw -p -- {tok} (literal)"
                        ));
                    }
                    Some(tok) => {
                        // `--` sentinel: skip it and take the token after as literal
                        let (prompt_text, skip) = if tok == "--" {
                            match args.get(index + 2) {
                                Some(t) => (t.as_str(), 3usize),
                                None => return Err("missing_prompt: -p -- requires a prompt string after `--`.\nUsage: claw -p -- <text>".to_string()),
                            }
                        } else {
                            (tok, 2usize)
                        };
                        if prompt_text.trim().is_empty() {
                            return Err("missing_prompt: -p requires a non-empty prompt string.\nUsage: claw -p <text>  or  claw prompt <text>".to_string());
                        }
                        short_p_prompt = Some(prompt_text.to_string());
                        index += skip;
                        continue;
                    }
                }
            }
            "--print" => {
                // Claw Code compat: --print makes output non-interactive
                output_format = CliOutputFormat::Text;
                index += 1;
            }
            "--resume" if rest.is_empty() => {
                rest.push("--resume".to_string());
                index += 1;
            }
            // #457: --help after --resume should show resume help, not be consumed as session-id
            "--help" | "-h" if rest.first().map(String::as_str) == Some("--resume") => {
                wants_help = true;
                index += 1;
            }
            flag if rest.is_empty() && flag.starts_with("--resume=") => {
                rest.push("--resume".to_string());
                rest.push(flag[9..].to_string());
                index += 1;
            }
            "--acp" | "-acp" => {
                rest.push("acp".to_string());
                index += 1;
            }
            "--allowedTools" | "--allowed-tools" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(allowed_tools_missing_error)?;
                if value.starts_with('-') || is_known_top_level_subcommand(value) {
                    return Err(allowed_tools_missing_error());
                }
                allowed_tool_values.push(value.clone());
                index += 2;
            }
            flag if flag.starts_with("--allowedTools=") => {
                let value = flag[15..].to_string();
                if value.trim().is_empty() {
                    return Err(allowed_tools_missing_error());
                }
                allowed_tool_values.push(value);
                index += 1;
            }
            flag if flag.starts_with("--allowed-tools=") => {
                let value = flag[16..].to_string();
                if value.trim().is_empty() {
                    return Err(allowed_tools_missing_error());
                }
                allowed_tool_values.push(value);
                index += 1;
            }
            other if rest.is_empty() && other.starts_with('-') => {
                if should_reject_unknown_option_like(other) {
                    return Err(format_unknown_option(other));
                }
                rest.push(other.to_string());
                index += 1;
            }
            other => {
                rest.push(other.to_string());
                index += 1;
            }
        }
    }

    if wants_help {
        // #684: --help before subcommand should still route to subcommand-specific
        // help when the subcommand is one of the local-help-topic commands.
        if let Some(action) = parse_local_help_action(&rest, output_format) {
            return action;
        }
        // When --help was consumed before the subcommand, rest has no help flag.
        // If rest is a simple local-help subcommand with no extra args, route there.
        if !rest.is_empty() && rest[1..].iter().all(|a| is_help_flag(a)) {
            let topic = match rest[0].as_str() {
                "status" => Some(LocalHelpTopic::Status),
                "sandbox" => Some(LocalHelpTopic::Sandbox),
                "doctor" => Some(LocalHelpTopic::Doctor),
                "acp" => Some(LocalHelpTopic::Acp),
                "init" => Some(LocalHelpTopic::Init),
                "setup" => Some(LocalHelpTopic::Setup),
                "state" => Some(LocalHelpTopic::State),
                "resume" => Some(LocalHelpTopic::Resume),
                "session" => Some(LocalHelpTopic::Session),
                "compact" => Some(LocalHelpTopic::Compact),
                "--resume" => Some(LocalHelpTopic::Resume),
                "export" => Some(LocalHelpTopic::Export),
                "version" => Some(LocalHelpTopic::Version),
                "system-prompt" => Some(LocalHelpTopic::SystemPrompt),
                "dump-manifests" => Some(LocalHelpTopic::DumpManifests),
                "bootstrap-plan" => Some(LocalHelpTopic::BootstrapPlan),
                "agents" | "agent" => Some(LocalHelpTopic::Agents),
                "skills" | "skill" => Some(LocalHelpTopic::Skills),
                "plugins" | "plugin" | "marketplace" => Some(LocalHelpTopic::Plugins),
                "mcp" => Some(LocalHelpTopic::Mcp),
                "config" => Some(LocalHelpTopic::Config),
                "model" | "models" => Some(LocalHelpTopic::Model),
                "settings" => Some(LocalHelpTopic::Settings),
                "diff" => Some(LocalHelpTopic::Diff),
                _ => None,
            };
            if let Some(topic) = topic {
                return Ok(CliAction::HelpTopic {
                    topic,
                    output_format,
                });
            }
        }
        return Ok(CliAction::Help { output_format });
    }

    if wants_version {
        return Ok(CliAction::Version { output_format });
    }

    let allowed_tools = normalize_allowed_tools(&allowed_tool_values)?;

    // #755: -p consumed exactly one token; dispatch now that all flags are parsed
    if let Some(prompt) = short_p_prompt {
        return Ok(CliAction::Prompt {
            prompt,
            model: resolve_model_alias_with_config(&model),
            output_format,
            allowed_tools,
            permission_mode: permission_mode_override.unwrap_or_else(default_permission_mode),
            compact,
            base_commit,
            reasoning_effort,
            allow_broad_cwd,
        });
    }

    if positional_after_separator && !rest.is_empty() {
        let permission_mode = permission_mode_override.unwrap_or_else(default_permission_mode);
        return Ok(CliAction::Prompt {
            prompt: rest.join(" "),
            model,
            output_format,
            allowed_tools,
            permission_mode,
            compact,
            base_commit,
            reasoning_effort: reasoning_effort.clone(),
            allow_broad_cwd,
        });
    }

    if rest.is_empty() {
        let permission_mode = permission_mode_override.unwrap_or_else(default_permission_mode);
        let stdin_is_terminal = std::io::stdin().is_terminal();
        if compact && stdin_is_terminal {
            return Err(compact_missing_argument_error());
        }
        // When stdin is not a terminal (pipe/redirect) and no prompt is given on the
        // command line, read stdin as the prompt and dispatch as a one-shot Prompt
        // rather than starting the interactive REPL (which would consume the pipe and
        // print the startup banner, then exit without sending anything to the API).
        if !stdin_is_terminal {
            let mut buf = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
            let piped = buf.trim().to_string();
            if !piped.is_empty() {
                return Ok(CliAction::Prompt {
                    model,
                    prompt: piped,
                    allowed_tools,
                    permission_mode,
                    output_format,
                    compact,
                    base_commit,
                    reasoning_effort,
                    allow_broad_cwd,
                });
            }
            if compact {
                return Err(compact_missing_argument_error());
            }
            // Non-TTY stdin with no piped content: refuse to start the interactive
            // REPL (it would block forever waiting for input that will never arrive).
            // (#696: emit a typed error instead of hanging indefinitely)
            // Skip this guard in test builds (parse_args tests run in non-TTY context).
            #[cfg(not(test))]
            // #746: newline before remediation so split_error_hint populates hint field
            return Err("interactive_only: claw requires an interactive terminal.\nStdin is not a TTY and no prompt was provided — pipe a prompt with `echo 'task' | claw` or run `claw` in an interactive terminal.".into());
        }
        return Ok(CliAction::Repl {
            model,
            allowed_tools,
            permission_mode,
            base_commit,
            reasoning_effort: reasoning_effort.clone(),
            allow_broad_cwd,
        });
    }
    if let Some(action) = parse_local_help_action(&rest, output_format) {
        return action;
    }
    if rest.first().map(String::as_str) == Some("--resume") {
        return parse_resume_args(&rest[1..], output_format, allow_broad_cwd);
    }
    if rest.first().map(String::as_str) == Some("resume") {
        return parse_resume_args(&rest[1..], output_format, allow_broad_cwd);
    }
    // #696: `claw compact` is the bare name of the interactive `/compact`
    // slash command, not a prompt. When extra args such as `--help` appear
    // after the word `compact`, the generic prompt fallback used to send
    // `compact --help` to provider startup and could hang under closed stdin /
    // JSON output. Fail closed before any provider, prompt, TUI, or spinner
    // startup. `claw --resume SESSION.jsonl /compact` remains the supported
    // non-interactive session compaction path.
    if rest.first().map(String::as_str) == Some("compact") {
        return Err(compact_interactive_only_error());
    }
    if let Some(action) = parse_single_word_command_alias(
        &rest,
        &model,
        model_flag_raw.as_deref(),
        permission_mode_override,
        output_format,
        allowed_tools.clone(),
    ) {
        return action;
    }

    // Keep config-backed defaults lazy so pure-local JSON surfaces (notably
    // `claw --output-format json config`) can report config warnings
    // structurally without an earlier default-resolution load writing prose
    // warnings to stderr.
    let permission_mode = || permission_mode_override.unwrap_or_else(default_permission_mode);
    let permission_mode_provenance = || {
        permission_mode_override
            .map(PermissionModeProvenance::from_flag)
            .unwrap_or_else(permission_mode_provenance_for_current_dir)
    };

    // #98: --compact is only meaningful for prompt mode. When a known non-prompt
    // subcommand is being dispatched, reject --compact so callers don't silently
    // lose the flag.
    if compact
        && rest
            .first()
            .map(|s| s.as_str())
            .is_some_and(|s| s != "prompt")
    {
        // Allow compact for the default prompt fallback (unknown tokens).
        // Only reject for known top-level subcommands that don't use compact.
        let first = rest[0].as_str();
        if is_known_top_level_subcommand(first) && first != "prompt" {
            return Err(format!(
                "invalid_flag_value: --compact is only supported with prompt mode.\nUsage: claw --compact \"<prompt>\" or echo \"<prompt>\" | claw --compact"
            ));
        }
    }

    match rest[0].as_str() {
        "dump-manifests" => parse_dump_manifests_args(&rest[1..], output_format),
        "bootstrap-plan" => Ok(CliAction::BootstrapPlan { output_format }),
        "agents" => Ok(CliAction::Agents {
            args: join_optional_args(&rest[1..]),
            output_format,
        }),
        "mcp" => Ok(CliAction::Mcp {
            args: join_optional_args(&rest[1..]),
            output_format,
        }),
        // #145: `plugins` was routed through the prompt fallback because no
        // top-level parser arm produced CliAction::Plugins. That made `claw
        // plugins` (and `claw plugins --help`, `claw plugins list`, ...)
        // attempt an Anthropic network call, surfacing the misleading error
        // `missing Anthropic credentials` even though the command is purely
        // local introspection. Mirror `agents`/`mcp`/`skills`: action is the
        // first positional arg, target is the second.
        // `plugin` (singular) and `marketplace` are aliases for `plugins`.
        // All three must route to the same local handler so that no form
        // falls through to the LLM/prompt path.
        "plugins" | "plugin" | "marketplace" => {
            let tail = &rest[1..];
            let action = tail.first().cloned();
            let target = tail.get(1).cloned();
            if tail.len() > 2 {
                // #797: append \n usage hint so split_error_hint extracts it (parity with #791 config fix)
                return Err(format!(
                    "unexpected extra arguments after `claw {} {}`: {}\nUsage: claw plugins [list|show <id>|install <id>|enable <id>|disable <id>|uninstall <id>|update <id>|help]",
                    rest[0],
                    tail[..2].join(" "),
                    tail[2..].join(" ")
                ));
            }
            Ok(CliAction::Plugins {
                action,
                target,
                output_format,
            })
        }
        // #146: `config` is pure-local read-only introspection (merges
        // `.claw.json` + `.claw/settings.json` from disk, no network, no
        // state mutation). Previously callers had to spin up a session with
        // `claw --resume SESSION.jsonl /config` to see their own config,
        // which is synthetic friction. Accepts an optional section name
        // (env|hooks|model|plugins) matching the slash command shape.
        "config" => {
            let tail = &rest[1..];
            let section = tail.first().cloned();
            if tail.len() > 1 {
                // #791: append \n hint so split_error_hint extracts it and hint is non-null
                return Err(format!(
                    "unexpected extra arguments after `claw config {}`: {}\nUsage: claw config [env|hooks|model|plugins|mcp|settings]",
                    tail[0],
                    tail[1..].join(" ")
                ));
            }
            Ok(CliAction::Config {
                section,
                output_format,
            })
        }
        // #146: `diff` is pure-local (shells out to `git diff --cached` +
        // `git diff`). No session needed to inspect the working tree.
        "diff" => {
            if rest.len() > 1 {
                // #3129: keep malformed `diff ... --output-format json` on the
                // parser/error path, not the prompt/TUI fallback. The newline
                // before Usage is part of the JSON hint contract.
                return Err(unexpected_diff_args_error(&rest[1..]));
            }
            Ok(CliAction::Diff { output_format })
        }
        // `claw permissions <mode>` falls through to the LLM when called
        // with a subcommand argument because parse_single_word_command_alias
        // only intercepts the bare single-word form. Catch all multi-word
        // forms here and return a structured guidance error so no network
        // call or session is created.
        "permissions" => Err(
            "`claw permissions` is a slash command. Start `claw` and run `/permissions` inside the REPL.\n  Usage  /permissions [read-only|workspace-write|danger-full-access]"
                .to_string(),
        ),
        // #767: `claw session bogus` bypassed parse_single_word_command_alias (rest.len()>1),
        // had no match arm, and fell to CliAction::Prompt — reaching the credential gate
        // instead of a structured error. Mirror the guard on `permissions`.
        "session" => {
            // #449: `claw session list` is a pure local filesystem read that
            // requires no API credentials. Route directly to SessionList instead
            // of falling through to the resume/auth path.
            if rest.get(1).map(|s| s.as_str()) == Some("list") {
                Ok(CliAction::SessionList { output_format })
            } else {
                let action_hint = rest.get(1).map_or(String::new(), |a| format!(" (got: `{a}`)" ));
                Err(format!(
                    "interactive_only: `claw session` is a slash command{action_hint}.\nUse `claw --resume SESSION.jsonl /session <action>` or start `claw` and run `/session [list|exists|switch|fork|delete]`."
                ))
            }
        }
        // #770: same fallthrough gap as #767 — these slash commands had no multi-arg match arm
        // and fell to CliAction::Prompt reaching the credential gate when called with args.
        "cost" => Err(
            "interactive_only: `claw cost` is a slash command.\nUse `claw --resume SESSION.jsonl /cost` or start `claw` and run `/cost`."
                .to_string(),
        ),
        "clear" => Err(
            "interactive_only: `claw clear` is a slash command.\nUse `claw --resume SESSION.jsonl /clear [--confirm]` or start `claw` and run `/clear`."
                .to_string(),
        ),
        "memory" => Err(
            "interactive_only: `claw memory` is a slash command.\nStart `claw` and run `/memory` inside the REPL."
                .to_string(),
        ),
        "ultraplan" => Err(
            "interactive_only: `claw ultraplan` is a slash command.\nStart `claw` and run `/ultraplan` inside the REPL."
                .to_string(),
        ),
        "model" | "models" => {
            let tail = &rest[1..];
            let action = tail.first().cloned();
            if tail.len() > 1 {
                return Err(format!(
                    "unexpected extra arguments after `claw {} {}`: {}\nUsage: claw {} [help] [--output-format json]",
                    rest[0],
                    tail[0],
                    tail[1..].join(" "),
                    rest[0]
                ));
            }
            Ok(CliAction::Models {
                action,
                output_format,
            })
        }
        // #771: usage/stats/fork are slash-only verbs with no multi-arg match arms
        "usage" => Err(
            "interactive_only: `claw usage` is a slash command.\nUse `claw --resume SESSION.jsonl /usage` or start `claw` and run `/usage`."
                .to_string(),
        ),
        "stats" => Err(
            "interactive_only: `claw stats` is a slash command.\nUse `claw --resume SESSION.jsonl /stats` or start `claw` and run `/stats`."
                .to_string(),
        ),
        "fork" => Err(
            "interactive_only: `claw fork` is a slash command.\nStart `claw` and run `/session fork [branch-name]` inside the REPL."
                .to_string(),
        ),
        "skills" => {
            let args = join_optional_args(&rest[1..]);
            if let Some(action) = args.as_deref() {
                let first_word = action.split_whitespace().next().unwrap_or(action);
                if matches!(first_word, "add") {
                    return Err(format!(
                        "unsupported skills action: {first_word}. Supported actions: list, show <name>, install <path>, uninstall <name>, help, or <skill> [args]"
                    ));
                }
            }
            match classify_skills_slash_command(args.as_deref()) {
                SkillSlashDispatch::Invoke(prompt) => Ok(CliAction::Prompt {
                    prompt,
                    model,
                    output_format,
                    allowed_tools,
                    permission_mode: permission_mode(),
                    compact,
                    base_commit,
                    reasoning_effort: reasoning_effort.clone(),
                    allow_broad_cwd,
                }),
                SkillSlashDispatch::Local => Ok(CliAction::Skills {
                    args,
                    output_format,
                }),
            }
        }
        "settings" => {
            let tail = &rest[1..];
            if tail.is_empty() {
                Ok(CliAction::Config {
                    section: Some("settings".to_string()),
                    output_format,
                })
            } else if tail.len() == 1 && matches!(tail[0].as_str(), "help" | "--help" | "-h") {
                Ok(CliAction::HelpTopic {
                    topic: LocalHelpTopic::Settings,
                    output_format,
                })
            } else {
                Err(format!(
                    "unexpected extra arguments after `claw settings`: {}\nUsage: claw settings [help] [--output-format json]",
                    tail.join(" ")
                ))
            }
        }
        "system-prompt" => parse_system_prompt_args(&rest[1..], model, output_format),
        "acp" => parse_acp_args(&rest[1..], output_format),
        "login" | "logout" => Err(removed_auth_surface_error(rest[0].as_str())),
        "init" => {
            // #771: extra positional args to `init` were silently ignored — now rejected
            if rest.len() > 1 {
                let extra = rest[1..].join(" ");
                return Err(format!(
                    "unexpected extra arguments after `claw init`: {extra}\nUsage: claw init [--cwd <dir>] [--date <date>] [--session <session-id>]"
                ));
            }
            Ok(CliAction::Init { output_format })
        }
        "setup" => {
            if rest.len() > 1 {
                let extra = rest[1..].join(" ");
                return Err(format!(
                    "unexpected extra arguments after `claw setup`: {extra}\nUsage: claw setup"
                ));
            }
            Ok(CliAction::Setup { output_format })
        }
        "export" => parse_export_args(&rest[1..], output_format),
        "prompt" => {
            let mut read_stdin = false;
            let prompt_parts = rest[1..]
                .iter()
                .filter_map(|arg| {
                    if matches!(arg.as_str(), "--stdin" | "--prompt-stdin") {
                        read_stdin = true;
                        None
                    } else {
                        Some(arg.as_str())
                    }
                })
                .collect::<Vec<_>>();
            let positional_prompt = prompt_parts.join(" ");
            let stdin_prompt = if read_stdin || positional_prompt.trim().is_empty() {
                read_piped_stdin()
            } else {
                None
            };
            let prompt = if read_stdin {
                merge_prompt_with_stdin(&positional_prompt, stdin_prompt.as_deref())
            } else {
                stdin_prompt
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or(&positional_prompt)
                    .to_string()
            };
            if prompt.trim().is_empty() {
                // #750/#823/#423: provide error_kind-compatible prefix + \n for hint extraction.
                return Err("missing_prompt: prompt subcommand requires a prompt string.
Usage: claw prompt <text>  or  echo '<text>' | claw prompt".to_string());
            }
            Ok(CliAction::Prompt {
                prompt,
                model,
                output_format,
                allowed_tools,
                permission_mode: permission_mode(),
                compact,
                base_commit: base_commit.clone(),
                reasoning_effort: reasoning_effort.clone(),
                allow_broad_cwd,
            })
        }
        other if other.starts_with('/') => parse_direct_slash_cli_action(
            &rest,
            model,
            output_format,
            allowed_tools,
            permission_mode_provenance(),
            compact,
            base_commit,
            reasoning_effort,
            allow_broad_cwd,
        ),
        other => {
            if !compact
                && !other.starts_with('-')
                && looks_like_subcommand_typo(other)
                && (rest.len() == 1
                    || (output_format == CliOutputFormat::Json && model_flag_raw.is_none()))
            {
                // #825/#826: emit command_not_found before provider startup for
                // command-shaped tokens that do not match known subcommands.
                // Text-mode multi-word prompt shorthand remains available, but
                // JSON-mode automation must not turn an unknown command into a
                // credential-gated prompt request.
                let mut message = format!("command_not_found: unknown subcommand: {other}.");
                if let Some(suggestions) = suggest_similar_subcommand(other) {
                    if let Some(line) = render_suggestion_line("Did you mean", &suggestions) {
                        message.push('\n');
                        message.push_str(&line);
                    }
                }
                message.push_str(
                    "\nRun `claw --help` for the full list. If you meant to send a prompt literally, use `claw prompt <text>`.",
                );
                return Err(message);
            }
            // #147: guard empty/whitespace-only prompts at the fallthrough
            // path the same way `"prompt"` arm above does. Without this,
            // `claw ""`, `claw "   "`, and `claw "" ""` silently route to
            // the Anthropic call and surface a misleading
            // `missing Anthropic credentials` error (or burn API tokens on
            // an empty prompt when credentials are present).
            let joined = rest.join(" ");
            if joined.trim().is_empty() {
                // #798: add \n hint so split_error_hint extracts it (was empty_prompt + null)
                return Err(
                    "empty prompt: provide a subcommand or a non-empty prompt string.\nUsage: claw <subcommand> or claw -p <prompt>. Run `claw --help` for the full list."
                        .to_string(),
                );
            }
            Ok(CliAction::Prompt {
                prompt: joined,
                model,
                output_format,
                allowed_tools,
                permission_mode: permission_mode(),
                compact,
                base_commit,
                reasoning_effort: reasoning_effort.clone(),
                allow_broad_cwd,
            })
        }
    }
}

fn parse_local_help_action(
    rest: &[String],
    output_format: CliOutputFormat,
) -> Option<Result<CliAction, String>> {
    if rest.is_empty() {
        return None;
    }
    if !rest.iter().any(|a| is_help_flag(a)) {
        return None;
    }

    let topic = match rest[0].as_str() {
        "status" => LocalHelpTopic::Status,
        "sandbox" => LocalHelpTopic::Sandbox,
        "doctor" => LocalHelpTopic::Doctor,
        "acp" => LocalHelpTopic::Acp,
        "init" => LocalHelpTopic::Init,
        "setup" => LocalHelpTopic::Setup,
        "state" => LocalHelpTopic::State,
        "export" => LocalHelpTopic::Export,
        "version" => LocalHelpTopic::Version,
        "system-prompt" => LocalHelpTopic::SystemPrompt,
        "dump-manifests" => LocalHelpTopic::DumpManifests,
        "bootstrap-plan" => LocalHelpTopic::BootstrapPlan,
        "resume" | "--resume" => LocalHelpTopic::Resume,
        "session" => LocalHelpTopic::Session,
        "compact" => LocalHelpTopic::Compact,
        "model" | "models" => LocalHelpTopic::Model,
        "settings" => LocalHelpTopic::Settings,
        _ => return None,
    };
    let has_non_help = rest[1..].iter().any(|a| !is_help_flag(a));
    if has_non_help {
        return None;
    }
    Some(Ok(CliAction::HelpTopic {
        topic,
        output_format,
    }))
}

fn is_help_flag(value: &str) -> bool {
    matches!(value, "--help" | "-h")
}

fn parse_single_word_command_alias(
    rest: &[String],
    model: &str,
    // #148: raw --model flag input for status provenance. None = no flag.
    model_flag_raw: Option<&str>,
    permission_mode_override: Option<PermissionMode>,
    output_format: CliOutputFormat,
    allowed_tools: Option<AllowedToolSet>,
) -> Option<Result<CliAction, String>> {
    if rest.is_empty() {
        return None;
    }

    // Diagnostic verbs (help, version, status, sandbox, doctor, state) accept only the verb itself
    // or --help / -h as a suffix. Any other suffix args are unrecognized.
    let verb = &rest[0];
    let is_diagnostic = matches!(
        verb.as_str(),
        "help" | "version" | "status" | "sandbox" | "doctor" | "setup" | "state"
    );

    if is_diagnostic && rest.len() > 1 {
        // Diagnostic verb with trailing args: reject unrecognized suffix
        let all_extra_are_help = rest[1..].iter().all(|a| is_help_flag(a));
        if all_extra_are_help {
            // "doctor --help -h" is valid, routed to parse_local_help_action() instead
            return None;
        }
        // #720: `claw help <topic>` — when the verb is "help" and exactly one
        // non-flag argument follows, try to route to the topic's handler.
        if verb == "help" && rest.len() == 2 {
            let topic_name = rest[1].as_str();
            let topic = match topic_name {
                "status" => Some(LocalHelpTopic::Status),
                "sandbox" => Some(LocalHelpTopic::Sandbox),
                "doctor" => Some(LocalHelpTopic::Doctor),
                "acp" => Some(LocalHelpTopic::Acp),
                "init" => Some(LocalHelpTopic::Init),
                "setup" => Some(LocalHelpTopic::Setup),
                "state" => Some(LocalHelpTopic::State),
                "export" => Some(LocalHelpTopic::Export),
                "version" => Some(LocalHelpTopic::Version),
                "system-prompt" => Some(LocalHelpTopic::SystemPrompt),
                "dump-manifests" => Some(LocalHelpTopic::DumpManifests),
                "bootstrap-plan" => Some(LocalHelpTopic::BootstrapPlan),
                "resume" => Some(LocalHelpTopic::Resume),
                "session" => Some(LocalHelpTopic::Session),
                "compact" => Some(LocalHelpTopic::Compact),
                "agents" | "agent" => Some(LocalHelpTopic::Agents),
                "skills" | "skill" => Some(LocalHelpTopic::Skills),
                "plugins" | "plugin" | "marketplace" => Some(LocalHelpTopic::Plugins),
                "mcp" => Some(LocalHelpTopic::Mcp),
                "config" => Some(LocalHelpTopic::Config),
                "model" | "models" => Some(LocalHelpTopic::Model),
                "settings" => Some(LocalHelpTopic::Settings),
                "diff" => Some(LocalHelpTopic::Diff),
                _ => None,
            };
            if let Some(t) = topic {
                return Some(Ok(CliAction::HelpTopic {
                    topic: t,
                    output_format,
                }));
            }
            // Unknown topic: fall through to generic help.
            return Some(Ok(CliAction::Help { output_format }));
        }
        // Unrecognized suffix like "--json"
        let mut msg = format!(
            "unrecognized argument `{}` for subcommand `{}`",
            rest[1], verb
        );
        // #152: common mistake — users type `--json` expecting JSON output.
        // Hint at the correct flag so they don't have to re-read --help.
        if rest[1] == "--json" {
            msg.push_str("\nDid you mean `--output-format json`?");
        } else {
            // #752: generic fallback hint so cli_parse errors always have non-null hint
            msg.push_str(&format!("\nRun `claw {} --help` for usage.", verb));
        }
        return Some(Err(msg));
    }

    // #720: `claw help <topic>` — when `help` is the verb and a topic follows,
    // try to route to the topic's help handler instead of erroring.
    if rest.len() == 2 && rest[0] == "help" {
        let topic_name = rest[1].as_str();
        let topic = match topic_name {
            "status" => Some(LocalHelpTopic::Status),
            "sandbox" => Some(LocalHelpTopic::Sandbox),
            "doctor" => Some(LocalHelpTopic::Doctor),
            "acp" => Some(LocalHelpTopic::Acp),
            "init" => Some(LocalHelpTopic::Init),
            "setup" => Some(LocalHelpTopic::Setup),
            "state" => Some(LocalHelpTopic::State),
            "export" => Some(LocalHelpTopic::Export),
            "version" => Some(LocalHelpTopic::Version),
            "system-prompt" => Some(LocalHelpTopic::SystemPrompt),
            "dump-manifests" => Some(LocalHelpTopic::DumpManifests),
            "bootstrap-plan" => Some(LocalHelpTopic::BootstrapPlan),
            "resume" => Some(LocalHelpTopic::Resume),
            "session" => Some(LocalHelpTopic::Session),
            "compact" => Some(LocalHelpTopic::Compact),
            "agents" | "agent" => Some(LocalHelpTopic::Agents),
            "skills" | "skill" => Some(LocalHelpTopic::Skills),
            "plugins" | "plugin" | "marketplace" => Some(LocalHelpTopic::Plugins),
            "mcp" => Some(LocalHelpTopic::Mcp),
            "config" => Some(LocalHelpTopic::Config),
            "model" | "models" => Some(LocalHelpTopic::Model),
            "settings" => Some(LocalHelpTopic::Settings),
            "diff" => Some(LocalHelpTopic::Diff),
            _ => None,
        };
        if let Some(t) = topic {
            return Some(Ok(CliAction::HelpTopic {
                topic: t,
                output_format,
            }));
        }
        // Unknown topic falls through to the generic help action.
        return Some(Ok(CliAction::Help { output_format }));
    }

    // #453: fire guard for multi-word CLI subcommands too (claw cost list, claw model list, etc.)
    // For slash commands that are commonly used as prompts (explain, cost, tokens, etc.),
    // only fire the guard when there's exactly one token.
    if rest.is_empty() {
        return None;
    }
    // Known CLI subcommands that don't accept additional arguments
    const CLI_SUBCOMMANDS: &[&str] = &[
        "help", "version", "status", "sandbox", "doctor", "state", "config", "diff",
    ];
    if rest.len() > 1 && !CLI_SUBCOMMANDS.contains(&rest[0].as_str()) {
        return None;
    }

    match rest[0].as_str() {
        "help" => Some(Ok(CliAction::Help { output_format })),
        "version" => Some(Ok(CliAction::Version { output_format })),
        "status" => Some(Ok(CliAction::Status {
            model: model.to_string(),
            model_flag_raw: model_flag_raw.map(str::to_string), // #148
            permission_mode: permission_mode_override
                .map(PermissionModeProvenance::from_flag)
                .unwrap_or_else(permission_mode_provenance_for_current_dir),
            output_format,
            allowed_tools,
        })),
        "sandbox" => Some(Ok(CliAction::Sandbox { output_format })),
        "doctor" => Some(Ok(CliAction::Doctor {
            output_format,
            permission_mode: permission_mode_override
                .map(PermissionModeProvenance::from_flag)
                .unwrap_or_else(permission_mode_provenance_for_current_dir),
        })),
        "setup" => Some(Ok(CliAction::Setup { output_format })),
        "state" => Some(Ok(CliAction::State { output_format })),
        // #146: let `config` and `diff` fall through to parse_subcommand
        // where they are wired as pure-local introspection, instead of
        // producing the "is a slash command" guidance. Zero-arg cases
        // reach parse_subcommand too via this None.
        "config" | "diff" => None,
        other => bare_slash_command_guidance(other).map(Err),
    }
}

fn bare_slash_command_guidance(command_name: &str) -> Option<String> {
    if matches!(
        command_name,
        "dump-manifests"
            | "bootstrap-plan"
            | "agents"
            | "mcp"
            | "plugin"
            | "plugins"
            | "marketplace"
            | "skills"
            | "system-prompt"
            | "init"
            | "prompt"
            | "export"
    ) {
        return None;
    }
    let slash_command = slash_command_specs()
        .iter()
        // #772: check both spec.name and spec.aliases for command-line invocations
        .find(|spec| spec.name == command_name || spec.aliases.contains(&command_name))?;
    let canonical_name = slash_command.name;
    // #745: newline before remediation text so split_error_hint populates hint field
    let guidance = if slash_command.resume_supported {
        format!(
            "`claw {command_name}` is a slash command.\nUse `claw --resume SESSION.jsonl /{canonical_name}` or start `claw` and run `/{canonical_name}`."
        )
    } else {
        format!(
            "`claw {command_name}` is a slash command.\nStart `claw` and run `/{canonical_name}` inside the REPL."
        )
    };
    // #772: help text still mentions the alias, but the remediation shows canonical form
    Some(guidance)
}

fn compact_interactive_only_error() -> String {
    // #749: newline before remediation so split_error_hint populates hint field
    "interactive_only: `claw compact` is an interactive/session command.\nStart `claw` and run `/compact`, or use `claw --resume SESSION.jsonl /compact` to compact an existing session."
        .to_string()
}

fn removed_auth_surface_error(command_name: &str) -> String {
    // #765: two-line format so split_error_hint() extracts hint into JSON envelope
    format!(
        "`claw {command_name}` has been removed.\nSet ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN instead."
    )
}

fn unexpected_diff_args_error(extra: &[String]) -> String {
    format!(
        "unexpected extra arguments after `claw diff`: {}\nUsage: claw diff",
        extra.join(" ")
    )
}

fn parse_acp_args(args: &[String], output_format: CliOutputFormat) -> Result<CliAction, String> {
    match args {
        [] => Ok(CliAction::Acp { output_format }),
        [subcommand] if subcommand == "serve" => Ok(CliAction::Acp { output_format }),
        _ => Err(String::from(
            "unsupported_acp_invocation: unsupported ACP invocation. Use `claw acp` or `claw acp serve`.\nACP/Zed editor integration is not implemented yet; `claw acp serve` reports status only.",
        )),
    }
}

fn try_resolve_bare_skill_prompt(cwd: &Path, trimmed: &str) -> Option<String> {
    let bare_first_token = trimmed.split_whitespace().next().unwrap_or_default();
    let looks_like_skill_name = !bare_first_token.is_empty()
        && !bare_first_token.starts_with('/')
        && bare_first_token
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_');
    if !looks_like_skill_name {
        return None;
    }
    match resolve_skill_invocation(cwd, Some(trimmed)) {
        Ok(SkillSlashDispatch::Invoke(prompt)) => Some(prompt),
        _ => None,
    }
}

fn join_optional_args(args: &[String]) -> Option<String> {
    let joined = args.join(" ");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn parse_direct_slash_cli_action(
    rest: &[String],
    model: String,
    output_format: CliOutputFormat,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionModeProvenance,
    compact: bool,
    base_commit: Option<String>,
    reasoning_effort: Option<String>,
    allow_broad_cwd: bool,
) -> Result<CliAction, String> {
    let raw = rest.join(" ");
    match SlashCommand::parse(&raw) {
        Ok(Some(SlashCommand::Help)) => Ok(CliAction::Help { output_format }),
        Ok(Some(SlashCommand::Status)) => Ok(CliAction::Status {
            model,
            model_flag_raw: None,
            permission_mode,
            output_format,
            allowed_tools,
        }),
        Ok(Some(SlashCommand::Sandbox)) => Ok(CliAction::Sandbox { output_format }),
        Ok(Some(SlashCommand::Diff)) => Ok(CliAction::Diff { output_format }),
        Ok(Some(SlashCommand::Version)) => Ok(CliAction::Version { output_format }),
        Ok(Some(SlashCommand::Doctor)) => Ok(CliAction::Doctor {
            output_format,
            permission_mode,
        }),
        Ok(Some(SlashCommand::Agents { args })) => Ok(CliAction::Agents {
            args,
            output_format,
        }),
        Ok(Some(SlashCommand::Mcp { action, target })) => Ok(CliAction::Mcp {
            args: match (action, target) {
                (None, None) => None,
                (Some(action), None) => Some(action),
                (Some(action), Some(target)) => Some(format!("{action} {target}")),
                (None, Some(target)) => Some(target),
            },
            output_format,
        }),
        Ok(Some(SlashCommand::Skills { args })) => {
            match classify_skills_slash_command(args.as_deref()) {
                SkillSlashDispatch::Invoke(prompt) => Ok(CliAction::Prompt {
                    prompt,
                    model,
                    output_format,
                    allowed_tools,
                    permission_mode: permission_mode.mode,
                    compact,
                    base_commit,
                    reasoning_effort: reasoning_effort.clone(),
                    allow_broad_cwd,
                }),
                SkillSlashDispatch::Local => Ok(CliAction::Skills {
                    args,
                    output_format,
                }),
            }
        }
        Ok(Some(SlashCommand::Unknown(name))) => {
            // #828: /approve and /deny are valid REPL-only slash commands that
            // are not SlashCommand enum variants (they require an active tool
            // call in the REPL to be meaningful). Emit interactive_only so
            // machine consumers see the correct error_kind instead of
            // unknown_slash_command.
            if matches!(name.as_str(), "approve" | "yes" | "y" | "deny" | "no" | "n") {
                Err(format!(
                    "interactive_only: /{name} requires an active tool call in the REPL.\nStart `claw` and use /{name} to approve or deny a pending tool execution."
                ))
            } else {
                Err(format_unknown_direct_slash_command(&name))
            }
        }
        Ok(Some(command)) => Err({
            let _ = command;
            let command_name = &rest[0];
            // #829: only suggest --resume when the command is actually
            // resume-safe. Non-resume-safe commands (e.g. /commit, /pr)
            // previously suggested --resume, which just re-triggered
            // interactive_only on a second invocation.
            let bare_name = command_name.trim_start_matches('/');
            let is_resume_safe = commands::resume_supported_slash_commands()
                .iter()
                .any(|spec| spec.name == bare_name);
            if is_resume_safe {
                format!(
                    // #738: newline before remediation so split_error_hint populates hint field
                    "interactive_only: slash command {command_name} requires a live session.\nStart `claw` and run it there, or use `claw --resume SESSION.jsonl {command_name}` / `claw --resume {latest} {command_name}`.",
                    latest = LATEST_SESSION_REFERENCE,
                )
            } else {
                format!(
                    "interactive_only: slash command {command_name} requires a live REPL session.\nStart `claw` and run it there."
                )
            }
        }),
        Ok(None) => Err(format!("unknown subcommand: {}", rest[0])),
        Err(error) => Err(error.to_string()),
    }
}

fn format_unknown_option(option: &str) -> String {
    if option == "--" {
        return "end_of_flags: `--` terminates flag parsing. Pass literal prompt text after it, for example `claw -- \"-literal prompt\"`.\nRun `claw --help` for usage.".to_string();
    }
    let mut message = format!("unknown option: {option}");
    if let Some(suggestion) = suggest_closest_term(option, CLI_OPTION_SUGGESTIONS) {
        message.push_str("\nDid you mean ");
        message.push_str(suggestion);
        message.push('?');
    }
    message.push_str("\nRun `claw --help` for usage.");
    message
}

fn format_unknown_direct_slash_command(name: &str) -> String {
    // #827: prefix with classifier-friendly token so classify_error_kind
    // returns "unknown_slash_command" instead of the opaque fallback.
    let mut message =
        format!("unknown_slash_command: unknown slash command outside the REPL: /{name}");
    if let Some(suggestions) = render_suggestion_line("Did you mean", &suggest_slash_commands(name))
    {
        message.push('\n');
        message.push_str(&suggestions);
    }
    if let Some(note) = omc_compatibility_note_for_unknown_slash_command(name) {
        message.push('\n');
        message.push_str(note);
    }
    message.push_str("\nRun `claw --help` for CLI usage, or start `claw` and use /help.");
    message
}

fn format_unknown_slash_command(name: &str) -> String {
    // #827: prefix with classifier-friendly token so classify_error_kind
    // can return "unknown_slash_command" instead of the opaque fallback.
    let mut message = format!("unknown_slash_command: Unknown slash command: /{name}");
    if let Some(suggestions) = render_suggestion_line("Did you mean", &suggest_slash_commands(name))
    {
        message.push('\n');
        message.push_str(&suggestions);
    }
    if let Some(note) = omc_compatibility_note_for_unknown_slash_command(name) {
        message.push('\n');
        message.push_str(note);
    }
    message.push_str("\n  Help             /help lists available slash commands");
    message
}

fn omc_compatibility_note_for_unknown_slash_command(name: &str) -> Option<&'static str> {
    name.starts_with("oh-my-claudecode:")
        .then_some(
            "Compatibility note: `/oh-my-claudecode:*` is a Claude Code/OMC plugin command. `claw` does not yet load plugin slash commands, Claude statusline stdin, or OMC session hooks.",
        )
}

fn render_suggestion_line(label: &str, suggestions: &[String]) -> Option<String> {
    (!suggestions.is_empty()).then(|| format!("  {label:<16} {}", suggestions.join(", "),))
}

fn suggest_slash_commands(input: &str) -> Vec<String> {
    let mut candidates = slash_command_specs()
        .iter()
        .flat_map(|spec| {
            std::iter::once(spec.name)
                .chain(spec.aliases.iter().copied())
                .map(|name| format!("/{name}"))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    let candidate_refs = candidates.iter().map(String::as_str).collect::<Vec<_>>();
    ranked_suggestions(input.trim_start_matches('/'), &candidate_refs)
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn suggest_closest_term<'a>(input: &str, candidates: &'a [&'a str]) -> Option<&'a str> {
    ranked_suggestions(input, candidates).into_iter().next()
}

fn suggest_similar_subcommand(input: &str) -> Option<Vec<String>> {
    const KNOWN_SUBCOMMANDS: &[&str] = &[
        "help",
        "version",
        "status",
        "sandbox",
        "doctor",
        "setup",
        "state",
        "dump-manifests",
        "bootstrap-plan",
        "agents",
        "mcp",
        "skills",
        "system-prompt",
        "acp",
        "init",
        "export",
        "prompt",
        "list",
    ];

    let normalized_input = input.to_ascii_lowercase();
    let mut ranked = KNOWN_SUBCOMMANDS
        .iter()
        .filter_map(|candidate| {
            let normalized_candidate = candidate.to_ascii_lowercase();
            let distance = levenshtein_distance(&normalized_input, &normalized_candidate);
            let prefix_match = common_prefix_len(&normalized_input, &normalized_candidate) >= 4;
            let substring_match = normalized_candidate.contains(&normalized_input)
                || normalized_input.contains(&normalized_candidate);
            ((distance <= 2) || prefix_match || substring_match).then_some((distance, *candidate))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| left.cmp(right).then_with(|| left.1.cmp(right.1)));
    ranked.dedup_by(|left, right| left.1 == right.1);
    let suggestions = ranked
        .into_iter()
        .map(|(_, candidate)| candidate.to_string())
        .take(3)
        .collect::<Vec<_>>();
    (!suggestions.is_empty()).then_some(suggestions)
}

fn is_known_top_level_subcommand(value: &str) -> bool {
    matches!(
        value,
        "help"
            | "version"
            | "status"
            | "sandbox"
            | "doctor"
            | "state"
            | "dump-manifests"
            | "bootstrap-plan"
            | "agents"
            | "agent"
            | "mcp"
            | "skills"
            | "skill"
            | "plugins"
            | "plugin"
            | "marketplace"
            | "system-prompt"
            | "acp"
            | "init"
            | "export"
            | "prompt"
            | "resume"
            | "session"
            | "compact"
            | "config"
            | "model"
            | "models"
            | "settings"
            | "diff"
    )
}

fn common_prefix_len(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(l, r)| l == r)
        .count()
}

fn looks_like_subcommand_typo(input: &str) -> bool {
    !input.is_empty()
        && input
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() || ch == '-')
}

fn ranked_suggestions<'a>(input: &str, candidates: &'a [&'a str]) -> Vec<&'a str> {
    let normalized_input = input.trim_start_matches('/').to_ascii_lowercase();
    let mut ranked = candidates
        .iter()
        .filter_map(|candidate| {
            let normalized_candidate = candidate.trim_start_matches('/').to_ascii_lowercase();
            let distance = levenshtein_distance(&normalized_input, &normalized_candidate);
            let prefix_bonus = usize::from(
                !(normalized_candidate.starts_with(&normalized_input)
                    || normalized_input.starts_with(&normalized_candidate)),
            );
            let score = distance + prefix_bonus;
            (score <= 4).then_some((score, *candidate))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| left.cmp(right).then_with(|| left.1.cmp(right.1)));
    ranked
        .into_iter()
        .map(|(_, candidate)| candidate)
        .take(3)
        .collect()
}

fn levenshtein_distance(left: &str, right: &str) -> usize {
    if left.is_empty() {
        return right.chars().count();
    }
    if right.is_empty() {
        return left.chars().count();
    }

    let right_chars = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut current = vec![0; right_chars.len() + 1];

    for (left_index, left_char) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let substitution_cost = usize::from(left_char != *right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution_cost);
        }
        previous.clone_from(&current);
    }

    previous[right_chars.len()]
}

fn resolve_model_alias(model: &str) -> &str {
    match model {
        "opus" => "anthropic/claude-opus-4-7",
        "sonnet" => "anthropic/claude-sonnet-4-6",
        "haiku" => "anthropic/claude-haiku-4-5-20251213",
        _ => model,
    }
}

/// Resolve a model name through user-defined config aliases first, then fall
/// back to the built-in alias table. This is the entry point used wherever a
/// user-supplied model string is about to be dispatched to a provider.
fn resolve_model_alias_with_config(model: &str) -> String {
    let trimmed = model.trim();
    if let Some(resolved) = config_alias_for_current_dir(trimmed) {
        return resolve_model_alias(&resolved).to_string();
    }
    resolve_model_alias(trimmed).to_string()
}

/// Validate model syntax at parse time.
/// Accepts: known aliases (opus, sonnet, haiku) or provider/model pattern.
/// Rejects: empty, whitespace-only, strings with spaces, or invalid chars.
fn validate_model_syntax(model: &str) -> Result<(), String> {
    let trimmed = model.trim();
    // Ollama models use names like "qwen3:8b" that don't match provider/model
    // syntax. Skip strict validation when OLLAMA_HOST is configured.
    if std::env::var_os("OLLAMA_HOST").is_some() {
        if trimmed.is_empty() {
            return Err("invalid model syntax: model string cannot be empty.\nUsage: --model <model-name>  e.g. --model qwen3:8b".to_string());
        }
        return Ok(());
    }
    if trimmed.is_empty() {
        return Err("invalid model syntax: model string cannot be empty.\nUsage: --model <provider/model>  e.g. --model anthropic/claude-opus-4-7".to_string());
    }
    // Check for spaces (malformed)
    if trimmed.contains(' ') {
        return Err(format!(
            "invalid model syntax: '{}' contains spaces.\nUse provider/model format (e.g., anthropic/claude-opus-4-7) or a known alias.",
            trimmed
        ));
    }
    if is_bare_provider_model(trimmed) {
        return Ok(());
    }
    if is_local_openai_model_syntax(trimmed) {
        return Ok(());
    }
    // Check provider/model format: provider_id/model_id
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        // #154: hint if the model looks like it belongs to a different provider
        let mut err_msg = format!(
            "invalid model syntax: '{}'.\nExpected provider/model (e.g., anthropic/claude-opus-4-7)",
            trimmed
        );
        if trimmed.starts_with("gpt-") || trimmed.starts_with("gpt_") {
            err_msg.push_str("\nDid you mean `openai/");
            err_msg.push_str(trimmed);
            err_msg.push_str("`? (Requires OPENAI_API_KEY env var)");
        } else if trimmed.starts_with("qwen") && trimmed.contains(':') {
            err_msg.push_str("\nFor a local Ollama model, set `OPENAI_BASE_URL=http://127.0.0.1:11434/v1` before using tagged names like `");
            err_msg.push_str(trimmed);
            err_msg.push_str("`.");
        } else if trimmed.starts_with("qwen") {
            err_msg.push_str("\nDid you mean `qwen/");
            err_msg.push_str(trimmed);
            err_msg.push_str("`? (Requires DASHSCOPE_API_KEY env var)");
        } else if trimmed.starts_with("grok") {
            err_msg.push_str("\nDid you mean `xai/");
            err_msg.push_str(trimmed);
            err_msg.push_str("`? (Requires XAI_API_KEY env var)");
        }
        return Err(err_msg);
    }
    Ok(())
}

fn is_bare_provider_model(model: &str) -> bool {
    model.starts_with("claude-") || model.starts_with("gpt-")
}

fn is_local_openai_model_syntax(model: &str) -> bool {
    if let Some(rest) = model.strip_prefix("local/") {
        return !rest.is_empty() && rest.split('/').all(|segment| !segment.is_empty());
    }
    std::env::var_os("OPENAI_BASE_URL").is_some() && (model.contains(':') || model.contains('.'))
}

fn config_alias_for_current_dir(alias: &str) -> Option<String> {
    if alias.is_empty() {
        return None;
    }
    let cwd = env::current_dir().ok()?;
    let loader = ConfigLoader::default_for(&cwd);
    let config = loader.load().ok()?;
    config.aliases().get(alias).cloned()
}

fn normalize_allowed_tools(values: &[String]) -> Result<Option<AllowedToolSet>, String> {
    if values.is_empty() {
        return Ok(None);
    }
    current_tool_registry()?.normalize_allowed_tools(values)
}

fn allowed_tools_missing_error() -> String {
    "missing_argument: --allowedTools requires a tool list before subcommands or flags.\nUsage: --allowedTools <tool-name>[,<tool-name>...]  e.g. --allowedTools read,glob".to_string()
}

fn compact_missing_argument_error() -> String {
    "missing_argument: --compact requires prompt text, piped stdin, or a subcommand. argument: prompt or subcommand\nUsage: claw --compact <prompt>  or  echo '<prompt>' | claw --compact"
        .to_string()
}

fn allowed_tool_aliases_json(registry: &GlobalToolRegistry) -> Value {
    Value::Object(
        registry
            .allowed_tool_aliases()
            .into_iter()
            .map(|(alias, canonical)| (alias, Value::String(canonical)))
            .collect(),
    )
}

fn current_tool_registry() -> Result<GlobalToolRegistry, String> {
    let cwd = env::current_dir().map_err(|error| error.to_string())?;
    let loader = ConfigLoader::default_for(&cwd);
    let runtime_config = loader.load().map_err(|error| error.to_string())?;
    let state = build_runtime_plugin_state_with_loader(&cwd, &loader, &runtime_config)
        .map_err(|error| error.to_string())?;
    let registry = state.tool_registry.clone();
    if let Some(mcp_state) = state.mcp_state {
        mcp_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutdown()
            .map_err(|error| error.to_string())?;
    }
    Ok(registry)
}

fn parse_permission_mode_arg(value: &str) -> Result<PermissionMode, String> {
    normalize_permission_mode(value)
        .ok_or_else(|| {
            format!(
                "invalid_permission_mode: unsupported permission mode '{value}'.\nUsage: --permission-mode read-only|workspace-write|danger-full-access"
            )
        })
        .map(permission_mode_from_label)
}

fn permission_mode_from_label(mode: &str) -> PermissionMode {
    match mode {
        "read-only" => PermissionMode::ReadOnly,
        "workspace-write" => PermissionMode::WorkspaceWrite,
        "danger-full-access" => PermissionMode::DangerFullAccess,
        other => panic!("unsupported permission mode label: {other}"),
    }
}

fn permission_mode_from_resolved(mode: ResolvedPermissionMode) -> PermissionMode {
    match mode {
        ResolvedPermissionMode::ReadOnly => PermissionMode::ReadOnly,
        ResolvedPermissionMode::WorkspaceWrite => PermissionMode::WorkspaceWrite,
        ResolvedPermissionMode::DangerFullAccess => PermissionMode::DangerFullAccess,
    }
}

fn default_permission_mode() -> PermissionMode {
    permission_mode_provenance_for_current_dir().mode
}

fn permission_mode_provenance_for_current_dir() -> PermissionModeProvenance {
    if let Some(mode) = env::var("RUSTY_CLAUDE_PERMISSION_MODE")
        .ok()
        .as_deref()
        .and_then(normalize_permission_mode)
        .map(permission_mode_from_label)
    {
        return PermissionModeProvenance {
            mode,
            source: PermissionModeSource::Env,
            env_var: Some("RUSTY_CLAUDE_PERMISSION_MODE"),
        };
    }

    if let Some(mode) = config_permission_mode_for_current_dir() {
        return PermissionModeProvenance {
            mode,
            source: PermissionModeSource::Config,
            env_var: None,
        };
    }

    PermissionModeProvenance::default_fallback()
}

fn config_permission_mode_for_current_dir() -> Option<PermissionMode> {
    let cwd = env::current_dir().ok()?;
    let loader = ConfigLoader::default_for(&cwd);
    loader
        .load()
        .ok()?
        .permission_mode()
        .map(permission_mode_from_resolved)
}

fn config_model_for_current_dir() -> Option<String> {
    let cwd = env::current_dir().ok()?;
    let loader = ConfigLoader::default_for(&cwd);
    loader.load().ok()?.model().map(ToOwned::to_owned)
}

fn resolve_repl_model(cli_model: String) -> Result<String, String> {
    Ok(ModelProvenance::from_env_or_config_or_default(&cli_model)?.resolved)
}

fn print_model_validation_warning_status(
    error: &str,
    usage: StatusUsage,
    permission_mode: &str,
    context: &StatusContext,
    allowed_tools: Option<&AllowedToolSet>,
) -> Result<(), Box<dyn std::error::Error>> {
    let kind = classify_error_kind(error);
    let (short_reason, inline_hint) = split_error_hint(error);
    let hint = inline_hint.or_else(|| fallback_hint_for_error_kind(kind).map(String::from));
    let format_selection = current_output_format_selection();
    let mut value = status_json_value(
        None,
        usage,
        permission_mode,
        context,
        None,
        None,
        allowed_tools,
        Some(&format_selection),
    );
    let object = value
        .as_object_mut()
        .expect("status_json_value should render an object");
    object.insert("status".to_string(), serde_json::json!("warn"));
    object.insert("error_kind".to_string(), serde_json::json!(kind));
    object.insert(
        "model_validation_error".to_string(),
        serde_json::json!(short_reason),
    );
    object.insert(
        "model_validation_error_kind".to_string(),
        serde_json::json!(kind),
    );
    object.insert("model_validation_hint".to_string(), serde_json::json!(hint));
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn provider_label(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Xai => "xai",
        ProviderKind::OpenAi => "openai",
    }
}

fn format_connected_line(model: &str) -> String {
    let provider = provider_label(detect_provider_kind(model));
    format!("Connected: {model} via {provider}")
}

fn filter_tool_specs(
    tool_registry: &GlobalToolRegistry,
    allowed_tools: Option<&AllowedToolSet>,
) -> Vec<ToolDefinition> {
    tool_registry.definitions(allowed_tools)
}

fn parse_system_prompt_args(
    args: &[String],
    model: String,
    output_format: CliOutputFormat,
) -> Result<CliAction, String> {
    let mut cwd = env::current_dir().map_err(|error| error.to_string())?;
    let mut date = DEFAULT_DATE.to_string();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--cwd" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    "missing_flag_value: missing value for --cwd.\nUsage: --cwd <path>".to_string()
                })?;
                cwd = PathBuf::from(value);
                // #99: validate --cwd path exists and is a directory
                if !cwd.exists() {
                    return Err(format!(
                        "invalid_cwd: path '{value}' does not exist.\nUsage: claw system-prompt --cwd <existing-directory>"
                    ));
                }
                if !cwd.is_dir() {
                    return Err(format!(
                        "invalid_cwd: path '{value}' is not a directory.\nUsage: claw system-prompt --cwd <existing-directory>"
                    ));
                }
                index += 2;
            }
            "--date" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    "missing_flag_value: missing value for --date.\nUsage: --date <YYYY-MM-DD>"
                        .to_string()
                })?;
                // #99: validate --date is a plausible date string (no newlines, reasonable length)
                if value.contains('\n') || value.contains('\r') {
                    return Err(format!(
                        "invalid_flag_value: --date value contains invalid characters.\nUsage: --date <YYYY-MM-DD>"
                    ));
                }
                if value.len() > 20 {
                    return Err(format!(
                        "invalid_flag_value: --date value is too long ({len} chars, expected YYYY-MM-DD).\nUsage: --date <YYYY-MM-DD>",
                        len = value.len()
                    ));
                }
                date.clone_from(value);
                index += 2;
            }

            other => {
                // #152: hint `--output-format json` when user types `--json`.
                // #790: use unknown_option: prefix + \n hint so classify_error_kind returns
                // unknown_option and split_error_hint extracts the remediation text.
                let hint = if other == "--json" {
                    "Did you mean `--output-format json`? Usage: claw system-prompt [--cwd <dir>] [--date <YYYY-MM-DD>] [--output-format text|json]".to_string()
                } else {
                    "Usage: claw system-prompt [--cwd <dir>] [--date <YYYY-MM-DD>] [--output-format text|json]".to_string()
                };
                return Err(format!(
                    "unknown_option: unknown system-prompt option: {other}.\n{hint}"
                ));
            }
        }
    }

    Ok(CliAction::PrintSystemPrompt {
        cwd,
        date,
        model,
        output_format,
    })
}

fn parse_export_args(args: &[String], output_format: CliOutputFormat) -> Result<CliAction, String> {
    let mut session_reference = LATEST_SESSION_REFERENCE.to_string();
    let mut output_path: Option<PathBuf> = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--session" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing_flag_value: missing value for --session.\nUsage: --session <session-id>".to_string())?;
                session_reference.clone_from(value);
                index += 2;
            }
            flag if flag.starts_with("--session=") => {
                session_reference = flag[10..].to_string();
                index += 1;
            }
            "--output" | "-o" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| format!("missing_flag_value: missing value for {}.\nUsage: claw export [PATH] [--session SESSION] [--output PATH]", args[index]))?;
                output_path = Some(PathBuf::from(value));
                index += 2;
            }
            flag if flag.starts_with("--output=") => {
                output_path = Some(PathBuf::from(&flag[9..]));
                index += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown_option: unknown export option: {other}.\nRun `claw export --help` for usage."));
            }
            other if output_path.is_none() => {
                output_path = Some(PathBuf::from(other));
                index += 1;
            }
            other => {
                // #784: use typed prefix so classify_error_kind returns unexpected_extra_args
                return Err(format!("unexpected_extra_args: unexpected export argument: {other}.\nUsage: claw export [PATH] [--session SESSION] [--output PATH]"));
            }
        }
    }

    Ok(CliAction::Export {
        session_reference,
        output_path,
        output_format,
    })
}

fn parse_dump_manifests_args(
    args: &[String],
    output_format: CliOutputFormat,
) -> Result<CliAction, String> {
    let mut manifests_dir: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--manifests-dir" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| String::from("missing_flag_value: --manifests-dir requires a path.\nUsage: claw dump-manifests --manifests-dir <path> [--output-format json]"))?;
            manifests_dir = Some(PathBuf::from(value));
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--manifests-dir=") {
            if value.is_empty() {
                // #786: empty --manifests-dir= is also a missing value
                return Err(String::from("missing_flag_value: --manifests-dir requires a path.\nUsage: claw dump-manifests --manifests-dir <path> [--output-format json]"));
            }
            manifests_dir = Some(PathBuf::from(value));
            index += 1;
            continue;
        }
        return Err(format!("unknown_option: unknown dump-manifests option: {arg}.\nRun `claw dump-manifests --help` for usage."));
    }

    Ok(CliAction::DumpManifests {
        output_format,
        manifests_dir,
    })
}

fn parse_resume_args(
    args: &[String],
    output_format: CliOutputFormat,
    allow_broad_cwd: bool,
) -> Result<CliAction, String> {
    let (session_path, command_tokens): (PathBuf, &[String]) = match args.first() {
        None => (PathBuf::from(LATEST_SESSION_REFERENCE), &[]),
        Some(first) if looks_like_slash_command_token(first) => {
            (PathBuf::from(LATEST_SESSION_REFERENCE), args)
        }
        Some(first) => (PathBuf::from(first), &args[1..]),
    };
    let mut commands = Vec::new();
    let mut current_command = String::new();

    for token in command_tokens {
        if token.trim_start().starts_with('/') {
            if resume_command_can_absorb_token(&current_command, token) {
                current_command.push(' ');
                current_command.push_str(token);
                continue;
            }
            if !current_command.is_empty() {
                commands.push(current_command);
            }
            current_command = String::from(token.as_str());
            continue;
        }

        if current_command.is_empty() {
            // #768: typed prefix + \n hint so split_error_hint() extracts hint into JSON envelope
            return Err(format!(
                "invalid_resume_argument: `{token}` is not a slash command.\nUsage: claw --resume <session-id|latest> /<slash-command>  (e.g. /compact, /status)"
            ));
        }

        current_command.push(' ');
        current_command.push_str(token);
    }

    if !current_command.is_empty() {
        commands.push(current_command);
    }

    Ok(CliAction::ResumeSession {
        session_path,
        commands,
        output_format,
        allow_broad_cwd,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagnosticLevel {
    Ok,
    Warn,
    Fail,
}

impl DiagnosticLevel {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }

    fn is_failure(self) -> bool {
        matches!(self, Self::Fail)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiagnosticCheck {
    name: &'static str,
    level: DiagnosticLevel,
    summary: String,
    details: Vec<String>,
    data: Map<String, Value>,
    /// #778: stable remediation hint for warn/fail checks so automation can read
    /// a structured field instead of parsing details_prose.
    hint: Option<String>,
}

impl DiagnosticCheck {
    fn new(name: &'static str, level: DiagnosticLevel, summary: impl Into<String>) -> Self {
        Self {
            name,
            level,
            summary: summary.into(),
            details: Vec::new(),
            data: Map::new(),
            hint: None,
        }
    }

    fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }

    fn with_data(mut self, data: Map<String, Value>) -> Self {
        self.data = data;
        self
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        let h = hint.into();
        if !h.is_empty() {
            self.hint = Some(h);
        }
        self
    }

    fn json_value(&self) -> Value {
        // Derive a stable snake_case id from the check name for machine-readable keying (#704).
        let id = self
            .name
            .to_ascii_lowercase()
            .replace(' ', "_")
            .replace('-', "_");
        let mut value = Map::from_iter([
            ("id".to_string(), Value::String(id.clone())),
            (
                "name".to_string(),
                Value::String(self.name.to_ascii_lowercase()),
            ),
            (
                "status".to_string(),
                Value::String(self.level.label().to_string()),
            ),
            ("summary".to_string(), Value::String(self.summary.clone())),
            (
                // #701 (complete): `details[]` is now the canonical structured form —
                // `{key, value}` objects instead of padded prose strings. The legacy
                // prose representation is preserved as `details_prose[]` for callers
                // that still scrape the formatted strings.
                "details_prose".to_string(),
                Value::Array(
                    self.details
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect::<Vec<_>>(),
                ),
            ),
            (
                // details[] is now structured {key,value} objects (was prose strings).
                "details".to_string(),
                Value::Array(
                    self.details
                        .iter()
                        .map(|s| {
                            // Split on first run of 2+ spaces to separate key from value.
                            let parts: Vec<&str> = s.splitn(2, "  ").collect();
                            if parts.len() == 2 {
                                let k = parts[0].trim().to_string();
                                let v_str = parts[1].trim();
                                let v: Value = if v_str == "true" {
                                    Value::Bool(true)
                                } else if v_str == "false" {
                                    Value::Bool(false)
                                } else if let Ok(n) = v_str.parse::<i64>() {
                                    Value::Number(n.into())
                                } else {
                                    Value::String(v_str.to_string())
                                };
                                json!({"key": k, "value": v})
                            } else {
                                json!({"key": s.trim(), "value": Value::Null})
                            }
                        })
                        .collect::<Vec<_>>(),
                ),
            ),
        ]);
        // #778: include hint field so automation can read remediation without parsing prose
        value.insert(
            "hint".to_string(),
            self.hint
                .as_deref()
                .map(|h| Value::String(h.to_string()))
                .unwrap_or(Value::Null),
        );
        value.extend(self.data.clone());
        Value::Object(value)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ConfigWarningMode {
    EmitStderr,
    SuppressStderr,
}

fn load_config_with_warning_mode(
    loader: &ConfigLoader,
    mode: ConfigWarningMode,
) -> Result<runtime::RuntimeConfig, runtime::ConfigError> {
    match mode {
        ConfigWarningMode::EmitStderr => loader.load(),
        ConfigWarningMode::SuppressStderr => loader
            .load_collecting_warnings()
            .map(|(runtime_config, _warnings)| runtime_config),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoctorReport {
    checks: Vec<DiagnosticCheck>,
}

impl DoctorReport {
    fn counts(&self) -> (usize, usize, usize) {
        (
            self.checks
                .iter()
                .filter(|check| check.level == DiagnosticLevel::Ok)
                .count(),
            self.checks
                .iter()
                .filter(|check| check.level == DiagnosticLevel::Warn)
                .count(),
            self.checks
                .iter()
                .filter(|check| check.level == DiagnosticLevel::Fail)
                .count(),
        )
    }

    fn has_failures(&self) -> bool {
        self.checks.iter().any(|check| check.level.is_failure())
    }

    fn status(&self) -> &'static str {
        let (_, warn_count, fail_count) = self.counts();
        if fail_count > 0 {
            "fail"
        } else if warn_count > 0 {
            "warn"
        } else {
            "ok"
        }
    }

    fn render(&self) -> String {
        let (ok_count, warn_count, fail_count) = self.counts();
        let mut lines = vec![
            "Doctor".to_string(),
            format!(
                "Summary\n  OK               {ok_count}\n  Warnings         {warn_count}\n  Failures         {fail_count}"
            ),
        ];
        lines.extend(self.checks.iter().map(render_diagnostic_check));
        lines.join("\n\n")
    }

    fn json_value(&self) -> Value {
        let report = self.render();
        let (ok_count, warn_count, fail_count) = self.counts();
        let tool_registry = GlobalToolRegistry::builtin();
        json!({
            "kind": "doctor",
            "action": "doctor",
            "status": self.status(),
            "message": report,
            "report": report,
            "has_failures": self.has_failures(),
            "summary": {
                "total": self.checks.len(),
                "ok": ok_count,
                "warnings": warn_count,
                "failures": fail_count,
            },
            "checks": self
                .checks
                .iter()
                .map(DiagnosticCheck::json_value)
                .collect::<Vec<_>>(),
            "allowed_tools": {
                "available": tool_registry.canonical_allowed_tool_names(),
                "aliases": allowed_tool_aliases_json(&tool_registry),
            },
        })
    }
}

fn render_diagnostic_check(check: &DiagnosticCheck) -> String {
    let mut lines = vec![format!(
        "{}\n  Status           {}\n  Summary          {}",
        check.name,
        check.level.label(),
        check.summary
    )];
    if !check.details.is_empty() {
        lines.push("  Details".to_string());
        lines.extend(check.details.iter().map(|detail| format!("    - {detail}")));
    }
    lines.join("\n")
}

fn render_doctor_report(
    config_warning_mode: ConfigWarningMode,
    permission_mode: PermissionModeProvenance,
) -> Result<DoctorReport, Box<dyn std::error::Error>> {
    let cwd = friendly_cwd(env::current_dir()?);
    let config_loader = ConfigLoader::default_for(&cwd);
    let config = load_config_with_warning_mode(&config_loader, config_warning_mode);
    let discovered_config = config_loader.discover();
    let project_context = ProjectContext::discover_with_git(&cwd, DEFAULT_DATE)?;
    let (project_root, git_branch) =
        parse_git_status_metadata(project_context.git_status.as_deref());
    let git_summary = parse_git_workspace_summary(project_context.git_status.as_deref());
    let branch_freshness = BranchFreshness::from_git_status(project_context.git_status.as_deref());
    let stale_base_state = stale_base_state_for(&cwd, None);
    let empty_config = runtime::RuntimeConfig::empty();
    let sandbox_config = config.as_ref().ok().unwrap_or(&empty_config);
    let boot_preflight = build_boot_preflight_snapshot(
        &cwd,
        project_root.as_deref(),
        project_context.git_status.as_deref(),
        config.as_ref().ok(),
        config.as_ref().err().map(ToString::to_string).as_deref(),
    );
    let memory_files = memory_file_summaries_for(
        &cwd,
        project_root.as_deref(),
        &project_context.instruction_files,
    );
    let mcp_validation = config
        .as_ref()
        .ok()
        .map(|runtime_config| McpValidationSummary::from_collection(runtime_config.mcp()))
        .unwrap_or_default();
    let hook_validation = config
        .as_ref()
        .ok()
        .map(HookValidationSummary::from_config)
        .unwrap_or_default();
    let context = StatusContext {
        cwd: cwd.clone(),
        session_path: None,
        loaded_config_files: config
            .as_ref()
            .ok()
            .map_or(0, |runtime_config| runtime_config.loaded_entries().len()),
        discovered_config_files: discovered_config.len(),
        memory_file_count: project_context.instruction_files.len(),
        memory_files: memory_files.clone(),
        unloaded_memory_files: unloaded_memory_candidates(
            &cwd,
            project_root.as_deref(),
            &memory_files,
        ),
        project_root,
        git_branch,
        git_summary,
        branch_freshness,
        stale_base_state,
        session_lifecycle: classify_session_lifecycle_for(&cwd),
        boot_preflight,
        sandbox_status: resolve_sandbox_status(sandbox_config.sandbox(), &cwd),
        binary_provenance: binary_provenance_for(Some(&cwd)),
        // Doctor path has its own config check; StatusContext here is only
        // fed into health renderers that don't read config_load_error.
        config_load_error: config.as_ref().err().map(ToString::to_string),
        config_load_error_kind: None,
        mcp_validation: mcp_validation.clone(),

        hook_validation: hook_validation.clone(),
        duplicate_flags: Vec::new(),
    };
    Ok(DoctorReport {
        checks: vec![
            check_auth_health(),
            check_base_url_health(),
            check_config_health(&config_loader, config.as_ref()),
            check_mcp_validation_health(&mcp_validation),
            check_hook_validation_health(&hook_validation),
            check_install_source_health(),
            check_workspace_health(&context),
            check_memory_health(&context),
            check_boot_preflight_health(&context),
            check_sandbox_health(&context.sandbox_status),
            check_permission_health(permission_mode),
            check_system_health(&cwd, config.as_ref().ok()),
        ],
    })
}

fn run_doctor(
    output_format: CliOutputFormat,
    permission_mode: PermissionModeProvenance,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = render_doctor_report(
        match output_format {
            CliOutputFormat::Json => ConfigWarningMode::SuppressStderr,
            CliOutputFormat::Text => ConfigWarningMode::EmitStderr,
        },
        permission_mode,
    )?;
    let message = report.render();
    match output_format {
        CliOutputFormat::Text => println!("{message}"),
        CliOutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report.json_value())?);
        }
    }
    if report.has_failures() {
        return Err("doctor found failing checks".into());
    }
    Ok(())
}

/// Run the interactive setup wizard to configure provider, API key, and model.
fn run_setup() -> Result<(), Box<dyn std::error::Error>> {
    setup_wizard::run_setup_wizard()
}

/// Starts a minimal Model Context Protocol server that exposes claw's
/// built-in tools over stdio.
///
/// Tool descriptors come from [`tools::mvp_tool_specs`] and calls are
/// dispatched through [`tools::execute_tool`], so this server exposes exactly
/// Read `.claw/worker-state.json` from the current working directory and print it.
/// This is the file-based worker observability surface: `push_event()` in `worker_boot.rs`
/// atomically writes state transitions here so external observers (clawhip, orchestrators)
/// can poll current `WorkerStatus` without needing an HTTP route on the opencode binary.
fn run_worker_state(output_format: CliOutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let state_path = cwd.join(".claw").join("worker-state.json");
    if !state_path.exists() {
        // #139: this error used to say "run a worker first" without telling
        // callers how to run one. "worker" is an internal concept (there is
        // no `claw worker` subcommand), so claws/CI had no discoverable path
        // from the error to a fix. Emit an actionable, structured error that
        // names the two concrete commands that produce worker state.
        //
        // Format in both text and JSON modes is stable so scripts can match:
        //   error: no worker state file found at <path>
        //     Hint: worker state is written by the interactive REPL or a non-interactive prompt.
        //     Run:   claw               # start the REPL (writes state on first turn)
        //     Or:    claw prompt <text> # run one non-interactive turn
        //     Then rerun: claw state [--output-format json]
        return Err(format!(
            "no worker state file found at {path}\n  Hint: worker state is written by the interactive REPL or a non-interactive prompt.\n  Run:   claw               # start the REPL (writes state on first turn)\n  Or:    claw prompt <text> # run one non-interactive turn\n  Then rerun: claw state [--output-format json]",
            path = state_path.display()
        )
        .into());
    }
    let raw = std::fs::read_to_string(&state_path)?;
    match output_format {
        CliOutputFormat::Text => println!("{raw}"),
        CliOutputFormat::Json => {
            // Validate it parses as JSON before re-emitting
            let _: serde_json::Value = serde_json::from_str(&raw)?;
            println!("{raw}");
        }
    }
    Ok(())
}

/// the same surface the in-process agent loop uses.
fn run_mcp_serve() -> Result<(), Box<dyn std::error::Error>> {
    let tools = mvp_tool_specs()
        .into_iter()
        .map(|spec| McpTool {
            name: spec.name.to_string(),
            description: Some(spec.description.to_string()),
            input_schema: Some(spec.input_schema),
            annotations: None,
            meta: None,
        })
        .collect();

    let spec = McpServerSpec {
        server_name: "claw".to_string(),
        server_version: VERSION.to_string(),
        tools,
        tool_handler: Box::new(execute_tool),
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let mut server = McpServer::new(spec);
        server.run().await
    })?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn check_auth_health() -> DiagnosticCheck {
    let api_key_present = env::var("ANTHROPIC_API_KEY")
        .ok()
        .is_some_and(|value| !value.trim().is_empty());
    let auth_token_present = env::var("ANTHROPIC_AUTH_TOKEN")
        .ok()
        .is_some_and(|value| !value.trim().is_empty());
    let openai_key_present = env::var("OPENAI_API_KEY")
        .ok()
        .is_some_and(|value| !value.trim().is_empty());
    let any_auth_present = api_key_present || auth_token_present || openai_key_present;
    let prompt_ready = any_auth_present;
    let env_details = format!(
        "Environment       api_key={} auth_token={} openai_key={}",
        if api_key_present { "present" } else { "absent" },
        if auth_token_present {
            "present"
        } else {
            "absent"
        },
        if openai_key_present {
            "present"
        } else {
            "absent"
        }
    );

    match load_oauth_credentials() {
        Ok(Some(token_set)) => DiagnosticCheck::new(
            "Auth",
            if any_auth_present {
                DiagnosticLevel::Ok
            } else {
                DiagnosticLevel::Warn
            },
            if any_auth_present {
                "supported auth env vars are configured; legacy saved OAuth is ignored"
            } else {
                "legacy saved OAuth credentials are present but unsupported"
            },
        )
        .with_details(vec![
            env_details,
            format!(
                "Legacy OAuth      expires_at={} refresh_token={} scopes={}",
                token_set
                    .expires_at
                    .map_or_else(|| "<none>".to_string(), |value| value.to_string()),
                if token_set.refresh_token.is_some() {
                    "present"
                } else {
                    "absent"
                },
                if token_set.scopes.is_empty() {
                    "<none>".to_string()
                } else {
                    token_set.scopes.join(",")
                }
            ),
            "Suggested action  set ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN; `claw login` is removed"
                .to_string(),
        ])
        .with_hint("Set ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN env var. The saved OAuth token is no longer accepted.")
        .with_data(Map::from_iter([
            ("api_key_present".to_string(), json!(api_key_present)),
            ("auth_token_present".to_string(), json!(auth_token_present)),
            ("openai_key_present".to_string(), json!(openai_key_present)),
            ("prompt_ready".to_string(), json!(prompt_ready)),
            ("prompt_blocked_reason".to_string(), if prompt_ready { Value::Null } else { json!("auth_missing") }),

            ("legacy_saved_oauth_present".to_string(), json!(true)),
            (
                "legacy_saved_oauth_expires_at".to_string(),
                json!(token_set.expires_at),
            ),
            (
                "legacy_refresh_token_present".to_string(),
                json!(token_set.refresh_token.is_some()),
            ),
            ("legacy_scopes".to_string(), json!(token_set.scopes)),
        ])),
        Ok(None) => DiagnosticCheck::new(
            "Auth",
            if any_auth_present {
                DiagnosticLevel::Ok
            } else {
                DiagnosticLevel::Warn
            },
            if any_auth_present {
                "supported auth env vars are configured"
            } else {
                "no supported auth env vars were found"
            },
        )
        .with_details(vec![env_details])
        .with_hint(if !any_auth_present { "Set ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN to authenticate." } else { "" })
        .with_data(Map::from_iter([
            ("api_key_present".to_string(), json!(api_key_present)),
            ("auth_token_present".to_string(), json!(auth_token_present)),
            ("openai_key_present".to_string(), json!(openai_key_present)),
            ("prompt_ready".to_string(), json!(prompt_ready)),
            ("prompt_blocked_reason".to_string(), if prompt_ready { Value::Null } else { json!("auth_missing") }),
            ("legacy_saved_oauth_present".to_string(), json!(false)),
            ("legacy_saved_oauth_expires_at".to_string(), Value::Null),
            ("legacy_refresh_token_present".to_string(), json!(false)),
            ("legacy_scopes".to_string(), json!(Vec::<String>::new())),
        ])),
        Err(error) => DiagnosticCheck::new(
            "Auth",
            DiagnosticLevel::Fail,
            format!("failed to inspect legacy saved credentials: {error}"),
        )
        .with_hint("Set ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN env var to authenticate.")
        .with_data(Map::from_iter([
            ("api_key_present".to_string(), json!(api_key_present)),
            ("auth_token_present".to_string(), json!(auth_token_present)),
            ("openai_key_present".to_string(), json!(openai_key_present)),
            ("prompt_ready".to_string(), json!(prompt_ready)),
            ("prompt_blocked_reason".to_string(), if prompt_ready { Value::Null } else { json!("auth_missing") }),
            ("legacy_saved_oauth_present".to_string(), Value::Null),
            ("legacy_saved_oauth_expires_at".to_string(), Value::Null),
            ("legacy_refresh_token_present".to_string(), Value::Null),
            ("legacy_scopes".to_string(), Value::Null),
            ("legacy_saved_oauth_error".to_string(), json!(error.to_string())),
        ])),
    }
}

/// #466: validate provider BASE_URL env vars
fn check_base_url_health() -> DiagnosticCheck {
    let base_url_vars = [
        ("ANTHROPIC_BASE_URL", "https://api.anthropic.com"),
        ("OPENAI_BASE_URL", "https://api.openai.com"),
        ("XAI_BASE_URL", "https://api.x.ai"),
        ("DASHSCOPE_BASE_URL", "https://dashscope.aliyuncs.com"),
    ];
    let mut issues: Vec<String> = Vec::new();
    let mut details: Vec<String> = Vec::new();
    for (var_name, default_url) in &base_url_vars {
        if let Ok(value) = env::var(var_name) {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                issues.push(format!("{var_name} is empty"));
                details.push(format!(
                    "{var_name}  empty (will use default: {default_url})"
                ));
            } else if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
                issues.push(format!("{var_name}={trimmed} is not a valid HTTP(S) URL"));
                details.push(format!("{var_name}  invalid ({trimmed})"));
            } else {
                details.push(format!("{var_name}  {trimmed}"));
            }
        }
    }
    if issues.is_empty() {
        DiagnosticCheck::new(
            "Base URLs",
            DiagnosticLevel::Ok,
            "provider base URL env vars are valid or unset",
        )
        .with_details(details)
    } else {
        DiagnosticCheck::new(
            "Base URLs",
            DiagnosticLevel::Warn,
            format!("{} base URL issue(s) found", issues.len()),
        )
        .with_details(details)
        .with_hint("Fix the reported BASE_URL env vars or unset them to use provider defaults.")
    }
}

fn check_config_health(
    config_loader: &ConfigLoader,
    config: Result<&runtime::RuntimeConfig, &runtime::ConfigError>,
) -> DiagnosticCheck {
    let discovered = config_loader.discover();
    let discovered_count = discovered.len();
    // Separate candidate paths that actually exist from those that don't.
    // Showing non-existent paths as "Discovered file" implies they loaded
    // but something went wrong, which is confusing. We only surface paths
    // that exist on disk as discovered; non-existent ones are silently
    // omitted from the display (they are just the standard search locations).
    let present_paths: Vec<String> = discovered
        .iter()
        .filter(|e| e.path.exists())
        .map(|e| e.path.display().to_string())
        .collect();
    let discovered_paths = discovered
        .iter()
        .map(|entry| entry.path.display().to_string())
        .collect::<Vec<_>>();
    match config {
        Ok(runtime_config) => {
            let loaded_entries = runtime_config.loaded_entries();
            let loaded_count = loaded_entries.len();
            let present_count = present_paths.len();
            let mut details = vec![format!(
                "Config files      loaded {}/{}",
                loaded_count, present_count
            )];
            if let Some(model) = runtime_config.model() {
                details.push(format!("Resolved model    {model}"));
            }
            details.push(format!(
                "MCP servers       {}",
                runtime_config.mcp().valid_count()
            ));
            if runtime_config.mcp().invalid_count() > 0 {
                details.push(format!(
                    "MCP invalid       {}",
                    runtime_config.mcp().invalid_count()
                ));
            }
            if present_paths.is_empty() {
                details.push("Discovered files  <none> (defaults active)".to_string());
            } else {
                details.extend(
                    present_paths
                        .iter()
                        .map(|path| format!("Discovered file   {path}")),
                );
            }
            DiagnosticCheck::new(
                "Config",
                DiagnosticLevel::Ok,
                if present_count == 0 {
                    "no config files present; defaults are active"
                } else {
                    "runtime config loaded successfully"
                },
            )
            .with_details(details)
            .with_data(Map::from_iter([
                ("discovered_files".to_string(), json!(present_paths)),
                ("discovered_files_count".to_string(), json!(present_count)),
                ("loaded_config_files".to_string(), json!(loaded_count)),
                ("resolved_model".to_string(), json!(runtime_config.model())),
                (
                    "mcp_servers".to_string(),
                    json!(runtime_config.mcp().valid_count()),
                ),
                (
                    "mcp_invalid_servers".to_string(),
                    json!(runtime_config.mcp().invalid_count()),
                ),
                (
                    "hook_invalid_entries".to_string(),
                    json!(runtime_config.hooks().invalid_count()),
                ),
            ]))
        }
        Err(error) => DiagnosticCheck::new(
            "Config",
            DiagnosticLevel::Fail,
            format!("runtime config failed to load: {error}"),
        )
        .with_details(if discovered_paths.is_empty() {
            vec!["Discovered files  <none>".to_string()]
        } else {
            discovered_paths
                .iter()
                .map(|path| format!("Discovered file   {path}"))
                .collect()
        })
        .with_hint("Fix the JSON syntax error in the listed config file, then rerun `claw doctor`.")
        .with_data(Map::from_iter([
            ("discovered_files".to_string(), json!(discovered_paths)),
            (
                "discovered_files_count".to_string(),
                json!(discovered_count),
            ),
            ("loaded_config_files".to_string(), json!(0)),
            ("resolved_model".to_string(), Value::Null),
            ("mcp_servers".to_string(), Value::Null),
            ("load_error".to_string(), json!(error.to_string())),
        ])),
    }
}

fn check_mcp_validation_health(summary: &McpValidationSummary) -> DiagnosticCheck {
    let mut details = vec![
        format!("Total entries     {}", summary.total_configured),
        format!("Valid entries     {}", summary.valid_count),
        format!("Invalid entries   {}", summary.invalid_count()),
    ];
    details.extend(
        summary
            .invalid_servers
            .iter()
            .map(|server| format!("Invalid server   {} ({})", server.name, server.reason)),
    );

    DiagnosticCheck::new(
        "MCP validation",
        if summary.has_invalid_servers() {
            DiagnosticLevel::Warn
        } else {
            DiagnosticLevel::Ok
        },
        if summary.has_invalid_servers() {
            format!(
                "{} MCP server entries are invalid; {} valid entries remain loaded",
                summary.invalid_count(),
                summary.valid_count
            )
        } else {
            format!("{} MCP server entries validated", summary.valid_count)
        },
    )
    .with_hint(if summary.has_invalid_servers() {
        "Inspect `claw mcp list --output-format json` invalid_servers and fix each rejected mcpServers entry."
    } else {
        ""
    })
    .with_details(details)
    .with_data(Map::from_iter([
        (
            "total_configured".to_string(),
            json!(summary.total_configured),
        ),
        ("valid_count".to_string(), json!(summary.valid_count)),
        ("invalid_count".to_string(), json!(summary.invalid_count())),
        (
            "invalid_servers".to_string(),
            Value::Array(invalid_mcp_servers_json(&summary.invalid_servers)),
        ),
    ]))
}

fn check_hook_validation_health(summary: &HookValidationSummary) -> DiagnosticCheck {
    let mut details = vec![
        format!("Valid entries     {}", summary.valid_count),
        format!("Invalid entries   {}", summary.invalid_count()),
    ];
    details.extend(
        summary
            .invalid_hooks
            .iter()
            .map(|hook| format!("Invalid hook     {} ({})", hook.event, hook.reason)),
    );

    DiagnosticCheck::new(
        "Hook validation",
        if summary.has_invalid_hooks() {
            DiagnosticLevel::Warn
        } else {
            DiagnosticLevel::Ok
        },
        if summary.has_invalid_hooks() {
            format!(
                "{} hook entries are invalid; {} valid entries remain loaded",
                summary.invalid_count(),
                summary.valid_count
            )
        } else {
            format!("{} hook entries validated", summary.valid_count)
        },
    )
    .with_hint(if summary.has_invalid_hooks() {
        "Inspect `claw status --output-format json` hook_validation.invalid_hooks and fix each rejected hooks entry."
    } else {
        ""
    })
    .with_details(details)
    .with_data(Map::from_iter([
        ("valid_count".to_string(), json!(summary.valid_count)),
        ("invalid_count".to_string(), json!(summary.invalid_count())),
        (
            "invalid_hooks".to_string(),
            Value::Array(invalid_hooks_json(&summary.invalid_hooks)),
        ),
    ]))
}

fn check_permission_health(permission_mode: PermissionModeProvenance) -> DiagnosticCheck {
    let mode = permission_mode.mode.as_str();
    let source = permission_mode.source.as_str();
    let explicit = permission_mode.source.is_explicit();
    let warning = matches!(permission_mode.mode, PermissionMode::DangerFullAccess) && !explicit;
    let message = if warning {
        "running with full access without explicit opt-in"
    } else if matches!(permission_mode.mode, PermissionMode::DangerFullAccess) {
        "danger-full-access was explicitly selected"
    } else if matches!(permission_mode.mode, PermissionMode::WorkspaceWrite) && !explicit {
        "default permission mode is workspace-write"
    } else {
        "permission mode is explicitly bounded below danger-full-access"
    };
    let source_detail = permission_mode.env_var.map_or_else(
        || source.to_string(),
        |env_var| format!("{source}:{env_var}"),
    );
    let specs = mvp_tool_specs();
    let tools_satisfied = specs
        .iter()
        .filter(|spec| permission_mode.mode >= spec.required_permission)
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    let tools_gated = specs
        .iter()
        .filter(|spec| permission_mode.mode < spec.required_permission)
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    DiagnosticCheck::new(
        "Permissions",
        if warning {
            DiagnosticLevel::Warn
        } else {
            DiagnosticLevel::Ok
        },
        message,
    )
    .with_details(vec![
        format!("Mode             {mode}"),
        format!("Source           {source_detail}"),
        format!("Explicit opt-in  {explicit}"),
        format!("Tools allowed    {}", tools_satisfied.join(", ")),
        format!("Tools gated      {}", tools_gated.join(", ")),
    ])
    .with_hint(if warning {
        "Use the workspace-write default, or pass --permission-mode danger-full-access / --dangerously-skip-permissions only when full filesystem, network, and command access is intentional."
    } else {
        "Use --permission-mode read-only|workspace-write|danger-full-access to make the runtime permission boundary explicit."
    })
    .with_data(Map::from_iter([
        ("mode".to_string(), json!(mode)),
        ("source".to_string(), json!(source)),
        ("source_explicit".to_string(), json!(explicit)),
        ("env_var".to_string(), json!(permission_mode.env_var)),
        ("message".to_string(), json!(message)),
        ("tools_satisfied".to_string(), json!(tools_satisfied)),
        ("tools_gated".to_string(), json!(tools_gated)),
    ]))
}

fn check_install_source_health() -> DiagnosticCheck {
    DiagnosticCheck::new(
        "Install source",
        DiagnosticLevel::Ok,
        format!(
            "official source of truth is {OFFICIAL_REPO_SLUG}; avoid `{DEPRECATED_INSTALL_COMMAND}`"
        ),
    )
    .with_details(vec![
        format!("Official repo     {OFFICIAL_REPO_URL}"),
        "Recommended path  build from this repo or use the upstream binary documented in README.md"
            .to_string(),
        format!(
            "Deprecated crate  `{DEPRECATED_INSTALL_COMMAND}` installs a deprecated stub and does not provide the `claw` binary"
        )
            .to_string(),
    ])
    .with_data(Map::from_iter([
        ("official_repo".to_string(), json!(OFFICIAL_REPO_URL)),
        (
            "deprecated_install".to_string(),
            json!(DEPRECATED_INSTALL_COMMAND),
        ),
        (
            "recommended_install".to_string(),
            json!("build from source or follow the upstream binary instructions in README.md"),
        ),
    ]))
}

fn check_workspace_health(context: &StatusContext) -> DiagnosticCheck {
    let in_repo = context.project_root.is_some();
    let stale_base_warning = format_stale_base_warning(&context.stale_base_state);
    DiagnosticCheck::new(
        "Workspace",
        if in_repo && stale_base_warning.is_none() {
            DiagnosticLevel::Ok
        } else {
            DiagnosticLevel::Warn
        },
        if in_repo {
            format!(
                "project root detected on branch {}",
                context.git_branch.as_deref().unwrap_or("unknown")
            )
        } else {
            "current directory is not inside a git project".to_string()
        },
    )
    .with_hint(if !in_repo {
        "Run `git init` to initialise a repository, or `cd` into a git project."
    } else if stale_base_warning.is_some() {
        "Rebase or merge to bring the branch up to date with its base."
    } else {
        ""
    })
    .with_details(vec![
        format!("Cwd              {}", context.cwd.display()),
        format!(
            "Project root     {}",
            context
                .project_root
                .as_ref()
                .map_or_else(|| "<none>".to_string(), |path| path.display().to_string())
        ),
        format!(
            "Git branch       {}",
            context.git_branch.as_deref().unwrap_or("unknown")
        ),
        format!(
            "Git state        {}",
            if context.project_root.is_some() {
                context.git_summary.headline()
            } else {
                "no git repo".to_string()
            }
        ),
        format!("Changed files    {}", context.git_summary.changed_files),
        format!(
            "Memory files     {} · config files loaded {}/{}",
            context.memory_file_count, context.loaded_config_files, context.discovered_config_files
        ),
        format!(
            "Loaded memory    {}",
            if context.memory_files.is_empty() {
                "<none>".to_string()
            } else {
                context
                    .memory_files
                    .iter()
                    .map(|file| format!("{}:{}", file.source, file.path))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
        format!(
            "Stale base      {}",
            stale_base_warning.as_deref().unwrap_or("ok")
        ),
    ])
    .with_data(Map::from_iter([
        ("cwd".to_string(), json!(context.cwd.display().to_string())),
        (
            "project_root".to_string(),
            json!(context
                .project_root
                .as_ref()
                .map(|path| path.display().to_string())),
        ),
        ("in_git_repo".to_string(), json!(in_repo)),
        ("git_branch".to_string(), json!(context.git_branch)),
        (
            "git_state".to_string(),
            json!(if context.project_root.is_some() {
                context.git_summary.headline()
            } else {
                "no_git_repo".to_string()
            }),
        ),
        (
            "changed_files".to_string(),
            json!(context.git_summary.changed_files),
        ),
        (
            "memory_file_count".to_string(),
            json!(context.memory_file_count),
        ),
        (
            "memory_files".to_string(),
            Value::Array(memory_files_json(&context.memory_files)),
        ),
        (
            "unloaded_memory_files".to_string(),
            json!(context.unloaded_memory_files),
        ),
        (
            "loaded_config_files".to_string(),
            json!(context.loaded_config_files),
        ),
        (
            "discovered_config_files".to_string(),
            json!(context.discovered_config_files),
        ),
        (
            "stale_base".to_string(),
            stale_base_json_value(&context.stale_base_state),
        ),
    ]))
}

fn check_memory_health(context: &StatusContext) -> DiagnosticCheck {
    let has_unloaded = !context.unloaded_memory_files.is_empty();
    let has_outside_project = context.memory_files.iter().any(|file| file.outside_project);
    let mut details = vec![format!("Loaded files     {}", context.memory_file_count)];
    details.extend(context.memory_files.iter().map(|file| {
        format!(
            "Loaded          {} ({}, chars={})",
            file.path, file.source, file.chars
        )
    }));
    details.extend(
        context
            .unloaded_memory_files
            .iter()
            .map(|path| format!("Unloaded        {path}")),
    );

    DiagnosticCheck::new(
        "Memory",
        if has_unloaded || has_outside_project {
            DiagnosticLevel::Warn
        } else {
            DiagnosticLevel::Ok
        },
        if has_outside_project {
            "memory files outside the current git project are loaded".to_string()
        } else if has_unloaded {
            "some workspace memory files exist but were not loaded".to_string()
        } else {
            format!("{} workspace memory files loaded", context.memory_file_count)
        },
    )
    .with_hint(if has_outside_project {
        "Inspect workspace.memory_files in `claw status --output-format json`; move unintended ancestor instructions inside the git project or run from the intended workspace root."
    } else if has_unloaded {
        "Move instructions into CLAUDE.md, CLAW.md, or AGENTS.md within the current workspace ancestry, or inspect workspace.memory_files in `claw status --output-format json`."
    } else {
        ""
    })
    .with_details(details)
    .with_data(Map::from_iter([
        (
            "memory_file_count".to_string(),
            json!(context.memory_file_count),
        ),
        (
            "memory_files".to_string(),
            Value::Array(memory_files_json(&context.memory_files)),
        ),
        (
            "unloaded_memory_files".to_string(),
            json!(context.unloaded_memory_files),
        ),
    ]))
}

fn check_boot_preflight_health(context: &StatusContext) -> DiagnosticCheck {
    let preflight = &context.boot_preflight;
    let missing_binaries = preflight
        .required_binaries
        .iter()
        .filter(|binary| !binary.available)
        .map(|binary| binary.name)
        .collect::<Vec<_>>();
    let socket_details = preflight
        .control_sockets
        .iter()
        .map(|socket| {
            format!(
                "Control socket  {} configured={} exists={} path={}",
                socket.name,
                socket.configured,
                socket.exists,
                socket.path.as_deref().unwrap_or("<none>")
            )
        })
        .collect::<Vec<_>>();
    let mut details = vec![
        format!("Repo exists      {}", preflight.repo_exists),
        format!("Worktree exists  {}", preflight.worktree_exists),
        format!("Git dir exists   {}", preflight.git_dir_exists),
        format!("Branch behind    {}", preflight.branch_freshness.behind),
        format!(
            "Trust allowlist  {}",
            preflight
                .trust_gate_allowed
                .map_or("unknown".to_string(), |v| v.to_string())
        ),
        format!("Trusted roots    {}", preflight.trusted_roots_count),
        // #736: keep compound values readable but use " · " as intra-value separator
        // so the two-space prose splitter yields key="MCP eligible" value="true · servers 0"
        format!(
            "MCP eligible     {}",
            format!(
                "{}  ·  servers {}",
                preflight.mcp_startup_eligible, preflight.mcp_servers_configured
            )
        ),
        format!(
            "Plugin eligible  {}",
            format!(
                "{}  ·  configured {}",
                preflight.plugin_startup_eligible, preflight.plugins_configured
            )
        ),
        format!(
            // #736: use two-space separator so the detail_entries prose splitter
            // can extract key="Last failed boot" value="<none>|<reason>"
            "Last failed boot  {}",
            preflight
                .last_failed_boot_reason
                .as_deref()
                .unwrap_or("<none>")
        ),
    ];
    details.extend(preflight.required_binaries.iter().map(|binary| {
        format!(
            // #736: two-space separator → key="Required binary <name>" value="available=true|false"
            "Required binary {}  available={}",
            binary.name, binary.available
        )
    }));
    details.extend(socket_details);
    DiagnosticCheck::new(
        "Boot preflight",
        if preflight.repo_exists && preflight.worktree_exists && missing_binaries.is_empty() {
            DiagnosticLevel::Ok
        } else {
            DiagnosticLevel::Warn
        },
        preflight.summary(),
    )
    .with_details(details)
    .with_hint(
        // #778: stable remediation hint for automation
        if !preflight.repo_exists || !preflight.worktree_exists {
            "Ensure you are inside a git worktree (`git init` or `git worktree add`)."
        } else if !missing_binaries.is_empty() {
            "Install the listed missing required binaries."
        } else {
            ""
        },
    )
    .with_data(Map::from_iter([(
        "boot_preflight".to_string(),
        preflight.json_value(),
    )]))
}

fn check_sandbox_health(status: &runtime::SandboxStatus) -> DiagnosticCheck {
    let degraded = status.enabled && !status.active;
    let mut details = vec![
        format!("Enabled          {}", status.enabled),
        format!("Active           {}", status.active),
        format!("Supported        {}", status.supported),
        format!("Filesystem mode  {}", status.filesystem_mode.as_str()),
        format!("Filesystem live  {}", status.filesystem_active),
    ];
    if let Some(reason) = &status.fallback_reason {
        details.push(format!("Fallback reason  {reason}"));
    }
    DiagnosticCheck::new(
        "Sandbox",
        if degraded {
            DiagnosticLevel::Warn
        } else {
            DiagnosticLevel::Ok
        },
        if degraded {
            "sandbox was requested but is not currently active"
        } else if status.active {
            "sandbox protections are active"
        } else {
            "sandbox is not active for this session"
        },
    )
    .with_details(details)
    .with_hint(
        // #778: stable remediation hint — sandbox degraded on non-Linux hosts is expected, not an error
        if degraded && !status.supported {
            "Sandbox namespace isolation requires Linux with `unshare`. On macOS/non-Linux hosts this warning is expected and can be ignored. Filesystem isolation is still active."
        } else if degraded {
            "Check that the `unshare` binary is available and the process has the required capabilities."
        } else {
            ""
        },
    )
    .with_data(Map::from_iter([
        ("enabled".to_string(), json!(status.enabled)),
        ("active".to_string(), json!(status.active)),
        ("supported".to_string(), json!(status.supported)),
        (
            "namespace_supported".to_string(),
            json!(status.namespace_supported),
        ),
        (
            "namespace_active".to_string(),
            json!(status.namespace_active),
        ),
        (
            "network_supported".to_string(),
            json!(status.network_supported),
        ),
        ("network_active".to_string(), json!(status.network_active)),
        (
            "filesystem_mode".to_string(),
            json!(status.filesystem_mode.as_str()),
        ),
        (
            "filesystem_active".to_string(),
            json!(status.filesystem_active),
        ),
        ("allowed_mounts".to_string(), json!(status.allowed_mounts)),
        ("in_container".to_string(), json!(status.in_container)),
        (
            "container_markers".to_string(),
            json!(status.container_markers),
        ),
        ("fallback_reason".to_string(), json!(status.fallback_reason)),
    ]))
}

fn check_system_health(cwd: &Path, config: Option<&runtime::RuntimeConfig>) -> DiagnosticCheck {
    let default_model = config.and_then(runtime::RuntimeConfig::model);
    let mut details = vec![
        format!("OS               {} {}", env::consts::OS, env::consts::ARCH),
        format!("Working dir      {}", cwd.display()),
        format!("Version          {}", VERSION),
        format!("Build target     {}", BUILD_TARGET.unwrap_or("<unknown>")),
        format!("Git SHA          {}", GIT_SHA.unwrap_or("<unknown>")),
        format!(
            "Output format env  CLAW_OUTPUT_FORMAT={}",
            env::var("CLAW_OUTPUT_FORMAT").unwrap_or_else(|_| "<unset>".to_string())
        ),
        format!(
            "Logging env      CLAW_LOG={} RUST_LOG={}",
            env::var("CLAW_LOG").unwrap_or_else(|_| "<unset>".to_string()),
            env::var("RUST_LOG").unwrap_or_else(|_| "<unset>".to_string())
        ),
    ];
    if let Some(model) = default_model {
        details.push(format!("Default model    {model}"));
    }
    let binary_provenance = binary_provenance_for(Some(cwd));
    details.push(format!(
        "Binary provenance  status={} workspace_match={}",
        binary_provenance.status(),
        binary_provenance
            .workspace_match
            .map_or_else(|| "unknown".to_string(), |matches| matches.to_string())
    ));
    DiagnosticCheck::new(
        "System",
        DiagnosticLevel::Ok,
        "captured local runtime metadata",
    )
    .with_details(details)
    .with_data(Map::from_iter([
        ("os".to_string(), json!(env::consts::OS)),
        ("arch".to_string(), json!(env::consts::ARCH)),
        ("working_dir".to_string(), json!(cwd.display().to_string())),
        ("version".to_string(), json!(VERSION)),
        ("build_target".to_string(), json!(BUILD_TARGET)),
        ("git_sha".to_string(), json!(GIT_SHA)),
        (
            "binary_provenance".to_string(),
            binary_provenance.json_value(),
        ),
        ("default_model".to_string(), json!(default_model)),
        (
            "claw_output_format".to_string(),
            json!(env::var("CLAW_OUTPUT_FORMAT").ok()),
        ),
        ("claw_log".to_string(), json!(env::var("CLAW_LOG").ok())),
        ("rust_log".to_string(), json!(env::var("RUST_LOG").ok())),
    ]))
}

fn resume_command_can_absorb_token(current_command: &str, token: &str) -> bool {
    matches!(
        SlashCommand::parse(current_command),
        Ok(Some(SlashCommand::Export { path: None }))
    ) && !looks_like_slash_command_token(token)
}

fn looks_like_slash_command_token(token: &str) -> bool {
    let trimmed = token.trim_start();
    let Some(name) = trimmed.strip_prefix('/').and_then(|value| {
        value
            .split_whitespace()
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }) else {
        return false;
    };

    slash_command_specs()
        .iter()
        .any(|spec| spec.name == name || spec.aliases.contains(&name))
}

fn dump_manifests(
    manifests_dir: Option<&Path>,
    output_format: CliOutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let workspace_dir = env::current_dir()?;
    dump_manifests_at_path(&workspace_dir, manifests_dir, output_format)
}

const DUMP_MANIFESTS_USAGE_HINT: &str =
    "Usage: claw dump-manifests [--manifests-dir <path>] [--output-format json]";

// Internal function for testing that accepts a workspace directory path.
fn dump_manifests_at_path(
    workspace_dir: &std::path::Path,
    manifests_dir: Option<&Path>,
    output_format: CliOutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let discovery_root = manifests_dir.unwrap_or(workspace_dir);
    let resolved_root = discovery_root
        .canonicalize()
        .unwrap_or_else(|_| discovery_root.to_path_buf());

    if !resolved_root.exists() {
        return Err(format!(
            "missing_manifests: manifest discovery directory does not exist.\n  looked in: {}\n  {DUMP_MANIFESTS_USAGE_HINT}",
            resolved_root.display(),
        )
        .into());
    }
    if !resolved_root.is_dir() {
        return Err(format!(
            "missing_manifests: manifest discovery path is not a directory.\n  looked in: {}\n  {DUMP_MANIFESTS_USAGE_HINT}",
            resolved_root.display(),
        )
        .into());
    }

    let manifest = build_rust_resolver_manifest(&resolved_root)?;
    match output_format {
        CliOutputFormat::Text => {
            println!("Manifest Dump");
            println!("  Source           rust-resolver");
            println!("  Workspace        {}", resolved_root.display());
            println!("  Commands         {}", manifest["commands"]);
            println!("  Tools            {}", manifest["tools"]);
            println!("  Agents           {}", manifest["agents"]);
            println!("  Skills           {}", manifest["skills"]);
            println!("  Bootstrap phases {}", manifest["bootstrap_phases"]);
        }
        CliOutputFormat::Json => println!("{}", serde_json::to_string_pretty(&manifest)?),
    }
    Ok(())
}

fn build_rust_resolver_manifest(workspace_dir: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let command_entries = slash_command_specs()
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "aliases": spec.aliases,
                "summary": spec.summary,
                "argument_hint": spec.argument_hint,
                "resume_supported": spec.resume_supported,
                "implemented": !STUB_COMMANDS.contains(&spec.name),
            })
        })
        .collect::<Vec<_>>();

    let tool_entries = mvp_tool_specs()
        .into_iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "required_permission": spec.required_permission.as_str(),
                "input_schema": spec.input_schema,
            })
        })
        .collect::<Vec<_>>();

    let agent_report = handle_agents_slash_command_json(None, workspace_dir)?;
    let skill_report = handle_skills_slash_command_json(None, workspace_dir)?;
    let agents = agent_report
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let skills = skill_report
        .get("skills")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let bootstrap = runtime::BootstrapPlan::claude_code_default()
        .phases()
        .iter()
        .map(|phase| format!("{phase:?}"))
        .collect::<Vec<_>>();

    Ok(json!({
        "kind": "dump-manifests",
        "action": "dump",
        "status": "ok",
        "source": "rust-resolver",
        "workspace": workspace_dir.display().to_string(),
        "commands": command_entries.len(),
        "tools": tool_entries.len(),
        "agents": agents.len(),
        "skills": skills.len(),
        "bootstrap_phases": bootstrap.len(),
        "command_manifests": command_entries,
        "tool_manifests": tool_entries,
        "agent_manifests": agents,
        "skill_manifests": skills,
        "bootstrap_manifest": bootstrap,
    }))
}

fn print_bootstrap_plan(output_format: CliOutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    let phases = runtime::BootstrapPlan::claude_code_default();
    match output_format {
        CliOutputFormat::Text => {
            for phase in phases.phases() {
                println!("- {phase:?}");
            }
        }
        CliOutputFormat::Json => {
            // #412: emit structured phase objects with label and description
            let phase_objects: Vec<serde_json::Value> = phases
                .phases()
                .iter()
                .enumerate()
                .map(|(i, phase)| {
                    let (label, description) = bootstrap_phase_metadata(phase);
                    json!({
                        "id": format!("{phase:?}"),
                        "label": label,
                        "description": description,
                        "order": i,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "kind": "bootstrap-plan",
                    "action": "show",
                    "status": "ok",
                    "total_phases": phases.phases().len(),
                    "phases": phase_objects,
                }))?
            );
        }
    }
    Ok(())
}

fn bootstrap_phase_metadata(phase: &runtime::BootstrapPhase) -> (&'static str, &'static str) {
    use runtime::BootstrapPhase::*;
    match phase {
        CliEntry => (
            "CLI Entry",
            "Command-line argument parsing and global flag resolution",
        ),
        FastPathVersion => (
            "Fast-Path Version",
            "Short-circuit version/help requests before full startup",
        ),
        StartupProfiler => (
            "Startup Profiler",
            "Instrument startup timing for diagnostics",
        ),
        SystemPromptFastPath => (
            "System Prompt Fast-Path",
            "Serve system-prompt requests without provider init",
        ),
        ChromeMcpFastPath => (
            "Chrome MCP Fast-Path",
            "Serve Chrome MCP requests without full runtime",
        ),
        DaemonWorkerFastPath => (
            "Daemon Worker Fast-Path",
            "Handle daemon worker requests without full init",
        ),
        BridgeFastPath => (
            "Bridge Fast-Path",
            "Bridge/sibling process communication without full init",
        ),
        DaemonFastPath => (
            "Daemon Fast-Path",
            "Daemon lifecycle management without full runtime",
        ),
        BackgroundSessionFastPath => (
            "Background Session Fast-Path",
            "Resume/list background sessions without full init",
        ),
        TemplateFastPath => (
            "Template Fast-Path",
            "Template rendering without full runtime",
        ),
        EnvironmentRunnerFastPath => (
            "Environment Runner Fast-Path",
            "Environment/runner dispatch without full init",
        ),
        MainRuntime => (
            "Main Runtime",
            "Full interactive REPL or one-shot prompt execution",
        ),
    }
}

fn print_system_prompt(
    cwd: PathBuf,
    date: String,
    model: &str,
    output_format: CliOutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let (sections, project_context) = load_system_prompt_with_context(
        cwd,
        date,
        env::consts::OS,
        "unknown",
        model_family_identity_for(model),
    )?;
    let (project_root, _) =
        parse_git_status_metadata_for(&project_context.cwd, project_context.git_status.as_deref());
    let memory_files = memory_file_summaries_for(
        &project_context.cwd,
        project_root.as_deref(),
        &project_context.instruction_files,
    );
    let message = sections.join(
        "

",
    );
    // #418: filter out the internal boundary sentinel from the sections array
    // and expose the boundary index as a structured field.
    let filtered_sections: Vec<&str> = sections
        .iter()
        .filter(|s| !s.contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"))
        .map(|s| s.as_str())
        .collect();
    let boundary_index = sections
        .iter()
        .position(|s| s.contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"));
    match output_format {
        CliOutputFormat::Text => println!("{message}"),
        CliOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "kind": "system-prompt",
                "action": "show",
                "status": "ok",
                "message": message,
                "sections": filtered_sections,
                "boundary_index": boundary_index,
                "memory_file_count": memory_files.len(),
                "memory_files": memory_files_json(&memory_files),
            }))?
        ),
    }
    Ok(())
}

fn print_version(output_format: CliOutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    match output_format {
        CliOutputFormat::Text => println!("{}", render_version_report()),
        CliOutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&version_json_value())?);
        }
    }
    Ok(())
}

fn version_json_value() -> serde_json::Value {
    let cwd = env::current_dir().ok();
    let binary_provenance = binary_provenance_for(cwd.as_deref());
    json!({
        "kind": "version",
        "action": "show",
        "status": "ok",
        "human_readable": render_version_report(),
        "version": VERSION,
        "git_sha": binary_provenance.git_sha,
        "git_sha_short": binary_provenance.git_sha_short,
        "is_dirty": binary_provenance.is_dirty,
        "branch": binary_provenance.branch,
        "commit_date": binary_provenance.commit_date,
        "commit_timestamp": binary_provenance.commit_timestamp,
        "rustc_version": binary_provenance.rustc_version,
        "target": binary_provenance.target,
        "build_date": binary_provenance.build_date,
        "executable_path": binary_provenance.executable_path,
        "binary_provenance": binary_provenance.json_value(),
    })
}

#[allow(clippy::too_many_lines)]
fn resume_session(session_path: &Path, commands: &[String], output_format: CliOutputFormat) {
    let session_reference = session_path.display().to_string();
    let (handle, session) = match load_session_reference(&session_reference) {
        Ok(loaded) => loaded,
        Err(error) => {
            if output_format == CliOutputFormat::Json {
                // #77: classify session load errors for downstream consumers
                let full_message = format!("failed to restore session: {error}");
                let kind = classify_error_kind(&full_message);
                let (short_reason, inline_hint) = split_error_hint(&full_message);
                // #787: fall back to kind-derived hint when message has no \n delimiter
                let hint =
                    inline_hint.or_else(|| fallback_hint_for_error_kind(kind).map(String::from));
                let sessions_dir = sessions_dir().ok().map(|path| path.display().to_string());
                // #819: JSON mode resume errors go to stdout for parity with other
                // non-interactive command guards.
                println!(
                    "{}",
                    serde_json::json!({
                        "kind": kind,
                        "action": "restore",
                        "status": "error",
                        "error_kind": kind,
                        "error": short_reason,
                        "exit_code": 1,
                        "hint": hint,
                        "sessions_dir": sessions_dir,
                    })
                );
            } else {
                eprintln!("failed to restore session: {error}");
            }
            std::process::exit(1);
        }
    };
    let resolved_path = handle.path.clone();

    if commands.is_empty() {
        if output_format == CliOutputFormat::Json {
            println!(
                "{}",
                serde_json::json!({
                    "kind": "restored",
                    "action": "restore",
                    "status": "ok",
                    "session_id": session.session_id,
                    "path": handle.path.display().to_string(),
                    "message_count": session.messages.len(),
                })
            );
        } else {
            println!(
                "Restored session from {} ({} messages).",
                handle.path.display(),
                session.messages.len()
            );
        }
        return;
    }

    let mut session = session;
    for raw_command in commands {
        // Intercept spec commands that have no parse arm before calling
        // SlashCommand::parse — they return Err(SlashCommandParseError) which
        // formats as the confusing circular "Did you mean /X?" message.
        // STUB_COMMANDS covers both completions-filtered stubs and parse-less
        // spec entries; treat both as unsupported in resume mode.
        {
            let cmd_root = raw_command
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("");
            if STUB_COMMANDS.contains(&cmd_root) {
                if output_format == CliOutputFormat::Json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "kind": "unsupported_command",
                            "action": "resume",
                            "status": "error",
                            "error_kind": "unsupported_command",
                            "error": format!("/{cmd_root} is not yet implemented in this build"),
                            "hint": "This command is not available in the current build. Update claw or use a different command.",
                            "exit_code": 2,
                            "command": raw_command,
                        })
                    );
                } else {
                    eprintln!("/{cmd_root} is not yet implemented in this build");
                }
                std::process::exit(2);
            }
        }
        let command = match SlashCommand::parse(raw_command) {
            Ok(Some(command)) => command,
            Ok(None) => {
                if output_format == CliOutputFormat::Json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "kind": "unsupported_resumed_command",
                            "action": "resume",
                            "status": "error",
                            "error_kind": "unsupported_resumed_command",
                            "error": format!("unsupported resumed command: {raw_command}"),
                            "hint": "This command cannot be used with --resume. Use it in an interactive REPL session instead.",
                            "exit_code": 2,
                            "command": raw_command,
                        })
                    );
                } else {
                    eprintln!("unsupported resumed command: {raw_command}");
                }
                std::process::exit(2);
            }
            Err(error) => {
                if output_format == CliOutputFormat::Json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "kind": "cli_parse",
                            "action": "resume",
                            "status": "error",
                            "error_kind": "cli_parse",
                            "error": error.to_string(),
                            "hint": "Run `claw --help` for usage.",
                            "exit_code": 2,
                            "command": raw_command,
                        })
                    );
                } else {
                    eprintln!("{error}");
                }
                std::process::exit(2);
            }
        };
        match run_resume_command(&resolved_path, &session, &command) {
            Ok(ResumeCommandOutcome {
                session: next_session,
                message,
                json,
            }) => {
                session = next_session;
                if output_format == CliOutputFormat::Json {
                    if let Some(value) = json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&value)
                                .expect("resume command json output")
                        );
                    } else if let Some(message) = message {
                        println!("{message}");
                    }
                } else if let Some(message) = message {
                    println!("{message}");
                }
            }
            Err(error) => {
                if output_format == CliOutputFormat::Json {
                    // #776: classify + split so wrappers get typed fields instead of
                    // hardcoded "resume_command_error" + prose in the error field
                    let full_error = error.to_string();
                    let error_kind = classify_error_kind(&full_error);
                    let (short_reason, inline_hint) = split_error_hint(&full_error);
                    // #787: fall back to kind-derived hint when error has no \n delimiter
                    let hint = inline_hint
                        .or_else(|| fallback_hint_for_error_kind(error_kind).map(String::from));
                    println!(
                        "{}",
                        serde_json::json!({
                            "kind": error_kind,
                            "action": "resume",
                            "status": "error",
                            "error_kind": error_kind,
                            "error": short_reason,
                            "hint": hint,
                            "exit_code": 2,
                            "command": raw_command,
                        })
                    );
                } else {
                    eprintln!("{error}");
                }
                std::process::exit(2);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ResumeCommandOutcome {
    session: Session,
    message: Option<String>,
    json: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryFileSummary {
    path: String,
    source: String,
    chars: usize,
    origin: String,
    scope_path: String,
    outside_project: bool,
    contributes: bool,
}

impl MemoryFileSummary {
    fn json_value(&self) -> serde_json::Value {
        json!({
            "path": self.path,
            "source": self.source,
            "chars": self.chars,
            "origin": self.origin,
            "scope_path": self.scope_path,
            "outside_project": self.outside_project,
            "contributes": self.contributes,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct McpValidationSummary {
    total_configured: usize,
    valid_count: usize,
    invalid_servers: Vec<McpInvalidServerConfig>,
}

impl McpValidationSummary {
    fn from_collection(collection: &McpConfigCollection) -> Self {
        Self {
            total_configured: collection.total_configured(),
            valid_count: collection.valid_count(),
            invalid_servers: collection.invalid_servers().to_vec(),
        }
    }

    fn invalid_count(&self) -> usize {
        self.invalid_servers.len()
    }

    fn has_invalid_servers(&self) -> bool {
        !self.invalid_servers.is_empty()
    }

    fn json_value(&self) -> serde_json::Value {
        json!({
            "total_configured": self.total_configured,
            "valid_count": self.valid_count,
            "invalid_count": self.invalid_count(),
            "invalid_servers": invalid_mcp_servers_json(&self.invalid_servers),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HookValidationSummary {
    valid_count: usize,
    invalid_hooks: Vec<RuntimeInvalidHookConfig>,
}

impl HookValidationSummary {
    fn from_config(config: &runtime::RuntimeConfig) -> Self {
        let hooks = config.hooks();
        Self {
            valid_count: hooks.pre_tool_use_entries().len()
                + hooks.post_tool_use_entries().len()
                + hooks.post_tool_use_failure_entries().len(),
            invalid_hooks: hooks.invalid_hooks().to_vec(),
        }
    }

    fn invalid_count(&self) -> usize {
        self.invalid_hooks.len()
    }

    fn has_invalid_hooks(&self) -> bool {
        !self.invalid_hooks.is_empty()
    }

    fn json_value(&self) -> serde_json::Value {
        json!({
            "valid_count": self.valid_count,
            "invalid_count": self.invalid_count(),
            "invalid_hooks": invalid_hooks_json(&self.invalid_hooks),
        })
    }
}

fn invalid_hooks_json(invalid_hooks: &[RuntimeInvalidHookConfig]) -> Vec<serde_json::Value> {
    invalid_hooks
        .iter()
        .map(|hook| {
            json!({
                "event": &hook.event,
                "index": hook.index,
                "hook_index": hook.hook_index,
                "kind": &hook.kind,
                "error_field": &hook.error_field,
                "reason": &hook.reason,
                "valid": false,
            })
        })
        .collect()
}

fn invalid_mcp_servers_json(invalid_servers: &[McpInvalidServerConfig]) -> Vec<serde_json::Value> {
    invalid_servers
        .iter()
        .map(|server| {
            json!({
                "name": &server.name,
                "scope": config_source_json_value(server.scope),
                "path": server.path.display().to_string(),
                "error_field": &server.error_field,
                "reason": &server.reason,
                "valid": false,
            })
        })
        .collect()
}

fn config_source_json_value(source: ConfigSource) -> serde_json::Value {
    let id = match source {
        ConfigSource::User => "user",
        ConfigSource::Project => "project",
        ConfigSource::Local => "local",
    };
    json!({"id": id, "label": id})
}

fn memory_file_summaries_for(
    cwd: &Path,
    project_root: Option<&Path>,
    files: &[ContextFile],
) -> Vec<MemoryFileSummary> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let project_root =
        project_root.map(|path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
    files
        .iter()
        .map(|file| {
            let path = file
                .path
                .canonicalize()
                .unwrap_or_else(|_| file.path.clone());
            let scope_path = memory_scope_path(&path);
            let origin = memory_origin(&cwd, project_root.as_deref(), &scope_path);
            let outside_project = project_root
                .as_ref()
                .is_some_and(|root| !path.starts_with(root));
            MemoryFileSummary {
                path: file.path.display().to_string(),
                source: file.source().to_string(),
                origin: origin.to_string(),
                scope_path: scope_path.display().to_string(),
                chars: file.char_count(),
                outside_project,
                contributes: true,
            }
        })
        .collect()
}

fn memory_scope_path(path: &Path) -> PathBuf {
    let Some(parent) = path.parent() else {
        return PathBuf::from(".");
    };
    let parent_name = parent.file_name().and_then(|name| name.to_str());
    if matches!(parent_name, Some(".claw" | ".claude")) {
        return parent.parent().unwrap_or(parent).to_path_buf();
    }
    if matches!(parent_name, Some("rules" | "rules.local")) {
        if let Some(grandparent) = parent.parent() {
            if grandparent.file_name().and_then(|name| name.to_str()) == Some(".claw") {
                return grandparent.parent().unwrap_or(grandparent).to_path_buf();
            }
        }
    }
    parent.to_path_buf()
}

fn memory_origin(cwd: &Path, project_root: Option<&Path>, scope_path: &Path) -> &'static str {
    if scope_path == cwd {
        return "workspace";
    }
    if project_root.is_some_and(|root| !scope_path.starts_with(root)) {
        return "outside_project";
    }
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        let home = home.canonicalize().unwrap_or(home);
        if scope_path == home {
            return "home";
        }
    }
    if cwd.parent().is_some_and(|parent| parent == scope_path) {
        return "parent_dir";
    }
    if cwd.starts_with(scope_path) {
        return "ancestor";
    }
    "workspace"
}

fn memory_files_json(files: &[MemoryFileSummary]) -> Vec<serde_json::Value> {
    files.iter().map(MemoryFileSummary::json_value).collect()
}

fn unloaded_memory_candidates(
    cwd: &Path,
    project_root: Option<&Path>,
    files: &[MemoryFileSummary],
) -> Vec<String> {
    let mut loaded = files
        .iter()
        .map(|file| PathBuf::from(&file.path))
        .collect::<Vec<_>>();
    loaded.sort();

    let boundary = project_root.unwrap_or(cwd);
    let mut missing = Vec::new();
    let mut cursor = Some(cwd);
    while let Some(dir) = cursor {
        for name in ["CLAW.md", "AGENTS.md"] {
            let candidate = dir.join(name);
            if candidate.is_file() && !loaded.iter().any(|path| path == &candidate) {
                missing.push(candidate.display().to_string());
            }
        }
        if dir == boundary {
            break;
        }
        cursor = dir.parent();
    }
    missing.sort();
    missing.dedup();
    missing
}
#[derive(Debug, Clone)]
struct StatusContext {
    cwd: PathBuf,
    session_path: Option<PathBuf>,
    loaded_config_files: usize,
    discovered_config_files: usize,
    memory_file_count: usize,
    memory_files: Vec<MemoryFileSummary>,
    unloaded_memory_files: Vec<String>,
    project_root: Option<PathBuf>,
    git_branch: Option<String>,
    git_summary: GitWorkspaceSummary,
    branch_freshness: BranchFreshness,
    stale_base_state: BaseCommitState,
    session_lifecycle: SessionLifecycleSummary,
    boot_preflight: BootPreflightSnapshot,
    sandbox_status: runtime::SandboxStatus,
    binary_provenance: BinaryProvenance,
    /// #143: when `.claw.json` (or another loaded config file) fails to parse,
    /// we capture the parse error here and still populate every field that
    /// doesn't depend on runtime config (workspace, git, sandbox defaults,
    /// discovery counts). Top-level JSON output then reports
    /// `status: "degraded"` so claws can distinguish "status ran but config
    /// is broken" from "status ran cleanly".
    config_load_error: Option<String>,
    /// #143: machine-readable kind for the config load error, derived from
    /// `classify_error_kind`. Included in JSON output alongside the human
    /// readable string so downstream claws can switch on the kind token
    /// instead of regex-scraping the prose.
    config_load_error_kind: Option<&'static str>,
    mcp_validation: McpValidationSummary,

    hook_validation: HookValidationSummary,
    /// #468: duplicate global flag occurrences for provenance reporting
    duplicate_flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BinaryProvenance {
    git_sha: Option<String>,
    git_sha_short: Option<String>,
    is_dirty: bool,
    branch: Option<String>,
    commit_date: String,
    commit_timestamp: i64,
    rustc_version: String,
    target: Option<String>,
    build_date: String,
    executable_path: Option<String>,
    workspace_git_sha: Option<String>,
    workspace_match: Option<bool>,
    hint: Option<String>,
}

impl BinaryProvenance {
    fn status(&self) -> &'static str {
        if self.git_sha.is_some() {
            "known"
        } else {
            "unknown"
        }
    }

    fn json_value(&self) -> serde_json::Value {
        json!({
            "status": self.status(),
            "git_sha": self.git_sha,
            "git_sha_short": self.git_sha_short,
            "is_dirty": self.is_dirty,
            "branch": self.branch,
            "commit_date": self.commit_date,
            "commit_timestamp": self.commit_timestamp,
            "rustc_version": self.rustc_version,
            "target": self.target,
            "build_date": self.build_date,
            "executable_path": self.executable_path,
            "workspace_git_sha": self.workspace_git_sha,
            "workspace_match": self.workspace_match,
            "hint": self.hint,
        })
    }
}

fn known_build_metadata(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value == "unknown" {
        None
    } else {
        Some(value.to_string())
    }
}

fn parse_build_bool(value: Option<&str>) -> bool {
    value
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1")
}

fn parse_build_timestamp(value: Option<&str>) -> i64 {
    value
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

fn binary_provenance_for(cwd: Option<&Path>) -> BinaryProvenance {
    let git_sha = known_build_metadata(GIT_SHA);
    let git_sha_short = known_build_metadata(GIT_SHA_SHORT).or_else(|| {
        git_sha
            .as_ref()
            .map(|sha| sha.chars().take(12).collect::<String>())
    });
    let target = known_build_metadata(BUILD_TARGET);
    let workspace_git_sha = cwd.and_then(|cwd| {
        run_git_capture_in(cwd, &["rev-parse", "HEAD"])
            .map(|sha| sha.trim().to_string())
            .filter(|sha| !sha.is_empty())
    });
    let workspace_match = git_sha
        .as_deref()
        .zip(workspace_git_sha.as_deref())
        .map(|(binary, workspace)| binary == workspace);
    let hint = if git_sha.is_none() {
        Some(
            "Build metadata did not include a git SHA; rebuild from a git checkout before filing provenance-sensitive dogfood reports."
                .to_string(),
        )
    } else if workspace_match == Some(false) {
        Some(
            "The running binary was built from a different commit than the current workspace HEAD; rebuild or switch binaries before attributing behavior to this checkout."
                .to_string(),
        )
    } else {
        None
    };
    BinaryProvenance {
        git_sha,
        git_sha_short,
        is_dirty: parse_build_bool(GIT_DIRTY),
        branch: known_build_metadata(GIT_BRANCH),
        commit_date: known_build_metadata(GIT_COMMIT_DATE).unwrap_or_else(|| "unknown".to_string()),
        commit_timestamp: parse_build_timestamp(GIT_COMMIT_TIMESTAMP),
        rustc_version: known_build_metadata(RUSTC_VERSION).unwrap_or_else(|| "unknown".to_string()),
        target,
        build_date: DEFAULT_DATE.to_string(),
        executable_path: env::current_exe()
            .ok()
            .map(|path| path.display().to_string()),
        workspace_git_sha,
        workspace_match,
        hint,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BranchFreshness {
    upstream: Option<String>,
    ahead: u32,
    behind: u32,
    fresh: Option<bool>,
}

impl BranchFreshness {
    fn from_git_status(status: Option<&str>) -> Self {
        let first_line = status
            .and_then(|status| status.lines().next())
            .unwrap_or_default();
        let upstream = first_line
            .split_once("...")
            .and_then(|(_, rest)| rest.split([' ', '[']).next())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let mut ahead = 0;
        let mut behind = 0;
        if let Some((_, bracketed)) = first_line.split_once('[') {
            let bracketed = bracketed.trim_end_matches(']');
            for part in bracketed.split(',').map(str::trim) {
                if let Some(value) = part.strip_prefix("ahead ") {
                    ahead = value.parse().unwrap_or(0);
                } else if let Some(value) = part.strip_prefix("behind ") {
                    behind = value.parse().unwrap_or(0);
                }
            }
        }
        let fresh = upstream.as_ref().map(|_| behind == 0);
        Self {
            upstream,
            ahead,
            behind,
            fresh,
        }
    }

    fn json_value(&self) -> serde_json::Value {
        json!({
            "upstream": self.upstream,
            // #727: has_upstream disambiguates fresh:null-because-no-upstream
            // from fresh:null-because-unavailable; automation should check
            // has_upstream before branching on fresh.
            "has_upstream": self.upstream.is_some(),
            "ahead": self.ahead,
            "behind": self.behind,
            "fresh": self.fresh,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BinaryPreflight {
    name: &'static str,
    available: bool,
}

impl BinaryPreflight {
    fn json_value(&self) -> serde_json::Value {
        json!({
            "name": self.name,
            "available": self.available,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ControlSocketPreflight {
    name: &'static str,
    configured: bool,
    exists: bool,
    path: Option<String>,
}

impl ControlSocketPreflight {
    fn json_value(&self) -> serde_json::Value {
        json!({
            "name": self.name,
            "configured": self.configured,
            "exists": self.exists,
            "path": self.path,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BootPreflightSnapshot {
    repo_exists: bool,
    worktree_exists: bool,
    git_dir_exists: bool,
    branch_freshness: BranchFreshness,
    trust_gate_allowed: Option<bool>,
    trusted_roots_count: usize,
    required_binaries: Vec<BinaryPreflight>,
    control_sockets: Vec<ControlSocketPreflight>,
    mcp_startup_eligible: bool,
    mcp_servers_configured: usize,
    plugin_startup_eligible: bool,
    plugins_configured: usize,
    last_failed_boot_reason: Option<String>,
}

impl BootPreflightSnapshot {
    fn json_value(&self) -> serde_json::Value {
        json!({
            "repo": {
                "exists": self.repo_exists,
                "worktree_exists": self.worktree_exists,
                "git_dir_exists": self.git_dir_exists,
            },
            "branch_freshness": self.branch_freshness.json_value(),
            "trust_gate": {
                "allowlisted": self.trust_gate_allowed,
                "trusted_roots_count": self.trusted_roots_count,
            },
            "required_binaries": self.required_binaries.iter().map(BinaryPreflight::json_value).collect::<Vec<_>>(),
            "control_sockets": self.control_sockets.iter().map(ControlSocketPreflight::json_value).collect::<Vec<_>>(),
            "mcp_startup": {
                "eligible": self.mcp_startup_eligible,
                "servers_configured": self.mcp_servers_configured,
            },
            "plugin_startup": {
                "eligible": self.plugin_startup_eligible,
                "plugins_configured": self.plugins_configured,
            },
            "last_failed_boot_reason": self.last_failed_boot_reason,
        })
    }

    fn summary(&self) -> String {
        let trust = self
            .trust_gate_allowed
            .map(|value| {
                if value {
                    "allowlisted"
                } else {
                    "not allowlisted"
                }
            })
            .unwrap_or("unknown");
        let freshness = self
            .branch_freshness
            .fresh
            .map(|fresh| if fresh { "fresh" } else { "behind" })
            .unwrap_or("no upstream");
        format!(
            "repo={} worktree={} branch={} trust={} mcp={} plugins={} last_failed={}",
            self.repo_exists,
            self.worktree_exists,
            freshness,
            trust,
            self.mcp_startup_eligible,
            self.plugin_startup_eligible,
            self.last_failed_boot_reason.as_deref().unwrap_or("none")
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct StatusUsage {
    message_count: usize,
    turns: u32,
    latest: TokenUsage,
    cumulative: TokenUsage,
    estimated_tokens: usize,
}

#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GitWorkspaceSummary {
    changed_files: usize,
    staged_files: usize,
    unstaged_files: usize,
    untracked_files: usize,
    conflicted_files: usize,
    /// #89: detected mid-operation git state (rebase, merge, cherry-pick, bisect)
    operation: GitOperation,
}

/// #89: mid-operation git states detected from branch header in `git status --short --branch`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum GitOperation {
    #[default]
    None,
    Rebase,
    Merge,
    CherryPick,
    Bisect,
}

impl GitOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Rebase => "rebase-in-progress",
            Self::Merge => "merge-in-progress",
            Self::CherryPick => "cherry-pick-in-progress",
            Self::Bisect => "bisect-in-progress",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionLifecycleKind {
    RunningProcess,
    IdleShell,
    SavedOnly,
}

impl SessionLifecycleKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RunningProcess => "running_process",
            Self::IdleShell => "idle_shell",
            Self::SavedOnly => "saved_only",
        }
    }

    fn human_label(self) -> &'static str {
        match self {
            Self::RunningProcess => "running process",
            Self::IdleShell => "idle shell",
            Self::SavedOnly => "saved only",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionLifecycleSummary {
    kind: SessionLifecycleKind,
    pane_id: Option<String>,
    pane_command: Option<String>,
    pane_path: Option<PathBuf>,
    workspace_dirty: bool,
    abandoned: bool,
    // #326: all panes matching this workspace, not just the first one
    all_panes: Vec<TmuxPaneSnapshot>,
}

impl SessionLifecycleSummary {
    fn signal(&self) -> String {
        let mut parts = vec![self.kind.human_label().to_string()];
        if self.workspace_dirty {
            parts.push("dirty worktree".to_string());
        }
        if self.abandoned {
            parts.push("abandoned?".to_string());
        }
        if let Some(command) = self.pane_command.as_deref() {
            parts.push(format!("cmd={command}"));
        }
        parts.join(" · ")
    }

    fn json_value(&self) -> serde_json::Value {
        json!({
            "kind": self.kind.as_str(),
            "pane_id": self.pane_id,
            "pane_command": self.pane_command,
            "pane_path": self.pane_path.as_ref().map(|path| path.display().to_string()),
            "workspace_dirty": self.workspace_dirty,
            "abandoned": self.abandoned,
            // #326: include all workspace panes in the JSON output
            "panes": self.all_panes.iter().map(|p| {
                json!({
                    "pane_id": p.pane_id,
                    "pane_command": p.current_command,
                    "pane_path": p.current_path.display().to_string(),
                })
            }).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TmuxPaneSnapshot {
    pane_id: String,
    current_command: String,
    current_path: PathBuf,
}

impl GitWorkspaceSummary {
    fn is_clean(self) -> bool {
        self.changed_files == 0
    }

    fn headline(self) -> String {
        // #89: prefix with operation state when mid-operation
        let op_prefix = if self.operation != GitOperation::None {
            format!("{}, ", self.operation.as_str())
        } else {
            String::new()
        };
        if self.is_clean() {
            if self.operation != GitOperation::None {
                format!("{op_prefix}clean")
            } else {
                "clean".to_string()
            }
        } else {
            let mut details = Vec::new();
            if self.staged_files > 0 {
                details.push(format!("{} staged", self.staged_files));
            }
            if self.unstaged_files > 0 {
                details.push(format!("{} unstaged", self.unstaged_files));
            }
            if self.untracked_files > 0 {
                details.push(format!("{} untracked", self.untracked_files));
            }
            if self.conflicted_files > 0 {
                details.push(format!("{} conflicted", self.conflicted_files));
            }
            format!(
                "{op_prefix}dirty · {} files · {}",
                self.changed_files,
                details.join(", ")
            )
        }
    }
}

fn classify_session_lifecycle_for(workspace: &Path) -> SessionLifecycleSummary {
    classify_session_lifecycle_from_panes(workspace, discover_tmux_panes())
}

fn classify_session_lifecycle_from_panes(
    workspace: &Path,
    panes: Vec<TmuxPaneSnapshot>,
) -> SessionLifecycleSummary {
    let workspace_dirty = git_worktree_is_dirty(workspace);
    let mut idle_shell: Option<TmuxPaneSnapshot> = None;
    let mut all_workspace_panes: Vec<TmuxPaneSnapshot> = Vec::new();
    let mut running_pane: Option<TmuxPaneSnapshot> = None;
    for pane in panes {
        if !pane_path_matches_workspace(&pane.current_path, workspace) {
            continue;
        }
        all_workspace_panes.push(pane.clone());
        if is_idle_shell_command(&pane.current_command) {
            idle_shell.get_or_insert(pane);
        } else if running_pane.is_none() {
            running_pane = Some(pane);
        }
    }

    if let Some(pane) = running_pane {
        return SessionLifecycleSummary {
            kind: SessionLifecycleKind::RunningProcess,
            pane_id: Some(pane.pane_id),
            pane_command: Some(pane.current_command),
            pane_path: Some(pane.current_path),
            workspace_dirty,
            abandoned: false,
            all_panes: all_workspace_panes,
        };
    }

    if let Some(pane) = idle_shell {
        SessionLifecycleSummary {
            kind: SessionLifecycleKind::IdleShell,
            pane_id: Some(pane.pane_id),
            pane_command: Some(pane.current_command),
            pane_path: Some(pane.current_path),
            workspace_dirty,
            abandoned: workspace_dirty,
            all_panes: all_workspace_panes,
        }
    } else {
        SessionLifecycleSummary {
            kind: SessionLifecycleKind::SavedOnly,
            pane_id: None,
            pane_command: None,
            pane_path: None,
            workspace_dirty,
            abandoned: workspace_dirty,
            all_panes: all_workspace_panes,
        }
    }
}

fn discover_tmux_panes() -> Vec<TmuxPaneSnapshot> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{pane_current_command}\t#{pane_current_path}",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_tmux_pane_snapshots(&stdout)
}

fn parse_tmux_pane_snapshots(output: &str) -> Vec<TmuxPaneSnapshot> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let pane_id = fields.next()?.trim();
            let current_command = fields.next()?.trim();
            let current_path = fields.next()?.trim();
            if pane_id.is_empty() || current_path.is_empty() {
                return None;
            }
            Some(TmuxPaneSnapshot {
                pane_id: pane_id.to_string(),
                current_command: current_command.to_string(),
                current_path: PathBuf::from(current_path),
            })
        })
        .collect()
}

fn pane_path_matches_workspace(pane_path: &Path, workspace: &Path) -> bool {
    if pane_path == workspace || pane_path.starts_with(workspace) {
        return true;
    }
    let pane_path = fs::canonicalize(pane_path).unwrap_or_else(|_| pane_path.to_path_buf());
    let workspace = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    pane_path == workspace || pane_path.starts_with(&workspace)
}

fn is_idle_shell_command(command: &str) -> bool {
    let command = command.rsplit('/').next().unwrap_or(command);
    matches!(
        command,
        "bash" | "zsh" | "sh" | "fish" | "nu" | "pwsh" | "powershell" | "cmd"
    )
}

fn git_worktree_is_dirty(workspace: &Path) -> bool {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["status", "--porcelain"])
        .output();
    output
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| !output.stdout.is_empty())
}

#[cfg(test)]
fn format_unknown_slash_command_message(name: &str) -> String {
    let suggestions = suggest_slash_commands(name);
    let mut message = format!("unknown slash command: /{name}.");
    if !suggestions.is_empty() {
        message.push_str(" Did you mean ");
        message.push_str(&suggestions.join(", "));
        message.push('?');
    }
    if let Some(note) = omc_compatibility_note_for_unknown_slash_command(name) {
        message.push(' ');
        message.push_str(note);
    }
    message.push_str(" Use /help to list available commands.");
    message
}

fn format_model_report(model: &str, message_count: usize, turns: u32) -> String {
    format!(
        "Model
  Current model    {model}
  Session messages {message_count}
  Session turns    {turns}

Usage
  Inspect current model with /model
  Switch models with /model <name>"
    )
}

fn format_model_switch_report(previous: &str, next: &str, message_count: usize) -> String {
    format!(
        "Model updated
  Previous         {previous}
  Current          {next}
  Preserved msgs   {message_count}"
    )
}

fn format_permissions_report(mode: &str) -> String {
    let modes = [
        ("read-only", "Read/search tools only", mode == "read-only"),
        (
            "workspace-write",
            "Edit files inside the workspace",
            mode == "workspace-write",
        ),
        (
            "danger-full-access",
            "Unrestricted tool access",
            mode == "danger-full-access",
        ),
    ]
    .into_iter()
    .map(|(name, description, is_current)| {
        let marker = if is_current {
            "● current"
        } else {
            "○ available"
        };
        format!("  {name:<18} {marker:<11} {description}")
    })
    .collect::<Vec<_>>()
    .join(
        "
",
    );

    format!(
        "Permissions
  Active mode      {mode}
  Mode status      live session default

Modes
{modes}

Usage
  Inspect current mode with /permissions
  Switch modes with /permissions <mode>"
    )
}

fn format_permissions_switch_report(previous: &str, next: &str) -> String {
    format!(
        "Permissions updated
  Result           mode switched
  Previous mode    {previous}
  Active mode      {next}
  Applies to       subsequent tool calls
  Usage            /permissions to inspect current mode"
    )
}

fn format_cost_report(usage: TokenUsage) -> String {
    let estimated_cost = usage.estimate_cost_usd();
    format!(
        "Cost
  Input tokens     {}
  Output tokens    {}
  Cache create     {}
  Cache read       {}
  Total tokens     {}
  Estimated cost   {}",
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
        usage.total_tokens(),
        format_usd(estimated_cost.total_cost_usd()),
    )
}

fn format_resume_report(session_path: &str, message_count: usize, turns: u32) -> String {
    format!(
        "Session resumed
  Session file     {session_path}
  Messages         {message_count}
  Turns            {turns}"
    )
}

fn render_resume_usage() -> String {
    format!(
        "Resume
  Usage            /resume <session-path|session-id|{LATEST_SESSION_REFERENCE}>
  Auto-save        .claw/sessions/<workspace-fingerprint>/<session-id>.{PRIMARY_SESSION_EXTENSION}
  Tip              use /session list to inspect saved sessions"
    )
}

fn format_compact_report(removed: usize, resulting_messages: usize, skipped: bool) -> String {
    if skipped {
        format!(
            "Compact
  Result           skipped
  Reason           session below compaction threshold
  Messages kept    {resulting_messages}"
        )
    } else {
        format!(
            "Compact
  Result           compacted
  Messages removed {removed}
  Messages kept    {resulting_messages}"
        )
    }
}

fn format_auto_compaction_notice(removed: usize) -> String {
    format!("[auto-compacted: removed {removed} messages]")
}

fn parse_git_status_metadata(status: Option<&str>) -> (Option<PathBuf>, Option<String>) {
    parse_git_status_metadata_for(
        &env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        status,
    )
}

fn parse_git_status_branch(status: Option<&str>) -> Option<String> {
    let status = status?;
    let first_line = status.lines().next()?;
    let line = first_line.strip_prefix("## ")?;
    if line.starts_with("HEAD") {
        return Some("detached HEAD".to_string());
    }
    let branch = line.split(['.', ' ']).next().unwrap_or_default().trim();
    if branch.is_empty() {
        None
    } else {
        Some(branch.to_string())
    }
}

fn parse_git_workspace_summary(status: Option<&str>) -> GitWorkspaceSummary {
    let mut summary = GitWorkspaceSummary::default();
    let Some(status) = status else {
        return summary;
    };

    for line in status.lines() {
        if line.starts_with("## ") {
            // #89: detect mid-operation states from branch header
            // git status --short --branch shows:
            //   "## HEAD (no branch, rebasing feature-branch)"
            //   "## main [merge-in-progress]"
            //   "## HEAD (no branch, cherry-pick-in-progress)"
            //   "## main (no branch, bisect-in-progress)"
            let header = line.to_ascii_lowercase();
            if header.contains("rebasing") {
                summary.operation = GitOperation::Rebase;
            } else if header.contains("merge-in-progress") {
                summary.operation = GitOperation::Merge;
            } else if header.contains("cherry-pick-in-progress") {
                summary.operation = GitOperation::CherryPick;
            } else if header.contains("bisect-in-progress") {
                summary.operation = GitOperation::Bisect;
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }

        summary.changed_files += 1;
        let mut chars = line.chars();
        let index_status = chars.next().unwrap_or(' ');
        let worktree_status = chars.next().unwrap_or(' ');

        if index_status == '?' && worktree_status == '?' {
            summary.untracked_files += 1;
            continue;
        }

        if index_status != ' ' {
            summary.staged_files += 1;
        }
        if worktree_status != ' ' {
            summary.unstaged_files += 1;
        }
        if (matches!(index_status, 'U' | 'A') && matches!(worktree_status, 'U' | 'A'))
            || index_status == 'U'
            || worktree_status == 'U'
        {
            summary.conflicted_files += 1;
        }
    }

    summary
}

fn build_boot_preflight_snapshot(
    cwd: &Path,
    project_root: Option<&Path>,
    git_status: Option<&str>,
    runtime_config: Option<&runtime::RuntimeConfig>,
    config_load_error: Option<&str>,
) -> BootPreflightSnapshot {
    let branch_freshness = BranchFreshness::from_git_status(git_status);
    let worktree_exists = run_git_bool(cwd, &["rev-parse", "--is-inside-work-tree"]);
    let git_dir_exists = run_git_capture_in(cwd, &["rev-parse", "--git-dir"])
        .map(|path| {
            let path = PathBuf::from(path.trim());
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
        .is_some_and(|path| path.exists());
    let trusted_roots = runtime_config
        .map(runtime::RuntimeConfig::trusted_roots)
        .unwrap_or(&[]);
    let trust_gate_allowed = runtime_config.map(|_| {
        trusted_roots
            .iter()
            .any(|root| path_matches_trusted_root_local(cwd, root))
    });
    let plugin_configured = runtime_config
        .map(|config| config.plugins().enabled_plugins().len())
        .unwrap_or_default();
    let mcp_configured = runtime_config
        .map(|config| config.mcp().servers().len())
        .unwrap_or_default();
    let config_ok = config_load_error.is_none();
    BootPreflightSnapshot {
        repo_exists: project_root.is_some_and(Path::exists),
        worktree_exists,
        git_dir_exists,
        branch_freshness,
        trust_gate_allowed,
        trusted_roots_count: trusted_roots.len(),
        required_binaries: vec![
            BinaryPreflight {
                name: "claw",
                available: env::current_exe().is_ok_and(|path| path.exists()),
            },
            BinaryPreflight {
                name: "git",
                available: command_available("git"),
            },
            BinaryPreflight {
                name: "tmux",
                available: command_available("tmux"),
            },
        ],
        control_sockets: vec![tmux_control_socket_preflight()],
        mcp_startup_eligible: config_ok,
        mcp_servers_configured: mcp_configured,
        plugin_startup_eligible: config_ok,
        plugins_configured: plugin_configured,
        last_failed_boot_reason: last_failed_boot_reason(cwd),
    }
}

fn run_git_bool(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn tmux_control_socket_preflight() -> ControlSocketPreflight {
    let path = env::var("TMUX")
        .ok()
        .and_then(|value| value.split(',').next().map(str::to_string))
        .filter(|value| !value.is_empty());
    let exists = path.as_ref().is_some_and(|path| Path::new(path).exists());
    ControlSocketPreflight {
        name: "tmux",
        configured: path.is_some(),
        exists,
        path,
    }
}

fn last_failed_boot_reason(cwd: &Path) -> Option<String> {
    env::var("CLAW_LAST_FAILED_BOOT_REASON")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            fs::read_to_string(cwd.join(".claw").join("last-failed-boot.txt"))
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn path_matches_trusted_root_local(cwd: &Path, trusted_root: &str) -> bool {
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let trusted_root = Path::new(trusted_root);
    let trusted_root = if trusted_root.is_absolute() {
        trusted_root.to_path_buf()
    } else {
        cwd.join(trusted_root)
    };
    let trusted_root = fs::canonicalize(&trusted_root).unwrap_or(trusted_root);
    cwd == trusted_root || cwd.starts_with(trusted_root)
}

fn resolve_git_branch_for(cwd: &Path) -> Option<String> {
    let branch = run_git_capture_in(cwd, &["branch", "--show-current"])?;
    let branch = branch.trim();
    if !branch.is_empty() {
        return Some(branch.to_string());
    }

    let fallback = run_git_capture_in(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let fallback = fallback.trim();
    if fallback.is_empty() {
        None
    } else if fallback == "HEAD" {
        Some("detached HEAD".to_string())
    } else {
        Some(fallback.to_string())
    }
}

fn run_git_capture_in(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn find_git_root_in(cwd: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        return Err("not a git repository".into());
    }
    let path = String::from_utf8(output.stdout)?.trim().to_string();
    if path.is_empty() {
        return Err("empty git root".into());
    }
    Ok(PathBuf::from(path))
}

fn parse_git_status_metadata_for(
    cwd: &Path,
    status: Option<&str>,
) -> (Option<PathBuf>, Option<String>) {
    let branch = resolve_git_branch_for(cwd).or_else(|| parse_git_status_branch(status));
    let project_root = find_git_root_in(cwd).ok();
    (project_root, branch)
}

#[allow(clippy::too_many_lines)]
fn run_resume_command(
    session_path: &Path,
    session: &Session,
    command: &SlashCommand,
) -> Result<ResumeCommandOutcome, Box<dyn std::error::Error>> {
    let session_list_outcome = || -> Result<ResumeCommandOutcome, Box<dyn std::error::Error>> {
        let sessions = list_managed_sessions().unwrap_or_default();
        let session_ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
        let session_details = session_details_json(&sessions);
        let active_id = session.session_id.clone();
        let text = render_session_list(&active_id).unwrap_or_else(|e| format!("error: {e}"));
        Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(text),
            json: Some(serde_json::json!({
                "kind": "sessions",
                "status": "ok",
                "action": "list",
                "sessions": session_ids,
                "session_details": session_details,
                "active": active_id,
            })),
        })
    };

    match command {
        SlashCommand::Help => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_repl_help()),
            json: Some(
                serde_json::json!({ "kind": "help", "action": "help", "status": "ok", "message": render_repl_help() }),
            ),
        }),
        SlashCommand::Compact => {
            let result = runtime::trident::trident_compact_session(
                session,
                CompactionConfig {
                    max_estimated_tokens: 0,
                    ..CompactionConfig::default()
                },
                &runtime::trident::TridentConfig::default(),
            );
            let removed = result.removed_message_count;
            let kept = result.compacted_session.messages.len();
            let skipped = removed == 0;
            result.compacted_session.save_to_path(session_path)?;
            Ok(ResumeCommandOutcome {
                session: result.compacted_session,
                message: Some(format_compact_report(removed, kept, skipped)),
                json: Some(serde_json::json!({
                    "kind": "compact",
                    "skipped": skipped,
                    "removed_messages": removed,
                    "kept_messages": kept,
                })),
            })
        }
        SlashCommand::Clear { confirm } => {
            if !confirm {
                return Ok(ResumeCommandOutcome {
                    session: session.clone(),
                    message: Some(
                        "clear: confirmation required; rerun with /clear --confirm".to_string(),
                    ),
                    json: Some(serde_json::json!({
                        "kind": "error",
                        "error": "confirmation required",
                        "hint": "rerun with /clear --confirm",
                    })),
                });
            }
            let backup_path = write_session_clear_backup(session, session_path)?;
            // #114: preserve the session_id from the file to avoid filename/meta-header
            // divergence. /clear is "empty this session," not "fork to a new session."
            let previous_session_id = session.session_id.clone();
            let mut cleared = new_cli_session()?;
            cleared.session_id = previous_session_id.clone();
            cleared.save_to_path(session_path)?;
            Ok(ResumeCommandOutcome {
                session: cleared,
                message: Some(format!(
                    "Session cleared\n  Mode             resumed session reset\n  Previous session {previous_session_id}\n  Backup           {}\n  Resume previous  claw --resume {}\n  Session file     {}",
                    backup_path.display(),
                    backup_path.display(),
                    session_path.display()
                )),
                json: Some(serde_json::json!({
                    "kind": "clear",
                    "previous_session_id": previous_session_id,
                    "new_session_id": previous_session_id,
                    "backup": backup_path.display().to_string(),
                    "session_file": session_path.display().to_string(),
                })),
            })
        }
        SlashCommand::Status => {
            let tracker = UsageTracker::from_session(session);
            let usage = tracker.cumulative_usage();
            let context = status_context(Some(session_path))?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format_status_report(
                    session.model.as_deref().unwrap_or("restored-session"),
                    StatusUsage {
                        message_count: session.messages.len(),
                        turns: tracker.turns(),
                        latest: tracker.current_turn_usage(),
                        cumulative: usage,
                        estimated_tokens: 0,
                    },
                    default_permission_mode().as_str(),
                    &context,
                    None, // #148: resumed sessions don't have flag provenance
                    None,
                )),
                json: Some(status_json_value(
                    session.model.as_deref(),
                    StatusUsage {
                        message_count: session.messages.len(),
                        turns: tracker.turns(),
                        latest: tracker.current_turn_usage(),
                        cumulative: usage,
                        estimated_tokens: 0,
                    },
                    default_permission_mode().as_str(),
                    &context,
                    None, // #148: resumed sessions don't have flag provenance
                    None,
                    None,
                    None,
                )),
            })
        }
        SlashCommand::Sandbox => {
            let cwd = env::current_dir()?;
            let loader = ConfigLoader::default_for(&cwd);
            let runtime_config = loader.load()?;
            let status = resolve_sandbox_status(runtime_config.sandbox(), &cwd);
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format_sandbox_report(&status)),
                json: Some(sandbox_json_value(&status)),
            })
        }
        SlashCommand::Cost => {
            let usage = UsageTracker::from_session(session).cumulative_usage();
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format_cost_report(usage)),
                json: Some(serde_json::json!({
                    "kind": "cost",
                    "action": "show",
                    "status": "ok",
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                    "cache_read_input_tokens": usage.cache_read_input_tokens,
                    "total_tokens": usage.total_tokens(),
                    "estimated_cost_usd": format_usd(usage.estimate_cost_usd().total_cost_usd()), "estimated_cost_usd_num": usage.estimate_cost_usd().total_cost_usd(),
                    "pricing": "estimated-default",
                })),
            })
        }
        SlashCommand::Config { section } => {
            let message = render_config_report(section.as_deref())?;
            let json = render_config_json(section.as_deref())?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(message),
                json: Some(json),
            })
        }
        SlashCommand::Mcp { action, target } => {
            let cwd = env::current_dir()?;
            let args = match (action.as_deref(), target.as_deref()) {
                (None, None) => None,
                (Some(action), None) => Some(action.to_string()),
                (Some(action), Some(target)) => Some(format!("{action} {target}")),
                (None, Some(target)) => Some(target.to_string()),
            };
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(handle_mcp_slash_command(args.as_deref(), &cwd)?),
                json: Some(handle_mcp_slash_command_json(args.as_deref(), &cwd)?),
            })
        }
        SlashCommand::Memory => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_memory_report()?),
            json: Some(render_memory_json()?),
        }),
        SlashCommand::Init => {
            // #142: run the init once, then render both text + structured JSON
            // from the same InitReport so both surfaces stay in sync.
            let cwd = env::current_dir()?;
            let report = crate::init::initialize_repo(&cwd)?;
            let message = report.render();
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(message.clone()),
                json: Some(init_json_value(&report, &message)),
            })
        }
        SlashCommand::Diff => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let message = render_diff_report_for(&cwd)?;
            let json = render_diff_json_for(&cwd)?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(message),
                json: Some(json),
            })
        }
        SlashCommand::Version => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_version_report()),
            json: Some(version_json_value()),
        }),
        SlashCommand::Export { path } => {
            let export_path = resolve_export_path(path.as_deref(), session)?;
            fs::write(&export_path, render_export_text(session))?;
            let msg_count = session.messages.len();
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format!(
                    "Export\n  Result           wrote transcript\n  File             {}\n  Messages         {}",
                    export_path.display(),
                    msg_count,
                )),
                json: Some(serde_json::json!({
                    "kind": "export",
                    "action": "export",
                    "status": "ok",
                    "file": export_path.display().to_string(),
                    "message_count": msg_count,
                })),
            })
        }
        SlashCommand::Agents { args } => {
            let cwd = env::current_dir()?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(handle_agents_slash_command(args.as_deref(), &cwd)?),
                json: Some(
                    serde_json::to_value(handle_agents_slash_command_json(args.as_deref(), &cwd)?)
                        .unwrap_or(Value::Null),
                ),
            })
        }
        SlashCommand::Skills { args } => {
            if let SkillSlashDispatch::Invoke(_) = classify_skills_slash_command(args.as_deref()) {
                // #779: use interactive_only: prefix + \n hint so #776 classify/split emits
                // error_kind:interactive_only + non-null hint instead of unknown+null.
                let skill_name = args.as_deref().unwrap_or("<skill>");
                return Err(format!(
                    "interactive_only: /skills {skill_name} invocation requires a live session.\nStart `claw` and run `/skills {skill_name}` inside the REPL, or use `claw -p <prompt>` with skill context."
                ).into());
            }
            let cwd = env::current_dir()?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(handle_skills_slash_command(args.as_deref(), &cwd)?),
                json: Some(handle_skills_slash_command_json(args.as_deref(), &cwd)?),
            })
        }
        SlashCommand::Plugins { action, target } => {
            // Only list is supported in resume mode (no runtime to reload)
            match action.as_deref() {
                Some(action @ ("install" | "uninstall" | "enable" | "disable" | "update")) => {
                    // #777: use interactive_only: prefix + \n hint so #776's classify/split
                    // emits error_kind:interactive_only + non-null hint instead of unknown+null.
                    // Orchestrators can now detect this and switch to a live REPL instead of retrying.
                    return Err(format!(
                        "interactive_only: /plugins {action} requires a live session to reload the plugin runtime.\nStart `claw` and run `/plugins {action}` inside the REPL, or use `claw plugins {action}` as a direct CLI command."
                    ).into());
                }
                _ => {}
            }
            let cwd = env::current_dir()?;
            let payload = plugins_command_payload_for(
                &cwd,
                action.as_deref(),
                target.as_deref(),
                ConfigWarningMode::EmitStderr,
            )?;
            let action_str = action.as_deref().unwrap_or("list");
            let enabled_count = payload
                .plugins
                .iter()
                .filter(|p| p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false))
                .count();
            let disabled_count = payload.plugins.len().saturating_sub(enabled_count);
            let mut json = serde_json::json!({
                "kind": "plugin",
                "action": action_str,
                "status": payload.status,
                "summary": {
                    "total": payload.plugins.len(),
                    "enabled": enabled_count,
                    "disabled": disabled_count,
                    "load_failures": payload.load_failures.len(),
                },
                "config_load_error": payload.config_load_error,
                "mcp_validation": payload.mcp_validation.json_value(),
                "plugins": payload.plugins,
                "load_failures": payload.load_failures,
            });
            if action_str != "list" {
                json["target"] = serde_json::json!(target);
                json["reload_runtime"] = serde_json::json!(payload.reload_runtime);
                json["message"] = serde_json::json!(&payload.message);
            }
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(payload.message),
                json: Some(json),
            })
        }
        SlashCommand::Doctor => {
            let report = render_doctor_report(
                ConfigWarningMode::EmitStderr,
                permission_mode_provenance_for_current_dir(),
            )?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(report.render()),
                json: Some(report.json_value()),
            })
        }
        SlashCommand::Stats => {
            let usage = UsageTracker::from_session(session).cumulative_usage();
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format_cost_report(usage)),
                json: Some(serde_json::json!({
                    "kind": "stats",
                    "action": "show",
                    "status": "ok",
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                    "cache_read_input_tokens": usage.cache_read_input_tokens,
                    "total_tokens": usage.total_tokens(),
                    "estimated_cost_usd": format_usd(usage.estimate_cost_usd().total_cost_usd()), "estimated_cost_usd_num": usage.estimate_cost_usd().total_cost_usd(),
                    "pricing": "estimated-default",
                })),
            })
        }
        SlashCommand::History { count } => {
            let limit = parse_history_count(count.as_deref())
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
            let entries = collect_session_prompt_history(session);
            let shown: Vec<_> = entries.iter().rev().take(limit).rev().collect();
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(render_prompt_history_report(&entries, limit)),
                json: Some(serde_json::json!({
                    "kind": "history",
                    "action": "list",
                    "status": "ok",
                    "total": entries.len(),
                    "showing": shown.len(),
                    "entries": shown.iter().map(|e| serde_json::json!({
                        "timestamp_ms": e.timestamp_ms,
                        "text": e.text,
                    })).collect::<Vec<_>>(),
                })),
            })
        }
        SlashCommand::Unknown(name) => Err(format_unknown_slash_command(name).into()),
        // /session list/exists/delete can be served from the managed sessions directory
        // in resume mode without starting an interactive REPL. Mutating delete remains
        // opt-in through /session delete <id> --force so JSON callers never hang on a prompt.
        SlashCommand::Session { action, target } => {
            run_resumed_session_command(session_path, session, action.as_deref(), target.as_deref())
        }
        // #341: /tasks is resume-supported — return a no-op with structured JSON
        SlashCommand::Tasks { args } => {
            let args_str = args.as_deref().unwrap_or_default();
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format!(
                    "Tasks\n  Note           Background tasks are only available in the interactive REPL.\n  Command        /tasks {args_str}"
                )),
                json: Some(serde_json::json!({
                    "kind": "tasks",
                    "action": "list",
                    "status": "ok",
                    "note": "Background tasks are only available in the interactive REPL.",
                    "args": args_str,
                })),
            })
        }
        // #343: /model is resume-safe — returns model configuration
        SlashCommand::Model { model } => {
            let configured_model = config_model_for_current_dir();
            let resolved_config_model = configured_model
                .as_deref()
                .map(resolve_model_alias_with_config);
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format!(
                    "Models\n  Default          {}\n  Config model     {}",
                    DEFAULT_MODEL,
                    configured_model.as_deref().unwrap_or("<unset>")
                )),
                json: Some(serde_json::json!({
                    "kind": "models",
                    "action": "list",
                    "status": "ok",
                    "default_model": DEFAULT_MODEL,
                    "configured_model": configured_model,
                    "resolved_model": resolved_config_model,
                    "requested_model": model,
                })),
            })
        }
        SlashCommand::Bughunter { .. }
        | SlashCommand::Commit { .. }
        | SlashCommand::Pr { .. }
        | SlashCommand::Issue { .. }
        | SlashCommand::Ultraplan { .. }
        | SlashCommand::Teleport { .. }
        | SlashCommand::DebugToolCall { .. }
        | SlashCommand::Resume { .. }
        | SlashCommand::Permissions { .. }
        | SlashCommand::Login
        | SlashCommand::Logout
        | SlashCommand::Vim
        | SlashCommand::Upgrade
        | SlashCommand::Share
        | SlashCommand::Feedback
        | SlashCommand::Files
        | SlashCommand::Fast
        | SlashCommand::Exit
        | SlashCommand::Summary
        | SlashCommand::Desktop
        | SlashCommand::Brief
        | SlashCommand::Advisor
        | SlashCommand::Stickers
        | SlashCommand::Insights
        | SlashCommand::Thinkback
        | SlashCommand::ReleaseNotes
        | SlashCommand::SecurityReview
        | SlashCommand::Keybindings
        | SlashCommand::PrivacySettings
        | SlashCommand::Plan { .. }
        | SlashCommand::Review { .. }
        | SlashCommand::Theme { .. }
        | SlashCommand::Voice { .. }
        | SlashCommand::Usage { .. }
        | SlashCommand::Rename { .. }
        | SlashCommand::Copy { .. }
        | SlashCommand::Hooks { .. }
        | SlashCommand::Context { .. }
        | SlashCommand::Color { .. }
        | SlashCommand::Effort { .. }
        | SlashCommand::Branch { .. }
        | SlashCommand::Rewind { .. }
        | SlashCommand::Ide { .. }
        | SlashCommand::Tag { .. }
        | SlashCommand::OutputStyle { .. }
        | SlashCommand::AddDir { .. }
        | SlashCommand::Team { .. }
        | SlashCommand::Setup => Err("unsupported resumed slash command".into()),
    }
}

/// Detect if the current working directory is "broad" (home directory or
/// filesystem root). Returns the cwd path if broad, None otherwise.
fn detect_broad_cwd() -> Option<PathBuf> {
    let Ok(cwd) = env::current_dir() else {
        return None;
    };
    let is_home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .is_some_and(|h| Path::new(&h) == cwd);
    let is_root = cwd.parent().is_none();
    if is_home || is_root {
        Some(cwd)
    } else {
        None
    }
}

/// Enforce the broad-CWD policy: when running from home or root, either
/// require the --allow-broad-cwd flag, or prompt for confirmation (interactive),
/// or exit with an error (non-interactive).
fn enforce_broad_cwd_policy(
    allow_broad_cwd: bool,
    output_format: CliOutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    if allow_broad_cwd {
        return Ok(());
    }
    let Some(cwd) = detect_broad_cwd() else {
        return Ok(());
    };

    let is_interactive = io::stdin().is_terminal();

    if is_interactive {
        // Interactive mode: print warning and ask for confirmation
        eprintln!(
            "Warning: claw is running from a very broad directory ({}).\n\
             The agent can read and search everything under this path.\n\
             Consider running from inside your project: cd /path/to/project && claw",
            cwd.display()
        );
        eprint!("Continue anyway? [y/N]: ");
        io::stderr().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim().to_lowercase();
        if trimmed != "y" && trimmed != "yes" {
            eprintln!("Aborted.");
            std::process::exit(0);
        }
        Ok(())
    } else {
        // Non-interactive mode: exit with error (JSON or text)
        let message = format!(
            "claw is running from a very broad directory ({}). \
             The agent can read and search everything under this path. \
             Use --allow-broad-cwd to proceed anyway, \
             or run from inside your project: cd /path/to/project && claw",
            cwd.display()
        );
        match output_format {
            CliOutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::json!({
                        "kind": "broad_cwd",
                        "action": "abort",
                        "status": "error",
                        "error_kind": "broad_cwd",
                        "error": message,
                        "hint": "Change to a more specific project directory, or use --cwd to set the workspace root.",
                        "exit_code": 1,
                    })
                );
            }
            CliOutputFormat::Text => {
                eprintln!("error: {message}");
            }
        }
        std::process::exit(1);
    }
}

fn stale_base_state_for(cwd: &Path, flag_value: Option<&str>) -> BaseCommitState {
    let source = resolve_expected_base(flag_value, cwd);
    check_base_commit(cwd, source.as_ref())
}

fn stale_base_json_value(state: &BaseCommitState) -> serde_json::Value {
    match state {
        BaseCommitState::Matches => json!({"status": "matches", "fresh": true}),
        BaseCommitState::Diverged { expected, actual } => json!({
            "status": "diverged",
            "fresh": false,
            "expected": expected,
            "actual": actual,
        }),
        BaseCommitState::NoExpectedBase => json!({"status": "no_expected_base", "fresh": null}),
        BaseCommitState::NotAGitRepo => json!({"status": "not_git_repo", "fresh": null}),
    }
}

fn run_stale_base_preflight(flag_value: Option<&str>) {
    let Ok(cwd) = env::current_dir() else {
        return;
    };
    let state = stale_base_state_for(&cwd, flag_value);
    if let Some(warning) = format_stale_base_warning(&state) {
        eprintln!("{warning}");
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_repl(
    model: String,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    base_commit: Option<String>,
    reasoning_effort: Option<String>,
    allow_broad_cwd: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    enforce_broad_cwd_policy(allow_broad_cwd, CliOutputFormat::Text)?;
    run_stale_base_preflight(base_commit.as_deref());
    let resolved_model = resolve_repl_model(model)?;
    let mut cli = LiveCli::new(resolved_model, true, allowed_tools, permission_mode)?;
    cli.set_reasoning_effort(reasoning_effort);
    let mut editor =
        input::LineEditor::new("> ", cli.repl_completion_candidates().unwrap_or_default());
    println!("{}", cli.startup_banner());
    println!("{}", format_connected_line(&cli.model));

    loop {
        editor.set_completions(cli.repl_completion_candidates().unwrap_or_default());
        match editor.read_line()? {
            input::ReadOutcome::Submit(input) => {
                let trimmed = input.trim().to_string();
                if trimmed.is_empty() {
                    continue;
                }
                if matches!(trimmed.as_str(), "/exit" | "/quit") {
                    cli.persist_session()?;
                    break;
                }
                match SlashCommand::parse(&trimmed) {
                    Ok(Some(command)) => {
                        if cli.handle_repl_command(command)? {
                            cli.persist_session()?;
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("{error}");
                        continue;
                    }
                }
                // Bare-word skill dispatch: if the first token of the input
                // matches a known skill name, invoke it as `/skills <input>`
                // rather than forwarding raw text to the LLM (ROADMAP #36).
                let cwd = std::env::current_dir().unwrap_or_default();
                if let Some(prompt) = try_resolve_bare_skill_prompt(&cwd, &trimmed) {
                    editor.push_history(input);
                    cli.record_prompt_history(&trimmed);
                    cli.run_turn(&prompt)?;
                    continue;
                }
                editor.push_history(input);
                cli.record_prompt_history(&trimmed);
                cli.run_turn(&trimmed)?;
            }
            input::ReadOutcome::Cancel => {}
            input::ReadOutcome::Exit => {
                cli.persist_session()?;
                break;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct SessionHandle {
    id: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct ManagedSessionSummary {
    id: String,
    path: PathBuf,
    created_at_ms: u64,
    updated_at_ms: u64,
    modified_epoch_millis: u128,
    message_count: usize,
    parent_session_id: Option<String>,
    branch_name: Option<String>,
    lifecycle: SessionLifecycleSummary,
}

struct LiveCli {
    model: String,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    system_prompt: Vec<String>,
    runtime: BuiltRuntime,
    session: SessionHandle,
    prompt_history: Vec<PromptHistoryEntry>,
}

#[derive(Debug, Clone)]
struct PromptHistoryEntry {
    timestamp_ms: u64,
    text: String,
}

struct RuntimePluginState {
    feature_config: runtime::RuntimeFeatureConfig,
    tool_registry: GlobalToolRegistry,
    plugin_registry: PluginRegistry,
    mcp_state: Option<Arc<Mutex<RuntimeMcpState>>>,
}

struct RuntimeMcpState {
    runtime: tokio::runtime::Runtime,
    manager: McpServerManager,
    pending_servers: Vec<String>,
    degraded_report: Option<runtime::McpDegradedReport>,
}

struct BuiltRuntime {
    runtime: Option<ConversationRuntime<AnthropicRuntimeClient, CliToolExecutor>>,
    plugin_registry: PluginRegistry,
    plugins_active: bool,
    mcp_state: Option<Arc<Mutex<RuntimeMcpState>>>,
    mcp_active: bool,
}

impl BuiltRuntime {
    fn new(
        runtime: ConversationRuntime<AnthropicRuntimeClient, CliToolExecutor>,
        plugin_registry: PluginRegistry,
        mcp_state: Option<Arc<Mutex<RuntimeMcpState>>>,
    ) -> Self {
        Self {
            runtime: Some(runtime),
            plugin_registry,
            plugins_active: true,
            mcp_state,
            mcp_active: true,
        }
    }

    fn with_hook_abort_signal(mut self, hook_abort_signal: runtime::HookAbortSignal) -> Self {
        let runtime = self
            .runtime
            .take()
            .expect("runtime should exist before installing hook abort signal");
        self.runtime = Some(runtime.with_hook_abort_signal(hook_abort_signal));
        self
    }

    fn shutdown_plugins(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.plugins_active {
            self.plugin_registry.shutdown()?;
            self.plugins_active = false;
        }
        Ok(())
    }

    fn shutdown_mcp(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.mcp_active {
            if let Some(mcp_state) = &self.mcp_state {
                mcp_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .shutdown()?;
            }
            self.mcp_active = false;
        }
        Ok(())
    }
}

impl Deref for BuiltRuntime {
    type Target = ConversationRuntime<AnthropicRuntimeClient, CliToolExecutor>;

    fn deref(&self) -> &Self::Target {
        self.runtime
            .as_ref()
            .expect("runtime should exist while built runtime is alive")
    }
}

impl DerefMut for BuiltRuntime {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.runtime
            .as_mut()
            .expect("runtime should exist while built runtime is alive")
    }
}

impl Drop for BuiltRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown_mcp();
        let _ = self.shutdown_plugins();
    }
}

#[derive(Debug, Deserialize)]
struct ToolSearchRequest {
    query: String,
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct McpToolRequest {
    #[serde(rename = "qualifiedName")]
    qualified_name: Option<String>,
    tool: Option<String>,
    arguments: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ListMcpResourcesRequest {
    server: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadMcpResourceRequest {
    server: String,
    uri: String,
}

impl RuntimeMcpState {
    fn new(
        runtime_config: &runtime::RuntimeConfig,
    ) -> Result<Option<(Self, runtime::McpToolDiscoveryReport)>, Box<dyn std::error::Error>> {
        let mut manager = McpServerManager::from_runtime_config(runtime_config);
        if manager.server_names().is_empty() && manager.unsupported_servers().is_empty() {
            return Ok(None);
        }

        let runtime = tokio::runtime::Runtime::new()?;
        let discovery = runtime.block_on(manager.discover_tools_best_effort());
        let pending_servers = discovery
            .failed_servers
            .iter()
            .map(|failure| failure.server_name.clone())
            .chain(
                discovery
                    .unsupported_servers
                    .iter()
                    .map(|server| server.server_name.clone()),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let available_tools = discovery
            .tools
            .iter()
            .map(|tool| tool.qualified_name.clone())
            .collect::<Vec<_>>();
        let failed_server_names = pending_servers.iter().cloned().collect::<BTreeSet<_>>();
        let working_servers = manager
            .server_names()
            .into_iter()
            .filter(|server_name| !failed_server_names.contains(server_name))
            .collect::<Vec<_>>();
        let failed_servers =
            discovery
                .failed_servers
                .iter()
                .map(|failure| runtime::McpFailedServer {
                    server_name: failure.server_name.clone(),
                    phase: runtime::McpLifecyclePhase::ToolDiscovery,
                    error: runtime::McpErrorSurface::new(
                        runtime::McpLifecyclePhase::ToolDiscovery,
                        Some(failure.server_name.clone()),
                        failure.error.clone(),
                        std::collections::BTreeMap::from([(
                            "required".to_string(),
                            failure.required.to_string(),
                        )]),
                        true,
                    ),
                })
                .chain(discovery.unsupported_servers.iter().map(|server| {
                    runtime::McpFailedServer {
                        server_name: server.server_name.clone(),
                        phase: runtime::McpLifecyclePhase::ServerRegistration,
                        error: runtime::McpErrorSurface::new(
                            runtime::McpLifecyclePhase::ServerRegistration,
                            Some(server.server_name.clone()),
                            server.reason.clone(),
                            std::collections::BTreeMap::from([
                                (
                                    "transport".to_string(),
                                    format!("{:?}", server.transport).to_ascii_lowercase(),
                                ),
                                ("required".to_string(), server.required.to_string()),
                            ]),
                            false,
                        ),
                    }
                }))
                .collect::<Vec<_>>();
        let degraded_report = (!failed_servers.is_empty()).then(|| {
            runtime::McpDegradedReport::new(
                working_servers,
                failed_servers,
                available_tools.clone(),
                available_tools,
            )
        });

        Ok(Some((
            Self {
                runtime,
                manager,
                pending_servers,
                degraded_report,
            },
            discovery,
        )))
    }

    fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.runtime.block_on(self.manager.shutdown())?;
        Ok(())
    }

    fn pending_servers(&self) -> Option<Vec<String>> {
        (!self.pending_servers.is_empty()).then(|| self.pending_servers.clone())
    }

    fn degraded_report(&self) -> Option<runtime::McpDegradedReport> {
        self.degraded_report.clone()
    }

    fn server_names(&self) -> Vec<String> {
        self.manager.server_names()
    }

    fn call_tool(
        &mut self,
        qualified_tool_name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<String, ToolError> {
        let response = self
            .runtime
            .block_on(self.manager.call_tool(qualified_tool_name, arguments))
            .map_err(|error| ToolError::new(error.to_string()))?;
        if let Some(error) = response.error {
            return Err(ToolError::new(format!(
                "MCP tool `{qualified_tool_name}` returned JSON-RPC error: {} ({})",
                error.message, error.code
            )));
        }

        let result = response.result.ok_or_else(|| {
            ToolError::new(format!(
                "MCP tool `{qualified_tool_name}` returned no result payload"
            ))
        })?;
        serde_json::to_string_pretty(&result).map_err(|error| ToolError::new(error.to_string()))
    }

    fn list_resources_for_server(&mut self, server_name: &str) -> Result<String, ToolError> {
        let result = self
            .runtime
            .block_on(self.manager.list_resources(server_name))
            .map_err(|error| ToolError::new(error.to_string()))?;
        serde_json::to_string_pretty(&json!({
            "server": server_name,
            "resources": result.resources,
        }))
        .map_err(|error| ToolError::new(error.to_string()))
    }

    fn list_resources_for_all_servers(&mut self) -> Result<String, ToolError> {
        let mut resources = Vec::new();
        let mut failures = Vec::new();

        for server_name in self.server_names() {
            match self
                .runtime
                .block_on(self.manager.list_resources(&server_name))
            {
                Ok(result) => resources.push(json!({
                    "server": server_name,
                    "resources": result.resources,
                })),
                Err(error) => failures.push(json!({
                    "server": server_name,
                    "error": error.to_string(),
                })),
            }
        }

        if resources.is_empty() && !failures.is_empty() {
            let message = failures
                .iter()
                .filter_map(|failure| failure.get("error").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ToolError::new(message));
        }

        serde_json::to_string_pretty(&json!({
            "resources": resources,
            "failures": failures,
        }))
        .map_err(|error| ToolError::new(error.to_string()))
    }

    fn read_resource(&mut self, server_name: &str, uri: &str) -> Result<String, ToolError> {
        let result = self
            .runtime
            .block_on(self.manager.read_resource(server_name, uri))
            .map_err(|error| ToolError::new(error.to_string()))?;
        serde_json::to_string_pretty(&json!({
            "server": server_name,
            "contents": result.contents,
        }))
        .map_err(|error| ToolError::new(error.to_string()))
    }
}

fn build_runtime_mcp_state(
    runtime_config: &runtime::RuntimeConfig,
) -> Result<RuntimePluginStateBuildOutput, Box<dyn std::error::Error>> {
    let Some((mcp_state, discovery)) = RuntimeMcpState::new(runtime_config)? else {
        return Ok((None, Vec::new()));
    };

    let mut runtime_tools = discovery
        .tools
        .iter()
        .map(mcp_runtime_tool_definition)
        .collect::<Vec<_>>();
    if !mcp_state.server_names().is_empty() {
        runtime_tools.extend(mcp_wrapper_tool_definitions());
    }

    Ok((Some(Arc::new(Mutex::new(mcp_state))), runtime_tools))
}

fn mcp_runtime_tool_definition(tool: &runtime::ManagedMcpTool) -> RuntimeToolDefinition {
    RuntimeToolDefinition {
        name: tool.qualified_name.clone(),
        description: Some(
            tool.tool
                .description
                .clone()
                .unwrap_or_else(|| format!("Invoke MCP tool `{}`.", tool.qualified_name)),
        ),
        input_schema: tool
            .tool
            .input_schema
            .clone()
            .unwrap_or_else(|| json!({ "type": "object", "additionalProperties": true })),
        required_permission: permission_mode_for_mcp_tool(&tool.tool),
    }
}

fn mcp_wrapper_tool_definitions() -> Vec<RuntimeToolDefinition> {
    vec![
        RuntimeToolDefinition {
            name: "MCPTool".to_string(),
            description: Some(
                "Call a configured MCP tool by its qualified name and JSON arguments.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "qualifiedName": { "type": "string" },
                    "arguments": {}
                },
                "required": ["qualifiedName"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        RuntimeToolDefinition {
            name: "ListMcpResourcesTool".to_string(),
            description: Some(
                "List MCP resources from one configured server or from every connected server."
                    .to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" }
                },
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        RuntimeToolDefinition {
            name: "ReadMcpResourceTool".to_string(),
            description: Some("Read a specific MCP resource from a configured server.".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" },
                    "uri": { "type": "string" }
                },
                "required": ["server", "uri"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
    ]
}

fn permission_mode_for_mcp_tool(tool: &McpTool) -> PermissionMode {
    let read_only = mcp_annotation_flag(tool, "readOnlyHint");
    let destructive = mcp_annotation_flag(tool, "destructiveHint");
    let open_world = mcp_annotation_flag(tool, "openWorldHint");

    if read_only && !destructive && !open_world {
        PermissionMode::ReadOnly
    } else if destructive || open_world {
        PermissionMode::DangerFullAccess
    } else {
        PermissionMode::WorkspaceWrite
    }
}

fn mcp_annotation_flag(tool: &McpTool, key: &str) -> bool {
    tool.annotations
        .as_ref()
        .and_then(|annotations| annotations.get(key))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

struct HookAbortMonitor {
    stop_tx: Option<Sender<()>>,
    join_handle: Option<JoinHandle<()>>,
}

impl HookAbortMonitor {
    fn spawn(abort_signal: runtime::HookAbortSignal) -> Self {
        Self::spawn_with_waiter(abort_signal, move |stop_rx, abort_signal| {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };

            runtime.block_on(async move {
                let wait_for_stop = tokio::task::spawn_blocking(move || {
                    let _ = stop_rx.recv();
                });

                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        if result.is_ok() {
                            abort_signal.abort();
                        }
                    }
                    _ = wait_for_stop => {}
                }
            });
        })
    }

    fn spawn_with_waiter<F>(abort_signal: runtime::HookAbortSignal, wait_for_interrupt: F) -> Self
    where
        F: FnOnce(Receiver<()>, runtime::HookAbortSignal) + Send + 'static,
    {
        let (stop_tx, stop_rx) = mpsc::channel();
        let join_handle = thread::spawn(move || wait_for_interrupt(stop_rx, abort_signal));

        Self {
            stop_tx: Some(stop_tx),
            join_handle: Some(join_handle),
        }
    }

    fn stop(mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

impl LiveCli {
    fn new(
        model: String,
        enable_tools: bool,
        allowed_tools: Option<AllowedToolSet>,
        permission_mode: PermissionMode,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let system_prompt = build_system_prompt(&model)?;
        let session_state = new_cli_session()?;
        let session = create_managed_session_handle(&session_state.session_id)?;
        let runtime = build_runtime(
            session_state.with_persistence_path(session.path.clone()),
            &session.id,
            model.clone(),
            system_prompt.clone(),
            enable_tools,
            true,
            allowed_tools.clone(),
            permission_mode,
            None,
        )?;
        let cli = Self {
            model,
            allowed_tools,
            permission_mode,
            system_prompt,
            runtime,
            session,
            prompt_history: Vec::new(),
        };
        cli.persist_session()?;
        Ok(cli)
    }

    fn set_reasoning_effort(&mut self, effort: Option<String>) {
        if let Some(rt) = self.runtime.runtime.as_mut() {
            rt.api_client_mut().set_reasoning_effort(effort);
        }
    }

    fn startup_banner(&self) -> String {
        let cwd = env::current_dir().map_or_else(
            |_| "<unknown>".to_string(),
            |path| path.display().to_string(),
        );
        let status = status_context(None).ok();
        let git_branch = status
            .as_ref()
            .and_then(|context| context.git_branch.as_deref())
            .unwrap_or("unknown");
        let workspace = status.as_ref().map_or_else(
            || "unknown".to_string(),
            |context| context.git_summary.headline(),
        );
        let session_path = self.session.path.strip_prefix(Path::new(&cwd)).map_or_else(
            |_| self.session.path.display().to_string(),
            |path| path.display().to_string(),
        );
        format!(
            "\x1b[38;5;196m\
 ██████╗██╗      █████╗ ██╗    ██╗\n\
██╔════╝██║     ██╔══██╗██║    ██║\n\
██║     ██║     ███████║██║ █╗ ██║\n\
██║     ██║     ██╔══██║██║███╗██║\n\
╚██████╗███████╗██║  ██║╚███╔███╔╝\n\
 ╚═════╝╚══════╝╚═╝  ╚═╝ ╚══╝╚══╝\x1b[0m \x1b[38;5;208mCode\x1b[0m 🦞\n\n\
  \x1b[2mModel\x1b[0m            {}\n\
  \x1b[2mPermissions\x1b[0m      {}\n\
  \x1b[2mBranch\x1b[0m           {}\n\
  \x1b[2mWorkspace\x1b[0m        {}\n\
  \x1b[2mDirectory\x1b[0m        {}\n\
  \x1b[2mSession\x1b[0m          {}\n\
  \x1b[2mAuto-save\x1b[0m        {}\n\n\
  Type \x1b[1m/help\x1b[0m for commands · \x1b[1m/status\x1b[0m for live context · \x1b[2m/resume latest\x1b[0m jumps back to the newest session · \x1b[1m/diff\x1b[0m then \x1b[1m/commit\x1b[0m to ship · \x1b[2mTab\x1b[0m for workflow completions · \x1b[2mShift+Enter\x1b[0m for newline",
            self.model,
            self.permission_mode.as_str(),
            git_branch,
            workspace,
            cwd,
            self.session.id,
            session_path,
        )
    }

    fn repl_completion_candidates(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        Ok(slash_command_completion_candidates_with_sessions(
            &self.model,
            Some(&self.session.id),
            list_managed_sessions()?
                .into_iter()
                .map(|session| session.id)
                .collect(),
        ))
    }

    fn prepare_turn_runtime(
        &self,
        emit_output: bool,
    ) -> Result<(BuiltRuntime, HookAbortMonitor), Box<dyn std::error::Error>> {
        let hook_abort_signal = runtime::HookAbortSignal::new();
        let runtime = build_runtime(
            self.runtime.session().clone(),
            &self.session.id,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            emit_output,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?
        .with_hook_abort_signal(hook_abort_signal.clone());
        let hook_abort_monitor = HookAbortMonitor::spawn(hook_abort_signal);

        Ok((runtime, hook_abort_monitor))
    }

    fn replace_runtime(&mut self, runtime: BuiltRuntime) -> Result<(), Box<dyn std::error::Error>> {
        self.runtime.shutdown_plugins()?;
        self.runtime = runtime;
        Ok(())
    }

    fn run_turn(&mut self, input: &str) -> Result<(), Box<dyn std::error::Error>> {
        let (mut runtime, hook_abort_monitor) = self.prepare_turn_runtime(true)?;
        let mut spinner = Spinner::new();
        let mut stdout = io::stdout();
        spinner.tick(
            "🦀 Thinking...",
            TerminalRenderer::new().color_theme(),
            &mut stdout,
        )?;
        let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
        let result = runtime.run_turn(input, Some(&mut permission_prompter));
        hook_abort_monitor.stop();
        match result {
            Ok(summary) => {
                self.replace_runtime(runtime)?;
                spinner.finish(
                    "✨ Done",
                    TerminalRenderer::new().color_theme(),
                    &mut stdout,
                )?;
                let final_text = final_assistant_text(&summary);
                if !final_text.is_empty() {
                    println!("{final_text}");
                }
                println!();
                if let Some(event) = summary.auto_compaction {
                    println!(
                        "{}",
                        format_auto_compaction_notice(event.removed_message_count)
                    );
                }
                self.persist_session()?;
                Ok(())
            }
            Err(error) => {
                runtime.shutdown_plugins()?;
                spinner.fail(
                    "❌ Request failed",
                    TerminalRenderer::new().color_theme(),
                    &mut stdout,
                )?;

                // ============================================================================
                // Auto-compact retry on context window errors
                // ============================================================================
                // When the model API returns a context_window_blocked error (because the request
                // exceeds the model's context window), we automatically:
                // 1. Compact the session (remove old messages to free up space)
                // 2. Retry the original request with the compacted session
                // 3. Report results to the user
                //
                // This eliminates the need for users to manually run /compact when they
                // hit context limits - the recovery happens automatically.
                //
                // Detection: We look for "context_window" or "Context window" in the error
                // message, which covers error types like:
                // - "context_window_blocked"
                // - "Context window blocked"
                // - "This model's maximum context length is X tokens..."
                // ============================================================================

                let error_str = error.to_string();
                // Detect context window overflow. Some providers (e.g. OpenAI-compat backends)
                // return 400 with "no parseable body" instead of a proper context_length_exceeded
                // error when the request is too large to even parse — treat that as context overflow too.
                // Also detect model-specific context error markers (e.g. llama.cpp returns
                // "Context size has been exceeded." / "exceed_context_size_error" / "exceeds the available context size").
                let is_context_window = error_str.contains("context_window")
                    || error_str.contains("Context window")
                    || error_str.contains("no parseable body")
                    || error_str.contains("exceed_context_size")
                    || error_str.contains("exceeds the available context size")
                    || error_str
                        .to_ascii_lowercase()
                        .contains("context size has been exceeded");

                // Also treat "assistant stream produced no content" and reqwest decode failures
                // as recoverable errors that may benefit from auto-compaction. Some backends (e.g.
                // llama.cpp) return a non-SSE HTTP 500 body when context overflows, causing
                // reqwest to fail with "error decoding response body" — treat that as context overflow too.
                let is_no_content = error_str.contains("assistant stream produced no content")
                    || error_str.contains("Failed to parse input at pos")
                    || error_str.contains("error decoding response body");

                if is_context_window || is_no_content {
                    // If the error tells us the server's actual context window, adapt our
                    // auto-compaction threshold so future auto-compact-trigger checks are accurate.
                    if let Some(window) = extract_context_window_tokens_from_error(&error_str) {
                        // Set threshold at 70% of the reported window to leave headroom.
                        let threshold: u32 = (window as f64 * 0.7).round() as u32;
                        println!(
                            "  Server context window: {} tokens — setting auto-compaction threshold to {}",
                            window, threshold
                        );
                        runtime.set_auto_compaction_input_tokens_threshold(threshold);
                    }

                    // A single compaction pass may not free enough context space.
                    // Progressive retry: each round preserves fewer recent messages (4→2→1→0),
                    // trading conversation continuity for a smaller payload until it fits.
                    // Max 4 rounds before giving up and surfacing the error to the user.
                    let max_compact_rounds = 4;
                    let preserve_schedule = [4, 2, 1, 0];

                    for round in 0..max_compact_rounds {
                        let preserve = preserve_schedule[round];
                        println!(
                            "  Auto-compacting session (round {}/{}, preserving {} recent messages)...",
                            round + 1,
                            max_compact_rounds,
                            preserve
                        );

                        // Run Trident pipeline then summary-based compaction
                        let result = runtime::trident::trident_compact_session(
                            runtime.session(),
                            CompactionConfig {
                                preserve_recent_messages: preserve,
                                max_estimated_tokens: 0,
                            },
                            &runtime::trident::TridentConfig::default(),
                        );
                        let removed = result.removed_message_count;

                        if removed == 0 && round > 0 {
                            // No more messages to compact — further rounds won't help
                            println!("  No further compaction possible.");
                            break;
                        }

                        if removed > 0 {
                            println!(
                                "{}",
                                format_compact_report(
                                    removed,
                                    result.compacted_session.messages.len(),
                                    false
                                )
                            );
                        }

                        // Without this, prepare_turn_runtime() reads from self.runtime.session()
                        // which still holds the ORIGINAL un-compacted session, so every retry round
                        // would send the same bloated request — compaction was wasted.
                        *self.runtime.session_mut() = result.compacted_session.clone();

                        // Build a new runtime with the compacted session and retry
                        let (mut new_runtime, hook_abort_monitor) =
                            self.prepare_turn_runtime(true)?;
                        drop(hook_abort_monitor);

                        let mut rp = CliPermissionPrompter::new(self.permission_mode);
                        match new_runtime.run_turn(input, Some(&mut rp)) {
                            Ok(summary) => {
                                self.replace_runtime(new_runtime)?;
                                spinner.finish(
                                    if round == 0 {
                                        "✨ Done (after auto-compact)"
                                    } else {
                                        "✨ Done (after aggressive auto-compact)"
                                    },
                                    TerminalRenderer::new().color_theme(),
                                    &mut stdout,
                                )?;
                                println!();
                                if let Some(event) = summary.auto_compaction {
                                    println!(
                                        "{}",
                                        format_auto_compaction_notice(event.removed_message_count)
                                    );
                                }
                                self.persist_session()?;
                                return Ok(());
                            }
                            Err(retry_error) => {
                                let retry_str = retry_error.to_string();
                                let still_context_window = retry_str.contains("context_window")
                                    || retry_str.contains("Context window")
                                    || retry_str.contains("no parseable body")
                                    || retry_str.contains("exceed_context_size")
                                    || retry_str.contains("exceeds the available context size")
                                    || retry_str
                                        .to_ascii_lowercase()
                                        .contains("context size has been exceeded");
                                let still_no_content = retry_str
                                    .contains("assistant stream produced no content")
                                    || retry_str.contains("Failed to parse input at pos")
                                    || retry_str.contains("error decoding response body");

                                if (still_context_window || still_no_content)
                                    && round + 1 < max_compact_rounds
                                {
                                    // If the retry error reveals the context window, adapt threshold.
                                    if let Some(window) =
                                        extract_context_window_tokens_from_error(&retry_str)
                                    {
                                        let threshold: u32 = (window as f64 * 0.7).round() as u32;
                                        new_runtime
                                            .set_auto_compaction_input_tokens_threshold(threshold);
                                    }

                                    // The compacted session was still too large for the model's context.
                                    // Shut down the old runtime, adopt the partially-compacted one,
                                    // and loop — the next round will compact more aggressively.
                                    runtime.shutdown_plugins()?;
                                    runtime = new_runtime;
                                    continue;
                                }

                                // Not a context window error, or out of rounds
                                return Err(Box::new(retry_error));
                            }
                        }
                    }
                }

                // If not a context window error, return original error
                Err(Box::new(error))
            }
        }
    }

    fn run_turn_with_output(
        &mut self,
        input: &str,
        output_format: CliOutputFormat,
        compact: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match output_format {
            CliOutputFormat::Json if compact => self.run_prompt_compact_json(input),
            CliOutputFormat::Text if compact => self.run_prompt_compact(input),
            CliOutputFormat::Text => self.run_turn(input),
            CliOutputFormat::Json => self.run_prompt_json(input),
        }
    }

    fn run_prompt_compact(&mut self, input: &str) -> Result<(), Box<dyn std::error::Error>> {
        let (mut runtime, hook_abort_monitor) = self.prepare_turn_runtime(false)?;
        let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
        let result = runtime.run_turn(input, Some(&mut permission_prompter));
        hook_abort_monitor.stop();
        let summary = result?;
        self.replace_runtime(runtime)?;
        self.persist_session()?;
        let final_text = final_assistant_text(&summary);
        println!("{final_text}");
        Ok(())
    }

    fn run_prompt_compact_json(&mut self, input: &str) -> Result<(), Box<dyn std::error::Error>> {
        let (mut runtime, hook_abort_monitor) = self.prepare_turn_runtime(false)?;
        let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
        let result = runtime.run_turn(input, Some(&mut permission_prompter));
        hook_abort_monitor.stop();
        let summary = result?;
        self.replace_runtime(runtime)?;
        self.persist_session()?;
        println!(
            "{}",
            json!({
                "message": final_assistant_text(&summary),
                "compact": true,
                "model": self.model,
                "usage": {
                    "input_tokens": summary.usage.input_tokens,
                    "output_tokens": summary.usage.output_tokens,
                    "cache_creation_input_tokens": summary.usage.cache_creation_input_tokens,
                    "cache_read_input_tokens": summary.usage.cache_read_input_tokens,
                },
            })
        );
        Ok(())
    }

    fn run_prompt_json(&mut self, input: &str) -> Result<(), Box<dyn std::error::Error>> {
        let (mut runtime, hook_abort_monitor) = self.prepare_turn_runtime(false)?;
        let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
        let result = runtime.run_turn(input, Some(&mut permission_prompter));
        hook_abort_monitor.stop();
        let summary = result?;
        self.replace_runtime(runtime)?;
        self.persist_session()?;
        println!(
            "{}",
            json!({
                "message": final_assistant_text(&summary),
                "model": self.model,
                "iterations": summary.iterations,
                "auto_compaction": summary.auto_compaction.map(|event| json!({
                    "removed_messages": event.removed_message_count,
                    "notice": format_auto_compaction_notice(event.removed_message_count),
                })),
                "tool_uses": collect_tool_uses(&summary),
                "tool_results": collect_tool_results(&summary),
                "prompt_cache_events": collect_prompt_cache_events(&summary),
                "usage": {
                    "input_tokens": summary.usage.input_tokens,
                    "output_tokens": summary.usage.output_tokens,
                    "cache_creation_input_tokens": summary.usage.cache_creation_input_tokens,
                    "cache_read_input_tokens": summary.usage.cache_read_input_tokens,
                },
                "estimated_cost": format_usd(
                    summary.usage.estimate_cost_usd_with_pricing(
                        pricing_for_model(&self.model)
                            .unwrap_or_else(runtime::ModelPricing::default_sonnet_tier)
                    ).total_cost_usd()
                )
            })
        );
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn handle_repl_command(
        &mut self,
        command: SlashCommand,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(match command {
            SlashCommand::Help => {
                println!("{}", render_repl_help());
                false
            }
            SlashCommand::Status => {
                self.print_status();
                false
            }
            SlashCommand::Bughunter { scope } => {
                self.run_bughunter(scope.as_deref())?;
                false
            }
            SlashCommand::Commit => {
                self.run_commit(None)?;
                false
            }
            SlashCommand::Pr { context } => {
                self.run_pr(context.as_deref())?;
                false
            }
            SlashCommand::Issue { context } => {
                self.run_issue(context.as_deref())?;
                false
            }
            SlashCommand::Ultraplan { task } => {
                self.run_ultraplan(task.as_deref())?;
                false
            }
            SlashCommand::Teleport { target } => {
                Self::run_teleport(target.as_deref())?;
                false
            }
            SlashCommand::DebugToolCall => {
                self.run_debug_tool_call(None)?;
                false
            }
            SlashCommand::Sandbox => {
                Self::print_sandbox_status();
                false
            }
            SlashCommand::Compact => {
                self.compact()?;
                false
            }
            SlashCommand::Model { model } => self.set_model(model)?,
            SlashCommand::Permissions { mode } => self.set_permissions(mode)?,
            SlashCommand::Clear { confirm } => self.clear_session(confirm)?,
            SlashCommand::Cost => {
                self.print_cost();
                false
            }
            SlashCommand::Resume { session_path } => self.resume_session(session_path)?,
            SlashCommand::Config { section } => {
                Self::print_config(section.as_deref())?;
                false
            }
            SlashCommand::Mcp { action, target } => {
                let args = match (action.as_deref(), target.as_deref()) {
                    (None, None) => None,
                    (Some(action), None) => Some(action.to_string()),
                    (Some(action), Some(target)) => Some(format!("{action} {target}")),
                    (None, Some(target)) => Some(target.to_string()),
                };
                Self::print_mcp(args.as_deref(), CliOutputFormat::Text)?;
                false
            }
            SlashCommand::Memory => {
                Self::print_memory()?;
                false
            }
            SlashCommand::Init => {
                run_init(CliOutputFormat::Text)?;
                false
            }
            SlashCommand::Diff => {
                Self::print_diff()?;
                false
            }
            SlashCommand::Version => {
                Self::print_version(CliOutputFormat::Text);
                false
            }
            SlashCommand::Export { path } => {
                self.export_session(path.as_deref())?;
                false
            }
            SlashCommand::Session { action, target } => {
                self.handle_session_command(action.as_deref(), target.as_deref())?
            }
            SlashCommand::Plugins { action, target } => {
                self.handle_plugins_command(action.as_deref(), target.as_deref())?
            }
            SlashCommand::Agents { args } => {
                if let Err(error) = Self::print_agents(args.as_deref(), CliOutputFormat::Text) {
                    eprintln!("{error}");
                }
                false
            }
            SlashCommand::Skills { args } => {
                match classify_skills_slash_command(args.as_deref()) {
                    SkillSlashDispatch::Invoke(prompt) => self.run_turn(&prompt)?,
                    SkillSlashDispatch::Local => {
                        if let Err(error) =
                            Self::print_skills(args.as_deref(), CliOutputFormat::Text)
                        {
                            eprintln!("{error}");
                        }
                    }
                }
                false
            }
            SlashCommand::Doctor => {
                println!(
                    "{}",
                    render_doctor_report(
                        ConfigWarningMode::EmitStderr,
                        permission_mode_provenance_for_current_dir(),
                    )?
                    .render()
                );
                false
            }
            SlashCommand::Setup => {
                if let Err(e) = setup_wizard::run_setup_wizard() {
                    eprintln!("Setup wizard failed: {e}");
                }
                false
            }
            SlashCommand::History { count } => {
                self.print_prompt_history(count.as_deref());
                false
            }
            SlashCommand::Stats => {
                let usage = UsageTracker::from_session(self.runtime.session()).cumulative_usage();
                println!("{}", format_cost_report(usage));
                false
            }
            SlashCommand::Login
            | SlashCommand::Logout
            | SlashCommand::Vim
            | SlashCommand::Upgrade
            | SlashCommand::Share
            | SlashCommand::Feedback
            | SlashCommand::Files
            | SlashCommand::Fast
            | SlashCommand::Exit
            | SlashCommand::Summary
            | SlashCommand::Desktop
            | SlashCommand::Brief
            | SlashCommand::Advisor
            | SlashCommand::Stickers
            | SlashCommand::Insights
            | SlashCommand::Thinkback
            | SlashCommand::ReleaseNotes
            | SlashCommand::SecurityReview
            | SlashCommand::Keybindings
            | SlashCommand::PrivacySettings
            | SlashCommand::Plan { .. }
            | SlashCommand::Review { .. }
            | SlashCommand::Tasks { .. }
            | SlashCommand::Theme { .. }
            | SlashCommand::Voice { .. }
            | SlashCommand::Usage { .. }
            | SlashCommand::Rename { .. }
            | SlashCommand::Copy { .. }
            | SlashCommand::Hooks { .. }
            | SlashCommand::Context { .. }
            | SlashCommand::Color { .. }
            | SlashCommand::Effort { .. }
            | SlashCommand::Branch { .. }
            | SlashCommand::Rewind { .. }
            | SlashCommand::Ide { .. }
            | SlashCommand::Tag { .. }
            | SlashCommand::OutputStyle { .. }
            | SlashCommand::AddDir { .. }
            | SlashCommand::Team { .. } => {
                let cmd_name = command.slash_name();
                eprintln!("{cmd_name} is not yet implemented in this build.");
                false
            }
            SlashCommand::Unknown(name) => {
                eprintln!("{}", format_unknown_slash_command(&name));
                false
            }
        })
    }

    fn persist_session(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.runtime.session().save_to_path(&self.session.path)?;
        Ok(())
    }

    fn print_status(&self) {
        let cumulative = self.runtime.usage().cumulative_usage();
        let latest = self.runtime.usage().current_turn_usage();
        println!(
            "{}",
            format_status_report(
                &self.model,
                StatusUsage {
                    message_count: self.runtime.session().messages.len(),
                    turns: self.runtime.usage().turns(),
                    latest,
                    cumulative,
                    estimated_tokens: self.runtime.estimated_tokens(),
                },
                self.permission_mode.as_str(),
                &status_context(Some(&self.session.path)).expect("status context should load"),
                None, // #148: REPL /status doesn't carry flag provenance
                None,
            )
        );
    }

    fn record_prompt_history(&mut self, prompt: &str) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map_or(self.runtime.session().updated_at_ms, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            });
        let entry = PromptHistoryEntry {
            timestamp_ms,
            text: prompt.to_string(),
        };
        self.prompt_history.push(entry);
        if let Err(error) = self.runtime.session_mut().push_prompt_entry(prompt) {
            eprintln!("warning: failed to persist prompt history: {error}");
        }
    }

    fn print_prompt_history(&self, count: Option<&str>) {
        let limit = match parse_history_count(count) {
            Ok(limit) => limit,
            Err(message) => {
                eprintln!("{message}");
                return;
            }
        };
        let session_entries = &self.runtime.session().prompt_history;
        let entries = if session_entries.is_empty() {
            if self.prompt_history.is_empty() {
                collect_session_prompt_history(self.runtime.session())
            } else {
                self.prompt_history
                    .iter()
                    .map(|entry| PromptHistoryEntry {
                        timestamp_ms: entry.timestamp_ms,
                        text: entry.text.clone(),
                    })
                    .collect()
            }
        } else {
            session_entries
                .iter()
                .map(|entry| PromptHistoryEntry {
                    timestamp_ms: entry.timestamp_ms,
                    text: entry.text.clone(),
                })
                .collect()
        };
        println!("{}", render_prompt_history_report(&entries, limit));
    }

    fn print_sandbox_status() {
        let cwd = env::current_dir().expect("current dir");
        let loader = ConfigLoader::default_for(&cwd);
        let runtime_config = loader
            .load()
            .unwrap_or_else(|_| runtime::RuntimeConfig::empty());
        println!(
            "{}",
            format_sandbox_report(&resolve_sandbox_status(runtime_config.sandbox(), &cwd))
        );
    }

    fn set_model(&mut self, model: Option<String>) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(model) = model else {
            println!(
                "{}",
                format_model_report(
                    &self.model,
                    self.runtime.session().messages.len(),
                    self.runtime.usage().turns(),
                )
            );
            return Ok(false);
        };

        let model = resolve_model_alias_with_config(&model);

        if model == self.model {
            println!(
                "{}",
                format_model_report(
                    &self.model,
                    self.runtime.session().messages.len(),
                    self.runtime.usage().turns(),
                )
            );
            return Ok(false);
        }

        let previous = self.model.clone();
        let session = self.runtime.session().clone();
        let message_count = session.messages.len();
        let runtime = build_runtime(
            session,
            &self.session.id,
            model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        self.replace_runtime(runtime)?;
        self.model.clone_from(&model);
        println!(
            "{}",
            format_model_switch_report(&previous, &model, message_count)
        );
        Ok(true)
    }

    fn set_permissions(
        &mut self,
        mode: Option<String>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(mode) = mode else {
            println!(
                "{}",
                format_permissions_report(self.permission_mode.as_str())
            );
            return Ok(false);
        };

        let normalized = normalize_permission_mode(&mode).ok_or_else(|| {
            format!(
                "invalid_flag_value: unsupported permission mode '{mode}'.\nUsage: --permission-mode read-only|workspace-write|danger-full-access"
            )
        })?;

        if normalized == self.permission_mode.as_str() {
            println!("{}", format_permissions_report(normalized));
            return Ok(false);
        }

        let previous = self.permission_mode.as_str().to_string();
        let session = self.runtime.session().clone();
        self.permission_mode = permission_mode_from_label(normalized);
        let runtime = build_runtime(
            session,
            &self.session.id,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        self.replace_runtime(runtime)?;
        println!(
            "{}",
            format_permissions_switch_report(&previous, normalized)
        );
        Ok(true)
    }

    fn clear_session(&mut self, confirm: bool) -> Result<bool, Box<dyn std::error::Error>> {
        if !confirm {
            println!(
                "clear: confirmation required; run /clear --confirm to start a fresh session."
            );
            return Ok(false);
        }

        let previous_session = self.session.clone();
        let session_state = new_cli_session()?;
        self.session = create_managed_session_handle(&session_state.session_id)?;
        let runtime = build_runtime(
            session_state.with_persistence_path(self.session.path.clone()),
            &self.session.id,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        self.replace_runtime(runtime)?;
        println!(
            "Session cleared\n  Mode             fresh session\n  Previous session {}\n  Resume previous  /resume {}\n  Preserved model  {}\n  Permission mode  {}\n  New session      {}\n  Session file     {}",
            previous_session.id,
            previous_session.id,
            self.model,
            self.permission_mode.as_str(),
            self.session.id,
            self.session.path.display(),
        );
        Ok(true)
    }

    fn print_cost(&self) {
        let cumulative = self.runtime.usage().cumulative_usage();
        println!("{}", format_cost_report(cumulative));
    }

    fn resume_session(
        &mut self,
        session_path: Option<String>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(session_ref) = session_path else {
            println!("{}", render_resume_usage());
            return Ok(false);
        };

        let (handle, session) =
            load_session_reference_excluding(&session_ref, Some(&self.session.id))?;
        let message_count = session.messages.len();
        let session_id = session.session_id.clone();
        let runtime = build_runtime(
            session,
            &handle.id,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        self.replace_runtime(runtime)?;
        self.session = SessionHandle {
            id: session_id,
            path: handle.path,
        };
        println!(
            "{}",
            format_resume_report(
                &self.session.path.display().to_string(),
                message_count,
                self.runtime.usage().turns(),
            )
        );
        Ok(true)
    }

    fn print_config(section: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_config_report(section)?);
        Ok(())
    }

    fn print_memory() -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_memory_report()?);
        Ok(())
    }

    fn print_agents(
        args: Option<&str>,
        output_format: CliOutputFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        match output_format {
            CliOutputFormat::Text => println!("{}", handle_agents_slash_command(args, &cwd)?),
            CliOutputFormat::Json => {
                let value = handle_agents_slash_command_json(args, &cwd)?;
                // #789: parity with print_mcp/#788 print_skills — exit 1 when envelope
                // reports an error so automation can rely on exit code instead of
                // parsing the JSON status field.
                let is_error = value.get("status").and_then(|v| v.as_str()) == Some("error");
                println!("{}", serde_json::to_string_pretty(&value)?);
                if is_error {
                    std::process::exit(1);
                }
            }
        }
        Ok(())
    }

    fn print_mcp(
        args: Option<&str>,
        output_format: CliOutputFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // `claw mcp serve` starts a stdio MCP server exposing claw's built-in
        // tools. All other `mcp` subcommands fall through to the existing
        // configured-server reporter (`list`, `status`, ...).
        if matches!(args.map(str::trim), Some("serve")) {
            return run_mcp_serve();
        }
        let cwd = env::current_dir()?;
        match output_format {
            CliOutputFormat::Text => println!("{}", handle_mcp_slash_command(args, &cwd)?),
            CliOutputFormat::Json => {
                let value = handle_mcp_slash_command_json(args, &cwd)?;
                // Propagate ok:false → non-zero exit so automation callers
                // can rely on exit code instead of inspecting the envelope.
                // (#68: mcp error envelopes previously always exited 0.)
                let is_error = value.get("ok").and_then(serde_json::Value::as_bool) == Some(false)
                    || value.get("status").and_then(serde_json::Value::as_str) == Some("error");
                println!("{}", serde_json::to_string_pretty(&value)?);
                if is_error {
                    std::process::exit(1);
                }
            }
        }
        Ok(())
    }

    fn print_skills(
        args: Option<&str>,
        output_format: CliOutputFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        match output_format {
            CliOutputFormat::Text => println!("{}", handle_skills_slash_command(args, &cwd)?),
            CliOutputFormat::Json => {
                let result = handle_skills_slash_command_json(args, &cwd)?;
                let is_error = result.get("status").and_then(|v| v.as_str()) == Some("error");
                // #739: action:"help" with unexpected set is a usage response, not a fatal error;
                // don't return Err which would emit a second error envelope from the generic path.
                let is_help_action = result.get("action").and_then(|v| v.as_str()) == Some("help");
                println!("{}", serde_json::to_string_pretty(&result)?);
                if is_error && !is_help_action {
                    // #788: the error JSON is already emitted above; returning Err here
                    // would cause the top-level handler to emit a second error envelope.
                    // Exit directly to signal failure without a duplicate envelope.
                    std::process::exit(1);
                }
            }
        }
        Ok(())
    }

    fn print_plugins(
        action: Option<&str>,
        target: Option<&str>,
        output_format: CliOutputFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        // #803: reject flag-shaped tokens in list filter for BOTH text and JSON modes.
        // Previously the guard was JSON-only (#793); text mode silently returned empty success.
        if action.as_deref() == Some("list") {
            if let Some(filter) = target.as_deref() {
                if filter.starts_with('-') {
                    if matches!(output_format, CliOutputFormat::Json) {
                        // ROADMAP #817: this is a handled local inventory parse error.
                        // Keep it on stdout in JSON mode so `plugins list --` matches the
                        // sibling JSON inventory/local surfaces instead of falling through
                        // to the top-level stderr error path.
                        let obj = json!({
                            "type": "error",
                            "kind": "plugin",
                            "action": "list",
                            "status": "error",
                            "error_kind": "cli_parse",
                            "error": format!("unknown option for `claw plugins list`: {filter}"),
                            "message": format!("unknown option for `claw plugins list`: {filter}"),
                            "unexpected": filter,
                            "hint": "Usage: claw plugins list [<filter>]\nFilters are id substrings, not flags.",
                            "exit_code": 1,
                        });
                        println!("{}", serde_json::to_string_pretty(&obj)?);
                        std::process::exit(1);
                    }
                    return Err(format!(
                        "unknown option for `claw plugins list`: {filter}\nUsage: claw plugins list [<filter>]\nFilters are id substrings, not flags."
                    ).into());
                }
            }
        }
        let payload = plugins_command_payload_for(
            &cwd,
            action,
            target,
            match output_format {
                CliOutputFormat::Json => ConfigWarningMode::SuppressStderr,
                CliOutputFormat::Text => ConfigWarningMode::EmitStderr,
            },
        )?;
        match output_format {
            CliOutputFormat::Text => {
                // #806: text-mode show must return error when plugin not found (parity with JSON)
                let action_str = action.unwrap_or("list");
                if matches!(action_str, "show" | "info" | "describe") {
                    if let Some(name) = target {
                        let needle = name.to_lowercase();
                        let found = payload.plugins.iter().any(|p| {
                            p.get("id")
                                .and_then(|v| v.as_str())
                                .map(|id| id.to_lowercase() == needle)
                                .unwrap_or(false)
                        });
                        if !found {
                            return Err(format!(
                                "plugin_not_found: plugin '{}' not found\nRun `claw plugins list` to see available plugins.",
                                name
                            ).into());
                        }
                    }
                }
                println!("{}", payload.message);
            }
            CliOutputFormat::Json => {
                let action_str = action.unwrap_or("list");
                // #743/#420: plugins help must return a usage envelope matching agents/mcp/skills help shape.
                if matches!(action_str, "help" | "-h" | "--help") {
                    let cwd_str = cwd.display().to_string();
                    let obj = json!({
                        "kind": "plugin",
                        "action": "help",
                        "status": "ok",
                        "unexpected": null,
                        "usage": {
                            "direct_cli": "claw plugins [list|show <id>|install <id>|enable <id>|disable <id>|uninstall <id>|update <id>|help]",
                            "slash_command": "/plugins [list|show <id>|install <id>|enable <id>|disable <id>|uninstall <id>|update <id>|help]",
                        },
                        "cwd": cwd_str,
                    });
                    println!("{}", serde_json::to_string_pretty(&obj)?);
                    return Ok(());
                }
                // For show/info/describe, filter to the named plugin (exact match).
                // For list with a target, treat target as a substring filter.
                let is_show_action = matches!(action_str, "show" | "info" | "describe");
                let is_list_action = action_str == "list";
                let filtered_plugins: Vec<_> = if is_show_action {
                    if let Some(name) = target {
                        let needle = name.to_lowercase();
                        payload
                            .plugins
                            .iter()
                            .filter(|p| {
                                p.get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|id| id.to_lowercase() == needle)
                                    .unwrap_or(false)
                            })
                            .cloned()
                            .collect()
                    } else {
                        payload.plugins.clone()
                    }
                } else if is_list_action {
                    if let Some(filter) = target {
                        let needle = filter.to_lowercase();
                        payload
                            .plugins
                            .iter()
                            .filter(|p| {
                                p.get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|id| id.to_lowercase().contains(&needle))
                                    .unwrap_or(false)
                            })
                            .cloned()
                            .collect()
                    } else {
                        payload.plugins.clone()
                    }
                } else {
                    payload.plugins.clone()
                };
                // Return not-found error for show with missing target.
                if is_show_action {
                    if let Some(name) = target {
                        if filtered_plugins.is_empty() {
                            let obj = json!({
                                "kind": "plugin",
                                "action": action_str,
                                "status": "error",
                                "error_kind": "plugin_not_found",
                                "requested": name,
                                // #734: parity with skills show which always emits a message field
                                "message": format!("plugin '{}' not found", name),
                                // #760: hint so callers know how to enumerate available plugins
                                "hint": "Run `claw plugins list` to see available plugins.",
                            });
                            println!("{}", serde_json::to_string_pretty(&obj)?);
                            // #789: exit 1 on not-found so automation can rely on exit code
                            std::process::exit(1);
                        }
                    }
                }
                let enabled_count = filtered_plugins
                    .iter()
                    .filter(|p| p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false))
                    .count();
                let disabled_count = filtered_plugins.len().saturating_sub(enabled_count);
                let mut obj = json!({
                    "kind": "plugin",
                    "action": action_str,
                    "status": payload.status,
                    "summary": {
                        "total": filtered_plugins.len(),
                        "enabled": enabled_count,
                        "disabled": disabled_count,
                        "load_failures": payload.load_failures.len(),
                    },
                    "config_load_error": payload.config_load_error,
                    "mcp_validation": payload.mcp_validation.json_value(),
                    "plugins": filtered_plugins,
                    "load_failures": payload.load_failures,
                });
                // Only include operation-result fields for mutating actions (not list/show)
                if action_str != "list" && !is_show_action {
                    obj["target"] = json!(target);
                    obj["reload_runtime"] = json!(payload.reload_runtime);
                    obj["message"] = json!(payload.message);
                }
                println!("{}", serde_json::to_string_pretty(&obj)?);
            }
        }
        Ok(())
    }

    fn print_diff() -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", render_diff_report()?);
        Ok(())
    }

    fn print_version(output_format: CliOutputFormat) {
        let _ = crate::print_version(output_format);
    }

    fn export_session(
        &self,
        requested_path: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let export_path = resolve_export_path(requested_path, self.runtime.session())?;
        fs::write(&export_path, render_export_text(self.runtime.session()))?;
        println!(
            "Export\n  Result           wrote transcript\n  File             {}\n  Messages         {}",
            export_path.display(),
            self.runtime.session().messages.len(),
        );
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn handle_session_command(
        &mut self,
        action: Option<&str>,
        target: Option<&str>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        match action {
            None | Some("list") => {
                println!("{}", render_session_list(&self.session.id)?);
                Ok(false)
            }
            Some("exists") => {
                let Some(target) = target else {
                    println!("Usage: /session exists <session-id>");
                    return Ok(false);
                };
                let exists = session_reference_exists(target)?;
                let handle = resolve_session_reference(target).ok();
                println!(
                    "Session exists\n  Session          {target}\n  Exists           {exists}{}",
                    handle
                        .as_ref()
                        .map(|handle| format!("\n  File             {}", handle.path.display()))
                        .unwrap_or_default()
                );
                Ok(false)
            }
            Some("switch") => {
                let Some(target) = target else {
                    println!("Usage: /session switch <session-id>");
                    return Ok(false);
                };
                let (handle, session) = load_session_reference(target)?;
                let message_count = session.messages.len();
                let session_id = session.session_id.clone();
                let runtime = build_runtime(
                    session,
                    &handle.id,
                    self.model.clone(),
                    self.system_prompt.clone(),
                    true,
                    true,
                    self.allowed_tools.clone(),
                    self.permission_mode,
                    None,
                )?;
                self.replace_runtime(runtime)?;
                self.session = SessionHandle {
                    id: session_id,
                    path: handle.path,
                };
                println!(
                    "Session switched\n  Active session   {}\n  File             {}\n  Messages         {}",
                    self.session.id,
                    self.session.path.display(),
                    message_count,
                );
                Ok(true)
            }
            Some("fork") => {
                let forked = self.runtime.fork_session(target.map(ToOwned::to_owned));
                let parent_session_id = self.session.id.clone();
                let handle = create_managed_session_handle(&forked.session_id)?;
                let branch_name = forked
                    .fork
                    .as_ref()
                    .and_then(|fork| fork.branch_name.clone());
                let forked = forked.with_persistence_path(handle.path.clone());
                let message_count = forked.messages.len();
                forked.save_to_path(&handle.path)?;
                let runtime = build_runtime(
                    forked,
                    &handle.id,
                    self.model.clone(),
                    self.system_prompt.clone(),
                    true,
                    true,
                    self.allowed_tools.clone(),
                    self.permission_mode,
                    None,
                )?;
                self.replace_runtime(runtime)?;
                self.session = handle;
                println!(
                    "Session forked\n  Parent session   {}\n  Active session   {}\n  Branch           {}\n  File             {}\n  Messages         {}",
                    parent_session_id,
                    self.session.id,
                    branch_name.as_deref().unwrap_or("(unnamed)"),
                    self.session.path.display(),
                    message_count,
                );
                Ok(true)
            }
            Some("delete") => {
                let Some(target) = target else {
                    println!("Usage: /session delete <session-id> [--force]");
                    return Ok(false);
                };
                let handle = resolve_session_reference(target)?;
                if handle.id == self.session.id {
                    println!(
                        "delete: refusing to delete the active session '{}'.\nSwitch to another session first with /session switch <session-id>.",
                        handle.id
                    );
                    return Ok(false);
                }
                if !confirm_session_deletion(&handle.id) {
                    println!("delete: cancelled.");
                    return Ok(false);
                }
                delete_managed_session(&handle.path)?;
                println!(
                    "Session deleted\n  Deleted session  {}\n  File             {}",
                    handle.id,
                    handle.path.display(),
                );
                Ok(false)
            }
            Some("delete-force") => {
                let Some(target) = target else {
                    println!("Usage: /session delete <session-id> [--force]");
                    return Ok(false);
                };
                let handle = resolve_session_reference(target)?;
                if handle.id == self.session.id {
                    println!(
                        "delete: refusing to delete the active session '{}'.\nSwitch to another session first with /session switch <session-id>.",
                        handle.id
                    );
                    return Ok(false);
                }
                delete_managed_session(&handle.path)?;
                println!(
                    "Session deleted\n  Deleted session  {}\n  File             {}",
                    handle.id,
                    handle.path.display(),
                );
                Ok(false)
            }
            Some(other) => {
                println!(
                    "Unknown /session action '{other}'. Use /session list, /session exists <session-id>, /session switch <session-id>, /session fork [branch-name], or /session delete <session-id> [--force]."
                );
                Ok(false)
            }
        }
    }

    fn handle_plugins_command(
        &mut self,
        action: Option<&str>,
        target: Option<&str>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let cwd = env::current_dir()?;
        let payload =
            plugins_command_payload_for(&cwd, action, target, ConfigWarningMode::EmitStderr)?;
        println!("{}", payload.message);
        if payload.reload_runtime {
            self.reload_runtime_features()?;
        }
        Ok(false)
    }

    fn reload_runtime_features(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let runtime = build_runtime(
            self.runtime.session().clone(),
            &self.session.id,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        self.replace_runtime(runtime)?;
        self.persist_session()
    }

    fn compact(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let result = self.runtime.compact(CompactionConfig::default());
        let removed = result.removed_message_count;
        let kept = result.compacted_session.messages.len();
        let skipped = removed == 0;
        let runtime = build_runtime(
            result.compacted_session,
            &self.session.id,
            self.model.clone(),
            self.system_prompt.clone(),
            true,
            true,
            self.allowed_tools.clone(),
            self.permission_mode,
            None,
        )?;
        self.replace_runtime(runtime)?;
        self.persist_session()?;
        println!("{}", format_compact_report(removed, kept, skipped));
        Ok(())
    }

    fn run_internal_prompt_text_with_progress(
        &self,
        prompt: &str,
        enable_tools: bool,
        progress: Option<InternalPromptProgressReporter>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let session = self.runtime.session().clone();
        let mut runtime = build_runtime(
            session,
            &self.session.id,
            self.model.clone(),
            self.system_prompt.clone(),
            enable_tools,
            false,
            self.allowed_tools.clone(),
            self.permission_mode,
            progress,
        )?;
        let mut permission_prompter = CliPermissionPrompter::new(self.permission_mode);
        let summary = runtime.run_turn(prompt, Some(&mut permission_prompter))?;
        let text = final_assistant_text(&summary).trim().to_string();
        runtime.shutdown_plugins()?;
        Ok(text)
    }

    fn run_internal_prompt_text(
        &self,
        prompt: &str,
        enable_tools: bool,
    ) -> Result<String, Box<dyn std::error::Error>> {
        self.run_internal_prompt_text_with_progress(prompt, enable_tools, None)
    }

    fn run_bughunter(&self, scope: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", format_bughunter_report(scope));
        Ok(())
    }

    fn run_ultraplan(&self, task: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", format_ultraplan_report(task));
        Ok(())
    }

    fn run_teleport(target: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let Some(target) = target.map(str::trim).filter(|value| !value.is_empty()) else {
            println!("Usage: /teleport <symbol-or-path>");
            return Ok(());
        };

        println!("{}", render_teleport_report(target)?);
        Ok(())
    }

    fn run_debug_tool_call(&self, args: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        validate_no_args("/debug-tool-call", args)?;
        println!("{}", render_last_tool_debug_report(self.runtime.session())?);
        Ok(())
    }

    fn run_commit(&mut self, args: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        validate_no_args("/commit", args)?;
        let status = git_output(&["status", "--short", "--branch"])?;
        let summary = parse_git_workspace_summary(Some(&status));
        let branch = parse_git_status_branch(Some(&status));
        if summary.is_clean() {
            println!("{}", format_commit_skipped_report());
            return Ok(());
        }

        println!(
            "{}",
            format_commit_preflight_report(branch.as_deref(), summary)
        );
        Ok(())
    }

    fn run_pr(&self, context: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        let branch =
            resolve_git_branch_for(&env::current_dir()?).unwrap_or_else(|| "unknown".to_string());
        println!("{}", format_pr_report(&branch, context));
        Ok(())
    }

    fn run_issue(&self, context: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", format_issue_report(context));
        Ok(())
    }
}

fn sessions_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(current_session_store()?.sessions_dir().to_path_buf())
}

fn current_session_store() -> Result<runtime::SessionStore, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    runtime::SessionStore::from_cwd(&cwd).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
}

fn new_cli_session() -> Result<Session, Box<dyn std::error::Error>> {
    Ok(Session::new().with_workspace_root(env::current_dir()?))
}

fn create_managed_session_handle(
    session_id: &str,
) -> Result<SessionHandle, Box<dyn std::error::Error>> {
    let handle = current_session_store()?.create_handle(session_id);
    Ok(SessionHandle {
        id: handle.id,
        path: handle.path,
    })
}

fn resolve_session_reference(reference: &str) -> Result<SessionHandle, Box<dyn std::error::Error>> {
    let handle = current_session_store()?
        .resolve_reference(reference)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    Ok(SessionHandle {
        id: handle.id,
        path: handle.path,
    })
}

fn session_reference_exists(reference: &str) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(current_session_store()?.session_exists(reference))
}

fn resolve_managed_session_path(session_id: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    current_session_store()?
        .resolve_managed_path(session_id)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
}

fn list_managed_sessions() -> Result<Vec<ManagedSessionSummary>, Box<dyn std::error::Error>> {
    let store = current_session_store()?;
    let lifecycle = classify_session_lifecycle_for(store.workspace_root());
    Ok(store
        .list_sessions()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?
        .into_iter()
        .map(|session| ManagedSessionSummary {
            id: session.id,
            path: session.path,
            created_at_ms: session.created_at_ms,
            updated_at_ms: session.updated_at_ms,
            modified_epoch_millis: session.modified_epoch_millis,
            message_count: session.message_count,
            parent_session_id: session.parent_session_id,
            branch_name: session.branch_name,
            lifecycle: lifecycle.clone(),
        })
        .collect())
}

fn latest_managed_session() -> Result<ManagedSessionSummary, Box<dyn std::error::Error>> {
    let store = current_session_store()?;
    let lifecycle = classify_session_lifecycle_for(store.workspace_root());
    let session = store
        .latest_session()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    Ok(ManagedSessionSummary {
        id: session.id,
        path: session.path,
        created_at_ms: session.created_at_ms,
        updated_at_ms: session.updated_at_ms,
        modified_epoch_millis: session.modified_epoch_millis,
        message_count: session.message_count,
        parent_session_id: session.parent_session_id,
        branch_name: session.branch_name,
        lifecycle,
    })
}

fn load_session_reference(
    reference: &str,
) -> Result<(SessionHandle, Session), Box<dyn std::error::Error>> {
    load_session_reference_excluding(reference, None)
}

fn load_session_reference_excluding(
    reference: &str,
    exclude_id: Option<&str>,
) -> Result<(SessionHandle, Session), Box<dyn std::error::Error>> {
    let store = current_session_store()?;
    let loaded = store
        .load_session_excluding(reference, exclude_id)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    Ok((
        SessionHandle {
            id: loaded.handle.id,
            path: loaded.handle.path,
        },
        loaded.session,
    ))
}

fn delete_managed_session(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !path.exists() {
        return Err(format!("session file does not exist: {}", path.display()).into());
    }
    fs::remove_file(path)?;
    Ok(())
}

fn confirm_session_deletion(session_id: &str) -> bool {
    print!("Delete session '{session_id}'? This cannot be undone. [y/N]: ");
    io::stdout().flush().unwrap_or(());
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES")
}

fn session_details_json(sessions: &[ManagedSessionSummary]) -> Vec<serde_json::Value> {
    sessions
        .iter()
        .map(|session| {
            serde_json::json!({
                "id": session.id,
                "path": session.path.display().to_string(),
                "message_count": session.message_count,
                "created_at_ms": session.created_at_ms,
                "updated_at_ms": session.updated_at_ms,
                "modified_epoch_millis": session.modified_epoch_millis,
                "parent_session_id": session.parent_session_id,
                "branch_name": session.branch_name,
                "lifecycle": session.lifecycle.json_value(),
            })
        })
        .collect()
}

fn session_exists_json(
    target: &str,
    active_session_id: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let handle = create_managed_session_handle(target)?;
    let resolved = resolve_session_reference(target).ok();
    let exists = resolved.is_some();
    let resolved_id = resolved
        .as_ref()
        .map_or(target, |handle| handle.id.as_str());
    Ok(serde_json::json!({
        "kind": "session_exists",
        "action": "exists",
        "status": "ok",
        "session_id": resolved_id,
        "session": target,
        "requested": target,
        "exists": exists,
        "active": resolved_id == active_session_id,
        "path": resolved
            .as_ref()
            .map(|handle| handle.path.display().to_string()),
        "candidate_path": handle.path.display().to_string(),
    }))
}

fn run_resumed_session_command(
    session_path: &Path,
    session: &Session,
    action: Option<&str>,
    target: Option<&str>,
) -> Result<ResumeCommandOutcome, Box<dyn std::error::Error>> {
    match action {
        None | Some("list") => {
            let sessions = list_managed_sessions().unwrap_or_default();
            let session_ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
            let active_id = session.session_id.clone();
            let text = render_session_list(&active_id).unwrap_or_else(|e| format!("error: {e}"));
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(text),
                json: Some(serde_json::json!({
                    "kind": "sessions",
                    "status": "ok",
                    "action": "list",
                    "sessions": session_ids,
                    "session_details": session_details_json(&sessions),
                    "active": active_id,
                })),
            })
        }
        Some("exists") => {
            let Some(target) = target else {
                return Err("/session exists requires a session id.\nUsage: claw --resume <session> /session exists <session-id>".into());
            };
            let value = session_exists_json(target, &session.session_id)?;
            let exists = value
                .get("exists")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format!(
                    "Session exists\n  Session          {}\n  Exists           {}",
                    target,
                    if exists { "yes" } else { "no" }
                )),
                json: Some(value),
            })
        }
        Some("delete") => {
            let Some(target) = target else {
                return Err("/session delete requires a session id.\nUsage: claw --resume <session> /session delete <session-id> --force".into());
            };
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format!(
                    "delete: confirmation required; rerun with /session delete {target} --force"
                )),
                json: Some(serde_json::json!({
                    "kind": "error",
                    "error": "confirmation required",
                    "hint": format!("rerun with /session delete {target} --force"),
                    "session_id": target,
                })),
            })
        }
        Some("delete-force") => {
            let Some(target) = target else {
                return Err("/session delete requires a session id.\nUsage: claw --resume <session> /session delete <session-id> --force".into());
            };
            let handle = resolve_session_reference(target)?;
            if handle.id == session.session_id || handle.path == session_path {
                return Err(format!(
                    "delete: refusing to delete the active session '{}'. Resume or switch to another session first.",
                    handle.id
                )
                .into());
            }
            delete_managed_session(&handle.path)?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(format!(
                    "Session deleted\n  Deleted session  {}\n  File             {}",
                    handle.id,
                    handle.path.display(),
                )),
                json: Some(serde_json::json!({
                    "kind": "session_delete",
                    "action": "delete",
                    "status": "ok",
                    "deleted": true,
                    "session_id": handle.id,
                    "path": handle.path.display().to_string(),
                })),
            })
        }
        // #113: /session switch and /session fork require an interactive REPL —
        // return structured JSON instead of a raw error so resume callers can
        // detect the limitation programmatically.
        Some(switch_or_fork @ ("switch" | "fork")) => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(format!(
                "/session {switch_or_fork} requires an interactive REPL.\nUsage: claw (then /session {switch_or_fork} <id>)"
            )),
            json: Some(serde_json::json!({
                "kind": "error",
                "error_kind": "unsupported_resumed_command",
                "status": "error",
                "action": switch_or_fork,
                "error": format!("/session {switch_or_fork} requires an interactive REPL"),
                "hint": format!("Start a new claw session and use /session {switch_or_fork} <id> interactively"),
            })),
        }),
        Some(other) => Err(format!("unsupported_resumed_command: /session {other} is not supported in resume mode.\nSupported: list, exists, delete").into()),
    }
}

fn render_session_list(active_session_id: &str) -> Result<String, Box<dyn std::error::Error>> {
    let sessions = list_managed_sessions()?;
    let mut lines = vec![
        "Sessions".to_string(),
        format!("  Directory         {}", sessions_dir()?.display()),
    ];
    if sessions.is_empty() {
        lines.push("  No managed sessions saved yet.".to_string());
        return Ok(lines.join("\n"));
    }
    for session in sessions {
        let marker = if session.id == active_session_id {
            "● current"
        } else {
            "○ saved"
        };
        let lineage = match (
            session.branch_name.as_deref(),
            session.parent_session_id.as_deref(),
        ) {
            (Some(branch_name), Some(parent_session_id)) => {
                format!(" branch={branch_name} from={parent_session_id}")
            }
            (None, Some(parent_session_id)) => format!(" from={parent_session_id}"),
            (Some(branch_name), None) => format!(" branch={branch_name}"),
            (None, None) => String::new(),
        };
        lines.push(format!(
            "  {id:<20} {marker:<10} lifecycle={lifecycle} msgs={msgs:<4} modified={modified}{lineage} path={path}",
            id = session.id,
            lifecycle = session.lifecycle.signal(),
            msgs = session.message_count,
            modified = format_session_modified_age(session.modified_epoch_millis),
            lineage = lineage,
            path = session.path.display(),
        ));
    }
    Ok(lines.join("\n"))
}

/// #449: credentials-free session list that works without API keys.
/// `claw session list --output-format json` should work in CI/offline.
fn run_session_list(output_format: CliOutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = list_managed_sessions().unwrap_or_default();
    let session_ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
    let session_details = session_details_json(&sessions);
    match output_format {
        CliOutputFormat::Text => {
            let text = render_session_list("").unwrap_or_else(|e| format!("error: {e}"));
            println!("{text}");
        }
        CliOutputFormat::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "kind": "sessions",
                    "status": "ok",
                    "action": "list",
                    "sessions": session_ids,
                    "session_details": session_details,
                    "active": serde_json::Value::Null,
                })
            );
        }
    }
    Ok(())
}

fn format_session_modified_age(modified_epoch_millis: u128) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map_or(modified_epoch_millis, |duration| duration.as_millis());
    let delta_seconds = now
        .saturating_sub(modified_epoch_millis)
        .checked_div(1_000)
        .unwrap_or_default();
    match delta_seconds {
        0..=4 => "just-now".to_string(),
        5..=59 => format!("{delta_seconds}s-ago"),
        60..=3_599 => format!("{}m-ago", delta_seconds / 60),
        3_600..=86_399 => format!("{}h-ago", delta_seconds / 3_600),
        _ => format!("{}d-ago", delta_seconds / 86_400),
    }
}

fn write_session_clear_backup(
    session: &Session,
    session_path: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let backup_path = session_clear_backup_path(session_path);
    session.save_to_path(&backup_path)?;
    Ok(backup_path)
}

fn session_clear_backup_path(session_path: &Path) -> PathBuf {
    let timestamp = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map_or(0, |duration| duration.as_millis());
    let file_name = session_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("session.jsonl");
    session_path.with_file_name(format!("{file_name}.before-clear-{timestamp}.bak"))
}

fn render_repl_help() -> String {
    [
        "REPL".to_string(),
        "  /exit                Quit the REPL".to_string(),
        "  /quit                Quit the REPL".to_string(),
        "  Up/Down              Navigate prompt history".to_string(),
        "  Ctrl-R               Reverse-search prompt history".to_string(),
        "  Tab                  Complete commands, modes, and recent sessions".to_string(),
        "  Ctrl-C               Clear input (or exit on empty prompt)".to_string(),
        "  Shift+Enter/Ctrl+J   Insert a newline".to_string(),
        "  Auto-save            .claw/sessions/<workspace-fingerprint>/<session-id>.jsonl"
            .to_string(),
        "  Resume latest        /resume latest".to_string(),
        "  Browse sessions      /session list".to_string(),
        "  Show prompt history  /history [count]".to_string(),
        String::new(),
        render_slash_command_help_filtered(STUB_COMMANDS),
    ]
    .join(
        "
",
    )
}

fn print_status_snapshot(
    model: &str,
    model_flag_raw: Option<&str>,
    permission_mode: PermissionModeProvenance,
    output_format: CliOutputFormat,
    allowed_tools: Option<&AllowedToolSet>,
) -> Result<(), Box<dyn std::error::Error>> {
    let usage = StatusUsage {
        message_count: 0,
        turns: 0,
        latest: TokenUsage::default(),
        cumulative: TokenUsage::default(),
        estimated_tokens: 0,
    };
    let context = status_context(None)?;
    // #148: resolve model provenance. If user passed --model, source is
    // "flag" with the raw input preserved. Otherwise probe env -> config
    // -> default and record the winning source.
    let provenance_result = match model_flag_raw {
        Some(raw) => Ok(ModelProvenance::from_flag(raw, model)),
        None => ModelProvenance::from_env_or_config_or_default(model),
    };
    let provenance = match provenance_result {
        Ok(provenance) => provenance,
        Err(error) => match output_format {
            CliOutputFormat::Json => {
                return print_model_validation_warning_status(
                    &error,
                    usage,
                    permission_mode.mode.as_str(),
                    &context,
                    allowed_tools,
                );
            }
            CliOutputFormat::Text => return Err(error.into()),
        },
    };
    let format_selection = current_output_format_selection();
    match output_format {
        CliOutputFormat::Text => println!(
            "{}",
            format_status_report(
                &provenance.resolved,
                usage,
                permission_mode.mode.as_str(),
                &context,
                Some(&provenance),
                Some(&permission_mode),
            )
        ),
        CliOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&status_json_value(
                Some(&provenance.resolved),
                usage,
                permission_mode.mode.as_str(),
                &context,
                Some(&provenance),
                Some(&permission_mode),
                allowed_tools,
                Some(&format_selection),
            ))?
        ),
    }
    Ok(())
}

fn status_json_value(
    model: Option<&str>,
    usage: StatusUsage,
    permission_mode: &str,
    context: &StatusContext,
    // #148: optional provenance for `model` field. Surfaces `model_source`
    // ("flag" | "env" | "config" | "default") and `model_raw` (user input
    // before alias resolution, or null when source is "default"). Callers
    // that don't have provenance (legacy resume paths) pass None, in which
    // case both new fields are omitted.
    provenance: Option<&ModelProvenance>,
    permission_provenance: Option<&PermissionModeProvenance>,
    allowed_tools: Option<&AllowedToolSet>,
    format_selection: Option<&OutputFormatSelection>,
) -> serde_json::Value {
    // #143: top-level `status` marker so claws can distinguish
    // a clean run from a degraded run (config parse failed but other fields
    // are still populated). `config_load_error` carries the parse-error string
    // when present; it's a string rather than a typed object in Phase 1 and
    // will join the typed-error taxonomy in Phase 2 (ROADMAP §4.44).
    // `config_load_error_kind` is the machine-readable kind token derived from
    // `classify_error_kind` so downstream claws can switch on it directly.
    let degraded = context.config_load_error.is_some();
    let model_source = provenance.map(|p| p.source.as_str());
    let model_raw = provenance.and_then(|p| p.raw.clone());
    let model_alias_resolved_to = provenance.and_then(|p| p.alias_resolved_to.clone());
    let model_env_var = provenance.and_then(|p| p.env_var.clone());
    let permission_mode_source = permission_provenance.map(|p| p.source.as_str());
    let permission_mode_env_var = permission_provenance.and_then(|p| p.env_var);
    let tool_registry = GlobalToolRegistry::builtin();
    let available_tool_names = tool_registry.canonical_allowed_tool_names();
    let tool_aliases = allowed_tool_aliases_json(&tool_registry);
    let output_format_selection = format_selection.cloned().unwrap_or_default();
    // #732: always emit an array (empty when unrestricted) so callers can do
    // `.allowed_tools.entries | length > 0` without a null-check first.
    let allowed_tool_entries = allowed_tools
        .map(|tools| tools.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    json!({
        "kind": "status",
        "action": "show",
        "status": if degraded || context.mcp_validation.has_invalid_servers() || context.hook_validation.has_invalid_hooks() { "degraded" } else { "ok" },
        "config_load_error": context.config_load_error,
        "config_load_error_kind": context.config_load_error_kind,
        "mcp_validation": context.mcp_validation.json_value(),
        "hook_validation": context.hook_validation.json_value(),
        "duplicate_flags": context.duplicate_flags,

        "model": model,
        "model_source": model_source,
        "model_raw": model_raw,
        "model_alias_resolved_to": model_alias_resolved_to,
        "model_env_var": model_env_var,
        "permission_mode": permission_mode,
        "permission_mode_source": permission_mode_source,
        "permission_mode_env_var": permission_mode_env_var,
        "allowed_tools": {
            "source": if allowed_tools.is_some() { "flag" } else { "default" },
            "restricted": allowed_tools.is_some(),
            "entries": allowed_tool_entries,
            "available": available_tool_names,
            "aliases": tool_aliases,
        },
        "format_source": output_format_selection.source.as_str(),
        "format_raw": output_format_selection.raw,
        "format_overridden": output_format_selection.overridden,
        "binary_provenance": context.binary_provenance.json_value(),
        "usage": {
            "messages": usage.message_count,
            "turns": usage.turns,
            "latest_input": usage.latest.input_tokens,
            "latest_output": usage.latest.output_tokens,
            "latest_cache_creation_input": usage.latest.cache_creation_input_tokens,
            "latest_cache_read_input": usage.latest.cache_read_input_tokens,
            "latest_total": usage.latest.total_tokens(),
            "cumulative_input": usage.cumulative.input_tokens,
            "cumulative_output": usage.cumulative.output_tokens,
            "cumulative_cache_creation_input": usage.cumulative.cache_creation_input_tokens,
            "cumulative_cache_read_input": usage.cumulative.cache_read_input_tokens,
            "cumulative_total": usage.cumulative.total_tokens(),
            "estimated_cost_usd": format_usd(usage.cumulative.estimate_cost_usd().total_cost_usd()), "estimated_cost_usd_num": usage.cumulative.estimate_cost_usd().total_cost_usd(),
            "pricing": "estimated-default",
            "estimated_tokens": usage.estimated_tokens,
        },
        "lane_board": {
            "schema": "task_registry_v1",
            "status_json_supported": true,
            "heartbeat_freshness_supported": true,
            "states": ["active", "blocked", "finished"],
            "freshness_states": ["healthy", "stalled", "transport_dead", "unknown"],
        },
        "workspace": {
            "cwd": context.cwd,
            "project_root": context.project_root,
            "git_branch": context.git_branch,
            "git_state": if context.project_root.is_some() { context.git_summary.headline() } else { "no_git_repo".to_string() },
            // #408: changed_files counts ALL non-clean files (staged + unstaged + untracked + conflicted)
            "changed_files": context.git_summary.changed_files,
            "is_clean": context.git_summary.changed_files == 0,
            "staged_files": context.git_summary.staged_files,
            // #89: mid-operation git state (rebase, merge, cherry-pick, bisect)
            "git_operation": if context.git_summary.operation != GitOperation::None {
                Some(context.git_summary.operation.as_str())
            } else {
                None::<&str>
            },

            "unstaged_files": context.git_summary.unstaged_files,
            "untracked_files": context.git_summary.untracked_files,
            "session": context.session_path.as_ref().map_or_else(|| "live-repl".to_string(), |path| path.display().to_string()),
            "session_id": context.session_path.as_ref().and_then(|path| {
                // Session files are named <session-id>.jsonl directly under
                // .claw/sessions/. Extract the stem (drop the .jsonl extension).
                path.file_stem().map(|n| n.to_string_lossy().into_owned())
            }),
            "session_lifecycle": context.session_lifecycle.json_value(),
            "branch_freshness": context.branch_freshness.json_value(),
            "boot_preflight": context.boot_preflight.json_value(),
            "loaded_config_files": context.loaded_config_files,
            "discovered_config_files": context.discovered_config_files,
            "memory_file_count": context.memory_file_count,
            "memory_files": memory_files_json(&context.memory_files),
            "unloaded_memory_files": context.unloaded_memory_files,
            "mcp_validation": context.mcp_validation.json_value(),
            "hook_validation": context.hook_validation.json_value(),
        },
        "sandbox": {
            "enabled": context.sandbox_status.enabled,
            "active": context.sandbox_status.active,
            "supported": context.sandbox_status.supported,
            "in_container": context.sandbox_status.in_container,
            "requested_namespace": context.sandbox_status.requested.namespace_restrictions,
            "active_namespace": context.sandbox_status.namespace_active,
            "requested_network": context.sandbox_status.requested.network_isolation,
            "active_network": context.sandbox_status.network_active,
            "filesystem_mode": context.sandbox_status.filesystem_mode.as_str(),
            "filesystem_active": context.sandbox_status.filesystem_active,
            "allowed_mounts": context.sandbox_status.allowed_mounts,
            "markers": context.sandbox_status.container_markers,
            "fallback_reason": context.sandbox_status.fallback_reason,
        }
    })
}

/// #421: Strip macOS `/private` symlink prefix from paths so that
/// `status`, `doctor`, and `mcp list` JSON output matches the
/// user-visible invocation cwd instead of the canonicalized path.
fn friendly_cwd(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(stripped) = path.strip_prefix("/private") {
            if stripped.is_absolute() {
                return stripped.to_path_buf();
            }
        }
    }
    path
}

fn status_context(
    session_path: Option<&Path>,
) -> Result<StatusContext, Box<dyn std::error::Error>> {
    let cwd = friendly_cwd(env::current_dir()?);
    let loader = ConfigLoader::default_for(&cwd);
    // #456: count only paths that exist on disk, matching check_config_health behavior.
    let discovered_config_files = loader.discover().iter().filter(|e| e.path.exists()).count();
    // #143: degrade gracefully on config parse failure rather than hard-fail.
    // `claw doctor` already does this; `claw status` now matches that contract
    // so that one malformed `mcpServers.*` entry doesn't take down the whole
    // health surface (workspace, git, model, permission, sandbox can still be
    // reported independently).
    let runtime_config = loader.load();
    let (loaded_config_files, sandbox_status, config_load_error, config_load_error_kind) =
        match runtime_config.as_ref() {
            Ok(cfg) => (
                cfg.loaded_entries().len(),
                resolve_sandbox_status(cfg.sandbox(), &cwd),
                None,
                None,
            ),
            Err(err) => {
                let err_string = err.to_string();
                let err_kind = classify_error_kind(&err_string);
                (
                    0,
                    // Fall back to defaults for sandbox resolution so claws still see
                    // a populated sandbox section instead of a missing field. Defaults
                    // produce the same output as a runtime config with no sandbox
                    // overrides, which is the right degraded-mode shape: we cannot
                    // report what the user *intended*, only what is actually in effect.
                    resolve_sandbox_status(&runtime::SandboxConfig::default(), &cwd),
                    Some(err_string),
                    Some(err_kind),
                )
            }
        };
    let project_context = ProjectContext::discover_with_git(&cwd, DEFAULT_DATE)?;
    let (project_root, git_branch) =
        parse_git_status_metadata(project_context.git_status.as_deref());
    let git_summary = parse_git_workspace_summary(project_context.git_status.as_deref());
    let branch_freshness = BranchFreshness::from_git_status(project_context.git_status.as_deref());
    let stale_base_state = stale_base_state_for(&cwd, None);
    let boot_preflight = build_boot_preflight_snapshot(
        &cwd,
        project_root.as_deref(),
        project_context.git_status.as_deref(),
        runtime_config.as_ref().ok(),
        config_load_error.as_deref(),
    );
    let memory_files = memory_file_summaries_for(
        &cwd,
        project_root.as_deref(),
        &project_context.instruction_files,
    );
    let mcp_validation = runtime_config
        .as_ref()
        .ok()
        .map(|runtime_config| McpValidationSummary::from_collection(runtime_config.mcp()))
        .unwrap_or_default();
    let hook_validation = runtime_config
        .as_ref()
        .ok()
        .map(HookValidationSummary::from_config)
        .unwrap_or_default();
    Ok(StatusContext {
        cwd: cwd.clone(),
        session_path: session_path.map(Path::to_path_buf),
        loaded_config_files,
        discovered_config_files,
        memory_file_count: project_context.instruction_files.len(),
        memory_files: memory_files.clone(),
        unloaded_memory_files: unloaded_memory_candidates(
            &cwd,
            project_root.as_deref(),
            &memory_files,
        ),
        project_root,
        git_branch,
        git_summary,
        branch_freshness,
        stale_base_state,
        session_lifecycle: classify_session_lifecycle_for(&cwd),
        boot_preflight,
        sandbox_status,
        binary_provenance: binary_provenance_for(Some(&cwd)),
        config_load_error,
        config_load_error_kind,
        mcp_validation,

        hook_validation,
        duplicate_flags: take_duplicate_flags(),
    })
}

fn format_status_report(
    model: &str,
    usage: StatusUsage,
    permission_mode: &str,
    context: &StatusContext,
    // #148: optional model provenance to surface in a `Model source` line.
    // Callers without provenance (legacy resume paths) pass None and the
    // source line is omitted for backward compat.
    provenance: Option<&ModelProvenance>,
    permission_provenance: Option<&PermissionModeProvenance>,
) -> String {
    // #143: if config failed to parse, surface a degraded banner at the top
    // of the text report so humans see the parse error before the body, while
    // the body below still reports everything that could be resolved without
    // config (workspace, git, sandbox defaults, etc.).
    let status_line = if context.config_load_error.is_some() {
        "Status (degraded)"
    } else {
        "Status"
    };
    let mut blocks: Vec<String> = Vec::new();
    if let Some(err) = context.config_load_error.as_deref() {
        blocks.push(format!(
            "Config load error\n  Status           fail\n  Summary          runtime config failed to load; reporting partial status\n  Details          {err}\n  Hint             `claw doctor` classifies config parse errors; fix the listed field and rerun"
        ));
    }
    // #148: render Model source line after Model, showing where the string
    // came from (flag / env / config / default) and the raw input if any.
    let model_source_line = provenance
        .map(|p| match &p.raw {
            Some(raw) if raw != model => {
                let env_suffix = p
                    .env_var
                    .as_deref()
                    .map_or(String::new(), |name| format!(" via {name}"));
                format!(
                    "\n  Model source     {}{env_suffix} (raw: {raw}, alias: {model})",
                    p.source.as_str()
                )
            }
            Some(_) => {
                let env_suffix = p
                    .env_var
                    .as_deref()
                    .map_or(String::new(), |name| format!(" via {name}"));
                format!("\n  Model source     {}{env_suffix}", p.source.as_str())
            }
            None => format!("\n  Model source     {}", p.source.as_str()),
        })
        .unwrap_or_default();
    let permission_source_line = permission_provenance
        .map(|p| {
            let env_suffix = p
                .env_var
                .map_or(String::new(), |name| format!(" via {name}"));
            format!("\n  Permission source {}{env_suffix}", p.source.as_str())
        })
        .unwrap_or_default();
    blocks.extend([
        format!(
            "{status_line}
  Model            {model}{model_source_line}
  Permission mode  {permission_mode}{permission_source_line}
  Messages         {}
  Turns            {}
  Estimated tokens {}",
            usage.message_count, usage.turns, usage.estimated_tokens,
        ),
        format!(
            "Usage
  Latest total     {}
  Cumulative input {}
  Cumulative output {}
  Cache create     {}
  Cache read       {}
  Cumulative total {}
  Estimated cost   {}",
            usage.latest.total_tokens(),
            usage.cumulative.input_tokens,
            usage.cumulative.output_tokens,
            usage.cumulative.cache_creation_input_tokens,
            usage.cumulative.cache_read_input_tokens,
            usage.cumulative.total_tokens(),
            format_usd(usage.cumulative.estimate_cost_usd().total_cost_usd()),
        ),
        format!(
            "Workspace
  Cwd              {}
  Project root     {}
  Git branch       {}
  Git state        {}
  Changed files    {}
  Staged           {}
  Unstaged         {}
  Untracked        {}
  Session          {}
  Lifecycle        {}
  Branch fresh     {}
  Boot preflight   {}
  Config files     loaded {}/{}
  Memory files     {}
  Loaded memory    {}
  Suggested flow   /status → /diff → /commit",
            context.cwd.display(),
            context
                .project_root
                .as_ref()
                .map_or_else(|| "unknown".to_string(), |path| path.display().to_string()),
            context.git_branch.as_deref().unwrap_or("unknown"),
            if context.project_root.is_some() {
                context.git_summary.headline()
            } else {
                "no_git_repo".to_string()
            },
            context.git_summary.changed_files,
            context.git_summary.staged_files,
            context.git_summary.unstaged_files,
            context.git_summary.untracked_files,
            context.session_path.as_ref().map_or_else(
                || "live-repl".to_string(),
                |path| path.display().to_string()
            ),
            context.session_lifecycle.signal(),
            context
                .branch_freshness
                .fresh
                .map(|fresh| if fresh { "yes" } else { "behind" })
                .unwrap_or("no upstream"),
            context.boot_preflight.summary(),
            context.loaded_config_files,
            context.discovered_config_files,
            context.memory_file_count,
            if context.memory_files.is_empty() {
                "<none>".to_string()
            } else {
                context
                    .memory_files
                    .iter()
                    .map(|file| format!("{}:{}", file.source, file.path))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ),
        format_sandbox_report(&context.sandbox_status),
    ]);
    blocks.join("\n\n")
}

fn format_sandbox_report(status: &runtime::SandboxStatus) -> String {
    format!(
        "Sandbox
  Enabled           {}
  Active            {}
  Supported         {}
  In container      {}
  Requested ns      {}
  Active ns         {}
  Requested net     {}
  Active net        {}
  Filesystem mode   {}
  Filesystem active {}
  Allowed mounts    {}
  Markers           {}
  Fallback reason   {}",
        status.enabled,
        status.active,
        status.supported,
        status.in_container,
        status.requested.namespace_restrictions,
        status.namespace_active,
        status.requested.network_isolation,
        status.network_active,
        status.filesystem_mode.as_str(),
        status.filesystem_active,
        if status.allowed_mounts.is_empty() {
            "<none>".to_string()
        } else {
            status.allowed_mounts.join(", ")
        },
        if status.container_markers.is_empty() {
            "<none>".to_string()
        } else {
            status.container_markers.join(", ")
        },
        status
            .fallback_reason
            .clone()
            .unwrap_or_else(|| "<none>".to_string()),
    )
}

fn format_commit_preflight_report(branch: Option<&str>, summary: GitWorkspaceSummary) -> String {
    format!(
        "Commit
  Result           ready
  Branch           {}
  Workspace        {}
  Changed files    {}
  Action           create a git commit from the current workspace changes",
        branch.unwrap_or("unknown"),
        summary.headline(),
        summary.changed_files,
    )
}

fn format_commit_skipped_report() -> String {
    "Commit
  Result           skipped
  Reason           no workspace changes
  Action           create a git commit from the current workspace changes
  Next             /status to inspect context · /diff to inspect repo changes"
        .to_string()
}

fn print_sandbox_status_snapshot(
    output_format: CliOutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let runtime_config = loader
        .load()
        .unwrap_or_else(|_| runtime::RuntimeConfig::empty());
    let status = resolve_sandbox_status(runtime_config.sandbox(), &cwd);
    match output_format {
        CliOutputFormat::Text => println!("{}", format_sandbox_report(&status)),
        CliOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&sandbox_json_value(&status))?
        ),
    }
    Ok(())
}

fn sandbox_json_value(status: &runtime::SandboxStatus) -> serde_json::Value {
    // Derive top-level status so automation can do a single field check
    // instead of combining enabled/active/supported booleans.
    // ok   = not enabled (not requested), OR enabled and active
    // warn = enabled and supported but not yet active (degraded),
    //        OR enabled but unsupported on this platform AND filesystem sandbox is active
    //        (#731: "not supported on macOS" is a degraded state, not a hard error;
    //         filesystem_active:true means partial containment is working)
    // error = enabled but unsupported AND no filesystem sandbox either (nothing active)
    let top_status = if !status.enabled {
        "ok"
    } else if status.active {
        "ok"
    } else if status.supported {
        "warn"
    } else if status.filesystem_active {
        // Platform doesn't support namespace isolation but filesystem sandbox is active:
        // this is a degraded/partial state, not a hard error.
        "warn"
    } else {
        "error"
    };
    json!({
        "kind": "sandbox",
        "action": "status",
        "status": top_status,
        "enabled": status.enabled,
        "requested": status.enabled,
        "active": status.active,
        "supported": status.supported,
        "in_container": status.in_container,
        "requested_namespace": status.requested.namespace_restrictions,
        "active_namespace": status.namespace_active,
        "requested_network": status.requested.network_isolation,
        "active_network": status.network_active,
        "filesystem_mode": status.filesystem_mode.as_str(),
        "filesystem_active": status.filesystem_active,
        "allowed_mounts": status.allowed_mounts,
        "markers": status.container_markers,
        "fallback_reason": status.fallback_reason,
        "active_components": {
            "namespace": status.namespace_active,
            "network": status.network_active,
            "filesystem": status.filesystem_active,
        },
    })
}

fn render_help_topic(topic: LocalHelpTopic) -> String {
    match topic {
        LocalHelpTopic::Status => "Status
  Usage            claw status [--output-format <format>]
  Purpose          show the local workspace snapshot without entering the REPL
  Output           model, permissions, git state, config files, and sandbox status
  Formats          text (default), json
  Related          /status · claw --resume latest /status"
            .to_string(),
        LocalHelpTopic::Sandbox => "Sandbox
  Usage            claw sandbox [--output-format <format>]
  Purpose          inspect the resolved sandbox and isolation state for the current directory
  Output           namespace, network, filesystem, and fallback details
  Formats          text (default), json
  Related          /sandbox · claw status"
            .to_string(),
        LocalHelpTopic::Doctor => "Doctor
  Usage            claw doctor [--output-format <format>]
  Purpose          diagnose local auth, config, workspace, sandbox, and build metadata
  Output           local-only health report; no provider request or session resume required
  Formats          text (default), json
  Related          /doctor · claw --resume latest /doctor"
            .to_string(),
        LocalHelpTopic::Acp => "ACP / Zed
  Usage            claw acp [serve] [--output-format <format>]
  Aliases          claw --acp · claw -acp
  Purpose          explain the current editor-facing ACP/Zed launch contract without starting the runtime
  Status           discoverability only; `serve` is a status alias and does not launch a daemon yet
  Formats          text (default), json
  Related          ROADMAP #64a (discoverability) · ROADMAP #76 (real ACP support) · claw --help"
            .to_string(),
        LocalHelpTopic::Init => "Init
  Usage            claw init [--output-format <format>]
  Purpose          create .claw/settings.json, .claw.json, .gitignore, and CLAUDE.md in the current project
  Output           per-artifact created/updated/partial/deferred/skipped status (idempotent: safe to re-run)
  Formats          text (default), json
  Related          claw status · claw doctor"
            .to_string(),
        LocalHelpTopic::State => "State
  Usage            claw state [--output-format <format>]
  Purpose          read .claw/worker-state.json written by the interactive REPL or a one-shot prompt
  Output           worker id, model, permissions, session reference (text or json)
  Formats          text (default), json
  Produces state   `claw` (interactive REPL) or `claw prompt <text>` (one non-interactive turn)
  Observes state   `claw state` reads; clawhip/CI may poll this file without HTTP
  Exit codes       0 if state file exists and parses; 1 with actionable hint otherwise
  Related          claw status · ROADMAP #139 (this worker-concept contract)"
            .to_string(),
        LocalHelpTopic::Resume => format!(
            "Resume\n  Usage            claw resume [session-path|session-id|{LATEST_SESSION_REFERENCE}] [/slash-command ...] [--output-format <format>]\n  Alias            claw --resume [session-path|session-id|{LATEST_SESSION_REFERENCE}]\n  Purpose          restore or inspect a saved session without starting a new provider turn\n  Output           session restore or resume-safe command output; missing sessions return session_not_found\n  Formats          text (default), json\n  Related          /resume · /session list · claw --resume {LATEST_SESSION_REFERENCE} /status"
        ),
        LocalHelpTopic::Session => "Session
  Usage            claw session --help [--output-format <format>]
  Purpose          show /session command guidance without loading config, credentials, or a session
  Actions          list · exists <id> · switch <id> · fork <name> · delete <id>
  Direct use       run /session in the REPL or claw --resume SESSION.jsonl /session <action>
  Formats          text (default), json
  Related          claw resume · claw export · .claw/sessions/"
            .to_string(),
        LocalHelpTopic::Compact => "Compact
  Usage            claw compact --help [--output-format <format>]
  Purpose          show compaction guidance without loading config, credentials, or a session
  Direct use       run /compact in the REPL or claw --resume SESSION.jsonl /compact
  Output           compaction removes older tool-detail messages when the selected session is large enough
  Formats          text (default), json
  Related          claw resume · /compact · /status"
            .to_string(),
        LocalHelpTopic::Export => "Export
  Usage            claw export [--session <id|latest>] [--output <path>] [--output-format <format>]
  Purpose          serialize a managed session to JSON for review, transfer, or archival
  Defaults         --session latest (most recent managed session in .claw/sessions/)
  Formats          text (default), json
  Related          /session list · claw --resume latest"
            .to_string(),
        LocalHelpTopic::Version => "Version
  Usage            claw version [--output-format <format>]
  Aliases          claw --version · claw -V
  Purpose          print the claw CLI version and build metadata
  Formats          text (default), json
  Related          claw doctor (full build/auth/config diagnostic)"
            .to_string(),
        LocalHelpTopic::SystemPrompt => "System Prompt
  Usage            claw system-prompt [--cwd <path>] [--date YYYY-MM-DD] [--output-format <format>]
  Purpose          render the resolved system prompt that `claw` would send for the given cwd + date
  Options          --cwd overrides the workspace dir · --date injects a deterministic date stamp
  Formats          text (default), json
  Related          claw doctor · claw dump-manifests"
            .to_string(),
        LocalHelpTopic::DumpManifests => "Dump Manifests
  Usage            claw dump-manifests [--manifests-dir <path>] [--output-format <format>]
  Purpose          emit every skill/agent/tool manifest the resolver would load for the current cwd
  Options          --manifests-dir scopes discovery to a specific directory
  Formats          text (default), json
  Related          claw skills · claw agents · claw doctor"
            .to_string(),
        LocalHelpTopic::BootstrapPlan => "Bootstrap Plan
  Usage            claw bootstrap-plan [--output-format <format>]
  Purpose          list the ordered startup phases the CLI would execute before dispatch
  Output           phase names (text) or structured phase list (json) — primary output is the plan itself
  Formats          text (default), json
  Related          claw doctor · claw status"
            .to_string(),
        LocalHelpTopic::Agents => commands::handle_agents_slash_command(
            Some("--help"),
            &env::current_dir().unwrap_or_default(),
        )
        .unwrap_or_else(|_| "agents help unavailable".to_string()),
        LocalHelpTopic::Skills => commands::handle_skills_slash_command(
            Some("--help"),
            &env::current_dir().unwrap_or_default(),
        )
        .unwrap_or_else(|_| "skills help unavailable".to_string()),
        LocalHelpTopic::Plugins => "Plugins
  Usage            claw plugins [list|show <name>|install <path>|enable <name>|disable <name>|uninstall <name>]
  Purpose          manage lifecycle of plugins that extend tool and hook capabilities
  Formats          text (default), json
  Related          /plugins · claw plugins --help"
            .to_string(),
        LocalHelpTopic::Mcp => "MCP Servers
  Usage            claw mcp [list|show <server>] [--output-format <format>]
  Purpose          inspect configured MCP servers and their connection status
  Formats          text (default), json
  Related          /mcp · claw mcp list"
            .to_string(),
        LocalHelpTopic::Config => "Config
  Usage            claw config [section] [--output-format <format>]
  Purpose          show effective runtime configuration (model, hooks, plugins, env)
  Formats          text (default), json
  Related          /config · claw doctor"
            .to_string(),
        LocalHelpTopic::Model => "Models
  Usage            claw models [help] [--output-format <format>]
  Aliases          claw model
  Purpose          show bounded local model command guidance without entering the REPL
  Output           supported model-selection surfaces and current config model value
  Formats          text (default), json
  Related          /model · claw config model · claw status"
            .to_string(),
        LocalHelpTopic::Settings => "Settings
  Usage            claw settings [help] [--output-format <format>]
  Purpose          show effective settings/config using the local config envelope
  Output           same as claw config settings; no provider request or session resume required
  Formats          text (default), json
  Related          claw config · claw doctor"
            .to_string(),
        LocalHelpTopic::Diff => "Diff
  Usage            claw diff [--output-format <format>]
  Purpose          show the diff of changes relative to the expected base commit
  Formats          text (default), json
  Related          /diff · ROADMAP #148"
            .to_string(),
        LocalHelpTopic::Setup => "Setup
  Usage            claw setup
  Aliases          /setup (inside the REPL)
  Purpose          run the interactive provider setup wizard to configure API key, model, and base URL
  Output           writes provider settings to ~/.claw/settings.json (0600 permissions)
  Related          /model · /config · claw doctor"
            .to_string(),
    }
}

fn local_help_topic_command(topic: LocalHelpTopic) -> &'static str {
    match topic {
        LocalHelpTopic::Status => "status",
        LocalHelpTopic::Sandbox => "sandbox",
        LocalHelpTopic::Doctor => "doctor",
        LocalHelpTopic::Acp => "acp",
        LocalHelpTopic::Init => "init",
        LocalHelpTopic::State => "state",
        LocalHelpTopic::Resume => "resume",
        LocalHelpTopic::Session => "session",
        LocalHelpTopic::Compact => "compact",
        LocalHelpTopic::Export => "export",
        LocalHelpTopic::Version => "version",
        LocalHelpTopic::SystemPrompt => "system-prompt",
        LocalHelpTopic::DumpManifests => "dump-manifests",
        LocalHelpTopic::BootstrapPlan => "bootstrap-plan",
        LocalHelpTopic::Agents => "agents",
        LocalHelpTopic::Skills => "skills",
        LocalHelpTopic::Plugins => "plugins",
        LocalHelpTopic::Mcp => "mcp",
        LocalHelpTopic::Config => "config",
        LocalHelpTopic::Model => "models",
        LocalHelpTopic::Settings => "settings",
        LocalHelpTopic::Diff => "diff",
        LocalHelpTopic::Setup => "setup",
    }
}

fn print_models(
    action: Option<&str>,
    output_format: CliOutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let help_requested = action.is_some_and(|value| matches!(value, "help" | "--help" | "-h"));
    if help_requested {
        return print_help_topic(LocalHelpTopic::Model, output_format);
    }
    if let Some(action) = action {
        return Err(format!(
            "unsupported_models_action: unsupported models action: {action}.\nUsage: claw models [help] [--output-format json]"
        )
        .into());
    }

    let configured_model = config_model_for_current_dir();
    let resolved_config_model = configured_model
        .as_deref()
        .map(resolve_model_alias_with_config);

    match output_format {
        CliOutputFormat::Text => {
            println!("Models");
            println!("  Default          {DEFAULT_MODEL}");
            println!("  Built-in aliases opus, sonnet, haiku");
            if let Some(raw) = configured_model.as_deref() {
                println!(
                    "  Config model     {raw}{}",
                    resolved_config_model
                        .as_deref()
                        .filter(|resolved| *resolved != raw)
                        .map(|resolved| format!(" -> {resolved}"))
                        .unwrap_or_default()
                );
            } else {
                println!("  Config model     <unset>");
            }
            println!("  Usage            claw --model <provider/model> prompt <text>");
        }
        CliOutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "kind": "models",
                    "action": "list",
                    "status": "ok",
                    "default_model": DEFAULT_MODEL,
                    "aliases": [
                        {"name": "opus", "model": resolve_model_alias("opus")},
                        {"name": "sonnet", "model": resolve_model_alias("sonnet")},
                        {"name": "haiku", "model": resolve_model_alias("haiku")}
                    ],
                    "configured_model": configured_model,
                    "resolved_configured_model": resolved_config_model,
                    "local_only": true,
                    "requires_credentials": false,
                    "requires_provider_request": false,
                    "message": "Use --model <provider/model> or configure a model in claw settings."
                }))?
            );
        }
    }
    Ok(())
}

fn render_export_help_json() -> serde_json::Value {
    json!({
        "kind": "help",
        "action": "help",
        "status": "ok",
        "topic": "export",
        "command": "export",
        "usage": "claw export [--session <id|latest>] [--output <path>] [--output-format <format>]",
        "purpose": "serialize a managed session to JSON for review, transfer, or archival",
        "defaults": {
            "session": LATEST_SESSION_REFERENCE,
            "session_source": ".claw/sessions/",
            "output": "derived from the selected session when omitted"
        },
        "formats": ["text", "json"],
        "options": [
            {
                "name": "--session",
                "value": "<id|latest>",
                "default": LATEST_SESSION_REFERENCE,
                "description": "managed session to export"
            },
            {
                "name": "--output",
                "aliases": ["-o"],
                "value": "<path>",
                "description": "write the exported transcript to this path"
            },
            {
                "name": "--output-format",
                "value": "<format>",
                "values": ["text", "json"],
                "default": "text",
                "description": "format for the command result envelope"
            },
            {
                "name": "--help",
                "aliases": ["-h"],
                "description": "show help for the export command"
            }
        ],
        "related": ["/session list", "claw --resume latest"]
    })
}

fn render_doctor_help_json() -> serde_json::Value {
    json!({
        "kind": "help",
        "action": "help",
        "status": "ok",
        "topic": "doctor",
        "command": "doctor",
        "schema_version": "1.0",
        "usage": "claw doctor [--output-format <format>]",
        "purpose": "diagnose local auth, config, workspace memory, permissions, sandbox, boot preflight, and build metadata",
        "formats": ["text", "json"],
        "local_only": true,
        "requires_credentials": false,
        "requires_provider_request": false,
        "requires_session_resume": false,
        "mutates_workspace": false,
        "output_fields": ["kind", "action", "status", "message", "report", "has_failures", "summary", "checks", "allowed_tools"],
        "check_names": ["auth", "config", "mcp validation", "hook validation", "install source", "workspace", "memory", "boot preflight", "sandbox", "permissions", "system"],
        "status_values": ["ok", "warn", "fail"],
        "options": [
            {
                "name": "--output-format",
                "value": "<format>",
                "values": ["text", "json"],
                "default": "text",
                "description": "format for the doctor report or help envelope"
            },
            {
                "name": "--help",
                "aliases": ["-h"],
                "description": "show help for the doctor command without running diagnostics"
            }
        ],
        "related": ["/doctor", "claw --resume latest /doctor"],
        "message": render_help_topic(LocalHelpTopic::Doctor),
    })
}

/// #683-#692: extract structured metadata from help prose
fn extract_help_metadata(
    topic: LocalHelpTopic,
) -> (
    Option<String>,      // usage
    Option<String>,      // purpose
    Option<String>,      // output description
    Option<Vec<String>>, // formats
    Option<Vec<String>>, // related
    Option<Vec<String>>, // aliases
    bool,                // local_only
    bool,                // requires_credentials
) {
    let text = render_help_topic(topic);
    let mut usage = None;
    let mut purpose = None;
    let mut output_desc = None;
    let formats = Some(vec!["text".to_string(), "json".to_string()]);
    let mut related = None;
    let mut aliases = None;
    let local_only = matches!(
        topic,
        LocalHelpTopic::Status
            | LocalHelpTopic::Sandbox
            | LocalHelpTopic::Doctor
            | LocalHelpTopic::Version
            | LocalHelpTopic::State
            | LocalHelpTopic::Init
            | LocalHelpTopic::Export
            | LocalHelpTopic::SystemPrompt
            | LocalHelpTopic::DumpManifests
            | LocalHelpTopic::BootstrapPlan
    );
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Usage") {
            let value = rest.trim();
            if !value.is_empty() {
                usage = Some(value.to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("Purpose") {
            purpose = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("Output") {
            output_desc = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("Aliases") {
            let parts: Vec<String> = rest
                .split('·')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !parts.is_empty() {
                aliases = Some(parts);
            }
        } else if let Some(rest) = trimmed.strip_prefix("Related") {
            let parts: Vec<String> = rest
                .split('·')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !parts.is_empty() {
                related = Some(parts);
            }
        }
    }
    (
        usage,
        purpose,
        output_desc,
        formats,
        related,
        aliases,
        local_only,
        !local_only,
    )
}

fn render_help_topic_json(topic: LocalHelpTopic) -> serde_json::Value {
    if topic == LocalHelpTopic::Export {
        return render_export_help_json();
    }
    if topic == LocalHelpTopic::Doctor {
        return render_doctor_help_json();
    }

    // #683-#692: extract structured metadata from help prose for machine consumption
    let (usage, purpose, output_desc, formats, related, aliases, local_only, requires_credentials) =
        extract_help_metadata(topic);
    let mut obj = serde_json::json!({
        "kind": "help",
        "action": "help",
        "status": "ok",
        "topic": local_help_topic_command(topic),
        "command": local_help_topic_command(topic),
        "message": render_help_topic(topic),
        "usage": usage,
        "purpose": purpose,
        "formats": formats,
        "related": related,
        "local_only": local_only,
        "requires_credentials": requires_credentials,
    });
    if let Some(desc) = output_desc {
        obj["output_fields"] = serde_json::Value::String(desc);
    }
    if let Some(a) = aliases {
        obj["aliases"] = serde_json::json!(a);
    }
    obj
}

fn print_help_topic(
    topic: LocalHelpTopic,
    output_format: CliOutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir().unwrap_or_default();
    // For subsystem topics in JSON mode, delegate to the subsystem's usage JSON.
    if output_format == CliOutputFormat::Json {
        match topic {
            LocalHelpTopic::Agents => {
                let json = commands::handle_agents_slash_command_json(Some("--help"), &cwd)
                    .unwrap_or_else(
                        |_| serde_json::json!({"kind":"agents","action":"help","status":"error"}),
                    );
                println!("{}", serde_json::to_string_pretty(&json)?);
                return Ok(());
            }
            LocalHelpTopic::Skills => {
                let json = commands::handle_skills_slash_command_json(Some("--help"), &cwd)
                    .unwrap_or_else(
                        |_| serde_json::json!({"kind":"skills","action":"help","status":"error"}),
                    );
                println!("{}", serde_json::to_string_pretty(&json)?);
                return Ok(());
            }
            _ => {}
        }
    }
    match output_format {
        CliOutputFormat::Text => println!("{}", render_help_topic(topic)),
        CliOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&render_help_topic_json(topic))?
        ),
    }
    Ok(())
}

fn acp_status_message() -> &'static str {
    "ACP/Zed editor integration is not implemented in claw-code yet. `claw acp serve` reports status only and does not launch a daemon or JSON-RPC endpoint. Use the normal terminal surfaces for now."
}

fn acp_status_json() -> serde_json::Value {
    json!({
        "schema_version": "1.0",
        "kind": "acp",
        "action": "status",
        "status": "not_implemented",
        "supported": false,
        "message": acp_status_message(),
        "launch_command": serde_json::Value::Null,
        "protocol": {
            "name": "ACP/Zed",
            "json_rpc": false,
            "daemon": false,
            "endpoint": serde_json::Value::Null,
            "serve_starts_daemon": false
        },
        "contracts": {
            "blocking_gates": [
                "task_packet_schema",
                "session_control_schema",
                "event_report_schema"
            ],
            "stable_status_surface": "claw acp [serve] --output-format json",
            "unsupported_invocation_kind": "unsupported_acp_invocation"
        },
        "aliases": ["acp", "--acp", "-acp"],
    })
}

fn print_acp_status(output_format: CliOutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    match output_format {
        CliOutputFormat::Text => {
            println!(
                "ACP / Zed\n  Status           not implemented\n  Launch           `claw acp serve` reports status only; no editor daemon or JSON-RPC endpoint is available yet\n  Today            use `claw prompt`, the REPL, or `claw doctor` for local verification\n  Message          {}",
                acp_status_message()
            );
        }
        CliOutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&acp_status_json())?);
        }
    }
    Ok(())
}

fn render_config_report(section: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let discovered = loader.discover();
    let runtime_config = loader.load()?;

    let mut lines = vec![
        format!(
            "Config
  Working directory {}
  Loaded files      {}
  Merged keys       {}",
            cwd.display(),
            runtime_config.loaded_entries().len(),
            runtime_config.merged().len()
        ),
        "Discovered files".to_string(),
    ];
    for entry in discovered {
        let source = match entry.source {
            ConfigSource::User => "user",
            ConfigSource::Project => "project",
            ConfigSource::Local => "local",
        };
        let status = if runtime_config
            .loaded_entries()
            .iter()
            .any(|loaded_entry| loaded_entry.path == entry.path)
        {
            "loaded"
        } else {
            "missing"
        };
        lines.push(format!(
            "  {source:<7} {status:<7} {}",
            entry.path.display()
        ));
    }

    if let Some(section) = section {
        lines.push(format!("Merged section: {section}"));
        let rendered = match section {
            "env" => runtime_config.get("env").map(|value| value.render()),
            "hooks" => runtime_config.get("hooks").map(|value| value.render()),
            "model" => runtime_config.get("model").map(|value| value.render()),
            "plugins" => runtime_config
                .get("plugins")
                .or_else(|| runtime_config.get("enabledPlugins"))
                .map(|value| value.render()),
            "mcp" | "mcp_servers" | "mcpServers" => runtime_config
                .get("mcp")
                .or_else(|| runtime_config.get("mcp_servers"))
                .or_else(|| runtime_config.get("mcpServers"))
                .map(|value| value.render()),
            "sandbox" => runtime_config.get("sandbox").map(|value| value.render()),
            "permissions" => runtime_config
                .get("permissions")
                .map(|value| value.render()),
            "skills" => runtime_config.get("skills").map(|value| value.render()),
            "agents" => runtime_config.get("agents").map(|value| value.render()),
            "settings" => Some(runtime_config.as_json().render()),
            // #344: /config help shows available sections
            "help" => {
                lines.push("Available config sections:".to_string());
                lines.push("  env          Environment variables".to_string());
                lines.push("  hooks        Hook configuration".to_string());
                lines.push("  model        Model configuration".to_string());
                lines.push("  plugins      Plugin configuration".to_string());
                lines.push("  mcp          MCP server configuration".to_string());
                lines.push("  sandbox      Sandbox configuration".to_string());
                lines.push("  permissions  Permission rules".to_string());
                lines.push("  skills       Skills configuration".to_string());
                lines.push("  agents       Agent configuration".to_string());
                lines.push("  settings     Full merged settings".to_string());
                lines.push(format!("  Loaded keys: {}", runtime_config.merged().len()));
                return Ok(lines.join("\n"));
            }
            other => {
                lines.push(format!(
                    "  Unsupported config section '{other}'. Use: env, hooks, model, plugins, mcp, sandbox, permissions, skills, agents, or settings."
                ));
                return Ok(lines.join(
                    "
",
                ));
            }
        };
        lines.push(format!(
            "  {}",
            rendered.unwrap_or_else(|| "<unset>".to_string())
        ));
        return Ok(lines.join(
            "
",
        ));
    }

    lines.push("Merged JSON".to_string());
    lines.push(format!("  {}", runtime_config.as_json().render()));
    Ok(lines.join(
        "
",
    ))
}

fn render_config_json(
    section: Option<&str>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    // #773: keep deprecation warnings in the JSON envelope, and #407: include
    // per-file status/reason/detail for every discovered config path.
    let inspection = loader.inspect_collecting_warnings();
    if section.is_some() {
        if let Some(error) = &inspection.load_error {
            return Err(error.clone().into());
        }
    }
    let runtime_config = inspection
        .runtime_config
        .clone()
        .unwrap_or_else(runtime::RuntimeConfig::empty);
    let loaded_files = runtime_config.loaded_entries().len();
    let merged_keys = runtime_config.merged().len();
    // #415: expose actual merged key-value pairs, not just count
    let merged_json_str = serde_json::json!(runtime_config
        .merged()
        .iter()
        .map(|(k, v)| { (k.clone(), serde_json::Value::String(v.render())) })
        .collect::<serde_json::Map<String, serde_json::Value>>());
    let files: Vec<_> = inspection
        .files
        .iter()
        .map(config_file_report_json)
        .collect();

    let warnings_json: Vec<serde_json::Value> = inspection
        .warnings
        .iter()
        .map(|w| serde_json::Value::String(w.clone()))
        .collect();

    let hook_validation = HookValidationSummary::from_config(&runtime_config);
    let has_hook_issues = hook_validation.has_invalid_hooks();
    let status_value = if inspection.load_error.is_some() {
        "error"
    } else if has_hook_issues {
        "degraded"
    } else {
        "ok"
    };
    let base = serde_json::json!({
        "kind": "config",
        "action": if section.is_some() { "show" } else { "list" },
        "status": status_value,
        "cwd": cwd.display().to_string(),
        "loaded_files": loaded_files,
        "merged_keys": merged_keys,
        "merged_key_count": merged_keys,
        "merged": merged_json_str,
        "merged_keys_meaning": "count of top-level keys in the effective merged JSON object",

        "files": files,
        "warnings": warnings_json,
        "load_error": inspection.load_error.clone(),
        "hook_validation": hook_validation.json_value(),
    });

    if let Some(section) = section {
        let section_rendered: Option<String> = match section {
            "env" => runtime_config.get("env").map(|v| v.render()),
            "hooks" => runtime_config.get("hooks").map(|v| v.render()),
            "model" => runtime_config.get("model").map(|v| v.render()),
            "plugins" => runtime_config
                .get("plugins")
                .or_else(|| runtime_config.get("enabledPlugins"))
                .map(|v| v.render()),
            // These sections are structurally present in config files but may not have
            // dedicated runtime_config keys yet; return null section_value rather than error.
            "mcp" | "mcp_servers" | "mcpServers" => runtime_config
                .get("mcp")
                .or_else(|| runtime_config.get("mcp_servers"))
                .or_else(|| runtime_config.get("mcpServers"))
                .map(|v| v.render()),
            "sandbox" => runtime_config.get("sandbox").map(|v| v.render()),
            "permissions" => runtime_config.get("permissions").map(|v| v.render()),
            "skills" => runtime_config.get("skills").map(|v| v.render()),
            "agents" => runtime_config.get("agents").map(|v| v.render()),
            "settings" => Some(runtime_config.as_json().render()),
            // #344: /config help returns structured section list
            "help" => {
                return Ok(serde_json::json!({
                    "kind": "config",
                    "action": "help",
                    "status": "ok",
                    "section": "help",
                    "available_sections": ["env", "hooks", "model", "plugins", "mcp", "sandbox", "permissions", "skills", "agents", "settings"],
                    "loaded_keys": runtime_config.merged().len(),
                }));
            }
            other => {
                // #741: populate hint field for unsupported section errors so callers reading
                // .hint get actionable guidance instead of null
                let hint = if matches!(other, "list" | "show" | "info") {
                    format!(
                        "'claw config {other}' is not a subcommand. To list all config: `claw config`. To inspect a section: `claw config <section>` where section is one of: env, hooks, model, plugins, mcp, sandbox, permissions, skills, agents, settings."
                    )
                } else {
                    format!(
                        "'{other}' is not a config section. Supported: env, hooks, model, plugins, mcp, sandbox, permissions, skills, agents, settings."
                    )
                };
                return Ok(serde_json::json!({
                    "kind": "config",
                    "action": "show",
                    "status": "error",
                    "error_kind": "unsupported_config_section",
                    "section": other,
                    "ok": false,
                    "error": format!("Unsupported config section '{other}'. Use: env, hooks, model, plugins, mcp, sandbox, permissions, skills, agents, or settings."),
                    "hint": hint,
                    "supported_sections": ["env", "hooks", "model", "plugins", "mcp", "sandbox", "permissions", "skills", "agents", "settings"],
                    "cwd": cwd.display().to_string(),
                    "loaded_files": loaded_files,
                    "files": base["files"].clone(),
                }));
            }
        };
        // Parse the rendered JSON string back into serde_json::Value so that
        // section_value is a real JSON object/array in the envelope, not a quoted string.
        let section_value: serde_json::Value = section_rendered
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let mut obj = base;
        let map = obj.as_object_mut().expect("base is object");
        map.insert(
            "section".to_string(),
            serde_json::Value::String(section.to_string()),
        );
        map.insert("section_value".to_string(), section_value);
        return Ok(obj);
    }

    Ok(base)
}

fn config_file_report_json(file: &ConfigFileReport) -> serde_json::Value {
    let source = match file.entry.source {
        ConfigSource::User => "user",
        ConfigSource::Project => "project",
        ConfigSource::Local => "local",
    };
    let mut object = serde_json::Map::new();
    object.insert(
        "path".to_string(),
        serde_json::Value::String(file.entry.path.display().to_string()),
    );
    object.insert(
        "source".to_string(),
        serde_json::Value::String(source.to_string()),
    );
    object.insert("loaded".to_string(), serde_json::Value::Bool(file.loaded));
    object.insert(
        "precedence_rank".to_string(),
        serde_json::Value::Number(serde_json::Number::from(file.precedence_rank)),
    );
    object.insert(
        "wins_for_keys".to_string(),
        serde_json::Value::Array(
            file.wins_for_keys
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    object.insert(
        "shadowed_keys".to_string(),
        serde_json::Value::Array(
            file.shadowed_keys
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    object.insert(
        "status".to_string(),
        serde_json::Value::String(file.status.as_str().to_string()),
    );
    if let Some(reason) = &file.reason {
        object.insert(
            "reason".to_string(),
            serde_json::Value::String(reason.clone()),
        );
        object.insert(
            "skip_reason".to_string(),
            serde_json::Value::String(reason.clone()),
        );
    }
    if let Some(detail) = &file.detail {
        object.insert(
            "detail".to_string(),
            serde_json::Value::String(detail.clone()),
        );
    }
    serde_json::Value::Object(object)
}

fn render_memory_report() -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let project_context = ProjectContext::discover(&cwd, DEFAULT_DATE)?;
    let mut lines = vec![format!(
        "Memory
  Working directory {}
  Instruction files {}",
        cwd.display(),
        project_context.instruction_files.len()
    )];
    if project_context.instruction_files.is_empty() {
        lines.push("Discovered files".to_string());
        lines.push(
            "  No CLAUDE.md, CLAW.md, AGENTS.md, or scoped instruction files discovered in the current directory ancestry."
                .to_string(),
        );
    } else {
        lines.push("Discovered files".to_string());
        for (index, file) in project_context.instruction_files.iter().enumerate() {
            let preview = file.content.lines().next().unwrap_or("").trim();
            let preview = if preview.is_empty() {
                "<empty>"
            } else {
                preview
            };
            lines.push(format!("  {}. {}", index + 1, file.path.display(),));
            lines.push(format!(
                "     source={} lines={} chars={} preview={}",
                file.source(),
                file.content.lines().count(),
                file.char_count(),
                preview
            ));
        }
    }
    Ok(lines.join(
        "
",
    ))
}

fn render_memory_json() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let project_context = ProjectContext::discover(&cwd, DEFAULT_DATE)?;
    let files: Vec<_> = project_context
        .instruction_files
        .iter()
        .map(|f| {
            json!({
                "path": f.path.display().to_string(),
                "lines": f.content.lines().count(),
                "preview": f.content.lines().next().unwrap_or("").trim(),
            })
        })
        .collect();
    Ok(json!({
        "kind": "memory",
        "action": "list",
        "status": "ok",
        "cwd": cwd.display().to_string(),
        "instruction_files": files.len(),
        "files": files,
    }))
}

fn init_claude_md() -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    Ok(initialize_repo(&cwd)?.render())
}

fn run_init(output_format: CliOutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let report = initialize_repo(&cwd)?;
    let message = report.render();
    match output_format {
        CliOutputFormat::Text => println!("{message}"),
        CliOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&init_json_value(&report, &message))?
        ),
    }
    Ok(())
}

/// #142: emit first-class structured fields alongside the legacy `message`
/// string so claws can detect per-artifact state without substring matching.
fn init_json_value(report: &crate::init::InitReport, message: &str) -> serde_json::Value {
    use crate::init::InitStatus;
    // Derive top-level status: "ok" when all artifacts succeeded (created or
    // skipped = idempotent); no failure path exists today so always "ok".
    let status = "ok";
    // #783/#436: already_initialized lets orchestrators detect the idempotent
    // case without checking every status bucket; deferred session storage does
    // not make the workspace uninitialized because it is created on first save.
    let already_initialized = report.artifacts_with_status(InitStatus::Created).is_empty()
        && report.artifacts_with_status(InitStatus::Updated).is_empty()
        && report.artifacts_with_status(InitStatus::Partial).is_empty();
    let hint = if already_initialized {
        "Workspace already initialised. Run `claw doctor` to verify health, or edit CLAUDE.md to customise guidance."
    } else {
        "Review and tailor CLAUDE.md to your project, then run `claw doctor` to verify the workspace."
    };
    json!({
        "kind": "init",
        "action": "init",
        "status": status,
        "already_initialized": already_initialized,
        "project_path": report.project_root.display().to_string(),
        "created": report.artifacts_with_status(InitStatus::Created),
        "updated": report.artifacts_with_status(InitStatus::Updated),
        "skipped": report.artifacts_with_status(InitStatus::Skipped),
        "partial": report.artifacts_with_status(InitStatus::Partial),
        "deferred": report.artifacts_with_status(InitStatus::Deferred),
        "artifacts": report.artifact_json_entries(),
        "hint": hint,
        "next_step": crate::init::InitReport::NEXT_STEP,
        "message": message,
    })
}

fn normalize_permission_mode(mode: &str) -> Option<&'static str> {
    match mode.trim() {
        "default" | "plan" | "read-only" => Some("read-only"),
        "acceptEdits" | "auto" | "workspace-write" => Some("workspace-write"),
        "dontAsk" | "bypassPermissions" | "dangerFullAccess" | "danger-full-access" => {
            Some("danger-full-access")
        }
        _ => None,
    }
}

fn render_diff_report() -> Result<String, Box<dyn std::error::Error>> {
    render_diff_report_for(&env::current_dir()?)
}

fn render_diff_report_for(cwd: &Path) -> Result<String, Box<dyn std::error::Error>> {
    // Verify we are inside a git repository before calling `git diff`.
    // Running `git diff --cached` outside a git tree produces a misleading
    // "unknown option `cached`" error because git falls back to --no-index mode.
    let in_git_repo = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .is_ok_and(|o| o.status.success());
    if !in_git_repo {
        return Ok(format!(
            "Diff\n  Result           no git repository\n  Detail           {} is not inside a git project",
            cwd.display()
        ));
    }
    let staged = run_git_diff_command_in(cwd, &["diff", "--cached"])?;
    let unstaged = run_git_diff_command_in(cwd, &["diff"])?;
    if staged.trim().is_empty() && unstaged.trim().is_empty() {
        return Ok(
            "Diff\n  Result           clean working tree\n  Detail           no current changes"
                .to_string(),
        );
    }

    let mut sections = Vec::new();
    if !staged.trim().is_empty() {
        sections.push(format!("Staged changes:\n{}", staged.trim_end()));
    }
    if !unstaged.trim().is_empty() {
        sections.push(format!("Unstaged changes:\n{}", unstaged.trim_end()));
    }

    Ok(format!("Diff\n\n{}", sections.join("\n\n")))
}

fn render_diff_json_for(cwd: &Path) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let in_git_repo = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .is_ok_and(|o| o.status.success());
    if !in_git_repo {
        // #801: add error_kind, hint, message fields for envelope parity with other error paths
        return Ok(serde_json::json!({
            "kind": "diff",
            "action": "diff",
            "status": "error",
            "error_kind": "no_git_repo",
            "result": "no_git_repo",
            "message": format!("{} is not inside a git project", cwd.display()),
            "hint": "Run `git init` to create a repository, or change to a directory that is inside a git project.",
            "working_directory": cwd.display().to_string(),
            "detail": format!("{} is not inside a git project", cwd.display()),
        }));
    }
    let staged = run_git_diff_command_in(cwd, &["diff", "--cached"])?;
    let unstaged = run_git_diff_command_in(cwd, &["diff"])?;
    // #733: add changed_file_count so callers don't have to count diff hunks
    let staged_files =
        run_git_diff_command_in(cwd, &["diff", "--cached", "--name-only"]).unwrap_or_default();
    let unstaged_files = run_git_diff_command_in(cwd, &["diff", "--name-only"]).unwrap_or_default();
    let mut changed: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for line in staged_files.lines().chain(unstaged_files.lines()) {
        let t = line.trim();
        if !t.is_empty() {
            changed.insert(t);
        }
    }
    let changed_file_count = changed.len();
    Ok(serde_json::json!({
        "kind": "diff",
        "action": "diff",
        "status": "ok",
        "working_directory": cwd.display().to_string(),
        "result": if staged.trim().is_empty() && unstaged.trim().is_empty() { "clean" } else { "changes" },
        "changed_file_count": changed_file_count,
        "staged": staged.trim(),
        "unstaged": unstaged.trim(),
    }))
}

fn run_git_diff_command_in(
    cwd: &Path,
    args: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git {} failed: {stderr}", args.join(" ")).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn render_teleport_report(target: &str) -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;

    let file_list = Command::new("rg")
        .args(["--files"])
        .current_dir(&cwd)
        .output()?;
    let file_matches = if file_list.status.success() {
        String::from_utf8(file_list.stdout)?
            .lines()
            .filter(|line| line.contains(target))
            .take(10)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let content_output = Command::new("rg")
        .args(["-n", "-S", "--color", "never", target, "."])
        .current_dir(&cwd)
        .output()?;

    let mut lines = vec![
        "Teleport".to_string(),
        format!("  Target           {target}"),
        "  Action           search workspace files and content for the target".to_string(),
    ];
    if !file_matches.is_empty() {
        lines.push(String::new());
        lines.push("File matches".to_string());
        lines.extend(file_matches.into_iter().map(|path| format!("  {path}")));
    }

    if content_output.status.success() {
        let matches = String::from_utf8(content_output.stdout)?;
        if !matches.trim().is_empty() {
            lines.push(String::new());
            lines.push("Content matches".to_string());
            lines.push(truncate_for_prompt(&matches, 4_000));
        }
    }

    if lines.len() == 1 {
        lines.push("  Result           no matches found".to_string());
    }

    Ok(lines.join("\n"))
}

fn render_last_tool_debug_report(session: &Session) -> Result<String, Box<dyn std::error::Error>> {
    let last_tool_use = session
        .messages
        .iter()
        .rev()
        .find_map(|message| {
            message.blocks.iter().rev().find_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
        })
        .ok_or_else(|| "no prior tool call found in session".to_string())?;

    let tool_result = session.messages.iter().rev().find_map(|message| {
        message.blocks.iter().rev().find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } if tool_use_id == &last_tool_use.0 => {
                Some((tool_name.clone(), output.clone(), *is_error))
            }
            _ => None,
        })
    });

    let mut lines = vec![
        "Debug tool call".to_string(),
        "  Action           inspect the last recorded tool call and its result".to_string(),
        format!("  Tool id          {}", last_tool_use.0),
        format!("  Tool name        {}", last_tool_use.1),
        "  Input".to_string(),
        indent_block(&last_tool_use.2, 4),
    ];

    match tool_result {
        Some((tool_name, output, is_error)) => {
            lines.push("  Result".to_string());
            lines.push(format!("    name           {tool_name}"));
            lines.push(format!(
                "    status         {}",
                if is_error { "error" } else { "ok" }
            ));
            lines.push(indent_block(&output, 4));
        }
        None => lines.push("  Result           missing tool result".to_string()),
    }

    Ok(lines.join("\n"))
}

fn indent_block(value: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    value
        .lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_no_args(
    command_name: &str,
    args: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(args) = args.map(str::trim).filter(|value| !value.is_empty()) {
        return Err(format!(
            "{command_name} does not accept arguments. Received: {args}\nUsage: {command_name}"
        )
        .into());
    }
    Ok(())
}

fn format_bughunter_report(scope: Option<&str>) -> String {
    format!(
        "Bughunter
  Scope            {}
  Action           inspect the selected code for likely bugs and correctness issues
  Output           findings should include file paths, severity, and suggested fixes",
        scope.unwrap_or("the current repository")
    )
}

fn format_ultraplan_report(task: Option<&str>) -> String {
    format!(
        "Ultraplan
  Task             {}
  Action           break work into a multi-step execution plan
  Output           plan should cover goals, risks, sequencing, verification, and rollback",
        task.unwrap_or("the current repo work")
    )
}

fn format_pr_report(branch: &str, context: Option<&str>) -> String {
    format!(
        "PR
  Branch           {branch}
  Context          {}
  Action           draft or create a pull request for the current branch
  Output           title and markdown body suitable for GitHub",
        context.unwrap_or("none")
    )
}

fn format_issue_report(context: Option<&str>) -> String {
    format!(
        "Issue
  Context          {}
  Action           draft or create a GitHub issue from the current context
  Output           title and markdown body suitable for GitHub",
        context.unwrap_or("none")
    )
}

fn git_output(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git {} failed: {stderr}", args.join(" ")).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn git_status_ok(args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env::current_dir()?)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git {} failed: {stderr}", args.join(" ")).into());
    }
    Ok(())
}

fn command_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn write_temp_text_file(
    filename: &str,
    contents: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = env::temp_dir().join(filename);
    fs::write(&path, contents)?;
    Ok(path)
}

const DEFAULT_HISTORY_LIMIT: usize = 20;

fn parse_history_count(raw: Option<&str>) -> Result<usize, String> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_HISTORY_LIMIT);
    };
    // #776: use \n-delimited format so split_error_hint extracts hint into JSON envelopes
    let parsed: usize = raw
        .parse()
        .map_err(|_| format!("invalid_history_count: '{raw}' is not a positive integer.\nUsage: /history [count] (default: {DEFAULT_HISTORY_LIMIT})"))?;
    if parsed == 0 {
        return Err(format!("invalid_history_count: count must be greater than 0.\nUsage: /history [count] (default: {DEFAULT_HISTORY_LIMIT})"));
    }
    Ok(parsed)
}

fn format_history_timestamp(timestamp_ms: u64) -> String {
    let secs = timestamp_ms / 1_000;
    let subsec_ms = timestamp_ms % 1_000;
    let days_since_epoch = secs / 86_400;
    let seconds_of_day = secs % 86_400;
    let hours = seconds_of_day / 3_600;
    let minutes = (seconds_of_day % 3_600) / 60;
    let seconds = seconds_of_day % 60;

    let (year, month, day) = civil_from_days(i64::try_from(days_since_epoch).unwrap_or(0));
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{subsec_ms:03}Z")
}

// Computes civil (Gregorian) year/month/day from days since the Unix epoch
// (1970-01-01) using Howard Hinnant's `civil_from_days` algorithm.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation
)]
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u64; // [0, 146_096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = y + i64::from(m <= 2);
    (y as i32, m as u32, d as u32)
}

fn render_prompt_history_report(entries: &[PromptHistoryEntry], limit: usize) -> String {
    if entries.is_empty() {
        return "Prompt history\n  Result           no prompts recorded yet".to_string();
    }

    let total = entries.len();
    let start = total.saturating_sub(limit);
    let shown = &entries[start..];
    let mut lines = vec![
        "Prompt history".to_string(),
        format!("  Total            {total}"),
        format!("  Showing          {} most recent", shown.len()),
        format!("  Reverse search   Ctrl-R in the REPL"),
        String::new(),
    ];
    for (offset, entry) in shown.iter().enumerate() {
        let absolute_index = start + offset + 1;
        let timestamp = format_history_timestamp(entry.timestamp_ms);
        let first_line = entry.text.lines().next().unwrap_or("").trim();
        let display = if first_line.chars().count() > 80 {
            let truncated: String = first_line.chars().take(77).collect();
            format!("{truncated}...")
        } else {
            first_line.to_string()
        };
        lines.push(format!("  {absolute_index:>3}. [{timestamp}] {display}"));
    }
    lines.join("\n")
}

fn collect_session_prompt_history(session: &Session) -> Vec<PromptHistoryEntry> {
    if !session.prompt_history.is_empty() {
        return session
            .prompt_history
            .iter()
            .map(|entry| PromptHistoryEntry {
                timestamp_ms: entry.timestamp_ms,
                text: entry.text.clone(),
            })
            .collect();
    }
    let timestamp_ms = session.updated_at_ms;
    session
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .filter_map(|message| {
            message.blocks.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(PromptHistoryEntry {
                    timestamp_ms,
                    text: text.clone(),
                }),
                _ => None,
            })
        })
        .collect()
}

fn recent_user_context(session: &Session, limit: usize) -> String {
    let requests = session
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .filter_map(|message| {
            message.blocks.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.trim().to_string()),
                _ => None,
            })
        })
        .rev()
        .take(limit)
        .collect::<Vec<_>>();

    if requests.is_empty() {
        "<no prior user messages>".to_string()
    } else {
        requests
            .into_iter()
            .rev()
            .enumerate()
            .map(|(index, text)| format!("{}. {}", index + 1, text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn truncate_for_prompt(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.trim().to_string()
    } else {
        let truncated = value.chars().take(limit).collect::<String>();
        format!("{}\n…[truncated]", truncated.trim_end())
    }
}

fn sanitize_generated_message(value: &str) -> String {
    value.trim().trim_matches('`').trim().replace("\r\n", "\n")
}

fn parse_titled_body(value: &str) -> Option<(String, String)> {
    let normalized = sanitize_generated_message(value);
    let title = normalized
        .lines()
        .find_map(|line| line.strip_prefix("TITLE:").map(str::trim))?;
    let body_start = normalized.find("BODY:")?;
    let body = normalized[body_start + "BODY:".len()..].trim();
    Some((title.to_string(), body.to_string()))
}

fn render_version_report() -> String {
    let git_sha = GIT_SHA_SHORT.or(GIT_SHA).unwrap_or("unknown");
    let target = BUILD_TARGET.unwrap_or("unknown");
    let branch = GIT_BRANCH.unwrap_or("unknown");
    let dirty = GIT_DIRTY.unwrap_or("unknown");
    format!(
        "Claw Code\n  Version          {VERSION}\n  Git SHA          {git_sha}\n  Branch           {branch}\n  Dirty            {dirty}\n  Target           {target}\n  Build date       {DEFAULT_DATE}"
    )
}

fn render_export_text(session: &Session) -> String {
    let mut lines = vec!["# Conversation Export".to_string(), String::new()];
    for (index, message) in session.messages.iter().enumerate() {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        lines.push(format!("## {}. {role}", index + 1));
        for block in &message.blocks {
            match block {
                ContentBlock::Text { text } => lines.push(text.clone()),
                ContentBlock::Thinking { .. } => {}
                ContentBlock::ToolUse { id, name, input } => {
                    lines.push(format!("[tool_use id={id} name={name}] {input}"));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    tool_name,
                    output,
                    is_error,
                } => {
                    lines.push(format!(
                        "[tool_result id={tool_use_id} name={tool_name} error={is_error}] {output}"
                    ));
                }
            }
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

fn default_export_filename(session: &Session) -> String {
    let stem = session
        .messages
        .iter()
        .find_map(|message| match message.role {
            MessageRole::User => message.blocks.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            }),
            _ => None,
        })
        .map_or("conversation", |text| {
            text.lines().next().unwrap_or("conversation")
        })
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    let fallback = if stem.is_empty() {
        "conversation"
    } else {
        &stem
    };
    format!("{fallback}.txt")
}

fn resolve_export_path(
    requested_path: Option<&str>,
    session: &Session,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let file_name =
        requested_path.map_or_else(|| default_export_filename(session), ToOwned::to_owned);
    let final_name = if Path::new(&file_name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
    {
        file_name
    } else {
        format!("{file_name}.txt")
    };
    Ok(cwd.join(final_name))
}

fn validate_export_output_path(path: Option<&Path>) -> Result<(), InvalidOutputPathError> {
    let Some(path) = path else {
        return Ok(());
    };
    let raw = path.to_string_lossy();
    if raw.trim().is_empty() {
        return Err(InvalidOutputPathError::new(
            raw.to_string(),
            InvalidOutputPathReason::Empty,
        ));
    }
    if matches!(fs::metadata(path), Ok(metadata) if metadata.is_dir()) {
        return Err(InvalidOutputPathError::new(
            raw.to_string(),
            InvalidOutputPathReason::PathIsDirectory,
        ));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        match fs::metadata(parent) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(InvalidOutputPathError::new(
                    raw.to_string(),
                    InvalidOutputPathReason::ParentNotADirectory,
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(InvalidOutputPathError::new(
                    raw.to_string(),
                    InvalidOutputPathReason::ParentNotFound,
                ));
            }
            Err(_) => {
                return Err(InvalidOutputPathError::new(
                    raw.to_string(),
                    InvalidOutputPathReason::ParentNotFound,
                ));
            }
        }
    }
    Ok(())
}

const SESSION_MARKDOWN_TOOL_SUMMARY_LIMIT: usize = 280;

fn summarize_tool_payload_for_markdown(payload: &str) -> String {
    let compact = match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => value.to_string(),
        Err(_) => payload.split_whitespace().collect::<Vec<_>>().join(" "),
    };
    if compact.is_empty() {
        return String::new();
    }
    truncate_for_summary(&compact, SESSION_MARKDOWN_TOOL_SUMMARY_LIMIT)
}

fn run_export(
    session_reference: &str,
    output_path: Option<&Path>,
    output_format: CliOutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_export_output_path(output_path)?;
    let (handle, session) = load_session_reference(session_reference)?;
    let markdown = render_session_markdown(&session, &handle.id, &handle.path);

    if let Some(path) = output_path {
        fs::write(path, &markdown)?;
        let report = format!(
            "Export\n  Result           wrote markdown transcript\n  File             {}\n  Session          {}\n  Messages         {}",
            path.display(),
            handle.id,
            session.messages.len(),
        );
        match output_format {
            CliOutputFormat::Text => println!("{report}"),
            CliOutputFormat::Json => println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "kind": "export",
                    "action": "export",
                    "status": "ok",
                    "message": report,
                    "session_id": handle.id,
                    "file": path.display().to_string(),
                    "messages": session.messages.len(),
                }))?
            ),
        }
        return Ok(());
    }

    match output_format {
        CliOutputFormat::Text => {
            print!("{markdown}");
            if !markdown.ends_with('\n') {
                println!();
            }
        }
        CliOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "kind": "export",
                "action": "export",
                "status": "ok",
                "session_id": handle.id,
                "file": handle.path.display().to_string(),
                "messages": session.messages.len(),
                "markdown": markdown,
            }))?
        ),
    }
    Ok(())
}

fn render_session_markdown(session: &Session, session_id: &str, session_path: &Path) -> String {
    let mut lines = vec![
        "# Conversation Export".to_string(),
        String::new(),
        format!("- **Session**: `{session_id}`"),
        format!("- **File**: `{}`", session_path.display()),
        format!("- **Messages**: {}", session.messages.len()),
    ];
    if let Some(workspace_root) = session.workspace_root() {
        lines.push(format!("- **Workspace**: `{}`", workspace_root.display()));
    }
    if let Some(fork) = &session.fork {
        let branch = fork.branch_name.as_deref().unwrap_or("(unnamed)");
        lines.push(format!(
            "- **Forked from**: `{}` (branch `{branch}`)",
            fork.parent_session_id
        ));
    }
    if let Some(compaction) = &session.compaction {
        lines.push(format!(
            "- **Compactions**: {} (last removed {} messages)",
            compaction.count, compaction.removed_message_count
        ));
    }
    lines.push(String::new());
    lines.push("---".to_string());
    lines.push(String::new());

    for (index, message) in session.messages.iter().enumerate() {
        let role = match message.role {
            MessageRole::System => "System",
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::Tool => "Tool",
        };
        lines.push(format!("## {}. {role}", index + 1));
        lines.push(String::new());
        for block in &message.blocks {
            match block {
                ContentBlock::Text { text } => {
                    let trimmed = text.trim_end();
                    if !trimmed.is_empty() {
                        lines.push(trimmed.to_string());
                        lines.push(String::new());
                    }
                }
                ContentBlock::Thinking { .. } => {}
                ContentBlock::ToolUse { id, name, input } => {
                    lines.push(format!(
                        "**Tool call** `{name}` _(id `{}`)_",
                        short_tool_id(id)
                    ));
                    let summary = summarize_tool_payload_for_markdown(input);
                    if !summary.is_empty() {
                        lines.push(format!("> {summary}"));
                    }
                    lines.push(String::new());
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    tool_name,
                    output,
                    is_error,
                } => {
                    let status = if *is_error { "error" } else { "ok" };
                    lines.push(format!(
                        "**Tool result** `{tool_name}` _(id `{}`, {status})_",
                        short_tool_id(tool_use_id)
                    ));
                    let summary = summarize_tool_payload_for_markdown(output);
                    if !summary.is_empty() {
                        lines.push(format!("> {summary}"));
                    }
                    lines.push(String::new());
                }
            }
        }
        if let Some(usage) = message.usage {
            lines.push(format!(
                "_tokens: in={} out={} cache_create={} cache_read={}_",
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_creation_input_tokens,
                usage.cache_read_input_tokens,
            ));
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

fn short_tool_id(id: &str) -> String {
    let char_count = id.chars().count();
    if char_count <= 12 {
        return id.to_string();
    }
    let prefix: String = id.chars().take(12).collect();
    format!("{prefix}…")
}

fn build_system_prompt(model: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(load_system_prompt(
        env::current_dir()?,
        DEFAULT_DATE,
        env::consts::OS,
        "unknown",
        model_family_identity_for(model),
    )?)
}

struct PluginsCommandPayload {
    message: String,
    reload_runtime: bool,
    status: &'static str,
    config_load_error: Option<String>,
    mcp_validation: McpValidationSummary,
    plugins: Vec<Value>,
    load_failures: Vec<Value>,
}

fn plugins_command_payload_for(
    cwd: &Path,
    action: Option<&str>,
    target: Option<&str>,
    config_warning_mode: ConfigWarningMode,
) -> Result<PluginsCommandPayload, Box<dyn std::error::Error>> {
    let loader = ConfigLoader::default_for(cwd);
    let loaded_config = load_config_with_warning_mode(&loader, config_warning_mode);
    let (runtime_config, config_load_error, mcp_validation) = match loaded_config {
        Ok(runtime_config) => {
            let mcp_validation = McpValidationSummary::from_collection(runtime_config.mcp());
            (runtime_config, None, mcp_validation)
        }
        Err(error) => (
            runtime::RuntimeConfig::empty(),
            Some(error.to_string()),
            McpValidationSummary::default(),
        ),
    };
    let mut manager = build_plugin_manager(cwd, &loader, &runtime_config);
    let result = handle_plugins_slash_command(action, target, &mut manager)?;
    let report = manager.installed_plugin_registry_report()?;
    Ok(plugins_command_payload_from_result(
        result,
        config_load_error,
        mcp_validation,
        &report,
    ))
}

fn plugins_command_payload_from_result(
    result: PluginsCommandResult,
    config_load_error: Option<String>,
    mcp_validation: McpValidationSummary,
    report: &plugins::PluginRegistryReport,
) -> PluginsCommandPayload {
    let failures = report.failures();
    let status = if config_load_error.is_some()
        || mcp_validation.has_invalid_servers()
        || !failures.is_empty()
    {
        "degraded"
    } else {
        "ok"
    };
    let message = match config_load_error.as_deref() {
        Some(error) => format!(
            "Config load error\n  Status           fail\n  Summary          runtime config failed to load; reporting partial plugins view\n  Details          {error}\n  Hint             `claw doctor` classifies config parse errors; fix the listed field and rerun\n\n{}",
            result.message
        ),
        None if mcp_validation.has_invalid_servers() => format!(
            "MCP validation\n  Status           warn\n  Summary          {} MCP server entries are invalid; reporting plugins with valid MCP siblings only\n  Hint             Inspect `claw mcp list --output-format json` invalid_servers and fix each rejected mcpServers entry.\n\n{}",
            mcp_validation.invalid_count(),
            result.message
        ),
        None => result.message,
    };
    PluginsCommandPayload {
        message,
        reload_runtime: result.reload_runtime,
        status,
        config_load_error,
        mcp_validation,
        plugins: report.summaries().iter().map(plugin_summary_json).collect(),
        load_failures: failures.iter().map(plugin_load_failure_json).collect(),
    }
}

fn build_runtime_plugin_state() -> Result<RuntimePluginState, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let runtime_config = loader.load()?;
    build_runtime_plugin_state_with_loader(&cwd, &loader, &runtime_config)
}

fn build_runtime_plugin_state_with_loader(
    cwd: &Path,
    loader: &ConfigLoader,
    runtime_config: &runtime::RuntimeConfig,
) -> Result<RuntimePluginState, Box<dyn std::error::Error>> {
    let plugin_manager = build_plugin_manager(cwd, loader, runtime_config);
    let plugin_registry = plugin_manager.plugin_registry()?;
    let plugin_hook_config =
        runtime_hook_config_from_plugin_hooks(plugin_registry.aggregated_hooks()?);
    let feature_config = runtime_config
        .feature_config()
        .clone()
        .with_hooks(runtime_config.hooks().merged(&plugin_hook_config));
    let (mcp_state, runtime_tools) = build_runtime_mcp_state(runtime_config)?;
    let tool_registry = GlobalToolRegistry::with_plugin_tools(plugin_registry.aggregated_tools()?)?
        .with_runtime_tools(runtime_tools)?;
    Ok(RuntimePluginState {
        feature_config,
        tool_registry,
        plugin_registry,
        mcp_state,
    })
}

fn build_plugin_manager(
    cwd: &Path,
    loader: &ConfigLoader,
    runtime_config: &runtime::RuntimeConfig,
) -> PluginManager {
    let plugin_settings = runtime_config.plugins();
    let mut plugin_config = PluginManagerConfig::new(loader.config_home().to_path_buf());
    plugin_config.enabled_plugins = plugin_settings.enabled_plugins().clone();
    plugin_config.external_dirs = plugin_settings
        .external_directories()
        .iter()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path))
        .collect();
    plugin_config.install_root = plugin_settings
        .install_root()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    plugin_config.registry_path = plugin_settings
        .registry_path()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    plugin_config.bundled_root = plugin_settings
        .bundled_root()
        .map(|path| resolve_plugin_path(cwd, loader.config_home(), path));
    PluginManager::new(plugin_config)
}

fn resolve_plugin_path(cwd: &Path, config_home: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else if value.starts_with('.') {
        cwd.join(path)
    } else {
        config_home.join(path)
    }
}

fn runtime_hook_config_from_plugin_hooks(hooks: PluginHooks) -> runtime::RuntimeHookConfig {
    runtime::RuntimeHookConfig::new(
        hooks.pre_tool_use,
        hooks.post_tool_use,
        hooks.post_tool_use_failure,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InternalPromptProgressState {
    command_label: &'static str,
    task_label: String,
    step: usize,
    phase: String,
    detail: Option<String>,
    saw_final_text: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalPromptProgressEvent {
    Started,
    Update,
    Heartbeat,
    Complete,
    Failed,
}

#[derive(Debug)]
struct InternalPromptProgressShared {
    state: Mutex<InternalPromptProgressState>,
    output_lock: Mutex<()>,
    started_at: Instant,
}

#[derive(Debug, Clone)]
struct InternalPromptProgressReporter {
    shared: Arc<InternalPromptProgressShared>,
}

#[derive(Debug)]
struct InternalPromptProgressRun {
    reporter: InternalPromptProgressReporter,
    heartbeat_stop: Option<mpsc::Sender<()>>,
    heartbeat_handle: Option<thread::JoinHandle<()>>,
}

impl InternalPromptProgressReporter {
    fn ultraplan(task: &str) -> Self {
        Self {
            shared: Arc::new(InternalPromptProgressShared {
                state: Mutex::new(InternalPromptProgressState {
                    command_label: "Ultraplan",
                    task_label: task.to_string(),
                    step: 0,
                    phase: "planning started".to_string(),
                    detail: Some(format!("task: {task}")),
                    saw_final_text: false,
                }),
                output_lock: Mutex::new(()),
                started_at: Instant::now(),
            }),
        }
    }

    fn emit(&self, event: InternalPromptProgressEvent, error: Option<&str>) {
        let snapshot = self.snapshot();
        let line = format_internal_prompt_progress_line(event, &snapshot, self.elapsed(), error);
        self.write_line(&line);
    }

    fn mark_model_phase(&self) {
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            state.step += 1;
            state.phase = if state.step == 1 {
                "analyzing request".to_string()
            } else {
                "reviewing findings".to_string()
            };
            state.detail = Some(format!("task: {}", state.task_label));
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn mark_tool_phase(&self, name: &str, input: &str) {
        let detail = describe_tool_progress(name, input);
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            state.step += 1;
            state.phase = format!("running {name}");
            state.detail = Some(detail);
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn mark_text_phase(&self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let detail = truncate_for_summary(first_visible_line(trimmed), 120);
        let snapshot = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("internal prompt progress state poisoned");
            if state.saw_final_text {
                return;
            }
            state.saw_final_text = true;
            state.step += 1;
            state.phase = "drafting final plan".to_string();
            state.detail = (!detail.is_empty()).then_some(detail);
            state.clone()
        };
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Update,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn emit_heartbeat(&self) {
        let snapshot = self.snapshot();
        self.write_line(&format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Heartbeat,
            &snapshot,
            self.elapsed(),
            None,
        ));
    }

    fn snapshot(&self) -> InternalPromptProgressState {
        self.shared
            .state
            .lock()
            .expect("internal prompt progress state poisoned")
            .clone()
    }

    fn elapsed(&self) -> Duration {
        self.shared.started_at.elapsed()
    }

    fn write_line(&self, line: &str) {
        let _guard = self
            .shared
            .output_lock
            .lock()
            .expect("internal prompt progress output lock poisoned");
        let mut stdout = io::stdout();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

impl InternalPromptProgressRun {
    fn start_ultraplan(task: &str) -> Self {
        let reporter = InternalPromptProgressReporter::ultraplan(task);
        reporter.emit(InternalPromptProgressEvent::Started, None);

        let (heartbeat_stop, heartbeat_rx) = mpsc::channel();
        let heartbeat_reporter = reporter.clone();
        let heartbeat_handle = thread::spawn(move || loop {
            match heartbeat_rx.recv_timeout(INTERNAL_PROGRESS_HEARTBEAT_INTERVAL) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => heartbeat_reporter.emit_heartbeat(),
            }
        });

        Self {
            reporter,
            heartbeat_stop: Some(heartbeat_stop),
            heartbeat_handle: Some(heartbeat_handle),
        }
    }

    fn reporter(&self) -> InternalPromptProgressReporter {
        self.reporter.clone()
    }

    fn finish_success(&mut self) {
        self.stop_heartbeat();
        self.reporter
            .emit(InternalPromptProgressEvent::Complete, None);
    }

    fn finish_failure(&mut self, error: &str) {
        self.stop_heartbeat();
        self.reporter
            .emit(InternalPromptProgressEvent::Failed, Some(error));
    }

    fn stop_heartbeat(&mut self) {
        if let Some(sender) = self.heartbeat_stop.take() {
            let _ = sender.send(());
        }
        if let Some(handle) = self.heartbeat_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for InternalPromptProgressRun {
    fn drop(&mut self) {
        self.stop_heartbeat();
    }
}

fn format_internal_prompt_progress_line(
    event: InternalPromptProgressEvent,
    snapshot: &InternalPromptProgressState,
    elapsed: Duration,
    error: Option<&str>,
) -> String {
    let elapsed_seconds = elapsed.as_secs();
    let step_label = if snapshot.step == 0 {
        "current step pending".to_string()
    } else {
        format!("current step {}", snapshot.step)
    };
    let mut status_bits = vec![step_label, format!("phase {}", snapshot.phase)];
    if let Some(detail) = snapshot
        .detail
        .as_deref()
        .filter(|detail| !detail.is_empty())
    {
        status_bits.push(detail.to_string());
    }
    let status = status_bits.join(" · ");
    match event {
        InternalPromptProgressEvent::Started => {
            format!(
                "🧭 {} status · planning started · {status}",
                snapshot.command_label
            )
        }
        InternalPromptProgressEvent::Update => {
            format!("… {} status · {status}", snapshot.command_label)
        }
        InternalPromptProgressEvent::Heartbeat => format!(
            "… {} heartbeat · {elapsed_seconds}s elapsed · {status}",
            snapshot.command_label
        ),
        InternalPromptProgressEvent::Complete => format!(
            "✔ {} status · completed · {elapsed_seconds}s elapsed · {} steps total",
            snapshot.command_label, snapshot.step
        ),
        InternalPromptProgressEvent::Failed => format!(
            "✘ {} status · failed · {elapsed_seconds}s elapsed · {}",
            snapshot.command_label,
            error.unwrap_or("unknown error")
        ),
    }
}

fn describe_tool_progress(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));
    match name {
        "bash" | "Bash" => {
            let command = parsed
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if command.is_empty() {
                "running shell command".to_string()
            } else {
                format!("command {}", truncate_for_summary(command.trim(), 100))
            }
        }
        "read_file" | "Read" => format!("reading {}", extract_tool_path(&parsed)),
        "write_file" | "Write" => format!("writing {}", extract_tool_path(&parsed)),
        "edit_file" | "Edit" => format!("editing {}", extract_tool_path(&parsed)),
        "glob_search" | "Glob" => {
            let pattern = parsed
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let scope = parsed
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            format!("glob `{pattern}` in {scope}")
        }
        "grep_search" | "Grep" => {
            let pattern = parsed
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let scope = parsed
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            format!("grep `{pattern}` in {scope}")
        }
        "web_search" | "WebSearch" => parsed
            .get("query")
            .and_then(|value| value.as_str())
            .map_or_else(
                || "running web search".to_string(),
                |query| format!("query {}", truncate_for_summary(query, 100)),
            ),
        _ => {
            let summary = summarize_tool_payload(input);
            if summary.is_empty() {
                format!("running {name}")
            } else {
                format!("{name}: {summary}")
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_arguments)]
fn build_runtime(
    session: Session,
    session_id: &str,
    model: String,
    system_prompt: Vec<String>,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    progress_reporter: Option<InternalPromptProgressReporter>,
) -> Result<BuiltRuntime, Box<dyn std::error::Error>> {
    let runtime_plugin_state = build_runtime_plugin_state()?;
    build_runtime_with_plugin_state(
        session,
        session_id,
        model,
        system_prompt,
        enable_tools,
        emit_output,
        allowed_tools,
        permission_mode,
        progress_reporter,
        runtime_plugin_state,
    )
}

#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_arguments)]
fn build_runtime_with_plugin_state(
    mut session: Session,
    session_id: &str,
    model: String,
    system_prompt: Vec<String>,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    permission_mode: PermissionMode,
    progress_reporter: Option<InternalPromptProgressReporter>,
    runtime_plugin_state: RuntimePluginState,
) -> Result<BuiltRuntime, Box<dyn std::error::Error>> {
    // Persist the model in session metadata so resumed sessions can report it.
    if session.model.is_none() {
        session.model = Some(model.clone());
    }
    let RuntimePluginState {
        feature_config,
        tool_registry,
        plugin_registry,
        mcp_state,
    } = runtime_plugin_state;
    plugin_registry.initialize()?;
    let policy = permission_policy(permission_mode, &feature_config, &tool_registry)
        .map_err(std::io::Error::other)?;
    let mut runtime = ConversationRuntime::new_with_features(
        session,
        AnthropicRuntimeClient::new(
            session_id,
            model,
            enable_tools,
            emit_output,
            allowed_tools.clone(),
            tool_registry.clone(),
            progress_reporter,
        )?,
        CliToolExecutor::new(
            allowed_tools.clone(),
            emit_output,
            tool_registry.clone(),
            mcp_state.clone(),
        ),
        policy,
        system_prompt,
        &feature_config,
    );
    if emit_output {
        runtime = runtime.with_hook_progress_reporter(Box::new(CliHookProgressReporter));
    }
    Ok(BuiltRuntime::new(runtime, plugin_registry, mcp_state))
}

struct CliHookProgressReporter;

impl runtime::HookProgressReporter for CliHookProgressReporter {
    fn on_event(&mut self, event: &runtime::HookProgressEvent) {
        match event {
            runtime::HookProgressEvent::Started {
                event,
                tool_name,
                command,
            } => eprintln!(
                "[hook {event_name}] {tool_name}: {command}",
                event_name = event.as_str()
            ),
            runtime::HookProgressEvent::Completed {
                event,
                tool_name,
                command,
            } => eprintln!(
                "[hook done {event_name}] {tool_name}: {command}",
                event_name = event.as_str()
            ),
            runtime::HookProgressEvent::Cancelled {
                event,
                tool_name,
                command,
            } => eprintln!(
                "[hook cancelled {event_name}] {tool_name}: {command}",
                event_name = event.as_str()
            ),
        }
    }
}

struct CliPermissionPrompter {
    current_mode: PermissionMode,
}

impl CliPermissionPrompter {
    fn new(current_mode: PermissionMode) -> Self {
        Self { current_mode }
    }
}

impl runtime::PermissionPrompter for CliPermissionPrompter {
    fn decide(
        &mut self,
        request: &runtime::PermissionRequest,
    ) -> runtime::PermissionPromptDecision {
        println!();
        println!("Permission approval required");
        println!("  Tool             {}", request.tool_name);
        println!("  Current mode     {}", self.current_mode.as_str());
        println!("  Required mode    {}", request.required_mode.as_str());
        if let Some(reason) = &request.reason {
            println!("  Reason           {reason}");
        }
        println!("  Input            {}", request.input);
        print!("Approve this tool call? [y/N]: ");
        let _ = io::stdout().flush();

        let mut response = String::new();
        match io::stdin().read_line(&mut response) {
            Ok(_) => {
                let normalized = response.trim().to_ascii_lowercase();
                if matches!(normalized.as_str(), "y" | "yes") {
                    runtime::PermissionPromptDecision::Allow
                } else {
                    runtime::PermissionPromptDecision::Deny {
                        reason: format!(
                            "tool '{}' denied by user approval prompt",
                            request.tool_name
                        ),
                    }
                }
            }
            Err(error) => runtime::PermissionPromptDecision::Deny {
                reason: format!("permission approval failed: {error}"),
            },
        }
    }
}

// NOTE: Despite the historical name `AnthropicRuntimeClient`, this struct
// now holds an `ApiProviderClient` which dispatches to Anthropic, xAI,
// OpenAI, or DashScope at construction time based on
// `detect_provider_kind(&model)`. The struct name is kept to avoid
// churning `BuiltRuntime` and every Deref/DerefMut site that references
// it. See ROADMAP #29 for the provider-dispatch routing fix.
struct AnthropicRuntimeClient {
    runtime: tokio::runtime::Runtime,
    client: ApiProviderClient,
    session_id: String,
    model: String,
    enable_tools: bool,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    tool_registry: GlobalToolRegistry,
    progress_reporter: Option<InternalPromptProgressReporter>,
    reasoning_effort: Option<String>,
}

impl AnthropicRuntimeClient {
    fn new(
        session_id: &str,
        model: String,
        enable_tools: bool,
        emit_output: bool,
        allowed_tools: Option<AllowedToolSet>,
        tool_registry: GlobalToolRegistry,
        progress_reporter: Option<InternalPromptProgressReporter>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Dispatch to the correct provider at construction time.
        // `ApiProviderClient` (exposed by the api crate as
        // `ProviderClient`) is an enum over Anthropic / xAI / OpenAI
        // variants, where xAI and OpenAI both use the OpenAI-compat
        // wire format under the hood. We consult
        // `detect_provider_kind(&resolved_model)` so model-name prefix
        // routing (`openai/`, `gpt-`, `grok`, `qwen/`) wins over
        // env-var presence.
        //
        // For Anthropic we build the client directly instead of going
        // through `ApiProviderClient::from_model_with_anthropic_auth`
        // so we can explicitly apply `api::read_base_url()` — that
        // reads `ANTHROPIC_BASE_URL` and is required for the local
        // mock-server test harness
        // (`crates/rusty-claude-cli/tests/compact_output.rs`) to point
        // claw at its fake Anthropic endpoint. We also attach a
        // session-scoped prompt cache on the Anthropic path; the
        // prompt cache is Anthropic-only so non-Anthropic variants
        // skip it.
        let resolved_model = api::resolve_model_alias(&model);
        let client = match detect_provider_kind(&resolved_model) {
            ProviderKind::Anthropic => {
                let auth = resolve_cli_auth_source()?;
                let inner = AnthropicClient::from_auth(auth)
                    .with_base_url(api::read_base_url())
                    .with_prompt_cache(PromptCache::new(session_id));
                ApiProviderClient::Anthropic(inner)
            }
            ProviderKind::Xai | ProviderKind::OpenAi => {
                // The api crate's `ProviderClient::from_model_with_anthropic_auth`
                // with `None` for the anthropic auth routes via
                // `detect_provider_kind` and builds an
                // `OpenAiCompatClient::from_env` with the matching
                // `OpenAiCompatConfig` (openai / xai / dashscope).
                // That reads the correct API-key env var and BASE_URL
                // override internally, so this one call covers OpenAI,
                // OpenRouter, xAI, DashScope, Ollama, and any other
                // OpenAI-compat endpoint users configure via
                // `OPENAI_BASE_URL` / `XAI_BASE_URL` / `DASHSCOPE_BASE_URL`.
                ApiProviderClient::from_model_with_anthropic_auth(&resolved_model, None)?
            }
        };
        Ok(Self {
            runtime: tokio::runtime::Runtime::new()?,
            client,
            session_id: session_id.to_string(),
            model,
            enable_tools,
            emit_output,
            allowed_tools,
            tool_registry,
            progress_reporter,
            reasoning_effort: None,
        })
    }

    fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }
}

fn resolve_cli_auth_source() -> Result<AuthSource, Box<dyn std::error::Error>> {
    Ok(resolve_cli_auth_source_for_cwd()?)
}

#[allow(clippy::result_large_err)]
fn resolve_cli_auth_source_for_cwd() -> Result<AuthSource, api::ApiError> {
    resolve_startup_auth_source(|| Ok(None))
}

impl ApiClient for AnthropicRuntimeClient {
    #[allow(clippy::too_many_lines)]
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        if let Some(progress_reporter) = &self.progress_reporter {
            progress_reporter.mark_model_phase();
        }
        let is_post_tool = request_ends_with_tool_result(&request);
        let message_request = MessageRequest {
            model: self.model.clone(),
            max_tokens: max_tokens_for_model(&self.model),
            messages: convert_messages(&request.messages),
            system: (!request.system_prompt.is_empty()).then(|| request.system_prompt.join("\n\n")),
            tools: self
                .enable_tools
                .then(|| filter_tool_specs(&self.tool_registry, self.allowed_tools.as_ref())),
            tool_choice: self.enable_tools.then_some(ToolChoice::Auto),
            stream: true,
            reasoning_effort: self.reasoning_effort.clone(),
            ..Default::default()
        };

        self.runtime.block_on(async {
            // When resuming after tool execution, apply a stall timeout on the
            // first stream event.  If the model does not respond within the
            // deadline we drop the stalled connection and re-send the request as
            // a continuation nudge (one retry only).
            let max_attempts: usize = if is_post_tool { 2 } else { 1 };

            for attempt in 1..=max_attempts {
                let result = self
                    .consume_stream(&message_request, is_post_tool && attempt == 1)
                    .await;
                match result {
                    Ok(events) => return Ok(events),
                    Err(error)
                        if error.to_string().contains("post-tool stall")
                            && attempt < max_attempts =>
                    {
                        // Stalled after tool completion — nudge the model by
                        // re-sending the same request.
                    }
                    Err(error) => return Err(error),
                }
            }

            Err(RuntimeError::new("post-tool continuation nudge exhausted"))
        })
    }
}

impl AnthropicRuntimeClient {
    /// Consume a single streaming response, optionally applying a stall
    /// timeout on the first event for post-tool continuations.
    #[allow(clippy::too_many_lines)]
    async fn consume_stream(
        &self,
        message_request: &MessageRequest,
        apply_stall_timeout: bool,
    ) -> Result<Vec<AssistantEvent>, RuntimeError> {
        let mut stream = self
            .client
            .stream_message(message_request)
            .await
            .map_err(|error| {
                RuntimeError::new(format_user_visible_api_error(&self.session_id, &error))
            })?;
        let mut stdout = io::stdout();
        let mut sink = io::sink();
        let out: &mut dyn Write = if self.emit_output {
            &mut stdout
        } else {
            &mut sink
        };
        let renderer = TerminalRenderer::new();
        let mut markdown_stream = MarkdownStreamState::default();
        let mut events = Vec::new();
        let mut pending_tool: Option<(String, String, String)> = None;
        // 累积 reasoning_content 到 Thinking 块（修复 DeepSeek V4 reasoning_content 协议 bug）
        let mut pending_thinking: Option<(String, Option<String>)> = None;
        let mut block_has_thinking_summary = false;
        let mut saw_stop = false;
        let mut received_any_event = false;

        loop {
            let next = if apply_stall_timeout && !received_any_event {
                match tokio::time::timeout(POST_TOOL_STALL_TIMEOUT, stream.next_event()).await {
                    Ok(inner) => inner.map_err(|error| {
                        RuntimeError::new(format_user_visible_api_error(&self.session_id, &error))
                    })?,
                    Err(_elapsed) => {
                        return Err(RuntimeError::new(
                            "post-tool stall: model did not respond within timeout",
                        ));
                    }
                }
            } else {
                stream.next_event().await.map_err(|error| {
                    RuntimeError::new(format_user_visible_api_error(&self.session_id, &error))
                })?
            };

            let Some(event) = next else {
                break;
            };
            received_any_event = true;

            match event {
                ApiStreamEvent::MessageStart(start) => {
                    for block in start.message.content {
                        push_output_block(
                            block,
                            out,
                            &mut events,
                            &mut pending_tool,
                            true,
                            &mut block_has_thinking_summary,
                        )?;
                    }
                }
                ApiStreamEvent::ContentBlockStart(start) => {
                    // 特判 Thinking 块：初始化 pending_thinking（用于累积后续 ThinkingDelta）
                    if let OutputContentBlock::Thinking {
                        thinking,
                        signature,
                    } = &start.content_block
                    {
                        pending_thinking = Some((thinking.clone(), signature.clone()));
                    }
                    push_output_block(
                        start.content_block,
                        out,
                        &mut events,
                        &mut pending_tool,
                        true,
                        &mut block_has_thinking_summary,
                    )?;
                }
                ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                    ContentBlockDelta::TextDelta { text } => {
                        if !text.is_empty() {
                            if let Some(progress_reporter) = &self.progress_reporter {
                                progress_reporter.mark_text_phase(&text);
                            }
                            if let Some(rendered) = markdown_stream.push(&renderer, &text) {
                                write!(out, "{rendered}")
                                    .and_then(|()| out.flush())
                                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                            }
                            events.push(AssistantEvent::TextDelta(text));
                        }
                    }
                    ContentBlockDelta::InputJsonDelta { partial_json } => {
                        if let Some((_, _, input)) = &mut pending_tool {
                            input.push_str(&partial_json);
                        }
                    }
                    ContentBlockDelta::ThinkingDelta { thinking } => {
                        if !block_has_thinking_summary {
                            render_thinking_block_summary(out, None, false)?;
                            block_has_thinking_summary = true;
                        }
                        // 累积 thinking 文本到 pending_thinking（让 session 持久化能拿到）
                        if let Some((t, _)) = &mut pending_thinking {
                            t.push_str(&thinking);
                        }
                    }
                    ContentBlockDelta::SignatureDelta { signature } => {
                        // 累积 signature 到 pending_thinking
                        if let Some((_, sig)) = &mut pending_thinking {
                            sig.get_or_insert_with(String::new).push_str(&signature);
                        }
                    }
                },
                ApiStreamEvent::ContentBlockStop(_) => {
                    block_has_thinking_summary = false;
                    if let Some(rendered) = markdown_stream.flush(&renderer) {
                        write!(out, "{rendered}")
                            .and_then(|()| out.flush())
                            .map_err(|error| RuntimeError::new(error.to_string()))?;
                    }
                    // 把累积的 thinking 转成 AssistantEvent::Thinking（让 build_assistant_message 写入 session）
                    if let Some((thinking, signature)) = pending_thinking.take() {
                        events.push(AssistantEvent::Thinking {
                            thinking,
                            signature,
                        });
                    }
                    if let Some((id, name, input)) = pending_tool.take() {
                        if let Some(progress_reporter) = &self.progress_reporter {
                            progress_reporter.mark_tool_phase(&name, &input);
                        }
                        // Display tool call now that input is fully accumulated
                        writeln!(out, "\n{}", format_tool_call_start(&name, &input))
                            .and_then(|()| out.flush())
                            .map_err(|error| RuntimeError::new(error.to_string()))?;
                        events.push(AssistantEvent::ToolUse { id, name, input });
                    }
                }
                ApiStreamEvent::MessageDelta(delta) => {
                    events.push(AssistantEvent::Usage(delta.usage.token_usage()));
                }
                ApiStreamEvent::MessageStop(_) => {
                    saw_stop = true;
                    if let Some(rendered) = markdown_stream.flush(&renderer) {
                        write!(out, "{rendered}")
                            .and_then(|()| out.flush())
                            .map_err(|error| RuntimeError::new(error.to_string()))?;
                    }
                    events.push(AssistantEvent::MessageStop);
                }
            }
        }

        push_prompt_cache_record(&self.client, &mut events);

        if !saw_stop
            && events.iter().any(|event| {
                matches!(event, AssistantEvent::TextDelta(text) if !text.is_empty())
                    || matches!(event, AssistantEvent::ToolUse { .. })
            })
        {
            events.push(AssistantEvent::MessageStop);
        }

        if events
            .iter()
            .any(|event| matches!(event, AssistantEvent::MessageStop))
        {
            return Ok(events);
        }

        let response = self
            .client
            .send_message(&MessageRequest {
                stream: false,
                ..message_request.clone()
            })
            .await
            .map_err(|error| {
                RuntimeError::new(format_user_visible_api_error(&self.session_id, &error))
            })?;
        let mut events = response_to_events(response, out)?;
        push_prompt_cache_record(&self.client, &mut events);
        Ok(events)
    }
}

/// Returns `true` when the conversation ends with a tool-result message,
/// meaning the model is expected to continue after tool execution.
fn request_ends_with_tool_result(request: &ApiRequest) -> bool {
    request
        .messages
        .last()
        .is_some_and(|message| message.role == MessageRole::Tool)
}

/// Extract the server-reported context window size from an error message.
/// Returns `None` if no window size can be parsed.  The server must
/// mention something like "context size (81920 tokens)" or "available
/// context size (81920 tokens)" — the number inside parens after the
/// parenthesised phrase is taken as the window.
///
/// Known formats:
///   - "exceeds the available context size (81920 tokens)"
///   - "context size (128000 tokens)"
///   - "maximum context length is 200000 tokens"
fn extract_context_window_tokens_from_error(error_str: &str) -> Option<u32> {
    // Pattern: "(NNNNNN tokens)" appearing after context-size markers
    for line in error_str.lines() {
        let lowered = line.to_ascii_lowercase();
        if lowered.contains("context size")
            || lowered.contains("context length")
            || lowered.contains("context window")
        {
            // Try parenthesised form: (81920 tokens)
            if let Some(start) = lowered.find('(') {
                if let Some(end) = lowered.find(")") {
                    if start < end {
                        let inner = &line[start + 1..end];
                        let digits: String =
                            inner.chars().take_while(|c| c.is_ascii_digit()).collect();
                        if let Ok(n) = digits.parse::<u32>() {
                            if n > 1000 {
                                return Some(n);
                            }
                        }
                    }
                }
            }
            // Try "maximum context length is NNNNNN tokens"
            if let Some(pos) = lowered.find("is ") {
                let rest = &line[pos + 3..];
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = digits.parse::<u32>() {
                    if n > 1000 {
                        return Some(n);
                    }
                }
            }
            // Try "configured limit of NNNNNN tokens"
            if let Some(pos) = lowered.find("of ") {
                let rest = &line[pos + 3..];
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = digits.parse::<u32>() {
                    if n > 1000 {
                        return Some(n);
                    }
                }
            }
        }
    }
    None
}

fn format_user_visible_api_error(session_id: &str, error: &api::ApiError) -> String {
    if error.is_context_window_failure() {
        format_context_window_blocked_error(session_id, error)
    } else if error.is_generic_fatal_wrapper() {
        let mut qualifiers = vec![format!("session {session_id}")];
        if let Some(request_id) = error.request_id() {
            qualifiers.push(format!("trace {request_id}"));
        }
        format!(
            "{} ({}): {}",
            error.safe_failure_class(),
            qualifiers.join(", "),
            error
        )
    } else {
        error.to_string()
    }
}

fn format_context_window_blocked_error(session_id: &str, error: &api::ApiError) -> String {
    let mut lines = vec![
        "Context window blocked".to_string(),
        "  Failure class    context_window_blocked".to_string(),
        format!("  Session          {session_id}"),
    ];

    if let Some(request_id) = error.request_id() {
        lines.push(format!("  Trace            {request_id}"));
    }

    match error {
        api::ApiError::ContextWindowExceeded {
            model,
            estimated_input_tokens,
            requested_output_tokens,
            estimated_total_tokens,
            context_window_tokens,
        } => {
            lines.push(format!("  Model            {model}"));
            lines.push(format!(
                "  Input estimate   ~{estimated_input_tokens} tokens (heuristic)"
            ));
            lines.push(format!(
                "  Requested output {requested_output_tokens} tokens"
            ));
            lines.push(format!(
                "  Total estimate   ~{estimated_total_tokens} tokens (heuristic)"
            ));
            lines.push(format!("  Context window   {context_window_tokens} tokens"));
        }
        api::ApiError::Api { message, body, .. } => {
            let detail = message.as_deref().unwrap_or(body).trim();
            if !detail.is_empty() {
                lines.push(format!(
                    "  Detail           {}",
                    truncate_for_summary(detail, 120)
                ));
            }
        }
        api::ApiError::RetriesExhausted { last_error, .. } => {
            let detail = match last_error.as_ref() {
                api::ApiError::Api { message, body, .. } => message.as_deref().unwrap_or(body),
                other => return format_context_window_blocked_error(session_id, other),
            }
            .trim();
            if !detail.is_empty() {
                lines.push(format!(
                    "  Detail           {}",
                    truncate_for_summary(detail, 120)
                ));
            }
        }
        _ => {}
    }

    lines.push(String::new());
    lines.push("Recovery".to_string());
    lines.push("  Compact          /compact".to_string());
    lines.push(format!(
        "  Resume compact   claw --resume {session_id} /compact"
    ));
    lines.push("  Fresh session    /clear --confirm".to_string());
    lines.push(
        "  Reduce scope     remove large pasted context/files or ask for a smaller slice"
            .to_string(),
    );
    lines.push("  Retry            rerun after compacting or reducing the request".to_string());

    lines.join("\n")
}

fn final_assistant_text(summary: &runtime::TurnSummary) -> String {
    summary
        .assistant_messages
        .last()
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn collect_tool_uses(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .assistant_messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "id": id,
                "name": name,
                "input": input,
            })),
            _ => None,
        })
        .collect()
}

fn collect_tool_results(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .tool_results
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } => Some(json!({
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "output": output,
                "is_error": is_error,
            })),
            _ => None,
        })
        .collect()
}

fn collect_prompt_cache_events(summary: &runtime::TurnSummary) -> Vec<serde_json::Value> {
    summary
        .prompt_cache_events
        .iter()
        .map(|event| {
            json!({
                "unexpected": event.unexpected,
                "reason": event.reason,
                "previous_cache_read_input_tokens": event.previous_cache_read_input_tokens,
                "current_cache_read_input_tokens": event.current_cache_read_input_tokens,
                "token_drop": event.token_drop,
            })
        })
        .collect()
}

/// Slash commands that are registered in the spec list but not yet implemented
/// in this build. Used to filter both REPL completions and help output so the
/// discovery surface only shows commands that actually work (ROADMAP #39).
const STUB_COMMANDS: &[&str] = &[
    "login",
    "logout",
    "vim",
    "upgrade",
    "share",
    "feedback",
    "files",
    "fast",
    "exit",
    "summary",
    "desktop",
    "brief",
    "advisor",
    "stickers",
    "insights",
    "thinkback",
    "release-notes",
    "security-review",
    "keybindings",
    "privacy-settings",
    "plan",
    "review",
    "tasks",
    "theme",
    "voice",
    "usage",
    "rename",
    "copy",
    "hooks",
    "context",
    "color",
    "effort",
    "branch",
    "rewind",
    "ide",
    "tag",
    "output-style",
    "add-dir",
    // Spec entries with no parse arm — produce circular "Did you mean" error
    // without this guard. Adding here routes them to the proper unsupported
    // message and excludes them from REPL completions / help.
    // NOTE: do NOT add "stats", "tokens", "cache" — they are implemented.
    "allowed-tools",
    "bookmarks",
    "workspace",
    "reasoning",
    "budget",
    "rate-limit",
    "changelog",
    "diagnostics",
    "metrics",
    "tool-details",
    "focus",
    "unfocus",
    "pin",
    "unpin",
    "language",
    "profile",
    "max-tokens",
    "temperature",
    "system-prompt",
    "notifications",
    "telemetry",
    "env",
    "project",
    "terminal-setup",
    "api-key",
    "reset",
    "undo",
    "stop",
    "retry",
    "paste",
    "screenshot",
    "image",
    "search",
    "listen",
    "speak",
    "format",
    "test",
    "lint",
    "build",
    "run",
    "git",
    "stash",
    "blame",
    "log",
    "cron",
    "team",
    "benchmark",
    "migrate",
    "templates",
    "explain",
    "refactor",
    "docs",
    "fix",
    "perf",
    "chat",
    "web",
    "map",
    "symbols",
    "references",
    "definition",
    "hover",
    "autofix",
    "multi",
    "macro",
    "alias",
    "parallel",
    "subagent",
    "agent",
];

fn slash_command_completion_candidates_with_sessions(
    model: &str,
    active_session_id: Option<&str>,
    recent_session_ids: Vec<String>,
) -> Vec<String> {
    let mut completions = BTreeSet::new();

    for spec in slash_command_specs() {
        if STUB_COMMANDS.contains(&spec.name) {
            continue;
        }
        completions.insert(format!("/{}", spec.name));
        for alias in spec.aliases {
            if !STUB_COMMANDS.contains(alias) {
                completions.insert(format!("/{alias}"));
            }
        }
    }

    for candidate in [
        "/bughunter ",
        "/clear --confirm",
        "/config ",
        "/config env",
        "/config hooks",
        "/config model",
        "/config plugins",
        "/mcp ",
        "/mcp list",
        "/mcp show ",
        "/export ",
        "/issue ",
        "/model ",
        "/model opus",
        "/model sonnet",
        "/model haiku",
        "/permissions ",
        "/permissions read-only",
        "/permissions workspace-write",
        "/permissions danger-full-access",
        "/plugin list",
        "/plugin install ",
        "/plugin enable ",
        "/plugin disable ",
        "/plugin uninstall ",
        "/plugin update ",
        "/plugins list",
        "/pr ",
        "/resume ",
        "/session list",
        "/session switch ",
        "/session fork ",
        "/teleport ",
        "/ultraplan ",
        "/agents help",
        "/mcp help",
        "/skills help",
    ] {
        completions.insert(candidate.to_string());
    }

    if !model.trim().is_empty() {
        completions.insert(format!("/model {}", resolve_model_alias(model)));
        completions.insert(format!("/model {model}"));
    }

    if let Some(active_session_id) = active_session_id.filter(|value| !value.trim().is_empty()) {
        completions.insert(format!("/resume {active_session_id}"));
        completions.insert(format!("/session switch {active_session_id}"));
    }

    for session_id in recent_session_ids
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .take(10)
    {
        completions.insert(format!("/resume {session_id}"));
        completions.insert(format!("/session switch {session_id}"));
    }

    completions.into_iter().collect()
}

fn format_tool_call_start(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));

    let detail = match name {
        "bash" | "Bash" => format_bash_call(&parsed),
        "read_file" | "Read" => {
            let path = extract_tool_path(&parsed);
            format!("\x1b[2m📄 Reading {path}…\x1b[0m")
        }
        "write_file" | "Write" => {
            let path = extract_tool_path(&parsed);
            let lines = parsed
                .get("content")
                .and_then(|value| value.as_str())
                .map_or(0, |content| content.lines().count());
            format!("\x1b[1;32m✏️ Writing {path}\x1b[0m \x1b[2m({lines} lines)\x1b[0m")
        }
        "edit_file" | "Edit" => {
            let path = extract_tool_path(&parsed);
            let old_value = parsed
                .get("old_string")
                .or_else(|| parsed.get("oldString"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let new_value = parsed
                .get("new_string")
                .or_else(|| parsed.get("newString"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            format!(
                "\x1b[1;33m📝 Editing {path}\x1b[0m{}",
                format_patch_preview(old_value, new_value)
                    .map(|preview| format!("\n{preview}"))
                    .unwrap_or_default()
            )
        }
        "glob_search" | "Glob" => format_search_start("🔎 Glob", &parsed),
        "grep_search" | "Grep" => format_search_start("🔎 Grep", &parsed),
        "web_search" | "WebSearch" => parsed
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or("?")
            .to_string(),
        _ => summarize_tool_payload(input),
    };

    let border = "─".repeat(name.len() + 8);
    format!(
        "\x1b[38;5;245m╭─ \x1b[1;36m{name}\x1b[0;38;5;245m ─╮\x1b[0m\n\x1b[38;5;245m│\x1b[0m {detail}\n\x1b[38;5;245m╰{border}╯\x1b[0m"
    )
}

fn format_tool_result(name: &str, output: &str, is_error: bool) -> String {
    let icon = if is_error {
        "\x1b[1;31m✗\x1b[0m"
    } else {
        "\x1b[1;32m✓\x1b[0m"
    };
    if is_error {
        let summary = truncate_for_summary(output.trim(), 160);
        return if summary.is_empty() {
            format!("{icon} \x1b[38;5;245m{name}\x1b[0m")
        } else {
            format!("{icon} \x1b[38;5;245m{name}\x1b[0m\n\x1b[38;5;203m{summary}\x1b[0m")
        };
    }

    let parsed: serde_json::Value =
        serde_json::from_str(output).unwrap_or(serde_json::Value::String(output.to_string()));
    match name {
        "bash" | "Bash" => format_bash_result(icon, &parsed),
        "read_file" | "Read" => format_read_result(icon, &parsed),
        "write_file" | "Write" => format_write_result(icon, &parsed),
        "edit_file" | "Edit" => format_edit_result(icon, &parsed),
        "glob_search" | "Glob" => format_glob_result(icon, &parsed),
        "grep_search" | "Grep" => format_grep_result(icon, &parsed),
        _ => format_generic_tool_result(icon, name, &parsed),
    }
}

const DISPLAY_TRUNCATION_NOTICE: &str =
    "\x1b[2m… output truncated for display; full result preserved in session.\x1b[0m";
const READ_DISPLAY_MAX_LINES: usize = 80;
const READ_DISPLAY_MAX_CHARS: usize = 6_000;
const TOOL_OUTPUT_DISPLAY_MAX_LINES: usize = 60;
const TOOL_OUTPUT_DISPLAY_MAX_CHARS: usize = 4_000;

fn extract_tool_path(parsed: &serde_json::Value) -> String {
    parsed
        .get("file_path")
        .or_else(|| parsed.get("filePath"))
        .or_else(|| parsed.get("path"))
        .and_then(|value| value.as_str())
        .unwrap_or("?")
        .to_string()
}

fn format_search_start(label: &str, parsed: &serde_json::Value) -> String {
    let pattern = parsed
        .get("pattern")
        .and_then(|value| value.as_str())
        .unwrap_or("?");
    let scope = parsed
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or(".");
    format!("{label} {pattern}\n\x1b[2min {scope}\x1b[0m")
}

fn format_patch_preview(old_value: &str, new_value: &str) -> Option<String> {
    if old_value.is_empty() && new_value.is_empty() {
        return None;
    }
    Some(format!(
        "\x1b[38;5;203m- {}\x1b[0m\n\x1b[38;5;70m+ {}\x1b[0m",
        truncate_for_summary(first_visible_line(old_value), 72),
        truncate_for_summary(first_visible_line(new_value), 72)
    ))
}

fn format_bash_call(parsed: &serde_json::Value) -> String {
    let command = parsed
        .get("command")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if command.is_empty() {
        String::new()
    } else {
        format!(
            "\x1b[48;5;236;38;5;255m $ {} \x1b[0m",
            truncate_for_summary(command, 160)
        )
    }
}

fn first_visible_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
}

fn format_bash_result(icon: &str, parsed: &serde_json::Value) -> String {
    use std::fmt::Write as _;

    let mut lines = vec![format!("{icon} \x1b[38;5;245mbash\x1b[0m")];
    if let Some(task_id) = parsed
        .get("backgroundTaskId")
        .and_then(|value| value.as_str())
    {
        write!(&mut lines[0], " backgrounded ({task_id})").expect("write to string");
    } else if let Some(status) = parsed
        .get("returnCodeInterpretation")
        .and_then(|value| value.as_str())
        .filter(|status| !status.is_empty())
    {
        write!(&mut lines[0], " {status}").expect("write to string");
    }

    if let Some(stdout) = parsed.get("stdout").and_then(|value| value.as_str()) {
        if !stdout.trim().is_empty() {
            lines.push(truncate_output_for_display(
                stdout,
                TOOL_OUTPUT_DISPLAY_MAX_LINES,
                TOOL_OUTPUT_DISPLAY_MAX_CHARS,
            ));
        }
    }
    if let Some(stderr) = parsed.get("stderr").and_then(|value| value.as_str()) {
        if !stderr.trim().is_empty() {
            lines.push(format!(
                "\x1b[38;5;203m{}\x1b[0m",
                truncate_output_for_display(
                    stderr,
                    TOOL_OUTPUT_DISPLAY_MAX_LINES,
                    TOOL_OUTPUT_DISPLAY_MAX_CHARS,
                )
            ));
        }
    }

    lines.join("\n\n")
}

fn format_read_result(icon: &str, parsed: &serde_json::Value) -> String {
    let file = parsed.get("file").unwrap_or(parsed);
    let path = extract_tool_path(file);
    let start_line = file
        .get("startLine")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let num_lines = file
        .get("numLines")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let total_lines = file
        .get("totalLines")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(num_lines);
    let content = file
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let end_line = start_line.saturating_add(num_lines.saturating_sub(1));

    format!(
        "{icon} \x1b[2m📄 Read {path} (lines {}-{} of {})\x1b[0m\n{}",
        start_line,
        end_line.max(start_line),
        total_lines,
        truncate_output_for_display(content, READ_DISPLAY_MAX_LINES, READ_DISPLAY_MAX_CHARS)
    )
}

fn format_write_result(icon: &str, parsed: &serde_json::Value) -> String {
    let path = extract_tool_path(parsed);
    let kind = parsed
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("write");
    let line_count = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .map_or(0, |content| content.lines().count());
    format!(
        "{icon} \x1b[1;32m✏️ {} {path}\x1b[0m \x1b[2m({line_count} lines)\x1b[0m",
        if kind == "create" { "Wrote" } else { "Updated" },
    )
}

fn format_structured_patch_preview(parsed: &serde_json::Value) -> Option<String> {
    let hunks = parsed.get("structuredPatch")?.as_array()?;
    let mut preview = Vec::new();
    for hunk in hunks.iter().take(2) {
        let lines = hunk.get("lines")?.as_array()?;
        for line in lines.iter().filter_map(|value| value.as_str()).take(6) {
            match line.chars().next() {
                Some('+') => preview.push(format!("\x1b[38;5;70m{line}\x1b[0m")),
                Some('-') => preview.push(format!("\x1b[38;5;203m{line}\x1b[0m")),
                _ => preview.push(line.to_string()),
            }
        }
    }
    if preview.is_empty() {
        None
    } else {
        Some(preview.join("\n"))
    }
}

fn format_edit_result(icon: &str, parsed: &serde_json::Value) -> String {
    let path = extract_tool_path(parsed);
    let suffix = if parsed
        .get("replaceAll")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        " (replace all)"
    } else {
        ""
    };
    let preview = format_structured_patch_preview(parsed).or_else(|| {
        let old_value = parsed
            .get("oldString")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let new_value = parsed
            .get("newString")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        format_patch_preview(old_value, new_value)
    });

    match preview {
        Some(preview) => format!("{icon} \x1b[1;33m📝 Edited {path}{suffix}\x1b[0m\n{preview}"),
        None => format!("{icon} \x1b[1;33m📝 Edited {path}{suffix}\x1b[0m"),
    }
}

fn format_glob_result(icon: &str, parsed: &serde_json::Value) -> String {
    let num_files = parsed
        .get("numFiles")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let filenames = parsed
        .get("filenames")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|value| value.as_str())
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if filenames.is_empty() {
        format!("{icon} \x1b[38;5;245mglob_search\x1b[0m matched {num_files} files")
    } else {
        format!("{icon} \x1b[38;5;245mglob_search\x1b[0m matched {num_files} files\n{filenames}")
    }
}

fn format_grep_result(icon: &str, parsed: &serde_json::Value) -> String {
    let num_matches = parsed
        .get("numMatches")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let num_files = parsed
        .get("numFiles")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let content = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let filenames = parsed
        .get("filenames")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|value| value.as_str())
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let summary = format!(
        "{icon} \x1b[38;5;245mgrep_search\x1b[0m {num_matches} matches across {num_files} files"
    );
    if !content.trim().is_empty() {
        format!(
            "{summary}\n{}",
            truncate_output_for_display(
                content,
                TOOL_OUTPUT_DISPLAY_MAX_LINES,
                TOOL_OUTPUT_DISPLAY_MAX_CHARS,
            )
        )
    } else if !filenames.is_empty() {
        format!("{summary}\n{filenames}")
    } else {
        summary
    }
}

fn format_generic_tool_result(icon: &str, name: &str, parsed: &serde_json::Value) -> String {
    let rendered_output = match parsed {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            serde_json::to_string_pretty(parsed).unwrap_or_else(|_| parsed.to_string())
        }
        _ => parsed.to_string(),
    };
    let preview = truncate_output_for_display(
        &rendered_output,
        TOOL_OUTPUT_DISPLAY_MAX_LINES,
        TOOL_OUTPUT_DISPLAY_MAX_CHARS,
    );

    if preview.is_empty() {
        format!("{icon} \x1b[38;5;245m{name}\x1b[0m")
    } else if preview.contains('\n') {
        format!("{icon} \x1b[38;5;245m{name}\x1b[0m\n{preview}")
    } else {
        format!("{icon} \x1b[38;5;245m{name}:\x1b[0m {preview}")
    }
}

fn summarize_tool_payload(payload: &str) -> String {
    let compact = match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => value.to_string(),
        Err(_) => payload.trim().to_string(),
    };
    truncate_for_summary(&compact, 96)
}

fn truncate_for_summary(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn truncate_output_for_display(content: &str, max_lines: usize, max_chars: usize) -> String {
    let original = content.trim_end_matches('\n');
    if original.is_empty() {
        return String::new();
    }

    let mut preview_lines = Vec::new();
    let mut used_chars = 0usize;
    let mut truncated = false;

    for (index, line) in original.lines().enumerate() {
        if index >= max_lines {
            truncated = true;
            break;
        }

        let newline_cost = usize::from(!preview_lines.is_empty());
        let available = max_chars.saturating_sub(used_chars + newline_cost);
        if available == 0 {
            truncated = true;
            break;
        }

        let line_chars = line.chars().count();
        if line_chars > available {
            preview_lines.push(line.chars().take(available).collect::<String>());
            truncated = true;
            break;
        }

        preview_lines.push(line.to_string());
        used_chars += newline_cost + line_chars;
    }

    let mut preview = preview_lines.join("\n");
    if truncated {
        if !preview.is_empty() {
            preview.push('\n');
        }
        preview.push_str(DISPLAY_TRUNCATION_NOTICE);
    }
    preview
}

fn render_thinking_block_summary(
    out: &mut (impl Write + ?Sized),
    char_count: Option<usize>,
    redacted: bool,
) -> Result<(), RuntimeError> {
    let summary = if redacted {
        "\n▶ Thinking block hidden by provider\n".to_string()
    } else if let Some(char_count) = char_count {
        format!("\n▶ Thinking ({char_count} chars hidden)\n")
    } else {
        "\n▶ Thinking hidden\n".to_string()
    };
    write!(out, "{summary}")
        .and_then(|()| out.flush())
        .map_err(|error| RuntimeError::new(error.to_string()))
}

fn push_output_block(
    block: OutputContentBlock,
    out: &mut (impl Write + ?Sized),
    events: &mut Vec<AssistantEvent>,
    pending_tool: &mut Option<(String, String, String)>,
    streaming_tool_input: bool,
    block_has_thinking_summary: &mut bool,
) -> Result<(), RuntimeError> {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                let rendered = TerminalRenderer::new().markdown_to_ansi(&text);
                write!(out, "{rendered}")
                    .and_then(|()| out.flush())
                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse { id, name, input } => {
            // During streaming, the initial content_block_start has an empty input ({}).
            // The real input arrives via input_json_delta events. In
            // non-streaming responses, preserve a legitimate empty object.
            let initial_input = if streaming_tool_input
                && input.is_object()
                && input.as_object().is_some_and(serde_json::Map::is_empty)
            {
                String::new()
            } else {
                input.to_string()
            };
            *pending_tool = Some((id, name, initial_input));
        }
        OutputContentBlock::Thinking {
            thinking,
            signature,
        } => {
            render_thinking_block_summary(out, Some(thinking.chars().count()), false)?;
            events.push(AssistantEvent::Thinking {
                thinking,
                signature,
            });
            *block_has_thinking_summary = true;
        }
        OutputContentBlock::RedactedThinking { .. } => {
            render_thinking_block_summary(out, None, true)?;
            *block_has_thinking_summary = true;
        }
    }
    Ok(())
}

fn response_to_events(
    response: MessageResponse,
    out: &mut (impl Write + ?Sized),
) -> Result<Vec<AssistantEvent>, RuntimeError> {
    let mut events = Vec::new();
    let mut pending_tool = None;

    for block in response.content {
        let mut block_has_thinking_summary = false;
        push_output_block(
            block,
            out,
            &mut events,
            &mut pending_tool,
            false,
            &mut block_has_thinking_summary,
        )?;
        if let Some((id, name, input)) = pending_tool.take() {
            events.push(AssistantEvent::ToolUse { id, name, input });
        }
    }

    events.push(AssistantEvent::Usage(response.usage.token_usage()));
    events.push(AssistantEvent::MessageStop);
    Ok(events)
}

fn push_prompt_cache_record(client: &ApiProviderClient, events: &mut Vec<AssistantEvent>) {
    // `ApiProviderClient::take_last_prompt_cache_record` is a pass-through
    // to the Anthropic variant and returns `None` for OpenAI-compat /
    // xAI variants, which do not have a prompt cache. So this helper
    // remains a no-op on non-Anthropic providers without any extra
    // branching here.
    if let Some(record) = client.take_last_prompt_cache_record() {
        if let Some(event) = prompt_cache_record_to_runtime_event(record) {
            events.push(AssistantEvent::PromptCache(event));
        }
    }
}

fn prompt_cache_record_to_runtime_event(
    record: api::PromptCacheRecord,
) -> Option<PromptCacheEvent> {
    let cache_break = record.cache_break?;
    Some(PromptCacheEvent {
        unexpected: cache_break.unexpected,
        reason: cache_break.reason,
        previous_cache_read_input_tokens: cache_break.previous_cache_read_input_tokens,
        current_cache_read_input_tokens: cache_break.current_cache_read_input_tokens,
        token_drop: cache_break.token_drop,
    })
}

struct CliToolExecutor {
    renderer: TerminalRenderer,
    emit_output: bool,
    allowed_tools: Option<AllowedToolSet>,
    tool_registry: GlobalToolRegistry,
    mcp_state: Option<Arc<Mutex<RuntimeMcpState>>>,
}

impl CliToolExecutor {
    fn new(
        allowed_tools: Option<AllowedToolSet>,
        emit_output: bool,
        tool_registry: GlobalToolRegistry,
        mcp_state: Option<Arc<Mutex<RuntimeMcpState>>>,
    ) -> Self {
        Self {
            renderer: TerminalRenderer::new(),
            emit_output,
            allowed_tools,
            tool_registry,
            mcp_state,
        }
    }

    fn execute_search_tool(&self, value: serde_json::Value) -> Result<String, ToolError> {
        let input: ToolSearchRequest = serde_json::from_value(value)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
        let (pending_mcp_servers, mcp_degraded) =
            self.mcp_state.as_ref().map_or((None, None), |state| {
                let state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (state.pending_servers(), state.degraded_report())
            });
        serde_json::to_string_pretty(&self.tool_registry.search(
            &input.query,
            input.max_results.unwrap_or(5),
            pending_mcp_servers,
            mcp_degraded,
        ))
        .map_err(|error| ToolError::new(error.to_string()))
    }

    fn execute_runtime_tool(
        &self,
        tool_name: &str,
        value: serde_json::Value,
    ) -> Result<String, ToolError> {
        let Some(mcp_state) = &self.mcp_state else {
            return Err(ToolError::new(format!(
                "runtime tool `{tool_name}` is unavailable without configured MCP servers"
            )));
        };
        let mut mcp_state = mcp_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        match tool_name {
            "MCPTool" => {
                let input: McpToolRequest = serde_json::from_value(value)
                    .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
                let qualified_name = input
                    .qualified_name
                    .or(input.tool)
                    .ok_or_else(|| ToolError::new("missing required field `qualifiedName`"))?;
                mcp_state.call_tool(&qualified_name, input.arguments)
            }
            "ListMcpResourcesTool" => {
                let input: ListMcpResourcesRequest = serde_json::from_value(value)
                    .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
                match input.server {
                    Some(server_name) => mcp_state.list_resources_for_server(&server_name),
                    None => mcp_state.list_resources_for_all_servers(),
                }
            }
            "ReadMcpResourceTool" => {
                let input: ReadMcpResourceRequest = serde_json::from_value(value)
                    .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
                mcp_state.read_resource(&input.server, &input.uri)
            }
            _ => mcp_state.call_tool(tool_name, Some(value)),
        }
    }
}

impl ToolExecutor for CliToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if self
            .allowed_tools
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&canonical_allowed_tool_name(tool_name)))
        {
            return Err(ToolError::new(format!(
                "tool `{tool_name}` is not enabled by the current --allowedTools setting"
            )));
        }
        let value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
        let result = if tool_name == "ToolSearch" {
            self.execute_search_tool(value)
        } else if self.tool_registry.has_runtime_tool(tool_name) {
            self.execute_runtime_tool(tool_name, value)
        } else {
            self.tool_registry
                .execute(tool_name, &value)
                .map_err(ToolError::new)
        };
        match result {
            Ok(output) => {
                if self.emit_output {
                    let markdown = format_tool_result(tool_name, &output, false);
                    self.renderer
                        .stream_markdown(&markdown, &mut io::stdout())
                        .map_err(|error| ToolError::new(error.to_string()))?;
                }
                Ok(output)
            }
            Err(error) => {
                if self.emit_output {
                    let markdown = format_tool_result(tool_name, &error.to_string(), true);
                    self.renderer
                        .stream_markdown(&markdown, &mut io::stdout())
                        .map_err(|stream_error| ToolError::new(stream_error.to_string()))?;
                }
                Err(error)
            }
        }
    }
}

fn permission_policy(
    mode: PermissionMode,
    feature_config: &runtime::RuntimeFeatureConfig,
    tool_registry: &GlobalToolRegistry,
) -> Result<PermissionPolicy, String> {
    Ok(tool_registry.permission_specs(None)?.into_iter().fold(
        PermissionPolicy::new(mode).with_permission_rules(feature_config.permission_rules()),
        |policy, (name, required_permission)| {
            policy.with_tool_requirement(name, required_permission)
        },
    ))
}

fn convert_messages(messages: &[ConversationMessage]) -> Vec<InputMessage> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::System | MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "assistant",
            };
            let content = message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => {
                        Some(InputContentBlock::Text { text: text.clone() })
                    }
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                    } => {
                        // 保留 Thinking 块：OpenAI 兼容协议会把它转成 reasoning_content 字段
                        // 回传给 DeepSeek V4（避免 400 "reasoning_content must be passed back" 错误）
                        Some(InputContentBlock::Thinking {
                            thinking: thinking.clone(),
                            signature: signature.clone(),
                        })
                    }
                    ContentBlock::ToolUse { id, name, input } => Some(InputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::from_str(input)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": input })),
                    }),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    } => Some(InputContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContentBlock::Text {
                            text: output.clone(),
                        }],
                        is_error: *is_error,
                    }),
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn print_help_to(out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "claw v{VERSION}")?;
    writeln!(out)?;
    writeln!(out, "Usage:")?;
    writeln!(
        out,
        "  claw [--model MODEL] [--allowedTools TOOL[,TOOL...]]"
    )?;
    writeln!(out, "      Start the interactive REPL")?;
    writeln!(
        out,
        "  claw [--model MODEL] [--output-format text|json] prompt [--stdin] [TEXT]"
    )?;
    writeln!(
        out,
        "      Send one prompt and exit; reads stdin when TEXT is omitted"
    )?;
    writeln!(
        out,
        "  claw [--model MODEL] [--output-format text|json] TEXT"
    )?;
    writeln!(out, "      Shorthand non-interactive prompt mode")?;
    writeln!(
        out,
        "      Use `--` before TEXT when the prompt itself starts with '-' or '--'"
    )?;
    writeln!(
        out,
        "  claw --resume [SESSION.jsonl|session-id|latest] [/status] [/compact] [...]"
    )?;
    writeln!(
        out,
        "      Inspect or maintain a saved session without entering the REPL"
    )?;
    writeln!(out, "  claw help")?;
    writeln!(out, "      Alias for --help")?;
    writeln!(out, "  claw version")?;
    writeln!(out, "      Alias for --version")?;
    writeln!(out, "  claw status")?;
    writeln!(
        out,
        "      Show the current local workspace status snapshot"
    )?;
    writeln!(out, "  claw sandbox")?;
    writeln!(out, "      Show the current sandbox isolation snapshot")?;
    writeln!(out, "  claw doctor")?;
    writeln!(
        out,
        "      Diagnose local auth, config, workspace, and sandbox health"
    )?;
    writeln!(out, "  claw acp [serve]")?;
    writeln!(
        out,
        "      Show ACP/Zed editor integration status (currently unsupported; aliases: --acp, -acp)"
    )?;
    writeln!(out, "      Source of truth: {OFFICIAL_REPO_SLUG}")?;
    writeln!(
        out,
        "      Warning: do not `{DEPRECATED_INSTALL_COMMAND}` (deprecated stub)"
    )?;
    writeln!(out, "  claw dump-manifests [--manifests-dir PATH]")?;
    writeln!(out, "  claw bootstrap-plan")?;
    writeln!(out, "  claw agents")?;
    writeln!(out, "  claw mcp")?;
    writeln!(out, "  claw skills")?;
    writeln!(out, "  claw system-prompt [--cwd PATH] [--date YYYY-MM-DD]")?;
    writeln!(out, "  claw init")?;
    writeln!(
        out,
        "  claw export [PATH] [--session SESSION] [--output PATH]"
    )?;
    writeln!(
        out,
        "      Dump the latest (or named) session as markdown; writes to PATH or stdout"
    )?;
    writeln!(out)?;
    writeln!(out, "Flags:")?;
    writeln!(
        out,
        "  --model MODEL              Override the active model"
    )?;
    writeln!(
        out,
        "  --output-format FORMAT     Non-interactive output format: text or json (case-insensitive)"
    )?;
    writeln!(
        out,
        "                              CLAW_OUTPUT_FORMAT sets the default; flags override env"
    )?;
    writeln!(
        out,
        "                              Log env vars: CLAW_LOG or RUST_LOG"
    )?;
    writeln!(
        out,
        "  --cwd PATH, -C PATH, --directory PATH  Run as if launched from PATH"
    )?;
    writeln!(
        out,
        "  --compact                  Strip tool call details; print only the final assistant text (text mode only; useful for piping)"
    )?;
    writeln!(
        out,
        "  --permission-mode MODE     Set read-only, workspace-write, or danger-full-access"
    )?;
    writeln!(
        out,
        "  --dangerously-skip-permissions, --skip-permissions  Skip all permission checks"
    )?;
    writeln!(
        out,
        "  --allowedTools TOOLS       Restrict enabled tools by canonical snake_case name or alias"
    )?;
    writeln!(out, "                              Examples: read, glob, web_fetch, WebFetch; status JSON exposes aliases")?;
    writeln!(
        out,
        "  --version, -V              Print version and build information locally"
    )?;
    writeln!(out)?;
    writeln!(out, "Interactive slash commands:")?;
    writeln!(out, "{}", render_slash_command_help_filtered(STUB_COMMANDS))?;
    writeln!(out)?;
    let resume_commands = resume_supported_slash_commands()
        .into_iter()
        .filter(|spec| !STUB_COMMANDS.contains(&spec.name))
        .map(|spec| match spec.argument_hint {
            Some(argument_hint) => format!("/{} {}", spec.name, argument_hint),
            None => format!("/{}", spec.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "Resume-safe commands: {resume_commands}")?;
    writeln!(out)?;
    writeln!(out, "Session shortcuts:")?;
    writeln!(
        out,
        "  REPL turns auto-save to .claw/sessions/<session-id>.{PRIMARY_SESSION_EXTENSION}"
    )?;
    writeln!(
        out,
        "  Use `{LATEST_SESSION_REFERENCE}` with --resume, /resume, or /session switch to target the newest saved session"
    )?;
    writeln!(
        out,
        "  Use /session list in the REPL to browse managed sessions"
    )?;
    writeln!(out, "Examples:")?;
    writeln!(out, "  claw --model claude-opus \"summarize this repo\"")?;
    writeln!(
        out,
        "  claw --output-format json prompt \"explain src/main.rs\""
    )?;
    writeln!(out, "  claw --compact \"summarize Cargo.toml\" | wc -l")?;
    writeln!(
        out,
        "  claw --allowedTools read,glob \"summarize Cargo.toml\""
    )?;
    writeln!(out, "  claw --resume {LATEST_SESSION_REFERENCE}")?;
    writeln!(
        out,
        "  claw --resume {LATEST_SESSION_REFERENCE} /status /diff /export notes.txt"
    )?;
    writeln!(out, "  claw agents")?;
    writeln!(out, "  claw mcp show my-server")?;
    writeln!(out, "  claw /skills")?;
    writeln!(out, "  claw doctor")?;
    writeln!(out, "  source of truth: {OFFICIAL_REPO_URL}")?;
    writeln!(
        out,
        "  do not run `{DEPRECATED_INSTALL_COMMAND}` — it installs a deprecated stub"
    )?;
    writeln!(out, "  claw init")?;
    writeln!(out, "  claw export")?;
    writeln!(out, "  claw export conversation.md")?;
    Ok(())
}

fn print_help(output_format: CliOutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    let mut buffer = Vec::new();
    print_help_to(&mut buffer)?;
    let message = String::from_utf8(buffer)?;
    match output_format {
        CliOutputFormat::Text => print!("{message}"),
        CliOutputFormat::Json => {
            // #325: include structured command list in top-level help JSON
            let commands: Vec<serde_json::Value> = commands::slash_command_specs()
                .iter()
                .map(|spec| {
                    serde_json::json!({
                        "name": spec.name,
                        "summary": spec.summary,
                        "resume_supported": spec.resume_supported,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "kind": "help",
                    "action": "help",
                    "status": "ok",
                    "message": message,
                    "commands": commands,
                    "total_commands": commands.len(),
                }))?
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        acp_status_json, build_runtime_plugin_state_with_loader, build_runtime_with_plugin_state,
        classify_error_kind, classify_session_lifecycle_from_panes, collect_session_prompt_history,
        create_managed_session_handle, describe_tool_progress, filter_tool_specs,
        format_bughunter_report, format_commit_preflight_report, format_commit_skipped_report,
        format_compact_report, format_connected_line, format_cost_report, format_history_timestamp,
        format_internal_prompt_progress_line, format_issue_report, format_model_report,
        format_model_switch_report, format_permissions_report, format_permissions_switch_report,
        format_pr_report, format_resume_report, format_status_report, format_tool_call_start,
        format_tool_result, format_ultraplan_report, format_unknown_slash_command,
        format_unknown_slash_command_message, format_user_visible_api_error,
        merge_prompt_with_stdin, normalize_permission_mode, parse_args, parse_export_args,
        parse_git_status_branch, parse_git_status_metadata_for, parse_git_workspace_summary,
        parse_history_count, permission_policy, print_help_to, push_output_block,
        render_config_report, render_diff_report, render_diff_report_for, render_help_topic,
        render_help_topic_json, render_memory_report, render_prompt_history_report,
        render_repl_help, render_resume_usage, render_session_list, render_session_markdown,
        resolve_model_alias, resolve_model_alias_with_config, resolve_repl_model,
        resolve_session_reference, response_to_events, resume_supported_slash_commands,
        run_resume_command, short_tool_id, slash_command_completion_candidates_with_sessions,
        split_error_hint, status_context, status_json_value, summarize_tool_payload_for_markdown,
        try_resolve_bare_skill_prompt, validate_no_args, write_mcp_server_fixture, CliAction,
        CliOutputFormat, CliToolExecutor, GitOperation, GitWorkspaceSummary,
        InternalPromptProgressEvent, InternalPromptProgressState, LiveCli, LocalHelpTopic,
        PermissionModeProvenance, PromptHistoryEntry, SessionLifecycleKind,
        SessionLifecycleSummary, SlashCommand, StatusUsage, TmuxPaneSnapshot, DEFAULT_MODEL,
        LATEST_SESSION_REFERENCE, STUB_COMMANDS,
    };
    use api::{ApiError, MessageResponse, OutputContentBlock, Usage};
    use plugins::{
        PluginManager, PluginManagerConfig, PluginTool, PluginToolDefinition, PluginToolPermission,
    };
    use runtime::{
        load_oauth_credentials, save_oauth_credentials, AssistantEvent, ConfigLoader, ContentBlock,
        ConversationMessage, MessageRole, OAuthConfig, PermissionMode, Session, ToolExecutor,
    };
    use serde_json::json;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tools::GlobalToolRegistry;

    fn registry_with_plugin_tool() -> GlobalToolRegistry {
        GlobalToolRegistry::with_plugin_tools(vec![PluginTool::new(
            "plugin-demo@external",
            "plugin-demo",
            PluginToolDefinition {
                name: "plugin_echo".to_string(),
                description: Some("Echo plugin payload".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" }
                    },
                    "required": ["message"],
                    "additionalProperties": false
                }),
            },
            "echo".to_string(),
            Vec::new(),
            PluginToolPermission::WorkspaceWrite,
            None,
        )])
        .expect("plugin tool registry should build")
    }

    #[test]
    fn opaque_provider_wrapper_surfaces_failure_class_session_and_trace() {
        let error = ApiError::Api {
            status: "500".parse().expect("status"),
            error_type: Some("api_error".to_string()),
            message: Some(
                "Something went wrong while processing your request. Please try again, or use /new to start a fresh session."
                    .to_string(),
            ),
            request_id: Some("req_jobdori_789".to_string()),
            body: String::new(),
            retryable: true,
            suggested_action: None,
            retry_after: None,
};

        let rendered = format_user_visible_api_error("session-issue-22", &error);
        assert!(rendered.contains("provider_internal"));
        assert!(rendered.contains("session session-issue-22"));
        assert!(rendered.contains("trace req_jobdori_789"));
    }

    #[test]
    fn retry_exhaustion_uses_retry_failure_class_for_generic_provider_wrapper() {
        let error = ApiError::RetriesExhausted {
            attempts: 3,
            last_error: Box::new(ApiError::Api {
                status: "502".parse().expect("status"),
                error_type: Some("api_error".to_string()),
                message: Some(
                    "Something went wrong while processing your request. Please try again, or use /new to start a fresh session."
                        .to_string(),
                ),
                request_id: Some("req_jobdori_790".to_string()),
                body: String::new(),
                retryable: true,
                suggested_action: None,
                retry_after: None,
}),
        };

        let rendered = format_user_visible_api_error("session-issue-22", &error);
        assert!(rendered.contains("provider_retry_exhausted"), "{rendered}");
        assert!(rendered.contains("session session-issue-22"));
        assert!(rendered.contains("trace req_jobdori_790"));
    }

    #[test]
    fn context_window_preflight_errors_render_recovery_steps() {
        let error = ApiError::ContextWindowExceeded {
            model: "anthropic/claude-sonnet-4-6".to_string(),
            estimated_input_tokens: 182_000,
            requested_output_tokens: 64_000,
            estimated_total_tokens: 246_000,
            context_window_tokens: 200_000,
        };

        let rendered = format_user_visible_api_error("session-issue-32", &error);
        assert!(rendered.contains("Context window blocked"), "{rendered}");
        assert!(rendered.contains("context_window_blocked"), "{rendered}");
        assert!(
            rendered.contains("Session          session-issue-32"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Model            anthropic/claude-sonnet-4-6"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Input estimate   ~182000 tokens (heuristic)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Total estimate   ~246000 tokens (heuristic)"),
            "{rendered}"
        );
        assert!(rendered.contains("Compact          /compact"), "{rendered}");
        assert!(
            rendered.contains("Resume compact   claw --resume session-issue-32 /compact"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Fresh session    /clear --confirm"),
            "{rendered}"
        );
        assert!(rendered.contains("Reduce scope"), "{rendered}");
        assert!(rendered.contains("Retry            rerun"), "{rendered}");
    }

    #[test]
    fn provider_context_window_errors_are_reframed_with_same_guidance() {
        let error = ApiError::Api {
            status: "400".parse().expect("status"),
            error_type: Some("invalid_request_error".to_string()),
            message: Some(
                "This model's maximum context length is 200000 tokens, but your request used 230000 tokens."
                    .to_string(),
            ),
            request_id: Some("req_ctx_456".to_string()),
            body: String::new(),
            retryable: false,
            suggested_action: None,
            retry_after: None,
};

        let rendered = format_user_visible_api_error("session-issue-32", &error);
        assert!(rendered.contains("context_window_blocked"), "{rendered}");
        assert!(
            rendered.contains("Trace            req_ctx_456"),
            "{rendered}"
        );
        assert!(
            rendered
                .contains("Detail           This model's maximum context length is 200000 tokens"),
            "{rendered}"
        );
        assert!(rendered.contains("Compact          /compact"), "{rendered}");
        assert!(
            rendered.contains("Fresh session    /clear --confirm"),
            "{rendered}"
        );
    }

    #[test]
    fn openai_configured_limit_errors_are_rendered_as_context_window_guidance() {
        let error = ApiError::Api {
            status: "400".parse().expect("status"),
            error_type: Some("invalid_request_error".to_string()),
            message: Some(
                "Input tokens exceed the configured limit of 922000 tokens. Your messages resulted in 1860900 tokens. Please reduce the length of the messages."
                    .to_string(),
            ),
            request_id: Some("req_ctx_openai_456".to_string()),
            body: String::new(),
            retryable: false,
            suggested_action: None,
            retry_after: None,
        };

        let rendered = format_user_visible_api_error("session-issue-32", &error);
        assert!(rendered.contains("Context window blocked"), "{rendered}");
        assert!(rendered.contains("context_window_blocked"), "{rendered}");
        assert!(
            rendered.contains("Trace            req_ctx_openai_456"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "Detail           Input tokens exceed the configured limit of 922000 tokens."
            ),
            "{rendered}"
        );
        assert!(rendered.contains("Compact          /compact"), "{rendered}");
        assert!(
            rendered.contains("Fresh session    /clear --confirm"),
            "{rendered}"
        );
    }

    #[test]
    fn retry_wrapped_context_window_errors_keep_recovery_guidance() {
        let error = ApiError::RetriesExhausted {
            attempts: 2,
            last_error: Box::new(ApiError::Api {
                status: "413".parse().expect("status"),
                error_type: Some("invalid_request_error".to_string()),
                message: Some("Request is too large for this model's context window.".to_string()),
                request_id: Some("req_ctx_retry_789".to_string()),
                body: String::new(),
                retryable: false,
                suggested_action: None,
                retry_after: None,
            }),
        };

        let rendered = format_user_visible_api_error("session-issue-32", &error);
        assert!(rendered.contains("Context window blocked"), "{rendered}");
        assert!(rendered.contains("context_window_blocked"), "{rendered}");
        assert!(
            rendered.contains("Trace            req_ctx_retry_789"),
            "{rendered}"
        );
        assert!(
            rendered
                .contains("Detail           Request is too large for this model's context window."),
            "{rendered}"
        );
        assert!(rendered.contains("Compact          /compact"), "{rendered}");
        assert!(
            rendered.contains("Resume compact   claw --resume session-issue-32 /compact"),
            "{rendered}"
        );
    }

    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("rusty-claude-cli-{nanos}-{unique}"))
    }

    fn git(args: &[&str], cwd: &Path) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git command should run");
        assert!(
            status.success(),
            "git command failed: git {}",
            args.join(" ")
        );
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn with_current_dir<T>(cwd: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = cwd_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::current_dir().expect("cwd should load");
        std::env::set_current_dir(cwd).expect("cwd should change");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        std::env::set_current_dir(previous).expect("cwd should restore");
        match result {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn write_skill_fixture(root: &Path, name: &str, description: &str) {
        let skill_dir = root.join(name);
        fs::create_dir_all(&skill_dir).expect("skill dir should exist");
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )
        .expect("skill file should write");
    }

    fn write_plugin_fixture(root: &Path, name: &str, include_hooks: bool, include_lifecycle: bool) {
        fs::create_dir_all(root.join(".claude-plugin")).expect("manifest dir");
        if include_hooks {
            fs::create_dir_all(root.join("hooks")).expect("hooks dir");
            fs::write(
                root.join("hooks").join("pre.sh"),
                "#!/bin/sh\nprintf 'plugin pre hook'\n",
            )
            .expect("write hook");
        }
        if include_lifecycle {
            fs::create_dir_all(root.join("lifecycle")).expect("lifecycle dir");
            fs::write(
                root.join("lifecycle").join("init.sh"),
                "#!/bin/sh\nprintf 'init\\n' >> lifecycle.log\n",
            )
            .expect("write init lifecycle");
            fs::write(
                root.join("lifecycle").join("shutdown.sh"),
                "#!/bin/sh\nprintf 'shutdown\\n' >> lifecycle.log\n",
            )
            .expect("write shutdown lifecycle");
        }

        let hooks = if include_hooks {
            ",\n  \"hooks\": {\n    \"PreToolUse\": [\"./hooks/pre.sh\"]\n  }"
        } else {
            ""
        };
        let lifecycle = if include_lifecycle {
            ",\n  \"lifecycle\": {\n    \"Init\": [\"./lifecycle/init.sh\"],\n    \"Shutdown\": [\"./lifecycle/shutdown.sh\"]\n  }"
        } else {
            ""
        };
        fs::write(
            root.join(".claude-plugin").join("plugin.json"),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"1.0.0\",\n  \"description\": \"runtime plugin fixture\"{hooks}{lifecycle}\n}}"
            ),
        )
        .expect("write plugin manifest");
    }
    #[test]
    fn defaults_to_repl_when_no_args() {
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        assert_eq!(
            parse_args(&[]).expect("args should parse"),
            CliAction::Repl {
                model: DEFAULT_MODEL.to_string(),
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn default_permission_mode_uses_project_config_when_env_is_unset() {
        let _guard = env_lock();
        let root = temp_dir();
        let cwd = root.join("project");
        let config_home = root.join("config-home");
        std::fs::create_dir_all(cwd.join(".claw")).expect("project config dir should exist");
        std::fs::create_dir_all(&config_home).expect("config home should exist");
        std::fs::write(
            cwd.join(".claw").join("settings.json"),
            r#"{"permissionMode":"acceptEdits"}"#,
        )
        .expect("project config should write");

        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_permission_mode = std::env::var("RUSTY_CLAUDE_PERMISSION_MODE").ok();
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");

        let resolved = with_current_dir(&cwd, super::default_permission_mode);

        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        match original_permission_mode {
            Some(value) => std::env::set_var("RUSTY_CLAUDE_PERMISSION_MODE", value),
            None => std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE"),
        }
        std::fs::remove_dir_all(root).expect("temp config root should clean up");

        assert_eq!(resolved, PermissionMode::WorkspaceWrite);
    }

    #[test]
    fn env_permission_mode_overrides_project_config_default() {
        let _guard = env_lock();
        let root = temp_dir();
        let cwd = root.join("project");
        let config_home = root.join("config-home");
        std::fs::create_dir_all(cwd.join(".claw")).expect("project config dir should exist");
        std::fs::create_dir_all(&config_home).expect("config home should exist");
        std::fs::write(
            cwd.join(".claw").join("settings.json"),
            r#"{"permissionMode":"acceptEdits"}"#,
        )
        .expect("project config should write");

        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_permission_mode = std::env::var("RUSTY_CLAUDE_PERMISSION_MODE").ok();
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);
        std::env::set_var("RUSTY_CLAUDE_PERMISSION_MODE", "read-only");

        let resolved = with_current_dir(&cwd, super::default_permission_mode);

        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        match original_permission_mode {
            Some(value) => std::env::set_var("RUSTY_CLAUDE_PERMISSION_MODE", value),
            None => std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE"),
        }
        std::fs::remove_dir_all(root).expect("temp config root should clean up");

        assert_eq!(resolved, PermissionMode::ReadOnly);
    }

    #[test]
    fn resolve_cli_auth_source_ignores_saved_oauth_credentials() {
        let _guard = env_lock();
        let config_home = temp_dir();
        std::fs::create_dir_all(&config_home).expect("config home should exist");

        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_api_key = std::env::var("ANTHROPIC_API_KEY").ok();
        let original_auth_token = std::env::var("ANTHROPIC_AUTH_TOKEN").ok();
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");

        save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: "expired-access-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            expires_at: Some(0),
            scopes: vec!["org:create_api_key".to_string(), "user:profile".to_string()],
        })
        .expect("save expired oauth credentials");

        let error = super::resolve_cli_auth_source_for_cwd()
            .expect_err("saved oauth should be ignored without env auth");

        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        match original_api_key {
            Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        match original_auth_token {
            Some(value) => std::env::set_var("ANTHROPIC_AUTH_TOKEN", value),
            None => std::env::remove_var("ANTHROPIC_AUTH_TOKEN"),
        }
        std::fs::remove_dir_all(config_home).expect("temp config home should clean up");

        assert!(error.to_string().contains("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn parses_prompt_subcommand() {
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        let args = vec![
            "prompt".to_string(),
            "hello".to_string(),
            "world".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Prompt {
                prompt: "hello world".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn merge_prompt_with_stdin_returns_prompt_unchanged_when_no_pipe() {
        // given
        let prompt = "Review this";

        // when
        let merged = merge_prompt_with_stdin(prompt, None);

        // then
        assert_eq!(merged, "Review this");
    }

    #[test]
    fn merge_prompt_with_stdin_ignores_whitespace_only_pipe() {
        // given
        let prompt = "Review this";
        let piped = "   \n\t\n  ";

        // when
        let merged = merge_prompt_with_stdin(prompt, Some(piped));

        // then
        assert_eq!(merged, "Review this");
    }

    #[test]
    fn merge_prompt_with_stdin_appends_piped_content_as_context() {
        // given
        let prompt = "Review this";
        let piped = "fn main() { println!(\"hi\"); }\n";

        // when
        let merged = merge_prompt_with_stdin(prompt, Some(piped));

        // then
        assert_eq!(merged, "Review this\n\nfn main() { println!(\"hi\"); }");
    }

    #[test]
    fn merge_prompt_with_stdin_trims_surrounding_whitespace_on_pipe() {
        // given
        let prompt = "Summarize";
        let piped = "\n\n  some notes  \n\n";

        // when
        let merged = merge_prompt_with_stdin(prompt, Some(piped));

        // then
        assert_eq!(merged, "Summarize\n\nsome notes");
    }

    #[test]
    fn merge_prompt_with_stdin_returns_pipe_when_prompt_is_empty() {
        // given
        let prompt = "";
        let piped = "standalone body";

        // when
        let merged = merge_prompt_with_stdin(prompt, Some(piped));

        // then
        assert_eq!(merged, "standalone body");
    }

    #[test]
    fn parses_bare_prompt_and_json_output_flag() {
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        let args = vec![
            "--output-format=json".to_string(),
            "--model".to_string(),
            "opus".to_string(),
            "explain".to_string(),
            "this".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Prompt {
                prompt: "explain this".to_string(),
                model: "anthropic/claude-opus-4-7".to_string(),
                output_format: CliOutputFormat::Json,
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn parses_dash_prefixed_prompt_text_434() {
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");

        assert_eq!(
            parse_args(&["--".to_string(), "-prompt-with-dash".to_string()])
                .expect("-- should terminate flag parsing"),
            CliAction::Prompt {
                prompt: "-prompt-with-dash".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );

        assert_eq!(
            parse_args(&["-not-a-flag".to_string()])
                .expect("unknown dash-prefixed shorthand prompt should parse as prompt text"),
            CliAction::Prompt {
                prompt: "-not-a-flag".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );

        assert_eq!(
            parse_args(&["--bogus-flag-like".to_string(), "literal".to_string()])
                .expect("unknown double-dash text should stay eligible for prompt shorthand"),
            CliAction::Prompt {
                prompt: "--bogus-flag-like literal".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );

        assert!(parse_args(&["--".to_string()]).is_ok());

        let error = parse_args(&["--resum".to_string()])
            .expect_err("nearby real flags should still be rejected as unknown options");
        assert!(error.contains("unknown option: --resum"));
        assert!(error.contains("Did you mean --resume?"));
    }

    #[test]
    fn parses_compact_flag_for_prompt_mode() {
        // given a bare prompt invocation that includes the --compact flag
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        let args = vec![
            "--compact".to_string(),
            "summarize".to_string(),
            "this".to_string(),
        ];

        // when parse_args interprets the flag
        let parsed = parse_args(&args).expect("args should parse");

        // then compact mode is propagated and other defaults stay unchanged
        assert_eq!(
            parsed,
            CliAction::Prompt {
                prompt: "summarize this".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                compact: true,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
        assert_eq!(
            parse_args(&["--compact".to_string(), "hello".to_string()])
                .expect("compact single-word prompt should parse"),
            CliAction::Prompt {
                prompt: "hello".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                compact: true,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn prompt_subcommand_defaults_compact_to_false() {
        // given a `prompt` subcommand invocation without --compact
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        let args = vec!["prompt".to_string(), "hello".to_string()];

        // when parse_args runs
        let parsed = parse_args(&args).expect("args should parse");

        // then compact stays false (opt-in flag)
        match parsed {
            CliAction::Prompt { compact, .. } => assert!(!compact),
            other => panic!("expected Prompt action, got {other:?}"),
        }
    }

    #[test]
    fn resolves_model_aliases_in_args() {
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        let args = vec![
            "--model".to_string(),
            "opus".to_string(),
            "explain".to_string(),
            "this".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Prompt {
                prompt: "explain this".to_string(),
                model: "anthropic/claude-opus-4-7".to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn resolves_known_model_aliases() {
        assert_eq!(resolve_model_alias("opus"), "anthropic/claude-opus-4-7");
        assert_eq!(resolve_model_alias("sonnet"), "anthropic/claude-sonnet-4-6");
        assert_eq!(
            resolve_model_alias("haiku"),
            "anthropic/claude-haiku-4-5-20251213"
        );
        assert_eq!(resolve_model_alias("claude-opus"), "claude-opus");
    }

    #[test]
    fn default_model_alias_uses_anthropic_routing_prefix() {
        assert_eq!(DEFAULT_MODEL, "anthropic/claude-opus-4-7");
        assert_eq!(resolve_model_alias("opus"), "anthropic/claude-opus-4-7");
    }

    #[test]
    fn user_defined_aliases_resolve_before_provider_dispatch() {
        // given
        let _guard = env_lock();
        let root = temp_dir();
        let cwd = root.join("project");
        let config_home = root.join("config-home");
        std::fs::create_dir_all(cwd.join(".claw")).expect("project config dir should exist");
        std::fs::create_dir_all(&config_home).expect("config home should exist");
        std::fs::write(
            cwd.join(".claw").join("settings.json"),
            r#"{"aliases":{"fast":"anthropic/claude-haiku-4-5-20251213","smart":"opus","cheap":"grok-3-mini"}}"#,
        )
        .expect("project config should write");

        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);

        // when
        let direct = with_current_dir(&cwd, || resolve_model_alias_with_config("fast"));
        let chained = with_current_dir(&cwd, || resolve_model_alias_with_config("smart"));
        let cross_provider = with_current_dir(&cwd, || resolve_model_alias_with_config("cheap"));
        let unknown = with_current_dir(&cwd, || resolve_model_alias_with_config("unknown-model"));
        let builtin = with_current_dir(&cwd, || resolve_model_alias_with_config("haiku"));

        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        std::fs::remove_dir_all(root).expect("temp config root should clean up");

        // then
        assert_eq!(direct, "anthropic/claude-haiku-4-5-20251213");
        assert_eq!(chained, "anthropic/claude-opus-4-7");
        assert_eq!(cross_provider, "grok-3-mini");
        assert_eq!(unknown, "unknown-model");
        assert_eq!(builtin, "anthropic/claude-haiku-4-5-20251213");
    }

    #[test]
    fn parses_version_flags_without_initializing_prompt_mode() {
        assert_eq!(
            parse_args(&["--version".to_string()]).expect("args should parse"),
            CliAction::Version {
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["-V".to_string()]).expect("args should parse"),
            CliAction::Version {
                output_format: CliOutputFormat::Text,
            }
        );
    }

    #[test]
    fn parses_permission_mode_flag() {
        let args = vec!["--permission-mode=read-only".to_string()];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Repl {
                model: DEFAULT_MODEL.to_string(),
                allowed_tools: None,
                permission_mode: PermissionMode::ReadOnly,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn dangerously_skip_permissions_flag_forces_danger_full_access_in_repl() {
        let _guard = env_lock();
        std::env::set_var("RUSTY_CLAUDE_PERMISSION_MODE", "read-only");
        let args = vec!["--dangerously-skip-permissions".to_string()];
        let parsed = parse_args(&args).expect("args should parse");
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");

        assert_eq!(
            parsed,
            CliAction::Repl {
                model: DEFAULT_MODEL.to_string(),
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn dangerously_skip_permissions_flag_applies_to_prompt_subcommand() {
        let _guard = env_lock();
        std::env::set_var("RUSTY_CLAUDE_PERMISSION_MODE", "read-only");
        let args = vec![
            "--dangerously-skip-permissions".to_string(),
            "prompt".to_string(),
            "do".to_string(),
            "the".to_string(),
            "thing".to_string(),
        ];
        let parsed = parse_args(&args).expect("args should parse");
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");

        assert_eq!(
            parsed,
            CliAction::Prompt {
                prompt: "do the thing".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::DangerFullAccess,
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn parses_allowed_tools_flags_with_aliases_and_lists() {
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        let args = vec![
            "--allowedTools".to_string(),
            "read,glob".to_string(),
            "--allowed-tools=write_file".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::Repl {
                model: DEFAULT_MODEL.to_string(),
                allowed_tools: Some(
                    ["glob_search", "read_file", "write_file"]
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                ),
                permission_mode: PermissionMode::WorkspaceWrite,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn rejects_allowed_tools_followed_by_subcommand_or_flag_432() {
        let _env_guard = env_lock();
        let _cwd_guard = cwd_guard();
        for args in [
            vec!["--allowedTools".to_string(), "status".to_string()],
            vec![
                "--allowedTools".to_string(),
                "status".to_string(),
                "--output-format".to_string(),
                "json".to_string(),
            ],
            vec!["--allowedTools".to_string(), "--output-format".to_string()],
            vec!["--allowedTools=".to_string()],
        ] {
            let error = parse_args(&args).expect_err("allowedTools missing value should reject");
            assert!(
                error.starts_with("missing_argument: --allowedTools requires a tool list"),
                "unexpected error for {args:?}: {error}"
            );
        }
    }

    #[test]
    fn rejects_unknown_allowed_tools() {
        let _env_guard = env_lock();
        let _cwd_guard = cwd_guard();
        let error = parse_args(&["--allowedTools".to_string(), "teleport".to_string()])
            .expect_err("tool should be rejected");
        assert!(error.starts_with("invalid_tool_name:"));
        assert!(error.contains("unsupported tool in --allowedTools: teleport"));
        assert!(error.contains("Available: "));
        assert!(error.contains("web_fetch"));
        assert!(error.contains("Aliases: "));
        assert!(error.contains("WebFetch=web_fetch"));
    }

    #[test]
    fn rejects_empty_allowed_tools_flag() {
        let _env_guard = env_lock();
        let _cwd_guard = cwd_guard();
        for raw in ["", ",,"] {
            let error = parse_args(&["--allowedTools".to_string(), raw.to_string()])
                .expect_err("empty allowedTools should be rejected");
            assert!(
                error.contains("--allowedTools was provided with no usable tool names"),
                "unexpected error for {raw:?}: {error}"
            );
        }
    }

    #[test]
    fn parses_system_prompt_options() {
        // given: system-prompt options for cwd and date
        let args = vec![
            "system-prompt".to_string(),
            "--cwd".to_string(),
            "/tmp".to_string(),
            "--date".to_string(),
            "2026-04-01".to_string(),
        ];

        // when: parsing the direct system-prompt command
        let action = parse_args(&args).expect("args should parse");

        // then: the action carries prompt options and default model
        assert_eq!(
            action,
            CliAction::PrintSystemPrompt {
                cwd: PathBuf::from("/tmp"),
                date: "2026-04-01".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
            }
        );
    }

    #[test]
    fn parses_global_model_for_system_prompt() {
        // given: a global OpenAI-compatible model before system-prompt
        let args = vec![
            "--model".to_string(),
            "openai/gpt-4.1-mini".to_string(),
            "system-prompt".to_string(),
        ];

        // when: parsing the CLI arguments
        let action = parse_args(&args).expect("args should parse");

        // then: the system-prompt action carries the selected model
        match action {
            CliAction::PrintSystemPrompt { model, .. } => {
                assert_eq!(model, "openai/gpt-4.1-mini");
            }
            other => panic!("expected PrintSystemPrompt, got {other:?}"),
        }
    }

    #[test]
    fn removed_login_and_logout_subcommands_error_helpfully() {
        let login = parse_args(&["login".to_string()]).expect_err("login should be removed");
        assert!(login.contains("ANTHROPIC_API_KEY"));
        let logout = parse_args(&["logout".to_string()]).expect_err("logout should be removed");
        assert!(logout.contains("ANTHROPIC_AUTH_TOKEN"));
        assert_eq!(
            parse_args(&["doctor".to_string()]).expect("doctor should parse"),
            CliAction::Doctor {
                output_format: CliOutputFormat::Text,
                permission_mode: PermissionModeProvenance::default_fallback(),
            }
        );
        assert_eq!(
            parse_args(&["state".to_string()]).expect("state should parse"),
            CliAction::State {
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&[
                "state".to_string(),
                "--output-format".to_string(),
                "json".to_string()
            ])
            .expect("state --output-format json should parse"),
            CliAction::State {
                output_format: CliOutputFormat::Json,
            }
        );
        assert_eq!(
            parse_args(&["init".to_string()]).expect("init should parse"),
            CliAction::Init {
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["agents".to_string()]).expect("agents should parse"),
            CliAction::Agents {
                args: None,
                output_format: CliOutputFormat::Text
            }
        );
        assert_eq!(
            parse_args(&["mcp".to_string()]).expect("mcp should parse"),
            CliAction::Mcp {
                args: None,
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["skills".to_string()]).expect("skills should parse"),
            CliAction::Skills {
                args: None,
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&[
                "skills".to_string(),
                "help".to_string(),
                "overview".to_string()
            ])
            .expect("skills help overview should invoke"),
            CliAction::Prompt {
                prompt: "$help overview".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: crate::default_permission_mode(),
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
        assert_eq!(
            parse_args(&["agents".to_string(), "--help".to_string()])
                .expect("agents help should parse"),
            CliAction::Agents {
                args: Some("--help".to_string()),
                output_format: CliOutputFormat::Text,
            }
        );
        // #145: `plugins` must parse as CliAction::Plugins (not fall through
        // to the prompt path, which would hit the Anthropic API for a purely
        // local introspection command).
        assert_eq!(
            parse_args(&["plugins".to_string()]).expect("plugins should parse"),
            CliAction::Plugins {
                action: None,
                target: None,
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["plugins".to_string(), "list".to_string()])
                .expect("plugins list should parse"),
            CliAction::Plugins {
                action: Some("list".to_string()),
                target: None,
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&[
                "plugins".to_string(),
                "enable".to_string(),
                "example-bundled".to_string(),
            ])
            .expect("plugins enable <target> should parse"),
            CliAction::Plugins {
                action: Some("enable".to_string()),
                target: Some("example-bundled".to_string()),
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&[
                "plugins".to_string(),
                "--output-format".to_string(),
                "json".to_string(),
            ])
            .expect("plugins --output-format json should parse"),
            CliAction::Plugins {
                action: None,
                target: None,
                output_format: CliOutputFormat::Json,
            }
        );
        for alias in ["plugin", "marketplace"] {
            assert_eq!(
                parse_args(&[alias.to_string()]).expect("plugin alias should parse"),
                CliAction::Plugins {
                    action: None,
                    target: None,
                    output_format: CliOutputFormat::Text,
                },
                "{alias} should route to local plugin handling, not Prompt"
            );
            assert_eq!(
                parse_args(&[alias.to_string(), "list".to_string()])
                    .expect("plugin alias list should parse"),
                CliAction::Plugins {
                    action: Some("list".to_string()),
                    target: None,
                    output_format: CliOutputFormat::Text,
                },
                "{alias} list should route to local plugin handling, not Prompt"
            );
            assert_eq!(
                parse_args(&[
                    alias.to_string(),
                    "install".to_string(),
                    "./fixtures/plugin-demo".to_string(),
                ])
                .expect("plugin alias install should parse"),
                CliAction::Plugins {
                    action: Some("install".to_string()),
                    target: Some("./fixtures/plugin-demo".to_string()),
                    output_format: CliOutputFormat::Text,
                },
                "{alias} install should route to local plugin handling, not Prompt"
            );
        }
        // #146: `config` and `diff` must parse as standalone CLI actions,
        // not fall through to the "is a slash command" error. Both are
        // pure-local read-only introspection.
        assert_eq!(
            parse_args(&["config".to_string()]).expect("config should parse"),
            CliAction::Config {
                section: None,
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["config".to_string(), "env".to_string()])
                .expect("config env should parse"),
            CliAction::Config {
                section: Some("env".to_string()),
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&[
                "config".to_string(),
                "--output-format".to_string(),
                "json".to_string(),
            ])
            .expect("config --output-format json should parse"),
            CliAction::Config {
                section: None,
                output_format: CliOutputFormat::Json,
            }
        );
        assert_eq!(
            parse_args(&["diff".to_string()]).expect("diff should parse"),
            CliAction::Diff {
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&[
                "diff".to_string(),
                "--output-format".to_string(),
                "json".to_string(),
            ])
            .expect("diff --output-format json should parse"),
            CliAction::Diff {
                output_format: CliOutputFormat::Json,
            }
        );
        // #147: empty / whitespace-only positional args must be rejected
        // with a specific error instead of falling through to the prompt
        // path (where they surface a misleading "missing Anthropic
        // credentials" error or burn API tokens on an empty prompt).
        let empty_err =
            parse_args(&["".to_string()]).expect_err("empty positional arg should be rejected");
        assert!(
            empty_err.starts_with("empty prompt:"),
            "empty-arg error should be specific, got: {empty_err}"
        );
        let whitespace_err = parse_args(&["   ".to_string()])
            .expect_err("whitespace-only positional arg should be rejected");
        assert!(
            whitespace_err.starts_with("empty prompt:"),
            "whitespace-only error should be specific, got: {whitespace_err}"
        );
        let multi_empty_err = parse_args(&["".to_string(), "".to_string()])
            .expect_err("multiple empty positional args should be rejected");
        assert!(
            multi_empty_err.starts_with("empty prompt:"),
            "multi-empty error should be specific, got: {multi_empty_err}"
        );
        // Typo guard from #108 must still take precedence for non-empty
        // single-word non-prompt-looking inputs.
        let typo_err = parse_args(&["sttaus".to_string()])
            .expect_err("typo'd subcommand should be caught by #108 guard");
        assert!(
            typo_err.contains("unknown subcommand:"),
            "typo guard should fire for 'sttaus', got: {typo_err}"
        );
        // #148: `--model` flag must be captured as model_flag_raw so status
        // JSON can report provenance (source: flag, raw: <user-input>).
        match parse_args(&[
            "--model".to_string(),
            "sonnet".to_string(),
            "status".to_string(),
        ])
        .expect("--model sonnet status should parse")
        {
            CliAction::Status {
                model,
                model_flag_raw,
                ..
            } => {
                assert_eq!(
                    model, "anthropic/claude-sonnet-4-6",
                    "sonnet alias should resolve"
                );
                assert_eq!(
                    model_flag_raw.as_deref(),
                    Some("sonnet"),
                    "raw flag input should be preserved"
                );
            }
            other => panic!("expected CliAction::Status, got: {other:?}"),
        }
        // --model= form should also capture raw.
        match parse_args(&[
            "--model=anthropic/claude-opus-4-6".to_string(),
            "status".to_string(),
        ])
        .expect("--model=... status should parse")
        {
            CliAction::Status {
                model,
                model_flag_raw,
                ..
            } => {
                assert_eq!(model, "anthropic/claude-opus-4-6");
                assert_eq!(
                    model_flag_raw.as_deref(),
                    Some("anthropic/claude-opus-4-6"),
                    "--model= form should also preserve raw input"
                );
            }
            other => panic!("expected CliAction::Status, got: {other:?}"),
        }
        match parse_args(&["--model=claude-opus-4-6".to_string(), "status".to_string()])
            .expect("bare Anthropic model should parse")
        {
            CliAction::Status {
                model,
                model_flag_raw,
                ..
            } => {
                assert_eq!(model, "claude-opus-4-6");
                assert_eq!(model_flag_raw.as_deref(), Some("claude-opus-4-6"));
            }
            other => panic!("expected CliAction::Status, got: {other:?}"),
        }
    }

    #[test]
    fn dump_manifests_subcommand_accepts_explicit_manifest_dir() {
        assert_eq!(
            parse_args(&[
                "dump-manifests".to_string(),
                "--manifests-dir".to_string(),
                "/tmp/upstream".to_string(),
            ])
            .expect("dump-manifests should parse"),
            CliAction::DumpManifests {
                output_format: CliOutputFormat::Text,
                manifests_dir: Some(PathBuf::from("/tmp/upstream")),
            }
        );
        assert_eq!(
            parse_args(&[
                "dump-manifests".to_string(),
                "--manifests-dir=/tmp/upstream".to_string()
            ])
            .expect("inline dump-manifests flag should parse"),
            CliAction::DumpManifests {
                output_format: CliOutputFormat::Text,
                manifests_dir: Some(PathBuf::from("/tmp/upstream")),
            }
        );
    }

    #[test]
    fn parses_acp_command_surfaces() {
        assert_eq!(
            parse_args(&["acp".to_string()]).expect("acp should parse"),
            CliAction::Acp {
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["acp".to_string(), "serve".to_string()]).expect("acp serve should parse"),
            CliAction::Acp {
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["--acp".to_string()]).expect("--acp should parse"),
            CliAction::Acp {
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["-acp".to_string()]).expect("-acp should parse"),
            CliAction::Acp {
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&[
                "acp".to_string(),
                "serve".to_string(),
                "--output-format".to_string(),
                "json".to_string()
            ])
            .expect("acp serve json should parse"),
            CliAction::Acp {
                output_format: CliOutputFormat::Json,
            }
        );
        let unsupported = parse_args(&["acp".to_string(), "start".to_string()])
            .expect_err("unknown ACP subcommand should fail with a typed contract");
        assert!(unsupported.contains("unsupported ACP invocation"));
    }

    #[test]
    fn acp_status_json_is_truthful_unsupported_contract() {
        let value = acp_status_json();
        assert_eq!(value["schema_version"], "1.0");
        assert_eq!(value["kind"], "acp");
        assert_eq!(value["status"], "not_implemented");
        assert_eq!(value["supported"], false);
        assert_eq!(value["protocol"]["json_rpc"], false);
        assert_eq!(value["protocol"]["daemon"], false);
        assert_eq!(value["protocol"]["serve_starts_daemon"], false);
        assert!(value["protocol"]["endpoint"].is_null());
        assert_eq!(
            value["contracts"]["unsupported_invocation_kind"],
            "unsupported_acp_invocation"
        );
    }

    #[test]
    fn local_command_help_flags_stay_on_the_local_parser_path() {
        assert_eq!(
            parse_args(&["status".to_string(), "--help".to_string()])
                .expect("status help should parse"),
            CliAction::HelpTopic {
                topic: LocalHelpTopic::Status,
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["sandbox".to_string(), "-h".to_string()])
                .expect("sandbox help should parse"),
            CliAction::HelpTopic {
                topic: LocalHelpTopic::Sandbox,
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["doctor".to_string(), "--help".to_string()])
                .expect("doctor help should parse"),
            CliAction::HelpTopic {
                topic: LocalHelpTopic::Doctor,
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["acp".to_string(), "--help".to_string()]).expect("acp help should parse"),
            CliAction::HelpTopic {
                topic: LocalHelpTopic::Acp,
                output_format: CliOutputFormat::Text,
            }
        );
    }

    #[test]
    fn subcommand_help_flag_has_one_contract_across_all_subcommands_141() {
        // #141: every documented subcommand must resolve `<subcommand> --help`
        // to a subcommand-specific help topic, never to global help, never to
        // an "unknown option" error, never to the subcommand's primary output.
        let cases: &[(&str, LocalHelpTopic)] = &[
            ("status", LocalHelpTopic::Status),
            ("sandbox", LocalHelpTopic::Sandbox),
            ("doctor", LocalHelpTopic::Doctor),
            ("acp", LocalHelpTopic::Acp),
            ("init", LocalHelpTopic::Init),
            ("state", LocalHelpTopic::State),
            ("export", LocalHelpTopic::Export),
            ("version", LocalHelpTopic::Version),
            ("system-prompt", LocalHelpTopic::SystemPrompt),
            ("dump-manifests", LocalHelpTopic::DumpManifests),
            ("bootstrap-plan", LocalHelpTopic::BootstrapPlan),
        ];
        for (subcommand, expected_topic) in cases {
            for flag in ["--help", "-h"] {
                let parsed = parse_args(&[subcommand.to_string(), flag.to_string()])
                    .unwrap_or_else(|error| {
                        panic!("`{subcommand} {flag}` should parse as help but errored: {error}")
                    });
                assert_eq!(
                    parsed,
                    CliAction::HelpTopic {
                        topic: *expected_topic,
                        output_format: CliOutputFormat::Text,
                    },
                    "`{subcommand} {flag}` should resolve to HelpTopic({expected_topic:?})"
                );
            }
            let json_parsed = parse_args(&[
                subcommand.to_string(),
                "--help".to_string(),
                "--output-format".to_string(),
                "json".to_string(),
            ])
            .unwrap_or_else(|error| {
                panic!("`{subcommand} --help --output-format json` should parse: {error}")
            });
            assert_eq!(
                json_parsed,
                CliAction::HelpTopic {
                    topic: *expected_topic,
                    output_format: CliOutputFormat::Json,
                },
                "`{subcommand} --help --output-format json` should preserve json output format"
            );
            // And the rendered help must actually mention the subcommand name
            // (or its canonical title) so users know they got the right help.
            let rendered = render_help_topic(*expected_topic);
            assert!(
                !rendered.is_empty(),
                "{subcommand} help text should not be empty"
            );
            assert!(
                rendered.contains("Usage"),
                "{subcommand} help text should contain a Usage line"
            );
        }
    }

    #[test]
    fn export_help_json_is_bounded_and_parseable_384() {
        let value = render_help_topic_json(LocalHelpTopic::Export);
        assert_eq!(value["kind"], "help");
        assert_eq!(value["topic"], "export");
        assert_eq!(value["command"], "export");
        assert_eq!(
            value["usage"],
            "claw export [--session <id|latest>] [--output <path>] [--output-format <format>]"
        );
        assert_eq!(value["defaults"]["session"], LATEST_SESSION_REFERENCE);
        assert!(value["options"].as_array().expect("options array").len() >= 4);
        assert!(
            value.get("message").is_none(),
            "export help json should be a bounded envelope, not plaintext help wrapped in json"
        );
    }

    #[test]
    fn plugins_degrades_on_invalid_mcp_server_without_global_config_error_440() {
        // #440: invalid MCP entries should not make local plugin introspection
        // unusable, and should surface as validation metadata instead of a
        // whole-config parse failure.
        let _guard = env_lock();
        let root = temp_dir();
        let cwd = root.join("project-with-malformed-mcp-for-plugins");
        let config_home = root.join("config-home");
        std::fs::create_dir_all(&cwd).expect("project dir should exist");
        std::fs::create_dir_all(&config_home).expect("config home should exist");
        std::fs::write(
            cwd.join(".claw.json"),
            r#"{
  "mcpServers": {
    "missing-command": {"args": ["arg-only-no-command"]}
  }
}
"#,
        )
        .expect("write malformed .claw.json");

        let previous_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);
        let payload = super::plugins_command_payload_for(
            &cwd,
            None,
            None,
            super::ConfigWarningMode::EmitStderr,
        )
        .expect("plugins list should not hard-fail on malformed MCP config");
        match previous_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }

        assert_eq!(payload.status, "degraded");
        assert!(payload.config_load_error.is_none());
        assert_eq!(payload.mcp_validation.total_configured, 1);
        assert_eq!(payload.mcp_validation.valid_count, 0);
        assert_eq!(payload.mcp_validation.invalid_count(), 1);
        assert_eq!(
            payload.mcp_validation.invalid_servers[0].name,
            "missing-command"
        );
        assert!(payload.mcp_validation.invalid_servers[0]
            .reason
            .contains("missing string field command"));
        assert!(payload.message.contains("MCP validation"));
        assert!(payload.message.contains("valid MCP siblings only"));
        assert!(payload.message.contains("Plugins"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn status_degrades_gracefully_on_malformed_mcp_config_143() {
        // #143: previously `claw status` hard-failed on any config parse error,
        // taking down the entire health surface for one malformed MCP entry.
        // `claw doctor` already degrades gracefully; this test locks `status`
        // to the same contract.
        let _guard = env_lock();
        let root = temp_dir();
        let cwd = root.join("project-with-malformed-mcp");
        std::fs::create_dir_all(&cwd).expect("project dir should exist");
        // Top-level `mcpServers` shape errors still degrade through the
        // config_load_error path; per-server errors are handled by the #440
        // MCP validation summary instead.
        std::fs::write(
            cwd.join(".claw.json"),
            r#"{
  "mcpServers": "not-an-object"
}
"#,
        )
        .expect("write malformed .claw.json");

        let context = with_current_dir(&cwd, || {
            super::status_context(None)
                .expect("status_context should not hard-fail on config parse errors (#143)")
        });

        // Config-shape errors still populate config_load_error.
        let err = context
            .config_load_error
            .as_ref()
            .expect("config_load_error should be Some when config shape parsing fails");
        assert!(
            err.contains("mcpServers"),
            "config_load_error should name the malformed mcpServers path: {err}"
        );
        assert!(
            err.contains("must be an object"),
            "config_load_error should carry the underlying parse error: {err}"
        );

        // Phase 1 contract: workspace/git/sandbox fields are still populated
        // (independent of config parse). Sandbox falls back to defaults.
        assert_eq!(context.cwd, cwd.canonicalize().unwrap_or(cwd.clone()));
        assert_eq!(
            context.loaded_config_files, 0,
            "loaded_config_files should be 0 when config parse fails"
        );
        assert!(
            context.discovered_config_files > 0,
            "discovered_config_files should still count the file that failed to parse"
        );

        // JSON output contract: top-level `status: "degraded"` + config_load_error field.
        let usage = super::StatusUsage {
            message_count: 0,
            turns: 0,
            latest: runtime::TokenUsage::default(),
            cumulative: runtime::TokenUsage::default(),
            estimated_tokens: 0,
        };
        let json = super::status_json_value(
            Some("test-model"),
            usage,
            "workspace-write",
            &context,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            json.get("status").and_then(|v| v.as_str()),
            Some("degraded"),
            "top-level status marker should be 'degraded' when config parse failed: {json}"
        );
        assert!(
            json.get("config_load_error")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("mcpServers")),
            "config_load_error should surface in JSON output: {json}"
        );
        // Independent fields still populated.
        assert_eq!(
            json.get("model").and_then(|v| v.as_str()),
            Some("test-model")
        );
        assert!(
            json.get("workspace").is_some(),
            "workspace field still reported"
        );
        assert_eq!(
            json.pointer("/lane_board/status_json_supported")
                .and_then(|v| v.as_bool()),
            Some(true),
            "status JSON should advertise lane board support: {json}"
        );
        assert_eq!(
            json.pointer("/lane_board/freshness_states/2")
                .and_then(|v| v.as_str()),
            Some("transport_dead"),
            "status JSON should advertise transport-dead freshness: {json}"
        );
        assert!(
            json.get("sandbox").is_some(),
            "sandbox field still reported"
        );
        assert_eq!(
            json.pointer("/allowed_tools/source")
                .and_then(|v| v.as_str()),
            Some("default"),
            "default status should expose unrestricted tool source: {json}"
        );
        assert_eq!(
            json.pointer("/allowed_tools/restricted")
                .and_then(|v| v.as_bool()),
            Some(false),
            "default status should expose unrestricted tool state: {json}"
        );
        assert_eq!(
            json.pointer("/allowed_tools/available/0")
                .and_then(|v| v.as_str()),
            Some("agent"),
            "status JSON should expose canonical snake_case available tools: {json}"
        );
        assert_eq!(
            json.pointer("/allowed_tools/aliases/WebFetch")
                .and_then(|v| v.as_str()),
            Some("web_fetch"),
            "status JSON should expose allowed-tool aliases: {json}"
        );

        let allowed: super::AllowedToolSet = ["read_file", "grep_search"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let restricted_json = super::status_json_value(
            Some("test-model"),
            usage,
            "workspace-write",
            &context,
            None,
            None,
            Some(&allowed),
            None,
        );
        assert_eq!(
            restricted_json
                .pointer("/allowed_tools/source")
                .and_then(|v| v.as_str()),
            Some("flag"),
            "flag status should expose allow-list source: {restricted_json}"
        );
        assert_eq!(
            restricted_json
                .pointer("/allowed_tools/entries")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(2),
            "flag status should expose allow-list entries: {restricted_json}"
        );

        // Clean path: no config error → status: "ok", config_load_error: null.
        let clean_cwd = root.join("project-with-clean-config");
        std::fs::create_dir_all(&clean_cwd).expect("clean project dir");
        let clean_context = with_current_dir(&clean_cwd, || {
            super::status_context(None).expect("clean status_context should succeed")
        });
        assert!(clean_context.config_load_error.is_none());
        let clean_json = super::status_json_value(
            Some("test-model"),
            usage,
            "workspace-write",
            &clean_context,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            clean_json.get("status").and_then(|v| v.as_str()),
            Some("ok"),
            "clean run should report status: 'ok'"
        );
    }

    #[test]
    fn state_error_surfaces_actionable_worker_commands_139() {
        // #139: the error for missing `.claw/worker-state.json` must name
        // the concrete commands that produce worker state, otherwise claws
        // have no discoverable path from the error to a fix.
        let _guard = env_lock();
        let root = temp_dir();
        let cwd = root.join("project-with-no-state");
        std::fs::create_dir_all(&cwd).expect("project dir should exist");

        let error = with_current_dir(&cwd, || {
            super::run_worker_state(CliOutputFormat::Text).expect_err("missing state should error")
        });
        let message = error.to_string();

        // Keep the original locator so scripts grepping for it still work.
        assert!(
            message.contains("no worker state file found at"),
            "error should keep the canonical prefix: {message}"
        );
        // New actionable hints — this is what #139 is fixing.
        assert!(
            message.contains("claw prompt"),
            "error should name `claw prompt <text>` as a producer: {message}"
        );
        assert!(
            message.contains("REPL"),
            "error should mention the interactive REPL as a producer: {message}"
        );
        assert!(
            message.contains("claw state"),
            "error should tell the user what to rerun once state exists: {message}"
        );
        // And the State --help topic must document the worker relationship
        // so claws can discover the contract without hitting the error first.
        let state_help = render_help_topic(LocalHelpTopic::State);
        assert!(
            state_help.contains("Produces state"),
            "state help must document how state is produced: {state_help}"
        );
        assert!(
            state_help.contains("claw prompt"),
            "state help must name `claw prompt <text>` as a producer: {state_help}"
        );
    }

    #[test]
    fn parses_single_word_command_aliases_without_falling_back_to_prompt_mode() {
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        assert_eq!(
            parse_args(&["help".to_string()]).expect("help should parse"),
            CliAction::Help {
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["version".to_string()]).expect("version should parse"),
            CliAction::Version {
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["status".to_string()]).expect("status should parse"),
            CliAction::Status {
                model: DEFAULT_MODEL.to_string(),
                model_flag_raw: None, // #148: no --model flag passed
                permission_mode: PermissionModeProvenance::default_fallback(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
            }
        );
        assert_eq!(
            parse_args(&["sandbox".to_string()]).expect("sandbox should parse"),
            CliAction::Sandbox {
                output_format: CliOutputFormat::Text,
            }
        );
        // #152: `--json` on diagnostic verbs should hint the correct flag.
        let err = parse_args(&["doctor".to_string(), "--json".to_string()])
            .expect_err("`doctor --json` should fail with hint");
        assert!(
            err.contains("unrecognized argument `--json` for subcommand `doctor`"),
            "error should name the verb: {err}"
        );
        assert!(
            err.contains("Did you mean `--output-format json`?"),
            "error should hint the correct flag: {err}"
        );
        // Other unrecognized args should NOT trigger the --json hint.
        let err_other = parse_args(&["doctor".to_string(), "garbage".to_string()])
            .expect_err("`doctor garbage` should fail without --json hint");
        assert!(
            !err_other.contains("--output-format json"),
            "unrelated args should not trigger --json hint: {err_other}"
        );
        // #424: bare canonical GPT model ids should parse and route via provider
        // detection instead of forcing the local-only `openai/` routing prefix.
        match parse_args(&[
            "prompt".to_string(),
            "test".to_string(),
            "--model".to_string(),
            "gpt-4".to_string(),
        ])
        .expect("`--model gpt-4` should parse as a bare OpenAI model")
        {
            CliAction::Prompt { model, .. } => assert_eq!(model, "gpt-4"),
            other => panic!("expected CliAction::Prompt, got: {other:?}"),
        }
        let err_qwen = parse_args(&[
            "prompt".to_string(),
            "test".to_string(),
            "--model".to_string(),
            "qwen-plus".to_string(),
        ])
        .expect_err("`--model qwen-plus` should fail with DashScope hint");
        assert!(
            err_qwen.contains("Did you mean `qwen/qwen-plus`?"),
            "Qwen model error should hint qwen/ prefix: {err_qwen}"
        );
        assert!(
            err_qwen.contains("DASHSCOPE_API_KEY"),
            "Qwen model error should mention env var: {err_qwen}"
        );
        // Unrelated invalid model should NOT get a hint
        let err_garbage = parse_args(&[
            "prompt".to_string(),
            "test".to_string(),
            "--model".to_string(),
            "asdfgh".to_string(),
        ])
        .expect_err("`--model asdfgh` should fail");
        assert!(
            !err_garbage.contains("Did you mean"),
            "Unrelated model errors should not get a hint: {err_garbage}"
        );

        let original_openai_base_url = std::env::var_os("OPENAI_BASE_URL");
        std::env::set_var("OPENAI_BASE_URL", "http://127.0.0.1:11434/v1");
        match parse_args(&[
            "prompt".to_string(),
            "test".to_string(),
            "--model".to_string(),
            "qwen2.5-coder:7b".to_string(),
        ])
        .expect("Ollama-style tag should parse when OPENAI_BASE_URL is set")
        {
            CliAction::Prompt { model, .. } => assert_eq!(model, "qwen2.5-coder:7b"),
            other => panic!("expected CliAction::Prompt, got: {other:?}"),
        }
        match parse_args(&[
            "prompt".to_string(),
            "test".to_string(),
            "--model".to_string(),
            "local/Qwen/Qwen3.6-27B-FP8".to_string(),
        ])
        .expect("local/ slash-containing model should parse")
        {
            CliAction::Prompt { model, .. } => assert_eq!(model, "local/Qwen/Qwen3.6-27B-FP8"),
            other => panic!("expected CliAction::Prompt, got: {other:?}"),
        }
        match original_openai_base_url {
            Some(value) => std::env::set_var("OPENAI_BASE_URL", value),
            None => std::env::remove_var("OPENAI_BASE_URL"),
        }
    }

    #[test]
    fn classify_error_kind_returns_correct_discriminants() {
        // #77: error kind classification for JSON error payloads
        assert_eq!(
            classify_error_kind("missing Anthropic credentials; export ..."),
            "missing_credentials"
        );
        assert_eq!(
            classify_error_kind("no worker state file found at /tmp/..."),
            "missing_worker_state"
        );
        assert_eq!(
            classify_error_kind("session not found: abc123"),
            "session_not_found"
        );
        // #780: "no managed sessions found" is more specific than generic "failed to restore"
        // session_load_failed; the reordered classifier now correctly returns no_managed_sessions.
        assert_eq!(
            classify_error_kind("failed to restore session: no managed sessions found"),
            "no_managed_sessions"
        );
        // Bare session load failures that aren't no_managed_sessions or legacy_binding still map here
        assert_eq!(
            classify_error_kind("failed to restore session: file not found"),
            "session_load_failed"
        );
        // #787: directory-as-session-path gets its own kind (precedes generic session_load_failed)
        assert_eq!(
            classify_error_kind("failed to restore session: Is a directory (os error 21)"),
            "session_path_is_directory"
        );
        assert_eq!(
            classify_error_kind("unrecognized argument `--foo` for subcommand `doctor`"),
            "cli_parse"
        );
        // #785/#825: unknown top-level subcommand (typo or unrecognised command)
        assert_eq!(
            classify_error_kind("unknown subcommand: dump.\nDid you mean     dump-manifests"),
            "command_not_found" // #825: unified from unknown_subcommand
        );
        assert_eq!(
            classify_error_kind("unsupported ACP invocation. Use `claw acp`."),
            "unsupported_acp_invocation"
        );
        assert_eq!(
            classify_error_kind("invalid model syntax: 'gpt-4'. Expected ..."),
            "invalid_model_syntax"
        );
        assert_eq!(
            classify_error_kind("unsupported resumed command: /blargh"),
            "unsupported_resumed_command"
        );
        assert_eq!(
            classify_error_kind("api failed after 3 attempts: ..."),
            "api_http_error"
        );
        assert_eq!(
            classify_error_kind("/tmp/settings.json: mcpServers.foo: expected JSON object"),
            "malformed_mcp_config"
        );
        assert_eq!(
            classify_error_kind("settings.json: mcpServers: field must be an object"),
            "malformed_mcp_config"
        );
        assert_eq!(
            classify_error_kind("empty prompt: provide a subcommand or a non-empty prompt string"),
            "empty_prompt"
        );
        assert_eq!(
            classify_error_kind("something completely unknown"),
            "unknown"
        );
        // #762: coverage for all classifier arms added since #77 — prevents silent fallback
        // to "unknown" if discriminant strings drift.
        assert_eq!(
            classify_error_kind("Manifest source files are missing: /tmp/x"),
            "missing_manifests"
        );
        assert_eq!(
            classify_error_kind("no managed sessions found in /tmp"),
            "no_managed_sessions"
        );
        assert_eq!(
            classify_error_kind("legacy session is missing workspace binding"),
            "legacy_session_no_workspace_binding"
        );
        // #780: full error string produced by resume_session includes the
        // "failed to restore session: " prefix — the specific arm must win.
        assert_eq!(
            classify_error_kind("failed to restore session: legacy session is missing workspace binding: /path/to/session.jsonl"),
            "legacy_session_no_workspace_binding"
        );
        assert_eq!(
            classify_error_kind("unsupported skills action: bogus. Supported actions: list"),
            "unsupported_skills_action"
        );
        assert_eq!(
            classify_error_kind("invalid_install_source: bogus"),
            "invalid_install_source"
        );
        assert_eq!(
            classify_error_kind("invalid_tool_name: unsupported tool in --allowedTools: teleport"),
            "invalid_tool_name"
        );
        assert_eq!(
            classify_error_kind(
                "invalid_output_format: unsupported value for --output-format: YAML"
            ),
            "invalid_output_format"
        );
        assert_eq!(
            classify_error_kind(
                "missing_flag_value: missing value for --model.\nUsage: --model <provider/model>"
            ),
            "missing_flag_value"
        );
        assert_eq!(
            classify_error_kind("invalid_permission_mode: unsupported permission mode 'bogus'.\nUsage: --permission-mode read-only|workspace-write|danger-full-access"),
            "invalid_permission_mode"
        );
        assert_eq!(
            classify_error_kind("invalid_cwd: not_found: `/tmp/missing`\nUsage: --cwd <path>"),
            "invalid_cwd"
        );
        assert_eq!(
            classify_error_kind("is not yet implemented"),
            "unsupported_command"
        );
        assert_eq!(
            classify_error_kind("confirmation required before running destructive operation"),
            "confirmation_required"
        );
        // #781: 429 and 401 now sub-classify; generic 5xx/other still api_http_error
        assert_eq!(
            classify_error_kind("api returned unexpected status 429"),
            "api_rate_limit_error"
        );
        assert_eq!(
            classify_error_kind(
                "api returned 401 Unauthorized (authentication_error): invalid x-api-key"
            ),
            "api_auth_error"
        );
        assert_eq!(
            classify_error_kind("api returned 500 Internal Server Error"),
            "api_http_error"
        );
        assert_eq!(
            classify_error_kind("interactive_only: this command requires an interactive terminal"),
            "interactive_only"
        );
        assert_eq!(
            classify_error_kind("slash command /compact is interactive-only"),
            "interactive_only"
        );
        // #774: agents now uses \n-delimited format — update test string to match real emission
        assert_eq!(
            classify_error_kind("unknown agents subcommand: bogus.\nSupported: list, show, help"),
            "unknown_agents_subcommand"
        );
        assert_eq!(
            classify_error_kind("agent not found: my-agent"),
            "agent_not_found"
        );
        assert_eq!(
            classify_error_kind("my-plugin is not installed"),
            "plugin_not_found"
        );
        // #794: plugins install with missing source path
        assert_eq!(
            classify_error_kind("plugin source `/nonexistent/path` was not found"),
            "plugin_source_not_found"
        );
        assert_eq!(
            classify_error_kind("skill source /path/to/skill not found"),
            "skill_not_found"
        );
        assert_eq!(
            classify_error_kind("skill 'my-skill' does not exist"),
            "skill_not_found"
        );
        assert_eq!(
            classify_error_kind("Unsupported config section 'show'. Use: env, hooks, model"),
            "unsupported_config_section"
        );
        assert_eq!(
            classify_error_kind("unknown_plugins_action: bogus"),
            "unknown_plugins_action"
        );
        assert_eq!(
            classify_error_kind(
                "missing_prompt: -p requires a prompt string.\nUsage: claw -p <text>"
            ),
            "missing_prompt"
        );
        assert_eq!(
            classify_error_kind("/tmp/.claw/settings.json: expected ',', found end of input"),
            "config_parse_error"
        );
        assert_eq!(
            classify_error_kind(
                "/path/to/.claw.json: field \"model\" must be a string, got a number"
            ),
            "config_parse_error"
        );
        // #765: removed auth subcommands must classify as removed_subcommand
        assert_eq!(
            classify_error_kind(
                "`claw login` has been removed.\nSet ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN instead."
            ),
            "removed_subcommand"
        );
        // #766: unexpected extra arguments must classify as unexpected_extra_args
        assert_eq!(
            classify_error_kind(
                "unexpected extra arguments after `claw diff`: --bogus\nUsage: claw diff"
            ),
            "unexpected_extra_args"
        );
        assert_eq!(
            classify_error_kind(
                "`claw logout` has been removed.\nSet ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN instead."
            ),
            "removed_subcommand"
        );
        // #768: invalid resume trailing arg must classify as invalid_resume_argument
        assert_eq!(
            classify_error_kind(
                "invalid_resume_argument: `compact` is not a slash command.\nUsage: claw --resume <session-id|latest> /<slash-command>"
            ),
            "invalid_resume_argument"
        );
        // coverage: invalid_history_count arm
        assert_eq!(
            classify_error_kind("invalid_history_count: abc is not a valid count"),
            "invalid_history_count"
        );
        assert_eq!(
            classify_error_kind("something invalid count something"),
            "invalid_history_count"
        );
        // coverage: unknown_option arm (#790)
        assert_eq!(
            classify_error_kind("unknown_option: unknown system-prompt option: --foo."),
            "unknown_option"
        );
        // #830: known command with missing required argument must not collapse to unknown.
        assert_eq!(
            classify_error_kind("missing_argument: mcp show requires a server name."),
            "missing_argument"
        );
    }

    #[test]
    fn split_error_hint_separates_reason_from_runbook() {
        // #77: short reason / hint separation for JSON error payloads
        let (short, hint) = split_error_hint("missing credentials\nHint: export ANTHROPIC_API_KEY");
        assert_eq!(short, "missing credentials");
        assert_eq!(hint, Some("Hint: export ANTHROPIC_API_KEY".to_string()));

        let (short, hint) = split_error_hint("simple error with no hint");
        assert_eq!(short, "simple error with no hint");
        assert_eq!(hint, None);
    }

    #[test]
    fn parses_bare_export_subcommand_targeting_latest_session() {
        // given
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        let args = vec!["export".to_string()];

        // when
        let parsed = parse_args(&args).expect("bare export should parse");

        // then
        assert_eq!(
            parsed,
            CliAction::Export {
                session_reference: LATEST_SESSION_REFERENCE.to_string(),
                output_path: None,
                output_format: CliOutputFormat::Text,
            }
        );
    }

    #[test]
    fn parses_export_subcommand_with_positional_output_path() {
        // given
        let args = vec!["export".to_string(), "conversation.md".to_string()];

        // when
        let parsed = parse_args(&args).expect("export with path should parse");

        // then
        assert_eq!(
            parsed,
            CliAction::Export {
                session_reference: LATEST_SESSION_REFERENCE.to_string(),
                output_path: Some(PathBuf::from("conversation.md")),
                output_format: CliOutputFormat::Text,
            }
        );
    }

    #[test]
    fn parses_export_subcommand_with_session_and_output_flags() {
        // given
        let args = vec![
            "export".to_string(),
            "--session".to_string(),
            "session-alpha".to_string(),
            "--output".to_string(),
            "/tmp/share.md".to_string(),
        ];

        // when
        let parsed = parse_args(&args).expect("export flags should parse");

        // then
        assert_eq!(
            parsed,
            CliAction::Export {
                session_reference: "session-alpha".to_string(),
                output_path: Some(PathBuf::from("/tmp/share.md")),
                output_format: CliOutputFormat::Text,
            }
        );
    }

    #[test]
    fn parses_export_subcommand_with_inline_flag_values() {
        // given
        let args = vec![
            "export".to_string(),
            "--session=session-beta".to_string(),
            "--output=/tmp/beta.md".to_string(),
        ];

        // when
        let parsed = parse_args(&args).expect("export inline flags should parse");

        // then
        assert_eq!(
            parsed,
            CliAction::Export {
                session_reference: "session-beta".to_string(),
                output_path: Some(PathBuf::from("/tmp/beta.md")),
                output_format: CliOutputFormat::Text,
            }
        );
    }

    #[test]
    fn parses_export_subcommand_with_json_output_format() {
        // given
        let args = vec![
            "--output-format=json".to_string(),
            "export".to_string(),
            "/tmp/notes.md".to_string(),
        ];

        // when
        let parsed = parse_args(&args).expect("json export should parse");

        // then
        assert_eq!(
            parsed,
            CliAction::Export {
                session_reference: LATEST_SESSION_REFERENCE.to_string(),
                output_path: Some(PathBuf::from("/tmp/notes.md")),
                output_format: CliOutputFormat::Json,
            }
        );
    }

    #[test]
    fn rejects_unknown_export_options_with_helpful_message() {
        // given
        let args = vec!["export".to_string(), "--bogus".to_string()];

        // when
        let error = parse_args(&args).expect_err("unknown export option should fail");

        // then
        assert!(error.contains("unknown export option: --bogus"));
    }

    #[test]
    fn rejects_export_with_extra_positional_after_path() {
        // given
        let args = vec![
            "export".to_string(),
            "first.md".to_string(),
            "second.md".to_string(),
        ];

        // when
        let error = parse_args(&args).expect_err("multiple positionals should fail");

        // then
        assert!(error.contains("unexpected export argument: second.md"));
    }

    #[test]
    fn parse_export_args_helper_defaults_to_latest_reference_and_no_output() {
        // given
        let args: Vec<String> = vec![];

        // when
        let parsed = parse_export_args(&args, CliOutputFormat::Text)
            .expect("empty export args should parse");

        // then
        assert_eq!(
            parsed,
            CliAction::Export {
                session_reference: LATEST_SESSION_REFERENCE.to_string(),
                output_path: None,
                output_format: CliOutputFormat::Text,
            }
        );
    }

    #[test]
    fn render_session_markdown_includes_header_and_summarized_tool_calls() {
        // given
        let mut session = Session::new();
        session.session_id = "session-export-test".to_string();
        session.messages = vec![
            ConversationMessage::user_text("How do I list files?"),
            ConversationMessage::assistant(vec![
                ContentBlock::Text {
                    text: "I'll run a tool.".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "toolu_abcdefghijklmnop".to_string(),
                    name: "bash".to_string(),
                    input: r#"{"command":"ls -la"}"#.to_string(),
                },
            ]),
            ConversationMessage {
                role: MessageRole::Tool,
                blocks: vec![ContentBlock::ToolResult {
                    tool_use_id: "toolu_abcdefghijklmnop".to_string(),
                    tool_name: "bash".to_string(),
                    output: "total 8\ndrwxr-xr-x  2 user staff   64 Apr  7 12:00 .".to_string(),
                    is_error: false,
                }],
                usage: None,
            },
        ];

        // when
        let markdown = render_session_markdown(
            &session,
            "session-export-test",
            std::path::Path::new("/tmp/sessions/session-export-test.jsonl"),
        );

        // then
        assert!(markdown.starts_with("# Conversation Export"));
        assert!(markdown.contains("- **Session**: `session-export-test`"));
        assert!(markdown.contains("- **Messages**: 3"));
        assert!(markdown.contains("## 1. User"));
        assert!(markdown.contains("How do I list files?"));
        assert!(markdown.contains("## 2. Assistant"));
        assert!(markdown.contains("**Tool call** `bash`"));
        assert!(markdown.contains("toolu_abcdef…"));
        assert!(markdown.contains("ls -la"));
        assert!(markdown.contains("## 3. Tool"));
        assert!(markdown.contains("**Tool result** `bash`"));
        assert!(markdown.contains("ok"));
        assert!(markdown.contains("total 8"));
    }

    #[test]
    fn render_session_markdown_marks_tool_errors_and_skips_empty_summaries() {
        // given
        let mut session = Session::new();
        session.session_id = "errs".to_string();
        session.messages = vec![ConversationMessage {
            role: MessageRole::Tool,
            blocks: vec![ContentBlock::ToolResult {
                tool_use_id: "short".to_string(),
                tool_name: "read_file".to_string(),
                output: "   ".to_string(),
                is_error: true,
            }],
            usage: None,
        }];

        // when
        let markdown =
            render_session_markdown(&session, "errs", std::path::Path::new("errs.jsonl"));

        // then
        assert!(markdown.contains("**Tool result** `read_file` _(id `short`, error)_"));
        // an empty summary should not produce a stray blockquote line
        assert!(!markdown.contains("> \n"));
    }

    #[test]
    fn summarize_tool_payload_for_markdown_compacts_json_and_truncates_overflow() {
        // given
        let json_payload = r#"{
            "command":   "ls -la",
            "cwd": "/tmp"
        }"#;
        let long_payload = "a".repeat(600);

        // when
        let compacted = summarize_tool_payload_for_markdown(json_payload);
        let truncated = summarize_tool_payload_for_markdown(&long_payload);

        // then
        assert_eq!(compacted, r#"{"command":"ls -la","cwd":"/tmp"}"#);
        assert!(truncated.ends_with('…'));
        assert!(truncated.chars().count() <= 281);
    }

    #[test]
    fn short_tool_id_truncates_long_identifiers_with_ellipsis() {
        // given
        let long = "toolu_01ABCDEFGHIJKLMN";
        let short = "tool_1";

        // when
        let trimmed_long = short_tool_id(long);
        let trimmed_short = short_tool_id(short);

        // then
        assert_eq!(trimmed_long, "toolu_01ABCD…");
        assert_eq!(trimmed_short, "tool_1");
    }

    #[test]
    fn parses_json_output_for_mcp_and_skills_commands() {
        assert_eq!(
            parse_args(&["--output-format=json".to_string(), "mcp".to_string()])
                .expect("json mcp should parse"),
            CliAction::Mcp {
                args: None,
                output_format: CliOutputFormat::Json,
            }
        );
        assert_eq!(
            parse_args(&[
                "--output-format=json".to_string(),
                "/skills".to_string(),
                "help".to_string(),
            ])
            .expect("json /skills help should parse"),
            CliAction::Skills {
                args: Some("help".to_string()),
                output_format: CliOutputFormat::Json,
            }
        );
    }

    #[test]
    fn single_word_slash_command_names_return_guidance_instead_of_hitting_prompt_mode() {
        let error = parse_args(&["cost".to_string()]).expect_err("cost should return guidance");
        assert!(error.contains("slash command"));
        assert!(error.contains("/cost"));
    }

    #[test]
    fn multi_word_prompt_still_uses_shorthand_prompt_mode() {
        let _guard = env_lock();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        // Input is ["--model", "opus", "please", "debug", "this"] so the joined
        // prompt shorthand must stay a normal multi-word prompt while still
        // honoring alias validation at parse time.
        assert_eq!(
            parse_args(&[
                "--model".to_string(),
                "opus".to_string(),
                "please".to_string(),
                "debug".to_string(),
                "this".to_string(),
            ])
            .expect("prompt shorthand should still work"),
            CliAction::Prompt {
                prompt: "please debug this".to_string(),
                model: "anthropic/claude-opus-4-7".to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: crate::default_permission_mode(),
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn parses_direct_agents_mcp_and_skills_slash_commands() {
        let _guard = env_lock();
        let _cwd_guard = cwd_guard();
        std::env::remove_var("RUSTY_CLAUDE_PERMISSION_MODE");
        assert_eq!(
            parse_args(&["/agents".to_string()]).expect("/agents should parse"),
            CliAction::Agents {
                args: None,
                output_format: CliOutputFormat::Text
            }
        );
        assert_eq!(
            parse_args(&["/mcp".to_string(), "show".to_string(), "demo".to_string()])
                .expect("/mcp show demo should parse"),
            CliAction::Mcp {
                args: Some("show demo".to_string()),
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["/skills".to_string()]).expect("/skills should parse"),
            CliAction::Skills {
                args: None,
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["/skill".to_string()]).expect("/skill should parse"),
            CliAction::Skills {
                args: None,
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["/skills".to_string(), "help".to_string()])
                .expect("/skills help should parse"),
            CliAction::Skills {
                args: Some("help".to_string()),
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["/skill".to_string(), "list".to_string()])
                .expect("/skill list should parse"),
            CliAction::Skills {
                args: Some("list".to_string()),
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&[
                "/skills".to_string(),
                "help".to_string(),
                "overview".to_string()
            ])
            .expect("/skills help overview should invoke"),
            CliAction::Prompt {
                prompt: "$help overview".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: crate::default_permission_mode(),
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
        assert_eq!(
            parse_args(&[
                "/skills".to_string(),
                "install".to_string(),
                "./fixtures/help-skill".to_string(),
            ])
            .expect("/skills install should parse"),
            CliAction::Skills {
                args: Some("install ./fixtures/help-skill".to_string()),
                output_format: CliOutputFormat::Text,
            }
        );
        assert_eq!(
            parse_args(&["/skills".to_string(), "/test".to_string()])
                .expect("/skills /test should normalize to a single skill prompt prefix"),
            CliAction::Prompt {
                prompt: "$test".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: crate::default_permission_mode(),
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
        assert_eq!(
            parse_args(&["/status".to_string()]).expect("/status should parse as local status"),
            CliAction::Status {
                model: DEFAULT_MODEL.to_string(),
                model_flag_raw: None,
                permission_mode: PermissionModeProvenance::default_fallback(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
            }
        );
    }

    #[test]
    fn direct_slash_commands_surface_shared_validation_errors() {
        let compact_error = parse_args(&["/compact".to_string(), "now".to_string()])
            .expect_err("invalid /compact shape should be rejected");
        assert!(compact_error.contains("Unexpected arguments for /compact."));
        assert!(compact_error.contains("Usage            /compact"));

        let plugins_error = parse_args(&[
            "/plugins".to_string(),
            "list".to_string(),
            "extra".to_string(),
        ])
        .expect_err("invalid /plugins list shape should be rejected");
        assert!(plugins_error.contains("Usage: /plugin list"));
        assert!(plugins_error.contains("Aliases          /plugins, /marketplace"));

        for alias in ["/plugin", "/plugins", "/marketplace"] {
            let error = parse_args(&[alias.to_string()])
                .expect_err("valid plugin slash aliases are local/interactive, never prompts");
            // #829: prefix changed from "interactive-only" to "interactive_only:"
            assert!(
                error.contains("interactive_only:") || error.contains("interactive-only"),
                "{alias} should reject as an interactive plugin command outside the REPL, got: {error}"
            );
        }
    }

    #[test]
    fn formats_unknown_slash_command_with_suggestions() {
        let report = format_unknown_slash_command_message("statsu");
        assert!(report.contains("unknown slash command: /statsu"));
        assert!(report.contains("Did you mean"));
        assert!(report.contains("Use /help"));
    }

    #[test]
    fn typoed_doctor_subcommand_returns_did_you_mean_error() {
        let error = parse_args(&["doctorr".to_string()]).expect_err("doctorr should error");
        assert!(error.contains("unknown subcommand: doctorr."));
        assert!(error.contains("Did you mean"));
        assert!(error.contains("doctor"));
    }

    #[test]
    fn typoed_skills_subcommand_returns_did_you_mean_error() {
        let error = parse_args(&["skilsl".to_string()]).expect_err("skilsl should error");
        assert!(error.contains("unknown subcommand: skilsl."));
        assert!(error.contains("skills"));
    }

    #[test]
    fn unsupported_skills_actions_return_typed_error_683() {
        let error = parse_args(&["skills".to_string(), "add".to_string()])
            .expect_err("skills add should error");
        assert!(
            error.contains("unsupported skills action"),
            "skills add should contain 'unsupported skills action', got: {error}"
        );
        assert_eq!(
            classify_error_kind(&error),
            "unsupported_skills_action",
            "skills add should classify as unsupported_skills_action, got: {error}"
        );

        for action in ["remove", "uninstall", "delete"] {
            assert_eq!(
                parse_args(&["skills".to_string(), action.to_string()])
                    .expect(&format!("skills {action} should parse")),
                CliAction::Skills {
                    args: Some(action.to_string()),
                    output_format: CliOutputFormat::Text,
                },
                "skills {action} should route locally so missing targets are handled without credentials"
            );
        }
    }

    #[test]
    fn typoed_status_subcommand_returns_did_you_mean_error() {
        let error = parse_args(&["statuss".to_string()]).expect_err("statuss should error");
        assert!(error.contains("unknown subcommand: statuss."));
        assert!(error.contains("status"));
    }

    #[test]
    fn typoed_export_subcommand_returns_did_you_mean_error() {
        let error = parse_args(&["exporrt".to_string()]).expect_err("exporrt should error");
        assert!(error.contains("unknown subcommand: exporrt."));
        assert!(error.contains("Did you mean"));
        assert!(error.contains("export"));
    }

    #[test]
    fn typoed_mcp_subcommand_returns_did_you_mean_error() {
        let error = parse_args(&["mcpp".to_string()]).expect_err("mcpp should error");
        assert!(error.contains("unknown subcommand: mcpp."));
        assert!(error.contains("mcp"));
    }

    #[test]
    fn multi_word_prompt_still_bypasses_subcommand_typo_guard() {
        assert_eq!(
            parse_args(&[
                "hello".to_string(),
                "world".to_string(),
                "this".to_string(),
                "is".to_string(),
                "a".to_string(),
                "prompt".to_string(),
            ])
            .expect("multi-word prompt should still parse"),
            CliAction::Prompt {
                prompt: "hello world this is a prompt".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: crate::default_permission_mode(),
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn prompt_subcommand_allows_literal_typo_word() {
        assert_eq!(
            parse_args(&["prompt".to_string(), "doctorr".to_string()])
                .expect("explicit prompt subcommand should allow literal typo word"),
            CliAction::Prompt {
                prompt: "doctorr".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn punctuation_bearing_single_token_still_dispatches_to_prompt() {
        // #140: Guard against test pollution — isolate cwd + env so this test
        // doesn't pick up a stale .claw/settings.json from other tests that
        // may have set `permissionMode: acceptEdits` in a shared cwd.
        let _guard = env_lock();
        let root = temp_dir();
        let cwd = root.join("project");
        std::fs::create_dir_all(&cwd).expect("project dir should exist");
        let result = with_current_dir(&cwd, || {
            parse_args(&["PARITY_SCENARIO:bash_permission_prompt_approved".to_string()])
                .expect("scenario token should still dispatch to prompt")
        });
        assert_eq!(
            result,
            CliAction::Prompt {
                prompt: "PARITY_SCENARIO:bash_permission_prompt_approved".to_string(),
                model: DEFAULT_MODEL.to_string(),
                output_format: CliOutputFormat::Text,
                allowed_tools: None,
                permission_mode: PermissionMode::WorkspaceWrite,
                compact: false,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn formats_namespaced_omc_slash_command_with_contract_guidance() {
        let report = format_unknown_slash_command_message("oh-my-claudecode:hud");
        assert!(report.contains("unknown slash command: /oh-my-claudecode:hud"));
        assert!(report.contains("Claude Code/OMC plugin command"));
        assert!(report.contains("plugin slash commands"));
        assert!(report.contains("statusline"));
        assert!(report.contains("session hooks"));
    }

    #[test]
    fn parses_resume_flag_with_slash_command() {
        let args = vec![
            "--resume".to_string(),
            "session.jsonl".to_string(),
            "/compact".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("session.jsonl"),
                commands: vec!["/compact".to_string()],
                output_format: CliOutputFormat::Text,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn parses_resume_flag_without_path_as_latest_session() {
        assert_eq!(
            parse_args(&["--resume".to_string()]).expect("args should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("latest"),
                commands: vec![],
                output_format: CliOutputFormat::Text,
                allow_broad_cwd: false,
            }
        );
        assert_eq!(
            parse_args(&["--resume".to_string(), "/status".to_string()])
                .expect("resume shortcut should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("latest"),
                commands: vec!["/status".to_string()],
                output_format: CliOutputFormat::Text,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn parses_resume_flag_with_multiple_slash_commands() {
        let args = vec![
            "--resume".to_string(),
            "session.jsonl".to_string(),
            "/status".to_string(),
            "/compact".to_string(),
            "/cost".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("session.jsonl"),
                commands: vec![
                    "/status".to_string(),
                    "/compact".to_string(),
                    "/cost".to_string(),
                ],
                output_format: CliOutputFormat::Text,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn rejects_unknown_options_with_helpful_guidance() {
        let error = parse_args(&["--resum".to_string()]).expect_err("unknown option should fail");
        assert!(error.contains("unknown option: --resum"));
        assert!(error.contains("Did you mean --resume?"));
        assert!(error.contains("claw --help"));
    }

    #[test]
    fn parses_resume_flag_with_slash_command_arguments() {
        let args = vec![
            "--resume".to_string(),
            "session.jsonl".to_string(),
            "/export".to_string(),
            "notes.txt".to_string(),
            "/clear".to_string(),
            "--confirm".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("session.jsonl"),
                commands: vec![
                    "/export notes.txt".to_string(),
                    "/clear --confirm".to_string(),
                ],
                output_format: CliOutputFormat::Text,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn parses_resume_flag_with_absolute_export_path() {
        let args = vec![
            "--resume".to_string(),
            "session.jsonl".to_string(),
            "/export".to_string(),
            "/tmp/notes.txt".to_string(),
            "/status".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("session.jsonl"),
                commands: vec!["/export /tmp/notes.txt".to_string(), "/status".to_string()],
                output_format: CliOutputFormat::Text,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn filtered_tool_specs_respect_allowlist() {
        let allowed = ["read_file", "grep_search"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let filtered = filter_tool_specs(&GlobalToolRegistry::builtin(), Some(&allowed));
        let names = filtered
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["read_file", "grep_search"]);
    }

    #[test]
    fn filtered_tool_specs_include_plugin_tools() {
        let filtered = filter_tool_specs(&registry_with_plugin_tool(), None);
        let names = filtered
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"bash".to_string()));
        assert!(names.contains(&"plugin_echo".to_string()));
    }

    #[test]
    fn permission_policy_uses_plugin_tool_permissions() {
        let feature_config = runtime::RuntimeFeatureConfig::default();
        let policy = permission_policy(
            PermissionMode::ReadOnly,
            &feature_config,
            &registry_with_plugin_tool(),
        )
        .expect("permission policy should build");
        let required = policy.required_mode_for("plugin_echo");
        assert_eq!(required, PermissionMode::WorkspaceWrite);
    }

    #[test]
    fn shared_help_uses_resume_annotation_copy() {
        let help = commands::render_slash_command_help();
        assert!(help.contains("Slash commands"));
        assert!(help.contains("works with --resume SESSION.jsonl"));
    }

    #[test]
    fn bare_skill_dispatch_resolves_known_project_skill_to_prompt() {
        let _guard = env_lock();
        let workspace = temp_dir();
        write_skill_fixture(
            &workspace.join(".codex").join("skills"),
            "caveman",
            "Project skill fixture",
        );

        let prompt = try_resolve_bare_skill_prompt(&workspace, "caveman sharpen club")
            .expect("known bare skill should dispatch");
        assert_eq!(prompt, "$caveman sharpen club");

        fs::remove_dir_all(workspace).expect("workspace should clean up");
    }

    #[test]
    fn bare_skill_dispatch_ignores_unknown_or_non_skill_input() {
        let _guard = env_lock();
        let workspace = temp_dir();
        fs::create_dir_all(&workspace).expect("workspace should exist");

        assert_eq!(
            try_resolve_bare_skill_prompt(&workspace, "not-a-known-skill do thing"),
            None
        );
        assert_eq!(try_resolve_bare_skill_prompt(&workspace, "/status"), None);

        fs::remove_dir_all(workspace).expect("workspace should clean up");
    }

    #[test]
    fn repl_help_includes_shared_commands_and_exit() {
        let help = render_repl_help();
        assert!(help.contains("REPL"));
        assert!(help.contains("/help"));
        assert!(help.contains("Complete commands, modes, and recent sessions"));
        assert!(help.contains("/status"));
        assert!(help.contains("/sandbox"));
        assert!(help.contains("/model [model]"));
        assert!(help.contains("/permissions [read-only|workspace-write|danger-full-access]"));
        assert!(help.contains("/clear [--confirm]"));
        assert!(help.contains("/cost"));
        assert!(help.contains("/resume <session-path>"));
        assert!(help.contains("/config [env|hooks|model|plugins]"));
        assert!(help.contains("/mcp [list|show <server>|help]"));
        assert!(help.contains("/memory"));
        assert!(help.contains("/init"));
        assert!(help.contains("/diff"));
        assert!(help.contains("/version"));
        assert!(help.contains("/export [file]"));
        // Batch 5 added `/session delete`; match on the stable core rather than
        // the trailing bracket so future additions don't re-break this.
        assert!(help
            .contains("/session [list|exists <session-id>|switch <session-id>|fork [branch-name]"));
        assert!(help.contains(
            "/plugin [list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]"
        ));
        assert!(help.contains("aliases: /plugins, /marketplace"));
        assert!(help.contains("/agents"));
        assert!(help.contains("/skills"));
        assert!(help.contains("/exit"));
        assert!(help.contains(
            "Auto-save            .claw/sessions/<workspace-fingerprint>/<session-id>.jsonl"
        ));
        assert!(help.contains("Resume latest        /resume latest"));
    }

    #[test]
    fn completion_candidates_include_workflow_shortcuts_and_dynamic_sessions() {
        let completions = slash_command_completion_candidates_with_sessions(
            "sonnet",
            Some("session-current"),
            vec!["session-old".to_string()],
        );

        assert!(completions.contains(&"/model anthropic/claude-sonnet-4-6".to_string()));
        assert!(completions.contains(&"/permissions workspace-write".to_string()));
        assert!(completions.contains(&"/session list".to_string()));
        assert!(completions.contains(&"/session switch session-current".to_string()));
        assert!(completions.contains(&"/resume session-old".to_string()));
        assert!(completions.contains(&"/mcp list".to_string()));
        assert!(completions.contains(&"/ultraplan ".to_string()));
    }

    #[test]
    fn startup_banner_mentions_workflow_completions() {
        let _guard = env_lock();
        // Inject dummy credentials so LiveCli can construct without real Anthropic key
        std::env::set_var("ANTHROPIC_API_KEY", "test-dummy-key-for-banner-test");
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir");

        let banner = with_current_dir(&root, || {
            LiveCli::new(
                "anthropic/claude-sonnet-4-6".to_string(),
                true,
                None,
                PermissionMode::DangerFullAccess,
            )
            .expect("cli should initialize")
            .startup_banner()
        });

        assert!(banner.contains("Tab"));
        assert!(banner.contains("workflow completions"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn format_connected_line_renders_anthropic_provider_for_claude_model() {
        let model = "anthropic/claude-sonnet-4-6";

        let line = format_connected_line(model);

        assert_eq!(line, "Connected: anthropic/claude-sonnet-4-6 via anthropic");
    }

    #[test]
    fn format_connected_line_renders_xai_provider_for_grok_model() {
        let model = "grok-3";

        let line = format_connected_line(model);

        assert_eq!(line, "Connected: grok-3 via xai");
    }

    #[test]
    fn resolve_repl_model_returns_user_supplied_model_unchanged_when_explicit() {
        let user_model = "anthropic/claude-sonnet-4-6".to_string();

        let resolved = resolve_repl_model(user_model).expect("explicit model should resolve");

        assert_eq!(resolved, "anthropic/claude-sonnet-4-6");
    }

    #[test]
    fn resolve_repl_model_falls_back_to_anthropic_model_env_when_default() {
        let _guard = env_lock();
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir");
        let config_home = root.join("config");
        fs::create_dir_all(&config_home).expect("config home dir");
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);
        std::env::remove_var("ANTHROPIC_MODEL");
        std::env::set_var("ANTHROPIC_MODEL", "sonnet");

        let resolved = with_current_dir(&root, || resolve_repl_model(DEFAULT_MODEL.to_string()))
            .expect("env model should resolve");

        assert_eq!(resolved, "anthropic/claude-sonnet-4-6");

        std::env::remove_var("ANTHROPIC_MODEL");
        std::env::remove_var("CLAW_CONFIG_HOME");
        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn resolve_repl_model_returns_default_when_env_unset_and_no_config() {
        let _guard = env_lock();
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir");
        let config_home = root.join("config");
        fs::create_dir_all(&config_home).expect("config home dir");
        std::env::set_var("CLAW_CONFIG_HOME", &config_home);
        std::env::remove_var("ANTHROPIC_MODEL");

        let resolved = with_current_dir(&root, || resolve_repl_model(DEFAULT_MODEL.to_string()))
            .expect("default model should resolve");

        assert_eq!(resolved, DEFAULT_MODEL);

        std::env::remove_var("CLAW_CONFIG_HOME");
        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn resume_supported_command_list_matches_expected_surface() {
        let names = resume_supported_slash_commands()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        // Now with 135+ slash commands, verify minimum resume support
        assert!(
            names.len() >= 39,
            "expected at least 39 resume-supported commands, got {}",
            names.len()
        );
        // Verify key resume commands still exist
        assert!(names.contains(&"help"));
        assert!(names.contains(&"status"));
        assert!(names.contains(&"compact"));
    }

    #[test]
    fn session_exists_resume_command_reports_json_contract() {
        let session = Session::new();
        let path = PathBuf::from("missing-session.jsonl");
        let outcome = run_resume_command(
            &path,
            &session,
            &SlashCommand::Session {
                action: Some("exists".to_string()),
                target: Some("definitely-missing-session".to_string()),
            },
        )
        .expect("exists command should not fail for missing sessions");

        let json = outcome.json.expect("json contract");
        assert_eq!(json["kind"], "session_exists");
        assert_eq!(json["exists"], false);
        assert_eq!(json["session"], "definitely-missing-session");
    }

    #[test]
    fn resume_report_uses_sectioned_layout() {
        let report = format_resume_report("session.jsonl", 14, 6);
        assert!(report.contains("Session resumed"));
        assert!(report.contains("Session file     session.jsonl"));
        assert!(report.contains("Messages         14"));
        assert!(report.contains("Turns            6"));
    }

    #[test]
    fn compact_report_uses_structured_output() {
        let compacted = format_compact_report(8, 5, false);
        assert!(compacted.contains("Compact"));
        assert!(compacted.contains("Result           compacted"));
        assert!(compacted.contains("Messages removed 8"));
        let skipped = format_compact_report(0, 3, true);
        assert!(skipped.contains("Result           skipped"));
    }

    #[test]
    fn cost_report_uses_sectioned_layout() {
        let report = format_cost_report(runtime::TokenUsage {
            input_tokens: 20,
            output_tokens: 8,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 1,
        });
        assert!(report.contains("Cost"));
        assert!(report.contains("Input tokens     20"));
        assert!(report.contains("Output tokens    8"));
        assert!(report.contains("Cache create     3"));
        assert!(report.contains("Cache read       1"));
        assert!(report.contains("Total tokens     32"));
        assert!(report.contains("Estimated cost"));
    }

    #[test]
    fn permissions_report_uses_sectioned_layout() {
        let report = format_permissions_report("workspace-write");
        assert!(report.contains("Permissions"));
        assert!(report.contains("Active mode      workspace-write"));
        assert!(report.contains("Modes"));
        assert!(report.contains("read-only          ○ available Read/search tools only"));
        assert!(report.contains("workspace-write    ● current   Edit files inside the workspace"));
        assert!(report.contains("danger-full-access ○ available Unrestricted tool access"));
    }

    #[test]
    fn permissions_switch_report_is_structured() {
        let report = format_permissions_switch_report("read-only", "workspace-write");
        assert!(report.contains("Permissions updated"));
        assert!(report.contains("Result           mode switched"));
        assert!(report.contains("Previous mode    read-only"));
        assert!(report.contains("Active mode      workspace-write"));
        assert!(report.contains("Applies to       subsequent tool calls"));
    }

    #[test]
    fn init_help_mentions_direct_subcommand() {
        let mut help = Vec::new();
        print_help_to(&mut help).expect("help should render");
        let help = String::from_utf8(help).expect("help should be utf8");
        assert!(help.contains("claw help"));
        assert!(help.contains("claw version"));
        assert!(help.contains("claw status"));
        assert!(help.contains("claw sandbox"));
        assert!(help.contains("claw init"));
        assert!(help.contains("claw acp [serve]"));
        assert!(help.contains("claw agents"));
        assert!(help.contains("claw mcp"));
        assert!(help.contains("claw skills"));
        assert!(help.contains("claw /skills"));
        assert!(help.contains("ultraworkers/claw-code"));
        assert!(help.contains("cargo install claw-code"));
        assert!(!help.contains("claw login"));
        assert!(!help.contains("claw logout"));
    }

    #[test]
    fn model_report_uses_sectioned_layout() {
        let report = format_model_report("claude-sonnet", 12, 4);
        assert!(report.contains("Model"));
        assert!(report.contains("Current model    claude-sonnet"));
        assert!(report.contains("Session messages 12"));
        assert!(report.contains("Switch models with /model <name>"));
    }

    fn test_branch_freshness() -> super::BranchFreshness {
        super::BranchFreshness {
            upstream: Some("origin/main".to_string()),
            ahead: 0,
            behind: 0,
            fresh: Some(true),
        }
    }

    fn test_boot_preflight() -> super::BootPreflightSnapshot {
        super::BootPreflightSnapshot {
            repo_exists: true,
            worktree_exists: true,
            git_dir_exists: true,
            branch_freshness: test_branch_freshness(),
            trust_gate_allowed: Some(false),
            trusted_roots_count: 0,
            required_binaries: Vec::new(),
            control_sockets: Vec::new(),
            mcp_startup_eligible: true,
            mcp_servers_configured: 0,
            plugin_startup_eligible: true,
            plugins_configured: 0,
            last_failed_boot_reason: None,
        }
    }

    #[test]
    fn model_switch_report_preserves_context_summary() {
        let report = format_model_switch_report("claude-sonnet", "claude-opus", 9);
        assert!(report.contains("Model updated"));
        assert!(report.contains("Previous         claude-sonnet"));
        assert!(report.contains("Current          claude-opus"));
        assert!(report.contains("Preserved msgs   9"));
    }

    #[test]
    fn status_line_reports_model_and_token_totals() {
        let status = format_status_report(
            "claude-sonnet",
            StatusUsage {
                message_count: 7,
                turns: 3,
                latest: runtime::TokenUsage {
                    input_tokens: 5,
                    output_tokens: 4,
                    cache_creation_input_tokens: 1,
                    cache_read_input_tokens: 0,
                },
                cumulative: runtime::TokenUsage {
                    input_tokens: 20,
                    output_tokens: 8,
                    cache_creation_input_tokens: 2,
                    cache_read_input_tokens: 1,
                },
                estimated_tokens: 128,
            },
            "workspace-write",
            &super::StatusContext {
                cwd: PathBuf::from("/tmp/project"),
                session_path: Some(PathBuf::from("session.jsonl")),
                loaded_config_files: 2,
                discovered_config_files: 3,
                memory_file_count: 4,
                memory_files: vec![super::MemoryFileSummary {
                    path: "/tmp/project/CLAUDE.md".to_string(),
                    source: "claude_md".to_string(),
                    origin: "workspace".to_string(),
                    scope_path: "/tmp/project".to_string(),
                    outside_project: false,
                    chars: 42,
                    contributes: true,
                }],
                unloaded_memory_files: Vec::new(),
                project_root: Some(PathBuf::from("/tmp")),
                git_branch: Some("main".to_string()),
                git_summary: GitWorkspaceSummary {
                    changed_files: 3,
                    staged_files: 1,
                    unstaged_files: 1,
                    untracked_files: 1,
                    conflicted_files: 0,
                    operation: GitOperation::None,
                },
                branch_freshness: test_branch_freshness(),
                stale_base_state: super::BaseCommitState::NoExpectedBase,
                session_lifecycle: SessionLifecycleSummary {
                    kind: SessionLifecycleKind::IdleShell,
                    pane_id: Some("%7".to_string()),
                    pane_command: Some("zsh".to_string()),
                    pane_path: Some(PathBuf::from("/tmp/project")),
                    workspace_dirty: true,
                    abandoned: true,
                    all_panes: vec![],
                },
                boot_preflight: test_boot_preflight(),
                sandbox_status: runtime::SandboxStatus::default(),
                binary_provenance: super::binary_provenance_for(None),
                config_load_error: None,
                config_load_error_kind: None,
                mcp_validation: super::McpValidationSummary::default(),

                hook_validation: super::HookValidationSummary::default(),
                duplicate_flags: Vec::new(),
            },
            None, // #148
            None,
        );
        assert!(status.contains("Status"));
        assert!(status.contains("Model            claude-sonnet"));
        assert!(status.contains("Permission mode  workspace-write"));
        assert!(status.contains("Messages         7"));
        assert!(status.contains("Latest total     10"));
        assert!(status.contains("Cache create     2"));
        assert!(status.contains("Cache read       1"));
        assert!(status.contains("Cumulative total 31"));
        assert!(status.contains("Estimated cost"));
        assert!(status.contains("Cwd              /tmp/project"));
        assert!(status.contains("Project root     /tmp"));
        assert!(status.contains("Git branch       main"));
        assert!(
            status.contains("Git state        dirty · 3 files · 1 staged, 1 unstaged, 1 untracked")
        );
        assert!(status.contains("Changed files    3"));
        assert!(status.contains("Loaded memory    claude_md:/tmp/project/CLAUDE.md"));
        assert!(status.contains("Staged           1"));
        assert!(status.contains("Unstaged         1"));
        assert!(status.contains("Untracked        1"));
        assert!(status.contains("Session          session.jsonl"));
        assert!(
            status.contains("Lifecycle        idle shell · dirty worktree · abandoned? · cmd=zsh")
        );
        assert!(status.contains("Config files     loaded 2/3"));
        assert!(status.contains("Memory files     4"));
        assert!(status.contains("Suggested flow   /status → /diff → /commit"));
    }

    #[test]
    fn session_lifecycle_prefers_running_process_over_idle_shell() {
        let workspace = PathBuf::from("/tmp/project");
        let lifecycle = classify_session_lifecycle_from_panes(
            &workspace,
            vec![
                TmuxPaneSnapshot {
                    pane_id: "%1".to_string(),
                    current_command: "zsh".to_string(),
                    current_path: workspace.clone(),
                },
                TmuxPaneSnapshot {
                    pane_id: "%2".to_string(),
                    current_command: "claw".to_string(),
                    current_path: workspace.join("rust"),
                },
            ],
        );

        assert_eq!(lifecycle.kind, SessionLifecycleKind::RunningProcess);
        assert_eq!(lifecycle.pane_id.as_deref(), Some("%2"));
        assert_eq!(lifecycle.pane_command.as_deref(), Some("claw"));
        assert!(!lifecycle.abandoned);
    }

    #[test]
    fn session_lifecycle_marks_dirty_idle_shell_as_abandoned() {
        let _guard = env_lock();
        let workspace = temp_workspace("dirty-idle-shell");
        fs::create_dir_all(&workspace).expect("workspace should create");
        git(&["init", "--quiet"], &workspace);
        git(&["config", "user.email", "tests@example.com"], &workspace);
        git(&["config", "user.name", "Rusty Claude Tests"], &workspace);
        fs::write(workspace.join("tracked.txt"), "hello\n").expect("write tracked");
        git(&["add", "tracked.txt"], &workspace);
        git(&["commit", "-m", "init", "--quiet"], &workspace);
        fs::write(workspace.join("tracked.txt"), "hello\nchanged\n").expect("dirty tracked");

        let lifecycle = classify_session_lifecycle_from_panes(
            &workspace,
            vec![TmuxPaneSnapshot {
                pane_id: "%3".to_string(),
                current_command: "bash".to_string(),
                current_path: workspace.clone(),
            }],
        );

        assert_eq!(lifecycle.kind, SessionLifecycleKind::IdleShell);
        assert!(lifecycle.workspace_dirty);
        assert!(lifecycle.abandoned);

        fs::remove_dir_all(workspace).expect("cleanup temp dir");
    }

    #[test]
    fn session_list_surfaces_saved_dirty_abandoned_lifecycle() {
        let _guard = cwd_guard();
        let workspace = temp_workspace("session-list-lifecycle");
        fs::create_dir_all(&workspace).expect("workspace should create");
        git(&["init", "--quiet"], &workspace);
        git(&["config", "user.email", "tests@example.com"], &workspace);
        git(&["config", "user.name", "Rusty Claude Tests"], &workspace);
        fs::write(workspace.join(".gitignore"), ".claw/\n").expect("write gitignore");
        fs::write(workspace.join("tracked.txt"), "hello\n").expect("write tracked");
        git(&["add", ".gitignore", "tracked.txt"], &workspace);
        git(&["commit", "-m", "init", "--quiet"], &workspace);

        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&workspace).expect("switch cwd");
        let handle = create_managed_session_handle("session-alpha").expect("session handle");
        Session::new()
            .with_workspace_root(workspace.clone())
            .with_persistence_path(handle.path.clone())
            .save_to_path(&handle.path)
            .expect("session should save");
        fs::write(workspace.join("tracked.txt"), "hello\nchanged\n").expect("dirty tracked");

        let report = render_session_list("session-alpha").expect("session list should render");

        assert!(report.contains("session-alpha"));
        assert!(report.contains("lifecycle=saved only · dirty worktree · abandoned?"));

        std::env::set_current_dir(previous).expect("restore cwd");
        fs::remove_dir_all(workspace).expect("cleanup temp dir");
    }

    #[test]
    fn workspace_health_warns_when_stale_base_diverged() {
        let context = super::StatusContext {
            cwd: PathBuf::from("/tmp/project"),
            session_path: None,
            loaded_config_files: 0,
            discovered_config_files: 0,
            memory_file_count: 0,
            memory_files: Vec::new(),
            unloaded_memory_files: Vec::new(),
            project_root: Some(PathBuf::from("/tmp/project")),
            git_branch: Some("feature/stale-base".to_string()),
            git_summary: GitWorkspaceSummary::default(),
            branch_freshness: test_branch_freshness(),
            stale_base_state: super::BaseCommitState::Diverged {
                expected: "base".to_string(),
                actual: "head".to_string(),
            },
            session_lifecycle: SessionLifecycleSummary {
                kind: SessionLifecycleKind::SavedOnly,
                pane_id: None,
                pane_command: None,
                pane_path: None,
                workspace_dirty: false,
                abandoned: false,
                all_panes: vec![],
            },
            boot_preflight: test_boot_preflight(),
            sandbox_status: runtime::SandboxStatus::default(),
            binary_provenance: super::binary_provenance_for(None),
            config_load_error: None,
            config_load_error_kind: None,
            mcp_validation: super::McpValidationSummary::default(),

            hook_validation: super::HookValidationSummary::default(),
            duplicate_flags: Vec::new(),
        };

        let check = super::check_workspace_health(&context);

        assert_eq!(check.level, super::DiagnosticLevel::Warn);
        assert_eq!(check.data["stale_base"]["status"], "diverged");
        assert_eq!(check.data["stale_base"]["fresh"], false);
        assert!(check
            .details
            .iter()
            .any(|detail| detail.contains("stale codebase")));
    }

    #[test]
    fn memory_health_surfaces_loaded_and_unloaded_files_438() {
        let context = super::StatusContext {
            cwd: PathBuf::from("/tmp/project"),
            session_path: None,
            loaded_config_files: 0,
            discovered_config_files: 0,
            memory_file_count: 1,
            memory_files: vec![super::MemoryFileSummary {
                path: "/tmp/project/CLAUDE.md".to_string(),
                source: "claude_md".to_string(),
                origin: "workspace".to_string(),
                scope_path: "/tmp/project".to_string(),
                outside_project: false,
                chars: 12,
                contributes: true,
            }],
            unloaded_memory_files: vec!["/tmp/project/AGENTS.md".to_string()],
            project_root: Some(PathBuf::from("/tmp/project")),
            git_branch: Some("main".to_string()),
            git_summary: GitWorkspaceSummary::default(),
            branch_freshness: test_branch_freshness(),
            stale_base_state: super::BaseCommitState::NoExpectedBase,
            session_lifecycle: SessionLifecycleSummary {
                kind: SessionLifecycleKind::SavedOnly,
                pane_id: None,
                pane_command: None,
                pane_path: None,
                workspace_dirty: false,
                abandoned: false,
                all_panes: vec![],
            },
            boot_preflight: test_boot_preflight(),
            sandbox_status: runtime::SandboxStatus::default(),
            binary_provenance: super::binary_provenance_for(None),
            config_load_error: None,
            config_load_error_kind: None,
            mcp_validation: super::McpValidationSummary::default(),

            hook_validation: super::HookValidationSummary::default(),
            duplicate_flags: Vec::new(),
        };

        let check = super::check_memory_health(&context);

        assert_eq!(check.level, super::DiagnosticLevel::Warn);
        assert_eq!(check.data["memory_file_count"], 1);
        assert_eq!(check.data["memory_files"][0]["source"], "claude_md");
        assert_eq!(
            check.data["unloaded_memory_files"][0],
            "/tmp/project/AGENTS.md"
        );
    }

    #[test]
    fn status_json_surfaces_session_lifecycle_for_clawhip() {
        let context = super::StatusContext {
            cwd: PathBuf::from("/tmp/project"),
            session_path: None,
            loaded_config_files: 0,
            discovered_config_files: 0,
            memory_file_count: 0,
            memory_files: Vec::new(),
            unloaded_memory_files: Vec::new(),
            project_root: Some(PathBuf::from("/tmp/project")),
            git_branch: Some("feature/session-lifecycle".to_string()),
            git_summary: GitWorkspaceSummary::default(),
            branch_freshness: test_branch_freshness(),
            stale_base_state: super::BaseCommitState::NoExpectedBase,
            session_lifecycle: SessionLifecycleSummary {
                kind: SessionLifecycleKind::RunningProcess,
                pane_id: Some("%9".to_string()),
                pane_command: Some("claw".to_string()),
                pane_path: Some(PathBuf::from("/tmp/project")),
                workspace_dirty: false,
                abandoned: false,
                all_panes: vec![],
            },
            boot_preflight: test_boot_preflight(),
            sandbox_status: runtime::SandboxStatus::default(),
            binary_provenance: super::binary_provenance_for(None),
            config_load_error: None,
            config_load_error_kind: None,
            mcp_validation: super::McpValidationSummary::default(),

            hook_validation: super::HookValidationSummary::default(),
            duplicate_flags: Vec::new(),
        };

        let value = status_json_value(
            Some("claude-sonnet"),
            StatusUsage {
                message_count: 0,
                turns: 0,
                latest: runtime::TokenUsage::default(),
                cumulative: runtime::TokenUsage::default(),
                estimated_tokens: 0,
            },
            "workspace-write",
            &context,
            None,
            None,
            None,
            None,
        );

        assert_eq!(
            value["workspace"]["session_lifecycle"]["kind"],
            "running_process"
        );
        assert_eq!(
            value["workspace"]["session_lifecycle"]["pane_command"],
            "claw"
        );
        assert_eq!(value["workspace"]["session_lifecycle"]["abandoned"], false);
        assert_eq!(value["workspace"]["branch_freshness"]["fresh"], true);
        assert_eq!(
            value["workspace"]["boot_preflight"]["repo"]["worktree_exists"],
            true
        );
        assert_eq!(
            value["workspace"]["boot_preflight"]["mcp_startup"]["eligible"],
            true
        );
        assert_eq!(
            value["workspace"]["boot_preflight"]["last_failed_boot_reason"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn branch_freshness_parses_ahead_behind_status_header() {
        let freshness = super::BranchFreshness::from_git_status(Some(
            "## feature/boot...origin/feature/boot [ahead 2, behind 3]\n M src/main.rs",
        ));

        assert_eq!(freshness.upstream.as_deref(), Some("origin/feature/boot"));
        assert_eq!(freshness.ahead, 2);
        assert_eq!(freshness.behind, 3);
        assert_eq!(freshness.fresh, Some(false));
    }

    #[test]
    fn boot_preflight_snapshot_reports_machine_readable_contract_fields() {
        let _guard = env_lock();
        let workspace = temp_workspace("boot-preflight-json");
        fs::create_dir_all(&workspace).expect("workspace should create");
        git(&["init", "--quiet"], &workspace);
        git(&["config", "user.email", "tests@example.com"], &workspace);
        git(&["config", "user.name", "Rusty Claude Tests"], &workspace);
        fs::write(workspace.join("tracked.txt"), "hello\n").expect("write tracked");
        fs::write(workspace.join(".claw.json"), r#"{"trustedRoots": ["."]}"#)
            .expect("write config");
        git(&["add", "tracked.txt"], &workspace);
        git(&["commit", "-m", "init", "--quiet"], &workspace);

        let loader = ConfigLoader::default_for(&workspace);
        let config = loader.load().expect("config should load");
        let status = super::run_git_capture_in(&workspace, &["status", "--short", "--branch"]);
        let snapshot = super::build_boot_preflight_snapshot(
            &workspace,
            Some(&workspace),
            status.as_deref(),
            Some(&config),
            None,
        );
        let json = snapshot.json_value();

        assert_eq!(json["repo"]["exists"], true);
        assert_eq!(json["repo"]["worktree_exists"], true);
        assert_eq!(json["trust_gate"]["allowlisted"], true);
        assert_eq!(json["mcp_startup"]["eligible"], true);
        assert!(json["required_binaries"]
            .as_array()
            .is_some_and(|items| { items.iter().any(|item| item["name"] == "git") }));
        fs::remove_dir_all(workspace).expect("cleanup temp dir");
    }

    #[test]
    fn commit_reports_surface_workspace_context() {
        let summary = GitWorkspaceSummary {
            changed_files: 2,
            staged_files: 1,
            unstaged_files: 1,
            untracked_files: 0,
            conflicted_files: 0,
            operation: GitOperation::None,
        };

        let preflight = format_commit_preflight_report(Some("feature/ux"), summary);
        assert!(preflight.contains("Result           ready"));
        assert!(preflight.contains("Branch           feature/ux"));
        assert!(preflight.contains("Workspace        dirty · 2 files · 1 staged, 1 unstaged"));
        assert!(preflight
            .contains("Action           create a git commit from the current workspace changes"));
    }

    #[test]
    fn commit_skipped_report_points_to_next_steps() {
        let report = format_commit_skipped_report();
        assert!(report.contains("Reason           no workspace changes"));
        assert!(report
            .contains("Action           create a git commit from the current workspace changes"));
        assert!(report.contains("/status to inspect context"));
        assert!(report.contains("/diff to inspect repo changes"));
    }

    #[test]
    fn runtime_slash_reports_describe_command_behavior() {
        let bughunter = format_bughunter_report(Some("runtime"));
        assert!(bughunter.contains("Scope            runtime"));
        assert!(bughunter.contains("inspect the selected code for likely bugs"));

        let ultraplan = format_ultraplan_report(Some("ship the release"));
        assert!(ultraplan.contains("Task             ship the release"));
        assert!(ultraplan.contains("break work into a multi-step execution plan"));

        let pr = format_pr_report("feature/ux", Some("ready for review"));
        assert!(pr.contains("Branch           feature/ux"));
        assert!(pr.contains("draft or create a pull request"));

        let issue = format_issue_report(Some("flaky test"));
        assert!(issue.contains("Context          flaky test"));
        assert!(issue.contains("draft or create a GitHub issue"));
    }

    #[test]
    fn no_arg_commands_reject_unexpected_arguments() {
        assert!(validate_no_args("/commit", None).is_ok());

        let error = validate_no_args("/commit", Some("now"))
            .expect_err("unexpected arguments should fail")
            .to_string();
        assert!(error.contains("/commit does not accept arguments"));
        assert!(error.contains("Received: now"));
    }

    #[test]
    fn config_report_supports_section_views() {
        let report = render_config_report(Some("env")).expect("config report should render");
        assert!(report.contains("Merged section: env"));
        let plugins_report =
            render_config_report(Some("plugins")).expect("plugins config report should render");
        assert!(plugins_report.contains("Merged section: plugins"));
    }

    #[test]
    fn memory_report_uses_sectioned_layout() {
        let report = render_memory_report().expect("memory report should render");
        assert!(report.contains("Memory"));
        assert!(report.contains("Working directory"));
        assert!(report.contains("Instruction files"));
        assert!(report.contains("Discovered files"));
    }

    #[test]
    fn config_report_uses_sectioned_layout() {
        let report = render_config_report(None).expect("config report should render");
        assert!(report.contains("Config"));
        assert!(report.contains("Discovered files"));
        assert!(report.contains("Merged JSON"));
    }

    #[test]
    fn parses_git_status_metadata() {
        let _guard = env_lock();
        let temp_root = temp_dir();
        fs::create_dir_all(&temp_root).expect("root dir");
        let (project_root, branch) = parse_git_status_metadata_for(
            &temp_root,
            Some(
                "## rcc/cli...origin/rcc/cli
 M src/main.rs",
            ),
        );
        assert_eq!(branch.as_deref(), Some("rcc/cli"));
        assert!(project_root.is_none());
        fs::remove_dir_all(temp_root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_detached_head_from_status_snapshot() {
        let _guard = env_lock();
        assert_eq!(
            parse_git_status_branch(Some(
                "## HEAD (no branch)
 M src/main.rs"
            )),
            Some("detached HEAD".to_string())
        );
    }

    #[test]
    fn parses_git_workspace_summary_counts() {
        let summary = parse_git_workspace_summary(Some(
            "## feature/ux
M  src/main.rs
 M README.md
?? notes.md
UU conflicted.rs",
        ));

        assert_eq!(
            summary,
            GitWorkspaceSummary {
                changed_files: 4,
                staged_files: 2,
                unstaged_files: 2,
                untracked_files: 1,
                conflicted_files: 1,
                operation: GitOperation::None,
            }
        );
        assert_eq!(
            summary.headline(),
            "dirty · 4 files · 2 staged, 2 unstaged, 1 untracked, 1 conflicted"
        );
    }

    #[test]
    fn render_diff_report_shows_clean_tree_for_committed_repo() {
        let _guard = env_lock();
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir");
        git(&["init", "--quiet"], &root);
        git(&["config", "user.email", "tests@example.com"], &root);
        git(&["config", "user.name", "Rusty Claude Tests"], &root);
        fs::write(root.join("tracked.txt"), "hello\n").expect("write file");
        git(&["add", "tracked.txt"], &root);
        git(&["commit", "-m", "init", "--quiet"], &root);

        let report = render_diff_report_for(&root).expect("diff report should render");
        assert!(report.contains("clean working tree"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn render_diff_report_includes_staged_and_unstaged_sections() {
        let _guard = env_lock();
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir");
        git(&["init", "--quiet"], &root);
        git(&["config", "user.email", "tests@example.com"], &root);
        git(&["config", "user.name", "Rusty Claude Tests"], &root);
        fs::write(root.join("tracked.txt"), "hello\n").expect("write file");
        git(&["add", "tracked.txt"], &root);
        git(&["commit", "-m", "init", "--quiet"], &root);

        fs::write(root.join("tracked.txt"), "hello\nstaged\n").expect("update file");
        git(&["add", "tracked.txt"], &root);
        fs::write(root.join("tracked.txt"), "hello\nstaged\nunstaged\n")
            .expect("update file twice");

        let report = render_diff_report_for(&root).expect("diff report should render");
        assert!(report.contains("Staged changes:"));
        assert!(report.contains("Unstaged changes:"));
        assert!(report.contains("tracked.txt"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn render_diff_report_omits_ignored_files() {
        let _guard = env_lock();
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir");
        git(&["init", "--quiet"], &root);
        git(&["config", "user.email", "tests@example.com"], &root);
        git(&["config", "user.name", "Rusty Claude Tests"], &root);
        fs::write(root.join(".gitignore"), ".omx/\nignored.txt\n").expect("write gitignore");
        fs::write(root.join("tracked.txt"), "hello\n").expect("write tracked");
        git(&["add", ".gitignore", "tracked.txt"], &root);
        git(&["commit", "-m", "init", "--quiet"], &root);
        fs::create_dir_all(root.join(".omx")).expect("write omx dir");
        fs::write(root.join(".omx").join("state.json"), "{}").expect("write ignored omx");
        fs::write(root.join("ignored.txt"), "secret\n").expect("write ignored file");
        fs::write(root.join("tracked.txt"), "hello\nworld\n").expect("write tracked change");

        let report = render_diff_report_for(&root).expect("diff report should render");
        assert!(report.contains("tracked.txt"));
        assert!(!report.contains("+++ b/ignored.txt"));
        assert!(!report.contains("+++ b/.omx/state.json"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn resume_diff_command_renders_report_for_saved_session() {
        let _guard = env_lock();
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir");
        git(&["init", "--quiet"], &root);
        git(&["config", "user.email", "tests@example.com"], &root);
        git(&["config", "user.name", "Rusty Claude Tests"], &root);
        fs::write(root.join("tracked.txt"), "hello\n").expect("write tracked");
        git(&["add", "tracked.txt"], &root);
        git(&["commit", "-m", "init", "--quiet"], &root);
        fs::write(root.join("tracked.txt"), "hello\nworld\n").expect("modify tracked");
        let session_path = root.join("session.json");
        Session::new()
            .save_to_path(&session_path)
            .expect("session should save");

        let session = Session::load_from_path(&session_path).expect("session should load");
        let outcome = with_current_dir(&root, || {
            run_resume_command(&session_path, &session, &SlashCommand::Diff)
                .expect("resume diff should work")
        });
        let message = outcome.message.expect("diff message should exist");
        assert!(message.contains("Unstaged changes:"));
        assert!(message.contains("tracked.txt"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn status_context_reads_real_workspace_metadata() {
        let context = status_context(None).expect("status context should load");
        assert!(context.cwd.is_absolute());
        assert!(context.discovered_config_files >= context.loaded_config_files);
        assert!(context.loaded_config_files <= context.discovered_config_files);
    }

    #[test]
    fn normalizes_supported_permission_modes() {
        assert_eq!(normalize_permission_mode("read-only"), Some("read-only"));
        assert_eq!(
            normalize_permission_mode("workspace-write"),
            Some("workspace-write")
        );
        assert_eq!(
            normalize_permission_mode("danger-full-access"),
            Some("danger-full-access")
        );
        assert_eq!(normalize_permission_mode("unknown"), None);
    }

    #[test]
    fn clear_command_requires_explicit_confirmation_flag() {
        assert_eq!(
            SlashCommand::parse("/clear"),
            Ok(Some(SlashCommand::Clear { confirm: false }))
        );
        assert_eq!(
            SlashCommand::parse("/clear --confirm"),
            Ok(Some(SlashCommand::Clear { confirm: true }))
        );
    }

    #[test]
    fn parses_resume_and_config_slash_commands() {
        assert_eq!(
            SlashCommand::parse("/resume saved-session.jsonl"),
            Ok(Some(SlashCommand::Resume {
                session_path: Some("saved-session.jsonl".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/clear --confirm"),
            Ok(Some(SlashCommand::Clear { confirm: true }))
        );
        assert_eq!(
            SlashCommand::parse("/config"),
            Ok(Some(SlashCommand::Config { section: None }))
        );
        assert_eq!(
            SlashCommand::parse("/config env"),
            Ok(Some(SlashCommand::Config {
                section: Some("env".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/memory"),
            Ok(Some(SlashCommand::Memory))
        );
        assert_eq!(SlashCommand::parse("/init"), Ok(Some(SlashCommand::Init)));
        assert_eq!(
            SlashCommand::parse("/session fork incident-review"),
            Ok(Some(SlashCommand::Session {
                action: Some("fork".to_string()),
                target: Some("incident-review".to_string())
            }))
        );
    }

    #[test]
    fn help_mentions_jsonl_resume_examples() {
        let mut help = Vec::new();
        print_help_to(&mut help).expect("help should render");
        let help = String::from_utf8(help).expect("help should be utf8");
        assert!(help.contains("claw --resume [SESSION.jsonl|session-id|latest]"));
        assert!(help.contains("Use `latest` with --resume, /resume, or /session switch"));
        assert!(help.contains("claw --resume latest"));
        assert!(help.contains("claw --resume latest /status /diff /export notes.txt"));
    }

    #[test]
    fn managed_sessions_default_to_jsonl_and_resolve_legacy_json() {
        let _guard = cwd_guard();
        let workspace = temp_workspace("session-resolution");
        std::fs::create_dir_all(&workspace).expect("workspace should create");
        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&workspace).expect("switch cwd");

        let handle = create_managed_session_handle("session-alpha").expect("jsonl handle");
        assert!(handle.path.ends_with("session-alpha.jsonl"));

        let legacy_path = workspace.join(".claw/sessions/legacy.json");
        std::fs::create_dir_all(
            legacy_path
                .parent()
                .expect("legacy path should have parent directory"),
        )
        .expect("session dir should exist");
        Session::new()
            .with_workspace_root(workspace.clone())
            .with_persistence_path(legacy_path.clone())
            .save_to_path(&legacy_path)
            .expect("legacy session should save");

        let resolved = resolve_session_reference("legacy").expect("legacy session should resolve");
        assert_eq!(
            resolved
                .path
                .canonicalize()
                .expect("resolved path should exist"),
            legacy_path
                .canonicalize()
                .expect("legacy path should exist")
        );

        std::env::set_current_dir(previous).expect("restore cwd");
        std::fs::remove_dir_all(workspace).expect("workspace should clean up");
    }

    #[test]
    fn resumed_session_exists_and_delete_have_json_contracts() {
        let _guard = cwd_guard();
        let workspace = temp_workspace("resume-session-json-contracts");
        std::fs::create_dir_all(&workspace).expect("workspace should create");
        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&workspace).expect("switch cwd");

        let active = create_managed_session_handle("session-active").expect("active handle");
        let active_session = Session::new()
            .with_workspace_root(workspace.clone())
            .with_persistence_path(active.path.clone());
        active_session
            .save_to_path(&active.path)
            .expect("active session should save");
        let saved = create_managed_session_handle("session-saved").expect("saved handle");
        Session::new()
            .with_workspace_root(workspace.clone())
            .with_persistence_path(saved.path.clone())
            .save_to_path(&saved.path)
            .expect("saved session should save");

        let exists_command = SlashCommand::parse("/session exists session-saved")
            .expect("parse should succeed")
            .expect("command should exist");
        let exists = run_resume_command(&active.path, &active_session, &exists_command)
            .expect("exists should run")
            .json
            .expect("exists should return json");
        assert_eq!(exists["kind"], "session_exists");
        assert_eq!(exists["session_id"], "session-saved");
        assert_eq!(exists["exists"], true);
        assert_eq!(exists["active"], false);
        assert!(exists["path"].as_str().is_some());

        let missing_command = SlashCommand::parse("/session exists missing-session")
            .expect("parse should succeed")
            .expect("command should exist");
        let missing = run_resume_command(&active.path, &active_session, &missing_command)
            .expect("missing exists should run")
            .json
            .expect("missing exists should return json");
        assert_eq!(missing["kind"], "session_exists");
        assert_eq!(missing["exists"], false);
        assert_eq!(missing["session_id"], "missing-session");
        assert!(missing["candidate_path"].as_str().is_some());

        let list_command = SlashCommand::parse("/session list")
            .expect("parse should succeed")
            .expect("command should exist");
        let list = run_resume_command(&active.path, &active_session, &list_command)
            .expect("list should run")
            .json
            .expect("list should return json");
        assert_eq!(list["kind"], "sessions");
        let details = list["session_details"]
            .as_array()
            .expect("session_details should be an array");
        let saved_path = saved.path.display().to_string();
        let saved_detail = details
            .iter()
            .find(|detail| detail["path"] == saved_path)
            .expect("saved session detail should exist");
        let created_at_ms = saved_detail["created_at_ms"]
            .as_u64()
            .expect("created_at_ms should be present");
        let updated_at_ms = saved_detail["updated_at_ms"]
            .as_u64()
            .expect("updated_at_ms should be present");
        assert!(
            created_at_ms <= updated_at_ms,
            "created_at_ms should not be after updated_at_ms"
        );

        let delete_command = SlashCommand::parse("/session delete session-saved --force")
            .expect("parse should succeed")
            .expect("command should exist");
        let deleted = run_resume_command(&active.path, &active_session, &delete_command)
            .expect("delete should run")
            .json
            .expect("delete should return json");
        assert_eq!(deleted["kind"], "session_delete");
        assert_eq!(deleted["deleted"], true);
        assert!(!saved.path.exists(), "saved session should be deleted");

        std::env::set_current_dir(previous).expect("restore cwd");
        std::fs::remove_dir_all(workspace).expect("workspace should clean up");
    }

    #[test]
    fn latest_session_alias_resolves_most_recent_managed_session() {
        let _guard = cwd_guard();
        let workspace = temp_workspace("latest-session-alias");
        std::fs::create_dir_all(&workspace).expect("workspace should create");
        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&workspace).expect("switch cwd");

        let older = create_managed_session_handle("session-older").expect("older handle");
        {
            let mut session = Session::new().with_persistence_path(older.path.clone());
            session
                .push_user_text("older session message")
                .expect("older message should save");
            session
                .save_to_path(&older.path)
                .expect("older session should save");
        }
        std::thread::sleep(Duration::from_millis(20));
        let newer = create_managed_session_handle("session-newer").expect("newer handle");
        {
            let mut session = Session::new().with_persistence_path(newer.path.clone());
            session
                .push_user_text("newer session message")
                .expect("newer message should save");
            session
                .save_to_path(&newer.path)
                .expect("newer session should save");
        }

        let resolved = resolve_session_reference("latest").expect("latest session should resolve");
        assert_eq!(
            resolved
                .path
                .canonicalize()
                .expect("resolved path should exist"),
            newer.path.canonicalize().expect("newer path should exist")
        );

        std::env::set_current_dir(previous).expect("restore cwd");
        std::fs::remove_dir_all(workspace).expect("workspace should clean up");
    }

    #[test]
    fn load_session_reference_rejects_workspace_mismatch() {
        let _guard = cwd_guard();
        let workspace_a = temp_workspace("session-mismatch-a");
        let workspace_b = temp_workspace("session-mismatch-b");
        std::fs::create_dir_all(&workspace_a).expect("workspace a should create");
        std::fs::create_dir_all(&workspace_b).expect("workspace b should create");
        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&workspace_b).expect("switch cwd");

        let session_path = workspace_a.join(".claw/sessions/legacy-cross.jsonl");
        std::fs::create_dir_all(
            session_path
                .parent()
                .expect("session path should have parent directory"),
        )
        .expect("session dir should exist");
        Session::new()
            .with_workspace_root(workspace_a.clone())
            .with_persistence_path(session_path.clone())
            .save_to_path(&session_path)
            .expect("session should save");

        let error = crate::load_session_reference(&session_path.display().to_string())
            .expect_err("mismatched workspace should fail");
        assert!(
            error.to_string().contains("session workspace mismatch"),
            "unexpected error: {error}"
        );
        assert!(
            error
                .to_string()
                .contains(&workspace_b.display().to_string()),
            "expected current workspace in error: {error}"
        );
        assert!(
            error
                .to_string()
                .contains(&workspace_a.display().to_string()),
            "expected originating workspace in error: {error}"
        );

        std::env::set_current_dir(previous).expect("restore cwd");
        std::fs::remove_dir_all(workspace_a).expect("workspace a should clean up");
        std::fs::remove_dir_all(workspace_b).expect("workspace b should clean up");
    }

    #[test]
    fn unknown_slash_command_guidance_suggests_nearby_commands() {
        let message = format_unknown_slash_command("stats");
        assert!(message.contains("Unknown slash command: /stats"));
        assert!(message.contains("/status"));
        assert!(message.contains("/help"));
    }

    #[test]
    fn unknown_omc_slash_command_guidance_explains_runtime_gap() {
        let message = format_unknown_slash_command("oh-my-claudecode:hud");
        assert!(message.contains("Unknown slash command: /oh-my-claudecode:hud"));
        assert!(message.contains("Claude Code/OMC plugin command"));
        assert!(message.contains("does not yet load plugin slash commands"));
    }

    #[test]
    fn resume_usage_mentions_latest_shortcut() {
        let usage = render_resume_usage();
        assert!(usage.contains("/resume <session-path|session-id|latest>"));
        assert!(usage.contains(".claw/sessions/<workspace-fingerprint>/<session-id>.jsonl"));
        assert!(usage.contains("/session list"));
    }

    fn cwd_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn cwd_guard() -> MutexGuard<'static, ()> {
        cwd_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn cwd_guard_recovers_after_poisoning() {
        let poisoned = std::thread::spawn(|| {
            let _guard = cwd_guard();
            panic!("poison cwd lock");
        })
        .join();
        assert!(poisoned.is_err(), "poisoning thread should panic");

        let _guard = cwd_guard();
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("claw-cli-{label}-{nanos}"))
    }

    #[test]
    fn init_template_mentions_detected_rust_workspace() {
        let _guard = cwd_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let rendered = crate::init::render_init_claude_md(&workspace_root);
        assert!(rendered.contains("# CLAUDE.md"));
        assert!(rendered.contains("cargo clippy --workspace --all-targets -- -D warnings"));
    }

    #[test]
    fn converts_tool_roundtrip_messages() {
        let messages = vec![
            ConversationMessage::user_text("hello"),
            ConversationMessage::assistant(vec![ContentBlock::ToolUse {
                id: "tool-1".to_string(),
                name: "bash".to_string(),
                input: "{\"command\":\"pwd\"}".to_string(),
            }]),
            ConversationMessage {
                role: MessageRole::Tool,
                blocks: vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-1".to_string(),
                    tool_name: "bash".to_string(),
                    output: "ok".to_string(),
                    is_error: false,
                }],
                usage: None,
            },
        ];

        let converted = super::convert_messages(&messages);
        assert_eq!(converted.len(), 3);
        assert_eq!(converted[1].role, "assistant");
        assert_eq!(converted[2].role, "user");
    }
    #[test]
    fn repl_help_mentions_history_completion_and_multiline() {
        let help = render_repl_help();
        assert!(help.contains("Up/Down"));
        assert!(help.contains("Tab"));
        assert!(help.contains("Shift+Enter/Ctrl+J"));
        assert!(help.contains("Ctrl-R"));
        assert!(help.contains("Reverse-search prompt history"));
        assert!(help.contains("/history [count]"));
    }

    #[test]
    fn parse_history_count_defaults_to_twenty_when_missing() {
        // given
        let raw: Option<&str> = None;

        // when
        let parsed = parse_history_count(raw);

        // then
        assert_eq!(parsed, Ok(20));
    }

    #[test]
    fn parse_history_count_accepts_positive_integers() {
        // given
        let raw = Some("25");

        // when
        let parsed = parse_history_count(raw);

        // then
        assert_eq!(parsed, Ok(25));
    }

    #[test]
    fn parse_history_count_rejects_zero() {
        // given
        let raw = Some("0");

        // when
        let parsed = parse_history_count(raw);

        // then
        assert!(parsed.is_err());
        assert!(parsed.unwrap_err().contains("greater than 0"));
    }

    #[test]
    fn parse_history_count_rejects_non_numeric() {
        // given
        let raw = Some("abc");

        // when
        let parsed = parse_history_count(raw);

        // then
        // #776: updated to match new invalid_history_count: prefix format
        let err = parsed.expect_err("non-numeric count should fail");
        assert!(err.contains("invalid_history_count:") && err.contains("'abc'"));
    }

    #[test]
    fn format_history_timestamp_renders_iso8601_utc() {
        // given
        // 2023-01-15T12:34:56.789Z -> 1673786096789 ms
        let timestamp_ms: u64 = 1_673_786_096_789;

        // when
        let formatted = format_history_timestamp(timestamp_ms);

        // then
        assert_eq!(formatted, "2023-01-15T12:34:56.789Z");
    }

    #[test]
    fn format_history_timestamp_renders_unix_epoch_origin() {
        // given
        let timestamp_ms: u64 = 0;

        // when
        let formatted = format_history_timestamp(timestamp_ms);

        // then
        assert_eq!(formatted, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn render_prompt_history_report_lists_entries_with_timestamps() {
        // given
        let entries = vec![
            PromptHistoryEntry {
                timestamp_ms: 1_673_786_096_000,
                text: "first prompt".to_string(),
            },
            PromptHistoryEntry {
                timestamp_ms: 1_673_786_100_000,
                text: "second prompt".to_string(),
            },
        ];

        // when
        let rendered = render_prompt_history_report(&entries, 10);

        // then
        assert!(rendered.contains("Prompt history"));
        assert!(rendered.contains("Total            2"));
        assert!(rendered.contains("Showing          2 most recent"));
        assert!(rendered.contains("Reverse search   Ctrl-R in the REPL"));
        assert!(rendered.contains("2023-01-15T12:34:56.000Z"));
        assert!(rendered.contains("first prompt"));
        assert!(rendered.contains("second prompt"));
    }

    #[test]
    fn render_prompt_history_report_truncates_to_limit_from_the_tail() {
        // given
        let entries = vec![
            PromptHistoryEntry {
                timestamp_ms: 1_000,
                text: "older".to_string(),
            },
            PromptHistoryEntry {
                timestamp_ms: 2_000,
                text: "middle".to_string(),
            },
            PromptHistoryEntry {
                timestamp_ms: 3_000,
                text: "latest".to_string(),
            },
        ];

        // when
        let rendered = render_prompt_history_report(&entries, 2);

        // then
        assert!(rendered.contains("Total            3"));
        assert!(rendered.contains("Showing          2 most recent"));
        assert!(!rendered.contains("older"));
        assert!(rendered.contains("middle"));
        assert!(rendered.contains("latest"));
    }

    #[test]
    fn render_prompt_history_report_handles_empty_history() {
        // given
        let entries: Vec<PromptHistoryEntry> = Vec::new();

        // when
        let rendered = render_prompt_history_report(&entries, 10);

        // then
        assert!(rendered.contains("no prompts recorded yet"));
    }

    #[test]
    fn collect_session_prompt_history_extracts_user_text_blocks() {
        // given
        let mut session = Session::new();
        session.push_user_text("hello").unwrap();
        session.push_user_text("world").unwrap();

        // when
        let entries = collect_session_prompt_history(&session);

        // then
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "hello");
        assert_eq!(entries[1].text, "world");
    }

    #[test]
    fn tool_rendering_helpers_compact_output() {
        let start = format_tool_call_start("read_file", r#"{"path":"src/main.rs"}"#);
        assert!(start.contains("read_file"));
        assert!(start.contains("src/main.rs"));

        let done = format_tool_result(
            "read_file",
            r#"{"file":{"filePath":"src/main.rs","content":"hello","numLines":1,"startLine":1,"totalLines":1}}"#,
            false,
        );
        assert!(done.contains("📄 Read src/main.rs"));
        assert!(done.contains("hello"));
    }

    #[test]
    fn tool_rendering_truncates_large_read_output_for_display_only() {
        let content = (0..200)
            .map(|index| format!("line {index:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let output = json!({
            "file": {
                "filePath": "src/main.rs",
                "content": content,
                "numLines": 200,
                "startLine": 1,
                "totalLines": 200
            }
        })
        .to_string();

        let rendered = format_tool_result("read_file", &output, false);

        assert!(rendered.contains("line 000"));
        assert!(rendered.contains("line 079"));
        assert!(!rendered.contains("line 199"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("line 199"));
    }

    #[test]
    fn tool_rendering_truncates_large_bash_output_for_display_only() {
        let stdout = (0..120)
            .map(|index| format!("stdout {index:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let output = json!({
            "stdout": stdout,
            "stderr": "",
            "returnCodeInterpretation": "completed successfully"
        })
        .to_string();

        let rendered = format_tool_result("bash", &output, false);

        assert!(rendered.contains("stdout 000"));
        assert!(rendered.contains("stdout 059"));
        assert!(!rendered.contains("stdout 119"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("stdout 119"));
    }

    #[test]
    fn tool_rendering_truncates_generic_long_output_for_display_only() {
        let items = (0..120)
            .map(|index| format!("payload {index:03}"))
            .collect::<Vec<_>>();
        let output = json!({
            "summary": "plugin payload",
            "items": items,
        })
        .to_string();

        let rendered = format_tool_result("plugin_echo", &output, false);

        assert!(rendered.contains("plugin_echo"));
        assert!(rendered.contains("payload 000"));
        assert!(rendered.contains("payload 040"));
        assert!(!rendered.contains("payload 080"));
        assert!(!rendered.contains("payload 119"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("payload 119"));
    }

    #[test]
    fn tool_rendering_truncates_raw_generic_output_for_display_only() {
        let output = (0..120)
            .map(|index| format!("raw {index:03}"))
            .collect::<Vec<_>>()
            .join("\n");

        let rendered = format_tool_result("plugin_echo", &output, false);

        assert!(rendered.contains("plugin_echo"));
        assert!(rendered.contains("raw 000"));
        assert!(rendered.contains("raw 059"));
        assert!(!rendered.contains("raw 119"));
        assert!(rendered.contains("full result preserved in session"));
        assert!(output.contains("raw 119"));
    }

    #[test]
    fn ultraplan_progress_lines_include_phase_step_and_elapsed_status() {
        let snapshot = InternalPromptProgressState {
            command_label: "Ultraplan",
            task_label: "ship plugin progress".to_string(),
            step: 3,
            phase: "running read_file".to_string(),
            detail: Some("reading rust/crates/rusty-claude-cli/src/main.rs".to_string()),
            saw_final_text: false,
        };

        let started = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Started,
            &snapshot,
            Duration::from_secs(0),
            None,
        );
        let heartbeat = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Heartbeat,
            &snapshot,
            Duration::from_secs(9),
            None,
        );
        let completed = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Complete,
            &snapshot,
            Duration::from_secs(12),
            None,
        );
        let failed = format_internal_prompt_progress_line(
            InternalPromptProgressEvent::Failed,
            &snapshot,
            Duration::from_secs(12),
            Some("network timeout"),
        );

        assert!(started.contains("planning started"));
        assert!(started.contains("current step 3"));
        assert!(heartbeat.contains("heartbeat"));
        assert!(heartbeat.contains("9s elapsed"));
        assert!(heartbeat.contains("phase running read_file"));
        assert!(completed.contains("completed"));
        assert!(completed.contains("3 steps total"));
        assert!(failed.contains("failed"));
        assert!(failed.contains("network timeout"));
    }

    #[test]
    fn describe_tool_progress_summarizes_known_tools() {
        assert_eq!(
            describe_tool_progress("read_file", r#"{"path":"src/main.rs"}"#),
            "reading src/main.rs"
        );
        assert!(
            describe_tool_progress("bash", r#"{"command":"cargo test -p rusty-claude-cli"}"#)
                .contains("cargo test -p rusty-claude-cli")
        );
        assert_eq!(
            describe_tool_progress("grep_search", r#"{"pattern":"ultraplan","path":"rust"}"#),
            "grep `ultraplan` in rust"
        );
    }

    #[test]
    fn push_output_block_renders_markdown_text() {
        let mut out = Vec::new();
        let mut events = Vec::new();
        let mut pending_tool = None;
        let mut block_has_thinking_summary = false;

        push_output_block(
            OutputContentBlock::Text {
                text: "# Heading".to_string(),
            },
            &mut out,
            &mut events,
            &mut pending_tool,
            false,
            &mut block_has_thinking_summary,
        )
        .expect("text block should render");

        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("Heading"));
        assert!(rendered.contains('\u{1b}'));
    }

    #[test]
    fn push_output_block_skips_empty_object_prefix_for_tool_streams() {
        let mut out = Vec::new();
        let mut events = Vec::new();
        let mut pending_tool = None;
        let mut block_has_thinking_summary = false;

        push_output_block(
            OutputContentBlock::ToolUse {
                id: "tool-1".to_string(),
                name: "read_file".to_string(),
                input: json!({}),
            },
            &mut out,
            &mut events,
            &mut pending_tool,
            true,
            &mut block_has_thinking_summary,
        )
        .expect("tool block should accumulate");

        assert!(events.is_empty());
        assert_eq!(
            pending_tool,
            Some(("tool-1".to_string(), "read_file".to_string(), String::new(),))
        );
    }

    #[test]
    fn response_to_events_preserves_empty_object_json_input_outside_streaming() {
        let mut out = Vec::new();
        let events = response_to_events(
            MessageResponse {
                id: "msg-1".to_string(),
                kind: "message".to_string(),
                model: "anthropic/claude-opus-4-6".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentBlock::ToolUse {
                    id: "tool-1".to_string(),
                    name: "read_file".to_string(),
                    input: json!({}),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                request_id: None,
            },
            &mut out,
        )
        .expect("response conversion should succeed");

        assert!(matches!(
            &events[0],
            AssistantEvent::ToolUse { name, input, .. }
                if name == "read_file" && input == "{}"
        ));
    }

    #[test]
    fn response_to_events_preserves_non_empty_json_input_outside_streaming() {
        let mut out = Vec::new();
        let events = response_to_events(
            MessageResponse {
                id: "msg-2".to_string(),
                kind: "message".to_string(),
                model: "anthropic/claude-opus-4-6".to_string(),
                role: "assistant".to_string(),
                content: vec![OutputContentBlock::ToolUse {
                    id: "tool-2".to_string(),
                    name: "read_file".to_string(),
                    input: json!({ "path": "rust/Cargo.toml" }),
                }],
                stop_reason: Some("tool_use".to_string()),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                request_id: None,
            },
            &mut out,
        )
        .expect("response conversion should succeed");

        assert!(matches!(
            &events[0],
            AssistantEvent::ToolUse { name, input, .. }
                if name == "read_file" && input == "{\"path\":\"rust/Cargo.toml\"}"
        ));
    }

    #[test]
    fn response_to_events_renders_collapsed_thinking_summary() {
        let mut out = Vec::new();
        let events = response_to_events(
            MessageResponse {
                id: "msg-3".to_string(),
                kind: "message".to_string(),
                model: "anthropic/claude-opus-4-6".to_string(),
                role: "assistant".to_string(),
                content: vec![
                    OutputContentBlock::Thinking {
                        thinking: "step 1".to_string(),
                        signature: Some("sig_123".to_string()),
                    },
                    OutputContentBlock::Text {
                        text: "Final answer".to_string(),
                    },
                ],
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                request_id: None,
            },
            &mut out,
        )
        .expect("response conversion should succeed");

        assert!(matches!(
            &events[0],
            AssistantEvent::Thinking {
                thinking,
                signature
            } if thinking == "step 1" && signature.as_deref() == Some("sig_123")
        ));
        assert!(matches!(
            &events[1],
            AssistantEvent::TextDelta(text) if text == "Final answer"
        ));
        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("▶ Thinking (6 chars hidden)"));
        assert!(!rendered.contains("step 1"));
    }

    #[test]
    fn build_runtime_plugin_state_merges_plugin_hooks_into_runtime_features() {
        let config_home = temp_dir();
        let workspace = temp_dir();
        let source_root = temp_dir();
        fs::create_dir_all(&config_home).expect("config home");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&source_root).expect("source root");
        write_plugin_fixture(&source_root, "hook-runtime-demo", true, false);

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        manager
            .install(source_root.to_str().expect("utf8 source path"))
            .expect("plugin install should succeed");
        let loader = ConfigLoader::new(&workspace, &config_home);
        let runtime_config = loader.load().expect("runtime config should load");
        let state = build_runtime_plugin_state_with_loader(&workspace, &loader, &runtime_config)
            .expect("plugin state should load");
        let pre_hooks = state.feature_config.hooks().pre_tool_use();
        assert_eq!(pre_hooks.len(), 1);
        assert!(
            pre_hooks[0].ends_with("hooks/pre.sh"),
            "expected installed plugin hook path, got {pre_hooks:?}"
        );

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn build_runtime_plugin_state_discovers_mcp_tools_and_surfaces_pending_servers() {
        let config_home = temp_dir();
        let workspace = temp_dir();
        fs::create_dir_all(&config_home).expect("config home");
        fs::create_dir_all(&workspace).expect("workspace");
        let script_path = workspace.join("fixture-mcp.py");
        write_mcp_server_fixture(&script_path);
        fs::write(
            config_home.join("settings.json"),
            format!(
                r#"{{
                  "mcpServers": {{
                    "alpha": {{
                      "command": "python3",
                      "args": ["{}"]
                    }},
                    "broken": {{
                      "command": "python3",
                      "args": ["-c", "import sys; sys.exit(0)"]
                    }}
                  }}
                }}"#,
                script_path.to_string_lossy()
            ),
        )
        .expect("write mcp settings");

        let loader = ConfigLoader::new(&workspace, &config_home);
        let runtime_config = loader.load().expect("runtime config should load");
        let state = build_runtime_plugin_state_with_loader(&workspace, &loader, &runtime_config)
            .expect("runtime plugin state should load");

        let allowed = state
            .tool_registry
            .normalize_allowed_tools(&["mcp__alpha__echo".to_string(), "MCPTool".to_string()])
            .expect("mcp tools should be allow-listable")
            .expect("allow-list should exist");
        assert!(allowed.contains("mcp__alpha__echo"));
        assert!(allowed.contains("mcp_tool"));

        let mut executor = CliToolExecutor::new(
            None,
            false,
            state.tool_registry.clone(),
            state.mcp_state.clone(),
        );

        let tool_output = executor
            .execute("mcp__alpha__echo", r#"{"text":"hello"}"#)
            .expect("discovered mcp tool should execute");
        let tool_json: serde_json::Value =
            serde_json::from_str(&tool_output).expect("tool output should be json");
        assert_eq!(tool_json["structuredContent"]["echoed"], "hello");

        let wrapped_output = executor
            .execute(
                "MCPTool",
                r#"{"qualifiedName":"mcp__alpha__echo","arguments":{"text":"wrapped"}}"#,
            )
            .expect("generic mcp wrapper should execute");
        let wrapped_json: serde_json::Value =
            serde_json::from_str(&wrapped_output).expect("wrapped output should be json");
        assert_eq!(wrapped_json["structuredContent"]["echoed"], "wrapped");

        let search_output = executor
            .execute("ToolSearch", r#"{"query":"alpha echo","max_results":5}"#)
            .expect("tool search should execute");
        let search_json: serde_json::Value =
            serde_json::from_str(&search_output).expect("search output should be json");
        assert_eq!(search_json["matches"][0], "mcp__alpha__echo");
        assert_eq!(search_json["pending_mcp_servers"][0], "broken");
        assert_eq!(
            search_json["mcp_degraded"]["failed_servers"][0]["server_name"],
            "broken"
        );
        assert_eq!(
            search_json["mcp_degraded"]["failed_servers"][0]["phase"],
            "tool_discovery"
        );
        assert_eq!(
            search_json["mcp_degraded"]["available_tools"][0],
            "mcp__alpha__echo"
        );

        let listed = executor
            .execute("ListMcpResourcesTool", r#"{"server":"alpha"}"#)
            .expect("resources should list");
        let listed_json: serde_json::Value =
            serde_json::from_str(&listed).expect("resource output should be json");
        assert_eq!(listed_json["resources"][0]["uri"], "file://guide.txt");

        let read = executor
            .execute(
                "ReadMcpResourceTool",
                r#"{"server":"alpha","uri":"file://guide.txt"}"#,
            )
            .expect("resource should read");
        let read_json: serde_json::Value =
            serde_json::from_str(&read).expect("resource read output should be json");
        assert_eq!(
            read_json["contents"][0]["text"],
            "contents for file://guide.txt"
        );

        if let Some(mcp_state) = state.mcp_state {
            mcp_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .shutdown()
                .expect("mcp shutdown should succeed");
        }

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn build_runtime_plugin_state_surfaces_unsupported_mcp_servers_structurally() {
        let config_home = temp_dir();
        let workspace = temp_dir();
        fs::create_dir_all(&config_home).expect("config home");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::write(
            config_home.join("settings.json"),
            r#"{
              "mcpServers": {
                "remote": {
                  "url": "https://example.test/mcp"
                }
              }
            }"#,
        )
        .expect("write mcp settings");

        let loader = ConfigLoader::new(&workspace, &config_home);
        let runtime_config = loader.load().expect("runtime config should load");
        let state = build_runtime_plugin_state_with_loader(&workspace, &loader, &runtime_config)
            .expect("runtime plugin state should load");
        let mut executor = CliToolExecutor::new(
            None,
            false,
            state.tool_registry.clone(),
            state.mcp_state.clone(),
        );

        let search_output = executor
            .execute("ToolSearch", r#"{"query":"remote","max_results":5}"#)
            .expect("tool search should execute");
        let search_json: serde_json::Value =
            serde_json::from_str(&search_output).expect("search output should be json");
        assert_eq!(search_json["pending_mcp_servers"][0], "remote");
        assert_eq!(
            search_json["mcp_degraded"]["failed_servers"][0]["server_name"],
            "remote"
        );
        assert_eq!(
            search_json["mcp_degraded"]["failed_servers"][0]["phase"],
            "server_registration"
        );
        assert_eq!(
            search_json["mcp_degraded"]["failed_servers"][0]["error"]["context"]["transport"],
            "http"
        );

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn build_runtime_runs_plugin_lifecycle_init_and_shutdown() {
        // Serialize access to process-wide env vars so parallel tests that
        // set/remove ANTHROPIC_API_KEY do not race with this test.
        let _guard = env_lock();
        let config_home = temp_dir();
        // Inject a dummy API key so runtime construction succeeds without real credentials.
        // This test only exercises plugin lifecycle (init/shutdown), never calls the API.
        std::env::set_var("ANTHROPIC_API_KEY", "test-dummy-key-for-plugin-lifecycle");
        let workspace = temp_dir();
        let source_root = temp_dir();
        fs::create_dir_all(&config_home).expect("config home");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&source_root).expect("source root");
        write_plugin_fixture(&source_root, "lifecycle-runtime-demo", false, true);

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        let install = manager
            .install(source_root.to_str().expect("utf8 source path"))
            .expect("plugin install should succeed");
        let log_path = install.install_path.join("lifecycle.log");
        let loader = ConfigLoader::new(&workspace, &config_home);
        let runtime_config = loader.load().expect("runtime config should load");
        let runtime_plugin_state =
            build_runtime_plugin_state_with_loader(&workspace, &loader, &runtime_config)
                .expect("plugin state should load");
        let mut runtime = build_runtime_with_plugin_state(
            Session::new(),
            "runtime-plugin-lifecycle",
            DEFAULT_MODEL.to_string(),
            vec!["test system prompt".to_string()],
            true,
            false,
            None,
            PermissionMode::DangerFullAccess,
            None,
            runtime_plugin_state,
        )
        .expect("runtime should build");

        assert_eq!(
            fs::read_to_string(&log_path).expect("init log should exist"),
            "init\n"
        );

        runtime
            .shutdown_plugins()
            .expect("plugin shutdown should succeed");

        assert_eq!(
            fs::read_to_string(&log_path).expect("shutdown log should exist"),
            "init\nshutdown\n"
        );

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(source_root);
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn rejects_invalid_reasoning_effort_value() {
        let err = parse_args(&[
            "--reasoning-effort".to_string(),
            "turbo".to_string(),
            "prompt".to_string(),
            "hello".to_string(),
        ])
        .unwrap_err();
        assert!(
            err.contains("invalid value for --reasoning-effort"),
            "unexpected error: {err}"
        );
        assert!(err.contains("turbo"), "unexpected error: {err}");
    }

    #[test]
    fn accepts_valid_reasoning_effort_values() {
        for value in ["low", "medium", "high"] {
            let result = parse_args(&[
                "--reasoning-effort".to_string(),
                value.to_string(),
                "prompt".to_string(),
                "hello".to_string(),
            ]);
            assert!(
                result.is_ok(),
                "--reasoning-effort {value} should be accepted, got: {result:?}"
            );
            if let Ok(CliAction::Prompt {
                reasoning_effort, ..
            }) = result
            {
                assert_eq!(reasoning_effort.as_deref(), Some(value));
            }
        }
    }

    #[test]
    fn stub_commands_absent_from_repl_completions() {
        let candidates =
            slash_command_completion_candidates_with_sessions("claude-3-5-sonnet", None, vec![]);
        for stub in STUB_COMMANDS {
            let with_slash = format!("/{stub}");
            assert!(
                !candidates.contains(&with_slash),
                "stub command {with_slash} should not appear in REPL completions"
            );
        }
    }

    #[test]
    fn stub_commands_absent_from_resume_safe_help() {
        let mut help = Vec::new();
        print_help_to(&mut help).expect("help should render");
        let help = String::from_utf8(help).expect("help should be utf8");
        let resume_line = help
            .lines()
            .find(|line| line.starts_with("Resume-safe commands:"))
            .expect("resume-safe command line should exist");
        let resume_roots = resume_line
            .trim_start_matches("Resume-safe commands:")
            .split(',')
            .filter_map(|entry| entry.trim().strip_prefix('/'))
            .filter_map(|entry| entry.split_whitespace().next())
            .collect::<Vec<_>>();

        for stub in STUB_COMMANDS {
            assert!(
                !resume_roots.contains(stub),
                "stub command /{stub} should not appear in resume-safe command list"
            );
        }

        assert!(resume_roots.contains(&"status"));
    }
}

fn write_mcp_server_fixture(script_path: &Path) {
    let script = [
            "#!/usr/bin/env python3",
            "import json, sys",
            "",
            "def read_message():",
            "    header = b''",
            r"    while not header.endswith(b'\r\n\r\n'):",
            "        chunk = sys.stdin.buffer.read(1)",
            "        if not chunk:",
            "            return None",
            "        header += chunk",
            "    length = 0",
            r"    for line in header.decode().split('\r\n'):",
            r"        if line.lower().startswith('content-length:'):",
            "            length = int(line.split(':', 1)[1].strip())",
            "    payload = sys.stdin.buffer.read(length)",
            "    return json.loads(payload.decode())",
            "",
            "def send_message(message):",
            "    payload = json.dumps(message).encode()",
            r"    sys.stdout.buffer.write(f'Content-Length: {len(payload)}\r\n\r\n'.encode() + payload)",
            "    sys.stdout.buffer.flush()",
            "",
            "while True:",
            "    request = read_message()",
            "    if request is None:",
            "        break",
            "    method = request['method']",
            "    if method == 'initialize':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'protocolVersion': request['params']['protocolVersion'],",
            "                'capabilities': {'tools': {}, 'resources': {}},",
            "                'serverInfo': {'name': 'fixture', 'version': '1.0.0'}",
            "            }",
            "        })",
            "    elif method == 'tools/list':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'tools': [",
            "                    {",
            "                        'name': 'echo',",
            "                        'description': 'Echo from MCP fixture',",
            "                        'inputSchema': {",
            "                            'type': 'object',",
            "                            'properties': {'text': {'type': 'string'}},",
            "                            'required': ['text'],",
            "                            'additionalProperties': False",
            "                        },",
            "                        'annotations': {'readOnlyHint': True}",
            "                    }",
            "                ]",
            "            }",
            "        })",
            "    elif method == 'tools/call':",
            "        args = request['params'].get('arguments') or {}",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'content': [{'type': 'text', 'text': f\"echo:{args.get('text', '')}\"}],",
            "                'structuredContent': {'echoed': args.get('text', '')},",
            "                'isError': False",
            "            }",
            "        })",
            "    elif method == 'resources/list':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'resources': [{'uri': 'file://guide.txt', 'name': 'guide', 'mimeType': 'text/plain'}]",
            "            }",
            "        })",
            "    elif method == 'resources/read':",
            "        uri = request['params']['uri']",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'contents': [{'uri': uri, 'mimeType': 'text/plain', 'text': f'contents for {uri}'}]",
            "            }",
            "        })",
            "    else:",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'error': {'code': -32601, 'message': method}",
            "        })",
            "",
        ]
        .join("\n");
    fs::write(script_path, script).expect("mcp fixture script should write");
}

#[cfg(test)]
mod sandbox_report_tests {
    use super::{format_sandbox_report, HookAbortMonitor};
    use runtime::HookAbortSignal;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn sandbox_report_renders_expected_fields() {
        let report = format_sandbox_report(&runtime::SandboxStatus::default());
        assert!(report.contains("Sandbox"));
        assert!(report.contains("Enabled"));
        assert!(report.contains("Filesystem mode"));
        assert!(report.contains("Fallback reason"));
    }

    #[test]
    fn hook_abort_monitor_stops_without_aborting() {
        let abort_signal = HookAbortSignal::new();
        let (ready_tx, ready_rx) = mpsc::channel();
        let monitor = HookAbortMonitor::spawn_with_waiter(
            abort_signal.clone(),
            move |stop_rx, abort_signal| {
                ready_tx.send(()).expect("ready signal");
                let _ = stop_rx.recv();
                assert!(!abort_signal.is_aborted());
            },
        );

        ready_rx.recv().expect("waiter should be ready");
        monitor.stop();

        assert!(!abort_signal.is_aborted());
    }

    #[test]
    fn hook_abort_monitor_propagates_interrupt() {
        let abort_signal = HookAbortSignal::new();
        let (done_tx, done_rx) = mpsc::channel();
        let monitor = HookAbortMonitor::spawn_with_waiter(
            abort_signal.clone(),
            move |_stop_rx, abort_signal| {
                abort_signal.abort();
                done_tx.send(()).expect("done signal");
            },
        );

        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("interrupt should complete");
        monitor.stop();

        assert!(abort_signal.is_aborted());
    }
}

#[cfg(test)]
mod dump_manifests_tests {
    use super::{build_rust_resolver_manifest, dump_manifests_at_path, CliOutputFormat};
    use std::fs;

    #[test]
    fn dump_manifests_defaults_to_rust_resolver_inventory() {
        let root =
            std::env::temp_dir().join(format!("claw_test_rust_manifests_{}", std::process::id()));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace should exist");

        let manifest = build_rust_resolver_manifest(&workspace).expect("manifest should build");
        assert_eq!(manifest["kind"], "dump-manifests");
        assert_eq!(manifest["source"], "rust-resolver");
        assert!(manifest["commands"].as_u64().expect("commands count") > 0);
        assert!(manifest["tools"].as_u64().expect("tools count") > 0);
        assert!(manifest["command_manifests"]
            .as_array()
            .expect("command manifests")
            .iter()
            .any(|entry| entry["name"] == "status"));
        assert!(manifest["tool_manifests"]
            .as_array()
            .expect("tool manifests")
            .iter()
            .any(|entry| entry["name"] == "read_file"));
        assert!(dump_manifests_at_path(&workspace, None, CliOutputFormat::Text).is_ok());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dump_manifests_scopes_explicit_manifest_dir_without_upstream_ts() {
        let root = std::env::temp_dir().join(format!(
            "claw_test_explicit_manifest_dir_{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let manifest_dir = root.join("manifest-source");
        fs::create_dir_all(&workspace).expect("workspace should exist");
        fs::create_dir_all(&manifest_dir).expect("manifest dir should exist");

        let result = dump_manifests_at_path(&workspace, Some(&manifest_dir), CliOutputFormat::Text);
        assert!(
            result.is_ok(),
            "explicit manifest dir should not require upstream TS files: {result:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dump_manifests_missing_explicit_dir_has_typed_kind() {
        let root = std::env::temp_dir().join(format!(
            "claw_test_missing_manifest_dir_{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let missing = root.join("missing");
        fs::create_dir_all(&workspace).expect("workspace should exist");

        let result = dump_manifests_at_path(&workspace, Some(&missing), CliOutputFormat::Text);
        let error = result.expect_err("missing explicit manifest dir should fail");
        let error_msg = error.to_string();
        assert!(error_msg.starts_with("missing_manifests:"));
        assert!(error_msg.contains(&missing.display().to_string()));
        assert!(!error_msg.contains("CLAUDE_CODE_UPSTREAM"));
        assert!(!error_msg.contains("src/commands.ts"));

        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod alias_resolution_tests {
    fn ollama_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("ollama env lock poisoned")
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn unset(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }

        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    use super::{resolve_model_alias_with_config, validate_model_syntax};

    #[test]
    fn test_alias_resolution_builtin() {
        // Built-in aliases should resolve to their full IDs
        assert_eq!(
            resolve_model_alias_with_config("opus"),
            "anthropic/claude-opus-4-7"
        );
        assert_eq!(
            resolve_model_alias_with_config("sonnet"),
            "anthropic/claude-sonnet-4-6"
        );
        assert_eq!(
            resolve_model_alias_with_config("haiku"),
            "anthropic/claude-haiku-4-5-20251213"
        );
    }

    #[test]
    fn test_alias_resolution_syntax_validation() {
        let _guard = ollama_env_lock();
        let _env = EnvVarGuard::unset("OLLAMA_HOST");
        // Resolved aliases should pass syntax validation
        let resolved = resolve_model_alias_with_config("opus");
        assert!(validate_model_syntax(&resolved).is_ok());

        // Raw aliases should FAIL syntax validation (this is why we resolve first!)
        assert!(validate_model_syntax("opus").is_err());
    }

    #[test]
    fn test_unknown_alias_fails_validation() {
        let _guard = ollama_env_lock();
        let _env = EnvVarGuard::unset("OLLAMA_HOST");
        // Unknown aliases resolve to themselves
        let resolved = resolve_model_alias_with_config("unknown-alias");
        assert_eq!(resolved, "unknown-alias");

        // And then fail validation with a helpful error
        let result = validate_model_syntax(&resolved);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid model syntax"));
    }

    #[test]
    fn qwen_invalid_model_hint_mentions_local_ollama_openai_base_url() {
        let _guard = ollama_env_lock();
        let _ollama_env = EnvVarGuard::unset("OLLAMA_HOST");
        let _openai_env = EnvVarGuard::unset("OPENAI_BASE_URL");
        let result = validate_model_syntax("qwen3:8b");

        let error = result.expect_err("Ollama tag without local base URL should fail");
        assert!(
            error.contains("Ollama"),
            "Qwen Ollama tag error should mention Ollama: {error}"
        );
        assert!(
            error.contains("OPENAI_BASE_URL"),
            "Qwen Ollama tag error should mention OPENAI_BASE_URL: {error}"
        );
        assert!(
            error.contains("http://127.0.0.1:11434/v1"),
            "Qwen Ollama tag error should show local Ollama OpenAI URL: {error}"
        );
    }

    #[test]
    fn test_direct_provider_model_passes() {
        // Direct provider/model strings should remain unchanged and pass
        let model = "openai/gpt-4o";
        assert_eq!(resolve_model_alias_with_config(model), model);
        assert!(validate_model_syntax(model).is_ok());
    }
    #[test]
    fn test_ollama_host_bypasses_model_validation() {
        let _guard = ollama_env_lock();
        let _env = EnvVarGuard::set("OLLAMA_HOST", "http://127.0.0.1:11434");
        // Ollama model names with colons pass
        assert!(validate_model_syntax("qwen3:8b").is_ok());
        assert!(validate_model_syntax("gemma4:e2b").is_ok());
        assert!(validate_model_syntax("qwen3.6:27b-nvfp4").is_ok());
        // Empty model still rejected
        assert!(validate_model_syntax("").is_err());
    }
}
