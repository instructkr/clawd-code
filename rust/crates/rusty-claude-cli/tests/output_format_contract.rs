use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::Session;
use serde_json::Value;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn help_emits_json_when_requested() {
    let root = unique_temp_dir("help-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let parsed = assert_json_command(&root, &["--output-format", "json", "help"]);
    assert_eq!(parsed["kind"], "help");
    assert!(parsed["message"]
        .as_str()
        .expect("help text")
        .contains("Usage:"));
}

#[test]
fn version_emits_json_when_requested() {
    let root = unique_temp_dir("version-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let parsed = assert_json_command(&root, &["--output-format", "json", "version"]);
    assert_eq!(parsed["kind"], "version");
    assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn status_and_sandbox_emit_json_when_requested() {
    let root = unique_temp_dir("status-sandbox-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let status = assert_json_command(&root, &["--output-format", "json", "status"]);
    assert_eq!(status["kind"], "status");
    assert!(status["workspace"]["cwd"].as_str().is_some());

    let sandbox = assert_json_command(&root, &["--output-format", "json", "sandbox"]);
    assert_eq!(sandbox["kind"], "sandbox");
    assert!(sandbox["filesystem_mode"].as_str().is_some());
}

#[test]
fn acp_guidance_emits_json_when_requested() {
    let root = unique_temp_dir("acp-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let acp = assert_json_command(&root, &["--output-format", "json", "acp"]);
    assert_eq!(acp["kind"], "acp");
    assert_eq!(acp["status"], "discoverability_only");
    assert_eq!(acp["supported"], false);
    assert_eq!(acp["serve_alias_only"], true);
    assert_eq!(acp["discoverability_tracking"], "ROADMAP #64a");
    assert_eq!(acp["tracking"], "ROADMAP #76");
    assert!(acp["message"]
        .as_str()
        .expect("acp message")
        .contains("discoverability alias"));
}

#[test]
fn inventory_commands_emit_structured_json_when_requested() {
    let root = unique_temp_dir("inventory-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let isolated_home = root.join("home");
    let isolated_config = root.join("config-home");
    let isolated_codex = root.join("codex-home");
    fs::create_dir_all(&isolated_home).expect("isolated home should exist");

    let agents = assert_json_command_with_env(
        &root,
        &["--output-format", "json", "agents"],
        &[
            ("HOME", isolated_home.to_str().expect("utf8 home")),
            (
                "CLAW_CONFIG_HOME",
                isolated_config.to_str().expect("utf8 config home"),
            ),
            (
                "CODEX_HOME",
                isolated_codex.to_str().expect("utf8 codex home"),
            ),
        ],
    );
    assert_eq!(agents["kind"], "agents");
    assert_eq!(agents["action"], "list");
    assert_eq!(agents["count"], 0);
    assert_eq!(agents["summary"]["active"], 0);
    assert!(agents["agents"]
        .as_array()
        .expect("agents array")
        .is_empty());

    let mcp = assert_json_command(&root, &["--output-format", "json", "mcp"]);
    assert_eq!(mcp["kind"], "mcp");
    assert_eq!(mcp["action"], "list");

    let skills = assert_json_command(&root, &["--output-format", "json", "skills"]);
    assert_eq!(skills["kind"], "skills");
    assert_eq!(skills["action"], "list");
}

#[test]
fn agents_command_emits_structured_agent_entries_when_requested() {
    let root = unique_temp_dir("agents-json-populated");
    let workspace = root.join("workspace");
    let project_agents = workspace.join(".codex").join("agents");
    let home = root.join("home");
    let user_agents = home.join(".codex").join("agents");
    let isolated_config = root.join("config-home");
    let isolated_codex = root.join("codex-home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    write_agent(
        &project_agents,
        "planner",
        "Project planner",
        "gpt-5.4",
        "medium",
    );
    write_agent(
        &project_agents,
        "verifier",
        "Verification agent",
        "gpt-5.4-mini",
        "high",
    );
    write_agent(
        &user_agents,
        "planner",
        "User planner",
        "gpt-5.4-mini",
        "high",
    );

    let parsed = assert_json_command_with_env(
        &workspace,
        &["--output-format", "json", "agents"],
        &[
            ("HOME", home.to_str().expect("utf8 home")),
            (
                "CLAW_CONFIG_HOME",
                isolated_config.to_str().expect("utf8 config home"),
            ),
            (
                "CODEX_HOME",
                isolated_codex.to_str().expect("utf8 codex home"),
            ),
        ],
    );

    assert_eq!(parsed["kind"], "agents");
    assert_eq!(parsed["action"], "list");
    assert_eq!(parsed["count"], 3);
    assert_eq!(parsed["summary"]["active"], 2);
    assert_eq!(parsed["summary"]["shadowed"], 1);
    assert_eq!(parsed["agents"][0]["name"], "planner");
    assert_eq!(parsed["agents"][0]["source"]["id"], "project_claw");
    assert_eq!(parsed["agents"][0]["active"], true);
    assert_eq!(parsed["agents"][1]["name"], "verifier");
    assert_eq!(parsed["agents"][2]["name"], "planner");
    assert_eq!(parsed["agents"][2]["active"], false);
    assert_eq!(parsed["agents"][2]["shadowed_by"]["id"], "project_claw");
}

#[test]
fn bootstrap_and_system_prompt_emit_json_when_requested() {
    let root = unique_temp_dir("bootstrap-system-prompt-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let plan = assert_json_command(&root, &["--output-format", "json", "bootstrap-plan"]);
    assert_eq!(plan["kind"], "bootstrap-plan");
    assert!(plan["phases"].as_array().expect("phases").len() > 1);

    let prompt = assert_json_command(&root, &["--output-format", "json", "system-prompt"]);
    assert_eq!(prompt["kind"], "system-prompt");
    assert!(prompt["message"]
        .as_str()
        .expect("prompt text")
        .contains("interactive agent"));
}

#[test]
fn dump_manifests_and_init_emit_json_when_requested() {
    let root = unique_temp_dir("manifest-init-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let upstream = write_upstream_fixture(&root);
    let manifests = assert_json_command(
        &root,
        &[
            "--output-format",
            "json",
            "dump-manifests",
            "--manifests-dir",
            upstream.to_str().expect("utf8 upstream"),
        ],
    );
    assert_eq!(manifests["kind"], "dump-manifests");
    assert_eq!(manifests["commands"], 1);
    assert_eq!(manifests["tools"], 1);

    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let init = assert_json_command(&workspace, &["--output-format", "json", "init"]);
    assert_eq!(init["kind"], "init");
    assert!(workspace.join("CLAUDE.md").exists());
}

#[test]
fn doctor_and_resume_status_emit_json_when_requested() {
    let root = unique_temp_dir("doctor-resume-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let doctor = assert_json_command(&root, &["--output-format", "json", "doctor"]);
    assert_eq!(doctor["kind"], "doctor");
    assert!(doctor["message"].is_string());
    let summary = doctor["summary"].as_object().expect("doctor summary");
    assert!(summary["ok"].as_u64().is_some());
    assert!(summary["warnings"].as_u64().is_some());
    assert!(summary["failures"].as_u64().is_some());

    let checks = doctor["checks"].as_array().expect("doctor checks");
    assert_eq!(checks.len(), 6);
    let check_names = checks
        .iter()
        .map(|check| {
            assert!(check["status"].as_str().is_some());
            assert!(check["summary"].as_str().is_some());
            assert!(check["details"].is_array());
            check["name"].as_str().expect("doctor check name")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        check_names,
        vec![
            "auth",
            "config",
            "install source",
            "workspace",
            "sandbox",
            "system"
        ]
    );

    let install_source = checks
        .iter()
        .find(|check| check["name"] == "install source")
        .expect("install source check");
    assert_eq!(
        install_source["official_repo"],
        "https://github.com/ultraworkers/claw-code"
    );
    assert_eq!(
        install_source["deprecated_install"],
        "cargo install claw-code"
    );

    let workspace = checks
        .iter()
        .find(|check| check["name"] == "workspace")
        .expect("workspace check");
    assert!(workspace["cwd"].as_str().is_some());
    assert!(workspace["in_git_repo"].is_boolean());

    let sandbox = checks
        .iter()
        .find(|check| check["name"] == "sandbox")
        .expect("sandbox check");
    assert!(sandbox["filesystem_mode"].as_str().is_some());
    assert!(sandbox["enabled"].is_boolean());
    assert!(sandbox["fallback_reason"].is_null() || sandbox["fallback_reason"].is_string());

    let session_path = write_session_fixture(&root, "resume-json", Some("hello"));
    let resumed = assert_json_command(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/status",
        ],
    );
    assert_eq!(resumed["kind"], "status");
    // model is null in resume mode (not known without --model flag)
    assert!(resumed["model"].is_null());
    assert_eq!(resumed["usage"]["messages"], 1);
    assert!(resumed["workspace"]["cwd"].as_str().is_some());
    assert!(resumed["sandbox"]["filesystem_mode"].as_str().is_some());
}

#[test]
fn resumed_inventory_commands_emit_structured_json_when_requested() {
    let root = unique_temp_dir("resume-inventory-json");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let session_path = write_session_fixture(&root, "resume-inventory-json", Some("inventory"));

    let mcp = assert_json_command_with_env(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/mcp",
        ],
        &[
            (
                "CLAW_CONFIG_HOME",
                config_home.to_str().expect("utf8 config home"),
            ),
            ("HOME", home.to_str().expect("utf8 home")),
        ],
    );
    assert_eq!(mcp["kind"], "mcp");
    assert_eq!(mcp["action"], "list");
    assert!(mcp["servers"].is_array());

    let skills = assert_json_command_with_env(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/skills",
        ],
        &[
            (
                "CLAW_CONFIG_HOME",
                config_home.to_str().expect("utf8 config home"),
            ),
            ("HOME", home.to_str().expect("utf8 home")),
        ],
    );
    assert_eq!(skills["kind"], "skills");
    assert_eq!(skills["action"], "list");
    assert!(skills["summary"]["total"].is_number());
    assert!(skills["skills"].is_array());
}

#[test]
fn resumed_version_and_init_emit_structured_json_when_requested() {
    let root = unique_temp_dir("resume-version-init-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let session_path = write_session_fixture(&root, "resume-version-init-json", None);

    let version = assert_json_command(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/version",
        ],
    );
    assert_eq!(version["kind"], "version");
    assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));

    let init = assert_json_command(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/init",
        ],
    );
    assert_eq!(init["kind"], "init");
    assert!(root.join("CLAUDE.md").exists());
}

fn assert_json_command(current_dir: &Path, args: &[&str]) -> Value {
    assert_json_command_with_env(current_dir, args, &[])
}

/// #247 regression helper: run claw expecting a non-zero exit and return
/// the JSON error envelope parsed from stdout. Asserts exit != 0 and that
/// the envelope includes `type: "error"` at the very least.
///
/// #168c: Error envelopes under --output-format json are now emitted to
/// STDOUT (not stderr). This matches the emission contract that stdout
/// carries the contractual envelope (success OR error) while stderr is
/// reserved for non-contractual diagnostics.
fn assert_json_error_envelope(current_dir: &Path, args: &[&str]) -> Value {
    let output = run_claw(current_dir, args, &[]);
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // #168c: The JSON envelope is written to STDOUT for error cases under
    // --output-format json (see main.rs). Previously was stderr.
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout should be a JSON error envelope but failed to parse: {err}\nstdout bytes:\n{}\nstderr bytes:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(
        envelope["type"], "error",
        "envelope should carry type=error"
    );
    envelope
}

/// #168c regression test: under `--output-format json`, error envelopes
/// must be emitted to STDOUT (not stderr). This is the emission contract:
/// stdout carries the JSON envelope regardless of success/error; stderr
/// is reserved for non-contractual diagnostics.
///
/// Refutes cycle #84's "bootstrap silent failure" claim (cycle #87 controlled
/// matrix showed errors were on stderr, not silent; cycle #88 locked the
/// emission contract to require stdout).
#[test]
fn error_envelope_emitted_to_stdout_under_output_format_json_168c() {
    let root = unique_temp_dir("168c-emission-stdout");
    fs::create_dir_all(&root).expect("temp dir should exist");

    // Trigger an error via `prompt` without arg (known cli_parse error).
    let output = run_claw(&root, &["--output-format", "json", "prompt"], &[]);

    // Exit code must be non-zero (error).
    assert!(
        !output.status.success(),
        "prompt without arg must fail; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // #168c primary assertion: stdout carries the JSON envelope.
    let stdout_text = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout_text.trim().is_empty(),
        "stdout must contain JSON envelope under --output-format json (#168c emission contract). stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout should be valid JSON under --output-format json (#168c): {err}\nstdout bytes:\n{stdout_text}"
        )
    });
    assert_eq!(envelope["type"], "error", "envelope must be typed error");
    assert!(
        envelope["kind"].as_str().is_some(),
        "envelope must carry machine-readable kind"
    );

    // #168c secondary assertion: stderr should NOT carry the JSON envelope
    // (it may be empty or contain non-JSON diagnostics, but the envelope
    // belongs on stdout under --output-format json).
    let stderr_text = String::from_utf8_lossy(&output.stderr);
    let stderr_trimmed = stderr_text.trim();
    if !stderr_trimmed.is_empty() {
        // If stderr has content, it must NOT be the JSON envelope.
        let stderr_is_json: Result<Value, _> = serde_json::from_slice(&output.stderr);
        assert!(
            stderr_is_json.is_err(),
            "stderr must not duplicate the JSON envelope (#168c); stderr was:\n{stderr_trimmed}"
        );
    }
}

#[test]
fn prompt_subcommand_without_arg_emits_cli_parse_envelope_with_hint_247() {
    // #247: `claw prompt` with no argument must classify as `cli_parse`
    // (not `unknown`) and the JSON envelope must carry the same actionable
    // `Run claw --help for usage.` hint that text-mode stderr appends.
    let root = unique_temp_dir("247-prompt-no-arg");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let envelope = assert_json_error_envelope(&root, &["--output-format", "json", "prompt"]);
    assert_eq!(
        envelope["kind"], "cli_parse",
        "prompt subcommand without arg should classify as cli_parse, envelope: {envelope}"
    );
    assert_eq!(
        envelope["error"], "prompt subcommand requires a prompt string",
        "short reason should match the raw error, envelope: {envelope}"
    );
    assert_eq!(
        envelope["hint"],
        "Run `claw --help` for usage.",
        "JSON envelope must carry the same help-runbook hint as text mode, envelope: {envelope}"
    );
}

#[test]
fn empty_positional_arg_emits_cli_parse_envelope_247() {
    // #247: `claw ""` must classify as `cli_parse`, not `unknown`. The
    // message itself embeds a ``run `claw --help`` pointer so the explicit
    // hint field is allowed to remain null to avoid duplication — what
    // matters for the typed-error contract is that `kind == cli_parse`.
    let root = unique_temp_dir("247-empty-arg");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let envelope = assert_json_error_envelope(&root, &["--output-format", "json", ""]);
    assert_eq!(
        envelope["kind"], "cli_parse",
        "empty-prompt error should classify as cli_parse, envelope: {envelope}"
    );
    let short = envelope["error"]
        .as_str()
        .expect("error field should be a string");
    assert!(
        short.starts_with("empty prompt:"),
        "short reason should preserve the original empty-prompt message, got: {short}"
    );
}

#[test]
fn whitespace_only_positional_arg_emits_cli_parse_envelope_247() {
    // #247: same rule for `claw "   "` — any whitespace-only prompt must
    // flow through the empty-prompt path and classify as `cli_parse`.
    let root = unique_temp_dir("247-whitespace-arg");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let envelope = assert_json_error_envelope(&root, &["--output-format", "json", "   "]);
    assert_eq!(
        envelope["kind"], "cli_parse",
        "whitespace-only prompt should classify as cli_parse, envelope: {envelope}"
    );
}

/// #168c Phase 0 Task 2: No-silent guarantee.
///
/// Under `--output-format json`, every verb must satisfy the emission contract:
/// either emit a valid JSON envelope to stdout (with exit 0 for success, or
/// exit != 0 for error), OR exit with an error code. Silent success (exit 0
/// with empty stdout) is forbidden under the JSON contract because consumers
/// cannot distinguish success from broken emission.
///
/// This test iterates a catalog of clawable verbs and asserts:
/// 1. Each verb produces stdout output when exit == 0 (no silent success)
/// 2. The stdout output parses as JSON (emission contract integrity)
/// 3. Error cases (exit != 0) produce JSON on stdout (#168c routing fix)
///
/// Phase 0 Task 2 deliverable: prevents regressions in the emission contract
/// for the full set of discoverable verbs.
#[test]
fn emission_contract_no_silent_success_under_output_format_json_168c_task2() {
    let root = unique_temp_dir("168c-task2-no-silent");
    fs::create_dir_all(&root).expect("temp dir should exist");

    // Verbs expected to succeed (exit 0) with non-empty JSON on stdout.
    // Covers the discovery-safe subset — verbs that don't require external
    // credentials or network and should be safely invokable in CI.
    let safe_success_verbs: &[(&str, &[&str])] = &[
        ("help", &["help"]),
        ("version", &["version"]),
        ("list-sessions", &["list-sessions"]),
        ("doctor", &["doctor"]),
        ("mcp", &["mcp"]),
        ("skills", &["skills"]),
        ("agents", &["agents"]),
        ("sandbox", &["sandbox"]),
        ("status", &["status"]),
        ("system-prompt", &["system-prompt"]),
        ("bootstrap-plan", &["bootstrap-plan", "test"]),
        ("acp", &["acp"]),
    ];

    for (verb, args) in safe_success_verbs {
        let mut full_args = vec!["--output-format", "json"];
        full_args.extend_from_slice(args);
        let output = run_claw(&root, &full_args, &[]);

        // Emission contract clause 1: if exit == 0, stdout must be non-empty.
        if output.status.success() {
            let stdout_text = String::from_utf8_lossy(&output.stdout);
            assert!(
                !stdout_text.trim().is_empty(),
                "#168c Task 2 emission contract violation: `{verb}` exit 0 with empty stdout (silent success). stderr was:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );

            // Emission contract clause 2: stdout must be valid JSON.
            let envelope: Result<Value, _> = serde_json::from_slice(&output.stdout);
            assert!(
                envelope.is_ok(),
                "#168c Task 2 emission contract violation: `{verb}` stdout is not valid JSON:\n{stdout_text}"
            );
        }
        // If exit != 0, it's an error path; #168c primary test covers error routing.
    }

    // Verbs expected to fail (exit != 0) in test env (require external state).
    // Emission contract clause 3: error paths must still emit JSON on stdout.
    let safe_error_verbs: &[(&str, &[&str])] = &[
        ("prompt-no-arg", &["prompt"]),
        ("doctor-bad-arg", &["doctor", "--foo"]),
    ];

    for (label, args) in safe_error_verbs {
        let mut full_args = vec!["--output-format", "json"];
        full_args.extend_from_slice(args);
        let output = run_claw(&root, &full_args, &[]);

        assert!(
            !output.status.success(),
            "{label} was expected to fail but exited 0"
        );

        // #168c: error envelopes must be on stdout.
        let stdout_text = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout_text.trim().is_empty(),
            "#168c Task 2 emission contract violation: {label} failed with empty stdout. stderr was:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let envelope: Result<Value, _> = serde_json::from_slice(&output.stdout);
        assert!(
            envelope.is_ok(),
            "#168c Task 2 emission contract violation: {label} stdout not valid JSON:\n{stdout_text}"
        );
        let envelope = envelope.unwrap();
        assert_eq!(
            envelope["type"], "error",
            "{label} error envelope must carry type=error, got: {envelope}"
        );
    }
}

/// #168c Phase 0 Task 4: Shape parity / regression guard.
///
/// Locks the v1.5 emission baseline (documented in SCHEMAS.md § v1.5 Emission
/// Baseline) so any future PR that introduces shape drift in a documented
/// verb fails this test at PR time.
///
/// This complements Task 2 (no-silent guarantee) by asserting the SPECIFIC
/// top-level key sets documented in the catalog. If a verb adds/removes a
/// top-level field, this test fails — forcing the PR author to:
/// (a) update SCHEMAS.md § v1.5 Emission Baseline with the new shape, and
/// (b) acknowledge the v1.5 baseline is changing.
///
/// Phase 0 Task 4 deliverable: prevents undocumented shape drift in v1.5
/// baseline before Phase 1 (shape normalization) begins.
///
/// Note: This test intentionally asserts the CURRENT (possibly imperfect)
/// shape, NOT the target. Phase 1 will update these expectations as shapes
/// normalize.
#[test]
fn v1_5_emission_baseline_shape_parity_168c_task4() {
    let root = unique_temp_dir("168c-task4-shape-parity");
    fs::create_dir_all(&root).expect("temp dir should exist");

    // v1.5 baseline per-verb shape catalog (from SCHEMAS.md § v1.5 Emission Baseline).
    // Each entry: (verb, args, expected_top_level_keys_sorted).
    //
    // This catalog was captured by the cycle #87 controlled matrix and is
    // enforced by SCHEMAS.md § v1.5 Emission Baseline documentation.
    let baseline: &[(&str, &[&str], &[&str])] = &[
        // Verbs using `kind` field (12 of 13 success paths)
        ("help", &["help"], &["kind", "message"]),
        (
            "version",
            &["version"],
            &["git_sha", "kind", "message", "target", "version"],
        ),
        (
            "doctor",
            &["doctor"],
            &["checks", "has_failures", "kind", "message", "report", "summary"],
        ),
        (
            "skills",
            &["skills"],
            &["action", "kind", "skills", "summary"],
        ),
        (
            "agents",
            &["agents"],
            &["action", "agents", "count", "kind", "summary", "working_directory"],
        ),
        (
            "system-prompt",
            &["system-prompt"],
            &["kind", "message", "sections"],
        ),
        (
            "bootstrap-plan",
            &["bootstrap-plan", "test"],
            &["kind", "phases"],
        ),
        // Verb using `command` field (the 1-of-13 deviation — Phase 1 target)
        (
            "list-sessions",
            &["list-sessions"],
            &["command", "sessions"],
        ),
    ];

    for (verb, args, expected_keys) in baseline {
        let mut full_args = vec!["--output-format", "json"];
        full_args.extend_from_slice(args);
        let output = run_claw(&root, &full_args, &[]);

        assert!(
            output.status.success(),
            "#168c Task 4: `{verb}` expected success path but exited with {:?}. stdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
            panic!(
                "#168c Task 4: `{verb}` stdout not valid JSON: {err}\nstdout:\n{}",
                String::from_utf8_lossy(&output.stdout)
            )
        });

        let actual_keys: Vec<String> = envelope
            .as_object()
            .unwrap_or_else(|| panic!("#168c Task 4: `{verb}` envelope not a JSON object"))
            .keys()
            .cloned()
            .collect();
        let mut actual_sorted = actual_keys.clone();
        actual_sorted.sort();

        let mut expected_sorted: Vec<String> = expected_keys.iter().map(|s| s.to_string()).collect();
        expected_sorted.sort();

        assert_eq!(
            actual_sorted, expected_sorted,
            "#168c Task 4: shape drift detected in `{verb}`!\n\
             Expected top-level keys (v1.5 baseline): {expected_sorted:?}\n\
             Actual top-level keys: {actual_sorted:?}\n\
             If this is intentional, update:\n\
             1. SCHEMAS.md § v1.5 Emission Baseline catalog\n\
             2. This test's `baseline` array\n\
             Envelope: {envelope}"
        );
    }

    // Error envelope shape parity (all error paths).
    // Standard v1.5 error envelope: {error, hint, kind, type} (always 4 keys).
    let error_cases: &[(&str, &[&str])] = &[
        ("prompt-no-arg", &["prompt"]),
        ("doctor-bad-arg", &["doctor", "--foo"]),
    ];

    let expected_error_keys = ["error", "hint", "kind", "type"];
    let mut expected_error_sorted: Vec<String> =
        expected_error_keys.iter().map(|s| s.to_string()).collect();
    expected_error_sorted.sort();

    for (label, args) in error_cases {
        let mut full_args = vec!["--output-format", "json"];
        full_args.extend_from_slice(args);
        let output = run_claw(&root, &full_args, &[]);

        assert!(
            !output.status.success(),
            "{label}: expected error exit, got success"
        );

        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
            panic!(
                "#168c Task 4: {label} stdout not valid JSON: {err}\nstdout:\n{}",
                String::from_utf8_lossy(&output.stdout)
            )
        });

        let actual_keys: Vec<String> = envelope
            .as_object()
            .unwrap_or_else(|| panic!("#168c Task 4: {label} envelope not a JSON object"))
            .keys()
            .cloned()
            .collect();
        let mut actual_sorted = actual_keys.clone();
        actual_sorted.sort();

        assert_eq!(
            actual_sorted, expected_error_sorted,
            "#168c Task 4: error envelope shape drift detected in {label}!\n\
             Expected v1.5 error envelope keys: {expected_error_sorted:?}\n\
             Actual keys: {actual_sorted:?}\n\
             If this is intentional, update SCHEMAS.md § Standard Error Envelope (v1.5).\n\
             Envelope: {envelope}"
        );
    }
}

#[test]
fn unrecognized_argument_still_classifies_as_cli_parse_247_regression_guard() {
    // #247 regression guard: the new empty-prompt / prompt-subcommand
    // patterns must NOT hijack the existing #77 unrecognized-argument
    // classification. `claw doctor --foo` must still surface as cli_parse
    // with the runbook hint present.
    let root = unique_temp_dir("247-unrecognized-arg");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let envelope =
        assert_json_error_envelope(&root, &["--output-format", "json", "doctor", "--foo"]);
    assert_eq!(
        envelope["kind"], "cli_parse",
        "unrecognized-argument must remain cli_parse, envelope: {envelope}"
    );
    assert_eq!(
        envelope["hint"],
        "Run `claw --help` for usage.",
        "unrecognized-argument hint should stay intact, envelope: {envelope}"
    );
}

fn assert_json_command_with_env(current_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Value {
    let output = run_claw(current_dir, args, envs);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout should be valid json")
}

fn run_claw(current_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_claw"));
    command.current_dir(current_dir).args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("claw should launch")
}

fn write_upstream_fixture(root: &Path) -> PathBuf {
    let upstream = root.join("claw-code");
    let src = upstream.join("src");
    let entrypoints = src.join("entrypoints");
    fs::create_dir_all(&entrypoints).expect("upstream entrypoints dir should exist");
    fs::write(
        src.join("commands.ts"),
        "import FooCommand from './commands/foo'\n",
    )
    .expect("commands fixture should write");
    fs::write(
        src.join("tools.ts"),
        "import ReadTool from './tools/read'\n",
    )
    .expect("tools fixture should write");
    fs::write(
        entrypoints.join("cli.tsx"),
        "if (args[0] === '--version') {}\nstartupProfiler()\n",
    )
    .expect("cli fixture should write");
    upstream
}

fn write_session_fixture(root: &Path, session_id: &str, user_text: Option<&str>) -> PathBuf {
    let session_path = root.join("session.jsonl");
    let mut session = Session::new()
        .with_workspace_root(root.to_path_buf())
        .with_persistence_path(session_path.clone());
    session.session_id = session_id.to_string();
    if let Some(text) = user_text {
        session
            .push_user_text(text)
            .expect("session fixture message should persist");
    } else {
        session
            .save_to_path(&session_path)
            .expect("session fixture should persist");
    }
    session_path
}

fn write_agent(root: &Path, name: &str, description: &str, model: &str, reasoning: &str) {
    fs::create_dir_all(root).expect("agent root should exist");
    fs::write(
        root.join(format!("{name}.toml")),
        format!(
            "name = \"{name}\"\ndescription = \"{description}\"\nmodel = \"{model}\"\nmodel_reasoning_effort = \"{reasoning}\"\n"
        ),
    )
    .expect("agent fixture should write");
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_millis();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "claw-output-format-{label}-{}-{millis}-{counter}",
        std::process::id()
    ))
}
