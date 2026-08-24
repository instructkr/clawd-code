# Security Policy

## Supported versions

Security fixes target the current `main` branch and the latest published
release artifacts when available. Older experimental branches are not supported
unless a maintainer explicitly marks them as supported.

## Reporting a vulnerability

Please do **not** open a public issue for a suspected vulnerability. Use GitHub
private vulnerability reporting for `ultraworkers/claw-code` when available, or
contact a maintainer through the repository's published support channel with a
minimal, non-destructive reproduction.

Include:

- affected command, crate, or workflow;
- operating system and shell, especially for Windows/PowerShell path issues;
- whether live credentials, MCP servers, plugins, or workspace filesystem
  access are involved;
- expected impact and any safe proof-of-concept steps.

Do not include real API keys, private prompts, session transcripts with secrets,
or exploit payloads that modify third-party systems.

## Scope

In scope:

- workspace path traversal or symlink escapes;
- permission bypasses, sandbox misreporting, or unsafe tool execution;
- credential disclosure in logs, JSON output, telemetry, docs, or examples;
- plugin, hook, MCP, provider, or config behavior that can unexpectedly execute
  code or leak secrets.

Out of scope:

- social engineering;
- denial-of-service without a practical security impact;
- issues that require already-compromised local developer credentials;
- reports against third-party providers or upstream tools without a Claw Code
  integration issue.

## Handling expectations

Maintainers will acknowledge valid private reports as soon as practical, keep
discussion private until a fix or mitigation is available, and credit reporters
when requested and appropriate.
