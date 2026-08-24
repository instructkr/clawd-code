# G010 clone disambiguation metadata and verification map

Scope: worker-2 task 5 for `G010-session-hygiene` / Stream 9 session hygiene, local state, and recovery UX. This artifact maps the clone/worktree disambiguation contract and the focused verification surface without mutating leader-owned `.omx/ultragoal` state.

## Contract summary

Claw session state is intentionally scoped to the current workspace clone/worktree. Operators and automation should treat the **session partition**, not a bare session id or the flat `.claw/sessions/` directory, as the identity boundary.

Required metadata and behaviors:

- **Workspace-bound partition**: managed sessions live under `.claw/sessions/<workspace_fingerprint>/`, where the fingerprint is a stable 16-character FNV-1a digest of the canonical workspace path.
- **Canonical path input**: `SessionStore::from_cwd` and `SessionStore::from_data_dir` canonicalize their workspace path before computing the partition, preventing `/tmp/foo` vs `/private/tmp/foo` and relative-vs-absolute spelling from creating two stores for the same clone.
- **Clone/worktree isolation**: two distinct clones or worktrees must get different session partitions, even if session ids collide.
- **Legacy safety**: flat legacy sessions under `.claw/sessions/` remain readable only when they are bound to the same workspace or are unbound but physically inside the current workspace; sessions whose persisted `workspace_root` points at another clone are rejected as `WorkspaceMismatch`.
- **Fork lineage stays local**: `/session fork` / managed session forking keeps the forked session in the same workspace partition and records parent id plus optional branch name.
- **User-facing disambiguation**: empty-session copy names the actual fingerprint directory and explains that sessions from other CWDs are intentionally invisible.

## Implementation anchors

| Contract area | Repo anchor | Evidence role |
| --- | --- | --- |
| Partition layout and canonical workspace root | `rust/crates/runtime/src/session_control.rs:10-18`, `:32-47`, `:54-71` | Documents and implements `.claw/sessions/<workspace_hash>/` for `from_cwd` and explicit data-dir stores. |
| Fingerprint algorithm | `rust/crates/runtime/src/session_control.rs:300-312` | Defines the 16-character FNV-1a workspace fingerprint used as the clone disambiguator. |
| Managed create/resolve/list/load/fork APIs | `rust/crates/runtime/src/session_control.rs:86-204` | Ensures handles, `latest`, load, and fork resolve inside the active partition. |
| Legacy/cross-workspace guard | `rust/crates/runtime/src/session_control.rs:213-233`, `:557-567` | Rejects mismatched persisted `workspace_root` and allows only same-workspace legacy files. |
| Empty partition copy | `rust/crates/runtime/src/session_control.rs:535-543` | Reports `.claw/sessions/<fingerprint>/` plus the workspace-partition note. |
| CLI wrapper | `rust/crates/rusty-claude-cli/src/main.rs:5952-6040` | Routes session CLI helpers through `current_session_store()`, so CLI list/latest/load uses the same partition. |
| CLI session-list lifecycle context | `rust/crates/rusty-claude-cli/src/main.rs:5991-6027`, `:12960-12990` | Renders saved-only/dirty/abandoned lifecycle context for the current partition. |
| CLI session resolution regression | `rust/crates/rusty-claude-cli/src/main.rs:13470-13579` | Covers JSONL default, legacy flat resolution, latest selection, and workspace mismatch rejection from CLI wrappers. |

## Covered roadmap and dogfood anchors

- `ROADMAP.md:1125-1129` — session files are namespaced by workspace fingerprint, and wrong-workspace session access is rejected.
- `ROADMAP.md:1419-1441` — empty/missing session messages must expose the fingerprint directory instead of implying a flat `.claw/sessions/` search.
- `ROADMAP.md:1453-1476` — the session partition boundary must be visible or shared deliberately; current contract is visible CWD/workspace partitioning.
- `ROADMAP.md:5797-5902` — canonicalization closes the symlink/path-equivalence split in workspace fingerprints.
- `ROADMAP.md:6342-6366` and `ROADMAP.md:6384-6411` — remaining Stream 9 risks around reported CWD form, failed-resume filesystem side effects, and broad-CWD resume guards are related UX/recovery lanes, not clone identity itself.

## Focused verification map

| Claim | Focused check |
| --- | --- |
| Same canonical workspace spellings share one partition | `cargo test --manifest-path rust/Cargo.toml -p runtime session_store_from_cwd_canonicalizes_equivalent_paths -- --nocapture` |
| Distinct clones/worktrees do not see each other's sessions | `cargo test --manifest-path rust/Cargo.toml -p runtime session_store_from_cwd_isolates_sessions_by_workspace -- --nocapture` |
| Explicit data-dir stores still namespace by workspace | `cargo test --manifest-path rust/Cargo.toml -p runtime session_store_from_data_dir_namespaces_by_workspace -- --nocapture` |
| Same-workspace legacy sessions are readable; cross-workspace ones are rejected | `cargo test --manifest-path rust/Cargo.toml -p runtime session_store_rejects_legacy_session_from_other_workspace session_store_loads_safe_legacy_session_from_same_workspace session_store_loads_unbound_legacy_session_from_same_workspace -- --nocapture` |
| `latest` and managed reference resolution stay inside the active partition | `cargo test --manifest-path rust/Cargo.toml -p runtime session_store_latest_and_resolve_reference -- --nocapture` and `cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli latest_session_alias_resolves_most_recent_managed_session -- --nocapture` |
| Forks retain partition and lineage metadata | `cargo test --manifest-path rust/Cargo.toml -p runtime session_store_fork_stays_in_same_namespace -- --nocapture` |
| CLI wrapper rejects wrong-workspace files | `cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli load_session_reference_rejects_workspace_mismatch -- --nocapture` |
| Docs-only map is syntactically clean | `git diff --check` |
| Broader type/test gate for the touched domain | `cargo check --manifest-path rust/Cargo.toml -p runtime -p rusty-claude-cli` plus `cargo test --manifest-path rust/Cargo.toml -p runtime session_control -- --nocapture` |

## Known boundaries and integration notes

- This worker intentionally did **not** edit `docs/g010-session-hygiene-verification-map.md` because worker-4 task 7 also names that final aggregate map. This file is the worker-2 clone-disambiguation map that worker-4/leader can link or merge into the aggregate map.
- The current `SessionStore::from_cwd` contract keys on the canonical current directory, not necessarily the git top-level. That is acceptable only if status/help surfaces keep the partition boundary visible; `ROADMAP.md:1453-1476` remains the product tradeoff record.
- Failed-resume directory creation and broad-CWD guards are related session hygiene hazards but are owned by the Stream 9 CLI/recovery lanes, not this docs-only clone-disambiguation task.
- No `.omx/ultragoal` files were changed; leader-owned aggregate checkpointing consumes this commit and task lifecycle evidence.

## Delegation evidence

Subagent spawn evidence: 1, repository map probe `019e295d-a3dc-7041-bc96-30ee52b95698`; spawned before deeper serial mapping per task contract, but it errored with `429 Too Many Requests`, so direct repo evidence above was integrated instead.
