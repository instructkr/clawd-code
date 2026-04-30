# Git-Aware Context Tools

Adds five native git tools to claw-code that provide structured, read-only access to repository state. These replace ad-hoc `git` commands via bash with purpose-built tool definitions the model can discover and invoke directly.

## Tools

### GitStatus

Show the working tree status (branch, staged, unstaged, untracked). Equivalent to `git status --short --branch`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `short` | boolean | no | `true` | Use `--short --branch` format for concise output |

**Example input:**
```json
{}
```

**Example output:**
```json
{
  "output": "## feat/git-aware-tools...upstream/main [ahead 1]\nM rust/crates/tools/src/lib.rs"
}
```

---

### GitDiff

Show changes between commits, the index, and the working tree. Supports staged changes, specific paths, commit ranges, and comparing two commits.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `staged` | boolean | no | `false` | Show staged changes (`git diff --cached`) |
| `commit` | string | no | — | Commit hash, tag, or branch to diff against |
| `commit2` | string | no | — | Second commit for range diff (`commit...commit2`) |
| `path` | string | no | — | File path to restrict the diff to |

**Example inputs:**
```json
{}
```
```json
{ "staged": true }
```
```json
{ "commit": "HEAD~3", "path": "rust/crates/tools/src/lib.rs" }
```
```json
{ "commit": "main", "commit2": "feat/git-aware-tools" }
```

---

### GitLog

Show commit history. Supports limiting count, filtering by author/date/path, and oneline format.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `count` | integer | no | `20` | Maximum number of commits to return |
| `oneline` | boolean | no | `false` | Use `--oneline` format (hash + subject only) |
| `author` | string | no | — | Filter commits by author pattern |
| `since` | string | no | — | Filter commits since date (e.g. `"2024-01-01"` or `"2.weeks"`) |
| `until` | string | no | — | Filter commits until date |
| `path` | string | no | — | File or directory path to filter commits by |

**Example inputs:**
```json
{ "count": 5, "oneline": true }
```
```json
{ "author": "alice", "since": "1.week", "path": "src/main.rs" }
```

---

### GitShow

Show a commit, tag, or tree object with its diff. Supports showing a specific file at a commit and stat-only mode.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `commit` | string | **yes** | — | Commit hash, tag, or branch ref to show |
| `path` | string | no | — | Show only this file at the given commit (`commit:path` syntax) |
| `stat` | boolean | no | `false` | Show diffstat summary instead of full diff |

**Example inputs:**
```json
{ "commit": "HEAD" }
```
```json
{ "commit": "abc1234", "stat": true }
```
```json
{ "commit": "main", "path": "src/lib.rs" }
```

---

### GitBlame

Show what revision and author last modified each line of a file. Supports line range filtering.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `path` | string | **yes** | — | File path to blame |
| `start_line` | integer | no | — | Start of line range (1-based) |
| `end_line` | integer | no | — | End of line range (1-based) |

**Example inputs:**
```json
{ "path": "src/main.rs" }
```
```json
{ "path": "src/main.rs", "start_line": 100, "end_line": 150 }
```

---

## Architecture

All five tools follow the same pattern:

1. **ToolSpec** — Defines the tool name, description, JSON input schema, and `PermissionMode::ReadOnly`
2. **Input struct** — Derives `Deserialize` with `#[serde(default)]` on optional fields
3. **Run function** — Builds git arguments, calls `git_stdout()`, wraps result in JSON via `to_pretty_json()`
4. **Dispatch** — Matched in `execute_tool_with_enforcer()` like all other tools

The existing `git_stdout(args: &[&str]) -> Option<String>` helper (at `tools/src/lib.rs`) handles running the `git` subprocess and returning trimmed stdout. Git tools simply construct the right arguments and delegate to this helper.

## Why native git tools?

Before this PR, the model had to use the `bash` tool for git operations, which has several drawbacks:

- **No structured output** — Bash returns raw text that the model must parse
- **Over-permissioned** — Bash requires `DangerFullAccess` even for read-only git commands
- **No discoverability** — The model can't search for git-capable tools via `ToolSearch`
- **Inconsistent** — Each invocation may use different flags or formatting

With native git tools:

- All five are `ReadOnly` — safe in restricted permission modes
- Structured JSON output — consistent, parseable results
- Discoverable via `ToolSearch` with keywords like "git", "diff", "blame"
- Model-friendly descriptions explain when to use each tool vs bash

## Testing

```bash
cd rust
cargo build --release
cargo test -p tools
```

The 3 pre-existing test failures (agent_fake_runner, agent_persists_handoff, worker_create_merges_config) are unrelated to this change — they fail due to local settings.json incompatibilities.
