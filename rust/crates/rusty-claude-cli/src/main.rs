#![allow(
    dead_code,
    unused_imports,
    unused_variables,
    clippy::unneeded_struct_pattern,
    clippy::unnecessary_wraps,
    clippy::unused_self
)]
mod app;
mod args;
mod cli_commands;
mod format;
mod init;
mod input;
mod render;
mod tui;

// Re-exports from extracted format modules so existing code still compiles.
// After Phase 0 is complete, this import brings all extracted items into scope
// as if they were still defined in main.rs.
use args::*;
use format::*;
// Selective imports from app — avoid conflicting with format::* names
use app::*;
use cli_commands::*;

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::net::TcpListener;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, UNIX_EPOCH};

use api::{
    detect_provider_kind, resolve_startup_auth_source, AnthropicClient, AuthSource,
    ContentBlockDelta, InputContentBlock, InputMessage, MessageRequest, MessageResponse,
    OutputContentBlock, PromptCache, ProviderClient as ApiProviderClient, ProviderKind,
    StreamEvent as ApiStreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock,
};

use commands::{
    classify_skills_slash_command, handle_agents_slash_command, handle_agents_slash_command_json,
    handle_mcp_slash_command, handle_mcp_slash_command_json, handle_plugins_slash_command,
    handle_skills_slash_command, handle_skills_slash_command_json, render_slash_command_help,
    render_slash_command_help_filtered, resolve_skill_invocation, resume_supported_slash_commands,
    slash_command_specs, validate_slash_command_input, SkillSlashDispatch, SlashCommand,
};
use compat_harness::{extract_manifest, UpstreamPaths};
use init::initialize_repo;
use plugins::{PluginHooks, PluginManager, PluginManagerConfig, PluginRegistry};
use render::{MarkdownStreamState, Spinner, TerminalRenderer};
use runtime::{
    check_base_commit, format_stale_base_warning, format_usd, load_oauth_credentials,
    load_system_prompt, pricing_for_model, resolve_expected_base, resolve_sandbox_status,
    ApiClient, ApiRequest, AssistantEvent, CompactionConfig, ConfigLoader, ConfigSource,
    ContentBlock, ConversationMessage, ConversationRuntime, McpServer, McpServerManager,
    McpServerSpec, McpTool, MessageRole, ModelPricing, PermissionMode, PermissionPolicy,
    ProjectContext, PromptCacheEvent, ResolvedPermissionMode, RuntimeError, Session, TokenUsage,
    ToolError, ToolExecutor, UsageTracker,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tools::{
    execute_tool, mvp_tool_specs, GlobalToolRegistry, RuntimeToolDefinition, ToolSearchOutput,
};

const DEFAULT_MODEL: &str = "claude-opus-4-6";

// Build-time constants injected by build.rs (fall back to static values when
// build.rs hasn't run, e.g. in doc-test or unusual toolchain environments).
pub(crate) const DEFAULT_DATE: &str = match option_env!("BUILD_DATE") {
    Some(d) => d,
    None => "unknown",
};
const DEFAULT_OAUTH_CALLBACK_PORT: u16 = 4545;
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const BUILD_TARGET: Option<&str> = option_env!("TARGET");
pub(crate) const GIT_SHA: Option<&str> = option_env!("GIT_SHA");
pub(crate) const INTERNAL_PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);
pub(crate) const POST_TOOL_STALL_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const PRIMARY_SESSION_EXTENSION: &str = "jsonl";
pub(crate) const LEGACY_SESSION_EXTENSION: &str = "json";
pub(crate) const OFFICIAL_REPO_URL: &str = "https://github.com/ultraworkers/claw-code";
pub(crate) const OFFICIAL_REPO_SLUG: &str = "ultraworkers/claw-code";
pub(crate) const DEPRECATED_INSTALL_COMMAND: &str = "cargo install claw-code";
pub(crate) const LATEST_SESSION_REFERENCE: &str = "latest";
pub(crate) const SESSION_REFERENCE_ALIASES: &[&str] = &[LATEST_SESSION_REFERENCE, "last", "recent"];
pub(crate) const CLI_OPTION_SUGGESTIONS: &[&str] = &[
    "--help",
    "-h",
    "--version",
    "-V",
    "--model",
    "--output-format",
    "--permission-mode",
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

pub(crate) type AllowedToolSet = BTreeSet<String>;
pub(crate) type RuntimePluginStateBuildOutput = (
    Option<Arc<Mutex<RuntimeMcpState>>>,
    Vec<RuntimeToolDefinition>,
);

#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn normalize_allowed_tools(values: &[String]) -> Result<Option<AllowedToolSet>, String> {
    if values.is_empty() {
        return Ok(None);
    }
    current_tool_registry()?.normalize_allowed_tools(values)
}

pub(crate) fn current_tool_registry() -> Result<GlobalToolRegistry, String> {
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

fn main() {
    if let Err(error) = run() {
        let message = error.to_string();
        // When --output-format json is active, emit errors as JSON so downstream
        // tools can parse failures the same way they parse successes (ROADMAP #42).
        let argv: Vec<String> = std::env::args().collect();
        let json_output = argv
            .windows(2)
            .any(|w| w[0] == "--output-format" && w[1] == "json")
            || argv.iter().any(|a| a == "--output-format=json");
        if json_output {
            // #77: classify error by prefix so downstream claws can route without
            // regex-scraping the prose. Split short-reason from hint-runbook.
            let kind = classify_error_kind(&message);
            let (short_reason, hint) = split_error_hint(&message);
            eprintln!(
                "{}",
                serde_json::json!({
                    "type": "error",
                    "error": short_reason,
                    "kind": kind,
                    "hint": hint,
                })
            );
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

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
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
            output_format,
        } => print_system_prompt(cwd, date, output_format)?,
        CliAction::Version { output_format } => print_version(output_format)?,
        CliAction::ResumeSession {
            session_path,
            commands,
            output_format,
        } => resume_session(&session_path, &commands, output_format),
        CliAction::Status {
            model,
            model_flag_raw,
            permission_mode,
            output_format,
        } => print_status_snapshot(
            &model,
            model_flag_raw.as_deref(),
            permission_mode,
            output_format,
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
            let mut cli = LiveCli::new(model, true, allowed_tools, permission_mode, None)?;
            cli.set_reasoning_effort(reasoning_effort);
            cli.run_turn_with_output(&effective_prompt, output_format, compact)?;
        }
        CliAction::Doctor { output_format } => run_doctor(output_format)?,
        CliAction::Acp { output_format } => print_acp_status(output_format)?,
        CliAction::State { output_format } => run_worker_state(output_format)?,
        CliAction::Init { output_format } => run_init(output_format)?,
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
        CliAction::Diff { output_format } => match output_format {
            CliOutputFormat::Text => {
                println!("{}", render_diff_report()?);
            }
            CliOutputFormat::Json => {
                let cwd = env::current_dir()?;
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
            None,
        )?,
        CliAction::HelpTopic(topic) => print_help_topic(topic),
        CliAction::Help { output_format } => print_help(output_format)?,
        CliAction::Rpc => {
            sdk::run_rpc_server()?;
        }
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<CliAction, String> {
    let mut model = DEFAULT_MODEL.to_string();
    // #148: when user passes --model/--model=, capture the raw input so we
    // can attribute source: "flag" later. None means no flag was supplied.
    let mut model_flag_raw: Option<String> = None;
    let mut output_format = CliOutputFormat::Text;
    let mut permission_mode_override = None;
    let mut wants_help = false;
    let mut wants_version = false;
    let mut allowed_tool_values = Vec::new();
    let mut compact = false;
    let mut base_commit: Option<String> = None;
    let mut reasoning_effort: Option<String> = None;
    let mut allow_broad_cwd = false;
    let mut rest: Vec<String> = Vec::new();
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
                    .ok_or_else(|| "missing value for --model".to_string())?;
                validate_model_syntax(value)?;
                model = resolve_model_alias_with_config(value);
                model_flag_raw = Some(value.clone()); // #148
                index += 2;
            }
            flag if flag.starts_with("--model=") => {
                let value = &flag[8..];
                validate_model_syntax(value)?;
                model = resolve_model_alias_with_config(value);
                model_flag_raw = Some(value.to_string()); // #148
                index += 1;
            }
            "--output-format" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --output-format".to_string())?;
                output_format = CliOutputFormat::parse(value)?;
                index += 2;
            }
            "--mode" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --mode".to_string())?;
                if value == "rpc" {
                    return Ok(CliAction::Rpc);
                } else {
                    return Err(format!("unknown mode: {value} (supported: rpc)"));
                }
            }
            "--mode=rpc" => {
                return Ok(CliAction::Rpc);
            }
            "--permission-mode" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --permission-mode".to_string())?;
                permission_mode_override = Some(parse_permission_mode_arg(value)?);
                index += 2;
            }
            flag if flag.starts_with("--output-format=") => {
                output_format = CliOutputFormat::parse(&flag[16..])?;
                index += 1;
            }
            flag if flag.starts_with("--permission-mode=") => {
                permission_mode_override = Some(parse_permission_mode_arg(&flag[18..])?);
                index += 1;
            }
            "--dangerously-skip-permissions" => {
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
                    .ok_or_else(|| "missing value for --base-commit".to_string())?;
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
                    .ok_or_else(|| "missing value for --reasoning-effort".to_string())?;
                if !matches!(value.as_str(), "low" | "medium" | "high") {
                    return Err(format!(
                        "invalid value for --reasoning-effort: '{value}'; must be low, medium, or high"
                    ));
                }
                reasoning_effort = Some(value.clone());
                index += 2;
            }
            flag if flag.starts_with("--reasoning-effort=") => {
                let value = &flag[19..];
                if !matches!(value, "low" | "medium" | "high") {
                    return Err(format!(
                        "invalid value for --reasoning-effort: '{value}'; must be low, medium, or high"
                    ));
                }
                reasoning_effort = Some(value.to_string());
                index += 1;
            }
            "--allow-broad-cwd" => {
                allow_broad_cwd = true;
                index += 1;
            }
            "-p" => {
                // Claw Code compat: -p "prompt" = one-shot prompt
                let prompt = args[index + 1..].join(" ");
                if prompt.trim().is_empty() {
                    return Err("-p requires a prompt string".to_string());
                }
                return Ok(CliAction::Prompt {
                    prompt,
                    model: resolve_model_alias_with_config(&model),
                    output_format,
                    allowed_tools: normalize_allowed_tools(&allowed_tool_values)?,
                    permission_mode: permission_mode_override
                        .unwrap_or_else(default_permission_mode),
                    compact,
                    base_commit: base_commit.clone(),
                    reasoning_effort: reasoning_effort.clone(),
                    allow_broad_cwd,
                });
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
                    .ok_or_else(|| "missing value for --allowedTools".to_string())?;
                allowed_tool_values.push(value.clone());
                index += 2;
            }
            flag if flag.starts_with("--allowedTools=") => {
                allowed_tool_values.push(flag[15..].to_string());
                index += 1;
            }
            flag if flag.starts_with("--allowed-tools=") => {
                allowed_tool_values.push(flag[16..].to_string());
                index += 1;
            }
            other if rest.is_empty() && other.starts_with('-') => {
                return Err(format_unknown_option(other))
            }
            other => {
                rest.push(other.to_string());
                index += 1;
            }
        }
    }

    if wants_help {
        return Ok(CliAction::Help { output_format });
    }

    if wants_version {
        return Ok(CliAction::Version { output_format });
    }

    let allowed_tools = normalize_allowed_tools(&allowed_tool_values)?;

    if rest.is_empty() {
        let permission_mode = permission_mode_override.unwrap_or_else(default_permission_mode);
        // When stdin is not a terminal (pipe/redirect) and no prompt is given on the
        // command line, read stdin as the prompt and dispatch as a one-shot Prompt
        // rather than starting the interactive REPL (which would consume the pipe and
        // print the startup banner, then exit without sending anything to the API).
        if !std::io::stdin().is_terminal() {
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
                    compact: false,
                    base_commit,
                    reasoning_effort,
                    allow_broad_cwd,
                });
            }
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
    if rest.first().map(String::as_str) == Some("--resume") {
        return parse_resume_args(&rest[1..], output_format);
    }
    if let Some(action) = parse_local_help_action(&rest) {
        return action;
    }
    if let Some(action) = parse_single_word_command_alias(
        &rest,
        &model,
        model_flag_raw.as_deref(),
        permission_mode_override,
        output_format,
    ) {
        return action;
    }

    let permission_mode = permission_mode_override.unwrap_or_else(default_permission_mode);

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
        "plugins" => {
            let tail = &rest[1..];
            let action = tail.first().cloned();
            let target = tail.get(1).cloned();
            if tail.len() > 2 {
                return Err(format!(
                    "unexpected extra arguments after `claw plugins {}`: {}",
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
                return Err(format!(
                    "unexpected extra arguments after `claw config {}`: {}",
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
                return Err(format!(
                    "unexpected extra arguments after `claw diff`: {}",
                    rest[1..].join(" ")
                ));
            }
            Ok(CliAction::Diff { output_format })
        }
        "skills" => {
            let args = join_optional_args(&rest[1..]);
            match classify_skills_slash_command(args.as_deref()) {
                SkillSlashDispatch::Invoke(prompt) => Ok(CliAction::Prompt {
                    prompt,
                    model,
                    output_format,
                    allowed_tools,
                    permission_mode,
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
        "system-prompt" => parse_system_prompt_args(&rest[1..], output_format),
        "acp" => parse_acp_args(&rest[1..], output_format),
        "login" | "logout" => Err(removed_auth_surface_error(rest[0].as_str())),
        "init" => Ok(CliAction::Init { output_format }),
        "export" => parse_export_args(&rest[1..], output_format),
        "prompt" => {
            let prompt = rest[1..].join(" ");
            if prompt.trim().is_empty() {
                return Err("prompt subcommand requires a prompt string".to_string());
            }
            Ok(CliAction::Prompt {
                prompt,
                model,
                output_format,
                allowed_tools,
                permission_mode,
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
            permission_mode,
            compact,
            base_commit,
            reasoning_effort,
            allow_broad_cwd,
        ),
        other => {
            if rest.len() == 1 && looks_like_subcommand_typo(other) {
                if let Some(suggestions) = suggest_similar_subcommand(other) {
                    let mut message = format!("unknown subcommand: {other}.");
                    if let Some(line) = render_suggestion_line("Did you mean", &suggestions) {
                        message.push('\n');
                        message.push_str(&line);
                    }
                    message.push_str(
                        "\nRun `claw --help` for the full list. If you meant to send a prompt literally, use `claw prompt <text>`.",
                    );
                    return Err(message);
                }
            }
            // #147: guard empty/whitespace-only prompts at the fallthrough
            // path the same way `"prompt"` arm above does. Without this,
            // `claw ""`, `claw "   "`, and `claw "" ""` silently route to
            // the Anthropic call and surface a misleading
            // `missing Anthropic credentials` error (or burn API tokens on
            // an empty prompt when credentials are present).
            let joined = rest.join(" ");
            if joined.trim().is_empty() {
                return Err(
                    "empty prompt: provide a subcommand (run `claw --help`) or a non-empty prompt string"
                        .to_string(),
                );
            }
            Ok(CliAction::Prompt {
                prompt: joined,
                model,
                output_format,
                allowed_tools,
                permission_mode,
                compact,
                base_commit,
                reasoning_effort: reasoning_effort.clone(),
                allow_broad_cwd,
            })
        }
    }
}

fn parse_local_help_action(rest: &[String]) -> Option<Result<CliAction, String>> {
    if rest.len() != 2 || !is_help_flag(&rest[1]) {
        return None;
    }

    let topic = match rest[0].as_str() {
        "status" => LocalHelpTopic::Status,
        "sandbox" => LocalHelpTopic::Sandbox,
        "doctor" => LocalHelpTopic::Doctor,
        "acp" => LocalHelpTopic::Acp,
        // #141: add the subcommands that were previously falling back
        // to global help (init/state/export/version) or erroring out
        // (system-prompt/dump-manifests) or printing their primary
        // output instead of help text (bootstrap-plan).
        "init" => LocalHelpTopic::Init,
        "state" => LocalHelpTopic::State,
        "export" => LocalHelpTopic::Export,
        "version" => LocalHelpTopic::Version,
        "system-prompt" => LocalHelpTopic::SystemPrompt,
        "dump-manifests" => LocalHelpTopic::DumpManifests,
        "bootstrap-plan" => LocalHelpTopic::BootstrapPlan,
        _ => return None,
    };
    Some(Ok(CliAction::HelpTopic(topic)))
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
) -> Option<Result<CliAction, String>> {
    if rest.is_empty() {
        return None;
    }

    // Diagnostic verbs (help, version, status, sandbox, doctor, state) accept only the verb itself
    // or --help / -h as a suffix. Any other suffix args are unrecognized.
    let verb = &rest[0];
    let is_diagnostic = matches!(
        verb.as_str(),
        "help" | "version" | "status" | "sandbox" | "doctor" | "state"
    );

    if is_diagnostic && rest.len() > 1 {
        // Diagnostic verb with trailing args: reject unrecognized suffix
        if is_help_flag(&rest[1]) && rest.len() == 2 {
            // "doctor --help" is valid, routed to parse_local_help_action() instead
            return None;
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
        }
        return Some(Err(msg));
    }

    if rest.len() != 1 {
        return None;
    }

    match rest[0].as_str() {
        "help" => Some(Ok(CliAction::Help { output_format })),
        "version" => Some(Ok(CliAction::Version { output_format })),
        "status" => Some(Ok(CliAction::Status {
            model: model.to_string(),
            model_flag_raw: model_flag_raw.map(str::to_string), // #148
            permission_mode: permission_mode_override.unwrap_or_else(default_permission_mode),
            output_format,
        })),
        "sandbox" => Some(Ok(CliAction::Sandbox { output_format })),
        "doctor" => Some(Ok(CliAction::Doctor { output_format })),
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
        .find(|spec| spec.name == command_name)?;
    let guidance = if slash_command.resume_supported {
        format!(
            "`claw {command_name}` is a slash command. Use `claw --resume SESSION.jsonl /{command_name}` or start `claw` and run `/{command_name}`."
        )
    } else {
        format!(
            "`claw {command_name}` is a slash command. Start `claw` and run `/{command_name}` inside the REPL."
        )
    };
    Some(guidance)
}

fn removed_auth_surface_error(command_name: &str) -> String {
    format!(
        "`claw {command_name}` has been removed. Set ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN instead."
    )
}

fn parse_acp_args(args: &[String], output_format: CliOutputFormat) -> Result<CliAction, String> {
    match args {
        [] => Ok(CliAction::Acp { output_format }),
        [subcommand] if subcommand == "serve" => Ok(CliAction::Acp { output_format }),
        _ => Err(String::from(
            "unsupported ACP invocation. Use `claw acp`, `claw acp serve`, `claw --acp`, or `claw -acp`.",
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
    permission_mode: PermissionMode,
    compact: bool,
    base_commit: Option<String>,
    reasoning_effort: Option<String>,
    allow_broad_cwd: bool,
) -> Result<CliAction, String> {
    let raw = rest.join(" ");
    match SlashCommand::parse(&raw) {
        Ok(Some(SlashCommand::Help)) => Ok(CliAction::Help { output_format }),
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
                    permission_mode,
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
        Ok(Some(SlashCommand::Unknown(name))) => Err(format_unknown_direct_slash_command(&name)),
        Ok(Some(command)) => Err({
            let _ = command;
            format!(
                "slash command {command_name} is interactive-only. Start `claw` and run it there, or use `claw --resume SESSION.jsonl {command_name}` / `claw --resume {latest} {command_name}` when the command is marked [resume] in /help.",
                command_name = rest[0],
                latest = LATEST_SESSION_REFERENCE,
            )
        }),
        Ok(None) => Err(format!("unknown subcommand: {}", rest[0])),
        Err(error) => Err(error.to_string()),
    }
}

fn format_unknown_option(option: &str) -> String {
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
    let mut message = format!("unknown slash command outside the REPL: /{name}");
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
    let mut message = format!("Unknown slash command: /{name}");
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

const DUMP_MANIFESTS_OVERRIDE_HINT: &str =
    "Hint: set CLAUDE_CODE_UPSTREAM=/path/to/upstream or pass `claw dump-manifests --manifests-dir /path/to/upstream`.";

fn version_json_value() -> serde_json::Value {
    json!({
        "kind": "version",
        "message": render_version_report(),
        "version": VERSION,
        "git_sha": GIT_SHA,
        "target": BUILD_TARGET,
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
                let (short_reason, hint) = split_error_hint(&full_message);
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "type": "error",
                        "error": short_reason,
                        "kind": kind,
                        "hint": hint,
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
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "type": "error",
                            "error": format!("/{cmd_root} is not yet implemented in this build"),
                            "kind": "unsupported_command",
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
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "type": "error",
                            "error": format!("unsupported resumed command: {raw_command}"),
                            "kind": "unsupported_resumed_command",
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
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "type": "error",
                            "error": error.to_string(),
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
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "type": "error",
                            "error": error.to_string(),
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

#[allow(clippy::too_many_lines)]
fn run_resume_command(
    session_path: &Path,
    session: &Session,
    command: &SlashCommand,
) -> Result<ResumeCommandOutcome, Box<dyn std::error::Error>> {
    match command {
        SlashCommand::Help => Ok(ResumeCommandOutcome {
            session: session.clone(),
            message: Some(render_repl_help()),
            json: Some(serde_json::json!({ "kind": "help", "text": render_repl_help() })),
        }),
        SlashCommand::Compact => {
            let result = runtime::compact_session(
                session,
                CompactionConfig {
                    max_estimated_tokens: 0,
                    ..CompactionConfig::default()
                },
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
            let previous_session_id = session.session_id.clone();
            let cleared = new_cli_session()?;
            let new_session_id = cleared.session_id.clone();
            cleared.save_to_path(session_path)?;
            Ok(ResumeCommandOutcome {
                session: cleared,
                message: Some(format!(
                    "Session cleared\n  Mode             resumed session reset\n  Previous session {previous_session_id}\n  Backup           {}\n  Resume previous  claw --resume {}\n  New session      {new_session_id}\n  Session file     {}",
                    backup_path.display(),
                    backup_path.display(),
                    session_path.display()
                )),
                json: Some(serde_json::json!({
                    "kind": "clear",
                    "previous_session_id": previous_session_id,
                    "new_session_id": new_session_id,
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
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                    "cache_read_input_tokens": usage.cache_read_input_tokens,
                    "total_tokens": usage.total_tokens(),
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
                json: Some(serde_json::json!({
                    "kind": "agents",
                    "text": handle_agents_slash_command(args.as_deref(), &cwd)?,
                })),
            })
        }
        SlashCommand::Skills { args } => {
            if let SkillSlashDispatch::Invoke(_) = classify_skills_slash_command(args.as_deref()) {
                return Err(
                    "resumed /skills invocations are interactive-only; start `claw` and run `/skills <skill>` in the REPL".into(),
                );
            }
            let cwd = env::current_dir()?;
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(handle_skills_slash_command(args.as_deref(), &cwd)?),
                json: Some(handle_skills_slash_command_json(args.as_deref(), &cwd)?),
            })
        }
        SlashCommand::Doctor => {
            let report = render_doctor_report()?;
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
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                    "cache_read_input_tokens": usage.cache_read_input_tokens,
                    "total_tokens": usage.total_tokens(),
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
        // /session list can be served from the sessions directory without a live session.
        SlashCommand::Session {
            action: Some(ref act),
            ..
        } if act == "list" => {
            let sessions = list_managed_sessions().unwrap_or_default();
            let session_ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
            let active_id = session.session_id.clone();
            let text = render_session_list(&active_id).unwrap_or_else(|e| format!("error: {e}"));
            Ok(ResumeCommandOutcome {
                session: session.clone(),
                message: Some(text),
                json: Some(serde_json::json!({
                    "kind": "session_list",
                    "sessions": session_ids,
                    "active": active_id,
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
        | SlashCommand::Model { .. }
        | SlashCommand::Permissions { .. }
        | SlashCommand::Session { .. }
        | SlashCommand::Plugins { .. }
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
        | SlashCommand::AddDir { .. } => Err("unsupported resumed slash command".into()),
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
            "  {id:<20} {marker:<10} msgs={msgs:<4} modified={modified}{lineage} path={path}",
            id = session.id,
            msgs = session.message_count,
            modified = format_session_modified_age(session.modified_epoch_millis),
            lineage = lineage,
            path = session.path.display(),
        ));
    }
    Ok(lines.join("\n"))
}

fn print_status_snapshot(
    model: &str,
    model_flag_raw: Option<&str>,
    permission_mode: PermissionMode,
    output_format: CliOutputFormat,
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
    let provenance = match model_flag_raw {
        Some(raw) => ModelProvenance {
            resolved: model.to_string(),
            raw: Some(raw.to_string()),
            source: ModelSource::Flag,
        },
        None => ModelProvenance::from_env_or_config_or_default(model),
    };
    match output_format {
        CliOutputFormat::Text => println!(
            "{}",
            format_status_report(
                &provenance.resolved,
                usage,
                permission_mode.as_str(),
                &context,
                Some(&provenance)
            )
        ),
        CliOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&status_json_value(
                Some(&provenance.resolved),
                usage,
                permission_mode.as_str(),
                &context,
                Some(&provenance),
            ))?
        ),
    }
    Ok(())
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
  Purpose          create .claw/, .claw.json, .gitignore, and CLAUDE.md in the current project
  Output           list of created vs. skipped files (idempotent: safe to re-run)
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
    }
}

fn print_acp_status(output_format: CliOutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    let message = "ACP/Zed editor integration is not implemented in claw-code yet. `claw acp serve` is only a discoverability alias today; it does not launch a daemon or Zed-specific protocol endpoint. Use the normal terminal surfaces for now and track ROADMAP #76 for real ACP support.";
    match output_format {
        CliOutputFormat::Text => {
            println!(
                "ACP / Zed\n  Status           discoverability only\n  Launch           `claw acp serve` / `claw --acp` / `claw -acp` report status only; no editor daemon is available yet\n  Today            use `claw prompt`, the REPL, or `claw doctor` for local verification\n  Tracking         ROADMAP #76\n  Message          {message}"
            );
        }
        CliOutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "kind": "acp",
                    "status": "discoverability_only",
                    "supported": false,
                    "serve_alias_only": true,
                    "message": message,
                    "launch_command": serde_json::Value::Null,
                    "aliases": ["acp", "--acp", "-acp"],
                    "discoverability_tracking": "ROADMAP #64a",
                    "tracking": "ROADMAP #76",
                    "recommended_workflows": [
                        "claw prompt TEXT",
                        "claw",
                        "claw doctor"
                    ],
                }))?
            );
        }
    }
    Ok(())
}

fn init_claude_md() -> Result<String, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    Ok(initialize_repo(&cwd)?.render())
}

/// #142: emit first-class structured fields alongside the legacy `message`
/// string so claws can detect per-artifact state without substring matching.
fn init_json_value(report: &crate::init::InitReport, message: &str) -> serde_json::Value {
    use crate::init::InitStatus;
    json!({
        "kind": "init",
        "project_path": report.project_root.display().to_string(),
        "created": report.artifacts_with_status(InitStatus::Created),
        "updated": report.artifacts_with_status(InitStatus::Updated),
        "skipped": report.artifacts_with_status(InitStatus::Skipped),
        "artifacts": report.artifact_json_entries(),
        "next_step": crate::init::InitReport::NEXT_STEP,
        "message": message,
    })
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

fn indent_block(value: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    value
        .lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
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
        .map(|output| output.status.success())
        .unwrap_or(false)
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

// Computes civil (Gregorian) year/month/day from days since the Unix epoch
// (1970-01-01) using Howard Hinnant's `civil_from_days` algorithm.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation
)]
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
    let git_sha = GIT_SHA.unwrap_or("unknown");
    let target = BUILD_TARGET.unwrap_or("unknown");
    format!(
        "Claw Code\n  Version          {VERSION}\n  Git SHA          {git_sha}\n  Target           {target}\n  Build date       {DEFAULT_DATE}"
    )
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

const SESSION_MARKDOWN_TOOL_SUMMARY_LIMIT: usize = 280;

fn run_export(
    session_reference: &str,
    output_path: Option<&Path>,
    output_format: CliOutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
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

const DISPLAY_TRUNCATION_NOTICE: &str =
    "\x1b[2m… output truncated for display; full result preserved in session.\x1b[0m";
const READ_DISPLAY_MAX_LINES: usize = 80;
const READ_DISPLAY_MAX_CHARS: usize = 6_000;
const TOOL_OUTPUT_DISPLAY_MAX_LINES: usize = 60;
const TOOL_OUTPUT_DISPLAY_MAX_CHARS: usize = 4_000;

fn print_help(output_format: CliOutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    let mut buffer = Vec::new();
    print_help_to(&mut buffer)?;
    let message = String::from_utf8(buffer)?;
    match output_format {
        CliOutputFormat::Text => print!("{message}"),
        CliOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "kind": "help",
                "message": message,
            }))?
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_runtime_plugin_state_with_loader, build_runtime_with_plugin_state,
        classify_error_kind, collect_session_prompt_history, create_managed_session_handle,
        describe_tool_progress, filter_tool_specs, format_bughunter_report,
        format_commit_preflight_report, format_commit_skipped_report, format_compact_report,
        format_connected_line, format_cost_report, format_history_timestamp,
        format_internal_prompt_progress_line, format_issue_report, format_model_report,
        format_model_switch_report, format_permissions_report, format_permissions_switch_report,
        format_pr_report, format_resume_report, format_status_report, format_tool_call_start,
        format_tool_result, format_ultraplan_report, format_unknown_slash_command,
        format_unknown_slash_command_message, format_user_visible_api_error,
        merge_prompt_with_stdin, normalize_permission_mode, parse_args, parse_export_args,
        parse_git_status_branch, parse_git_status_metadata_for, parse_git_workspace_summary,
        parse_history_count, permission_policy, print_help_to, push_output_block,
        render_config_report, render_diff_report, render_diff_report_for, render_help_topic,
        render_memory_report, render_prompt_history_report, render_repl_help, render_resume_usage,
        render_session_markdown, resolve_model_alias, resolve_model_alias_with_config,
        resolve_repl_model, resolve_session_reference, response_to_events,
        resume_supported_slash_commands, run_resume_command, short_tool_id,
        slash_command_completion_candidates_with_sessions, split_error_hint, status_context,
        summarize_tool_payload_for_markdown, try_resolve_bare_skill_prompt, validate_no_args,
        write_mcp_server_fixture, CliAction, CliOutputFormat, CliToolExecutor, GitWorkspaceSummary,
        InternalPromptProgressEvent, InternalPromptProgressState, LiveCli, LocalHelpTopic,
        PromptHistoryEntry, SlashCommand, StatusUsage, DEFAULT_MODEL, LATEST_SESSION_REFERENCE,
        STUB_COMMANDS,
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
            model: "claude-sonnet-4-6".to_string(),
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
            rendered.contains("Model            claude-sonnet-4-6"),
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
                permission_mode: PermissionMode::DangerFullAccess,
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
                permission_mode: PermissionMode::DangerFullAccess,
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
                model: "claude-opus-4-6".to_string(),
                output_format: CliOutputFormat::Json,
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
                permission_mode: PermissionMode::DangerFullAccess,
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
                model: "claude-opus-4-6".to_string(),
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
    fn resolves_known_model_aliases() {
        assert_eq!(resolve_model_alias("opus"), "claude-opus-4-6");
        assert_eq!(resolve_model_alias("sonnet"), "claude-sonnet-4-6");
        assert_eq!(resolve_model_alias("haiku"), "claude-haiku-4-5-20251213");
        assert_eq!(resolve_model_alias("claude-opus"), "claude-opus");
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
            r#"{"aliases":{"fast":"claude-haiku-4-5-20251213","smart":"opus","cheap":"grok-3-mini"}}"#,
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
        assert_eq!(direct, "claude-haiku-4-5-20251213");
        assert_eq!(chained, "claude-opus-4-6");
        assert_eq!(cross_provider, "grok-3-mini");
        assert_eq!(unknown, "unknown-model");
        assert_eq!(builtin, "claude-haiku-4-5-20251213");
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
                permission_mode: PermissionMode::DangerFullAccess,
                base_commit: None,
                reasoning_effort: None,
                allow_broad_cwd: false,
            }
        );
    }

    #[test]
    fn rejects_unknown_allowed_tools() {
        let error = parse_args(&["--allowedTools".to_string(), "teleport".to_string()])
            .expect_err("tool should be rejected");
        assert!(error.contains("unsupported tool in --allowedTools: teleport"));
    }

    #[test]
    fn parses_system_prompt_options() {
        let args = vec![
            "system-prompt".to_string(),
            "--cwd".to_string(),
            "/tmp/project".to_string(),
            "--date".to_string(),
            "2026-04-01".to_string(),
        ];
        assert_eq!(
            parse_args(&args).expect("args should parse"),
            CliAction::PrintSystemPrompt {
                cwd: PathBuf::from("/tmp/project"),
                date: "2026-04-01".to_string(),
                output_format: CliOutputFormat::Text,
            }
        );
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
            typo_err.starts_with("unknown subcommand:"),
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
                assert_eq!(model, "claude-sonnet-4-6", "sonnet alias should resolve");
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
    }

    #[test]
    fn local_command_help_flags_stay_on_the_local_parser_path() {
        assert_eq!(
            parse_args(&["status".to_string(), "--help".to_string()])
                .expect("status help should parse"),
            CliAction::HelpTopic(LocalHelpTopic::Status)
        );
        assert_eq!(
            parse_args(&["sandbox".to_string(), "-h".to_string()])
                .expect("sandbox help should parse"),
            CliAction::HelpTopic(LocalHelpTopic::Sandbox)
        );
        assert_eq!(
            parse_args(&["doctor".to_string(), "--help".to_string()])
                .expect("doctor help should parse"),
            CliAction::HelpTopic(LocalHelpTopic::Doctor)
        );
        assert_eq!(
            parse_args(&["acp".to_string(), "--help".to_string()]).expect("acp help should parse"),
            CliAction::HelpTopic(LocalHelpTopic::Acp)
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
                    CliAction::HelpTopic(*expected_topic),
                    "`{subcommand} {flag}` should resolve to HelpTopic({expected_topic:?})"
                );
            }
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
    fn status_degrades_gracefully_on_malformed_mcp_config_143() {
        // #143: previously `claw status` hard-failed on any config parse error,
        // taking down the entire health surface for one malformed MCP entry.
        // `claw doctor` already degrades gracefully; this test locks `status`
        // to the same contract.
        let _guard = env_lock();
        let root = temp_dir();
        let cwd = root.join("project-with-malformed-mcp");
        std::fs::create_dir_all(&cwd).expect("project dir should exist");
        // One valid server + one malformed entry missing `command`.
        std::fs::write(
            cwd.join(".claw.json"),
            r#"{
  "mcpServers": {
    "everything": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-everything"]},
    "missing-command": {"args": ["arg-only-no-command"]}
  }
}
"#,
        )
        .expect("write malformed .claw.json");

        let context = with_current_dir(&cwd, || {
            super::status_context(None)
                .expect("status_context should not hard-fail on config parse errors (#143)")
        });

        // Phase 1 contract: config_load_error is populated with the parse error.
        let err = context
            .config_load_error
            .as_ref()
            .expect("config_load_error should be Some when config parse fails");
        assert!(
            err.contains("mcpServers.missing-command"),
            "config_load_error should name the malformed field path: {err}"
        );
        assert!(
            err.contains("missing string field command"),
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
        let json =
            super::status_json_value(Some("test-model"), usage, "workspace-write", &context, None);
        assert_eq!(
            json.get("status").and_then(|v| v.as_str()),
            Some("degraded"),
            "top-level status marker should be 'degraded' when config parse failed: {json}"
        );
        assert!(
            json.get("config_load_error")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("mcpServers.missing-command")),
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
        assert!(
            json.get("sandbox").is_some(),
            "sandbox field still reported"
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
                permission_mode: PermissionMode::DangerFullAccess,
                output_format: CliOutputFormat::Text,
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
        // #154: model syntax error should hint at provider prefix when applicable
        let err_gpt = parse_args(&[
            "prompt".to_string(),
            "test".to_string(),
            "--model".to_string(),
            "gpt-4".to_string(),
        ])
        .expect_err("`--model gpt-4` should fail with OpenAI hint");
        assert!(
            err_gpt.contains("Did you mean `openai/gpt-4`?"),
            "GPT model error should hint openai/ prefix: {err_gpt}"
        );
        assert!(
            err_gpt.contains("OPENAI_API_KEY"),
            "GPT model error should mention env var: {err_gpt}"
        );
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
        assert_eq!(
            classify_error_kind("failed to restore session: no managed sessions found"),
            "session_load_failed"
        );
        assert_eq!(
            classify_error_kind("unrecognized argument `--foo` for subcommand `doctor`"),
            "cli_parse"
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
            classify_error_kind("something completely unknown"),
            "unknown"
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
                model: "claude-opus-4-6".to_string(),
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
        let error = parse_args(&["/status".to_string()])
            .expect_err("/status should remain REPL-only when invoked directly");
        assert!(error.contains("interactive-only"));
        assert!(error.contains("claw --resume SESSION.jsonl /status"));
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
                permission_mode: PermissionMode::DangerFullAccess,
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
                permission_mode: PermissionMode::DangerFullAccess,
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
            }
        );
        assert_eq!(
            parse_args(&["--resume".to_string(), "/status".to_string()])
                .expect("resume shortcut should parse"),
            CliAction::ResumeSession {
                session_path: PathBuf::from("latest"),
                commands: vec!["/status".to_string()],
                output_format: CliOutputFormat::Text,
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
        assert!(help.contains("/session [list|switch <session-id>|fork [branch-name]"));
        assert!(help.contains(
            "/plugin [list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]"
        ));
        assert!(help.contains("aliases: /plugins, /marketplace"));
        assert!(help.contains("/agents"));
        assert!(help.contains("/skills"));
        assert!(help.contains("/exit"));
        assert!(help.contains("Auto-save            .claw/sessions/<session-id>.jsonl"));
        assert!(help.contains("Resume latest        /resume latest"));
    }

    #[test]
    fn completion_candidates_include_workflow_shortcuts_and_dynamic_sessions() {
        let completions = slash_command_completion_candidates_with_sessions(
            "sonnet",
            Some("session-current"),
            vec!["session-old".to_string()],
        );

        assert!(completions.contains(&"/model claude-sonnet-4-6".to_string()));
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
                "claude-sonnet-4-6".to_string(),
                true,
                None,
                PermissionMode::DangerFullAccess,
                None,
            )
            .expect("cli should initialize")
            .startup_banner()
        });

        assert!(banner.contains("Tab"));
        assert!(banner.contains("completions"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn format_connected_line_renders_anthropic_provider_for_claude_model() {
        let model = "claude-sonnet-4-6";

        let line = format_connected_line(model);

        assert_eq!(line, "Connected: claude-sonnet-4-6 via anthropic");
    }

    #[test]
    fn format_connected_line_renders_xai_provider_for_grok_model() {
        let model = "grok-3";

        let line = format_connected_line(model);

        assert_eq!(line, "Connected: grok-3 via xai");
    }

    #[test]
    fn resolve_repl_model_returns_user_supplied_model_unchanged_when_explicit() {
        let user_model = "claude-sonnet-4-6".to_string();

        let resolved = resolve_repl_model(user_model);

        assert_eq!(resolved, "claude-sonnet-4-6");
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

        let resolved = with_current_dir(&root, || resolve_repl_model(DEFAULT_MODEL.to_string()));

        assert_eq!(resolved, "claude-sonnet-4-6");

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

        let resolved = with_current_dir(&root, || resolve_repl_model(DEFAULT_MODEL.to_string()));

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
                project_root: Some(PathBuf::from("/tmp")),
                git_branch: Some("main".to_string()),
                git_summary: GitWorkspaceSummary {
                    changed_files: 3,
                    staged_files: 1,
                    unstaged_files: 1,
                    untracked_files: 1,
                    conflicted_files: 0,
                },
                sandbox_status: runtime::SandboxStatus::default(),
                config_load_error: None,
            },
            None, // #148
        );
        assert!(status.contains("Status"));
        assert!(status.contains("Model            claude-sonnet"));
        assert!(status.contains("Permission mode  workspace-write"));
        assert!(status.contains("Messages         7"));
        assert!(status.contains("Latest total     10"));
        assert!(status.contains("Cumulative total 31"));
        assert!(status.contains("Cwd              /tmp/project"));
        assert!(status.contains("Project root     /tmp"));
        assert!(status.contains("Git branch       main"));
        assert!(
            status.contains("Git state        dirty · 3 files · 1 staged, 1 unstaged, 1 untracked")
        );
        assert!(status.contains("Changed files    3"));
        assert!(status.contains("Staged           1"));
        assert!(status.contains("Unstaged         1"));
        assert!(status.contains("Untracked        1"));
        assert!(status.contains("Session          session.jsonl"));
        assert!(status.contains("Config files     loaded 2/3"));
        assert!(status.contains("Memory files     4"));
        assert!(status.contains("Suggested flow   /status → /diff → /commit"));
    }

    #[test]
    fn commit_reports_surface_workspace_context() {
        let summary = GitWorkspaceSummary {
            changed_files: 2,
            staged_files: 1,
            unstaged_files: 1,
            untracked_files: 0,
            conflicted_files: 0,
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
    fn latest_session_alias_resolves_most_recent_managed_session() {
        let _guard = cwd_guard();
        let workspace = temp_workspace("latest-session-alias");
        std::fs::create_dir_all(&workspace).expect("workspace should create");
        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&workspace).expect("switch cwd");

        let older = create_managed_session_handle("session-older").expect("older handle");
        Session::new()
            .with_persistence_path(older.path.clone())
            .save_to_path(&older.path)
            .expect("older session should save");
        std::thread::sleep(Duration::from_millis(20));
        let newer = create_managed_session_handle("session-newer").expect("newer handle");
        Session::new()
            .with_persistence_path(newer.path.clone())
            .save_to_path(&newer.path)
            .expect("newer session should save");

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
        assert!(usage.contains(".claw/sessions/<session-id>.jsonl"));
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
        assert!(parsed.is_err());
        assert!(parsed.unwrap_err().contains("invalid count 'abc'"));
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
        assert!(rendered.contains("stdout 009"));
        assert!(!rendered.contains("stdout 010"));
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
        assert!(rendered.contains("payload 007"));
        assert!(!rendered.contains("payload 008"));
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
        assert!(rendered.contains("raw 009"));
        assert!(!rendered.contains("raw 010"));
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
                model: "claude-opus-4-6".to_string(),
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
                model: "claude-opus-4-6".to_string(),
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
                model: "claude-opus-4-6".to_string(),
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
            AssistantEvent::TextDelta(text) if text == "Final answer"
        ));
        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("Reasoning (6 chars)"));
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
        assert!(allowed.contains("MCPTool"));

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
    use super::{dump_manifests_at_path, CliOutputFormat};
    use std::fs;

    #[test]
    fn dump_manifests_shows_helpful_error_when_manifests_missing() {
        let root = std::env::temp_dir().join(format!(
            "claw_test_missing_manifests_{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("failed to create temp workspace");

        let result = dump_manifests_at_path(&workspace, None, CliOutputFormat::Text);
        assert!(
            result.is_err(),
            "expected an error when manifests are missing"
        );

        let error_msg = result.unwrap_err().to_string();

        assert!(
            error_msg.contains("Manifest source files are missing"),
            "error message should mention missing manifest sources: {error_msg}"
        );
        assert!(
            error_msg.contains(&root.display().to_string()),
            "error message should contain the resolved repo root path: {error_msg}"
        );
        assert!(
            error_msg.contains("src/commands.ts"),
            "error message should mention missing commands.ts: {error_msg}"
        );
        assert!(
            error_msg.contains("CLAUDE_CODE_UPSTREAM"),
            "error message should explain how to supply the upstream path: {error_msg}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dump_manifests_uses_explicit_manifest_dir() {
        let root = std::env::temp_dir().join(format!(
            "claw_test_explicit_manifest_dir_{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let upstream = root.join("upstream");
        fs::create_dir_all(workspace.join("nested")).expect("workspace should exist");
        fs::create_dir_all(upstream.join("src/entrypoints"))
            .expect("upstream fixture should exist");
        fs::write(
            upstream.join("src/commands.ts"),
            "import FooCommand from './commands/foo'\n",
        )
        .expect("commands fixture should write");
        fs::write(
            upstream.join("src/tools.ts"),
            "import ReadTool from './tools/read'\n",
        )
        .expect("tools fixture should write");
        fs::write(
            upstream.join("src/entrypoints/cli.tsx"),
            "startupProfiler()\n",
        )
        .expect("cli fixture should write");

        let result = dump_manifests_at_path(&workspace, Some(&upstream), CliOutputFormat::Text);
        assert!(
            result.is_ok(),
            "explicit manifest dir should succeed: {result:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
