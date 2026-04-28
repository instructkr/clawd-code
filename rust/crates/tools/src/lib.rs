use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use api::{
    max_tokens_for_model, resolve_model_alias, ApiError, ContentBlockDelta, InputContentBlock,
    InputMessage, MessageRequest, MessageResponse, OutputContentBlock, ProviderClient,
    StreamEvent as ApiStreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock,
};
use plugins::PluginTool;
use reqwest::blocking::Client;
use runtime::{
    check_freshness, dedupe_superseded_commit_events, edit_file, execute_bash, glob_search,
    grep_search, load_system_prompt,
    lsp_client::LspRegistry,
    mcp_tool_bridge::McpToolRegistry,
    permission_enforcer::{EnforcementResult, PermissionEnforcer},
    read_file,
    summary_compression::compress_summary_text,
    task_registry::TaskRegistry,
    team_cron_registry::{CronRegistry, TeamRegistry},
    worker_boot::{WorkerReadySnapshot, WorkerRegistry, WorkerTaskReceipt},
    write_file, ApiClient, ApiRequest, AssistantEvent, BashCommandInput, BashCommandOutput,
    BranchFreshness, ConfigLoader, ContentBlock, ConversationMessage, ConversationRuntime,
    GrepSearchInput, LaneCommitProvenance, LaneEvent, LaneEventBlocker, LaneEventName,
    LaneEventStatus, LaneFailureClass, McpDegradedReport, MessageRole, PermissionMode,
    PermissionPolicy, PromptCacheEvent, ProviderFallbackConfig, RuntimeError, Session, TaskPacket,
    ToolError, ToolExecutor,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

mod disk;

/// Global task registry shared across tool invocations within a session.
fn global_lsp_registry() -> &'static LspRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<LspRegistry> = OnceLock::new();
    REGISTRY.get_or_init(LspRegistry::new)
}

fn global_mcp_registry() -> &'static McpToolRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<McpToolRegistry> = OnceLock::new();
    REGISTRY.get_or_init(McpToolRegistry::new)
}

fn global_team_registry() -> &'static TeamRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<TeamRegistry> = OnceLock::new();
    REGISTRY.get_or_init(TeamRegistry::new)
}

fn global_cron_registry() -> &'static CronRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<CronRegistry> = OnceLock::new();
    REGISTRY.get_or_init(CronRegistry::new)
}

fn global_task_registry() -> &'static TaskRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<TaskRegistry> = OnceLock::new();
    REGISTRY.get_or_init(TaskRegistry::new)
}

fn global_worker_registry() -> &'static WorkerRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<WorkerRegistry> = OnceLock::new();
    REGISTRY.get_or_init(WorkerRegistry::new)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolManifestEntry {
    pub name: String,
    pub source: ToolSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSource {
    Base,
    Conditional,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolRegistry {
    entries: Vec<ToolManifestEntry>,
}

impl ToolRegistry {
    #[must_use]
    pub fn new(entries: Vec<ToolManifestEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[ToolManifestEntry] {
        &self.entries
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub required_permission: PermissionMode,
}

#[derive(Debug, Clone)]
pub struct GlobalToolRegistry {
    plugin_tools: Vec<PluginTool>,
    runtime_tools: Vec<RuntimeToolDefinition>,
    enforcer: Option<PermissionEnforcer>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeToolDefinition {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub required_permission: PermissionMode,
}

impl GlobalToolRegistry {
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            plugin_tools: Vec::new(),
            runtime_tools: Vec::new(),
            enforcer: None,
        }
    }

    pub fn with_plugin_tools(plugin_tools: Vec<PluginTool>) -> Result<Self, String> {
        let builtin_names = mvp_tool_specs()
            .into_iter()
            .map(|spec| spec.name.to_string())
            .collect::<BTreeSet<_>>();
        let mut seen_plugin_names = BTreeSet::new();

        for tool in &plugin_tools {
            let name = tool.definition().name.clone();
            if builtin_names.contains(&name) {
                return Err(format!(
                    "plugin tool `{name}` conflicts with a built-in tool name"
                ));
            }
            if !seen_plugin_names.insert(name.clone()) {
                return Err(format!("duplicate plugin tool name `{name}`"));
            }
        }

        Ok(Self {
            plugin_tools,
            runtime_tools: Vec::new(),
            enforcer: None,
        })
    }

    pub fn with_runtime_tools(
        mut self,
        runtime_tools: Vec<RuntimeToolDefinition>,
    ) -> Result<Self, String> {
        let mut seen_names = mvp_tool_specs()
            .into_iter()
            .map(|spec| spec.name.to_string())
            .chain(
                self.plugin_tools
                    .iter()
                    .map(|tool| tool.definition().name.clone()),
            )
            .collect::<BTreeSet<_>>();

        for tool in &runtime_tools {
            if !seen_names.insert(tool.name.clone()) {
                return Err(format!(
                    "runtime tool `{}` conflicts with an existing tool name",
                    tool.name
                ));
            }
        }

        self.runtime_tools = runtime_tools;
        Ok(self)
    }

    #[must_use]
    pub fn with_enforcer(mut self, enforcer: PermissionEnforcer) -> Self {
        self.set_enforcer(enforcer);
        self
    }

    pub fn normalize_allowed_tools(
        &self,
        values: &[String],
    ) -> Result<Option<BTreeSet<String>>, String> {
        if values.is_empty() {
            return Ok(None);
        }

        let builtin_specs = mvp_tool_specs();
        let canonical_names = builtin_specs
            .iter()
            .map(|spec| spec.name.to_string())
            .chain(
                self.plugin_tools
                    .iter()
                    .map(|tool| tool.definition().name.clone()),
            )
            .chain(self.runtime_tools.iter().map(|tool| tool.name.clone()))
            .collect::<Vec<_>>();
        let mut name_map = canonical_names
            .iter()
            .map(|name| (normalize_tool_name(name), name.clone()))
            .collect::<BTreeMap<_, _>>();

        for (alias, canonical) in [
            ("read", "read_file"),
            ("write", "write_file"),
            ("edit", "edit_file"),
            ("glob", "glob_search"),
            ("grep", "grep_search"),
        ] {
            name_map.insert(alias.to_string(), canonical.to_string());
        }

        let mut allowed = BTreeSet::new();
        for value in values {
            for token in value
                .split(|ch: char| ch == ',' || ch.is_whitespace())
                .filter(|token| !token.is_empty())
            {
                let normalized = normalize_tool_name(token);
                let canonical = name_map.get(&normalized).ok_or_else(|| {
                    format!(
                        "unsupported tool in --allowedTools: {token} (expected one of: {})",
                        canonical_names.join(", ")
                    )
                })?;
                allowed.insert(canonical.clone());
            }
        }

        if allowed.is_empty() {
            return Err(format!(
                "--allowedTools was provided with no usable tool names (got `{}`). Omit the flag to allow all tools.",
                values.join(" ")
            ));
        }

        Ok(Some(allowed))
    }

    #[must_use]
    pub fn definitions(&self, allowed_tools: Option<&BTreeSet<String>>) -> Vec<ToolDefinition> {
        let builtin = mvp_tool_specs()
            .into_iter()
            .filter(|spec| allowed_tools.is_none_or(|allowed| allowed.contains(spec.name)))
            .map(|spec| ToolDefinition {
                name: spec.name.to_string(),
                description: Some(spec.description.to_string()),
                input_schema: spec.input_schema,
            });
        let runtime = self
            .runtime_tools
            .iter()
            .filter(|tool| allowed_tools.is_none_or(|allowed| allowed.contains(tool.name.as_str())))
            .map(|tool| ToolDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
            });
        let plugin = self
            .plugin_tools
            .iter()
            .filter(|tool| {
                allowed_tools
                    .is_none_or(|allowed| allowed.contains(tool.definition().name.as_str()))
            })
            .map(|tool| ToolDefinition {
                name: tool.definition().name.clone(),
                description: tool.definition().description.clone(),
                input_schema: tool.definition().input_schema.clone(),
            });
        builtin.chain(runtime).chain(plugin).collect()
    }

    pub fn permission_specs(
        &self,
        allowed_tools: Option<&BTreeSet<String>>,
    ) -> Result<Vec<(String, PermissionMode)>, String> {
        let builtin = mvp_tool_specs()
            .into_iter()
            .filter(|spec| allowed_tools.is_none_or(|allowed| allowed.contains(spec.name)))
            .map(|spec| (spec.name.to_string(), spec.required_permission));
        let runtime = self
            .runtime_tools
            .iter()
            .filter(|tool| allowed_tools.is_none_or(|allowed| allowed.contains(tool.name.as_str())))
            .map(|tool| (tool.name.clone(), tool.required_permission));
        let plugin = self
            .plugin_tools
            .iter()
            .filter(|tool| {
                allowed_tools
                    .is_none_or(|allowed| allowed.contains(tool.definition().name.as_str()))
            })
            .map(|tool| {
                permission_mode_from_plugin(tool.required_permission())
                    .map(|permission| (tool.definition().name.clone(), permission))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(builtin.chain(runtime).chain(plugin).collect())
    }

    #[must_use]
    pub fn has_runtime_tool(&self, name: &str) -> bool {
        self.runtime_tools.iter().any(|tool| tool.name == name)
    }

    #[must_use]
    pub fn search(
        &self,
        query: &str,
        max_results: usize,
        pending_mcp_servers: Option<Vec<String>>,
        mcp_degraded: Option<McpDegradedReport>,
    ) -> ToolSearchOutput {
        let query = query.trim().to_string();
        let normalized_query = normalize_tool_search_query(&query);
        let matches = search_tool_specs(&query, max_results.max(1), &self.searchable_tool_specs());

        ToolSearchOutput {
            matches,
            query,
            normalized_query,
            total_deferred_tools: self.searchable_tool_specs().len(),
            pending_mcp_servers,
            mcp_degraded,
        }
    }

    pub fn set_enforcer(&mut self, enforcer: PermissionEnforcer) {
        self.enforcer = Some(enforcer);
    }

    pub fn execute(&self, name: &str, input: &Value) -> Result<String, String> {
        if mvp_tool_specs().iter().any(|spec| spec.name == name) {
            return execute_tool_with_enforcer(self.enforcer.as_ref(), name, input);
        }
        self.plugin_tools
            .iter()
            .find(|tool| tool.definition().name == name)
            .ok_or_else(|| format!("unsupported tool: {name}"))?
            .execute(input)
            .map_err(|error| error.to_string())
    }

    fn searchable_tool_specs(&self) -> Vec<SearchableToolSpec> {
        let builtin = deferred_tool_specs()
            .into_iter()
            .map(|spec| SearchableToolSpec {
                name: spec.name.to_string(),
                description: spec.description.to_string(),
            });
        let runtime = self.runtime_tools.iter().map(|tool| SearchableToolSpec {
            name: tool.name.clone(),
            description: tool.description.clone().unwrap_or_default(),
        });
        let plugin = self.plugin_tools.iter().map(|tool| SearchableToolSpec {
            name: tool.definition().name.clone(),
            description: tool.definition().description.clone().unwrap_or_default(),
        });
        builtin.chain(runtime).chain(plugin).collect()
    }
}

fn normalize_tool_name(value: &str) -> String {
    value.trim().replace('-', "_").to_ascii_lowercase()
}

fn permission_mode_from_plugin(value: &str) -> Result<PermissionMode, String> {
    match value {
        "read-only" => Ok(PermissionMode::ReadOnly),
        "workspace-write" => Ok(PermissionMode::WorkspaceWrite),
        "danger-full-access" => Ok(PermissionMode::DangerFullAccess),
        other => Err(format!("unsupported plugin permission: {other}")),
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn mvp_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "bash",
            description: "Execute a shell command in the current workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout": { "type": "integer", "minimum": 1 },
                    "description": { "type": "string" },
                    "run_in_background": { "type": "boolean" },
                    "dangerouslyDisableSandbox": { "type": "boolean" },
                    "namespaceRestrictions": { "type": "boolean" },
                    "isolateNetwork": { "type": "boolean" },
                    "filesystemMode": { "type": "string", "enum": ["off", "workspace-only", "allow-list"] },
                    "allowedMounts": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "read_file",
            description: "Read a text file from the workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "write_file",
            description: "Write a text file in the workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "edit_file",
            description: "Replace text in a workspace file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "glob_search",
            description: "Find files by glob pattern.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "grep_search",
            description: "Search file contents with a regex pattern.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "glob": { "type": "string" },
                    "output_mode": { "type": "string" },
                    "-B": { "type": "integer", "minimum": 0 },
                    "-A": { "type": "integer", "minimum": 0 },
                    "-C": { "type": "integer", "minimum": 0 },
                    "context": { "type": "integer", "minimum": 0 },
                    "-n": { "type": "boolean" },
                    "-i": { "type": "boolean" },
                    "type": { "type": "string" },
                    "head_limit": { "type": "integer", "minimum": 1 },
                    "offset": { "type": "integer", "minimum": 0 },
                    "multiline": { "type": "boolean" }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "git_diff",
            description: "Read-only `git diff` from the current workspace repo (no color).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cached": { "type": "boolean", "description": "Use --cached (staged diff)" },
                    "rev_range": { "type": "string", "description": "Revision range like `main...HEAD` or `HEAD~3..HEAD`" },
                    "context_lines": { "type": "integer", "minimum": 0, "maximum": 100, "description": "Unified diff context lines (-U<n>)" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Path filters (relative to repo)" }
                },
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "git_log",
            description: "Read-only `git log` from the current workspace repo (no color).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "max_count": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Max commits (default 20)" },
                    "rev_range": { "type": "string", "description": "Revision range like `HEAD~20..HEAD`" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Path filters (relative to repo)" }
                },
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WebFetch",
            description:
                "Fetch a URL, convert it into readable text, and answer a prompt about it.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "format": "uri" },
                    "prompt": { "type": "string" }
                },
                "required": ["url", "prompt"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WebSearch",
            description: "Search the web for current information and return cited results.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 2 },
                    "allowed_domains": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "blocked_domains": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "TodoWrite",
            description: "Update the structured task list for the current session.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "activeForm": { "type": "string" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["content", "activeForm", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["todos"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "Skill",
            description: "Load a local skill definition and its instructions.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "skill": { "type": "string" },
                    "args": { "type": "string" }
                },
                "required": ["skill"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "Agent",
            description: "Launch a specialized agent task and persist its handoff metadata.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string" },
                    "prompt": { "type": "string" },
                    "subagent_type": { "type": "string" },
                    "name": { "type": "string" },
                    "model": { "type": "string" }
                },
                "required": ["description", "prompt"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "ToolSearch",
            description: "Search for deferred or specialized tools by exact name or keywords.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "max_results": { "type": "integer", "minimum": 1 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "NotebookEdit",
            description: "Replace, insert, or delete a cell in a Jupyter notebook.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "notebook_path": { "type": "string" },
                    "cell_id": { "type": "string" },
                    "new_source": { "type": "string" },
                    "cell_type": { "type": "string", "enum": ["code", "markdown"] },
                    "edit_mode": { "type": "string", "enum": ["replace", "insert", "delete"] }
                },
                "required": ["notebook_path"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "Sleep",
            description: "Wait for a specified duration without holding a shell process.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "duration_ms": { "type": "integer", "minimum": 0 }
                },
                "required": ["duration_ms"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "SendUserMessage",
            description: "Send a message to the user.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string" },
                    "attachments": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "status": {
                        "type": "string",
                        "enum": ["normal", "proactive"]
                    }
                },
                "required": ["message", "status"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "Config",
            description: "Get or set Claude Code settings.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "setting": { "type": "string" },
                    "value": {
                        "type": ["string", "boolean", "number"]
                    }
                },
                "required": ["setting"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "EnterPlanMode",
            description: "Enable a worktree-local planning mode override and remember the previous local setting for ExitPlanMode.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "ExitPlanMode",
            description: "Restore or clear the worktree-local planning mode override created by EnterPlanMode.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "StructuredOutput",
            description: "Return structured output in the requested format.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": true
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "REPL",
            description: "Execute code in a REPL-like subprocess.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string" },
                    "language": { "type": "string" },
                    "timeout_ms": { "type": "integer", "minimum": 1 }
                },
                "required": ["code", "language"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "PowerShell",
            description: "Execute a PowerShell command with optional timeout.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout": { "type": "integer", "minimum": 1 },
                    "description": { "type": "string" },
                    "run_in_background": { "type": "boolean" }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "DiskUsage",
            description: "Read-only disk usage report + cleanup suggestions for a directory (no deletion).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to scan (default: current working directory)." },
                    "max_entries": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Max number of top entries in the report." },
                    "max_files": { "type": "integer", "minimum": 1, "maximum": 2_000_000, "description": "Stop after scanning this many files." },
                    "max_seconds": { "type": "integer", "minimum": 1, "maximum": 120, "description": "Best-effort time limit for scanning." },
                    "min_file_mb": { "type": "integer", "minimum": 1, "maximum": 10240, "description": "Include files >= this size (MB) in the 'big files' list." }
                },
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "AskUserQuestion",
            description: "Ask the user a question and wait for their response.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["question"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "TaskCreate",
            description: "Create a background task that runs in a separate subprocess.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["prompt"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "RunTaskPacket",
            description: "Create a background task from a structured task packet.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "objective": { "type": "string" },
                    "scope": { "type": "string" },
                    "repo": { "type": "string" },
                    "branch_policy": { "type": "string" },
                    "acceptance_tests": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "commit_policy": { "type": "string" },
                    "reporting_contract": { "type": "string" },
                    "escalation_policy": { "type": "string" }
                },
                "required": [
                    "objective",
                    "scope",
                    "repo",
                    "branch_policy",
                    "acceptance_tests",
                    "commit_policy",
                    "reporting_contract",
                    "escalation_policy"
                ],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TaskGet",
            description: "Get the status and details of a background task by ID.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "TaskList",
            description: "List all background tasks and their current status.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "TaskStop",
            description: "Stop a running background task by ID.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TaskUpdate",
            description: "Send a message or update to a running background task.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "message": { "type": "string" }
                },
                "required": ["task_id", "message"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TaskOutput",
            description: "Retrieve the output produced by a background task.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WorkerCreate",
            description: "Create a coding worker boot session with trust-gate and prompt-delivery guards.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "trusted_roots": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "auto_recover_prompt_misdelivery": { "type": "boolean" }
                },
                "required": ["cwd"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "WorkerGet",
            description: "Fetch the current worker boot state, last error, and event history.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WorkerObserve",
            description: "Feed a terminal snapshot into worker boot detection to resolve trust gates, ready handshakes, and prompt misdelivery.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" },
                    "screen_text": { "type": "string" }
                },
                "required": ["worker_id", "screen_text"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WorkerResolveTrust",
            description: "Resolve a detected trust prompt so worker boot can continue.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "WorkerAwaitReady",
            description: "Return the current ready-handshake verdict for a coding worker.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WorkerSendPrompt",
            description: "Send a task prompt only after the worker reaches ready_for_prompt; can replay a recovered prompt.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" },
                    "prompt": { "type": "string" },
                    "task_receipt": {
                        "type": "object",
                        "properties": {
                            "repo": { "type": "string" },
                            "task_kind": { "type": "string" },
                            "source_surface": { "type": "string" },
                            "expected_artifacts": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "objective_preview": { "type": "string" }
                        },
                        "required": ["repo", "task_kind", "source_surface", "objective_preview"],
                        "additionalProperties": false
                    }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "WorkerRestart",
            description: "Restart worker boot state after a failed or stale startup.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "WorkerTerminate",
            description: "Terminate a worker and mark the lane finished from the control plane.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "WorkerObserveCompletion",
            description: "Report session completion to the worker, classifying finish_reason into Finished or Failed (provider-degraded). Use after the opencode session completes to advance the worker to its terminal state.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" },
                    "finish_reason": { "type": "string" },
                    "tokens_output": { "type": "integer", "minimum": 0 }
                },
                "required": ["worker_id", "finish_reason", "tokens_output"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TeamCreate",
            description: "Create a team of sub-agents for parallel task execution.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "prompt": { "type": "string" },
                                "description": { "type": "string" }
                            },
                            "required": ["prompt"]
                        }
                    }
                },
                "required": ["name", "tasks"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TeamDelete",
            description: "Delete a team and stop all its running tasks.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "team_id": { "type": "string" }
                },
                "required": ["team_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "CronCreate",
            description: "Create a scheduled recurring task.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "schedule": { "type": "string" },
                    "prompt": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["schedule", "prompt"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "CronDelete",
            description: "Delete a scheduled recurring task by ID.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cron_id": { "type": "string" }
                },
                "required": ["cron_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "CronList",
            description: "List all scheduled recurring tasks.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "LSP",
            description: "Query Language Server Protocol for code intelligence (symbols, references, diagnostics).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["symbols", "references", "diagnostics", "definition", "hover"] },
                    "path": { "type": "string" },
                    "line": { "type": "integer", "minimum": 0 },
                    "character": { "type": "integer", "minimum": 0 },
                    "query": { "type": "string" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "ListMcpResources",
            description: "List available resources from connected MCP servers.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" }
                },
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "ReadMcpResource",
            description: "Read a specific resource from an MCP server by URI.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" },
                    "uri": { "type": "string" }
                },
                "required": ["uri"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "McpAuth",
            description: "Authenticate with an MCP server that requires OAuth or credentials.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" }
                },
                "required": ["server"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "RemoteTrigger",
            description: "Trigger a remote action or webhook endpoint.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "method": { "type": "string", "enum": ["GET", "POST", "PUT", "DELETE"] },
                    "headers": { "type": "object" },
                    "body": { "type": "string" }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "MCP",
            description: "Execute a tool provided by a connected MCP server.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" },
                    "tool": { "type": "string" },
                    "arguments": { "type": "object" }
                },
                "required": ["server", "tool"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TestingPermission",
            description: "Test-only tool for verifying permission enforcement behavior.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
    ]
}

/// Check permission before executing a tool. Returns Err with denial reason if blocked.
pub fn enforce_permission_check(
    enforcer: &PermissionEnforcer,
    tool_name: &str,
    input: &Value,
) -> Result<(), String> {
    let input_str = serde_json::to_string(input).unwrap_or_default();
    let result = enforcer.check(tool_name, &input_str);

    match result {
        EnforcementResult::Allowed => Ok(()),
        EnforcementResult::Denied { reason, .. } => Err(reason),
    }
}

pub fn execute_tool(name: &str, input: &Value) -> Result<String, String> {
    execute_tool_with_enforcer(None, name, input)
}

fn execute_tool_with_enforcer(
    enforcer: Option<&PermissionEnforcer>,
    name: &str,
    input: &Value,
) -> Result<String, String> {
    match name {
        "bash" => {
            // Parse input to get the command for permission classification
            let bash_input: BashCommandInput = from_value(input)?;
            let classified_mode = classify_bash_permission(&bash_input.command);
            maybe_enforce_permission_check_with_mode(enforcer, name, input, classified_mode)?;
            run_bash(bash_input)
        }
        "read_file" => {
            maybe_enforce_permission_check(enforcer, name, input)?;
            from_value::<ReadFileInput>(input).and_then(run_read_file)
        }
        "write_file" => {
            maybe_enforce_permission_check(enforcer, name, input)?;
            from_value::<WriteFileInput>(input).and_then(run_write_file)
        }
        "edit_file" => {
            maybe_enforce_permission_check(enforcer, name, input)?;
            from_value::<EditFileInput>(input).and_then(run_edit_file)
        }
        "glob_search" => {
            maybe_enforce_permission_check(enforcer, name, input)?;
            from_value::<GlobSearchInputValue>(input).and_then(run_glob_search)
        }
        "grep_search" => {
            maybe_enforce_permission_check(enforcer, name, input)?;
            from_value::<GrepSearchInput>(input).and_then(run_grep_search)
        }
        "git_diff" => {
            maybe_enforce_permission_check(enforcer, name, input)?;
            from_value::<GitDiffInput>(input).and_then(run_git_diff)
        }
        "git_log" => {
            maybe_enforce_permission_check(enforcer, name, input)?;
            from_value::<GitLogInput>(input).and_then(run_git_log)
        }
        "WebFetch" => from_value::<WebFetchInput>(input).and_then(run_web_fetch),
        "WebSearch" => from_value::<WebSearchInput>(input).and_then(run_web_search),
        "TodoWrite" => from_value::<TodoWriteInput>(input).and_then(run_todo_write),
        "Skill" => from_value::<SkillInput>(input).and_then(run_skill),
        "Agent" => from_value::<AgentInput>(input).and_then(run_agent),
        "ToolSearch" => from_value::<ToolSearchInput>(input).and_then(run_tool_search),
        "NotebookEdit" => from_value::<NotebookEditInput>(input).and_then(run_notebook_edit),
        "Sleep" => from_value::<SleepInput>(input).and_then(run_sleep),
        "SendUserMessage" | "Brief" => from_value::<BriefInput>(input).and_then(run_brief),
        "Config" => from_value::<ConfigInput>(input).and_then(run_config),
        "EnterPlanMode" => from_value::<EnterPlanModeInput>(input).and_then(run_enter_plan_mode),
        "ExitPlanMode" => from_value::<ExitPlanModeInput>(input).and_then(run_exit_plan_mode),
        "StructuredOutput" => {
            from_value::<StructuredOutputInput>(input).and_then(run_structured_output)
        }
        "REPL" => from_value::<ReplInput>(input).and_then(run_repl),
        "PowerShell" => {
            // Parse input to get the command for permission classification
            let ps_input: PowerShellInput = from_value(input)?;
            let classified_mode = classify_powershell_permission(&ps_input.command);
            maybe_enforce_permission_check_with_mode(enforcer, name, input, classified_mode)?;
            run_powershell(ps_input)
        }
        "DiskUsage" => {
            maybe_enforce_permission_check(enforcer, name, input)?;
            from_value::<disk::DiskUsageInput>(input).and_then(|v| run_disk_usage(&v))
        }
        "AskUserQuestion" => {
            from_value::<AskUserQuestionInput>(input).and_then(run_ask_user_question)
        }
        "TaskCreate" => from_value::<TaskCreateInput>(input).and_then(run_task_create),
        "RunTaskPacket" => from_value::<TaskPacket>(input).and_then(run_task_packet),
        "TaskGet" => from_value::<TaskIdInput>(input).and_then(run_task_get),
        "TaskList" => run_task_list(input.clone()),
        "TaskStop" => from_value::<TaskIdInput>(input).and_then(run_task_stop),
        "TaskUpdate" => from_value::<TaskUpdateInput>(input).and_then(run_task_update),
        "TaskOutput" => from_value::<TaskIdInput>(input).and_then(run_task_output),
        "WorkerCreate" => from_value::<WorkerCreateInput>(input).and_then(run_worker_create),
        "WorkerGet" => from_value::<WorkerIdInput>(input).and_then(run_worker_get),
        "WorkerObserve" => from_value::<WorkerObserveInput>(input).and_then(run_worker_observe),
        "WorkerResolveTrust" => {
            from_value::<WorkerIdInput>(input).and_then(run_worker_resolve_trust)
        }
        "WorkerAwaitReady" => from_value::<WorkerIdInput>(input).and_then(run_worker_await_ready),
        "WorkerSendPrompt" => {
            from_value::<WorkerSendPromptInput>(input).and_then(run_worker_send_prompt)
        }
        "WorkerRestart" => from_value::<WorkerIdInput>(input).and_then(run_worker_restart),
        "WorkerTerminate" => from_value::<WorkerIdInput>(input).and_then(run_worker_terminate),
        "WorkerObserveCompletion" => from_value::<WorkerObserveCompletionInput>(input)
            .and_then(run_worker_observe_completion),
        "TeamCreate" => from_value::<TeamCreateInput>(input).and_then(run_team_create),
        "TeamDelete" => from_value::<TeamDeleteInput>(input).and_then(run_team_delete),
        "CronCreate" => from_value::<CronCreateInput>(input).and_then(run_cron_create),
        "CronDelete" => from_value::<CronDeleteInput>(input).and_then(run_cron_delete),
        "CronList" => run_cron_list(input.clone()),
        "LSP" => from_value::<LspInput>(input).and_then(run_lsp),
        "ListMcpResources" => {
            from_value::<McpResourceInput>(input).and_then(run_list_mcp_resources)
        }
        "ReadMcpResource" => from_value::<McpResourceInput>(input).and_then(run_read_mcp_resource),
        "McpAuth" => from_value::<McpAuthInput>(input).and_then(run_mcp_auth),
        "RemoteTrigger" => from_value::<RemoteTriggerInput>(input).and_then(run_remote_trigger),
        "MCP" => from_value::<McpToolInput>(input).and_then(run_mcp_tool),
        "TestingPermission" => {
            from_value::<TestingPermissionInput>(input).and_then(run_testing_permission)
        }
        _ => Err(format!("unsupported tool: {name}")),
    }
}

fn maybe_enforce_permission_check(
    enforcer: Option<&PermissionEnforcer>,
    tool_name: &str,
    input: &Value,
) -> Result<(), String> {
    if let Some(enforcer) = enforcer {
        enforce_permission_check(enforcer, tool_name, input)?;
    }
    Ok(())
}

/// Enforce permission check with a dynamically classified permission mode.
/// Used for tools like bash and `PowerShell` where the required permission
/// depends on the actual command being executed.
fn maybe_enforce_permission_check_with_mode(
    enforcer: Option<&PermissionEnforcer>,
    tool_name: &str,
    input: &Value,
    required_mode: PermissionMode,
) -> Result<(), String> {
    if let Some(enforcer) = enforcer {
        let input_str = serde_json::to_string(input).unwrap_or_default();
        let result = enforcer.check_with_required_mode(tool_name, &input_str, required_mode);

        match result {
            EnforcementResult::Allowed => Ok(()),
            EnforcementResult::Denied { reason, .. } => Err(reason),
        }
    } else {
        Ok(())
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_ask_user_question(input: AskUserQuestionInput) -> Result<String, String> {
    use std::io::{self, BufRead, Write};

    // Display the question to the user via stdout
    let stdout = io::stdout();
    let stdin = io::stdin();
    let mut out = stdout.lock();

    writeln!(out, "\n[Question] {}", input.question).map_err(|e| e.to_string())?;

    if let Some(ref options) = input.options {
        for (i, option) in options.iter().enumerate() {
            writeln!(out, "  {}. {}", i + 1, option).map_err(|e| e.to_string())?;
        }
        write!(out, "Enter choice (1-{}): ", options.len()).map_err(|e| e.to_string())?;
    } else {
        write!(out, "Your answer: ").map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())?;

    // Read user response from stdin
    let mut response = String::new();
    stdin
        .lock()
        .read_line(&mut response)
        .map_err(|e| e.to_string())?;
    let response = response.trim().to_string();

    // If options were provided, resolve the numeric choice
    let answer = if let Some(ref options) = input.options {
        if let Ok(idx) = response.parse::<usize>() {
            if idx >= 1 && idx <= options.len() {
                options[idx - 1].clone()
            } else {
                response.clone()
            }
        } else {
            response.clone()
        }
    } else {
        response.clone()
    };

    to_pretty_json(json!({
        "question": input.question,
        "answer": answer,
        "status": "answered"
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_create(input: TaskCreateInput) -> Result<String, String> {
    let registry = global_task_registry();
    let task = registry.create(&input.prompt, input.description.as_deref());
    to_pretty_json(json!({
        "task_id": task.task_id,
        "status": task.status,
        "prompt": task.prompt,
        "description": task.description,
        "task_packet": task.task_packet,
        "created_at": task.created_at
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_packet(input: TaskPacket) -> Result<String, String> {
    let registry = global_task_registry();
    let task = registry
        .create_from_packet(input)
        .map_err(|error| error.to_string())?;

    to_pretty_json(json!({
        "task_id": task.task_id,
        "status": task.status,
        "prompt": task.prompt,
        "description": task.description,
        "task_packet": task.task_packet,
        "created_at": task.created_at
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_get(input: TaskIdInput) -> Result<String, String> {
    let registry = global_task_registry();
    match registry.get(&input.task_id) {
        Some(task) => to_pretty_json(json!({
            "task_id": task.task_id,
            "status": task.status,
            "prompt": task.prompt,
            "description": task.description,
            "task_packet": task.task_packet,
            "created_at": task.created_at,
            "updated_at": task.updated_at,
            "messages": task.messages,
            "team_id": task.team_id
        })),
        None => Err(format!("task not found: {}", input.task_id)),
    }
}

fn run_task_list(_input: Value) -> Result<String, String> {
    let registry = global_task_registry();
    let tasks: Vec<_> = registry
        .list(None)
        .into_iter()
        .map(|t| {
            json!({
                "task_id": t.task_id,
                "status": t.status,
                "prompt": t.prompt,
                "description": t.description,
                "task_packet": t.task_packet,
                "created_at": t.created_at,
                "updated_at": t.updated_at,
                "team_id": t.team_id
            })
        })
        .collect();
    to_pretty_json(json!({
        "tasks": tasks,
        "count": tasks.len()
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_stop(input: TaskIdInput) -> Result<String, String> {
    let registry = global_task_registry();
    match registry.stop(&input.task_id) {
        Ok(task) => to_pretty_json(json!({
            "task_id": task.task_id,
            "status": task.status,
            "message": "Task stopped"
        })),
        Err(e) => Err(e),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_update(input: TaskUpdateInput) -> Result<String, String> {
    let registry = global_task_registry();
    match registry.update(&input.task_id, &input.message) {
        Ok(task) => to_pretty_json(json!({
            "task_id": task.task_id,
            "status": task.status,
            "message_count": task.messages.len(),
            "last_message": input.message
        })),
        Err(e) => Err(e),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_output(input: TaskIdInput) -> Result<String, String> {
    let registry = global_task_registry();
    match registry.output(&input.task_id) {
        Ok(output) => to_pretty_json(json!({
            "task_id": input.task_id,
            "output": output,
            "has_output": !output.is_empty()
        })),
        Err(e) => Err(e),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_create(input: WorkerCreateInput) -> Result<String, String> {
    // Merge config-level trusted_roots with per-call overrides.
    // Config provides the default allowlist; per-call roots add on top.
    let config_roots: Vec<String> = ConfigLoader::default_for(&input.cwd)
        .load()
        .ok()
        .map(|c| c.trusted_roots().to_vec())
        .unwrap_or_default();
    let merged_roots: Vec<String> = config_roots
        .into_iter()
        .chain(input.trusted_roots.iter().cloned())
        .collect();
    let worker = global_worker_registry().create(
        &input.cwd,
        &merged_roots,
        input.auto_recover_prompt_misdelivery,
    );
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_get(input: WorkerIdInput) -> Result<String, String> {
    global_worker_registry().get(&input.worker_id).map_or_else(
        || Err(format!("worker not found: {}", input.worker_id)),
        to_pretty_json,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_observe(input: WorkerObserveInput) -> Result<String, String> {
    let worker = global_worker_registry().observe(&input.worker_id, &input.screen_text)?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_resolve_trust(input: WorkerIdInput) -> Result<String, String> {
    let worker = global_worker_registry().resolve_trust(&input.worker_id)?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_await_ready(input: WorkerIdInput) -> Result<String, String> {
    let snapshot: WorkerReadySnapshot = global_worker_registry().await_ready(&input.worker_id)?;
    to_pretty_json(snapshot)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_send_prompt(input: WorkerSendPromptInput) -> Result<String, String> {
    let worker = global_worker_registry().send_prompt(
        &input.worker_id,
        input.prompt.as_deref(),
        input.task_receipt,
    )?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_restart(input: WorkerIdInput) -> Result<String, String> {
    let worker = global_worker_registry().restart(&input.worker_id)?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_terminate(input: WorkerIdInput) -> Result<String, String> {
    let worker = global_worker_registry().terminate(&input.worker_id)?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_observe_completion(input: WorkerObserveCompletionInput) -> Result<String, String> {
    let worker = global_worker_registry().observe_completion(
        &input.worker_id,
        &input.finish_reason,
        input.tokens_output,
    )?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_team_create(input: TeamCreateInput) -> Result<String, String> {
    let task_ids: Vec<String> = input
        .tasks
        .iter()
        .filter_map(|t| t.get("task_id").and_then(|v| v.as_str()).map(str::to_owned))
        .collect();
    let team = global_team_registry().create(&input.name, task_ids);
    // Register team assignment on each task
    for task_id in &team.task_ids {
        let _ = global_task_registry().assign_team(task_id, &team.team_id);
    }
    to_pretty_json(json!({
        "team_id": team.team_id,
        "name": team.name,
        "task_count": team.task_ids.len(),
        "task_ids": team.task_ids,
        "status": team.status,
        "created_at": team.created_at
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_team_delete(input: TeamDeleteInput) -> Result<String, String> {
    match global_team_registry().delete(&input.team_id) {
        Ok(team) => to_pretty_json(json!({
            "team_id": team.team_id,
            "name": team.name,
            "status": team.status,
            "message": "Team deleted"
        })),
        Err(e) => Err(e),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_cron_create(input: CronCreateInput) -> Result<String, String> {
    let entry =
        global_cron_registry().create(&input.schedule, &input.prompt, input.description.as_deref());
    to_pretty_json(json!({
        "cron_id": entry.cron_id,
        "schedule": entry.schedule,
        "prompt": entry.prompt,
        "description": entry.description,
        "enabled": entry.enabled,
        "created_at": entry.created_at
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_cron_delete(input: CronDeleteInput) -> Result<String, String> {
    match global_cron_registry().delete(&input.cron_id) {
        Ok(entry) => to_pretty_json(json!({
            "cron_id": entry.cron_id,
            "schedule": entry.schedule,
            "status": "deleted",
            "message": "Cron entry removed"
        })),
        Err(e) => Err(e),
    }
}

fn run_cron_list(_input: Value) -> Result<String, String> {
    let entries: Vec<_> = global_cron_registry()
        .list(false)
        .into_iter()
        .map(|e| {
            json!({
                "cron_id": e.cron_id,
                "schedule": e.schedule,
                "prompt": e.prompt,
                "description": e.description,
                "enabled": e.enabled,
                "run_count": e.run_count,
                "last_run_at": e.last_run_at,
                "created_at": e.created_at
            })
        })
        .collect();
    to_pretty_json(json!({
        "crons": entries,
        "count": entries.len()
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_lsp(input: LspInput) -> Result<String, String> {
    let registry = global_lsp_registry();
    let action = &input.action;
    let path = input.path.as_deref();
    let line = input.line;
    let character = input.character;
    let query = input.query.as_deref();

    match registry.dispatch(action, path, line, character, query) {
        Ok(result) => to_pretty_json(result),
        Err(e) => to_pretty_json(json!({
            "action": action,
            "error": e,
            "status": "error"
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_list_mcp_resources(input: McpResourceInput) -> Result<String, String> {
    let registry = global_mcp_registry();
    let server = input.server.as_deref().unwrap_or("default");
    match registry.list_resources(server) {
        Ok(resources) => {
            let items: Vec<_> = resources
                .iter()
                .map(|r| {
                    json!({
                        "uri": r.uri,
                        "name": r.name,
                        "description": r.description,
                        "mime_type": r.mime_type,
                    })
                })
                .collect();
            to_pretty_json(json!({
                "server": server,
                "resources": items,
                "count": items.len()
            }))
        }
        Err(e) => to_pretty_json(json!({
            "server": server,
            "resources": [],
            "error": e
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_read_mcp_resource(input: McpResourceInput) -> Result<String, String> {
    let registry = global_mcp_registry();
    let uri = input.uri.as_deref().unwrap_or("");
    let server = input.server.as_deref().unwrap_or("default");
    match registry.read_resource(server, uri) {
        Ok(resource) => to_pretty_json(json!({
            "server": server,
            "uri": resource.uri,
            "name": resource.name,
            "description": resource.description,
            "mime_type": resource.mime_type
        })),
        Err(e) => to_pretty_json(json!({
            "server": server,
            "uri": uri,
            "error": e
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_mcp_auth(input: McpAuthInput) -> Result<String, String> {
    let registry = global_mcp_registry();
    match registry.get_server(&input.server) {
        Some(state) => to_pretty_json(json!({
            "server": input.server,
            "status": state.status,
            "server_info": state.server_info,
            "tool_count": state.tools.len(),
            "resource_count": state.resources.len()
        })),
        None => to_pretty_json(json!({
            "server": input.server,
            "status": "disconnected",
            "message": "Server not registered. Use MCP tool to connect first."
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_remote_trigger(input: RemoteTriggerInput) -> Result<String, String> {
    let method = input.method.unwrap_or_else(|| "GET".to_string());
    let client = Client::new();

    let mut request = match method.to_uppercase().as_str() {
        "GET" => client.get(&input.url),
        "POST" => client.post(&input.url),
        "PUT" => client.put(&input.url),
        "DELETE" => client.delete(&input.url),
        "PATCH" => client.patch(&input.url),
        "HEAD" => client.head(&input.url),
        other => return Err(format!("unsupported HTTP method: {other}")),
    };

    // Apply custom headers
    if let Some(ref headers) = input.headers {
        if let Some(obj) = headers.as_object() {
            for (key, value) in obj {
                if let Some(val) = value.as_str() {
                    request = request.header(key.as_str(), val);
                }
            }
        }
    }

    // Apply body
    if let Some(ref body) = input.body {
        request = request.body(body.clone());
    }

    // Execute with a 30-second timeout
    let request = request.timeout(Duration::from_secs(30));

    match request.send() {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            let truncated_body = if body.len() > 8192 {
                format!(
                    "{}\n\n[response truncated — {} bytes total]",
                    &body[..8192],
                    body.len()
                )
            } else {
                body
            };
            to_pretty_json(json!({
                "url": input.url,
                "method": method,
                "status_code": status,
                "body": truncated_body,
                "success": (200..300).contains(&status)
            }))
        }
        Err(e) => to_pretty_json(json!({
            "url": input.url,
            "method": method,
            "error": e.to_string(),
            "success": false
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_mcp_tool(input: McpToolInput) -> Result<String, String> {
    let registry = global_mcp_registry();
    let args = input.arguments.unwrap_or(serde_json::json!({}));
    match registry.call_tool(&input.server, &input.tool, &args) {
        Ok(result) => to_pretty_json(json!({
            "server": input.server,
            "tool": input.tool,
            "result": result,
            "status": "success"
        })),
        Err(e) => to_pretty_json(json!({
            "server": input.server,
            "tool": input.tool,
            "error": e,
            "status": "error"
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_testing_permission(input: TestingPermissionInput) -> Result<String, String> {
    to_pretty_json(json!({
        "action": input.action,
        "permitted": true,
        "message": "Testing permission tool stub"
    }))
}
fn from_value<T: for<'de> Deserialize<'de>>(input: &Value) -> Result<T, String> {
    serde_json::from_value(input.clone()).map_err(|error| error.to_string())
}

/// Classify bash command permission based on command type and path.
/// ROADMAP #50: Read-only commands targeting CWD paths get `WorkspaceWrite`,
/// all others remain `DangerFullAccess`.
fn classify_bash_permission(command: &str) -> PermissionMode {
    // Read-only commands that are safe when targeting workspace paths
    const READ_ONLY_COMMANDS: &[&str] = &[
        "cat", "head", "tail", "less", "more", "ls", "ll", "dir", "find", "test", "[", "[[",
        "grep", "rg", "awk", "sed", "file", "stat", "readlink", "wc", "sort", "uniq", "cut", "tr",
        "pwd", "echo", "printf",
    ];

    // Get the base command (first word before any args or pipes)
    let base_cmd = command.split_whitespace().next().unwrap_or("");
    let base_cmd = base_cmd.split('|').next().unwrap_or("").trim();
    let base_cmd = base_cmd.split(';').next().unwrap_or("").trim();
    let base_cmd = base_cmd.split('>').next().unwrap_or("").trim();
    let base_cmd = base_cmd.split('<').next().unwrap_or("").trim();

    // Check if it's a read-only command
    let cmd_name = base_cmd.split('/').next_back().unwrap_or(base_cmd);
    let is_read_only = READ_ONLY_COMMANDS.contains(&cmd_name);

    if !is_read_only {
        return PermissionMode::DangerFullAccess;
    }

    // Check if any path argument is outside workspace
    // Simple heuristic: check for absolute paths not starting with CWD
    if has_dangerous_paths(command) {
        return PermissionMode::DangerFullAccess;
    }

    PermissionMode::WorkspaceWrite
}

/// Check if command has dangerous paths (outside workspace).
fn has_dangerous_paths(command: &str) -> bool {
    // Look for absolute paths
    let tokens: Vec<&str> = command.split_whitespace().collect();

    for token in tokens {
        // Skip flags/options
        if token.starts_with('-') {
            continue;
        }

        // Check for absolute paths
        if token.starts_with('/') || token.starts_with("~/") {
            // Check if it's within CWD
            let path =
                PathBuf::from(token.replace('~', &std::env::var("HOME").unwrap_or_default()));
            if let Ok(cwd) = std::env::current_dir() {
                if !path.starts_with(&cwd) {
                    return true; // Path outside workspace
                }
            }
        }

        // Check for parent directory traversal that escapes workspace
        if token.contains("../..") || token.starts_with("../") && !token.starts_with("./") {
            return true;
        }
    }

    false
}

fn run_bash(input: BashCommandInput) -> Result<String, String> {
    if let Some(output) = workspace_test_branch_preflight(&input.command) {
        return serde_json::to_string_pretty(&output).map_err(|error| error.to_string());
    }
    serde_json::to_string_pretty(&execute_bash(input).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn workspace_test_branch_preflight(command: &str) -> Option<BashCommandOutput> {
    if !is_workspace_test_command(command) {
        return None;
    }

    let branch = git_stdout(&["branch", "--show-current"])?;
    let main_ref = resolve_main_ref(&branch)?;
    let freshness = check_freshness(&branch, &main_ref);
    match freshness {
        BranchFreshness::Fresh => None,
        BranchFreshness::Stale {
            commits_behind,
            missing_fixes,
        } => Some(branch_divergence_output(
            command,
            &branch,
            &main_ref,
            commits_behind,
            None,
            &missing_fixes,
        )),
        BranchFreshness::Diverged {
            ahead,
            behind,
            missing_fixes,
        } => Some(branch_divergence_output(
            command,
            &branch,
            &main_ref,
            behind,
            Some(ahead),
            &missing_fixes,
        )),
    }
}

fn is_workspace_test_command(command: &str) -> bool {
    let normalized = normalize_shell_command(command);
    [
        "cargo test --workspace",
        "cargo test --all",
        "cargo nextest run --workspace",
        "cargo nextest run --all",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn normalize_shell_command(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn resolve_main_ref(branch: &str) -> Option<String> {
    let has_local_main = git_ref_exists("main");
    let has_remote_main = git_ref_exists("origin/main");

    if branch == "main" && has_remote_main {
        Some("origin/main".to_string())
    } else if has_local_main {
        Some("main".to_string())
    } else if has_remote_main {
        Some("origin/main".to_string())
    } else {
        None
    }
}

fn git_ref_exists(reference: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", reference])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn git_stdout(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!stdout.is_empty()).then_some(stdout)
}

fn branch_divergence_output(
    command: &str,
    branch: &str,
    main_ref: &str,
    commits_behind: usize,
    commits_ahead: Option<usize>,
    missing_fixes: &[String],
) -> BashCommandOutput {
    let relation = commits_ahead.map_or_else(
        || format!("is {commits_behind} commit(s) behind"),
        |ahead| format!("has diverged ({ahead} ahead, {commits_behind} behind)"),
    );
    let missing_summary = if missing_fixes.is_empty() {
        "(none surfaced)".to_string()
    } else {
        missing_fixes.join("; ")
    };
    let stderr = format!(
        "branch divergence detected before workspace tests: `{branch}` {relation} `{main_ref}`. Missing commits: {missing_summary}. Merge or rebase `{main_ref}` before re-running `{command}`."
    );

    BashCommandOutput {
        stdout: String::new(),
        stderr: stderr.clone(),
        raw_output_path: None,
        interrupted: false,
        is_image: None,
        background_task_id: None,
        backgrounded_by_user: None,
        assistant_auto_backgrounded: None,
        dangerously_disable_sandbox: None,
        return_code_interpretation: Some("preflight_blocked:branch_divergence".to_string()),
        no_output_expected: Some(false),
        structured_content: Some(vec![serde_json::to_value(
            LaneEvent::new(
                LaneEventName::BranchStaleAgainstMain,
                LaneEventStatus::Blocked,
                iso8601_now(),
            )
            .with_failure_class(LaneFailureClass::BranchDivergence)
            .with_detail(stderr.clone())
            .with_data(json!({
                "branch": branch,
                "mainRef": main_ref,
                "commitsBehind": commits_behind,
                "commitsAhead": commits_ahead,
                "missingCommits": missing_fixes,
                "blockedCommand": command,
                "recommendedAction": format!("merge or rebase {main_ref} before workspace tests")
            })),
        )
        .expect("lane event should serialize")]),
        persisted_output_path: None,
        persisted_output_size: None,
        sandbox_status: None,
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_read_file(input: ReadFileInput) -> Result<String, String> {
    to_pretty_json(read_file(&input.path, input.offset, input.limit).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_write_file(input: WriteFileInput) -> Result<String, String> {
    to_pretty_json(write_file(&input.path, &input.content).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_edit_file(input: EditFileInput) -> Result<String, String> {
    to_pretty_json(
        edit_file(
            &input.path,
            &input.old_string,
            &input.new_string,
            input.replace_all.unwrap_or(false),
        )
        .map_err(io_to_string)?,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_glob_search(input: GlobSearchInputValue) -> Result<String, String> {
    to_pretty_json(glob_search(&input.pattern, input.path.as_deref()).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_grep_search(input: GrepSearchInput) -> Result<String, String> {
    to_pretty_json(grep_search(&input).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_git_diff(input: GitDiffInput) -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(io_to_string)?;
    let out = runtime::git_diff_in_workspace(
        &cwd,
        runtime::GitDiffOptions {
            cached: input.cached.unwrap_or(false),
            rev_range: input.rev_range,
            context_lines: input.context_lines,
            paths: input.paths,
        },
        256 * 1024,
    )
    .map_err(io_to_string)?;
    to_pretty_json(out)
}

#[allow(clippy::needless_pass_by_value)]
fn run_git_log(input: GitLogInput) -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(io_to_string)?;
    let out = runtime::git_log_in_workspace(
        &cwd,
        runtime::GitLogOptions {
            max_count: input.max_count,
            rev_range: input.rev_range,
            paths: input.paths,
        },
        256 * 1024,
    )
    .map_err(io_to_string)?;
    to_pretty_json(out)
}

#[allow(clippy::needless_pass_by_value)]
fn run_web_fetch(input: WebFetchInput) -> Result<String, String> {
    to_pretty_json(execute_web_fetch(&input)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_web_search(input: WebSearchInput) -> Result<String, String> {
    to_pretty_json(execute_web_search(&input)?)
}

fn run_todo_write(input: TodoWriteInput) -> Result<String, String> {
    to_pretty_json(execute_todo_write(input)?)
}

fn run_skill(input: SkillInput) -> Result<String, String> {
    to_pretty_json(execute_skill(input)?)
}

fn run_agent(input: AgentInput) -> Result<String, String> {
    to_pretty_json(execute_agent(input)?)
}

fn run_tool_search(input: ToolSearchInput) -> Result<String, String> {
    to_pretty_json(execute_tool_search(input))
}

fn run_notebook_edit(input: NotebookEditInput) -> Result<String, String> {
    to_pretty_json(execute_notebook_edit(input)?)
}

fn run_sleep(input: SleepInput) -> Result<String, String> {
    to_pretty_json(execute_sleep(input)?)
}

fn run_brief(input: BriefInput) -> Result<String, String> {
    to_pretty_json(execute_brief(input)?)
}

fn run_config(input: ConfigInput) -> Result<String, String> {
    to_pretty_json(execute_config(input)?)
}

fn run_enter_plan_mode(input: EnterPlanModeInput) -> Result<String, String> {
    to_pretty_json(execute_enter_plan_mode(input)?)
}

fn run_exit_plan_mode(input: ExitPlanModeInput) -> Result<String, String> {
    to_pretty_json(execute_exit_plan_mode(input)?)
}

fn run_structured_output(input: StructuredOutputInput) -> Result<String, String> {
    to_pretty_json(execute_structured_output(input)?)
}

fn run_repl(input: ReplInput) -> Result<String, String> {
    to_pretty_json(execute_repl(input)?)
}

/// Classify `PowerShell` command permission based on command type and path.
/// ROADMAP #50: Read-only commands targeting CWD paths get `WorkspaceWrite`,
/// all others remain `DangerFullAccess`.
fn classify_powershell_permission(command: &str) -> PermissionMode {
    // Read-only commands that are safe when targeting workspace paths
    const READ_ONLY_COMMANDS: &[&str] = &[
        "Get-Content",
        "Get-ChildItem",
        "Test-Path",
        "Get-Item",
        "Get-ItemProperty",
        "Get-FileHash",
        "Select-String",
    ];

    // Check if command starts with a read-only cmdlet
    let cmd_lower = command.trim().to_lowercase();
    let is_read_only_cmd = READ_ONLY_COMMANDS
        .iter()
        .any(|cmd| cmd_lower.starts_with(&cmd.to_lowercase()));

    if !is_read_only_cmd {
        return PermissionMode::DangerFullAccess;
    }

    // Check if the path is within workspace (CWD or subdirectory)
    // Extract path from command - look for -Path or positional parameter
    let path = extract_powershell_path(command);
    match path {
        Some(p) if is_within_workspace(&p) => PermissionMode::WorkspaceWrite,
        _ => PermissionMode::DangerFullAccess,
    }
}

/// Extract the path argument from a `PowerShell` command.
fn extract_powershell_path(command: &str) -> Option<String> {
    // Look for -Path parameter
    if let Some(idx) = command.to_lowercase().find("-path") {
        let after_path = &command[idx + 5..];
        let path = after_path.split_whitespace().next()?;
        return Some(path.trim_matches('"').trim_matches('\'').to_string());
    }

    // Look for positional path parameter (after command name)
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.len() >= 2 {
        // Skip the cmdlet name and take the first argument
        let first_arg = parts[1];
        // Check if it looks like a path (contains \, /, or .)
        if first_arg.contains(['\\', '/', '.']) {
            return Some(first_arg.trim_matches('"').trim_matches('\'').to_string());
        }
    }

    None
}

/// Check if a path is within the current workspace.
fn is_within_workspace(path: &str) -> bool {
    let path = PathBuf::from(path);

    // If path is absolute, check if it starts with CWD
    if path.is_absolute() {
        if let Ok(cwd) = std::env::current_dir() {
            return path.starts_with(&cwd);
        }
    }

    // Relative paths are assumed to be within workspace
    !path.starts_with("/") && !path.starts_with("\\") && !path.starts_with("..")
}

fn run_powershell(input: PowerShellInput) -> Result<String, String> {
    to_pretty_json(execute_powershell(input).map_err(|error| error.to_string())?)
}

fn run_disk_usage(input: &disk::DiskUsageInput) -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let report = disk::disk_usage_report(&cwd, input)?;
    to_pretty_json(report)
}

fn to_pretty_json<T: serde::Serialize>(value: T) -> Result<String, String> {
    serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn io_to_string(error: std::io::Error) -> String {
    error.to_string()
}

#[derive(Debug, Deserialize)]
struct ReadFileInput {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct WriteFileInput {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct EditFileInput {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GlobSearchInputValue {
    pattern: String,
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitDiffInput {
    cached: Option<bool>,
    rev_range: Option<String>,
    context_lines: Option<i64>,
    paths: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct GitLogInput {
    max_count: Option<u64>,
    rev_range: Option<String>,
    paths: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct WebFetchInput {
    url: String,
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct WebSearchInput {
    query: String,
    allowed_domains: Option<Vec<String>>,
    blocked_domains: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct TodoWriteInput {
    todos: Vec<TodoItem>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
struct TodoItem {
    content: String,
    #[serde(rename = "activeForm")]
    active_form: String,
    status: TodoStatus,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Deserialize)]
struct SkillInput {
    skill: String,
    args: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgentInput {
    description: String,
    prompt: String,
    subagent_type: Option<String>,
    name: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolSearchInput {
    query: String,
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct NotebookEditInput {
    notebook_path: String,
    cell_id: Option<String>,
    new_source: Option<String>,
    cell_type: Option<NotebookCellType>,
    edit_mode: Option<NotebookEditMode>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum NotebookCellType {
    Code,
    Markdown,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum NotebookEditMode {
    Replace,
    Insert,
    Delete,
}

#[derive(Debug, Deserialize)]
struct SleepInput {
    duration_ms: u64,
}

#[derive(Debug, Deserialize)]
struct BriefInput {
    message: String,
    attachments: Option<Vec<String>>,
    status: BriefStatus,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum BriefStatus {
    Normal,
    Proactive,
}

#[derive(Debug, Deserialize)]
struct ConfigInput {
    setting: String,
    value: Option<ConfigValue>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EnterPlanModeInput {}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ExitPlanModeInput {}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConfigValue {
    String(String),
    Bool(bool),
    Number(f64),
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct StructuredOutputInput(BTreeMap<String, Value>);

#[derive(Debug, Deserialize)]
struct ReplInput {
    code: String,
    language: String,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PowerShellInput {
    command: String,
    timeout: Option<u64>,
    description: Option<String>,
    run_in_background: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AskUserQuestionInput {
    question: String,
    #[serde(default)]
    options: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct TaskCreateInput {
    prompt: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskIdInput {
    task_id: String,
}

#[derive(Debug, Deserialize)]
struct TaskUpdateInput {
    task_id: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct WorkerCreateInput {
    cwd: String,
    #[serde(default)]
    trusted_roots: Vec<String>,
    #[serde(default = "default_auto_recover_prompt_misdelivery")]
    auto_recover_prompt_misdelivery: bool,
}

#[derive(Debug, Deserialize)]
struct WorkerIdInput {
    worker_id: String,
}

#[derive(Debug, Deserialize)]
struct WorkerObserveCompletionInput {
    worker_id: String,
    finish_reason: String,
    tokens_output: u64,
}

#[derive(Debug, Deserialize)]
struct WorkerObserveInput {
    worker_id: String,
    screen_text: String,
}

#[derive(Debug, Deserialize)]
struct WorkerSendPromptInput {
    worker_id: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    task_receipt: Option<WorkerTaskReceipt>,
}

const fn default_auto_recover_prompt_misdelivery() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct TeamCreateInput {
    name: String,
    tasks: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct TeamDeleteInput {
    team_id: String,
}

#[derive(Debug, Deserialize)]
struct CronCreateInput {
    schedule: String,
    prompt: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CronDeleteInput {
    cron_id: String,
}

#[derive(Debug, Deserialize)]
struct LspInput {
    action: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    character: Option<u32>,
    #[serde(default)]
    query: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpResourceInput {
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpAuthInput {
    server: String,
}

#[derive(Debug, Deserialize)]
struct RemoteTriggerInput {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Option<Value>,
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpToolInput {
    server: String,
    tool: String,
    #[serde(default)]
    arguments: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct TestingPermissionInput {
    action: String,
}

#[derive(Debug, Serialize)]
struct WebFetchOutput {
    bytes: usize,
    code: u16,
    #[serde(rename = "codeText")]
    code_text: String,
    result: String,
    #[serde(rename = "durationMs")]
    duration_ms: u128,
    url: String,
}

#[derive(Debug, Serialize)]
struct WebSearchOutput {
    query: String,
    results: Vec<WebSearchResultItem>,
    #[serde(rename = "durationSeconds")]
    duration_seconds: f64,
}

#[derive(Debug, Serialize)]
struct TodoWriteOutput {
    #[serde(rename = "oldTodos")]
    old_todos: Vec<TodoItem>,
    #[serde(rename = "newTodos")]
    new_todos: Vec<TodoItem>,
    #[serde(rename = "verificationNudgeNeeded")]
    verification_nudge_needed: Option<bool>,
}

#[derive(Debug, Serialize)]
struct SkillOutput {
    skill: String,
    path: String,
    args: Option<String>,
    description: Option<String>,
    prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentOutput {
    #[serde(rename = "agentId")]
    agent_id: String,
    name: String,
    description: String,
    #[serde(rename = "subagentType")]
    subagent_type: Option<String>,
    model: Option<String>,
    status: String,
    #[serde(rename = "outputFile")]
    output_file: String,
    #[serde(rename = "manifestFile")]
    manifest_file: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(rename = "completedAt", skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    #[serde(rename = "laneEvents", default, skip_serializing_if = "Vec::is_empty")]
    lane_events: Vec<LaneEvent>,
    #[serde(rename = "currentBlocker", skip_serializing_if = "Option::is_none")]
    current_blocker: Option<LaneEventBlocker>,
    #[serde(rename = "derivedState")]
    derived_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct AgentJob {
    manifest: AgentOutput,
    prompt: String,
    system_prompt: Vec<String>,
    allowed_tools: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolSearchOutput {
    matches: Vec<String>,
    query: String,
    normalized_query: String,
    #[serde(rename = "total_deferred_tools")]
    total_deferred_tools: usize,
    #[serde(rename = "pending_mcp_servers")]
    pending_mcp_servers: Option<Vec<String>>,
    #[serde(rename = "mcp_degraded", skip_serializing_if = "Option::is_none")]
    mcp_degraded: Option<McpDegradedReport>,
}

#[derive(Debug, Serialize)]
struct NotebookEditOutput {
    new_source: String,
    cell_id: Option<String>,
    cell_type: Option<NotebookCellType>,
    language: String,
    edit_mode: String,
    error: Option<String>,
    notebook_path: String,
    original_file: String,
    updated_file: String,
}

#[derive(Debug, Serialize)]
struct SleepOutput {
    duration_ms: u64,
    message: String,
}

#[derive(Debug, Serialize)]
struct BriefOutput {
    message: String,
    attachments: Option<Vec<ResolvedAttachment>>,
    #[serde(rename = "sentAt")]
    sent_at: String,
}

#[derive(Debug, Serialize)]
struct ResolvedAttachment {
    path: String,
    size: u64,
    #[serde(rename = "isImage")]
    is_image: bool,
}

#[derive(Debug, Serialize)]
struct ConfigOutput {
    success: bool,
    operation: Option<String>,
    setting: Option<String>,
    value: Option<Value>,
    #[serde(rename = "previousValue")]
    previous_value: Option<Value>,
    #[serde(rename = "newValue")]
    new_value: Option<Value>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanModeState {
    #[serde(rename = "hadLocalOverride")]
    had_local_override: bool,
    #[serde(rename = "previousLocalMode")]
    previous_local_mode: Option<Value>,
}

#[derive(Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct PlanModeOutput {
    success: bool,
    operation: String,
    changed: bool,
    active: bool,
    managed: bool,
    message: String,
    #[serde(rename = "settingsPath")]
    settings_path: String,
    #[serde(rename = "statePath")]
    state_path: String,
    #[serde(rename = "previousLocalMode")]
    previous_local_mode: Option<Value>,
    #[serde(rename = "currentLocalMode")]
    current_local_mode: Option<Value>,
}

#[derive(Debug, Clone)]
struct SearchableToolSpec {
    name: String,
    description: String,
}

#[derive(Debug, Serialize)]
struct StructuredOutputResult {
    data: String,
    structured_output: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
struct ReplOutput {
    language: String,
    stdout: String,
    stderr: String,
    #[serde(rename = "exitCode")]
    exit_code: i32,
    #[serde(rename = "durationMs")]
    duration_ms: u128,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum WebSearchResultItem {
    SearchResult {
        tool_use_id: String,
        content: Vec<SearchHit>,
    },
    Commentary(String),
}

#[derive(Debug, Serialize)]
struct SearchHit {
    title: String,
    url: String,
}

fn execute_web_fetch(input: &WebFetchInput) -> Result<WebFetchOutput, String> {
    let started = Instant::now();
    let client = build_http_client()?;
    let request_url = normalize_fetch_url(&input.url)?;
    let response = client
        .get(request_url.clone())
        .send()
        .map_err(|error| error.to_string())?;

    let status = response.status();
    let final_url = response.url().to_string();
    let code = status.as_u16();
    let code_text = status.canonical_reason().unwrap_or("Unknown").to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response.text().map_err(|error| error.to_string())?;
    let bytes = body.len();
    let normalized = normalize_fetched_content(&body, &content_type);
    let result = summarize_web_fetch(&final_url, &input.prompt, &normalized, &body, &content_type);

    Ok(WebFetchOutput {
        bytes,
        code,
        code_text,
        result,
        duration_ms: started.elapsed().as_millis(),
        url: final_url,
    })
}

fn execute_web_search(input: &WebSearchInput) -> Result<WebSearchOutput, String> {
    let started = Instant::now();
    let client = build_http_client()?;
    let search_url = build_search_url(&input.query)?;
    let response = client
        .get(search_url)
        .send()
        .map_err(|error| error.to_string())?;

    let final_url = response.url().clone();
    let html = response.text().map_err(|error| error.to_string())?;
    let mut hits = extract_search_hits(&html);

    if hits.is_empty() && final_url.host_str().is_some() {
        hits = extract_search_hits_from_generic_links(&html);
    }

    if let Some(allowed) = input.allowed_domains.as_ref() {
        hits.retain(|hit| host_matches_list(&hit.url, allowed));
    }
    if let Some(blocked) = input.blocked_domains.as_ref() {
        hits.retain(|hit| !host_matches_list(&hit.url, blocked));
    }

    dedupe_hits(&mut hits);
    hits.truncate(8);

    let summary = if hits.is_empty() {
        format!("No web search results matched the query {:?}.", input.query)
    } else {
        let rendered_hits = hits
            .iter()
            .map(|hit| format!("- [{}]({})", hit.title, hit.url))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Search results for {:?}. Include a Sources section in the final answer.\n{}",
            input.query, rendered_hits
        )
    };

    Ok(WebSearchOutput {
        query: input.query.clone(),
        results: vec![
            WebSearchResultItem::Commentary(summary),
            WebSearchResultItem::SearchResult {
                tool_use_id: String::from("web_search_1"),
                content: hits,
            },
        ],
        duration_seconds: started.elapsed().as_secs_f64(),
    })
}

fn build_http_client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("clawd-rust-tools/0.1")
        .build()
        .map_err(|error| error.to_string())
}

fn normalize_fetch_url(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    if parsed.scheme() == "http" {
        let host = parsed.host_str().unwrap_or_default();
        if host != "localhost" && host != "127.0.0.1" && host != "::1" {
            let mut upgraded = parsed;
            upgraded
                .set_scheme("https")
                .map_err(|()| String::from("failed to upgrade URL to https"))?;
            return Ok(upgraded.to_string());
        }
    }
    Ok(parsed.to_string())
}

fn build_search_url(query: &str) -> Result<reqwest::Url, String> {
    if let Ok(base) = std::env::var("CLAWD_WEB_SEARCH_BASE_URL") {
        let mut url = reqwest::Url::parse(&base).map_err(|error| error.to_string())?;
        url.query_pairs_mut().append_pair("q", query);
        return Ok(url);
    }

    let mut url = reqwest::Url::parse("https://html.duckduckgo.com/html/")
        .map_err(|error| error.to_string())?;
    url.query_pairs_mut().append_pair("q", query);
    Ok(url)
}

fn normalize_fetched_content(body: &str, content_type: &str) -> String {
    if content_type.contains("html") {
        html_to_text(body)
    } else {
        body.trim().to_string()
    }
}

fn summarize_web_fetch(
    url: &str,
    prompt: &str,
    content: &str,
    raw_body: &str,
    content_type: &str,
) -> String {
    let lower_prompt = prompt.to_lowercase();
    let compact = collapse_whitespace(content);

    let detail = if lower_prompt.contains("title") {
        extract_title(content, raw_body, content_type).map_or_else(
            || preview_text(&compact, 600),
            |title| format!("Title: {title}"),
        )
    } else if lower_prompt.contains("summary") || lower_prompt.contains("summarize") {
        preview_text(&compact, 900)
    } else {
        let preview = preview_text(&compact, 900);
        format!("Prompt: {prompt}\nContent preview:\n{preview}")
    };

    format!("Fetched {url}\n{detail}")
}

fn extract_title(content: &str, raw_body: &str, content_type: &str) -> Option<String> {
    if content_type.contains("html") {
        let lowered = raw_body.to_lowercase();
        if let Some(start) = lowered.find("<title>") {
            let after = start + "<title>".len();
            if let Some(end_rel) = lowered[after..].find("</title>") {
                let title =
                    collapse_whitespace(&decode_html_entities(&raw_body[after..after + end_rel]));
                if !title.is_empty() {
                    return Some(title);
                }
            }
        }
    }

    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut previous_was_space = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            '&' => {
                text.push('&');
                previous_was_space = false;
            }
            ch if ch.is_whitespace() => {
                if !previous_was_space {
                    text.push(' ');
                    previous_was_space = true;
                }
            }
            _ => {
                text.push(ch);
                previous_was_space = false;
            }
        }
    }

    collapse_whitespace(&decode_html_entities(&text))
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn preview_text(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let shortened = input.chars().take(max_chars).collect::<String>();
    format!("{}…", shortened.trim_end())
}

fn extract_search_hits(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut remaining = html;

    while let Some(anchor_start) = remaining.find("result__a") {
        let after_class = &remaining[anchor_start..];
        let Some(href_idx) = after_class.find("href=") else {
            remaining = &after_class[1..];
            continue;
        };
        let href_slice = &after_class[href_idx + 5..];
        let Some((url, rest)) = extract_quoted_value(href_slice) else {
            remaining = &after_class[1..];
            continue;
        };
        let Some(close_tag_idx) = rest.find('>') else {
            remaining = &after_class[1..];
            continue;
        };
        let after_tag = &rest[close_tag_idx + 1..];
        let Some(end_anchor_idx) = after_tag.find("</a>") else {
            remaining = &after_tag[1..];
            continue;
        };
        let title = html_to_text(&after_tag[..end_anchor_idx]);
        if let Some(decoded_url) = decode_duckduckgo_redirect(&url) {
            hits.push(SearchHit {
                title: title.trim().to_string(),
                url: decoded_url,
            });
        }
        remaining = &after_tag[end_anchor_idx + 4..];
    }

    hits
}

fn extract_search_hits_from_generic_links(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut remaining = html;

    while let Some(anchor_start) = remaining.find("<a") {
        let after_anchor = &remaining[anchor_start..];
        let Some(href_idx) = after_anchor.find("href=") else {
            remaining = &after_anchor[2..];
            continue;
        };
        let href_slice = &after_anchor[href_idx + 5..];
        let Some((url, rest)) = extract_quoted_value(href_slice) else {
            remaining = &after_anchor[2..];
            continue;
        };
        let Some(close_tag_idx) = rest.find('>') else {
            remaining = &after_anchor[2..];
            continue;
        };
        let after_tag = &rest[close_tag_idx + 1..];
        let Some(end_anchor_idx) = after_tag.find("</a>") else {
            remaining = &after_anchor[2..];
            continue;
        };
        let title = html_to_text(&after_tag[..end_anchor_idx]);
        if title.trim().is_empty() {
            remaining = &after_tag[end_anchor_idx + 4..];
            continue;
        }
        let decoded_url = decode_duckduckgo_redirect(&url).unwrap_or(url);
        if decoded_url.starts_with("http://") || decoded_url.starts_with("https://") {
            hits.push(SearchHit {
                title: title.trim().to_string(),
                url: decoded_url,
            });
        }
        remaining = &after_tag[end_anchor_idx + 4..];
    }

    hits
}

fn extract_quoted_value(input: &str) -> Option<(String, &str)> {
    let quote = input.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &input[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some((rest[..end].to_string(), &rest[end + quote.len_utf8()..]))
}

fn decode_duckduckgo_redirect(url: &str) -> Option<String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Some(html_entity_decode_url(url));
    }

    let joined = if url.starts_with("//") {
        format!("https:{url}")
    } else if url.starts_with('/') {
        format!("https://duckduckgo.com{url}")
    } else {
        return None;
    };

    let parsed = reqwest::Url::parse(&joined).ok()?;
    if parsed.path() == "/l/" || parsed.path() == "/l" {
        for (key, value) in parsed.query_pairs() {
            if key == "uddg" {
                return Some(html_entity_decode_url(value.as_ref()));
            }
        }
    }
    Some(joined)
}

fn html_entity_decode_url(url: &str) -> String {
    decode_html_entities(url)
}

fn host_matches_list(url: &str, domains: &[String]) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    domains.iter().any(|domain| {
        let normalized = normalize_domain_filter(domain);
        !normalized.is_empty() && (host == normalized || host.ends_with(&format!(".{normalized}")))
    })
}

fn normalize_domain_filter(domain: &str) -> String {
    let trimmed = domain.trim();
    let candidate = reqwest::Url::parse(trimmed)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| trimmed.to_string());
    candidate
        .trim()
        .trim_start_matches('.')
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn dedupe_hits(hits: &mut Vec<SearchHit>) {
    let mut seen = BTreeSet::new();
    hits.retain(|hit| seen.insert(hit.url.clone()));
}

fn execute_todo_write(input: TodoWriteInput) -> Result<TodoWriteOutput, String> {
    validate_todos(&input.todos)?;
    let store_path = todo_store_path()?;
    let old_todos = if store_path.exists() {
        serde_json::from_str::<Vec<TodoItem>>(
            &std::fs::read_to_string(&store_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?
    } else {
        Vec::new()
    };

    let all_done = input
        .todos
        .iter()
        .all(|todo| matches!(todo.status, TodoStatus::Completed));
    let persisted = if all_done {
        Vec::new()
    } else {
        input.todos.clone()
    };

    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        &store_path,
        serde_json::to_string_pretty(&persisted).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let verification_nudge_needed = (all_done
        && input.todos.len() >= 3
        && !input
            .todos
            .iter()
            .any(|todo| todo.content.to_lowercase().contains("verif")))
    .then_some(true);

    Ok(TodoWriteOutput {
        old_todos,
        new_todos: input.todos,
        verification_nudge_needed,
    })
}

fn execute_skill(input: SkillInput) -> Result<SkillOutput, String> {
    let skill_path = resolve_skill_path(&input.skill)?;
    let prompt = std::fs::read_to_string(&skill_path).map_err(|error| error.to_string())?;
    let description = parse_skill_description(&prompt);

    Ok(SkillOutput {
        skill: input.skill,
        path: skill_path.display().to_string(),
        args: input.args,
        description,
        prompt,
    })
}

fn validate_todos(todos: &[TodoItem]) -> Result<(), String> {
    if todos.is_empty() {
        return Err(String::from("todos must not be empty"));
    }
    // Allow multiple in_progress items for parallel workflows
    if todos.iter().any(|todo| todo.content.trim().is_empty()) {
        return Err(String::from("todo content must not be empty"));
    }
    if todos.iter().any(|todo| todo.active_form.trim().is_empty()) {
        return Err(String::from("todo activeForm must not be empty"));
    }
    Ok(())
}

fn todo_store_path() -> Result<std::path::PathBuf, String> {
    if let Ok(path) = std::env::var("CLAWD_TODO_STORE") {
        return Ok(std::path::PathBuf::from(path));
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    Ok(cwd.join(".clawd-todos.json"))
}

fn resolve_skill_path(skill: &str) -> Result<std::path::PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    match commands::resolve_skill_path(&cwd, skill) {
        Ok(path) => Ok(path),
        Err(_) => resolve_skill_path_from_compat_roots(skill),
    }
}

fn resolve_skill_path_from_compat_roots(skill: &str) -> Result<std::path::PathBuf, String> {
    let requested = skill.trim().trim_start_matches('/').trim_start_matches('$');
    if requested.is_empty() {
        return Err(String::from("skill must not be empty"));
    }

    for root in skill_lookup_roots() {
        if let Some(path) = resolve_skill_path_in_root(&root, requested) {
            return Ok(path);
        }
    }

    Err(format!("unknown skill: {requested}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillLookupOrigin {
    SkillsDir,
    LegacyCommandsDir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillLookupRoot {
    path: std::path::PathBuf,
    origin: SkillLookupOrigin,
}

fn skill_lookup_roots() -> Vec<SkillLookupRoot> {
    let mut roots = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        push_project_skill_lookup_roots(&mut roots, &cwd);
    }

    if let Ok(claw_config_home) = std::env::var("CLAW_CONFIG_HOME") {
        push_prefixed_skill_lookup_roots(&mut roots, std::path::Path::new(&claw_config_home));
    }
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        push_prefixed_skill_lookup_roots(&mut roots, std::path::Path::new(&codex_home));
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        push_home_skill_lookup_roots(&mut roots, std::path::Path::new(&home));
    }
    if let Ok(claude_config_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let claude_config_dir = std::path::PathBuf::from(claude_config_dir);
        push_skill_lookup_root(
            &mut roots,
            claude_config_dir.join("skills"),
            SkillLookupOrigin::SkillsDir,
        );
        push_skill_lookup_root(
            &mut roots,
            claude_config_dir.join("skills").join("omc-learned"),
            SkillLookupOrigin::SkillsDir,
        );
        push_skill_lookup_root(
            &mut roots,
            claude_config_dir.join("commands"),
            SkillLookupOrigin::LegacyCommandsDir,
        );
    }
    push_skill_lookup_root(
        &mut roots,
        std::path::PathBuf::from("/home/bellman/.claw/skills"),
        SkillLookupOrigin::SkillsDir,
    );
    push_skill_lookup_root(
        &mut roots,
        std::path::PathBuf::from("/home/bellman/.codex/skills"),
        SkillLookupOrigin::SkillsDir,
    );

    roots
}

fn push_project_skill_lookup_roots(roots: &mut Vec<SkillLookupRoot>, cwd: &std::path::Path) {
    for ancestor in cwd.ancestors() {
        push_prefixed_skill_lookup_roots(roots, &ancestor.join(".omc"));
        push_prefixed_skill_lookup_roots(roots, &ancestor.join(".agents"));
        push_prefixed_skill_lookup_roots(roots, &ancestor.join(".claw"));
        push_prefixed_skill_lookup_roots(roots, &ancestor.join(".codex"));
        push_prefixed_skill_lookup_roots(roots, &ancestor.join(".claude"));
    }
}

fn push_home_skill_lookup_roots(roots: &mut Vec<SkillLookupRoot>, home: &std::path::Path) {
    push_prefixed_skill_lookup_roots(roots, &home.join(".omc"));
    push_prefixed_skill_lookup_roots(roots, &home.join(".claw"));
    push_prefixed_skill_lookup_roots(roots, &home.join(".codex"));
    push_prefixed_skill_lookup_roots(roots, &home.join(".claude"));
    push_skill_lookup_root(
        roots,
        home.join(".agents").join("skills"),
        SkillLookupOrigin::SkillsDir,
    );
    push_skill_lookup_root(
        roots,
        home.join(".config").join("opencode").join("skills"),
        SkillLookupOrigin::SkillsDir,
    );
    push_skill_lookup_root(
        roots,
        home.join(".claude").join("skills").join("omc-learned"),
        SkillLookupOrigin::SkillsDir,
    );
}

fn push_prefixed_skill_lookup_roots(roots: &mut Vec<SkillLookupRoot>, prefix: &std::path::Path) {
    push_skill_lookup_root(roots, prefix.join("skills"), SkillLookupOrigin::SkillsDir);
    push_skill_lookup_root(
        roots,
        prefix.join("commands"),
        SkillLookupOrigin::LegacyCommandsDir,
    );
}

fn push_skill_lookup_root(
    roots: &mut Vec<SkillLookupRoot>,
    path: std::path::PathBuf,
    origin: SkillLookupOrigin,
) {
    if path.is_dir() && !roots.iter().any(|existing| existing.path == path) {
        roots.push(SkillLookupRoot { path, origin });
    }
}

fn resolve_skill_path_in_root(
    root: &SkillLookupRoot,
    requested: &str,
) -> Option<std::path::PathBuf> {
    match root.origin {
        SkillLookupOrigin::SkillsDir => resolve_skill_path_in_skills_dir(&root.path, requested),
        SkillLookupOrigin::LegacyCommandsDir => {
            resolve_skill_path_in_legacy_commands_dir(&root.path, requested)
        }
    }
}

fn resolve_skill_path_in_skills_dir(
    root: &std::path::Path,
    requested: &str,
) -> Option<std::path::PathBuf> {
    let direct = root.join(requested).join("SKILL.md");
    if direct.is_file() {
        return Some(direct);
    }

    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let skill_path = entry.path().join("SKILL.md");
        if !skill_path.is_file() {
            continue;
        }
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(requested)
            || skill_frontmatter_name_matches(&skill_path, requested)
        {
            return Some(skill_path);
        }
    }

    None
}

fn resolve_skill_path_in_legacy_commands_dir(
    root: &std::path::Path,
    requested: &str,
) -> Option<std::path::PathBuf> {
    let direct_dir = root.join(requested).join("SKILL.md");
    if direct_dir.is_file() {
        return Some(direct_dir);
    }

    let direct_markdown = root.join(format!("{requested}.md"));
    if direct_markdown.is_file() {
        return Some(direct_markdown);
    }

    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let candidate_path = if path.is_dir() {
            let skill_path = path.join("SKILL.md");
            if !skill_path.is_file() {
                continue;
            }
            skill_path
        } else if path
            .extension()
            .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("md"))
        {
            path
        } else {
            continue;
        };

        let matches_entry_name = candidate_path
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case(requested))
            || entry
                .file_name()
                .to_string_lossy()
                .trim_end_matches(".md")
                .eq_ignore_ascii_case(requested);
        if matches_entry_name || skill_frontmatter_name_matches(&candidate_path, requested) {
            return Some(candidate_path);
        }
    }

    None
}

fn skill_frontmatter_name_matches(path: &std::path::Path, requested: &str) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| parse_skill_name(&contents))
        .is_some_and(|name| name.eq_ignore_ascii_case(requested))
}

fn parse_skill_name(contents: &str) -> Option<String> {
    parse_skill_frontmatter_value(contents, "name")
}

fn parse_skill_frontmatter_value(contents: &str, key: &str) -> Option<String> {
    let mut lines = contents.lines();
    if lines.next().map(str::trim) != Some("---") {
        return None;
    }

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(value) = trimmed.strip_prefix(&format!("{key}:")) {
            let value = value
                .trim()
                .trim_matches(|ch| matches!(ch, '"' | '\''))
                .trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    None
}

const DEFAULT_AGENT_MODEL: &str = "claude-opus-4-6";
const DEFAULT_AGENT_SYSTEM_DATE: &str = "2026-03-31";
const DEFAULT_AGENT_MAX_ITERATIONS: usize = 32;

fn execute_agent(input: AgentInput) -> Result<AgentOutput, String> {
    execute_agent_with_spawn(input, spawn_agent_job)
}

fn execute_agent_with_spawn<F>(input: AgentInput, spawn_fn: F) -> Result<AgentOutput, String>
where
    F: FnOnce(AgentJob) -> Result<(), String>,
{
    if input.description.trim().is_empty() {
        return Err(String::from("description must not be empty"));
    }
    if input.prompt.trim().is_empty() {
        return Err(String::from("prompt must not be empty"));
    }

    let agent_id = make_agent_id();
    let output_dir = agent_store_dir()?;
    std::fs::create_dir_all(&output_dir).map_err(|error| error.to_string())?;
    let output_file = output_dir.join(format!("{agent_id}.md"));
    let manifest_file = output_dir.join(format!("{agent_id}.json"));
    let normalized_subagent_type = normalize_subagent_type(input.subagent_type.as_deref());
    let model = resolve_agent_model(input.model.as_deref());
    let agent_name = input
        .name
        .as_deref()
        .map(slugify_agent_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| slugify_agent_name(&input.description));
    let created_at = iso8601_now();
    let system_prompt = build_agent_system_prompt(&normalized_subagent_type)?;
    let allowed_tools = allowed_tools_for_subagent(&normalized_subagent_type);

    let output_contents = format!(
        "# Agent Task

- id: {}
- name: {}
- description: {}
- subagent_type: {}
- created_at: {}

## Prompt

{}
",
        agent_id, agent_name, input.description, normalized_subagent_type, created_at, input.prompt
    );
    std::fs::write(&output_file, output_contents).map_err(|error| error.to_string())?;

    let manifest = AgentOutput {
        agent_id,
        name: agent_name,
        description: input.description,
        subagent_type: Some(normalized_subagent_type),
        model: Some(model),
        status: String::from("running"),
        output_file: output_file.display().to_string(),
        manifest_file: manifest_file.display().to_string(),
        created_at: created_at.clone(),
        started_at: Some(created_at),
        completed_at: None,
        lane_events: vec![LaneEvent::started(iso8601_now())],
        current_blocker: None,
        derived_state: String::from("working"),
        error: None,
    };
    write_agent_manifest(&manifest)?;

    let manifest_for_spawn = manifest.clone();
    let job = AgentJob {
        manifest: manifest_for_spawn,
        prompt: input.prompt,
        system_prompt,
        allowed_tools,
    };
    if let Err(error) = spawn_fn(job) {
        let error = format!("failed to spawn sub-agent: {error}");
        persist_agent_terminal_state(&manifest, "failed", None, Some(error.clone()))?;
        return Err(error);
    }

    Ok(manifest)
}

fn spawn_agent_job(job: AgentJob) -> Result<(), String> {
    let thread_name = format!("clawd-agent-{}", job.manifest.agent_id);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_agent_job(&job)));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let _ =
                        persist_agent_terminal_state(&job.manifest, "failed", None, Some(error));
                }
                Err(_) => {
                    let _ = persist_agent_terminal_state(
                        &job.manifest,
                        "failed",
                        None,
                        Some(String::from("sub-agent thread panicked")),
                    );
                }
            }
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn run_agent_job(job: &AgentJob) -> Result<(), String> {
    let mut runtime = build_agent_runtime(job)?.with_max_iterations(DEFAULT_AGENT_MAX_ITERATIONS);
    let summary = runtime
        .run_turn(job.prompt.clone(), None)
        .map_err(|error| error.to_string())?;
    let final_text = final_assistant_text(&summary);
    persist_agent_terminal_state(&job.manifest, "completed", Some(final_text.as_str()), None)
}

fn build_agent_runtime(
    job: &AgentJob,
) -> Result<ConversationRuntime<ProviderRuntimeClient, SubagentToolExecutor>, String> {
    let model = job
        .manifest
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_AGENT_MODEL.to_string());
    let allowed_tools = job.allowed_tools.clone();
    let api_client = ProviderRuntimeClient::new(model, allowed_tools.clone())?;
    let permission_policy = agent_permission_policy();
    let tool_executor = SubagentToolExecutor::new(allowed_tools)
        .with_enforcer(PermissionEnforcer::new(permission_policy.clone()));
    Ok(ConversationRuntime::new(
        Session::new(),
        api_client,
        tool_executor,
        permission_policy,
        job.system_prompt.clone(),
    ))
}

fn build_agent_system_prompt(subagent_type: &str) -> Result<Vec<String>, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut prompt = load_system_prompt(
        cwd,
        DEFAULT_AGENT_SYSTEM_DATE.to_string(),
        std::env::consts::OS,
        "unknown",
    )
    .map_err(|error| error.to_string())?;
    prompt.push(format!(
        "You are a background sub-agent of type `{subagent_type}`. Work only on the delegated task, use only the tools available to you, do not ask the user questions, and finish with a concise result."
    ));
    Ok(prompt)
}

fn resolve_agent_model(model: Option<&str>) -> String {
    model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(DEFAULT_AGENT_MODEL)
        .to_string()
}

fn allowed_tools_for_subagent(subagent_type: &str) -> BTreeSet<String> {
    let tools = match subagent_type {
        "Explore" => vec![
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "Skill",
            "StructuredOutput",
        ],
        "Plan" => vec![
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "Skill",
            "TodoWrite",
            "StructuredOutput",
            "SendUserMessage",
        ],
        "Verification" => vec![
            "bash",
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "TodoWrite",
            "StructuredOutput",
            "SendUserMessage",
            "PowerShell",
        ],
        "claw-guide" => vec![
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "Skill",
            "StructuredOutput",
            "SendUserMessage",
        ],
        "statusline-setup" => vec![
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "glob_search",
            "grep_search",
            "ToolSearch",
        ],
        _ => vec![
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "TodoWrite",
            "Skill",
            "ToolSearch",
            "NotebookEdit",
            "Sleep",
            "SendUserMessage",
            "Config",
            "StructuredOutput",
            "REPL",
            "PowerShell",
        ],
    };
    tools.into_iter().map(str::to_string).collect()
}

fn agent_permission_policy() -> PermissionPolicy {
    mvp_tool_specs().into_iter().fold(
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        |policy, spec| policy.with_tool_requirement(spec.name, spec.required_permission),
    )
}

fn write_agent_manifest(manifest: &AgentOutput) -> Result<(), String> {
    let mut normalized = manifest.clone();
    normalized.lane_events = dedupe_superseded_commit_events(&normalized.lane_events);
    std::fs::write(
        &normalized.manifest_file,
        serde_json::to_string_pretty(&normalized).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn persist_agent_terminal_state(
    manifest: &AgentOutput,
    status: &str,
    result: Option<&str>,
    error: Option<String>,
) -> Result<(), String> {
    let blocker = error.as_deref().map(classify_lane_blocker);
    append_agent_output(
        &manifest.output_file,
        &format_agent_terminal_output(status, result, blocker.as_ref(), error.as_deref()),
    )?;
    let mut next_manifest = manifest.clone();
    next_manifest.status = status.to_string();
    next_manifest.completed_at = Some(iso8601_now());
    next_manifest.current_blocker.clone_from(&blocker);
    next_manifest.derived_state =
        derive_agent_state(status, result, error.as_deref(), blocker.as_ref()).to_string();
    next_manifest.error = error;
    if let Some(blocker) = blocker {
        next_manifest
            .lane_events
            .push(LaneEvent::blocked(iso8601_now(), &blocker));
        next_manifest
            .lane_events
            .push(LaneEvent::failed(iso8601_now(), &blocker));
    } else {
        next_manifest.current_blocker = None;
        let mut finished_summary = build_lane_finished_summary(&next_manifest, result);
        finished_summary.data.disabled_cron_ids = disable_matching_crons(&next_manifest, result);
        next_manifest.lane_events.push(
            LaneEvent::finished(iso8601_now(), finished_summary.detail).with_data(
                serde_json::to_value(&finished_summary.data)
                    .expect("lane summary metadata should serialize"),
            ),
        );
        if let Some(provenance) = maybe_commit_provenance(result) {
            next_manifest.lane_events.push(LaneEvent::commit_created(
                iso8601_now(),
                Some(format!("commit {}", provenance.commit)),
                provenance,
            ));
        }
    }
    write_agent_manifest(&next_manifest)
}

const MIN_LANE_SUMMARY_WORDS: usize = 7;
const REVIEW_VERDICTS: &[(&str, &str)] = &[
    ("APPROVE", "approve"),
    ("REJECT", "reject"),
    ("BLOCKED", "blocked"),
];
const CONTROL_ONLY_SUMMARY_WORDS: &[&str] = &[
    "ack",
    "commit",
    "continue",
    "everyting",
    "everything",
    "keep",
    "next",
    "push",
    "ralph",
    "resume",
    "retry",
    "run",
    "stop",
    "sweep",
    "sweeping",
    "team",
];
const CONTEXTUAL_SUMMARY_WORDS: &[&str] = &[
    "added",
    "audited",
    "blocked",
    "completed",
    "documented",
    "failed",
    "finished",
    "fixed",
    "implemented",
    "investigated",
    "merged",
    "pushed",
    "refactored",
    "removed",
    "reviewed",
    "tested",
    "updated",
    "verified",
];

#[derive(Debug, Clone, Serialize)]
struct LaneFinishedSummaryData {
    #[serde(rename = "qualityFloorApplied")]
    quality_floor_applied: bool,
    reasons: Vec<String>,
    #[serde(rename = "rawSummary", skip_serializing_if = "Option::is_none")]
    raw_summary: Option<String>,
    #[serde(rename = "wordCount")]
    word_count: usize,
    #[serde(rename = "reviewVerdict", skip_serializing_if = "Option::is_none")]
    review_verdict: Option<String>,
    #[serde(rename = "reviewTarget", skip_serializing_if = "Option::is_none")]
    review_target: Option<String>,
    #[serde(rename = "reviewRationale", skip_serializing_if = "Option::is_none")]
    review_rationale: Option<String>,
    #[serde(rename = "selectionOutcome", skip_serializing_if = "Option::is_none")]
    selection_outcome: Option<SelectionOutcome>,
    #[serde(rename = "recoveryOutcome", skip_serializing_if = "Option::is_none")]
    recovery_outcome: Option<RecoveryOutcome>,
    #[serde(rename = "artifactProvenance", skip_serializing_if = "Option::is_none")]
    artifact_provenance: Option<ArtifactProvenance>,
    #[serde(rename = "disabledCronIds", skip_serializing_if = "Vec::is_empty")]
    disabled_cron_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct LaneFinishedSummary {
    detail: Option<String>,
    data: LaneFinishedSummaryData,
}

#[derive(Debug)]
struct LaneSummaryAssessment {
    apply_quality_floor: bool,
    reasons: Vec<String>,
    word_count: usize,
    review_outcome: Option<ReviewLaneOutcome>,
    recovery_outcome: Option<RecoveryOutcome>,
}

#[derive(Debug, Clone)]
struct ReviewLaneOutcome {
    verdict: String,
    rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SelectionOutcome {
    #[serde(rename = "chosenItems", skip_serializing_if = "Vec::is_empty")]
    chosen_items: Vec<String>,
    #[serde(rename = "skippedItems", skip_serializing_if = "Vec::is_empty")]
    skipped_items: Vec<String>,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RecoveryOutcome {
    cause: String,
    #[serde(rename = "targetLane", skip_serializing_if = "Option::is_none")]
    target_lane: Option<String>,
    #[serde(rename = "preservedState", skip_serializing_if = "Option::is_none")]
    preserved_state: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ArtifactProvenance {
    #[serde(rename = "sourceLanes", skip_serializing_if = "Vec::is_empty")]
    source_lanes: Vec<String>,
    #[serde(rename = "roadmapIds", skip_serializing_if = "Vec::is_empty")]
    roadmap_ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<String>,
    #[serde(rename = "diffStat", skip_serializing_if = "Option::is_none")]
    diff_stat: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    verification: Vec<String>,
    #[serde(rename = "commitSha", skip_serializing_if = "Option::is_none")]
    commit_sha: Option<String>,
}

fn build_lane_finished_summary(
    manifest: &AgentOutput,
    result: Option<&str>,
) -> LaneFinishedSummary {
    let raw_summary = result.map(str::trim).filter(|value| !value.is_empty());
    let assessment = assess_lane_summary_quality(raw_summary.unwrap_or_default());
    let detail = match raw_summary {
        Some(summary) if !assessment.apply_quality_floor => Some(compress_summary_text(summary)),
        Some(summary) => Some(compose_lane_summary_fallback(
            manifest,
            Some(summary),
            assessment.recovery_outcome.as_ref(),
        )),
        None => Some(compose_lane_summary_fallback(manifest, None, None)),
    };
    let review_outcome = assessment.review_outcome.clone();
    let recovery_outcome = assessment.recovery_outcome.clone();
    let review_target = review_outcome
        .as_ref()
        .map(|_| manifest.description.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let artifact_provenance = extract_artifact_provenance(manifest, raw_summary);

    LaneFinishedSummary {
        detail,
        data: LaneFinishedSummaryData {
            quality_floor_applied: raw_summary.is_none() || assessment.apply_quality_floor,
            reasons: assessment.reasons,
            raw_summary: raw_summary.map(str::to_string),
            word_count: assessment.word_count,
            review_verdict: review_outcome
                .as_ref()
                .map(|outcome| outcome.verdict.clone()),
            review_target,
            review_rationale: review_outcome.and_then(|outcome| outcome.rationale),
            selection_outcome: extract_selection_outcome(raw_summary.unwrap_or_default()),
            recovery_outcome,
            artifact_provenance,
            disabled_cron_ids: Vec::new(),
        },
    }
}

fn assess_lane_summary_quality(summary: &str) -> LaneSummaryAssessment {
    let words = summary
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '#'))
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();

    let word_count = words.len();
    let mut reasons = Vec::new();
    if summary.trim().is_empty() {
        reasons.push(String::from("empty"));
    }

    let review_outcome = extract_review_outcome(summary);
    let recovery_outcome = extract_recovery_outcome(summary);
    if recovery_outcome.is_some() {
        reasons.push(String::from("recovery_control_prose"));
    }

    let control_only = !words.is_empty()
        && words
            .iter()
            .all(|word| CONTROL_ONLY_SUMMARY_WORDS.contains(&word.as_str()));
    if control_only && review_outcome.is_none() {
        reasons.push(String::from("control_only"));
    }

    let has_context_signal = summary.contains('`')
        || summary.contains('/')
        || summary.contains(':')
        || summary.contains('#')
        || review_outcome.is_some()
        || words
            .iter()
            .any(|word| CONTEXTUAL_SUMMARY_WORDS.contains(&word.as_str()));
    if word_count < MIN_LANE_SUMMARY_WORDS && !has_context_signal {
        reasons.push(String::from("too_short_without_context"));
    }

    LaneSummaryAssessment {
        apply_quality_floor: !reasons.is_empty(),
        reasons,
        word_count,
        review_outcome,
        recovery_outcome,
    }
}

fn compose_lane_summary_fallback(
    manifest: &AgentOutput,
    raw_summary: Option<&str>,
    recovery_outcome: Option<&RecoveryOutcome>,
) -> String {
    let target = manifest.description.trim();
    let base = format!(
        "Completed lane `{}` for target: {}. Status: completed.",
        manifest.name,
        if target.is_empty() {
            "unspecified task"
        } else {
            target
        }
    );
    if let Some(outcome) = recovery_outcome {
        let mut detail = format!(
            "{base} Recovery handoff observed via tmux reinjection (cause: `{}`).",
            outcome.cause
        );
        if let Some(target_lane) = &outcome.target_lane {
            let _ = std::fmt::Write::write_fmt(
                &mut detail,
                format_args!(" Target lane: `{target_lane}`."),
            );
        }
        if let Some(preserved_state) = &outcome.preserved_state {
            let _ = std::fmt::Write::write_fmt(
                &mut detail,
                format_args!(" Preserved state: {preserved_state}."),
            );
        }
        return detail;
    }
    match raw_summary {
        Some(summary) => format!(
            "{base} Original stop summary was too vague to keep as the lane result: \"{}\".",
            summary.trim()
        ),
        None => format!("{base} No usable stop summary was produced by the lane."),
    }
}

fn extract_review_outcome(summary: &str) -> Option<ReviewLaneOutcome> {
    let mut lines = summary
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = lines.next()?;
    let verdict = REVIEW_VERDICTS.iter().find_map(|(prefix, verdict)| {
        first
            .eq_ignore_ascii_case(prefix)
            .then(|| (*verdict).to_string())
    })?;
    let rationale = lines.collect::<Vec<_>>().join(" ").trim().to_string();
    Some(ReviewLaneOutcome {
        verdict,
        rationale: (!rationale.is_empty()).then_some(compress_summary_text(&rationale)),
    })
}

fn extract_selection_outcome(summary: &str) -> Option<SelectionOutcome> {
    let mut chosen_items = Vec::new();
    let mut skipped_items = Vec::new();
    let mut action = None;
    let mut rationale = None;

    for line in summary
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let lowered = line.to_ascii_lowercase();
        let roadmap_items = extract_roadmap_items(line);

        if lowered.starts_with("chosen:")
            || lowered.starts_with("picked:")
            || lowered.starts_with("selected:")
            || (lowered.contains("picked") && !roadmap_items.is_empty())
            || (lowered.contains("selected") && !roadmap_items.is_empty())
        {
            chosen_items.extend(roadmap_items);
        } else if lowered.starts_with("skipped:")
            || lowered.starts_with("skip:")
            || (lowered.contains("skipped") && !roadmap_items.is_empty())
        {
            skipped_items.extend(roadmap_items);
        }

        if let Some(rest) = lowered.strip_prefix("action:") {
            if rest.contains("execute") || rest.contains("implement") || rest.contains("fix") {
                action = Some(String::from("execute"));
            } else if rest.contains("review") || rest.contains("audit") {
                action = Some(String::from("review"));
            } else if rest.contains("no-op") || rest.contains("noop") {
                action = Some(String::from("no-op"));
            }
        }

        if let Some(rest) = line.strip_prefix("Rationale:") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                rationale = Some(compress_summary_text(trimmed));
            }
        }
    }

    chosen_items.sort();
    chosen_items.dedup();
    skipped_items.sort();
    skipped_items.dedup();

    if chosen_items.is_empty() && skipped_items.is_empty() && action.is_none() {
        return None;
    }

    let default_action = if chosen_items.is_empty() {
        String::from("no-op")
    } else {
        String::from("execute")
    };

    Some(SelectionOutcome {
        chosen_items,
        skipped_items,
        action: action.unwrap_or(default_action),
        rationale,
    })
}

fn extract_recovery_outcome(summary: &str) -> Option<RecoveryOutcome> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lowered = trimmed.to_ascii_lowercase();
    let has_tmux_inject_marker = lowered.contains("omx_tmux_inject");
    let has_recovery_phrase = lowered.contains("continue from current mode state")
        || (lowered.starts_with("team ") && lowered.contains(" next:"));
    if !has_tmux_inject_marker && !has_recovery_phrase {
        return None;
    }

    let cause = if lowered.contains("current mode state") {
        "resume_after_stop"
    } else if lowered.contains("tool failure") {
        "retry_after_tool_failure"
    } else if lowered.contains("worker panes stalled")
        || lowered.contains("no progress")
        || lowered.contains("leader stale")
        || lowered.contains("all workers idle")
        || lowered.contains("all 1 worker idle")
        || lowered.contains("pane(s) active")
    {
        "tmux_reinject_after_idle"
    } else {
        "manual_recovery"
    };

    let target_lane = trimmed.lines().map(str::trim).find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if !lower.starts_with("team ") {
            return None;
        }
        line[5..]
            .split_once(':')
            .map(|(name, _)| name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
    });

    let preserved_state = lowered
        .contains("current mode state")
        .then(|| String::from("current mode state"));

    Some(RecoveryOutcome {
        cause: cause.to_string(),
        target_lane,
        preserved_state,
    })
}

fn extract_roadmap_items(line: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '#' {
            let mut digits = String::new();
            while let Some(next) = chars.peek() {
                if next.is_ascii_digit() {
                    digits.push(*next);
                    chars.next();
                } else {
                    break;
                }
            }
            if !digits.is_empty() {
                items.push(format!("ROADMAP #{digits}"));
            }
        }
    }
    items
}

fn extract_artifact_provenance(
    manifest: &AgentOutput,
    raw_summary: Option<&str>,
) -> Option<ArtifactProvenance> {
    let summary = raw_summary?;
    let mut roadmap_ids = extract_roadmap_items(summary);
    roadmap_ids.extend(extract_roadmap_items(&manifest.description));
    roadmap_ids.sort();
    roadmap_ids.dedup();

    let mut files = extract_file_paths(summary);
    files.sort();
    files.dedup();

    let mut verification = Vec::new();
    let lowered = summary.to_ascii_lowercase();
    for (needle, label) in [
        ("tested", "tested"),
        ("committed", "committed"),
        ("pushed", "pushed"),
        ("merged", "merged"),
    ] {
        if lowered.contains(needle) {
            verification.push(label.to_string());
        }
    }

    let commit_sha = extract_commit_sha(summary);
    let diff_stat = extract_diff_stat(summary);
    let source_lanes = vec![manifest.name.clone()];

    if roadmap_ids.is_empty()
        && files.is_empty()
        && verification.is_empty()
        && commit_sha.is_none()
        && diff_stat.is_none()
    {
        return None;
    }

    Some(ArtifactProvenance {
        source_lanes,
        roadmap_ids,
        files,
        diff_stat,
        verification,
        commit_sha,
    })
}

fn extract_file_paths(summary: &str) -> Vec<String> {
    summary
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']'))
        .map(|token| {
            token
                .trim_matches('`')
                .trim_matches('"')
                .trim_matches('\'')
                .trim_end_matches('.')
        })
        .filter(|token| {
            token.contains('.')
                && !token.starts_with("http")
                && !token
                    .chars()
                    .all(|ch| ch.is_ascii_digit() || ch == '.' || ch == '+' || ch == '-')
        })
        .map(str::to_string)
        .collect()
}

fn extract_diff_stat(summary: &str) -> Option<String> {
    summary
        .split('\n')
        .map(str::trim)
        .find_map(|line| {
            line.find("Diff stat:")
                .map(|index| normalize_diff_stat(&line[(index + "Diff stat:".len())..]))
                .or_else(|| {
                    line.find("Diff:")
                        .map(|index| normalize_diff_stat(&line[(index + "Diff:".len())..]))
                })
        })
        .filter(|value| !value.is_empty())
}

fn normalize_diff_stat(value: &str) -> String {
    let trimmed = value.trim();
    for marker in [" Tested", " Committed", " committed", " pushed", " merged"] {
        if let Some((prefix, _)) = trimmed.split_once(marker) {
            return prefix.trim().to_string();
        }
    }
    trimmed.to_string()
}

fn disable_matching_crons(manifest: &AgentOutput, result: Option<&str>) -> Vec<String> {
    let tokens = cron_match_tokens(manifest, result);
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut disabled = Vec::new();
    for entry in global_cron_registry().list(true) {
        let haystack = format!(
            "{} {}",
            entry.prompt,
            entry.description.as_deref().unwrap_or_default()
        )
        .to_ascii_lowercase();
        if tokens.iter().any(|token| haystack.contains(token))
            && global_cron_registry().disable(&entry.cron_id).is_ok()
        {
            disabled.push(entry.cron_id);
        }
    }
    disabled.sort();
    disabled
}

fn cron_match_tokens(manifest: &AgentOutput, result: Option<&str>) -> Vec<String> {
    let mut tokens = extract_roadmap_items(manifest.description.as_str())
        .into_iter()
        .chain(extract_roadmap_items(result.unwrap_or_default()))
        .map(|item| item.to_ascii_lowercase())
        .collect::<Vec<_>>();

    if tokens.is_empty() && !manifest.name.trim().is_empty() {
        tokens.push(manifest.name.trim().to_ascii_lowercase());
    }

    tokens.sort();
    tokens.dedup();
    tokens
}

fn derive_agent_state(
    status: &str,
    result: Option<&str>,
    error: Option<&str>,
    blocker: Option<&LaneEventBlocker>,
) -> &'static str {
    let normalized_status = status.trim().to_ascii_lowercase();
    let normalized_error = error.unwrap_or_default().to_ascii_lowercase();

    if normalized_status == "running" {
        return "working";
    }
    if normalized_status == "completed" {
        return if result.is_some_and(|value| !value.trim().is_empty()) {
            "finished_cleanable"
        } else {
            "finished_pending_report"
        };
    }
    if normalized_error.contains("background") {
        return "blocked_background_job";
    }
    if normalized_error.contains("merge conflict") || normalized_error.contains("cherry-pick") {
        return "blocked_merge_conflict";
    }
    if normalized_error.contains("mcp") {
        return "degraded_mcp";
    }
    if normalized_error.contains("transport")
        || normalized_error.contains("broken pipe")
        || normalized_error.contains("connection")
        || normalized_error.contains("interrupted")
    {
        return "interrupted_transport";
    }
    if blocker.is_some() {
        return "truly_idle";
    }
    "truly_idle"
}

fn maybe_commit_provenance(result: Option<&str>) -> Option<LaneCommitProvenance> {
    let commit = extract_commit_sha(result?)?;
    let branch = current_git_branch().unwrap_or_else(|| "unknown".to_string());
    let worktree = std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string());
    Some(LaneCommitProvenance {
        commit: commit.clone(),
        branch,
        worktree,
        canonical_commit: Some(commit.clone()),
        superseded_by: None,
        lineage: vec![commit],
    })
}

fn extract_commit_sha(result: &str) -> Option<String> {
    result
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|token| token.len() >= 7 && token.len() <= 40)
        .map(str::to_string)
}

fn current_git_branch() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn append_agent_output(path: &str, suffix: &str) -> Result<(), String> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(suffix.as_bytes())
        .map_err(|error| error.to_string())
}

fn format_agent_terminal_output(
    status: &str,
    result: Option<&str>,
    blocker: Option<&LaneEventBlocker>,
    error: Option<&str>,
) -> String {
    let mut sections = vec![format!("\n## Result\n\n- status: {status}\n")];
    if let Some(blocker) = blocker {
        sections.push(format!(
            "\n### Blocker\n\n- failure_class: {}\n- detail: {}\n",
            serde_json::to_string(&blocker.failure_class)
                .unwrap_or_else(|_| "\"infra\"".to_string())
                .trim_matches('"'),
            blocker.detail.trim()
        ));
    }
    if let Some(result) = result.filter(|value| !value.trim().is_empty()) {
        sections.push(format!("\n### Final response\n\n{}\n", result.trim()));
    }
    if let Some(error) = error.filter(|value| !value.trim().is_empty()) {
        sections.push(format!("\n### Error\n\n{}\n", error.trim()));
    }
    sections.join("")
}

fn classify_lane_blocker(error: &str) -> LaneEventBlocker {
    let detail = error.trim().to_string();
    LaneEventBlocker {
        failure_class: classify_lane_failure(error),
        detail,
        subphase: None,
    }
}

fn classify_lane_failure(error: &str) -> LaneFailureClass {
    let normalized = error.to_ascii_lowercase();

    if normalized.contains("prompt") && normalized.contains("deliver") {
        LaneFailureClass::PromptDelivery
    } else if normalized.contains("trust") {
        LaneFailureClass::TrustGate
    } else if normalized.contains("branch")
        && (normalized.contains("stale") || normalized.contains("diverg"))
    {
        LaneFailureClass::BranchDivergence
    } else if normalized.contains("gateway") || normalized.contains("routing") {
        LaneFailureClass::GatewayRouting
    } else if normalized.contains("compile")
        || normalized.contains("build failed")
        || normalized.contains("cargo check")
    {
        LaneFailureClass::Compile
    } else if normalized.contains("test") {
        LaneFailureClass::Test
    } else if normalized.contains("tool failed")
        || normalized.contains("runtime tool")
        || normalized.contains("tool runtime")
    {
        LaneFailureClass::ToolRuntime
    } else if normalized.contains("workspace") && normalized.contains("mismatch") {
        LaneFailureClass::WorkspaceMismatch
    } else if normalized.contains("plugin") {
        LaneFailureClass::PluginStartup
    } else if normalized.contains("mcp") && normalized.contains("handshake") {
        LaneFailureClass::McpHandshake
    } else if normalized.contains("mcp") {
        LaneFailureClass::McpStartup
    } else {
        LaneFailureClass::Infra
    }
}

struct ProviderEntry {
    model: String,
    client: ProviderClient,
}

struct ProviderRuntimeClient {
    runtime: tokio::runtime::Runtime,
    chain: Vec<ProviderEntry>,
    allowed_tools: BTreeSet<String>,
}

impl ProviderRuntimeClient {
    #[allow(clippy::needless_pass_by_value)]
    fn new(model: String, allowed_tools: BTreeSet<String>) -> Result<Self, String> {
        let fallback_config = load_provider_fallback_config();
        Self::new_with_fallback_config(model, allowed_tools, &fallback_config)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn new_with_fallback_config(
        model: String,
        allowed_tools: BTreeSet<String>,
        fallback_config: &ProviderFallbackConfig,
    ) -> Result<Self, String> {
        let primary_model = fallback_config.primary().map_or(model, str::to_string);
        let primary = build_provider_entry(&primary_model)?;
        let mut chain = vec![primary];
        for fallback_model in fallback_config.fallbacks() {
            match build_provider_entry(fallback_model) {
                Ok(entry) => chain.push(entry),
                Err(error) => {
                    eprintln!(
                        "warning: skipping unavailable fallback provider {fallback_model}: {error}"
                    );
                }
            }
        }
        Ok(Self {
            runtime: tokio::runtime::Runtime::new().map_err(|error| error.to_string())?,
            chain,
            allowed_tools,
        })
    }
}

fn build_provider_entry(model: &str) -> Result<ProviderEntry, String> {
    let resolved = resolve_model_alias(model).clone();
    let client = ProviderClient::from_model(&resolved).map_err(|error| error.to_string())?;
    Ok(ProviderEntry {
        model: resolved,
        client,
    })
}

fn load_provider_fallback_config() -> ProviderFallbackConfig {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| ConfigLoader::default_for(cwd).load().ok())
        .map_or_else(ProviderFallbackConfig::default, |config| {
            config.provider_fallbacks().clone()
        })
}

impl ApiClient for ProviderRuntimeClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        let tools = tool_specs_for_allowed_tools(Some(&self.allowed_tools))
            .into_iter()
            .map(|spec| ToolDefinition {
                name: spec.name.to_string(),
                description: Some(spec.description.to_string()),
                input_schema: spec.input_schema,
            })
            .collect::<Vec<_>>();
        let messages = convert_messages(&request.messages);
        let system =
            (!request.system_prompt.is_empty()).then(|| request.system_prompt.join("\n\n"));
        let tool_choice = (!self.allowed_tools.is_empty()).then_some(ToolChoice::Auto);

        let runtime = &self.runtime;
        let chain = &self.chain;
        let mut last_error: Option<ApiError> = None;
        for (index, entry) in chain.iter().enumerate() {
            let message_request = MessageRequest {
                model: entry.model.clone(),
                max_tokens: max_tokens_for_model(&entry.model),
                messages: messages.clone(),
                system: system.clone(),
                tools: (!tools.is_empty()).then(|| tools.clone()),
                tool_choice: tool_choice.clone(),
                stream: true,
                ..Default::default()
            };

            let attempt = runtime.block_on(stream_with_provider(&entry.client, &message_request));
            match attempt {
                Ok(events) => return Ok(events),
                Err(error) if error.is_retryable() && index + 1 < chain.len() => {
                    eprintln!(
                        "provider {} failed with retryable error, falling back: {error}",
                        entry.model
                    );
                    last_error = Some(error);
                }
                Err(error) => return Err(RuntimeError::new(error.to_string())),
            }
        }

        Err(RuntimeError::new(last_error.map_or_else(
            || String::from("provider chain exhausted with no attempts"),
            |error| error.to_string(),
        )))
    }
}

#[allow(clippy::too_many_lines)]
async fn stream_with_provider(
    client: &ProviderClient,
    message_request: &MessageRequest,
) -> Result<Vec<AssistantEvent>, ApiError> {
    let mut stream = client.stream_message(message_request).await?;
    let mut events = Vec::new();
    let mut pending_tools: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
    let mut saw_stop = false;

    while let Some(event) = stream.next_event().await? {
        match event {
            ApiStreamEvent::MessageStart(start) => {
                for block in start.message.content {
                    push_output_block(block, 0, &mut events, &mut pending_tools, true);
                }
            }
            ApiStreamEvent::ContentBlockStart(start) => {
                push_output_block(
                    start.content_block,
                    start.index,
                    &mut events,
                    &mut pending_tools,
                    true,
                );
            }
            ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                ContentBlockDelta::TextDelta { text } => {
                    if !text.is_empty() {
                        events.push(AssistantEvent::TextDelta(text));
                    }
                }
                ContentBlockDelta::InputJsonDelta { partial_json } => {
                    if let Some((_, _, input)) = pending_tools.get_mut(&delta.index) {
                        input.push_str(&partial_json);
                    }
                }
                ContentBlockDelta::ThinkingDelta { .. }
                | ContentBlockDelta::SignatureDelta { .. } => {}
            },
            ApiStreamEvent::ContentBlockStop(stop) => {
                if let Some((id, name, input)) = pending_tools.remove(&stop.index) {
                    events.push(AssistantEvent::ToolUse { id, name, input });
                }
            }
            ApiStreamEvent::MessageDelta(delta) => {
                events.push(AssistantEvent::Usage(delta.usage.token_usage()));
            }
            ApiStreamEvent::MessageStop(_) => {
                saw_stop = true;
                events.push(AssistantEvent::MessageStop);
            }
        }
    }

    push_prompt_cache_record(client, &mut events);

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

    let response = client
        .send_message(&MessageRequest {
            stream: false,
            ..message_request.clone()
        })
        .await?;
    let mut events = response_to_events(response);
    push_prompt_cache_record(client, &mut events);
    Ok(events)
}

struct SubagentToolExecutor {
    allowed_tools: BTreeSet<String>,
    enforcer: Option<PermissionEnforcer>,
}

impl SubagentToolExecutor {
    fn new(allowed_tools: BTreeSet<String>) -> Self {
        Self {
            allowed_tools,
            enforcer: None,
        }
    }

    fn with_enforcer(mut self, enforcer: PermissionEnforcer) -> Self {
        self.enforcer = Some(enforcer);
        self
    }
}

impl ToolExecutor for SubagentToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if !self.allowed_tools.contains(tool_name) {
            return Err(ToolError::new(format!(
                "tool `{tool_name}` is not enabled for this sub-agent"
            )));
        }
        let value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
        execute_tool_with_enforcer(self.enforcer.as_ref(), tool_name, &value)
            .map_err(ToolError::new)
    }
}

fn tool_specs_for_allowed_tools(allowed_tools: Option<&BTreeSet<String>>) -> Vec<ToolSpec> {
    mvp_tool_specs()
        .into_iter()
        .filter(|spec| allowed_tools.is_none_or(|allowed| allowed.contains(spec.name)))
        .collect()
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
                .map(|block| match block {
                    ContentBlock::Text { text } => InputContentBlock::Text { text: text.clone() },
                    ContentBlock::ToolUse { id, name, input } => InputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::from_str(input)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": input })),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    } => InputContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContentBlock::Text {
                            text: output.clone(),
                        }],
                        is_error: *is_error,
                    },
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}

fn push_output_block(
    block: OutputContentBlock,
    block_index: u32,
    events: &mut Vec<AssistantEvent>,
    pending_tools: &mut BTreeMap<u32, (String, String, String)>,
    streaming_tool_input: bool,
) {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse { id, name, input } => {
            let initial_input = if streaming_tool_input
                && input.is_object()
                && input.as_object().is_some_and(serde_json::Map::is_empty)
            {
                String::new()
            } else {
                input.to_string()
            };
            pending_tools.insert(block_index, (id, name, initial_input));
        }
        OutputContentBlock::Thinking { .. } | OutputContentBlock::RedactedThinking { .. } => {}
    }
}

fn response_to_events(response: MessageResponse) -> Vec<AssistantEvent> {
    let mut events = Vec::new();
    let mut pending_tools = BTreeMap::new();

    for (index, block) in response.content.into_iter().enumerate() {
        let index = u32::try_from(index).expect("response block index overflow");
        push_output_block(block, index, &mut events, &mut pending_tools, false);
        if let Some((id, name, input)) = pending_tools.remove(&index) {
            events.push(AssistantEvent::ToolUse { id, name, input });
        }
    }

    events.push(AssistantEvent::Usage(response.usage.token_usage()));
    events.push(AssistantEvent::MessageStop);
    events
}

fn push_prompt_cache_record(client: &ProviderClient, events: &mut Vec<AssistantEvent>) {
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

#[allow(clippy::needless_pass_by_value)]
fn execute_tool_search(input: ToolSearchInput) -> ToolSearchOutput {
    GlobalToolRegistry::builtin().search(&input.query, input.max_results.unwrap_or(5), None, None)
}

fn deferred_tool_specs() -> Vec<ToolSpec> {
    mvp_tool_specs()
        .into_iter()
        .filter(|spec| {
            !matches!(
                spec.name,
                "bash" | "read_file" | "write_file" | "edit_file" | "glob_search" | "grep_search"
            )
        })
        .collect()
}

fn search_tool_specs(query: &str, max_results: usize, specs: &[SearchableToolSpec]) -> Vec<String> {
    let lowered = query.to_lowercase();
    if let Some(selection) = lowered.strip_prefix("select:") {
        return selection
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .filter_map(|wanted| {
                let wanted = canonical_tool_token(wanted);
                specs
                    .iter()
                    .find(|spec| canonical_tool_token(&spec.name) == wanted)
                    .map(|spec| spec.name.clone())
            })
            .take(max_results)
            .collect();
    }

    let mut required = Vec::new();
    let mut optional = Vec::new();
    for term in lowered.split_whitespace() {
        if let Some(rest) = term.strip_prefix('+') {
            if !rest.is_empty() {
                required.push(rest);
            }
        } else {
            optional.push(term);
        }
    }
    let terms = if required.is_empty() {
        optional.clone()
    } else {
        required.iter().chain(optional.iter()).copied().collect()
    };

    let mut scored = specs
        .iter()
        .filter_map(|spec| {
            let name = spec.name.to_lowercase();
            let canonical_name = canonical_tool_token(&spec.name);
            let normalized_description = normalize_tool_search_query(&spec.description);
            let haystack = format!(
                "{name} {} {canonical_name}",
                spec.description.to_lowercase()
            );
            let normalized_haystack = format!("{canonical_name} {normalized_description}");
            if required.iter().any(|term| !haystack.contains(term)) {
                return None;
            }

            let mut score = 0_i32;
            for term in &terms {
                let canonical_term = canonical_tool_token(term);
                if haystack.contains(term) {
                    score += 2;
                }
                if name == *term {
                    score += 8;
                }
                if name.contains(term) {
                    score += 4;
                }
                if canonical_name == canonical_term {
                    score += 12;
                }
                if normalized_haystack.contains(&canonical_term) {
                    score += 3;
                }
            }

            if score == 0 && !lowered.is_empty() {
                return None;
            }
            Some((score, spec.name.clone()))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    scored
        .into_iter()
        .map(|(_, name)| name)
        .take(max_results)
        .collect()
}

fn normalize_tool_search_query(query: &str) -> String {
    query
        .trim()
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .filter(|term| !term.is_empty())
        .map(canonical_tool_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn canonical_tool_token(value: &str) -> String {
    let mut canonical = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if let Some(stripped) = canonical.strip_suffix("tool") {
        canonical = stripped.to_string();
    }
    canonical
}

fn agent_store_dir() -> Result<std::path::PathBuf, String> {
    if let Ok(path) = std::env::var("CLAWD_AGENT_STORE") {
        return Ok(std::path::PathBuf::from(path));
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    if let Some(workspace_root) = cwd.ancestors().nth(2) {
        return Ok(workspace_root.join(".clawd-agents"));
    }
    Ok(cwd.join(".clawd-agents"))
}

fn make_agent_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("agent-{nanos}")
}

fn slugify_agent_name(description: &str) -> String {
    let mut out = description
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').chars().take(32).collect()
}

fn normalize_subagent_type(subagent_type: Option<&str>) -> String {
    let trimmed = subagent_type.map(str::trim).unwrap_or_default();
    if trimmed.is_empty() {
        return String::from("general-purpose");
    }

    match canonical_tool_token(trimmed).as_str() {
        "general" | "generalpurpose" | "generalpurposeagent" => String::from("general-purpose"),
        "explore" | "explorer" | "exploreagent" => String::from("Explore"),
        "plan" | "planagent" => String::from("Plan"),
        "verification" | "verificationagent" | "verify" | "verifier" => {
            String::from("Verification")
        }
        "clawguide" | "clawguideagent" | "guide" => String::from("claw-guide"),
        "statusline" | "statuslinesetup" => String::from("statusline-setup"),
        _ => trimmed.to_string(),
    }
}

fn iso8601_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[allow(clippy::too_many_lines)]
fn execute_notebook_edit(input: NotebookEditInput) -> Result<NotebookEditOutput, String> {
    let path = std::path::PathBuf::from(&input.notebook_path);
    if path.extension().and_then(|ext| ext.to_str()) != Some("ipynb") {
        return Err(String::from(
            "File must be a Jupyter notebook (.ipynb file).",
        ));
    }

    let original_file = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let mut notebook: serde_json::Value =
        serde_json::from_str(&original_file).map_err(|error| error.to_string())?;
    let language = notebook
        .get("metadata")
        .and_then(|metadata| metadata.get("kernelspec"))
        .and_then(|kernelspec| kernelspec.get("language"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("python")
        .to_string();
    let cells = notebook
        .get_mut("cells")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| String::from("Notebook cells array not found"))?;

    let edit_mode = input.edit_mode.unwrap_or(NotebookEditMode::Replace);
    let target_index = match input.cell_id.as_deref() {
        Some(cell_id) => Some(resolve_cell_index(cells, Some(cell_id), edit_mode)?),
        None if matches!(
            edit_mode,
            NotebookEditMode::Replace | NotebookEditMode::Delete
        ) =>
        {
            Some(resolve_cell_index(cells, None, edit_mode)?)
        }
        None => None,
    };
    let resolved_cell_type = match edit_mode {
        NotebookEditMode::Delete => None,
        NotebookEditMode::Insert => Some(input.cell_type.unwrap_or(NotebookCellType::Code)),
        NotebookEditMode::Replace => Some(input.cell_type.unwrap_or_else(|| {
            target_index
                .and_then(|index| cells.get(index))
                .and_then(cell_kind)
                .unwrap_or(NotebookCellType::Code)
        })),
    };
    let new_source = require_notebook_source(input.new_source, edit_mode)?;

    let cell_id = match edit_mode {
        NotebookEditMode::Insert => {
            let resolved_cell_type = resolved_cell_type
                .ok_or_else(|| String::from("insert mode requires a cell type"))?;
            let new_id = make_cell_id(cells.len());
            let new_cell = build_notebook_cell(&new_id, resolved_cell_type, &new_source);
            let insert_at = target_index.map_or(cells.len(), |index| index + 1);
            cells.insert(insert_at, new_cell);
            cells
                .get(insert_at)
                .and_then(|cell| cell.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        }
        NotebookEditMode::Delete => {
            let idx = target_index
                .ok_or_else(|| String::from("delete mode requires a target cell index"))?;
            let removed = cells.remove(idx);
            removed
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        }
        NotebookEditMode::Replace => {
            let resolved_cell_type = resolved_cell_type
                .ok_or_else(|| String::from("replace mode requires a cell type"))?;
            let idx = target_index
                .ok_or_else(|| String::from("replace mode requires a target cell index"))?;
            let cell = cells
                .get_mut(idx)
                .ok_or_else(|| String::from("Cell index out of range"))?;
            cell["source"] = serde_json::Value::Array(source_lines(&new_source));
            cell["cell_type"] = serde_json::Value::String(match resolved_cell_type {
                NotebookCellType::Code => String::from("code"),
                NotebookCellType::Markdown => String::from("markdown"),
            });
            match resolved_cell_type {
                NotebookCellType::Code => {
                    if !cell.get("outputs").is_some_and(serde_json::Value::is_array) {
                        cell["outputs"] = json!([]);
                    }
                    if cell.get("execution_count").is_none() {
                        cell["execution_count"] = serde_json::Value::Null;
                    }
                }
                NotebookCellType::Markdown => {
                    if let Some(object) = cell.as_object_mut() {
                        object.remove("outputs");
                        object.remove("execution_count");
                    }
                }
            }
            cell.get("id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        }
    };

    let updated_file =
        serde_json::to_string_pretty(&notebook).map_err(|error| error.to_string())?;
    std::fs::write(&path, &updated_file).map_err(|error| error.to_string())?;

    Ok(NotebookEditOutput {
        new_source,
        cell_id,
        cell_type: resolved_cell_type,
        language,
        edit_mode: format_notebook_edit_mode(edit_mode),
        error: None,
        notebook_path: path.display().to_string(),
        original_file,
        updated_file,
    })
}

fn require_notebook_source(
    source: Option<String>,
    edit_mode: NotebookEditMode,
) -> Result<String, String> {
    match edit_mode {
        NotebookEditMode::Delete => Ok(source.unwrap_or_default()),
        NotebookEditMode::Insert | NotebookEditMode::Replace => source
            .ok_or_else(|| String::from("new_source is required for insert and replace edits")),
    }
}

fn build_notebook_cell(cell_id: &str, cell_type: NotebookCellType, source: &str) -> Value {
    let mut cell = json!({
        "cell_type": match cell_type {
            NotebookCellType::Code => "code",
            NotebookCellType::Markdown => "markdown",
        },
        "id": cell_id,
        "metadata": {},
        "source": source_lines(source),
    });
    if let Some(object) = cell.as_object_mut() {
        match cell_type {
            NotebookCellType::Code => {
                object.insert(String::from("outputs"), json!([]));
                object.insert(String::from("execution_count"), Value::Null);
            }
            NotebookCellType::Markdown => {}
        }
    }
    cell
}

fn cell_kind(cell: &serde_json::Value) -> Option<NotebookCellType> {
    cell.get("cell_type")
        .and_then(serde_json::Value::as_str)
        .map(|kind| {
            if kind == "markdown" {
                NotebookCellType::Markdown
            } else {
                NotebookCellType::Code
            }
        })
}

const MAX_SLEEP_DURATION_MS: u64 = 300_000;

#[allow(clippy::needless_pass_by_value)]
fn execute_sleep(input: SleepInput) -> Result<SleepOutput, String> {
    if input.duration_ms > MAX_SLEEP_DURATION_MS {
        return Err(format!(
            "duration_ms {} exceeds maximum allowed sleep of {MAX_SLEEP_DURATION_MS}ms",
            input.duration_ms,
        ));
    }
    std::thread::sleep(Duration::from_millis(input.duration_ms));
    Ok(SleepOutput {
        duration_ms: input.duration_ms,
        message: format!("Slept for {}ms", input.duration_ms),
    })
}

fn execute_brief(input: BriefInput) -> Result<BriefOutput, String> {
    if input.message.trim().is_empty() {
        return Err(String::from("message must not be empty"));
    }

    let attachments = input
        .attachments
        .as_ref()
        .map(|paths| {
            paths
                .iter()
                .map(|path| resolve_attachment(path))
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?;

    let message = match input.status {
        BriefStatus::Normal | BriefStatus::Proactive => input.message,
    };

    Ok(BriefOutput {
        message,
        attachments,
        sent_at: iso8601_timestamp(),
    })
}

fn resolve_attachment(path: &str) -> Result<ResolvedAttachment, String> {
    let resolved = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
    let metadata = std::fs::metadata(&resolved).map_err(|error| error.to_string())?;
    Ok(ResolvedAttachment {
        path: resolved.display().to_string(),
        size: metadata.len(),
        is_image: is_image_path(&resolved),
    })
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")
    )
}

fn execute_config(input: ConfigInput) -> Result<ConfigOutput, String> {
    let setting = input.setting.trim();
    if setting.is_empty() {
        return Err(String::from("setting must not be empty"));
    }
    let Some(spec) = supported_config_setting(setting) else {
        return Ok(ConfigOutput {
            success: false,
            operation: None,
            setting: None,
            value: None,
            previous_value: None,
            new_value: None,
            error: Some(format!("Unknown setting: \"{setting}\"")),
        });
    };

    let path = config_file_for_scope(spec.scope)?;
    let mut document = read_json_object(&path)?;

    if let Some(value) = input.value {
        let normalized = normalize_config_value(spec, value)?;
        let previous_value = get_nested_value(&document, spec.path).cloned();
        set_nested_value(&mut document, spec.path, normalized.clone());
        write_json_object(&path, &document)?;
        Ok(ConfigOutput {
            success: true,
            operation: Some(String::from("set")),
            setting: Some(setting.to_string()),
            value: Some(normalized.clone()),
            previous_value,
            new_value: Some(normalized),
            error: None,
        })
    } else {
        Ok(ConfigOutput {
            success: true,
            operation: Some(String::from("get")),
            setting: Some(setting.to_string()),
            value: get_nested_value(&document, spec.path).cloned(),
            previous_value: None,
            new_value: None,
            error: None,
        })
    }
}

const PERMISSION_DEFAULT_MODE_PATH: &[&str] = &["permissions", "defaultMode"];

fn execute_enter_plan_mode(_input: EnterPlanModeInput) -> Result<PlanModeOutput, String> {
    let settings_path = config_file_for_scope(ConfigScope::Settings)?;
    let state_path = plan_mode_state_file()?;
    let mut document = read_json_object(&settings_path)?;
    let current_local_mode = get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned();
    let current_is_plan =
        matches!(current_local_mode.as_ref(), Some(Value::String(value)) if value == "plan");

    if let Some(state) = read_plan_mode_state(&state_path)? {
        if current_is_plan {
            return Ok(PlanModeOutput {
                success: true,
                operation: String::from("enter"),
                changed: false,
                active: true,
                managed: true,
                message: String::from("Plan mode override is already active for this worktree."),
                settings_path: settings_path.display().to_string(),
                state_path: state_path.display().to_string(),
                previous_local_mode: state.previous_local_mode,
                current_local_mode,
            });
        }
        clear_plan_mode_state(&state_path)?;
    }

    if current_is_plan {
        return Ok(PlanModeOutput {
            success: true,
            operation: String::from("enter"),
            changed: false,
            active: true,
            managed: false,
            message: String::from(
                "Worktree-local plan mode is already enabled outside EnterPlanMode; leaving it unchanged.",
            ),
            settings_path: settings_path.display().to_string(),
            state_path: state_path.display().to_string(),
            previous_local_mode: None,
            current_local_mode,
        });
    }

    let state = PlanModeState {
        had_local_override: current_local_mode.is_some(),
        previous_local_mode: current_local_mode.clone(),
    };
    write_plan_mode_state(&state_path, &state)?;
    set_nested_value(
        &mut document,
        PERMISSION_DEFAULT_MODE_PATH,
        Value::String(String::from("plan")),
    );
    write_json_object(&settings_path, &document)?;

    Ok(PlanModeOutput {
        success: true,
        operation: String::from("enter"),
        changed: true,
        active: true,
        managed: true,
        message: String::from("Enabled worktree-local plan mode override."),
        settings_path: settings_path.display().to_string(),
        state_path: state_path.display().to_string(),
        previous_local_mode: state.previous_local_mode,
        current_local_mode: get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned(),
    })
}

fn execute_exit_plan_mode(_input: ExitPlanModeInput) -> Result<PlanModeOutput, String> {
    let settings_path = config_file_for_scope(ConfigScope::Settings)?;
    let state_path = plan_mode_state_file()?;
    let mut document = read_json_object(&settings_path)?;
    let current_local_mode = get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned();
    let current_is_plan =
        matches!(current_local_mode.as_ref(), Some(Value::String(value)) if value == "plan");

    let Some(state) = read_plan_mode_state(&state_path)? else {
        return Ok(PlanModeOutput {
            success: true,
            operation: String::from("exit"),
            changed: false,
            active: current_is_plan,
            managed: false,
            message: String::from("No EnterPlanMode override is active for this worktree."),
            settings_path: settings_path.display().to_string(),
            state_path: state_path.display().to_string(),
            previous_local_mode: None,
            current_local_mode,
        });
    };

    if !current_is_plan {
        clear_plan_mode_state(&state_path)?;
        return Ok(PlanModeOutput {
            success: true,
            operation: String::from("exit"),
            changed: false,
            active: false,
            managed: false,
            message: String::from(
                "Cleared stale EnterPlanMode state because plan mode was already changed outside the tool.",
            ),
            settings_path: settings_path.display().to_string(),
            state_path: state_path.display().to_string(),
            previous_local_mode: state.previous_local_mode,
            current_local_mode,
        });
    }

    if state.had_local_override {
        if let Some(previous_local_mode) = state.previous_local_mode.clone() {
            set_nested_value(
                &mut document,
                PERMISSION_DEFAULT_MODE_PATH,
                previous_local_mode,
            );
        } else {
            remove_nested_value(&mut document, PERMISSION_DEFAULT_MODE_PATH);
        }
    } else {
        remove_nested_value(&mut document, PERMISSION_DEFAULT_MODE_PATH);
    }
    write_json_object(&settings_path, &document)?;
    clear_plan_mode_state(&state_path)?;

    Ok(PlanModeOutput {
        success: true,
        operation: String::from("exit"),
        changed: true,
        active: false,
        managed: false,
        message: String::from("Restored the prior worktree-local plan mode setting."),
        settings_path: settings_path.display().to_string(),
        state_path: state_path.display().to_string(),
        previous_local_mode: state.previous_local_mode,
        current_local_mode: get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned(),
    })
}

fn execute_structured_output(
    input: StructuredOutputInput,
) -> Result<StructuredOutputResult, String> {
    if input.0.is_empty() {
        return Err(String::from("structured output payload must not be empty"));
    }
    Ok(StructuredOutputResult {
        data: String::from("Structured output provided successfully"),
        structured_output: input.0,
    })
}

fn execute_repl(input: ReplInput) -> Result<ReplOutput, String> {
    if input.code.trim().is_empty() {
        return Err(String::from("code must not be empty"));
    }
    let runtime = resolve_repl_runtime(&input.language)?;
    let started = Instant::now();
    let mut process = Command::new(runtime.program);
    process
        .args(runtime.args)
        .arg(&input.code)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = if let Some(timeout_ms) = input.timeout_ms {
        let mut child = process.spawn().map_err(|error| error.to_string())?;
        loop {
            if child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
            {
                break child
                    .wait_with_output()
                    .map_err(|error| error.to_string())?;
            }
            if started.elapsed() >= Duration::from_millis(timeout_ms) {
                child.kill().map_err(|error| error.to_string())?;
                child
                    .wait_with_output()
                    .map_err(|error| error.to_string())?;
                return Err(format!(
                    "REPL execution exceeded timeout of {timeout_ms} ms"
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    } else {
        process
            .spawn()
            .map_err(|error| error.to_string())?
            .wait_with_output()
            .map_err(|error| error.to_string())?
    };

    Ok(ReplOutput {
        language: input.language,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(1),
        duration_ms: started.elapsed().as_millis(),
    })
}

struct ReplRuntime {
    program: &'static str,
    args: &'static [&'static str],
}

fn resolve_repl_runtime(language: &str) -> Result<ReplRuntime, String> {
    match language.trim().to_ascii_lowercase().as_str() {
        "python" | "py" => Ok(ReplRuntime {
            program: detect_first_command(&["python3", "python"])
                .ok_or_else(|| String::from("python runtime not found"))?,
            args: &["-c"],
        }),
        "javascript" | "js" | "node" => Ok(ReplRuntime {
            program: detect_first_command(&["node"])
                .ok_or_else(|| String::from("node runtime not found"))?,
            args: &["-e"],
        }),
        "sh" | "shell" | "bash" => Ok(ReplRuntime {
            program: detect_first_command(&["bash", "sh"])
                .ok_or_else(|| String::from("shell runtime not found"))?,
            args: &["-lc"],
        }),
        other => Err(format!("unsupported REPL language: {other}")),
    }
}

fn detect_first_command(commands: &[&'static str]) -> Option<&'static str> {
    commands
        .iter()
        .copied()
        .find(|command| command_exists(command))
}

#[derive(Clone, Copy)]
enum ConfigScope {
    Global,
    Settings,
}

#[derive(Clone, Copy)]
struct ConfigSettingSpec {
    scope: ConfigScope,
    kind: ConfigKind,
    path: &'static [&'static str],
    options: Option<&'static [&'static str]>,
}

#[derive(Clone, Copy)]
enum ConfigKind {
    Boolean,
    String,
}

fn supported_config_setting(setting: &str) -> Option<ConfigSettingSpec> {
    Some(match setting {
        "theme" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["theme"],
            options: None,
        },
        "editorMode" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["editorMode"],
            options: Some(&["default", "vim", "emacs"]),
        },
        "verbose" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["verbose"],
            options: None,
        },
        "preferredNotifChannel" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["preferredNotifChannel"],
            options: None,
        },
        "autoCompactEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["autoCompactEnabled"],
            options: None,
        },
        "autoMemoryEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::Boolean,
            path: &["autoMemoryEnabled"],
            options: None,
        },
        "autoDreamEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::Boolean,
            path: &["autoDreamEnabled"],
            options: None,
        },
        "fileCheckpointingEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["fileCheckpointingEnabled"],
            options: None,
        },
        "showTurnDuration" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["showTurnDuration"],
            options: None,
        },
        "terminalProgressBarEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["terminalProgressBarEnabled"],
            options: None,
        },
        "todoFeatureEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["todoFeatureEnabled"],
            options: None,
        },
        "model" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::String,
            path: &["model"],
            options: None,
        },
        "alwaysThinkingEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::Boolean,
            path: &["alwaysThinkingEnabled"],
            options: None,
        },
        "permissions.defaultMode" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::String,
            path: &["permissions", "defaultMode"],
            options: Some(&["default", "plan", "acceptEdits", "dontAsk", "auto"]),
        },
        "language" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::String,
            path: &["language"],
            options: None,
        },
        "teammateMode" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["teammateMode"],
            options: Some(&["tmux", "in-process", "auto"]),
        },
        _ => return None,
    })
}

fn normalize_config_value(spec: ConfigSettingSpec, value: ConfigValue) -> Result<Value, String> {
    let normalized = match (spec.kind, value) {
        (ConfigKind::Boolean, ConfigValue::Bool(value)) => Value::Bool(value),
        (ConfigKind::Boolean, ConfigValue::String(value)) => {
            match value.trim().to_ascii_lowercase().as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return Err(String::from("setting requires true or false")),
            }
        }
        (ConfigKind::Boolean, ConfigValue::Number(_)) => {
            return Err(String::from("setting requires true or false"))
        }
        (ConfigKind::String, ConfigValue::String(value)) => Value::String(value),
        (ConfigKind::String, ConfigValue::Bool(value)) => Value::String(value.to_string()),
        (ConfigKind::String, ConfigValue::Number(value)) => json!(value),
    };

    if let Some(options) = spec.options {
        let Some(as_str) = normalized.as_str() else {
            return Err(String::from("setting requires a string value"));
        };
        if !options.iter().any(|option| option == &as_str) {
            return Err(format!(
                "Invalid value \"{as_str}\". Options: {}",
                options.join(", ")
            ));
        }
    }

    Ok(normalized)
}

fn config_file_for_scope(scope: ConfigScope) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    Ok(match scope {
        ConfigScope::Global => config_home_dir()?.join("settings.json"),
        ConfigScope::Settings => cwd.join(".claw").join("settings.local.json"),
    })
}

fn config_home_dir() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("CLAW_CONFIG_HOME") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .or_else(|| {
            let drive = std::env::var("HOMEDRIVE").ok()?;
            let path = std::env::var("HOMEPATH").ok()?;
            Some(format!("{drive}{path}"))
        })
        .ok_or_else(|| {
            String::from(
                "HOME is not set (on Windows, set USERPROFILE or HOME, \
                 or use CLAW_CONFIG_HOME to point directly at the config directory)",
            )
        })?;
    Ok(PathBuf::from(home).join(".claw"))
}

fn read_json_object(path: &Path) -> Result<serde_json::Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            if contents.trim().is_empty() {
                return Ok(serde_json::Map::new());
            }
            serde_json::from_str::<Value>(&contents)
                .map_err(|error| error.to_string())?
                .as_object()
                .cloned()
                .ok_or_else(|| String::from("config file must contain a JSON object"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Map::new()),
        Err(error) => Err(error.to_string()),
    }
}

fn write_json_object(path: &Path, value: &serde_json::Map<String, Value>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn get_nested_value<'a>(
    value: &'a serde_json::Map<String, Value>,
    path: &[&str],
) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut current = value.get(*first)?;
    for key in rest {
        current = current.as_object()?.get(*key)?;
    }
    Some(current)
}

fn set_nested_value(root: &mut serde_json::Map<String, Value>, path: &[&str], new_value: Value) {
    let (first, rest) = path.split_first().expect("config path must not be empty");
    if rest.is_empty() {
        root.insert((*first).to_string(), new_value);
        return;
    }

    let entry = root
        .entry((*first).to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    let map = entry.as_object_mut().expect("object inserted");
    set_nested_value(map, rest, new_value);
}

fn remove_nested_value(root: &mut serde_json::Map<String, Value>, path: &[&str]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return root.remove(*first).is_some();
    }

    let mut should_remove_parent = false;
    let removed = root.get_mut(*first).is_some_and(|entry| {
        entry.as_object_mut().is_some_and(|map| {
            let removed = remove_nested_value(map, rest);
            should_remove_parent = removed && map.is_empty();
            removed
        })
    });

    if should_remove_parent {
        root.remove(*first);
    }

    removed
}

fn plan_mode_state_file() -> Result<PathBuf, String> {
    Ok(config_file_for_scope(ConfigScope::Settings)?
        .parent()
        .ok_or_else(|| String::from("settings.local.json has no parent directory"))?
        .join("tool-state")
        .join("plan-mode.json"))
}

fn read_plan_mode_state(path: &Path) -> Result<Option<PlanModeState>, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            if contents.trim().is_empty() {
                return Ok(None);
            }
            serde_json::from_str(&contents)
                .map(Some)
                .map_err(|error| error.to_string())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn write_plan_mode_state(path: &Path, state: &PlanModeState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(state).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn clear_plan_mode_state(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn iso8601_timestamp() -> String {
    if let Ok(output) = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
    {
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
    }
    iso8601_now()
}

#[allow(clippy::needless_pass_by_value)]
fn execute_powershell(input: PowerShellInput) -> std::io::Result<runtime::BashCommandOutput> {
    let _ = &input.description;
    if let Some(output) = workspace_test_branch_preflight(&input.command) {
        return Ok(output);
    }
    let shell = detect_powershell_shell()?;
    execute_shell_command(
        shell,
        &input.command,
        input.timeout,
        input.run_in_background,
    )
}

fn detect_powershell_shell() -> std::io::Result<&'static str> {
    if command_exists("pwsh") {
        Ok("pwsh")
    } else if command_exists("powershell") {
        Ok("powershell")
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "PowerShell executable not found (expected `pwsh` or `powershell` in PATH)",
        ))
    }
}

fn command_exists(command: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[allow(clippy::too_many_lines)]
fn execute_shell_command(
    shell: &str,
    command: &str,
    timeout: Option<u64>,
    run_in_background: Option<bool>,
) -> std::io::Result<runtime::BashCommandOutput> {
    if run_in_background.unwrap_or(false) {
        let child = std::process::Command::new(shell)
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        return Ok(runtime::BashCommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            raw_output_path: None,
            interrupted: false,
            is_image: None,
            background_task_id: Some(child.id().to_string()),
            backgrounded_by_user: Some(true),
            assistant_auto_backgrounded: Some(false),
            dangerously_disable_sandbox: None,
            return_code_interpretation: None,
            no_output_expected: Some(true),
            structured_content: None,
            persisted_output_path: None,
            persisted_output_size: None,
            sandbox_status: None,
        });
    }

    let mut process = std::process::Command::new(shell);
    process
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(command);
    process
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if let Some(timeout_ms) = timeout {
        let mut child = process.spawn()?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                let output = child.wait_with_output()?;
                return Ok(runtime::BashCommandOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    raw_output_path: None,
                    interrupted: false,
                    is_image: None,
                    background_task_id: None,
                    backgrounded_by_user: None,
                    assistant_auto_backgrounded: None,
                    dangerously_disable_sandbox: None,
                    return_code_interpretation: status
                        .code()
                        .filter(|code| *code != 0)
                        .map(|code| format!("exit_code:{code}")),
                    no_output_expected: Some(output.stdout.is_empty() && output.stderr.is_empty()),
                    structured_content: None,
                    persisted_output_path: None,
                    persisted_output_size: None,
                    sandbox_status: None,
                });
            }
            if started.elapsed() >= Duration::from_millis(timeout_ms) {
                let _ = child.kill();
                let output = child.wait_with_output()?;
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                let stderr = if stderr.trim().is_empty() {
                    format!("Command exceeded timeout of {timeout_ms} ms")
                } else {
                    format!(
                        "{}
Command exceeded timeout of {timeout_ms} ms",
                        stderr.trim_end()
                    )
                };
                return Ok(runtime::BashCommandOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr,
                    raw_output_path: None,
                    interrupted: true,
                    is_image: None,
                    background_task_id: None,
                    backgrounded_by_user: None,
                    assistant_auto_backgrounded: None,
                    dangerously_disable_sandbox: None,
                    return_code_interpretation: Some(String::from("timeout")),
                    no_output_expected: Some(false),
                    structured_content: None,
                    persisted_output_path: None,
                    persisted_output_size: None,
                    sandbox_status: None,
                });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let output = process.output()?;
    Ok(runtime::BashCommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        raw_output_path: None,
        interrupted: false,
        is_image: None,
        background_task_id: None,
        backgrounded_by_user: None,
        assistant_auto_backgrounded: None,
        dangerously_disable_sandbox: None,
        return_code_interpretation: output
            .status
            .code()
            .filter(|code| *code != 0)
            .map(|code| format!("exit_code:{code}")),
        no_output_expected: Some(output.stdout.is_empty() && output.stderr.is_empty()),
        structured_content: None,
        persisted_output_path: None,
        persisted_output_size: None,
        sandbox_status: None,
    })
}

fn resolve_cell_index(
    cells: &[serde_json::Value],
    cell_id: Option<&str>,
    edit_mode: NotebookEditMode,
) -> Result<usize, String> {
    if cells.is_empty()
        && matches!(
            edit_mode,
            NotebookEditMode::Replace | NotebookEditMode::Delete
        )
    {
        return Err(String::from("Notebook has no cells to edit"));
    }
    if let Some(cell_id) = cell_id {
        cells
            .iter()
            .position(|cell| cell.get("id").and_then(serde_json::Value::as_str) == Some(cell_id))
            .ok_or_else(|| format!("Cell id not found: {cell_id}"))
    } else {
        Ok(cells.len().saturating_sub(1))
    }
}

fn source_lines(source: &str) -> Vec<serde_json::Value> {
    if source.is_empty() {
        return vec![serde_json::Value::String(String::new())];
    }
    source
        .split_inclusive('\n')
        .map(|line| serde_json::Value::String(line.to_string()))
        .collect()
}

fn format_notebook_edit_mode(mode: NotebookEditMode) -> String {
    match mode {
        NotebookEditMode::Replace => String::from("replace"),
        NotebookEditMode::Insert => String::from("insert"),
        NotebookEditMode::Delete => String::from("delete"),
    }
}

fn make_cell_id(index: usize) -> String {
    format!("cell-{}", index + 1)
}

fn parse_skill_description(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("description:") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

pub mod lane_completion;
pub mod pdf_extract;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;
    use std::time::Duration;

    use super::{
        agent_permission_policy, allowed_tools_for_subagent, classify_lane_failure,
        derive_agent_state, execute_agent_with_spawn, execute_tool, extract_recovery_outcome,
        final_assistant_text, global_cron_registry, maybe_commit_provenance, mvp_tool_specs,
        permission_mode_from_plugin, persist_agent_terminal_state, push_output_block,
        run_task_packet, AgentInput, AgentJob, GlobalToolRegistry, LaneEventName, LaneFailureClass,
        ProviderRuntimeClient, SubagentToolExecutor,
    };
    use api::OutputContentBlock;
    use runtime::ProviderFallbackConfig;
    use runtime::{
        permission_enforcer::PermissionEnforcer, ApiRequest, AssistantEvent, ConversationRuntime,
        PermissionMode, PermissionPolicy, RuntimeError, Session, TaskPacket, ToolExecutor,
    };
    use serde_json::json;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn env_guard_recovers_after_poisoning() {
        let poisoned = std::thread::spawn(|| {
            let _guard = env_guard();
            panic!("poison env lock");
        })
        .join();
        assert!(poisoned.is_err(), "poisoning thread should panic");

        let _guard = env_guard();
    }

    fn temp_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("clawd-tools-{unique}-{name}"))
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap_or_else(|error| panic!("git {} failed: {error}", args.join(" ")));
        assert!(
            status.success(),
            "git {} exited with {status}",
            args.join(" ")
        );
    }

    fn init_git_repo(path: &Path) {
        std::fs::create_dir_all(path).expect("create repo");
        run_git(path, &["init", "--quiet", "-b", "main"]);
        run_git(path, &["config", "user.email", "tests@example.com"]);
        run_git(path, &["config", "user.name", "Tools Tests"]);
        std::fs::write(path.join("README.md"), "initial\n").expect("write readme");
        run_git(path, &["add", "README.md"]);
        run_git(path, &["commit", "-m", "initial commit", "--quiet"]);
    }

    #[allow(dead_code)]
    fn commit_file(path: &Path, file: &str, contents: &str, message: &str) {
        std::fs::write(path.join(file), contents).expect("write file");
        run_git(path, &["add", file]);
        run_git(path, &["commit", "-m", message, "--quiet"]);
    }

    fn permission_policy_for_mode(mode: PermissionMode) -> PermissionPolicy {
        mvp_tool_specs()
            .into_iter()
            .fold(PermissionPolicy::new(mode), |policy, spec| {
                policy.with_tool_requirement(spec.name, spec.required_permission)
            })
    }

    #[test]
    fn exposes_mvp_tools() {
        let names = mvp_tool_specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"git_diff"));
        assert!(names.contains(&"git_log"));
        assert!(names.contains(&"WebFetch"));
        assert!(names.contains(&"WebSearch"));
        assert!(names.contains(&"TodoWrite"));
        assert!(names.contains(&"Skill"));
        assert!(names.contains(&"Agent"));
        assert!(names.contains(&"ToolSearch"));
        assert!(names.contains(&"NotebookEdit"));
        assert!(names.contains(&"Sleep"));
        assert!(names.contains(&"SendUserMessage"));
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"EnterPlanMode"));
        assert!(names.contains(&"ExitPlanMode"));
        assert!(names.contains(&"StructuredOutput"));
        assert!(names.contains(&"REPL"));
        assert!(names.contains(&"PowerShell"));
        assert!(names.contains(&"WorkerCreate"));
        assert!(names.contains(&"WorkerObserve"));
        assert!(names.contains(&"WorkerAwaitReady"));
        assert!(names.contains(&"WorkerSendPrompt"));
    }

    #[test]
    fn rejects_unknown_tool_names() {
        let error = execute_tool("nope", &json!({})).expect_err("tool should be rejected");
        assert!(error.contains("unsupported tool"));
    }

    #[test]
    fn git_diff_and_log_execute_in_repo() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("git-tools");
        init_git_repo(&root);

        // Ensure tool executor runs in the repo.
        let old = std::env::current_dir().ok();
        std::env::set_current_dir(&root).expect("set cwd");

        // dirty working tree diff
        std::fs::write(root.join("README.md"), "changed\n").expect("modify");

        let log = execute_tool("git_log", &json!({"max_count": 5})).expect("git_log");
        assert!(log.contains("initial commit"), "log={log}");

        let diff = execute_tool("git_diff", &json!({})).expect("git_diff");
        assert!(
            diff.contains("diff --git") || diff.contains("@@"),
            "diff={diff}"
        );

        if let Some(old) = old {
            let _ = std::env::set_current_dir(old);
        }
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn worker_tools_gate_prompt_delivery_until_ready_and_support_auto_trust() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({
                "cwd": "/tmp/worktree/repo",
                "trusted_roots": ["/tmp/worktree"]
            }),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"]
            .as_str()
            .expect("worker id")
            .to_string();
        assert_eq!(created_output["status"], "spawning");
        assert_eq!(created_output["trust_auto_resolve"], true);

        let gated = execute_tool(
            "WorkerSendPrompt",
            &json!({
                "worker_id": worker_id,
                "prompt": "ship the change"
            }),
        )
        .expect_err("prompt delivery before ready should fail");
        assert!(gated.contains("not ready for prompt delivery"));

        let observed = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": created_output["worker_id"],
                "screen_text": "Do you trust the files in this folder?\n1. Yes, proceed\n2. No"
            }),
        )
        .expect("WorkerObserve should auto-resolve trust");
        let observed_output: serde_json::Value = serde_json::from_str(&observed).expect("json");
        assert_eq!(observed_output["status"], "spawning");
        assert_eq!(observed_output["trust_gate_cleared"], true);
        assert_eq!(
            observed_output["events"][1]["payload"]["type"],
            "trust_prompt"
        );
        assert_eq!(
            observed_output["events"][2]["payload"]["resolution"],
            "auto_allowlisted"
        );

        let ready = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": created_output["worker_id"],
                "screen_text": "Ready for your input\n>"
            }),
        )
        .expect("WorkerObserve should mark worker ready");
        let ready_output: serde_json::Value = serde_json::from_str(&ready).expect("json");
        assert_eq!(ready_output["status"], "ready_for_prompt");

        let await_ready = execute_tool(
            "WorkerAwaitReady",
            &json!({
                "worker_id": created_output["worker_id"]
            }),
        )
        .expect("WorkerAwaitReady should succeed");
        let await_ready_output: serde_json::Value =
            serde_json::from_str(&await_ready).expect("json");
        assert_eq!(await_ready_output["ready"], true);

        let accepted = execute_tool(
            "WorkerSendPrompt",
            &json!({
                "worker_id": created_output["worker_id"],
                "prompt": "ship the change"
            }),
        )
        .expect("WorkerSendPrompt should succeed after ready");
        let accepted_output: serde_json::Value = serde_json::from_str(&accepted).expect("json");
        assert_eq!(accepted_output["status"], "running");
        assert_eq!(accepted_output["prompt_delivery_attempts"], 1);
        assert_eq!(accepted_output["prompt_in_flight"], true);
    }

    #[test]
    fn worker_create_merges_config_trusted_roots_without_per_call_override() {
        use std::fs;
        // Write a .claw/settings.json in a temp dir with trustedRoots
        let worktree = temp_path("config-trust-worktree");
        let claw_dir = worktree.join(".claw");
        fs::create_dir_all(&claw_dir).expect("create .claw dir");
        // Use the actual OS temp dir so the worktree path matches the allowlist
        let tmp_root = std::env::temp_dir().to_str().expect("utf-8").to_string();
        let settings = serde_json::json!({ "trustedRoots": [tmp_root] });
        fs::write(
            claw_dir.join("settings.json"),
            serde_json::to_string(&settings).expect("serialize settings"),
        )
        .expect("write settings");

        // WorkerCreate with no per-call trusted_roots — config should supply them
        let cwd = worktree.to_str().expect("valid utf-8").to_string();
        let created = execute_tool(
            "WorkerCreate",
            &json!({
                "cwd": cwd
                // trusted_roots intentionally omitted
            }),
        )
        .expect("WorkerCreate should succeed");
        let output: serde_json::Value = serde_json::from_str(&created).expect("json");

        // worktree is under /tmp, so config roots auto-resolve trust
        assert_eq!(
            output["trust_auto_resolve"], true,
            "config-level trustedRoots should auto-resolve trust without per-call override"
        );

        fs::remove_dir_all(&worktree).ok();
    }

    #[test]
    fn worker_terminate_sets_finished_status() {
        // Create a worker in running state
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/terminate-test", "trusted_roots": ["/tmp"]}),
        )
        .expect("WorkerCreate should succeed");
        let output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = output["worker_id"].as_str().expect("worker_id").to_string();

        // Terminate
        let terminated = execute_tool("WorkerTerminate", &json!({"worker_id": worker_id}))
            .expect("WorkerTerminate should succeed");
        let term_output: serde_json::Value = serde_json::from_str(&terminated).expect("json");
        assert_eq!(
            term_output["status"], "finished",
            "terminated worker should be finished"
        );
        assert_eq!(
            term_output["prompt_in_flight"], false,
            "prompt_in_flight should be cleared on termination"
        );
    }

    #[test]
    fn worker_restart_resets_to_spawning() {
        // Create and advance worker to ready_for_prompt
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/restart-test", "trusted_roots": ["/tmp"]}),
        )
        .expect("WorkerCreate should succeed");
        let output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = output["worker_id"].as_str().expect("worker_id").to_string();

        // Advance to ready_for_prompt via observe
        execute_tool(
            "WorkerObserve",
            &json!({"worker_id": worker_id, "screen_text": "Ready for input\n>"}),
        )
        .expect("WorkerObserve should succeed");

        // Restart
        let restarted = execute_tool("WorkerRestart", &json!({"worker_id": worker_id}))
            .expect("WorkerRestart should succeed");
        let restart_output: serde_json::Value = serde_json::from_str(&restarted).expect("json");
        assert_eq!(
            restart_output["status"], "spawning",
            "restarted worker should return to spawning"
        );
        assert_eq!(
            restart_output["prompt_in_flight"], false,
            "prompt_in_flight should be cleared on restart"
        );
        assert_eq!(
            restart_output["trust_gate_cleared"], false,
            "trust_gate_cleared should be reset on restart (re-trust required)"
        );
    }

    #[test]
    fn worker_get_returns_worker_state() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/worker-get-test", "trusted_roots": ["/tmp"]}),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"].as_str().expect("worker_id");

        let fetched = execute_tool("WorkerGet", &json!({"worker_id": worker_id}))
            .expect("WorkerGet should succeed");
        let fetched_output: serde_json::Value = serde_json::from_str(&fetched).expect("json");
        assert_eq!(fetched_output["worker_id"], worker_id);
        assert_eq!(fetched_output["status"], "spawning");
        assert_eq!(fetched_output["cwd"], "/tmp/worker-get-test");
    }

    #[test]
    fn worker_get_on_unknown_id_returns_error() {
        let result = execute_tool(
            "WorkerGet",
            &json!({"worker_id": "worker_nonexistent_get_00000000"}),
        );
        assert!(
            result.is_err(),
            "WorkerGet on unknown id should return error"
        );
        assert!(
            result.unwrap_err().contains("worker not found"),
            "error should mention worker not found"
        );
    }

    #[test]
    fn worker_await_ready_on_spawning_worker_returns_not_ready() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/worker-await-not-ready"}),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"].as_str().expect("worker_id");

        // Worker is still in spawning — await_ready should return not-ready snapshot
        let snapshot = execute_tool("WorkerAwaitReady", &json!({"worker_id": worker_id}))
            .expect("WorkerAwaitReady should succeed even when not ready");
        let snap_output: serde_json::Value = serde_json::from_str(&snapshot).expect("json");
        assert_eq!(
            snap_output["ready"], false,
            "WorkerAwaitReady on a spawning worker must return ready=false"
        );
        assert_eq!(snap_output["worker_id"], worker_id);
    }

    #[test]
    fn worker_send_prompt_on_non_ready_worker_returns_error() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/worker-send-not-ready"}),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"].as_str().expect("worker_id");

        let result = execute_tool(
            "WorkerSendPrompt",
            &json!({"worker_id": worker_id, "prompt": "too early"}),
        );
        assert!(
            result.is_err(),
            "WorkerSendPrompt on a non-ready worker should fail"
        );
    }

    #[test]
    fn recovery_loop_state_file_reflects_transitions() {
        // End-to-end proof: .claw/worker-state.json reflects every transition
        // through the stall-detect -> resolve-trust -> ready loop.
        use std::fs;

        // Use a real temp CWD so state file can be written
        let worktree = temp_path("recovery-loop-state");
        fs::create_dir_all(&worktree).expect("create worktree");
        let cwd = worktree.to_str().expect("utf-8").to_string();
        let state_path = worktree.join(".claw").join("worker-state.json");

        // 1. Create worker WITHOUT trusted_roots
        let created = execute_tool("WorkerCreate", &json!({"cwd": cwd}))
            .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"]
            .as_str()
            .expect("worker_id")
            .to_string();
        // State file should exist after create
        assert!(
            state_path.exists(),
            "state file should be written after WorkerCreate"
        );
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).expect("read state"))
                .expect("parse state");
        assert_eq!(state["status"], "spawning");
        assert_eq!(state["is_ready"], false);
        assert!(
            state["seconds_since_update"].is_number(),
            "seconds_since_update must be present"
        );

        // 2. Force trust_required via observe
        execute_tool(
            "WorkerObserve",
            &json!({"worker_id": worker_id, "screen_text": "Do you trust the files in this folder?"}),
        )
        .expect("WorkerObserve should succeed");
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).expect("read state"))
                .expect("parse state");
        assert_eq!(
            state["status"], "trust_required",
            "state file must reflect trust_required stall"
        );
        assert_eq!(state["is_ready"], false);
        assert_eq!(state["trust_gate_cleared"], false);
        assert!(state["seconds_since_update"].is_number());

        // 3. WorkerResolveTrust -> state file reflects recovery
        execute_tool("WorkerResolveTrust", &json!({"worker_id": worker_id}))
            .expect("WorkerResolveTrust should succeed");
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).expect("read state"))
                .expect("parse state");
        assert_eq!(
            state["status"], "spawning",
            "state file must show spawning after trust resolved"
        );
        assert_eq!(state["trust_gate_cleared"], true);

        // 4. Observe ready screen -> state file shows ready_for_prompt
        execute_tool(
            "WorkerObserve",
            &json!({"worker_id": worker_id, "screen_text": "Ready for input\n>"}),
        )
        .expect("WorkerObserve ready should succeed");
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).expect("read state"))
                .expect("parse state");
        assert_eq!(
            state["status"], "ready_for_prompt",
            "state file must show ready_for_prompt after ready screen"
        );
        assert_eq!(
            state["is_ready"], true,
            "is_ready must be true in state file at ready_for_prompt"
        );

        fs::remove_dir_all(&worktree).ok();
    }

    #[test]
    fn stall_detect_and_resolve_trust_end_to_end() {
        // 1. Create worker WITHOUT trusted_roots so trust won't auto-resolve
        let created = execute_tool("WorkerCreate", &json!({"cwd": "/no/trusted/root/here"}))
            .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"]
            .as_str()
            .expect("worker_id")
            .to_string();
        assert_eq!(created_output["trust_auto_resolve"], false);

        // 2. Observe trust prompt screen text -> worker stalls at trust_required
        let stalled = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": worker_id,
                "screen_text": "Do you trust the files in this folder?\n[Allow] [Deny]"
            }),
        )
        .expect("WorkerObserve should succeed");
        let stalled_output: serde_json::Value = serde_json::from_str(&stalled).expect("json");
        assert_eq!(
            stalled_output["status"], "trust_required",
            "worker should stall at trust_required when trust prompt seen without allowlist"
        );
        assert_eq!(stalled_output["trust_gate_cleared"], false);
        // 3. Clawhip calls WorkerResolveTrust to unblock
        let resolved = execute_tool("WorkerResolveTrust", &json!({"worker_id": worker_id}))
            .expect("WorkerResolveTrust should succeed");
        let resolved_output: serde_json::Value = serde_json::from_str(&resolved).expect("json");
        assert_eq!(
            resolved_output["status"], "spawning",
            "worker should return to spawning after trust resolved"
        );
        assert_eq!(resolved_output["trust_gate_cleared"], true);

        // 4. Ready screen text now advances worker normally
        let ready = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": worker_id,
                "screen_text": "Ready for input\n>"
            }),
        )
        .expect("WorkerObserve should succeed after trust resolved");
        let ready_output: serde_json::Value = serde_json::from_str(&ready).expect("json");
        assert_eq!(
            ready_output["status"], "ready_for_prompt",
            "worker should reach ready_for_prompt after trust resolved and ready screen seen"
        );
    }

    #[test]
    fn stall_detect_and_restart_recovery_end_to_end() {
        // Worker stalls at trust_required, clawhip restarts instead of resolving
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/no/trusted/root/restart-test"}),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"]
            .as_str()
            .expect("worker_id")
            .to_string();

        // Force trust_required
        let stalled = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": worker_id,
                "screen_text": "trust this folder? [Yes] [No]"
            }),
        )
        .expect("WorkerObserve should succeed");
        let stalled_output: serde_json::Value = serde_json::from_str(&stalled).expect("json");
        assert_eq!(stalled_output["status"], "trust_required");

        // WorkerRestart resets the worker
        let restarted = execute_tool("WorkerRestart", &json!({"worker_id": worker_id}))
            .expect("WorkerRestart should succeed");
        let restarted_output: serde_json::Value = serde_json::from_str(&restarted).expect("json");
        assert_eq!(
            restarted_output["status"], "spawning",
            "restarted worker should be back at spawning"
        );
        assert_eq!(
            restarted_output["trust_gate_cleared"], false,
            "restart clears trust — next observe loop must re-acquire trust"
        );
    }

    #[test]
    fn worker_terminate_on_unknown_id_returns_error() {
        let result = execute_tool(
            "WorkerTerminate",
            &json!({"worker_id": "worker_nonexistent_00000000"}),
        );
        assert!(result.is_err(), "terminating unknown worker should fail");
        assert!(
            result.unwrap_err().contains("worker not found"),
            "error should mention worker not found"
        );
    }

    #[test]
    fn worker_restart_on_unknown_id_returns_error() {
        let result = execute_tool(
            "WorkerRestart",
            &json!({"worker_id": "worker_nonexistent_00000001"}),
        );
        assert!(result.is_err(), "restarting unknown worker should fail");
        assert!(
            result.unwrap_err().contains("worker not found"),
            "error should mention worker not found"
        );
    }

    #[test]
    fn worker_observe_completion_success_finish_sets_finished_status() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/observe-completion-test", "trusted_roots": ["/tmp"]}),
        )
        .expect("WorkerCreate should succeed");
        let output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = output["worker_id"].as_str().expect("worker_id").to_string();

        let completed = execute_tool(
            "WorkerObserveCompletion",
            &json!({
                "worker_id": worker_id,
                "finish_reason": "end_turn",
                "tokens_output": 512
            }),
        )
        .expect("WorkerObserveCompletion should succeed");
        let completed_output: serde_json::Value = serde_json::from_str(&completed).expect("json");
        assert_eq!(completed_output["status"], "finished");
        assert_eq!(completed_output["prompt_in_flight"], false);
    }

    #[test]
    fn worker_observe_completion_degraded_provider_sets_failed_status() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/observe-degraded-test", "trusted_roots": ["/tmp"]}),
        )
        .expect("WorkerCreate should succeed");
        let output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = output["worker_id"].as_str().expect("worker_id").to_string();

        // finish=unknown + 0 tokens = degraded provider classification
        let failed = execute_tool(
            "WorkerObserveCompletion",
            &json!({
                "worker_id": worker_id,
                "finish_reason": "unknown",
                "tokens_output": 0
            }),
        )
        .expect("WorkerObserveCompletion should succeed");
        let failed_output: serde_json::Value = serde_json::from_str(&failed).expect("json");
        assert_eq!(
            failed_output["status"], "failed",
            "finish=unknown + 0 tokens should classify as provider failure"
        );
        assert_eq!(failed_output["prompt_in_flight"], false);
        // last_error should be set with provider failure message
        assert!(
            !failed_output["last_error"].is_null(),
            "last_error should be populated for provider failure"
        );
    }

    #[test]
    fn worker_tools_detect_misdelivery_and_arm_prompt_replay() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({
                "cwd": "/tmp/repo/worker-misdelivery"
            }),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"]
            .as_str()
            .expect("worker id")
            .to_string();

        execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": worker_id,
                "screen_text": "Ready for input\n>"
            }),
        )
        .expect("worker should become ready");

        execute_tool(
            "WorkerSendPrompt",
            &json!({
                "worker_id": worker_id,
                "prompt": "Investigate flaky boot"
            }),
        )
        .expect("prompt send should succeed");

        let recovered = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": worker_id,
                "screen_text": "% Investigate flaky boot\nzsh: command not found: Investigate"
            }),
        )
        .expect("misdelivery observe should succeed");
        let recovered_output: serde_json::Value = serde_json::from_str(&recovered).expect("json");
        assert_eq!(recovered_output["status"], "ready_for_prompt");
        assert_eq!(recovered_output["last_error"]["kind"], "prompt_delivery");
        assert_eq!(recovered_output["replay_prompt"], "Investigate flaky boot");
        assert_eq!(
            recovered_output["events"][3]["payload"]["observed_target"],
            "shell"
        );
        assert_eq!(
            recovered_output["events"][4]["payload"]["recovery_armed"],
            true
        );

        let replayed = execute_tool(
            "WorkerSendPrompt",
            &json!({
                "worker_id": worker_id
            }),
        )
        .expect("WorkerSendPrompt should replay recovered prompt");
        let replayed_output: serde_json::Value = serde_json::from_str(&replayed).expect("json");
        assert_eq!(replayed_output["status"], "running");
        assert_eq!(replayed_output["prompt_delivery_attempts"], 2);
        assert_eq!(replayed_output["prompt_in_flight"], true);
    }

    #[test]
    fn global_tool_registry_denies_blocked_tool_before_dispatch() {
        // given
        let policy = permission_policy_for_mode(PermissionMode::ReadOnly);
        let registry = GlobalToolRegistry::builtin().with_enforcer(PermissionEnforcer::new(policy));

        // when
        let error = registry
            .execute(
                "write_file",
                &json!({
                    "path": "blocked.txt",
                    "content": "blocked"
                }),
            )
            .expect_err("write tool should be denied before dispatch");

        // then
        assert!(error.contains("requires workspace-write permission"));
    }

    #[test]
    fn subagent_tool_executor_denies_blocked_tool_before_dispatch() {
        // given
        let policy = permission_policy_for_mode(PermissionMode::ReadOnly);
        let mut executor = SubagentToolExecutor::new(BTreeSet::from([String::from("write_file")]))
            .with_enforcer(PermissionEnforcer::new(policy));

        // when
        let error = executor
            .execute(
                "write_file",
                &json!({
                    "path": "blocked.txt",
                    "content": "blocked"
                })
                .to_string(),
            )
            .expect_err("subagent write tool should be denied before dispatch");

        // then
        assert!(error
            .to_string()
            .contains("requires workspace-write permission"));
    }

    #[test]
    fn permission_mode_from_plugin_rejects_invalid_inputs() {
        let unknown_permission = permission_mode_from_plugin("admin")
            .expect_err("unknown plugin permission should fail");
        assert!(unknown_permission.contains("unsupported plugin permission: admin"));

        let empty_permission =
            permission_mode_from_plugin("").expect_err("empty plugin permission should fail");
        assert!(empty_permission.contains("unsupported plugin permission: "));
    }

    #[test]
    fn allowed_tools_rejects_empty_token_lists() {
        let registry = GlobalToolRegistry::builtin();

        for raw in ["", ",,", "   "] {
            let err = registry
                .normalize_allowed_tools(&[raw.to_string()])
                .expect_err("empty allow-list input should be rejected");
            assert!(
                err.contains("--allowedTools was provided with no usable tool names"),
                "unexpected error for {raw:?}: {err}"
            );
        }
    }

    #[test]
    fn runtime_tools_extend_registry_definitions_permissions_and_search() {
        let registry = GlobalToolRegistry::builtin()
            .with_runtime_tools(vec![super::RuntimeToolDefinition {
                name: "mcp__demo__echo".to_string(),
                description: Some("Echo text from the demo MCP server".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "additionalProperties": false
                }),
                required_permission: runtime::PermissionMode::ReadOnly,
            }])
            .expect("runtime tools should register");

        let allowed = registry
            .normalize_allowed_tools(&["mcp__demo__echo".to_string()])
            .expect("runtime tool should be allow-listable")
            .expect("allow-list should be populated");
        assert!(allowed.contains("mcp__demo__echo"));

        let definitions = registry.definitions(Some(&allowed));
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name, "mcp__demo__echo");

        let permissions = registry
            .permission_specs(Some(&allowed))
            .expect("runtime tool permissions should resolve");
        assert_eq!(
            permissions,
            vec![(
                "mcp__demo__echo".to_string(),
                runtime::PermissionMode::ReadOnly
            )]
        );

        let search = registry.search(
            "demo echo",
            5,
            Some(vec!["pending-server".to_string()]),
            Some(runtime::McpDegradedReport::new(
                vec!["demo".to_string()],
                vec![runtime::McpFailedServer {
                    server_name: "pending-server".to_string(),
                    phase: runtime::McpLifecyclePhase::ToolDiscovery,
                    error: runtime::McpErrorSurface::new(
                        runtime::McpLifecyclePhase::ToolDiscovery,
                        Some("pending-server".to_string()),
                        "tool discovery failed",
                        BTreeMap::new(),
                        true,
                    ),
                }],
                vec!["mcp__demo__echo".to_string()],
                vec!["mcp__demo__echo".to_string()],
            )),
        );
        let output = serde_json::to_value(search).expect("search output should serialize");
        assert_eq!(output["matches"][0], "mcp__demo__echo");
        assert_eq!(output["pending_mcp_servers"][0], "pending-server");
        assert_eq!(
            output["mcp_degraded"]["failed_servers"][0]["phase"],
            "tool_discovery"
        );
    }

    #[test]
    fn web_fetch_returns_prompt_aware_summary() {
        let server = TestServer::spawn(Arc::new(|request_line: &str| {
            assert!(request_line.starts_with("GET /page "));
            HttpResponse::html(
                200,
                "OK",
                "<html><head><title>Ignored</title></head><body><h1>Test Page</h1><p>Hello <b>world</b> from local server.</p></body></html>",
            )
        }));

        let result = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/page", server.addr()),
                "prompt": "Summarize this page"
            }),
        )
        .expect("WebFetch should succeed");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(output["code"], 200);
        let summary = output["result"].as_str().expect("result string");
        assert!(summary.contains("Fetched"));
        assert!(summary.contains("Test Page"));
        assert!(summary.contains("Hello world from local server"));

        let titled = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/page", server.addr()),
                "prompt": "What is the page title?"
            }),
        )
        .expect("WebFetch title query should succeed");
        let titled_output: serde_json::Value = serde_json::from_str(&titled).expect("valid json");
        let titled_summary = titled_output["result"].as_str().expect("result string");
        assert!(titled_summary.contains("Title: Ignored"));
    }

    #[test]
    fn web_fetch_supports_plain_text_and_rejects_invalid_url() {
        let server = TestServer::spawn(Arc::new(|request_line: &str| {
            assert!(request_line.starts_with("GET /plain "));
            HttpResponse::text(200, "OK", "plain text response")
        }));

        let result = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/plain", server.addr()),
                "prompt": "Show me the content"
            }),
        )
        .expect("WebFetch should succeed for text content");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(output["url"], format!("http://{}/plain", server.addr()));
        assert!(output["result"]
            .as_str()
            .expect("result")
            .contains("plain text response"));

        let error = execute_tool(
            "WebFetch",
            &json!({
                "url": "not a url",
                "prompt": "Summarize"
            }),
        )
        .expect_err("invalid URL should fail");
        assert!(error.contains("relative URL without a base") || error.contains("invalid"));
    }

    #[test]
    fn web_search_extracts_and_filters_results() {
        // Serialize env-var mutation so this test cannot race with the sibling
        // web_search_handles_generic_links_and_invalid_base_url test that also
        // sets CLAWD_WEB_SEARCH_BASE_URL. Without the lock, parallel test
        // runners can interleave the set/remove calls and cause assertion
        // failures on the wrong port.
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str| {
            assert!(request_line.contains("GET /search?q=rust+web+search "));
            HttpResponse::html(
                200,
                "OK",
                r#"
                <html><body>
                  <a class="result__a" href="https://docs.rs/reqwest">Reqwest docs</a>
                  <a class="result__a" href="https://example.com/blocked">Blocked result</a>
                </body></html>
                "#,
            )
        }));

        std::env::set_var(
            "CLAWD_WEB_SEARCH_BASE_URL",
            format!("http://{}/search", server.addr()),
        );
        let result = execute_tool(
            "WebSearch",
            &json!({
                "query": "rust web search",
                "allowed_domains": ["https://DOCS.rs/"],
                "blocked_domains": ["HTTPS://EXAMPLE.COM"]
            }),
        )
        .expect("WebSearch should succeed");
        std::env::remove_var("CLAWD_WEB_SEARCH_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(output["query"], "rust web search");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["title"], "Reqwest docs");
        assert_eq!(content[0]["url"], "https://docs.rs/reqwest");
    }

    #[test]
    fn web_search_handles_generic_links_and_invalid_base_url() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str| {
            assert!(request_line.contains("GET /fallback?q=generic+links "));
            HttpResponse::html(
                200,
                "OK",
                r#"
                <html><body>
                  <a href="https://example.com/one">Example One</a>
                  <a href="https://example.com/one">Duplicate Example One</a>
                  <a href="https://docs.rs/tokio">Tokio Docs</a>
                </body></html>
                "#,
            )
        }));

        std::env::set_var(
            "CLAWD_WEB_SEARCH_BASE_URL",
            format!("http://{}/fallback", server.addr()),
        );
        let result = execute_tool(
            "WebSearch",
            &json!({
                "query": "generic links"
            }),
        )
        .expect("WebSearch fallback parsing should succeed");
        std::env::remove_var("CLAWD_WEB_SEARCH_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["url"], "https://example.com/one");
        assert_eq!(content[1]["url"], "https://docs.rs/tokio");

        std::env::set_var("CLAWD_WEB_SEARCH_BASE_URL", "://bad-base-url");
        let error = execute_tool("WebSearch", &json!({ "query": "generic links" }))
            .expect_err("invalid base URL should fail");
        std::env::remove_var("CLAWD_WEB_SEARCH_BASE_URL");
        assert!(error.contains("relative URL without a base") || error.contains("empty host"));
    }

    #[test]
    fn pending_tools_preserve_multiple_streaming_tool_calls_by_index() {
        let mut events = Vec::new();
        let mut pending_tools = BTreeMap::new();

        push_output_block(
            OutputContentBlock::ToolUse {
                id: "tool-1".to_string(),
                name: "read_file".to_string(),
                input: json!({}),
            },
            1,
            &mut events,
            &mut pending_tools,
            true,
        );
        push_output_block(
            OutputContentBlock::ToolUse {
                id: "tool-2".to_string(),
                name: "grep_search".to_string(),
                input: json!({}),
            },
            2,
            &mut events,
            &mut pending_tools,
            true,
        );

        pending_tools
            .get_mut(&1)
            .expect("first tool pending")
            .2
            .push_str("{\"path\":\"src/main.rs\"}");
        pending_tools
            .get_mut(&2)
            .expect("second tool pending")
            .2
            .push_str("{\"pattern\":\"TODO\"}");

        assert_eq!(
            pending_tools.remove(&1),
            Some((
                "tool-1".to_string(),
                "read_file".to_string(),
                "{\"path\":\"src/main.rs\"}".to_string(),
            ))
        );
        assert_eq!(
            pending_tools.remove(&2),
            Some((
                "tool-2".to_string(),
                "grep_search".to_string(),
                "{\"pattern\":\"TODO\"}".to_string(),
            ))
        );
    }

    #[test]
    fn todo_write_persists_and_returns_previous_state() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = temp_path("todos.json");
        std::env::set_var("CLAWD_TODO_STORE", &path);

        let first = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "Add tool", "activeForm": "Adding tool", "status": "in_progress"},
                    {"content": "Run tests", "activeForm": "Running tests", "status": "pending"}
                ]
            }),
        )
        .expect("TodoWrite should succeed");
        let first_output: serde_json::Value = serde_json::from_str(&first).expect("valid json");
        assert_eq!(first_output["oldTodos"].as_array().expect("array").len(), 0);

        let second = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "Add tool", "activeForm": "Adding tool", "status": "completed"},
                    {"content": "Run tests", "activeForm": "Running tests", "status": "completed"},
                    {"content": "Verify", "activeForm": "Verifying", "status": "completed"}
                ]
            }),
        )
        .expect("TodoWrite should succeed");
        std::env::remove_var("CLAWD_TODO_STORE");
        let _ = std::fs::remove_file(path);

        let second_output: serde_json::Value = serde_json::from_str(&second).expect("valid json");
        assert_eq!(
            second_output["oldTodos"].as_array().expect("array").len(),
            2
        );
        assert_eq!(
            second_output["newTodos"].as_array().expect("array").len(),
            3
        );
        assert!(second_output["verificationNudgeNeeded"].is_null());
    }

    #[test]
    fn todo_write_rejects_invalid_payloads_and_sets_verification_nudge() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = temp_path("todos-errors.json");
        std::env::set_var("CLAWD_TODO_STORE", &path);

        let empty = execute_tool("TodoWrite", &json!({ "todos": [] }))
            .expect_err("empty todos should fail");
        assert!(empty.contains("todos must not be empty"));

        // Multiple in_progress items are now allowed for parallel workflows
        let _multi_active = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "One", "activeForm": "Doing one", "status": "in_progress"},
                    {"content": "Two", "activeForm": "Doing two", "status": "in_progress"}
                ]
            }),
        )
        .expect("multiple in-progress todos should succeed");

        let blank_content = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "   ", "activeForm": "Doing it", "status": "pending"}
                ]
            }),
        )
        .expect_err("blank content should fail");
        assert!(blank_content.contains("todo content must not be empty"));

        let nudge = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "Write tests", "activeForm": "Writing tests", "status": "completed"},
                    {"content": "Fix errors", "activeForm": "Fixing errors", "status": "completed"},
                    {"content": "Ship branch", "activeForm": "Shipping branch", "status": "completed"}
                ]
            }),
        )
        .expect("completed todos should succeed");
        std::env::remove_var("CLAWD_TODO_STORE");
        let _ = fs::remove_file(path);

        let output: serde_json::Value = serde_json::from_str(&nudge).expect("valid json");
        assert_eq!(output["verificationNudgeNeeded"], true);
    }

    #[test]
    fn skill_loads_local_skill_prompt() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = temp_path("skills-home");
        let skill_dir = home.join(".agents").join("skills").join("help");
        fs::create_dir_all(&skill_dir).expect("skill dir should exist");
        fs::write(
            skill_dir.join("SKILL.md"),
            "# help\n\nGuide on using oh-my-codex plugin\n",
        )
        .expect("skill file should exist");
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);

        let result = execute_tool(
            "Skill",
            &json!({
                "skill": "help",
                "args": "overview"
            }),
        )
        .expect("Skill should succeed");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(output["skill"], "help");
        let path = output["path"].as_str().expect("path").replace('\\', "/");
        assert!(path.ends_with("/help/SKILL.md"));
        assert!(output["prompt"]
            .as_str()
            .expect("prompt")
            .contains("Guide on using oh-my-codex plugin"));

        let dollar_result = execute_tool(
            "Skill",
            &json!({
                "skill": "$help"
            }),
        )
        .expect("Skill should accept $skill invocation form");
        let dollar_output: serde_json::Value =
            serde_json::from_str(&dollar_result).expect("valid json");
        assert_eq!(dollar_output["skill"], "$help");
        let path = dollar_output["path"]
            .as_str()
            .expect("path")
            .replace('\\', "/");
        assert!(path.ends_with("/help/SKILL.md"));

        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        fs::remove_dir_all(home).expect("temp home should clean up");
    }

    #[test]
    fn skill_resolves_project_local_skills_and_legacy_commands() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("project-skills");
        let skill_dir = root.join(".claw").join("skills").join("plan");
        let command_dir = root.join(".claw").join("commands");
        fs::create_dir_all(&skill_dir).expect("skill dir should exist");
        fs::create_dir_all(&command_dir).expect("command dir should exist");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: plan\ndescription: Project planning guidance\n---\n\n# plan\n",
        )
        .expect("skill file should exist");
        fs::write(
            command_dir.join("handoff.md"),
            "---\nname: handoff\ndescription: Legacy handoff guidance\n---\n\n# handoff\n",
        )
        .expect("command file should exist");

        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("set cwd");

        let skill_result = execute_tool("Skill", &json!({ "skill": "$plan" }))
            .expect("project-local skill should resolve");
        let skill_output: serde_json::Value =
            serde_json::from_str(&skill_result).expect("valid json");
        let path = skill_output["path"]
            .as_str()
            .expect("path")
            .replace('\\', "/");
        assert!(path.ends_with(".claw/skills/plan/SKILL.md"));

        let command_result = execute_tool("Skill", &json!({ "skill": "/handoff" }))
            .expect("legacy command should resolve");
        let command_output: serde_json::Value =
            serde_json::from_str(&command_result).expect("valid json");
        let path = command_output["path"]
            .as_str()
            .expect("path")
            .replace('\\', "/");
        assert!(path.ends_with(".claw/commands/handoff.md"));

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        fs::remove_dir_all(root).expect("temp project should clean up");
    }

    #[test]
    fn skill_loads_project_local_claude_skill_prompt() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("project-skills");
        let home = root.join("home");
        let workspace = root.join("workspace");
        let nested = workspace.join("nested");
        let skill_dir = workspace.join(".claude").join("skills").join("trace");
        fs::create_dir_all(&skill_dir).expect("skill dir should exist");
        fs::create_dir_all(&nested).expect("nested cwd should exist");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: trace\ndescription: Project-local trace helper\n---\n# trace\n",
        )
        .expect("skill file should exist");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_codex_home = std::env::var("CODEX_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::set_current_dir(&nested).expect("set cwd");

        let result = execute_tool("Skill", &json!({ "skill": "trace" }))
            .expect("project-local skill should resolve");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let path = output["path"].as_str().expect("path").replace('\\', "/");
        assert!(path.ends_with(".claude/skills/trace/SKILL.md"));
        assert_eq!(output["description"], "Project-local trace helper");

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        match original_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        fs::remove_dir_all(root).expect("temp tree should clean up");
    }

    #[test]
    fn skill_loads_project_local_omc_and_agents_skill_prompts() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("project-omc-skills");
        let home = root.join("home");
        let workspace = root.join("workspace");
        let nested = workspace.join("nested");
        let omc_skill_dir = workspace.join(".omc").join("skills").join("hud");
        let agents_skill_dir = workspace.join(".agents").join("skills").join("trace");
        fs::create_dir_all(&omc_skill_dir).expect("omc skill dir should exist");
        fs::create_dir_all(&agents_skill_dir).expect("agents skill dir should exist");
        fs::create_dir_all(&nested).expect("nested cwd should exist");
        fs::write(
            omc_skill_dir.join("SKILL.md"),
            "---\nname: hud\ndescription: Project-local OMC HUD helper\n---\n# hud\n",
        )
        .expect("omc skill file should exist");
        fs::write(
            agents_skill_dir.join("SKILL.md"),
            "---\nname: trace\ndescription: Project-local agents compatibility helper\n---\n# trace\n",
        )
        .expect("agents skill file should exist");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_codex_home = std::env::var("CODEX_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::set_current_dir(&nested).expect("set cwd");

        let omc_result =
            execute_tool("Skill", &json!({ "skill": "hud" })).expect("omc skill should resolve");
        let agents_result = execute_tool("Skill", &json!({ "skill": "trace" }))
            .expect("agents skill should resolve");

        let omc_output: serde_json::Value = serde_json::from_str(&omc_result).expect("valid json");
        let agents_output: serde_json::Value =
            serde_json::from_str(&agents_result).expect("valid json");
        let omc_path = omc_output["path"]
            .as_str()
            .expect("path")
            .replace('\\', "/");
        assert!(omc_path.ends_with(".omc/skills/hud/SKILL.md"));
        assert_eq!(omc_output["description"], "Project-local OMC HUD helper");
        let agents_path = agents_output["path"]
            .as_str()
            .expect("path")
            .replace('\\', "/");
        assert!(agents_path.ends_with(".agents/skills/trace/SKILL.md"));
        assert_eq!(
            agents_output["description"],
            "Project-local agents compatibility helper"
        );

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        match original_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        fs::remove_dir_all(root).expect("temp tree should clean up");
    }

    #[test]
    fn skill_loads_learned_skill_from_claude_config_dir() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("claude-config-learned-skill");
        let home = root.join("home");
        let claude_config_dir = root.join("claude-config");
        let learned_skill_dir = claude_config_dir
            .join("skills")
            .join("omc-learned")
            .join("learned");
        fs::create_dir_all(&learned_skill_dir).expect("learned skill dir should exist");
        fs::write(
            learned_skill_dir.join("SKILL.md"),
            "---\nname: learned\ndescription: Learned OMC skill\n---\n# learned\n",
        )
        .expect("learned skill file should exist");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_codex_home = std::env::var("CODEX_HOME").ok();
        let original_claude_config_dir = std::env::var("CLAUDE_CONFIG_DIR").ok();
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::set_var("CLAUDE_CONFIG_DIR", &claude_config_dir);

        let result = execute_tool("Skill", &json!({ "skill": "learned" }))
            .expect("learned skill should resolve");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let path = output["path"].as_str().expect("path").replace('\\', "/");
        assert!(path.ends_with("skills/omc-learned/learned/SKILL.md"));
        assert_eq!(output["description"], "Learned OMC skill");

        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        match original_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        match original_claude_config_dir {
            Some(value) => std::env::set_var("CLAUDE_CONFIG_DIR", value),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        fs::remove_dir_all(root).expect("temp tree should clean up");
    }

    #[test]
    fn skill_loads_direct_skill_and_legacy_command_from_claude_config_dir() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("claude-config-direct-skill");
        let home = root.join("home");
        let claude_config_dir = root.join("claude-config");
        let skill_dir = claude_config_dir.join("skills").join("statusline");
        let command_dir = claude_config_dir.join("commands");
        fs::create_dir_all(&skill_dir).expect("direct skill dir should exist");
        fs::create_dir_all(&command_dir).expect("command dir should exist");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: statusline\ndescription: Claude config skill\n---\n# statusline\n",
        )
        .expect("direct skill file should exist");
        fs::write(
            command_dir.join("doctor-check.md"),
            "---\nname: doctor-check\ndescription: Claude config command\n---\n# doctor-check\n",
        )
        .expect("direct command file should exist");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_codex_home = std::env::var("CODEX_HOME").ok();
        let original_claude_config_dir = std::env::var("CLAUDE_CONFIG_DIR").ok();
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::set_var("CLAUDE_CONFIG_DIR", &claude_config_dir);

        let direct_skill =
            execute_tool("Skill", &json!({ "skill": "statusline" })).expect("direct skill");
        let direct_skill_output: serde_json::Value =
            serde_json::from_str(&direct_skill).expect("valid skill json");
        let path = direct_skill_output["path"]
            .as_str()
            .expect("path")
            .replace('\\', "/");
        assert!(path.ends_with("skills/statusline/SKILL.md"));
        assert_eq!(direct_skill_output["description"], "Claude config skill");

        let legacy_command =
            execute_tool("Skill", &json!({ "skill": "doctor-check" })).expect("direct command");
        let legacy_command_output: serde_json::Value =
            serde_json::from_str(&legacy_command).expect("valid command json");
        let path = legacy_command_output["path"]
            .as_str()
            .expect("path")
            .replace('\\', "/");
        assert!(path.ends_with("commands/doctor-check.md"));
        assert_eq!(
            legacy_command_output["description"],
            "Claude config command"
        );

        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        match original_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        match original_claude_config_dir {
            Some(value) => std::env::set_var("CLAUDE_CONFIG_DIR", value),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        fs::remove_dir_all(root).expect("temp tree should clean up");
    }

    #[test]
    fn skill_loads_project_local_legacy_command_markdown() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("project-legacy-command");
        let home = root.join("home");
        let workspace = root.join("workspace");
        let nested = workspace.join("nested");
        let command_dir = workspace.join(".claude").join("commands");
        fs::create_dir_all(&command_dir).expect("legacy command dir should exist");
        fs::create_dir_all(&nested).expect("nested cwd should exist");
        fs::write(
            command_dir.join("team.md"),
            "---\nname: team\ndescription: Legacy team workflow\n---\n# team\n",
        )
        .expect("legacy command file should exist");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_codex_home = std::env::var("CODEX_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::set_current_dir(&nested).expect("set cwd");

        let result = execute_tool("Skill", &json!({ "skill": "team" }))
            .expect("legacy command markdown should resolve");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let path = output["path"].as_str().expect("path").replace('\\', "/");
        assert!(path.ends_with(".claude/commands/team.md"));
        assert_eq!(output["description"], "Legacy team workflow");

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        match original_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        fs::remove_dir_all(root).expect("temp tree should clean up");
    }

    #[test]
    fn tool_search_supports_keyword_and_select_queries() {
        let keyword = execute_tool(
            "ToolSearch",
            &json!({"query": "web current", "max_results": 3}),
        )
        .expect("ToolSearch should succeed");
        let keyword_output: serde_json::Value = serde_json::from_str(&keyword).expect("valid json");
        let matches = keyword_output["matches"].as_array().expect("matches");
        assert!(matches.iter().any(|value| value == "WebSearch"));

        let selected = execute_tool("ToolSearch", &json!({"query": "select:Agent,Skill"}))
            .expect("ToolSearch should succeed");
        let selected_output: serde_json::Value =
            serde_json::from_str(&selected).expect("valid json");
        assert_eq!(selected_output["matches"][0], "Agent");
        assert_eq!(selected_output["matches"][1], "Skill");

        let aliased = execute_tool("ToolSearch", &json!({"query": "AgentTool"}))
            .expect("ToolSearch should support tool aliases");
        let aliased_output: serde_json::Value = serde_json::from_str(&aliased).expect("valid json");
        assert_eq!(aliased_output["matches"][0], "Agent");
        assert_eq!(aliased_output["normalized_query"], "agent");

        let selected_with_alias =
            execute_tool("ToolSearch", &json!({"query": "select:AgentTool,Skill"}))
                .expect("ToolSearch alias select should succeed");
        let selected_with_alias_output: serde_json::Value =
            serde_json::from_str(&selected_with_alias).expect("valid json");
        assert_eq!(selected_with_alias_output["matches"][0], "Agent");
        assert_eq!(selected_with_alias_output["matches"][1], "Skill");
    }

    #[test]
    fn agent_persists_handoff_metadata() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = temp_path("agent-store");
        std::env::set_var("CLAWD_AGENT_STORE", &dir);
        let captured = Arc::new(Mutex::new(None::<AgentJob>));
        let captured_for_spawn = Arc::clone(&captured);

        let manifest = execute_agent_with_spawn(
            AgentInput {
                description: "Audit the branch".to_string(),
                prompt: "Check tests and outstanding work.".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("ship-audit".to_string()),
                model: None,
            },
            move |job| {
                *captured_for_spawn
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(job);
                Ok(())
            },
        )
        .expect("Agent should succeed");
        std::env::remove_var("CLAWD_AGENT_STORE");

        assert_eq!(manifest.name, "ship-audit");
        assert_eq!(manifest.subagent_type.as_deref(), Some("Explore"));
        assert_eq!(manifest.status, "running");
        assert!(!manifest.created_at.is_empty());
        assert!(manifest.started_at.is_some());
        assert!(manifest.completed_at.is_none());
        let contents = std::fs::read_to_string(&manifest.output_file).expect("agent file exists");
        let manifest_contents =
            std::fs::read_to_string(&manifest.manifest_file).expect("manifest file exists");
        let manifest_json: serde_json::Value =
            serde_json::from_str(&manifest_contents).expect("manifest should be valid json");
        assert!(contents.contains("Audit the branch"));
        assert!(contents.contains("Check tests and outstanding work."));
        assert!(manifest_contents.contains("\"subagentType\": \"Explore\""));
        assert!(manifest_contents.contains("\"status\": \"running\""));
        assert_eq!(manifest_json["laneEvents"][0]["event"], "lane.started");
        assert_eq!(manifest_json["laneEvents"][0]["status"], "running");
        assert!(manifest_json["currentBlocker"].is_null());
        let captured_job = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("spawn job should be captured");
        assert_eq!(captured_job.prompt, "Check tests and outstanding work.");
        assert!(captured_job.allowed_tools.contains("read_file"));
        assert!(!captured_job.allowed_tools.contains("Agent"));

        let normalized = execute_tool(
            "Agent",
            &json!({
                "description": "Verify the branch",
                "prompt": "Check tests.",
                "subagent_type": "explorer"
            }),
        )
        .expect("Agent should normalize built-in aliases");
        let normalized_output: serde_json::Value =
            serde_json::from_str(&normalized).expect("valid json");
        assert_eq!(normalized_output["subagentType"], "Explore");

        let named = execute_tool(
            "Agent",
            &json!({
                "description": "Review the branch",
                "prompt": "Inspect diff.",
                "name": "Ship Audit!!!"
            }),
        )
        .expect("Agent should normalize explicit names");
        let named_output: serde_json::Value = serde_json::from_str(&named).expect("valid json");
        assert_eq!(named_output["name"], "ship-audit");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn agent_fake_runner_can_persist_completion_and_failure() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = temp_path("agent-runner");
        std::env::set_var("CLAWD_AGENT_STORE", &dir);

        let completed = execute_agent_with_spawn(
            AgentInput {
                description: "Complete the task".to_string(),
                prompt: "Do the work".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("complete-task".to_string()),
                model: Some("claude-sonnet-4-6".to_string()),
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some("Finished successfully in commit abc1234"),
                    None,
                )
            },
        )
        .expect("completed agent should succeed");

        let completed_manifest = std::fs::read_to_string(&completed.manifest_file)
            .expect("completed manifest should exist");
        let completed_manifest_json: serde_json::Value =
            serde_json::from_str(&completed_manifest).expect("completed manifest json");
        let completed_output =
            std::fs::read_to_string(&completed.output_file).expect("completed output should exist");
        assert!(completed_manifest.contains("\"status\": \"completed\""));
        assert!(completed_output.contains("Finished successfully"));
        assert_eq!(
            completed_manifest_json["laneEvents"][0]["event"],
            "lane.started"
        );
        assert_eq!(
            completed_manifest_json["laneEvents"][1]["event"],
            "lane.finished"
        );
        assert_eq!(
            completed_manifest_json["laneEvents"][1]["data"]["qualityFloorApplied"],
            false
        );
        assert_eq!(
            completed_manifest_json["laneEvents"][1]["detail"],
            "Finished successfully in commit abc1234"
        );
        assert_eq!(
            completed_manifest_json["laneEvents"][2]["event"],
            "lane.commit.created"
        );
        assert_eq!(
            completed_manifest_json["laneEvents"][2]["data"]["commit"],
            "abc1234"
        );
        assert!(completed_manifest_json["currentBlocker"].is_null());
        assert_eq!(
            completed_manifest_json["derivedState"],
            "finished_cleanable"
        );

        let failed = execute_agent_with_spawn(
            AgentInput {
                description: "Fail the task".to_string(),
                prompt: "Do the failing work".to_string(),
                subagent_type: Some("Verification".to_string()),
                name: Some("fail-task".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "failed",
                    None,
                    Some(String::from("tool failed: simulated failure")),
                )
            },
        )
        .expect("failed agent should still spawn");

        let failed_manifest =
            std::fs::read_to_string(&failed.manifest_file).expect("failed manifest should exist");
        let failed_manifest_json: serde_json::Value =
            serde_json::from_str(&failed_manifest).expect("failed manifest json");
        let failed_output =
            std::fs::read_to_string(&failed.output_file).expect("failed output should exist");
        assert!(failed_manifest.contains("\"status\": \"failed\""));
        assert!(failed_manifest.contains("simulated failure"));
        assert!(failed_output.contains("simulated failure"));
        assert!(failed_output.contains("failure_class: tool_runtime"));
        assert_eq!(
            failed_manifest_json["currentBlocker"]["failureClass"],
            "tool_runtime"
        );
        assert_eq!(
            failed_manifest_json["laneEvents"][1]["event"],
            "lane.blocked"
        );
        assert_eq!(
            failed_manifest_json["laneEvents"][2]["event"],
            "lane.failed"
        );
        assert_eq!(
            failed_manifest_json["laneEvents"][2]["failureClass"],
            "tool_runtime"
        );
        assert_eq!(failed_manifest_json["derivedState"], "truly_idle");

        let normalized = execute_agent_with_spawn(
            AgentInput {
                description: "Sweep the next backlog item".to_string(),
                prompt: "Produce a low-signal stop summary".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("summary-floor".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some("commit push everyting, keep sweeping $ralph"),
                    None,
                )
            },
        )
        .expect("normalized agent should succeed");

        let normalized_manifest = std::fs::read_to_string(&normalized.manifest_file)
            .expect("normalized manifest should exist");
        let normalized_manifest_json: serde_json::Value =
            serde_json::from_str(&normalized_manifest).expect("normalized manifest json");
        assert_eq!(
            normalized_manifest_json["laneEvents"][1]["event"],
            "lane.finished"
        );
        let normalized_detail = normalized_manifest_json["laneEvents"][1]["detail"]
            .as_str()
            .expect("normalized detail");
        assert!(normalized_detail.contains("Completed lane `summary-floor`"));
        assert!(normalized_detail.contains("Sweep the next backlog item"));
        assert_eq!(
            normalized_manifest_json["laneEvents"][1]["data"]["qualityFloorApplied"],
            true
        );
        assert_eq!(
            normalized_manifest_json["laneEvents"][1]["data"]["rawSummary"],
            "commit push everyting, keep sweeping $ralph"
        );
        assert_eq!(
            normalized_manifest_json["laneEvents"][1]["data"]["reasons"][0],
            "control_only"
        );

        let recovery = execute_agent_with_spawn(
            AgentInput {
                description: "Recover the stalled audit lane".to_string(),
                prompt: "Normalize OMX reinjection control prose".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("recovery-lane".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some(
                        "Team read-only-audit-only-for-roadm: worker panes stalled, no progress 2m30s. Next: omx team status read-only-audit-only-for-roadm; read worker messages; unblock/reassign or shutdown. [OMX_TMUX_INJECT]",
                    ),
                    None,
                )
            },
        )
        .expect("recovery agent should succeed");

        let recovery_manifest = std::fs::read_to_string(&recovery.manifest_file)
            .expect("recovery manifest should exist");
        let recovery_manifest_json: serde_json::Value =
            serde_json::from_str(&recovery_manifest).expect("recovery manifest json");
        let recovery_detail = recovery_manifest_json["laneEvents"][1]["detail"]
            .as_str()
            .expect("recovery detail");
        assert!(recovery_detail.contains("Recovery handoff observed via tmux reinjection"));
        assert!(recovery_detail.contains("read-only-audit-only-for-roadm"));
        assert!(!recovery_detail.contains("OMX_TMUX_INJECT"));
        assert_eq!(
            recovery_manifest_json["laneEvents"][1]["data"]["recoveryOutcome"]["cause"],
            "tmux_reinject_after_idle"
        );
        assert_eq!(
            recovery_manifest_json["laneEvents"][1]["data"]["recoveryOutcome"]["targetLane"],
            "read-only-audit-only-for-roadm"
        );
        assert_eq!(
            recovery_manifest_json["laneEvents"][1]["data"]["qualityFloorApplied"],
            true
        );
        assert_eq!(
            recovery_manifest_json["laneEvents"][1]["data"]["reasons"][0],
            "recovery_control_prose"
        );

        let review = execute_agent_with_spawn(
            AgentInput {
                description: "Review commit 1234abcd for ROADMAP #67".to_string(),
                prompt: "Review the scoped diff".to_string(),
                subagent_type: Some("Verification".to_string()),
                name: Some("review-lane".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some("APPROVE\n\nTarget: commit 1234abcd\nRationale: scoped diff is safe."),
                    None,
                )
            },
        )
        .expect("review agent should succeed");

        let review_manifest =
            std::fs::read_to_string(&review.manifest_file).expect("review manifest should exist");
        let review_manifest_json: serde_json::Value =
            serde_json::from_str(&review_manifest).expect("review manifest json");
        assert_eq!(
            review_manifest_json["laneEvents"][1]["data"]["reviewVerdict"],
            "approve"
        );
        assert_eq!(
            review_manifest_json["laneEvents"][1]["data"]["reviewTarget"],
            "Review commit 1234abcd for ROADMAP #67"
        );
        assert_eq!(
            review_manifest_json["laneEvents"][1]["data"]["reviewRationale"],
            "Target: commit 1234abcd Rationale: scoped diff is safe."
        );
        assert_eq!(
            review_manifest_json["laneEvents"][1]["data"]["qualityFloorApplied"],
            false
        );

        let selection = execute_agent_with_spawn(
            AgentInput {
                description: "Scan ROADMAP Immediate Backlog for the next repo-local item".to_string(),
                prompt: "Choose the next backlog target".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("backlog-scan".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some(
                        "Selected next backlog target.\nChosen: ROADMAP #65\nSkipped: ROADMAP #63, ROADMAP #64\nAction: execute\nRationale: #65 is the next repo-local lane-finished metadata task.",
                    ),
                    None,
                )
            },
        )
        .expect("selection agent should succeed");

        let selection_manifest = std::fs::read_to_string(&selection.manifest_file)
            .expect("selection manifest should exist");
        let selection_manifest_json: serde_json::Value =
            serde_json::from_str(&selection_manifest).expect("selection manifest json");
        assert_eq!(
            selection_manifest_json["laneEvents"][1]["data"]["selectionOutcome"]["chosenItems"][0],
            "ROADMAP #65"
        );
        assert_eq!(
            selection_manifest_json["laneEvents"][1]["data"]["selectionOutcome"]["skippedItems"][0],
            "ROADMAP #63"
        );
        assert_eq!(
            selection_manifest_json["laneEvents"][1]["data"]["selectionOutcome"]["skippedItems"][1],
            "ROADMAP #64"
        );
        assert_eq!(
            selection_manifest_json["laneEvents"][1]["data"]["selectionOutcome"]["action"],
            "execute"
        );
        assert_eq!(
            selection_manifest_json["laneEvents"][1]["data"]["selectionOutcome"]["rationale"],
            "#65 is the next repo-local lane-finished metadata task."
        );

        let artifact = execute_agent_with_spawn(
            AgentInput {
                description: "Land ROADMAP #64 provenance hardening".to_string(),
                prompt: "Ship structured artifact provenance".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("artifact-lane".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some(
                        "Completed ROADMAP #64. Files: rust/crates/tools/src/lib.rs ROADMAP.md. Diff stat: 2 files, +12/-1. Tested, committed, pushed as commit deadbee.",
                    ),
                    None,
                )
            },
        )
        .expect("artifact agent should succeed");

        let artifact_manifest = std::fs::read_to_string(&artifact.manifest_file)
            .expect("artifact manifest should exist");
        let artifact_manifest_json: serde_json::Value =
            serde_json::from_str(&artifact_manifest).expect("artifact manifest json");
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["sourceLanes"][0],
            "artifact-lane"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["roadmapIds"][0],
            "ROADMAP #64"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["files"][0],
            "ROADMAP.md"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["files"][1],
            "rust/crates/tools/src/lib.rs"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["diffStat"],
            "2 files, +12/-1."
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["verification"]
                [0],
            "tested"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["verification"]
                [1],
            "committed"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["verification"]
                [2],
            "pushed"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["commitSha"],
            "deadbee"
        );

        let cron = global_cron_registry().create(
            "*/10 * * * *",
            "roadmap-nudge-10min for ROADMAP #66",
            Some("ROADMAP #66 reminder"),
        );
        let reminder = execute_agent_with_spawn(
            AgentInput {
                description: "Close ROADMAP #66 reminder shutdown".to_string(),
                prompt: "Finish the cron shutdown fix".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("cron-closeout".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some("Completed ROADMAP #66 after verification."),
                    None,
                )
            },
        )
        .expect("reminder agent should succeed");

        let reminder_manifest = std::fs::read_to_string(&reminder.manifest_file)
            .expect("reminder manifest should exist");
        let reminder_manifest_json: serde_json::Value =
            serde_json::from_str(&reminder_manifest).expect("reminder manifest json");
        assert_eq!(
            reminder_manifest_json["laneEvents"][1]["data"]["disabledCronIds"][0],
            cron.cron_id
        );
        let disabled_entry = global_cron_registry()
            .get(&cron.cron_id)
            .expect("cron should still exist");
        assert!(!disabled_entry.enabled);

        let resume_outcome =
            extract_recovery_outcome("Continue from current mode state. [OMX_TMUX_INJECT]")
                .expect("resume outcome should be detected");
        assert_eq!(resume_outcome.cause, "resume_after_stop");
        assert_eq!(
            resume_outcome.preserved_state.as_deref(),
            Some("current mode state")
        );

        let spawn_error = execute_agent_with_spawn(
            AgentInput {
                description: "Spawn error task".to_string(),
                prompt: "Never starts".to_string(),
                subagent_type: None,
                name: Some("spawn-error".to_string()),
                model: None,
            },
            |_| Err(String::from("thread creation failed")),
        )
        .expect_err("spawn errors should surface");
        assert!(spawn_error.contains("failed to spawn sub-agent"));
        let spawn_error_manifest = std::fs::read_dir(&dir)
            .expect("agent dir should exist")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .find_map(|path| {
                let contents = std::fs::read_to_string(&path).ok()?;
                contents
                    .contains("\"name\": \"spawn-error\"")
                    .then_some(contents)
            })
            .expect("failed manifest should still be written");
        let spawn_error_manifest_json: serde_json::Value =
            serde_json::from_str(&spawn_error_manifest).expect("spawn error manifest json");
        assert!(spawn_error_manifest.contains("\"status\": \"failed\""));
        assert!(spawn_error_manifest.contains("thread creation failed"));
        assert_eq!(
            spawn_error_manifest_json["currentBlocker"]["failureClass"],
            "infra"
        );
        assert_eq!(spawn_error_manifest_json["derivedState"], "truly_idle");

        std::env::remove_var("CLAWD_AGENT_STORE");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn agent_state_classification_covers_finished_and_specific_blockers() {
        assert_eq!(derive_agent_state("running", None, None, None), "working");
        assert_eq!(
            derive_agent_state("completed", Some("done"), None, None),
            "finished_cleanable"
        );
        assert_eq!(
            derive_agent_state("completed", None, None, None),
            "finished_pending_report"
        );
        assert_eq!(
            derive_agent_state("failed", None, Some("mcp handshake timed out"), None),
            "degraded_mcp"
        );
        assert_eq!(
            derive_agent_state(
                "failed",
                None,
                Some("background terminal still running"),
                None
            ),
            "blocked_background_job"
        );
        assert_eq!(
            derive_agent_state("failed", None, Some("merge conflict while rebasing"), None),
            "blocked_merge_conflict"
        );
        assert_eq!(
            derive_agent_state(
                "failed",
                None,
                Some("transport interrupted after partial progress"),
                None
            ),
            "interrupted_transport"
        );
    }

    #[test]
    fn commit_provenance_is_extracted_from_agent_results() {
        let provenance = maybe_commit_provenance(Some("landed as commit deadbee with clean push"))
            .expect("commit provenance");
        assert_eq!(provenance.commit, "deadbee");
        assert_eq!(provenance.canonical_commit.as_deref(), Some("deadbee"));
        assert_eq!(provenance.lineage, vec!["deadbee".to_string()]);
    }
    #[test]
    fn lane_failure_taxonomy_normalizes_common_blockers() {
        let cases = [
            (
                "prompt delivery failed in tmux pane",
                LaneFailureClass::PromptDelivery,
            ),
            (
                "trust prompt is still blocking startup",
                LaneFailureClass::TrustGate,
            ),
            (
                "branch stale against main after divergence",
                LaneFailureClass::BranchDivergence,
            ),
            (
                "compile failed after cargo check",
                LaneFailureClass::Compile,
            ),
            ("targeted tests failed", LaneFailureClass::Test),
            ("plugin bootstrap failed", LaneFailureClass::PluginStartup),
            ("mcp handshake timed out", LaneFailureClass::McpHandshake),
            (
                "mcp startup failed before listing tools",
                LaneFailureClass::McpStartup,
            ),
            (
                "gateway routing rejected the request",
                LaneFailureClass::GatewayRouting,
            ),
            (
                "tool failed: denied tool execution from hook",
                LaneFailureClass::ToolRuntime,
            ),
            (
                "workspace mismatch while resuming the managed session",
                LaneFailureClass::WorkspaceMismatch,
            ),
            ("thread creation failed", LaneFailureClass::Infra),
        ];

        for (message, expected) in cases {
            assert_eq!(classify_lane_failure(message), expected, "{message}");
        }
    }

    #[test]
    fn lane_event_schema_serializes_to_canonical_names() {
        let cases = [
            (LaneEventName::Started, "lane.started"),
            (LaneEventName::Ready, "lane.ready"),
            (LaneEventName::PromptMisdelivery, "lane.prompt_misdelivery"),
            (LaneEventName::Blocked, "lane.blocked"),
            (LaneEventName::Red, "lane.red"),
            (LaneEventName::Green, "lane.green"),
            (LaneEventName::CommitCreated, "lane.commit.created"),
            (LaneEventName::PrOpened, "lane.pr.opened"),
            (LaneEventName::MergeReady, "lane.merge.ready"),
            (LaneEventName::Finished, "lane.finished"),
            (LaneEventName::Failed, "lane.failed"),
            (
                LaneEventName::BranchStaleAgainstMain,
                "branch.stale_against_main",
            ),
            (
                LaneEventName::BranchWorkspaceMismatch,
                "branch.workspace_mismatch",
            ),
        ];

        for (event, expected) in cases {
            assert_eq!(
                serde_json::to_value(event).expect("serialize lane event"),
                json!(expected)
            );
        }
    }

    #[test]
    fn agent_tool_subset_mapping_is_expected() {
        let general = allowed_tools_for_subagent("general-purpose");
        assert!(general.contains("bash"));
        assert!(general.contains("write_file"));
        assert!(!general.contains("Agent"));

        let explore = allowed_tools_for_subagent("Explore");
        assert!(explore.contains("read_file"));
        assert!(explore.contains("grep_search"));
        assert!(!explore.contains("bash"));

        let plan = allowed_tools_for_subagent("Plan");
        assert!(plan.contains("TodoWrite"));
        assert!(plan.contains("StructuredOutput"));
        assert!(!plan.contains("Agent"));

        let verification = allowed_tools_for_subagent("Verification");
        assert!(verification.contains("bash"));
        assert!(verification.contains("PowerShell"));
        assert!(!verification.contains("write_file"));
    }

    #[derive(Debug)]
    struct MockSubagentApiClient {
        calls: usize,
        input_path: String,
    }

    impl runtime::ApiClient for MockSubagentApiClient {
        fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.calls += 1;
            match self.calls {
                1 => {
                    assert_eq!(request.messages.len(), 1);
                    Ok(vec![
                        AssistantEvent::ToolUse {
                            id: "tool-1".to_string(),
                            name: "read_file".to_string(),
                            input: json!({ "path": self.input_path }).to_string(),
                        },
                        AssistantEvent::MessageStop,
                    ])
                }
                2 => {
                    assert!(request.messages.len() >= 3);
                    Ok(vec![
                        AssistantEvent::TextDelta("Scope: completed mock review".to_string()),
                        AssistantEvent::MessageStop,
                    ])
                }
                _ => unreachable!("extra mock stream call"),
            }
        }
    }

    #[test]
    fn subagent_runtime_executes_tool_loop_with_isolated_session() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = temp_path("subagent-input.txt");
        std::fs::write(&path, "hello from child").expect("write input file");

        let mut runtime = ConversationRuntime::new(
            Session::new(),
            MockSubagentApiClient {
                calls: 0,
                input_path: path.display().to_string(),
            },
            SubagentToolExecutor::new(BTreeSet::from([String::from("read_file")])),
            agent_permission_policy(),
            vec![String::from("system prompt")],
        );

        let summary = runtime
            .run_turn("Inspect the delegated file", None)
            .expect("subagent loop should succeed");

        assert_eq!(
            final_assistant_text(&summary),
            "Scope: completed mock review"
        );
        assert!(runtime
            .session()
            .messages
            .iter()
            .flat_map(|message| message.blocks.iter())
            .any(|block| matches!(
                block,
                runtime::ContentBlock::ToolResult { output, .. }
                    if output.contains("hello from child")
            )));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_rejects_blank_required_fields() {
        let missing_description = execute_tool(
            "Agent",
            &json!({
                "description": "  ",
                "prompt": "Inspect"
            }),
        )
        .expect_err("blank description should fail");
        assert!(missing_description.contains("description must not be empty"));

        let missing_prompt = execute_tool(
            "Agent",
            &json!({
                "description": "Inspect branch",
                "prompt": " "
            }),
        )
        .expect_err("blank prompt should fail");
        assert!(missing_prompt.contains("prompt must not be empty"));
    }

    #[test]
    fn notebook_edit_replaces_inserts_and_deletes_cells() {
        let path = temp_path("notebook.ipynb");
        std::fs::write(
            &path,
            r#"{
  "cells": [
    {"cell_type": "code", "id": "cell-a", "metadata": {}, "source": ["print(1)\n"], "outputs": [], "execution_count": null}
  ],
  "metadata": {"kernelspec": {"language": "python"}},
  "nbformat": 4,
  "nbformat_minor": 5
}"#,
        )
        .expect("write notebook");

        let replaced = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "new_source": "print(2)\n",
                "edit_mode": "replace"
            }),
        )
        .expect("NotebookEdit replace should succeed");
        let replaced_output: serde_json::Value = serde_json::from_str(&replaced).expect("json");
        assert_eq!(replaced_output["cell_id"], "cell-a");
        assert_eq!(replaced_output["cell_type"], "code");

        let inserted = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "new_source": "# heading\n",
                "cell_type": "markdown",
                "edit_mode": "insert"
            }),
        )
        .expect("NotebookEdit insert should succeed");
        let inserted_output: serde_json::Value = serde_json::from_str(&inserted).expect("json");
        assert_eq!(inserted_output["cell_type"], "markdown");
        let appended = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "new_source": "print(3)\n",
                "edit_mode": "insert"
            }),
        )
        .expect("NotebookEdit append should succeed");
        let appended_output: serde_json::Value = serde_json::from_str(&appended).expect("json");
        assert_eq!(appended_output["cell_type"], "code");

        let deleted = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "edit_mode": "delete"
            }),
        )
        .expect("NotebookEdit delete should succeed without new_source");
        let deleted_output: serde_json::Value = serde_json::from_str(&deleted).expect("json");
        assert!(deleted_output["cell_type"].is_null());
        assert_eq!(deleted_output["new_source"], "");

        let final_notebook: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read notebook"))
                .expect("valid notebook json");
        let cells = final_notebook["cells"].as_array().expect("cells array");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["cell_type"], "markdown");
        assert!(cells[0].get("outputs").is_none());
        assert_eq!(cells[1]["cell_type"], "code");
        assert_eq!(cells[1]["source"][0], "print(3)\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn notebook_edit_rejects_invalid_inputs() {
        let text_path = temp_path("notebook.txt");
        fs::write(&text_path, "not a notebook").expect("write text file");
        let wrong_extension = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": text_path.display().to_string(),
                "new_source": "print(1)\n"
            }),
        )
        .expect_err("non-ipynb file should fail");
        assert!(wrong_extension.contains("Jupyter notebook"));
        let _ = fs::remove_file(&text_path);

        let empty_notebook = temp_path("empty.ipynb");
        fs::write(
            &empty_notebook,
            r#"{"cells":[],"metadata":{"kernelspec":{"language":"python"}},"nbformat":4,"nbformat_minor":5}"#,
        )
        .expect("write empty notebook");

        let missing_source = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": empty_notebook.display().to_string(),
                "edit_mode": "insert"
            }),
        )
        .expect_err("insert without source should fail");
        assert!(missing_source.contains("new_source is required"));

        let missing_cell = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": empty_notebook.display().to_string(),
                "edit_mode": "delete"
            }),
        )
        .expect_err("delete on empty notebook should fail");
        assert!(missing_cell.contains("Notebook has no cells to edit"));
        let _ = fs::remove_file(empty_notebook);
    }

    #[test]
    #[cfg(not(windows))]
    fn bash_tool_reports_success_exit_failure_timeout_and_background() {
        let success = execute_tool("bash", &json!({ "command": "printf 'hello'" }))
            .expect("bash should succeed");
        let success_output: serde_json::Value = serde_json::from_str(&success).expect("json");
        assert_eq!(success_output["stdout"], "hello");
        assert_eq!(success_output["interrupted"], false);

        let failure = execute_tool("bash", &json!({ "command": "printf 'oops' >&2; exit 7" }))
            .expect("bash failure should still return structured output");
        let failure_output: serde_json::Value = serde_json::from_str(&failure).expect("json");
        assert_eq!(failure_output["returnCodeInterpretation"], "exit_code:7");
        assert!(failure_output["stderr"]
            .as_str()
            .expect("stderr")
            .contains("oops"));

        let timeout = execute_tool("bash", &json!({ "command": "sleep 1", "timeout": 10 }))
            .expect("bash timeout should return output");
        let timeout_output: serde_json::Value = serde_json::from_str(&timeout).expect("json");
        assert_eq!(timeout_output["interrupted"], true);
        assert_eq!(timeout_output["returnCodeInterpretation"], "timeout");
        assert!(timeout_output["stderr"]
            .as_str()
            .expect("stderr")
            .contains("Command exceeded timeout"));

        let background = execute_tool(
            "bash",
            &json!({ "command": "sleep 1", "run_in_background": true }),
        )
        .expect("bash background should succeed");
        let background_output: serde_json::Value = serde_json::from_str(&background).expect("json");
        assert!(background_output["backgroundTaskId"].as_str().is_some());
        assert_eq!(background_output["noOutputExpected"], true);
    }

    #[test]
    #[cfg(not(windows))]
    fn bash_workspace_tests_are_blocked_when_branch_is_behind_main() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("workspace-test-preflight");
        let original_dir = std::env::current_dir().expect("cwd");
        init_git_repo(&root);
        run_git(&root, &["checkout", "-b", "feature/stale-tests"]);
        run_git(&root, &["checkout", "main"]);
        commit_file(
            &root,
            "hotfix.txt",
            "fix from main\n",
            "fix: unblock workspace tests",
        );
        run_git(&root, &["checkout", "feature/stale-tests"]);
        std::env::set_current_dir(&root).expect("set cwd");

        let output = execute_tool(
            "bash",
            &json!({ "command": "cargo test --workspace --all-targets" }),
        )
        .expect("preflight should return structured output");
        let output_json: serde_json::Value = serde_json::from_str(&output).expect("json");
        assert_eq!(
            output_json["returnCodeInterpretation"],
            "preflight_blocked:branch_divergence"
        );
        assert!(output_json["stderr"]
            .as_str()
            .expect("stderr")
            .contains("branch divergence detected before workspace tests"));
        assert_eq!(
            output_json["structuredContent"][0]["event"],
            "branch.stale_against_main"
        );
        assert_eq!(
            output_json["structuredContent"][0]["failureClass"],
            "branch_divergence"
        );
        assert_eq!(
            output_json["structuredContent"][0]["data"]["missingCommits"][0],
            "fix: unblock workspace tests"
        );

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn bash_targeted_tests_skip_branch_preflight() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("targeted-test-no-preflight");
        let original_dir = std::env::current_dir().expect("cwd");
        init_git_repo(&root);
        run_git(&root, &["checkout", "-b", "feature/targeted-tests"]);
        run_git(&root, &["checkout", "main"]);
        commit_file(
            &root,
            "hotfix.txt",
            "fix from main\n",
            "fix: only broad tests should block",
        );
        run_git(&root, &["checkout", "feature/targeted-tests"]);
        std::env::set_current_dir(&root).expect("set cwd");

        let output = execute_tool(
            "bash",
            &json!({ "command": "printf 'targeted ok'; cargo test -p runtime stale_branch" }),
        )
        .expect("targeted commands should still execute");
        let output_json: serde_json::Value = serde_json::from_str(&output).expect("json");
        assert_ne!(
            output_json["returnCodeInterpretation"],
            "preflight_blocked:branch_divergence"
        );

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_tools_cover_read_write_and_edit_behaviors() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("fs-suite");
        fs::create_dir_all(&root).expect("create root");
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("set cwd");

        let write_create = execute_tool(
            "write_file",
            &json!({ "path": "nested/demo.txt", "content": "alpha\nbeta\nalpha\n" }),
        )
        .expect("write create should succeed");
        let write_create_output: serde_json::Value =
            serde_json::from_str(&write_create).expect("json");
        assert_eq!(write_create_output["type"], "create");
        assert!(root.join("nested/demo.txt").exists());

        let write_update = execute_tool(
            "write_file",
            &json!({ "path": "nested/demo.txt", "content": "alpha\nbeta\ngamma\n" }),
        )
        .expect("write update should succeed");
        let write_update_output: serde_json::Value =
            serde_json::from_str(&write_update).expect("json");
        assert_eq!(write_update_output["type"], "update");
        assert_eq!(write_update_output["originalFile"], "alpha\nbeta\nalpha\n");

        let read_full = execute_tool("read_file", &json!({ "path": "nested/demo.txt" }))
            .expect("read full should succeed");
        let read_full_output: serde_json::Value = serde_json::from_str(&read_full).expect("json");
        assert_eq!(read_full_output["file"]["content"], "alpha\nbeta\ngamma");
        assert_eq!(read_full_output["file"]["startLine"], 1);

        let read_slice = execute_tool(
            "read_file",
            &json!({ "path": "nested/demo.txt", "offset": 1, "limit": 1 }),
        )
        .expect("read slice should succeed");
        let read_slice_output: serde_json::Value = serde_json::from_str(&read_slice).expect("json");
        assert_eq!(read_slice_output["file"]["content"], "beta");
        assert_eq!(read_slice_output["file"]["startLine"], 2);

        let read_past_end = execute_tool(
            "read_file",
            &json!({ "path": "nested/demo.txt", "offset": 50 }),
        )
        .expect("read past EOF should succeed");
        let read_past_end_output: serde_json::Value =
            serde_json::from_str(&read_past_end).expect("json");
        assert_eq!(read_past_end_output["file"]["content"], "");
        assert_eq!(read_past_end_output["file"]["startLine"], 4);

        let read_error = execute_tool("read_file", &json!({ "path": "missing.txt" }))
            .expect_err("missing file should fail");
        assert!(!read_error.is_empty());

        let edit_once = execute_tool(
            "edit_file",
            &json!({ "path": "nested/demo.txt", "old_string": "alpha", "new_string": "omega" }),
        )
        .expect("single edit should succeed");
        let edit_once_output: serde_json::Value = serde_json::from_str(&edit_once).expect("json");
        assert_eq!(edit_once_output["replaceAll"], false);
        assert_eq!(
            fs::read_to_string(root.join("nested/demo.txt")).expect("read file"),
            "omega\nbeta\ngamma\n"
        );

        execute_tool(
            "write_file",
            &json!({ "path": "nested/demo.txt", "content": "alpha\nbeta\nalpha\n" }),
        )
        .expect("reset file");
        let edit_all = execute_tool(
            "edit_file",
            &json!({
                "path": "nested/demo.txt",
                "old_string": "alpha",
                "new_string": "omega",
                "replace_all": true
            }),
        )
        .expect("replace all should succeed");
        let edit_all_output: serde_json::Value = serde_json::from_str(&edit_all).expect("json");
        assert_eq!(edit_all_output["replaceAll"], true);
        assert_eq!(
            fs::read_to_string(root.join("nested/demo.txt")).expect("read file"),
            "omega\nbeta\nomega\n"
        );

        let edit_same = execute_tool(
            "edit_file",
            &json!({ "path": "nested/demo.txt", "old_string": "omega", "new_string": "omega" }),
        )
        .expect_err("identical old/new should fail");
        assert!(edit_same.contains("must differ"));

        let edit_missing = execute_tool(
            "edit_file",
            &json!({ "path": "nested/demo.txt", "old_string": "missing", "new_string": "omega" }),
        )
        .expect_err("missing substring should fail");
        assert!(edit_missing.contains("old_string not found"));

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn glob_and_grep_tools_cover_success_and_errors() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("search-suite");
        fs::create_dir_all(root.join("nested")).expect("create root");
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("set cwd");

        fs::write(
            root.join("nested/lib.rs"),
            "fn main() {}\nlet alpha = 1;\nlet alpha = 2;\n",
        )
        .expect("write rust file");
        fs::write(root.join("nested/notes.txt"), "alpha\nbeta\n").expect("write txt file");

        let globbed = execute_tool("glob_search", &json!({ "pattern": "nested/*.rs" }))
            .expect("glob should succeed");
        let globbed_output: serde_json::Value = serde_json::from_str(&globbed).expect("json");
        assert_eq!(globbed_output["numFiles"], 1);
        let filename = globbed_output["filenames"][0].as_str().expect("filename");
        assert!(
            filename.ends_with("nested/lib.rs") || filename.ends_with("nested\\lib.rs"),
            "filename={filename:?}"
        );

        let glob_error = execute_tool("glob_search", &json!({ "pattern": "[" }))
            .expect_err("invalid glob should fail");
        assert!(!glob_error.is_empty());

        let grep_content = execute_tool(
            "grep_search",
            &json!({
                "pattern": "alpha",
                "path": "nested",
                "glob": "*.rs",
                "output_mode": "content",
                "-n": true,
                "head_limit": 1,
                "offset": 1
            }),
        )
        .expect("grep content should succeed");
        let grep_content_output: serde_json::Value =
            serde_json::from_str(&grep_content).expect("json");
        assert_eq!(grep_content_output["numFiles"], 0);
        assert!(grep_content_output["appliedLimit"].is_null());
        assert_eq!(grep_content_output["appliedOffset"], 1);
        assert!(grep_content_output["content"]
            .as_str()
            .expect("content")
            .contains("let alpha = 2;"));

        let grep_count = execute_tool(
            "grep_search",
            &json!({ "pattern": "alpha", "path": "nested", "output_mode": "count" }),
        )
        .expect("grep count should succeed");
        let grep_count_output: serde_json::Value = serde_json::from_str(&grep_count).expect("json");
        assert_eq!(grep_count_output["numMatches"], 3);

        let grep_error = execute_tool(
            "grep_search",
            &json!({ "pattern": "(alpha", "path": "nested" }),
        )
        .expect_err("invalid regex should fail");
        assert!(!grep_error.is_empty());

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sleep_waits_and_reports_duration() {
        let started = std::time::Instant::now();
        let result =
            execute_tool("Sleep", &json!({"duration_ms": 20})).expect("Sleep should succeed");
        let elapsed = started.elapsed();
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["duration_ms"], 20);
        assert!(output["message"]
            .as_str()
            .expect("message")
            .contains("Slept for 20ms"));
        assert!(elapsed >= Duration::from_millis(15));
    }

    #[test]
    fn given_excessive_duration_when_sleep_then_rejects_with_error() {
        let result = execute_tool("Sleep", &json!({"duration_ms": 999_999_999_u64}));
        let error = result.expect_err("excessive sleep should fail");
        assert!(error.contains("exceeds maximum allowed sleep"));
    }

    #[test]
    fn given_zero_duration_when_sleep_then_succeeds() {
        let result =
            execute_tool("Sleep", &json!({"duration_ms": 0})).expect("0ms sleep should succeed");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["duration_ms"], 0);
    }

    #[test]
    fn brief_returns_sent_message_and_attachment_metadata() {
        let attachment = std::env::temp_dir().join(format!(
            "clawd-brief-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::write(&attachment, b"png-data").expect("write attachment");

        let result = execute_tool(
            "SendUserMessage",
            &json!({
                "message": "hello user",
                "attachments": [attachment.display().to_string()],
                "status": "normal"
            }),
        )
        .expect("SendUserMessage should succeed");

        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["message"], "hello user");
        assert!(output["sentAt"].as_str().is_some());
        assert_eq!(output["attachments"][0]["isImage"], true);
        let _ = std::fs::remove_file(attachment);
    }

    #[test]
    fn config_reads_and_writes_supported_values() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "clawd-config-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let home = root.join("home");
        let cwd = root.join("cwd");
        std::fs::create_dir_all(home.join(".claw")).expect("home dir");
        std::fs::create_dir_all(cwd.join(".claw")).expect("cwd dir");
        std::fs::write(
            home.join(".claw").join("settings.json"),
            r#"{"verbose":false}"#,
        )
        .expect("write global settings");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::set_current_dir(&cwd).expect("set cwd");

        let get = execute_tool("Config", &json!({"setting": "verbose"})).expect("get config");
        let get_output: serde_json::Value = serde_json::from_str(&get).expect("json");
        assert_eq!(get_output["value"], false);

        let set = execute_tool(
            "Config",
            &json!({"setting": "permissions.defaultMode", "value": "plan"}),
        )
        .expect("set config");
        let set_output: serde_json::Value = serde_json::from_str(&set).expect("json");
        assert_eq!(set_output["operation"], "set");
        assert_eq!(set_output["newValue"], "plan");

        let invalid = execute_tool(
            "Config",
            &json!({"setting": "permissions.defaultMode", "value": "bogus"}),
        )
        .expect_err("invalid config value should error");
        assert!(invalid.contains("Invalid value"));

        let unknown =
            execute_tool("Config", &json!({"setting": "nope"})).expect("unknown setting result");
        let unknown_output: serde_json::Value = serde_json::from_str(&unknown).expect("json");
        assert_eq!(unknown_output["success"], false);

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn enter_and_exit_plan_mode_round_trip_existing_local_override() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "clawd-plan-mode-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let home = root.join("home");
        let cwd = root.join("cwd");
        std::fs::create_dir_all(home.join(".claw")).expect("home dir");
        std::fs::create_dir_all(cwd.join(".claw")).expect("cwd dir");
        std::fs::write(
            cwd.join(".claw").join("settings.local.json"),
            r#"{"permissions":{"defaultMode":"acceptEdits"}}"#,
        )
        .expect("write local settings");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::set_current_dir(&cwd).expect("set cwd");

        let enter = execute_tool("EnterPlanMode", &json!({})).expect("enter plan mode");
        let enter_output: serde_json::Value = serde_json::from_str(&enter).expect("json");
        assert_eq!(enter_output["changed"], true);
        assert_eq!(enter_output["managed"], true);
        assert_eq!(enter_output["previousLocalMode"], "acceptEdits");
        assert_eq!(enter_output["currentLocalMode"], "plan");

        let local_settings = std::fs::read_to_string(cwd.join(".claw").join("settings.local.json"))
            .expect("local settings after enter");
        assert!(local_settings.contains(r#""defaultMode": "plan""#));
        let state =
            std::fs::read_to_string(cwd.join(".claw").join("tool-state").join("plan-mode.json"))
                .expect("plan mode state");
        assert!(state.contains(r#""hadLocalOverride": true"#));
        assert!(state.contains(r#""previousLocalMode": "acceptEdits""#));

        let exit = execute_tool("ExitPlanMode", &json!({})).expect("exit plan mode");
        let exit_output: serde_json::Value = serde_json::from_str(&exit).expect("json");
        assert_eq!(exit_output["changed"], true);
        assert_eq!(exit_output["managed"], false);
        assert_eq!(exit_output["previousLocalMode"], "acceptEdits");
        assert_eq!(exit_output["currentLocalMode"], "acceptEdits");

        let local_settings = std::fs::read_to_string(cwd.join(".claw").join("settings.local.json"))
            .expect("local settings after exit");
        assert!(local_settings.contains(r#""defaultMode": "acceptEdits""#));
        assert!(!cwd
            .join(".claw")
            .join("tool-state")
            .join("plan-mode.json")
            .exists());

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn exit_plan_mode_clears_override_when_enter_created_it_from_empty_local_state() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "clawd-plan-mode-empty-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let home = root.join("home");
        let cwd = root.join("cwd");
        std::fs::create_dir_all(home.join(".claw")).expect("home dir");
        std::fs::create_dir_all(cwd.join(".claw")).expect("cwd dir");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::set_current_dir(&cwd).expect("set cwd");

        let enter = execute_tool("EnterPlanMode", &json!({})).expect("enter plan mode");
        let enter_output: serde_json::Value = serde_json::from_str(&enter).expect("json");
        assert_eq!(enter_output["previousLocalMode"], serde_json::Value::Null);
        assert_eq!(enter_output["currentLocalMode"], "plan");

        let exit = execute_tool("ExitPlanMode", &json!({})).expect("exit plan mode");
        let exit_output: serde_json::Value = serde_json::from_str(&exit).expect("json");
        assert_eq!(exit_output["changed"], true);
        assert_eq!(exit_output["currentLocalMode"], serde_json::Value::Null);

        let local_settings = std::fs::read_to_string(cwd.join(".claw").join("settings.local.json"))
            .expect("local settings after exit");
        let local_settings_json: serde_json::Value =
            serde_json::from_str(&local_settings).expect("valid settings json");
        assert_eq!(
            local_settings_json.get("permissions"),
            None,
            "permissions override should be removed on exit"
        );
        assert!(!cwd
            .join(".claw")
            .join("tool-state")
            .join("plan-mode.json")
            .exists());

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn structured_output_echoes_input_payload() {
        let result = execute_tool("StructuredOutput", &json!({"ok": true, "items": [1, 2, 3]}))
            .expect("StructuredOutput should succeed");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["data"], "Structured output provided successfully");
        assert_eq!(output["structured_output"]["ok"], true);
        assert_eq!(output["structured_output"]["items"][1], 2);
    }

    #[test]
    fn given_empty_payload_when_structured_output_then_rejects_with_error() {
        let result = execute_tool("StructuredOutput", &json!({}));
        let error = result.expect_err("empty payload should fail");
        assert!(error.contains("must not be empty"));
    }

    #[test]
    #[cfg(not(windows))]
    fn repl_executes_python_code() {
        let result = execute_tool(
            "REPL",
            &json!({"language": "python", "code": "print(1 + 1)", "timeout_ms": 500}),
        )
        .expect("REPL should succeed");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["language"], "python");
        assert_eq!(output["exitCode"], 0);
        assert!(output["stdout"].as_str().expect("stdout").contains('2'));
    }

    #[test]
    fn given_empty_code_when_repl_then_rejects_with_error() {
        let result = execute_tool("REPL", &json!({"language": "python", "code": "   "}));

        let error = result.expect_err("empty REPL code should fail");
        assert!(error.contains("code must not be empty"));
    }

    #[test]
    fn given_unsupported_language_when_repl_then_rejects_with_error() {
        let result = execute_tool("REPL", &json!({"language": "ruby", "code": "puts 1"}));

        let error = result.expect_err("unsupported REPL language should fail");
        assert!(error.contains("unsupported REPL language: ruby"));
    }

    #[test]
    #[cfg(not(windows))]
    fn given_timeout_ms_when_repl_blocks_then_returns_timeout_error() {
        let result = execute_tool(
            "REPL",
            &json!({
                "language": "python",
                "code": "import time\ntime.sleep(1)",
                "timeout_ms": 10
            }),
        );

        let error = result.expect_err("timed out REPL execution should fail");
        assert!(error.contains("REPL execution exceeded timeout of 10 ms"));
    }

    #[test]
    #[cfg(not(windows))]
    fn powershell_runs_via_stub_shell() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "clawd-pwsh-bin-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let script = dir.join("pwsh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
while [ "$1" != "-Command" ] && [ $# -gt 0 ]; do shift; done
shift
printf 'pwsh:%s' "$1"
"#,
        )
        .expect("write script");
        std::process::Command::new("/bin/chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .expect("chmod");
        let original_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", dir.display(), original_path));

        let result = execute_tool(
            "PowerShell",
            &json!({"command": "Write-Output hello", "timeout": 1000}),
        )
        .expect("PowerShell should succeed");

        let background = execute_tool(
            "PowerShell",
            &json!({"command": "Write-Output hello", "run_in_background": true}),
        )
        .expect("PowerShell background should succeed");

        std::env::set_var("PATH", original_path);
        let _ = std::fs::remove_dir_all(dir);

        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["stdout"], "pwsh:Write-Output hello");
        assert!(output["stderr"].as_str().expect("stderr").is_empty());

        let background_output: serde_json::Value = serde_json::from_str(&background).expect("json");
        assert!(background_output["backgroundTaskId"].as_str().is_some());
        assert_eq!(background_output["backgroundedByUser"], true);
        assert_eq!(background_output["assistantAutoBackgrounded"], false);
    }

    #[test]
    fn powershell_errors_when_shell_is_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_path = std::env::var("PATH").unwrap_or_default();
        let empty_dir = std::env::temp_dir().join(format!(
            "clawd-empty-bin-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&empty_dir).expect("create empty dir");
        std::env::set_var("PATH", empty_dir.display().to_string());

        let err = execute_tool("PowerShell", &json!({"command": "Write-Output hello"}))
            .expect_err("PowerShell should fail when shell is missing");

        std::env::set_var("PATH", original_path);
        let _ = std::fs::remove_dir_all(empty_dir);

        assert!(err.contains("PowerShell executable not found"));
    }

    fn read_only_registry() -> super::GlobalToolRegistry {
        use runtime::permission_enforcer::PermissionEnforcer;
        use runtime::PermissionPolicy;

        let policy = mvp_tool_specs().into_iter().fold(
            PermissionPolicy::new(runtime::PermissionMode::ReadOnly),
            |policy, spec| policy.with_tool_requirement(spec.name, spec.required_permission),
        );
        let mut registry = super::GlobalToolRegistry::builtin();
        registry.set_enforcer(PermissionEnforcer::new(policy));
        registry
    }

    #[test]
    fn given_read_only_enforcer_when_bash_then_denied() {
        let registry = read_only_registry();
        // Use a command that requires DangerFullAccess (rm) to ensure it's blocked in read-only mode
        let err = registry
            .execute("bash", &json!({ "command": "rm -rf /" }))
            .expect_err("bash should be denied in read-only mode");
        assert!(
            err.contains("current mode is 'read-only'"),
            "should cite active mode: {err}"
        );
    }

    #[test]
    fn given_read_only_enforcer_when_write_file_then_denied() {
        let registry = read_only_registry();
        let err = registry
            .execute(
                "write_file",
                &json!({ "path": "/tmp/x.txt", "content": "x" }),
            )
            .expect_err("write_file should be denied in read-only mode");
        assert!(
            err.contains("current mode is read-only"),
            "should cite active mode: {err}"
        );
    }

    #[test]
    fn given_read_only_enforcer_when_edit_file_then_denied() {
        let registry = read_only_registry();
        let err = registry
            .execute(
                "edit_file",
                &json!({ "path": "/tmp/x.txt", "old_string": "a", "new_string": "b" }),
            )
            .expect_err("edit_file should be denied in read-only mode");
        assert!(
            err.contains("current mode is read-only"),
            "should cite active mode: {err}"
        );
    }

    #[test]
    fn given_read_only_enforcer_when_read_file_then_not_permission_denied() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("perm-read");
        fs::create_dir_all(&root).expect("create root");
        let file = root.join("readable.txt");
        fs::write(&file, "content\n").expect("write test file");

        let registry = read_only_registry();
        let result = registry.execute("read_file", &json!({ "path": file.display().to_string() }));
        assert!(result.is_ok(), "read_file should be allowed: {result:?}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn given_read_only_enforcer_when_glob_search_then_not_permission_denied() {
        let registry = read_only_registry();
        let result = registry.execute("glob_search", &json!({ "pattern": "*.rs" }));
        assert!(
            result.is_ok(),
            "glob_search should be allowed in read-only mode: {result:?}"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn given_no_enforcer_when_bash_then_executes_normally() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let registry = super::GlobalToolRegistry::builtin();
        let result = registry
            .execute("bash", &json!({ "command": "printf 'ok'" }))
            .expect("bash should succeed without enforcer");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["stdout"], "ok");
    }

    #[test]
    fn provider_runtime_client_chain_uses_only_primary_when_no_fallbacks_configured() {
        // given
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "anthropic-test-key");
        let fallback_config = ProviderFallbackConfig::default();

        // when
        let client = ProviderRuntimeClient::new_with_fallback_config(
            "claude-sonnet-4-6".to_string(),
            BTreeSet::new(),
            &fallback_config,
        )
        .expect("primary-only chain should construct");

        // then
        assert_eq!(client.chain.len(), 1);
        assert_eq!(client.chain[0].model, "claude-sonnet-4-6");

        match original_anthropic {
            Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }

    #[test]
    fn provider_runtime_client_chain_appends_configured_fallbacks_in_order() {
        // given
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
        let original_xai = std::env::var_os("XAI_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "anthropic-test-key");
        std::env::set_var("XAI_API_KEY", "xai-test-key");
        let fallback_config = ProviderFallbackConfig::new(
            None,
            vec!["grok-3".to_string(), "grok-3-mini".to_string()],
        );

        // when
        let client = ProviderRuntimeClient::new_with_fallback_config(
            "claude-sonnet-4-6".to_string(),
            BTreeSet::new(),
            &fallback_config,
        )
        .expect("chain with fallbacks should construct");

        // then
        assert_eq!(client.chain.len(), 3);
        assert_eq!(client.chain[0].model, "claude-sonnet-4-6");
        assert_eq!(client.chain[1].model, "grok-3");
        assert_eq!(client.chain[2].model, "grok-3-mini");

        match original_anthropic {
            Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        match original_xai {
            Some(value) => std::env::set_var("XAI_API_KEY", value),
            None => std::env::remove_var("XAI_API_KEY"),
        }
    }

    #[test]
    fn provider_runtime_client_chain_primary_override_replaces_constructor_model() {
        // given
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
        let original_xai = std::env::var_os("XAI_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "anthropic-test-key");
        std::env::set_var("XAI_API_KEY", "xai-test-key");
        let fallback_config = ProviderFallbackConfig::new(
            Some("grok-3".to_string()),
            vec!["claude-sonnet-4-6".to_string()],
        );

        // when
        let client = ProviderRuntimeClient::new_with_fallback_config(
            "claude-haiku-4-5-20251213".to_string(),
            BTreeSet::new(),
            &fallback_config,
        )
        .expect("chain with primary override should construct");

        // then
        assert_eq!(client.chain.len(), 2);
        assert_eq!(client.chain[0].model, "grok-3");
        assert_eq!(client.chain[1].model, "claude-sonnet-4-6");

        match original_anthropic {
            Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        match original_xai {
            Some(value) => std::env::set_var("XAI_API_KEY", value),
            None => std::env::remove_var("XAI_API_KEY"),
        }
    }

    #[test]
    fn provider_runtime_client_chain_skips_fallbacks_missing_credentials() {
        // given
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
        let original_xai = std::env::var_os("XAI_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "anthropic-test-key");
        std::env::remove_var("XAI_API_KEY");
        let fallback_config = ProviderFallbackConfig::new(
            None,
            vec![
                "grok-3".to_string(),
                "claude-haiku-4-5-20251213".to_string(),
            ],
        );

        // when
        let client = ProviderRuntimeClient::new_with_fallback_config(
            "claude-sonnet-4-6".to_string(),
            BTreeSet::new(),
            &fallback_config,
        )
        .expect("chain construction should not fail when only some fallbacks are unavailable");

        // then
        assert_eq!(client.chain.len(), 2);
        assert_eq!(client.chain[0].model, "claude-sonnet-4-6");
        assert_eq!(client.chain[1].model, "claude-haiku-4-5-20251213");

        match original_anthropic {
            Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        if let Some(value) = original_xai {
            std::env::set_var("XAI_API_KEY", value);
        }
    }

    #[test]
    fn run_task_packet_creates_packet_backed_task() {
        use runtime::task_packet::TaskScope;
        let result = run_task_packet(TaskPacket {
            objective: "Ship packetized runtime task".to_string(),
            scope: TaskScope::Module,
            scope_path: Some("runtime/task system".to_string()),
            worktree: Some("/tmp/wt-packet".to_string()),
            repo: "claw-code-parity".to_string(),
            branch_policy: "origin/main only".to_string(),
            acceptance_tests: vec![
                "cargo build --workspace".to_string(),
                "cargo test --workspace".to_string(),
            ],
            commit_policy: "single commit".to_string(),
            reporting_contract: "print build/test result and sha".to_string(),
            escalation_policy: "manual escalation".to_string(),
        })
        .expect("task packet should create a task");

        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["status"], "created");
        assert_eq!(output["prompt"], "Ship packetized runtime task");
        assert_eq!(output["description"], "runtime/task system");
        assert_eq!(output["task_packet"]["repo"], "claw-code-parity");
        assert_eq!(
            output["task_packet"]["acceptance_tests"][1],
            "cargo test --workspace"
        );
    }

    struct TestServer {
        addr: SocketAddr,
        shutdown: Option<std::sync::mpsc::Sender<()>>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn spawn(handler: Arc<dyn Fn(&str) -> HttpResponse + Send + Sync + 'static>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
            listener
                .set_nonblocking(true)
                .expect("set nonblocking listener");
            let addr = listener.local_addr().expect("local addr");
            let (tx, rx) = std::sync::mpsc::channel::<()>();

            let handle = thread::spawn(move || loop {
                if rx.try_recv().is_ok() {
                    break;
                }

                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buffer = [0_u8; 4096];
                        let size = stream.read(&mut buffer).expect("read request");
                        let request = String::from_utf8_lossy(&buffer[..size]).into_owned();
                        let request_line = request.lines().next().unwrap_or_default().to_string();
                        let response = handler(&request_line);
                        stream
                            .write_all(response.to_bytes().as_slice())
                            .expect("write response");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("server accept failed: {error}"),
                }
            });

            Self {
                addr,
                shutdown: Some(tx),
                handle: Some(handle),
            }
        }

        fn addr(&self) -> SocketAddr {
            self.addr
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = self.handle.take() {
                handle.join().expect("join test server");
            }
        }
    }

    struct HttpResponse {
        status: u16,
        reason: &'static str,
        content_type: &'static str,
        body: String,
    }

    impl HttpResponse {
        fn html(status: u16, reason: &'static str, body: &str) -> Self {
            Self {
                status,
                reason,
                content_type: "text/html; charset=utf-8",
                body: body.to_string(),
            }
        }

        fn text(status: u16, reason: &'static str, body: &str) -> Self {
            Self {
                status,
                reason,
                content_type: "text/plain; charset=utf-8",
                body: body.to_string(),
            }
        }

        fn to_bytes(&self) -> Vec<u8> {
            format!(
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                self.status,
                self.reason,
                self.content_type,
                self.body.len(),
                self.body
            )
            .into_bytes()
        }
    }
}
