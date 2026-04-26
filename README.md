# Claw Code

**Agent-first autonomous coding harness.** A Rust SDK and CLI for building, orchestrating, and reviewing AI-driven coding workflows — designed primarily for machine consumers, with a human escape hatch.

<p align="center">
  <a href="./docs/ROADMAP.md">Roadmap</a>
  ·
  <a href="./docs/AGENT-INTEGRATION.md">Agent Integration</a>
  ·
  <a href="./docs/HUMAN-DX.md">Human Experience</a>
  ·
  <a href="./docs/PI-MONO-PARITY-DESIGN.md">Architecture</a>
</p>

---

## What is Claw Code?

Claw Code is an **autonomous coding harness** — a system where AI agents execute coding tasks, manage sessions, branch conversations, coordinate with each other, and surface results for human review. It is:

- **Agent-first:** The primary API consumer is an AI agent, not a human at a keyboard. The SDK, CLI, and event bus are designed for programmatic orchestration.
- **Human-aware:** Humans get a "rip cord" — the ability to step in, review outputs, approve/reject changes, and orchestrate plans through an agent orchestrator interface.
- **Security-first:** Permission modes, sandboxed execution, audit logging, and credential isolation are built in, not bolted on.
- **Review-friendly:** Outputs are structured for easy human consumption — summaries, diffs, deployment previews, and notification routing to email, chat, or mobile.

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Agent Orchestrator                     │
│         (plans, delegates, reviews, approves)            │
├──────────┬──────────┬──────────┬────────────────────────┤
│  Agent A │  Agent B │  Agent C │  ...                   │
│  (code)  │  (test)  │  (review)│                        │
├──────────┴──────────┴──────────┴────────────────────────┤
│                     Claw SDK (Rust)                       │
│  AgentSession · EventBus · SessionTree · AgentContext    │
│  ToolRegistry · Extension · TaskRegistry                 │
├─────────────────────────────────────────────────────────┤
│                     Claw CLI (`claw`)                     │
│  prompt · session · doctor · status · mcp · tools        │
├─────────────────────────────────────────────────────────┤
│              Provider Layer (models.json)                 │
│  Anthropic · OpenAI · xAI · DeepSeek · DashScope · custom │
│  Ollama · vLLM · Qwen (external) · models.json           │
└─────────────────────────────────────────────────────────┘
```

## Quick Start

### Build from source

```bash
git clone https://github.com/deep-thinking-llc/claw-code
cd claw-code/rust
cargo build --workspace
```

### Configure a provider

Create `~/.claw/models.json` for any OpenAI-compatible or Anthropic-compatible provider:

```json
{
  "providers": {
    "ollama": {
      "baseUrl": "http://localhost:11434/v1",
      "api": "openai-completions",
      "apiKey": "ollama",
      "models": [{ "id": "llama3.1:8b" }]
    }
  }
}
```

Or set an API key directly:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
# or
export OPENAI_API_KEY="sk-..."
```

### Run

```bash
# One-shot prompt
./target/debug/claw prompt "explain this codebase"

# Interactive REPL
./target/debug/claw

# Health check
./target/debug/claw doctor

# Structured JSON output (for agents)
./target/debug/claw --output-format json status
```

### Use the SDK from Rust

Add to your `Cargo.toml`:

```toml
[dependencies]
sdk = { path = "../claw-code/rust/crates/sdk" }
runtime = { path = "../claw-code/rust/crates/runtime" }
```

```rust
use sdk::{AgentSession, ToolRegistry, EventBus};
use runtime::PermissionMode;

let (mut session, event_bus) = AgentSession::new(
    "claude-sonnet-4-6",
    vec!["You are a helpful coding assistant.".into()],
    ToolRegistry::new(),
    PermissionMode::DangerFullAccess,
)?;

// Subscribe to events
let sub = event_bus.subscribe();

// Run a turn
let result = session.run_turn("Read the main.rs and summarize it");
```

## Key Concepts

### Sessions

Sessions persist conversation state across turns. They can be created, resumed, forked, and listed:

```bash
claw                        # start interactive session
claw prompt "do a thing"    # one-shot, auto-creates session
claw --resume latest        # resume last session
```

### Session Trees

Conversations can branch — like git for chat history. Fork at any point, navigate between branches, and explore alternative approaches without losing context.

### Agent Context

Multiple agents share a thread-safe key-value store (`AgentContext`) for coordination. Tasks are tracked through a `TaskRegistry` with completion/failure lifecycle.

### Event Bus

Subscribe to typed events — turn started/completed, tool execution, session lifecycle, errors — for real-time monitoring and orchestration.

### Extensions

Register custom tools and lifecycle hooks via the `Extension` trait. Extensions receive turn start/complete/error notifications and can add tools to the registry.

### Built-in Providers

Claw Code ships with native routing for these providers. Prefix your model name to select a provider, or let the credential sniffer auto-detect from your environment.

| Provider | Env var (API key) | Env var (base URL) | Model prefix / alias |
|----------|------------------|---------------------|---------------------|
| **Anthropic** | `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` | `ANTHROPIC_BASE_URL` | `claude-*`, aliases: `opus`, `sonnet`, `haiku` |
| **OpenAI** | `OPENAI_API_KEY` | `OPENAI_BASE_URL` | `openai/*`, `gpt-*` |
| **xAI (Grok)** | `XAI_API_KEY` | `XAI_BASE_URL` | `grok-*`, aliases: `grok`, `grok-mini`, `grok-2` |
| **DeepSeek** | `DEEPSEEK_API_KEY` | `DEEPSEEK_BASE_URL` | `deepseek-chat`, `deepseek-reasoner`, alias: `deepseek-r1` |
| **DashScope** (Alibaba) | `DASHSCOPE_API_KEY` | `DASHSCOPE_BASE_URL` | `qwen-*` (bare), `kimi-*`, `kimi` |
| **Ollama** (local) | none | `OLLAMA_BASE_URL` | `ollama/*` |
| **vLLM** (local) | none | `VLLM_BASE_URL` | `vllm/*` |
| **Qwen** (external) | `QWEN_API_KEY` | `QWEN_BASE_URL` | `qwen/*` |

**Provider auto-detection order:** when the model name doesn't match a built-in prefix, the system checks environment variables in this order: model prefix → custom models.json → Anthropic auth → OpenAI auth → xAI auth → DeepSeek auth → Qwen auth → `OLLAMA_BASE_URL` → `VLLM_BASE_URL` → `OPENAI_BASE_URL` → Anthropic fallback.

**Examples:**

```bash
# Anthropic
claw --model sonnet prompt "hello"

# DeepSeek
export DEEPSEEK_API_KEY="sk-..."
claw --model deepseek-chat prompt "hello"

# Ollama (local)
claw --model ollama/llama3.1:8b prompt "hello"

# vLLM (local)
export VLLM_BASE_URL="http://localhost:8000/v1"
claw --model vllm/meta-llama/Llama-3.1-8B prompt "hello"
```

### Custom Providers

Add any OpenAI-compatible or Anthropic-compatible provider via `models.json` — Ollama, vLLM, LM Studio, OpenRouter, local servers, anything. No recompile needed. The `api` field accepts: `openai-completions`, `anthropic-messages`, `deepseek`, `ollama`, `qwen`, `vllm`.

## Repository Layout

```
claw-code/
├── rust/                        # Rust workspace
│   ├── crates/
│   │   ├── sdk/                 # Agent SDK crate (public API)
│   │   ├── api/                 # Provider layer (Anthropic, OpenAI, custom)
│   │   ├── runtime/             # Session engine, permissions, plugins
│   │   ├── tools/               # Built-in tool implementations
│   │   ├── plugins/             # Plugin system
│   │   └── rusty-claude-cli/    # CLI binary (`claw`)
│   └── Cargo.toml               # Workspace root
├── docs/                        # Documentation
│   ├── ROADMAP.md               # Project roadmap
│   ├── AGENT-INTEGRATION.md     # Agent integration guide
│   ├── HUMAN-DX.md              # Human experience design
│   └── PI-MONO-PARITY-DESIGN.md # Architecture comparison
└── CLAUDE.md                    # AI coding assistant guidance
```

## Documentation

| Document | Purpose |
|----------|---------|
| [docs/ROADMAP.md](docs/ROADMAP.md) | Project direction and planned work |
| [docs/AGENT-INTEGRATION.md](docs/AGENT-INTEGRATION.md) | How to integrate agents via SDK, CLI, and RPC |
| [docs/HUMAN-DX.md](docs/HUMAN-DX.md) | Human review workflows, notifications, deployment previews |
| [docs/PI-MONO-PARITY-DESIGN.md](docs/PI-MONO-PARITY-DESIGN.md) | Architecture comparison with pi-mono reference |

## Development

```bash
cd rust

# Build
cargo build --workspace

# Test
cargo test --workspace

# Lint
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
```

## License

This project is a fork of [claw-code](https://github.com/ultraworkers/claw-code) by UltraWorkers. See the upstream repository for original authorship and license details.
