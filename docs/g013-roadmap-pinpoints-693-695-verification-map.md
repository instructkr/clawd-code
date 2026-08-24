# G013 ROADMAP pinpoints #693-#695 verification map

This map records the current-head follow-up that was discovered after resetting
`main` to `origin/main`: ROADMAP.md contained three new Pinpoint headings not
covered by the Claw Code 2.0 board.

## Pinpoint #693 — typed phase error instead of silent `unknown`

- Code: `rust/crates/claw-analog/src/lib.rs`
- Behavior: `format_rag_query_json_for_model` now rejects missing, empty, or
  literal `"unknown"` phase values with a structured error envelope containing
  `kind:"unknown_bootstrap_phase"`, `field:"phase"`, and `received_value`.
- Regression tests: `rag_response_missing_phase_returns_typed_error` and
  `rag_response_unknown_phase_returns_typed_error`.

## Pinpoint #694 — local pre-push build gate

- Hook: `.github/hooks/pre-push`
- Install command: `git config core.hooksPath .github/hooks`
- Gate: `cargo build --manifest-path rust/Cargo.toml --workspace --locked`
- Escape hatch: `SKIP_CLAW_PRE_PUSH_BUILD=1` prints an explicit skip message.
- Regression test: `tests/test_pre_push_hook_contract.py` locks the skip
  hatch and `--locked` build command contract.
- Purpose: mirror the CI build job locally so stale field/variant references are
  caught before push.

## Pinpoint #695 — startup/worktree preflight diagnostics

- Code: `rust/crates/runtime/src/worker_boot.rs`
- Behavior: `startup_preflight_warnings` and
  `WorkerRegistry::observe_startup_preflight` emit structured warnings before
  the first model turn when a task mentions a path not tracked on the current
  branch (`file_absent_on_branch`) or git metadata is not writable
  (`git_metadata_not_writable`).
- Regression tests:
  - `startup_preflight_warns_when_task_file_is_absent_on_branch`
  - `startup_preflight_records_structured_warning_event`

## Verification commands

```bash
python3 scripts/generate_cc2_board.py
python3 scripts/validate_cc2_board.py --board .omx/cc2/board.json
python3 .omx/cc2/validate_issue_parity_intake.py .omx/cc2/issue-parity-intake.json
bash -n .github/hooks/pre-push
python3 tests/test_pre_push_hook_contract.py -v
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo test --manifest-path rust/Cargo.toml -p claw-analog rag_response_ -- --nocapture
cargo test --manifest-path rust/Cargo.toml -p runtime startup_preflight -- --nocapture
cargo build --manifest-path rust/Cargo.toml --workspace --locked
```
