# claw-code Architecture

A high-level overview of how claw-code is structured. For implementation details, see source code in `rust/crates/`. For provider details, see [SUPPORTED_PROVIDERS.md](./SUPPORTED_PROVIDERS.md). For pinpoint navigation, see [ROADMAP.md](../ROADMAP.md#pinpoint-cluster-index).

## Overview

claw-code is a Rust-based CLI for interacting with LLM providers (Anthropic, OpenAI-compatible, xAI, DashScope, etc.). It provides:

- Streaming conversation with auto-compaction
- Tool execution (file read/write, bash, MCP)
- Multi-provider routing
- Session persistence
- Parallel agent execution

## Workspace Layout

The Rust workspace is organized in `rust/crates/`:

### Core crates

- **`rusty-claude-cli`** — CLI entry point. Parses args, routes commands, manages TUI/headless modes.
- **`runtime`** — Conversation engine. Manages session state, message history, auto-compaction, tool dispatch, hooks, MCP, and branch/lane events.
- **`api`** — Provider abstraction. Hosts `MODEL_REGISTRY` (provider/model routing), SSE streaming, request/response handling. Providers: `anthropic`, `openai_compat`.
- **`tools`** — Tool definitions. File I/O, bash execution, MCP integration, PDF extraction.

### Support crates

- **`commands`** — Parsed command dispatch layer between CLI and runtime.
- **`plugins`** — Plugin/hook lifecycle (`hooks.rs`).
- **`telemetry`** — Metrics and tracing instrumentation.
- **`compat-harness`** — Parity test harness for Rust-port validation.
- **`mock-anthropic-service`** — Local mock server for offline/test use.

## Request Flow

1. **CLI parse** (`rusty-claude-cli/src/main.rs`) — interprets args, env vars, settings.json
2. **Provider selection** (`api/src/providers/mod.rs`) — routes to provider via `MODEL_REGISTRY` based on model prefix
3. **Conversation execution** (`runtime/src/conversation.rs`) — sends to provider via SSE, receives streamed response
4. **Tool dispatch** (`tools/src/lib.rs`) — if response includes `tool_use`, execute and feed back `tool_result`
5. **Auto-compaction check** (`runtime/src/compact.rs`) — REACTIVE-AFTER-SUCCESS only (see #287 for preflight gap)
6. **Output** — JSON envelope (`--output-format json`) or text (default)

## Key Subsystems

### Auto-compaction

Triggered post-turn when `usage.input_tokens > threshold`. See:
- Threshold via env-only (#283)
- Reactive-not-preflight (#287, CRITICAL)
- Manual `/compact` skip-reasons (#289)
- Failure envelope coverage (#288)

### Provider routing

Hard-coded `MODEL_REGISTRY` + env-var-based auth + model-prefix heuristics. See:
- [SUPPORTED_PROVIDERS.md](./SUPPORTED_PROVIDERS.md) for current providers
- #285 for declarative providers/models/websearch source-of-truth
- #245, #246 for declarative config & backend swap
- #290, #291, #292 for transport resilience (stream-init, circuit-breaker, escalation)

### Parallel agents

Lane-based execution via `runtime/src/lane_events.rs`. Manifest-driven lifecycle. See:
- #286 for detached-thread + no-heartbeat issue (CRITICAL)

### Tool lifecycle / hooks

Tools defined in `tools/src/`. Hook events emitted via `runtime/src/hooks.rs` and `plugins/src/hooks.rs`. See:
- #254 (MCP refresh)
- #268 (tool-rendering parity)
- #274 (hook-execution-event envelope)
- #280 (hook event tap)

### Session persistence

Sessions managed in `runtime/src/session.rs`. See:
- #278 (version-comparison)
- #279 (unknown-field policy)

### CLI dispatch

CLI parsing in `rusty-claude-cli/src/main.rs`. Issues:
- #262 `--max-turns` spec
- #267 `--cwd` runtime fix
- #272 position-independent parsing
- #282 env-vs-config consolidation

## Build & Test

See [CONTRIBUTING.md](../CONTRIBUTING.md) for build commands. Quick reference:

```
cd rust && cargo build        # Build all crates
cd rust && cargo test         # Run all Rust tests
```

## Tracing & Debugging

- **Session state:** `runtime/src/session.rs` + `~/.claw/sessions/<id>/`
- **Provider responses:** Set `RUST_LOG=trace` for verbose SSE logs
- **Parity checks:** Use `compat-harness` crate for Rust-port validation

## Related Documents

- [ROADMAP.md](../ROADMAP.md) — Pinpoints by cluster
- [TROUBLESHOOTING.md](../TROUBLESHOOTING.md) — User-facing failure mitigation
- [SUPPORTED_PROVIDERS.md](./SUPPORTED_PROVIDERS.md) — Provider/model details
- [CONTRIBUTING.md](../CONTRIBUTING.md) — Pinpoint filing format
- [PINPOINT_FILING_GUIDE.md](./PINPOINT_FILING_GUIDE.md) — Filing workflow
- [CHANGELOG.md](../CHANGELOG.md) — Recent changes
