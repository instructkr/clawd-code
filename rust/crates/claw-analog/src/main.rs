//! Binary wrapper for `claw_analog::run` — see `how_to_run.md` in repo root.

mod agents;
mod config_cmd;
mod doctor;

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use claw_analog::{
    load_analog_toml, load_profile_hint, permission_mode_from_toml_str, print_tools_dry_run,
    resolve_analog_profile_path, resolve_rag_base_url, AnalogConfig, AnalogFileConfig,
    AnalogLanguage, OutputFormat, PermissionMode, Preset, ANALOG_DEFAULT_MODEL,
};

#[derive(Copy, Clone, Debug, ValueEnum)]
enum PermissionArg {
    ReadOnly,
    WorkspaceWrite,
    Prompt,
    #[value(name = "danger-full-access")]
    DangerFullAccess,
    /// Same unrestricted posture as danger-full-access for this narrow tool set.
    Allow,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OutputFormatArg {
    Rich,
    Json,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum LangArg {
    En,
    Ru,
}

impl From<LangArg> for AnalogLanguage {
    fn from(a: LangArg) -> Self {
        match a {
            LangArg::En => AnalogLanguage::En,
            LangArg::Ru => AnalogLanguage::Ru,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum PresetCli {
    None,
    /// Automatically infer a preset from the initial prompt.
    Auto,
    Audit,
    Explain,
    Implement,
}

impl From<PresetCli> for Preset {
    fn from(p: PresetCli) -> Self {
        match p {
            PresetCli::None => Preset::None,
            PresetCli::Auto => Preset::None,
            PresetCli::Audit => Preset::Audit,
            PresetCli::Explain => Preset::Explain,
            PresetCli::Implement => Preset::Implement,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "claw-analog",
    version,
    about = "Lean tool-agent loop (read/list/grep/write) on claw-code `api` providers"
)]
#[command(args_conflicts_with_subcommands = true)]
struct RootCli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[command(flatten)]
    run: RunCli,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Verify credentials, `cargo check -p claw-analog` (or `--release-build`), config merge preview, optional `--tcp-ping`.
    Doctor(doctor::DoctorCli),
    Config {
        #[command(subcommand)]
        command: ConfigSub,
    },
    /// Print shell completion script for this binary (redirect to a file or `source` it).
    Complete(CompleteCli),
    /// Run multiple specialized sub-agents sequentially (shared base session).
    Agents(agents::AgentsCli),
}

#[derive(Subcommand, Debug)]
enum ConfigSub {
    /// Parse `.claw-analog.toml` and profile; print a merge preview (no API calls).
    Validate(config_cmd::ValidateCli),
}

#[derive(Parser, Debug)]
struct CompleteCli {
    #[arg(value_enum)]
    shell: ShellKind,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ShellKind {
    Bash,
    Zsh,
    Fish,
    #[value(name = "powershell", alias = "pwsh")]
    Powershell,
}

#[derive(Parser, Debug)]
struct RunCli {
    /// Config file (default: `<workspace>/.claw-analog.toml` if that path exists).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    #[arg(short, long)]
    model: Option<String>,
    #[arg(short = 'w', long, default_value = ".")]
    workspace: PathBuf,
    #[arg(long, value_enum)]
    permission: Option<PermissionArg>,
    #[arg(long, value_enum)]
    preset: Option<PresetCli>,
    /// Reply language hint for the assistant (`en` or `ru` in system prompt; not the API model id).
    #[arg(long, value_enum)]
    lang: Option<LangArg>,
    /// Print effective tools for merged `permission` / enforcer, then exit (no prompt, no API).
    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    print_tools: bool,
    /// Persist message history for resume (JSON). See `how_to_run.md` for risks.
    #[arg(long, value_name = "PATH")]
    session: Option<PathBuf>,
    /// Write session JSON to this path on each snapshot (export without `--session`, or an extra copy).
    #[arg(long, value_name = "PATH")]
    save_session: Option<PathBuf>,
    /// Profile snippet TOML (`line = "..."`). Default: `~/.claw-analog/profile.toml` if it exists.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
    /// Stream assistant text to stdout as tokens arrive (uses `stream_message`).
    #[arg(long, default_value_t = false, conflicts_with = "no_stream")]
    stream: bool,
    /// Turn streaming off (overrides `stream` in config).
    #[arg(long, default_value_t = false, conflicts_with = "stream")]
    no_stream: bool,
    /// Newline-delimited JSON events on stdout (for agents / CI). Diagnostics stay on stderr.
    #[arg(long, value_enum)]
    output_format: Option<OutputFormatArg>,
    /// Disable `runtime::PermissionEnforcer` (paths are still jailed; policy checks are weakened).
    #[arg(long = "no-runtime-enforcer", default_value_t = false, action = clap::ArgAction::SetTrue)]
    no_runtime_enforcer: bool,
    /// Allow `danger-full-access` / `allow` when stdin is not a TTY (CI/automation; use with care).
    #[arg(long = "accept-danger-non-interactive", default_value_t = false, action = clap::ArgAction::SetTrue)]
    accept_danger_non_interactive: bool,
    #[arg(long)]
    max_read_bytes: Option<u64>,
    #[arg(long)]
    max_turns: Option<u32>,
    #[arg(long)]
    max_list_entries: Option<usize>,
    #[arg(long)]
    grep_max_lines: Option<usize>,
    #[arg(long)]
    glob_max_paths: Option<usize>,
    #[arg(long)]
    glob_max_depth: Option<usize>,
    prompt: Option<String>,
}

const DEF_MAX_READ: u64 = 256 * 1024;
const DEF_MAX_TURNS: u32 = 24;
const DEF_MAX_LIST: usize = 500;
const DEF_GREP_MAX: usize = 200;
const DEF_GLOB_PATHS: usize = 2000;
const DEF_GLOB_DEPTH: usize = 32;
const DEF_RAG_TIMEOUT_SECS: u64 = 30;
const DEF_RAG_TOP_K_MAX: u32 = 32;
const RAG_TOP_K_ABS_CAP: u32 = 256;

fn config_file_path(cli: &RunCli) -> PathBuf {
    cli.config
        .clone()
        .unwrap_or_else(|| cli.workspace.join(".claw-analog.toml"))
}

fn load_file_config(path: &Path) -> AnalogFileConfig {
    if !path.is_file() {
        return AnalogFileConfig::default();
    }
    match load_analog_toml(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[claw-analog] warning: failed to read {}: {e}",
                path.display()
            );
            AnalogFileConfig::default()
        }
    }
}

fn output_format_from_toml(s: &str) -> Option<OutputFormat> {
    match s.to_ascii_lowercase().as_str() {
        "json" => Some(OutputFormat::Json),
        "rich" => Some(OutputFormat::Rich),
        _ => None,
    }
}

fn resolve_session_path(
    cli: Option<PathBuf>,
    file: Option<&str>,
    workspace: &Path,
) -> Option<PathBuf> {
    let p = cli.or_else(|| file.map(PathBuf::from))?;
    Some(if p.is_absolute() {
        p
    } else {
        workspace.join(p)
    })
}

fn merge_language(cli: Option<LangArg>, file: Option<&str>) -> AnalogLanguage {
    if let Some(l) = cli {
        return l.into();
    }
    file.and_then(AnalogLanguage::from_toml_str)
        .unwrap_or_default()
}

fn merge_preset(cli: Option<PresetCli>, file: Option<&str>, prompt: &str) -> Preset {
    if let Some(p) = cli {
        return match p {
            PresetCli::Auto => claw_analog::infer_preset_from_prompt(prompt),
            other => Preset::from(other),
        };
    }
    if file.is_some_and(|s| s.trim().eq_ignore_ascii_case("auto")) {
        return claw_analog::infer_preset_from_prompt(prompt);
    }
    if let Some(s) = file.and_then(Preset::from_toml_str) {
        return s;
    }
    claw_analog::infer_preset_from_prompt(prompt)
}

fn merge_permission(
    cli: Option<PermissionArg>,
    file_perm: Option<String>,
    preset: Preset,
) -> PermissionMode {
    if let Some(p) = cli {
        return match p {
            PermissionArg::ReadOnly => PermissionMode::ReadOnly,
            PermissionArg::WorkspaceWrite => PermissionMode::WorkspaceWrite,
            PermissionArg::Prompt => PermissionMode::Prompt,
            PermissionArg::DangerFullAccess => PermissionMode::DangerFullAccess,
            PermissionArg::Allow => PermissionMode::Allow,
        };
    }
    if let Some(s) = file_perm.as_deref().and_then(permission_mode_from_toml_str) {
        return s;
    }
    match preset {
        Preset::Implement => PermissionMode::WorkspaceWrite,
        _ => PermissionMode::ReadOnly,
    }
}

fn build_config(
    cli: &RunCli,
    file: &AnalogFileConfig,
    prompt: String,
    profile_hint: Option<String>,
    session_path: Option<PathBuf>,
    preset: Preset,
    permission_mode: PermissionMode,
) -> AnalogConfig {
    let model = cli
        .model
        .clone()
        .or_else(|| file.model.clone())
        .unwrap_or_else(|| ANALOG_DEFAULT_MODEL.into());

    let output_format = cli
        .output_format
        .map(|o| match o {
            OutputFormatArg::Rich => OutputFormat::Rich,
            OutputFormatArg::Json => OutputFormat::Json,
        })
        .or_else(|| {
            file.output_format
                .as_deref()
                .and_then(output_format_from_toml)
        })
        .unwrap_or(OutputFormat::Rich);

    let use_stream = if cli.no_stream {
        false
    } else if cli.stream {
        true
    } else {
        file.stream.unwrap_or(false)
    };

    let use_runtime_enforcer =
        !cli.no_runtime_enforcer && !file.no_runtime_enforcer.unwrap_or(false);

    let accept_danger_non_interactive =
        cli.accept_danger_non_interactive || file.accept_danger_non_interactive.unwrap_or(false);

    let max_read_bytes = cli
        .max_read_bytes
        .or(file.max_read_bytes)
        .unwrap_or(DEF_MAX_READ);
    let max_turns = cli.max_turns.or(file.max_turns).unwrap_or(DEF_MAX_TURNS);
    let max_list_entries = cli
        .max_list_entries
        .or(file.max_list_entries)
        .unwrap_or(DEF_MAX_LIST);
    let grep_max_lines = cli
        .grep_max_lines
        .or(file.grep_max_lines)
        .unwrap_or(DEF_GREP_MAX);
    let glob_max_paths = cli
        .glob_max_paths
        .or(file.glob_max_paths)
        .unwrap_or(DEF_GLOB_PATHS);
    let glob_max_depth = cli
        .glob_max_depth
        .or(file.glob_max_depth)
        .unwrap_or(DEF_GLOB_DEPTH);

    let rag_base_url = resolve_rag_base_url(file);
    let rag_http_timeout =
        Duration::from_secs(file.rag_timeout_secs.unwrap_or(DEF_RAG_TIMEOUT_SECS).max(1));
    let rag_top_k_max = file
        .rag_top_k_max
        .unwrap_or(DEF_RAG_TOP_K_MAX)
        .clamp(1, RAG_TOP_K_ABS_CAP);

    let session_save_path = cli.save_session.as_ref().map(|p| {
        if p.is_absolute() {
            p.clone()
        } else {
            cli.workspace.join(p)
        }
    });

    let language = merge_language(cli.lang, file.language.as_deref());

    AnalogConfig {
        model,
        workspace: cli.workspace.clone(),
        permission_mode,
        accept_danger_non_interactive,
        use_stream,
        output_format,
        use_runtime_enforcer,
        max_read_bytes,
        max_turns,
        max_list_entries,
        grep_max_lines,
        glob_max_paths,
        glob_max_depth,
        preset,
        language,
        session_path,
        session_save_path,
        profile_hint,
        prompt,
        rag_base_url,
        rag_http_timeout,
        rag_top_k_max,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let root = RootCli::parse();
    match root.command {
        Some(Commands::Doctor(d)) => {
            let code = doctor::run_doctor(d);
            std::process::exit(code);
        }
        Some(Commands::Agents(a)) => {
            let code = match agents::run_agents(a) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("agents: {e}");
                    1
                }
            };
            std::process::exit(code);
        }
        Some(Commands::Config { command }) => {
            let code = match command {
                ConfigSub::Validate(v) => config_cmd::run_validate(v),
            };
            std::process::exit(code);
        }
        Some(Commands::Complete(co)) => {
            let shell = match co.shell {
                ShellKind::Bash => Shell::Bash,
                ShellKind::Zsh => Shell::Zsh,
                ShellKind::Fish => Shell::Fish,
                ShellKind::Powershell => Shell::PowerShell,
            };
            let mut cmd = RootCli::command();
            generate(shell, &mut cmd, "claw-analog", &mut std::io::stdout());
            return Ok(());
        }
        None => {}
    }
    let cli = root.run;
    let cfg_path = config_file_path(&cli);
    let file_cfg = load_file_config(&cfg_path);

    if cli.print_tools {
        let preset = merge_preset(
            cli.preset,
            file_cfg.preset.as_deref(),
            &cli.prompt.clone().unwrap_or_default(),
        );
        let permission_mode = merge_permission(cli.permission, file_cfg.permission.clone(), preset);
        let use_runtime_enforcer =
            !cli.no_runtime_enforcer && !file_cfg.no_runtime_enforcer.unwrap_or(false);
        let rag_url = resolve_rag_base_url(&file_cfg);
        print_tools_dry_run(
            permission_mode,
            use_runtime_enforcer,
            rag_url.as_deref(),
            &mut std::io::stdout(),
        )?;
        return Ok(());
    }

    let pre_output_format = cli
        .output_format
        .map(|o| match o {
            OutputFormatArg::Rich => OutputFormat::Rich,
            OutputFormatArg::Json => OutputFormat::Json,
        })
        .or_else(|| {
            file_cfg
                .output_format
                .as_deref()
                .and_then(output_format_from_toml)
        })
        .unwrap_or(OutputFormat::Rich);

    let prompt = if let Some(p) = cli.prompt.clone() {
        p
    } else {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        if buf.trim().is_empty() {
            if matches!(pre_output_format, OutputFormat::Json) {
                println!(
                    "{}",
                    serde_json::json!({"type": "error", "message": "empty prompt (pass as arg or stdin)"})
                );
            }
            return Err("empty prompt (pass as arg or stdin)".into());
        }
        buf
    };

    let preset = merge_preset(cli.preset, file_cfg.preset.as_deref(), &prompt);
    let permission_mode = merge_permission(cli.permission, file_cfg.permission.clone(), preset);

    let session_path = resolve_session_path(
        cli.session.clone(),
        file_cfg.session.as_deref(),
        &cli.workspace,
    );

    let profile_path = resolve_analog_profile_path(
        &cli.workspace,
        cli.profile.clone(),
        file_cfg.profile.as_deref(),
    );

    let profile_hint = if let Some(ref p) = profile_path {
        load_profile_hint(p)?
    } else {
        None
    };

    let config = build_config(
        &cli,
        &file_cfg,
        prompt,
        profile_hint,
        session_path,
        preset,
        permission_mode,
    );
    let output_format = config.output_format;

    let mut out = std::io::stdout();
    if let Err(e) = claw_analog::run(config, &mut out).await {
        if matches!(output_format, OutputFormat::Json) {
            println!(
                "{}",
                serde_json::json!({"type": "error", "message": e.to_string()})
            );
        }
        return Err(e);
    }
    Ok(())
}
