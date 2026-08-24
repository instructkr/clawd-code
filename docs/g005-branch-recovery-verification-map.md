# G005 Branch Recovery Verification Map

Scope: worker-1 follow-up map for G005 branch/test awareness and recovery. This file intentionally does not mutate leader-owned `.omx/ultragoal` state.

## Covered ROADMAP / PRD pinpoints

- `ROADMAP.md:912-921` — Phase 3 §7 stale-branch detection before broad verification: broad workspace test commands are preflighted before execution, stale/diverged branches emit `branch.stale_against_main`, and targeted tests bypass the broad-test gate.
- `ROADMAP.md:922-933` — Phase 3 §8 recovery recipes: stale-branch recovery remains represented by the `stale_branch` recipe, with one automatic attempt before escalation.
- `ROADMAP.md:935-949` — Phase 3 §8.5 recovery attempt ledger: `RecoveryContext` now exposes ledger entries with recipe id, attempt count, state, started/finished markers, last failure summary, and escalation reason.
- `ROADMAP.md:951-970` — Phase 3 §9 green-ness / hung-test reporting: timed-out test commands now classify as `test.hung` with structured provenance instead of generic timeout.
- `prd.json:37-44` — US-003 stale-branch detection before broad verification: verified through the `workspace_test_branch_preflight` broad-test block and targeted-test bypass tests.
- `prd.json:50-57` — US-004 recovery recipes with ledger: verified through recovery ledger unit coverage and serialization-compatible recovery structs.

## Implementation anchors

- `rust/crates/runtime/src/stale_branch.rs` — existing branch freshness model and policy actions for fresh, stale, and diverged branches.
- `rust/crates/tools/src/lib.rs` — `workspace_test_branch_preflight`, `branch_divergence_output`, Bash/PowerShell broad-test gating, and `test.hung` structured timeout provenance on tool-shell timeouts.
- `rust/crates/runtime/src/recovery_recipes.rs` — recovery recipes plus `RecoveryLedgerEntry` / `RecoveryAttemptState` ledger surface.
- `rust/crates/runtime/src/bash.rs` — runtime Bash timeout classification and structured provenance for hung test commands.
- `rust/crates/runtime/src/lib.rs` — public exports for the recovery ledger types.

## Verification evidence

- `cargo test -p runtime` → PASS: 538 unit tests, 2 G004 conformance tests, 12 integration tests, and doctests passed.
- `cargo test -p tools bash_tool_classifies_test_timeout_as_hung_with_provenance -- --nocapture` → PASS.
- `cargo test -p tools bash_workspace_tests_are_blocked_when_branch_is_behind_main -- --nocapture` → PASS.
- `cargo test -p tools bash_targeted_tests_skip_branch_preflight -- --nocapture` → PASS.
- `cargo check -p runtime -p tools` → PASS.
- `cargo clippy -p runtime --all-targets -- -D warnings` → PASS.
- `cargo clippy -p tools --lib --no-deps -- -D warnings` → PASS.

## Known unresolved / out-of-scope items

- Full `cargo test -p tools` is still red on six permission-enforcer expectation tests unrelated to G005 branch freshness, recovery ledger, or hung-test classification. The failing tests assert old permission wording/read-only behavior and pre-existed this follow-up scope.
- ROADMAP stale-base JSON/doctor/status pinpoints remain broader CLI diagnostic-surface work, especially `ROADMAP.md:2425-2489`, `ROADMAP.md:4346-4431`, and `ROADMAP.md:5061-5086`. They are related to branch freshness, but task 1 only required the broad-test freshness gate and narrow reporting surfaces.
- No `.omx/ultragoal` files were changed; leader-owned Ultragoal checkpointing remains outside worker scope.

## Delegation evidence

Subagent spawn evidence: 1, Repository map probe `019e25d5-9be9-7193-8a33-f21450beb62c`; spawned before further serial task-2 mapping per contract, but errored with 429 Too Many Requests, so direct repo evidence was integrated instead.
