# AGENTS.md — rust/ workspace

## OVERVIEW

Virtual Cargo workspace (resolver 2, edition 2021) housing 11 crates that compose the `claw` CLI and supporting services.

## STRUCTURE

| Crate | Kind | Purpose |
|---|---|---|
| `rusty-claude-cli` | bin (`claw`) | Main CLI binary. Package name ≠ binary name. |
| `claw-analog` | lib+bin | Alternate entry point; depends on api + runtime only. |
| `claw-rag-service` | bin | RAG service. Only crate with `[features]` (`qdrant-index`). |
| `mock-anthropic-service` | lib+bin | Mock Anthropic Messages API. Prints `MOCK_ANTHROPIC_BASE_URL`. Dev-dep for CLI and analog tests. |
| `runtime` | lib | Core: sessions, permissions, MCP, conversation loop. ~47 modules. |
| `api` | lib | Provider clients: Anthropic, OpenAI-compat (xAI, OpenAI, DashScope, Ollama). |
| `tools` | lib | 55-tool surface area. Depends on `commands` (not vice versa). |
| `commands` | lib | 120+ slash commands. |
| `plugins` | lib | Plugin manifest and lifecycle. |
| `telemetry` | lib | Request identity + analytics sinks. |
| `compat-harness` | lib | Extracts upstream TS claude-code manifest/commands/tools for parity comparison. |

Dependency direction: `rusty-claude-cli` → tools/commands/runtime/api/plugins. `tools` → `commands`.

## WHERE TO LOOK

- **Parity testing**: `mock_parity_scenarios.json` at workspace root, loaded via `CARGO_MANIFEST_DIR/../../mock_parity_scenarios.json`. Scripts in `scripts/` (`run_mock_parity_harness.sh`, `run_mock_parity_diff.py`).
- **CI**: `.github/workflows/rust-ci.yml` (fmt, clippy, test, docs, Windows smoke) and `release.yml` (v* tag builds for linux-x64/macos-arm64/windows-x64).
- **Committed test fixtures**: `.clawd-agents/`, `.omc/`, `.sandbox-home/` are checked-in harness dotdirs.
- **Docs**: `PARITY.md`, `TUI-ENHANCEMENT-PLAN.md`, `README.md` alongside this file.

## CONVENTIONS

Workspace lints (all crates opt in via `[lints] workspace = true`):
- `unsafe_code` = **forbid**. No exceptions.
- clippy `all` = warn, `pedantic` = allow. Explicitly allowed: `module_name_repetitions`, `missing_panics_doc`, `missing_errors_doc`.

No `rustfmt.toml` or `clippy.toml`. Stock defaults only.

TUI rule: formatting fns take `&mut impl Write`, never stdout directly. Never mix raw ANSI escapes with crossterm.

Library crates don't carry the `claw-` prefix. Binary crates do (except legacy `rusty-claude-cli`).

Workspace version is `0.1.3`, `publish = false`, MIT license.

No rust-toolchain file, no MSRV. CI pins `dtolnay/rust-toolchain@stable`.

## ANTI-PATTERNS

- Don't run `cargo fmt --manifest-path rust/Cargo.toml` from the repo root. Use `../scripts/fmt.sh` instead.
- Don't add `unsafe` code. The lint is set to `forbid`, not `deny`. You can't `#[allow]` it.
- Don't create dependencies from `commands` → `tools`. The arrow goes `tools` → `commands`.
- Don't write TUI output directly to stdout or use raw ANSI escape sequences.
- Don't add features to crates other than `claw-rag-service` without good reason; the workspace is feature-lean by design.

## COMMANDS

All run from `rust/`:

```sh
# Format (check only)
../scripts/fmt.sh --check

# Format (apply)
../scripts/fmt.sh

# Lint (strict, matches what you should pass before pushing)
cargo clippy --workspace --all-targets -- -D warnings

# Test
cargo test --workspace

# Build specific binary
cargo build -p rusty-claude-cli
cargo build -p claw-analog
cargo build -p claw-rag-service
cargo build -p mock-anthropic-service
```

Note: CI clippy runs without `-D warnings`, so the local check above is stricter than the gate.
