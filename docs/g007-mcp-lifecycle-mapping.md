# G007 MCP Lifecycle Mapping

This map captures the current MCP/plugin lifecycle implementation surfaces for the
G007 plugin/MCP maturity lane. It is intentionally evidence-oriented: each row
names the runtime surface, the code owner boundary, and the current gap when the
surface is metadata-only.

## Degraded MCP startup

| Concern | Current surface | Notes |
| --- | --- | --- |
| Best-effort discovery | `rust/crates/runtime/src/mcp_stdio.rs` (`McpServerManager::discover_tools_best_effort`) | Discovers every configured stdio server, keeps tools from working servers, and records per-server failures without aborting the whole startup. |
| Failure payload | `rust/crates/runtime/src/mcp_stdio.rs` (`McpDiscoveryFailure`, `UnsupportedMcpServer`) | Failure records include `server_name`, lifecycle `phase`, `required`, `error`, `recoverable`, and structured `context`. Unsupported non-stdio servers keep `transport`, `required`, and `reason`. |
| Degraded report model | `rust/crates/runtime/src/mcp_lifecycle_hardened.rs` (`McpDegradedReport`, `McpFailedServer`, `McpErrorSurface`) | Normalizes degraded startup into working servers, failed servers, available tools, and missing tools. `McpErrorSurface` carries phase, server, message, context, and recoverability. |
| CLI runtime handoff | `rust/crates/rusty-claude-cli/src/main.rs` (`RuntimeMcpState::new`) | Converts discovery failures and unsupported servers into a runtime degraded report, including `required` in the error context. |

## Required vs. optional MCP servers

| Concern | Current surface | Notes |
| --- | --- | --- |
| Config contract | `rust/crates/runtime/src/config.rs` (`ScopedMcpServerConfig.required`) | `mcpServers.<name>.required` parses as a boolean and defaults to `false`; invalid non-boolean values are rejected by the shared optional-bool parser. |
| Scope merge | `rust/crates/runtime/src/config.rs` (`merge_mcp_servers`) | Requiredness is stored beside the scope and transport-specific config after normal user/project/local merging. |
| Inventory/reporting | `rust/crates/commands/src/lib.rs` (`mcp_server_json`, `render_mcp_server_report`) | JSON reports expose `server.required`; text `show` reports include `Required`. |
| Discovery propagation | `rust/crates/runtime/src/mcp_stdio.rs` | Requiredness is copied into managed stdio servers, unsupported server records, discovery failures, and degraded startup context. |
| Cache/signature identity | `rust/crates/runtime/src/mcp.rs` (`scoped_mcp_config_hash`) | The hash includes `required:<bool>` so required/optional changes affect MCP config identity. |
| Remaining policy gap | runtime behavior | The flag is currently surfaced and propagated as lifecycle metadata. It does not yet fail the whole runtime/session solely because a required server failed; consumers must inspect the degraded report context today. |

## Config interpolation and redaction surfaces

| Concern | Current surface | Notes |
| --- | --- | --- |
| Raw config parsing | `rust/crates/runtime/src/config.rs` (`parse_mcp_server_config`, `parse_mcp_remote_server_config`) | `command`, `args`, `url`, `headers`, and `headersHelper` are loaded as literal strings. No dedicated environment, tilde, or workspace-root interpolation pass is present in this parser. |
| Redacted key reporting | `rust/crates/commands/src/lib.rs` (`mcp_server_details_json`, `render_mcp_server_report`) | Stdio env and remote/websocket header values are not printed; only `env_keys` / `Header keys` are surfaced. |
| Unredacted reporting risk | `rust/crates/commands/src/lib.rs` (`mcp_server_summary`, `mcp_server_details_json`, text `show`) | Command, args, URL, `headers_helper`, OAuth metadata URL/client id, and managed proxy URL/id are currently emitted verbatim. Treat these fields as not-redacted unless a future policy layer classifies them safe. |
| OAuth exposure | `rust/crates/commands/src/lib.rs` (`mcp_oauth_json`, `format_mcp_oauth`) | OAuth secret-like values are mostly absent from the current config model, but client id and metadata URL are still reported directly. |

## Plugin lifecycle contract adjacency

| Concern | Current surface | Notes |
| --- | --- | --- |
| Manifest lifecycle | `rust/crates/plugins/src/lib.rs` (`PluginLifecycle`) | Plugin manifests support `lifecycle.Init` and `lifecycle.Shutdown` command arrays. |
| Registry summary | `rust/crates/plugins/src/lib.rs` (`PluginSummary::lifecycle_state`) | Installed summaries include enabled state, lifecycle commands, and derived lifecycle state (`ready` or `disabled`). Load failures remain first-class in registry reports. |
| CLI JSON output | `rust/crates/rusty-claude-cli/src/main.rs` (`plugin_command_json`) | Plugin command JSON emits top-level `status`, per-plugin `lifecycle_state` and lifecycle command counts, plus `load_failures` with `lifecycle_state: load_failed`. |

## Verification anchors

The current regression anchors for this map are:

- `cargo test -p runtime parses_typed_mcp_and_oauth_config -- --nocapture`
- `cargo test -p runtime manager_discovery_report_keeps_healthy_servers_when_one_server_fails -- --nocapture`
- `cargo test -p runtime manager_records_unsupported_non_stdio_servers_without_panicking -- --nocapture`
- `cargo test -p commands renders_mcp_reports -- --nocapture`
- `cargo test -p plugins installed_plugin_registry_report_collects_load_failures_from_install_root -- --nocapture`
- `cargo test -p rusty-claude-cli --test output_format_contract plugins_json_surfaces_lifecycle_contract_when_plugin_is_installed -- --nocapture`

