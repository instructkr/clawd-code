//! `claw-analog agents` — run multiple specialized sub-agents sequentially.

use std::path::{Path, PathBuf};

use api::InputMessage;
use clap::{Parser, ValueEnum};
use claw_analog::{
    enforce_non_interactive_permission_rules, load_analog_toml, resolve_analog_options,
    resolve_analog_profile_path, resolve_rag_base_url, AnalogConfig, AnalogDoctorOverrides,
    AnalogFileConfig, OutputFormat, PermissionMode, Preset, StreamOverride,
};

const DEF_MAX_READ: u64 = 256 * 1024;
const DEF_MAX_TURNS: u32 = 24;
const DEF_MAX_LIST: usize = 500;
const DEF_GREP_MAX: usize = 200;
const DEF_GLOB_PATHS: usize = 2000;
const DEF_GLOB_DEPTH: usize = 32;
const DEF_RAG_TIMEOUT_SECS: u64 = 30;
const DEF_RAG_TOP_K_MAX: u32 = 32;
const RAG_TOP_K_ABS_CAP: u32 = 256;

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum AgentsPresetArg {
    Audit,
    Explain,
    Implement,
}

impl From<AgentsPresetArg> for Preset {
    fn from(p: AgentsPresetArg) -> Self {
        match p {
            AgentsPresetArg::Audit => Preset::Audit,
            AgentsPresetArg::Explain => Preset::Explain,
            AgentsPresetArg::Implement => Preset::Implement,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum AgentsPermissionArg {
    ReadOnly,
    WorkspaceWrite,
    Prompt,
    #[value(name = "danger-full-access")]
    DangerFullAccess,
    Allow,
}

impl From<AgentsPermissionArg> for PermissionMode {
    fn from(p: AgentsPermissionArg) -> Self {
        match p {
            AgentsPermissionArg::ReadOnly => PermissionMode::ReadOnly,
            AgentsPermissionArg::WorkspaceWrite => PermissionMode::WorkspaceWrite,
            AgentsPermissionArg::Prompt => PermissionMode::Prompt,
            AgentsPermissionArg::DangerFullAccess => PermissionMode::DangerFullAccess,
            AgentsPermissionArg::Allow => PermissionMode::Allow,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub name: String,
    pub preset: Preset,
    pub permission: PermissionMode,
    pub model: Option<String>,
    pub prompt: Option<String>,
}

fn default_permission_for_preset(p: Preset) -> PermissionMode {
    match p {
        Preset::Audit | Preset::Explain => PermissionMode::ReadOnly,
        Preset::Implement => PermissionMode::WorkspaceWrite,
        Preset::None => PermissionMode::ReadOnly,
    }
}

fn parse_agent_spec(s: &str) -> Result<AgentSpec, String> {
    // Allowed forms:
    // - "audit" | "explain" | "implement"
    // - "name=audit,preset=audit,permission=read-only,model=...,prompt=..."
    let raw = s.trim();
    if raw.is_empty() {
        return Err("empty --agent spec".to_string());
    }

    if !raw.contains('=') {
        let preset = match raw.to_ascii_lowercase().as_str() {
            "audit" => Preset::Audit,
            "explain" => Preset::Explain,
            "implement" | "fix" => Preset::Implement,
            other => return Err(format!("unknown agent shorthand: {other}")),
        };
        return Ok(AgentSpec {
            name: raw.to_string(),
            preset,
            permission: default_permission_for_preset(preset),
            model: None,
            prompt: None,
        });
    }

    let mut name: Option<String> = None;
    let mut preset: Option<Preset> = None;
    let mut permission: Option<PermissionMode> = None;
    let mut model: Option<String> = None;
    let mut prompt: Option<String> = None;

    for part in raw.split(',') {
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("invalid agent spec part {part:?} (expected k=v)"))?;
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        match k.as_str() {
            "name" => name = Some(v.to_string()),
            "preset" => {
                let p = match v.to_ascii_lowercase().as_str() {
                    "audit" => Preset::Audit,
                    "explain" => Preset::Explain,
                    "implement" | "fix" => Preset::Implement,
                    "none" => Preset::None,
                    other => return Err(format!("unknown preset {other:?}")),
                };
                preset = Some(p);
            }
            "permission" => {
                let pm = match v.to_ascii_lowercase().replace('_', "-").as_str() {
                    "read-only" | "readonly" => PermissionMode::ReadOnly,
                    "workspace-write" | "write" => PermissionMode::WorkspaceWrite,
                    "prompt" => PermissionMode::Prompt,
                    "danger-full-access" | "danger" => PermissionMode::DangerFullAccess,
                    "allow" => PermissionMode::Allow,
                    other => return Err(format!("unknown permission {other:?}")),
                };
                permission = Some(pm);
            }
            "model" => model = Some(v.to_string()),
            "prompt" => prompt = Some(v.to_string()),
            other => return Err(format!("unknown agent spec key {other:?}")),
        }
    }

    let preset = preset.unwrap_or(Preset::Audit);
    let permission = permission.unwrap_or_else(|| default_permission_for_preset(preset));
    let name = name.unwrap_or_else(|| preset.label().unwrap_or("agent").to_string());

    Ok(AgentSpec {
        name,
        preset,
        permission,
        model,
        prompt,
    })
}

#[derive(Debug, Parser)]
pub struct AgentsCli {
    /// Workspace root.
    #[arg(short = 'w', long, default_value = ".", value_name = "DIR")]
    pub workspace: PathBuf,

    /// Config path (default: `<workspace>/.claw-analog.toml`).
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Base session path. If missing, it will be created from the base prompt.
    #[arg(long, value_name = "PATH")]
    pub base_session: PathBuf,

    /// Base prompt. If omitted, reads from stdin.
    #[arg(long)]
    pub prompt: Option<String>,

    /// Repeatable agent specs, e.g. `--agent audit` or `--agent name=fix,preset=implement,permission=workspace-write`.
    #[arg(long, required = true)]
    pub agent: Vec<String>,

    /// If set, each agent writes its own session file next to base session.
    #[arg(long, default_value_t = true)]
    pub split_sessions: bool,
}

fn load_file_config(path: &Path) -> AnalogFileConfig {
    if !path.is_file() {
        return AnalogFileConfig::default();
    }
    load_analog_toml(path).unwrap_or_default()
}

fn config_path(args: &AgentsCli) -> PathBuf {
    args.config
        .clone()
        .unwrap_or_else(|| args.workspace.join(".claw-analog.toml"))
}

fn derive_agent_session_path(base: &Path, agent_name: &str) -> PathBuf {
    let base_s = base.to_string_lossy();
    PathBuf::from(format!("{base_s}.agent-{agent_name}.json"))
}

fn read_stdin_prompt() -> Result<String, String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| e.to_string())?;
    let t = buf.trim();
    if t.is_empty() {
        return Err("empty prompt (pass --prompt or stdin)".to_string());
    }
    Ok(t.to_string())
}

fn ensure_base_session(base_session: &Path, workspace: &Path, prompt: &str) -> Result<(), String> {
    if base_session.exists() {
        return Ok(());
    }
    let ws_s = workspace.display().to_string();
    let model = "base".to_string();
    let messages = if prompt.trim().is_empty() {
        Vec::new()
    } else {
        vec![InputMessage::user_text(prompt.to_string())]
    };
    claw_analog::session_save(base_session, &ws_s, &model, Preset::None, &messages)?;
    Ok(())
}

pub fn run_agents(args: AgentsCli) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async { run_agents_async(args).await })
}

pub async fn run_agents_async(args: AgentsCli) -> Result<(), String> {
    run_agents_inner(args, |cfg, out| {
        Box::pin(async move {
            claw_analog::run(cfg, out)
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        })
    })
    .await
}

type RunFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + 'a>>;

async fn run_agents_inner<F>(args: AgentsCli, mut run_one: F) -> Result<(), String>
where
    for<'a> F: FnMut(AnalogConfig, &'a mut Vec<u8>) -> RunFuture<'a>,
{
    let workspace = if args.workspace.is_absolute() {
        args.workspace.clone()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(&args.workspace)
    };
    let cfg_path = config_path(&args);
    let file_cfg = load_file_config(&cfg_path);

    let base_prompt = match args.prompt.clone() {
        Some(p) => p,
        None => read_stdin_prompt()?,
    };
    ensure_base_session(&args.base_session, &workspace, base_prompt.as_str())?;

    let mut specs = Vec::new();
    for a in &args.agent {
        specs.push(parse_agent_spec(a)?);
    }

    println!("claw-analog agents (sequential)\n");
    println!("  workspace: {}", workspace.display());
    println!("  base_session: {}", args.base_session.display());
    println!("  agents: {}", specs.len());
    println!();

    for (i, spec) in specs.into_iter().enumerate() {
        println!(
            "== Agent {} / {}: {} ==",
            i + 1,
            args.agent.len(),
            spec.name
        );
        println!("  preset: {}", spec.preset.label().unwrap_or("none"));
        println!("  permission: {}", spec.permission.as_str());
        if let Some(m) = &spec.model {
            println!("  model: {m}");
        }

        enforce_non_interactive_permission_rules(spec.permission, false)?;

        let agent_session = if args.split_sessions {
            derive_agent_session_path(&args.base_session, spec.name.as_str())
        } else {
            args.base_session.clone()
        };
        if args.split_sessions {
            std::fs::copy(&args.base_session, &agent_session).map_err(|e| e.to_string())?;
        }

        let overrides = AnalogDoctorOverrides {
            model: spec.model.clone(),
            permission: Some(spec.permission),
            preset: Some(spec.preset),
            output_format: Some(OutputFormat::Rich),
            stream: StreamOverride::ForceOff,
            ..Default::default()
        };
        let resolved = resolve_analog_options(&file_cfg, &overrides);

        let profile_path =
            resolve_analog_profile_path(&workspace, None, file_cfg.profile.as_deref());
        let profile_hint = if let Some(ref p) = profile_path {
            claw_analog::load_profile_hint(p).unwrap_or(None)
        } else {
            None
        };

        let rag_base_url = resolve_rag_base_url(&file_cfg);

        let agent_prompt = spec.prompt.unwrap_or_else(|| {
            format!(
                "Agent {}: run preset {}",
                spec.name,
                resolved.preset.label().unwrap_or("none")
            )
        });

        let cfg = AnalogConfig {
            model: resolved.model,
            workspace: workspace.clone(),
            permission_mode: resolved.permission_mode,
            accept_danger_non_interactive: false,
            use_stream: false,
            output_format: resolved.output_format,
            use_runtime_enforcer: resolved.use_runtime_enforcer,
            max_read_bytes: file_cfg.max_read_bytes.unwrap_or(DEF_MAX_READ),
            max_turns: file_cfg.max_turns.unwrap_or(DEF_MAX_TURNS),
            max_list_entries: file_cfg.max_list_entries.unwrap_or(DEF_MAX_LIST),
            grep_max_lines: file_cfg.grep_max_lines.unwrap_or(DEF_GREP_MAX),
            glob_max_paths: file_cfg.glob_max_paths.unwrap_or(DEF_GLOB_PATHS),
            glob_max_depth: file_cfg.glob_max_depth.unwrap_or(DEF_GLOB_DEPTH),
            preset: resolved.preset,
            language: file_cfg
                .language
                .as_deref()
                .and_then(claw_analog::AnalogLanguage::from_toml_str)
                .unwrap_or_default(),
            session_path: Some(agent_session.clone()),
            session_save_path: None,
            profile_hint,
            prompt: agent_prompt,
            rag_base_url,
            rag_http_timeout: std::time::Duration::from_secs(
                file_cfg.rag_timeout_secs.unwrap_or(DEF_RAG_TIMEOUT_SECS),
            ),
            rag_top_k_max: file_cfg
                .rag_top_k_max
                .unwrap_or(DEF_RAG_TOP_K_MAX)
                .clamp(1, RAG_TOP_K_ABS_CAP),
        };

        let mut buf: Vec<u8> = Vec::new();
        let run_res = run_one(cfg, &mut buf).await;
        match run_res {
            Ok(()) => {
                let text = String::from_utf8_lossy(&buf);
                let summary = tail_chars(text.as_ref(), 1600);
                println!("  result: OK");
                if args.split_sessions {
                    println!("  session: {}", agent_session.display());
                }
                println!("  summary_tail:\n{}\n", indent_lines(&summary, 4));
            }
            Err(e) => {
                println!("  result: FAIL — {e}\n");
            }
        }
    }

    Ok(())
}

fn tail_chars(s: &str, n: usize) -> String {
    let total = s.chars().count();
    if total <= n {
        return s.to_string();
    }
    s.chars().skip(total - n).collect()
}

fn indent_lines(s: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    s.lines()
        .map(|l| format!("{pad}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
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

    #[test]
    fn parses_agent_shorthand() {
        let a = parse_agent_spec("audit").unwrap();
        assert_eq!(a.preset, Preset::Audit);
        assert_eq!(a.permission, PermissionMode::ReadOnly);
    }

    #[test]
    fn parses_agent_kv() {
        let a = parse_agent_spec("name=fix,preset=implement,permission=workspace-write").unwrap();
        assert_eq!(a.name, "fix");
        assert_eq!(a.preset, Preset::Implement);
        assert_eq!(a.permission, PermissionMode::WorkspaceWrite);
    }

    #[test]
    fn runs_two_agents_sequentially_with_stub_runner() {
        let _g = mock_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().unwrap();
        std::fs::write(workspace.join("fixture.txt"), "hello parity fixture\n").unwrap();

        let base_session = workspace.join(".claw").join("agents-base.json");
        std::fs::create_dir_all(base_session.parent().unwrap()).unwrap();
        std::fs::write(
            &base_session,
            format!(
                "{{\n  \"version\": 1,\n  \"workspace\": \"{}\",\n  \"model\": \"base\",\n  \"messages\": []\n}}\n",
                workspace.display()
            ),
        )
        .unwrap();
        let args = AgentsCli {
            workspace: workspace.clone(),
            config: None,
            base_session: base_session.clone(),
            prompt: Some(String::new()),
            agent: vec![
                "name=audit,preset=audit,permission=read-only,prompt=check 1".to_string(),
                "name=explain,preset=explain,permission=read-only,prompt=check 2".to_string(),
            ],
            split_sessions: true,
        };
        let called = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let called2 = called.clone();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            run_agents_inner(args, move |_cfg, out| {
                let called3 = called2.clone();
                Box::pin(async move {
                    called3.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    out.extend_from_slice(b"stub ok");
                    Ok(())
                })
            })
            .await
            .expect("agents should run");
        });
        assert_eq!(called.load(std::sync::atomic::Ordering::Relaxed), 2);

        assert!(derive_agent_session_path(&base_session, "audit").is_file());
        assert!(derive_agent_session_path(&base_session, "explain").is_file());
    }
}
