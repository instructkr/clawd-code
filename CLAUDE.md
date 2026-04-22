# CLAUDE.md — Python Reference Implementation

**This file guides work on `src/` and `tests/` — the Python reference harness for claw-code protocol.**

The production CLI lives in `rust/`; this directory (`src/`, `tests/`, `.py` files) is a **protocol validation and dogfood surface**.

## What this Python harness does

**Machine-first orchestration layer** — proves that the claw-code JSON protocol is:
- Deterministic and recoverable (every output is reproducible)
- Self-describing (SCHEMAS.md documents every field)
- Clawable (external agents can build ONE error handler for all commands)

## Stack
- **Language:** Python 3.13+
- **Dependencies:** minimal (no frameworks; pure stdlibs + attrs/dataclasses)
- **Test runner:** pytest
- **Protocol contract:** SCHEMAS.md (machine-readable JSON envelope)

## Quick start

```bash
# 1. Install dependencies (if not already in venv)
python3 -m venv .venv && source .venv/bin/activate
# (dependencies minimal; standard library mostly)

# 2. Run tests
python3 -m pytest tests/ -q

# 3. Try a command
python3 -m src.main bootstrap "hello" --output-format json | python3 -m json.tool
```

## Verification workflow

```bash
# Unit tests (fast)
python3 -m pytest tests/ -q 2>&1 | tail -3

# Type checking (optional but recommended)
python3 -m mypy src/ --ignore-missing-imports 2>&1 | tail -5
```

## Repository shape

- **`src/`** — Python reference harness implementing SCHEMAS.md protocol
  - `main.py` — CLI entry point; all 14 clawable commands
  - `query_engine.py` — core TurnResult / QueryEngineConfig
  - `runtime.py` — PortRuntime; turn loop + cancellation (#164 Stage A/B)
  - `session_store.py` — session persistence
  - `transcript.py` — turn transcript assembly
  - `commands.py`, `tools.py` — simulated command/tool trees
  - `models.py` — PermissionDenial, UsageSummary, etc.

- **`tests/`** — comprehensive protocol validation (22 baseline → 192 passing as of 2026-04-22)
  - `test_cli_parity_audit.py` — proves all 14 clawable commands accept --output-format
  - `test_json_envelope_field_consistency.py` — validates SCHEMAS.md contract
  - `test_cancel_observed_field.py` — #164 Stage B: cancellation observability + safe-to-reuse semantics
  - `test_run_turn_loop_*.py` — turn loop behavior (timeout, cancellation, continuation, permissions)
  - `test_submit_message_*.py` — budget, cancellation contracts
  - `test_*_cli.py` — command-specific JSON output validation

- **`SCHEMAS.md`** — canonical JSON contract (**target v2.0 design; see note below**)
  - **Target v2.0 common fields** (all envelopes): timestamp, command, exit_code, output_format, schema_version
  - **Current v1.0 binary fields** (what the Rust binary actually emits): flat top-level `kind` + verb-specific fields OR `{error, hint, kind, type}` for errors
  - Error envelope shape (target v2.0: nested error object)
  - Not-found envelope shape (target v2.0)
  - Per-command success schemas (14 commands documented)
  - Turn Result fields (including cancel_observed as of #164 Stage B)

  > **Important:** SCHEMAS.md describes the **v2.0 target envelope**, not the current v1.0 binary behavior. The binary does NOT currently emit `timestamp`, `command`, `exit_code`, `output_format`, or `schema_version` fields. See [`FIX_LOCUS_164.md`](./FIX_LOCUS_164.md) for the migration plan (Phase 1: dual-mode flag; Phase 2: default bump; Phase 3: deprecation).

- **`.gitignore`** — excludes `.port_sessions/` (dogfood-run state)

## Key concepts

### Clawable surface (14 commands)

Every clawable command **must**:
1. Accept `--output-format {text,json}`
2. Return JSON envelopes (current v1.0: flat shape with top-level `kind`; target v2.0: nested with common fields per SCHEMAS.md)
3. **v1.0 (current):** Emit flat top-level fields: verb-specific data + `kind` (verb identity for success, error classification for errors)
4. **v2.0 (target, post-FIX_LOCUS_164):** Use common wrapper fields (timestamp, command, exit_code, output_format, schema_version) with nested `data` or `error` objects
5. Exit 0 on success, 1 on error/not-found, 2 on timeout

**Migration note:** The Python reference harness in `src/` was written against the v2.0 target schema (SCHEMAS.md). The Rust binary in `rust/` currently emits v1.0 (flat). See [`FIX_LOCUS_164.md`](./FIX_LOCUS_164.md) for the full migration plan and timeline.

**Commands:** list-sessions, delete-session, load-session, flush-transcript, show-command, show-tool, exec-command, exec-tool, route, bootstrap, command-graph, tool-pool, bootstrap-graph, turn-loop

**Validation:** `test_cli_parity_audit.py` auto-tests all 14 for --output-format acceptance.

### OPT_OUT surfaces (12 commands)

Explicitly exempt from --output-format requirement (for now):
- Rich-Markdown reports: summary, manifest, parity-audit, setup-report
- List commands with query filters: subsystems, commands, tools
- Simulation/debug: remote-mode, ssh-mode, teleport-mode, direct-connect-mode, deep-link-mode

**Future work:** audit OPT_OUT surfaces for JSON promotion (post-#164).

### Protocol layers

**Coverage (#167–#170):** All clawable commands emit JSON
**Enforcement (#171):** Parity CI prevents new commands skipping JSON
**Documentation (#172):** SCHEMAS.md locks field contract
**Alignment (#173):** Test framework validates docs ↔ code match
**Field evolution (#164 Stage B):** cancel_observed proves protocol extensibility

## Testing & coverage

### Run full suite
```bash
python3 -m pytest tests/ -q
```

### Run one test file
```bash
python3 -m pytest tests/test_cancel_observed_field.py -v
```

### Run one test
```bash
python3 -m pytest tests/test_cancel_observed_field.py::TestCancelObservedField::test_default_value_is_false -v
```

### Check coverage (optional)
```bash
python3 -m pip install coverage  # if not already installed
python3 -m coverage run -m pytest tests/
python3 -m coverage report --skip-covered
```

Target: >90% line coverage for src/ (currently ~85%).

## Common workflows

### Add a new clawable command

1. Add parser in `main.py` (argparse)
2. Add `--output-format` flag
3. Emit JSON envelope using `wrap_json_envelope(data, command_name)`
4. Add command to CLAWABLE_SURFACES in test_cli_parity_audit.py
5. Document in SCHEMAS.md (schema + example)
6. Write test in tests/test_*_cli.py or tests/test_json_envelope_field_consistency.py
7. Run full suite to confirm parity

### Modify TurnResult or protocol fields

1. Update dataclass in `query_engine.py`
2. Update SCHEMAS.md with new field + rationale
3. Write test in `tests/test_json_envelope_field_consistency.py` that validates field presence
4. Update all places that construct TurnResult (grep for `TurnResult(`)
5. Update bootstrap/turn-loop JSON builders in main.py
6. Run `tests/` to ensure no regressions

### Promote an OPT_OUT surface to CLAWABLE

**Prerequisite:** Real demand signal logged in `OPT_OUT_DEMAND_LOG.md` (threshold: 2+ independent signals per surface). Speculative promotions are not allowed.

Once demand is evidenced:
1. Add --output-format flag to argparse
2. Emit wrap_json_envelope() output in JSON path
3. Move command from OPT_OUT_SURFACES to CLAWABLE_SURFACES
4. Document in SCHEMAS.md
5. Write test for JSON output
6. Run parity audit to confirm no regressions
7. Update `OPT_OUT_DEMAND_LOG.md` to mark signal as resolved

### File a demand signal (when a claw actually needs JSON from an OPT_OUT surface)

1. Open `OPT_OUT_DEMAND_LOG.md`
2. Find the surface's entry under Group A/B/C
3. Append a dated entry with Source, Use Case, and Markdown-alternative-checked explanation
4. If this is the 2nd signal for the same surface, file a promotion pinpoint in ROADMAP.md

## Dogfood principles

The Python harness is continuously dogfood-tested:
- Every cycle ships to `main` with detailed commit messages
- New tests are written before/alongside implementation
- Test suite must pass before pushing (zero-regression principle)
- Commits grouped by pinpoint (#159, #160, ..., #174)
- Failure modes classified per exit code: 0=success, 1=error, 2=timeout

## Protocol governance

- **SCHEMAS.md is the source of truth** — any implementation must match field-for-field
- **Tests enforce the contract** — drift is caught by test suite
- **Field additions are forward-compatible** — new fields get defaults, old clients ignore them
- **Exit codes are signals** — claws use them for conditional logic (0→continue, 1→escalate, 2→timeout)
- **Timestamps are audit trails** — every envelope includes ISO 8601 UTC time for chronological ordering

## Related docs

- **`ERROR_HANDLING.md`** — Unified error-handling pattern for claws (one handler for all 14 clawable commands)
- **`SCHEMAS.md`** — JSON protocol specification (read before implementing)
- **`OPT_OUT_AUDIT.md`** — Governance for the 12 non-clawable surfaces
- **`OPT_OUT_DEMAND_LOG.md`** — Active survey recording real demand signals (evidence base for decisions)
- **`ROADMAP.md`** — macro roadmap and macro pain points
- **`PHILOSOPHY.md`** — system design intent
- **`PARITY.md`** — status of Python ↔ Rust protocol equivalence
