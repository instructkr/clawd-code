# Error Handling for Claw Code Claws

**Purpose:** Build a unified error handler for orchestration code using claw-code as a library or subprocess.

After cycles #178–#179 (parser-front-door hole closure), claw-code's error interface is deterministic, machine-readable, and clawable: **one error handler for all 14 clawable commands.**

---

## Quick Reference: Exit Codes and Envelopes

Every clawable command returns JSON on stdout when `--output-format json` is requested.

| Exit Code | Meaning | Response Format | Example |
|---|---|---|---|
| **0** | Success | `{success fields}` | `{"session_id": "...", "loaded": true}` |
| **1** | Error / Not Found | `{error: {kind, message, ...}}` | `{"error": {"kind": "session_not_found", ...}}` |
| **2** | Timeout | `{final_stop_reason: "timeout", final_cancel_observed: ...}` | `{"final_stop_reason": "timeout", ...}` |

---

## One-Handler Pattern

Build a single error-recovery function that works for all 14 clawable commands:

```python
import subprocess
import json
import sys
from typing import Any

def run_claw_command(command: list[str], timeout_seconds: float = 30.0) -> dict[str, Any]:
    """
    Run a clawable claw-code command and handle errors uniformly.
    
    Args:
        command: Full command list, e.g. ["claw", "load-session", "id", "--output-format", "json"]
        timeout_seconds: Wall-clock timeout
    
    Returns:
        Parsed JSON result from stdout
    
    Raises:
        ClawError: Classified by error.kind (parse, session_not_found, runtime, timeout, etc.)
    """
    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired:
        raise ClawError(
            kind='subprocess_timeout',
            message=f'Command exceeded {timeout_seconds}s wall-clock timeout',
            retryable=True,  # Caller's decision; subprocess timeout != engine timeout
        )
    
    # Parse JSON (valid for all success/error/timeout paths in claw-code)
    try:
        envelope = json.loads(result.stdout)
    except json.JSONDecodeError as err:
        raise ClawError(
            kind='parse_failure',
            message=f'Command output is not JSON: {err}',
            hint='Check that --output-format json is being passed',
            retryable=False,
        )
    
    # Classify by exit code and error.kind
    match (result.returncode, envelope.get('error', {}).get('kind')):
        case (0, _):
            # Success
            return envelope
        
        case (1, 'parse'):
            # #179: argparse error — typically a typo or missing required argument
            raise ClawError(
                kind='parse',
                message=envelope['error']['message'],
                hint=envelope['error'].get('hint'),
                retryable=False,  # Typos don't fix themselves
            )
        
        case (1, 'session_not_found'):
            # Common: load-session on nonexistent ID
            raise ClawError(
                kind='session_not_found',
                message=envelope['error']['message'],
                session_id=envelope.get('session_id'),
                retryable=False,  # Session won't appear on retry
            )
        
        case (1, 'filesystem'):
            # Directory missing, permission denied, disk full
            raise ClawError(
                kind='filesystem',
                message=envelope['error']['message'],
                retryable=True,  # Might be transient (disk space, NFS flake)
            )
        
        case (1, 'runtime'):
            # Generic engine error (unexpected exception, malformed input, etc.)
            raise ClawError(
                kind='runtime',
                message=envelope['error']['message'],
                retryable=envelope['error'].get('retryable', False),
            )
        
        case (1, _):
            # Catch-all for any new error.kind values
            raise ClawError(
                kind=envelope['error']['kind'],
                message=envelope['error']['message'],
                retryable=envelope['error'].get('retryable', False),
            )
        
        case (2, _):
            # Timeout (engine was asked to cancel and had fair chance to observe)
            cancel_observed = envelope.get('final_cancel_observed', False)
            raise ClawError(
                kind='timeout',
                message=f'Turn exceeded timeout (cancel_observed={cancel_observed})',
                cancel_observed=cancel_observed,
                retryable=True,  # Caller can retry with a fresh session
                safe_to_reuse_session=(cancel_observed is True),
            )
        
        case (exit_code, _):
            # Unexpected exit code
            raise ClawError(
                kind='unexpected_exit_code',
                message=f'Unexpected exit code {exit_code}',
                retryable=False,
            )


class ClawError(Exception):
    """Unified error type for claw-code commands."""
    
    def __init__(
        self,
        kind: str,
        message: str,
        hint: str | None = None,
        retryable: bool = False,
        cancel_observed: bool = False,
        safe_to_reuse_session: bool = False,
        session_id: str | None = None,
    ):
        self.kind = kind
        self.message = message
        self.hint = hint
        self.retryable = retryable
        self.cancel_observed = cancel_observed
        self.safe_to_reuse_session = safe_to_reuse_session
        self.session_id = session_id
        super().__init__(self.message)
    
    def __str__(self) -> str:
        parts = [f"{self.kind}: {self.message}"]
        if self.hint:
            parts.append(f"Hint: {self.hint}")
        if self.retryable:
            parts.append("(retryable)")
        if self.cancel_observed:
            parts.append(f"(safe_to_reuse_session={self.safe_to_reuse_session})")
        return "\n".join(parts)
```

---

## Practical Recovery Patterns

### Pattern 1: Retry on transient errors

```python
from time import sleep

def run_with_retry(
    command: list[str],
    max_attempts: int = 3,
    backoff_seconds: float = 0.5,
) -> dict:
    """Retry on transient errors (filesystem, timeout)."""
    for attempt in range(1, max_attempts + 1):
        try:
            return run_claw_command(command)
        except ClawError as err:
            if not err.retryable:
                raise  # Non-transient; fail fast
            
            if attempt == max_attempts:
                raise  # Last attempt; propagate
            
            print(f"Attempt {attempt} failed ({err.kind}); retrying in {backoff_seconds}s...", file=sys.stderr)
            sleep(backoff_seconds)
            backoff_seconds *= 1.5  # exponential backoff
    
    raise RuntimeError("Unreachable")
```

### Pattern 2: Reuse session after timeout (if safe)

```python
def run_with_timeout_recovery(
    command: list[str],
    timeout_seconds: float = 30.0,
    fallback_timeout: float = 60.0,
) -> dict:
    """
    On timeout, check cancel_observed. If True, the session is safe for retry.
    If False, the session is potentially wedged; use a fresh one.
    """
    try:
        return run_claw_command(command, timeout_seconds=timeout_seconds)
    except ClawError as err:
        if err.kind != 'timeout':
            raise
        
        if err.safe_to_reuse_session:
            # Engine saw the cancel signal; safe to reuse this session with a larger timeout
            print(f"Timeout observed (cancel_observed=true); retrying with {fallback_timeout}s...", file=sys.stderr)
            return run_claw_command(command, timeout_seconds=fallback_timeout)
        else:
            # Engine didn't see the cancel signal; session may be wedged
            print(f"Timeout not observed (cancel_observed=false); session is potentially wedged", file=sys.stderr)
            raise  # Caller should allocate a fresh session
```

### Pattern 3: Detect parse errors (typos in command-line construction)

```python
def validate_command_before_dispatch(command: list[str]) -> None:
    """
    Dry-run with --help to detect obvious syntax errors before dispatching work.
    
    This is cheap (no API call) and catches typos like:
    - Unknown subcommand: `claw typo-command`
    - Unknown flag: `claw bootstrap --invalid-flag`
    - Missing required argument: `claw load-session` (no session_id)
    """
    help_cmd = command + ['--help']
    try:
        result = subprocess.run(help_cmd, capture_output=True, timeout=2.0)
        if result.returncode != 0:
            print(f"Warning: {' '.join(help_cmd)} returned {result.returncode}", file=sys.stderr)
            print("(This doesn't prove the command is invalid, just that --help failed)", file=sys.stderr)
    except subprocess.TimeoutExpired:
        pass  # --help shouldn't hang, but don't block on it
```

### Pattern 4: Log and forward errors to observability

```python
import logging

logger = logging.getLogger(__name__)

def run_claw_with_logging(command: list[str]) -> dict:
    """Run command and log errors for observability."""
    try:
        result = run_claw_command(command)
        logger.info(f"Claw command succeeded: {' '.join(command)}")
        return result
    except ClawError as err:
        logger.error(
            "Claw command failed",
            extra={
                'command': ' '.join(command),
                'error_kind': err.kind,
                'error_message': err.message,
                'retryable': err.retryable,
                'cancel_observed': err.cancel_observed,
            },
        )
        raise
```

---

## Error Kinds (Enumeration)

After cycles #178–#179, the complete set of `error.kind` values is:

| Kind | Exit Code | Meaning | Retryable | Notes |
|---|---|---|---|---|
| **parse** | 1 | Argparse error (unknown command, missing arg, invalid flag) | No | Real error message included (#179); valid choices list for discoverability |
| **session_not_found** | 1 | load-session target doesn't exist | No | session_id and directory included in envelope |
| **filesystem** | 1 | Directory missing, permission denied, disk full | Yes | Transient issues (disk space, NFS flake) can be retried |
| **runtime** | 1 | Engine error (unexpected exception, malformed input) | Depends | `error.retryable` field in envelope specifies |
| **timeout** | 2 | Engine timeout with cooperative cancellation | Yes* | `cancel_observed` field signals session safety (#164) |

*Retry safety depends on `cancel_observed`:
- `cancel_observed=true` → session is safe to reuse
- `cancel_observed=false` → session may be wedged; allocate fresh one

---

## What We Did to Make This Work

### Cycle #178: Parse-Error Envelope

**Problem:** `claw nonexistent --output-format json` returned argparse help text on stderr instead of an envelope.
**Solution:** Catch argparse `SystemExit` in JSON mode and emit a structured error envelope.
**Benefit:** Claws no longer need to parse human help text to understand parse errors.

### Cycle #179: Stderr Hygiene + Real Error Message

**Problem:** Even after #178, argparse usage was leaking to stderr AND the envelope message was generic ("invalid command or argument").
**Solution:** Monkey-patch `parser.error()` in JSON mode to raise an internal exception, preserving argparse's real message verbatim. Suppress stderr entirely in JSON mode.
**Benefit:** Claws see one stream (stdout), one envelope, and real error context (e.g., "invalid choice: typo (choose from ...)") for discoverability.

### Contract: #164 Stage B (`cancel_observed` field)

**Problem:** Timeout results didn't signal whether the engine actually observed the cancellation request.
**Solution:** Add `cancel_observed: bool` field to timeout TurnResult; signal true iff the engine had a fair chance to observe the cancel event.
**Benefit:** Claws can decide "retry with fresh session" vs "reuse this session with larger timeout" based on a single boolean.

---

## Common Mistakes to Avoid

❌ **Don't parse exit code alone**  
```python
# BAD: Exit code 1 could mean parse error, not-found, filesystem, or runtime
if result.returncode == 1:
    # What should I do? Unclear.
    pass
```

✅ **Do parse error.kind**  
```python
# GOOD: error.kind tells you exactly how to recover
match envelope['error']['kind']:
    case 'parse': ...
    case 'session_not_found': ...
    case 'filesystem': ...
```

---

❌ **Don't capture both stdout and stderr and assume they're separate concerns**  
```python
# BAD (pre-#179): Capture stdout + stderr, then parse stdout as JSON
# But stderr might contain argparse noise that you have to string-match
result = subprocess.run(..., capture_output=True, text=True)
if "invalid choice" in result.stderr:
    # ... custom error handling
```

✅ **Do silence stderr in JSON mode**  
```python
# GOOD (post-#179): In JSON mode, stderr is guaranteed silent
# Envelope on stdout is your single source of truth
result = subprocess.run(..., capture_output=True, text=True)
envelope = json.loads(result.stdout)  # Always valid in JSON mode
```

---

❌ **Don't retry on parse errors**  
```python
# BAD: Typos don't fix themselves
error_kind = envelope['error']['kind']
if error_kind == 'parse':
    retry()  # Will fail again
```

✅ **Do check retryable before retrying**  
```python
# GOOD: Let the error tell you
error = envelope['error']
if error.get('retryable', False):
    retry()
else:
    raise
```

---

❌ **Don't reuse a session after timeout without checking cancel_observed**  
```python
# BAD: Reuse session = potential wedge
result = run_claw_command(...)  # times out
# ... later, reuse same session
result = run_claw_command(...)  # might be stuck in the previous turn
```

✅ **Do allocate a fresh session if cancel_observed=false**  
```python
# GOOD: Allocate fresh session if wedge is suspected
try:
    result = run_claw_command(...)
except ClawError as err:
    if err.cancel_observed:
        # Safe to reuse
        result = run_claw_command(...)
    else:
        # Allocate fresh session
        fresh_session = create_session()
        result = run_claw_command_in_session(fresh_session, ...)
```

---

## Testing Your Error Handler

```python
def test_error_handler_parse_error():
    """Verify parse errors are caught and classified."""
    try:
        run_claw_command(['claw', 'nonexistent', '--output-format', 'json'])
        assert False, "Should have raised ClawError"
    except ClawError as err:
        assert err.kind == 'parse'
        assert 'invalid choice' in err.message.lower()
        assert err.retryable is False

def test_error_handler_timeout_safe():
    """Verify timeout with cancel_observed=true marks session as safe."""
    # Requires a live claw-code server; mock this test
    try:
        run_claw_command(
            ['claw', 'turn-loop', '"x"', '--timeout-seconds', '0.0001'],
            timeout_seconds=2.0,
        )
        assert False, "Should have raised ClawError"
    except ClawError as err:
        assert err.kind == 'timeout'
        assert err.safe_to_reuse_session is True  # cancel_observed=true

def test_error_handler_not_found():
    """Verify session_not_found is clearly classified."""
    try:
        run_claw_command(['claw', 'load-session', 'nonexistent', '--output-format', 'json'])
        assert False, "Should have raised ClawError"
    except ClawError as err:
        assert err.kind == 'session_not_found'
        assert err.retryable is False
```

---

## Appendix: SCHEMAS.md Error Shape

For reference, the canonical JSON error envelope shape (SCHEMAS.md):

```json
{
  "timestamp": "2026-04-22T11:40:00Z",
  "command": "load-session",
  "exit_code": 1,
  "output_format": "json",
  "schema_version": "1.0",
  "error": {
    "kind": "session_not_found",
    "operation": "session_store.load_session",
    "target": "nonexistent",
    "retryable": false,
    "message": "session 'nonexistent' not found in .port_sessions",
    "hint": "use 'list-sessions' to see available sessions"
  }
}
```

All commands that emit errors follow this shape (with error.kind varying). See `SCHEMAS.md` for the complete contract.

---

## Summary

After cycles #178–#179, **one error handler works for all 14 clawable commands.** No more string-matching, no more stderr parsing, no more exit-code ambiguity. Just parse the JSON, check `error.kind`, and decide: retry, escalate, or reuse session (if safe).

The handler itself is ~80 lines of Python; the patterns are reusable across any language that can speak JSON.
