# Pi-mono Parity Design & Implementation Plan

> **Status:** Phases 1–5 implemented. This document captures the design rationale, architectural comparison, and remaining gaps.
> **Date:** 2026-04-25
> **Reference:** [pi-mono/coding-agent](https://github.com/badlogic/pi-mono) — a TypeScript coding agent SDK by Mario Zechner

---

## 1. Motivation

Claw-code aims to serve as an **autonomous AI coding harness** — a system where machines (claws, orchestrators, CI pipelines) drive coding agents without human babysitting. The pi-mono project provides a mature reference implementation of several key primitives:

- Runtime model configuration (no recompile to add providers)
- Programmatic SDK for embedding agent capabilities
- Extension system for tools, commands, and lifecycle hooks
- Session trees with branching/forking
- Inter-agent communication
- Event bus for typed lifecycle events

This document compares pi-mono's approach with claw-code's, tracks what's been implemented, and identifies remaining gaps.

---

## 2. Architectural Comparison

| Dimension | Pi-mono (TypeScript) | Claw-code (Rust) |
|-----------|----------------------|-------------------|
| **Language** | TypeScript / Node.js | Rust |
| **Philosophy** | Minimal core, extensions for everything | Monolithic core + SDK extraction layer |
| **Extension model** | Rich TS API: event interception, UI, commands, tools | Rust trait-based, lifecycle hooks only |
| **Session tree** | Full JSONL tree with compaction, branches, labels, custom entries | In-memory BTreeMap tree with basic branching |
| **Event system** | Extension-oriented lifecycle events | Dual: basic SDK EventBus + rich runtime LaneEvents |
| **Multi-agent** | Delegated to extensions (philosophical choice) | Built-in AgentContext, AgentTask, TaskRegistry, Worker boot |
| **MCP** | Explicitly rejected | Full MCP lifecycle with hardened states |
| **Package manager** | npm/git/local with filtering, enable/disable, gallery | Plugin config with enable/disable, no remote install |
| **Model config** | Rich compat flags, shell-command keys, modelOverrides, hot reload | Basic provider/model with env-var keys, no compat, no hot reload |
| **Orchestration focus** | Human-facing TUI extension ecosystem | Autonomous multi-agent orchestration with lane events, policy engine, recovery |

**Key insight:** Pi-mono optimizes for **human extensibility** (rich extension API, TUI hooks, package gallery). Claw-code optimizes for **machine orchestration** (typed lane events, policy engine, recovery recipes, worktree isolation). The parity work focuses on the primitives both need, while preserving claw-code's automation-first philosophy.

---

## 3. Implementation Plan

### Phase 1: Runtime Model Configuration (`models.json`) — DONE

**Pi-mono reference:** `~/.pi/agent/models.json` with providers, models, compat flags, shell-command API key resolution, and hot reload.

**Claw-code implementation:** `rust/crates/api/src/providers/models_file.rs`

#### Schema

```json
{
  "providers": {
    "ollama": {
      "baseUrl": "http://localhost:11434/v1",
      "api": "openai-completions",
      "apiKey": "ollama",
      "headers": {},
      "models": [
        {
          "id": "llama3.1:8b",
          "name": "Llama 3.1 8B",
          "reasoning": false,
          "input": ["text"],
          "contextWindow": 128000,
          "maxTokens": 32768
        }
      ]
    }
  }
}
```

#### Design decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| File locations | `~/.claw/models.json` (user) + `.claw/models.json` (project) | Matches existing `.claw` config convention |
| Merge strategy | Project entries override same-key user entries; different keys coexist | User can have global Ollama + project-specific providers without conflict |
| API key resolution | All-uppercase string → env var; otherwise literal | Simple heuristic that covers `OLLAMA_API_KEY`, `MY_KEY` etc. |
| API protocol | `"openai-completions"` (default) or `"anthropic-messages"` | Routes to `OpenAiCompatClient` or `AnthropicClient` respectively |
| Provider lookup | Provider-prefixed (`ollama/llama3.1:8b`) takes priority over bare ID | Disambiguates when multiple providers have the same model |
| Registry | Global `OnceLock<RwLock<Option<ModelsFile>>>` | Lazy-loaded, thread-safe, can be cleared for testing |

#### Integration points

Custom models are hooked into the existing provider dispatch chain:

1. **`metadata_for_model()`** — Checks custom models first, falls back to built-in
2. **`detect_provider_kind()`** — Custom models resolve to the provider kind matching their `api` field
3. **`max_tokens_for_model()`** / **`model_token_limit()`** — Custom models return their configured limits
4. **`ProviderClient::from_model_with_anthropic_auth()`** — Routes to `AnthropicClient` or `OpenAiCompatClient` based on `api` field, with custom `base_url` and `api_key`

#### Remaining gaps vs pi-mono

- **No shell-command API key resolution** (`!command` syntax)
- **No `compat` object** (per-model API compatibility flags)
- **No `modelOverrides`** (patch built-in models without full redefinition)
- **No cost tracking** fields
- **No hot reload** (requires restart)
- **Only 2 API types** vs pi-mono's 4 (missing `google-generative-ai`, `openai-responses`)

---

### Phase 2: SDK Crate — DONE

**Pi-mono reference:** `@mariozechner/pi-coding-agent` npm package with `createAgentSession()`, `AgentSessionRuntime`, `ResourceLoader`, `defineTool()`.

**Claw-code implementation:** `rust/crates/sdk/` (workspace crate)

#### Module structure

```
sdk/
├── Cargo.toml
├── src/
│   ├── lib.rs              — Public facade with re-exports
│   ├── session.rs          — AgentSession wrapping ConversationRuntime
│   ├── event_bus.rs        — Multi-subscriber broadcast EventBus
│   ├── session_manager.rs  — Session CRUD (create, list, save, load, delete)
│   ├── session_tree.rs     — Branching session tree with fork/navigate
│   ├── tool_registry.rs    — ToolRegistry + SdkToolExecutor
│   ├── extension.rs        — Extension trait + ExtensionRegistry
│   ├── agent_context.rs    — Inter-agent KV store + AgentTask + TaskRegistry
│   └── resource_loader.rs  — ResourceLoader trait + DefaultResourceLoader
```

#### Core types

| Type | Purpose |
|------|---------|
| `AgentSession` | Wraps `ConversationRuntime` with event-driven lifecycle. Provides `run_turn()`, `subscribe()`, `emit_event()` |
| `EventBus` | `mpsc`-channel-based pub/sub. `subscribe()` returns `EventSubscription` with `try_recv()`/`recv()` |
| `SessionManager` | In-memory or persisted session management. `create_session()`, `list_sessions()`, `save_session()`, `load_session()`, `delete_session()` |
| `ToolRegistry` | Name + description registry. `register_builtin()`, `has_tool()`, `create_builtin_tools()` |
| `SdkToolExecutor` | Implements `ToolExecutor`; stub by default (errors on execute), real implementations should provide a custom executor |

#### Design decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Dummy API client | `AgentSession` uses `DummyApiClient` that errors on stream | SDK consumers provide their own `ApiClient` via the runtime |
| Tool executor | `SdkToolExecutor` is a stub | Real tool execution requires the full `tools` crate; SDK provides the interface |
| Event bus | `mpsc::channel` (standard library) | No external dependency; sufficient for pub/sub within a process |
| Re-exports | Key `runtime` types re-exported from SDK | Convenience: consumers don't need both `sdk` and `runtime` in Cargo.toml |

#### Remaining gaps vs pi-mono

- **No async factory pattern** for creating cwd-bound services per session
- **No steering/follow-up message queueing** (`steer()`, `followUp()`)
- **No model cycling** (`setModel()`, `cycleModel()`)
- **No `defineTool()` ergonomic builder** (typebox schemas, async execute)
- **No settings manager** integration
- **No pluggable API client** at construction time (uses DummyApiClient)
- **No RPC mode** (stdin/stdout JSON-RPC)

---

### Phase 3: Extension System — DONE

**Pi-mono reference:** TypeScript modules with `registerTool()`, `registerCommand()`, `registerShortcut()`, `on(event, handler)`, UI access (`ctx.ui.confirm()`, `ctx.ui.select()`), tool interception (`tool_call`, `tool_result`), hot reload.

**Claw-code implementation:** `rust/crates/sdk/extension.rs`

#### Design

```rust
pub trait Extension: Debug + Send {
    fn name(&self) -> &str;
    fn register_tools(&self, registry: &mut ToolRegistry);
    fn on_turn_start(&self) {}
    fn on_turn_complete(&self) {}
    fn on_error(&self, _message: &str) {}
}
```

`ExtensionRegistry` manages loaded extensions and provides:
- `register()` — Add an extension
- `collect_tools()` — Gather tools from all extensions
- `notify_turn_start()` / `notify_turn_complete()` / `notify_error()` — Lifecycle notifications

`SimpleExtension` provides a convenience builder for tool-only extensions.

#### Complementary systems in runtime

The SDK extension trait is lightweight by design. Claw-code already has richer extension/plugin mechanisms in the runtime crate:

- **`crates/runtime/plugin_lifecycle.rs`** — `PluginLifecycle` trait with `validate_config()`, `healthcheck()`, `discover()`, `shutdown()`. States: `Unconfigured → Validated → Starting → Healthy | Degraded | Failed → ShuttingDown → Stopped`
- **`crates/runtime/mcp_stdio.rs`** — Full MCP server lifecycle (spawn, handshake, tool discovery, invocation)
- **`crates/plugins/`** — Bundled plugins with `PluginManager` for install/enable/disable/uninstall
- **`crates/runtime/hooks.rs`** — `HookRunner` with `PreToolUse`, `PostToolUse`, `PostToolUseFailure` command hooks

#### Remaining gaps vs pi-mono

- **No event interception** (tool_call blocking, tool_result modification)
- **No UI API** (confirm, select, input, custom components)
- **No provider request/response hooks**
- **No context modification** (modifying messages before they reach the provider)
- **No message rendering** customization
- **No command/shortcut/flag registration** from extensions
- **No hot reload**

---

### Phase 4: Session Tree — DONE

**Pi-mono reference:** JSONL tree with `id`/`parentId` per entry, compaction summaries, branch labels, custom entries, model/thinking change entries, `/tree` navigation, `/fork` to new file, `buildSessionContext()`.

**Claw-code implementation:** `rust/crates/sdk/session_tree.rs`

#### Design

All nodes live in a single flat `BTreeMap<String, SessionTreeNode>`. Parent-child relationships are maintained through `parent_id` back-links and `children: Vec<String>` ID lists. No data duplication — there is only one copy of each node.

```rust
pub struct SessionTreeNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub role: String,
    pub label: Option<String>,
    pub children: Vec<String>,  // Child IDs (not cloned nodes)
}
```

Operations:
- `set_root()` — Create the root node
- `add_child()` — Add a child under a parent (validates parent exists, ID unique)
- `fork_at()` — Create a sibling at the same parent level
- `navigate_to()` — Move the active pointer to any node
- `active_path()` — Walk from root to active leaf via `parent_id` back-links

#### Remaining gaps vs pi-mono

- **No persistence** — In-memory only; no JSONL tree file format
- **No compaction entries** in tree
- **No branch summaries**
- **No labels** as first-class entries
- **No model_change / thinking_level_change entries**
- **No custom entries**
- **No fork-to-new-file** (forking stays in the same tree)
- **No `buildSessionContext()`** (walk tree to build provider context)
- **No tree UI** (`/tree` command)

---

### Phase 5: Inter-Agent Communication — DONE

**Pi-mono reference:** No built-in sub-agents. Pi delegates this to extensions. Cross-extension communication via `pi.events` event bus.

**Claw-code implementation:** `rust/crates/sdk/agent_context.rs`

This is an area where claw-code diverges from pi-mono's philosophy. Pi treats sub-agents as an extension concern. Claw-code bakes multi-agent coordination into the core.

#### Design

| Type | Purpose |
|------|---------|
| `AgentContext` | `Arc<RwLock<BTreeMap<String, String>>>` — thread-safe shared KV store. Clones share the same backing store. |
| `AgentTask` | Delegatable sub-task with `id`, `agent_type` (explore/plan/verify), `prompt`, `model`, `allowed_tools`, `context`, `output`, `error` |
| `TaskRegistry` | `BTreeMap<String, AgentTask>` with lifecycle: `register()` → `complete()` / `fail()` → `completed()` / `failed()` |

Complementary systems in runtime:
- **`crates/runtime/worker_boot.rs`** — `Worker` struct with lifecycle states (`Spawning → TrustRequired → ReadyForPrompt → Running → Finished/Failed`)
- **Built-in `Agent` tool** — Sub-agent spawning from the tool surface

#### Design decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Thread-safe context | `Arc<RwLock<BTreeMap>>` | Multiple agents on different threads can share state |
| String values | Values stored as `String` (JSON serialization is caller's responsibility) | Keeps the context store simple; callers decide serialization format |
| Silent RwLock errors | `if let Ok(...)` swallows poisoning | RwLock poisoning is unrecoverable in practice; matches Rust convention |

---

### Phase 6: Remaining Gaps (Not Yet Implemented)

These items were identified during the pi-mono research but not yet implemented:

#### 6.1. Richer Model Config

- Shell-command API key resolution (`"apiKey": "!vault read secret/key"`)
- `compat` object for per-model API compatibility flags
- `modelOverrides` to patch built-in models
- Cost tracking fields (`cost: { input: 0.01, output: 0.03 }` per 1M tokens)
- Hot reload (watch file, reload on change)
- Additional API types (`google-generative-ai`, `openai-responses`)

#### 6.2. SDK Completeness

- Pluggable `ApiClient` at `AgentSession` construction time
- `steer()` / `followUp()` for message queueing mid-turn
- `setModel()` / `cycleModel()` for runtime model switching
- `defineTool()` ergonomic builder with schema validation
- RPC mode (`--mode rpc` for stdin/stdout JSON-RPC)
- Settings manager integration
- Async factory pattern for cwd-bound service creation

#### 6.3. Extension Richness

- Event interception: `tool_call` blocking, `tool_result` modification
- Provider request/response hooks (`before_provider_request`, `after_provider_response`)
- Context modification (modify messages before provider)
- Message rendering customization
- Command/shortcut/flag registration from extensions
- UI API (`confirm()`, `select()`, `input()`)
- Hot reload (`/reload`)

#### 6.4. Session Tree Persistence

- JSONL tree file format with typed entries
- Compaction entries in tree
- Branch summaries for abandoned paths
- Labels and custom entries
- Model change / thinking level change entries
- `buildSessionContext()` for provider context from tree walk
- Fork-to-new-file (create independent session from a tree node)
- `/tree` navigation UI

#### 6.5. Package Manager

- Remote package install (npm, git URL)
- Package manifest format (`package.json` with `pi` key)
- Per-resource enable/disable and filtering
- Version pinning
- Package gallery / registry

---

## 4. Test Coverage

| Component | Unit tests | E2E tests | Integration tests |
|-----------|-----------|-----------|-------------------|
| models_file | 6 (parse, find, merge, override, missing, empty) | 7 (discover, env-var, prefix, fallback, merge) | — |
| SDK session | 2 (create, run_turn) | — | — |
| SDK event_bus | 3 (broadcast, dead removal, try_recv) | — | — |
| SDK session_manager | 5 (create, list, delete, load, error) | — | — |
| SDK session_tree | 8 (root, children, fork, navigate, error) | — | — |
| SDK extension | 2 (collect_tools, lifecycle) | — | — |
| SDK agent_context | 4 (operations, shared, registry, lifecycle) | — | — |
| SDK tool_registry | 2 (manage, builtins) | — | — |
| SDK resource_loader | 1 (create+load) | — | — |
| **Total SDK + models_file** | **33** | **7** | — |
| **Full workspace** | — | — | **1,072** |

---

## 5. Files Changed

| File | Status | Description |
|------|--------|-------------|
| `rust/Cargo.toml` | Modified | Added `sdk` workspace member |
| `rust/Cargo.lock` | Modified | Added `sdk` crate lock |
| `rust/crates/api/src/providers/models_file.rs` | New | Runtime models.json loader and registry |
| `rust/crates/api/src/providers/mod.rs` | Modified | Custom model hooks in dispatch chain |
| `rust/crates/api/src/client.rs` | Modified | Custom provider routing with api field support |
| `rust/crates/api/src/lib.rs` | Modified | Public re-exports for models_file |
| `rust/crates/api/tests/models_file_e2e.rs` | New | 7 end-to-end tests |
| `rust/crates/sdk/Cargo.toml` | New | Crate manifest |
| `rust/crates/sdk/src/lib.rs` | New | Public facade |
| `rust/crates/sdk/src/session.rs` | New | AgentSession |
| `rust/crates/sdk/src/event_bus.rs` | New | EventBus + typed events |
| `rust/crates/sdk/src/session_manager.rs` | New | SessionManager |
| `rust/crates/sdk/src/tool_registry.rs` | New | ToolRegistry + SdkToolExecutor |
| `rust/crates/sdk/src/extension.rs` | New | Extension trait + registry |
| `rust/crates/sdk/src/session_tree.rs` | New | SessionTree with branching |
| `rust/crates/sdk/src/agent_context.rs` | New | AgentContext + AgentTask + TaskRegistry |
| `rust/crates/sdk/src/resource_loader.rs` | New | ResourceLoader trait + default impl |
