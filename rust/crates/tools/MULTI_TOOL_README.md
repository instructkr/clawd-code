# Multi-Tool Execution & Sub-Agent Delegation

Two complementary features that dramatically reduce latency and token usage when the model needs to perform multiple operations or gather context.

## Feature 1: Parallel Tool Execution

When the model returns multiple tool_use blocks in a single response, read-only tools now execute concurrently instead of sequentially.

### How it works

The `run_turn` loop is refactored into 3 phases:

1. **Pre-hooks + permission checks** (sequential — hooks may mutate state)
2. **Tool execution** (batch — parallel for read-only tools via `std::thread::scope`)
3. **Post-hooks + session updates** (sequential — preserves original ordering)

### Parallel-safe tools

These tools are safe to run concurrently because they only read state and dispatch through the stateless tool registry:

- `read_file`, `glob_search`, `grep_search`
- `WebFetch`, `WebSearch`
- `ToolSearch`, `Skill`
- `LSP`
- `GitStatus`, `GitDiff`, `GitLog`, `GitShow`, `GitBlame`

### Sequential-only tools

Tools that require `&mut self` or have side effects continue to run one at a time:

- `bash`, `write_file`, `edit_file` (side effects)
- `MCP`, `McpAuth`, `RemoteTrigger` (network state)
- `Agent`, `TaskCreate`, `WorkerCreate` (stateful)
- `NotebookEdit`, `REPL`, `PowerShell` (side effects)

### Safety guarantees

- Pre/post hooks always run sequentially
- Permission checks complete before any tool executes
- Tool results are pushed to the session in the original model order
- Falls back to sequential for single-tool batches
- Thread scopes ensure all parallel work completes before `execute_batch` returns

### Impact

For a response with 5 `read_file` calls: **~5x faster** execution. The main model still sees all results in order.

---

## Feature 2: SubAgent Delegation

A `SubAgent` tool that lets the main model delegate multi-step tasks to a fast sub-agent. The sub-agent runs autonomously with its own `ConversationRuntime`, making multiple tool calls without round-tripping through the main model.

### Tool parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `prompt` | string | yes | Task description for the sub-agent |
| `task_type` | string | no | `Explore` (default), `Plan`, or `Verify` |
| `model` | string | no | Override the sub-agent model |

### Task types

| Type | Available tools | Use for |
|------|----------------|---------|
| `Explore` | read_file, glob_search, grep_search, WebFetch, WebSearch, ToolSearch, StructuredOutput | Searching code, reading files, gathering context |
| `Plan` | Explore + TodoWrite | Planning approaches with structured todo output |
| `Verify` | Plan + bash | Running tests, checking builds, verifying changes |

### Configuration

Set the sub-agent model in `~/.claw/settings.json`:

```json
{
  "model": "openai/glm-5.1-fast",
  "subagentModel": "openai/qwen3.6-35b-fast"
}
```

If `subagentModel` is not set, the sub-agent uses the same model as the main session (or the `model` override parameter on the tool call).

### Example

Main model prompt: "Find all Rust files that import `ConversationRuntime` and list their paths"

Without SubAgent: 5–10 sequential tool calls (grep → read each file → summarize)
With SubAgent: 1 SubAgent call → sub-agent does all the work autonomously → returns summary

**Result**: ~10x fewer tokens consumed by the main model, faster overall completion.

### Architecture

The sub-agent reuses the same building blocks as the existing `Agent` tool:

- `ProviderRuntimeClient` — API client with fallback chain
- `SubagentToolExecutor` — filtered tool access with permission enforcement
- `ConversationRuntime` — full conversation loop with hooks and compaction
- `agent_permission_policy()` — auto-approve read-only, deny write tools

Key differences from the `Agent` tool:
- **Synchronous** — blocks until complete, returns result directly
- **Lighter** — fewer default tools, focused on the task type
- **Configurable model** — uses `subagentModel` or the tool's `model` param
- **Structured output** — returns `result`, `tool_calls`, and `iterations`

---

## Changed files

| File | Changes |
|------|---------|
| `rust/crates/runtime/src/conversation.rs` | `ToolCall`, `ToolResult` types; `execute_batch` on `ToolExecutor`; 3-phase `run_turn` |
| `rust/crates/runtime/src/lib.rs` | Exports for `ToolCall`, `ToolResult` |
| `rust/crates/runtime/src/config.rs` | `subagent_model` field, `parse_optional_subagent_model()`, accessor |
| `rust/crates/runtime/src/config_validate.rs` | `subagentModel` field spec |
| `rust/crates/rusty-claude-cli/src/main.rs` | `CliToolExecutor::execute_batch` with parallel-safe classification |
| `rust/crates/tools/src/lib.rs` | `SubAgent` tool spec, `SubAgentInput`, `run_sub_agent()`, `load_subagent_model_from_config()`, `build_sub_agent_system_prompt()` |
