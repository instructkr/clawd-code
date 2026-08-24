# Navigation and file context guide

This guide answers the common “how do I browse output?” and “how do I submit a file?” questions for Claw Code. Claw is an agent CLI, not a full file manager: terminal navigation comes from your shell or terminal, while file context is passed explicitly in prompts.

## Prompt and terminal navigation

Use your terminal’s normal controls for command history and long output:

- `Up` / `Down` usually move through shell or REPL prompt history.
- `Ctrl-r` searches shell history in most shells.
- Long command output is viewed with your terminal scrollback. In tmux, enter copy mode with `Ctrl-b [` then use arrows, PageUp/PageDown, search, or your mouse depending on tmux config.
- If output is too large to scroll comfortably, redirect it to a file and give that file to Claw as context:
  ```bash
  cargo test --workspace 2>&1 | tee logs/test-output.txt
  claw prompt "Use @logs/test-output.txt as context and summarize the failing tests."
  ```

Claw may provide slash commands that inspect workspace state, but those commands do not replace your terminal’s scrollback or shell history.

## Submit repository files with `@path`

Mention files from the current workspace with `@` paths. Use relative paths from the repository or current working directory:

```text
Read @src/app.ts and explain the bug.
Compare @old.md and @new.md.
Use @logs/error.txt as context and suggest a fix.
Review @README.md and @docs/navigation-file-context.md for consistency.
```

Tips:

- Prefer the smallest useful file set. Large directories or logs can consume context quickly.
- Use exact paths when possible (`@rust/crates/runtime/src/lib.rs`) instead of vague descriptions.
- For generated logs, save them under a temporary or ignored directory such as `logs/` and reference the file.
- If the file is outside the repository, copy it into a safe workspace location first or use an app/UI attachment feature if your Claw surface supports attachments.

## Browse or inspect files

Claw can answer questions about files you reference, and you can ask it to inspect likely locations:

```text
Find where provider routing is implemented and summarize the relevant files.
Read @USAGE.md and tell me where local model setup is documented.
Search for the command that handles skills install, then explain the control flow.
```

For deterministic shell-side browsing, ordinary commands still work:

```bash
find docs -maxdepth 2 -type f | sort
rg -n "OPENAI_BASE_URL|skills install" USAGE.md docs rust
sed -n '250,340p' USAGE.md
```

## Attach external files where supported

Some UI surfaces let you drag and drop or attach files directly. When that is available, use attachments for files that should not be committed to the repo. In terminal-only usage, copy the file into the workspace, reference it with `@path`, then remove it when finished if it was temporary.

## Secret and credential safety

Do not paste real API keys, OAuth tokens, private logs, or customer data into prompts, issue comments, screenshots, or committed docs. Before submitting a file:

- Replace live keys with placeholders such as `sk-ant-REPLACE_ME`, `sk-or-v1-REPLACE_ME`, or `local-dev-token`.
- Redact bearer tokens, cookies, session IDs, and private base URLs.
- Prefer minimal reproductions over full production logs.
- Keep `.env`, key files, and private logs out of git.

If a task requires credentials, describe the variable names and expected shapes instead of sharing the values.
