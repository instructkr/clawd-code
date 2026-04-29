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
cargo run -p rusty-claude-cli -- --model claude-opus-4-6

# One-shot prompt
cargo run -p rusty-claude-cli -- prompt "explain this codebase"

# JSON output for automation
cargo run -p rusty-claude-cli -- --output-format json prompt "summarize src/main.rs"
```

### Minimal harness (`claw-analog`)

Smaller tool loop on the same `api` providers: `read_file`, `list_dir`, `glob_workspace`, `grep_workspace` / `grep_search`, optional `write_file`, workspace jail, optional `stream_message` (`--stream` / `--no-stream`), `.claw-analog.toml`, `--session` JSON resume, `--preset`, optional `~/.claw-analog/profile.toml`, `claw-analog doctor`, and `runtime::PermissionEnforcer` by default. See [`../how_to_run.md`](../how_to_run.md) for usage and design notes.

```bash
cd rust/
cargo run -p claw-analog -- --help
cargo run -p claw-analog -- -w . "What does rust/README.md say about parity?"
cargo run -p claw-analog -- --stream -w . "Short summary of crates/claw-analog"
# NDJSON lines for CI/agents (see how_to_run.md):
cargo run -p claw-analog -- --output-format json --stream -w . "Say hi once."
# allow writes under -w:
cargo run -p claw-analog -- --permission workspace-write -w . "Add a one-line comment to Cargo.toml if missing"
cargo test -p claw-analog
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
| CLAUDE.md / project memory | ✅ |
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
| Skills inventory / install surfaces | ✅ |
| Machine-readable JSON output across core CLI surfaces | ✅ |

## Model Aliases

Short names resolve to the latest model versions:

| Alias | Resolves To |
|-------|------------|
| `opus` | `claude-opus-4-6` |
| `sonnet` | `claude-sonnet-4-6` |
| `haiku` | `claude-haiku-4-5-20251213` |

## CLI Flags and Commands

Representative current surface:

```text
claw [OPTIONS] [COMMAND]

Flags:
  --model MODEL
  --output-format text|json
  --permission-mode MODE
  --dangerously-skip-permissions
  --allowedTools TOOLS
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

`claw acp` is a local discoverability surface for editor-first users: it reports the current ACP/Zed status without starting the runtime. As of April 16, 2026, claw-code does **not** ship an ACP/Zed daemon entrypoint yet, and `claw acp serve` is only a status alias until the real protocol surface lands.

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
- `/skills [list|install <path>|help]`
- `/agents [list|help]`
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
    ├── compat-harness/     # TS manifest extraction harness
    ├── mock-anthropic-service/ # Deterministic local Anthropic-compatible mock
    ├── claw-rag-service/   # Local RAG: ingest + SQLite + HTTP query (embeddings via OpenAI API or mock)
    ├── plugins/            # Plugin metadata, manager, install/enable/disable surfaces
    ├── runtime/            # Session, config, permissions, MCP, prompts, auth/runtime loop
    ├── rusty-claude-cli/   # Main CLI binary (`claw`)
    ├── telemetry/          # Session tracing and usage telemetry types
    └── tools/              # Built-in tools, skill resolution, tool search, agent runtime surfaces
```

### Crate Responsibilities

- **api** — provider clients, SSE streaming, request/response types, auth (`ANTHROPIC_API_KEY` + bearer-token support), request-size/context-window preflight
- **commands** — slash command definitions, parsing, help text generation, JSON/text command rendering
- **compat-harness** — extracts tool/prompt manifests from upstream TS source
- **mock-anthropic-service** — deterministic `/v1/messages` mock for CLI parity tests and local harness runs
- **claw-rag-service** — workspace ingest, SQLite chunk+embedding store, HTTP `serve` with `GET /` (minimal UI), `/v1/stats`, `/v1/query` (cosine search; optional mock embeddings for tests)
- **plugins** — plugin metadata, install/enable/disable/update flows, plugin tool definitions, hook integration surfaces
- **runtime** — `ConversationRuntime`, config loading, session persistence, permission policy, MCP client lifecycle, system prompt assembly, usage tracking
- **rusty-claude-cli** — REPL, one-shot prompt, direct CLI subcommands, streaming display, tool call rendering, CLI argument parsing
- **telemetry** — session trace events and supporting telemetry payloads
- **tools** — tool specs + execution: Bash, ReadFile, WriteFile, EditFile, GlobSearch, GrepSearch, WebSearch, WebFetch, Agent, TodoWrite, NotebookEdit, Skill, ToolSearch, and runtime-facing tool discovery

## Stats

- **~20K lines** of Rust
- **9 crates** in workspace
- **Binary name:** `claw`
- **Default model:** `claude-opus-4-6`
- **Default permissions:** `danger-full-access`

## License

See repository root.
