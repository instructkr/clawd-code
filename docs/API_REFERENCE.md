# API Reference — JSON Output Envelope Contract

This document describes the machine-readable JSON output emitted by `claw` when
`--output-format json` is passed.  All JSON envelopes are written to **stdout**.
Stderr is reserved for non-contractual diagnostics only (see pinpoint #168c).

---

## Output Format Flag

```
claw [command] --output-format json
claw [command] --output-format text   # default
```

When `json` is active, **all** output (success and error) is emitted as a single
JSON object on stdout.  Consumers must not parse stderr for errors.

---

## Success Envelope — `claw -p <prompt>`

Full non-compact run (default):

```json
{
  "message": "<final assistant text>",
  "model": "claude-opus-4-5",
  "iterations": 3,
  "auto_compaction": null,
  "tool_uses": [...],
  "tool_results": [...],
  "prompt_cache_events": [...],
  "usage": {
    "input_tokens": 1234,
    "output_tokens": 567,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 0
  },
  "estimated_cost": "$0.0123"
}
```

Compact run (`--compact`):

```json
{
  "message": "<final assistant text>",
  "compact": true,
  "model": "claude-opus-4-5",
  "usage": {
    "input_tokens": 1234,
    "output_tokens": 567,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 0
  }
}
```

### Field Reference

| Field | Type | Description |
|---|---|---|
| `message` | string | Final assistant reply text |
| `model` | string | Model identifier used for the turn |
| `iterations` | integer | Number of tool-use / re-prompt iterations |
| `compact` | boolean | Present and `true` when `--compact` mode was active |
| `auto_compaction` | object\|null | Non-null when auto-compaction fired (see below) |
| `tool_uses` | array | Tool calls made during the turn (TODO: verify schema) |
| `tool_results` | array | Results returned to the model (TODO: verify schema) |
| `prompt_cache_events` | array | Cache-hit/miss events (TODO: verify schema) |
| `usage.input_tokens` | integer | Input tokens billed |
| `usage.output_tokens` | integer | Output tokens billed |
| `usage.cache_creation_input_tokens` | integer | Tokens written to prompt cache |
| `usage.cache_read_input_tokens` | integer | Tokens served from prompt cache |
| `estimated_cost` | string | Human-readable USD cost estimate (e.g. `"$0.0123"`) |

#### `auto_compaction` sub-object

```json
{
  "removed_messages": 12,
  "notice": "Auto-compacted: removed 12 messages to free context."
}
```

---

## Error Envelope

When a command fails under `--output-format json`, an error envelope is written
to **stdout** (pinpoint #168c / #288):

```json
{
  "type": "error",
  "error": "<short human-readable reason>",
  "kind": "<snake_case error kind token>",
  "hint": "<optional actionable hint>"
}
```

### Error Envelope Fields

| Field | Type | Description |
|---|---|---|
| `type` | string | Always `"error"` |
| `error` | string | Short prose description of the failure |
| `kind` | string | Machine-readable snake_case token (see §Error Kinds) |
| `hint` | string\|null | Optional remediation hint |

### Error Kinds (selected)

`kind` values are classified by `classify_error_kind()`.  Common tokens include:

- `not_yet_implemented` — command stub not yet shipped
- `config_error` — configuration file parse / validation failure
- `auth_error` — API key or credential problem
- `permission_denied` — tool-use permission denied
- `model_error` — upstream model API error

See pinpoint #266 (typed-error-kind) for the full taxonomy.

---

## Streaming Behavior

`claw` always uses streaming internally (HTTP chunked transfer to the Anthropic
API) but the **JSON output envelope is emitted once**, after the turn completes.
There is no per-token or per-chunk JSON stream exposed to the caller.

In REPL / interactive mode (`claw` with no `-p`) the JSON format applies only to
structured sub-commands, not to the interactive session itself.

---

## Status Snapshot (`claw status`)

```json
{
  "kind": "status",
  "status": "ok",
  "config_load_error": null,
  "model": "claude-opus-4-5",
  "model_source": "config",
  "model_raw": null,
  "permission_mode": "default",
  "usage": {
    "messages": 42,
    "turns": 10,
    "latest_total": 5678,
    "cumulative_input": 12345,
    "cumulative_output": 4567,
    "cumulative_total": 16912,
    "estimated_tokens": 16912
  },
  "workspace": {
    "cwd": "/Users/you/project",
    "project_root": "/Users/you/project",
    "git_branch": "main",
    "git_state": "clean",
    "changed_files": 0
  }
}
```

---

## Related Pinpoints

- **#288** — error-envelope stdout emission contract
- **#266** — typed-error-kind taxonomy
- **#168c** — `--output-format json` routes error envelopes to stdout
- **#247** — JSON envelope field preservation (hint / help text)
