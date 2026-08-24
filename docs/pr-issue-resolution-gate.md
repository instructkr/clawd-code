# Claw Code 2.0 PR and Issue Resolution Gate

This gate was added to the Claw Code 2.0 Ultragoal after the explicit requirement:

> all PRs should be merged and all issues should be resolved if resolvable and correct.

## Scope

Before the Claw Code 2.0 Ultragoal can be marked complete:

1. Every open GitHub PR at the current final-gate snapshot must be triaged.
2. PRs that are correct, compatible with Claw Code 2.0 direction, and pass required verification must be merged.
3. PRs that are stale, incorrect, duplicative, unsafe, spam, or outside Claw Code scope must not be merged; each needs a recorded rationale.
4. Every open GitHub issue at the current final-gate snapshot must be triaged.
5. Issues that are resolvable and correct must be fixed or explicitly linked to a merged fix.
6. Issues that are spam, duplicates, incorrect, unactionable, externally blocked, or not Claw Code work must be closed or labeled/commented with rationale when repository policy allows.
7. The final completion audit must use a fresh GitHub snapshot, not only the planning snapshot.

## Current live snapshot

A fresh non-destructive snapshot was captured locally during G011 W3 execution:

- Command: `gh pr list --state open --limit 1000 --json number,title,state,updatedAt,url`
- Command: `gh issue list --state open --limit 1000 --json number,title,state,updatedAt,url,labels`
- Captured on: 2026-05-15T02:39:41Z during the active Ultragoal run.
- Observed counts: 51 open PR records and 1000 open issue records from GitHub CLI list calls.
- Most recent open PR in the snapshot: #3040, `fix: recognize OPENAI_API_KEY as valid auth for OpenAI-compatible endpoints`, updated 2026-05-14T11:35:23Z.
- Most recent open issue in the snapshot: #3039, `How to install skills?`, updated 2026-05-14T08:14:36Z.
- The issue snapshot hit the configured `--limit 1000`, so the final gate must treat the issue count as at least 1000 unless a higher-limit export or paginated ledger is captured.

These command outputs are evidence inputs, not final proof. The final gate must refresh them and compare deltas before any completion claim.

## Anti-slop triage templates

Use `docs/anti-slop-triage.md` plus the repository templates before acting on the live snapshot:

- `.github/ISSUE_TEMPLATE/anti_slop_triage.yml` records the initial issue classification, evidence, and non-destructive next action.
- `.github/PULL_REQUEST_TEMPLATE.md` adds PR classification, verification, and resolution-gate checklist items.

The anti-slop classifications are: `actionable-bug`, `actionable-docs`, `actionable-feature`, `duplicate`, `spam-or-promotion`, `generated-slop-or-hallucinated`, `unsafe-or-security-sensitive`, `not-reproducible-yet`, and `externally-blocked`.

Automation lanes may recommend labels, comments, defer/close rationales, or merge candidates, but must not merge or close remote PRs/issues without maintainer-owned approval.


## G012 final PR reconciliation snapshot

Worker-3 captured a fresh PR ledger for the final Claw Code 2.0 gate in `docs/pr-triage-g012-final-gate.json`.

- Captured on: 2026-05-15T02:58:00Z during G012 final-gate execution.
- Commands: `gh pr list --state open --limit 100 ...` plus `gh pr view <number> ...` for per-PR file and merge-state evidence.
- Observed count: 51 open PR records.
- Merge action taken by worker-3: none. The safety policy requires correct, safe, non-conflicting, resolvable PRs with evidence; this snapshot found 32 PRs in `CONFLICTING`/`DIRTY` state and 19 `MERGEABLE` PRs that GitHub reported as `UNSTABLE` with no fresh check-rollup evidence in the live snapshot.
- Docs-only candidate-review PRs: #3021 and #2824 remain deferred until content/source-of-truth review and fresh verification are available.

## Required final evidence

The final report must include:

- Fresh `gh pr list --state open` and `gh issue list --state open` snapshots.
- A PR ledger with one row per PR: merge / reject / defer, reason, verification, commit/merge reference.
- An issue ledger with one row per issue: fixed / duplicate / spam / invalid / deferred-with-rationale / externally-blocked, reason, and linked evidence.
- Verification that no correct, mergeable PR remains unmerged without rationale.
- Verification that no resolvable, correct issue remains open without a fix or rationale.

## Non-goals

This gate does not require merging unsafe, unverified, incompatible, spam, or incorrect contributions. It requires explicit evidence-backed triage and action for everything that is correct and resolvable.
