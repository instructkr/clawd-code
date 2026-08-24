# AGENTS.md — rust/crates/tools

## OVERVIEW

Single-crate tool surface: registry, 55 tool specs, permission-gated dispatch, all implementations. One flat `src/lib.rs` (10,892 lines, ~37% tests).

## lib.rs MAP

| Lines | Landmark |
|-------------|----------|
| 1–74 | Imports + six `global_*_registry()` OnceLock singletons (Lsp, McpTool, Team, Cron, Task, Worker) |
| 75–483 | Registry API: `ToolManifestEntry`, `ToolSource`, `ToolRegistry`, `ToolSpec`, `GlobalToolRegistry`, `RuntimeToolDefinition`, `canonical_allowed_tool_name` |
| 484–1348 | `mvp_tool_specs()` static table of 55 tools with inline JSON schemas |
| 1349–1524 | `enforce_permission_check` + `execute_tool()` string-match dispatch, permission classification helpers |
| 1525–2735 | `run_*` wrappers: deserialize input, call into `execute_*` or runtime fns |
| 2736–2763 | `workspace_traversal_guard_tests` mod |
| 2764–3354 | ~45 private serde IO structs |
| 3355–6824 | Real implementations: web fetch/search, todo store, skill resolution, agent/subagent spawning (`ProviderRuntimeClient` L5182, `SubagentToolExecutor` L5361), notebook edit, sleep, config, plan-mode, structured output, REPL, PowerShell |
| 6825–6826 | `pub mod lane_completion; pub mod pdf_extract;` |
| 6829–10892 | `mod tests` (~4000 lines) |

## ADDING A TOOL

1. Add a `ToolSpec` entry in `mvp_tool_specs()`. Include `name`, `description`, `input_schema` (inline JSON), and `required_permission: PermissionMode`.
2. Add a dispatch arm in `execute_tool()` matching the tool name string.
3. Write a `run_<tool>()` wrapper. Deserialize input from a dedicated serde struct.
4. Implement the actual logic below L3355 (or call into another crate).
5. Add inline tests in `mod tests`. Follow BDD naming: `given_x_when_y_then_z`.
6. Permission gating is automatic: `GlobalToolRegistry` / `SubagentToolExecutor` hold an optional `PermissionEnforcer` checked pre-dispatch.

## CONVENTIONS

- **Tool boundary signature**: `Result<String, String>`. Always.
- **Tool naming**: snake_case for file/shell tools, PascalCase otherwise. `canonical_allowed_tool_name` normalizes aliases.
- **State**: OnceLock registries for global singletons. JSON state files under config dirs for persistence.
- **Input validation**: reject empty strings for todos, descriptions, prompts, messages, code. ~12 validation sites between L3806–6167. Keep that contract.
- **Test naming**: BDD style (`given_x_when_y_then_z`).
- **Env-mutating tests**: acquire `env_lock()` mutex first.
- **Dependencies**: runtime, api, plugins, commands, reqwest(blocking), aspect-*, tokio.

## ANTI-PATTERNS

- **Don't add more `#[allow(clippy::...)]` suppressions.** ~50 `needless_pass_by_value` and several `too_many_lines` allows exist as legacy debt. Don't extend.
- **Don't skip input validation.** Empty-string rejection is a contract across all user-facing text fields.
- **Don't scatter implementation across new submodules.** The crate is intentionally flat (one lib.rs + two leaf mods). Only `lane_completion` and `pdf_extract` break out.
- **Don't duplicate tool names.** The canonical name mapping already handles aliases.
- **Don't bypass `enforce_permission_check`.** Every tool dispatch goes through permission gating. No exceptions.
