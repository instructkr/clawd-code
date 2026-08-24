use std::fs;
use std::path::{Path, PathBuf};

use commands::{CommandManifestEntry, CommandRegistry, CommandSource};
use runtime::{BootstrapPhase, BootstrapPlan};
use tools::{ToolManifestEntry, ToolRegistry, ToolSource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamPaths {
    repo_root: PathBuf,
}

impl UpstreamPaths {
    #[must_use]
    pub fn from_repo_root(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
        }
    }

    /// Returns the repository root path.
    #[must_use]
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    #[must_use]
    pub fn from_workspace_dir(workspace_dir: impl AsRef<Path>) -> Self {
        let workspace_dir = workspace_dir
            .as_ref()
            .canonicalize()
            .unwrap_or_else(|_| workspace_dir.as_ref().to_path_buf());
        let primary_repo_root = workspace_dir
            .parent()
            .map_or_else(|| PathBuf::from(".."), Path::to_path_buf);
        let repo_root = resolve_upstream_repo_root(&primary_repo_root);
        Self { repo_root }
    }

    #[must_use]
    pub fn commands_path(&self) -> PathBuf {
        self.repo_root.join("src/commands.ts")
    }

    #[must_use]
    pub fn tools_path(&self) -> PathBuf {
        self.repo_root.join("src/tools.ts")
    }

    #[must_use]
    pub fn cli_path(&self) -> PathBuf {
        self.repo_root.join("src/entrypoints/cli.tsx")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedManifest {
    pub commands: CommandRegistry,
    pub tools: ToolRegistry,
    pub bootstrap: BootstrapPlan,
}

fn resolve_upstream_repo_root(primary_repo_root: &Path) -> PathBuf {
    let candidates = upstream_repo_candidates(primary_repo_root);
    candidates
        .into_iter()
        .find(|candidate| candidate.join("src/commands.ts").is_file())
        .unwrap_or_else(|| primary_repo_root.to_path_buf())
}

fn upstream_repo_candidates(primary_repo_root: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![primary_repo_root.to_path_buf()];

    if let Some(explicit) = std::env::var_os("CLAUDE_CODE_UPSTREAM") {
        candidates.push(PathBuf::from(explicit));
    }

    for ancestor in primary_repo_root.ancestors().take(4) {
        candidates.push(ancestor.join("claw-code"));
        candidates.push(ancestor.join("clawd-code"));
    }

    candidates.push(primary_repo_root.join("reference-source").join("claw-code"));
    candidates.push(primary_repo_root.join("vendor").join("claw-code"));

    let mut deduped = Vec::new();
    for candidate in candidates {
        if !deduped.iter().any(|seen: &PathBuf| seen == &candidate) {
            deduped.push(candidate);
        }
    }
    deduped
}

pub fn extract_manifest(paths: &UpstreamPaths) -> std::io::Result<ExtractedManifest> {
    let commands_source = fs::read_to_string(paths.commands_path())?;
    let tools_source = fs::read_to_string(paths.tools_path())?;
    let cli_source = fs::read_to_string(paths.cli_path())?;

    Ok(ExtractedManifest {
        commands: extract_commands(&commands_source),
        tools: extract_tools(&tools_source),
        bootstrap: extract_bootstrap_plan(&cli_source),
    })
}

#[must_use]
pub fn extract_commands(source: &str) -> CommandRegistry {
    let mut entries = Vec::new();
    let mut in_internal_block = false;

    for raw_line in source.lines() {
        let line = raw_line.trim();

        if line.starts_with("export const INTERNAL_ONLY_COMMANDS = [") {
            in_internal_block = true;
            continue;
        }

        if in_internal_block {
            if line.starts_with(']') {
                in_internal_block = false;
                continue;
            }
            if let Some(name) = first_identifier(line) {
                entries.push(CommandManifestEntry {
                    name,
                    source: CommandSource::InternalOnly,
                });
            }
            continue;
        }

        if line.starts_with("import ") {
            for imported in imported_symbols(line) {
                entries.push(CommandManifestEntry {
                    name: imported,
                    source: CommandSource::Builtin,
                });
            }
        }

        if line.contains("feature('") && line.contains("./commands/") {
            if let Some(name) = first_assignment_identifier(line) {
                entries.push(CommandManifestEntry {
                    name,
                    source: CommandSource::FeatureGated,
                });
            }
        }
    }

    dedupe_commands(entries)
}

#[must_use]
pub fn extract_tools(source: &str) -> ToolRegistry {
    let mut entries = Vec::new();

    for raw_line in source.lines() {
        let line = raw_line.trim();
        if line.starts_with("import ") && line.contains("./tools/") {
            for imported in imported_symbols(line) {
                if imported.ends_with("Tool") {
                    entries.push(ToolManifestEntry {
                        name: imported,
                        source: ToolSource::Base,
                    });
                }
            }
        }

        if line.contains("feature('") && line.contains("Tool") {
            if let Some(name) = first_assignment_identifier(line) {
                if name.ends_with("Tool") || name.ends_with("Tools") {
                    entries.push(ToolManifestEntry {
                        name,
                        source: ToolSource::Conditional,
                    });
                }
            }
        }
    }

    dedupe_tools(entries)
}

#[must_use]
pub fn extract_bootstrap_plan(source: &str) -> BootstrapPlan {
    let mut phases = vec![BootstrapPhase::CliEntry];

    if source.contains("--version") {
        phases.push(BootstrapPhase::FastPathVersion);
    }
    if source.contains("startupProfiler") {
        phases.push(BootstrapPhase::StartupProfiler);
    }
    if source.contains("--dump-system-prompt") {
        phases.push(BootstrapPhase::SystemPromptFastPath);
    }
    if source.contains("--claude-in-chrome-mcp") {
        phases.push(BootstrapPhase::ChromeMcpFastPath);
    }
    if source.contains("--daemon-worker") {
        phases.push(BootstrapPhase::DaemonWorkerFastPath);
    }
    if source.contains("remote-control") {
        phases.push(BootstrapPhase::BridgeFastPath);
    }
    if source.contains("args[0] === 'daemon'") {
        phases.push(BootstrapPhase::DaemonFastPath);
    }
    if source.contains("args[0] === 'ps'") || source.contains("args.includes('--bg')") {
        phases.push(BootstrapPhase::BackgroundSessionFastPath);
    }
    if source.contains("args[0] === 'new' || args[0] === 'list' || args[0] === 'reply'") {
        phases.push(BootstrapPhase::TemplateFastPath);
    }
    if source.contains("environment-runner") {
        phases.push(BootstrapPhase::EnvironmentRunnerFastPath);
    }
    phases.push(BootstrapPhase::MainRuntime);

    BootstrapPlan::from_phases(phases)
}

fn imported_symbols(line: &str) -> Vec<String> {
    let Some(after_import) = line.strip_prefix("import ") else {
        return Vec::new();
    };

    let before_from = after_import
        .split(" from ")
        .next()
        .unwrap_or_default()
        .trim();
    if before_from.starts_with('{') {
        return before_from
            .trim_matches(|c| c == '{' || c == '}')
            .split(',')
            .filter_map(|part| {
                let trimmed = part.trim();
                if trimmed.is_empty() {
                    return None;
                }
                Some(trimmed.split_whitespace().next()?.to_string())
            })
            .collect();
    }

    let first = before_from.split(',').next().unwrap_or_default().trim();
    if first.is_empty() {
        Vec::new()
    } else {
        vec![first.to_string()]
    }
}

fn first_assignment_identifier(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let candidate = trimmed.split('=').next()?.trim();
    first_identifier(candidate)
}

fn first_identifier(line: &str) -> Option<String> {
    let mut out = String::new();
    for ch in line.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else if !out.is_empty() {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

fn dedupe_commands(entries: Vec<CommandManifestEntry>) -> CommandRegistry {
    let mut deduped = Vec::new();
    for entry in entries {
        let exists = deduped.iter().any(|seen: &CommandManifestEntry| {
            seen.name == entry.name && seen.source == entry.source
        });
        if !exists {
            deduped.push(entry);
        }
    }
    CommandRegistry::new(deduped)
}

fn dedupe_tools(entries: Vec<ToolManifestEntry>) -> ToolRegistry {
    let mut deduped = Vec::new();
    for entry in entries {
        let exists = deduped
            .iter()
            .any(|seen: &ToolManifestEntry| seen.name == entry.name && seen.source == entry.source);
        if !exists {
            deduped.push(entry);
        }
    }
    ToolRegistry::new(deduped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_paths() -> UpstreamPaths {
        let workspace_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        UpstreamPaths::from_workspace_dir(workspace_dir)
    }

    fn has_upstream_fixture(paths: &UpstreamPaths) -> bool {
        paths.commands_path().is_file()
            && paths.tools_path().is_file()
            && paths.cli_path().is_file()
    }

    #[test]
    fn extracts_non_empty_manifests_from_upstream_repo() {
        let paths = fixture_paths();
        if !has_upstream_fixture(&paths) {
            return;
        }
        let manifest = extract_manifest(&paths).expect("manifest should load");
        assert!(!manifest.commands.entries().is_empty());
        assert!(!manifest.tools.entries().is_empty());
        assert!(!manifest.bootstrap.phases().is_empty());
    }

    #[test]
    fn detects_known_upstream_command_symbols() {
        let paths = fixture_paths();
        if !paths.commands_path().is_file() {
            return;
        }
        let commands =
            extract_commands(&fs::read_to_string(paths.commands_path()).expect("commands.ts"));
        let names: Vec<_> = commands
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert!(names.contains(&"addDir"));
        assert!(names.contains(&"review"));
        assert!(!names.contains(&"INTERNAL_ONLY_COMMANDS"));
    }

    #[test]
    fn detects_known_upstream_tool_symbols() {
        let paths = fixture_paths();
        if !paths.tools_path().is_file() {
            return;
        }
        let tools = extract_tools(&fs::read_to_string(paths.tools_path()).expect("tools.ts"));
        let names: Vec<_> = tools
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert!(names.contains(&"AgentTool"));
        assert!(names.contains(&"BashTool"));
    }
}
