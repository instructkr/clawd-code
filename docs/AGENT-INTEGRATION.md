# Agent Integration Guide

How to consume Claw Code from your agent framework, orchestration tool, or custom code.

---

## Three Integration Paths

| Path | Best for | Latency | Complexity |
|------|----------|---------|------------|
| **Rust SDK** | Native Rust consumers, maximum control | In-process | Low |
| **CLI** | Shell scripts, CI pipelines, quick automation | Process spawn | Minimal |
| **RPC** (planned) | Python/Node/any language, framework adapters | IPC (stdin/stdout) | Medium |

---

## 1. Rust SDK

### Add the dependency

```toml
[dependencies]
sdk = { path = "../claw-code/rust/crates/sdk" }
runtime = { path = "../claw-code/rust/crates/runtime" }
api = { path = "../claw-code/rust/crates/api" }
```

### Create a session

```rust
use sdk::{AgentSession, ToolRegistry, EventBus, create_builtin_tools};
use runtime::PermissionMode;

// Create session with built-in tools
let (mut session, event_bus) = AgentSession::new(
    "claude-sonnet-4-6",
    vec!["You are a coding assistant. Follow the plan precisely.".into()],
    create_builtin_tools(),
    PermissionMode::DangerFullAccess,
)?;

// Subscribe to events
let mut sub = event_bus.subscribe();
```

### Run turns

```rust
// Single turn
let summary = session.run_turn("Read src/main.rs and list all public functions")?;

// Check result
println!("Tokens used: {:?}", summary);
```

### Listen to events

```rust
use sdk::AgentSessionEvent;

loop {
    match sub.try_recv() {
        Some(AgentSessionEvent::TurnCompleted(summary)) => {
            println!("Turn completed: {:?}", summary);
        }
        Some(AgentSessionEvent::ToolExecutionStarted { name }) => {
            println!("Tool started: {}", name);
        }
        Some(AgentSessionEvent::Error(msg)) => {
            eprintln!("Error: {}", msg);
            break;
        }
        None => break, // No more events
        _ => {}
    }
}
```

### Session trees (branching)

```rust
use sdk::{SessionTree, SessionTreeNode};

let mut tree = SessionTree::new();
tree.set_root("root", "user", Some("Initial plan".into()));
tree.add_child("step-1", "root", "assistant", Some("Read codebase".into()))?;
tree.add_child("step-2", "step-1", "assistant", Some("Write tests".into()))?;

// Branch at step-1 to try a different approach
tree.fork_at("step-1", "step-1b")?;
tree.navigate_to("step-1b")?;

// Walk root → active
let path = tree.active_path();
for node in &path {
    println!("{} [{}]: {:?}", node.id, node.role, node.label);
}
```

### Inter-agent coordination

```rust
use sdk::{AgentContext, AgentTask, TaskRegistry};

// Shared context between agents
let ctx = AgentContext::new();
ctx.set("project_root", "/path/to/project");
ctx.set("test_command", "cargo test --workspace");

// Agent A sets a result
ctx.set("analysis_result", "Found 3 modules with missing tests");

// Agent B reads it
let analysis = ctx.get("analysis_result");

// Task tracking
let mut registry = TaskRegistry::new();
registry.register(AgentTask::new("t1", "explore", "Explore codebase structure")
    .with_model("claude-sonnet-4-6")
    .with_tools(vec!["read_file".into(), "glob_search".into()])
    .with_context(ctx.clone()));

registry.complete("t1", "Found 12 source files, 3 test files")?;
```

### Extensions

```rust
use sdk::{Extension, ExtensionRegistry, ToolRegistry, SimpleExtension};

// Simple extension: just adds tools
let ext = SimpleExtension::new("my-tools", vec!["custom_lint".into()]);

let mut registry = ExtensionRegistry::new();
registry.register(Box::new(ext));

// Collect tools from all extensions
let mut tools = ToolRegistry::new();
registry.collect_tools(&mut tools);
```

---

## 2. CLI

The `claw` binary is designed for both interactive use and programmatic consumption.

### Structured JSON output

Every output-producing command supports `--output-format json`:

```bash
# Get structured status
claw --output-format json status

# One-shot prompt with JSON output
claw --output-format json prompt "list all TODO comments in the codebase"
```

### Session management

```bash
# Create and run
claw prompt "implement feature X"

# Resume by ID or "latest"
claw --resume latest
claw --resume abc123

# List sessions
claw --output-format json status  # includes session list
```

### Custom providers

```bash
# Use a model from models.json
claw --model ollama/llama3.1:8b prompt "summarize this project"

# Use OpenAI with custom base URL
OPENAI_BASE_URL=https://openrouter.ai/api/v1 claw --model openai/gpt-4o prompt "hello"

# Use DashScope (Alibaba Qwen)
claw --model qwen-plus prompt "translate to English"
```

### CI/automation usage

```bash
# Non-interactive: run a task and exit
claw prompt "run the test suite and fix any failures"

# Capture JSON output for processing
result=$(claw --output-format json prompt "analyze code quality")
echo "$result" | jq '.summary'

# Health check before running
claw doctor || exit 1
```

---

## 3. RPC Mode (Planned)

JSON-RPC over stdin/stdout for language-agnostic integration.

### Planned protocol

```jsonc
// Initialize
→ {"jsonrpc": "2.0", "method": "session.create", "params": {"model": "claude-sonnet-4-6", "systemPrompt": ["You are a coding assistant."]}, "id": 1}
← {"jsonrpc": "2.0", "result": {"sessionId": "abc123"}, "id": 1}

// Run a turn
→ {"jsonrpc": "2.0", "method": "session.turn", "params": {"sessionId": "abc123", "input": "Read main.rs"}, "id": 2}
← {"jsonrpc": "2.0", "result": {"summary": {"tokensUsed": 1500, "toolCalls": 2}}, "id": 2}

// Subscribe to events
→ {"jsonrpc": "2.0", "method": "events.subscribe", "params": {"sessionId": "abc123"}}
← {"jsonrpc": "2.0", "method": "events.stream", "params": {"event": "tool_execution_started", "data": {"name": "read_file"}}}
← {"jsonrpc": "2.0", "method": "events.stream", "params": {"event": "turn_completed", "data": {"tokensUsed": 1500}}}

// Fork session
→ {"jsonrpc": "2.0", "method": "session.tree.fork", "params": {"sessionId": "abc123", "nodeId": "turn-3"}, "id": 3}
← {"jsonrpc": "2.0", "result": {"newSessionId": "def456"}, "id": 3}

// Close
→ {"jsonrpc": "2.0", "method": "session.close", "params": {"sessionId": "abc123"}, "id": 4}
```

### Planned framework adapters

| Framework | Language | Status |
|-----------|----------|--------|
| LangChain | Python | Planned |
| AutoGen | Python | Planned |
| CrewAI | Python | Planned |
| Generic HTTP | Any | Planned |

---

## 4. Model Configuration

### models.json schema

```json
{
  "providers": {
    "<provider-name>": {
      "baseUrl": "http://localhost:11434/v1",
      "api": "openai-completions",
      "apiKey": "OLLAMA_API_KEY",
      "headers": {},
      "models": [
        {
          "id": "model-name",
          "name": "Human-readable name",
          "reasoning": false,
          "input": ["text"],
          "contextWindow": 128000,
          "maxTokens": 32768
        }
      ]
    }
  }
}
```

### Supported API types

| `api` value | Wire format | Routes to |
|-------------|-------------|-----------|
| `openai-completions` | OpenAI chat completions | `OpenAiCompatClient` |
| `anthropic-messages` | Anthropic messages | `AnthropicClient` |

### API key resolution

- All-uppercase string (e.g. `OLLAMA_API_KEY`) → read from environment variable
- Any other string → used as literal API key value

### File locations

- User-level: `~/.claw/models.json`
- Project-level: `.claw/models.json`
- Both are merged; project entries override same-key user entries

### Model lookup order

1. Provider-prefixed: `ollama/llama3.1:8b` → exact match on provider + model ID
2. Bare ID: `llama3.1:8b` → first match across all providers
3. Built-in providers: `claude-sonnet-4-6`, `grok-3`, etc.

---

## 5. Event Types

| Event | When |
|-------|------|
| `TurnStarted` | Agent begins processing a turn |
| `TurnCompleted(TurnSummary)` | Agent finishes a turn |
| `TextDelta(String)` | Streaming text from the provider |
| `ToolUse { id, name, input }` | Model requests a tool call |
| `ToolExecutionStarted { name }` | Tool begins executing |
| `ToolExecutionCompleted { name, result, is_error }` | Tool finishes |
| `SessionLifecycle(Created/Loaded/Saved/Closed)` | Session state change |
| `AutoCompaction(Event)` | Context auto-compacted |
| `Error(String)` | Unrecoverable error |
