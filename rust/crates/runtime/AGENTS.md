# AGENTS.md — runtime crate

## OVERVIEW

Core crate of `claw`: session persistence, permissions, prompt assembly, MCP plumbing, tool-facing file ops, conversation loop. 47 flat modules in src/, ~330 pub symbols, ~170 re-exported flat from lib.rs.

## WHERE TO LOOK

| Group | Files | Entry points |
|---|---|---|
| session/conversation | session.rs, session_control.rs, conversation.rs, compact.rs, summary_compression.rs, usage.rs | `Session` (L117), `SessionStore`, `ConversationRuntime` (L130), `ApiClient`/`ToolExecutor` traits |
| config | config.rs, config_validate.rs, bootstrap.rs | `ConfigLoader` (L409), type-export heaviest file; MCP server config enums live here |
| MCP (6-file split) | mcp.rs, mcp_client.rs, mcp_stdio.rs, mcp_server.rs, mcp_tool_bridge.rs, mcp_lifecycle_hardened.rs | `McpServerManager` (L488 in mcp_stdio.rs), JSON-RPC spawn |
| hooks/plugins | hooks.rs, plugin_lifecycle.rs | `HookRunner` (L155), abort signal, healthcheck, degraded mode |
| permissions/safety | permissions.rs, permission_enforcer.rs, policy_engine.rs, approval_tokens.rs, sandbox.rs, bash_validation.rs, trust_resolver.rs | `PermissionEnforcer` (L27), `GreenLevel`, lane decisions |
| tools/execution | bash.rs, file_ops.rs, lsp_client.rs | `execute_bash`, `*_in_workspace` file op variants |
| lane/worker | lane_events.rs, worker_boot.rs, task_packet.rs, task_registry.rs, team_cron_registry.rs, branch_lock.rs, stale_base.rs, stale_branch.rs | `LaneEvent` dedupe/provenance, `LaneBoard` |
| prompt | prompt.rs | `SystemPromptBuilder`, `ContextFile`, dynamic boundary marker |
| git/remote/auth | git_context.rs, remote.rs, oauth.rs | Upstream proxy, PKCE flow |
| misc | json.rs, sse.rs, g004_conformance.rs, green_contract.rs, recovery_recipes.rs, report_schema.rs, trident.rs | Report v1 + redaction |

Largest files by line count: config.rs (3894), mcp_stdio.rs (2969), lane_events.rs (2561), worker_boot.rs (2441), session.rs (1961), conversation.rs (1878).

## CONVENTIONS

- One file per module, flat layout. No subdirectories.
- Most modules are private `mod x` with selective `pub use`. 21 modules are `pub mod`, so consumers use both the re-export and the qualified path.
- Deps kept minimal: serde, tokio, glob, regex, sha2, walkdir + internal plugins/telemetry. No reqwest, no async-trait. Remote/SSE done by hand.
- Inline `#[cfg(test)]` tests per file. session.rs has two test modules.
- `pub(crate) test_env_lock()` mutex in lib.rs serializes env-mutating tests. Use it when touching env vars.
- trust_resolver.rs is `#[cfg(test)]`-gated yet pub-used: test-only API surface.

## INVARIANTS (do not break)

1. **Compaction pairs**: compact.rs must never split assistant(ToolUse)/ToolResult pairs.
2. **No side effects on construction**: SessionStore construction must not create `.claw` directories (session_control.rs:1090).
3. **Workspace containment**: file_ops.rs workspace ops must not escape the workspace root.
4. **Permission ordering**: a leading read-only permission token must not launder a trailing destructive one (permission_enforcer.rs:450).

## ANTI-PATTERNS

- Don't extend whole-file `#![allow(...)]` blocks. They exist as legacy tolerance in worker_boot.rs, mcp_tool_bridge.rs, lsp_client.rs, stale_branch.rs, stale_base.rs, recovery_recipes.rs, mcp_lifecycle_hardened.rs, session_control.rs. Adding new ones is not acceptable.
- Don't add reqwest or async-trait as deps. Remote calls go through the manual SSE/proxy layer in remote.rs and sse.rs.
- Don't create subdirectories under src/. The flat module layout is intentional.
- Don't bypass `*_in_workspace` variants for file ops when running inside a workspace context. The unchecked versions exist for bootstrap and out-of-workspace scenarios only.
- Don't add new `pub mod` exports without reason. Prefer private mod + selective `pub use` from lib.rs.
