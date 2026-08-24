# Roadmap PR goal intake

Captured: 2026-05-14 (Asia/Seoul) during the Claw Code 2.0 Ultragoal run.

Purpose: make the user's follow-up requirement durable: all roadmap PRs should be merged when correct/resolvable, and unresolved roadmap deltas should become Ultragoal work rather than being lost. This file is a tracked companion to the leader-owned `.omx/ultragoal/goals.json` and `.omx/ultragoal/ledger.jsonl` artifacts.

## Merge policy

- Merge only PRs that are still relevant to Claw Code 2.0, are non-draft, target `main`, and are conflict-free after a fresh mergeability refresh.
- Prefer squash merges with a Lore-style body when GitHub allows a direct PR merge.
- If a PR is documentation-only but adds a real roadmap gap, merging it is acceptable once checks/conflicts are clean.
- If a PR is stale, duplicated by already-landed work, or not product-aligned, do not force-merge; record the rationale and map any still-correct requirement into G011/G012.
- After merging roadmap PRs, refresh generated board artifacts (`.omx/cc2/board.json`, `.omx/cc2/board.md`) so Stream 0 coverage stays current.

## Open roadmap PRs with green historical checks

These are first-pass merge candidates, pending fresh mergeability and conflict checks against current `main`.

| PR | Title | Branch | Checks | Mergeable | URL |
| --- | --- | --- | --- | --- | --- |
| #2848 | docs(roadmap): add #333 — no in-session settings inspect command | `docs/roadmap-333-no-settings-inspect-command` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2848 |
| #2846 | docs(roadmap): add #331 — export silently overwrites on repeated invocations | `docs/roadmap-331-export-filename-collision` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2846 |
| #2869 | docs(roadmap): add #358 — history entries missing role field, no pagination | `docs/roadmap-348-history-entries-missing-role` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2869 |
| #2850 | docs(roadmap): add #335 — session list omits created_at_ms field | `docs/roadmap-335-session-list-no-created-at` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2850 |
| #2868 | docs(roadmap): add #356 — session list title always null; no rename command | `docs/roadmap-347-session-list-title-always-null` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2868 |
| #2865 | docs(roadmap): add #362 — doctor auth false-positive: misses CLI session tokens | `docs/roadmap-345-doctor-auth-check-incomplete` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2865 |
| #2864 | docs(roadmap): add #364 — /cost returns no cost_usd; identical to /stats | `docs/roadmap-344-cost-command-no-dollar-amount` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2864 |
| #2867 | docs(roadmap): add #368 — export always appends .txt; response.file reflects mangled path | `docs/roadmap-346-export-forces-txt-extension` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2867 |
| #2862 | docs(roadmap): add #342 — status json omits active session ID, workspace counters ambiguous | `docs/roadmap-342-v2` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2862 |
| #2876 | docs(roadmap): add #354 — /cwd suggests itself in did-you-mean; self-referential loop | `docs/roadmap-354-cwd-self-referential-suggestion` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2876 |
| #2872 | docs(roadmap): add #360 — /tokens, /stats, /cost identical output; no context-window or cost_usd | `docs/roadmap-349-tokens-stats-cost-identical` -> `main` | 4/4 checks successful | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2872 |

## Open roadmap PRs needing local validation or CI refresh

These have no check rollup in the live snapshot; validate locally or refresh CI before merging.

| PR | Title | Branch | Checks | Mergeable | URL |
| --- | --- | --- | --- | --- | --- |
| #2858 | docs(roadmap): add #343 — session subcommand resume-safety inconsistently enforced | `docs/roadmap-340-session-resume-safe-inconsistent` -> `main` | no checks reported | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2858 |
| #2839 | docs(roadmap): add #330 — resume mode stats/cost always zero | `docs/roadmap-324-resume-stats-zero` -> `main` | no checks reported | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2839 |
| #2841 | docs(roadmap): add #332 — doctor json missing top-level status field | `docs/roadmap-325-doctor-no-status-field` -> `main` | no checks reported | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2841 |
| #2844 | docs(roadmap): add #336 — session subcommand resume inconsistency and type/kind error mismatch | `docs/roadmap-329-session-subcommand-resume-inconsistency` -> `main` | no checks reported | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2844 |
| #2842 | docs(roadmap): add #334 — version json omits build_date and uses short sha only | `docs/roadmap-328-version-json-incomplete` -> `main` | no checks reported | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2842 |

## Product-fit review before merge

These may be broader than the Claw Code 2.0 roadmap scope and need a product-fit decision before merge.

| PR | Title | Branch | Checks | Mergeable | URL |
| --- | --- | --- | --- | --- | --- |
| #2824 | docs: personal assistant roadmap | `pr/docs-personal-assistant-roadmap` -> `main` | no checks reported | UNKNOWN | https://github.com/ultraworkers/claw-code/pull/2824 |

## Ultragoal mapping

- G003-G010: close implementation gaps that overlap a roadmap PR title if the requirement belongs to the active stream.
- G011: reconcile ecosystem/ops/UX roadmap PRs and unresolved correct issues that do not fit earlier streams.
- G012: final release gate must prove that every open roadmap PR was merged, closed as duplicate/obsolete, or converted into an explicit remaining goal with evidence.

