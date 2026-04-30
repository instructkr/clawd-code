use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use plugins::{PluginError, PluginLoadFailure, PluginManager, PluginSummary};
use runtime::{
    compact_session, CompactionConfig, ConfigLoader, ConfigSource, McpOAuthConfig, McpServerConfig,
    ScopedMcpServerConfig, Session,
};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandManifestEntry {
    pub name: String,
    pub source: CommandSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    Builtin,
    InternalOnly,
    FeatureGated,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandRegistry {
    entries: Vec<CommandManifestEntry>,
}

impl CommandRegistry {
    #[must_use]
    pub fn new(entries: Vec<CommandManifestEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[CommandManifestEntry] {
        &self.entries
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub summary: &'static str,
    pub argument_hint: Option<&'static str>,
    pub resume_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSlashDispatch {
    Local,
    Invoke(String),
}

const SLASH_COMMAND_SPECS: &[SlashCommandSpec] = &[
    SlashCommandSpec {
        name: "help",
        aliases: &[],
        summary: "Show available slash commands",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "status",
        aliases: &[],
        summary: "Show current session status",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "sandbox",
        aliases: &[],
        summary: "Show sandbox isolation status",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "compact",
        aliases: &[],
        summary: "Compact local session history",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "model",
        aliases: &[],
        summary: "Show or switch the active model",
        argument_hint: Some("[model]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "permissions",
        aliases: &[],
        summary: "Show or switch the active permission mode",
        argument_hint: Some("[read-only|workspace-write|danger-full-access]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "clear",
        aliases: &[],
        summary: "Start a fresh local session",
        argument_hint: Some("[--confirm]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "cost",
        aliases: &[],
        summary: "Show cumulative token usage for this session",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "resume",
        aliases: &[],
        summary: "Load a saved session into the REPL",
        argument_hint: Some("<session-path>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "config",
        aliases: &[],
        summary: "Inspect Claude config files or merged sections",
        argument_hint: Some("[env|hooks|model|plugins]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "mcp",
        aliases: &[],
        summary: "Inspect configured MCP servers",
        argument_hint: Some("[list|show <server>|help]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "memory",
        aliases: &[],
        summary: "Inspect loaded Claude instruction memory files",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "init",
        aliases: &[],
        summary: "Create a starter CLAUDE.md for this repo",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "diff",
        aliases: &[],
        summary: "Show git diff for current workspace changes",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "version",
        aliases: &[],
        summary: "Show CLI version and build information",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "bughunter",
        aliases: &[],
        summary: "Inspect the codebase for likely bugs",
        argument_hint: Some("[scope]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "commit",
        aliases: &[],
        summary: "Generate a commit message and create a git commit",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "pr",
        aliases: &[],
        summary: "Draft or create a pull request from the conversation",
        argument_hint: Some("[context]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "issue",
        aliases: &[],
        summary: "Draft or create a GitHub issue from the conversation",
        argument_hint: Some("[context]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "ultraplan",
        aliases: &[],
        summary: "Run a deep planning prompt with multi-step reasoning",
        argument_hint: Some("[task]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "teleport",
        aliases: &[],
        summary: "Jump to a file or symbol by searching the workspace",
        argument_hint: Some("<symbol-or-path>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "debug-tool-call",
        aliases: &[],
        summary: "Replay the last tool call with debug details",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "export",
        aliases: &[],
        summary: "Export the current conversation to a file",
        argument_hint: Some("[file]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "session",
        aliases: &[],
        summary: "List, switch, fork, or delete managed local sessions",
        argument_hint: Some(
            "[list|switch <session-id>|fork [branch-name]|delete <session-id> [--force]]",
        ),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "plugin",
        aliases: &["plugins", "marketplace"],
        summary: "Manage Claw Code plugins",
        argument_hint: Some(
            "[list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]",
        ),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "agents",
        aliases: &[],
        summary: "List configured agents",
        argument_hint: Some("[list|help]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "skills",
        aliases: &["skill"],
        summary: "List, install, or invoke available skills",
        argument_hint: Some("[list|install <path>|help|<skill> [args]]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "doctor",
        aliases: &[],
        summary: "Diagnose setup issues and environment health",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "plan",
        aliases: &[],
        summary: "Toggle or inspect planning mode",
        argument_hint: Some("[on|off]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "review",
        aliases: &[],
        summary: "Run a code review on current changes",
        argument_hint: Some("[scope]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "tasks",
        aliases: &[],
        summary: "List and manage background tasks",
        argument_hint: Some("[list|get <id>|stop <id>]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "theme",
        aliases: &[],
        summary: "Switch the terminal color theme",
        argument_hint: Some("[theme-name]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "vim",
        aliases: &[],
        summary: "Toggle vim keybinding mode",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "voice",
        aliases: &[],
        summary: "Toggle voice input mode",
        argument_hint: Some("[on|off]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "upgrade",
        aliases: &[],
        summary: "Check for and install CLI updates",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "usage",
        aliases: &[],
        summary: "Show detailed API usage statistics",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "stats",
        aliases: &[],
        summary: "Show workspace and session statistics",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "rename",
        aliases: &[],
        summary: "Rename the current session",
        argument_hint: Some("<name>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "copy",
        aliases: &[],
        summary: "Copy conversation or output to clipboard",
        argument_hint: Some("[last|all]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "share",
        aliases: &[],
        summary: "Share the current conversation",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "feedback",
        aliases: &[],
        summary: "Submit feedback about the current session",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "hooks",
        aliases: &[],
        summary: "List and manage lifecycle hooks",
        argument_hint: Some("[list|run <hook>]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "files",
        aliases: &[],
        summary: "List files in the current context window",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "context",
        aliases: &[],
        summary: "Inspect or manage the conversation context",
        argument_hint: Some("[show|clear]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "color",
        aliases: &[],
        summary: "Configure terminal color settings",
        argument_hint: Some("[scheme]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "effort",
        aliases: &[],
        summary: "Set the effort level for responses",
        argument_hint: Some("[low|medium|high]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "fast",
        aliases: &[],
        summary: "Toggle fast/concise response mode",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "exit",
        aliases: &[],
        summary: "Exit the REPL session",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "branch",
        aliases: &[],
        summary: "Create or switch git branches",
        argument_hint: Some("[name]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "rewind",
        aliases: &[],
        summary: "Rewind the conversation to a previous state",
        argument_hint: Some("[steps]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "summary",
        aliases: &[],
        summary: "Generate a summary of the conversation",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "desktop",
        aliases: &[],
        summary: "Open or manage the desktop app integration",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "ide",
        aliases: &[],
        summary: "Open or configure IDE integration",
        argument_hint: Some("[vscode|cursor]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "tag",
        aliases: &[],
        summary: "Tag the current conversation point",
        argument_hint: Some("[label]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "brief",
        aliases: &[],
        summary: "Toggle brief output mode",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "advisor",
        aliases: &[],
        summary: "Toggle advisor mode for guidance-only responses",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "stickers",
        aliases: &[],
        summary: "Browse and manage sticker packs",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "insights",
        aliases: &[],
        summary: "Show AI-generated insights about the session",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "thinkback",
        aliases: &[],
        summary: "Replay the thinking process of the last response",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "release-notes",
        aliases: &[],
        summary: "Generate release notes from recent changes",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "security-review",
        aliases: &[],
        summary: "Run a security review on the codebase",
        argument_hint: Some("[scope]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "keybindings",
        aliases: &[],
        summary: "Show or configure keyboard shortcuts",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "privacy-settings",
        aliases: &[],
        summary: "View or modify privacy settings",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "output-style",
        aliases: &[],
        summary: "Switch output formatting style",
        argument_hint: Some("[style]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "add-dir",
        aliases: &[],
        summary: "Add an additional directory to the context",
        argument_hint: Some("<path>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "allowed-tools",
        aliases: &[],
        summary: "Show or modify the allowed tools list",
        argument_hint: Some("[add|remove|list] [tool]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "api-key",
        aliases: &[],
        summary: "Show or set the Anthropic API key",
        argument_hint: Some("[key]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "approve",
        aliases: &["yes", "y"],
        summary: "Approve a pending tool execution",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "deny",
        aliases: &["no", "n"],
        summary: "Deny a pending tool execution",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "undo",
        aliases: &[],
        summary: "Undo the last file write or edit",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "stop",
        aliases: &[],
        summary: "Stop the current generation",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "retry",
        aliases: &[],
        summary: "Retry the last failed message",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "paste",
        aliases: &[],
        summary: "Paste clipboard content as input",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "screenshot",
        aliases: &[],
        summary: "Take a screenshot and add to conversation",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "image",
        aliases: &[],
        summary: "Add an image file to the conversation",
        argument_hint: Some("<path>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "terminal-setup",
        aliases: &[],
        summary: "Configure terminal integration settings",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "search",
        aliases: &[],
        summary: "Search files in the workspace",
        argument_hint: Some("<query>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "listen",
        aliases: &[],
        summary: "Listen for voice input",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "speak",
        aliases: &[],
        summary: "Read the last response aloud",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "language",
        aliases: &[],
        summary: "Set the interface language",
        argument_hint: Some("[language]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "profile",
        aliases: &[],
        summary: "Show or switch user profile",
        argument_hint: Some("[name]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "max-tokens",
        aliases: &[],
        summary: "Show or set the max output tokens",
        argument_hint: Some("[count]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "temperature",
        aliases: &[],
        summary: "Show or set the sampling temperature",
        argument_hint: Some("[value]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "system-prompt",
        aliases: &[],
        summary: "Show the active system prompt",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "tool-details",
        aliases: &[],
        summary: "Show detailed info about a specific tool",
        argument_hint: Some("<tool-name>"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "format",
        aliases: &[],
        summary: "Format the last response in a different style",
        argument_hint: Some("[markdown|plain|json]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "pin",
        aliases: &[],
        summary: "Pin a message to persist across compaction",
        argument_hint: Some("[message-index]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "unpin",
        aliases: &[],
        summary: "Unpin a previously pinned message",
        argument_hint: Some("[message-index]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "bookmarks",
        aliases: &[],
        summary: "List or manage conversation bookmarks",
        argument_hint: Some("[add|remove|list]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "workspace",
        aliases: &["cwd"],
        summary: "Show or change the working directory",
        argument_hint: Some("[path]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "history",
        aliases: &[],
        summary: "Show conversation history summary",
        argument_hint: Some("[count]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "tokens",
        aliases: &[],
        summary: "Show token count for the current conversation",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "cache",
        aliases: &[],
        summary: "Show prompt cache statistics",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "providers",
        aliases: &[],
        summary: "List available model providers",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "notifications",
        aliases: &[],
        summary: "Show or configure notification settings",
        argument_hint: Some("[on|off|status]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "changelog",
        aliases: &[],
        summary: "Show recent changes to the codebase",
        argument_hint: Some("[count]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "test",
        aliases: &[],
        summary: "Run tests for the current project",
        argument_hint: Some("[filter]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "lint",
        aliases: &[],
        summary: "Run linting for the current project",
        argument_hint: Some("[filter]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "build",
        aliases: &[],
        summary: "Build the current project",
        argument_hint: Some("[target]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "run",
        aliases: &[],
        summary: "Run a command in the project context",
        argument_hint: Some("<command>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "git",
        aliases: &[],
        summary: "Run a git command in the workspace",
        argument_hint: Some("<subcommand>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "stash",
        aliases: &[],
        summary: "Stash or unstash workspace changes",
        argument_hint: Some("[pop|list|apply]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "blame",
        aliases: &[],
        summary: "Show git blame for a file",
        argument_hint: Some("<file> [line]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "log",
        aliases: &[],
        summary: "Show git log for the workspace",
        argument_hint: Some("[count]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "cron",
        aliases: &[],
        summary: "Manage scheduled tasks",
        argument_hint: Some("[list|add|remove]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "team",
        aliases: &[],
        summary: "Manage agent teams",
        argument_hint: Some("[list|create|delete]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "benchmark",
        aliases: &[],
        summary: "Run performance benchmarks",
        argument_hint: Some("[suite]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "migrate",
        aliases: &[],
        summary: "Run pending data migrations",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "reset",
        aliases: &[],
        summary: "Reset configuration to defaults",
        argument_hint: Some("[section]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "telemetry",
        aliases: &[],
        summary: "Show or configure telemetry settings",
        argument_hint: Some("[on|off|status]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "env",
        aliases: &[],
        summary: "Show environment variables visible to tools",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "project",
        aliases: &[],
        summary: "Show project detection info",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "templates",
        aliases: &[],
        summary: "List or apply prompt templates",
        argument_hint: Some("[list|apply <name>]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "explain",
        aliases: &[],
        summary: "Explain a file or code snippet",
        argument_hint: Some("<path> [line-range]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "refactor",
        aliases: &[],
        summary: "Suggest refactoring for a file or function",
        argument_hint: Some("<path> [scope]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "docs",
        aliases: &[],
        summary: "Generate or show documentation",
        argument_hint: Some("[path]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "fix",
        aliases: &[],
        summary: "Fix errors in a file or project",
        argument_hint: Some("[path]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "perf",
        aliases: &[],
        summary: "Analyze performance of a function or file",
        argument_hint: Some("<path>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "chat",
        aliases: &[],
        summary: "Switch to free-form chat mode",
        argument_hint: None,
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "focus",
        aliases: &[],
        summary: "Focus context on specific files or directories",
        argument_hint: Some("<path> [path...]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "unfocus",
        aliases: &[],
        summary: "Remove focus from files or directories",
        argument_hint: Some("[path...]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "web",
        aliases: &[],
        summary: "Fetch and summarize a web page",
        argument_hint: Some("<url>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "map",
        aliases: &[],
        summary: "Show a visual map of the codebase structure",
        argument_hint: Some("[depth]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "symbols",
        aliases: &[],
        summary: "List symbols (functions, classes, etc.) in a file",
        argument_hint: Some("<path>"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "references",
        aliases: &[],
        summary: "Find all references to a symbol",
        argument_hint: Some("<symbol>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "definition",
        aliases: &[],
        summary: "Go to the definition of a symbol",
        argument_hint: Some("<symbol>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "hover",
        aliases: &[],
        summary: "Show hover information for a symbol",
        argument_hint: Some("<symbol>"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "diagnostics",
        aliases: &[],
        summary: "Show LSP diagnostics for a file",
        argument_hint: Some("[path]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "autofix",
        aliases: &[],
        summary: "Auto-fix all fixable diagnostics",
        argument_hint: Some("[path]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "multi",
        aliases: &[],
        summary: "Execute multiple slash commands in sequence",
        argument_hint: Some("<commands>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "macro",
        aliases: &[],
        summary: "Record or replay command macros",
        argument_hint: Some("[record|stop|play <name>]"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "alias",
        aliases: &[],
        summary: "Create a command alias",
        argument_hint: Some("<name> <command>"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "parallel",
        aliases: &[],
        summary: "Run commands in parallel subagents",
        argument_hint: Some("<count> <prompt>"),
        resume_supported: false,
    },
    SlashCommandSpec {
        name: "agent",
        aliases: &[],
        summary: "Manage sub-agents and spawned sessions",
        argument_hint: Some("[list|spawn|kill]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "subagent",
        aliases: &[],
        summary: "Control active subagent execution",
        argument_hint: Some("[list|steer <target> <msg>|kill <id>]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "reasoning",
        aliases: &[],
        summary: "Toggle extended reasoning mode",
        argument_hint: Some("[on|off|stream]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "budget",
        aliases: &[],
        summary: "Show or set token budget limits",
        argument_hint: Some("[show|set <limit>]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "rate-limit",
        aliases: &[],
        summary: "Configure API rate limiting",
        argument_hint: Some("[status|set <rpm>]"),
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "metrics",
        aliases: &[],
        summary: "Show performance and usage metrics",
        argument_hint: None,
        resume_supported: true,
    },
    SlashCommandSpec {
        name: "lsp",
        aliases: &[],
        summary: "Show or manage LSP server status",
        argument_hint: Some("[start|stop|restart <language>]"),
        resume_supported: true,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Help,
    Status,
    Sandbox,
    Compact,
    Bughunter {
        scope: Option<String>,
    },
    Commit,
    Pr {
        context: Option<String>,
    },
    Issue {
        context: Option<String>,
    },
    Ultraplan {
        task: Option<String>,
    },
    Teleport {
        target: Option<String>,
    },
    DebugToolCall,
    Model {
        model: Option<String>,
    },
    Permissions {
        mode: Option<String>,
    },
    Clear {
        confirm: bool,
    },
    Cost,
    Resume {
        session_path: Option<String>,
    },
    Config {
        section: Option<String>,
    },
    Mcp {
        action: Option<String>,
        target: Option<String>,
    },
    Memory,
    Init,
    Diff,
    Version,
    Export {
        path: Option<String>,
    },
    Session {
        action: Option<String>,
        target: Option<String>,
    },
    Plugins {
        action: Option<String>,
        target: Option<String>,
    },
    Agents {
        args: Option<String>,
    },
    Skills {
        args: Option<String>,
    },
    Doctor,
    Login,
    Logout,
    Vim,
    Upgrade,
    Stats,
    Share,
    Feedback,
    Files,
    Fast,
    Exit,
    Summary,
    Desktop,
    Brief,
    Advisor,
    Stickers,
    Insights,
    Thinkback,
    ReleaseNotes,
    SecurityReview,
    Keybindings,
    PrivacySettings,
    Plan {
        mode: Option<String>,
    },
    Review {
        scope: Option<String>,
    },
    Tasks {
        args: Option<String>,
    },
    Theme {
        name: Option<String>,
    },
    Voice {
        mode: Option<String>,
    },
    Usage {
        scope: Option<String>,
    },
    Rename {
        name: Option<String>,
    },
    Copy {
        target: Option<String>,
    },
    Hooks {
        args: Option<String>,
    },
    Context {
        action: Option<String>,
    },
    Color {
        scheme: Option<String>,
    },
    Effort {
        level: Option<String>,
    },
    Branch {
        name: Option<String>,
    },
    Rewind {
        steps: Option<String>,
    },
    Ide {
        target: Option<String>,
    },
    Tag {
        label: Option<String>,
    },
    OutputStyle {
        style: Option<String>,
    },
    AddDir {
        path: Option<String>,
    },
    History {
        count: Option<String>,
    },
    Lsp {
        action: Option<String>,
        target: Option<String>,
    },
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandParseError {
    message: String,
}

impl SlashCommandParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SlashCommandParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SlashCommandParseError {}

impl SlashCommand {
    pub fn parse(input: &str) -> Result<Option<Self>, SlashCommandParseError> {
        validate_slash_command_input(input)
    }

    /// Returns the canonical slash-command name (e.g. `"/branch"`) for use in
    /// error messages and logging. Derived from the spec table so it always
    /// matches what the user would have typed.
    #[must_use]
    pub fn slash_name(&self) -> &'static str {
        match self {
            Self::Help => "/help",
            Self::Clear { .. } => "/clear",
            Self::Compact { .. } => "/compact",
            Self::Cost => "/cost",
            Self::Doctor => "/doctor",
            Self::Config { .. } => "/config",
            Self::Memory { .. } => "/memory",
            Self::History { .. } => "/history",
            Self::Diff => "/diff",
            Self::Status => "/status",
            Self::Stats => "/stats",
            Self::Version => "/version",
            Self::Commit { .. } => "/commit",
            Self::Pr { .. } => "/pr",
            Self::Issue { .. } => "/issue",
            Self::Init => "/init",
            Self::Bughunter { .. } => "/bughunter",
            Self::Ultraplan { .. } => "/ultraplan",
            Self::Teleport { .. } => "/teleport",
            Self::DebugToolCall { .. } => "/debug-tool-call",
            Self::Resume { .. } => "/resume",
            Self::Model { .. } => "/model",
            Self::Permissions { .. } => "/permissions",
            Self::Session { .. } => "/session",
            Self::Plugins { .. } => "/plugins",
            Self::Login => "/login",
            Self::Logout => "/logout",
            Self::Vim => "/vim",
            Self::Upgrade => "/upgrade",
            Self::Share => "/share",
            Self::Feedback => "/feedback",
            Self::Files => "/files",
            Self::Fast => "/fast",
            Self::Exit => "/exit",
            Self::Summary => "/summary",
            Self::Desktop => "/desktop",
            Self::Brief => "/brief",
            Self::Advisor => "/advisor",
            Self::Stickers => "/stickers",
            Self::Insights => "/insights",
            Self::Thinkback => "/thinkback",
            Self::ReleaseNotes => "/release-notes",
            Self::SecurityReview => "/security-review",
            Self::Keybindings => "/keybindings",
            Self::PrivacySettings => "/privacy-settings",
            Self::Plan { .. } => "/plan",
            Self::Review { .. } => "/review",
            Self::Tasks { .. } => "/tasks",
            Self::Theme { .. } => "/theme",
            Self::Voice { .. } => "/voice",
            Self::Usage { .. } => "/usage",
            Self::Rename { .. } => "/rename",
            Self::Copy { .. } => "/copy",
            Self::Hooks { .. } => "/hooks",
            Self::Context { .. } => "/context",
            Self::Color { .. } => "/color",
            Self::Effort { .. } => "/effort",
            Self::Branch { .. } => "/branch",
            Self::Rewind { .. } => "/rewind",
            Self::Ide { .. } => "/ide",
            Self::Tag { .. } => "/tag",
            Self::OutputStyle { .. } => "/output-style",
            Self::AddDir { .. } => "/add-dir",
            Self::Lsp { .. } => "/lsp",
            Self::Sandbox => "/sandbox",
            Self::Mcp { .. } => "/mcp",
            Self::Export { .. } => "/export",
            #[allow(unreachable_patterns)]
            _ => "/unknown",
        }
    }
}

#[allow(clippy::too_many_lines)]
pub fn validate_slash_command_input(
    input: &str,
) -> Result<Option<SlashCommand>, SlashCommandParseError> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return Ok(None);
    }

    let mut parts = trimmed.trim_start_matches('/').split_whitespace();
    let command = parts.next().unwrap_or_default();
    if command.is_empty() {
        return Err(SlashCommandParseError::new(
            "Slash command name is missing. Use /help to list available slash commands.",
        ));
    }

    let args = parts.collect::<Vec<_>>();
    let remainder = remainder_after_command(trimmed, command);

    Ok(Some(match command {
        "help" => {
            validate_no_args(command, &args)?;
            SlashCommand::Help
        }
        "status" => {
            validate_no_args(command, &args)?;
            SlashCommand::Status
        }
        "sandbox" => {
            validate_no_args(command, &args)?;
            SlashCommand::Sandbox
        }
        "compact" => {
            validate_no_args(command, &args)?;
            SlashCommand::Compact
        }
        "bughunter" => SlashCommand::Bughunter { scope: remainder },
        "commit" => {
            validate_no_args(command, &args)?;
            SlashCommand::Commit
        }
        "pr" => SlashCommand::Pr { context: remainder },
        "issue" => SlashCommand::Issue { context: remainder },
        "ultraplan" => SlashCommand::Ultraplan { task: remainder },
        "teleport" => SlashCommand::Teleport {
            target: Some(require_remainder(command, remainder, "<symbol-or-path>")?),
        },
        "debug-tool-call" => {
            validate_no_args(command, &args)?;
            SlashCommand::DebugToolCall
        }
        "model" => SlashCommand::Model {
            model: optional_single_arg(command, &args, "[model]")?,
        },
        "permissions" => SlashCommand::Permissions {
            mode: parse_permissions_mode(&args)?,
        },
        "clear" => SlashCommand::Clear {
            confirm: parse_clear_args(&args)?,
        },
        "cost" => {
            validate_no_args(command, &args)?;
            SlashCommand::Cost
        }
        "resume" => SlashCommand::Resume {
            session_path: Some(require_remainder(command, remainder, "<session-path>")?),
        },
        "config" => SlashCommand::Config {
            section: parse_config_section(&args)?,
        },
        "mcp" => parse_mcp_command(&args)?,
        "memory" => {
            validate_no_args(command, &args)?;
            SlashCommand::Memory
        }
        "init" => {
            validate_no_args(command, &args)?;
            SlashCommand::Init
        }
        "diff" => {
            validate_no_args(command, &args)?;
            SlashCommand::Diff
        }
        "version" => {
            validate_no_args(command, &args)?;
            SlashCommand::Version
        }
        "export" => SlashCommand::Export { path: remainder },
        "session" => parse_session_command(&args)?,
        "plugin" | "plugins" | "marketplace" => parse_plugin_command(&args)?,
        "agents" => SlashCommand::Agents {
            args: parse_list_or_help_args(command, remainder)?,
        },
        "skills" | "skill" => SlashCommand::Skills {
            args: parse_skills_args(remainder.as_deref())?,
        },
        "doctor" | "providers" => {
            validate_no_args(command, &args)?;
            SlashCommand::Doctor
        }
        "login" | "logout" => {
            return Err(command_error(
                "This auth flow was removed. Set ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN instead.",
                command,
                "",
            ));
        }
        "vim" => {
            validate_no_args(command, &args)?;
            SlashCommand::Vim
        }
        "upgrade" => {
            validate_no_args(command, &args)?;
            SlashCommand::Upgrade
        }
        "stats" | "tokens" | "cache" => {
            validate_no_args(command, &args)?;
            SlashCommand::Stats
        }
        "share" => {
            validate_no_args(command, &args)?;
            SlashCommand::Share
        }
        "feedback" => {
            validate_no_args(command, &args)?;
            SlashCommand::Feedback
        }
        "files" => {
            validate_no_args(command, &args)?;
            SlashCommand::Files
        }
        "fast" => {
            validate_no_args(command, &args)?;
            SlashCommand::Fast
        }
        "exit" => {
            validate_no_args(command, &args)?;
            SlashCommand::Exit
        }
        "summary" => {
            validate_no_args(command, &args)?;
            SlashCommand::Summary
        }
        "desktop" => {
            validate_no_args(command, &args)?;
            SlashCommand::Desktop
        }
        "brief" => {
            validate_no_args(command, &args)?;
            SlashCommand::Brief
        }
        "advisor" => {
            validate_no_args(command, &args)?;
            SlashCommand::Advisor
        }
        "stickers" => {
            validate_no_args(command, &args)?;
            SlashCommand::Stickers
        }
        "insights" => {
            validate_no_args(command, &args)?;
            SlashCommand::Insights
        }
        "thinkback" => {
            validate_no_args(command, &args)?;
            SlashCommand::Thinkback
        }
        "release-notes" => {
            validate_no_args(command, &args)?;
            SlashCommand::ReleaseNotes
        }
        "security-review" => {
            validate_no_args(command, &args)?;
            SlashCommand::SecurityReview
        }
        "keybindings" => {
            validate_no_args(command, &args)?;
            SlashCommand::Keybindings
        }
        "privacy-settings" => {
            validate_no_args(command, &args)?;
            SlashCommand::PrivacySettings
        }
        "plan" => SlashCommand::Plan { mode: remainder },
        "review" => SlashCommand::Review { scope: remainder },
        "team" => SlashCommand::Team { action: remainder },
        "tasks" => SlashCommand::Tasks { args: remainder },
        "theme" => SlashCommand::Theme { name: remainder },
        "voice" => SlashCommand::Voice { mode: remainder },
        "usage" => SlashCommand::Usage { scope: remainder },
<<<<<<< HEAD
=======
        "setup" => SlashCommand::Setup,
>>>>>>> 2f6a225 (fix: make id field optional in OpenAI response parsing)
        "rename" => SlashCommand::Rename { name: remainder },
        "copy" => SlashCommand::Copy { target: remainder },
        "hooks" => SlashCommand::Hooks { args: remainder },
        "context" => SlashCommand::Context { action: remainder },
        "color" => SlashCommand::Color { scheme: remainder },
        "effort" => SlashCommand::Effort { level: remainder },
        "branch" => SlashCommand::Branch { name: remainder },
        "rewind" => SlashCommand::Rewind { steps: remainder },
        "ide" => SlashCommand::Ide { target: remainder },
        "tag" => SlashCommand::Tag { label: remainder },
        "output-style" => SlashCommand::OutputStyle { style: remainder },
        "add-dir" => SlashCommand::AddDir { path: remainder },
        "lsp" => SlashCommand::Lsp {
            action: args.first().map(|s| (*s).to_string()),
            target: args.get(1).map(|s| (*s).to_string()),
        },
        "history" => SlashCommand::History {
            count: optional_single_arg(command, &args, "[count]")?,
        },
        other => SlashCommand::Unknown(other.to_string()),
    }))
}
fn validate_no_args(command: &str, args: &[&str]) -> Result<(), SlashCommandParseError> {
    if args.is_empty() {
        return Ok(());
    }

    Err(command_error(
        &format!("Unexpected arguments for /{command}."),
        command,
        &format!("/{command}"),
    ))
}

fn optional_single_arg(
    command: &str,
    args: &[&str],
    argument_hint: &str,
) -> Result<Option<String>, SlashCommandParseError> {
    match args {
        [] => Ok(None),
        [value] => Ok(Some((*value).to_string())),
        _ => Err(usage_error(command, argument_hint)),
    }
}

fn require_remainder(
    command: &str,
    remainder: Option<String>,
    argument_hint: &str,
) -> Result<String, SlashCommandParseError> {
    remainder.ok_or_else(|| usage_error(command, argument_hint))
}

fn parse_permissions_mode(args: &[&str]) -> Result<Option<String>, SlashCommandParseError> {
    let mode = optional_single_arg(
        "permissions",
        args,
        "[read-only|workspace-write|danger-full-access]",
    )?;
    if let Some(mode) = mode {
        if matches!(
            mode.as_str(),
            "read-only" | "workspace-write" | "danger-full-access"
        ) {
            return Ok(Some(mode));
        }
        return Err(command_error(
            &format!(
                "Unsupported /permissions mode '{mode}'. Use read-only, workspace-write, or danger-full-access."
            ),
            "permissions",
            "/permissions [read-only|workspace-write|danger-full-access]",
        ));
    }

    Ok(None)
}

fn parse_clear_args(args: &[&str]) -> Result<bool, SlashCommandParseError> {
    match args {
        [] => Ok(false),
        ["--confirm"] => Ok(true),
        [unexpected] => Err(command_error(
            &format!("Unsupported /clear argument '{unexpected}'. Use /clear or /clear --confirm."),
            "clear",
            "/clear [--confirm]",
        )),
        _ => Err(usage_error("clear", "[--confirm]")),
    }
}

fn parse_config_section(args: &[&str]) -> Result<Option<String>, SlashCommandParseError> {
    let section = optional_single_arg("config", args, "[env|hooks|model|plugins]")?;
    if let Some(section) = section {
        if matches!(section.as_str(), "env" | "hooks" | "model" | "plugins") {
            return Ok(Some(section));
        }
        return Err(command_error(
            &format!("Unsupported /config section '{section}'. Use env, hooks, model, or plugins."),
            "config",
            "/config [env|hooks|model|plugins]",
        ));
    }

    Ok(None)
}

fn parse_session_command(args: &[&str]) -> Result<SlashCommand, SlashCommandParseError> {
    match args {
        [] => Ok(SlashCommand::Session {
            action: None,
            target: None,
        }),
        ["list"] => Ok(SlashCommand::Session {
            action: Some("list".to_string()),
            target: None,
        }),
        ["list", ..] => Err(usage_error("session", "[list|switch <session-id>|fork [branch-name]|delete <session-id> [--force]]")),
        ["switch"] => Err(usage_error("session switch", "<session-id>")),
        ["switch", target] => Ok(SlashCommand::Session {
            action: Some("switch".to_string()),
            target: Some((*target).to_string()),
        }),
        ["switch", ..] => Err(command_error(
            "Unexpected arguments for /session switch.",
            "session",
            "/session switch <session-id>",
        )),
        ["fork"] => Ok(SlashCommand::Session {
            action: Some("fork".to_string()),
            target: None,
        }),
        ["fork", target] => Ok(SlashCommand::Session {
            action: Some("fork".to_string()),
            target: Some((*target).to_string()),
        }),
        ["fork", ..] => Err(command_error(
            "Unexpected arguments for /session fork.",
            "session",
            "/session fork [branch-name]",
        )),
        ["delete"] => Err(usage_error("session delete", "<session-id> [--force]")),
        ["delete", target] => Ok(SlashCommand::Session {
            action: Some("delete".to_string()),
            target: Some((*target).to_string()),
        }),
        ["delete", target, "--force"] => Ok(SlashCommand::Session {
            action: Some("delete-force".to_string()),
            target: Some((*target).to_string()),
        }),
        ["delete", _target, unexpected] => Err(command_error(
            &format!(
                "Unsupported /session delete flag '{unexpected}'. Use --force to skip confirmation."
            ),
            "session",
            "/session delete <session-id> [--force]",
        )),
        ["delete", ..] => Err(command_error(
            "Unexpected arguments for /session delete.",
            "session",
            "/session delete <session-id> [--force]",
        )),
        [action, ..] => Err(command_error(
            &format!(
                "Unknown /session action '{action}'. Use list, switch <session-id>, fork [branch-name], or delete <session-id> [--force]."
            ),
            "session",
            "/session [list|switch <session-id>|fork [branch-name]|delete <session-id> [--force]]",
        )),
    }
}

fn parse_mcp_command(args: &[&str]) -> Result<SlashCommand, SlashCommandParseError> {
    match args {
        [] => Ok(SlashCommand::Mcp {
            action: None,
            target: None,
        }),
        ["list"] => Ok(SlashCommand::Mcp {
            action: Some("list".to_string()),
            target: None,
        }),
        ["list", ..] => Err(usage_error("mcp list", "")),
        ["show"] => Err(usage_error("mcp show", "<server>")),
        ["show", target] => Ok(SlashCommand::Mcp {
            action: Some("show".to_string()),
            target: Some((*target).to_string()),
        }),
        ["show", ..] => Err(command_error(
            "Unexpected arguments for /mcp show.",
            "mcp",
            "/mcp show <server>",
        )),
        ["help" | "-h" | "--help"] => Ok(SlashCommand::Mcp {
            action: Some("help".to_string()),
            target: None,
        }),
        [action, ..] => Err(command_error(
            &format!("Unknown /mcp action '{action}'. Use list, show <server>, or help."),
            "mcp",
            "/mcp [list|show <server>|help]",
        )),
    }
}

fn parse_plugin_command(args: &[&str]) -> Result<SlashCommand, SlashCommandParseError> {
    match args {
        [] => Ok(SlashCommand::Plugins {
            action: None,
            target: None,
        }),
        ["list"] => Ok(SlashCommand::Plugins {
            action: Some("list".to_string()),
            target: None,
        }),
        ["list", ..] => Err(usage_error("plugin list", "")),
        ["install"] => Err(usage_error("plugin install", "<path>")),
        ["install", target @ ..] => Ok(SlashCommand::Plugins {
            action: Some("install".to_string()),
            target: Some(target.join(" ")),
        }),
        ["enable"] => Err(usage_error("plugin enable", "<name>")),
        ["enable", target] => Ok(SlashCommand::Plugins {
            action: Some("enable".to_string()),
            target: Some((*target).to_string()),
        }),
        ["enable", ..] => Err(command_error(
            "Unexpected arguments for /plugin enable.",
            "plugin",
            "/plugin enable <name>",
        )),
        ["disable"] => Err(usage_error("plugin disable", "<name>")),
        ["disable", target] => Ok(SlashCommand::Plugins {
            action: Some("disable".to_string()),
            target: Some((*target).to_string()),
        }),
        ["disable", ..] => Err(command_error(
            "Unexpected arguments for /plugin disable.",
            "plugin",
            "/plugin disable <name>",
        )),
        ["uninstall"] => Err(usage_error("plugin uninstall", "<id>")),
        ["uninstall", target] => Ok(SlashCommand::Plugins {
            action: Some("uninstall".to_string()),
            target: Some((*target).to_string()),
        }),
        ["uninstall", ..] => Err(command_error(
            "Unexpected arguments for /plugin uninstall.",
            "plugin",
            "/plugin uninstall <id>",
        )),
        ["update"] => Err(usage_error("plugin update", "<id>")),
        ["update", target] => Ok(SlashCommand::Plugins {
            action: Some("update".to_string()),
            target: Some((*target).to_string()),
        }),
        ["update", ..] => Err(command_error(
            "Unexpected arguments for /plugin update.",
            "plugin",
            "/plugin update <id>",
        )),
        [action, ..] => Err(command_error(
            &format!(
                "Unknown /plugin action '{action}'. Use list, install <path>, enable <name>, disable <name>, uninstall <id>, or update <id>."
            ),
            "plugin",
            "/plugin [list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]",
        )),
    }
}

fn parse_list_or_help_args(
    command: &str,
    args: Option<String>,
) -> Result<Option<String>, SlashCommandParseError> {
    match normalize_optional_args(args.as_deref()) {
        None | Some("list" | "help" | "-h" | "--help") => Ok(args),
        Some(unexpected) => Err(command_error(
            &format!(
                "Unexpected arguments for /{command}: {unexpected}. Use /{command}, /{command} list, or /{command} help."
            ),
            command,
            &format!("/{command} [list|help]"),
        )),
    }
}

fn parse_skills_args(args: Option<&str>) -> Result<Option<String>, SlashCommandParseError> {
    let Some(args) = normalize_optional_args(args) else {
        return Ok(None);
    };

    if matches!(args, "list" | "help" | "-h" | "--help") {
        return Ok(Some(args.to_string()));
    }

    if args == "install" {
        return Err(command_error(
            "Usage: /skills install <path>",
            "skills",
            "/skills install <path>",
        ));
    }

    if let Some(target) = args.strip_prefix("install").map(str::trim) {
        if !target.is_empty() {
            return Ok(Some(format!("install {target}")));
        }
    }

    Ok(Some(args.to_string()))
}

fn usage_error(command: &str, argument_hint: &str) -> SlashCommandParseError {
    let usage = format!("/{command} {argument_hint}");
    let usage = usage.trim_end().to_string();
    command_error(
        &format!("Usage: {usage}"),
        command_root_name(command),
        &usage,
    )
}

fn command_error(message: &str, command: &str, usage: &str) -> SlashCommandParseError {
    let detail = render_slash_command_help_detail(command)
        .map(|detail| format!("\n\n{detail}"))
        .unwrap_or_default();
    SlashCommandParseError::new(format!("{message}\n  Usage            {usage}{detail}"))
}

fn remainder_after_command(input: &str, command: &str) -> Option<String> {
    input
        .trim()
        .strip_prefix(&format!("/{command}"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn find_slash_command_spec(name: &str) -> Option<&'static SlashCommandSpec> {
    slash_command_specs().iter().find(|spec| {
        spec.name.eq_ignore_ascii_case(name)
            || spec
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    })
}

fn command_root_name(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or(command)
}

fn slash_command_usage(spec: &SlashCommandSpec) -> String {
    match spec.argument_hint {
        Some(argument_hint) => format!("/{} {argument_hint}", spec.name),
        None => format!("/{}", spec.name),
    }
}

fn slash_command_detail_lines(spec: &SlashCommandSpec) -> Vec<String> {
    let mut lines = vec![format!("/{}", spec.name)];
    lines.push(format!("  Summary          {}", spec.summary));
    lines.push(format!("  Usage            {}", slash_command_usage(spec)));
    lines.push(format!(
        "  Category         {}",
        slash_command_category(spec.name)
    ));
    if !spec.aliases.is_empty() {
        lines.push(format!(
            "  Aliases          {}",
            spec.aliases
                .iter()
                .map(|alias| format!("/{alias}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if spec.resume_supported {
        lines.push("  Resume           Supported with --resume SESSION.jsonl".to_string());
    }
    lines
}

#[must_use]
pub fn render_slash_command_help_detail(name: &str) -> Option<String> {
    find_slash_command_spec(name).map(|spec| slash_command_detail_lines(spec).join("\n"))
}

#[must_use]
pub fn slash_command_specs() -> &'static [SlashCommandSpec] {
    SLASH_COMMAND_SPECS
}

#[must_use]
pub fn resume_supported_slash_commands() -> Vec<&'static SlashCommandSpec> {
    slash_command_specs()
        .iter()
        .filter(|spec| spec.resume_supported)
        .collect()
}

fn slash_command_category(name: &str) -> &'static str {
    match name {
        "help" | "status" | "cost" | "resume" | "session" | "version" | "usage" | "stats"
        | "rename" | "clear" | "compact" | "history" | "tokens" | "cache" | "exit" | "summary"
        | "tag" | "thinkback" | "copy" | "share" | "feedback" | "rewind" | "pin" | "unpin"
        | "bookmarks" | "context" | "files" | "focus" | "unfocus" | "retry" | "stop" | "undo" => {
            "Session"
        }
        "model" | "permissions" | "config" | "memory" | "theme" | "vim" | "voice" | "color"
        | "effort" | "fast" | "brief" | "output-style" | "keybindings" | "privacy-settings"
        | "stickers" | "language" | "profile" | "max-tokens" | "temperature" | "system-prompt"
        | "api-key" | "terminal-setup" | "notifications" | "telemetry" | "providers" | "env"
        | "project" | "reasoning" | "budget" | "rate-limit" | "workspace" | "reset" | "ide"
        | "desktop" | "upgrade" => "Config",
        "debug-tool-call" | "doctor" | "sandbox" | "diagnostics" | "tool-details" | "changelog"
        | "metrics" => "Debug",
        _ => "Tools",
    }
}

fn format_slash_command_help_line(spec: &SlashCommandSpec) -> String {
    let name = slash_command_usage(spec);
    let alias_suffix = if spec.aliases.is_empty() {
        String::new()
    } else {
        format!(
            " (aliases: {})",
            spec.aliases
                .iter()
                .map(|alias| format!("/{alias}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let resume = if spec.resume_supported {
        " [resume]"
    } else {
        ""
    };
    format!("  {name:<66} {}{alias_suffix}{resume}", spec.summary)
}

fn levenshtein_distance(left: &str, right: &str) -> usize {
    if left == right {
        return 0;
    }
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
            current[right_index + 1] = (current[right_index] + 1)
                .min(previous[right_index + 1] + 1)
                .min(previous[right_index] + substitution_cost);
        }
        previous.clone_from(&current);
    }

    previous[right_chars.len()]
}

#[must_use]
pub fn suggest_slash_commands(input: &str, limit: usize) -> Vec<String> {
    let query = input.trim().trim_start_matches('/').to_ascii_lowercase();
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut suggestions = slash_command_specs()
        .iter()
        .filter_map(|spec| {
            let best = std::iter::once(spec.name)
                .chain(spec.aliases.iter().copied())
                .map(str::to_ascii_lowercase)
                .map(|candidate| {
                    let prefix_rank =
                        if candidate.starts_with(&query) || query.starts_with(&candidate) {
                            0
                        } else if candidate.contains(&query) || query.contains(&candidate) {
                            1
                        } else {
                            2
                        };
                    let distance = levenshtein_distance(&candidate, &query);
                    (prefix_rank, distance)
                })
                .min();

            best.and_then(|(prefix_rank, distance)| {
                if prefix_rank <= 1 || distance <= 2 {
                    Some((prefix_rank, distance, spec.name.len(), spec.name))
                } else {
                    None
                }
            })
        })
        .collect::<Vec<_>>();

    suggestions.sort_unstable();
    suggestions
        .into_iter()
        .map(|(_, _, _, name)| format!("/{name}"))
        .take(limit)
        .collect()
}

#[must_use]
/// Render the slash-command help section, optionally excluding stub commands
/// (commands that are registered in the spec list but not yet implemented).
/// Pass an empty slice to include all commands.
pub fn render_slash_command_help_filtered(exclude: &[&str]) -> String {
    let mut lines = vec![
        "Slash commands".to_string(),
        "  Start here        /status, /diff, /agents, /skills, /commit".to_string(),
        "  [resume]          also works with --resume SESSION.jsonl".to_string(),
        String::new(),
    ];

    let categories = ["Session", "Tools", "Config", "Debug"];

    for category in categories {
        lines.push(category.to_string());
        for spec in slash_command_specs()
            .iter()
            .filter(|spec| slash_command_category(spec.name) == category)
            .filter(|spec| !exclude.contains(&spec.name))
        {
            lines.push(format_slash_command_help_line(spec));
        }
        lines.push(String::new());
    }

    lines
        .into_iter()
        .rev()
        .skip_while(String::is_empty)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_slash_command_help() -> String {
    let mut lines = vec![
        "Slash commands".to_string(),
        "  Start here        /status, /diff, /agents, /skills, /commit".to_string(),
        "  [resume]          also works with --resume SESSION.jsonl".to_string(),
        String::new(),
    ];

    let categories = ["Session", "Tools", "Config", "Debug"];

    for category in categories {
        lines.push(category.to_string());
        for spec in slash_command_specs()
            .iter()
            .filter(|spec| slash_command_category(spec.name) == category)
        {
            lines.push(format_slash_command_help_line(spec));
        }
        lines.push(String::new());
    }

    lines.push("Keyboard shortcuts".to_string());
    lines.push("  Up/Down              Navigate prompt history".to_string());
    lines.push("  Tab                  Complete commands, modes, and recent sessions".to_string());
    lines.push("  Ctrl-C               Clear input (or exit on empty prompt)".to_string());
    lines.push("  Shift+Enter/Ctrl+J   Insert a newline".to_string());

    lines
        .into_iter()
        .rev()
        .skip_while(String::is_empty)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandResult {
    pub message: String,
    pub session: Session,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginsCommandResult {
    pub message: String,
    pub reload_runtime: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DefinitionSource {
    ProjectClaw,
    ProjectCodex,
    ProjectClaude,
    UserClawConfigHome,
    UserCodexHome,
    UserClaw,
    UserCodex,
    UserClaude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DefinitionScope {
    Project,
    UserConfigHome,
    UserHome,
}

impl DefinitionScope {
    fn label(self) -> &'static str {
        match self {
            Self::Project => "Project roots",
            Self::UserConfigHome => "User config roots",
            Self::UserHome => "User home roots",
        }
    }
}

impl DefinitionSource {
    fn report_scope(self) -> DefinitionScope {
        match self {
            Self::ProjectClaw | Self::ProjectCodex | Self::ProjectClaude => {
                DefinitionScope::Project
            }
            Self::UserClawConfigHome | Self::UserCodexHome => DefinitionScope::UserConfigHome,
            Self::UserClaw | Self::UserCodex | Self::UserClaude => DefinitionScope::UserHome,
        }
    }

    fn label(self) -> &'static str {
        self.report_scope().label()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentSummary {
    name: String,
    description: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    source: DefinitionSource,
    shadowed_by: Option<DefinitionSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillSummary {
    name: String,
    description: Option<String>,
    source: DefinitionSource,
    shadowed_by: Option<DefinitionSource>,
    origin: SkillOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillOrigin {
    SkillsDir,
    LegacyCommandsDir,
}

impl SkillOrigin {
    fn detail_label(self) -> Option<&'static str> {
        match self {
            Self::SkillsDir => None,
            Self::LegacyCommandsDir => Some("legacy /commands"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillRoot {
    source: DefinitionSource,
    path: PathBuf,
    origin: SkillOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstalledSkill {
    invocation_name: String,
    display_name: Option<String>,
    source: PathBuf,
    registry_root: PathBuf,
    installed_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SkillInstallSource {
    Directory { root: PathBuf, prompt_path: PathBuf },
    MarkdownFile { path: PathBuf },
}

#[allow(clippy::too_many_lines)]
pub fn handle_plugins_slash_command(
    action: Option<&str>,
    target: Option<&str>,
    manager: &mut PluginManager,
) -> Result<PluginsCommandResult, PluginError> {
    match action {
        None | Some("list") => {
            let report = manager.installed_plugin_registry_report()?;
            let plugins = report.summaries();
            let failures = report.failures();
            Ok(PluginsCommandResult {
                message: render_plugins_report_with_failures(&plugins, failures),
                reload_runtime: false,
            })
        }
        Some("install") => {
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugins install <path>".to_string(),
                    reload_runtime: false,
                });
            };
            let install = manager.install(target)?;
            let plugin = manager
                .list_installed_plugins()?
                .into_iter()
                .find(|plugin| plugin.metadata.id == install.plugin_id);
            Ok(PluginsCommandResult {
                message: render_plugin_install_report(&install.plugin_id, plugin.as_ref()),
                reload_runtime: true,
            })
        }
        Some("enable") => {
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugins enable <name>".to_string(),
                    reload_runtime: false,
                });
            };
            let plugin = resolve_plugin_target(manager, target)?;
            manager.enable(&plugin.metadata.id)?;
            Ok(PluginsCommandResult {
                message: format!(
                    "Plugins\n  Result           enabled {}\n  Name             {}\n  Version          {}\n  Status           enabled",
                    plugin.metadata.id, plugin.metadata.name, plugin.metadata.version
                ),
                reload_runtime: true,
            })
        }
        Some("disable") => {
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugins disable <name>".to_string(),
                    reload_runtime: false,
                });
            };
            let plugin = resolve_plugin_target(manager, target)?;
            manager.disable(&plugin.metadata.id)?;
            Ok(PluginsCommandResult {
                message: format!(
                    "Plugins\n  Result           disabled {}\n  Name             {}\n  Version          {}\n  Status           disabled",
                    plugin.metadata.id, plugin.metadata.name, plugin.metadata.version
                ),
                reload_runtime: true,
            })
        }
        Some("uninstall") => {
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugins uninstall <plugin-id>".to_string(),
                    reload_runtime: false,
                });
            };
            manager.uninstall(target)?;
            Ok(PluginsCommandResult {
                message: format!("Plugins\n  Result           uninstalled {target}"),
                reload_runtime: true,
            })
        }
        Some("update") => {
            let Some(target) = target else {
                return Ok(PluginsCommandResult {
                    message: "Usage: /plugins update <plugin-id>".to_string(),
                    reload_runtime: false,
                });
            };
            let update = manager.update(target)?;
            let plugin = manager
                .list_installed_plugins()?
                .into_iter()
                .find(|plugin| plugin.metadata.id == update.plugin_id);
            Ok(PluginsCommandResult {
                message: format!(
                    "Plugins\n  Result           updated {}\n  Name             {}\n  Old version      {}\n  New version      {}\n  Status           {}",
                    update.plugin_id,
                    plugin
                        .as_ref()
                        .map_or_else(|| update.plugin_id.clone(), |plugin| plugin.metadata.name.clone()),
                    update.old_version,
                    update.new_version,
                    plugin
                        .as_ref()
                        .map_or("unknown", |plugin| if plugin.enabled { "enabled" } else { "disabled" }),
                ),
                reload_runtime: true,
            })
        }
        Some(other) => Ok(PluginsCommandResult {
            message: format!(
                "Unknown /plugins action '{other}'. Use list, install, enable, disable, uninstall, or update."
            ),
            reload_runtime: false,
        }),
    }
}

pub fn handle_agents_slash_command(args: Option<&str>, cwd: &Path) -> std::io::Result<String> {
    if let Some(args) = normalize_optional_args(args) {
        if let Some(help_path) = help_path_from_args(args) {
            return Ok(match help_path.as_slice() {
                [] => render_agents_usage(None),
                _ => render_agents_usage(Some(&help_path.join(" "))),
            });
        }
    }

    match normalize_optional_args(args) {
        None | Some("list") => {
            let roots = discover_definition_roots(cwd, "agents");
            let agents = load_agents_from_roots(&roots)?;
            Ok(render_agents_report(&agents))
        }
        Some(args) if is_help_arg(args) => Ok(render_agents_usage(None)),
        Some(args) => Ok(render_agents_usage(Some(args))),
    }
}

pub fn handle_agents_slash_command_json(args: Option<&str>, cwd: &Path) -> std::io::Result<Value> {
    if let Some(args) = normalize_optional_args(args) {
        if let Some(help_path) = help_path_from_args(args) {
            return Ok(match help_path.as_slice() {
                [] => render_agents_usage_json(None),
                _ => render_agents_usage_json(Some(&help_path.join(" "))),
            });
        }
    }

    match normalize_optional_args(args) {
        None | Some("list") => {
            let roots = discover_definition_roots(cwd, "agents");
            let agents = load_agents_from_roots(&roots)?;
            Ok(render_agents_report_json(cwd, &agents))
        }
        Some(args) if is_help_arg(args) => Ok(render_agents_usage_json(None)),
        Some(args) => Ok(render_agents_usage_json(Some(args))),
    }
}

pub fn handle_mcp_slash_command(
    args: Option<&str>,
    cwd: &Path,
) -> Result<String, runtime::ConfigError> {
    let loader = ConfigLoader::default_for(cwd);
    render_mcp_report_for(&loader, cwd, args)
}

pub fn handle_mcp_slash_command_json(
    args: Option<&str>,
    cwd: &Path,
) -> Result<Value, runtime::ConfigError> {
    let loader = ConfigLoader::default_for(cwd);
    render_mcp_report_json_for(&loader, cwd, args)
}

pub fn handle_skills_slash_command(args: Option<&str>, cwd: &Path) -> std::io::Result<String> {
    if let Some(args) = normalize_optional_args(args) {
        if let Some(help_path) = help_path_from_args(args) {
            return Ok(match help_path.as_slice() {
                [] => render_skills_usage(None),
                ["install", ..] => render_skills_usage(Some("install")),
                _ => render_skills_usage(Some(&help_path.join(" "))),
            });
        }
    }

    match normalize_optional_args(args) {
        None | Some("list") => {
            let roots = discover_skill_roots(cwd);
            let skills = load_skills_from_roots(&roots)?;
            Ok(render_skills_report(&skills))
        }
        Some("install") => Ok(render_skills_usage(Some("install"))),
        Some(args) if args.starts_with("install ") => {
            let target = args["install ".len()..].trim();
            if target.is_empty() {
                return Ok(render_skills_usage(Some("install")));
            }
            let install = install_skill(target, cwd)?;
            Ok(render_skill_install_report(&install))
        }
        Some(args) if is_help_arg(args) => Ok(render_skills_usage(None)),
        Some(args) => Ok(render_skills_usage(Some(args))),
    }
}

pub fn handle_skills_slash_command_json(args: Option<&str>, cwd: &Path) -> std::io::Result<Value> {
    if let Some(args) = normalize_optional_args(args) {
        if let Some(help_path) = help_path_from_args(args) {
            return Ok(match help_path.as_slice() {
                [] => render_skills_usage_json(None),
                ["install", ..] => render_skills_usage_json(Some("install")),
                _ => render_skills_usage_json(Some(&help_path.join(" "))),
            });
        }
    }

    match normalize_optional_args(args) {
        None | Some("list") => {
            let roots = discover_skill_roots(cwd);
            let skills = load_skills_from_roots(&roots)?;
            Ok(render_skills_report_json(&skills))
        }
        Some("install") => Ok(render_skills_usage_json(Some("install"))),
        Some(args) if args.starts_with("install ") => {
            let target = args["install ".len()..].trim();
            if target.is_empty() {
                return Ok(render_skills_usage_json(Some("install")));
            }
            let install = install_skill(target, cwd)?;
            Ok(render_skill_install_report_json(&install))
        }
        Some(args) if is_help_arg(args) => Ok(render_skills_usage_json(None)),
        Some(args) => Ok(render_skills_usage_json(Some(args))),
    }
}

#[must_use]
pub fn classify_skills_slash_command(args: Option<&str>) -> SkillSlashDispatch {
    match normalize_optional_args(args) {
        None | Some("list" | "help" | "-h" | "--help") => SkillSlashDispatch::Local,
        Some(args) if args == "install" || args.starts_with("install ") => {
            SkillSlashDispatch::Local
        }
        Some(args) => SkillSlashDispatch::Invoke(format!("${}", args.trim_start_matches('/'))),
    }
}

/// Resolve a skill invocation by validating the skill exists on disk before
/// returning the dispatch.  When the skill is not found, returns `Err` with a
/// human-readable message that lists nearby skill names.
pub fn resolve_skill_invocation(
    cwd: &Path,
    args: Option<&str>,
) -> Result<SkillSlashDispatch, String> {
    let dispatch = classify_skills_slash_command(args);
    if let SkillSlashDispatch::Invoke(ref prompt) = dispatch {
        // Extract the skill name from the "$skill [args]" prompt.
        let skill_token = prompt
            .trim_start_matches('$')
            .split_whitespace()
            .next()
            .unwrap_or_default();
        if !skill_token.is_empty() {
            if let Err(error) = resolve_skill_path(cwd, skill_token) {
                let mut message = format!("Unknown skill: {skill_token} ({error})");
                let roots = discover_skill_roots(cwd);
                if let Ok(available) = load_skills_from_roots(&roots) {
                    let names: Vec<String> = available
                        .iter()
                        .filter(|s| s.shadowed_by.is_none())
                        .map(|s| s.name.clone())
                        .collect();
                    if !names.is_empty() {
                        message.push_str("\n  Available skills: ");
                        message.push_str(&names.join(", "));
                    }
                }
                message.push_str("\n  Usage: /skills [list|install <path>|help|<skill> [args]]");
                return Err(message);
            }
        }
    }
    Ok(dispatch)
}

pub fn resolve_skill_path(cwd: &Path, skill: &str) -> std::io::Result<PathBuf> {
    let requested = skill.trim().trim_start_matches('/').trim_start_matches('$');
    if requested.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "skill must not be empty",
        ));
    }

    let roots = discover_skill_roots(cwd);
    for root in &roots {
        let mut entries = Vec::new();
        for entry in fs::read_dir(&root.path)? {
            let entry = entry?;
            match root.origin {
                SkillOrigin::SkillsDir => {
                    if !entry.path().is_dir() {
                        continue;
                    }
                    let skill_path = entry.path().join("SKILL.md");
                    if !skill_path.is_file() {
                        continue;
                    }
                    let contents = fs::read_to_string(&skill_path)?;
                    let (name, _) = parse_skill_frontmatter(&contents);
                    entries.push((
                        name.unwrap_or_else(|| entry.file_name().to_string_lossy().to_string()),
                        skill_path,
                    ));
                }
                SkillOrigin::LegacyCommandsDir => {
                    let path = entry.path();
                    let markdown_path = if path.is_dir() {
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

                    let contents = fs::read_to_string(&markdown_path)?;
                    let fallback_name = markdown_path.file_stem().map_or_else(
                        || entry.file_name().to_string_lossy().to_string(),
                        |stem| stem.to_string_lossy().to_string(),
                    );
                    let (name, _) = parse_skill_frontmatter(&contents);
                    entries.push((name.unwrap_or(fallback_name), markdown_path));
                }
            }
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        if let Some((_, path)) = entries
            .into_iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(requested))
        {
            return Ok(path);
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("unknown skill: {requested}"),
    ))
}

fn render_mcp_report_for(
    loader: &ConfigLoader,
    cwd: &Path,
    args: Option<&str>,
) -> Result<String, runtime::ConfigError> {
    if let Some(args) = normalize_optional_args(args) {
        if let Some(help_path) = help_path_from_args(args) {
            return Ok(match help_path.as_slice() {
                [] => render_mcp_usage(None),
                ["show", ..] => render_mcp_usage(Some("show")),
                _ => render_mcp_usage(Some(&help_path.join(" "))),
            });
        }
    }

    match normalize_optional_args(args) {
        None | Some("list") => {
            // #144: degrade gracefully on config parse failure (same contract
            // as #143 for `status`). Text mode prepends a "Config load error"
            // block before the MCP list; the list falls back to empty.
            match loader.load() {
                Ok(runtime_config) => Ok(render_mcp_summary_report(
                    cwd,
                    runtime_config.mcp().servers(),
                )),
                Err(err) => {
                    let empty = std::collections::BTreeMap::new();
                    Ok(format!(
                        "Config load error\n  Status           fail\n  Summary          runtime config failed to load; reporting partial MCP view\n  Details          {err}\n  Hint             `claw doctor` classifies config parse errors; fix the listed field and rerun\n\n{}",
                        render_mcp_summary_report(cwd, &empty)
                    ))
                }
            }
        }
        Some(args) if is_help_arg(args) => Ok(render_mcp_usage(None)),
        Some("show") => Ok(render_mcp_usage(Some("show"))),
        Some(args) if args.split_whitespace().next() == Some("show") => {
            let mut parts = args.split_whitespace();
            let _ = parts.next();
            let Some(server_name) = parts.next() else {
                return Ok(render_mcp_usage(Some("show")));
            };
            if parts.next().is_some() {
                return Ok(render_mcp_usage(Some(args)));
            }
            // #144: same degradation for `mcp show`; if config won't parse,
            // the specific server lookup can't succeed, so report the parse
            // error with context.
            match loader.load() {
                Ok(runtime_config) => Ok(render_mcp_server_report(
                    cwd,
                    server_name,
                    runtime_config.mcp().get(server_name),
                )),
                Err(err) => Ok(format!(
                    "Config load error\n  Status           fail\n  Summary          runtime config failed to load; cannot resolve `{server_name}`\n  Details          {err}\n  Hint             `claw doctor` classifies config parse errors; fix the listed field and rerun"
                )),
            }
        }
        Some(args) => Ok(render_mcp_usage(Some(args))),
    }
}

fn render_mcp_report_json_for(
    loader: &ConfigLoader,
    cwd: &Path,
    args: Option<&str>,
) -> Result<Value, runtime::ConfigError> {
    if let Some(args) = normalize_optional_args(args) {
        if let Some(help_path) = help_path_from_args(args) {
            return Ok(match help_path.as_slice() {
                [] => render_mcp_usage_json(None),
                ["show", ..] => render_mcp_usage_json(Some("show")),
                _ => render_mcp_usage_json(Some(&help_path.join(" "))),
            });
        }
    }

    match normalize_optional_args(args) {
        None | Some("list") => {
            // #144: match #143's degraded envelope contract. On config parse
            // failure, emit top-level `status: "degraded"` with
            // `config_load_error`, empty servers[], and exit 0. On clean
            // runs, the existing serializer adds `status: "ok"` below.
            match loader.load() {
                Ok(runtime_config) => {
                    let mut value =
                        render_mcp_summary_report_json(cwd, runtime_config.mcp().servers());
                    if let Some(map) = value.as_object_mut() {
                        map.insert("status".to_string(), Value::String("ok".to_string()));
                        map.insert("config_load_error".to_string(), Value::Null);
                    }
                    Ok(value)
                }
                Err(err) => {
                    let empty = std::collections::BTreeMap::new();
                    let mut value = render_mcp_summary_report_json(cwd, &empty);
                    if let Some(map) = value.as_object_mut() {
                        map.insert("status".to_string(), Value::String("degraded".to_string()));
                        map.insert(
                            "config_load_error".to_string(),
                            Value::String(err.to_string()),
                        );
                    }
                    Ok(value)
                }
            }
        }
        Some(args) if is_help_arg(args) => Ok(render_mcp_usage_json(None)),
        Some("show") => Ok(render_mcp_usage_json(Some("show"))),
        Some(args) if args.split_whitespace().next() == Some("show") => {
            let mut parts = args.split_whitespace();
            let _ = parts.next();
            let Some(server_name) = parts.next() else {
                return Ok(render_mcp_usage_json(Some("show")));
            };
            if parts.next().is_some() {
                return Ok(render_mcp_usage_json(Some(args)));
            }
            // #144: same degradation pattern for show action.
            match loader.load() {
                Ok(runtime_config) => {
                    let mut value = render_mcp_server_report_json(
                        cwd,
                        server_name,
                        runtime_config.mcp().get(server_name),
                    );
                    if let Some(map) = value.as_object_mut() {
                        map.insert("status".to_string(), Value::String("ok".to_string()));
                        map.insert("config_load_error".to_string(), Value::Null);
                    }
                    Ok(value)
                }
                Err(err) => Ok(serde_json::json!({
                    "kind": "mcp",
                    "action": "show",
                    "server": server_name,
                    "status": "degraded",
                    "config_load_error": err.to_string(),
                    "working_directory": cwd.display().to_string(),
                })),
            }
        }
        Some(args) => Ok(render_mcp_usage_json(Some(args))),
    }
}

#[must_use]
pub fn render_plugins_report(plugins: &[PluginSummary]) -> String {
    let mut lines = vec!["Plugins".to_string()];
    if plugins.is_empty() {
        lines.push("  No plugins installed.".to_string());
        return lines.join("\n");
    }
    for plugin in plugins {
        let enabled = if plugin.enabled {
            "enabled"
        } else {
            "disabled"
        };
        lines.push(format!(
            "  {name:<20} v{version:<10} {enabled}",
            name = plugin.metadata.name,
            version = plugin.metadata.version,
        ));
    }
    lines.join("\n")
}

#[must_use]
pub fn render_plugins_report_with_failures(
    plugins: &[PluginSummary],
    failures: &[PluginLoadFailure],
) -> String {
    let mut lines = vec!["Plugins".to_string()];

    // Show successfully loaded plugins
    if plugins.is_empty() {
        lines.push("  No plugins installed.".to_string());
    } else {
        for plugin in plugins {
            let enabled = if plugin.enabled {
                "enabled"
            } else {
                "disabled"
            };
            lines.push(format!(
                "  {name:<20} v{version:<10} {enabled}",
                name = plugin.metadata.name,
                version = plugin.metadata.version,
            ));
        }
    }

    // Show warnings for broken plugins
    if !failures.is_empty() {
        lines.push(String::new());
        lines.push("Warnings:".to_string());
        for failure in failures {
            lines.push(format!(
                "  ⚠️  Failed to load {} plugin from `{}`",
                failure.kind,
                failure.plugin_root.display()
            ));
            lines.push(format!("      Error: {}", failure.error()));
        }
    }

    lines.join("\n")
}

fn render_plugin_install_report(plugin_id: &str, plugin: Option<&PluginSummary>) -> String {
    let name = plugin.map_or(plugin_id, |plugin| plugin.metadata.name.as_str());
    let version = plugin.map_or("unknown", |plugin| plugin.metadata.version.as_str());
    let enabled = plugin.is_some_and(|plugin| plugin.enabled);
    format!(
        "Plugins\n  Result           installed {plugin_id}\n  Name             {name}\n  Version          {version}\n  Status           {}",
        if enabled { "enabled" } else { "disabled" }
    )
}

fn resolve_plugin_target(
    manager: &PluginManager,
    target: &str,
) -> Result<PluginSummary, PluginError> {
    let mut matches = manager
        .list_installed_plugins()?
        .into_iter()
        .filter(|plugin| plugin.metadata.id == target || plugin.metadata.name == target)
        .collect::<Vec<_>>();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(PluginError::NotFound(format!(
            "plugin `{target}` is not installed or discoverable"
        ))),
        _ => Err(PluginError::InvalidManifest(format!(
            "plugin name `{target}` is ambiguous; use the full plugin id"
        ))),
    }
}

fn discover_definition_roots(cwd: &Path, leaf: &str) -> Vec<(DefinitionSource, PathBuf)> {
    let mut roots = Vec::new();

    for ancestor in cwd.ancestors() {
        push_unique_root(
            &mut roots,
            DefinitionSource::ProjectClaw,
            ancestor.join(".claw").join(leaf),
        );
        push_unique_root(
            &mut roots,
            DefinitionSource::ProjectCodex,
            ancestor.join(".codex").join(leaf),
        );
        push_unique_root(
            &mut roots,
            DefinitionSource::ProjectClaude,
            ancestor.join(".claude").join(leaf),
        );
    }

    if let Ok(claw_config_home) = env::var("CLAW_CONFIG_HOME") {
        push_unique_root(
            &mut roots,
            DefinitionSource::UserClawConfigHome,
            PathBuf::from(claw_config_home).join(leaf),
        );
    }

    if let Ok(codex_home) = env::var("CODEX_HOME") {
        push_unique_root(
            &mut roots,
            DefinitionSource::UserCodexHome,
            PathBuf::from(codex_home).join(leaf),
        );
    }

    if let Ok(claude_config_dir) = env::var("CLAUDE_CONFIG_DIR") {
        push_unique_root(
            &mut roots,
            DefinitionSource::UserClaude,
            PathBuf::from(claude_config_dir).join(leaf),
        );
    }

    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        push_unique_root(
            &mut roots,
            DefinitionSource::UserClaw,
            home.join(".claw").join(leaf),
        );
        push_unique_root(
            &mut roots,
            DefinitionSource::UserCodex,
            home.join(".codex").join(leaf),
        );
        push_unique_root(
            &mut roots,
            DefinitionSource::UserClaude,
            home.join(".claude").join(leaf),
        );
    }

    roots
}

#[allow(clippy::too_many_lines)]
fn discover_skill_roots(cwd: &Path) -> Vec<SkillRoot> {
    let mut roots = Vec::new();

    for ancestor in cwd.ancestors() {
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectClaw,
            ancestor.join(".claw").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectClaw,
            ancestor.join(".omc").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectClaw,
            ancestor.join(".agents").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectCodex,
            ancestor.join(".codex").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectClaude,
            ancestor.join(".claude").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectClaw,
            ancestor.join(".claw").join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectCodex,
            ancestor.join(".codex").join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::ProjectClaude,
            ancestor.join(".claude").join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
    }

    if let Ok(claw_config_home) = env::var("CLAW_CONFIG_HOME") {
        let claw_config_home = PathBuf::from(claw_config_home);
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClawConfigHome,
            claw_config_home.join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClawConfigHome,
            claw_config_home.join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
    }

    if let Ok(codex_home) = env::var("CODEX_HOME") {
        let codex_home = PathBuf::from(codex_home);
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserCodexHome,
            codex_home.join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserCodexHome,
            codex_home.join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
    }

    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClaw,
            home.join(".claw").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClaw,
            home.join(".omc").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClaw,
            home.join(".claw").join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserCodex,
            home.join(".codex").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserCodex,
            home.join(".codex").join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClaude,
            home.join(".claude").join("skills"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClaude,
            home.join(".claude").join("skills").join("omc-learned"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClaude,
            home.join(".claude").join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
    }

    if let Ok(claude_config_dir) = env::var("CLAUDE_CONFIG_DIR") {
        let claude_config_dir = PathBuf::from(claude_config_dir);
        let skills_dir = claude_config_dir.join("skills");
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClaude,
            skills_dir.clone(),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClaude,
            skills_dir.join("omc-learned"),
            SkillOrigin::SkillsDir,
        );
        push_unique_skill_root(
            &mut roots,
            DefinitionSource::UserClaude,
            claude_config_dir.join("commands"),
            SkillOrigin::LegacyCommandsDir,
        );
    }

    roots
}

fn install_skill(source: &str, cwd: &Path) -> std::io::Result<InstalledSkill> {
    let registry_root = default_skill_install_root()?;
    install_skill_into(source, cwd, &registry_root)
}

fn install_skill_into(
    source: &str,
    cwd: &Path,
    registry_root: &Path,
) -> std::io::Result<InstalledSkill> {
    let source = resolve_skill_install_source(source, cwd)?;
    let prompt_path = source.prompt_path();
    let contents = fs::read_to_string(prompt_path)?;
    let display_name = parse_skill_frontmatter(&contents).0;
    let invocation_name = derive_skill_install_name(&source, display_name.as_deref())?;
    let installed_path = registry_root.join(&invocation_name);

    if installed_path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "skill '{invocation_name}' is already installed at {}",
                installed_path.display()
            ),
        ));
    }

    fs::create_dir_all(&installed_path)?;
    let install_result = match &source {
        SkillInstallSource::Directory { root, .. } => {
            copy_directory_contents(root, &installed_path)
        }
        SkillInstallSource::MarkdownFile { path } => {
            fs::copy(path, installed_path.join("SKILL.md")).map(|_| ())
        }
    };
    if let Err(error) = install_result {
        let _ = fs::remove_dir_all(&installed_path);
        return Err(error);
    }

    Ok(InstalledSkill {
        invocation_name,
        display_name,
        source: source.report_path().to_path_buf(),
        registry_root: registry_root.to_path_buf(),
        installed_path,
    })
}

fn default_skill_install_root() -> std::io::Result<PathBuf> {
    if let Ok(claw_config_home) = env::var("CLAW_CONFIG_HOME") {
        return Ok(PathBuf::from(claw_config_home).join("skills"));
    }
    if let Ok(codex_home) = env::var("CODEX_HOME") {
        return Ok(PathBuf::from(codex_home).join("skills"));
    }
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home).join(".claw").join("skills"));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "unable to resolve a skills install root; set CLAW_CONFIG_HOME or HOME",
    ))
}

fn resolve_skill_install_source(source: &str, cwd: &Path) -> std::io::Result<SkillInstallSource> {
    let candidate = PathBuf::from(source);
    let source = if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    };
    let source = fs::canonicalize(&source)?;

    if source.is_dir() {
        let prompt_path = source.join("SKILL.md");
        if !prompt_path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "skill directory '{}' must contain SKILL.md",
                    source.display()
                ),
            ));
        }
        return Ok(SkillInstallSource::Directory {
            root: source,
            prompt_path,
        });
    }

    if source
        .extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("md"))
    {
        return Ok(SkillInstallSource::MarkdownFile { path: source });
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "skill source '{}' must be a directory with SKILL.md or a markdown file",
            source.display()
        ),
    ))
}

fn derive_skill_install_name(
    source: &SkillInstallSource,
    declared_name: Option<&str>,
) -> std::io::Result<String> {
    for candidate in [declared_name, source.fallback_name().as_deref()] {
        if let Some(candidate) = candidate.and_then(sanitize_skill_invocation_name) {
            return Ok(candidate);
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "unable to derive an installable invocation name from '{}'",
            source.report_path().display()
        ),
    ))
}

fn sanitize_skill_invocation_name(candidate: &str) -> Option<String> {
    let trimmed = candidate
        .trim()
        .trim_start_matches('/')
        .trim_start_matches('$');
    if trimmed.is_empty() {
        return None;
    }

    let mut sanitized = String::new();
    let mut last_was_separator = false;
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            sanitized.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if (ch.is_whitespace() || matches!(ch, '/' | '\\'))
            && !last_was_separator
            && !sanitized.is_empty()
        {
            sanitized.push('-');
            last_was_separator = true;
        }
    }

    let sanitized = sanitized
        .trim_matches(|ch| matches!(ch, '-' | '_' | '.'))
        .to_string();
    (!sanitized.is_empty()).then_some(sanitized)
}

fn copy_directory_contents(source: &Path, destination: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let entry_type = entry.file_type()?;
        let destination_path = destination.join(entry.file_name());
        if entry_type.is_dir() {
            fs::create_dir_all(&destination_path)?;
            copy_directory_contents(&entry.path(), &destination_path)?;
        } else {
            fs::copy(entry.path(), destination_path)?;
        }
    }
    Ok(())
}

impl SkillInstallSource {
    fn prompt_path(&self) -> &Path {
        match self {
            Self::Directory { prompt_path, .. } => prompt_path,
            Self::MarkdownFile { path } => path,
        }
    }

    fn fallback_name(&self) -> Option<String> {
        match self {
            Self::Directory { root, .. } => root
                .file_name()
                .map(|name| name.to_string_lossy().to_string()),
            Self::MarkdownFile { path } => path
                .file_stem()
                .map(|name| name.to_string_lossy().to_string()),
        }
    }

    fn report_path(&self) -> &Path {
        match self {
            Self::Directory { root, .. } => root,
            Self::MarkdownFile { path } => path,
        }
    }
}

fn push_unique_root(
    roots: &mut Vec<(DefinitionSource, PathBuf)>,
    source: DefinitionSource,
    path: PathBuf,
) {
    if path.is_dir() && !roots.iter().any(|(_, existing)| existing == &path) {
        roots.push((source, path));
    }
}

fn push_unique_skill_root(
    roots: &mut Vec<SkillRoot>,
    source: DefinitionSource,
    path: PathBuf,
    origin: SkillOrigin,
) {
    if path.is_dir() && !roots.iter().any(|existing| existing.path == path) {
        roots.push(SkillRoot {
            source,
            path,
            origin,
        });
    }
}

fn load_agents_from_roots(
    roots: &[(DefinitionSource, PathBuf)],
) -> std::io::Result<Vec<AgentSummary>> {
    let mut agents = Vec::new();
    let mut active_sources = BTreeMap::<String, DefinitionSource>::new();

    for (source, root) in roots {
        let mut root_agents = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.path().extension().is_none_or(|ext| ext != "toml") {
                continue;
            }
            let contents = fs::read_to_string(entry.path())?;
            let fallback_name = entry.path().file_stem().map_or_else(
                || entry.file_name().to_string_lossy().to_string(),
                |stem| stem.to_string_lossy().to_string(),
            );
            root_agents.push(AgentSummary {
                name: parse_toml_string(&contents, "name").unwrap_or(fallback_name),
                description: parse_toml_string(&contents, "description"),
                model: parse_toml_string(&contents, "model"),
                reasoning_effort: parse_toml_string(&contents, "model_reasoning_effort"),
                source: *source,
                shadowed_by: None,
            });
        }
        root_agents.sort_by(|left, right| left.name.cmp(&right.name));

        for mut agent in root_agents {
            let key = agent.name.to_ascii_lowercase();
            if let Some(existing) = active_sources.get(&key) {
                agent.shadowed_by = Some(*existing);
            } else {
                active_sources.insert(key, agent.source);
            }
            agents.push(agent);
        }
    }

    Ok(agents)
}

fn load_skills_from_roots(roots: &[SkillRoot]) -> std::io::Result<Vec<SkillSummary>> {
    let mut skills = Vec::new();
    let mut active_sources = BTreeMap::<String, DefinitionSource>::new();

    for root in roots {
        let mut root_skills = Vec::new();
        for entry in fs::read_dir(&root.path)? {
            let entry = entry?;
            match root.origin {
                SkillOrigin::SkillsDir => {
                    if !entry.path().is_dir() {
                        continue;
                    }
                    let skill_path = entry.path().join("SKILL.md");
                    if !skill_path.is_file() {
                        continue;
                    }
                    let contents = fs::read_to_string(skill_path)?;
                    let (name, description) = parse_skill_frontmatter(&contents);
                    root_skills.push(SkillSummary {
                        name: name
                            .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string()),
                        description,
                        source: root.source,
                        shadowed_by: None,
                        origin: root.origin,
                    });
                }
                SkillOrigin::LegacyCommandsDir => {
                    let path = entry.path();
                    let markdown_path = if path.is_dir() {
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

                    let contents = fs::read_to_string(&markdown_path)?;
                    let fallback_name = markdown_path.file_stem().map_or_else(
                        || entry.file_name().to_string_lossy().to_string(),
                        |stem| stem.to_string_lossy().to_string(),
                    );
                    let (name, description) = parse_skill_frontmatter(&contents);
                    root_skills.push(SkillSummary {
                        name: name.unwrap_or(fallback_name),
                        description,
                        source: root.source,
                        shadowed_by: None,
                        origin: root.origin,
                    });
                }
            }
        }
        root_skills.sort_by(|left, right| left.name.cmp(&right.name));

        for mut skill in root_skills {
            let key = skill.name.to_ascii_lowercase();
            if let Some(existing) = active_sources.get(&key) {
                skill.shadowed_by = Some(*existing);
            } else {
                active_sources.insert(key, skill.source);
            }
            skills.push(skill);
        }
    }

    Ok(skills)
}

fn parse_toml_string(contents: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} =");
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(value) = trimmed.strip_prefix(&prefix) else {
            continue;
        };
        let value = value.trim();
        let Some(value) = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        else {
            continue;
        };
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn parse_skill_frontmatter(contents: &str) -> (Option<String>, Option<String>) {
    let mut lines = contents.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }

    let mut name = None;
    let mut description = None;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("name:") {
            let value = unquote_frontmatter_value(value.trim());
            if !value.is_empty() {
                name = Some(value);
            }
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("description:") {
            let value = unquote_frontmatter_value(value.trim());
            if !value.is_empty() {
                description = Some(value);
            }
        }
    }

    (name, description)
}

fn unquote_frontmatter_value(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|trimmed| trimmed.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|trimmed| trimmed.strip_suffix('\''))
        })
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn render_agents_report(agents: &[AgentSummary]) -> String {
    if agents.is_empty() {
        return "No agents found.".to_string();
    }

    let total_active = agents
        .iter()
        .filter(|agent| agent.shadowed_by.is_none())
        .count();
    let mut lines = vec![
        "Agents".to_string(),
        format!("  {total_active} active agents"),
        String::new(),
    ];

    for scope in [
        DefinitionScope::Project,
        DefinitionScope::UserConfigHome,
        DefinitionScope::UserHome,
    ] {
        let group = agents
            .iter()
            .filter(|agent| agent.source.report_scope() == scope)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }

        lines.push(format!("{}:", scope.label()));
        for agent in group {
            let detail = agent_detail(agent);
            match agent.shadowed_by {
                Some(winner) => lines.push(format!("  (shadowed by {}) {detail}", winner.label())),
                None => lines.push(format!("  {detail}")),
            }
        }
        lines.push(String::new());
    }

    lines.join("\n").trim_end().to_string()
}

fn render_agents_report_json(cwd: &Path, agents: &[AgentSummary]) -> Value {
    let active = agents
        .iter()
        .filter(|agent| agent.shadowed_by.is_none())
        .count();
    json!({
        "kind": "agents",
        "action": "list",
        "working_directory": cwd.display().to_string(),
        "count": agents.len(),
        "summary": {
            "total": agents.len(),
            "active": active,
            "shadowed": agents.len().saturating_sub(active),
        },
        "agents": agents.iter().map(agent_summary_json).collect::<Vec<_>>(),
    })
}

fn agent_detail(agent: &AgentSummary) -> String {
    let mut parts = vec![agent.name.clone()];
    if let Some(description) = &agent.description {
        parts.push(description.clone());
    }
    if let Some(model) = &agent.model {
        parts.push(model.clone());
    }
    if let Some(reasoning) = &agent.reasoning_effort {
        parts.push(reasoning.clone());
    }
    parts.join(" · ")
}

fn render_skills_report(skills: &[SkillSummary]) -> String {
    if skills.is_empty() {
        return "No skills found.".to_string();
    }

    let total_active = skills
        .iter()
        .filter(|skill| skill.shadowed_by.is_none())
        .count();
    let mut lines = vec![
        "Skills".to_string(),
        format!("  {total_active} available skills"),
        String::new(),
    ];

    for scope in [
        DefinitionScope::Project,
        DefinitionScope::UserConfigHome,
        DefinitionScope::UserHome,
    ] {
        let group = skills
            .iter()
            .filter(|skill| skill.source.report_scope() == scope)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }

        lines.push(format!("{}:", scope.label()));
        for skill in group {
            let mut parts = vec![skill.name.clone()];
            if let Some(description) = &skill.description {
                parts.push(description.clone());
            }
            if let Some(detail) = skill.origin.detail_label() {
                parts.push(detail.to_string());
            }
            let detail = parts.join(" · ");
            match skill.shadowed_by {
                Some(winner) => lines.push(format!("  (shadowed by {}) {detail}", winner.label())),
                None => lines.push(format!("  {detail}")),
            }
        }
        lines.push(String::new());
    }

    lines.join("\n").trim_end().to_string()
}

fn render_skills_report_json(skills: &[SkillSummary]) -> Value {
    let active = skills
        .iter()
        .filter(|skill| skill.shadowed_by.is_none())
        .count();
    json!({
        "kind": "skills",
        "action": "list",
        "summary": {
            "total": skills.len(),
            "active": active,
            "shadowed": skills.len().saturating_sub(active),
        },
        "skills": skills.iter().map(skill_summary_json).collect::<Vec<_>>(),
    })
}

fn render_skill_install_report(skill: &InstalledSkill) -> String {
    let mut lines = vec![
        "Skills".to_string(),
        format!("  Result           installed {}", skill.invocation_name),
        format!("  Invoke as        ${}", skill.invocation_name),
    ];
    if let Some(display_name) = &skill.display_name {
        lines.push(format!("  Display name     {display_name}"));
    }
    lines.push(format!("  Source           {}", skill.source.display()));
    lines.push(format!(
        "  Registry         {}",
        skill.registry_root.display()
    ));
    lines.push(format!(
        "  Installed path   {}",
        skill.installed_path.display()
    ));
    lines.join("\n")
}

fn render_skill_install_report_json(skill: &InstalledSkill) -> Value {
    json!({
        "kind": "skills",
        "action": "install",
        "result": "installed",
        "invocation_name": &skill.invocation_name,
        "invoke_as": format!("${}", skill.invocation_name),
        "display_name": &skill.display_name,
        "source": skill.source.display().to_string(),
        "registry_root": skill.registry_root.display().to_string(),
        "installed_path": skill.installed_path.display().to_string(),
    })
}

fn render_mcp_summary_report(
    cwd: &Path,
    servers: &BTreeMap<String, ScopedMcpServerConfig>,
) -> String {
    let mut lines = vec![
        "MCP".to_string(),
        format!("  Working directory {}", cwd.display()),
        format!("  Configured servers {}", servers.len()),
    ];
    if servers.is_empty() {
        lines.push("  No MCP servers configured.".to_string());
        return lines.join("\n");
    }

    lines.push(String::new());
    for (name, server) in servers {
        lines.push(format!(
            "  {name:<16} {transport:<13} {scope:<7} {summary}",
            transport = mcp_transport_label(&server.config),
            scope = config_source_label(server.scope),
            summary = mcp_server_summary(&server.config)
        ));
    }

    lines.join("\n")
}

fn render_mcp_summary_report_json(
    cwd: &Path,
    servers: &BTreeMap<String, ScopedMcpServerConfig>,
) -> Value {
    json!({
        "kind": "mcp",
        "action": "list",
        "working_directory": cwd.display().to_string(),
        "configured_servers": servers.len(),
        "servers": servers
            .iter()
            .map(|(name, server)| mcp_server_json(name, server))
            .collect::<Vec<_>>(),
    })
}

fn render_mcp_server_report(
    cwd: &Path,
    server_name: &str,
    server: Option<&ScopedMcpServerConfig>,
) -> String {
    let Some(server) = server else {
        return format!(
            "MCP\n  Working directory {}\n  Result            server `{server_name}` is not configured",
            cwd.display()
        );
    };

    let mut lines = vec![
        "MCP".to_string(),
        format!("  Working directory {}", cwd.display()),
        format!("  Name              {server_name}"),
        format!("  Scope             {}", config_source_label(server.scope)),
        format!(
            "  Transport         {}",
            mcp_transport_label(&server.config)
        ),
    ];

    match &server.config {
        McpServerConfig::Stdio(config) => {
            lines.push(format!("  Command           {}", config.command));
            lines.push(format!(
                "  Args              {}",
                format_optional_list(&config.args)
            ));
            lines.push(format!(
                "  Env keys          {}",
                format_optional_keys(config.env.keys().cloned().collect())
            ));
            lines.push(format!(
                "  Tool timeout      {}",
                config
                    .tool_call_timeout_ms
                    .map_or_else(|| "<default>".to_string(), |value| format!("{value} ms"))
            ));
        }
        McpServerConfig::Sse(config) | McpServerConfig::Http(config) => {
            lines.push(format!("  URL               {}", config.url));
            lines.push(format!(
                "  Header keys       {}",
                format_optional_keys(config.headers.keys().cloned().collect())
            ));
            lines.push(format!(
                "  Header helper     {}",
                config.headers_helper.as_deref().unwrap_or("<none>")
            ));
            lines.push(format!(
                "  OAuth             {}",
                format_mcp_oauth(config.oauth.as_ref())
            ));
        }
        McpServerConfig::Ws(config) => {
            lines.push(format!("  URL               {}", config.url));
            lines.push(format!(
                "  Header keys       {}",
                format_optional_keys(config.headers.keys().cloned().collect())
            ));
            lines.push(format!(
                "  Header helper     {}",
                config.headers_helper.as_deref().unwrap_or("<none>")
            ));
        }
        McpServerConfig::Sdk(config) => {
            lines.push(format!("  SDK name          {}", config.name));
        }
        McpServerConfig::ManagedProxy(config) => {
            lines.push(format!("  URL               {}", config.url));
            lines.push(format!("  Proxy id          {}", config.id));
        }
    }

    lines.join("\n")
}

fn render_mcp_server_report_json(
    cwd: &Path,
    server_name: &str,
    server: Option<&ScopedMcpServerConfig>,
) -> Value {
    match server {
        Some(server) => json!({
            "kind": "mcp",
            "action": "show",
            "working_directory": cwd.display().to_string(),
            "found": true,
            "server": mcp_server_json(server_name, server),
        }),
        None => json!({
            "kind": "mcp",
            "action": "show",
            "working_directory": cwd.display().to_string(),
            "found": false,
            "server_name": server_name,
            "message": format!("server `{server_name}` is not configured"),
        }),
    }
}

fn normalize_optional_args(args: Option<&str>) -> Option<&str> {
    args.map(str::trim).filter(|value| !value.is_empty())
}

fn is_help_arg(arg: &str) -> bool {
    matches!(arg, "help" | "-h" | "--help")
}

fn help_path_from_args(args: &str) -> Option<Vec<&str>> {
    let parts = args.split_whitespace().collect::<Vec<_>>();
    let help_index = parts.iter().position(|part| is_help_arg(part))?;
    Some(parts[..help_index].to_vec())
}

fn render_agents_usage(unexpected: Option<&str>) -> String {
    let mut lines = vec![
        "Agents".to_string(),
        "  Usage            /agents [list|help]".to_string(),
        "  Direct CLI       claw agents".to_string(),
        "  Sources          .claw/agents, ~/.claw/agents, $CLAW_CONFIG_HOME/agents".to_string(),
    ];
    if let Some(args) = unexpected {
        lines.push(format!("  Unexpected       {args}"));
    }
    lines.join("\n")
}

fn render_agents_usage_json(unexpected: Option<&str>) -> Value {
    json!({
        "kind": "agents",
        "action": "help",
        "usage": {
            "slash_command": "/agents [list|help]",
            "direct_cli": "claw agents [list|help]",
            "sources": [".claw/agents", "~/.claw/agents", "$CLAW_CONFIG_HOME/agents"],
        },
        "unexpected": unexpected,
    })
}

fn render_skills_usage(unexpected: Option<&str>) -> String {
    let mut lines = vec![
        "Skills".to_string(),
        "  Usage            /skills [list|install <path>|help|<skill> [args]]".to_string(),
        "  Alias            /skill".to_string(),
        "  Direct CLI       claw skills [list|install <path>|help|<skill> [args]]".to_string(),
        "  Invoke           /skills help overview -> $help overview".to_string(),
        "  Install root     $CLAW_CONFIG_HOME/skills or ~/.claw/skills".to_string(),
        "  Sources          .claw/skills, .omc/skills, .agents/skills, .codex/skills, .claude/skills, ~/.claw/skills, ~/.omc/skills, ~/.claude/skills/omc-learned, ~/.codex/skills, ~/.claude/skills, legacy /commands".to_string(),
    ];
    if let Some(args) = unexpected {
        lines.push(format!("  Unexpected       {args}"));
    }
    lines.join("\n")
}

fn render_skills_usage_json(unexpected: Option<&str>) -> Value {
    json!({
        "kind": "skills",
        "action": "help",
        "usage": {
            "slash_command": "/skills [list|install <path>|help|<skill> [args]]",
            "aliases": ["/skill"],
            "direct_cli": "claw skills [list|install <path>|help|<skill> [args]]",
            "invoke": "/skills help overview -> $help overview",
            "install_root": "$CLAW_CONFIG_HOME/skills or ~/.claw/skills",
            "sources": [
                ".claw/skills",
                ".omc/skills",
                ".agents/skills",
                ".codex/skills",
                ".claude/skills",
                "~/.claw/skills",
                "~/.omc/skills",
                "~/.claude/skills/omc-learned",
                "~/.codex/skills",
                "~/.claude/skills",
                "legacy /commands",
                "legacy fallback dirs still load automatically"
            ],
        },
        "unexpected": unexpected,
    })
}

fn render_mcp_usage(unexpected: Option<&str>) -> String {
    let mut lines = vec![
        "MCP".to_string(),
        "  Usage            /mcp [list|show <server>|help]".to_string(),
        "  Direct CLI       claw mcp [list|show <server>|help]".to_string(),
        "  Sources          .claw/settings.json, .claw/settings.local.json".to_string(),
    ];
    if let Some(args) = unexpected {
        lines.push(format!("  Unexpected       {args}"));
    }
    lines.join("\n")
}

fn render_mcp_usage_json(unexpected: Option<&str>) -> Value {
    json!({
        "kind": "mcp",
        "action": "help",
        "usage": {
            "slash_command": "/mcp [list|show <server>|help]",
            "direct_cli": "claw mcp [list|show <server>|help]",
            "sources": [".claw/settings.json", ".claw/settings.local.json"],
        },
        "unexpected": unexpected,
    })
}

fn config_source_label(source: ConfigSource) -> &'static str {
    match source {
        ConfigSource::User => "user",
        ConfigSource::Project => "project",
        ConfigSource::Local => "local",
    }
}

fn mcp_transport_label(config: &McpServerConfig) -> &'static str {
    match config {
        McpServerConfig::Stdio(_) => "stdio",
        McpServerConfig::Sse(_) => "sse",
        McpServerConfig::Http(_) => "http",
        McpServerConfig::Ws(_) => "ws",
        McpServerConfig::Sdk(_) => "sdk",
        McpServerConfig::ManagedProxy(_) => "managed-proxy",
    }
}

fn mcp_server_summary(config: &McpServerConfig) -> String {
    match config {
        McpServerConfig::Stdio(config) => {
            if config.args.is_empty() {
                config.command.clone()
            } else {
                format!("{} {}", config.command, config.args.join(" "))
            }
        }
        McpServerConfig::Sse(config) | McpServerConfig::Http(config) => config.url.clone(),
        McpServerConfig::Ws(config) => config.url.clone(),
        McpServerConfig::Sdk(config) => config.name.clone(),
        McpServerConfig::ManagedProxy(config) => format!("{} ({})", config.id, config.url),
    }
}

fn format_optional_list(values: &[String]) -> String {
    if values.is_empty() {
        "<none>".to_string()
    } else {
        values.join(" ")
    }
}

fn format_optional_keys(mut keys: Vec<String>) -> String {
    if keys.is_empty() {
        return "<none>".to_string();
    }
    keys.sort();
    keys.join(", ")
}

fn format_mcp_oauth(oauth: Option<&McpOAuthConfig>) -> String {
    let Some(oauth) = oauth else {
        return "<none>".to_string();
    };

    let mut parts = Vec::new();
    if let Some(client_id) = &oauth.client_id {
        parts.push(format!("client_id={client_id}"));
    }
    if let Some(port) = oauth.callback_port {
        parts.push(format!("callback_port={port}"));
    }
    if let Some(url) = &oauth.auth_server_metadata_url {
        parts.push(format!("metadata_url={url}"));
    }
    if let Some(xaa) = oauth.xaa {
        parts.push(format!("xaa={xaa}"));
    }
    if parts.is_empty() {
        "enabled".to_string()
    } else {
        parts.join(", ")
    }
}

fn definition_source_id(source: DefinitionSource) -> &'static str {
    match source {
        DefinitionSource::ProjectClaw
        | DefinitionSource::ProjectCodex
        | DefinitionSource::ProjectClaude => "project_claw",
        DefinitionSource::UserClawConfigHome | DefinitionSource::UserCodexHome => {
            "user_claw_config_home"
        }
        DefinitionSource::UserClaw | DefinitionSource::UserCodex | DefinitionSource::UserClaude => {
            "user_claw"
        }
    }
}

fn definition_source_json(source: DefinitionSource) -> Value {
    json!({
        "id": definition_source_id(source),
        "label": source.label(),
    })
}

fn agent_summary_json(agent: &AgentSummary) -> Value {
    json!({
        "name": &agent.name,
        "description": &agent.description,
        "model": &agent.model,
        "reasoning_effort": &agent.reasoning_effort,
        "source": definition_source_json(agent.source),
        "active": agent.shadowed_by.is_none(),
        "shadowed_by": agent.shadowed_by.map(definition_source_json),
    })
}

fn skill_origin_id(origin: SkillOrigin) -> &'static str {
    match origin {
        SkillOrigin::SkillsDir => "skills_dir",
        SkillOrigin::LegacyCommandsDir => "legacy_commands_dir",
    }
}

fn skill_origin_json(origin: SkillOrigin) -> Value {
    json!({
        "id": skill_origin_id(origin),
        "detail_label": origin.detail_label(),
    })
}

fn skill_summary_json(skill: &SkillSummary) -> Value {
    json!({
        "name": &skill.name,
        "description": &skill.description,
        "source": definition_source_json(skill.source),
        "origin": skill_origin_json(skill.origin),
        "active": skill.shadowed_by.is_none(),
        "shadowed_by": skill.shadowed_by.map(definition_source_json),
    })
}

fn config_source_id(source: ConfigSource) -> &'static str {
    match source {
        ConfigSource::User => "user",
        ConfigSource::Project => "project",
        ConfigSource::Local => "local",
    }
}

fn config_source_json(source: ConfigSource) -> Value {
    json!({
        "id": config_source_id(source),
        "label": config_source_label(source),
    })
}

fn mcp_transport_json(config: &McpServerConfig) -> Value {
    let label = mcp_transport_label(config);
    json!({
        "id": label,
        "label": label,
    })
}

fn mcp_oauth_json(oauth: Option<&McpOAuthConfig>) -> Value {
    let Some(oauth) = oauth else {
        return Value::Null;
    };
    json!({
        "client_id": &oauth.client_id,
        "callback_port": oauth.callback_port,
        "auth_server_metadata_url": &oauth.auth_server_metadata_url,
        "xaa": oauth.xaa,
    })
}

fn mcp_server_details_json(config: &McpServerConfig) -> Value {
    match config {
        McpServerConfig::Stdio(config) => json!({
            "command": &config.command,
            "args": &config.args,
            "env_keys": config.env.keys().cloned().collect::<Vec<_>>(),
            "tool_call_timeout_ms": config.tool_call_timeout_ms,
        }),
        McpServerConfig::Sse(config) | McpServerConfig::Http(config) => json!({
            "url": &config.url,
            "header_keys": config.headers.keys().cloned().collect::<Vec<_>>(),
            "headers_helper": &config.headers_helper,
            "oauth": mcp_oauth_json(config.oauth.as_ref()),
        }),
        McpServerConfig::Ws(config) => json!({
            "url": &config.url,
            "header_keys": config.headers.keys().cloned().collect::<Vec<_>>(),
            "headers_helper": &config.headers_helper,
        }),
        McpServerConfig::Sdk(config) => json!({
            "name": &config.name,
        }),
        McpServerConfig::ManagedProxy(config) => json!({
            "url": &config.url,
            "id": &config.id,
        }),
    }
}

fn mcp_server_json(name: &str, server: &ScopedMcpServerConfig) -> Value {
    json!({
        "name": name,
        "scope": config_source_json(server.scope),
        "transport": mcp_transport_json(&server.config),
        "summary": mcp_server_summary(&server.config),
        "details": mcp_server_details_json(&server.config),
    })
}

#[must_use]
pub fn handle_slash_command(
    input: &str,
    session: &Session,
    compaction: CompactionConfig,
) -> Option<SlashCommandResult> {
    let command = match SlashCommand::parse(input) {
        Ok(Some(command)) => command,
        Ok(None) => return None,
        Err(error) => {
            return Some(SlashCommandResult {
                message: error.to_string(),
                session: session.clone(),
            });
        }
    };

    match command {
        SlashCommand::Compact => {
            let result = compact_session(session, compaction);
            let message = if result.removed_message_count == 0 {
                "Compaction skipped: session is below the compaction threshold.".to_string()
            } else {
                format!(
                    "Compacted {} messages into a resumable system summary.",
                    result.removed_message_count
                )
            };
            Some(SlashCommandResult {
                message,
                session: result.compacted_session,
            })
        }
        SlashCommand::Help => Some(SlashCommandResult {
            message: render_slash_command_help(),
            session: session.clone(),
        }),
        SlashCommand::Status
        | SlashCommand::Bughunter { .. }
        | SlashCommand::Commit
        | SlashCommand::Pr { .. }
        | SlashCommand::Issue { .. }
        | SlashCommand::Ultraplan { .. }
        | SlashCommand::Teleport { .. }
        | SlashCommand::DebugToolCall
        | SlashCommand::Sandbox
        | SlashCommand::Model { .. }
        | SlashCommand::Permissions { .. }
        | SlashCommand::Clear { .. }
        | SlashCommand::Cost
        | SlashCommand::Resume { .. }
        | SlashCommand::Config { .. }
        | SlashCommand::Mcp { .. }
        | SlashCommand::Memory
        | SlashCommand::Init
        | SlashCommand::Diff
        | SlashCommand::Version
        | SlashCommand::Export { .. }
        | SlashCommand::Session { .. }
        | SlashCommand::Plugins { .. }
        | SlashCommand::Agents { .. }
        | SlashCommand::Skills { .. }
        | SlashCommand::Doctor
        | SlashCommand::Login
        | SlashCommand::Logout
        | SlashCommand::Vim
        | SlashCommand::Upgrade
        | SlashCommand::Stats
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
        | SlashCommand::History { .. }
        | SlashCommand::Lsp { .. }
        | SlashCommand::Unknown(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_skills_slash_command, handle_agents_slash_command_json,
        handle_plugins_slash_command, handle_skills_slash_command_json, handle_slash_command,
        load_agents_from_roots, load_skills_from_roots, render_agents_report,
        render_agents_report_json, render_mcp_report_json_for, render_plugins_report,
        render_plugins_report_with_failures, render_skills_report, render_slash_command_help,
        render_slash_command_help_detail, resolve_skill_path, resume_supported_slash_commands,
        slash_command_specs, suggest_slash_commands, validate_slash_command_input,
        DefinitionSource, SkillOrigin, SkillRoot, SkillSlashDispatch, SlashCommand,
    };
    use plugins::{
        PluginError, PluginKind, PluginLoadFailure, PluginManager, PluginManagerConfig,
        PluginMetadata, PluginSummary,
    };
    use runtime::{
        CompactionConfig, ConfigLoader, ContentBlock, ConversationMessage, MessageRole, Session,
    };
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("commands-plugin-{label}-{nanos}"))
    }

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

    fn restore_env_var(key: &str, original: Option<OsString>) {
        match original {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    fn write_external_plugin(root: &Path, name: &str, version: &str) {
        fs::create_dir_all(root.join(".claude-plugin")).expect("manifest dir");
        fs::write(
            root.join(".claude-plugin").join("plugin.json"),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"commands plugin\"\n}}"
            ),
        )
        .expect("write manifest");
    }

    fn write_bundled_plugin(root: &Path, name: &str, version: &str, default_enabled: bool) {
        fs::create_dir_all(root.join(".claude-plugin")).expect("manifest dir");
        fs::write(
            root.join(".claude-plugin").join("plugin.json"),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"bundled commands plugin\",\n  \"defaultEnabled\": {}\n}}",
                if default_enabled { "true" } else { "false" }
            ),
        )
        .expect("write bundled manifest");
    }

    fn write_agent(root: &Path, name: &str, description: &str, model: &str, reasoning: &str) {
        fs::create_dir_all(root).expect("agent root");
        fs::write(
            root.join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\ndescription = \"{description}\"\nmodel = \"{model}\"\nmodel_reasoning_effort = \"{reasoning}\"\n"
            ),
        )
        .expect("write agent");
    }

    fn write_skill(root: &Path, name: &str, description: &str) {
        let skill_root = root.join(name);
        fs::create_dir_all(&skill_root).expect("skill root");
        fs::write(
            skill_root.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )
        .expect("write skill");
    }

    fn write_legacy_command(root: &Path, name: &str, description: &str) {
        fs::create_dir_all(root).expect("commands root");
        fs::write(
            root.join(format!("{name}.md")),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )
        .expect("write command");
    }

    fn parse_error_message(input: &str) -> String {
        SlashCommand::parse(input)
            .expect_err("slash command should be rejected")
            .to_string()
    }

    #[allow(clippy::too_many_lines)]
    #[test]
    fn parses_supported_slash_commands() {
        assert_eq!(SlashCommand::parse("/help"), Ok(Some(SlashCommand::Help)));
        assert_eq!(
            SlashCommand::parse(" /status "),
            Ok(Some(SlashCommand::Status))
        );
        assert_eq!(
            SlashCommand::parse("/sandbox"),
            Ok(Some(SlashCommand::Sandbox))
        );
        assert_eq!(
            SlashCommand::parse("/bughunter runtime"),
            Ok(Some(SlashCommand::Bughunter {
                scope: Some("runtime".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/commit"),
            Ok(Some(SlashCommand::Commit))
        );
        assert_eq!(
            SlashCommand::parse("/pr ready for review"),
            Ok(Some(SlashCommand::Pr {
                context: Some("ready for review".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/issue flaky test"),
            Ok(Some(SlashCommand::Issue {
                context: Some("flaky test".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/ultraplan ship both features"),
            Ok(Some(SlashCommand::Ultraplan {
                task: Some("ship both features".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/teleport conversation.rs"),
            Ok(Some(SlashCommand::Teleport {
                target: Some("conversation.rs".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/debug-tool-call"),
            Ok(Some(SlashCommand::DebugToolCall))
        );
        assert_eq!(
            SlashCommand::parse("/bughunter runtime"),
            Ok(Some(SlashCommand::Bughunter {
                scope: Some("runtime".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/commit"),
            Ok(Some(SlashCommand::Commit))
        );
        assert_eq!(
            SlashCommand::parse("/pr ready for review"),
            Ok(Some(SlashCommand::Pr {
                context: Some("ready for review".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/issue flaky test"),
            Ok(Some(SlashCommand::Issue {
                context: Some("flaky test".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/ultraplan ship both features"),
            Ok(Some(SlashCommand::Ultraplan {
                task: Some("ship both features".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/teleport conversation.rs"),
            Ok(Some(SlashCommand::Teleport {
                target: Some("conversation.rs".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/debug-tool-call"),
            Ok(Some(SlashCommand::DebugToolCall))
        );
        assert_eq!(
            SlashCommand::parse("/model claude-opus"),
            Ok(Some(SlashCommand::Model {
                model: Some("claude-opus".to_string()),
            }))
        );
        assert_eq!(
            SlashCommand::parse("/model"),
            Ok(Some(SlashCommand::Model { model: None }))
        );
        assert_eq!(
            SlashCommand::parse("/permissions read-only"),
            Ok(Some(SlashCommand::Permissions {
                mode: Some("read-only".to_string()),
            }))
        );
        assert_eq!(
            SlashCommand::parse("/clear"),
            Ok(Some(SlashCommand::Clear { confirm: false }))
        );
        assert_eq!(
            SlashCommand::parse("/clear --confirm"),
            Ok(Some(SlashCommand::Clear { confirm: true }))
        );
        assert_eq!(SlashCommand::parse("/cost"), Ok(Some(SlashCommand::Cost)));
        assert_eq!(
            SlashCommand::parse("/resume session.json"),
            Ok(Some(SlashCommand::Resume {
                session_path: Some("session.json".to_string()),
            }))
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
            SlashCommand::parse("/mcp"),
            Ok(Some(SlashCommand::Mcp {
                action: None,
                target: None
            }))
        );
        assert_eq!(
            SlashCommand::parse("/mcp show remote"),
            Ok(Some(SlashCommand::Mcp {
                action: Some("show".to_string()),
                target: Some("remote".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/memory"),
            Ok(Some(SlashCommand::Memory))
        );
        assert_eq!(SlashCommand::parse("/init"), Ok(Some(SlashCommand::Init)));
        assert_eq!(SlashCommand::parse("/diff"), Ok(Some(SlashCommand::Diff)));
        assert_eq!(
            SlashCommand::parse("/version"),
            Ok(Some(SlashCommand::Version))
        );
        assert_eq!(
            SlashCommand::parse("/export notes.txt"),
            Ok(Some(SlashCommand::Export {
                path: Some("notes.txt".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/session switch abc123"),
            Ok(Some(SlashCommand::Session {
                action: Some("switch".to_string()),
                target: Some("abc123".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/plugins install demo"),
            Ok(Some(SlashCommand::Plugins {
                action: Some("install".to_string()),
                target: Some("demo".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/plugins list"),
            Ok(Some(SlashCommand::Plugins {
                action: Some("list".to_string()),
                target: None
            }))
        );
        assert_eq!(
            SlashCommand::parse("/plugins enable demo"),
            Ok(Some(SlashCommand::Plugins {
                action: Some("enable".to_string()),
                target: Some("demo".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/skills install ./fixtures/help-skill"),
            Ok(Some(SlashCommand::Skills {
                args: Some("install ./fixtures/help-skill".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/plugins disable demo"),
            Ok(Some(SlashCommand::Plugins {
                action: Some("disable".to_string()),
                target: Some("demo".to_string())
            }))
        );
        assert_eq!(
            SlashCommand::parse("/session fork incident-review"),
            Ok(Some(SlashCommand::Session {
                action: Some("fork".to_string()),
                target: Some("incident-review".to_string())
            }))
        );
    }

    #[test]
    fn parses_history_command_without_count() {
        // given
        let input = "/history";

        // when
        let parsed = SlashCommand::parse(input);

        // then
        assert_eq!(parsed, Ok(Some(SlashCommand::History { count: None })));
    }

    #[test]
    fn parses_history_command_with_numeric_count() {
        // given
        let input = "/history 25";

        // when
        let parsed = SlashCommand::parse(input);

        // then
        assert_eq!(
            parsed,
            Ok(Some(SlashCommand::History {
                count: Some("25".to_string())
            }))
        );
    }

    #[test]
    fn rejects_history_with_extra_arguments() {
        // given
        let input = "/history 25 extra";

        // when
        let error = parse_error_message(input);

        // then
        assert!(error.contains("Usage: /history [count]"));
    }

    #[test]
    fn rejects_unexpected_arguments_for_no_arg_commands() {
        // given
        let input = "/compact now";

        // when
        let error = parse_error_message(input);

        // then
        assert!(error.contains("Unexpected arguments for /compact."));
        assert!(error.contains("  Usage            /compact"));
        assert!(error.contains("  Summary          Compact local session history"));
    }

    #[test]
    fn rejects_invalid_argument_values() {
        // given
        let input = "/permissions admin";

        // when
        let error = parse_error_message(input);

        // then
        assert!(error.contains(
            "Unsupported /permissions mode 'admin'. Use read-only, workspace-write, or danger-full-access."
        ));
        assert!(error.contains(
            "  Usage            /permissions [read-only|workspace-write|danger-full-access]"
        ));
    }

    #[test]
    fn rejects_missing_required_arguments() {
        // given
        let input = "/teleport";

        // when
        let error = parse_error_message(input);

        // then
        assert!(error.contains("Usage: /teleport <symbol-or-path>"));
        assert!(error.contains("  Category         Tools"));
    }

    #[test]
    fn rejects_invalid_session_and_plugin_shapes() {
        // given
        let session_input = "/session switch";
        let plugin_input = "/plugins list extra";

        // when
        let session_error = parse_error_message(session_input);
        let plugin_error = parse_error_message(plugin_input);

        // then
        assert!(session_error.contains("Usage: /session switch <session-id>"));
        assert!(session_error.contains("/session"));
        assert!(plugin_error.contains("Usage: /plugin list"));
        assert!(plugin_error.contains("Aliases          /plugins, /marketplace"));
    }

    #[test]
    fn rejects_invalid_agents_arguments() {
        // given
        let agents_input = "/agents show planner";

        // when
        let agents_error = parse_error_message(agents_input);

        // then
        assert!(agents_error.contains(
            "Unexpected arguments for /agents: show planner. Use /agents, /agents list, or /agents help."
        ));
        assert!(agents_error.contains("  Usage            /agents [list|help]"));
    }

    #[test]
    fn accepts_skills_invocation_arguments_for_prompt_dispatch() {
        assert_eq!(
            SlashCommand::parse("/skills help overview"),
            Ok(Some(SlashCommand::Skills {
                args: Some("help overview".to_string()),
            }))
        );
        assert_eq!(
            classify_skills_slash_command(Some("help overview")),
            SkillSlashDispatch::Invoke("$help overview".to_string())
        );
        assert_eq!(
            classify_skills_slash_command(Some("/test")),
            SkillSlashDispatch::Invoke("$test".to_string())
        );
        assert_eq!(
            classify_skills_slash_command(Some("install ./skill-pack")),
            SkillSlashDispatch::Local
        );
    }

    #[test]
    fn rejects_invalid_mcp_arguments() {
        let show_error = parse_error_message("/mcp show alpha beta");
        assert!(show_error.contains("Unexpected arguments for /mcp show."));
        assert!(show_error.contains("  Usage            /mcp show <server>"));

        let action_error = parse_error_message("/mcp inspect alpha");
        assert!(action_error
            .contains("Unknown /mcp action 'inspect'. Use list, show <server>, or help."));
        assert!(action_error.contains("  Usage            /mcp [list|show <server>|help]"));
    }

    #[test]
    fn removed_login_and_logout_commands_report_env_auth_guidance() {
        let login_error = parse_error_message("/login");
        assert!(login_error.contains("ANTHROPIC_API_KEY"));
        let logout_error = parse_error_message("/logout");
        assert!(logout_error.contains("ANTHROPIC_AUTH_TOKEN"));
    }

    #[test]
    fn renders_help_from_shared_specs() {
        let help = render_slash_command_help();
        assert!(help.contains("Start here        /status, /diff, /agents, /skills, /commit"));
        assert!(help.contains("[resume]          also works with --resume SESSION.jsonl"));
        assert!(help.contains("Session"));
        assert!(help.contains("Tools"));
        assert!(help.contains("Config"));
        assert!(help.contains("Debug"));
        assert!(help.contains("/help"));
        assert!(help.contains("/status"));
        assert!(help.contains("/sandbox"));
        assert!(help.contains("/compact"));
        assert!(help.contains("/bughunter [scope]"));
        assert!(help.contains("/commit"));
        assert!(help.contains("/pr [context]"));
        assert!(help.contains("/issue [context]"));
        assert!(help.contains("/ultraplan [task]"));
        assert!(help.contains("/teleport <symbol-or-path>"));
        assert!(help.contains("/debug-tool-call"));
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
        assert!(help.contains("/session"), "help must mention /session");
        assert!(help.contains("/sandbox"));
        assert!(help.contains(
            "/plugin [list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]"
        ));
        assert!(help.contains("aliases: /plugins, /marketplace"));
        assert!(help.contains("/agents [list|help]"));
        assert!(help.contains("/skills [list|install <path>|help|<skill> [args]]"));
        assert!(help.contains("aliases: /skill"));
        assert!(!help.contains("/login"));
        assert!(!help.contains("/logout"));
        assert_eq!(slash_command_specs().len(), 140);
        assert!(resume_supported_slash_commands().len() >= 39);
    }

    #[test]
    fn renders_help_with_grouped_categories_and_keyboard_shortcuts() {
        // given
        let categories = ["Session", "Tools", "Config", "Debug"];

        // when
        let help = render_slash_command_help();

        // then
        for category in categories {
            assert!(
                help.contains(category),
                "expected help to contain category {category}"
            );
        }
        let session_index = help.find("Session").expect("Session header should exist");
        let tools_index = help.find("Tools").expect("Tools header should exist");
        let config_index = help.find("Config").expect("Config header should exist");
        let debug_index = help.find("Debug").expect("Debug header should exist");
        assert!(session_index < tools_index);
        assert!(tools_index < config_index);
        assert!(config_index < debug_index);

        assert!(help.contains("Keyboard shortcuts"));
        assert!(help.contains("Up/Down              Navigate prompt history"));
        assert!(help.contains("Tab                  Complete commands, modes, and recent sessions"));
        assert!(help.contains("Ctrl-C               Clear input (or exit on empty prompt)"));
        assert!(help.contains("Shift+Enter/Ctrl+J   Insert a newline"));

        // every command should still render with a summary line
        for spec in slash_command_specs() {
            let usage = match spec.argument_hint {
                Some(hint) => format!("/{} {hint}", spec.name),
                None => format!("/{}", spec.name),
            };
            assert!(
                help.contains(&usage),
                "expected help to contain command {usage}"
            );
            assert!(
                help.contains(spec.summary),
                "expected help to contain summary for /{}",
                spec.name
            );
        }
    }

    #[test]
    fn renders_per_command_help_detail() {
        // given
        let command = "plugins";

        // when
        let help = render_slash_command_help_detail(command).expect("detail help should exist");

        // then
        assert!(help.contains("/plugin"));
        assert!(help.contains("Summary          Manage Claw Code plugins"));
        assert!(help.contains("Aliases          /plugins, /marketplace"));
        assert!(help.contains("Category         Tools"));
    }

    #[test]
    fn renders_per_command_help_detail_for_mcp() {
        let help = render_slash_command_help_detail("mcp").expect("detail help should exist");
        assert!(help.contains("/mcp"));
        assert!(help.contains("Summary          Inspect configured MCP servers"));
        assert!(help.contains("Category         Tools"));
        assert!(help.contains("Resume           Supported with --resume SESSION.jsonl"));
    }

    #[test]
    fn validate_slash_command_input_rejects_extra_single_value_arguments() {
        // given
        let session_input = "/session switch current next";
        let plugin_input = "/plugin enable demo extra";

        // when
        let session_error = validate_slash_command_input(session_input)
            .expect_err("session input should be rejected")
            .to_string();
        let plugin_error = validate_slash_command_input(plugin_input)
            .expect_err("plugin input should be rejected")
            .to_string();

        // then
        assert!(session_error.contains("Unexpected arguments for /session switch."));
        assert!(session_error.contains("  Usage            /session switch <session-id>"));
        assert!(plugin_error.contains("Unexpected arguments for /plugin enable."));
        assert!(plugin_error.contains("  Usage            /plugin enable <name>"));
    }

    #[test]
    fn suggests_closest_slash_commands_for_typos_and_aliases() {
        let suggestions = suggest_slash_commands("stats", 3);
        assert!(suggestions.contains(&"/stats".to_string()));
        assert!(suggestions.contains(&"/status".to_string()));
        assert!(suggestions.len() <= 3);
        let plugin_suggestions = suggest_slash_commands("/plugns", 3);
        assert!(plugin_suggestions.contains(&"/plugin".to_string()));
        assert_eq!(suggest_slash_commands("zzz", 3), Vec::<String>::new());
    }

    #[test]
    fn compacts_sessions_via_slash_command() {
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage::user_text("a ".repeat(200)),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "b ".repeat(200),
            }]),
            ConversationMessage::tool_result("1", "bash", "ok ".repeat(200), false),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "recent".to_string(),
            }]),
        ];

        let result = handle_slash_command(
            "/compact",
            &session,
            CompactionConfig {
                preserve_recent_messages: 2,
                max_estimated_tokens: 1,
            },
        )
        .expect("slash command should be handled");

        // With the tool-use/tool-result boundary guard the compaction may
        // preserve one extra message, so 1 or 2 messages may be removed.
        assert!(
            result.message.contains("Compacted 1 messages")
                || result.message.contains("Compacted 2 messages"),
            "unexpected compaction message: {}",
            result.message
        );
        assert_eq!(result.session.messages[0].role, MessageRole::System);
    }

    #[test]
    fn help_command_is_non_mutating() {
        let session = Session::new();
        let result = handle_slash_command("/help", &session, CompactionConfig::default())
            .expect("help command should be handled");
        assert_eq!(result.session, session);
        assert!(result.message.contains("Slash commands"));
    }

    #[test]
    fn ignores_unknown_or_runtime_bound_slash_commands() {
        let session = Session::new();
        assert!(handle_slash_command("/unknown", &session, CompactionConfig::default()).is_none());
        assert!(handle_slash_command("/status", &session, CompactionConfig::default()).is_none());
        assert!(handle_slash_command("/sandbox", &session, CompactionConfig::default()).is_none());
        assert!(
            handle_slash_command("/bughunter", &session, CompactionConfig::default()).is_none()
        );
        assert!(handle_slash_command("/commit", &session, CompactionConfig::default()).is_none());
        assert!(handle_slash_command("/pr", &session, CompactionConfig::default()).is_none());
        assert!(handle_slash_command("/issue", &session, CompactionConfig::default()).is_none());
        assert!(
            handle_slash_command("/ultraplan", &session, CompactionConfig::default()).is_none()
        );
        assert!(
            handle_slash_command("/teleport foo", &session, CompactionConfig::default()).is_none()
        );
        assert!(
            handle_slash_command("/debug-tool-call", &session, CompactionConfig::default())
                .is_none()
        );
        assert!(
            handle_slash_command("/model claude", &session, CompactionConfig::default()).is_none()
        );
        assert!(handle_slash_command(
            "/permissions read-only",
            &session,
            CompactionConfig::default()
        )
        .is_none());
        assert!(handle_slash_command("/clear", &session, CompactionConfig::default()).is_none());
        assert!(
            handle_slash_command("/clear --confirm", &session, CompactionConfig::default())
                .is_none()
        );
        assert!(handle_slash_command("/cost", &session, CompactionConfig::default()).is_none());
        assert!(handle_slash_command(
            "/resume session.json",
            &session,
            CompactionConfig::default()
        )
        .is_none());
        assert!(handle_slash_command(
            "/resume session.jsonl",
            &session,
            CompactionConfig::default()
        )
        .is_none());
        assert!(handle_slash_command("/config", &session, CompactionConfig::default()).is_none());
        assert!(
            handle_slash_command("/config env", &session, CompactionConfig::default()).is_none()
        );
        assert!(handle_slash_command("/mcp list", &session, CompactionConfig::default()).is_none());
        assert!(handle_slash_command("/diff", &session, CompactionConfig::default()).is_none());
        assert!(handle_slash_command("/version", &session, CompactionConfig::default()).is_none());
        assert!(
            handle_slash_command("/export note.txt", &session, CompactionConfig::default())
                .is_none()
        );
        assert!(
            handle_slash_command("/session list", &session, CompactionConfig::default()).is_none()
        );
        assert!(
            handle_slash_command("/plugins list", &session, CompactionConfig::default()).is_none()
        );
    }

    #[test]
    fn renders_plugins_report_with_name_version_and_status() {
        let rendered = render_plugins_report(&[
            PluginSummary {
                metadata: PluginMetadata {
                    id: "demo@external".to_string(),
                    name: "demo".to_string(),
                    version: "1.2.3".to_string(),
                    description: "demo plugin".to_string(),
                    kind: PluginKind::External,
                    source: "demo".to_string(),
                    default_enabled: false,
                    root: None,
                },
                enabled: true,
            },
            PluginSummary {
                metadata: PluginMetadata {
                    id: "sample@external".to_string(),
                    name: "sample".to_string(),
                    version: "0.9.0".to_string(),
                    description: "sample plugin".to_string(),
                    kind: PluginKind::External,
                    source: "sample".to_string(),
                    default_enabled: false,
                    root: None,
                },
                enabled: false,
            },
        ]);

        assert!(rendered.contains("demo"));
        assert!(rendered.contains("v1.2.3"));
        assert!(rendered.contains("enabled"));
        assert!(rendered.contains("sample"));
        assert!(rendered.contains("v0.9.0"));
        assert!(rendered.contains("disabled"));
    }

    #[test]
    fn renders_plugins_report_with_broken_plugin_warnings() {
        let rendered = render_plugins_report_with_failures(
            &[PluginSummary {
                metadata: PluginMetadata {
                    id: "demo@external".to_string(),
                    name: "demo".to_string(),
                    version: "1.2.3".to_string(),
                    description: "demo plugin".to_string(),
                    kind: PluginKind::External,
                    source: "demo".to_string(),
                    default_enabled: false,
                    root: None,
                },
                enabled: true,
            }],
            &[PluginLoadFailure::new(
                PathBuf::from("/tmp/broken-plugin"),
                PluginKind::External,
                "broken".to_string(),
                PluginError::InvalidManifest("hook path `hooks/pre.sh` does not exist".to_string()),
            )],
        );

        assert!(rendered.contains("Warnings:"));
        assert!(rendered.contains("Failed to load external plugin"));
        assert!(rendered.contains("/tmp/broken-plugin"));
        assert!(rendered.contains("does not exist"));
    }

    #[test]
    fn lists_agents_from_project_and_user_roots() {
        let workspace = temp_dir("agents-workspace");
        let project_agents = workspace.join(".codex").join("agents");
        let user_home = temp_dir("agents-home");
        let user_agents = user_home.join(".claude").join("agents");

        write_agent(
            &project_agents,
            "planner",
            "Project planner",
            "gpt-5.4",
            "medium",
        );
        write_agent(
            &user_agents,
            "planner",
            "User planner",
            "gpt-5.4-mini",
            "high",
        );
        write_agent(
            &user_agents,
            "verifier",
            "Verification agent",
            "gpt-5.4-mini",
            "high",
        );

        let roots = vec![
            (DefinitionSource::ProjectCodex, project_agents),
            (DefinitionSource::UserCodex, user_agents),
        ];
        let report =
            render_agents_report(&load_agents_from_roots(&roots).expect("agent roots should load"));

        assert!(report.contains("Agents"));
        assert!(report.contains("2 active agents"));
        assert!(report.contains("Project roots:"));
        assert!(report.contains("planner · Project planner · gpt-5.4 · medium"));
        assert!(report.contains("User home roots:"));
        assert!(report.contains("(shadowed by Project roots) planner · User planner"));
        assert!(report.contains("verifier · Verification agent · gpt-5.4-mini · high"));

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(user_home);
    }

    #[test]
    fn renders_agents_reports_as_json() {
        let workspace = temp_dir("agents-json-workspace");
        let project_agents = workspace.join(".codex").join("agents");
        let user_home = temp_dir("agents-json-home");
        let user_agents = user_home.join(".codex").join("agents");

        write_agent(
            &project_agents,
            "planner",
            "Project planner",
            "gpt-5.4",
            "medium",
        );
        write_agent(
            &project_agents,
            "verifier",
            "Verification agent",
            "gpt-5.4-mini",
            "high",
        );
        write_agent(
            &user_agents,
            "planner",
            "User planner",
            "gpt-5.4-mini",
            "high",
        );

        let roots = vec![
            (DefinitionSource::ProjectCodex, project_agents),
            (DefinitionSource::UserCodex, user_agents),
        ];
        let report = render_agents_report_json(
            &workspace,
            &load_agents_from_roots(&roots).expect("agent roots should load"),
        );

        assert_eq!(report["kind"], "agents");
        assert_eq!(report["action"], "list");
        assert_eq!(report["working_directory"], workspace.display().to_string());
        assert_eq!(report["count"], 3);
        assert_eq!(report["summary"]["active"], 2);
        assert_eq!(report["summary"]["shadowed"], 1);
        assert_eq!(report["agents"][0]["name"], "planner");
        assert_eq!(report["agents"][0]["model"], "gpt-5.4");
        assert_eq!(report["agents"][0]["active"], true);
        assert_eq!(report["agents"][1]["name"], "verifier");
        assert_eq!(report["agents"][2]["name"], "planner");
        assert_eq!(report["agents"][2]["active"], false);
        assert_eq!(report["agents"][2]["shadowed_by"]["id"], "project_claw");

        let help = handle_agents_slash_command_json(Some("help"), &workspace).expect("agents help");
        assert_eq!(help["kind"], "agents");
        assert_eq!(help["action"], "help");
        assert_eq!(help["usage"]["direct_cli"], "claw agents [list|help]");

        let unexpected = handle_agents_slash_command_json(Some("show planner"), &workspace)
            .expect("agents usage");
        assert_eq!(unexpected["action"], "help");
        assert_eq!(unexpected["unexpected"], "show planner");

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(user_home);
    }

    #[test]
    fn lists_skills_from_project_and_user_roots() {
        let workspace = temp_dir("skills-workspace");
        let project_skills = workspace.join(".codex").join("skills");
        let project_commands = workspace.join(".claude").join("commands");
        let user_home = temp_dir("skills-home");
        let user_skills = user_home.join(".codex").join("skills");

        write_skill(&project_skills, "plan", "Project planning guidance");
        write_legacy_command(&project_commands, "deploy", "Legacy deployment guidance");
        write_skill(&user_skills, "plan", "User planning guidance");
        write_skill(&user_skills, "help", "Help guidance");

        let roots = vec![
            SkillRoot {
                source: DefinitionSource::ProjectCodex,
                path: project_skills,
                origin: SkillOrigin::SkillsDir,
            },
            SkillRoot {
                source: DefinitionSource::ProjectClaude,
                path: project_commands,
                origin: SkillOrigin::LegacyCommandsDir,
            },
            SkillRoot {
                source: DefinitionSource::UserCodex,
                path: user_skills,
                origin: SkillOrigin::SkillsDir,
            },
        ];
        let report =
            render_skills_report(&load_skills_from_roots(&roots).expect("skill roots should load"));

        assert!(report.contains("Skills"));
        assert!(report.contains("3 available skills"));
        assert!(report.contains("Project roots:"));
        assert!(report.contains("plan · Project planning guidance"));
        assert!(report.contains("deploy · Legacy deployment guidance · legacy /commands"));
        assert!(report.contains("User home roots:"));
        assert!(report.contains("(shadowed by Project roots) plan · User planning guidance"));
        assert!(report.contains("help · Help guidance"));

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(user_home);
    }

    #[test]
    fn resolves_project_skills_and_legacy_commands_from_shared_registry() {
        let workspace = temp_dir("resolve-project-skills");
        let project_skills = workspace.join(".claw").join("skills");
        let legacy_commands = workspace.join(".claw").join("commands");

        write_skill(&project_skills, "plan", "Project planning guidance");
        write_legacy_command(&legacy_commands, "handoff", "Legacy handoff guidance");

        assert_eq!(
            resolve_skill_path(&workspace, "$plan").expect("project skill should resolve"),
            project_skills.join("plan").join("SKILL.md")
        );
        assert_eq!(
            resolve_skill_path(&workspace, "/handoff").expect("legacy command should resolve"),
            legacy_commands.join("handoff.md")
        );
    }

    #[test]
    fn renders_skills_reports_as_json() {
        let workspace = temp_dir("skills-json-workspace");
        let project_skills = workspace.join(".codex").join("skills");
        let project_commands = workspace.join(".claude").join("commands");
        let user_home = temp_dir("skills-json-home");
        let user_skills = user_home.join(".codex").join("skills");

        write_skill(&project_skills, "plan", "Project planning guidance");
        write_legacy_command(&project_commands, "deploy", "Legacy deployment guidance");
        write_skill(&user_skills, "plan", "User planning guidance");
        write_skill(&user_skills, "help", "Help guidance");

        let roots = vec![
            SkillRoot {
                source: DefinitionSource::ProjectCodex,
                path: project_skills,
                origin: SkillOrigin::SkillsDir,
            },
            SkillRoot {
                source: DefinitionSource::ProjectClaude,
                path: project_commands,
                origin: SkillOrigin::LegacyCommandsDir,
            },
            SkillRoot {
                source: DefinitionSource::UserCodex,
                path: user_skills,
                origin: SkillOrigin::SkillsDir,
            },
        ];
        let report = super::render_skills_report_json(
            &load_skills_from_roots(&roots).expect("skills should load"),
        );
        assert_eq!(report["kind"], "skills");
        assert_eq!(report["action"], "list");
        assert_eq!(report["summary"]["active"], 3);
        assert_eq!(report["summary"]["shadowed"], 1);
        assert_eq!(report["skills"][0]["name"], "plan");
        assert_eq!(report["skills"][0]["source"]["id"], "project_claw");
        assert_eq!(report["skills"][1]["name"], "deploy");
        assert_eq!(report["skills"][1]["origin"]["id"], "legacy_commands_dir");
        assert_eq!(report["skills"][3]["shadowed_by"]["id"], "project_claw");

        let help = handle_skills_slash_command_json(Some("help"), &workspace).expect("skills help");
        assert_eq!(help["kind"], "skills");
        assert_eq!(help["action"], "help");
        assert_eq!(help["usage"]["aliases"][0], "/skill");
        assert_eq!(
            help["usage"]["direct_cli"],
            "claw skills [list|install <path>|help|<skill> [args]]"
        );

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(user_home);
    }

    #[test]
    fn agents_and_skills_usage_support_help_and_unexpected_args() {
        let cwd = temp_dir("slash-usage");

        let agents_help =
            super::handle_agents_slash_command(Some("help"), &cwd).expect("agents help");
        assert!(agents_help.contains("Usage            /agents [list|help]"));
        assert!(agents_help.contains("Direct CLI       claw agents"));
        assert!(agents_help
            .contains("Sources          .claw/agents, ~/.claw/agents, $CLAW_CONFIG_HOME/agents"));

        let agents_unexpected =
            super::handle_agents_slash_command(Some("show planner"), &cwd).expect("agents usage");
        assert!(agents_unexpected.contains("Unexpected       show planner"));

        let skills_help =
            super::handle_skills_slash_command(Some("--help"), &cwd).expect("skills help");
        assert!(skills_help
            .contains("Usage            /skills [list|install <path>|help|<skill> [args]]"));
        assert!(skills_help.contains("Alias            /skill"));
        assert!(skills_help.contains("Invoke           /skills help overview -> $help overview"));
        assert!(skills_help.contains("Install root     $CLAW_CONFIG_HOME/skills or ~/.claw/skills"));
        assert!(skills_help.contains(".omc/skills"));
        assert!(skills_help.contains(".agents/skills"));
        assert!(skills_help.contains("~/.claude/skills/omc-learned"));
        assert!(skills_help.contains("legacy /commands"));

        let skills_unexpected =
            super::handle_skills_slash_command(Some("show help"), &cwd).expect("skills usage");
        assert!(skills_unexpected.contains("Unexpected       show"));

        let skills_install_help = super::handle_skills_slash_command(Some("install --help"), &cwd)
            .expect("nested skills help");
        assert!(skills_install_help
            .contains("Usage            /skills [list|install <path>|help|<skill> [args]]"));
        assert!(skills_install_help.contains("Alias            /skill"));
        assert!(skills_install_help.contains("Unexpected       install"));

        let skills_unknown_help =
            super::handle_skills_slash_command(Some("show --help"), &cwd).expect("skills help");
        assert!(skills_unknown_help
            .contains("Usage            /skills [list|install <path>|help|<skill> [args]]"));
        assert!(skills_unknown_help.contains("Unexpected       show"));

        let skills_help_json =
            super::handle_skills_slash_command_json(Some("help"), &cwd).expect("skills help json");
        let sources = skills_help_json["usage"]["sources"]
            .as_array()
            .expect("skills help sources");
        assert_eq!(skills_help_json["usage"]["aliases"][0], "/skill");
        assert!(sources.iter().any(|value| value == ".omc/skills"));
        assert!(sources.iter().any(|value| value == ".agents/skills"));
        assert!(sources.iter().any(|value| value == "~/.omc/skills"));
        assert!(sources
            .iter()
            .any(|value| value == "~/.claude/skills/omc-learned"));

        let _ = fs::remove_dir_all(cwd);
    }

    #[test]
    fn discovers_omc_skills_from_project_and_user_compatibility_roots() {
        let _guard = env_guard();
        let workspace = temp_dir("skills-omc-workspace");
        let user_home = temp_dir("skills-omc-home");
        let claude_config_dir = temp_dir("skills-omc-claude-config");
        let project_omc_skills = workspace.join(".omc").join("skills");
        let project_agents_skills = workspace.join(".agents").join("skills");
        let user_omc_skills = user_home.join(".omc").join("skills");
        let claude_config_skills = claude_config_dir.join("skills");
        let claude_config_commands = claude_config_dir.join("commands");
        let learned_skills = claude_config_dir.join("skills").join("omc-learned");
        let original_home = std::env::var_os("HOME");
        let original_claude_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR");

        write_skill(&project_omc_skills, "hud", "OMC HUD guidance");
        write_skill(
            &project_agents_skills,
            "trace",
            "Compatibility skill guidance",
        );
        write_skill(&user_omc_skills, "cancel", "OMC cancel guidance");
        write_skill(
            &claude_config_skills,
            "statusline",
            "Claude config skill guidance",
        );
        write_legacy_command(
            &claude_config_commands,
            "doctor-check",
            "Claude config command guidance",
        );
        write_skill(&learned_skills, "learned", "Learned skill guidance");
        std::env::set_var("HOME", &user_home);
        std::env::set_var("CLAUDE_CONFIG_DIR", &claude_config_dir);

        let report = super::handle_skills_slash_command(None, &workspace).expect("skills list");
        assert!(report.contains("available skills"));
        assert!(report.contains("hud · OMC HUD guidance"));
        assert!(report.contains("trace · Compatibility skill guidance"));
        assert!(report.contains("cancel · OMC cancel guidance"));
        assert!(report.contains("statusline · Claude config skill guidance"));
        assert!(report.contains("doctor-check · Claude config command guidance · legacy /commands"));
        assert!(report.contains("learned · Learned skill guidance"));

        let help =
            super::handle_skills_slash_command_json(Some("help"), &workspace).expect("skills help");
        let sources = help["usage"]["sources"]
            .as_array()
            .expect("skills help sources");
        assert_eq!(help["usage"]["aliases"][0], "/skill");
        assert!(sources.iter().any(|value| value == ".omc/skills"));
        assert!(sources.iter().any(|value| value == ".agents/skills"));
        assert!(sources.iter().any(|value| value == "~/.omc/skills"));
        assert!(sources
            .iter()
            .any(|value| value == "~/.claude/skills/omc-learned"));

        restore_env_var("HOME", original_home);
        restore_env_var("CLAUDE_CONFIG_DIR", original_claude_config_dir);
        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(user_home);
        let _ = fs::remove_dir_all(claude_config_dir);
    }

    #[test]
    fn mcp_usage_supports_help_and_unexpected_args() {
        let cwd = temp_dir("mcp-usage");

        let help = super::handle_mcp_slash_command(Some("help"), &cwd).expect("mcp help");
        assert!(help.contains("Usage            /mcp [list|show <server>|help]"));
        assert!(help.contains("Direct CLI       claw mcp [list|show <server>|help]"));

        let unexpected =
            super::handle_mcp_slash_command(Some("show alpha beta"), &cwd).expect("mcp usage");
        assert!(unexpected.contains("Unexpected       show alpha beta"));

        let nested_help =
            super::handle_mcp_slash_command(Some("show --help"), &cwd).expect("mcp help");
        assert!(nested_help.contains("Usage            /mcp [list|show <server>|help]"));
        assert!(nested_help.contains("Unexpected       show"));

        let unknown_help =
            super::handle_mcp_slash_command(Some("inspect --help"), &cwd).expect("mcp usage");
        assert!(unknown_help.contains("Usage            /mcp [list|show <server>|help]"));
        assert!(unknown_help.contains("Unexpected       inspect"));

        let _ = fs::remove_dir_all(cwd);
    }

    #[test]
    fn renders_mcp_reports_from_loaded_config() {
        let workspace = temp_dir("mcp-config-workspace");
        let config_home = temp_dir("mcp-config-home");
        fs::create_dir_all(workspace.join(".claw")).expect("workspace config dir");
        fs::create_dir_all(&config_home).expect("config home");
        fs::write(
            workspace.join(".claw").join("settings.json"),
            r#"{
              "mcpServers": {
                "alpha": {
                  "command": "uvx",
                  "args": ["alpha-server"],
                  "env": {"ALPHA_TOKEN": "secret"},
                  "toolCallTimeoutMs": 1200
                },
                "remote": {
                  "type": "http",
                  "url": "https://remote.example/mcp",
                  "headers": {"Authorization": "Bearer secret"},
                  "headersHelper": "./bin/headers",
                  "oauth": {
                    "clientId": "remote-client",
                    "callbackPort": 7878
                  }
                }
              }
            }"#,
        )
        .expect("write settings");
        fs::write(
            workspace.join(".claw").join("settings.local.json"),
            r#"{
              "mcpServers": {
                "remote": {
                  "type": "ws",
                  "url": "wss://remote.example/mcp"
                }
              }
            }"#,
        )
        .expect("write local settings");

        let loader = ConfigLoader::new(&workspace, &config_home);
        let list = super::render_mcp_report_for(&loader, &workspace, None)
            .expect("mcp list report should render");
        assert!(list.contains("Configured servers 2"));
        assert!(list.contains("alpha"));
        assert!(list.contains("stdio"));
        assert!(list.contains("project"));
        assert!(list.contains("uvx alpha-server"));
        assert!(list.contains("remote"));
        assert!(list.contains("ws"));
        assert!(list.contains("local"));
        assert!(list.contains("wss://remote.example/mcp"));

        let show = super::render_mcp_report_for(&loader, &workspace, Some("show alpha"))
            .expect("mcp show report should render");
        assert!(show.contains("Name              alpha"));
        assert!(show.contains("Command           uvx"));
        assert!(show.contains("Args              alpha-server"));
        assert!(show.contains("Env keys          ALPHA_TOKEN"));
        assert!(show.contains("Tool timeout      1200 ms"));

        let remote = super::render_mcp_report_for(&loader, &workspace, Some("show remote"))
            .expect("mcp show remote report should render");
        assert!(remote.contains("Transport         ws"));
        assert!(remote.contains("URL               wss://remote.example/mcp"));

        let missing = super::render_mcp_report_for(&loader, &workspace, Some("show missing"))
            .expect("missing report should render");
        assert!(missing.contains("server `missing` is not configured"));

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(config_home);
    }

    #[test]
    fn renders_mcp_reports_as_json() {
        let workspace = temp_dir("mcp-json-workspace");
        let config_home = temp_dir("mcp-json-home");
        fs::create_dir_all(workspace.join(".claw")).expect("workspace config dir");
        fs::create_dir_all(&config_home).expect("config home");
        fs::write(
            workspace.join(".claw").join("settings.json"),
            r#"{
              "mcpServers": {
                "alpha": {
                  "command": "uvx",
                  "args": ["alpha-server"],
                  "env": {"ALPHA_TOKEN": "secret"},
                  "toolCallTimeoutMs": 1200
                },
                "remote": {
                  "type": "http",
                  "url": "https://remote.example/mcp",
                  "headers": {"Authorization": "Bearer secret"},
                  "headersHelper": "./bin/headers",
                  "oauth": {
                    "clientId": "remote-client",
                    "callbackPort": 7878
                  }
                }
              }
            }"#,
        )
        .expect("write settings");
        fs::write(
            workspace.join(".claw").join("settings.local.json"),
            r#"{
              "mcpServers": {
                "remote": {
                  "type": "ws",
                  "url": "wss://remote.example/mcp"
                }
              }
            }"#,
        )
        .expect("write local settings");

        let loader = ConfigLoader::new(&workspace, &config_home);
        let list =
            render_mcp_report_json_for(&loader, &workspace, None).expect("mcp list json render");
        assert_eq!(list["kind"], "mcp");
        assert_eq!(list["action"], "list");
        assert_eq!(list["configured_servers"], 2);
        assert_eq!(list["servers"][0]["name"], "alpha");
        assert_eq!(list["servers"][0]["transport"]["id"], "stdio");
        assert_eq!(list["servers"][0]["details"]["command"], "uvx");
        assert_eq!(list["servers"][1]["name"], "remote");
        assert_eq!(list["servers"][1]["scope"]["id"], "local");
        assert_eq!(list["servers"][1]["transport"]["id"], "ws");
        assert_eq!(
            list["servers"][1]["details"]["url"],
            "wss://remote.example/mcp"
        );

        let show = render_mcp_report_json_for(&loader, &workspace, Some("show alpha"))
            .expect("mcp show json render");
        assert_eq!(show["action"], "show");
        assert_eq!(show["found"], true);
        assert_eq!(show["server"]["name"], "alpha");
        assert_eq!(show["server"]["details"]["env_keys"][0], "ALPHA_TOKEN");
        assert_eq!(show["server"]["details"]["tool_call_timeout_ms"], 1200);

        let missing = render_mcp_report_json_for(&loader, &workspace, Some("show missing"))
            .expect("mcp missing json render");
        assert_eq!(missing["found"], false);
        assert_eq!(missing["server_name"], "missing");

        let help =
            render_mcp_report_json_for(&loader, &workspace, Some("help")).expect("mcp help json");
        assert_eq!(help["action"], "help");
        assert_eq!(help["usage"]["sources"][0], ".claw/settings.json");

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(config_home);
    }

    #[test]
    fn mcp_degrades_gracefully_on_malformed_mcp_config_144() {
        // #144: mirror of #143's partial-success contract for `claw mcp`.
        // Previously `mcp` hard-failed on any config parse error, hiding
        // well-formed servers and forcing claws to fall back to `doctor`.
        // Now `mcp` emits a degraded envelope instead: exit 0, status:
        // "degraded", config_load_error populated, servers[] empty.
        let _guard = env_guard();
        let workspace = temp_dir("mcp-degrades-144");
        let config_home = temp_dir("mcp-degrades-144-cfg");
        fs::create_dir_all(workspace.join(".claw")).expect("create workspace .claw dir");
        fs::create_dir_all(&config_home).expect("create config home");
        // One valid server + one malformed entry missing `command`.
        fs::write(
            workspace.join(".claw.json"),
            r#"{
  "mcpServers": {
    "everything": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-everything"]},
    "missing-command": {"args": ["arg-only-no-command"]}
  }
}
"#,
        )
        .expect("write malformed .claw.json");

        let loader = ConfigLoader::new(&workspace, &config_home);
        // list action: must return Ok (not Err) with degraded envelope.
        let list = render_mcp_report_json_for(&loader, &workspace, None)
            .expect("mcp list should not hard-fail on config parse errors (#144)");
        assert_eq!(list["kind"], "mcp");
        assert_eq!(list["action"], "list");
        assert_eq!(
            list["status"].as_str(),
            Some("degraded"),
            "top-level status should be 'degraded': {list}"
        );
        let err = list["config_load_error"]
            .as_str()
            .expect("config_load_error must be a string on degraded runs");
        assert!(
            err.contains("mcpServers.missing-command"),
            "config_load_error should name the malformed field path: {err}"
        );
        assert_eq!(list["configured_servers"], 0);
        assert!(list["servers"].as_array().unwrap().is_empty());

        // show action: should also degrade (not hard-fail).
        let show = render_mcp_report_json_for(&loader, &workspace, Some("show everything"))
            .expect("mcp show should not hard-fail on config parse errors (#144)");
        assert_eq!(show["kind"], "mcp");
        assert_eq!(show["action"], "show");
        assert_eq!(
            show["status"].as_str(),
            Some("degraded"),
            "show action should also report status: 'degraded': {show}"
        );
        assert!(show["config_load_error"].is_string());

        // Clean path: status: "ok", config_load_error: null.
        let clean_ws = temp_dir("mcp-degrades-144-clean");
        fs::create_dir_all(&clean_ws).expect("clean ws");
        let clean_loader = ConfigLoader::new(&clean_ws, &config_home);
        let clean_list = render_mcp_report_json_for(&clean_loader, &clean_ws, None)
            .expect("clean mcp list should succeed");
        assert_eq!(
            clean_list["status"].as_str(),
            Some("ok"),
            "clean run should report status: 'ok'"
        );
        assert!(clean_list["config_load_error"].is_null());

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(clean_ws);
    }

    #[test]
    fn parses_quoted_skill_frontmatter_values() {
        let contents = "---\nname: \"hud\"\ndescription: 'Quoted description'\n---\n";
        let (name, description) = super::parse_skill_frontmatter(contents);
        assert_eq!(name.as_deref(), Some("hud"));
        assert_eq!(description.as_deref(), Some("Quoted description"));
    }

    #[test]
    fn installs_skill_into_user_registry_and_preserves_nested_files() {
        let workspace = temp_dir("skills-install-workspace");
        let source_root = workspace.join("source").join("help");
        let install_root = temp_dir("skills-install-root");
        write_skill(
            source_root.parent().expect("parent"),
            "help",
            "Helpful skill",
        );
        let script_dir = source_root.join("scripts");
        fs::create_dir_all(&script_dir).expect("script dir");
        fs::write(script_dir.join("run.sh"), "#!/bin/sh\necho help\n").expect("write script");

        let installed = super::install_skill_into(
            source_root.to_str().expect("utf8 skill path"),
            &workspace,
            &install_root,
        )
        .expect("skill should install");

        assert_eq!(installed.invocation_name, "help");
        assert_eq!(installed.display_name.as_deref(), Some("help"));
        assert!(installed.installed_path.ends_with(Path::new("help")));
        assert!(installed.installed_path.join("SKILL.md").is_file());
        assert!(installed
            .installed_path
            .join("scripts")
            .join("run.sh")
            .is_file());

        let report = super::render_skill_install_report(&installed);
        assert!(report.contains("Result           installed help"));
        assert!(report.contains("Invoke as        $help"));
        assert!(report.contains(&install_root.display().to_string()));

        let roots = vec![SkillRoot {
            source: DefinitionSource::UserCodexHome,
            path: install_root.clone(),
            origin: SkillOrigin::SkillsDir,
        }];
        let listed = render_skills_report(
            &load_skills_from_roots(&roots).expect("installed skills should load"),
        );
        assert!(listed.contains("User config roots:"));
        assert!(listed.contains("help · Helpful skill"));

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(install_root);
    }

    #[test]
    fn installs_plugin_from_path_and_lists_it() {
        let config_home = temp_dir("home");
        let source_root = temp_dir("source");
        write_external_plugin(&source_root, "demo", "1.0.0");

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        let install = handle_plugins_slash_command(
            Some("install"),
            Some(source_root.to_str().expect("utf8 path")),
            &mut manager,
        )
        .expect("install command should succeed");
        assert!(install.reload_runtime);
        assert!(install.message.contains("installed demo@external"));
        assert!(install.message.contains("Name             demo"));
        assert!(install.message.contains("Version          1.0.0"));
        assert!(install.message.contains("Status           enabled"));

        let list = handle_plugins_slash_command(Some("list"), None, &mut manager)
            .expect("list command should succeed");
        assert!(!list.reload_runtime);
        assert!(list.message.contains("demo"));
        assert!(list.message.contains("v1.0.0"));
        assert!(list.message.contains("enabled"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn enables_and_disables_plugin_by_name() {
        let config_home = temp_dir("toggle-home");
        let source_root = temp_dir("toggle-source");
        write_external_plugin(&source_root, "demo", "1.0.0");

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        handle_plugins_slash_command(
            Some("install"),
            Some(source_root.to_str().expect("utf8 path")),
            &mut manager,
        )
        .expect("install command should succeed");

        let disable = handle_plugins_slash_command(Some("disable"), Some("demo"), &mut manager)
            .expect("disable command should succeed");
        assert!(disable.reload_runtime);
        assert!(disable.message.contains("disabled demo@external"));
        assert!(disable.message.contains("Name             demo"));
        assert!(disable.message.contains("Status           disabled"));

        let list = handle_plugins_slash_command(Some("list"), None, &mut manager)
            .expect("list command should succeed");
        assert!(list.message.contains("demo"));
        assert!(list.message.contains("disabled"));

        let enable = handle_plugins_slash_command(Some("enable"), Some("demo"), &mut manager)
            .expect("enable command should succeed");
        assert!(enable.reload_runtime);
        assert!(enable.message.contains("enabled demo@external"));
        assert!(enable.message.contains("Name             demo"));
        assert!(enable.message.contains("Status           enabled"));

        let list = handle_plugins_slash_command(Some("list"), None, &mut manager)
            .expect("list command should succeed");
        assert!(list.message.contains("demo"));
        assert!(list.message.contains("enabled"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn lists_auto_installed_bundled_plugins_with_status() {
        let config_home = temp_dir("bundled-home");
        let bundled_root = temp_dir("bundled-root");
        let bundled_plugin = bundled_root.join("starter");
        write_bundled_plugin(&bundled_plugin, "starter", "0.1.0", false);

        let mut config = PluginManagerConfig::new(&config_home);
        config.bundled_root = Some(bundled_root.clone());
        let mut manager = PluginManager::new(config);

        let list = handle_plugins_slash_command(Some("list"), None, &mut manager)
            .expect("list command should succeed");
        assert!(!list.reload_runtime);
        assert!(list.message.contains("starter"));
        assert!(list.message.contains("v0.1.0"));
        assert!(list.message.contains("disabled"));

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(bundled_root);
    }
}
