# JSON Envelope Schemas — Clawable CLI Contract

> **⚠️ CRITICAL: This document describes the TARGET v2.0 envelope schema, not the current v1.0 binary behavior.** The Rust binary currently emits a **flat v1.0 envelope** that does NOT include `timestamp`, `command`, `exit_code`, `output_format`, or `schema_version` fields. See [`FIX_LOCUS_164.md`](./FIX_LOCUS_164.md) for the full migration plan and timeline. **Do not build automation against the field shapes below without first testing against the actual binary output.** Use `claw <command> --output-format json` to inspect what your binary version actually emits.

This document locks the **target** field-level contract for all clawable-surface commands. After the v1.0→v2.0 migration (FIX_LOCUS_164 Phase 2), every command accepting `--output-format json` will conform to the envelope shapes documented here.

**Target audience:** Claws planning v2.0 migration, reference implementers, contract validators.

**Current v1.0 reality:** See [`ERROR_HANDLING.md`](./ERROR_HANDLING.md) Appendix A for the flat envelope shape the binary actually emits today.

---

## Common Fields (All Envelopes) — TARGET v2.0 SCHEMA

**This section describes the v2.0 target schema. The current v1.0 binary does NOT emit these fields.** See FIX_LOCUS_164.md for the migration timeline.

After v2.0 migration, every command response, success or error, will carry:

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "list-sessions",
  "exit_code": 0,
  "output_format": "json",
  "schema_version": "2.0"
}
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `timestamp` | ISO 8601 UTC | Yes | Time command completed |
| `command` | string | Yes | argv[1] (e.g. "list-sessions") |
| `exit_code` | int (0/1/2) | Yes | 0=success, 1=error/not-found, 2=timeout |
| `output_format` | string | Yes | Always "json" (for symmetry with text mode) |
| `schema_version` | string | Yes | "1.0" (bump for breaking changes) |

---

## Turn Result Fields (Multi-Turn Sessions)

When a command's response includes a `turn` object (e.g., in `bootstrap` or `turn-loop`), it carries:

| Field | Type | Required | Notes |
|---|---|---|---|
| `prompt` | string | Yes | User input for this turn |
| `output` | string | Yes | Assistant response |
| `stop_reason` | enum | Yes | One of: `completed`, `timeout`, `cancelled`, `max_budget_reached`, `max_turns_reached` |
| `cancel_observed` | bool | Yes | #164 Stage B: cancellation was signaled and observed (#161/#164) |

---

## Error Envelope

When a command fails (exit code 1), responses carry:

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "exec-command",
  "exit_code": 1,
  "error": {
    "kind": "filesystem",
    "operation": "write",
    "target": "/tmp/nonexistent/out.md",
    "retryable": true,
    "message": "No such file or directory",
    "hint": "intermediate directory does not exist; try mkdir -p /tmp/nonexistent"
  }
}
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `error.kind` | enum | Yes | One of: `filesystem`, `auth`, `session`, `parse`, `runtime`, `mcp`, `delivery`, `usage`, `policy`, `unknown` |
| `error.operation` | string | Yes | Syscall/method that failed (e.g. "write", "open", "resolve_session") |
| `error.target` | string | Yes | Resource that failed (path, session-id, server-name, etc.) |
| `error.retryable` | bool | Yes | Whether caller can safely retry without intervention |
| `error.message` | string | Yes | Platform error message (e.g. errno text) |
| `error.hint` | string | No | Optional actionable next step |

---

## Not-Found Envelope

When an entity does not exist (exit code 1, but not a failure):

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "load-session",
  "exit_code": 1,
  "name": "does-not-exist",
  "found": false,
  "error": {
    "kind": "session_not_found",
    "message": "session 'does-not-exist' not found in .claw/sessions/",
    "retryable": false
  }
}
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `name` | string | Yes | Entity name/id that was looked up |
| `found` | bool | Yes | Always `false` for not-found |
| `error.kind` | enum | Yes | One of: `command_not_found`, `tool_not_found`, `session_not_found` |
| `error.message` | string | Yes | User-visible explanation |
| `error.retryable` | bool | Yes | Usually `false` (entity will not magically appear) |

---

## Per-Command Success Schemas

### `list-sessions`

**Status**: ✅ Implemented (closed #251 cycle #45, 2026-04-23).

**Actual binary envelope** (as of #251 fix):
```json
{
  "command": "list-sessions",
  "sessions": [
    {
      "id": "session-1775777421902-1",
      "path": "/path/to/.claw/sessions/session-1775777421902-1.jsonl",
      "updated_at_ms": 1775777421902,
      "message_count": 0
    }
  ]
}
```

**Aspirational (future) shape**:
```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "list-sessions",
  "exit_code": 0,
  "output_format": "json",
  "schema_version": "1.0",
  "directory": ".claw/sessions",
  "sessions_count": 2,
  "sessions": [
    {
      "session_id": "sess_abc123",
      "created_at": "2026-04-21T15:30:00Z",
      "last_modified": "2026-04-22T09:45:00Z",
      "prompt_count": 5,
      "stopped": false
    }
  ]
}
```

**Gap**: Current impl lacks `timestamp`, `exit_code`, `output_format`, `schema_version`, `directory`, `sessions_count` (derivable), and the session object uses `id`/`updated_at_ms`/`message_count` instead of `session_id`/`last_modified`/`prompt_count`. Follow-up #250 Option B to align field names and add common-envelope fields.

### `delete-session`

**Status**: ⚠️ Stub only (closed #251 dispatch-order fix; full impl deferred).

**Actual binary envelope** (as of #251 fix):
```json
{
  "type": "error",
  "command": "delete-session",
  "error": "not_yet_implemented",
  "kind": "not_yet_implemented"
}
```

Exit code: 1. No credentials required. The stub ensures the verb does NOT fall through to Prompt/auth (the #251 fix), but the actual delete operation is not yet wired.

**Aspirational (future) shape**:
```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "delete-session",
  "exit_code": 0,
  "session_id": "sess_abc123",
  "deleted": true,
  "directory": ".claw/sessions"
}
```

### `load-session`

**Status**: ✅ Implemented (closed #251 cycle #45, 2026-04-23).

**Actual binary envelope** (as of #251 fix):
```json
{
  "command": "load-session",
  "session": {
    "id": "session-abc123",
    "path": "/path/to/.claw/sessions/session-abc123.jsonl",
    "messages": 5
  }
}
```

For nonexistent sessions, emits a local `session_not_found` error (NOT `missing_credentials`):
```json
{
  "error": "session not found: nonexistent",
  "kind": "session_not_found",
  "type": "error",
  "hint": "Hint: managed sessions live in .claw/sessions/<hash>/ ..."
}
```

**Aspirational (future) shape**:
```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "load-session",
  "exit_code": 0,
  "session_id": "sess_abc123",
  "loaded": true,
  "directory": ".claw/sessions",
  "path": ".claw/sessions/sess_abc123.jsonl"
}
```

**Gap**: Current impl uses nested `session: {...}` instead of flat fields, and omits common-envelope fields. Follow-up #250 Option B to align.

### `flush-transcript`

**Status**: ⚠️ Stub only (closed #251 dispatch-order fix; full impl deferred).

**Actual binary envelope** (as of #251 fix):
```json
{
  "type": "error",
  "command": "flush-transcript",
  "error": "not_yet_implemented",
  "kind": "not_yet_implemented"
}
```

Exit code: 1. No credentials required. Like `delete-session`, this stub resolves the #251 dispatch-order bug but the actual flush operation is not yet wired.

**Aspirational (future) shape**:
```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "flush-transcript",
  "exit_code": 0,
  "session_id": "sess_abc123",
  "path": ".claw/sessions/sess_abc123.jsonl",
  "flushed": true,
  "messages_count": 12,
  "input_tokens": 4500,
  "output_tokens": 1200
}
```

### `show-command`

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "show-command",
  "exit_code": 0,
  "name": "add-dir",
  "found": true,
  "source_hint": "commands/add-dir/add-dir.tsx",
  "responsibility": "creates a new directory in the worktree"
}
```

### `show-tool`

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "show-tool",
  "exit_code": 0,
  "name": "BashTool",
  "found": true,
  "source_hint": "tools/BashTool/BashTool.tsx"
}
```

### `exec-command`

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "exec-command",
  "exit_code": 0,
  "name": "add-dir",
  "prompt": "create src/util/",
  "handled": true,
  "message": "created directory",
  "source_hint": "commands/add-dir/add-dir.tsx"
}
```

### `exec-tool`

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "exec-tool",
  "exit_code": 0,
  "name": "BashTool",
  "payload": "cargo build",
  "handled": true,
  "message": "exit code 0",
  "source_hint": "tools/BashTool/BashTool.tsx"
}
```

### `route`

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "route",
  "exit_code": 0,
  "prompt": "add a test",
  "limit": 10,
  "match_count": 3,
  "matches": [
    {
      "kind": "command",
      "name": "add-file",
      "score": 0.92,
      "source_hint": "commands/add-file/add-file.tsx"
    }
  ]
}
```

### `bootstrap`

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "bootstrap",
  "exit_code": 0,
  "prompt": "hello",
  "setup": {
    "python_version": "3.13.12",
    "implementation": "CPython",
    "platform_name": "darwin",
    "test_command": "pytest"
  },
  "routed_matches": [
    {"kind": "command", "name": "init", "score": 0.85, "source_hint": "..."}
  ],
  "turn": {
    "prompt": "hello",
    "output": "...",
    "stop_reason": "completed"
  },
  "persisted_session_path": ".claw/sessions/sess_abc.jsonl"
}
```

### `command-graph`

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "command-graph",
  "exit_code": 0,
  "builtins_count": 185,
  "plugin_like_count": 20,
  "skill_like_count": 2,
  "total_count": 207,
  "builtins": [
    {"name": "add-dir", "source_hint": "commands/add-dir/add-dir.tsx"}
  ],
  "plugin_like": [],
  "skill_like": []
}
```

### `tool-pool`

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "tool-pool",
  "exit_code": 0,
  "simple_mode": false,
  "include_mcp": true,
  "tool_count": 184,
  "tools": [
    {"name": "BashTool", "source_hint": "tools/BashTool/BashTool.tsx"}
  ]
}
```

### `bootstrap-graph`

```json
{
  "timestamp": "2026-04-22T10:10:00Z",
  "command": "bootstrap-graph",
  "exit_code": 0,
  "stages": ["stage 1", "stage 2", "..."],
  "note": "bootstrap-graph is markdown-only in this version"
}
```

---

## Versioning & Compatibility

- **schema_version = "1.0":** Current as of 2026-04-22. Covers all 13 clawable commands.
- **Breaking changes** (e.g. renaming a field) bump schema_version to "2.0".
- **Additive changes** (e.g. new optional field) stay at "1.0" and are backward compatible.
- Downstream claws **must** check `schema_version` before relying on field presence.

---

## Regression Testing

Each command is covered by:
1. **Fixture file** (golden JSON snapshot under `tests/fixtures/json/<command>.json`)
2. **Parametrised test** in `test_cli_parity_audit.py::TestJsonOutputContractEndToEnd`
3. **Field consistency test** (new, tracked as ROADMAP #172)

To update a fixture after a intentional schema change:
```bash
claw <command> --output-format json <args> > tests/fixtures/json/<command>.json
# Review the diff, commit
git add tests/fixtures/json/<command>.json
```

To verify no regressions:
```bash
cargo test --release test_json_envelope_field_consistency
```

---

## Design Notes

**Why common fields on every response?**
- Downstream claws can build one error handler that works for all commands
- Timestamp + command + exit_code give context without scraping argv or timestamps from command output
- `schema_version` signals compatibility for future upgrades

**Why both "found" and "error" on not-found?**
- Exit code 1 covers both "entity missing" and "operation failed"
- `found=false` distinguishes not-found from error without string matching
- `error.kind` and `error.retryable` let automation decide: retry a temporary miss vs escalate a permanent refusal

**Why "operation" and "target" in error?**
- Claws can aggregate failures by operation type (e.g. "how many `write` ops failed?")
- Claws can implement per-target retry policy (e.g. "skip missing files, retry networking")
- Pure text errors ("No such file") do not provide enough structure for pattern matching

**Why "handled" vs "found"?**
- `show-command` reports `found: bool` (inventory signal: "does this exist?")
- `exec-command` reports `handled: bool` (operational signal: "was this work performed?")
- The names matter: a command can be found but not handled (e.g. too large for context window), or handled silently (no output message)

---

## Appendix: Current v1.0 vs. Target v2.0 Envelope Shapes

### ⚠️ IMPORTANT: Binary Reality vs. This Document

**This entire SCHEMAS.md document describes the TARGET v2.0 schema.** The actual Rust binary currently emits v1.0 (flat) envelopes.

**Do not assume the fields documented above are in the binary right now.** They are not.

### Current v1.0 Envelope (What the Rust Binary Actually Emits)

The Rust binary in `rust/` currently emits a **flat v1.0 envelope** without common metadata wrapper:

#### v1.0 Success Envelope Example

```json
{
  "kind": "list-sessions",
  "sessions": [
    {"id": "abc123", "created": "2026-04-22T10:00:00Z", "turns": 5}
  ],
  "type": "success"
}
```

**Key differences from v2.0 above:**
- NO `timestamp`, `command`, `exit_code`, `output_format`, `schema_version` fields
- `kind` field contains the verb name (or is entirely absent for success)
- `type: "success"` flag at top level
- Verb-specific fields (`sessions`, `turn`, etc.) at top level

#### v1.0 Error Envelope Example

```json
{
  "error": "session 'xyz789' not found in .claw/sessions",
  "hint": "use 'list-sessions' to see available sessions",
  "kind": "session_not_found",
  "type": "error"
}
```

**Key differences from v2.0 error above:**
- `error` field is a **STRING**, not a nested object
- NO `error.operation`, `error.target`, `error.retryable` structured fields
- `kind` is at top-level, not nested
- NO `timestamp`, `command`, `exit_code`, `output_format`, `schema_version`
- Extra `type: "error"` flag

### Migration Timeline (FIX_LOCUS_164)

See [`FIX_LOCUS_164.md`](./FIX_LOCUS_164.md) for the full phased migration:

- **Phase 1 (Opt-in):** `claw <cmd> --output-format json --envelope-version=2.0` emits v2.0 shape
- **Phase 2 (Default):** v2.0 becomes default; `--legacy-envelope` flag opts into v1.0
- **Phase 3 (Deprecation):** v1.0 warnings, then removal

### Building Automation Against v1.0 (Current)

**For claws building automation today** (against the real binary, not this schema):

1. **Check `type` field first** (string: "success" or "error")
2. **For success:** verb-specific fields are at top level. Use `jq .kind` for verb ID (if present)
3. **For error:** access `error` (string), `hint` (string), `kind` (string) all at top level
4. **Do not expect:** `timestamp`, `command`, `exit_code`, `output_format`, `schema_version` — they don't exist yet
5. **Test your code** against `claw <cmd> --output-format json` output to verify assumptions before deploying

### Example: Python Consumer Code (v1.0)

**Correct pattern for v1.0 (current binary):**

```python
import json
import subprocess

result = subprocess.run(
    ["claw", "list-sessions", "--output-format", "json"],
    capture_output=True,
    text=True
)
envelope = json.loads(result.stdout)

# v1.0: type is at top level
if envelope.get("type") == "error":
    error_msg = envelope.get("error", "unknown error")  # error is a STRING
    error_kind = envelope.get("kind")  # kind is at TOP LEVEL
    print(f"Error: {error_kind} — {error_msg}")
else:
    # Success path: verb-specific fields at top level
    sessions = envelope.get("sessions", [])
    for session in sessions:
        print(f"Session: {session['id']}")
```

**After v2.0 migration, this code will break.** Claws building for v2.0 compatibility should:

1. Check `schema_version` field
2. Parse differently based on version
3. Or wait until Phase 2 default bump is announced, then migrate

### Why This Mismatch Exists

SCHEMAS.md was written as the **target design** for v2.0. The Rust binary is still on v1.0. The migration (FIX_LOCUS_164) will bring the binary in line with this schema, but it hasn't happened yet.

**This mismatch is the root cause of doc-truthfulness issues #78, #79, #165.** All three docs were documenting the v2.0 target as if it were current reality.

### Questions?

- **"Is v2.0 implemented?"** No. The binary is v1.0. See FIX_LOCUS_164.md for the implementation roadmap.
- **"Should I build against v2.0 schema?"** No. Build against v1.0 (current). Test your code with `claw` to verify.
- **"When does v2.0 ship?"** See FIX_LOCUS_164.md Phase 1 estimate: ~6 dev-days. Not scheduled yet.
- **"Can I use v2.0 now?"** Only if you explicitly pass `--envelope-version=2.0` (which doesn't exist yet in v1.0 binary).

