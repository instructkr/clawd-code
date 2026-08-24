# Model Compatibility Guide

This document describes model-specific handling in the OpenAI-compatible provider. When adding new models or providers, review this guide to ensure proper compatibility.

## Table of Contents

- [Overview](#overview)
- [Model-Specific Handling](#model-specific-handling)
  - [Kimi Models (is_error Exclusion)](#kimi-models-is_error-exclusion)
  - [Reasoning Models (Tuning Parameter Stripping)](#reasoning-models-tuning-parameter-stripping)
  - [GPT-5 (max_completion_tokens)](#gpt-5-max_completion_tokens)
  - [Qwen and Kimi Models (DashScope Routing)](#qwen-and-kimi-models-dashscope-routing)
  - [Custom Gateway Slugs and Extra Body Parameters](#custom-gateway-slugs-and-extra-body-parameters)
- [Implementation Details](#implementation-details)
- [Adding New Models](#adding-new-models)
- [Testing](#testing)

## Overview

The `openai_compat.rs` provider translates Claude Code's internal message format to OpenAI-compatible chat completion requests. Different models have varying requirements for:

- Tool result message fields (`is_error`)
- Sampling parameters (temperature, top_p, etc.)
- Token limit fields (`max_tokens` vs `max_completion_tokens`)
- Base URL routing
- Provider-specific extra body parameters (`web_search_options`, `parallel_tool_calls`, local-server switches, etc.)
- Provider diagnostics for status/doctor-style surfaces

## Model-Specific Handling

### Kimi Models (is_error Exclusion)

**Affected models:** `kimi-k2.5`, `kimi-k1.5`, `kimi-moonshot`, and any model with `kimi` in the name (case-insensitive)

**Behavior:** The `is_error` field is **excluded** from tool result messages.

**Rationale:** Kimi models (via Moonshot AI and DashScope) reject the `is_error` field with a 400 Bad Request error:
```json
{
  "error": {
    "type": "invalid_request_error",
    "message": "Unknown field: is_error"
  }
}
```

**Detection:**
```rust
fn model_rejects_is_error_field(model: &str) -> bool {
    let lowered = model.to_ascii_lowercase();
    let canonical = lowered.rsplit('/').next().unwrap_or(lowered.as_str());
    canonical.starts_with("kimi")
}
```

**Testing:** See `model_rejects_is_error_field_detects_kimi_models` and related tests in `openai_compat.rs`.

---

### Reasoning Models (Tuning Parameter Stripping)

**Affected models:**
- OpenAI: `o1`, `o1-*`, `o3`, `o3-*`, `o4`, `o4-*`
- xAI: `grok-3-mini`
- Alibaba DashScope: `qwen-qwq-*`, `qwq-*`, `qwen3-*-thinking`

**Behavior:** The following tuning parameters are **stripped** from requests:
- `temperature`
- `top_p`
- `frequency_penalty`
- `presence_penalty`

**Rationale:** Reasoning/chain-of-thought models use fixed sampling strategies and reject these parameters with 400 errors.

**Exception:** `reasoning_effort` is included for compatible models when explicitly set.

**Detection:**
```rust
fn is_reasoning_model(model: &str) -> bool {
    let canonical = model.to_ascii_lowercase()
        .rsplit('/')
        .next()
        .unwrap_or(model);
    canonical.starts_with("o1")
        || canonical.starts_with("o3")
        || canonical.starts_with("o4")
        || canonical == "grok-3-mini"
        || canonical.starts_with("qwen-qwq")
        || canonical.starts_with("qwq")
        || (canonical.starts_with("qwen3") && canonical.contains("-thinking"))
}
```

**Testing:** See `reasoning_model_strips_tuning_params`, `grok_3_mini_is_reasoning_model`, and `qwen_reasoning_variants_are_detected` tests.

---

### GPT-5 (max_completion_tokens)

**Affected models:** All models starting with `gpt-5`

**Behavior:** Uses `max_completion_tokens` instead of `max_tokens` in the request payload.

**Rationale:** GPT-5 models require the `max_completion_tokens` field. Legacy `max_tokens` causes request validation failures:
```json
{
  "error": {
    "message": "Unknown field: max_tokens"
  }
}
```

**Implementation:**
```rust
let max_tokens_key = if wire_model.starts_with("gpt-5") {
    "max_completion_tokens"
} else {
    "max_tokens"
};
```

**Testing:** See `gpt5_uses_max_completion_tokens_not_max_tokens` and `non_gpt5_uses_max_tokens` tests.

---

### Qwen and Kimi Models (DashScope Routing)

**Affected models:** All models with `qwen` or `kimi` prefixes, including `qwen/`, `qwen-`, `kimi/`, and `kimi-` forms.

**Behavior:** Routed to DashScope (`https://dashscope.aliyuncs.com/compatible-mode/v1`) rather than ambient-credential fallback providers. Known routing prefixes are stripped before sending the wire model.

**Rationale:** Qwen and Kimi compatible-mode models are hosted through Alibaba Cloud's DashScope service, not OpenAI or Anthropic.

**Configuration:**
```rust
pub const DEFAULT_DASHSCOPE_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
```

**Authentication:** Uses `DASHSCOPE_API_KEY` environment variable.

**Note:** Some Qwen models are also reasoning models (see [Reasoning Models](#reasoning-models-tuning-parameter-stripping) above) and receive both treatments.


---

### Custom Gateway Slugs and Extra Body Parameters

**Affected models:** Slash-containing model IDs routed through the OpenAI-compatible provider, especially custom gateways configured with `OPENAI_BASE_URL` such as OpenRouter, local routers, or other `/v1/chat/completions` services.

**Behavior:**
- The default OpenAI API and local/private OpenAI-compatible base URLs treat `openai/` as a routing prefix and send the bare model name on the wire.
- Non-local custom OpenAI-compatible base URLs preserve slash-containing slugs such as `openai/gpt-4.1-mini` so gateways like OpenRouter receive the exact model ID they expect. Local slash-containing model IDs can use `local/`, which strips only that escape-hatch prefix and sends the remainder verbatim.
- `MessageRequest::extra_body` passes through custom request JSON after core fields are populated. This supports provider-specific options such as `web_search_options` and `parallel_tool_calls`.
- Protected core fields (`model`, `messages`, `stream`, `tools`, `tool_choice`, `max_tokens`, `max_completion_tokens`) cannot be overridden through `extra_body`.

**Testing:** See `custom_openai_gateway_preserves_slash_model_ids_and_extra_body_params` in `openai_compat_integration.rs`, `wire_model_strips_openai_prefix_for_default_and_local_preserves_custom_gateways`, `local_routing_prefix_strips_only_escape_hatch`, and `extra_body_params_are_passed_through_without_overriding_core_fields` in `openai_compat.rs`.

## Implementation Details

### File Location
All model-specific logic is in:
```
rust/crates/api/src/providers/openai_compat.rs
```

### Key Functions

| Function | Purpose |
|----------|---------|
| `model_rejects_is_error_field()` | Detects models that don't support `is_error` in tool results |
| `is_reasoning_model()` | Detects reasoning models that need tuning param stripping |
| `translate_message()` | Converts internal messages to OpenAI format (applies `is_error` logic) |
| `build_chat_completion_request()` | Constructs full request payload (applies all model-specific logic and safe `extra_body` passthrough) |
| `provider_diagnostics_for_model()` | Produces provider/status diagnostics including auth/base-url vars, reasoning behavior, proxy support, extra-body support, and slash-model preservation |

### Provider Prefix Handling

All model detection functions strip provider prefixes (e.g., `dashscope/kimi-k2.5` → `kimi-k2.5`) before matching:

```rust
let canonical = model.to_ascii_lowercase()
    .rsplit('/')
    .next()
    .unwrap_or(model);
```

This ensures consistent detection regardless of whether models are referenced with or without provider prefixes. Wire-model handling is more specific: known routing prefixes are stripped for provider-native defaults, while custom OpenAI-compatible base URLs preserve slash-containing gateway slugs.

## Adding New Models

When adding support for new models:

1. **Check if the model is a reasoning model**
   - Does it reject temperature/top_p parameters?
   - Add to `is_reasoning_model()` detection

2. **Check tool result compatibility**
   - Does it reject the `is_error` field?
   - Add to `model_rejects_is_error_field()` detection

3. **Check token limit field**
   - Does it require `max_completion_tokens` instead of `max_tokens`?
   - Update the `max_tokens_key` logic

4. **Check custom gateway behavior**
   - Should slash-containing IDs be preserved for custom `OPENAI_BASE_URL` gateways?
   - Does the feature belong in a typed request field or `extra_body` passthrough?

5. **Add tests**
   - Unit test for detection function
   - Integration test in `build_chat_completion_request`

6. **Update this documentation**
   - Add the model to the affected lists
   - Document any special behavior

## Testing

### Running Model-Specific Tests

```bash
# All OpenAI compatibility tests
cargo test --package api providers::openai_compat

# Specific test categories
cargo test --package api model_rejects_is_error_field
cargo test --package api reasoning_model
cargo test --package api gpt5
cargo test --package api qwen
cargo test --package api custom_openai_gateway_preserves_slash_model_ids_and_extra_body_params
cargo test --package api provider_diagnostics_explain_openai_compatible_capabilities
```

### Test Files

- Unit tests: `rust/crates/api/src/providers/openai_compat.rs` (in `mod tests`)
- Integration tests: `rust/crates/api/tests/openai_compat_integration.rs`

### Verifying Model Detection

To verify a model is detected correctly without making API calls:

```rust
#[test]
fn my_new_model_is_detected() {
    // is_error handling
    assert!(model_rejects_is_error_field("my-model"));
    
    // Reasoning model detection
    assert!(is_reasoning_model("my-model"));
    
    // Provider prefix handling
    assert!(model_rejects_is_error_field("provider/my-model"));
}
```

---

*Last updated: 2026-05-15*

For questions or updates, see the implementation in `rust/crates/api/src/providers/openai_compat.rs`.
