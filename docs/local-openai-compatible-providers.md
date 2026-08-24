# Local OpenAI-compatible providers and skills setup

This guide covers two common offline/local workflows:

1. running Claw against an OpenAI-compatible local model server such as Ollama, llama.cpp, or vLLM; and
2. installing local skills from disk so Claw can discover them without network access.

## Claw is not Claude-only

Claw Code is a Claude-Code-shaped workflow/runtime, not a Claude-only product. It supports Anthropic directly and can target OpenAI-compatible, provider-routed, and local models depending on configuration. Non-Claude providers are supported honestly: they may require stricter tool-call and response-shape compatibility, and some slash/tool workflows can be rougher than first-party Anthropic/OpenAI paths. Provider-specific identity leaks are bugs, not intended product positioning.

If you need the most polished daily-driver experience for a specific non-Claude model today, compare that provider’s native tools. If you need runtime/provider hackability, Claw’s OpenAI-compatible route is the intended extension path.

## OpenAI-compatible routing basics

Set `OPENAI_BASE_URL` to the server’s `/v1` endpoint and set `OPENAI_API_KEY` to either the required token or a harmless placeholder for local servers that expect an Authorization header. Authless local/private OpenAI-compatible servers can leave `OPENAI_API_KEY` unset. The model name must match what the server exposes.

```bash
export OPENAI_BASE_URL="http://127.0.0.1:11434/v1"
export OPENAI_API_KEY="local-dev-token"
claw --model "qwen3:latest" prompt "Reply exactly HELLO_WORLD_123"
```

Routing notes:

- Use the `openai/` prefix for OpenAI-compatible gateways when you need prefix routing to win over ambient Anthropic credentials, for example `--model "openai/gpt-4.1-mini"` with OpenRouter.
- For local servers, prefer the exact model ID reported by the server (`qwen3:latest`, `llama3.2`, etc.). If your local gateway exposes slash-containing IDs, prefix the exact slug with `local/` so Claw routes through OpenAI-compatible transport while sending the rest verbatim, for example `--model "local/Qwen/Qwen2.5-Coder-7B-Instruct"`.
- If you have multiple provider keys in your environment, `OPENAI_BASE_URL` plus local-looking tags such as `llama3.2` or `qwen2.5-coder:7b` selects the local OpenAI-compatible route; use `local/` for slash-containing local IDs.
- Tool workflows need model/server support for OpenAI-compatible tool calls. Plain prompt smoke tests can pass even when slash/tool workflows still fail because the server returns an incompatible tool-call shape.

## Raw `/v1/chat/completions` smoke test

Before debugging Claw, verify the local server speaks the expected wire format:

```bash
curl -sS "$OPENAI_BASE_URL/chat/completions" \
  -H "Authorization: Bearer ${OPENAI_API_KEY:-local-dev-token}" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3:latest",
    "messages": [{"role": "user", "content": "Reply exactly HELLO_WORLD_123"}],
    "stream": false
  }'
```

Expected result: a JSON response with one assistant message containing `HELLO_WORLD_123`. If this fails, fix the local server, model name, or auth token before changing Claw settings.

## Ollama

Start Ollama and pull a model:

```bash
ollama pull qwen3:latest
ollama serve
```

In another shell:

```bash
export OLLAMA_HOST="http://127.0.0.1:11434"
claw --model "qwen3:latest" prompt "Reply exactly HELLO_WORLD_123"
```

`OLLAMA_HOST` is the preferred env var for Ollama. Claw routes all models to the local OpenAI-compatible endpoint automatically when this is set, and no API key is needed. The older `OPENAI_BASE_URL` + `OPENAI_API_KEY` workaround is also supported for existing setups.

If Ollama is running without auth, `unset OPENAI_API_KEY` is acceptable. Use a placeholder token rather than a real cloud API key if your local server requires an Authorization header.

## llama.cpp server

Start a llama.cpp OpenAI-compatible server with the model name you want Claw to send:

```bash
llama-server -m ./models/qwen2.5-coder.gguf --host 127.0.0.1 --port 8080 --alias qwen2.5-coder
```

Then smoke-test through Claw:

```bash
export OPENAI_BASE_URL="http://127.0.0.1:8080/v1"
export OPENAI_API_KEY="local-dev-token"
claw --model "qwen2.5-coder" prompt "Reply exactly HELLO_WORLD_123"
```

## vLLM or another OpenAI-compatible server

Start vLLM with an OpenAI-compatible API server:

```bash
vllm serve Qwen/Qwen2.5-Coder-7B-Instruct --host 127.0.0.1 --port 8000
```

Then route Claw to it:

```bash
export OPENAI_BASE_URL="http://127.0.0.1:8000/v1"
export OPENAI_API_KEY="local-dev-token"
claw --model "Qwen/Qwen2.5-Coder-7B-Instruct" prompt "Reply exactly HELLO_WORLD_123"
```

## mlx-lm (Apple Silicon)

On Apple Silicon, [mlx-lm](https://github.com/ml-explore/mlx-lm) gives meaningfully faster inference than llama.cpp-based backends for models under roughly 14B parameters.

Install and start the server:

```bash
pipx install mlx-lm
mlx_lm.server --model mlx-community/Qwen2.5-Coder-7B-Instruct-4bit --port 8080
```

Then route Claw to it:

```bash
export OPENAI_BASE_URL="http://127.0.0.1:8080/v1"
export OPENAI_API_KEY="local-dev-token"
claw --model "mlx-community/Qwen2.5-Coder-7B-Instruct-4bit" prompt "Reply exactly HELLO_WORLD_123"
```

mlx-lm serves models under their full Hugging Face repo ID. Use the exact `id` field from `curl $OPENAI_BASE_URL/models` for `--model`. A bare name like `qwen2.5-coder-7b-instruct` will fail model resolution before the request ever reaches the server.

## Local skills install from disk

Skills are discovered from Claw skill roots such as `.claw/skills/` in a workspace and `~/.claw/skills/` for user-level installs. Legacy `.codex/skills/` roots may also be scanned for compatibility, but new local Claw projects should prefer `.claw/skills/`.

A skill directory should contain a `SKILL.md` file with frontmatter:

```text
my-skill/
└── SKILL.md
```

```markdown
---
name: my-skill
description: Explain when this skill should be used.
---

# My Skill

Instructions for the agent go here.
```

Install a skill from a local path in the interactive REPL:

```text
/skills install /absolute/path/to/my-skill
/skills list
/skills my-skill
```

Or inspect skills from the direct CLI surface:

```bash
claw skills --output-format json
```

Offline install checklist:

- Install the specific skill directory, not only the repository root, unless that repository root itself contains `SKILL.md`.
- Keep the frontmatter `name` aligned with the directory name users will type.
- After installing, run `/skills list` or `claw skills --output-format json` to confirm the discovered name and source path.
- If a skill invocation fails with an HTTP/provider error, the skill may have installed correctly but the current model/provider call failed. Run `claw doctor`, verify provider credentials, and try a simple prompt smoke test before reinstalling the skill.

## Troubleshooting

| Symptom | Check |
|---|---|
| Claw still asks for Anthropic credentials | Use an explicit OpenAI-compatible model route or remove unrelated Anthropic env vars during local smoke tests. |
| `model not found` from local server | Use the exact model ID exposed by Ollama/llama.cpp/vLLM. |
| Plain prompt works but tools fail | Confirm the model/server supports OpenAI-compatible tool calls and response shapes. |
| Skill says installed but `/skills <name>` fails | Check `/skills list` for the discovered name and source; verify provider credentials separately with `claw doctor`. |
| A local docs/log file contains secrets | Redact it before using `@path` file context or attaching it to an issue. |
| `404 Repository Not Found` from huggingface.co when running `claw` | The `--model` value isn't a full Hugging Face repo ID. Use the exact `id` field from `curl $OPENAI_BASE_URL/models`, not a bare model name. |
| mlx-lm output includes a trailing `<|im_end|>`, or generation runs long | Unfixed mlx-lm bug ([#973](https://github.com/ml-explore/mlx-lm/issues/973), closed without a merge). Set `eos_token_id` in the cached `generation_config.json` (or `config.json`) to the real end-of-turn token. |
