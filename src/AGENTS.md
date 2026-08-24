# src/ AGENTS.md

## OVERVIEW

This is a **porting workspace**, not production code. Nothing here is imported by,
built with, or shipped in the Rust product (`rust/crates/`). The Python tree exists
solely to mirror and track parity against the Claude Code TypeScript source.

## STRUCTURE

```
src/
├── main.py              # argparse CLI entry point (summary, parity-audit, manifest, etc.)
├── ~38 top-level .py    # mirror TS root files one-to-one
├── models.py            # frozen dataclasses: Subsystem, PortingModule, PermissionDenial, ...
├── parity_audit.py      # hard-coded TS→Python filename mapping (main.tsx→main.py, etc.)
├── port_manifest.py     # builds a manifest of the src/ tree itself
├── reference_data/      # tracked JSON snapshots extracted from the TS archive
│   ├── archive_surface_snapshot.json
│   ├── tools_snapshot.json, commands_snapshot.json
│   └── subsystems/*.json   (29 per-subsystem records)
└── ~30 subdirectories/  # PLACEHOLDER PACKAGES (assistant/, bootstrap/, voice/, vim/, ...)
    └── each contains only __init__.py loading subsystems/<name>.json
```

Placeholder packages follow an identical template: load
`reference_data/subsystems/<name>.json` via `_archive_helper.load_archive_metadata()`,
re-export `ARCHIVE_NAME`, `MODULE_COUNT`, `SAMPLE_FILES`, `PORTING_NOTE`. There is no
voice, vim, or other real functionality behind them.

## WHERE TO LOOK

| Goal | Start here |
|---|---|
| Understand the CLI subcommands | `main.py` |
| See which TS files map to which .py | `parity_audit.py` |
| Find shared data structures | `models.py` |
| Check TS archive metadata | `reference_data/` |
| Real logic (permissions, path scoping) | `permissions.py`, `path_scope.py` |
| Query engine shim | `query_engine.py` |
| Runtime simulation | `runtime.py` |
| Tests | repo-root `tests/` (test_porting_workspace.py, test_security_scope.py) |

Run tests: `python -m unittest discover -s tests` from repo root.

## CONVENTIONS

- snake_case filenames, preserving TS names. camelCase exceptions exist where the
  TS original used it: `QueryEngine.py`, `costHook.py`, `replLauncher.py`.
- Every mirrored entry carries a `source_hint` pointing back to its original `.ts`/`.tsx` path.
- `from __future__ import annotations` throughout. Stdlib only, no third-party deps.
- `models.py` dataclasses are frozen. Renderers are `as_markdown()` / `to_markdown()`.
- main.py simulates routing, turn-loops, bootstrap over mirrored inventories.
  Read-only shims return handled/message results. It never calls an LLM.
- Thin shim modules (e.g. `ink.py`) exist as backlog metadata, not working code.

## ANTI-PATTERNS

**Don't add real agent, tool, voice, or vim functionality here.** That belongs in
`rust/crates/`. This tree is a scaffold, not an implementation target.

**Don't commit anything from `archive/`.** The local TS snapshot
(`archive/claude_code_ts_snapshot/src`) is gitignored. `reference_data/` holds the
tracked extracts; work from those.

**Don't let parity_audit.py drift.** When you rename a mirrored module, update the
hard-coded mapping in `parity_audit.py` to match.

**Don't add third-party dependencies.** Everything runs on stdlib.
