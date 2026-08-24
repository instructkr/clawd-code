# G010 Session Hygiene Verification Map

Stream 9 session hygiene is implemented in the Rust runtime/CLI as workspace-scoped session storage plus resume-safe recovery commands.

## Acceptance mapping

| Acceptance area | Code surface | Evidence |
| --- | --- | --- |
| Generated session files are not accidentally committed | `.gitignore`, `rust/.gitignore` ignore `.claw/sessions/` and `.claude/sessions/` | `git check-ignore .claw/sessions/example.jsonl rust/.claw/sessions/example.jsonl .claude/sessions/example.json` |
| Per-worktree session isolation | `rust/crates/runtime/src/session_control.rs` (`SessionStore`, `workspace_fingerprint`, workspace validation) | `cargo test -p runtime session_store_from_cwd_isolates_sessions_by_workspace` |
| List/resume/delete/exists contracts | `rust/crates/commands/src/lib.rs` parses `/session list`, `/session exists`, `/session delete`, `/resume`; `rust/crates/rusty-claude-cli/src/main.rs` renders text/JSON resume-safe session commands | `cargo test -p rusty-claude-cli session_exists_resume_command_reports_json_contract`; `cargo test -p rusty-claude-cli resume_report_uses_sectioned_layout` |
| Compact and provider context-window recovery | `rust/crates/runtime/src/compact.rs`; `rust/crates/rusty-claude-cli/src/main.rs` context-window error recovery guidance and resumed `/compact` | `cargo test -p rusty-claude-cli provider_context_window_errors_are_reframed_with_same_guidance`; `cargo test -p commands compacts_sessions_via_slash_command` |
| JSONL bloat safeguards | `rust/crates/runtime/src/session.rs` rotates oversized JSONL session files and keeps bounded rotated logs | `cargo test -p runtime rotates_and_cleans_up_large_session_logs` |
| Interrupt/recovery path | `rust/crates/rusty-claude-cli/src/main.rs` keeps `/clear --confirm`, `/compact`, `/status`, and `/resume latest` resume-safe for unusable threads | `cargo test -p rusty-claude-cli context_window_preflight_errors_render_recovery_steps`; `cargo test -p rusty-claude-cli parses_resume_flag_with_multiple_slash_commands` |
| Clone/session disambiguation | `Session` persists `workspace_root`; forks persist parent/branch metadata; session list shows lineage and lifecycle | `cargo test -p runtime persists_workspace_root_round_trip_and_forks_inherit_it`; `cargo test -p runtime forks_sessions_with_branch_metadata_and_persists_it` |

## Notes for leader audit

- Workers did not mutate `.omx/ultragoal`; this file is a repo-local verification map for team evidence only.
- Runtime-owned session state remains under ignored `.claw/sessions/<workspace-fingerprint>/` paths.
- Resume-safe JSON output uses stable `kind` fields (`restored`, `compact`, `session_list`, `session_exists`, etc.) so claws can route without scraping text.
