#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mock_anthropic_service::{MockAnthropicService, SCENARIO_PREFIX};
use serde_json::{json, Value};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
#[allow(clippy::too_many_lines)]
fn clean_env_cli_reaches_mock_anthropic_service_across_scripted_parity_scenarios() {
    let manifest_entries = load_scenario_manifest();
    let manifest = manifest_entries
        .iter()
        .cloned()
        .map(|entry| (entry.name.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let base_url = server.base_url();

    let cases = [
        ScenarioCase {
            name: "streaming_text",
            permission_mode: "read-only",
            allowed_tools: None,
            stdin: None,
            prepare: prepare_noop,
            assert: assert_streaming_text,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "read_file_roundtrip",
            permission_mode: "read-only",
            allowed_tools: Some("read_file"),
            stdin: None,
            prepare: prepare_read_fixture,
            assert: assert_read_file_roundtrip,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "grep_chunk_assembly",
            permission_mode: "read-only",
            allowed_tools: Some("grep_search"),
            stdin: None,
            prepare: prepare_grep_fixture,
            assert: assert_grep_chunk_assembly,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "write_file_allowed",
            permission_mode: "workspace-write",
            allowed_tools: Some("write_file"),
            stdin: None,
            prepare: prepare_noop,
            assert: assert_write_file_allowed,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "write_file_denied",
            permission_mode: "read-only",
            allowed_tools: Some("write_file"),
            stdin: None,
            prepare: prepare_noop,
            assert: assert_write_file_denied,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "multi_tool_turn_roundtrip",
            permission_mode: "read-only",
            allowed_tools: Some("read_file,grep_search"),
            stdin: None,
            prepare: prepare_multi_tool_fixture,
            assert: assert_multi_tool_turn_roundtrip,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "bash_stdout_roundtrip",
            permission_mode: "danger-full-access",
            allowed_tools: Some("bash"),
            stdin: None,
            prepare: prepare_noop,
            assert: assert_bash_stdout_roundtrip,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "bash_permission_prompt_approved",
            permission_mode: "workspace-write",
            allowed_tools: Some("bash"),
            stdin: Some("y\n"),
            prepare: prepare_noop,
            assert: assert_bash_permission_prompt_approved,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "bash_permission_prompt_denied",
            permission_mode: "workspace-write",
            allowed_tools: Some("bash"),
            stdin: Some("n\n"),
            prepare: prepare_noop,
            assert: assert_bash_permission_prompt_denied,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "plugin_tool_roundtrip",
            permission_mode: "workspace-write",
            allowed_tools: None,
            stdin: None,
            prepare: prepare_plugin_fixture,
            assert: assert_plugin_tool_roundtrip,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "auto_compact_triggered",
            permission_mode: "read-only",
            allowed_tools: None,
            stdin: None,
            prepare: prepare_noop,
            assert: assert_auto_compact_triggered,
            extra_env: None,
            resume_session: None,
        },
        ScenarioCase {
            name: "token_cost_reporting",
            permission_mode: "read-only",
            allowed_tools: None,
            stdin: None,
            prepare: prepare_noop,
            assert: assert_token_cost_reporting,
            extra_env: None,
            resume_session: None,
        },
    ];

    let case_names = cases.iter().map(|case| case.name).collect::<Vec<_>>();
    let manifest_names = manifest_entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        case_names, manifest_names,
        "manifest and harness cases must stay aligned"
    );

    let mut scenario_reports = Vec::new();

    for case in cases {
        let workspace = HarnessWorkspace::new(unique_temp_dir(case.name));
        workspace.create().expect("workspace should exist");
        (case.prepare)(&workspace);

        let run = run_case(case, &workspace, &base_url);
        (case.assert)(&workspace, &run);

        let manifest_entry = manifest
            .get(case.name)
            .unwrap_or_else(|| panic!("missing manifest entry for {}", case.name));
        scenario_reports.push(build_scenario_report(
            case.name,
            manifest_entry,
            &run.response,
        ));

        fs::remove_dir_all(&workspace.root).expect("workspace cleanup should succeed");
    }

    let captured = runtime.block_on(server.captured_requests());
    // After `be561bf` added count_tokens preflight, each turn sends an
    // extra POST to `/v1/messages/count_tokens` before the messages POST.
    // The original count (21) assumed messages-only requests.  We now
    // filter to `/v1/messages` and verify that subset matches the original
    // scenario expectation.
    let messages_only: Vec<_> = captured
        .iter()
        .filter(|r| r.path == "/v1/messages")
        .collect();
    assert_eq!(
        messages_only.len(),
        21,
        "twelve scenarios should produce twenty-one /v1/messages requests (total captured: {}, includes count_tokens)",
        captured.len()
    );
    assert!(messages_only.iter().all(|request| request.stream));

    let scenarios = messages_only
        .iter()
        .map(|request| request.scenario.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        scenarios,
        vec![
            "streaming_text",
            "read_file_roundtrip",
            "read_file_roundtrip",
            "grep_chunk_assembly",
            "grep_chunk_assembly",
            "write_file_allowed",
            "write_file_allowed",
            "write_file_denied",
            "write_file_denied",
            "multi_tool_turn_roundtrip",
            "multi_tool_turn_roundtrip",
            "bash_stdout_roundtrip",
            "bash_stdout_roundtrip",
            "bash_permission_prompt_approved",
            "bash_permission_prompt_approved",
            "bash_permission_prompt_denied",
            "bash_permission_prompt_denied",
            "plugin_tool_roundtrip",
            "plugin_tool_roundtrip",
            "auto_compact_triggered",
            "token_cost_reporting",
        ]
    );

    let mut request_counts = BTreeMap::new();
    for request in &captured {
        *request_counts
            .entry(request.scenario.as_str())
            .or_insert(0_usize) += 1;
    }
    for report in &mut scenario_reports {
        report.request_count = *request_counts
            .get(report.name.as_str())
            .unwrap_or_else(|| panic!("missing request count for {}", report.name));
    }

    maybe_write_report(&scenario_reports);
}

#[derive(Clone, Copy)]
struct ScenarioCase {
    name: &'static str,
    permission_mode: &'static str,
    allowed_tools: Option<&'static str>,
    stdin: Option<&'static str>,
    prepare: fn(&HarnessWorkspace),
    assert: fn(&HarnessWorkspace, &ScenarioRun),
    extra_env: Option<(&'static str, &'static str)>,
    resume_session: Option<&'static str>,
}

struct HarnessWorkspace {
    root: PathBuf,
    config_home: PathBuf,
    home: PathBuf,
}

impl HarnessWorkspace {
    fn new(root: PathBuf) -> Self {
        Self {
            config_home: root.join("config-home"),
            home: root.join("home"),
            root,
        }
    }

    fn create(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::create_dir_all(&self.config_home)?;
        fs::create_dir_all(&self.home)?;
        Ok(())
    }
}

struct ScenarioRun {
    response: Value,
    stdout: String,
}

#[derive(Debug, Clone)]
struct ScenarioManifestEntry {
    name: String,
    category: String,
    description: String,
    parity_refs: Vec<String>,
}

#[derive(Debug)]
struct ScenarioReport {
    name: String,
    category: String,
    description: String,
    parity_refs: Vec<String>,
    iterations: u64,
    request_count: usize,
    tool_uses: Vec<String>,
    tool_error_count: usize,
    final_message: String,
}

fn run_case(case: ScenarioCase, workspace: &HarnessWorkspace, base_url: &str) -> ScenarioRun {
    let mut command = Command::new(env!("CARGO_BIN_EXE_claw"));
    command
        .current_dir(&workspace.root)
        .env_clear()
        .env("ANTHROPIC_API_KEY", "test-parity-key")
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("CLAW_CONFIG_HOME", &workspace.config_home)
        .env("HOME", &workspace.home)
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .args([
            "--model",
            "sonnet",
            "--permission-mode",
            case.permission_mode,
            "--output-format=json",
        ]);

    if let Some(allowed_tools) = case.allowed_tools {
        command.args(["--allowedTools", allowed_tools]);
    }
    if let Some((key, value)) = case.extra_env {
        command.env(key, value);
    }
    if let Some(session_id) = case.resume_session {
        command.args(["--resume", session_id]);
    }

    let prompt = format!("{SCENARIO_PREFIX}{}", case.name);
    command.arg(prompt);

    let output = if let Some(stdin) = case.stdin {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("claw should launch");
        child
            .stdin
            .as_mut()
            .expect("stdin should be piped")
            .write_all(stdin.as_bytes())
            .expect("stdin should write");
        child.wait_with_output().expect("claw should finish")
    } else {
        command.output().expect("claw should launch")
    };

    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    ScenarioRun {
        response: parse_json_output(&stdout),
        stdout,
    }
}

#[allow(dead_code)]
fn prepare_auto_compact_fixture(workspace: &HarnessWorkspace) {
    let sessions_dir = workspace.root.join(".claw").join("sessions");
    fs::create_dir_all(&sessions_dir).expect("sessions dir should exist");

    // Write a pre-seeded session with 6 messages so auto-compact can remove them
    let session_id = "parity-auto-compact-seed";
    let session_jsonl = r#"{"type":"session_meta","version":3,"session_id":"parity-auto-compact-seed","created_at_ms":1743724800000,"updated_at_ms":1743724800000}
{"type":"message","message":{"role":"user","blocks":[{"type":"text","text":"step one of the parity scenario"}]}}
{"type":"message","message":{"role":"assistant","blocks":[{"type":"text","text":"acknowledged step one"}]}}
{"type":"message","message":{"role":"user","blocks":[{"type":"text","text":"step two of the parity scenario"}]}}
{"type":"message","message":{"role":"assistant","blocks":[{"type":"text","text":"acknowledged step two"}]}}
{"type":"message","message":{"role":"user","blocks":[{"type":"text","text":"step three of the parity scenario"}]}}
{"type":"message","message":{"role":"assistant","blocks":[{"type":"text","text":"acknowledged step three"}]}}
"#;
    fs::write(
        sessions_dir.join(format!("{session_id}.jsonl")),
        session_jsonl,
    )
    .expect("pre-seeded session should write");
}

fn prepare_noop(_: &HarnessWorkspace) {}

fn prepare_read_fixture(workspace: &HarnessWorkspace) {
    fs::write(workspace.root.join("fixture.txt"), "alpha parity line\n")
        .expect("fixture should write");
}

fn prepare_grep_fixture(workspace: &HarnessWorkspace) {
    fs::write(
        workspace.root.join("fixture.txt"),
        "alpha parity line\nbeta line\ngamma parity line\n",
    )
    .expect("grep fixture should write");
}

fn prepare_multi_tool_fixture(workspace: &HarnessWorkspace) {
    fs::write(
        workspace.root.join("fixture.txt"),
        "alpha parity line\nbeta line\ngamma parity line\n",
    )
    .expect("multi tool fixture should write");
}

fn prepare_plugin_fixture(workspace: &HarnessWorkspace) {
    let plugin_root = workspace
        .root
        .join("external-plugins")
        .join("parity-plugin");
    let tool_dir = plugin_root.join("tools");
    let manifest_dir = plugin_root.join(".claude-plugin");
    fs::create_dir_all(&tool_dir).expect("plugin tools dir");
    fs::create_dir_all(&manifest_dir).expect("plugin manifest dir");

    let script_path = tool_dir.join("echo-json.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\nINPUT=$(cat)\nprintf '{\"plugin\":\"%s\",\"tool\":\"%s\",\"input\":%s}\\n' \"$CLAWD_PLUGIN_ID\" \"$CLAWD_TOOL_NAME\" \"$INPUT\"\n",
    )
    .expect("plugin script should write");
    let mut permissions = fs::metadata(&script_path)
        .expect("plugin script metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("plugin script should be executable");

    fs::write(
        manifest_dir.join("plugin.json"),
        r#"{
  "name": "parity-plugin",
  "version": "1.0.0",
  "description": "mock parity plugin",
  "tools": [
    {
      "name": "plugin_echo",
      "description": "Echo JSON input",
      "inputSchema": {
        "type": "object",
        "properties": {
          "message": { "type": "string" }
        },
        "required": ["message"],
        "additionalProperties": false
      },
      "command": "./tools/echo-json.sh",
      "requiredPermission": "workspace-write"
    }
  ]
}"#,
    )
    .expect("plugin manifest should write");

    fs::write(
        workspace.config_home.join("settings.json"),
        json!({
            "enabledPlugins": {
                "parity-plugin@external": true
            },
            "plugins": {
                "externalDirectories": [plugin_root.parent().expect("plugin parent").display().to_string()]
            }
        })
        .to_string(),
    )
    .expect("plugin settings should write");
}

fn assert_streaming_text(_: &HarnessWorkspace, run: &ScenarioRun) {
    assert_eq!(
        run.response["message"],
        Value::String("Mock streaming says hello from the parity harness.".to_string())
    );
    assert_eq!(run.response["iterations"], Value::from(1));
    assert_eq!(run.response["tool_uses"], Value::Array(Vec::new()));
    assert_eq!(run.response["tool_results"], Value::Array(Vec::new()));
}

fn assert_read_file_roundtrip(workspace: &HarnessWorkspace, run: &ScenarioRun) {
    assert_eq!(run.response["iterations"], Value::from(2));
    assert_eq!(
        run.response["tool_uses"][0]["name"],
        Value::String("read_file".to_string())
    );
    assert_eq!(
        run.response["tool_uses"][0]["input"],
        Value::String(r#"{"path":"fixture.txt"}"#.to_string())
    );
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("alpha parity line"));
    let output = run.response["tool_results"][0]["output"]
        .as_str()
        .expect("tool output");
    assert!(output.contains(&workspace.root.join("fixture.txt").display().to_string()));
    assert!(output.contains("alpha parity line"));
}

fn assert_grep_chunk_assembly(_: &HarnessWorkspace, run: &ScenarioRun) {
    assert_eq!(run.response["iterations"], Value::from(2));
    assert_eq!(
        run.response["tool_uses"][0]["name"],
        Value::String("grep_search".to_string())
    );
    assert_eq!(
        run.response["tool_uses"][0]["input"],
        Value::String(
            r#"{"pattern":"parity","path":"fixture.txt","output_mode":"count"}"#.to_string()
        )
    );
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("2 occurrences"));
    assert_eq!(
        run.response["tool_results"][0]["is_error"],
        Value::Bool(false)
    );
}

fn assert_write_file_allowed(workspace: &HarnessWorkspace, run: &ScenarioRun) {
    assert_eq!(run.response["iterations"], Value::from(2));
    assert_eq!(
        run.response["tool_uses"][0]["name"],
        Value::String("write_file".to_string())
    );
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("generated/output.txt"));
    let generated = workspace.root.join("generated").join("output.txt");
    let contents = fs::read_to_string(&generated).expect("generated file should exist");
    assert_eq!(contents, "created by mock service\n");
    assert_eq!(
        run.response["tool_results"][0]["is_error"],
        Value::Bool(false)
    );
}

fn assert_write_file_denied(workspace: &HarnessWorkspace, run: &ScenarioRun) {
    assert_eq!(run.response["iterations"], Value::from(2));
    assert_eq!(
        run.response["tool_uses"][0]["name"],
        Value::String("write_file".to_string())
    );
    let tool_output = run.response["tool_results"][0]["output"]
        .as_str()
        .expect("tool output");
    assert!(tool_output.contains("requires workspace-write permission"));
    assert_eq!(
        run.response["tool_results"][0]["is_error"],
        Value::Bool(true)
    );
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("denied as expected"));
    assert!(!workspace.root.join("generated").join("denied.txt").exists());
}

fn assert_multi_tool_turn_roundtrip(_: &HarnessWorkspace, run: &ScenarioRun) {
    assert_eq!(run.response["iterations"], Value::from(2));
    let tool_uses = run.response["tool_uses"]
        .as_array()
        .expect("tool uses array");
    assert_eq!(
        tool_uses.len(),
        2,
        "expected two tool uses in a single turn"
    );
    assert_eq!(tool_uses[0]["name"], Value::String("read_file".to_string()));
    assert_eq!(
        tool_uses[1]["name"],
        Value::String("grep_search".to_string())
    );
    let tool_results = run.response["tool_results"]
        .as_array()
        .expect("tool results array");
    assert_eq!(
        tool_results.len(),
        2,
        "expected two tool results in a single turn"
    );
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("alpha parity line"));
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("2 occurrences"));
}

fn assert_bash_stdout_roundtrip(_: &HarnessWorkspace, run: &ScenarioRun) {
    assert_eq!(run.response["iterations"], Value::from(2));
    assert_eq!(
        run.response["tool_uses"][0]["name"],
        Value::String("bash".to_string())
    );
    let tool_output = run.response["tool_results"][0]["output"]
        .as_str()
        .expect("tool output");
    let parsed: Value = serde_json::from_str(tool_output).expect("bash output json");
    assert_eq!(
        parsed["stdout"],
        Value::String("alpha from bash".to_string())
    );
    assert_eq!(
        run.response["tool_results"][0]["is_error"],
        Value::Bool(false)
    );
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("alpha from bash"));
}

fn assert_bash_permission_prompt_approved(_: &HarnessWorkspace, run: &ScenarioRun) {
    assert!(run.stdout.contains("Permission approval required"));
    assert!(run.stdout.contains("Approve this tool call? [y/N]:"));
    assert_eq!(run.response["iterations"], Value::from(2));
    assert_eq!(
        run.response["tool_results"][0]["is_error"],
        Value::Bool(false)
    );
    let tool_output = run.response["tool_results"][0]["output"]
        .as_str()
        .expect("tool output");
    let parsed: Value = serde_json::from_str(tool_output).expect("bash output json");
    assert_eq!(
        parsed["stdout"],
        Value::String("approved via prompt".to_string())
    );
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("approved and executed"));
}

fn assert_bash_permission_prompt_denied(_: &HarnessWorkspace, run: &ScenarioRun) {
    assert!(run.stdout.contains("Permission approval required"));
    assert!(run.stdout.contains("Approve this tool call? [y/N]:"));
    assert_eq!(run.response["iterations"], Value::from(2));
    let tool_output = run.response["tool_results"][0]["output"]
        .as_str()
        .expect("tool output");
    assert!(tool_output.contains("denied by user approval prompt"));
    assert_eq!(
        run.response["tool_results"][0]["is_error"],
        Value::Bool(true)
    );
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("denied as expected"));
}

fn assert_plugin_tool_roundtrip(_: &HarnessWorkspace, run: &ScenarioRun) {
    assert_eq!(run.response["iterations"], Value::from(2));
    assert_eq!(
        run.response["tool_uses"][0]["name"],
        Value::String("plugin_echo".to_string())
    );
    let tool_output = run.response["tool_results"][0]["output"]
        .as_str()
        .expect("tool output");
    let parsed: Value = serde_json::from_str(tool_output).expect("plugin output json");
    assert_eq!(
        parsed["plugin"],
        Value::String("parity-plugin@external".to_string())
    );
    assert_eq!(parsed["tool"], Value::String("plugin_echo".to_string()));
    assert_eq!(
        parsed["input"]["message"],
        Value::String("hello from plugin parity".to_string())
    );
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("hello from plugin parity"));
}

fn assert_auto_compact_triggered(_: &HarnessWorkspace, run: &ScenarioRun) {
    // Validates that the auto_compaction field is present in JSON output (format parity).
    // Trigger behavior is covered by conversation::tests::auto_compacts_when_cumulative_input_threshold_is_crossed.
    assert_eq!(run.response["iterations"], Value::from(1));
    assert_eq!(run.response["tool_uses"], Value::Array(Vec::new()));
    assert!(
        run.response["message"]
            .as_str()
            .expect("message text")
            .contains("auto compact parity complete."),
        "expected auto compact message in response"
    );
    // auto_compaction key must be present in JSON (may be null for below-threshold sessions)
    assert!(
        run.response
            .as_object()
            .expect("response object")
            .contains_key("auto_compaction"),
        "auto_compaction key must be present in JSON output"
    );
    // Verify input_tokens field reflects the large mock token counts
    let input_tokens = run.response["usage"]["input_tokens"]
        .as_u64()
        .expect("input_tokens should be present");
    assert!(
        input_tokens >= 50_000,
        "input_tokens should reflect mock service value (got {input_tokens})"
    );
}

fn assert_token_cost_reporting(_: &HarnessWorkspace, run: &ScenarioRun) {
    assert_eq!(run.response["iterations"], Value::from(1));
    assert!(run.response["message"]
        .as_str()
        .expect("message text")
        .contains("token cost reporting parity complete."),);
    let usage = &run.response["usage"];
    assert!(
        usage["input_tokens"].as_u64().unwrap_or(0) > 0,
        "input_tokens should be non-zero"
    );
    assert!(
        usage["output_tokens"].as_u64().unwrap_or(0) > 0,
        "output_tokens should be non-zero"
    );
    assert!(
        run.response["estimated_cost"]
            .as_str()
            .is_some_and(|cost| cost.starts_with('$')),
        "estimated_cost should be a dollar-prefixed string"
    );
}

fn parse_json_output(stdout: &str) -> Value {
    if let Some(index) = stdout.rfind("{\"auto_compaction\"") {
        return serde_json::from_str(&stdout[index..]).unwrap_or_else(|error| {
            panic!("failed to parse JSON response from stdout: {error}\n{stdout}")
        });
    }

    stdout
        .lines()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('{') && trimmed.ends_with('}') {
                serde_json::from_str(trimmed).ok()
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no JSON response line found in stdout:\n{stdout}"))
}

fn build_scenario_report(
    name: &str,
    manifest_entry: &ScenarioManifestEntry,
    response: &Value,
) -> ScenarioReport {
    ScenarioReport {
        name: name.to_string(),
        category: manifest_entry.category.clone(),
        description: manifest_entry.description.clone(),
        parity_refs: manifest_entry.parity_refs.clone(),
        iterations: response["iterations"]
            .as_u64()
            .expect("iterations should exist"),
        request_count: 0,
        tool_uses: response["tool_uses"]
            .as_array()
            .expect("tool uses array")
            .iter()
            .filter_map(|value| value["name"].as_str().map(ToOwned::to_owned))
            .collect(),
        tool_error_count: response["tool_results"]
            .as_array()
            .expect("tool results array")
            .iter()
            .filter(|value| value["is_error"].as_bool().unwrap_or(false))
            .count(),
        final_message: response["message"]
            .as_str()
            .expect("message text")
            .to_string(),
    }
}

fn maybe_write_report(reports: &[ScenarioReport]) {
    let Some(path) = std::env::var_os("MOCK_PARITY_REPORT_PATH") else {
        return;
    };

    let payload = json!({
        "scenario_count": reports.len(),
        "request_count": reports.iter().map(|report| report.request_count).sum::<usize>(),
        "scenarios": reports.iter().map(scenario_report_json).collect::<Vec<_>>(),
    });
    fs::write(
        path,
        serde_json::to_vec_pretty(&payload).expect("report json should serialize"),
    )
    .expect("report should write");
}

fn load_scenario_manifest() -> Vec<ScenarioManifestEntry> {
    let manifest_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mock_parity_scenarios.json");
    let manifest = fs::read_to_string(&manifest_path).expect("scenario manifest should exist");
    serde_json::from_str::<Vec<Value>>(&manifest)
        .expect("scenario manifest should parse")
        .into_iter()
        .map(|entry| ScenarioManifestEntry {
            name: entry["name"]
                .as_str()
                .expect("scenario name should be a string")
                .to_string(),
            category: entry["category"]
                .as_str()
                .expect("scenario category should be a string")
                .to_string(),
            description: entry["description"]
                .as_str()
                .expect("scenario description should be a string")
                .to_string(),
            parity_refs: entry["parity_refs"]
                .as_array()
                .expect("parity refs should be an array")
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .expect("parity ref should be a string")
                        .to_string()
                })
                .collect(),
        })
        .collect()
}

fn scenario_report_json(report: &ScenarioReport) -> Value {
    json!({
        "name": report.name,
        "category": report.category,
        "description": report.description,
        "parity_refs": report.parity_refs,
        "iterations": report.iterations,
        "request_count": report.request_count,
        "tool_uses": report.tool_uses,
        "tool_error_count": report.tool_error_count,
        "final_message": report.final_message,
    })
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_millis();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "claw-mock-parity-{label}-{}-{millis}-{counter}",
        std::process::id()
    ))
}
