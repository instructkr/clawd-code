# AGENTS.md — api crate

## OVERVIEW

LLM provider client layer: dispatches Anthropic, xAI, OpenAI, DashScope, and Ollama behind two wire protocols (Anthropic Messages native, OpenAI Chat Completions compat).

## WHERE TO LOOK

| Module | What lives here |
|---|---|
| `client.rs` | `ProviderClient` enum (facade). `from_model(model)` resolves alias, picks `ProviderKind`, handles OLLAMA_HOST and DashScope qwen-prefix cases. `send_message`/`stream_message`. |
| `providers/mod.rs` | `Provider` trait (generic, dead_code-allowed, not used for dispatch). `ProviderKind`, `resolve_model_alias`, `ProviderMetadata` (auth_env, base_url_env, default_base_url), `max_tokens_for_model[_with_override]`, capability/diagnostic reporting, `preflight_message_request` validation. |
| `providers/anthropic.rs` | `AnthropicClient` (re-exported as `ApiClient` at crate root). Dual auth: API key env vs saved OAuth (`AuthSource`, `OAuthTokenSet`, token expiry checks). Base-url resolution. SSE `MessageStream`. Prompt-cache hooks. |
| `providers/openai_compat.rs` | `OpenAiCompatClient` parameterized by `OpenAiCompatConfig` (presets: `xai()`, `openai()`, `dashscope()`, `OLLAMA_CONFIG`). Heavy translation layer: `build_chat_completion_request`, `translate_message`, `sanitize_tool_message_pairing`, `flatten_tool_result_content`. Model-quirk predicates (`is_reasoning_model`, etc.). Body-size estimation/guards. |
| `types.rs` | Provider-agnostic wire types: `MessageRequest`, `InputMessage`, `ContentBlock`, `StreamEvent`, `Usage`, `ToolDefinition`, `ToolChoice`. |
| `sse.rs` | `SseParser`, `parse_frame`. |
| `http_client.rs` | reqwest builders, `ProxyConfig` from env proxy vars, `TimeoutConfig`. |
| `error.rs` | `ApiError`. |
| `prompt_cache.rs` | `PromptCache` + `Stats` (Anthropic-only). |
| `lib.rs` | Curated `pub use` lists define the public surface. Also re-exports sibling telemetry crate items. |

## CONVENTIONS

- Module-private by default. `lib.rs` `pub use` lists are the sole public API surface.
- `#[must_use]` on pure constructors.
- Provider config follows an env-var pair pattern: `*_API_KEY` / `*_BASE_URL`, recorded in `ProviderMetadata`.
- Leaf files carry targeted `#![allow(clippy::cast_possible_truncation)]` where needed.
- Dispatch goes through the `ProviderClient` enum, not trait objects. The `Provider` trait exists but is dead-code-allowed.
- Streams unify into `MessageStream` with `next_event()` yielding `StreamEvent`.

## TESTS

- Four integration test files under `tests/`:
  - `client_integration` — core client behavior
  - `openai_compat_integration` — OpenAI-compat translation paths
  - `provider_client_integration` — `ProviderClient` dispatch
  - `proxy_integration` — proxy config
- Tests that touch env vars serialize through a shared `env_lock()` mutex. Don't skip this or you'll get flaky parallel failures.
- `benches/request_building.rs` is the workspace's only Criterion bench. Targets hot translation functions. This file bulk-opts out of strict lints.
