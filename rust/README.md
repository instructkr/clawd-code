# 🦞 Claw Code — Rust Implementation

A high-performance Rust rewrite of the Claw Code CLI agent harness. Built for speed, safety, and native tool execution.

For a task-oriented guide with copy/paste examples, see [`../USAGE.md`](../USAGE.md).

## Quick Start

```bash
# Inspect available commands
cd rust/
cargo run -p rusty-claude-cli -- --help

# Build the workspace
cargo build --workspace

# Run the interactive REPL
cargo run -p rusty-claude-cli -- --model claude-opus-4-7

# One-shot prompt
cargo run -p rusty-claude-cli -- prompt "explain this codebase"

# JSON output for automation
cargo run -p rusty-claude-cli -- --output-format json prompt "summarize src/main.rs"
```

## Configuration

Set your API credentials:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
# Or use a proxy
export ANTHROPIC_BASE_URL="https://your-proxy.com"
```

Or provide an OAuth bearer token directly:

```bash
export ANTHROPIC_AUTH_TOKEN="anthropic-oauth-or-proxy-bearer-token"
```

For local OpenAI-compatible servers such as Ollama, including Qwen reasoning
models, see [`../docs/local-openai-compatible-providers.md`](../docs/local-openai-compatible-providers.md).
Use the exact model tag exposed by the server, for example `qwen3:latest`, and
prefer `OLLAMA_HOST` for Ollama-specific local routing.

## Mock parity harness

The workspace now includes a deterministic Anthropic-compatible mock service and a clean-environment CLI harness for end-to-end parity checks.

```bash
cd rust/

# Run the scripted clean-environment harness
./scripts/run_mock_parity_harness.sh

# Or start the mock service manually for ad hoc CLI runs
cargo run -p mock-anthropic-service -- --bind 127.0.0.1:0
```

Harness coverage:

- `streaming_text`
- `read_file_roundtrip`
- `grep_chunk_assembly`
- `write_file_allowed`
- `write_file_denied`
- `multi_tool_turn_roundtrip`
- `bash_stdout_roundtrip`
- `bash_permission_prompt_approved`
- `bash_permission_prompt_denied`
- `plugin_tool_roundtrip`

Primary artifacts:

- `crates/mock-anthropic-service/` — reusable mock Anthropic-compatible service
- `crates/rusty-claude-cli/tests/mock_parity_harness.rs` — clean-env CLI harness
- `scripts/run_mock_parity_harness.sh` — reproducible wrapper
- `scripts/run_mock_parity_diff.py` — scenario checklist + PARITY mapping runner
- `mock_parity_scenarios.json` — scenario-to-PARITY manifest

## Features

| Feature | Status |
|---------|--------|
| Anthropic / OpenAI-compatible provider flows + streaming | ✅ |
| Direct bearer-token auth via `ANTHROPIC_AUTH_TOKEN` | ✅ |
| Interactive REPL (rustyline) | ✅ |
| Tool system (bash, read, write, edit, grep, glob) | ✅ |
| Web tools (search, fetch) | ✅ |
| Sub-agent / agent surfaces | ✅ |
| Todo tracking | ✅ |
| Notebook editing | ✅ |
| CLAUDE.md / CLAW.md / AGENTS.md project memory | ✅ |
| Config file hierarchy (`.claw.json` + merged config sections) | ✅ |
| Permission system | ✅ |
| MCP server lifecycle + inspection | ✅ |
| Session persistence + resume | ✅ |
| Cost / usage / stats surfaces | ✅ |
| Git integration | ✅ |
| Markdown terminal rendering (ANSI) | ✅ |
| Model aliases (opus/sonnet/haiku) | ✅ |
| Direct CLI subcommands (`status`, `sandbox`, `agents`, `mcp`, `skills`, `doctor`) | ✅ |
| Slash commands (including `/skills`, `/agents`, `/mcp`, `/doctor`, `/plugin`, `/subagent`) | ✅ |
| Hooks (`/hooks`, config-backed lifecycle hooks) | ✅ |
| Plugin management surfaces | ✅ |
| Skills inventory / install / uninstall surfaces | ✅ |
| Machine-readable JSON output across core CLI surfaces | ✅ |

## Model Aliases

Short names resolve to the latest model versions:

| Alias | Resolves To |
|-------|------------|
| `opus` | `claude-opus-4-7` |
| `sonnet` | `claude-sonnet-4-6` |
| `haiku` | `claude-haiku-4-5-20251213` |

## CLI Flags and Commands

Representative current surface:

```text
claw [OPTIONS] [COMMAND]

Flags:
  --model MODEL
  --output-format text|json  (case-insensitive; CLAW_OUTPUT_FORMAT supplies the default, flags override env)
  --permission-mode MODE
  --cwd PATH, -C PATH, --directory PATH
  --dangerously-skip-permissions, --skip-permissions
  --allowedTools TOOLS        canonical snake_case names or aliases; status JSON exposes allowed_tools.available/aliases
  --resume [SESSION.jsonl|session-id|latest]
  --version, -V

Top-level commands:
  prompt <text>
  help
  version
  status
  sandbox
  acp [serve]
  dump-manifests
  bootstrap-plan
  agents
  mcp
  skills
  system-prompt
  init
```

`claw acp` is a local discoverability surface for editor-first users: it reports the current ACP/Zed status without starting the runtime. As of April 16, 2026, claw-code does **not** ship an ACP/Zed daemon or JSON-RPC entrypoint yet, and `claw acp serve` is only a status alias until the real protocol surface lands. Status queries exit 0 and expose the same machine-readable contract via `--output-format json`; malformed ACP invocations exit 1 with `kind: unsupported_acp_invocation`.
`--output-format` accepts `text` or `json` in any casing. `CLAW_OUTPUT_FORMAT=json` selects JSON as the default for non-interactive commands, explicit flags override it, repeated flags warn on stderr, and status JSON exposes `format_source`, `format_raw`, and `format_overridden`. Help and doctor output also surface `CLAW_LOG` / `RUST_LOG` as the logging environment knobs.
`claw version --output-format json` is the provenance probe for automation: it reports full `git_sha`, derived `git_sha_short`, `is_dirty`, `branch`, `commit_date`, `commit_timestamp`, `rustc_version`, runtime `executable_path`, and `binary_provenance`; the text report is available as `human_readable` instead of a duplicate `message` field.
`status --output-format json` reports loaded project memory files under `workspace.memory_files[]` with each file's `path`, `source` (`claude_md`, `claw_md`, `agents_md`, or scoped/rule sources), `origin`, `scope_path`, `outside_project`, `chars`, and `contributes`; `claw doctor --output-format json` includes a dedicated `memory` check. Root instruction-file priority is `CLAUDE.md`, then `CLAW.md`, then `AGENTS.md`, discovery is bounded to the current git root when present (otherwise cwd only), and all non-duplicate loaded files contribute to the rendered system prompt.
`claw mcp --output-format json` reports partial MCP config success: valid servers remain in `servers[]` while malformed siblings appear in `invalid_servers[]`, with `total_configured`, `valid_count`, and `invalid_count` split out for automation. `status` mirrors this as `mcp_validation`, and doctor includes an `mcp validation` check.
`status --output-format json` also reports partial hook config success under `hook_validation`: valid hook entries are retained while malformed or unknown-event siblings appear in `invalid_hooks[]`, with `valid_count`, `invalid_count`, and typed `kind` fields (`invalid_hooks_config` or `unknown_hook_event`) for automation. `doctor --output-format json` includes a `hook validation` check, and `config --output-format json` includes `hook_validation` metadata with degraded status when invalid entries exist.
Shorthand prompt mode honors the POSIX `--` end-of-flags separator, so `claw -- "-prompt-with-dash"` and unknown dash-prefixed non-flag text stay on the prompt path instead of being treated as CLI options.
`claw dump-manifests` is self-contained: it emits the Rust resolver inventory for the selected workspace (commands, tools, agents, skills, and bootstrap phases) without requiring an upstream Claude Code TypeScript checkout. Use `--manifests-dir PATH` only to scope resolver discovery to another directory.

The command surface is moving quickly. For the canonical live help text, run:

```bash
cargo run -p rusty-claude-cli -- --help
```

## Slash Commands (REPL)

Tab completion expands slash commands, model aliases, permission modes, and recent session IDs.

The REPL now exposes a much broader surface than the original minimal shell:

- session / visibility: `/help`, `/status`, `/sandbox`, `/cost`, `/resume`, `/session`, `/version`, `/usage`, `/stats`
- workspace / git: `/compact`, `/clear`, `/config`, `/memory`, `/init`, `/diff`, `/commit`, `/pr`, `/issue`, `/export`, `/hooks`, `/files`, `/release-notes`
- discovery / debugging: `/mcp`, `/agents`, `/skills`, `/doctor`, `/tasks`, `/context`, `/desktop`
- automation / analysis: `/review`, `/advisor`, `/insights`, `/security-review`, `/subagent`, `/team`, `/telemetry`, `/providers`, `/cron`, and more
- plugin management: `/plugin` (with aliases `/plugins`, `/marketplace`)

Notable claw-first surfaces now available directly in slash form:
- `/skills [list|show <name>|install <path>|uninstall <name>|help]`
- `/agents [list|show <name>|create <name>|help]`
- `/mcp [list|show <server>|help]`
- `/doctor`
- `/plugin [list|install <path>|enable <name>|disable <name>|uninstall <id>|update <id>]`
- `/subagent [list|steer <target> <msg>|kill <id>]`

See [`../USAGE.md`](../USAGE.md) for usage examples and run `cargo run -p rusty-claude-cli -- --help` for the live canonical command list.

## Workspace Layout

```text
rust/
├── Cargo.toml              # Workspace root
├── Cargo.lock
└── crates/
    ├── api/                # Provider clients + streaming + request preflight
    ├── commands/           # Shared slash-command registry + help rendering
    ├── compat-harness/     # Compatibility/parity harness utilities
    ├── mock-anthropic-service/ # Deterministic local Anthropic-compatible mock
    ├── plugins/            # Plugin metadata, manager, install/enable/disable surfaces
    ├── runtime/            # Session, config, permissions, MCP, prompts, auth/runtime loop
    ├── rusty-claude-cli/   # Main CLI binary (`claw`)
    ├── telemetry/          # Session tracing and usage telemetry types
    └── tools/              # Built-in tools, skill resolution, tool search, agent runtime surfaces
```

### Crate Responsibilities

- **api** — provider clients, SSE streaming, request/response types, auth (`ANTHROPIC_API_KEY` + bearer-token support), request-size/context-window preflight
- **commands** — slash command definitions, parsing, help text generation, JSON/text command rendering
- **compat-harness** — compatibility and parity helpers for comparing behavior with upstream fixtures
- **mock-anthropic-service** — deterministic `/v1/messages` mock for CLI parity tests and local harness runs
- **plugins** — plugin metadata, install/enable/disable/update flows, plugin tool definitions, hook integration surfaces
- **runtime** — `ConversationRuntime`, config loading, session persistence, permission policy, MCP client lifecycle, system prompt assembly, usage tracking
- **rusty-claude-cli** — REPL, one-shot prompt, direct CLI subcommands, streaming display, tool call rendering, CLI argument parsing
- **telemetry** — session trace events and supporting telemetry payloads
- **tools** — tool specs + execution: Bash, ReadFile, WriteFile, EditFile, GlobSearch, GrepSearch, WebSearch, WebFetch, Agent, TodoWrite, NotebookEdit, Skill, ToolSearch, and runtime-facing tool discovery

## Stats

- **~20K lines** of Rust
- **9 crates** in workspace
- **Binary name:** `claw`
- **Default model:** `claude-opus-4-7`
- **Default permissions:** `workspace-write`

## License

See repository root.
