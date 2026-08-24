# G004 events/reports verification map

Scope source: OMX team `g004-events-reports-u-e61d2271`, worker-1 tasks 1, 2, 4, 5. Workers must not mutate `.omx/ultragoal`; leader owns aggregate checkpoints.

## Ownership boundaries

- **Lane events / event identity / terminal reconciliation** — `rust/crates/runtime/src/lane_events.rs`, exported through `rust/crates/runtime/src/lib.rs`; tool-manifest consumers in `rust/crates/tools/src/lib.rs` write `LaneEvent` vectors.
- **Report schema v1 / projection / redaction / capability negotiation** — `rust/crates/runtime/src/report_schema.rs`, exported through `rust/crates/runtime/src/lib.rs`; fixture note at `rust/crates/runtime/tests/fixtures/report_schema_v1/README.md`.
- **Approval-token chain** — ROADMAP §§4.38-4.40; owned by worker-2 for this team split. Worker-1 did not edit it.
- **Pinpoint closure batch** — runtime hygiene across compact/search-parser/policy/sandbox/integration-test surfaces: `rust/crates/runtime/src/compact.rs`, `rust/crates/runtime/src/file_ops.rs`, `rust/crates/runtime/src/policy_engine.rs`, `rust/crates/runtime/src/sandbox.rs`, `rust/crates/runtime/tests/integration_tests.rs`.
- **Regression harness / docs alignment** — worker-3/worker-4 lanes per leader split. Coordinate before editing shared docs/tests.

## Relevant symbols and files

- `LaneEventName`, `LaneEventStatus`, `LaneEventMetadata`, `LaneEventBuilder`, `compute_event_fingerprint`, `dedupe_terminal_events`, `reconcile_terminal_events` in `runtime/src/lane_events.rs`.
- `CanonicalReportV1`, `ReportClaim`, `NegativeEvidence`, `FieldDelta`, `ConsumerCapabilities`, `ReportProjectionV1`, `canonicalize_report`, `project_report`, `report_schema_v1_registry` in `runtime/src/report_schema.rs`.
- `AgentOutput.lane_events`, `persist_agent_terminal_state`, `write_agent_manifest`, `maybe_commit_provenance` in `tools/src/lib.rs`.
- Search/parser closure helpers: `summarize_messages` in `compact.rs`, `grep_search_impl` / `build_grep_content_output` in `file_ops.rs`.

## Completed worker-1 commits

- `f45f05e` / task 1 auto-checkpoint — terminal event fingerprints use stable SHA-256-derived canonical JSON, and production convenience terminal events attach/refresh fingerprints after payload changes.
- `3989fc0` — report schema v1 contract, deterministic projection/redaction provenance, capability negotiation, and fixture note.
- `7fff4c4` / task 4 auto-checkpoint — strict runtime clippy closure batch across compact/file_ops/policy/sandbox/integration tests.

## Current verification evidence

Run from `rust/` unless noted:

- `cargo test -p runtime lane_events -- --nocapture` — PASS, 46 lane-event tests.
- `cargo test -p runtime report_schema -- --nocapture` — PASS, 4 report-schema tests.
- `cargo check -p runtime` — PASS.
- `cargo clippy -p runtime --all-targets -- -D warnings` — PASS after task 4 closure batch.
- `cargo test -p runtime -- --nocapture` — PASS, 531 unit tests, 12 integration tests, doc-tests pass.
- `cargo test -p tools lane_event_schema_serializes_to_canonical_names -- --nocapture` — PASS, 1 targeted tools contract test.

## Leader integration verification plan

1. Inspect worker commits: `git log --oneline --decorate --max-count=8`.
2. Re-run focused contracts:
   - `cd rust && cargo test -p runtime lane_events -- --nocapture`
   - `cd rust && cargo test -p runtime report_schema -- --nocapture`
   - `cd rust && cargo test -p tools lane_event_schema_serializes_to_canonical_names -- --nocapture`
3. Re-run runtime quality gate:
   - `cd rust && cargo check -p runtime`
   - `cd rust && cargo clippy -p runtime --all-targets -- -D warnings`
   - `cd rust && cargo test -p runtime -- --nocapture`
4. If merging with worker-2 approval-token work, additionally run the worker-2 focused approval-token tests and check for export conflicts in `runtime/src/lib.rs`.
5. If merging with worker-3/4 docs or harness work, re-run their named regression harnesses plus `git diff --check`.

## Integration hazards

- `runtime/src/lib.rs` export blocks are shared; resolve conflicts by keeping both lane-event and report-schema exports sorted enough to remain readable.
- `tools/src/lib.rs` serializes lane events into agent manifests; terminal fingerprint changes intentionally affect `metadata.event_fingerprint` for finished/failed/superseded/merged/closed events with payloads.
- `report_schema.rs` currently defines the reusable contract and in-code deterministic fixtures; it does not yet wire report emission into CLI/status surfaces.
- ROADMAP approval-token §§4.38-4.40 remain a separate lane; do not treat worker-1 report schema as an approval artifact.
- Full workspace checks may include unrelated slow/provider-dependent tests; the verified local gate for this stream is runtime + targeted tools tests above.
