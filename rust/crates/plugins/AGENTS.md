# AGENTS.md — plugins crate

## OVERVIEW

Plugin subsystem: how third-party, builtin, and bundled tools/commands/hooks enter the runtime.

## WHERE TO LOOK

- `src/lib.rs` (~3,863 lines): the bulk of the crate. Manifest parsing (`.claude-plugin/plugin.json`), installed-plugin registry, lifecycle model, permission model, install/update management.
- `src/hooks.rs`: hook event model (`HookEvent`, `HookRunResult`) and `HookRunner` for shell-hook execution. Re-exported from `lib.rs`. **Caution:** the runtime crate has its own `hooks.rs` with a separate `HookRunner` (execution + abort-signal side, around L155). Know which layer you need before editing.
- `src/test_isolation.rs`: test isolation helpers.
- `bundled/`: example plugin fixtures. `example-bundled/` and `sample-hooks/` each contain `.claude-plugin/plugin.json` plus `pre.sh`/`post.sh` shell hooks. Treat these as the reference shape when authoring a new plugin.

## CONVENTIONS

**Key public types** (all in `src/lib.rs` unless noted):

- Kinds/definitions: `PluginKind`, `PluginDefinition`, `BuiltinPlugin`, `BundledPlugin`, `ExternalPlugin`.
- Manifests: `PluginManifest`, `PluginToolManifest`, `PluginToolDefinition`, `PluginToolPermission`, `PluginCommandManifest`.
- Hooks: `PluginHooks`, `HookEvent`, `HookRunResult` (from `hooks.rs`).
- Lifecycle/permissions: `PluginLifecycle`, `PluginPermission`.
- Registry: `InstalledPluginRecord`, `InstalledPluginRegistry`, `RegisteredPlugin`, `PluginRegistry` (+ `Report`, `Summary`, `LoadFailure`).
- Management: `PluginManager` (+ `Config`), `InstallOutcome`, `UpdateOutcome`.
- Trait: `Plugin`.
- Errors: `PluginError`.
- Entry points: `builtin_plugins()`, `load_plugin_from_directory()`.

**Lifecycle spans two crates.** Manifest parsing and registry live here. Health checks, degraded-mode, and `PluginState` live in `runtime/src/plugin_lifecycle.rs`. Changes to plugin lifecycle logic often touch both.

**Plugin shape.** A plugin directory contains `.claude-plugin/plugin.json` at minimum. Shell hooks (`pre.sh`, `post.sh`) sit alongside. See `bundled/` for working examples.

**Consumers.** `PluginManager` has ~48 references across the workspace. CLI wires plugins via `RuntimePluginStateBuildOutput` in `rusty-claude-cli`. The tools crate exposes plugin tools through `GlobalToolRegistry`.

## NOTES

- Don't confuse the two `HookRunner` implementations. This crate's version handles the event model. The runtime crate's version handles execution and abort signals.
- `lib.rs` is large. Most searches for plugin behavior start and end there.
- Bundled plugin fixtures under `bundled/` are used in tests. Breaking their structure breaks CI.
- Permission model is enforced at install time and checked at runtime. Both paths matter when modifying `PluginPermission`.
