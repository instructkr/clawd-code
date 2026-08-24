# AGENTS.md — rusty-claude-cli (the `claw` binary)

## OVERVIEW

Crate `rusty-claude-cli` produces the `claw` binary (~25 subcommands). Wires together api, runtime, tools, commands, plugins; terminal via crossterm/rustyline/syntect. Hand-rolled arg parser (no clap).

## main.rs MAP

src/main.rs is ~19,800 lines. Landmark table:

| Lines | Section |
|---|---|
| 73–330 | Provenance/model types (`ModelSource`, `ModelProvenance`, `PermissionModeSource`), build constants |
| 330–682 | Error taxonomy (`classify_error_kind`, JSON/text error output) |
| 774–994 | Global flags, `--cwd`/`-C` stripping, stdin plumbing |
| 995–1158 | `run()`: single flat match dispatching `CliAction` variants |
| 1162–1280 | `CliAction` enum, 25 struct variants (each carries `output_format`) |
| 1312–1477 | Output-format machinery: `OnceLock` statics, duplicate-flag tracking |
| 1478–2272 | `parse_args`: ~800-line manual token loop |
| 2542–3389 | Per-subcommand sub-parsers inside `parse_args` |
| 2902–3389 | Model/permission/allowed-tools resolution |
| 3390–4696 | Doctor subsystem (~13 `check_*_health` fns) |
| 4697–6062 | Manifests, bootstrap-plan, system-prompt, version, `resume_session` |
| 5459–7047 | `StatusContext`, `BinaryProvenance`, broad-cwd policy, stale-base preflight |
| 7048–9423 | `run_repl` interactive loop (`LiveCli` struct, streaming, heartbeat, `HookAbortMonitor`) |
| 9424–14264 | Snapshot printers: session-list, status, sandbox, models, help topics, acp, `run_init` (L11027), `run_export` (L11715), `print_help` (L14055) |
| 14266–19831 | In-file `mod tests` + 3 smaller test modules (one embeds a Python MCP fixture) |

## CONVENTIONS

- Every subcommand supports `--output-format text|json`, env-overridable.
- `raw_args_request_json_output` pre-scans argv before parsing to suppress stderr config warnings in JSON mode.
- Dual renderers for each output path: human text + structured `*_json`.
- Unknown flags/typos get levenshtein-distance suggestions.
- Comments carry issue numbers (#824, #146, etc.) when tracking known problems.
- `main()` (L330) is only the error envelope. All real work happens in `run()`.
- Error output always includes `status`, `error_kind`, `action`, `hint`, `exit_code` fields.
- `classify_error_kind` maps message prefixes to snake_case kind tokens.
- JSON errors go to stdout, text errors to stderr.
- Sibling modules: `init.rs`, `input.rs` (rustyline), `render.rs` (MarkdownStreamState, Spinner, syntect), `setup_wizard.rs`.
- `build.rs` injects `GIT_SHA`, `RUSTC_VERSION`, `GIT_DIRTY` via `cargo:rustc-env`.

## ANTI-PATTERNS

- File opens with crate-wide `#![allow(...)]` suppressing 13 lints including `dead_code` and `unused_imports`. Legacy. Do NOT extend this list.
- 9+ functions carry `#[allow(clippy::too_many_lines)]`. Tolerated, not license for more.
- Package/bin name mismatch: crate is `rusty-claude-cli`, binary is `claw`. Watch for this in paths and test macros.

## TESTS

All tests live in `tests/` (6 files), black-box style. They spawn `env!("CARGO_BIN_EXE_claw")` in unique temp dirs (AtomicU64 counter).

| File | What it covers |
|---|---|
| `output_format_contract.rs` | ~105 tests pinning `kind`/`status`/`action` JSON contract for EVERY subcommand. Must update when adding/changing any command output. 5,986 lines. |
| `resume_slash_commands.rs` | Resume and slash-command behavior |
| `cli_flags_and_config_defaults.rs` | Flag parsing, config file defaults |
| `compact_output.rs` | Compact output mode |
| `compact_repl_panic.rs` | Nested-runtime panic regression |
| `mock_parity_harness.rs` | Scenario-driven tests against mock-anthropic-service, driven by `rust/mock_parity_scenarios.json` |
