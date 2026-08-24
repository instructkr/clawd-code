# Anti-slop issue and PR triage

Use this checklist before spending engineering time on low-signal issues, generated PRs, duplicate fixes, or broad unsolicited changes. The goal is not to reject community work by default; it is to make each merge, defer, or close recommendation evidence-backed and safe.

## Classifications

| Classification | Use when | Required evidence | Safe action |
| --- | --- | --- | --- |
| `actionable-bug` | The report has a reproducible product failure. | Repro steps, failing test, logs with secrets removed, or matching roadmap item. | Fix, assign, or link to an existing fix. |
| `actionable-docs` | The report identifies missing, stale, or confusing documentation. | Current doc path plus desired corrected source of truth. | Patch docs or link to the owning docs lane. |
| `actionable-feature` | The request matches Claw Code direction and has a concrete acceptance shape. | Issue/PR link plus roadmap or maintainer rationale. | Defer to planning or implement if already scoped. |
| `duplicate` | Another issue/PR already covers the same user-visible outcome. | Link the canonical issue/PR and note any extra evidence worth preserving. | Cross-link; close only with maintainer/owner policy. |
| `spam-or-promotion` | The content is promotional, irrelevant, or abusive. | URL/title/body excerpt summary, not a full repost. | Label/close per repository policy. |
| `generated-slop-or-hallucinated` | The change is broad, mechanically generated, unreviewable, or names APIs/files that do not exist. | Diff/path examples, missing symbols, or unverifiable claims. | Request a narrow repro or reject/defer with rationale. |
| `unsafe-or-security-sensitive` | The report includes secrets, exploit detail, or risky operational instructions. | Redacted summary and security policy link. | Move to the private/security path; do not expand public details. |
| `not-reproducible-yet` | The claim might be valid but lacks enough evidence to act. | Missing command, environment, expected/actual behavior, or version. | Ask for repro details; do not implement speculative fixes. |
| `externally-blocked` | Progress depends on upstream services, credentials, policy, or unavailable owner approval. | Blocking dependency and owner/gate. | Defer with a concrete unblock condition. |

## PR review gate

Every PR triage note should answer:

1. Is the PR a merge candidate, a request-changes candidate, a duplicate, unsafe, out-of-scope, or generated slop?
2. What exact evidence supports that classification?
3. Which tests/docs checks were run or intentionally skipped?
4. Which issue, roadmap row, or user problem does it resolve?
5. If it should not merge now, what is the minimal non-destructive next action?

Automation lanes must not merge or close remote PRs/issues. They may produce a ledger row, add local documentation/templates, and report recommended actions for a maintainer-owned final gate.

## Issue intake gate

Every issue triage note should answer:

1. Is the issue correct, duplicate, spam, invalid, externally blocked, or not reproducible yet?
2. If correct and resolvable, what fix path or already-merged commit resolves it?
3. If not currently resolvable, what evidence would change the classification?
4. Are secrets, private data, or security details present that require a private path?

## Template locations

- Issue intake form: `.github/ISSUE_TEMPLATE/anti_slop_triage.yml`
- PR review checklist: `.github/PULL_REQUEST_TEMPLATE.md`
- Final aggregate gate: `docs/pr-issue-resolution-gate.md`
