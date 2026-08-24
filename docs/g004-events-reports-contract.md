# G004 event and report contract guidance

Captured: 2026-05-14 during the Stream 2 `G004-events-reports` team run.

Purpose: keep the user/developer-facing contract guidance for ROADMAP Phase 2 in one tracked source that points back to the code and roadmap anchors. This document is intentionally not the implementation map for task 5; it describes the interoperability contract consumers should rely on as the lane-event, report-schema, approval-token, and capability-negotiation lanes land.

## Source-of-truth anchors

| Contract family | Roadmap anchor | Current implementation / owner-facing anchor | Consumer guidance |
| --- | --- | --- | --- |
| Canonical lane events | `ROADMAP.md` Phase 2 §4, §4.5, §4.6, §4.7 | `rust/crates/runtime/src/lane_events.rs` (`LaneEventName`, `LaneEventStatus`, `LaneEventMetadata`, terminal reconciliation helpers) | Consume `event`, `status`, `emittedAt`, and `metadata` fields as the canonical state stream; do not infer lane state from terminal text when a structured event is present. |
| Report schema v1 and projections | `ROADMAP.md` §4.25-§4.34 | Stream 2 report-schema lane / fixtures as they land | Treat a report as a versioned canonical payload plus derived projections. A projection may omit or transform fields only with explicit provenance: compatibility downgrade, redaction policy, truncation, or source absence. |
| Policy-blocked handoff and approval-token chain | `ROADMAP.md` §4.37-§4.39 | Stream 2 approval-token lane as it lands | Treat policy blocks and owner approvals as typed artifacts, not prose. Execute an exception only when the approval token matches actor, policy, action, repo/branch/commit scope, expiry, and one-time-use state. |
| Capability negotiation | `ROADMAP.md` §4.25, §4.26, §4.32, §4.34 | Report-schema/projection fixtures and consumer conformance cases as they land | Consumers must advertise supported schema versions, optional field families, projection views, redaction semantics, and downgrade handling before relying on reduced payloads. |

## Lane event contract

The lane-event stream is the first machine-trustworthy surface for Stream 2. Consumers should expect these invariants when reading `LaneEvent` payloads:

- `event` is a typed event name, currently including the core lane lifecycle (`lane.started`, `lane.ready`, `lane.blocked`, `lane.red`, `lane.green`, `lane.finished`, `lane.failed`), branch health (`branch.stale_against_main`, `branch.workspace_mismatch`), reconciliation (`lane.reconciled`, `lane.superseded`, `lane.closed`), and ship provenance (`ship.prepared`, `ship.commits_selected`, `ship.merged`, `ship.pushed_main`).
- `status` is the normalized state for the event; consumers should prefer it over freeform `detail` text for automation.
- `metadata.seq`, `metadata.timestamp_ms`, and terminal fingerprints are the ordering/deduplication hooks. Consumers should use terminal reconciliation output rather than double-reporting contradictory terminal bursts.
- `metadata.provenance`, `metadata.environment_label`, `metadata.emitter_identity`, and `metadata.confidence_level` tell consumers whether an event is live lane truth, test traffic, healthcheck/replay output, or transport-layer evidence.
- `metadata.session_identity` and `metadata.ownership` bind a lane event to the session, workspace, workflow scope, owner, and watcher action. A watcher should not act on events whose ownership says `observe` or `ignore`.

Minimal consumer rule: if a structured event exists, pane text is supporting evidence only. Pane scraping must not override a higher-confidence typed event with matching session/workflow ownership.

## Report schema v1 contract

A Stream 2 report should be treated as a canonical fact record with optional projections. Consumers should preserve these semantics even when they receive only a downgraded view:

- Every report payload declares a schema version and a stable report identity/content hash for the full-fidelity canonical payload.
- Assertions are labeled as `fact`, `hypothesis`, or another declared evidence class, with confidence and source references. Negative evidence is first-class: `not observed`, `checked and absent`, and `redacted` are distinct states.
- Field deltas name the field, previous value/state, new value/state, attribution, and whether the delta came from source content, projection, downgrade, or redaction policy.
- Projections carry lineage back to the canonical report id/content hash and name the projection view, capability set, schema version, redaction policy, and deterministic rendering inputs.
- Redaction provenance is explicit. A missing field without a redaction/downgrade/source-absence reason is not enough evidence for an automated consumer to conclude the underlying fact is absent.

Minimal consumer rule: store the canonical identity and projection metadata together. Do not compare two projections as state changes unless their canonical content hash or declared projection inputs differ.

## Approval-token and policy-blocked contract

Policy-blocked actions and owner-approved exceptions belong in the same structured event/report family:

- A policy block names the typed reason, policy source, actor scope, blocked action, and safe fallback path.
- An approval token names the approving actor, policy exception, action, repository/worktree/branch/commit scope, expiry, and allowed use count.
- Token consumption records the exact action and scope that spent the token. Replays, scope expansion, expired tokens, and revoked tokens should surface typed policy errors.
- Delegation traceability stays attached when another worker/lane executes the approved action; the executor must be able to prove which approval artifact authorized the exception.

Minimal consumer rule: prose such as "approved" is not an executable approval. Require the structured token and verify that it is unconsumed and scoped to the exact action before proceeding.

## Capability negotiation and conformance

Mixed-version consumers are expected during Stream 2 rollout. Producers and consumers should negotiate instead of silently dropping fields:

- Consumers advertise supported report schema versions, field families, projection views, redaction states, downgrade semantics, and fixture/conformance suite version.
- Producers preserve one canonical full-fidelity report and emit downgraded projections only with `downgraded_for_compatibility` metadata.
- Deterministic projection inputs include schema version, consumer capability set, projection policy version, redaction policy version, and canonical content hash.
- Consumer conformance should distinguish syntax acceptance from semantic correctness, especially for `redacted` vs `missing`, stale vs current projections, negative evidence, and approval-token replay states.

Minimal consumer rule: an older consumer may accept a downgraded projection, but it must surface the downgrade as a capability limitation rather than treating omitted fields as canonical absence.

## Documentation maintenance rules

- Keep ROADMAP Phase 2 as the product requirement source and this file as the contract-reading guide.
- Keep Rust type names and event names aligned with `rust/crates/runtime/src/lane_events.rs`; update this document in the same change when public event names or metadata semantics change.
- Keep report-schema examples/fixtures aligned with this guide once the schema lane lands; fixture updates should explain intentional schema or projection changes.
- Do not mutate `.omx/ultragoal` from worker lanes. Leader-owned Ultragoal checkpointing consumes commits and verification evidence from task results.
