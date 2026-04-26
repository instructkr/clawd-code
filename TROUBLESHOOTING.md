# Troubleshooting

## Upstream stream-init failures (`500 empty_stream`)

**Symptom:** claw-code exits with `500 empty_stream: upstream stream closed before first payload` or similar upstream stream-init error.

**Root cause:** Upstream provider (Anthropic, OpenAI, other) closed the HTTP connection before sending the first response payload. Common causes:
- Transient network issue between claw-code and provider
- Provider overload / temporary service degradation
- Authentication token expired or invalid
- Rate limit exceeded (even if not visible in response headers)

**Mitigation:**
1. **Check credentials:** Verify `claw whoami` shows the expected provider and account. Re-authenticate if expired.
2. **Wait and retry:** Provider transient issues usually resolve within 30-60 seconds. Wait a minute, then retry the same command.
3. **Check provider status:** Visit the provider's status page (e.g., status.anthropic.com, status.openai.com).
4. **Reduce request size:** If the prompt is large, try a smaller request first to isolate stream-init from context-window failures.
5. **Check network:** Ensure your network connection is stable. If behind a proxy, verify proxy allows streaming responses.

**When to escalate:**
- If stream-init failures persist >10 minutes across multiple requests
- If `claw whoami` fails to authenticate
- If no provider status page shows degradation

**Related pinpoint:** #290 (typed stream-init failure envelope — future improvement for better diagnostics)

---

## Context-window-blocked errors

**Symptom:** claw-code exits with `context_window_blocked` or similar provider error when resuming a long session, or when sending a request with a very large prompt + accumulated history.

**Root cause:** Session size exceeded provider context window before claw-code's auto-compaction could reduce it. Auto-compaction is currently REACTIVE-AFTER-SUCCESS — it only fires after a successful provider response. If the request itself is oversized, compaction never runs.

**Mitigation:**
1. **Resume with manual compact:** `claw resume <session> --compact-before` (if available); else manually compact via `/compact` slash command before retrying
2. **Start a fresh session:** Sometimes the cleanest path; existing session-state preserved in `~/.claw/sessions/<id>/`
3. **Reduce prompt size:** If interactive, send shorter prompts; truncate file contents before pasting
4. **Adjust threshold:** Lower `CLAW_AUTO_COMPACT_INPUT_TOKENS_THRESHOLD` env var (default varies by provider)

**Related pinpoints:** #287 (auto-compaction reactive-not-preflight, CRITICAL), #283 (threshold env-only no settings.json key), #288 (failure envelope omits diagnostics)

---

## Manual `/compact` reports "session below compaction threshold"

**Symptom:** You run `/compact` to manually compact a session, but it reports `session below compaction threshold` even though the session feels large.

**Root cause:** The "below threshold" message is currently a catch-all for multiple skip reasons:
- Too few compactable messages
- Already compacted (only summary remains)
- Compactable tokens below threshold
- Tool-use/tool-result boundary preserved
- Live vs resume threshold divergence

**Mitigation:**
1. **Check session state:** `claw session info <id>` to inspect message count, total tokens
2. **Force compaction:** Currently no `--force` flag exists; track #289 for typed skip-reason discriminants
3. **Workaround:** Continue session and let auto-compact fire after next provider response (when reactive-after-success path is available)

**Related pinpoint:** #289 (manual `/compact` skip-reason flattened, lacks typed discriminants)

---

## Parallel agent stuck in "running" state

**Symptom:** A parallel agent lane shows `status: running` indefinitely, never transitioning to `completed` or `error`. Downstream coordination treats it as still-working.

**Root cause:** `Agent::execute_agent` writes a `running` manifest BEFORE spawning a detached `std::thread::spawn`. The `JoinHandle` is dropped. If the process crashes during agent execution, the manifest stays as `running` forever (zombie state). No heartbeat or stale-reaper exists.

**Mitigation:**
1. **Manual cleanup:** Inspect `~/.claw/agents/<lane>/` and remove stale `manifest.json` files where last-modified > N minutes ago
2. **Restart agent lane:** `claw agent restart <lane>`
3. **Kill orphaned processes:** `pgrep claw` to find lingering processes

**Related pinpoint:** #286 (Parallel `Agent` detached-thread no-heartbeat no-reaper)

---

## Sustained upstream provider failures (`500 empty_stream` repeating)

**Symptom:** Same upstream provider error (e.g., `500 empty_stream: upstream stream closed before first payload`) repeats 5+ times in <60 minutes. Retries hit the same dead upstream blindly.

**Root cause:** claw-code does NOT detect repeat-failure patterns. No circuit-breaker. No automatic provider-fallback when configured. Each retry attempts the same provider+endpoint regardless of recent failure history.

**Mitigation:**
1. **Manual circuit-breaker:** Wait 5-10 minutes after repeated failures before retrying
2. **Switch provider:** If you have multiple providers configured (`ANTHROPIC_API_KEY` + `OPENAI_API_KEY`), restart with different model prefix (e.g., `gpt-4` instead of `claude-`)
3. **Check provider status pages:** status.anthropic.com, status.openai.com
4. **Verify upstream endpoint:** If using a proxy (CCAPI, custom OpenAI-compatible endpoint), check proxy logs

**Related pinpoints:** #291 (no repeat-failure detection / circuit-breaker), #285 (declarative providers config for fallback), #290 (stream-init failure envelope)

---

## Other common failures

*[placeholder for future sections: tool-use failures, session corruption]*
