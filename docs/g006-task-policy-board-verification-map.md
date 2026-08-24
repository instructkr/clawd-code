# G006 Task Policy Board Verification Map

Goal: `G006-task-policy-board` — Stream 4 task packets, executable policy engine, lane board/status JSON, and running-state liveness heartbeat.

## Prompt-to-artifact checklist

| Requirement | Artifact/evidence |
| --- | --- |
| Typed task packet schema with objective, scope, files/resources, acceptance criteria, model/provider, permission profile, recovery policy, verification plan, reporting targets | `rust/crates/runtime/src/task_packet.rs` extends `TaskPacket` with `acceptance_criteria`, `resources`, `model`, `provider`, `permission_profile`, `recovery_policy`, `verification_plan`, and `reporting_targets`; tests cover legacy defaulted JSON and rich CC2 roundtrip. |
| Backwards compatibility for existing task packets and tool callers | `serde(default)`/optional fields in `task_packet.rs`; `rust/crates/tools/src/lib.rs` `run_task_packet_creates_packet_backed_task` updated for rich schema; legacy packet test keeps old JSON accepted. |
| Executable policy decisions for retry/rebase/merge/escalate/stale cleanup/approval token | `rust/crates/runtime/src/policy_engine.rs` adds `RetryAvailable`, `RebaseRequired`, `StaleCleanupRequired`, approval-token conditions/actions, `PolicyEvaluation`, `PolicyDecisionEvent`, and decision-table tests. |
| Policy decisions explainable and typed-event logged/emittable | `PolicyDecisionEvent` serializable typed event with `rule_name`, `priority`, `kind`, `explanation`, `approval_token_id`; `evaluate_with_events` emits event per flattened action. |
| Active lane board/dashboard/status JSON over canonical state | `rust/crates/runtime/src/task_registry.rs` adds `LaneBoard`, `LaneBoardEntry`, `LaneFreshness`, `lane_board_at`, and `lane_status_json_at`; CLI status JSON advertises lane board contract in `rust/crates/rusty-claude-cli/src/main.rs`. |
| Heartbeats independent of terminal rendering with healthy/stalled/transport-dead cases | `rust/crates/runtime/src/session.rs` adds `SessionHeartbeat`/`SessionLiveness` from persisted session health state; `task_registry.rs` heartbeat freshness is computed from canonical heartbeat timestamps and transport state. |
| Task/lane status JSON shows active/blocked/finished lanes with heartbeat freshness | `task_registry::tests::lane_board_groups_active_blocked_finished_and_reports_freshness`; `status_json_surfaces_session_lifecycle_for_clawhip`/status JSON surfaces lane board metadata. |
| Leader-owned ultragoal audit remains separate from workers | No worker changed `.omx/ultragoal`; leader will checkpoint with fresh `get_goal` only after terminal verification. |

## Verification run

- `git diff --check` — PASS
- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` — PASS
- `cargo check --manifest-path rust/Cargo.toml -p runtime -p tools -p rusty-claude-cli` — PASS
- `cargo test --manifest-path rust/Cargo.toml -p runtime task_packet -- --nocapture` — PASS (5 task packet tests)
- `cargo test --manifest-path rust/Cargo.toml -p runtime policy_engine -- --nocapture` — PASS (12 unit + 1 integration match)
- `cargo test --manifest-path rust/Cargo.toml -p runtime task_registry -- --nocapture` — PASS (17 task registry tests)
- `cargo test --manifest-path rust/Cargo.toml -p runtime session_heartbeat -- --nocapture` — PASS (1 heartbeat test)
- `cargo test --manifest-path rust/Cargo.toml -p tools run_task_packet_creates_packet_backed_task -- --nocapture` — PASS
- `cargo test --manifest-path rust/Cargo.toml -p tools lane_completion -- --nocapture` — PASS (6 tests)
- `cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli status_json_surfaces -- --nocapture` — PASS

## Remaining gates

- G006 can be checkpointed after team lifecycle is reconciled terminal and this commit is pushed.
- Open PR/issue reconciliation remains explicitly deferred to G011/G012 via `docs/pr-issue-resolution-gate.md`.
