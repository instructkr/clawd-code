# G011 Ecosystem/Ops/UX Verification Map

G011 closes the laterals that were intentionally deferred from the earlier safety,
session, MCP, Windows, and docs streams. This map is the cross-lane gate for the
team run: it names the surfaces that can be verified locally, the exact checks to
rerun after worker integrations, and the UX deferrals that must remain explicit
until their product contracts are stable.

## Cross-lane acceptance matrix

| Lane | Owned surface | Regression evidence | Gate / gap |
| --- | --- | --- | --- |
| ACP/Zed status and JSON contracts | `rust/crates/rusty-claude-cli/src/main.rs` parses `claw acp`, `claw acp serve`, `--acp`, and `-acp`; `README.md` and `rust/README.md` document discoverability-only status | `cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli --test output_format_contract acp_guidance_emits_json_when_requested -- --nocapture`; `cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli local_command_help_flags_stay_on_the_local_parser_path -- --nocapture` | Real ACP/Zed daemon support remains deferred; status output must not imply a running protocol endpoint. |
| Plugin/marketplace local routing | `rust/crates/rusty-claude-cli/src/main.rs` routes `claw plugins`, `claw plugin`, and `claw marketplace` to local plugin handling; `rust/crates/commands/src/lib.rs` keeps `/plugin` aliases in shared slash-command help | `cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli removed_login_and_logout_subcommands_error_helpfully -- --nocapture`; `cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli direct_slash_commands_surface_shared_validation_errors -- --nocapture`; `python3 -m unittest tests.test_porting_workspace.PortingWorkspaceTests.test_plugin_command_filter_excludes_plugin_sources tests.test_porting_workspace.PortingWorkspaceTests.test_plugin_command_aliases_execute_as_local_commands tests.test_porting_workspace.PortingWorkspaceTests.test_route_plugin_slash_commands_match_commands tests.test_porting_workspace.PortingWorkspaceTests.test_plugin_command_stream_emits_command_match tests.test_porting_workspace.PortingWorkspaceTests.test_turn_loop_plugin_commands_are_not_prompt_only` | Marketplace is an alias to local plugin management only; no remote marketplace browsing/install contract is claimed. |
| TUI/copy/paste/clickable path UX | `rust/crates/commands/src/lib.rs` advertises `/copy`, `/paste`, `/desktop`, and path-oriented commands; `rust/crates/rusty-claude-cli/src/main.rs` renders compact file/tool paths for terminal readability | `cargo test --manifest-path rust/Cargo.toml -p commands renders_help_with_grouped_categories_and_keyboard_shortcuts -- --nocapture`; `cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli tool_rendering_helpers_compact_output -- --nocapture`; `cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli tool_rendering_truncates_large_read_output_for_display_only -- --nocapture` | Clipboard integration, full-screen TUI mode, and clickable terminal hyperlinks are not stable product contracts yet; keep them as roadmap/UX follow-ups unless a targeted implementation lands. |
| Desktop integration deferral | `rust/crates/commands/src/lib.rs` includes `/desktop`; `rust/crates/rusty-claude-cli/src/main.rs` treats it as not implemented in the current build | `cargo test --manifest-path rust/Cargo.toml -p commands renders_help_from_shared_specs -- --nocapture`; `cargo test --manifest-path rust/Cargo.toml -p commands renders_per_command_help_detail -- --nocapture` | `/desktop` must stay discoverable but non-committal until a desktop launch/API contract exists. |
| Navigation/file-context/local-provider docs | `README.md`, `USAGE.md`, `rust/README.md`, `docs/MODEL_COMPATIBILITY.md`, and worker-2 docs updates | `python3 .github/scripts/check_doc_source_of_truth.py`; `python3 .github/scripts/check_release_readiness.py`; `git diff --check` | Re-run after docs integrations; this lane should not alter Rust behavior unless docs expose a code contract gap. |
| Issue/PR ops gate | `docs/pr-issue-resolution-gate.md`, `docs/roadmap-pr-goals.md`, and issue/PR triage templates if present | `python3 .github/scripts/check_release_readiness.py`; `git diff --check`; optional `python3 scripts/validate_cc2_board.py` only when `.omx/cc2/board.md` changes | Worker lanes must not merge/close remote PRs or issues; final reconciliation remains leader-owned. |

## Task 5 UX/deferral support notes

- `/copy`, `/paste`, and `/desktop` are parsed slash-command names, but current
  runtime handling still reports unimplemented commands rather than performing
  clipboard or desktop side effects. That is safer than pretending support exists.
- `/marketplace` is intentionally a plugin alias; it should not be described as
  a remote marketplace until install/search/update semantics and trust policy are
  specified.
- Path readability is covered by terminal rendering helpers that compact long
  tool outputs and preserve paths in read/write/edit summaries. Clickable OSC-8
  links, if added later, need separate tests because terminal support varies.
- Full-screen TUI mode remains aspirational (`rust/TUI-ENHANCEMENT-PLAN.md`);
  current verification should focus on the inline REPL/help/status surfaces.

## Final verification sequence

Run these after all G011 worker commits are integrated into the leader branch:

```bash
git diff --check
python3 .github/scripts/check_doc_source_of_truth.py
python3 .github/scripts/check_release_readiness.py
cargo check --manifest-path rust/Cargo.toml -p commands -p rusty-claude-cli
cargo test --manifest-path rust/Cargo.toml -p commands renders_help_from_shared_specs -- --nocapture
cargo test --manifest-path rust/Cargo.toml -p commands renders_help_with_grouped_categories_and_keyboard_shortcuts -- --nocapture
cargo test --manifest-path rust/Cargo.toml -p commands renders_per_command_help_detail -- --nocapture
cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli removed_login_and_logout_subcommands_error_helpfully -- --nocapture
cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli direct_slash_commands_surface_shared_validation_errors -- --nocapture
cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli local_command_help_flags_stay_on_the_local_parser_path -- --nocapture
cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli tool_rendering_helpers_compact_output -- --nocapture
cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli tool_rendering_truncates_large_read_output_for_display_only -- --nocapture
cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli --test output_format_contract acp_guidance_emits_json_when_requested -- --nocapture
cargo test --manifest-path rust/Cargo.toml -p rusty-claude-cli --test output_format_contract plugins_json_surfaces_lifecycle_contract_when_plugin_is_installed -- --nocapture
python3 -m unittest tests.test_porting_workspace.PortingWorkspaceTests.test_plugin_command_filter_excludes_plugin_sources tests.test_porting_workspace.PortingWorkspaceTests.test_plugin_command_aliases_execute_as_local_commands tests.test_porting_workspace.PortingWorkspaceTests.test_route_plugin_slash_commands_match_commands tests.test_porting_workspace.PortingWorkspaceTests.test_plugin_command_stream_emits_command_match tests.test_porting_workspace.PortingWorkspaceTests.test_turn_loop_plugin_commands_are_not_prompt_only
```

## Leader audit notes

- This map is repo-local evidence only; workers must not mutate `.omx/ultragoal`.
- If a check fails because another lane is still in progress, record the failing
  command and rerun after that lane is integrated instead of weakening the gate.
- The minimum terminal condition is: docs checks pass, Rust targeted tests pass,
  and any still-deferred UX surface is explicitly named above.
