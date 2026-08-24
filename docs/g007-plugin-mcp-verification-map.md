# G007 Plugin/MCP Lifecycle Verification Map

Goal: `G007-plugin-mcp` — Stream 5 plugin/MCP lifecycle maturity from ROADMAP Phase 5.

Scope: worker-2 follow-up map for W4 mock integration and regression verification. This file intentionally does not mutate leader-owned `.omx/ultragoal` state.

## Covered ROADMAP / CC2 anchors

- `ROADMAP.md:55-57` — Current pain point §6: plugin/MCP startup failures, handshake failures, config errors, partial startup, and degraded mode need clean classification.
- `ROADMAP.md:67` — Product principle §5: MCP partial success must be first-class and structurally report successful and failed servers.
- `ROADMAP.md:1033-1059` — Phase 5: first-class plugin/MCP lifecycle contract and MCP end-to-end lifecycle parity.
- `.omx/cc2/board.md` Stream 5 active headings: `CC2-RM-H0010`, `CC2-RM-H0080`, `CC2-RM-H0081`, and `CC2-RM-H0082` remain the goal-level source-of-truth anchors for plugin/MCP lifecycle maturity.
- `PARITY.md` harness checklist: mock parity scenarios are the executable regression surface for streamed model turns, plugin tool roundtrips, permissions, compaction metadata, and token/cost output.

## Mock integration anchors

| Area | Artifact/evidence |
| --- | --- |
| Deterministic model server | `rust/crates/mock-anthropic-service/src/lib.rs` implements the Anthropic-compatible mock server and scenario router used by CLI parity tests. |
| End-to-end CLI mock harness | `rust/crates/rusty-claude-cli/tests/mock_parity_harness.rs` starts the mock server, runs clean-environment `claw` commands, asserts JSON output, and optionally writes a machine-readable report via `MOCK_PARITY_REPORT_PATH`. |
| Scenario manifest / docs parity guard | `rust/mock_parity_scenarios.json` is required to stay ordered with harness cases; `rust/scripts/run_mock_parity_diff.py --no-run` verifies every manifest `parity_refs[]` string exists in `PARITY.md`. |
| Convenience runner | `rust/scripts/run_mock_parity_harness.sh` runs `cargo test -p rusty-claude-cli --test mock_parity_harness -- --nocapture`. |
| Plugin-path regression | `plugin_tool_roundtrip` loads an external plugin fixture from isolated settings and executes `plugin_echo` through the runtime tool registry. |
| Lifecycle-adjacent regression | `auto_compact_triggered` and `token_cost_reporting` prove runtime JSON keeps compaction and usage/cost fields parseable under mock responses, preventing parity drift in machine-readable output. |
| MCP degraded-startup regression | `rust/crates/runtime/src/mcp_stdio.rs::manager_discovery_report_keeps_healthy_servers_when_one_server_fails` proves a healthy MCP server remains callable while a broken peer is surfaced in a structured degraded report. |
| Plugin lifecycle state regression | `rust/crates/runtime/src/plugin_lifecycle.rs` unit tests cover healthy, degraded, failed, and shutdown states plus startup-event mapping. |

## Regression verification commands

Use the smallest command that proves the changed or audited surface, then broaden only when integration risk requires it.

- Mock scenario/docs map only:
  - `cd rust && python3 scripts/run_mock_parity_diff.py --no-run`
- Full mock integration:
  - `cd rust && cargo test -p rusty-claude-cli --test mock_parity_harness -- --nocapture`
  - `cd rust && python3 scripts/run_mock_parity_diff.py`
- Plugin/MCP lifecycle contract:
  - `cd rust && cargo test -p runtime plugin_lifecycle -- --nocapture`
  - `cd rust && cargo test -p runtime mcp_stdio::tests::manager_discovery_report_keeps_healthy_servers_when_one_server_fails -- --exact --nocapture`
- Standard Rust gates for implementation changes touching these surfaces:
  - `cd rust && cargo fmt --all -- --check`
  - `cd rust && cargo check -p runtime -p rusty-claude-cli -p mock-anthropic-service`
  - `cd rust && cargo clippy -p runtime --all-targets -- -D warnings`

## Known gaps / follow-ups

- The mock parity harness validates plugin tool execution but does not yet spin up a real MCP stdio server through the CLI prompt path; MCP degraded-startup remains covered by runtime manager tests.
- Worker-4 owns the plugin command fallthrough regression implementation lane (`task-10`); this map records the verification/docs boundary and should not duplicate that parser work.
- Full `cargo clippy -p runtime --all-targets -- -D warnings` can be blocked by unrelated `policy_engine.rs` clippy violations in this worktree; when that happens, report the exact pre-existing diagnostics and keep focused lifecycle tests green.
- No `.omx/ultragoal` files were changed; leader-owned Ultragoal checkpointing remains outside worker scope.

## Delegation evidence

Subagent spawn evidence: Task 9 spawned repository map probe `019e291d-e700-7171-b7bc-27ec0f6c850f`, debug/root-cause probe `019e291d-e86f-78d0-a137-214ede03285c`, and test/docs probe `019e291e-135c-79e1-80d0-9fd82866bd6e` before deeper local inspection. The repository-map probe errored with 429; the remaining probes did not return before the local verification map was grounded from repo evidence, so direct findings above were integrated.
