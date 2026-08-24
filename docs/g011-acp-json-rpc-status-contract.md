# G011 ACP/Zed and JSON-RPC status contract

Claw Code 2.0 keeps ACP/Zed and JSON-RPC serving behind the stable task,
session-control, and event/report contracts from the roadmap. The current public
surface is therefore a **truthful unsupported status**, not a hidden daemon.

## Supported status queries

The following commands are status queries and exit with code `0`:

```bash
claw acp
claw acp serve
claw --acp
claw -acp
claw acp --output-format json
claw acp serve --output-format json
```

`serve` is deliberately an alias for status today. It does not bind a socket,
start a daemon, or expose a JSON-RPC endpoint.

## JSON envelope

`claw acp --output-format json` returns a stable envelope for editor probes and
CI checks:

```json
{
  "schema_version": "1.0",
  "kind": "acp",
  "status": "unsupported",
  "phase": "discoverability_only",
  "supported": false,
  "exit_code": 0,
  "serve_alias_only": true,
  "protocol": {
    "name": "ACP/Zed",
    "json_rpc": false,
    "daemon": false,
    "endpoint": null,
    "serve_starts_daemon": false
  }
}
```

Consumers should check `kind == "acp"`, `supported == false`, and
`protocol.json_rpc == false` instead of inferring support from command presence.

## Unsupported invocations

Malformed ACP invocations, such as `claw acp start`, exit with code `1`. With
`--output-format json`, stderr uses the normal CLI error envelope and sets:

```json
{
  "type": "error",
  "kind": "unsupported_acp_invocation",
  "exit_code": 1
}
```

## Deferral gate

Real ACP/Zed or JSON-RPC serve work remains deferred until the roadmap contracts
for task packets, session control, and event/report schemas are stable. This
keeps desktop, marketplace, and editor integrations from becoming alternate
sources of truth before the CLI/file/API contracts are ready.
