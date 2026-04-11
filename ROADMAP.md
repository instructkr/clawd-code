# Clawable Coding Harness Roadmap

## Goal

Turn claw-code into the most **clawable** coding harness:
- no human-first terminal assumptions
- no fragile prompt injection timing
- no opaque session state
- no hidden plugin or MCP failures
- no manual babysitting for routine recovery

This roadmap assumes the primary users are **claws wired through hooks, plugins, sessions, and channel events**.

## Definition of "clawable"

A clawable harness is:
- deterministic to start
- machine-readable in state and failure modes
- recoverable without a human watching the terminal
- branch/test/worktree aware
- plugin/MCP lifecycle aware
- event-first, not log-first
- capable of autonomous next-step execution

## Current Pain Points

### 1. Session boot is fragile
- trust prompts can block TUI startup
- prompts can land in the shell instead of the coding agent
- "session exists" does not mean "session is ready"

### 2. Truth is split across layers
- tmux state
- clawhip event stream
- git/worktree state
- test state
- gateway/plugin/MCP runtime state

### 3. Events are too log-shaped
- claws currently infer too much from noisy text
- important states are not normalized into machine-readable events

### 4. Recovery loops are too manual
- restart worker
- accept trust prompt
- re-inject prompt
- detect stale branch
- retry failed startup
- classify infra vs code failures manually

### 5. Branch freshness is not enforced enough
- side branches can miss already-landed main fixes
- broad test failures can be stale-branch noise instead of real regressions

### 6. Plugin/MCP failures are under-classified
- startup failures, handshake failures, config errors, partial startup, and degraded mode are not exposed cleanly enough

### 7. Human UX still leaks into claw workflows
- too much depends on terminal/TUI behavior instead of explicit agent state transitions and control APIs

## Product Principles

1. **State machine first** — every worker has explicit lifecycle states.
2. **Events over scraped prose** — channel output should be derived from typed events.
3. **Recovery before escalation** — known failure modes should auto-heal once before asking for help.
4. **Branch freshness before blame** — detect stale branches before treating red tests as new regressions.
5. **Partial success is first-class** — e.g. MCP startup can succeed for some servers and fail for others, with structured degraded-mode reporting.
6. **Terminal is transport, not truth** — tmux/TUI may remain implementation details, but orchestration state must live above them.
7. **Policy is executable** — merge, retry, rebase, stale cleanup, and escalation rules should be machine-enforced.

## Roadmap

## Phase 1 — Reliable Worker Boot

### 1. Ready-handshake lifecycle for coding workers
Add explicit states:
- `spawning`
- `trust_required`
- `ready_for_prompt`
- `prompt_accepted`
- `running`
- `blocked`
- `finished`
- `failed`

Acceptance:
- prompts are never sent before `ready_for_prompt`
- trust prompt state is detectable and emitted
- shell misdelivery becomes detectable as a first-class failure state

### 2. Trust prompt resolver
Add allowlisted auto-trust behavior for known repos/worktrees.

Acceptance:
- trusted repos auto-clear trust prompts
- events emitted for `trust_required` and `trust_resolved`
- non-allowlisted repos remain gated

### 3. Structured session control API
Provide machine control above tmux:
- create worker
- await ready
- send task
- fetch state
- fetch last error
- restart worker
- terminate worker

Acceptance:
- a claw can operate a coding worker without raw send-keys as the primary control plane

## Phase 2 — Event-Native Clawhip Integration

### 4. Canonical lane event schema
Define typed events such as:
- `lane.started`
- `lane.ready`
- `lane.prompt_misdelivery`
- `lane.blocked`
- `lane.red`
- `lane.green`
- `lane.commit.created`
- `lane.pr.opened`
- `lane.merge.ready`
- `lane.finished`
- `lane.failed`
- `branch.stale_against_main`

Acceptance:
- clawhip consumes typed lane events
- Discord summaries are rendered from structured events instead of pane scraping alone

### 5. Failure taxonomy
Normalize failure classes:
- `prompt_delivery`
- `trust_gate`
- `branch_divergence`
- `compile`
- `test`
- `plugin_startup`
- `mcp_startup`
- `mcp_handshake`
- `gateway_routing`
- `tool_runtime`
- `infra`

Acceptance:
- blockers are machine-classified
- dashboards and retry policies can branch on failure type

### 6. Actionable summary compression
Collapse noisy event streams into:
- current phase
- last successful checkpoint
- current blocker
- recommended next recovery action

Acceptance:
- channel status updates stay short and machine-grounded
- claws stop inferring state from raw build spam

## Phase 3 — Branch/Test Awareness and Auto-Recovery

### 7. Stale-branch detection before broad verification
Before broad test runs, compare current branch to `main` and detect if known fixes are missing.

Acceptance:
- emit `branch.stale_against_main`
- suggest or auto-run rebase/merge-forward according to policy
- avoid misclassifying stale-branch failures as new regressions

### 8. Recovery recipes for common failures
Encode known automatic recoveries for:
- trust prompt unresolved
- prompt delivered to shell
- stale branch
- compile red after cross-crate refactor
- MCP startup handshake failure
- partial plugin startup

Acceptance:
- one automatic recovery attempt occurs before escalation
- the attempted recovery is itself emitted as structured event data

### 9. Green-ness contract
Workers should distinguish:
- targeted tests green
- package green
- workspace green
- merge-ready green

Acceptance:
- no more ambiguous "tests passed" messaging
- merge policy can require the correct green level for the lane type
- a single hung test must not mask other failures: enforce per-test
  timeouts in CI (`cargo test --workspace`) so a 6-minute hang in one
  crate cannot prevent downstream crates from running their suites
- when a CI job fails because of a hang, the worker must report it as
  `test.hung` rather than a generic failure, so triage doesn't conflate
  it with a normal `assertion failed`
- recorded pinpoint (2026-04-08): `be561bf` swapped the local
  byte-estimate preflight for a `count_tokens` round-trip and silently
  returned `Ok(())` on any error, so `send_message_blocks_oversized_*`
  hung for ~6 minutes per attempt; the resulting workspace job crash
  hid 6 *separate* pre-existing CLI regressions (compact flag
  discarded, piped stdin vs permission prompter, legacy session layout,
  help/prompt assertions, mock harness count) that only became
  diagnosable after `8c6dfe5` + `5851f2d` restored the fast-fail path

## Phase 4 — Claws-First Task Execution

### 10. Typed task packet format
Define a structured task packet with fields like:
- objective
- scope
- repo/worktree
- branch policy
- acceptance tests
- commit policy
- reporting contract
- escalation policy

Acceptance:
- claws can dispatch work without relying on long natural-language prompt blobs alone
- task packets can be logged, retried, and transformed safely

### 11. Policy engine for autonomous coding
Encode automation rules such as:
- if green + scoped diff + review passed -> merge to dev
- if stale branch -> merge-forward before broad tests
- if startup blocked -> recover once, then escalate
- if lane completed -> emit closeout and cleanup session

Acceptance:
- doctrine moves from chat instructions into executable rules

### 12. Claw-native dashboards / lane board
Expose a machine-readable board of:
- repos
- active claws
- worktrees
- branch freshness
- red/green state
- current blocker
- merge readiness
- last meaningful event

Acceptance:
- claws can query status directly
- human-facing views become a rendering layer, not the source of truth

## Phase 5 — Plugin and MCP Lifecycle Maturity

### 13. First-class plugin/MCP lifecycle contract
Each plugin/MCP integration should expose:
- config validation contract
- startup healthcheck
- discovery result
- degraded-mode behavior
- shutdown/cleanup contract

Acceptance:
- partial-startup and per-server failures are reported structurally
- successful servers remain usable even when one server fails

### 14. MCP end-to-end lifecycle parity
Close gaps from:
- config load
- server registration
- spawn/connect
- initialize handshake
- tool/resource discovery
- invocation path
- error surfacing
- shutdown/cleanup

Acceptance:
- parity harness and runtime tests cover healthy and degraded startup cases
- broken servers are surfaced as structured failures, not opaque warnings

## Immediate Backlog (from current real pain)

Priority order: P0 = blocks CI/green state, P1 = blocks integration wiring, P2 = clawability hardening, P3 = swarm-efficiency improvements.

**P0 — Fix first (CI reliability)**
1. Isolate `render_diff_report` tests into tmpdir — **done**: `render_diff_report_for()` tests run in temp git repos instead of the live working tree, and targeted `cargo test -p rusty-claude-cli render_diff_report -- --nocapture` now stays green during branch/worktree activity
2. Expand GitHub CI from single-crate coverage to workspace-grade verification — **done**: `.github/workflows/rust-ci.yml` now runs `cargo test --workspace` plus fmt/clippy at the workspace level
3. Add release-grade binary workflow — **done**: `.github/workflows/release.yml` now builds tagged Rust release artifacts for the CLI
4. Add container-first test/run docs — **done**: `Containerfile` + `docs/container.md` document the canonical Docker/Podman workflow for build, bind-mount, and `cargo test --workspace` usage
5. Surface `doctor` / preflight diagnostics in onboarding docs and help — **done**: README + USAGE now put `claw doctor` / `/doctor` in the first-run path and point at the built-in preflight report
6. Automate branding/source-of-truth residue checks in CI — **done**: `.github/scripts/check_doc_source_of_truth.py` and the `doc-source-of-truth` CI job now block stale repo/org/invite residue in tracked docs and metadata
7. Eliminate warning spam from first-run help/build path — **done**: current `cargo run -q -p rusty-claude-cli -- --help` renders clean help output without a warning wall before the product surface
8. Promote `doctor` from slash-only to top-level CLI entrypoint — **done**: `claw doctor` is now a local shell entrypoint with regression coverage for direct help and health-report output
9. Make machine-readable status commands actually machine-readable — **done**: `claw --output-format json status` and `claw --output-format json sandbox` now emit structured JSON snapshots instead of prose tables
10. Unify legacy config/skill namespaces in user-facing output — **done**: skills/help JSON/text output now present `.claw` as the canonical namespace and collapse legacy roots behind `.claw`-shaped source ids/labels
11. Honor JSON output on inventory commands like `skills` and `mcp` — **done**: direct CLI inventory commands now honor `--output-format json` with structured payloads for both skills and MCP inventory
12. Audit `--output-format` contract across the whole CLI surface — **done**: direct CLI commands now honor deterministic JSON/text handling across help/version/status/sandbox/agents/mcp/skills/bootstrap-plan/system-prompt/init/doctor, with regression coverage in `output_format_contract.rs` and resumed `/status` JSON coverage

**P1 — Next (integration wiring, unblocks verification)**
1. Worker readiness handshake + trust resolution — **done**: `WorkerStatus` state machine with `Spawning` → `TrustRequired` → `ReadyForPrompt` → `PromptAccepted` → `Running` lifecycle, `trust_auto_resolve` + `trust_gate_cleared` gating
2. Add cross-module integration tests — **done**: 12 integration tests covering worker→recovery→policy, stale_branch→policy, green_contract→policy, reconciliation flows
3. Wire lane-completion emitter — **done**: `lane_completion` module with `detect_lane_completion()` auto-sets `LaneContext::completed` from session-finished + tests-green + push-complete → policy closeout
4. Wire `SummaryCompressor` into the lane event pipeline — **done**: `compress_summary_text()` feeds into `LaneEvent::Finished` detail field in `tools/src/lib.rs`

**P2 — Clawability hardening (original backlog)**
5. Worker readiness handshake + trust resolution — **done**: `WorkerStatus` state machine with `Spawning` → `TrustRequired` → `ReadyForPrompt` → `PromptAccepted` → `Running` lifecycle, `trust_auto_resolve` + `trust_gate_cleared` gating
6. Prompt misdelivery detection and recovery — **done**: `prompt_delivery_attempts` counter, `PromptMisdelivery` event detection, `auto_recover_prompt_misdelivery` + `replay_prompt` recovery arm
7. Canonical lane event schema in clawhip — **done**: `LaneEvent` enum with `Started/Blocked/Failed/Finished` variants, `LaneEvent::new()` typed constructor, `tools/src/lib.rs` integration
8. Failure taxonomy + blocker normalization — **done**: `WorkerFailureKind` enum (`TrustGate/PromptDelivery/Protocol/Provider`), `FailureScenario::from_worker_failure_kind()` bridge to recovery recipes
9. Stale-branch detection before workspace tests — **done**: `stale_branch.rs` module with freshness detection, behind/ahead metrics, policy integration
10. MCP structured degraded-startup reporting — **done**: `McpManager` degraded-startup reporting (+183 lines in `mcp_stdio.rs`), failed server classification (startup/handshake/config/partial), structured `failed_servers` + `recovery_recommendations` in tool output
11. Structured task packet format — **done**: `task_packet.rs` module with `TaskPacket` struct, validation, serialization, `TaskScope` resolution (workspace/module/single-file/custom), integrated into `tools/src/lib.rs`
12. Lane board / machine-readable status API — **done**: Lane completion hardening + `LaneContext::completed` auto-detection + MCP degraded reporting surface machine-readable state
13. **Session completion failure classification** — **done**: `WorkerFailureKind::Provider` + `observe_completion()` + recovery recipe bridge landed
14. **Config merge validation gap** — **done**: `config.rs` hook validation before deep-merge (+56 lines), malformed entries fail with source-path context instead of merged parse errors
15. **MCP manager discovery flaky test** — **done**: `manager_discovery_report_keeps_healthy_servers_when_one_server_fails` now runs as a normal workspace test again after repeated stable passes, so degraded-startup coverage is no longer hidden behind `#[ignore]`

16. **Commit provenance / worktree-aware push events** — **done**: `LaneCommitProvenance` now carries branch/worktree/canonical-commit/supersession metadata in lane events, and `dedupe_superseded_commit_events()` is applied before agent manifests are written so superseded commit events collapse to the latest canonical lineage
17. **Orphaned module integration audit** — **done**: `runtime` now keeps `session_control` and `trust_resolver` behind `#[cfg(test)]` until they are wired into a real non-test execution path, so normal builds no longer advertise dead clawability surface area.
18. **Context-window preflight gap** — **done**: provider request sizing now emits `context_window_blocked` before oversized requests leave the process, using a model-context registry instead of the old naive max-token heuristic.
19. **Subcommand help falls through into runtime/API path** — **done**: `claw doctor --help`, `claw status --help`, `claw sandbox --help`, and nested `mcp`/`skills` help are now intercepted locally without runtime/provider startup, with regression tests covering the direct CLI paths.
20. **Session state classification gap (working vs blocked vs finished vs truly stale)** — **done**: agent manifests now derive machine states such as `working`, `blocked_background_job`, `blocked_merge_conflict`, `degraded_mcp`, `interrupted_transport`, `finished_pending_report`, and `finished_cleanable`, and terminal-state persistence records commit provenance plus derived state so downstream monitoring can distinguish quiet progress from truly idle sessions.
21. **Resumed `/status` JSON parity gap** — **done**: resolved by the broader "Resumed local-command JSON parity gap" work tracked as #26 below. Re-verified on `main` HEAD `8dc6580` — `cargo test --release -p rusty-claude-cli resumed_status_command_emits_structured_json_when_requested` passes cleanly (1 passed, 0 failed), so resumed `/status --output-format json` now goes through the same structured renderer as the fresh CLI path. The original failure (`expected value at line 1 column 1` because resumed dispatch fell back to prose) no longer reproduces.
22. **Opaque failure surface for session/runtime crashes** — **done**: `safe_failure_class()` in `error.rs` classifies all API errors into 8 user-safe classes (`provider_auth`, `provider_internal`, `provider_retry_exhausted`, `provider_rate_limit`, `provider_transport`, `provider_error`, `context_window`, `runtime_io`). `format_user_visible_api_error` in `main.rs` attaches session ID + request trace ID to every user-visible error. Coverage in `opaque_provider_wrapper_surfaces_failure_class_session_and_trace` and 3 related tests.
23. **`doctor --output-format json` check-level structure gap** — **done**: `claw doctor --output-format json` now keeps the human-readable `message`/`report` while also emitting structured per-check diagnostics (`name`, `status`, `summary`, `details`, plus typed fields like workspace paths and sandbox fallback data), with regression coverage in `output_format_contract.rs`.
24. **Plugin lifecycle init/shutdown test flakes under workspace-parallel execution** — dogfooding surfaced that `build_runtime_runs_plugin_lifecycle_init_and_shutdown` could fail under `cargo test --workspace` while passing in isolation because sibling tests raced on tempdir-backed shell init script paths. **Done (re-verified 2026-04-11):** the current mainline helpers now isolate plugin lifecycle temp resources robustly enough that both `cargo test -p rusty-claude-cli build_runtime_runs_plugin_lifecycle_init_and_shutdown -- --nocapture` and `cargo test -p plugins plugin_registry_runs_initialize_and_shutdown_for_enabled_plugins -- --nocapture` pass, and the current `cargo test --workspace` run includes both tests as green. Treat the old filing as stale unless a new parallel-execution repro appears.
25. **`plugins::hooks::collects_and_runs_hooks_from_enabled_plugins` flaked on Linux CI, root cause was a stdin-write race not missing exec bit** — **done at `172a2ad` on 2026-04-08**. Dogfooding reproduced this four times on `main` (CI runs [24120271422](https://github.com/ultraworkers/claw-code/actions/runs/24120271422), [24120538408](https://github.com/ultraworkers/claw-code/actions/runs/24120538408), [24121392171](https://github.com/ultraworkers/claw-code/actions/runs/24121392171), [24121776826](https://github.com/ultraworkers/claw-code/actions/runs/24121776826)), escalating from first-attempt-flake to deterministic-red on the third push. Failure mode was `PostToolUse hook .../hooks/post.sh failed to start for "Read": Broken pipe (os error 32)` surfacing from `HookRunResult`. **Initial diagnosis was wrong.** The first theory (documented in earlier revisions of this entry and in the root-cause note on commit `79da4b8`) was that `write_hook_plugin` in `rust/crates/plugins/src/hooks.rs` was writing the generated `.sh` files without the execute bit and `Command::new(path).spawn()` was racing on fork/exec. An initial chmod-only fix at `4f7b674` was shipped against that theory and **still failed CI on run `24121776826`** with the same `Broken pipe` symptom, falsifying the chmod-only hypothesis. **Actual root cause.** `CommandWithStdin::output_with_stdin` in `rust/crates/plugins/src/hooks.rs` was unconditionally propagating `write_all` errors on the child's stdin pipe, including `std::io::ErrorKind::BrokenPipe`. The test hook scripts run in microseconds (`#!/bin/sh` + a single `printf`), so the child exits and closes its stdin before the parent finishes writing the ~200-byte JSON hook payload. On Linux the pipe raises `EPIPE` immediately; on macOS the pipe happens to buffer the small payload before the child exits, which is why the race only surfaced on ubuntu CI runners. The parent's `write_all` returned `Err(BrokenPipe)`, `output_with_stdin` returned that as a hook failure, and `run_command` classified the hook as "failed to start" even though the child had already run to completion and printed the expected message to stdout. **Fix (commit `172a2ad`, force-pushed over `4f7b674`).** Three parts: (1) **actual fix** — `output_with_stdin` now matches the `write_all` result and swallows `BrokenPipe` specifically, while propagating all other write errors unchanged; after a `BrokenPipe` swallow the code still calls `wait_with_output()` so stdout/stderr/exit code are still captured from the cleanly-exited child. (2) **hygiene hardening** — a new `make_executable` helper sets mode `0o755` on each generated `.sh` via `std::os::unix::fs::PermissionsExt` under `#[cfg(unix)]`. This is defense-in-depth for future non-sh hook runners, not the bug that was biting CI. (3) **regression guard** — new `generated_hook_scripts_are_executable` test under `#[cfg(unix)]` asserts each generated `.sh` file has at least one execute bit set (`mode & 0o111 != 0`) so future tweaks cannot silently regress the hygiene change. **Verification.** `cargo test --release -p plugins` 35 passing, fmt clean, clippy `-D warnings` clean; CI run [24121999385](https://github.com/ultraworkers/claw-code/actions/runs/24121999385) went green on first attempt on `main` for the hotfix commit. **Meta-lesson.** `Broken pipe (os error 32)` from a child-process spawn path is ambiguous between "could not exec" and "exec'd and exited before the parent finished writing stdin." The first theory cargo-culted the "could not exec" reading because the ROADMAP scaffolding anchored on the exec-bit guess; falsification came from empirical CI, not from code inspection. Record the pattern: when a pipe error surfaces on fork/exec, instrument what `wait_with_output()` actually reports on the child before attributing the failure to a permissions or issue.
26. **Resumed local-command JSON parity gap** — **done**: direct `claw --output-format json` already had structured renderers for `sandbox`, `mcp`, `skills`, `version`, and `init`, but resumed `claw --output-format json --resume <session> /…` paths still fell back to prose because resumed slash dispatch only emitted JSON for `/status`. Resumed `/sandbox`, `/mcp`, `/skills`, `/version`, and `/init` now reuse the same JSON envelopes as their direct CLI counterparts, with regression coverage in `rust/crates/rusty-claude-cli/tests/resume_slash_commands.rs` and `rust/crates/rusty-claude-cli/tests/output_format_contract.rs`.
27. **`dev/rust` `cargo test -p rusty-claude-cli` reads host `~/.claude/plugins/installed/` from real `$HOME` and fails parse-time on any half-installed user plugin** — dogfooding on 2026-04-08 (filed from gaebal-gajae's clawhip bullet at message `1491322807026454579` after the provider-matrix branch QA surfaced it) reproduced 11 deterministic failures on clean `dev/rust` HEAD of the form `panicked at crates/rusty-claude-cli/src/main.rs:3953:31: args should parse: "hook path \`/Users/yeongyu/.claude/plugins/installed/sample-hooks-bundled/./hooks/pre.sh\` does not exist; hook path \`...\post.sh\` does not exist"` covering `parses_prompt_subcommand`, `parses_permission_mode_flag`, `defaults_to_repl_when_no_args`, `parses_resume_flag_with_slash_command`, `parses_system_prompt_options`, `parses_bare_prompt_and_json_output_flag`, `rejects_unknown_allowed_tools`, `parses_resume_flag_with_multiple_slash_commands`, `resolves_model_aliases_in_args`, `parses_allowed_tools_flags_with_aliases_and_lists`, `parses_login_and_logout_subcommands`. **Same failures do NOT reproduce on `main`** (re-verified with `cargo test --release -p rusty-claude-cli` against `main` HEAD `79da4b8`, all 156 tests pass). **Root cause is two-layered.** First, on `dev/rust` `parse_args` eagerly walks user-installed plugin manifests under `~/.claude/plugins/installed/` and validates that every declared hook script exists on disk before returning a `CliAction`, so any half-installed plugin in the developer's real `$HOME` (in this case `~/.claude/plugins/installed/sample-hooks-bundled/` whose `.claude-plugin` manifest references `./hooks/pre.sh` and `./hooks/post.sh` but whose `hooks/` subdirectory was deleted) makes argv parsing itself fail. Second, the test harness on `dev/rust` does not redirect `$HOME` or `XDG_CONFIG_HOME` to a fixture for the duration of the test — there is no `env_lock`-style guard equivalent to the one `main` already uses (`grep -n env_lock rust/crates/rusty-claude-cli/src/main.rs` returns 0 hits on `dev/rust` and 30+ hits on `main`). Together those two gaps mean `dev/rust` `cargo test -p rusty-claude-cli` is non-deterministic on every clean clone whose owner happens to have any non-pristine plugin in `~/.claude/`. **Action (two parts).** (a) Backport the `env_lock`-based test isolation pattern from `main` into `dev/rust`'s `rusty-claude-cli` test module so each test runs against a temp `$HOME`/`XDG_CONFIG_HOME` and cannot read host plugin state. (b) Decouple `parse_args` from filesystem hook validation on `dev/rust` (the same decoupling already on `main`, where hook validation happens later in the lifecycle than argv parsing) so even outside tests a partially installed user plugin cannot break basic CLI invocation. **Branch scope.** This is a `dev/rust` catchup against `main`, not a `main` regression. Tracking it here so the dev/rust merge train picks it up before the next dev/rust release rather than rediscovering it in CI.
28. **Auth-provider truth: error copy fails real users at the env-var-vs-header layer** — dogfooded live on 2026-04-08 in #claw-code (Sisyphus Labs guild), two separate new users hit adjacent failure modes within minutes of each other that both trace back to the same root: the `MissingApiKey` / 401 error surface does not teach users how the auth inputs map to HTTP semantics, so a user who sets a "reasonable-looking" env var still hits a hard error with no signpost. **Case 1 (varleg, Norway).** Wanted to use OpenRouter via the OpenAI-compat path. Found a comparison table claiming "provider-agnostic (Claude, OpenAI, local models)" and assumed it Just Worked. Set `OPENAI_API_KEY` to an OpenRouter `sk-or-v1-...` key and a model name without an `openai/` prefix; claw's provider detection fell through to Anthropic first because `ANTHROPIC_API_KEY` was still in the environment. Unsetting `ANTHROPIC_API_KEY` got them `ANTHROPIC_AUTH_TOKEN or ANTHROPIC_API_KEY is not set` instead of a useful hint that the OpenAI path was right there. Fix delivered live as a channel reply: use `main` branch (not `dev/rust`), export `OPENAI_BASE_URL=https://openrouter.ai/api/v1` alongside `OPENAI_API_KEY`, and prefix the model name with `openai/` so the prefix router wins over env-var presence. **Case 2 (stanley078852).** Had set `ANTHROPIC_AUTH_TOKEN="sk-ant-..."` and was getting 401 `Invalid bearer token` from Anthropic. Root cause: `sk-ant-` keys are `x-api-key`-header keys, not bearer tokens. `ANTHROPIC_API_KEY` path in `anthropic.rs` sends the value as `x-api-key`; `ANTHROPIC_AUTH_TOKEN` path sends it as `Authorization: Bearer` (for OAuth access tokens from `claw login`). Setting an `sk-ant-` key in the wrong env var makes claw send it as `Bearer sk-ant-...` which Anthropic rejects at the edge with 401 before it ever reaches the completions endpoint. The error text propagated all the way to the user (`api returned 401 Unauthorized (authentication_error) ... Invalid bearer token`) with zero signal that the problem was env-var choice, not key validity. Fix delivered live as a channel reply: move the `sk-ant-...` key to `ANTHROPIC_API_KEY` and unset `ANTHROPIC_AUTH_TOKEN`. **Pattern.** Both cases are failures at the *auth-intent translation* layer: the user chose an env var that made syntactic sense to them (`OPENAI_API_KEY` for OpenAI, `ANTHROPIC_AUTH_TOKEN` for Anthropic auth) but the actual wire-format routing requires a more specific choice. The error messages surface the HTTP-layer symptom (401, missing-key) without bridging back to "which env var should you have used and why." **Action.** Three concrete improvements, scoped for a single `main`-side PR: (a) In `ApiError::MissingCredentials` Display, when the Anthropic path is the one being reported but `OPENAI_API_KEY`, `XAI_API_KEY`, or `DASHSCOPE_API_KEY` are present in the environment, extend the message with "— but I see `$OTHER_KEY` set; if you meant to use that provider, prefix your model name with `openai/`, `grok`, or `qwen/` respectively so prefix routing selects it." (b) In the 401-from-Anthropic error path in `anthropic.rs`, when the failing auth source is `BearerToken` AND the bearer token starts with `sk-ant-`, append "— looks like you put an `sk-ant-*` API key in `ANTHROPIC_AUTH_TOKEN`, which is the Bearer-header path. Move it to `ANTHROPIC_API_KEY` instead (that env var maps to `x-api-key`, which is the correct header for `sk-ant-*` keys)." Same treatment for OAuth access tokens landing in `ANTHROPIC_API_KEY` (symmetric mis-assignment). (c) In `rust/README.md` on `main` and the matrix section on `dev/rust`, add a short "Which env var goes where" paragraph mapping `sk-ant-*` → `ANTHROPIC_API_KEY` and OAuth access token → `ANTHROPIC_AUTH_TOKEN`, with the one-line explanation of `x-api-key` vs `Authorization: Bearer`. **Verification path.** Both improvements can be tested with unit tests against `ApiError::fmt` output (the prefix-routing hint) and with a targeted integration test that feeds an `sk-ant-*`-shaped token into `BearerToken` and asserts the fmt output surfaces the correction hint (no HTTP call needed). **Source.** Live users in #claw-code at `1491328554598924389` (varleg) and `1491329840706486376` (stanley078852) on 2026-04-08. **Partial landing (`ff1df4c`).** Action parts (a), (b), (c) shipped on `main`: `MissingCredentials` now carries an optional hint field and renders adjacent-provider signals, Anthropic 401 + `sk-ant-*` bearer gets a correction hint, USAGE.md has a "Which env var goes where" section. BUT the copy fix only helps users who fell through to the Anthropic auth path by accident — it does NOT fix the underlying routing bug where the CLI instantiates `AnthropicRuntimeClient` unconditionally and ignores prefix routing at the runtime-client layer. That deeper routing gap is tracked separately as #29 below and was filed within hours of #28 landing when live users still hit `missing Anthropic credentials` with `--model openai/gpt-4` and all `ANTHROPIC_*` env vars unset.
29. **CLI provider dispatch is hardcoded to Anthropic, ignoring prefix routing** — **done at `8dc6580` on 2026-04-08**. Changed `AnthropicRuntimeClient.client` from concrete `AnthropicClient` to `ApiProviderClient` (the api crate's `ProviderClient` enum), which dispatches to Anthropic / xAI / OpenAi at construction time based on `detect_provider_kind(&resolved_model)`. 1 file, +59 −7, all 182 rusty-claude-cli tests pass, CI green at run `24125825431`. Users can now run `claw --model openai/gpt-4.1-mini prompt "hello"` with only `OPENAI_API_KEY` set and it routes correctly. **Original filing below for the trace record.** Dogfooded live on 2026-04-08 within hours of ROADMAP #28 landing. Users in #claw-code (nicma at `1491342350960562277`, Jengro at `1491345009021030533`) followed the exact "use main, set OPENAI_API_KEY and OPENAI_BASE_URL, unset ANTHROPIC_*, prefix the model with `openai/`" checklist from the #28 error-copy improvements AND STILL hit `error: missing Anthropic credentials; export ANTHROPIC_AUTH_TOKEN or ANTHROPIC_API_KEY before calling the Anthropic API`. **Reproduction on `main` HEAD `ff1df4c`:** `unset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN; export OPENAI_API_KEY=sk-...; export OPENAI_BASE_URL=https://api.openai.com/v1; claw --model openai/gpt-4 prompt 'test'` → reproduces the error deterministically. **Root cause (traced).** `rust/crates/rusty-claude-cli/src/main.rs` at `build_runtime_with_plugin_state` (line ~6221) unconditionally builds `AnthropicRuntimeClient::new(session_id, model, ...)` without consulting `providers::detect_provider_kind(&model)`. `BuiltRuntime` at line ~2855 is statically typed as `ConversationRuntime<AnthropicRuntimeClient, CliToolExecutor>`, so even if the dispatch logic existed there would be nowhere to slot an alternative client. `providers/mod.rs::metadata_for_model` correctly identifies `openai/gpt-4` as `ProviderKind::OpenAi` at the metadata layer — the routing decision is *computed* correctly, it's just *never used* to pick a runtime client. The result is that the CLI is structurally single-provider (Anthropic only) even though the `api` crate's `openai_compat.rs`, `XAI_ENV_VARS`, `DASHSCOPE_ENV_VARS`, and `send_message_streaming` all exist and are exercised by unit tests inside the `api` crate. The provider matrix in `rust/README.md` is misleading because it describes the api-crate capabilities, not the CLI's actual dispatch behaviour. **Why #28 didn't catch this.** ROADMAP #28 focused on the `MissingCredentials` error *message* (adding hints when adjacent provider env vars are set, or when a bearer token starts with `sk-ant-*`). None of its tests exercised the `build_runtime` code path — they were all unit tests against `ApiError::fmt` output. The routing bug survives #28 because the `Display` improvements fire AFTER the hardcoded Anthropic client has already been constructed and failed. You need the CLI to dispatch to a different client in the first place for the new hints to even surface at the right moment. **Action (single focused commit).** (1) New `OpenAiCompatRuntimeClient` struct in `rust/crates/rusty-claude-cli/src/main.rs` mirroring `AnthropicRuntimeClient` but delegating to `openai_compat::send_message_streaming`. One client type handles OpenAI, xAI, DashScope, and any OpenAI-compat endpoint — they differ only in base URL and auth env var, both of which come from the `ProviderMetadata` returned by `metadata_for_model`. (2) New enum `DynamicApiClient { Anthropic(AnthropicRuntimeClient), OpenAiCompat(OpenAiCompatRuntimeClient) }` that implements `runtime::ApiClient` by matching on the variant and delegating. (3) Retype `BuiltRuntime` from `ConversationRuntime<AnthropicRuntimeClient, CliToolExecutor>` to `ConversationRuntime<DynamicApiClient, CliToolExecutor>`, update the Deref/DerefMut/new spots. (4) In `build_runtime_with_plugin_state`, call `detect_provider_kind(&model)` and construct either variant of `DynamicApiClient`. Prefix routing wins over env-var presence (that's the whole point). (5) Integration test using a mock OpenAI-compat server (reuse `mock_parity_harness` pattern from `crates/api/tests/`) that feeds `claw --model openai/gpt-4 prompt 'test'` with `OPENAI_BASE_URL` pointed at the mock and no `ANTHROPIC_*` env vars, asserts the request reaches the mock, and asserts the response round-trips as an `AssistantEvent`. (6) Unit test that `build_runtime_with_plugin_state` with `model="openai/gpt-4"` returns a `BuiltRuntime` whose inner client is the `DynamicApiClient::OpenAiCompat` variant. **Verification.** `cargo test --workspace`, `cargo fmt --all`, `cargo clippy --workspace`. **Source.** Live users nicma (`1491342350960562277`) and Jengro (`1491345009021030533`) in #claw-code on 2026-04-08, within hours of #28 landing.

41. **Phantom completions root cause: global session store has no per-worktree isolation** —

    **Root cause.** The session store under `~/.local/share/opencode` is global to the host. Every `opencode serve` instance — including the parallel lane workers spawned per worktree — reads and writes the same on-disk session directory. Sessions are keyed only by id and timestamp, not by the workspace they were created in, so there is no structural barrier between a session created in worktree `/tmp/b4-phantom-diag` and one created in `/tmp/b4-omc-flat`. Whichever serve instance picks up a given session id can drive it from whatever CWD that serve happens to be running in.

    **Impact.** Parallel lanes silently cross wires. A lane reports a clean run — file edits, builds, tests — and the orchestrator marks the lane green, but the writes were applied against another worktree's CWD because a sibling `opencode serve` won the session race. The originating worktree shows no diff, the *other* worktree gains unexplained edits, and downstream consumers (clawhip lane events, PR pushes, merge gates) treat the empty originator as a successful no-op. These are the "phantom completions" we keep chasing: success messaging without any landed changes in the lane that claimed them, plus stray edits in unrelated lanes whose own runs never touched those files. Because the report path is happy, retries and recovery recipes never fire, so the lane silently wedges until a human notices the diff is empty.

    **Proposed fix.** Bind every session to its workspace root + branch at creation time and refuse to drive it from any other CWD.

    - At session creation, capture the canonical workspace root (resolved git worktree path) and the active branch and persist them on the session record.
    - On every load (`opencode serve`, slash-command resume, lane recovery), validate that the current process CWD matches the persisted workspace root before any tool with side effects (file_ops, bash, git) is allowed to run. Mismatches surface as a typed `WorkspaceMismatch` failure class instead of silently writing to the wrong tree.
    - Namespace the on-disk session path under the workspace fingerprint (e.g. `<session_store>/<workspace_hash>/<session_id>`) so two parallel `opencode serve` instances physically cannot collide on the same session id.
    - Forks inherit the parent's workspace root by default; an explicit re-bind is required to move a session to a new worktree, and that re-bind is itself recorded as a structured event so the orchestrator can audit cross-worktree handoffs.
    - Surface a `branch.workspace_mismatch` lane event so clawhip stops counting wrong-CWD writes as lane completions.

    **Status.** Done. Managed-session creation/list/latest/load/fork now route through the per-worktree `SessionStore` namespace in runtime + CLI paths, session loads/resumes reject wrong-workspace access with typed `SessionControlError::WorkspaceMismatch` details, `branch.workspace_mismatch` / `workspace_mismatch` are available on the lane-event surface, and same-workspace legacy flat sessions remain readable while mismatched legacy access is blocked. Focused runtime/CLI/tools coverage for the isolation path is green, and `cargo test --workspace --exclude compat-harness` passes. `cargo clippy --workspace --all-targets -- -D warnings` still fails on pre-existing lints in unchanged `rust/crates/rusty-claude-cli/build.rs`, so that lint cleanup remains outside this roadmap item.

## Deployment Architecture Gap (filed from dogfood 2026-04-08)

### WorkerState is in the runtime; /state is NOT in opencode serve

**Root cause discovered during batch 8 dogfood.**

`worker_boot.rs` has a solid `WorkerStatus` state machine (`Spawning → TrustRequired → ReadyForPrompt → Running → Finished/Failed`). It is exported from `runtime/src/lib.rs` as a public API. But claw-code is a **plugin** loaded inside the `opencode` binary — it cannot add HTTP routes to `opencode serve`. The HTTP server is 100% owned by the upstream opencode process (v1.3.15).

**Impact:** There is no way to `curl localhost:4710/state` and get back a JSON `WorkerStatus`. Any such endpoint would require either:
1. Upstreaming a `/state` route into opencode's HTTP server (requires a PR to sst/opencode), or
2. Writing a sidecar HTTP process that queries the `WorkerRegistry` in-process (possible but fragile), or
3. Writing `WorkerStatus` to a well-known file path (`.claw/worker-state.json`) that an external observer can poll.

**Recommended path:** Option 3 — emit `WorkerStatus` transitions to `.claw/worker-state.json` on every state change. This is purely within claw-code's plugin scope, requires no upstream changes, and gives clawhip a file it can poll to distinguish a truly stalled worker from a quiet-but-progressing one.

**Action item:** Wire `WorkerRegistry::transition()` to atomically write `.claw/worker-state.json` on every state transition. Add a `claw state` CLI subcommand that reads and prints this file. Add regression test.

**Prior session note:** A previous session summary claimed commit `0984cca` landed a `/state` HTTP endpoint via axum. This was incorrect — no such commit exists on main, axum is not a dependency, and the HTTP server is not ours. The actual work that exists: `worker_boot.rs` with `WorkerStatus` enum + `WorkerRegistry`, fully wired into `runtime/src/lib.rs` as public exports.

## Startup Friction Gap: No Default trusted_roots in Settings (filed 2026-04-08)

### Every lane starts with manual trust babysitting unless caller explicitly passes roots

**Root cause discovered during direct dogfood of WorkerCreate tool.**

`WorkerCreate` accepts a `trusted_roots: Vec<String>` parameter. If the caller omits it (or passes `[]`), every new worker immediately enters `TrustRequired` and stalls — requiring manual intervention to advance to `ReadyForPrompt`. There is no mechanism to configure a default allowlist in `settings.json` or `.claw/settings.json`.

**Impact:** Batch tooling (clawhip, lane orchestrators) must pass `trusted_roots` explicitly on every `WorkerCreate` call. If a batch script forgets the field, all workers in that batch stall silently at `trust_required`. This was the root cause of several "batch 8 lanes not advancing" incidents.

**Recommended fix:**
1. Add a `trusted_roots` field to `RuntimeConfig` (or a nested `[trust]` table), loaded via `ConfigLoader`.
2. In `WorkerRegistry::spawn_worker()`, merge config-level `trusted_roots` with any per-call overrides.
3. Default: empty list (safest). Users opt in by adding their repo paths to settings.
4. Update `config_validate` schema with the new field.

**Action item:** Wire `RuntimeConfig::trusted_roots()` → `WorkerRegistry::spawn_worker()` default. Cover with test: config with `trusted_roots = ["/tmp"]` → spawning worker in `/tmp/x` auto-resolves trust without caller passing the field.

## Observability Transport Decision (filed 2026-04-08)

### Canonical state surface: CLI/file-based. HTTP endpoint deferred.

**Decision:** `claw state` reading `.claw/worker-state.json` is the **blessed observability contract** for clawhip and downstream tooling. This is not a stepping-stone — it is the supported surface. Build against it.

**Rationale:**
- claw-code is a plugin running inside the opencode binary. It cannot add HTTP routes to `opencode serve` — that server belongs to upstream sst/opencode.
- The file-based surface is fully within plugin scope: `emit_state_file()` in `worker_boot.rs` writes atomically on every `WorkerStatus` transition.
- `claw state --output-format json` gives clawhip everything it needs: `status`, `is_ready`, `seconds_since_update`, `trust_gate_cleared`, `last_event`, `updated_at`.
- Polling a local file has lower latency and fewer failure modes than an HTTP round-trip to a sidecar.
- An HTTP state endpoint would require either (a) upstreaming a route to sst/opencode — a multi-week PR cycle with no guarantee of acceptance — or (b) a sidecar process that queries `WorkerRegistry` in-process, which is fragile and adds an extra failure domain.

**What downstream tooling (clawhip) should do:**
1. After `WorkerCreate`, poll `.claw/worker-state.json` (or run `claw state --output-format json`) in the worker's CWD at whatever interval makes sense (e.g. 5s).
2. Trust `seconds_since_update > 60` in `trust_required` status as the stall signal.
3. Call `WorkerResolveTrust` tool to unblock, or `WorkerRestart` to reset.

**HTTP endpoint tracking:** Not scheduled. If a concrete use case emerges that file polling cannot serve (e.g. remote workers over a network boundary), open a new issue to upstream a `/worker/state` route to sst/opencode at that time. Until then: file/CLI is canonical.

## Provider Routing: Model-Name Prefix Must Win Over Env-Var Presence (fixed 2026-04-08, `0530c50`)

### `openai/gpt-4.1-mini` was silently misrouted to Anthropic when ANTHROPIC_API_KEY was set

**Root cause:** `metadata_for_model` returned `None` for any model not matching `claude` or `grok` prefix.
`detect_provider_kind` then fell through to auth-sniffer order: first `has_auth_from_env_or_saved()` (Anthropic), then `OPENAI_API_KEY`, then `XAI_API_KEY`.

If `ANTHROPIC_API_KEY` was present in the environment (e.g. user has both Anthropic and OpenRouter configured), any unknown model — including explicitly namespaced ones like `openai/gpt-4.1-mini` — was silently routed to the Anthropic client, which then failed with `missing Anthropic credentials` or a confusing 402/auth error rather than routing to OpenAI-compatible.

**Fix:** Added explicit prefix checks in `metadata_for_model`:
- `openai/` prefix → `ProviderKind::OpenAi`
- `gpt-` prefix → `ProviderKind::OpenAi`

Model name prefix now wins unconditionally over env-var presence. Regression test locked in: `providers::tests::openai_namespaced_model_routes_to_openai_not_anthropic`.

**Lesson:** Auth-sniffer fallback order is fragile. Any new provider added in the future should be registered in `metadata_for_model` via a model-name prefix, not left to env-var order. This is the canonical extension point.

30. **DashScope model routing in ProviderClient dispatch uses wrong config** — **done at `adcea6b` on 2026-04-08**. `ProviderClient::from_model_with_anthropic_auth` dispatched all `ProviderKind::OpenAi` matches to `OpenAiCompatConfig::openai()` (reads `OPENAI_API_KEY`, points at `api.openai.com`). But DashScope models (`qwen-plus`, `qwen/qwen-max`) return `ProviderKind::OpenAi` because DashScope speaks the OpenAI wire format — they need `OpenAiCompatConfig::dashscope()` (reads `DASHSCOPE_API_KEY`, points at `dashscope.aliyuncs.com/compatible-mode/v1`). Fix: consult `metadata_for_model` in the `OpenAi` dispatch arm and pick `dashscope()` vs `openai()` based on `metadata.auth_env`. Adds regression test + `pub base_url()` accessor. 2 files, +94/−3. Authored by droid (Kimi K2.5 Turbo) via acpx, cleaned up by Jobdori.

31. **`code-on-disk → verified commit lands` depends on undocumented executor quirks** — dogfooded 2026-04-08 during live fix session. Three hidden contracts tripped the "last mile" path when using droid via acpx in the claw-code workspace: **(a) hidden CWD contract** — droid's `terminal/create` rejects `cd /path && cargo build` compound commands with `spawn ENOENT`; callers must pass `--cwd` or split commands; **(b) hidden commit-message transport limit** — embedding a multi-line commit message in a single shell invocation hits `ENAMETOOLONG`; workaround is `git commit -F <file>` but the caller must know to write the file first; **(c) hidden workspace lint/edition contract** — `unsafe_code = "forbid"` workspace-wide with Rust 2021 edition makes `unsafe {}` wrappers incorrect for `set_var`/`remove_var`, but droid generates Rust 2024-style unsafe blocks without inspecting the workspace Cargo.toml or clippy config. Each of these required the orchestrator to learn the constraint by failing, then switching strategies. **Acceptance bar:** a fresh agent should be able to verify/commit/push a correct diff in this workspace without needing to know executor-specific shell trivia ahead of time. **Fix shape:** (1) `run-acpx.sh`-style wrapper that normalizes the commit idiom (always writes to temp file, sets `--cwd`, splits compound commands); (2) inject workspace constraints into the droid/acpx task preamble (edition, lint gates, known shell executor quirks) so the model doesn't have to discover them from failures; (3) or upstream a fix to the executor itself so `cd /path && cmd` chains work correctly.

32. **OpenAI-compatible provider/model-id passthrough is not fully literal** — **verified no-bug on 2026-04-09**: `resolve_model_alias()` only matches bare shorthand aliases (`opus`/`sonnet`/`haiku`) and passes everything else through unchanged, so `openai/gpt-4` reaches the dispatch layer unmodified. `strip_routing_prefix()` at `openai_compat.rs:732` then strips only recognised routing prefixes (`openai`, `xai`, `grok`, `qwen`) so the wire model is the bare backend id. No fix needed. **Original filing below.**

32. **OpenAI-compatible provider/model-id passthrough is not fully literal** — dogfooded 2026-04-08 via live user in #claw-code who confirmed the exact backend model id works outside claw but fails through claw for an OpenAI-compatible endpoint. The gap: `openai/` prefix is correctly used for **transport selection** (pick the OpenAI-compat client) but the **wire model id** — the string placed in `"model": "..."` in the JSON request body — may not be the literal backend model string the user supplied. Two candidate failure modes: **(a)** `resolve_model_alias()` is called on the model string before it reaches the wire — alias expansion designed for Anthropic/known models corrupts a user-supplied backend-specific id; **(b)** the `openai/` routing prefix may not be stripped before `build_chat_completion_request` packages the body, so backends receive `openai/gpt-4` instead of `gpt-4`. **Fix shape:** cleanly separate transport selection from wire model id. Transport selection uses the prefix; wire model id is the user-supplied string minus only the routing prefix — no alias expansion, no prefix leakage. **Trace path for next session:** (1) find where `resolve_model_alias()` is called relative to the OpenAI-compat dispatch path; (2) inspect what `build_chat_completion_request` puts in `"model"` for an `openai/some-backend-id` input. **Source:** live user in #claw-code 2026-04-08, confirmed exact model id works outside claw, fails through claw for OpenAI-compat backend.

33. **OpenAI `/responses` endpoint rejects claw's tool schema: `object schema missing properties` / `invalid_function_parameters`** — **done at `e7e0fd2` on 2026-04-09**. Added `normalize_object_schema()` in `openai_compat.rs` which recursively walks JSON Schema trees and injects `"properties": {}` and `"additionalProperties": false` on every object-type node (without overwriting existing values). Called from `openai_tool_definition()` so both `/chat/completions` and `/responses` receive strict-validator-safe schemas. 3 unit tests added. All api tests pass. **Original filing below.**
33. **OpenAI `/responses` endpoint rejects claw's tool schema: `object schema missing properties` / `invalid_function_parameters`** — dogfooded 2026-04-08 via live user in #claw-code. Repro: startup succeeds, provider routing succeeds (`Connected: gpt-5.4 via openai`), but request fails when claw sends tool/function schema to a `/responses`-compatible OpenAI backend. Backend rejects `StructuredOutput` with `object schema missing properties` and `invalid_function_parameters`. This is distinct from the `#32` model-id passthrough issue — routing and transport work correctly. The failure is at the schema validation layer: claw's tool schema is acceptable for `/chat/completions` but not strict enough for `/responses` endpoint validation. **Sharp next check:** emit what schema claw sends for `StructuredOutput` tool functions, compare against OpenAI `/responses` spec for strict JSON schema validation (required `properties` object, `additionalProperties: false`, etc). Likely fix: add missing `properties: {}` on object types, ensure `additionalProperties: false` is present on all object schemas in the function tool JSON. **Source:** live user in #claw-code 2026-04-08 with `gpt-5.4` on OpenAI-compat backend.


34. **`reasoning_effort` / `budget_tokens` not surfaced on OpenAI-compat path** — dogfooded 2026-04-09. Users asking for "reasoning effort parity with opencode" are hitting a structural gap: `MessageRequest` in `rust/crates/api/src/types.rs` has no `reasoning_effort` or `budget_tokens` field, and `build_chat_completion_request` in `openai_compat.rs` does not inject either into the request body. This means passing `--thinking` or equivalent to an OpenAI-compat reasoning model (e.g. `o4-mini`, `deepseek-r1`, any model that accepts `reasoning_effort`) silently drops the field — the model runs without the requested effort level, and the user gets no warning. **Contrast with Anthropic path:** `anthropic.rs` already maps `thinking` config into `anthropic.thinking.budget_tokens` in the request body. **Fix shape:** (a) Add optional `reasoning_effort: Option<String>` field to `MessageRequest`; (b) In `build_chat_completion_request`, if `reasoning_effort` is `Some`, emit `"reasoning_effort": value` in the JSON body; (c) In the CLI, wire `--thinking low/medium/high` or equivalent to populate the field when the resolved provider is `ProviderKind::OpenAi`; (d) Add unit test asserting `reasoning_effort` appears in the request body when set. **Source:** live user questions in #claw-code 2026-04-08/09 (dan_theman369 asking for "same flow as opencode for reasoning effort"; gaebal-gajae confirmed gap at `1491453913100976339`). Companion gap to #33 on the OpenAI-compat path.

35. **OpenAI gpt-5.x requires max_completion_tokens not max_tokens** -- dogfooded 2026-04-09. rklehm repro: gpt-5.2 via OpenAI-compat, startup OK, routing OK, but requests fail because claw emits max_tokens where gpt-5* requires max_completion_tokens. Fix: emit max_completion_tokens on OpenAI-compat path (backward-compatible). Add unit test. Source: rklehm in #claw-code 2026-04-09.

36. **Custom/project skill invocation disconnected from skill discovery** -- dogfooded 2026-04-09. /skills lists custom skills (e.g. caveman) but bare skill-name invocation does not dispatch them; falls through to plain model prompt. Fix: audit classify_skills_slash_command, ensure any skill listed by /skills has a deterministic invocation path, or document the correct syntax. Source: gaebal-gajae dogfood 2026-04-09.

37. **Claude subscription login path should be removed, not deprecated** -- dogfooded 2026-04-09. Official auth should be API key only (`ANTHROPIC_API_KEY`) or OAuth bearer token via `ANTHROPIC_AUTH_TOKEN`; the local `claw login` / `claw logout` subscription-style flow created legal/billing ambiguity and a misleading saved-OAuth fallback. **Done (verified 2026-04-11):** removed the direct `claw login` / `claw logout` CLI surface, removed `/login` and `/logout` from shared slash-command discovery, changed both CLI and provider startup auth resolution to ignore saved OAuth credentials, and updated auth diagnostics to point only at `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN`. Verification: targeted `commands`, `api`, and `rusty-claude-cli` tests for removed login/logout guidance and ignored saved OAuth all pass, and `cargo check -p api -p commands -p rusty-claude-cli` passes. Source: gaebal-gajae policy decision 2026-04-09.

38. **Dead-session opacity: bot cannot self-detect compaction vs broken tool surface** -- dogfooded 2026-04-09. Jobdori session spent ~15h declaring itself "dead" in-channel while tools were actually returning correct results within each turn. Root cause: context compaction causes tool outputs to be summarised away between turns, making the bot interpret absence-of-remembered-output as tool failure. This is a distinct failure mode from ROADMAP #31 (executor quirks): the session is alive and tools are functional, but the agent cannot tell the difference between "my last tool call produced no output" (compaction) and "the tool is broken". Downstream: repetitive false-dead signals in the channel, work not getting done despite the execution surface being live. Fix shape: (a) probe with a short known-output command at turn start if context has been compacted; (b) gate "I am dead" declarations behind at least one within-turn tool call with a verified non-empty result; (c) consider adding a session-health canary cron that fires a wake with a minimal probe and checks the result. Source: Jobdori self-dogfood 2026-04-09; observed in #clawcode-building-in-public across multiple Clawhip nudge cycles.

39. **Several slash commands are registered but not implemented: /branch, /rewind, /ide, /tag, /output-style, /add-dir** -- dogfooded 2026-04-09. These commands appear in the REPL completions surface but silently print 'Command registered but not yet implemented.' and return false. Users (mezz2301 in #claw-code) hit this as 'many features are not supported in this version now'. Fix shape: either (a) implement the missing commands, or (b) remove them from completions/help output until they are ready, so the discovery surface matches what actually works. Source: mezz2301 in #claw-code 2026-04-09; pinpointed in main.rs:3728.

40. **Surface broken installed plugins before they become support ghosts** — community-support lane. Clawhip commit `ff6d3b7` on worktree `claw-code-community-support-plugin-list-load-failures` / branch `community-support/plugin-list-load-failures`. When an installed plugin has a broken manifest (missing hook scripts, parse errors, bad json), the plugin silently fails to load and the user sees nothing — no warning, no list entry, no hint. Related to ROADMAP #27 (host plugin path leaking into tests) but at the user-facing surface: the test gap and the UX gap are siblings of the same root. Landing on `main` will close the silent-ghost class of support issues where users report "my plugin does nothing" with no error to share. Track until merged to `main`.

40. **Surface broken installed plugins before they become support ghosts** — community-support lane. Clawhip commit `ff6d3b7` on worktree `claw-code-community-support-plugin-list-load-failures` / branch `community-support/plugin-list-load-failures`. When an installed plugin has a broken manifest (missing hook scripts, parse errors, bad json), the plugin silently fails to load and the user sees nothing — no warning, no list entry, no hint. Related to ROADMAP #27 (host plugin path leaking into tests) but at the user-facing surface: the test gap and the UX gap are siblings of the same same root. Landing on `main` will close the silent-ghost class of support issues where users report "my plugin does nothing" with no error to share. Track until merged to `main`.

41. **Stop ambient plugin state from skewing CLI regression checks** — community-support lane. Clawhip commit `7d493a7` on worktree `claw-code-community-support-plugin-test-sealing` / branch `community-support/plugin-test-sealing`. Companion to #40: the test sealing gap is the CI/developer side of the same root — host `~/.claude/plugins/installed/` bleeds into CLI test runs, making regression checks non-deterministic on any machine with a non-pristine plugin install. Closely related to ROADMAP #27 (dev/rust `cargo test` reads host plugin state). Track until merged to `main`.

42. **`--output-format json` errors emitted as prose, not JSON** — dogfooded 2026-04-09. When `claw --output-format json prompt` hits an API error, the error was printed as plain text (`error: api returned 401 ...`) to stderr instead of a JSON object. Any tool or CI step parsing claw's JSON output gets nothing parseable on failure — the error is invisible to the consumer. **Fix (`a...`):** detect `--output-format json` in `main()` at process exit and emit `{"type":"error","error":"<message>"}` to stderr instead of the prose format. Non-JSON path unchanged. **Done** in this nudge cycle.

43. **Hook ingress opacity: typed hook-health/delivery report missing** — dogfooded 2026-04-09 while wiring the agentika timer→hook→session bridge. Debugging hook delivery required manual HTTP probing and inferring state from raw status codes (404 = no route, 405 = route exists, 400 = body missing required field). No typed endpoint exists to report: route present/absent, accepted methods, mapping matched/not matched, target session resolved/not resolved, last delivery failure class. Fix shape: add `GET /hooks/health` (or `/hooks/status`) returning a structured JSON diagnostic — no auth exposure, just routing/matching/session state. Source: gaebal-gajae dogfood 2026-04-09.

44. **Broad-CWD guardrail is warning-only; needs policy-level enforcement** — dogfooded 2026-04-09. `5f6f453` added a stderr warning when claw starts from `$HOME` or filesystem root (live user kapcomunica scanned their whole machine). Warning is a mitigation, not a guardrail: the agent still proceeds with unbounded scope. Follow-up fix shape: (a) add `--allow-broad-cwd` flag to suppress the warning explicitly (for legitimate home-dir use cases); (b) in default interactive mode, prompt "You are running from your home directory — continue? [y/N]" and exit unless confirmed; (c) in `--output-format json` or piped mode, treat broad-CWD as a hard error (exit 1) with `{"type":"error","error":"broad CWD: running from home directory requires --allow-broad-cwd"}`. Source: kapcomunica in #claw-code 2026-04-09; gaebal-gajae ROADMAP note same cycle.

45. **`claw dump-manifests` fails with opaque "No such file or directory"** — dogfooded 2026-04-09. `claw dump-manifests` emits `error: failed to extract manifests: No such file or directory (os error 2)` with no indication of which file or directory is missing. **Partial fix at `47aa1a5`+1**: error message now includes `looked in: <path>` so the build-tree path is visible, what manifests are, or how to fix it. Fix shape: (a) surface the missing path in the error message; (b) add a pre-check that explains what manifests are and where they should be (e.g. `.claw/manifests/` or the plugins directory); (c) if the command is only valid after `claw init` or after installing plugins, say so explicitly. Source: Jobdori dogfood 2026-04-09.

46. **`/tokens`, `/cache`, `/stats` were dead spec — parse arms missing** — dogfooded 2026-04-09. All three had spec entries with `resume_supported: true` but no parse arms, producing the circular error "Unknown slash command: /tokens — Did you mean /tokens". Also `SlashCommand::Stats` existed but was unimplemented in both REPL and resume dispatch. **Done at `60ec2ae` 2026-04-09**: `"tokens" | "cache"` now alias to `SlashCommand::Stats`; `Stats` is wired in both REPL and resume path with full JSON output. Source: Jobdori dogfood.

47. **`/diff` fails with cryptic "unknown option 'cached'" outside a git repo; resume /diff used wrong CWD** — dogfooded 2026-04-09. `claw --resume <session> /diff` in a non-git directory produced `git diff --cached failed: error: unknown option 'cached'` because git falls back to `--no-index` mode outside a git tree. Also resume `/diff` used `session_path.parent()` (the `.claw/sessions/<id>/` dir) as CWD for the diff — never a git repo. **Done at `aef85f8` 2026-04-09**: `render_diff_report_for()` now checks `git rev-parse --is-inside-work-tree` first and returns a clear "no git repository" message; resume `/diff` uses `std::env::current_dir()`. Source: Jobdori dogfood.

48. **Piped stdin triggers REPL startup and banner instead of one-shot prompt** — dogfooded 2026-04-09. `echo "hello" | claw` started the interactive REPL, printed the ASCII banner, consumed the pipe without sending anything to the API, then exited. `parse_args` always returned `CliAction::Repl` when no args were given, never checking whether stdin was a pipe. **Done at `84b77ec` 2026-04-09**: when `rest.is_empty()` and stdin is not a terminal, read the pipe and dispatch as `CliAction::Prompt`. Empty pipe still falls through to REPL. Source: Jobdori dogfood.

49. **Resumed slash command errors emitted as prose in `--output-format json` mode** — dogfooded 2026-04-09. `claw --output-format json --resume <session> /commit` called `eprintln!()` and `exit(2)` directly, bypassing the JSON formatter. Both the slash-command parse-error path and the `run_resume_command` Err path now check `output_format` and emit `{"type":"error","error":"...","command":"..."}`. **Done at `da42421` 2026-04-09**. Source: gaebal-gajae ROADMAP #26 track; Jobdori dogfood.

50. **PowerShell tool is registered as `danger-full-access` — workspace-aware reads still require escalation** — dogfooded 2026-04-10. User running `workspace-write` session mode (tanishq_devil in #claw-code) had to use `danger-full-access` even for simple in-workspace reads via PowerShell (e.g. `Get-Content`). Root cause traced by gaebal-gajae: `PowerShell` tool spec is registered with `required_permission: PermissionMode::DangerFullAccess` (same as the `bash` tool in `mvp_tool_specs`), not with per-command workspace-awareness. Bash shell and PowerShell execute arbitrary commands, so blanket promotion to `danger-full-access` is conservative — but it over-escalates read-only in-workspace operations. Fix shape: (a) add command-level heuristic analysis to the PowerShell executor (read-only commands like `Get-Content`, `Get-ChildItem`, `Test-Path` that target paths inside CWD → `WorkspaceWrite` required; everything else → `DangerFullAccess`); (b) mirror the same workspace-path check that the bash executor uses; (c) add tests covering the permission boundary for PowerShell read vs write vs network commands. Note: the `bash` tool in `mvp_tool_specs` is also `DangerFullAccess` and has the same gap — both should be fixed together. Source: tanishq_devil in #claw-code 2026-04-10; root cause identified by gaebal-gajae.

51. **Windows first-run onboarding missing: no explicit Rust + shell prerequisite branch** — dogfooded 2026-04-10 via #claw-code. User hit `bash: cargo: command not found`, `C:\...` vs `/c/...` path confusion in Git Bash, and misread `MINGW64` prompt as a broken MinGW install rather than normal Git Bash. Root cause: README/docs have no Windows-specific install path that says (1) install Rust first via rustup, (2) open Git Bash or WSL (not PowerShell or cmd), (3) use `/c/Users/...` style paths in bash, (4) then `cargo install claw-code`. Users can reach chat mode confusion before realizing claw was never installed. Fix shape: add a **Windows setup** section to README.md (or INSTALL.md) with explicit prerequisite steps, Git Bash vs WSL guidance, and a note that `MINGW64` in the prompt is expected and normal. Source: tanishq_devil in #claw-code 2026-04-10; traced by gaebal-gajae.

52. **`cargo install claw-code` false-positive install: deprecated stub silently succeeds** — dogfooded 2026-04-10 via #claw-code. User runs `cargo install claw-code`, install succeeds, Cargo places `claw-code-deprecated.exe`, user runs `claw` and gets `command not found`. The deprecated binary only prints `"claw-code has been renamed to agent-code"`. The success signal is false-positive: install appears to work but leaves the user with no working `claw` binary. Fix shape: (a) README must warn explicitly against `cargo install claw-code` with the hyphen (current note only warns about `clawcode` without hyphen); (b) if the deprecated crate is in our control, update its binary to print a clearer redirect message including `cargo install agent-code`; (c) ensure the Windows setup doc path mentions `agent-code` explicitly. Source: user in #claw-code 2026-04-10; traced by gaebal-gajae.

53. **`cargo install agent-code` produces `agent.exe`, not `agent-code.exe` — binary name mismatch in docs** — dogfooded 2026-04-10 via #claw-code. User follows the `claw-code` rename hint to run `cargo install agent-code`, install succeeds, but the installed binary is `agent.exe` (Unix: `agent`), not `agent-code` or `agent-code.exe`. User tries `agent-code --version`, gets `command not found`, concludes install is broken. The package name (`agent-code`), the crate name, and the installed binary name (`agent`) are all different. Fix shape: docs must show the full chain explicitly: `cargo install agent-code` → run via `agent` (Unix) / `agent.exe` (Windows). ROADMAP #52 note updated with corrected binary name. Source: user in #claw-code 2026-04-10; traced by gaebal-gajae.

54. **Circular "Did you mean /X?" error for spec-registered commands with no parse arm** — dogfooded 2026-04-10. 23 commands in the spec (shown in `/help` output) had no parse arm in `validate_slash_command_input`, so typing them produced `"Unknown slash command: /X — Did you mean /X?"`. The "Did you mean" suggestion pointed at the exact command the user just typed. Root cause: spec registration and parse-arm implementation were independent — a command could appear in help and completions without being parseable. **Done at `1e14d59` 2026-04-10**: added all 23 to STUB_COMMANDS and added pre-parse intercept in resume dispatch. Source: Jobdori dogfood.

55. **`/session list` unsupported in resume mode despite only needing directory read** — dogfooded 2026-04-10. `/session list` in `--output-format json --resume` mode returned `"unsupported resumed slash command"`. The command only reads the sessions directory — no live runtime needed. **Done at `8dcf103` 2026-04-10**: added `Session{action:"list"}` arm in `run_resume_command()`. Emits `{kind:session_list, sessions:[...ids], active:<id>}`. Partial progress on ROADMAP #21. Source: Jobdori dogfood.

56. **`--resume` with no command ignores `--output-format json`** — dogfooded 2026-04-10. `claw --output-format json --resume <session>` (no slash command) printed prose `"Restored session from <path> (N messages)."` to stdout, ignoring the JSON output format flag. **Done at `4f670e5` 2026-04-10**: empty-commands path now emits `{kind:restored, session_id, path, message_count}` in JSON mode. Source: Jobdori dogfood.

57. **Session load errors bypass `--output-format json` — prose error on corrupt JSONL** — dogfooded 2026-04-10. `claw --output-format json --resume <corrupt.jsonl> /status` printed bare prose `"failed to restore session: ..."` to stderr, not a JSON error object. Both the path-resolution and JSONL-load error paths ignored `output_format`. **Done at `cf129c8` 2026-04-10**: both paths now emit `{type:error, error:"failed to restore session: <detail>"}` in JSON mode. Source: Jobdori dogfood.

58. **Windows startup crash: `HOME is not set`** — user report 2026-04-10 in #claw-code (MaxDerVerpeilte). On Windows, `HOME` is often unset — `USERPROFILE` is the native equivalent. Four code paths only checked `HOME`: `config_home_dir()` (tools), `credentials_home_dir()` (runtime/oauth), `detect_broad_cwd()` (CLI), and skill lookup roots (tools). All crashed or silently skipped on stock Windows installs. **Done at `b95d330` 2026-04-10**: all four paths now fall back to `USERPROFILE` when `HOME` is absent. Error message updated to suggest `USERPROFILE` or `CLAW_CONFIG_HOME`. Source: MaxDerVerpeilte in #claw-code.

59. **Session metadata does not persist the model used** — dogfooded 2026-04-10. When resuming a session, `/status` reports `model: null` because the session JSONL stores no model field. A claw resuming a session cannot tell what model was originally used. The model is only known at runtime construction time via CLI flag or config. **Done at `0f34c66` 2026-04-10**: added `model: Option<String>` to Session struct, persisted in session_meta JSONL record, surfaced in resumed `/status`. Source: Jobdori dogfood.

60. **`glob_search` silently returns 0 results for brace expansion patterns** — user report 2026-04-10 in #claw-code (zero, Windows/Unity). Patterns like `Assets/**/*.{cs,uxml,uss}` returned 0 files because the `glob` crate (v0.3) does not support shell-style brace groups. The agent fell back to shell tools as a workaround. **Done at `3a6c9a5` 2026-04-10**: added `expand_braces()` pre-processor that expands brace groups before passing to `glob::glob()`. Handles nested braces. Results deduplicated via `HashSet`. 5 regression tests. Source: zero in #claw-code; traced by gaebal-gajae.

61. **`OPENAI_BASE_URL` ignored when model name has no recognized prefix** — user report 2026-04-10 in #claw-code (MaxDerVerpeilte, Ollama). User set `OPENAI_BASE_URL=http://127.0.0.1:11434/v1` with model `qwen2.5-coder:7b` but claw asked for Anthropic credentials. `detect_provider_kind()` checks model prefix first, then falls through to env-var presence — but `OPENAI_BASE_URL` was not in the cascade, so unrecognized model names always hit the Anthropic default. **Done at `1ecdb10` 2026-04-10**: `OPENAI_BASE_URL` + `OPENAI_API_KEY` now beats Anthropic env-check. `OPENAI_BASE_URL` alone (no key, e.g. Ollama) is last-resort before Anthropic default. Source: MaxDerVerpeilte in #claw-code; traced by gaebal-gajae.

62. **Worker state file surface not implemented** — dogfooded 2026-04-10. ROADMAP "Deployment Architecture Gap" section documents `.claw/worker-state.json` as the "canonical observability contract" for clawhip, but `emit_state_file()` is not wired in `WorkerRegistry::transition()`. The file is never written. **Status:** documentation exists, code does not. Fix shape: wire `emit_state_file()` to write `.claw/worker-state.json` atomically on every `WorkerStatus` transition; add `claw state` CLI subcommand to read/display it. Blocker: none. Source: Jobdori dogfood.

63. **Droid session completion semantics broken: code arrives after "status: completed"** — dogfooded 2026-04-12. Ultraclaw droid sessions (use-droid via acpx) report `session.status: completed` before file writes are fully flushed/synced to the working tree. Discovered +410 lines of "late-arriving" droid output that appeared after I had already assessed 8 sessions as "no code produced." This creates false-negative assessments and duplicate work. **Fix shape:** (a) droid agent should only report completion after explicit file-write confirmation (fsync or existence check); (b) or, claw-code should expose a `pending_writes` status that indicates "agent responded, disk flush pending"; (c) lane orchestrators should poll for file changes for N seconds after completion before final assessment. **Blocker:** none. Source: Jobdori ultraclaw dogfood 2026-04-12.
