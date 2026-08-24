# Contributing to Claw Code

Thanks for helping improve Claw Code. This repository is a Rust-first CLI
workspace with supporting docs and compatibility fixtures.

## Ground rules

- Keep changes small, reviewable, and tied to a concrete issue or behavior.
- Do not commit secrets, API keys, session transcripts with credentials, or
  generated build output.
- Prefer existing crate boundaries and utilities before adding dependencies.
- Update documentation when a user-facing command, config key, or provider
  behavior changes.
- Keep examples copy/paste safe. Use placeholder keys such as `sk-ant-...` and
  avoid commands that require live credentials unless the text explicitly says
  so.

## Local setup

```bash
git clone https://github.com/ultraworkers/claw-code
cd claw-code/rust
cargo build --workspace
cargo test --workspace
```

On Windows PowerShell, build from the same `rust` workspace and run the binary
with the `.exe` suffix:

```powershell
cd claw-code\rust
cargo build --workspace
.\target\debug\claw.exe --help
```

## Local pre-push build gate

Install the repository-local hook to catch stale compile errors before pushing:

```bash
git config core.hooksPath .github/hooks
```

This sets the repo's Git hook directory to `.github/hooks`; if you already use a
custom `core.hooksPath`, copy or chain `.github/hooks/pre-push` instead. The hook
runs the ROADMAP id guard, then runs
`cargo build --manifest-path rust/Cargo.toml --workspace --locked` from the
repository root. If you must bypass the cargo build for a docs-only push, set
`SKIP_CLAW_PRE_PUSH_BUILD=1`; the hook still runs the ROADMAP guard and prints
when the cargo-build escape hatch is used.

## ROADMAP id allocation

Before appending a new numeric ROADMAP entry, pull/rebase onto the latest
`main`, allocate the id from the file you are about to edit, and run the duplicate
id guard before pushing:

```bash
git pull --rebase
NEXT=$(scripts/roadmap-next-id.sh)
# append "${NEXT}. **...**" to ROADMAP.md
scripts/roadmap-check-ids.sh
```

The duplicate guard currently checks helper-era ids (`>=723`) by default so it
catches new optimistic-append collisions without failing on legacy numbered lists
already present in the historical roadmap. Use `scripts/roadmap-check-ids.sh
--min-id 1` for a strict whole-file audit after those legacy collisions are
cleaned up.

## Checks before opening a pull request

Run the smallest relevant tests for your change, then the broader checks when
you touch shared runtime, CLI, or docs surfaces:

```bash
cd rust
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace
```

For documentation and release-readiness changes, also run:

```bash
python .github/scripts/check_doc_source_of_truth.py
python .github/scripts/check_release_readiness.py
```

## Pull request guidance

- Describe the user-visible reason for the change.
- List the commands you ran and any known gaps.
- Call out compatibility risks for CLI output, JSON schemas, plugin contracts,
  provider behavior, or Windows/PowerShell examples.
- Keep unrelated cleanup out of feature or fix pull requests.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](./LICENSE).
