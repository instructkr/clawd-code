# Mock LLM parity harness

This milestone adds a deterministic Anthropic-compatible mock service plus a reproducible CLI harness for the Rust `claw` binary.

## Artifacts

- `crates/mock-anthropic-service/` — mock `/v1/messages` service
- `crates/rusty-claude-cli/tests/mock_parity_harness.rs` — end-to-end clean-environment harness
- `scripts/run_mock_parity_harness.sh` — convenience wrapper

## Scenarios

The harness runs these scripted scenarios against a fresh workspace and isolated environment variables:

1. `streaming_text`
2. `read_file_roundtrip`
3. `grep_chunk_assembly`
4. `write_file_allowed`
5. `write_file_denied`
6. `multi_tool_turn_roundtrip`
7. `bash_stdout_roundtrip`
8. `bash_permission_prompt_approved`
9. `bash_permission_prompt_denied`
10. `plugin_tool_roundtrip`
11. `auto_compact_triggered`
12. `token_cost_reporting`

## Run

```bash
cd rust/
./scripts/run_mock_parity_harness.sh
```

Behavioral checklist / parity diff:

```bash
cd rust/
python3 scripts/run_mock_parity_diff.py
```

Scenario-to-PARITY mappings live in `mock_parity_scenarios.json`; keep this manifest aligned with `rust/crates/rusty-claude-cli/tests/mock_parity_harness.rs` and `PARITY.md` via `python3 scripts/run_mock_parity_diff.py --no-run`.

## Manual mock server

```bash
cd rust/
cargo run -p mock-anthropic-service -- --bind 127.0.0.1:0
```

The server prints `MOCK_ANTHROPIC_BASE_URL=...`; point `ANTHROPIC_BASE_URL` at that URL and use any non-empty `ANTHROPIC_API_KEY`.
