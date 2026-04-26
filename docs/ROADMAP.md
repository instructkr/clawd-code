# Claw Code Roadmap

## Project Goal

Build the most agent-friendly autonomous coding harness: an SDK-first, security-first system where AI agents can plan, execute, test, review, and ship code — with humans stepping in only when they choose to.

## Design Principles

1. **Agent-first, human-aware.** The SDK, CLI, and event bus are optimized for programmatic consumers. Humans get a "rip cord" escape hatch to review, approve, and orchestrate — but agents drive by default.
2. **Security is not optional.** Permission modes, sandboxed execution, audit logging, and credential isolation are built into the core, not added as afterthoughts.
3. **Review-friendly by default.** Every agent action produces structured, reviewable output. Humans should never need to read raw logs to understand what happened.
4. **Easy integration.** Any agent framework, orchestrator, or CI system should be able to consume Claw Code with minimal glue code.
5. **One command to start.** A new user (human or agent) should be productive within 60 seconds of cloning the repo.

---

## Phase 1 — SDK Foundation (DONE)

Runtime model configuration, programmatic SDK crate, extensions, session trees, and inter-agent communication.

- [x] `models.json` — custom providers (Ollama, vLLM, OpenRouter, local servers)
- [x] SDK crate — `AgentSession`, `EventBus`, `SessionManager`, `ToolRegistry`
- [x] Extension trait — `register_tools()`, `on_turn_start()`, `on_turn_complete()`
- [x] Session tree — branching, forking, navigation with single-source-of-truth storage
- [x] Agent context — thread-safe KV store, `AgentTask`, `TaskRegistry`
- [x] 1,072 workspace tests passing

## Phase 2 — Agent Integration Surface

Make the SDK consumable by any agent framework with minimal effort.

### 2.1 RPC Mode

- [ ] `claw --mode rpc` — JSON-RPC over stdin/stdout
- [ ] Request/response protocol: `session.create`, `session.turn`, `session.tree.fork`, `session.list`, `session.destroy`
- [ ] Event streaming: `events.subscribe` → newline-delimited JSON stream
- [ ] Authentication: API key or token passed on init
- [ ] Graceful shutdown: `session.close` with state flush

### 2.2 SDK Hardening

- [ ] Pluggable `ApiClient` at `AgentSession` construction (replace `DummyApiClient`)
- [ ] `steer()` / `followUp()` for mid-turn message injection
- [ ] `setModel()` / `cycleModel()` for runtime model switching
- [ ] `compact()` for explicit context compaction
- [ ] `abort()` for mid-turn cancellation
- [ ] `dispose()` for clean session teardown
- [ ] Builder pattern: `AgentSessionBuilder::new().model("...").tools(registry).build()`

### 2.3 Tool Registration

- [x] `define_tool()` ergonomic builder with schema validation
- [x] Tool input/output schema enforcement (JSON Schema)
- [x] Custom tool handlers with `ToolHandler` trait and `FnToolHandler` closure wrapper
- [ ] Async tool execution support
- [ ] Custom tool factories per working directory

### 2.4 Framework Adapters

- [ ] LangChain adapter (Python) — thin wrapper over JSON-RPC (`claw --mode rpc`)
- [ ] AutoGen adapter (Python) — thin wrapper over JSON-RPC
- [ ] CrewAI adapter (Python) — thin wrapper over JSON-RPC
- [ ] Generic HTTP/WebSocket adapter for any framework
- [ ] Example: spawn 3 coordinated agents that code, test, and review

> **Note:** Framework adapters are Python libraries that consume the JSON-RPC
> server built in Phase 2.1. They are not Rust code — they wrap the `claw --mode rpc`
> protocol for each Python framework's tool/agent conventions. These belong in a
> separate repository or as standalone pip-installable packages.

### 2.5 Session Tree Persistence

- [ ] JSONL file format with typed entries (message, compaction, branch, custom)
- [ ] `buildSessionContext()` — walk tree to build provider context
- [ ] Branch labels and summaries
- [ ] Fork-to-new-file (create independent session from a tree node)
- [ ] Compaction entries in tree
- [ ] Model change / thinking level change entries

## Phase 3 — Human Experience

Humans need to review agent work efficiently. This phase focuses on making agent outputs digestible, actionable, and beautiful.

### 3.1 Review Workflow

- [ ] Structured diff view — generated patches with context, not raw file dumps
- [ ] Change summaries — one-paragraph human-readable summary per agent turn
- [ ] Risk classification — each change tagged as `low` / `medium` / `high` risk
- [ ] Approval/rejection flow — agent pauses for human sign-off at configurable gates
- [ ] Batch review — review multiple agent turns in one pass
- [ ] Review history — audit trail of all approvals and rejections

### 3.2 Notification & Delivery

- [ ] Email summaries — HTML-formatted change reports sent after each milestone
- [ ] Chat integration — Slack/Discord/webhook notifications with rich embeds
- [ ] Mobile push — lightweight notification when agent needs attention or completes a phase
- [ ] Configurable routing — different recipients for different risk levels
- [ ] Digest mode — daily/weekly summary of all agent activity

### 3.3 Demo Deployments

- [ ] Auto-provisioned preview environments per agent phase/gate
- [ ] Auto-expiring links — environments self-destruct after configurable TTL
- [ ] Phase-linked — each deployment tied to a specific milestone in the agent's plan
- [ ] Tailscale integration — one-command local deployment exposed remotely via tunnel
- [ ] Docker/Podman-based — containerized preview environments for isolation
- [ ] Status page — live dashboard showing active previews, their phase, and expiry
- [ ] Human verification — "does this look right?" flow linked to each deployment
- [ ] Rollback — revert any preview environment to a previous state

### 3.4 TUI / Interactive Review

- [ ] Terminal UI for real-time agent monitoring
- [ ] Split-pane: agent output + diff view
- [ ] Keyboard-driven approval/rejection (y/n/e for edit)
- [ ] Session tree navigator — visual branching history
- [ ] Tool execution viewer — see what tools ran, with what inputs, and what they produced

## Phase 4 — Agent Orchestration

Multi-agent coordination for complex workflows.

### 4.1 Agent Orchestrator

- [ ] Plan decomposition — break a high-level goal into agent-sized tasks
- [ ] Task assignment — route tasks to specialized agents (coder, tester, reviewer)
- [ ] Dependency graph — tasks wait for dependencies before starting
- [ ] Progress tracking — real-time status of all agents and tasks
- [ ] Failure handling — automatic retry, fallback, escalation policies
- [ ] Human escalation — pause and notify when agents get stuck

### 4.2 Inter-Agent Communication

- [ ] Shared context (AgentContext) — already implemented
- [ ] Message passing — typed channels between agents
- [ ] Shared file staging area — agents write to shared workspace
- [ ] Conflict detection — detect when two agents edit the same file
- [ ] Merge strategies — auto-merge, queue, or escalate conflicts

### 4.3 Policy Engine

- [ ] Execution policies — rules for when agents can auto-proceed vs need approval
- [ ] Branch policies — auto-create branch, auto-push, require review before merge
- [ ] Test policies — what test level is required before marking a task complete
- [ ] Deployment policies — when to create preview environments
- [ ] Notification policies — when to notify humans, at what urgency

## Phase 5 — Security & Operations

### 5.1 Auth & Credentials

- [ ] API key vault — encrypted storage for provider credentials
- [ ] Per-session credentials — isolate API keys between sessions
- [ ] OAuth flow — support OAuth-based provider auth
- [ ] Credential rotation — auto-rotate keys on schedule

### 5.2 Sandboxing

- [ ] Filesystem sandboxing — agents can only write to designated directories
- [ ] Network sandboxing — restrict outbound network access per tool
- [ ] Resource limits — CPU, memory, and time limits per agent
- [ ] Audit logging — every agent action logged with timestamp, agent ID, and outcome

### 5.3 Observability

- [ ] Structured logging (JSON) — every event machine-readable
- [ ] Metrics — turn count, token usage, tool invocation, error rate
- [ ] Tracing — distributed trace IDs across multi-agent workflows
- [ ] Dashboard — real-time view of all active agents, sessions, and tasks

## Phase 6 — Developer Experience

### 6.1 Onboarding

- [ ] `claw init` — one command to configure providers and start first session
- [ ] Interactive setup wizard — detect installed tools, suggest providers
- [ ] Example projects — 5-minute tutorials for SDK, CLI, and RPC usage
- [ ] Template library — starter templates for common agent patterns

### 6.2 Documentation

- [ ] API reference docs (generated from Rust doc comments)
- [ ] Integration guides per framework
- [ ] Architecture decision records (ADRs)
- [ ] Changelog and migration guides

### 6.3 Packaging

- [ ] `cargo install claw` — publish to crates.io
- [ ] Homebrew formula — `brew install claw`
- [ ] Docker image — `docker run claw-code`
- [ ] Pre-built binaries — GitHub Releases for macOS/Linux/Windows

---

## Priority Order

1. ~~**Phase 2.1** — RPC mode~~ (done)
2. ~~**Phase 2.2** — SDK hardening~~ (done)
3. ~~**Phase 2.3** — Tool registration~~ (done)
4. **Phase 2.5** — Session Tree Persistence
5. **Phase 3.1** — Review workflow (humans need to see what agents do)
6. **Phase 6.1** — Onboarding (first impression matters)
7. **Phase 4.1** — Agent orchestrator (multi-agent coordination)
8. **Phase 2.4** — Framework adapters (Python, separate repo)
9. **Phase 3.3** — Demo deployments (visual verification)
10. **Phase 5** — Security & operations
11. **Phase 3.2** — Notifications
12. **Phase 6.3** — Packaging
