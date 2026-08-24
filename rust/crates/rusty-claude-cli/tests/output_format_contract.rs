use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::Session;
use serde_json::{json, Value};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn help_emits_json_when_requested() {
    let root = unique_temp_dir("help-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let parsed = assert_json_command(&root, &["--output-format", "json", "help"]);
    assert_eq!(parsed["kind"], "help");
    assert_eq!(
        parsed["status"], "ok",
        "help JSON must have status:ok (#700)"
    );
    assert!(parsed["message"]
        .as_str()
        .expect("help text")
        .contains("Usage:"));
    assert!(
        parsed["message"]
            .as_str()
            .expect("help text")
            .contains("--cwd PATH, -C PATH, --directory PATH"),
        "help JSON should document global cwd override (#429): {parsed}"
    );
}

#[test]
fn export_help_emits_bounded_json_when_requested_384() {
    let root = unique_temp_dir("export-help-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let parsed = assert_json_command(&root, &["export", "--help", "--output-format", "json"]);
    assert_eq!(parsed["kind"], "help");
    assert_eq!(
        parsed["status"], "ok",
        "export help JSON must have status:ok (#700)"
    );
    assert_eq!(parsed["topic"], "export");
    assert_eq!(parsed["command"], "export");
    assert_eq!(
        parsed["usage"],
        "claw export [--session <id|latest>] [--output <path>] [--output-format <format>]"
    );
    assert_eq!(parsed["defaults"]["session"], "latest");
    assert!(parsed["options"].as_array().expect("options").len() >= 4);
    assert!(parsed.get("message").is_none());
}

#[test]
fn export_help_preserves_plaintext_in_text_mode_384() {
    let root = unique_temp_dir("export-help-text");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let output = run_claw(&root, &["export", "--help"], &[]);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    assert!(stdout.starts_with("Export\n"));
    assert!(stdout.contains("Usage            claw export"));
    serde_json::from_str::<Value>(&stdout).expect_err("text help should remain plaintext");
}

#[test]
fn doctor_help_json_is_local_structured_and_bounded_702() {
    let root = unique_temp_dir("doctor-help-json-702");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let parsed = assert_json_command(&root, &["--output-format", "json", "doctor", "--help"]);
    assert_doctor_help_json_contract(&parsed);

    let suffix_parsed =
        assert_json_command(&root, &["doctor", "--help", "--output-format", "json"]);
    assert_doctor_help_json_contract(&suffix_parsed);

    let help_topic_parsed =
        assert_json_command(&root, &["help", "doctor", "--output-format", "json"]);
    assert_doctor_help_json_contract(&help_topic_parsed);
}

fn assert_doctor_help_json_contract(parsed: &Value) {
    assert_eq!(parsed["kind"], "help");
    assert_eq!(parsed["action"], "help");
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["topic"], "doctor");
    assert_eq!(parsed["command"], "doctor");
    assert_eq!(parsed["usage"], "claw doctor [--output-format <format>]");
    assert_eq!(parsed["local_only"], true);
    assert_eq!(parsed["requires_credentials"], false);
    assert_eq!(parsed["requires_provider_request"], false);
    assert_eq!(parsed["requires_session_resume"], false);
    assert_eq!(parsed["mutates_workspace"], false);

    let fields = parsed["output_fields"].as_array().expect("output_fields");
    assert!(fields.iter().any(|field| field == "checks"));
    let statuses = parsed["status_values"].as_array().expect("status_values");
    assert!(statuses.iter().any(|status| status == "warn"));
    let checks = parsed["check_names"].as_array().expect("check_names");
    assert!(checks.iter().any(|check| check == "auth"));
    assert!(checks.iter().any(|check| check == "boot preflight"));
    assert!(checks.iter().any(|check| check == "memory"));
    assert!(checks.iter().any(|check| check == "mcp validation"));
    assert!(checks.iter().any(|check| check == "hook validation"));
}

#[test]
fn doctor_help_text_stays_plaintext_and_local_702() {
    let root = unique_temp_dir("doctor-help-text-702");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let output = run_claw(&root, &["doctor", "--help"], &[]);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    assert!(stdout.starts_with("Doctor\n"));
    assert!(stdout.contains("Usage            claw doctor"));
    assert!(stdout.contains("no provider request or session resume required"));
    serde_json::from_str::<Value>(&stdout).expect_err("text help should remain plaintext");
}

#[test]
fn resume_session_compact_help_short_circuits_before_config_or_auth_427() {
    let root = unique_temp_dir("session-help-local-427");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(root.join(".claw")).expect("project config dir should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(root.join(".claw").join("settings.json"), "{").expect("broken config should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
        ("ANTHROPIC_API_KEY", ""),
        ("ANTHROPIC_AUTH_TOKEN", ""),
        ("OPENAI_API_KEY", ""),
    ];

    let text_cases: &[(&[&str], &str)] = &[
        (&["resume", "--help"], "Resume\n"),
        (&["--resume", "--help"], "Resume\n"),
        (&["session", "--help"], "Session\n"),
        (&["compact", "--help"], "Compact\n"),
    ];
    for (args, heading) in text_cases {
        let output = run_claw(&root, args, &envs);
        assert!(
            output.status.success(),
            "{args:?} should exit 0 before auth/config; stdout:\n{}\n\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.starts_with(heading), "{args:?} stdout: {stdout}");
        assert!(stdout.contains("Usage"), "{args:?} stdout: {stdout}");
        assert!(
            !stdout.contains("missing_credentials") && !stderr.contains("missing_credentials"),
            "{args:?} must not hit provider auth: stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            !stdout.contains("config_parse_error") && stderr.is_empty(),
            "{args:?} must not load broken config: stdout={stdout:?} stderr={stderr:?}"
        );
        serde_json::from_str::<Value>(&stdout).expect_err("text help should remain plaintext");
    }

    let json_cases: &[(&[&str], &str)] = &[
        (&["resume", "--help", "--output-format", "json"], "resume"),
        (&["--resume", "--help", "--output-format", "json"], "resume"),
        (&["session", "--help", "--output-format", "json"], "session"),
        (&["compact", "--help", "--output-format", "json"], "compact"),
    ];
    for (args, topic) in json_cases {
        let parsed = assert_json_command_with_env(&root, args, &envs);
        assert_eq!(parsed["kind"], "help", "{args:?}: {parsed}");
        assert_eq!(parsed["status"], "ok", "{args:?}: {parsed}");
        assert_eq!(parsed["topic"], *topic, "{args:?}: {parsed}");
        assert!(
            parsed["message"]
                .as_str()
                .is_some_and(|message| message.contains("Usage")),
            "{args:?} should include static usage text: {parsed}"
        );
    }
}

#[test]
fn resume_missing_session_json_reports_local_store_before_auth_427() {
    let root = unique_temp_dir("resume-missing-local-427");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&root).expect("temp dir should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
        ("ANTHROPIC_API_KEY", ""),
        ("ANTHROPIC_AUTH_TOKEN", ""),
        ("OPENAI_API_KEY", ""),
    ];

    let output = run_claw(
        &root,
        &[
            "resume",
            "definitely-missing-session",
            "--output-format",
            "json",
        ],
        &envs,
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "missing session should exit 1"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.is_empty(),
        "JSON missing-session stderr should be empty: {stderr:?}"
    );
    assert!(
        !stdout.contains("missing_credentials") && !stderr.contains("missing_credentials"),
        "missing session must not reach provider auth: stdout={stdout:?} stderr={stderr:?}"
    );
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("resume missing session must emit JSON, got: {stdout:?}"));
    assert_eq!(parsed["error_kind"], "session_not_found", "{parsed}");
    assert_eq!(parsed["action"], "restore", "{parsed}");
    assert!(
        parsed["sessions_dir"]
            .as_str()
            .is_some_and(|path| path.contains(".claw") && path.contains("sessions")),
        "missing-session JSON should expose the searched sessions_dir: {parsed}"
    );
}

#[test]
fn version_emits_json_when_requested() {
    let root = unique_temp_dir("version-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let parsed = assert_json_command(&root, &["--output-format", "json", "version"]);
    assert_eq!(parsed["kind"], "version");
    assert_eq!(
        parsed["action"], "show",
        "version JSON must have action:show (#711)"
    );
    assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
    // Provenance fields must be present for binary identification (#507/#437).
    assert!(
        parsed.get("message").is_none(),
        "version JSON should not duplicate the text report in legacy message; use human_readable instead: {parsed}"
    );
    assert!(
        parsed["human_readable"]
            .as_str()
            .is_some_and(|text| text.contains("Claw Code")),
        "version JSON should keep text output only in human_readable: {parsed}"
    );
    let git_sha = parsed["git_sha"]
        .as_str()
        .expect("git_sha must be the full build commit SHA in version JSON");
    assert_eq!(git_sha.len(), 40, "git_sha must not be truncated: {parsed}");
    assert!(
        git_sha.chars().all(|ch| ch.is_ascii_hexdigit()),
        "git_sha must be a hex commit id: {parsed}"
    );
    let git_sha_short = parsed["git_sha_short"]
        .as_str()
        .expect("version JSON should expose the short SHA as a separate derived field");
    assert!(
        git_sha.starts_with(git_sha_short),
        "git_sha_short should derive from git_sha: {parsed}"
    );
    assert!(
        parsed["is_dirty"].is_boolean(),
        "is_dirty should be boolean: {parsed}"
    );
    assert!(
        parsed["branch"].is_string() || parsed["branch"].is_null(),
        "branch should be string|null: {parsed}"
    );
    assert!(
        parsed["commit_date"]
            .as_str()
            .is_some_and(|date| date != "unknown" && date.contains('T')),
        "commit_date should be an ISO-8601 commit timestamp string: {parsed}"
    );
    assert!(
        parsed["commit_timestamp"].as_i64().is_some_and(|ts| ts > 0),
        "commit_timestamp should be a positive Unix timestamp: {parsed}"
    );
    assert!(
        parsed["rustc_version"]
            .as_str()
            .is_some_and(|version| version.starts_with("rustc ")),
        "rustc_version should identify the compiler: {parsed}"
    );
    assert!(
        parsed["build_date"].is_string(),
        "build_date must be a string in version JSON"
    );
    assert!(
        parsed["executable_path"].as_str().is_some_and(|path| !path.is_empty()),
        "executable_path must be a runtime path string so callers can identify which binary is running"
    );
    let binary_provenance = parsed["binary_provenance"]
        .as_object()
        .expect("version JSON must include binary_provenance object (#797/#437)");
    assert!(matches!(
        binary_provenance["status"].as_str(),
        Some("known" | "unknown")
    ));
    for key in [
        "git_sha",
        "git_sha_short",
        "is_dirty",
        "branch",
        "commit_date",
        "commit_timestamp",
        "rustc_version",
        "target",
        "build_date",
        "executable_path",
    ] {
        assert_eq!(
            binary_provenance[key], parsed[key],
            "binary_provenance.{key} should mirror top-level version field"
        );
    }
    assert!(
        binary_provenance["hint"].is_string() || binary_provenance["hint"].is_null(),
        "binary provenance must classify missing/stale lineage with a structured hint field"
    );
}

#[test]
fn version_status_doctor_include_binary_provenance_797() {
    let root = git_temp_dir("binary-provenance-797");
    fs::write(root.join("tracked.txt"), "v1").expect("write tracked file");
    let git_commands: &[&[&str]] = &[
        &["config", "user.email", "test@claw.test"],
        &["config", "user.name", "Test"],
        &["add", "tracked.txt"],
        &["commit", "-m", "init"],
    ];
    for args in git_commands {
        let output = Command::new("git")
            .args(*args)
            .current_dir(&root)
            .output()
            .expect("git fixture command should launch");
        assert!(
            output.status.success(),
            "git fixture command failed: {args:?}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let version = assert_json_command(&root, &["--output-format", "json", "version"]);
    assert_eq!(version["kind"], "version");
    assert!(matches!(
        version["binary_provenance"]["status"].as_str(),
        Some("known" | "unknown")
    ));
    assert!(version["binary_provenance"]["workspace_git_sha"].is_string());
    assert!(
        version["binary_provenance"]["workspace_match"].is_boolean()
            || version["binary_provenance"]["workspace_match"].is_null()
    );
    let workspace_git_sha = version["binary_provenance"]["workspace_git_sha"]
        .as_str()
        .expect("workspace git sha should be a string");
    assert_eq!(
        workspace_git_sha.len(),
        40,
        "workspace_git_sha should be a full SHA, not a truncated prefix: {version}"
    );

    let status = assert_json_command(&root, &["--output-format", "json", "status"]);
    assert_eq!(status["kind"], "status");
    assert_eq!(
        status["binary_provenance"]["workspace_git_sha"],
        version["binary_provenance"]["workspace_git_sha"]
    );

    let doctor = assert_json_command(&root, &["--output-format", "json", "doctor"]);
    let system = doctor["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .find(|check| check["name"] == "system")
        .expect("system check");
    assert_eq!(
        system["binary_provenance"]["workspace_git_sha"],
        version["binary_provenance"]["workspace_git_sha"]
    );
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

// #831: direct resume-safe slash commands should use the same local CliAction
// JSON surfaces as their bare subcommands, not interactive_only guidance.
#[test]
fn direct_resume_safe_slash_commands_route_to_local_json_actions_831() {
    let root = unique_temp_dir("direct-resume-safe-slash-831");
    fs::create_dir_all(&root).expect("temp dir should exist");
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .expect("git init should launch");

    for (command, expected_kind, expected_status) in [
        ("/version", "version", "ok"),
        ("/sandbox", "sandbox", "warn"),
        ("/diff", "diff", "ok"),
        ("/status", "status", "ok"),
    ] {
        let output = run_claw(&root, &["--output-format", "json", command], &[]);
        assert!(
            output.status.success(),
            "{command} should route to a local CliAction, stdout:\n{}\n\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let parsed: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|_| panic!("{command} must emit JSON (#831), got: {stdout:?}"));

        assert_eq!(parsed["kind"], expected_kind, "{command} kind: {parsed}");
        assert_eq!(
            parsed["status"], expected_status,
            "{command} status: {parsed}"
        );
        assert_ne!(
            parsed["error_kind"], "interactive_only",
            "{command} must not emit interactive_only (#831): {parsed}"
        );
        assert!(
            stderr.is_empty(),
            "{command} JSON mode must keep stderr empty (#831): {stderr:?}"
        );
    }
}

#[test]
fn status_json_surfaces_permission_mode_override_for_security_audit() {
    let root = unique_temp_dir("status-json-permission-mode");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let parsed = assert_json_command(
        &root,
        &[
            "--permission-mode",
            "read-only",
            "--output-format",
            "json",
            "status",
        ],
    );

    assert_eq!(parsed["kind"], "status");
    assert_eq!(parsed["permission_mode"], "read-only");
    assert!(
        parsed["workspace"]["cwd"].as_str().is_some(),
        "status JSON should retain workspace context with permission mode"
    );

    fs::remove_dir_all(root).expect("cleanup temp dir");
}

#[test]
fn default_permission_mode_is_workspace_write_and_audited_428() {
    let root = unique_temp_dir("default-permission-mode-428");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&root).expect("temp dir should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
        ("RUSTY_CLAUDE_PERMISSION_MODE", ""),
    ];

    let status = assert_json_command_with_env(&root, &["--output-format", "json", "status"], &envs);
    assert_eq!(status["permission_mode"], "workspace-write");
    assert_eq!(status["permission_mode_source"], "default");

    let doctor = assert_json_command_with_env(&root, &["--output-format", "json", "doctor"], &envs);
    let permissions = doctor["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .find(|check| check["name"] == "permissions")
        .expect("permissions check");
    assert_eq!(permissions["status"], "ok");
    assert_eq!(permissions["mode"], "workspace-write");
    assert_eq!(permissions["source"], "default");
    assert_eq!(
        permissions["message"],
        "default permission mode is workspace-write"
    );
}

#[test]
fn explicit_danger_permission_mode_is_audited_and_alias_supported_428() {
    let root = unique_temp_dir("danger-permission-mode-428");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let status = assert_json_command(
        &root,
        &["--skip-permissions", "--output-format", "json", "status"],
    );
    assert_eq!(status["permission_mode"], "danger-full-access");
    assert_eq!(status["permission_mode_source"], "flag");

    let doctor = assert_json_command(
        &root,
        &[
            "--permission-mode",
            "danger-full-access",
            "--output-format",
            "json",
            "doctor",
        ],
    );
    let permissions = doctor["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .find(|check| check["name"] == "permissions")
        .expect("permissions check");
    assert_eq!(permissions["status"], "ok");
    assert_eq!(permissions["mode"], "danger-full-access");
    assert_eq!(permissions["source"], "flag");
    assert_eq!(permissions["source_explicit"], true);
}

#[test]
fn invalid_permission_mode_json_is_typed_428() {
    let root = unique_temp_dir("invalid-permission-mode-428");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let output = run_claw(
        &root,
        &[
            "--permission-mode",
            "bogus-mode",
            "status",
            "--output-format",
            "json",
        ],
        &[],
    );
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("invalid permission mode must emit JSON, got: {stdout:?}"));
    assert_eq!(parsed["error_kind"], "invalid_permission_mode");
    assert_eq!(parsed["kind"], "invalid_permission_mode");
    assert!(
        stderr.is_empty(),
        "JSON error stderr should be empty: {stderr:?}"
    );
}

#[test]
fn global_cwd_flag_routes_status_workspace_and_short_alias_429() {
    let parent = unique_temp_dir("global-cwd-parent-429");
    let workspace = parent.join("workspace");
    let launcher = parent.join("launcher");
    fs::create_dir_all(&workspace).expect("workspace dir should exist");
    fs::create_dir_all(&launcher).expect("launcher dir should exist");

    let workspace_str = workspace.to_str().expect("utf8 workspace");
    let expected_cwd = fs::canonicalize(&workspace)
        .expect("workspace should canonicalize")
        .display()
        .to_string();
    let status = assert_json_command(
        &launcher,
        &["--cwd", workspace_str, "--output-format", "json", "status"],
    );
    assert_eq!(status["kind"], "status");
    assert_eq!(status["workspace"]["cwd"], expected_cwd);

    let short_status = assert_json_command(
        &launcher,
        &["-C", workspace_str, "status", "--output-format", "json"],
    );
    assert_eq!(short_status["workspace"]["cwd"], expected_cwd);

    let directory_status = assert_json_command(
        &launcher,
        &[
            "--directory",
            workspace_str,
            "--output-format=json",
            "status",
        ],
    );
    assert_eq!(directory_status["workspace"]["cwd"], expected_cwd);
}

#[test]
fn global_cwd_flag_reports_typed_invalid_paths_429() {
    let root = unique_temp_dir("global-cwd-invalid-429");
    let file = root.join("not-a-directory");
    fs::create_dir_all(&root).expect("root dir should exist");
    fs::write(&file, "not a dir").expect("file fixture should write");

    let missing = root.join("missing");
    let output = run_claw(
        &root,
        &[
            "--cwd",
            missing.to_str().expect("utf8 missing path"),
            "status",
            "--output-format",
            "json",
        ],
        &[],
    );
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("invalid cwd should emit JSON, got: {stdout:?}"));
    assert_eq!(parsed["kind"], "invalid_cwd");
    assert_eq!(parsed["error_kind"], "invalid_cwd");
    assert_eq!(parsed["reason"], "not_found");
    assert_eq!(parsed["path"], missing.to_str().expect("utf8 missing path"));
    assert!(output.stderr.is_empty());

    let file_output = run_claw(
        &root,
        &[
            "--cwd",
            file.to_str().expect("utf8 file path"),
            "status",
            "--output-format=json",
        ],
        &[],
    );
    assert_eq!(file_output.status.code(), Some(1));
    let file_stdout = String::from_utf8_lossy(&file_output.stdout);
    let file_json: Value = serde_json::from_str(file_stdout.trim())
        .unwrap_or_else(|_| panic!("file cwd should emit JSON, got: {file_stdout:?}"));
    assert_eq!(file_json["kind"], "invalid_cwd");
    assert_eq!(file_json["reason"], "not_a_directory");

    let empty_output = run_claw(&root, &["--cwd", "", "status", "--output-format=json"], &[]);
    assert_eq!(empty_output.status.code(), Some(1));
    let empty_stdout = String::from_utf8_lossy(&empty_output.stdout);
    let empty_json: Value = serde_json::from_str(empty_stdout.trim())
        .unwrap_or_else(|_| panic!("empty cwd should emit JSON, got: {empty_stdout:?}"));
    assert_eq!(empty_json["kind"], "invalid_cwd");
    assert_eq!(empty_json["reason"], "empty");
}

#[test]
fn export_invalid_output_path_reports_typed_json_430() {
    let root = unique_temp_dir("export-invalid-output-430");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let missing_relative = "missing/transcript.md";
    let missing_output = run_claw(
        &root,
        &["--output-format", "json", "export", missing_relative],
        &[],
    );
    assert_eq!(missing_output.status.code(), Some(1));
    assert!(
        missing_output.stderr.is_empty(),
        "invalid export path JSON should keep stderr empty, got:\n{}",
        String::from_utf8_lossy(&missing_output.stderr)
    );
    let missing_stdout = String::from_utf8_lossy(&missing_output.stdout);
    let missing_json: Value = serde_json::from_str(missing_stdout.trim()).unwrap_or_else(|_| {
        panic!("invalid export path should emit JSON, got: {missing_stdout:?}")
    });
    assert_eq!(missing_json["kind"], "invalid_output_path");
    assert_eq!(missing_json["error_kind"], "invalid_output_path");
    assert_eq!(missing_json["reason"], "parent_not_found");
    assert_eq!(missing_json["path"], missing_relative);

    let directory = root.join("existing-directory");
    fs::create_dir_all(&directory).expect("directory fixture should exist");
    let directory_output = run_claw(
        &root,
        &[
            "--output-format=json",
            "export",
            "--output",
            directory.to_str().expect("utf8 directory path"),
        ],
        &[],
    );
    assert_eq!(directory_output.status.code(), Some(1));
    assert!(directory_output.stderr.is_empty());
    let directory_stdout = String::from_utf8_lossy(&directory_output.stdout);
    let directory_json: Value =
        serde_json::from_str(directory_stdout.trim()).unwrap_or_else(|_| {
            panic!("directory export path should emit JSON, got: {directory_stdout:?}")
        });
    assert_eq!(directory_json["kind"], "invalid_output_path");
    assert_eq!(directory_json["error_kind"], "invalid_output_path");
    assert_eq!(directory_json["reason"], "path_is_directory");
    assert_eq!(
        directory_json["path"],
        directory.to_str().expect("utf8 directory path")
    );
}

#[test]
fn status_json_accepts_namespaced_model_env_and_surfaces_alias_426() {
    let root = unique_temp_dir("status-model-env-426");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&root).expect("temp dir should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
        ("CLAW_MODEL", "opus"),
        ("ANTHROPIC_MODEL", ""),
        ("ANTHROPIC_DEFAULT_MODEL", ""),
    ];
    let parsed = assert_json_command_with_env(&root, &["--output-format", "json", "status"], &envs);

    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["model"], "anthropic/claude-opus-4-7");
    assert_eq!(parsed["model_source"], "env");
    assert_eq!(parsed["model_raw"], "opus");
    assert_eq!(
        parsed["model_alias_resolved_to"],
        "anthropic/claude-opus-4-7"
    );
    assert_eq!(parsed["model_env_var"], "CLAW_MODEL");
}

#[test]
fn status_json_warns_on_invalid_model_env_426() {
    let root = unique_temp_dir("status-invalid-model-env-426");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&root).expect("temp dir should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
        ("CLAW_MODEL", ""),
        ("ANTHROPIC_MODEL", "bogus-model-xyz"),
        ("ANTHROPIC_DEFAULT_MODEL", ""),
    ];
    let output = run_claw(&root, &["--output-format", "json", "status"], &envs);
    assert!(
        output.status.success(),
        "invalid env model should produce status warn, not process abort; stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("stdout valid json");

    assert_eq!(parsed["kind"], "status");
    assert_eq!(parsed["status"], "warn");
    assert_eq!(parsed["model"], Value::Null);
    assert_eq!(parsed["model_validation_error_kind"], "invalid_model");
    assert_eq!(parsed["error_kind"], "invalid_model");
    assert!(
        parsed["model_validation_error"]
            .as_str()
            .is_some_and(|message| message.contains("ANTHROPIC_MODEL")
                && message.contains("bogus-model-xyz")),
        "warning should name env var and raw model: {parsed}"
    );
    assert!(
        parsed["workspace"].is_object(),
        "status warning should keep local context: {parsed}"
    );
}

#[test]
fn acp_guidance_emits_json_when_requested() {
    let root = unique_temp_dir("acp-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    // #443: acp serve exits 2 (not implemented) instead of 0
    let output = run_claw(&root, &["--output-format", "json", "acp"], &[]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "acp should exit 2 (not implemented)"
    );
    let acp: Value =
        serde_json::from_slice(&output.stdout).expect("acp stdout should be valid json");
    assert_eq!(acp["kind"], "acp");
    assert_eq!(acp["schema_version"], "1.0");
    assert_eq!(acp["status"], "not_implemented");
    assert_eq!(acp["supported"], false);
    assert_eq!(acp["protocol"]["json_rpc"], false);
    assert_eq!(acp["protocol"]["daemon"], false);
    assert!(acp["protocol"]["endpoint"].is_null());
    assert_eq!(
        acp["contracts"]["unsupported_invocation_kind"],
        "unsupported_acp_invocation"
    );
    // #443: internal tracking IDs removed from public JSON
    assert!(
        acp.get("discoverability_tracking").is_none(),
        "discoverability_tracking should be removed (#443)"
    );
    assert!(
        acp.get("tracking").is_none(),
        "tracking should be removed (#443)"
    );
    assert!(
        acp.get("recommended_workflows").is_none(),
        "recommended_workflows should be removed (#443)"
    );
    assert!(acp["message"]
        .as_str()
        .expect("acp message")
        .contains("not implemented"));
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

    // #717: agents show <name> and agents list <filter> should be valid subcommands
    let agents_show_env = [
        ("HOME", isolated_home.to_str().expect("utf8 home")),
        (
            "CLAW_CONFIG_HOME",
            isolated_config.to_str().expect("utf8 config home"),
        ),
        (
            "CODEX_HOME",
            isolated_codex.to_str().expect("utf8 codex home"),
        ),
    ];
    // #789: agents show not-found now exits 1 (parity with skills #788);
    // use run_claw directly instead of assert_json_command_with_env which checks success.
    let agents_show_out = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "agents",
            "show",
            "nonexistent-xyz",
        ],
        &agents_show_env,
    );
    assert!(
        !agents_show_out.status.success(),
        "agents show not-found must exit non-zero"
    );
    let agents_show_missing: serde_json::Value =
        serde_json::from_slice(&agents_show_out.stdout).expect("agents show stdout should be json");
    assert_eq!(agents_show_missing["kind"], "agents", "agents show kind");
    assert_eq!(agents_show_missing["action"], "show", "agents show action");
    assert_eq!(
        agents_show_missing["status"], "error",
        "agents show not-found status"
    );
    assert_eq!(
        agents_show_missing["error_kind"], "agent_not_found",
        "agents show error_kind"
    );
    assert_eq!(
        agents_show_missing["requested"], "nonexistent-xyz",
        "agents show requested"
    );

    let agents_list_filtered = assert_json_command_with_env(
        &root,
        &[
            "--output-format",
            "json",
            "agents",
            "list",
            "nonexistent-filter-xyz",
        ],
        &agents_show_env,
    );
    assert_eq!(
        agents_list_filtered["kind"], "agents",
        "agents list filter kind"
    );
    assert_eq!(
        agents_list_filtered["action"], "list",
        "agents list filter action"
    );
    assert_eq!(
        agents_list_filtered["status"], "ok",
        "agents list filter status"
    );
    assert!(agents_list_filtered["agents"]
        .as_array()
        .expect("agents array")
        .is_empty());

    let mcp = assert_json_command(&root, &["--output-format", "json", "mcp"]);
    assert_eq!(mcp["kind"], "mcp");
    assert_eq!(mcp["action"], "list");
    assert_eq!(mcp["status"], "ok");
    assert!(mcp["config_load_error"].is_null());

    let skills = assert_json_command(&root, &["--output-format", "json", "skills"]);
    assert_eq!(skills["kind"], "skills");
    assert_eq!(skills["action"], "list");

    let plugins = assert_json_command(&root, &["--output-format", "json", "plugins"]);
    assert_eq!(plugins["kind"], "plugin");
    assert_eq!(plugins["action"], "list");
    assert_eq!(plugins["status"], "ok");
    assert!(plugins["config_load_error"].is_null());
    // reload_runtime and target are operation-result fields; list response omits them (#703)
    assert!(
        !plugins
            .as_object()
            .map_or(false, |o| o.contains_key("reload_runtime")),
        "plugins list should not include reload_runtime"
    );
    assert!(
        !plugins
            .as_object()
            .map_or(false, |o| o.contains_key("target")),
        "plugins list should not include target"
    );
    // #703: structured summary replaces prose message
    assert!(
        plugins["summary"]["total"].is_number(),
        "plugins list should have summary.total"
    );
    assert!(
        plugins["summary"]["enabled"].is_number(),
        "plugins list should have summary.enabled"
    );
    assert!(
        plugins["summary"]["disabled"].is_number(),
        "plugins list should have summary.disabled"
    );
    assert_eq!(plugins["status"], "ok");
    let plugin_entries = plugins["plugins"].as_array().expect("plugins array");
    for plugin in plugin_entries {
        assert!(
            plugin["lifecycle_state"].is_string(),
            "plugin entries should expose lifecycle_state"
        );
        assert!(
            plugin["lifecycle"]["configured"].is_boolean(),
            "plugin entries should expose lifecycle contract summary"
        );
    }
    assert!(plugins["load_failures"]
        .as_array()
        .expect("plugin load failures array")
        .is_empty());
}

#[test]
fn plugins_json_surfaces_lifecycle_contract_when_plugin_is_installed() {
    let root = unique_temp_dir("plugin-lifecycle-json");
    let workspace = root.join("workspace");
    let home = root.join("home");
    let config_home = root.join("config-home");
    let plugin_root = root.join("source-plugin");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(plugin_root.join(".claude-plugin")).expect("manifest dir should exist");
    fs::create_dir_all(plugin_root.join("lifecycle")).expect("lifecycle dir should exist");
    fs::write(
        plugin_root.join("lifecycle").join("init.sh"),
        "#!/bin/sh\nexit 0\n",
    )
    .expect("init lifecycle script should write");
    fs::write(
        plugin_root.join("lifecycle").join("shutdown.sh"),
        "#!/bin/sh\nexit 0\n",
    )
    .expect("shutdown lifecycle script should write");
    fs::write(
        plugin_root.join(".claude-plugin").join("plugin.json"),
        r#"{
  "name": "lifecycle-json",
  "version": "1.0.0",
  "description": "lifecycle JSON fixture",
  "lifecycle": {
    "Init": ["./lifecycle/init.sh"],
    "Shutdown": ["./lifecycle/shutdown.sh"]
  }
}"#,
    )
    .expect("plugin manifest should write");

    let parsed = assert_json_command_with_env(
        &workspace,
        &[
            "--output-format",
            "json",
            "plugins",
            "install",
            plugin_root
                .to_str()
                .expect("plugin source path should be utf8"),
        ],
        &[
            ("HOME", home.to_str().expect("home path should be utf8")),
            (
                "CLAW_CONFIG_HOME",
                config_home.to_str().expect("config path should be utf8"),
            ),
        ],
    );

    assert_eq!(parsed["kind"], "plugin");
    assert_eq!(parsed["action"], "install");
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["reload_runtime"], true);
    assert!(parsed["load_failures"]
        .as_array()
        .expect("load_failures array")
        .is_empty());
    let plugins = parsed["plugins"].as_array().expect("plugins array");
    let plugin = plugins
        .iter()
        .find(|plugin| plugin["id"] == "lifecycle-json@external")
        .expect("installed plugin should be present");
    assert_eq!(plugin["enabled"], true);
    assert_eq!(plugin["lifecycle_state"], "ready");
    assert_eq!(plugin["lifecycle"]["configured"], true);
    assert_eq!(plugin["lifecycle"]["init"]["configured"], true);
    assert_eq!(plugin["lifecycle"]["init"]["command_count"], 1);
    assert_eq!(plugin["lifecycle"]["shutdown"]["configured"], true);
    assert_eq!(plugin["lifecycle"]["shutdown"]["command_count"], 1);
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
    assert_eq!(parsed["agents"][0]["source"]["label"], "Project roots");
    assert_eq!(parsed["agents"][0]["source"]["detail_label"], Value::Null);
    assert_eq!(parsed["agents"][0]["active"], true);
    assert_eq!(parsed["agents"][1]["name"], "verifier");
    assert_eq!(parsed["agents"][2]["name"], "planner");
    assert_eq!(parsed["agents"][2]["active"], false);
    assert_eq!(parsed["agents"][2]["shadowed_by"]["id"], "project_claw");
}

#[test]
fn agents_and_skills_inventory_share_source_schema_702() {
    let root = unique_temp_dir("inventory-source-schema-702");
    let workspace = root.join("workspace");
    let project_agents = workspace.join(".codex").join("agents");
    let project_skills = workspace.join(".codex").join("skills");
    let legacy_commands = workspace.join(".claude").join("commands");
    let home = root.join("home");
    let isolated_config = root.join("config-home");
    let isolated_codex = root.join("codex-home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&home).expect("home should exist");

    write_agent(
        &project_agents,
        "planner",
        "Project planner",
        "gpt-5.4",
        "medium",
    );
    write_skill(&project_skills, "plan", "Project planning guidance");
    write_legacy_command(&legacy_commands, "deploy", "Legacy deployment guidance");

    let envs = [
        ("HOME", home.to_str().expect("utf8 home")),
        (
            "CLAW_CONFIG_HOME",
            isolated_config.to_str().expect("utf8 config home"),
        ),
        (
            "CODEX_HOME",
            isolated_codex.to_str().expect("utf8 codex home"),
        ),
    ];
    let agents =
        assert_json_command_with_env(&workspace, &["--output-format", "json", "agents"], &envs);
    let skills =
        assert_json_command_with_env(&workspace, &["--output-format", "json", "skills"], &envs);

    let agent_source = &agents["agents"][0]["source"];
    let skill_source = &skills["skills"][0]["source"];
    for source in [agent_source, skill_source] {
        assert!(
            source.get("id").is_some(),
            "inventory source must expose id: {source}"
        );
        assert!(
            source.get("label").is_some(),
            "inventory source must expose label: {source}"
        );
        assert!(
            source.get("detail_label").is_some(),
            "inventory source must expose detail_label for a stable cross-resource path: {source}"
        );
    }
    assert_eq!(agent_source["id"], "project_claw");
    assert_eq!(agent_source["label"], "Project roots");
    assert_eq!(agent_source["detail_label"], Value::Null);
    assert_eq!(skill_source["id"], "project_claw");
    assert_eq!(skill_source["label"], "Project roots");
    assert_eq!(skill_source["detail_label"], Value::Null);

    let legacy_skill = skills["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .find(|skill| skill["name"] == "deploy")
        .expect("legacy command skill should be listed");
    assert_eq!(legacy_skill["source"]["id"], "project_claw");
    assert_eq!(legacy_skill["source"]["label"], "Project roots");
    assert_eq!(legacy_skill["source"]["detail_label"], "legacy /commands");
    assert_eq!(
        legacy_skill["origin"]["id"], "legacy_commands_dir",
        "legacy origin stays for compatibility while generic parsers use source"
    );
}

#[test]
fn bootstrap_and_system_prompt_emit_json_when_requested() {
    let root = unique_temp_dir("bootstrap-system-prompt-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let plan = assert_json_command(&root, &["--output-format", "json", "bootstrap-plan"]);
    assert_eq!(plan["kind"], "bootstrap-plan");
    assert_eq!(
        plan["status"], "ok",
        "bootstrap-plan JSON must have status:ok (#458)"
    );
    assert!(plan["phases"].as_array().expect("phases").len() > 1);

    let prompt = assert_json_command(&root, &["--output-format", "json", "system-prompt"]);
    assert_eq!(prompt["kind"], "system-prompt");
    assert_eq!(
        prompt["action"], "show",
        "system-prompt JSON must have action:show (#711)"
    );
    assert!(prompt["message"]
        .as_str()
        .expect("prompt text")
        .contains("interactive agent"));
}

#[test]
fn memory_files_load_claude_claw_agents_and_surface_json_438() {
    let root = unique_temp_dir("memory-files-438");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&root).expect("temp dir should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(root.join("CLAUDE.md"), "MARKER-FROM-CLAUDE-MD\n").expect("write CLAUDE.md");
    fs::write(root.join("CLAW.md"), "MARKER-FROM-CLAW-MD\n").expect("write CLAW.md");
    fs::write(root.join("AGENTS.md"), "MARKER-FROM-AGENTS-MD\n").expect("write AGENTS.md");
    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];

    let status = assert_json_command_with_env(&root, &["--output-format", "json", "status"], &envs);
    assert_eq!(status["workspace"]["memory_file_count"], 3);
    let memory_files = status["workspace"]["memory_files"]
        .as_array()
        .expect("status memory files");
    let sources = memory_files
        .iter()
        .map(|file| file["source"].as_str().expect("memory source"))
        .collect::<Vec<_>>();
    assert_eq!(sources, vec!["claude_md", "claw_md", "agents_md"]);
    assert!(memory_files
        .iter()
        .all(|file| file["path"].as_str().is_some()));
    assert!(memory_files
        .iter()
        .all(|file| file["chars"].as_u64().unwrap_or(0) > 0));
    assert!(memory_files
        .iter()
        .all(|file| file["contributes"].as_bool() == Some(true)));
    assert!(memory_files
        .iter()
        .all(|file| file["origin"].as_str() == Some("workspace")));
    assert!(memory_files
        .iter()
        .all(|file| file["scope_path"].as_str().is_some()));
    assert!(memory_files
        .iter()
        .all(|file| file["outside_project"].as_bool() == Some(false)));

    let prompt =
        assert_json_command_with_env(&root, &["--output-format", "json", "system-prompt"], &envs);
    let message = prompt["message"].as_str().expect("prompt message");
    assert!(message.contains("MARKER-FROM-CLAUDE-MD"));
    assert!(message.contains("MARKER-FROM-CLAW-MD"));
    assert!(message.contains("MARKER-FROM-AGENTS-MD"));
    assert_eq!(prompt["memory_file_count"], 3);
    assert_eq!(prompt["memory_files"][1]["source"], "claw_md");

    let doctor = assert_json_command_with_env(&root, &["--output-format", "json", "doctor"], &envs);
    let memory = doctor["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .find(|check| check["name"] == "memory")
        .expect("memory check");
    assert_eq!(memory["status"], "ok");
    assert_eq!(memory["memory_file_count"], 3);
    assert_eq!(memory["memory_files"][2]["source"], "agents_md");
    assert!(memory["unloaded_memory_files"]
        .as_array()
        .expect("unloaded memory files")
        .is_empty());
}

#[test]
fn memory_discovery_stops_at_git_root_and_reports_origins_439() {
    let root = unique_temp_dir("memory-boundary-439");
    let repo = root.join("repo");
    let nested = repo.join("subproj").join("deep").join("nest");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&nested).expect("nested dir should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .output()
        .expect("git init should launch");
    fs::write(root.join("CLAUDE.md"), "PARENT_CLAUDE").expect("write parent");
    fs::write(repo.join("CLAUDE.md"), "REPO_CLAUDE").expect("write repo");
    fs::write(repo.join("subproj").join("CLAUDE.md"), "CHILD_CLAUDE").expect("write child");
    fs::write(
        repo.join("subproj").join("deep").join("CLAUDE.md"),
        "DEEP_CLAUDE",
    )
    .expect("write deep");
    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];

    let status =
        assert_json_command_with_env(&nested, &["--output-format", "json", "status"], &envs);
    assert_eq!(status["workspace"]["memory_file_count"], 3);
    let memory_files = status["workspace"]["memory_files"]
        .as_array()
        .expect("memory files");
    let origins = memory_files
        .iter()
        .map(|file| file["origin"].as_str().expect("origin"))
        .collect::<Vec<_>>();
    assert_eq!(origins, vec!["ancestor", "ancestor", "parent_dir"]);
    let serialized = serde_json::to_string(memory_files).expect("memory files serialize");
    assert!(!serialized.contains("PARENT_CLAUDE"));
    assert!(!serialized.contains(root.join("CLAUDE.md").to_str().expect("parent path")));

    let prompt = assert_json_command_with_env(
        &nested,
        &["--output-format", "json", "system-prompt"],
        &envs,
    );
    let message = prompt["message"].as_str().expect("prompt message");
    assert!(!message.contains("PARENT_CLAUDE"));
    assert!(message.contains("REPO_CLAUDE"));
    assert!(message.contains("CHILD_CLAUDE"));
    assert!(message.contains("DEEP_CLAUDE"));
    assert_eq!(prompt["memory_files"][0]["origin"], "ancestor");
}

#[test]
fn dump_manifests_and_init_emit_json_when_requested() {
    let root = unique_temp_dir("manifest-init-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let manifests = assert_json_command(&root, &["--output-format", "json", "dump-manifests"]);
    assert_eq!(manifests["kind"], "dump-manifests");
    assert_eq!(manifests["status"], "ok");
    assert_eq!(manifests["source"], "rust-resolver");
    assert!(manifests["commands"].as_u64().expect("commands count") > 0);
    assert!(manifests["tools"].as_u64().expect("tools count") > 0);
    assert!(manifests["command_manifests"]
        .as_array()
        .expect("command manifests")
        .iter()
        .any(|entry| entry["name"] == "status"));
    assert!(manifests["tool_manifests"]
        .as_array()
        .expect("tool manifests")
        .iter()
        .any(|entry| entry["name"] == "read_file"));

    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let init = assert_json_command(&workspace, &["--output-format", "json", "init"]);
    assert_eq!(init["kind"], "init");
    assert_eq!(
        init["action"], "init",
        "init JSON must have action:init (#711)"
    );
    assert!(workspace.join("CLAUDE.md").exists());
}

#[test]
fn doctor_and_resume_status_emit_json_when_requested() {
    let root = unique_temp_dir("doctor-resume-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let doctor = assert_json_command(&root, &["--output-format", "json", "doctor"]);
    assert_eq!(doctor["kind"], "doctor");
    assert!(
        matches!(doctor["status"].as_str(), Some("ok" | "warn")),
        "doctor may warn on platforms without namespace sandbox/tmux support: {doctor}"
    );
    assert!(doctor["message"].is_string());
    let summary = doctor["summary"].as_object().expect("doctor summary");
    assert!(summary["ok"].as_u64().is_some());
    assert!(summary["warnings"].as_u64().is_some());
    assert!(summary["failures"].as_u64().is_some());
    assert_eq!(doctor["allowed_tools"]["aliases"]["WebFetch"], "web_fetch");
    assert!(doctor["allowed_tools"]["available"]
        .as_array()
        .is_some_and(|available| available.iter().any(|name| name == "web_fetch")));

    let checks = doctor["checks"].as_array().expect("doctor checks");
    assert_eq!(checks.len(), 12);
    let check_names = checks
        .iter()
        .map(|check| {
            assert!(check["status"].as_str().is_some());
            assert!(check["summary"].as_str().is_some());
            assert!(check["details"].is_array());
            // #704: each check must have a stable snake_case id
            assert!(
                check["id"].as_str().is_some(),
                "doctor check missing stable id field: {:?}",
                check["name"]
            );
            check["name"].as_str().expect("doctor check name")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        check_names,
        vec![
            "auth",
            "base urls",
            "config",
            "mcp validation",
            "hook validation",
            "install source",
            "workspace",
            "memory",
            "boot preflight",
            "sandbox",
            "permissions",
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
    let status = assert_json_command(&root, &["--output-format", "json", "status"]);
    assert_eq!(status["kind"], "status");
    assert!(matches!(
        status["binary_provenance"]["status"].as_str(),
        Some("known" | "unknown")
    ));
    assert!(status["binary_provenance"]["executable_path"].is_string());
    assert!(
        status["binary_provenance"]["workspace_match"].is_boolean()
            || status["binary_provenance"]["workspace_match"].is_null()
    );

    let boot_preflight = checks
        .iter()
        .find(|check| check["name"] == "boot preflight")
        .expect("boot preflight check");
    assert!(boot_preflight["boot_preflight"]["repo"]["exists"].is_boolean());
    assert!(boot_preflight["boot_preflight"]["mcp_startup"]["eligible"].is_boolean());
    assert!(boot_preflight["boot_preflight"]["required_binaries"].is_array());
    // #736: details[] must be {key,value} objects with non-null values;
    // regression guard for the double-space separator fix on boot_preflight prose strings.
    let bp_details = boot_preflight["details"]
        .as_array()
        .expect("boot_preflight details must be array");
    for entry in bp_details {
        assert!(
            entry["key"].is_string(),
            "boot_preflight detail entry missing string key: {entry:?}"
        );
        assert!(
            !entry["value"].is_null(),
            "boot_preflight detail entry has null value (prose-splitter failed): key={:?}",
            entry["key"]
        );
    }

    let sandbox = checks
        .iter()
        .find(|check| check["name"] == "sandbox")
        .expect("sandbox check");
    assert!(sandbox["filesystem_mode"].as_str().is_some());
    assert!(sandbox["enabled"].is_boolean());
    assert!(sandbox["fallback_reason"].is_null() || sandbox["fallback_reason"].is_string());

    let system = checks
        .iter()
        .find(|check| check["name"] == "system")
        .expect("system check");
    assert!(matches!(
        system["binary_provenance"]["status"].as_str(),
        Some("known" | "unknown")
    ));
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

    let agents = assert_json_command_with_env(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/agents",
        ],
        &[
            (
                "CLAW_CONFIG_HOME",
                config_home.to_str().expect("utf8 config home"),
            ),
            ("HOME", home.to_str().expect("utf8 home")),
        ],
    );
    assert_eq!(agents["kind"], "agents");
    assert_eq!(agents["action"], "list");
    assert!(
        agents["agents"].is_array(),
        "agents field must be a JSON array"
    );
    assert!(
        agents["count"].is_number(),
        "count must be a number, not a text render"
    );

    let plugins = assert_json_command_with_env(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 session path"),
            "/plugins",
        ],
        &[
            (
                "CLAW_CONFIG_HOME",
                config_home.to_str().expect("utf8 config home"),
            ),
            ("HOME", home.to_str().expect("utf8 home")),
        ],
    );
    assert_eq!(plugins["kind"], "plugin");
    assert_eq!(plugins["action"], "list");
    assert_eq!(plugins["status"], "ok");
    assert!(plugins["config_load_error"].is_null());
    // reload_runtime and target are operation-result fields; list response omits them (#703)
    assert!(
        !plugins
            .as_object()
            .map_or(false, |o| o.contains_key("reload_runtime")),
        "plugins list should not include reload_runtime"
    );
    assert!(
        !plugins
            .as_object()
            .map_or(false, |o| o.contains_key("target")),
        "plugins list should not include target"
    );
    assert!(
        plugins["summary"]["total"].is_number(),
        "plugins list should have summary.total"
    );
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
    assert!(
        version.get("message").is_none(),
        "resumed /version JSON should not include legacy prose message: {version}"
    );
    assert!(version["human_readable"].as_str().is_some());

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

#[test]
fn config_section_json_emits_section_and_value() {
    let root = unique_temp_dir("config-section-json");
    fs::create_dir_all(&root).expect("temp dir should exist");

    // Without a section: should return base envelope (no section field).
    let base = assert_json_command(&root, &["--output-format", "json", "config"]);
    assert_eq!(base["kind"], "config");
    assert!(base["loaded_files"].is_number());
    assert!(base["merged_keys"].is_number());
    assert!(
        base.get("section").is_none(),
        "no section field without section arg"
    );

    // With a known section: should add section + section_value fields.
    for section in &["model", "env", "hooks", "plugins"] {
        let result = assert_json_command(&root, &["--output-format", "json", "config", section]);
        assert_eq!(result["kind"], "config", "section={section}");
        assert_eq!(
            result["section"].as_str(),
            Some(*section),
            "section field must match requested section, got {result:?}"
        );
        assert!(
            result.get("section_value").is_some(),
            "section_value field must be present for section={section}"
        );
    }

    // With an unsupported section: should return ok:false + error field.
    let bad = assert_json_command(&root, &["--output-format", "json", "config", "unknown"]);
    assert_eq!(bad["kind"], "config");
    assert_eq!(bad["ok"], false);
    assert!(bad["error"].as_str().is_some());
    assert!(bad["section"].as_str().is_some());
}

#[test]
fn mcp_json_reports_required_optional_and_redacts_secret_values() {
    let root = unique_temp_dir("mcp-required-optional");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(root.join(".claw")).expect("workspace config should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(
        root.join(".claw").join("settings.json"),
        r#"{
          "mcpServers": {
            "required-stdio": {
              "command": "python3",
              "args": ["-c", "print('ready')"],
              "env": {"TOKEN": "secret-token-value"},
              "required": true
            },
            "optional-remote": {
              "type": "http",
              "url": "https://example.test/mcp",
              "headers": {
                "Authorization": "Bearer secret-header-value",
                "X-Trace": "visible-key-only"
              },
              "required": false
            }
          }
        }"#,
    )
    .expect("mcp config should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("config home"),
        ),
        ("HOME", home.to_str().expect("home")),
    ];
    let list = assert_json_command_with_env(&root, &["--output-format", "json", "mcp"], &envs);

    assert_eq!(list["kind"], "mcp");
    assert_eq!(list["action"], "list");
    assert_eq!(list["status"], "ok");
    assert_eq!(list["configured_servers"], 2);
    assert_eq!(list["total_configured"], 2);
    assert_eq!(list["valid_count"], 2);
    assert_eq!(list["invalid_count"], 0);
    assert!(list["invalid_servers"]
        .as_array()
        .is_some_and(Vec::is_empty));
    let servers = list["servers"].as_array().expect("servers array");
    let required = servers
        .iter()
        .find(|server| server["name"] == "required-stdio")
        .expect("required stdio server should be listed");
    let optional = servers
        .iter()
        .find(|server| server["name"] == "optional-remote")
        .expect("optional remote server should be listed");
    assert_eq!(required["required"], true);
    assert_eq!(required["valid"], true);
    assert_eq!(optional["valid"], true);
    assert_eq!(optional["required"], false);
    assert_eq!(required["details"]["env_keys"][0], "TOKEN");
    assert_eq!(optional["details"]["header_keys"][0], "Authorization");
    assert_eq!(optional["details"]["header_keys"][1], "X-Trace");

    let list_text = serde_json::to_string(&list).expect("mcp list json should serialize");
    assert!(!list_text.contains("secret-token-value"));
    assert!(!list_text.contains("secret-header-value"));
    assert!(!list_text.contains("visible-key-only"));

    let show = assert_json_command_with_env(
        &root,
        &["--output-format", "json", "mcp", "show", "optional-remote"],
        &envs,
    );
    assert_eq!(show["action"], "show");
    assert_eq!(show["status"], "ok");
    assert_eq!(show["server"]["required"], false);
    assert_eq!(show["server"]["valid"], true);
    assert_eq!(show["total_configured"], 2);
    assert_eq!(show["valid_count"], 2);
    assert_eq!(show["invalid_count"], 0);
    assert_eq!(show["server"]["details"]["header_keys"][0], "Authorization");
    let show_text = serde_json::to_string(&show).expect("mcp show json should serialize");
    assert!(!show_text.contains("secret-header-value"));
    assert!(!show_text.contains("visible-key-only"));
}

#[test]
fn mcp_degraded_config_and_failed_usage_are_distinct_json_contracts() {
    let root = unique_temp_dir("mcp-degraded-vs-failed");
    let workspace = root.join("workspace");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(
        workspace.join(".claw.json"),
        r#"{
          "mcpServers": {
            "valid-server": {
              "command": "/bin/echo",
              "args": ["hello"]
            },
            "missing-command": {
              "args": ["arg-only-no-command"],
              "required": true
            },
            "empty-command": {
              "command": ""
            },
            "wrong-type-command": {
              "command": 42
            }
          }
        }"#,
    )
    .expect("malformed mcp config should write");
    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("config home"),
        ),
        ("HOME", home.to_str().expect("home")),
    ];

    let degraded =
        assert_json_command_with_env(&workspace, &["--output-format", "json", "mcp"], &envs);
    assert_eq!(degraded["kind"], "mcp");
    assert_eq!(degraded["action"], "list");
    assert_eq!(degraded["status"], "degraded");
    assert!(degraded["config_load_error"].is_null());
    assert_eq!(degraded["configured_servers"], 1);
    assert_eq!(degraded["total_configured"], 4);
    assert_eq!(degraded["valid_count"], 1);
    assert_eq!(degraded["invalid_count"], 3);
    assert_eq!(degraded["servers"][0]["name"], "valid-server");
    assert_eq!(degraded["servers"][0]["valid"], true);
    assert_eq!(degraded["invalid_servers"][0]["name"], "empty-command");
    assert_eq!(degraded["invalid_servers"][0]["error_field"], "command");
    assert!(degraded["invalid_servers"][0]["reason"]
        .as_str()
        .is_some_and(|error| error.contains("non-empty string")));
    assert_eq!(degraded["invalid_servers"][1]["name"], "missing-command");
    assert_eq!(degraded["invalid_servers"][1]["error_field"], "command");
    assert!(degraded["invalid_servers"][1]["reason"]
        .as_str()
        .is_some_and(|error| error.contains("missing string field command")));
    assert_eq!(degraded["invalid_servers"][2]["name"], "wrong-type-command");
    assert_eq!(degraded["invalid_servers"][2]["error_field"], "command");

    let status =
        assert_json_command_with_env(&workspace, &["--output-format", "json", "status"], &envs);
    assert_eq!(status["status"], "degraded");
    assert!(status["config_load_error"].is_null());
    assert_eq!(status["mcp_validation"]["total_configured"], 4);
    assert_eq!(status["mcp_validation"]["valid_count"], 1);
    assert_eq!(status["mcp_validation"]["invalid_count"], 3);

    let doctor =
        assert_json_command_with_env(&workspace, &["--output-format", "json", "doctor"], &envs);
    let mcp_validation = doctor["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .find(|check| check["name"] == "mcp validation")
        .expect("mcp validation check");
    assert_eq!(mcp_validation["status"], "warn");
    assert_eq!(mcp_validation["invalid_count"], 3);
    assert_eq!(
        mcp_validation["invalid_servers"][0]["name"],
        "empty-command"
    );

    let failed_output = run_claw(
        &workspace,
        &["--output-format", "json", "mcp", "list", "extra"],
        &envs,
    );
    assert!(
        !failed_output.status.success(),
        "unsupported MCP action should exit non-zero"
    );
    let failed: Value =
        serde_json::from_slice(&failed_output.stdout).expect("failed stdout should be json");
    assert_eq!(failed["kind"], "mcp");
    assert_eq!(failed["action"], "error");
    assert_eq!(failed["ok"], false);
    assert_eq!(failed["error_kind"], "unsupported_action");
    assert!(failed.get("config_load_error").is_none());
}

#[test]
fn local_json_surfaces_have_non_empty_action_contract_714() {
    let root = unique_temp_dir("json-action-sweep-714");
    let workspace = root.join("workspace");
    let init_workspace = root.join("init-workspace");
    let git_workspace = root.join("git-workspace");
    let home = root.join("home");
    let config_home = root.join("config-home");
    let codex_home = root.join("codex-home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&init_workspace).expect("init workspace should exist");
    fs::create_dir_all(&git_workspace).expect("git workspace should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&codex_home).expect("codex home should exist");

    let session_path = write_session_fixture(&workspace, "action-sweep-export", Some("export me"));
    let export_output = root.join("export.md");
    let git_init = Command::new("git")
        .arg("init")
        .current_dir(&git_workspace)
        .output()
        .expect("git init should launch");
    assert!(
        git_init.status.success(),
        "git init stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&git_init.stdout),
        String::from_utf8_lossy(&git_init.stderr)
    );

    let envs = [
        ("HOME", home.to_str().expect("home utf8")),
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("config utf8"),
        ),
        ("CODEX_HOME", codex_home.to_str().expect("codex utf8")),
    ];

    let surfaces: Vec<(&Path, Vec<String>)> = vec![
        (&workspace, strings(&["--output-format", "json", "help"])),
        (&workspace, strings(&["--output-format", "json", "version"])),
        (&workspace, strings(&["--output-format", "json", "doctor"])),
        (&workspace, strings(&["--output-format", "json", "status"])),
        (&workspace, strings(&["--output-format", "json", "sandbox"])),
        (
            &workspace,
            strings(&["--output-format", "json", "bootstrap-plan"]),
        ),
        (
            &workspace,
            strings(&["--output-format", "json", "system-prompt"]),
        ),
        (
            &workspace,
            strings(&["--output-format", "json", "dump-manifests"]),
        ),
        (
            &workspace,
            vec![
                "--output-format".into(),
                "json".into(),
                "export".into(),
                "--session".into(),
                session_path.to_str().expect("session utf8").into(),
            ],
        ),
        (
            &workspace,
            vec![
                "--output-format".into(),
                "json".into(),
                "export".into(),
                "--session".into(),
                session_path.to_str().expect("session utf8").into(),
                "--output".into(),
                export_output.to_str().expect("export output utf8").into(),
            ],
        ),
        (
            &init_workspace,
            strings(&["--output-format", "json", "init"]),
        ),
        (&workspace, strings(&["--output-format", "json", "diff"])),
        (
            &git_workspace,
            strings(&["--output-format", "json", "diff"]),
        ),
        // #443: ACP exits 2 (not implemented); tested separately in acp_guidance_emits_json_when_requested
        (&workspace, strings(&["--output-format", "json", "config"])),
        (
            &workspace,
            strings(&["--output-format", "json", "config", "model"]),
        ),
        (
            &workspace,
            strings(&["--output-format", "json", "config", "unknown"]),
        ),
        (&workspace, strings(&["--output-format", "json", "skills"])),
        (&workspace, strings(&["--output-format", "json", "agents"])),
        (&workspace, strings(&["--output-format", "json", "plugins"])),
        (&workspace, strings(&["--output-format", "json", "mcp"])),
    ];

    for (current_dir, args) in surfaces {
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let parsed = assert_json_command_with_env(current_dir, &arg_refs, &envs);
        assert_non_empty_action(&parsed, &arg_refs);
    }
}

#[test]
fn inventory_commands_deduplicate_config_deprecation_warnings_per_process() {
    let root = unique_temp_dir("config-warning-dedup");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(
        config_home.join("settings.json"),
        r#"{"enabledPlugins": {}}"#,
    )
    .expect("deprecated config fixture should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];

    for args in [&["plugins", "list"][..], &["mcp", "list"][..]] {
        let output = run_claw(&root, args, &envs);
        assert!(
            output.status.success(),
            "args={args:?}\nstdout:\n{}\n\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
        let warning_count = stderr
            .matches("field \"enabledPlugins\" is deprecated")
            .count();
        assert_eq!(
            warning_count, 1,
            "args={args:?} should emit the deprecated enabledPlugins warning once per process:\n{stderr}"
        );
    }
}

#[test]
fn config_json_reports_deprecations_structurally_without_stderr_duplicate_815() {
    let root = unique_temp_dir("config-json-warning-815");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(
        config_home.join("settings.json"),
        r#"{"enabledPlugins": {}}"#,
    )
    .expect("deprecated config fixture should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];
    let output = run_claw(&root, &["--output-format", "json", "config"], &envs);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed: Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid json");
    let warnings = parsed["warnings"]
        .as_array()
        .expect("config JSON should include warnings[]");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("field \"enabledPlugins\" is deprecated"))),
        "config JSON warnings[] should include enabledPlugins deprecation: {parsed}"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    assert!(
        !stderr.contains("field \"enabledPlugins\" is deprecated"),
        "JSON config should not duplicate collected config deprecations on stderr:\n{stderr}"
    );

    let text_output = run_claw(&root, &["config"], &envs);
    assert!(
        text_output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&text_output.stdout),
        String::from_utf8_lossy(&text_output.stderr)
    );
    let text_stderr = String::from_utf8(text_output.stderr).expect("stderr utf8");
    assert!(
        text_stderr.contains("field \"enabledPlugins\" is deprecated"),
        "text config should keep human-readable config warnings on stderr"
    );
}

#[test]
fn status_deduplicates_config_deprecation_warnings_per_invocation_425() {
    let root = unique_temp_dir("status-warning-dedup-425");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(
        config_home.join("settings.json"),
        r#"{"enabledPlugins": {}}"#,
    )
    .expect("deprecated config fixture should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];
    let output = run_claw(&root, &["status"], &envs);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    let warning_count = stderr
        .matches("field \"enabledPlugins\" is deprecated")
        .count();
    assert_eq!(
        warning_count, 1,
        "status should emit the deprecated enabledPlugins warning once per process:\n{stderr}"
    );
}

#[test]
fn config_json_attributes_precedence_and_shadowed_keys_425() {
    let root = unique_temp_dir("config-precedence-425");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(root.join(".claw")).expect("workspace config should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(
        root.join(".claw.json"),
        r#"{"model":"anthropic/claude-sonnet-4-6","env":{"A":"legacy","B":"legacy"}}"#,
    )
    .expect("legacy project config fixture should write");
    fs::write(
        root.join(".claw").join("settings.json"),
        r#"{"model":"anthropic/claude-opus-4-6","env":{"A":"settings","C":"settings"}}"#,
    )
    .expect("project settings fixture should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];
    let parsed = assert_json_command_with_env(&root, &["--output-format", "json", "config"], &envs);
    let files = parsed["files"].as_array().expect("files array");
    let legacy = files
        .iter()
        .find(|file| {
            file["source"] == "project"
                && file["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(".claw.json"))
        })
        .expect("project .claw.json entry");
    let settings = files
        .iter()
        .find(|file| {
            file["source"] == "project"
                && file["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(".claw/settings.json"))
        })
        .expect("project .claw/settings.json entry");

    assert_eq!(legacy["status"], "loaded");
    assert_eq!(settings["status"], "loaded");
    assert!(
        settings["precedence_rank"].as_u64().expect("settings rank")
            > legacy["precedence_rank"].as_u64().expect("legacy rank"),
        "later project settings must outrank legacy project config: legacy={legacy} settings={settings}"
    );
    for key in ["model", "env.A"] {
        assert!(
            legacy["shadowed_keys"]
                .as_array()
                .expect("legacy shadowed keys")
                .iter()
                .any(|value| value.as_str() == Some(key)),
            "legacy config should report {key} as shadowed: {legacy}"
        );
        assert!(
            settings["wins_for_keys"]
                .as_array()
                .expect("settings winning keys")
                .iter()
                .any(|value| value.as_str() == Some(key)),
            "project settings should report {key} as winning: {settings}"
        );
    }
    assert!(
        legacy["wins_for_keys"]
            .as_array()
            .expect("legacy winning keys")
            .iter()
            .any(|value| value.as_str() == Some("env.B")),
        "unshadowed legacy keys should remain attributed to .claw.json: {legacy}"
    );
}

#[test]
fn config_section_json_tolerates_unknown_keys_as_warnings_425() {
    let root = unique_temp_dir("config-unknown-warning-425");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(root.join(".claw.json"), r#"{"model":"opus","alpha":"x"}"#)
        .expect("legacy config fixture should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];
    let parsed = assert_json_command_with_env(
        &root,
        &["--output-format", "json", "config", "model"],
        &envs,
    );

    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["section"], "model");
    assert_eq!(parsed["section_value"], "opus");
    assert!(
        parsed["warnings"]
            .as_array()
            .expect("warnings array")
            .iter()
            .any(|warning| warning
                .as_str()
                .is_some_and(|text| text.contains("unknown key \"alpha\""))),
        "unknown keys should be structural warnings, not section failures: {parsed}"
    );
}

#[test]
fn config_json_reports_structured_unloaded_file_reasons_407() {
    let root = unique_temp_dir("config-file-status-407");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(root.join(".claw")).expect("workspace config should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(root.join(".claw.json"), "{not json").expect("legacy skip fixture should write");
    fs::write(
        root.join(".claw").join("settings.json"),
        r#"{"model":"opus"}"#,
    )
    .expect("project config fixture should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];
    let output = run_claw(&root, &["--output-format", "json", "config"], &envs);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("stdout valid json");

    assert_eq!(parsed["kind"], "config");
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["loaded_files"], 1);
    assert_eq!(parsed["merged_keys"], parsed["merged_key_count"]);
    assert_eq!(
        parsed["merged_keys_meaning"].as_str(),
        Some("count of top-level keys in the effective merged JSON object")
    );
    assert!(parsed["load_error"].is_null());

    let files = parsed["files"].as_array().expect("files array");
    let loaded = files
        .iter()
        .find(|file| file["loaded"] == true)
        .expect("loaded config file");
    assert_eq!(loaded["status"], "loaded");
    assert!(loaded.get("reason").is_none());
    let missing = files
        .iter()
        .find(|file| file["status"] == "not_found")
        .expect("missing config file");
    assert_eq!(missing["loaded"], false);
    assert_eq!(missing["reason"], "not_found");
    assert_eq!(missing["skip_reason"], "not_found");
    let skipped = files
        .iter()
        .find(|file| file["status"] == "skipped")
        .expect("skipped legacy config file");
    assert_eq!(skipped["loaded"], false);
    assert_eq!(skipped["reason"], "legacy_invalid_json");
    assert_eq!(skipped["skip_reason"], "legacy_invalid_json");
    assert!(skipped["detail"].as_str().is_some());
}

#[test]
fn config_json_list_reports_parse_errors_without_dropping_file_statuses_407() {
    let root = unique_temp_dir("config-file-load-error-407");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(root.join(".claw")).expect("workspace config should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(config_home.join("settings.json"), r#"{"model":"sonnet"}"#)
        .expect("user config fixture should write");
    fs::write(root.join(".claw").join("settings.json"), "{not json")
        .expect("invalid project config fixture should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];
    let output = run_claw(&root, &["--output-format", "json", "config"], &envs);
    assert!(
        output.status.success(),
        "config list should be best-effort even with one parse-broken file; stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("stdout valid json");

    assert_eq!(parsed["status"], "error");
    assert!(parsed["load_error"].as_str().is_some());
    assert_eq!(parsed["loaded_files"], 1);
    let files = parsed["files"].as_array().expect("files array");
    let error_file = files
        .iter()
        .find(|file| file["status"] == "load_error")
        .expect("load error config file");
    assert_eq!(error_file["loaded"], false);
    assert_eq!(error_file["reason"], "parse_error");
    assert_eq!(error_file["skip_reason"], "parse_error");
    assert!(error_file["detail"].as_str().is_some());
}

#[test]
fn global_json_surfaces_suppress_config_deprecation_stderr_810_821_824() {
    let root = unique_temp_dir("global-json-warning-810-821-824");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(
        config_home.join("settings.json"),
        r#"{"enabledPlugins": {}}"#,
    )
    .expect("deprecated config fixture should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];

    let session_path = write_session_fixture(&root, "resume-config-warning-824", Some("config"));
    let resume_config = format!("--resume={}", session_path.to_str().expect("utf8 session"));

    for (args, expected_kind, expected_action) in [
        (
            vec!["--output-format", "json", "plugins", "list"],
            "plugin",
            "list",
        ),
        (
            vec!["--output-format", "json", "mcp", "list"],
            "mcp",
            "list",
        ),
        (
            vec!["--output-format", "json", "doctor"],
            "doctor",
            "doctor",
        ),
        (vec!["--output-format", "json", "status"], "status", "show"),
        (
            vec!["--output-format", "json", "sandbox"],
            "sandbox",
            "status",
        ),
        (
            vec!["--output-format", "json", "system-prompt"],
            "system-prompt",
            "show",
        ),
        (
            vec!["--output-format", "json", "skills", "list"],
            "skills",
            "list",
        ),
        (
            vec!["--output-format", "json", "agents", "list"],
            "agents",
            "list",
        ),
        (
            vec!["--output-format", "json", resume_config.as_str(), "/config"],
            "config",
            "list",
        ),
    ] {
        let output = run_claw(&root, &args, &envs);
        assert!(
            output.status.success(),
            "args={args:?}\nstdout:\n{}\n\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout.first(),
            Some(&b'{'),
            "args={args:?} stdout JSON must start at byte 0, got: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let parsed: Value =
            serde_json::from_slice(&output.stdout).expect("stdout should be valid JSON");
        assert_eq!(parsed["kind"], expected_kind, "args={args:?}");
        assert_eq!(parsed["action"], expected_action, "args={args:?}");
        assert!(
            matches!(parsed["status"].as_str(), Some("ok" | "warn")),
            "args={args:?} should report successful local status: {parsed}"
        );
        assert!(
            output.stderr.is_empty(),
            "successful JSON surface must keep stderr empty for args={args:?}, got:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn local_text_surface_preserves_config_deprecation_stderr_816() {
    let root = unique_temp_dir("global-text-warning-816");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(
        config_home.join("settings.json"),
        r#"{"enabledPlugins": {}}"#,
    )
    .expect("deprecated config fixture should write");

    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
    ];

    let output = run_claw(&root, &["doctor"], &envs);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    assert!(
        stderr.contains("field \"enabledPlugins\" is deprecated"),
        "text-mode doctor should preserve human config deprecation warnings on stderr"
    );
}

fn assert_json_command(current_dir: &Path, args: &[&str]) -> Value {
    assert_json_command_with_env(current_dir, args, &[])
}

fn assert_json_command_with_env(current_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Value {
    let output = run_claw(current_dir, args, envs);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be valid json");
    assert_non_empty_action(&parsed, args);
    parsed
}

fn assert_non_empty_action(parsed: &Value, args: &[&str]) {
    let action = parsed
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        !action.trim().is_empty(),
        "JSON output for args={args:?} must include a non-empty stable action field: {parsed}"
    );
}

fn run_claw(current_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_claw"));
    command.current_dir(current_dir).args(args);
    for key in ["CLAW_OUTPUT_FORMAT", "CLAW_LOG", "RUST_LOG"] {
        if !envs.iter().any(|(env_key, _)| *env_key == key) {
            command.env_remove(key);
        }
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("claw should launch")
}

fn parse_json_stdout(output: &Output, context: &str) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{context} should emit valid stdout JSON; stdout:\n{}\n\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
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

fn write_skill(root: &Path, name: &str, description: &str) {
    let skill_root = root.join(name);
    fs::create_dir_all(&skill_root).expect("skill root should exist");
    fs::write(
        skill_root.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
    )
    .expect("skill fixture should write");
}

fn write_legacy_command(root: &Path, name: &str, description: &str) {
    fs::create_dir_all(root).expect("legacy command root should exist");
    fs::write(
        root.join(format!("{name}.md")),
        format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
    )
    .expect("legacy command fixture should write");
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

#[test]
fn diff_json_has_status_and_result_field_702() {
    // #458/#702: `claw diff --output-format json` must have status ∈ {ok,error}
    // and a `result` field to distinguish clean/changes/no-repo states.
    let root = unique_temp_dir("diff-json-status");
    fs::create_dir_all(&root).expect("temp dir should exist");

    // In a non-git directory, diff should report status:ok + result:no_git_repo
    // or status:error; in a git repo it should report ok + result:clean|changes.
    // We only assert the shape, not the value, to avoid flakiness.
    let parsed = assert_json_command(&root, &["--output-format", "json", "diff"]);
    assert_eq!(
        parsed["kind"], "diff",
        "diff JSON must have kind:diff (#458)"
    );
    let status = parsed["status"]
        .as_str()
        .expect("diff JSON must have status field (#458/#702)");
    assert!(
        matches!(status, "ok" | "error"),
        "diff status must be ok or error, got {status:?}"
    );
    assert!(
        parsed.get("result").is_some(),
        "diff JSON must have result field"
    );
    // #710: diff JSON must have action:diff and working_directory
    assert_eq!(
        parsed["action"], "diff",
        "diff JSON must have action:diff (#710)"
    );
    assert!(
        parsed
            .get("working_directory")
            .and_then(|v| v.as_str())
            .is_some(),
        "diff JSON must have working_directory field (#710)"
    );
    // #740: diff JSON changed_file_count contract: numeric in git repos, absent for no_git_repo
    let result_str = parsed.get("result").and_then(|v| v.as_str());
    if result_str == Some("no_git_repo") {
        // Non-git repos don't emit changed_file_count
        assert!(
            parsed.get("changed_file_count").is_none(),
            "diff JSON should not have changed_file_count for no_git_repo (#733)"
        );
    } else {
        // Git repos must emit numeric changed_file_count
        assert!(
            parsed
                .get("changed_file_count")
                .and_then(|v| v.as_u64())
                .is_some(),
            "diff JSON changed_file_count must be numeric in git repos (#733)"
        );
    }
}

#[test]
fn diff_json_changed_file_count_deduplication_733() {
    // #733/#742: changed_file_count must be numeric in a git repo, be 0 for clean,
    // and deduplicate staged+unstaged edits to the same file (1 file changed = count 1).
    use std::process::Command;
    let root = unique_temp_dir("diff-changed-dedup");
    fs::create_dir_all(&root).expect("temp dir");

    // git init + identity config + initial commit
    Command::new("git")
        .args(["init"])
        .current_dir(&root)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@claw.test"])
        .current_dir(&root)
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&root)
        .output()
        .expect("git config name");
    fs::write(root.join("tracked.txt"), b"v1").expect("write tracked");
    Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(&root)
        .output()
        .expect("git add");
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&root)
        .output()
        .expect("git commit");

    // Clean state: changed_file_count must be 0
    let bin = env!("CARGO_BIN_EXE_claw");
    let clean = Command::new(bin)
        .current_dir(&root)
        .args(["--output-format", "json", "diff"])
        .output()
        .expect("claw diff clean");
    let clean_json: serde_json::Value =
        serde_json::from_slice(&clean.stdout).expect("diff clean stdout must be valid JSON");
    assert_eq!(clean_json["result"], "clean", "fresh repo must be clean");
    assert_eq!(
        clean_json["changed_file_count"].as_u64(),
        Some(0),
        "clean repo must have changed_file_count:0 (#733)"
    );

    // Make a staged edit AND an unstaged edit to the same file
    fs::write(root.join("tracked.txt"), b"v2").expect("staged write");
    Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(&root)
        .output()
        .expect("git add staged");
    fs::write(root.join("tracked.txt"), b"v3").expect("unstaged write");

    // Dirty state: same file appears in staged+unstaged — must deduplicate to count 1
    let dirty = Command::new(bin)
        .current_dir(&root)
        .args(["--output-format", "json", "diff"])
        .output()
        .expect("claw diff dirty");
    let dirty_json: serde_json::Value =
        serde_json::from_slice(&dirty.stdout).expect("diff dirty stdout must be valid JSON");
    assert_eq!(
        dirty_json["result"], "changes",
        "dirty repo must have result:changes (#733)"
    );
    assert_eq!(
        dirty_json["changed_file_count"].as_u64(),
        Some(1),
        "staged+unstaged edits to same file must deduplicate to changed_file_count:1 (#733)"
    );
}

#[test]
fn prompt_no_arg_json_error_kind_750() {
    // #751/#750/#823: `claw prompt --output-format json` with no prompt argument must emit
    // error_kind:"missing_prompt" with stdout JSON, empty stderr, and a non-empty hint.
    // Before #823 the structured envelope could be routed to stderr, leaving stdout empty.
    use std::process::Command;
    let root = unique_temp_dir("prompt-no-arg");
    fs::create_dir_all(&root).expect("temp dir");
    let bin = env!("CARGO_BIN_EXE_claw");

    let output = Command::new(bin)
        .current_dir(&root)
        .args(["--output-format", "json", "prompt"])
        .output()
        .expect("claw prompt should run");
    assert!(
        !output.status.success(),
        "claw prompt with no arg must exit non-zero"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "claw prompt with no arg must exit rc=1 (#823)"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr, "",
        "claw prompt (no arg) --output-format json must keep stderr empty (#823); got: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        panic!(
            "claw prompt (no arg) --output-format json must emit valid stdout JSON; got: {stdout}"
        )
    });
    assert_eq!(
        parsed["error_kind"], "missing_prompt",
        "claw prompt no-arg must have error_kind:missing_prompt (#750/#823); got: {parsed}"
    );
    let hint = parsed["hint"].as_str().unwrap_or("");
    assert!(
        !hint.is_empty(),
        "claw prompt no-arg hint must be non-empty (#750/#823)"
    );
    assert!(
        hint.contains("claw prompt") || hint.contains("echo"),
        "hint should mention 'claw prompt' or 'echo': {hint}"
    );
}

#[test]
fn prompt_empty_arg_json_stdout_missing_prompt_823() {
    // #823: `claw --output-format json prompt ""` must match the missing prompt
    // channel contract: rc=1, stdout JSON, error_kind:"missing_prompt", empty stderr.
    use std::process::Command;
    let root = unique_temp_dir("prompt-empty-arg-823");
    fs::create_dir_all(&root).expect("temp dir");
    let bin = env!("CARGO_BIN_EXE_claw");

    let output = Command::new(bin)
        .current_dir(&root)
        .args(["--output-format", "json", "prompt", ""])
        .output()
        .expect("claw prompt empty arg should run");
    assert_eq!(
        output.status.code(),
        Some(1),
        "claw prompt empty arg must exit rc=1 (#823)"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr, "",
        "claw prompt empty arg --output-format json must keep stderr empty (#823); got: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        panic!(
            "claw prompt empty arg --output-format json must emit valid stdout JSON; got: {stdout}"
        )
    });
    assert_eq!(
        parsed["error_kind"], "missing_prompt",
        "claw prompt empty arg must have error_kind:missing_prompt (#823); got: {parsed}"
    );
    assert_eq!(
        parsed["action"], "abort",
        "claw prompt empty arg must retain abort action (#823); got: {parsed}"
    );
    assert!(
        parsed["hint"].as_str().map_or(false, |h| !h.is_empty()),
        "claw prompt empty arg missing_prompt hint must be non-empty (#823)"
    );
}

#[test]
fn flag_value_errors_have_error_kind_and_hint_756() {
    // #756: missing/invalid flag-value errors must emit typed error_kind + non-null hint.
    // Before #756: all returned error_kind:"unknown" + hint:null.
    use std::process::Command;
    let root = unique_temp_dir("flag-value-errors");
    fs::create_dir_all(&root).expect("temp dir");
    let bin = env!("CARGO_BIN_EXE_claw");

    // Case 1: --reasoning-effort with invalid value
    let out = Command::new(bin)
        .current_dir(&root)
        .args(["--output-format", "json", "--reasoning-effort", "HIGH"])
        .output()
        .expect("claw --reasoning-effort HIGH should run");
    assert!(
        !out.status.success(),
        "invalid reasoning-effort must exit non-zero"
    );
    // #819/#820/#823: abort envelopes route to stdout in JSON mode
    let raw = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with('{'))
        .collect::<Vec<_>>()
        .join("");
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| {
        panic!("invalid --reasoning-effort must emit JSON to stdout; got: {raw}")
    });
    assert_eq!(
        parsed["error_kind"], "invalid_flag_value",
        "invalid --reasoning-effort must be invalid_flag_value (#756): {parsed}"
    );
    assert!(
        parsed["hint"].as_str().map_or(false, |h| h.contains("low")
            || h.contains("medium")
            || h.contains("high")),
        "hint must mention valid values (#756): {parsed}"
    );

    // Case 2: --model flag with missing value (trailing flag)
    let out2 = Command::new(bin)
        .current_dir(&root)
        .args(["--output-format", "json", "--model"])
        .output()
        .expect("claw --model (no value) should run");
    assert!(
        !out2.status.success(),
        "missing --model value must exit non-zero"
    );
    let raw2 = String::from_utf8_lossy(&out2.stdout)
        .lines()
        .filter(|l| l.starts_with('{'))
        .collect::<Vec<_>>()
        .join("");
    let parsed2: serde_json::Value = serde_json::from_str(&raw2)
        .unwrap_or_else(|_| panic!("missing --model value must emit JSON to stdout; got: {raw2}"));
    assert_eq!(
        parsed2["error_kind"], "missing_flag_value",
        "missing --model value must be missing_flag_value (#756): {parsed2}"
    );
    assert!(
        parsed2["hint"].as_str().map_or(false, |h| !h.is_empty()),
        "missing --model hint must be non-empty (#756): {parsed2}"
    );
}
#[test]
fn output_format_flags_and_env_have_typed_contract_433() {
    let root = unique_temp_dir("output-format-433");
    fs::create_dir_all(&root).expect("temp dir");

    let repeated = run_claw(
        &root,
        &[
            "--output-format",
            "text",
            "--output-format",
            "JSON",
            "status",
        ],
        &[],
    );
    assert!(repeated.status.success());
    let repeated_stderr = String::from_utf8_lossy(&repeated.stderr);
    assert!(
        repeated_stderr.contains("warning: --output-format specified multiple times"),
        "repeated output-format should warn on stderr: {repeated_stderr}"
    );
    let repeated_json = parse_json_stdout(&repeated, "repeated output-format status");
    assert_eq!(repeated_json["kind"], "status");
    assert_eq!(repeated_json["format_source"], "flag");
    assert_eq!(repeated_json["format_raw"], "JSON");
    assert_eq!(repeated_json["format_overridden"][0], "text");

    let repeated_text = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "--output-format",
            "text",
            "status",
        ],
        &[],
    );
    assert!(repeated_text.status.success());
    let repeated_text_stderr = String::from_utf8_lossy(&repeated_text.stderr);
    assert!(
        repeated_text_stderr.contains("using last value 'text'"),
        "json-to-text repeated output-format should warn: {repeated_text_stderr}"
    );
    let repeated_text_stdout = String::from_utf8_lossy(&repeated_text.stdout);
    assert!(
        repeated_text_stdout.contains("Status"),
        "last text output-format should produce text status: {repeated_text_stdout}"
    );

    for value in ["json", "JSON", "Json"] {
        let parsed = assert_json_command(&root, &["--output-format", value, "status"]);
        assert_eq!(
            parsed["kind"], "status",
            "case {value} should parse as JSON"
        );
        assert_eq!(parsed["format_source"], "flag");
        assert_eq!(parsed["format_raw"], value);
    }

    let from_env =
        assert_json_command_with_env(&root, &["status"], &[("CLAW_OUTPUT_FORMAT", "json")]);
    assert_eq!(from_env["kind"], "status");
    assert_eq!(from_env["format_source"], "env");
    assert_eq!(from_env["format_raw"], "json");

    let flag_overrides_env = run_claw(
        &root,
        &["--output-format", "json", "status"],
        &[("CLAW_OUTPUT_FORMAT", "text")],
    );
    assert!(flag_overrides_env.status.success());
    let override_json = parse_json_stdout(&flag_overrides_env, "flag overrides env output-format");
    assert_eq!(override_json["kind"], "status");
    assert_eq!(override_json["format_source"], "flag");
    assert_eq!(override_json["format_raw"], "json");
    assert_eq!(
        override_json["format_overridden"].as_array().map(Vec::len),
        Some(0)
    );

    let invalid = run_claw(&root, &["--output-format", "YAML", "status"], &[]);
    assert_eq!(invalid.status.code(), Some(1));
    assert!(
        invalid.stderr.is_empty(),
        "invalid output-format in JSON mode must keep stderr empty: {}",
        String::from_utf8_lossy(&invalid.stderr)
    );
    let invalid_json = parse_json_stdout(&invalid, "invalid output-format JSON error");
    assert_eq!(invalid_json["error_kind"], "invalid_output_format");
    assert_eq!(invalid_json["value"], "YAML");
    assert_eq!(
        invalid_json["expected"],
        serde_json::json!(["text", "json"])
    );
    assert!(invalid_json["hint"]
        .as_str()
        .is_some_and(|hint| hint.contains("--output-format json")));

    let help = assert_json_command(&root, &["--output-format", "json", "help"]);
    let help_text = help["message"].as_str().expect("help message");
    assert!(
        help_text.contains("CLAW_OUTPUT_FORMAT"),
        "help should document CLAW_OUTPUT_FORMAT: {help_text}"
    );
    assert!(
        help_text.contains("CLAW_LOG"),
        "help should document CLAW_LOG: {help_text}"
    );
    assert!(
        help_text.contains("RUST_LOG"),
        "help should document RUST_LOG: {help_text}"
    );

    let doctor = assert_json_command_with_env(
        &root,
        &["doctor"],
        &[
            ("CLAW_OUTPUT_FORMAT", "json"),
            ("CLAW_LOG", "debug"),
            ("RUST_LOG", "claw=debug"),
        ],
    );
    let system_check = doctor["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .find(|check| check["name"] == "system")
        .expect("system check");
    assert_eq!(system_check["claw_output_format"], "json");
    assert_eq!(system_check["claw_log"], "debug");
    assert_eq!(system_check["rust_log"], "claw=debug");
}

#[test]
fn allowed_tools_errors_have_typed_json_and_alias_map_432() {
    let root = unique_temp_dir("allowed-tools-432");
    fs::create_dir_all(&root).expect("temp dir");

    let missing = run_claw(
        &root,
        &["--allowedTools", "status", "--output-format", "json"],
        &[],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        missing.stderr.is_empty(),
        "JSON missing allowedTools value must keep stderr empty: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
    let missing_json = parse_json_stdout(&missing, "allowedTools subcommand missing value");
    assert_eq!(missing_json["error_kind"], "missing_argument");
    assert_eq!(missing_json["argument"], "--allowedTools");
    assert!(missing_json["hint"]
        .as_str()
        .is_some_and(|hint| { hint.contains("--allowedTools") && hint.contains("read,glob") }));

    let invalid = run_claw(
        &root,
        &["--output-format", "json", "--allowedTools", "teleport"],
        &[],
    );
    assert_eq!(invalid.status.code(), Some(1));
    assert!(
        invalid.stderr.is_empty(),
        "JSON invalid allowedTools value must keep stderr empty: {}",
        String::from_utf8_lossy(&invalid.stderr)
    );
    let invalid_json = parse_json_stdout(&invalid, "allowedTools invalid tool");
    assert_eq!(invalid_json["error_kind"], "invalid_tool_name");
    assert_eq!(invalid_json["tool_name"], "teleport");
    assert!(invalid_json["available"]
        .as_array()
        .is_some_and(|available| available.iter().any(|name| name == "web_fetch")));
    assert_eq!(invalid_json["tool_aliases"]["WebFetch"], "web_fetch");
    assert!(invalid_json["hint"]
        .as_str()
        .is_some_and(|hint| { hint.contains("canonical snake_case") && hint.contains("aliases") }));
}

#[test]
fn short_p_flag_swallows_no_flags_755() {
    // #755: `claw -p hello --output-format json` must parse --output-format json
    // as a flag rather than swallowing it as part of the prompt. Before #755,
    // args[index+1..].join(" ") consumed all remaining tokens into the prompt.
    // After #755, -p consumes exactly one token and remaining flags are parsed.
    // We verify by checking that the envelope IS JSON (meaning --output-format json
    // was interpreted as a flag, not literal prompt text).
    use std::process::Command;
    let root = unique_temp_dir("short-p-flags");
    fs::create_dir_all(&root).expect("temp dir");
    let bin = env!("CARGO_BIN_EXE_claw");

    // -p hello --output-format json: with no credentials, should fail with
    // missing_credentials (not missing_prompt), proving --output-format json was parsed.
    let output = Command::new(bin)
        .current_dir(&root)
        .args(["-p", "hello", "--output-format", "json"])
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .output()
        .expect("claw -p should run");
    assert!(
        !output.status.success(),
        "claw -p hello --output-format json must exit non-zero (no credentials)"
    );
    // #819/#820/#823: abort envelopes route to stdout in JSON mode
    let raw = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| l.starts_with('{'))
        .collect::<Vec<_>>()
        .join("");
    // Must be valid JSON (i.e. --output-format json was parsed, not swallowed)
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| {
        panic!("--output-format json must be parsed as a flag, not prompt text; stdout: {raw}")
    });
    assert_eq!(
        parsed["error_kind"], "missing_credentials",
        "flags after -p prompt text must be parsed normally (#755); got: {parsed}"
    );

    // Also verify -p --model bogus is rejected as missing_prompt (flag-as-prompt guard)
    let output2 = Command::new(bin)
        .current_dir(&root)
        .args(["--output-format", "json", "-p", "--model", "sonnet"])
        .output()
        .expect("claw -p flag-as-prompt should run");
    let raw2 = String::from_utf8_lossy(&output2.stdout)
        .lines()
        .filter(|l| l.starts_with('{'))
        .collect::<Vec<_>>()
        .join("");
    let parsed2: serde_json::Value = serde_json::from_str(&raw2)
        .unwrap_or_else(|_| panic!("claw -p --model must emit JSON to stdout; got: {raw2}"));
    assert_eq!(
        parsed2["error_kind"], "missing_prompt",
        "flag-like token after -p must be rejected as missing_prompt (#755): {parsed2}"
    );
    assert!(
        parsed2["hint"].as_str().map_or(false, |h| !h.is_empty()),
        "missing_prompt hint must be non-empty (#755)"
    );
}

#[test]
fn short_p_flag_no_arg_json_error_kind_753() {
    // #753: `claw --output-format json -p` (no prompt) must emit error_kind:"missing_prompt"
    // and non-empty hint. Before #753 it returned error_kind:"unknown" + hint:null.
    // Parity with #750 which fixed the explicit `prompt` verb.
    use std::process::Command;
    let root = unique_temp_dir("short-p-no-arg");
    fs::create_dir_all(&root).expect("temp dir");
    let bin = env!("CARGO_BIN_EXE_claw");

    let output = Command::new(bin)
        .current_dir(&root)
        .args(["--output-format", "json", "-p"])
        .output()
        .expect("claw -p should run");
    assert!(
        !output.status.success(),
        "claw -p with no arg must exit non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw = if stdout.trim().starts_with('{') {
        stdout.trim().to_string()
    } else {
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|l| l.starts_with('{'))
            .collect::<Vec<_>>()
            .join("")
    };
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| {
        panic!("claw -p (no arg) --output-format json must emit valid JSON; got: {raw}")
    });
    assert_eq!(
        parsed["error_kind"], "missing_prompt",
        "claw -p no-arg must have error_kind:missing_prompt (#753); got: {parsed}"
    );
    let hint = parsed["hint"].as_str().unwrap_or("");
    assert!(
        !hint.is_empty(),
        "claw -p no-arg hint must be non-empty (#753)"
    );
    assert!(
        hint.contains("claw -p") || hint.contains("claw prompt"),
        "hint should mention 'claw -p' or 'claw prompt': {hint}"
    );
}

#[test]
fn bare_slash_command_hint_745() {
    // #747/#745: claw <slash-cmd> --output-format json must return non-null hint.
    // bare_slash_command_guidance() previously had no \n so split_error_hint returned hint:null.
    use std::process::Command;
    let root = unique_temp_dir("bare-slash-hint");
    fs::create_dir_all(&root).expect("temp dir");
    let bin = env!("CARGO_BIN_EXE_claw");

    // issue and pr are non-resume-supported; commit is resume-supported.
    // All must emit non-null hint in their interactive_only error envelope.
    for cmd in &["issue", "pr", "commit"] {
        let output = Command::new(bin)
            .current_dir(&root)
            .args(["--output-format", "json", cmd])
            .env("ANTHROPIC_API_KEY", "test")
            .output()
            .expect("claw should run");
        assert!(
            !output.status.success(),
            "claw {cmd} outside REPL must exit non-zero"
        );
        // Error envelope is on stderr (type:error path) or stdout
        let stderr = String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|l| l.starts_with('{'))
            .collect::<Vec<_>>()
            .join("");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let raw = if !stderr.is_empty() {
            stderr
        } else {
            stdout.trim().to_string()
        };
        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|_| panic!("claw {cmd} must emit JSON; got: {raw}"));
        assert_eq!(
            parsed["error_kind"], "interactive_only",
            "claw {cmd} must have error_kind:interactive_only (#745)"
        );
        let hint = parsed["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "claw {cmd} --output-format json hint must be non-empty (#745); got null"
        );
    }
}

#[test]
fn config_unsupported_section_json_hint_741() {
    // #744/#741: claw config <unknown-section> --output-format json must return
    // error_kind:unsupported_config_section with a non-null hint and supported_sections[].
    // This is the regression guard for #741 (hint was null before fix).
    use std::process::Command;
    let root = unique_temp_dir("config-unsupported-section");
    fs::create_dir_all(&root).expect("temp dir");
    let bin = env!("CARGO_BIN_EXE_claw");

    for section in &["list", "show", "bogus"] {
        let output = Command::new(bin)
            .current_dir(&root)
            .args(["--output-format", "json", "config", section])
            .output()
            .expect("claw config should run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
            panic!("claw config {section} --output-format json must emit valid JSON; got: {stdout}")
        });
        assert_eq!(
            parsed["kind"], "config",
            "config {section} JSON must have kind:config (#741)"
        );
        assert_eq!(
            parsed["status"], "error",
            "config {section} must return status:error (#741)"
        );
        assert_eq!(
            parsed["error_kind"], "unsupported_config_section",
            "config {section} must return error_kind:unsupported_config_section (#741)"
        );
        // #741: hint must be a non-empty string (was null before fix)
        let hint = parsed["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "config {section} --output-format json hint must be non-empty (#741)"
        );
        // supported_sections must still be present and non-empty
        assert!(
            parsed["supported_sections"]
                .as_array()
                .map_or(false, |a| !a.is_empty()),
            "config {section} JSON must include supported_sections (#741)"
        );
    }
}

#[test]
fn config_help_returns_structured_section_list_344() {
    // #344: /config help should return a structured section list, not an error
    use std::process::Command;
    let root = unique_temp_dir("config-help");
    fs::create_dir_all(&root).expect("temp dir");
    let bin = env!("CARGO_BIN_EXE_claw");
    let output = Command::new(bin)
        .current_dir(&root)
        .args(["--output-format", "json", "config", "help"])
        .output()
        .expect("claw config help should run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("config help should emit valid JSON");
    assert_eq!(parsed["kind"], "config", "config help kind must be config");
    assert_eq!(
        parsed["status"], "ok",
        "config help must return status:ok (#344)"
    );
    assert_eq!(
        parsed["section"], "help",
        "config help section must be help"
    );
    let sections = parsed["available_sections"]
        .as_array()
        .expect("config help must have available_sections array");
    assert!(!sections.is_empty(), "available_sections must not be empty");
}

#[test]
fn export_json_has_kind_702() {
    // #458/#702: `claw export --output-format json` must emit kind:export.
    // We check only the kind field to avoid flakiness from session-store state.
    // A success path with an actual session would also carry status:ok.
    let root = unique_temp_dir("export-json-kind");
    fs::create_dir_all(&root).expect("temp dir should exist");

    // Run without asserting exit code — may fail with no sessions or legacy sessions.
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_claw");
    let output = Command::new(bin)
        .current_dir(&root)
        .args(["--output-format", "json", "export"])
        .env("ANTHROPIC_API_KEY", "test")
        .output()
        .expect("claw binary should run");

    // On success stdout has kind:export; on failure stderr has type:error.
    // Either way, both envelopes must be valid JSON.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter(|l| l.starts_with('{'))
        .collect::<Vec<_>>()
        .join("");

    if output.status.success() {
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("export success stdout must be valid JSON");
        assert_eq!(
            parsed["kind"], "export",
            "export JSON must have kind:export (#458)"
        );
        let status = parsed["status"]
            .as_str()
            .expect("export JSON must have status");
        assert!(
            matches!(status, "ok" | "error"),
            "export status must be ok or error"
        );
    } else {
        // #819: Error envelope in JSON mode must be on stdout (not stderr).
        let stdout_json = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .expect("export failure must emit JSON to stdout (#819)");
        let parsed: serde_json::Value =
            serde_json::from_str(stdout_json).expect("export error stdout must be valid JSON");
        assert_eq!(
            parsed["type"], "error",
            "export error envelope must have type:error"
        );
    }
}

#[test]
fn export_missing_session_json_error_uses_stdout_819() {
    let root = unique_temp_dir("export-missing-session-819");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "export",
            "--session",
            "does-not-exist",
        ],
        &[],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "export missing session should exit rc=1 (#819)"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.is_empty(),
        "export missing session JSON mode must keep stderr empty (#819), got: {stderr:?}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        panic!("export missing session must emit valid stdout JSON (#819), got: {stdout:?}")
    });
    assert_eq!(
        parsed["error_kind"], "session_not_found",
        "export missing session must emit session_not_found (#819): {parsed}"
    );
    assert_eq!(
        parsed["action"], "abort",
        "export missing session should use the abort envelope (#819): {parsed}"
    );
}

#[test]
fn config_parse_error_has_typed_error_kind_and_hint_764() {
    // #764: Malformed .claw/settings.json must emit error_kind:config_parse_error
    // and a non-null hint in --output-format json mode (was error_kind:"unknown"
    // + hint:null before #763/#764 fixes).
    let root = unique_temp_dir("config-parse-error-764");
    fs::create_dir_all(root.join(".claw")).expect("temp .claw dir should exist");

    // Write an invalid JSON file (type mismatch: model must be a string)
    fs::write(root.join(".claw").join("settings.json"), r#"{"model": 99}"#)
        .expect("settings.json should write");

    let output = run_claw(&root, &["--output-format", "json", "config", "show"], &[]);
    assert!(
        !output.status.success(),
        "malformed settings.json should cause non-zero exit"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("stdout should contain a JSON error envelope (#819/#820/#823: abort envelopes route to stdout in JSON mode)");
    let parsed: serde_json::Value =
        serde_json::from_str(json_line).expect("error envelope should be valid JSON");

    assert_eq!(
        parsed["error_kind"], "config_parse_error",
        "malformed settings.json must return error_kind:config_parse_error (#763)"
    );
    let hint = parsed["hint"].as_str().unwrap_or("");
    assert!(
        !hint.is_empty(),
        "malformed settings.json must return non-null hint (#764), got: {hint:?}"
    );
}

#[test]
fn login_logout_removed_subcommands_have_error_kind_and_hint_765() {
    // #765: `claw login` and `claw logout` are removed; JSON envelope must carry
    // error_kind:removed_subcommand + non-null hint pointing to the env var migration.
    // Before fix: single-line error string → error_kind:"unknown" + hint:null.
    let root = unique_temp_dir("login-logout-removed-765");
    fs::create_dir_all(&root).expect("temp dir should exist");

    for subcmd in &["login", "logout"] {
        let output = run_claw(&root, &["--output-format", "json", subcmd], &[]);
        assert!(
            !output.status.success(),
            "claw {subcmd} should exit non-zero"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json_line = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .unwrap_or_else(|| panic!("claw {subcmd} stdout should contain a JSON envelope (#819/#820/#823: abort envelopes route to stdout in JSON mode)"));
        let parsed: serde_json::Value =
            serde_json::from_str(json_line).expect("error envelope should be valid JSON");

        assert_eq!(
            parsed["error_kind"], "removed_subcommand",
            "claw {subcmd} must return error_kind:removed_subcommand (#765)"
        );
        let hint = parsed["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "claw {subcmd} must return non-null hint (#765), got: {hint:?}"
        );
        assert!(
            hint.contains("ANTHROPIC_API_KEY") || hint.contains("ANTHROPIC_AUTH_TOKEN"),
            "claw {subcmd} hint must mention the env var migration path, got: {hint:?}"
        );
    }
}

#[test]
fn diff_extra_args_have_typed_error_kind_and_hint_766() {
    // #766: `claw diff --bogus` returned error_kind:"unknown" + hint:null.
    // `diff` takes no arguments; extra args were unclassified with no remediation.
    let root = git_temp_dir("diff-extra-args-766");

    assert_diff_unexpected_extra_args_json(
        &root,
        &["--output-format", "json", "diff", "--bogus"],
        "claw diff --bogus",
    );
}

#[test]
fn diff_trailing_json_after_malformed_args_is_bounded_json_3129() {
    // #3129: when --output-format json appeared after malformed `diff` args,
    // the parser fell through to the interactive/prompt path and emitted zero
    // JSON stdout. These forms must fail before any provider or TUI path starts.
    let root = git_temp_dir("diff-trailing-json-3129");

    for (args, label) in [
        (
            &["diff", "--bogus-flag", "--output-format", "json"][..],
            "claw diff --bogus-flag --output-format json",
        ),
        (
            &["diff", "does-not-exist", "--output-format", "json"][..],
            "claw diff does-not-exist --output-format json",
        ),
        (
            &[
                "diff",
                "--cached",
                "--bogus-flag",
                "--output-format",
                "json",
            ][..],
            "claw diff --cached --bogus-flag --output-format json",
        ),
    ] {
        assert_diff_unexpected_extra_args_json(&root, args, label);
    }
}

fn git_temp_dir(prefix: &str) -> PathBuf {
    let root = unique_temp_dir(prefix);
    fs::create_dir_all(&root).expect("temp dir should exist");
    // Need a git repo so `diff` reaches argument validation before git checks.
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .expect("git init should launch");
    root
}

fn assert_diff_unexpected_extra_args_json(root: &Path, args: &[&str], label: &str) {
    let output = run_claw(root, args, &[]);
    assert!(
        !output.status.success(),
        "{label} should exit non-zero; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // #819/#820/#823: JSON abort envelopes route to stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.lines().all(|l| !l.trim_start().starts_with('{')),
        "{label} stderr should not contain a JSON envelope in JSON mode (#819/#820/#823); stderr:\n{stderr}"
    );
    let json_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| {
            panic!("{label} stdout should contain a JSON error envelope (#819/#820/#823); stdout:\n{stdout}")
        });
    let parsed: serde_json::Value =
        serde_json::from_str(json_line).expect("error envelope should be valid JSON");

    assert_eq!(
        parsed["error_kind"], "unexpected_extra_args",
        "{label} must return error_kind:unexpected_extra_args"
    );
    let hint = parsed["hint"].as_str().unwrap_or("");
    assert!(
        !hint.is_empty(),
        "{label} must return non-null hint, got: {hint:?}"
    );
    assert!(
        parsed["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "{label} must return non-empty message"
    );
}

#[test]
fn resume_non_slash_trailing_arg_has_typed_error_kind_and_hint_768() {
    // #768: `claw --resume latest compact` (missing leading /) returned
    // error_kind:"unknown" + hint:null. Resume is orchestration-critical;
    // wrappers need a machine-readable signal with a recovery hint.
    let root = unique_temp_dir("resume-invalid-arg-768");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let output = run_claw(
        &root,
        &["--output-format", "json", "--resume", "latest", "compact"],
        &[],
    );
    assert!(
        !output.status.success(),
        "claw --resume latest compact should exit non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("stdout should contain a JSON error envelope (#819/#820/#823: abort envelopes route to stdout in JSON mode)");
    let parsed: serde_json::Value =
        serde_json::from_str(json_line).expect("error envelope should be valid JSON");

    assert_eq!(
        parsed["error_kind"], "invalid_resume_argument",
        "non-slash resume trailing arg must return error_kind:invalid_resume_argument (#768)"
    );
    let hint = parsed["hint"].as_str().unwrap_or("");
    assert!(
        !hint.is_empty(),
        "non-slash resume trailing arg must return non-null hint (#768), got: {hint:?}"
    );
    assert!(
        hint.contains("/compact") || hint.contains("slash-command"),
        "hint must reference slash-command usage, got: {hint:?}"
    );
}

#[test]
fn session_with_unknown_subcommand_returns_interactive_only_not_credentials_767() {
    // #767: `claw session bogus` bypassed all guards and fell through to
    // CliAction::Prompt, reaching the credential-check gate and returning
    // error_kind:"missing_credentials" instead of a structured routing error.
    // Fix: explicit "session" match arm returns interactive_only guidance.
    let root = unique_temp_dir("session-unknown-767");
    fs::create_dir_all(&root).expect("temp dir should exist");

    for sub in &["bogus", "nuke", "delete-all"] {
        let output = run_claw(&root, &["--output-format", "json", "session", sub], &[]);
        assert!(
            !output.status.success(),
            "claw session {sub} should exit non-zero"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json_line = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .unwrap_or_else(|| panic!("claw session {sub} stderr should contain JSON"));
        let parsed: serde_json::Value =
            serde_json::from_str(json_line).expect("error envelope should be valid JSON");

        assert_eq!(
            parsed["error_kind"], "interactive_only",
            "claw session {sub} must return error_kind:interactive_only (#767), not missing_credentials"
        );
        let hint = parsed["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "claw session {sub} must return non-null hint (#767)"
        );
        assert!(
            hint.contains("/session") || hint.contains("--resume"),
            "hint must reference /session usage, got: {hint:?}"
        );
    }
}

#[test]
fn slash_only_verbs_with_args_return_interactive_only_not_credentials_770() {
    // #770: `claw cost breakdown`, `claw clear --force`, `claw memory reset`,
    // and `claw ultraplan bogus` all fell through to CliAction::Prompt and
    // reached the credential gate, returning error_kind:"missing_credentials".
    // These remain slash-only commands; multi-token invocations should return
    // interactive_only guidance. `model` is now a local bounded surface (#807).
    let root = unique_temp_dir("slash-verbs-770");
    fs::create_dir_all(&root).expect("temp dir should exist");

    let cases: &[&[&str]] = &[
        &["cost", "breakdown"],
        &["clear", "--force"],
        &["memory", "reset"],
        &["ultraplan", "bogus"],
    ];

    for args in cases {
        let full_args: Vec<&str> = std::iter::once("--output-format")
            .chain(std::iter::once("json"))
            .chain(args.iter().copied())
            .collect();
        let output = run_claw(&root, &full_args, &[]);
        assert!(
            !output.status.success(),
            "claw {} should exit non-zero",
            args.join(" ")
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json_line = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .unwrap_or_else(|| {
                panic!(
                    "claw {} stderr should contain JSON, got: {stderr}",
                    args.join(" ")
                )
            });
        let parsed: serde_json::Value =
            serde_json::from_str(json_line).expect("error envelope should be valid JSON");

        assert_eq!(
            parsed["error_kind"],
            "interactive_only",
            "claw {} must return error_kind:interactive_only (#770), not missing_credentials",
            args.join(" ")
        );
        let hint = parsed["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "claw {} must return non-null hint (#770)",
            args.join(" ")
        );
    }
}

#[test]
fn agents_plugins_mcp_unknown_subcommand_have_hint_774() {
    // #774: `claw agents bogus`, `claw plugins bogus`, `claw mcp bogus` returned
    // hint:null despite having correct error_kind. Fixed by adding \n delimiter
    // to error strings in commands/src/lib.rs and explicit hint in mcp JSON envelope.
    let root = unique_temp_dir("unknown-subcommands-774");
    fs::create_dir_all(&root).expect("temp dir should exist");

    // agents bogus
    {
        let output = run_claw(&root, &["--output-format", "json", "agents", "bogus"], &[]);
        assert!(!output.status.success(), "agents bogus should fail");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json_line = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .expect("agents bogus should emit JSON error");
        let parsed: serde_json::Value = serde_json::from_str(json_line).unwrap();
        assert_eq!(parsed["error_kind"], "unknown_agents_subcommand");
        let hint = parsed["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "agents bogus hint must be non-null (#774)"
        );
        assert!(
            hint.contains("list") || hint.contains("show") || hint.contains("help"),
            "agents bogus hint must mention supported actions, got: {hint:?}"
        );
    }

    // plugins bogus
    {
        let output = run_claw(&root, &["--output-format", "json", "plugins", "bogus"], &[]);
        assert!(!output.status.success(), "plugins bogus should fail");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json_line = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .expect("plugins bogus should emit JSON error");
        let parsed: serde_json::Value = serde_json::from_str(json_line).unwrap();
        assert_eq!(parsed["error_kind"], "unknown_plugins_action");
        let hint = parsed["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "plugins bogus hint must be non-null (#774)"
        );
    }

    // mcp bogus
    {
        let output = run_claw(&root, &["--output-format", "json", "mcp", "bogus"], &[]);
        assert!(!output.status.success(), "mcp bogus should fail");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json_str = if stdout.trim().starts_with('{') {
            stdout.to_string()
        } else {
            stderr
                .lines()
                .find(|l| l.trim_start().starts_with('{'))
                .unwrap_or("")
                .to_string()
        };
        let parsed: serde_json::Value =
            serde_json::from_str(json_str.trim()).expect("mcp bogus should emit JSON");
        assert_eq!(parsed["error_kind"], "unsupported_action");
        let hint = parsed["hint"].as_str().unwrap_or("");
        assert!(!hint.is_empty(), "mcp bogus hint must be non-null (#774)");
    }
}

#[test]
fn mcp_show_missing_server_name_returns_missing_argument_830() {
    let root = unique_temp_dir("mcp-show-missing-830");
    fs::create_dir_all(&root).expect("temp dir");

    let output = run_claw(&root, &["--output-format", "json", "mcp", "show"], &[]);
    assert!(
        !output.status.success(),
        "mcp show without server must fail"
    );
    assert_eq!(output.status.code(), Some(1), "exit code must be 1 (#830)");
    assert!(
        output.stderr.is_empty(),
        "JSON mcp show missing-argument error must keep stderr empty (#830), got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("mcp show missing server should emit valid JSON on stdout");
    assert_eq!(parsed["kind"], "mcp");
    assert_eq!(parsed["action"], "show");
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error_kind"], "missing_argument");
    assert!(
        parsed["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("mcp show <server>"),
        "hint should contain usage example, got: {}",
        parsed["hint"]
    );
}

#[test]
fn interactive_only_guard_batch_769_to_771() {
    // #769-#771: a sweep of slash-only verbs with args that previously fell to
    // CliAction::Prompt hitting the credential gate. All must return
    // error_kind:interactive_only (not missing_credentials) with non-null hint.
    let root = unique_temp_dir("interactive-only-batch-769-771");
    fs::create_dir_all(&root).expect("temp dir should exist");
    // Need a git repo for some subcommands
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let cases: &[&[&str]] = &[
        // #769: session with unknown subcommand
        &["session", "bogus"],
        &["session", "nuke"],
        // #770: slash-only verbs with trailing args
        &["cost", "breakdown"],
        &["clear", "--force"],
        &["memory", "reset"],
        &["ultraplan", "bogus"],
        // #771: usage/stats/fork
        &["usage", "extra"],
        &["stats", "extra"],
        &["fork", "newbranch"],
    ];

    let model_output = run_claw(
        &root,
        &["--output-format", "json", "model", "opus", "extra"],
        &[],
    );
    assert!(
        !model_output.status.success(),
        "claw model opus extra should exit non-zero"
    );
    let model_stdout = String::from_utf8_lossy(&model_output.stdout);
    let model_json: serde_json::Value = serde_json::from_str(model_stdout.trim())
        .unwrap_or_else(|_| panic!("claw model opus extra should emit JSON, got: {model_stdout}"));
    assert_eq!(
        model_json["error_kind"], "unexpected_extra_args",
        "claw model opus extra should now stay local and typed (#807), not missing_credentials: {model_json}"
    );
    assert!(
        model_json["hint"]
            .as_str()
            .is_some_and(|hint| !hint.is_empty()),
        "claw model opus extra should include a usage hint: {model_json}"
    );

    for args in cases {
        let full_args: Vec<&str> = std::iter::once("--output-format")
            .chain(std::iter::once("json"))
            .chain(args.iter().copied())
            .collect();
        let output = run_claw(&root, &full_args, &[]);
        assert!(
            !output.status.success(),
            "claw {} should exit non-zero",
            args.join(" ")
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json_line = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .unwrap_or_else(|| {
                panic!(
                    "claw {} should emit JSON, got stderr: {stderr}",
                    args.join(" ")
                )
            });
        let parsed: serde_json::Value = serde_json::from_str(json_line).unwrap();
        assert_eq!(
            parsed["error_kind"],
            "interactive_only",
            "claw {} must return interactive_only, got {:?}",
            args.join(" "),
            parsed["error_kind"]
        );
        let hint = parsed["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "claw {} must have non-null hint",
            args.join(" ")
        );
    }
}

#[test]
fn resume_plugin_mutations_are_typed_interactive_only_777() {
    // #777: `/plugins install|enable|disable|uninstall|update` in resume mode returned
    // a generic single-line error; after #776's classify/split it fell to
    // error_kind:"unknown" + hint:null because there was no interactive_only: prefix.
    // Fix: each mutation arm now returns "interactive_only: ... \n..." so the caller
    // gets error_kind:interactive_only + non-null hint pointing at live REPL.
    let root = unique_temp_dir("resume-plugin-mutations-777");
    fs::create_dir_all(&root).expect("temp dir should exist");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    // Create a minimal session file so we get past session load and into command dispatch
    let session_file = write_session_fixture(&root, "resume-plugin-777", None);

    for mutation in &["install", "enable", "disable", "uninstall", "update"] {
        let cmd = format!("/plugins {mutation} my-plugin");
        let output = run_claw(
            &root,
            &[
                "--resume",
                session_file.to_str().unwrap(),
                "--output-format",
                "json",
                &cmd,
            ],
            &[],
        );
        assert!(
            !output.status.success(),
            "/plugins {mutation} in resume mode should exit non-zero"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json_line = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .unwrap_or_else(|| {
                panic!("/plugins {mutation} should emit JSON error on stdout, got: {stderr}")
            });
        let parsed: serde_json::Value = serde_json::from_str(json_line).unwrap();
        assert_eq!(
            parsed["error_kind"], "interactive_only",
            "/plugins {mutation} must return interactive_only, got {:?}",
            parsed["error_kind"]
        );
        let hint = parsed["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "/plugins {mutation} must have non-null hint (#777)"
        );
        assert!(
            hint.contains("claw") || hint.contains("REPL") || hint.contains("plugins"),
            "/plugins {mutation} hint must reference live session or CLI, got: {hint:?}"
        );
    }
}

#[test]
fn resume_skills_invocation_is_typed_interactive_only_779() {
    // #779: `/skills <skill>` invocation in resume mode returned bare prose;
    // after #776 classify/split it fell to error_kind:"unknown" + hint:null.
    // Fix: use interactive_only: prefix + \n hint so callers get typed fields.
    let root = unique_temp_dir("resume-skills-invocation-779");
    fs::create_dir_all(&root).expect("temp dir should exist");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();
    let session_file = write_session_fixture(&root, "resume-skills-779", None);

    // A non-empty skills arg that would classify as Invoke
    let output = run_claw(
        &root,
        &[
            "--resume",
            session_file.to_str().unwrap(),
            "--output-format",
            "json",
            "/skills my-skill",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "/skills <skill> in resume mode should exit non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| {
            panic!("/skills invocation should emit JSON error on stdout, got: {stderr}")
        });
    let parsed: serde_json::Value = serde_json::from_str(json_line).unwrap();
    assert_eq!(
        parsed["error_kind"], "interactive_only",
        "resumed /skills invocation must return interactive_only, got {:?}",
        parsed["error_kind"]
    );
    let hint = parsed["hint"].as_str().unwrap_or("");
    assert!(
        !hint.is_empty(),
        "resumed /skills invocation must have non-null hint (#779)"
    );
    assert!(
        hint.contains("claw") || hint.contains("REPL") || hint.contains("skills"),
        "hint must reference live session or CLI, got: {hint:?}"
    );
}

#[test]
fn acp_unsupported_invocation_has_hint_782() {
    // #782: `claw acp start` returned error_kind:unsupported_acp_invocation but hint:null
    // because the remediation text was on the same line as the error message.
    // Fix: add \n-delimited hint so split_error_hint extracts it.
    let root = unique_temp_dir("acp-unsupported-782");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(&root, &["--output-format", "json", "acp", "start"], &[]);
    assert!(!output.status.success(), "acp start should fail");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("should emit JSON error");
    let parsed: serde_json::Value = serde_json::from_str(json_line).unwrap();
    assert_eq!(
        parsed["error_kind"], "unsupported_acp_invocation",
        "unsupported ACP invocation should be classified correctly"
    );
    let hint = parsed["hint"]
        .as_str()
        .expect("hint must be non-null (#782)");
    assert!(!hint.is_empty(), "hint must not be empty");
    assert!(
        hint.contains("not implemented") || hint.contains("unsupported"),
        "hint should explain the not-implemented status, got: {hint:?}"
    );
}

#[test]
fn init_json_envelope_has_hint_and_already_initialized_783() {
    // #783: claw --output-format json init was missing the hint field entirely.
    // Also added already_initialized: bool so orchestrators can detect the idempotent
    // case without checking created.len() == 0.
    let root = unique_temp_dir("init-hint-783");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    // Fresh init — already_initialized should be false, hint should mention CLAUDE.md
    let output = run_claw(&root, &["--output-format", "json", "init"], &[]);
    assert!(output.status.success(), "init should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw = if stdout.trim_start().starts_with('{') {
        &*stdout
    } else {
        &*stderr
    };
    let parsed: serde_json::Value = serde_json::from_str(raw.trim()).unwrap_or_else(|_| {
        // multi-line JSON; find the whole block
        serde_json::from_str(raw).expect("should emit valid JSON")
    });

    assert_eq!(parsed["status"], "ok", "init should succeed");
    assert!(
        parsed.get("already_initialized").is_some(),
        "init JSON must include already_initialized field (#783)"
    );
    assert_eq!(
        parsed["already_initialized"], false,
        "first init: already_initialized must be false"
    );
    let hint = parsed["hint"]
        .as_str()
        .expect("hint must be present and non-null (#783)");
    assert!(!hint.is_empty(), "hint must not be empty");
    assert!(
        hint.contains("CLAUDE.md") || hint.contains("doctor"),
        "fresh-init hint should mention CLAUDE.md or doctor, got: {hint:?}"
    );
    assert_eq!(
        parsed["created"],
        json!([
            ".claw/",
            ".claw/settings.json",
            ".claw.json",
            ".gitignore",
            "CLAUDE.md"
        ]),
        "fresh init should materialize .claw/settings.json and safe .claw.json"
    );
    assert_eq!(
        parsed["deferred"],
        json!([".claw/sessions/"]),
        "session storage should be reported as deferred until first save"
    );
    assert_eq!(parsed["partial"], json!([]));
    let claw_json = fs::read_to_string(root.join(".claw.json")).expect("read .claw.json");
    assert!(
        claw_json.contains("\"defaultMode\": \"acceptEdits\""),
        "init must not scaffold dontAsk in .claw.json: {claw_json}"
    );
    assert!(
        !claw_json.contains("dontAsk"),
        "init must not scaffold unsafe dontAsk permission mode: {claw_json}"
    );
    let settings_json = root.join(".claw").join("settings.json");
    assert!(
        settings_json.is_file(),
        "init should template .claw/settings.json"
    );
    assert!(
        !root.join(".claw").join("sessions").exists(),
        "sessions directory should remain deferred until first save"
    );

    // Idempotent re-init — already_initialized should be true
    let output2 = run_claw(&root, &["--output-format", "json", "init"], &[]);
    assert!(output2.status.success(), "re-init should succeed");
    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    let raw2 = if stdout2.trim_start().starts_with('{') {
        &*stdout2
    } else {
        &*stderr2
    };
    let parsed2: serde_json::Value = serde_json::from_str(raw2.trim())
        .or_else(|_| serde_json::from_str(raw2))
        .expect("re-init should emit valid JSON");
    assert_eq!(
        parsed2["already_initialized"], true,
        "re-init: already_initialized must be true"
    );
    let hint2 = parsed2["hint"]
        .as_str()
        .expect("hint must be present on re-init");
    assert!(
        hint2.contains("already") || hint2.contains("doctor"),
        "re-init hint should acknowledge workspace exists, got: {hint2:?}"
    );

    let existing_claw_root = unique_temp_dir("init-existing-claw-436");
    fs::create_dir_all(existing_claw_root.join(".claw")).expect("existing .claw dir");
    let partial_output = run_claw(
        &existing_claw_root,
        &["--output-format", "json", "init"],
        &[],
    );
    assert!(
        partial_output.status.success(),
        "init with existing .claw should succeed"
    );
    let partial_stdout = String::from_utf8_lossy(&partial_output.stdout);
    let partial: serde_json::Value =
        serde_json::from_str(partial_stdout.trim()).expect("partial init should emit valid JSON");
    assert_eq!(
        partial["partial"],
        json!([".claw/"]),
        "existing .claw with newly-created settings should report partial .claw/"
    );
    assert_eq!(
        partial["created"],
        json!([
            ".claw/settings.json",
            ".claw.json",
            ".gitignore",
            "CLAUDE.md"
        ]),
        "init should still create missing sub-files when .claw already exists"
    );
    assert!(
        existing_claw_root
            .join(".claw")
            .join("settings.json")
            .is_file(),
        "existing .claw must receive missing settings template"
    );
}

#[test]
fn export_arg_errors_have_typed_kind_and_hint_784() {
    // #784: `claw export --output` (missing flag value) returned error_kind:"unknown" + hint:null.
    // `claw export a.md b.md` (extra positional) also returned unknown+null.
    // Both export arg errors now use typed prefixes + usage hint.
    let root = unique_temp_dir("export-arg-errors-784");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    // Missing --output value
    let out1 = run_claw(
        &root,
        &["--output-format", "json", "export", "--output"],
        &[],
    );
    assert!(!out1.status.success(), "--output with no value should fail");
    let stderr1 = String::from_utf8_lossy(&out1.stderr);
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    let j1: serde_json::Value = stdout1
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("missing --output should emit JSON error");
    assert_eq!(
        j1["error_kind"], "missing_flag_value",
        "missing --output value should be missing_flag_value, got {:?}",
        j1["error_kind"]
    );
    let h1 = j1["hint"]
        .as_str()
        .expect("missing_flag_value must have hint (#784)");
    assert!(
        !h1.is_empty() && h1.contains("export"),
        "hint must reference export usage, got: {h1:?}"
    );

    // Extra positional argument
    let out2 = run_claw(
        &root,
        &["--output-format", "json", "export", "first.md", "second.md"],
        &[],
    );
    assert!(!out2.status.success(), "extra positional should fail");
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let j2: serde_json::Value = stdout2
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("extra positional should emit JSON error");
    assert_eq!(
        j2["error_kind"], "unexpected_extra_args",
        "extra positional should be unexpected_extra_args, got {:?}",
        j2["error_kind"]
    );
    let h2 = j2["hint"]
        .as_str()
        .expect("unexpected_extra_args must have hint (#784)");
    assert!(
        !h2.is_empty() && h2.contains("export"),
        "hint must reference export usage, got: {h2:?}"
    );
}

#[test]
fn unknown_subcommand_returns_typed_kind_785() {
    // #785: `claw dump` (a near-miss for dump-manifests) returned error_kind:"unknown"
    // because the classifier had no arm for "unknown subcommand:" prose prefix.
    // Fix: added "unknown_subcommand" arm in classify_error_kind.
    let root = unique_temp_dir("unknown-subcommand-785");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    // "dump" is close enough to "dump-manifests" to trigger the typo suggestion path
    let output = run_claw(&root, &["--output-format", "json", "dump"], &[]);
    assert!(!output.status.success(), "unknown subcommand should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("unknown subcommand should emit JSON error");
    // #825: unified under command_not_found (previously unknown_subcommand)
    assert_eq!(
        j["error_kind"], "command_not_found",
        "unknown subcommand should return command_not_found kind (#825), got {:?}",
        j["error_kind"]
    );
    // hint should point at the suggestion and/or --help
    let hint = j["hint"].as_str().unwrap_or("");
    assert!(
        hint.contains("dump-manifests") || hint.contains("--help") || hint.contains("claw"),
        "hint should reference the suggested subcommand or help, got: {hint:?}"
    );
}

#[test]
fn dump_manifests_missing_dir_has_typed_kind_and_hint_786() {
    // #786: `claw dump-manifests --manifests-dir` (no value) and `--manifests-dir=` (empty)
    // both emitted plain "--manifests-dir requires a path" with error_kind:"unknown" + hint:null.
    // Fix: use missing_flag_value: prefix + \n usage hint.
    let root = unique_temp_dir("dump-manifests-missing-dir-786");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    // Case 1: --manifests-dir with no following value (next arg is --output-format)
    let out1 = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "dump-manifests",
            "--manifests-dir",
            "--output-format",
            "json",
        ],
        &[],
    );
    assert!(!out1.status.success());
    let stderr1 = String::from_utf8_lossy(&out1.stderr);
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    let j1: serde_json::Value = stdout1
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("missing --manifests-dir value should emit JSON error");
    assert_eq!(
        j1["error_kind"], "missing_flag_value",
        "missing --manifests-dir value should be missing_flag_value, got {:?}",
        j1["error_kind"]
    );
    let h1 = j1["hint"]
        .as_str()
        .expect("missing_flag_value must have hint (#786)");
    assert!(
        h1.contains("dump-manifests") || h1.contains("manifests-dir"),
        "hint should reference dump-manifests usage, got: {h1:?}"
    );

    // Case 2: --manifests-dir= with empty value
    let out2 = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "dump-manifests",
            "--manifests-dir=",
        ],
        &[],
    );
    assert!(!out2.status.success());
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let j2: serde_json::Value = stdout2
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("empty --manifests-dir= should emit JSON error");
    assert_eq!(
        j2["error_kind"], "missing_flag_value",
        "empty --manifests-dir= should be missing_flag_value, got {:?}",
        j2["error_kind"]
    );
    let h2 = j2["hint"]
        .as_str()
        .expect("missing_flag_value must have hint (#786)");
    assert!(!h2.is_empty(), "hint must not be empty");
}

#[test]
fn resume_directory_path_returns_typed_kind_and_hint_787() {
    // #787: `claw --resume /tmp` (directory instead of .jsonl file) returned
    // error_kind:"session_load_failed" + hint:null. The OS error "Is a directory (os error 21)"
    // had no \n delimiter so split_error_hint returned None, and the resume error path
    // didn't call fallback_hint_for_error_kind.
    // Fix: (1) added session_path_is_directory classifier arm for os error 21;
    //      (2) wired fallback_hint_for_error_kind into both resume error emission sites.
    let root = unique_temp_dir("resume-dir-787");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    // Pass the root directory itself as the session path
    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "--resume",
            root.to_str().unwrap(),
            "/status",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "resume with directory should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("resume with directory should emit JSON error");
    assert_eq!(
        j["error_kind"], "session_path_is_directory",
        "directory resume path should return session_path_is_directory, got {:?}",
        j["error_kind"]
    );
    let hint = j["hint"]
        .as_str()
        .expect("session_path_is_directory must have hint (#787)");
    assert!(
        hint.contains(".jsonl") || hint.contains("session") || hint.contains("file"),
        "hint should explain expected path format, got: {hint:?}"
    );
}

#[test]
fn skills_show_not_found_emits_single_json_object_788() {
    // #788: `claw --output-format json skills show no-such-skill` emitted TWO JSON objects:
    // one from the skills handler (action:"show", status:"error") and a second from the
    // top-level error handler (action:"abort"). The skills handler returned Err() after
    // printing its JSON, which caused the ? propagation to trigger a duplicate envelope.
    // Fix: exit(1) directly after the skills JSON is emitted instead of returning Err.
    let root = unique_temp_dir("skills-show-double-emit-788");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "skills",
            "show",
            "no-such-skill-xyz",
        ],
        &[],
    );
    assert!(!output.status.success(), "skills show unknown should fail");
    // Skills handler emits JSON to stdout; the duplicate was on stderr from the main error path.
    // After fix: stdout has 1 JSON object, stderr has none (no duplicate).
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Count JSON objects in stdout — must be exactly 1
    let json_objects: Vec<serde_json::Value> = {
        let mut objects = Vec::new();
        let mut remaining = stdout.trim();
        while !remaining.is_empty() {
            match serde_json::from_str::<serde_json::Value>(remaining) {
                Ok(v) => {
                    objects.push(v);
                    break;
                }
                Err(_) => {
                    // Try finding a complete JSON object
                    if let Some(pos) = remaining.find('{') {
                        remaining = &remaining[pos..];
                        let mut depth = 0i32;
                        let mut end = 0;
                        for (i, c) in remaining.char_indices() {
                            match c {
                                '{' => depth += 1,
                                '}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        end = i + 1;
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                        if end > 0 {
                            if let Ok(v) = serde_json::from_str(&remaining[..end]) {
                                objects.push(v);
                                remaining = remaining[end..].trim_start();
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        objects
    };

    assert_eq!(
        json_objects.len(),
        1,
        "skills show not-found must emit exactly 1 JSON object on stdout, got {}. stdout: {} stderr: {}",
        json_objects.len(),
        stdout,
        stderr
    );
    // Verify stderr has no duplicate error JSON (the pre-#788 bug was a second abort envelope here)
    let stderr_has_json = stderr.lines().any(|l| l.trim_start().starts_with('{'));
    assert!(
        !stderr_has_json,
        "stderr must have no duplicate JSON error envelope, got: {stderr}"
    );
    assert_eq!(
        json_objects[0]["error_kind"], "skill_not_found",
        "single JSON object must have skill_not_found error_kind"
    );
    assert_eq!(json_objects[0]["status"], "error");
}

#[test]
fn agents_show_not_found_exits_nonzero_789() {
    // #789: `claw --output-format json agents show <not-found>` returned exit 0 despite
    // emitting status:"error". print_agents had no error check — just println + Ok(()).
    // Skills was fixed in #788 (exit 1 via process::exit); agents/plugins had the same gap.
    let root = unique_temp_dir("agents-show-exit-789");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "agents",
            "show",
            "no-such-agent-xyz-789",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "agents show not-found must exit non-zero (#789), got exit 0"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("agents show should emit valid JSON");
    assert_eq!(j["error_kind"], "agent_not_found");
    assert_eq!(j["status"], "error");
}

#[test]
fn plugins_show_not_found_exits_nonzero_789() {
    // #789: same as agents — `claw --output-format json plugins show <not-found>` exited 0
    // despite status:"error". The not-found branch used `return Ok(())` instead of exit(1).
    let root = unique_temp_dir("plugins-show-exit-789");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "plugins",
            "show",
            "no-such-plugin-xyz-789",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "plugins show not-found must exit non-zero (#789), got exit 0"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("plugins show should emit valid JSON");
    assert_eq!(j["error_kind"], "plugin_not_found");
    assert_eq!(j["status"], "error");
}

#[test]
fn system_prompt_unknown_option_returns_typed_kind_790() {
    // #790: `claw --output-format json system-prompt bogus` returned error_kind:"unknown" + hint:null.
    // The unknown-option branch emitted plain "unknown system-prompt option: bogus" with no typed
    // prefix. Fix: use unknown_option: prefix + \n usage hint.
    let root = unique_temp_dir("system-prompt-unknown-opt-790");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    // Generic unknown option
    let out1 = run_claw(
        &root,
        &["--output-format", "json", "system-prompt", "bogus"],
        &[],
    );
    assert!(!out1.status.success());
    let stderr1 = String::from_utf8_lossy(&out1.stderr);
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    let j1: serde_json::Value = stdout1
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("unknown option should emit JSON error");
    assert_eq!(
        j1["error_kind"], "unknown_option",
        "system-prompt unknown option should be unknown_option, got {:?}",
        j1["error_kind"]
    );
    let h1 = j1["hint"]
        .as_str()
        .expect("unknown_option must have hint (#790)");
    assert!(
        h1.contains("system-prompt") || h1.contains("claw"),
        "hint should reference system-prompt usage, got: {h1:?}"
    );

    // Special --json case: hint should mention --output-format json
    let out2 = run_claw(
        &root,
        &["--output-format", "json", "system-prompt", "--json"],
        &[],
    );
    assert!(!out2.status.success());
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let j2: serde_json::Value = stdout2
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("--json flag should emit JSON error");
    assert_eq!(j2["error_kind"], "unknown_option");
    let h2 = j2["hint"]
        .as_str()
        .expect("--json case must have hint (#790)");
    assert!(
        h2.contains("output-format") || h2.contains("json"),
        "hint for --json should suggest --output-format json, got: {h2:?}"
    );
}

#[test]
fn config_extra_args_have_non_null_hint_791() {
    // #791: `claw config show bogus-key` and `claw config set a b` returned
    // error_kind:"unexpected_extra_args" + hint:null because the error message
    // "unexpected extra arguments after `claw config ...`: ..." had no \n delimiter.
    // Fix: appended \n + usage hint to the format string.
    let root = unique_temp_dir("config-extra-args-791");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    // config show with extra positional arg
    let out1 = run_claw(
        &root,
        &["--output-format", "json", "config", "show", "bogus-key"],
        &[],
    );
    assert!(!out1.status.success());
    let stderr1 = String::from_utf8_lossy(&out1.stderr);
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    let j1: serde_json::Value = stdout1
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("config show extra arg should emit JSON error");
    assert_eq!(
        j1["error_kind"], "unexpected_extra_args",
        "config show extra arg should be unexpected_extra_args, got {:?}",
        j1["error_kind"]
    );
    let h1 = j1["hint"]
        .as_str()
        .expect("unexpected_extra_args must have hint (#791)");
    assert!(
        h1.contains("config") || h1.contains("claw"),
        "hint should reference config usage, got: {h1:?}"
    );

    // config set with extra positionals
    let out2 = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "config",
            "set",
            "bogus-section.key",
            "value",
        ],
        &[],
    );
    assert!(!out2.status.success());
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let j2: serde_json::Value = stdout2
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("config set extra arg should emit JSON error");
    assert_eq!(j2["error_kind"], "unexpected_extra_args");
    assert!(
        j2["hint"].as_str().is_some_and(|h| !h.is_empty()),
        "config set extra arg must have non-null hint (#791)"
    );
}

#[test]
fn agents_list_flag_shaped_filter_returns_unknown_option_792() {
    // #792: `claw --output-format json agents list --bogus-flag` silently returned
    // status:"ok" count:0 instead of an error. The list filter arm in
    // handle_agents_slash_command_json treated "--bogus-flag" as a name substring
    // filter (no agents match), producing a false-positive empty success result.
    // Fix: detect filter tokens starting with "-" and return unknown_option + hint.
    let root = unique_temp_dir("agents-list-flag-792");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "agents",
            "list",
            "--unknown-flag",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "agents list --unknown-flag must exit non-zero (#792)"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("agents list flag-filter should emit valid JSON");
    assert_eq!(
        j["error_kind"], "unknown_option",
        "agents list flag-shaped filter must return unknown_option, got {:?}",
        j["error_kind"]
    );
    assert_eq!(j["status"], "error");
    let h = j["hint"]
        .as_str()
        .expect("unknown_option must have hint (#792)");
    assert!(
        h.contains("claw agents list") || h.contains("filter"),
        "hint should reference correct usage, got: {h:?}"
    );
}

#[test]
fn skills_list_flag_shaped_filter_returns_unknown_option_792() {
    // #792: same gap as agents — `claw skills list --bogus-flag` returned success
    // with empty list instead of unknown_option error.
    let root = unique_temp_dir("skills-list-flag-792");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "skills",
            "list",
            "--unknown-flag",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "skills list --unknown-flag must exit non-zero (#792)"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("skills list flag-filter should emit valid JSON");
    assert_eq!(
        j["error_kind"], "unknown_option",
        "skills list flag-shaped filter must return unknown_option, got {:?}",
        j["error_kind"]
    );
    assert_eq!(j["status"], "error");
    assert!(
        j["hint"]
            .as_str()
            .is_some_and(|h| h.contains("claw skills list") || h.contains("filter")),
        "hint should reference correct usage (#792)"
    );
}

#[test]
fn plugins_list_flag_shaped_filter_returns_cli_parse_on_stdout_793_817() {
    // #793: `claw plugins list --bogus-flag` silently returned status:"ok" with empty
    // plugins list instead of an error. The list filter branch in print_plugins treated
    // "--bogus-flag" as an id substring filter and found no matches, producing a false-positive.
    // #817: in JSON mode, handled local parse errors now return error_kind:"cli_parse"
    // on stdout with stderr empty.
    let root = unique_temp_dir("plugins-list-flag-793");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "plugins",
            "list",
            "--unknown-flag",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "plugins list --unknown-flag must exit non-zero (#793)"
    );
    assert_eq!(output.status.code(), Some(1), "exit code must be 1 (#817)");
    // #817: handled JSON local parse errors stay on stdout, with stderr empty.
    assert!(
        output.stderr.is_empty(),
        "plugins list flag-filter JSON error must keep stderr empty (#817), got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("plugins list flag-filter should emit valid JSON on stdout");
    assert_eq!(j["error_kind"], "cli_parse");
    assert_eq!(j["status"], "error");
    let h = j["hint"]
        .as_str()
        .expect("error must have hint (#793/#803)");
    assert!(
        h.contains("plugins list") || h.contains("filter") || h.contains("claw"),
        "hint should reference plugins list usage, got: {h:?}"
    );
}

#[test]
fn plugins_uninstall_not_found_has_hint_793() {
    // #793: `claw plugins uninstall no-such-plugin` returned plugin_not_found + hint:null.
    // The error propagated from plugins_command_payload_for via ? with no \n delimiter;
    // split_error_hint returned None and plugin_not_found wasn't in the fallback table.
    // Fix: added "plugin_not_found" entry to fallback_hint_for_error_kind().
    let root = unique_temp_dir("plugins-uninstall-793");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "plugins",
            "uninstall",
            "no-such-xyz-793",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "plugins uninstall not-found must exit non-zero (#793)"
    );
    // Error envelope goes to stderr (propagated via ? to main error handler)
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("plugins uninstall not-found should emit JSON error envelope");
    assert_eq!(j["error_kind"], "plugin_not_found");
    let h = j["hint"]
        .as_str()
        .expect("plugin_not_found must have non-null hint (#793)");
    assert!(
        h.contains("plugins list") || h.contains("claw plugins"),
        "hint should reference plugins list, got: {h:?}"
    );
}

#[test]
fn plugins_install_not_found_path_returns_typed_kind_794() {
    // #794: `claw plugins install /nonexistent/path` returned error_kind:"unknown" + hint:null.
    // The message "plugin source ... was not found" had no classifier arm; fell to "unknown".
    // Fix: added "plugin_source_not_found" classifier arm + fallback hint table entry.
    let root = unique_temp_dir("plugins-install-794");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "plugins",
            "install",
            "/nonexistent-path-xyz-794",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "plugins install not-found-path must exit non-zero (#794)"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("plugins install not-found should emit JSON error envelope");
    assert_eq!(
        j["error_kind"], "plugin_source_not_found",
        "plugins install not-found should be plugin_source_not_found, got {:?}",
        j["error_kind"]
    );
    let h = j["hint"]
        .as_str()
        .expect("plugin_source_not_found must have non-null hint (#794)");
    assert!(!h.is_empty(), "hint must be non-empty");
}

#[test]
fn skills_lifecycle_errors_have_typed_local_json_795_431() {
    // #431: skills install/uninstall lifecycle paths are local JSON surfaces and must not
    // fall through to provider credential checks. #795: every error envelope needs a hint.
    let root = unique_temp_dir("skills-lifecycle-431");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&home).expect("home");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();
    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
        ("ANTHROPIC_API_KEY", ""),
        ("ANTHROPIC_AUTH_TOKEN", ""),
        ("OPENAI_API_KEY", ""),
    ];

    let missing_arg = run_claw(
        &root,
        &["skills", "install", "--output-format", "json"],
        &envs,
    );
    assert_eq!(missing_arg.status.code(), Some(1));
    assert!(
        missing_arg.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&missing_arg.stderr)
    );
    let missing_arg_json = parse_json_stdout(&missing_arg, "skills install missing source");
    assert_eq!(missing_arg_json["kind"], "skills");
    assert_eq!(missing_arg_json["action"], "install");
    assert_eq!(missing_arg_json["error_kind"], "missing_argument");
    assert_eq!(missing_arg_json["argument"], "install_source");
    assert!(missing_arg_json["hint"]
        .as_str()
        .is_some_and(|hint| !hint.is_empty()));

    let invalid_source = run_claw(
        &root,
        &["skills", "install", "bogus-name", "--output-format", "json"],
        &envs,
    );
    assert_eq!(invalid_source.status.code(), Some(1));
    assert!(
        invalid_source.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&invalid_source.stderr)
    );
    let invalid_source_json = parse_json_stdout(&invalid_source, "skills install invalid source");
    assert_eq!(invalid_source_json["kind"], "skills");
    assert_eq!(invalid_source_json["action"], "install");
    assert_eq!(invalid_source_json["error_kind"], "invalid_install_source");
    assert_eq!(invalid_source_json["source"], "bogus-name");
    assert_eq!(invalid_source_json["source_kind"], "name");
    assert_eq!(invalid_source_json["reason"], "not_found");
    assert!(invalid_source_json["hint"]
        .as_str()
        .is_some_and(|hint| { hint.contains("local path") || hint.contains("SKILL.md") }));

    let missing_uninstall = run_claw(
        &root,
        &[
            "skills",
            "uninstall",
            "nonexistent-skill-xyz",
            "--output-format",
            "json",
        ],
        &envs,
    );
    assert_eq!(missing_uninstall.status.code(), Some(1));
    assert!(
        missing_uninstall.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&missing_uninstall.stderr)
    );
    let missing_uninstall_json =
        parse_json_stdout(&missing_uninstall, "skills uninstall missing skill");
    assert_eq!(missing_uninstall_json["kind"], "skills");
    assert_eq!(missing_uninstall_json["action"], "uninstall");
    assert_eq!(missing_uninstall_json["error_kind"], "skill_not_found");
    assert_eq!(missing_uninstall_json["requested"], "nonexistent-skill-xyz");
    assert_eq!(
        missing_uninstall_json["skills_dir"],
        config_home.join("skills").display().to_string()
    );
    assert_eq!(
        missing_uninstall_json["available_names"]
            .as_array()
            .expect("available_names")
            .len(),
        0
    );
    assert!(missing_uninstall_json["hint"]
        .as_str()
        .is_some_and(|hint| !hint.is_empty()));
}

#[test]
fn skills_install_uninstall_roundtrip_stays_local_431() {
    let root = unique_temp_dir("skills-roundtrip-431");
    let config_home = root.join("config-home");
    let home = root.join("home");
    let source_root = root.join("fixtures");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&home).expect("home");
    write_skill(&source_root, "roundtrip", "Roundtrip skill");
    let skill_source = source_root.join("roundtrip");
    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
        ("ANTHROPIC_API_KEY", ""),
        ("ANTHROPIC_AUTH_TOKEN", ""),
        ("OPENAI_API_KEY", ""),
    ];

    let install = run_claw(
        &root,
        &[
            "skills",
            "install",
            skill_source.to_str().expect("utf8 skill source"),
            "--output-format",
            "json",
        ],
        &envs,
    );
    assert!(
        install.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );
    let install_json = parse_json_stdout(&install, "skills install roundtrip");
    assert_eq!(install_json["kind"], "skills");
    assert_eq!(install_json["action"], "install");
    assert_eq!(install_json["status"], "ok");
    assert_eq!(install_json["invocation_name"], "roundtrip");
    let installed_path = config_home.join("skills").join("roundtrip");
    assert_eq!(
        install_json["installed_path"],
        installed_path.display().to_string()
    );
    assert!(installed_path.join("SKILL.md").is_file());

    let uninstall = run_claw(
        &root,
        &[
            "skills",
            "uninstall",
            "roundtrip",
            "--output-format",
            "json",
        ],
        &envs,
    );
    assert!(
        uninstall.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&uninstall.stdout),
        String::from_utf8_lossy(&uninstall.stderr)
    );
    let uninstall_json = parse_json_stdout(&uninstall, "skills uninstall roundtrip");
    assert_eq!(uninstall_json["kind"], "skills");
    assert_eq!(uninstall_json["action"], "uninstall");
    assert_eq!(uninstall_json["status"], "ok");
    assert_eq!(uninstall_json["removed"], "roundtrip");
    assert_eq!(
        uninstall_json["removed_path"],
        installed_path.display().to_string()
    );
    assert!(
        !installed_path.exists(),
        "uninstall should remove installed skill files"
    );
}

#[test]
fn agents_create_scaffolds_toml_and_lists_locally_431() {
    let root = unique_temp_dir("agents-create-431");
    let config_home = root.join("config-home");
    let home = root.join("home");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&home).expect("home");
    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("utf8 config home"),
        ),
        ("HOME", home.to_str().expect("utf8 home")),
        ("ANTHROPIC_API_KEY", ""),
        ("ANTHROPIC_AUTH_TOKEN", ""),
        ("OPENAI_API_KEY", ""),
    ];

    let create = run_claw(
        &root,
        &["agents", "create", "my-agent", "--output-format", "json"],
        &envs,
    );
    assert!(
        create.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    let create_json = parse_json_stdout(&create, "agents create my-agent");
    let agent_path = root.join(".claw").join("agents").join("my-agent.toml");
    let reported_agent_path = PathBuf::from(
        create_json["path"]
            .as_str()
            .expect("agents create should report path"),
    );
    assert_eq!(create_json["kind"], "agents");
    assert_eq!(create_json["action"], "create");
    assert_eq!(create_json["status"], "ok");
    assert_eq!(create_json["format"], "toml");
    assert_eq!(
        reported_agent_path,
        fs::canonicalize(&agent_path).expect("canonical agent path")
    );
    assert!(agent_path.is_file());
    let agent_contents = fs::read_to_string(&agent_path).expect("agent scaffold should read");
    assert!(agent_contents.contains("name = \"my-agent\""));

    let list =
        assert_json_command_with_env(&root, &["--output-format", "json", "agents", "list"], &envs);
    assert_eq!(list["kind"], "agents");
    assert_eq!(list["action"], "list");
    assert!(list["agents"]
        .as_array()
        .expect("agents array")
        .iter()
        .any(|agent| {
            agent["name"] == "my-agent"
                && PathBuf::from(agent["path"].as_str().expect("listed agent path"))
                    == fs::canonicalize(&agent_path).expect("canonical listed agent path")
        }));
}

#[test]
fn agents_show_extra_positional_arg_returns_unexpected_extra_796() {
    // #796: `claw agents show <name> <extra>` treated the full "name extra" as a single
    // agent name, producing agent_not_found for "name extra" instead of flagging the
    // unexpected extra argument. Fix: detect space-containing "name" and return
    // unexpected_extra_args with usage hint.
    let root = unique_temp_dir("agents-show-extra-796");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "agents",
            "show",
            "some-agent",
            "--extra-flag",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "agents show with extra arg must exit non-zero (#796)"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("agents show extra arg should emit valid JSON");
    assert_eq!(
        j["error_kind"], "unexpected_extra_args",
        "agents show extra arg should return unexpected_extra_args, got {:?}",
        j["error_kind"]
    );
    let h = j["hint"]
        .as_str()
        .expect("unexpected_extra_args must have hint (#796)");
    assert!(
        h.contains("claw agents show") || h.contains("Usage"),
        "hint should reference usage, got: {h:?}"
    );
}

#[test]
fn skills_show_extra_positional_arg_returns_unexpected_extra_796() {
    // #796: same gap as agents — `claw skills show <name> <extra>` treated "name extra"
    // as a single skill name → skill_not_found. Fix: detect space-containing name.
    let root = unique_temp_dir("skills-show-extra-796");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "skills",
            "show",
            "some-skill",
            "--extra-flag",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "skills show with extra arg must exit non-zero (#796)"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("skills show extra arg should emit valid JSON");
    assert_eq!(
        j["error_kind"], "unexpected_extra_args",
        "skills show extra arg should return unexpected_extra_args, got {:?}",
        j["error_kind"]
    );
    assert!(
        j["hint"]
            .as_str()
            .is_some_and(|h| h.contains("claw skills show") || h.contains("Usage")),
        "hint should reference usage (#796)"
    );
}

#[test]
fn plugins_extra_args_have_non_null_hint_797() {
    // #797: `claw plugins show <name> <extra>` returned unexpected_extra_args + hint:null.
    // The plugins arg parser at the top level emitted "unexpected extra arguments after
    // `claw plugins show ...`: ..." with no \n delimiter. Parity with #791 config fix.
    let root = unique_temp_dir("plugins-extra-args-797");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(
        &root,
        &[
            "--output-format",
            "json",
            "plugins",
            "show",
            "some-plugin",
            "extra-arg",
        ],
        &[],
    );
    assert!(
        !output.status.success(),
        "plugins show with extra arg must exit non-zero (#797)"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("plugins extra arg should emit JSON error");
    assert_eq!(j["error_kind"], "unexpected_extra_args");
    let h = j["hint"]
        .as_str()
        .expect("unexpected_extra_args must have non-null hint (#797)");
    assert!(
        h.contains("plugins") || h.contains("Usage"),
        "hint should reference plugins usage, got: {h:?}"
    );
}

#[test]
fn plugins_list_trailing_dash_json_error_uses_stdout_817() {
    // ROADMAP #817: JSON inventory/local parse errors are machine-readable on
    // stdout. `plugins list --` used to route through the top-level error path,
    // leaving stdout empty and writing the JSON envelope to stderr.
    let root = unique_temp_dir("plugins-list-dash-817");
    fs::create_dir_all(&root).expect("temp dir");

    let output = run_claw(
        &root,
        &["--output-format", "json", "plugins", "list", "--"],
        &[],
    );
    assert!(
        !output.status.success(),
        "plugins list -- must exit non-zero (#817)"
    );
    assert_eq!(output.status.code(), Some(1), "exit code must be 1 (#817)");
    assert!(
        output.stderr.is_empty(),
        "JSON parse error must keep stderr empty (#817), got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let j: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout should be JSON error (#817)");
    assert_eq!(j["kind"], "plugin");
    assert_eq!(j["action"], "list");
    assert_eq!(j["status"], "error");
    assert_eq!(j["error_kind"], "cli_parse");
    assert_eq!(j["unexpected"], "--");
}

#[test]
fn plugins_list_trailing_dash_text_error_stays_on_stderr_817() {
    let root = unique_temp_dir("plugins-list-dash-text-817");
    fs::create_dir_all(&root).expect("temp dir");

    let output = run_claw(&root, &["plugins", "list", "--"], &[]);
    assert!(
        !output.status.success(),
        "plugins list -- text mode must exit non-zero (#817)"
    );
    assert_eq!(output.status.code(), Some(1), "exit code must be 1 (#817)");
    assert!(
        output.stdout.is_empty(),
        "text parse error should not emit stdout (#817), got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stderr.contains("[error-kind: cli_parse]"), "{stderr}");
    assert!(
        stderr.contains("unknown option for `claw plugins list`: --"),
        "{stderr}"
    );
}

#[test]
fn empty_prompt_has_non_null_hint_798() {
    // #798: `claw --output-format json ""` returned empty_prompt + hint:null.
    // The error message "empty prompt: provide a subcommand..." had no \n delimiter.
    let root = unique_temp_dir("empty-prompt-798");
    fs::create_dir_all(&root).expect("temp dir");
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .ok();

    let output = run_claw(&root, &["--output-format", "json", ""], &[]);
    assert!(
        !output.status.success(),
        "empty prompt must exit non-zero (#798)"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .expect("empty prompt should emit JSON error envelope");
    assert_eq!(j["error_kind"], "empty_prompt");
    let h = j["hint"]
        .as_str()
        .expect("empty_prompt must have non-null hint (#798)");
    assert!(
        h.contains("claw") || h.contains("Usage"),
        "hint should reference usage, got: {h:?}"
    );
}

#[test]
fn diff_non_git_dir_has_error_kind_and_hint_801() {
    // #801: `claw --output-format json diff` in a non-git directory returned
    // status:"error" + result:"no_git_repo" but had no error_kind, hint, or
    // message fields — violating the error envelope contract. Fix: added all
    // three fields to the no_git_repo JSON branch.
    let root = unique_temp_dir("diff-nongit-801");
    fs::create_dir_all(&root).expect("temp dir");
    // Intentionally NOT running git init

    let output = run_claw(&root, &["--output-format", "json", "diff"], &[]);
    // diff non-git may exit 0 (custom JSON handler) — check envelope content
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("diff non-git should emit valid JSON");
    assert_eq!(j["status"], "error");
    assert_eq!(j["result"], "no_git_repo");
    assert_eq!(
        j["error_kind"], "no_git_repo",
        "diff non-git must have error_kind (#801), got {:?}",
        j["error_kind"]
    );
    let h = j["hint"]
        .as_str()
        .expect("diff non-git must have non-null hint (#801)");
    assert!(
        h.contains("git init") || h.contains("git"),
        "hint should suggest git init, got: {h:?}"
    );
    assert!(
        j["message"].as_str().is_some(),
        "diff non-git must have message field (#801)"
    );
}

fn assert_local_json_without_missing_credentials(
    output: &std::process::Output,
    expected_kind: &str,
) -> serde_json::Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "local JSON command should exit 0"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.trim().is_empty(),
        "local JSON command must emit stdout JSON"
    );
    assert!(
        stderr.is_empty(),
        "local JSON command must keep stderr empty, got: {stderr:?}"
    );
    assert!(
        !stdout.contains("missing_credentials"),
        "local JSON command must not hit provider credential startup: {stdout}"
    );
    let j: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("stdout must be parseable JSON, got: {stdout:?}"));
    assert_eq!(j["status"], "ok", "local JSON status: {j}");
    assert_eq!(j["kind"], expected_kind, "local JSON kind: {j}");
    j
}

// #807: model/model(s) JSON/help surfaces must stay bounded and local.
#[test]
fn models_json_and_model_help_json_are_local_807() {
    let root = unique_temp_dir("models-local-json-807");
    std::fs::create_dir_all(&root).expect("create temp dir");

    let models = run_claw(&root, &["models", "--output-format", "json"], &[]);
    let models_json = assert_local_json_without_missing_credentials(&models, "models");
    assert_eq!(
        models_json["action"], "list",
        "models action: {models_json}"
    );
    assert_eq!(
        models_json["requires_provider_request"], false,
        "models must be local: {models_json}"
    );

    let help = run_claw(&root, &["model", "help", "--output-format", "json"], &[]);
    let help_json = assert_local_json_without_missing_credentials(&help, "help");
    assert_eq!(
        help_json["command"], "models",
        "model help command: {help_json}"
    );
}

// #808: settings JSON/help surfaces must stay bounded and local.
#[test]
fn settings_json_and_help_json_are_local_808() {
    let root = unique_temp_dir("settings-local-json-808");
    std::fs::create_dir_all(&root).expect("create temp dir");

    let settings = run_claw(&root, &["settings", "--output-format", "json"], &[]);
    let settings_json = assert_local_json_without_missing_credentials(&settings, "config");
    assert_eq!(
        settings_json["action"], "show",
        "settings action: {settings_json}"
    );
    assert_eq!(
        settings_json["section"], "settings",
        "settings section: {settings_json}"
    );

    let help = run_claw(&root, &["settings", "help", "--output-format", "json"], &[]);
    let help_json = assert_local_json_without_missing_credentials(&help, "help");
    assert_eq!(
        help_json["command"], "settings",
        "settings help command: {help_json}"
    );
}

// #825: unknown single-word subcommand must return command_not_found, not
// fall through to missing_credentials after provider startup.
#[test]
fn unknown_subcommand_json_emits_command_not_found() {
    let root = unique_temp_dir("unknown-cmd-json-825");
    std::fs::create_dir_all(&root).expect("create temp dir");
    let output = run_claw(&root, &["--output-format", "json", "foobar"], &[]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "unknown subcommand should exit 1"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.trim().is_empty(),
        "unknown subcommand JSON envelope must be on stdout"
    );
    let j: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be parseable JSON (#825)");
    assert_eq!(
        j["error_kind"], "command_not_found",
        "unknown subcommand must emit command_not_found, not missing_credentials (#825): {j}"
    );
    assert_eq!(j["status"], "error");
    assert!(
        stderr.is_empty(),
        "unknown subcommand in JSON mode must have empty stderr (#825), got: {stderr:?}"
    );
}

#[test]
fn unknown_subcommand_text_emits_command_not_found_on_stderr() {
    let root = unique_temp_dir("unknown-cmd-text-825");
    std::fs::create_dir_all(&root).expect("create temp dir");
    let output = run_claw(&root, &["foobar"], &[]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "unknown subcommand should exit 1"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let _ = stdout;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("command_not_found"),
        "text mode unknown subcommand must mention command_not_found on stderr (#825), got: {stderr:?}"
    );
    assert!(
        !stderr.contains("missing_credentials"),
        "text mode unknown subcommand must not show missing_credentials (#825)"
    );
}

#[test]
fn unknown_subcommand_typo_with_suggestions_json_emits_command_not_found() {
    let root = unique_temp_dir("unknown-cmd-typo-825");
    std::fs::create_dir_all(&root).expect("create temp dir");
    let output = run_claw(&root, &["--output-format", "json", "statuz"], &[]);
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let j: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("typo envelope must be valid JSON (#825)");
    assert_eq!(j["error_kind"], "command_not_found", "#825 typo: {j}");
    let hint = j["hint"].as_str().unwrap_or("");
    assert!(
        hint.contains("status") || hint.contains("state"),
        "typo hint should suggest status/state, got: {hint:?}"
    );
    assert!(stderr.is_empty(), "typo JSON must have empty stderr (#825)");
}

// #826: JSON-mode multi-word unknown subcommands must not fall through to
// CliAction::Prompt and hit the provider credential gate.
#[test]
fn multi_word_unknown_subcommand_json_emits_command_not_found_826() {
    let root = unique_temp_dir("multi-word-command-not-found-826");
    std::fs::create_dir_all(&root).expect("create temp dir");
    let output = run_claw(&root, &["--output-format", "json", "foobar", "baz"], &[]);
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let j: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("multi-word unknown subcommand must emit JSON");
    assert_eq!(
        j["error_kind"], "command_not_found",
        "multi-word unknown subcommand must emit command_not_found, not missing_credentials (#826): {j}"
    );
    let hint = j["hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("claw prompt") || hint.contains("--help"),
        "hint should explain prompt/command recovery, got: {hint:?}"
    );
    assert!(
        stderr.is_empty(),
        "multi-word command_not_found JSON must have empty stderr: {stderr:?}"
    );
}

#[test]
fn compact_flag_missing_argument_and_shorthand_prompt_contract_435() {
    let root = unique_temp_dir("compact-flag-435");
    let config_home = root.join("config-home");
    let home = root.join("home");
    std::fs::create_dir_all(&root).expect("create temp dir");
    std::fs::create_dir_all(&config_home).expect("create config home");
    std::fs::create_dir_all(&home).expect("create home");
    let envs = [
        (
            "CLAW_CONFIG_HOME",
            config_home.to_str().expect("config home utf8"),
        ),
        ("HOME", home.to_str().expect("home utf8")),
        ("ANTHROPIC_API_KEY", ""),
        ("ANTHROPIC_AUTH_TOKEN", ""),
        ("OPENAI_API_KEY", ""),
    ];

    let missing = run_claw(&root, &["--output-format", "json", "--compact"], &envs);
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        missing.stderr.is_empty(),
        "compact missing-argument JSON should keep stderr empty: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
    let missing_json = parse_json_stdout(&missing, "compact missing argument");
    assert_eq!(missing_json["error_kind"], "missing_argument");
    assert_eq!(missing_json["argument"], "prompt or subcommand");

    let prompt = run_claw(
        &root,
        &["--output-format", "json", "--compact", "hello"],
        &envs,
    );
    assert_eq!(prompt.status.code(), Some(1));
    assert!(
        prompt.stderr.is_empty(),
        "compact prompt JSON should keep stderr empty: {}",
        String::from_utf8_lossy(&prompt.stderr)
    );
    let prompt_json = parse_json_stdout(&prompt, "compact shorthand prompt");
    assert_eq!(
        prompt_json["error_kind"], "missing_credentials",
        "--compact hello should stay on the prompt/provider path, not command_not_found: {prompt_json}"
    );
}

// #827: direct /unknown-slash-command must emit typed error_kind, not "unknown"
// Uses the direct-slash CLI path (no session load needed; reproducible on CI).
#[test]
fn direct_unknown_slash_command_emits_typed_error_kind() {
    let root = unique_temp_dir("direct-unknown-slash-827");
    std::fs::create_dir_all(&root).expect("create temp dir");
    let output = run_claw(&root, &["--output-format", "json", "/boguscommand"], &[]);
    assert_eq!(output.status.code(), Some(1), "unknown slash should exit 1");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let j: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("unknown slash must emit JSON (#827)");
    assert_ne!(
        j["error_kind"], "unknown",
        "direct unknown slash must not emit opaque \'unknown\' error_kind (#827): {j}"
    );
    assert_eq!(
        j["error_kind"], "unknown_slash_command",
        "direct unknown slash must emit unknown_slash_command (#827): {j}"
    );
    assert!(
        stderr.is_empty(),
        "direct unknown slash JSON must have empty stderr (#827)"
    );
}

#[test]
fn resume_unknown_slash_command_emits_typed_error_kind_827() {
    let root = unique_temp_dir("resume-unknown-slash-827");
    std::fs::create_dir_all(&root).expect("create temp dir");
    let session_path = write_session_fixture(&root, "resume-unknown-slash-827", Some("hello"));

    let output = run_claw(
        &root,
        &[
            "--resume",
            session_path.to_str().expect("session path utf8"),
            "--output-format",
            "json",
            "/boguscommand",
        ],
        &[],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "resume unknown slash should exit 2"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let j: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("resume unknown slash must emit JSON (#827), got: {stdout:?}"));
    assert_eq!(
        j["error_kind"], "unknown_slash_command",
        "resume unknown slash must emit unknown_slash_command (#827): {j}"
    );
    assert!(
        stderr.is_empty(),
        "resume unknown slash JSON must have empty stderr (#827): {stderr:?}"
    );
}

// #828: /approve and /deny outside REPL must emit interactive_only, not unknown_slash_command
#[test]
fn approve_deny_outside_repl_emits_interactive_only() {
    let root = unique_temp_dir("approve-deny-828");
    std::fs::create_dir_all(&root).expect("create temp dir");
    for cmd in &["/approve", "/yes", "/deny", "/no"] {
        let output = run_claw(&root, &["--output-format", "json", cmd], &[]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let j: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|_| panic!("{cmd} must emit JSON (#828), got: {stdout:?}"));
        assert_eq!(
            j["error_kind"], "interactive_only",
            "{cmd} outside REPL must emit interactive_only (#828): {j}"
        );
        assert!(
            stderr.is_empty(),
            "{cmd} JSON must have empty stderr (#828): {stderr:?}"
        );
    }
}

// #829: interactive_only hint must NOT suggest --resume for non-resume-safe commands
#[test]
fn non_resume_safe_interactive_only_hint_omits_resume_suggestion() {
    let root = unique_temp_dir("non-resume-hint-829");
    std::fs::create_dir_all(&root).expect("create temp dir");
    // /commit, /pr, /issue, /bughunter, /ultraplan are not resume-safe
    for cmd in &["/commit", "/pr", "/issue", "/bughunter", "/ultraplan"] {
        let output = run_claw(&root, &["--output-format", "json", cmd], &[]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let j: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|_| panic!("{cmd} must emit JSON (#829), got: {stdout:?}"));
        assert_eq!(
            j["error_kind"], "interactive_only",
            "{cmd} must emit interactive_only (#829): {j}"
        );
        let hint = j["hint"].as_str().unwrap_or("");
        assert!(
            !hint.contains("--resume"),
            "{cmd} hint must not suggest --resume for non-resume-safe command (#829): hint={hint:?}"
        );
    }
}

// #829: resume-safe commands should still suggest --resume in the hint
#[test]
fn resume_safe_interactive_only_hint_includes_resume_suggestion() {
    let root = unique_temp_dir("resume-hint-829");
    std::fs::create_dir_all(&root).expect("create temp dir");
    let output = run_claw(&root, &["--output-format", "json", "/compact"], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let j: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("/compact must emit JSON (#829), got: {stdout:?}"));
    let hint = j["hint"].as_str().unwrap_or("");
    assert!(
        hint.contains("--resume"),
        "/compact hint must suggest --resume (it is resume-safe and not a local direct action) (#829): hint={hint:?}"
    );
}
