# Configuration

claw-code configuration reference. For provider details, see [SUPPORTED_PROVIDERS.md](./SUPPORTED_PROVIDERS.md). For architecture, see [ARCHITECTURE.md](./ARCHITECTURE.md).

## Configuration Sources

claw-code reads configuration from multiple sources (in priority order):

1. **CLI flags** — highest priority (e.g., `--model`, `--max-turns`, `--cwd`)
2. **Environment variables** — `ANTHROPIC_*`, `OPENAI_*`, `XAI_*`, `DASHSCOPE_*`, `CLAW_*`, etc.
3. **settings.json** — `.claw/settings.json` in the project directory, or `~/.claw/settings.json` as a user-level default
4. **Hardcoded defaults** — lowest priority

> **Known issue (#283):** Auto-compaction threshold (`CLAUDE_CODE_AUTO_COMPACT_INPUT_TOKENS`) is env-var-only; no `settings.json` key exists yet.
> **Known issue (#282):** env-vs-config consolidation is incomplete; some settings only work in one source.

## Environment Variables

### Provider Authentication

| Variable | Provider | Notes |
|----------|----------|-------|
| `ANTHROPIC_API_KEY` | Anthropic (Claude models) | Primary credential for Claude |
| `ANTHROPIC_AUTH_TOKEN` | Anthropic | Alternative to `ANTHROPIC_API_KEY` |
| `ANTHROPIC_BASE_URL` | Anthropic | Custom endpoint (e.g., proxy) |
| `OPENAI_API_KEY` | OpenAI-compatible | Required for `gpt-*` / `openai/` models |
| `OPENAI_BASE_URL` | OpenAI-compatible | Custom endpoint (OpenRouter, Ollama, etc.) |
| `XAI_API_KEY` | xAI (Grok models) | Required for `grok-*` models |
| `XAI_BASE_URL` | xAI | Custom endpoint |
| `DASHSCOPE_API_KEY` | DashScope (Qwen/Kimi models) | Required for `qwen-*` / `kimi-*` models |
| `DASHSCOPE_BASE_URL` | DashScope | Custom endpoint |

### Model Selection

| Variable | Default | Description |
|----------|---------|-------------|
| `ANTHROPIC_MODEL` | `claude-sonnet-4-6` | Default model when `--model` flag is not passed |

### Runtime Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `CLAUDE_CODE_AUTO_COMPACT_INPUT_TOKENS` | provider-specific | Auto-compaction trigger threshold (see #283) |
| `CLAW_CONFIG_HOME` | `~/.claw` | Override config directory location |
| `CLAWD_WEB_SEARCH_BASE_URL` | (built-in) | Custom base URL for web search tool |
| `CLAWD_TODO_STORE` | `~/.claw/todos` | Override todo storage path |
| `CLAWD_AGENT_STORE` | `~/.claw/agents` | Override agent store path |
| `RUST_LOG` | `info` | Log verbosity (`trace`/`debug`/`info`/`warn`/`error`) |

**Related paths also respected:** `CODEX_HOME`, `CLAUDE_CONFIG_DIR` (legacy compatibility).

## settings.json

Located at `.claw/settings.json` (project-local) or `~/.claw/settings.json` (user-level). Project-local takes precedence over user-level.

Example:

```json
{
  "model": "claude-sonnet-4-6"
}
```

`claw /config` shows the merged, resolved configuration from all sources.

> **Known gap (#285):** No declarative `providers` or `models` block in `settings.json`. Provider selection is currently model-prefix-based via a hardcoded `MODEL_REGISTRY`. See [SUPPORTED_PROVIDERS.md](./SUPPORTED_PROVIDERS.md) for the full provider/model matrix.

## Provider Selection

Provider is auto-selected from model name prefix or the `openai/` namespace prefix:

| Model pattern | Provider | Auth env |
|--------------|----------|----------|
| `claude-*` | Anthropic | `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` |
| `gpt-*`, `openai/*` | OpenAI-compatible | `OPENAI_API_KEY` |
| `grok-*` | xAI | `XAI_API_KEY` |
| `qwen-*`, `kimi-*` | DashScope | `DASHSCOPE_API_KEY` |

When `OPENAI_BASE_URL` is set, the OpenAI-compatible provider is preferred for unrecognised model names — useful for Ollama or OpenRouter.

## Session Storage

Sessions are stored in `~/.claw/sessions/<session-id>/` (or under `CLAW_CONFIG_HOME`). Each session contains:

- Conversation history (messages)
- Session metadata (model, created_at, etc.)
- Tool execution state

See pinpoints #278 (version-comparison) and #279 (unknown-field policy) for known session persistence caveats.

## Related Documents

- [SUPPORTED_PROVIDERS.md](./SUPPORTED_PROVIDERS.md) — Provider/model matrix and auth details
- [ARCHITECTURE.md](./ARCHITECTURE.md) — Crate layout and request flow
- [TROUBLESHOOTING.md](../TROUBLESHOOTING.md) — Failure mitigation
- [ROADMAP.md](../ROADMAP.md) — Pinpoints by cluster
