use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::ContentBlock;
use runtime::Session;
use serde_json::Value;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn resumed_binary_accepts_slash_commands_with_arguments() {
    // given
    let temp_dir = unique_temp_dir("resume-slash-commands");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");

    let session_path = temp_dir.join("session.jsonl");
    let export_path = temp_dir.join("notes.txt");

    let mut session = workspace_session(&temp_dir);
    session
        .push_user_text("ship the slash command harness")
        .expect("session write should succeed");
    session
        .save_to_path(&session_path)
        .expect("session should persist");

    // when
    let output = run_claw(
        &temp_dir,
        &[
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/export",
            export_path.to_str().expect("utf8 path"),
            "/clear",
            "--confirm",
        ],
    );

    // then
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    assert!(stdout.contains("Export"));
    assert!(stdout.contains("wrote transcript"));
    assert!(stdout.contains(export_path.to_str().expect("utf8 path")));
    assert!(stdout.contains("Session cleared"));
    assert!(stdout.contains("Mode             resumed session reset"));
    assert!(stdout.contains("Previous session"));
    assert!(stdout.contains("Resume previous  claw --resume"));
    assert!(stdout.contains("Backup           "));
    assert!(stdout.contains("Session file     "));

    let export = fs::read_to_string(&export_path).expect("export file should exist");
    assert!(export.contains("# Conversation Export"));
    assert!(export.contains("ship the slash command harness"));

    let restored = Session::load_from_path(&session_path).expect("cleared session should load");
    assert!(restored.messages.is_empty());

    let backup_path = stdout
        .lines()
        .find_map(|line| line.strip_prefix("  Backup           "))
        .map(PathBuf::from)
        .expect("clear output should include backup path");
    let backup = Session::load_from_path(&backup_path).expect("backup session should load");
    assert_eq!(backup.messages.len(), 1);
    assert!(matches!(
        backup.messages[0].blocks.first(),
        Some(ContentBlock::Text { text }) if text == "ship the slash command harness"
    ));
}

#[test]
fn status_command_applies_cli_flags_end_to_end() {
    // given
    let temp_dir = unique_temp_dir("status-command-flags");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");

    // when
    let output = run_claw(
        &temp_dir,
        &[
            "--model",
            "sonnet",
            "--permission-mode",
            "read-only",
            "status",
        ],
    );

    // then
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    assert!(stdout.contains("Status"));
    assert!(stdout.contains("Model            anthropic/claude-sonnet-4-6"));
    assert!(stdout.contains("Permission mode  read-only"));
}

#[test]
fn resumed_config_command_loads_settings_files_end_to_end() {
    // given
    let temp_dir = unique_temp_dir("resume-config");
    let project_dir = temp_dir.join("project");
    let config_home = temp_dir.join("home").join(".claw");
    fs::create_dir_all(project_dir.join(".claw")).expect("project config dir should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");

    let session_path = project_dir.join("session.jsonl");
    workspace_session(&project_dir)
        .with_persistence_path(&session_path)
        .save_to_path(&session_path)
        .expect("session should persist");

    fs::write(config_home.join("settings.json"), r#"{"model":"haiku"}"#)
        .expect("user config should write");
    fs::write(
        project_dir.join(".claw").join("settings.local.json"),
        r#"{"model":"opus"}"#,
    )
    .expect("local config should write");

    // when
    let output = run_claw_with_env(
        &project_dir,
        &[
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/config",
            "model",
        ],
        &[("CLAW_CONFIG_HOME", config_home.to_str().expect("utf8 path"))],
    );

    // then
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    assert!(stdout.contains("Config"));
    assert!(stdout.contains("Loaded files      2"));
    assert!(stdout.contains(
        config_home
            .join("settings.json")
            .to_str()
            .expect("utf8 path")
    ));
    assert!(stdout.contains(
        project_dir
            .join(".claw")
            .join("settings.local.json")
            .to_str()
            .expect("utf8 path")
    ));
    assert!(stdout.contains("Merged section: model"));
    assert!(stdout.contains("opus"));
}

#[test]
fn resume_latest_restores_the_most_recent_managed_session() {
    // given
    let temp_dir = unique_temp_dir("resume-latest");
    let project_dir = temp_dir.join("project");
    fs::create_dir_all(&project_dir).expect("project dir should exist");
    let project_dir = fs::canonicalize(&project_dir).unwrap_or(project_dir);
    let store = runtime::SessionStore::from_cwd(&project_dir).expect("session store should build");
    let older_path = store.create_handle("session-older").path;
    let newer_path = store.create_handle("session-newer").path;

    let mut older = workspace_session(&project_dir).with_persistence_path(&older_path);
    older
        .push_user_text("older session")
        .expect("older session write should succeed");
    older
        .save_to_path(&older_path)
        .expect("older session should persist");

    let mut newer = workspace_session(&project_dir).with_persistence_path(&newer_path);
    newer
        .push_user_text("newer session")
        .expect("newer session write should succeed");
    newer
        .push_user_text("resume me")
        .expect("newer session write should succeed");
    newer
        .save_to_path(&newer_path)
        .expect("newer session should persist");

    // when
    let output = run_claw(&project_dir, &["--resume", "latest", "/status"]);

    // then
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    assert!(stdout.contains("Status"));
    assert!(stdout.contains("Messages         2"));
    assert!(stdout.contains(newer_path.to_str().expect("utf8 path")));
}

#[test]
fn resume_latest_missing_session_fails_without_creating_session_dirs_435() {
    // given
    let temp_dir = unique_temp_dir("resume-latest-missing-435");
    let project_dir = temp_dir.join("project");
    let config_home = temp_dir.join("config-home");
    let home = temp_dir.join("home");
    fs::create_dir_all(&project_dir).expect("project dir should exist");
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

    // when — both text and JSON resume failures should be non-zero and read-only.
    let text = run_claw_with_env(&project_dir, &["--resume", "latest"], &envs);
    let json = run_claw_with_env(
        &project_dir,
        &["--output-format", "json", "--resume", "latest"],
        &envs,
    );

    // then
    assert_eq!(
        text.status.code(),
        Some(1),
        "text resume failure must be non-zero"
    );
    assert!(
        text.stdout.is_empty(),
        "text resume failure should not claim success on stdout: {}",
        String::from_utf8_lossy(&text.stdout)
    );
    let text_stderr = String::from_utf8_lossy(&text.stderr);
    assert!(
        text_stderr.contains("no managed sessions found"),
        "text failure should explain missing sessions: {text_stderr}"
    );

    assert_eq!(
        json.status.code(),
        Some(1),
        "JSON resume failure must be non-zero"
    );
    assert!(
        json.stderr.is_empty(),
        "JSON resume failure should keep stderr empty: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let parsed: Value = serde_json::from_slice(&json.stdout)
        .expect("JSON resume failure should emit JSON to stdout");
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["action"], "restore");
    assert_eq!(parsed["error_kind"], "no_managed_sessions");
    assert!(
        !project_dir.join(".claw").exists(),
        "failed resume must not create .claw/session directories"
    );
}

#[test]
fn resumed_status_command_emits_structured_json_when_requested() {
    // given
    let temp_dir = unique_temp_dir("resume-status-json");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let config_home = temp_dir.join("config-home");
    fs::create_dir_all(&config_home).expect("isolated config home should exist");
    let session_path = temp_dir.join("session.jsonl");

    let mut session = workspace_session(&temp_dir);
    session
        .push_user_text("resume status json fixture")
        .expect("session write should succeed");
    session
        .save_to_path(&session_path)
        .expect("session should persist");

    // when
    // Use an isolated CLAW_CONFIG_HOME so ~/.claw/settings.json is not loaded,
    // which would cause loaded_config_files to be non-zero (#65).
    let output = run_claw_with_env(
        &temp_dir,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/status",
        ],
        &[("CLAW_CONFIG_HOME", config_home.to_str().expect("utf8 path"))],
    );

    // then
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let parsed: Value =
        serde_json::from_str(stdout.trim()).expect("resume status output should be json");
    assert_eq!(parsed["kind"], "status");
    // model is null in resume mode (not known without --model flag)
    assert!(parsed["model"].is_null());
    assert_eq!(parsed["permission_mode"], "workspace-write");
    assert_eq!(parsed["usage"]["messages"], 1);
    assert!(parsed["usage"]["turns"].is_number());
    assert!(parsed["workspace"]["cwd"].as_str().is_some());
    assert_eq!(
        parsed["workspace"]["session"],
        session_path.to_str().expect("utf8 path")
    );
    assert!(parsed["workspace"]["changed_files"].is_number());
    assert_eq!(parsed["workspace"]["loaded_config_files"].as_u64(), Some(0));
    assert!(parsed["sandbox"]["filesystem_mode"].as_str().is_some());
}

#[test]
fn resumed_status_surfaces_persisted_model() {
    // given — create a session with model already set
    let temp_dir = unique_temp_dir("resume-status-model");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let session_path = temp_dir.join("session.jsonl");

    let mut session = workspace_session(&temp_dir);
    session.model = Some("anthropic/claude-sonnet-4-6".to_string());
    session
        .push_user_text("model persistence fixture")
        .expect("write ok");
    session.save_to_path(&session_path).expect("persist ok");

    // when
    let output = run_claw(
        &temp_dir,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/status",
        ],
    );

    // then
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("should be json");
    assert_eq!(parsed["kind"], "status");
    assert_eq!(
        parsed["model"], "anthropic/claude-sonnet-4-6",
        "model should round-trip through session metadata"
    );
}

#[test]
fn resumed_sandbox_command_emits_structured_json_when_requested() {
    // given
    let temp_dir = unique_temp_dir("resume-sandbox-json");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let session_path = temp_dir.join("session.jsonl");

    workspace_session(&temp_dir)
        .save_to_path(&session_path)
        .expect("session should persist");

    // when
    let output = run_claw(
        &temp_dir,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/sandbox",
        ],
    );

    // then
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let parsed: Value =
        serde_json::from_str(stdout.trim()).expect("resume sandbox output should be json");
    assert_eq!(parsed["kind"], "sandbox");
    assert!(parsed["enabled"].is_boolean());
    assert!(parsed["active"].is_boolean());
    assert!(parsed["supported"].is_boolean());
    assert!(parsed["filesystem_mode"].as_str().is_some());
    assert!(parsed["allowed_mounts"].is_array());
    assert!(parsed["markers"].is_array());
}

#[test]
fn resumed_version_command_emits_structured_json() {
    let temp_dir = unique_temp_dir("resume-version-json");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let session_path = temp_dir.join("session.jsonl");
    workspace_session(&temp_dir)
        .save_to_path(&session_path)
        .expect("session should persist");

    let output = run_claw(
        &temp_dir,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/version",
        ],
    );

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("should be json");
    assert_eq!(parsed["kind"], "version");
    assert!(parsed["version"].as_str().is_some());
    assert!(parsed["git_sha"].as_str().is_some());
    assert!(parsed["target"].as_str().is_some());
    assert!(parsed["git_sha_short"].as_str().is_some());
    assert!(parsed.get("message").is_none());
    assert!(parsed["human_readable"].as_str().is_some());
}

#[test]
fn resumed_export_command_emits_structured_json() {
    let temp_dir = unique_temp_dir("resume-export-json");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let session_path = temp_dir.join("session.jsonl");
    let mut session = workspace_session(&temp_dir);
    session
        .push_user_text("export json fixture")
        .expect("write ok");
    session.save_to_path(&session_path).expect("persist ok");

    let output = run_claw(
        &temp_dir,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/export",
        ],
    );

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("should be json");
    assert_eq!(parsed["kind"], "export");
    assert!(parsed["file"].as_str().is_some());
    assert_eq!(parsed["message_count"], 1);
}

#[test]
fn resumed_help_command_emits_structured_json() {
    let temp_dir = unique_temp_dir("resume-help-json");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let session_path = temp_dir.join("session.jsonl");
    workspace_session(&temp_dir)
        .save_to_path(&session_path)
        .expect("persist ok");

    let output = run_claw(
        &temp_dir,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/help",
        ],
    );

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("should be json");
    assert_eq!(parsed["kind"], "help");
    // #338: resume help now uses 'message' field for parity with top-level help
    assert!(parsed["message"].as_str().is_some());
    let text = parsed["message"].as_str().unwrap();
    assert!(text.contains("/status"), "help text should list /status");
}

#[test]
fn resumed_no_command_emits_restored_json() {
    let temp_dir = unique_temp_dir("resume-no-cmd-json");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let session_path = temp_dir.join("session.jsonl");
    let mut session = workspace_session(&temp_dir);
    session
        .push_user_text("restored json fixture")
        .expect("write ok");
    session.save_to_path(&session_path).expect("persist ok");

    let output = run_claw(
        &temp_dir,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 path"),
        ],
    );

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("should be json");
    assert_eq!(parsed["kind"], "restored");
    assert!(parsed["session_id"].as_str().is_some());
    assert!(parsed["path"].as_str().is_some());
    assert_eq!(parsed["message_count"], 1);
}

#[test]
fn resumed_stub_command_emits_not_implemented_json() {
    let temp_dir = unique_temp_dir("resume-stub-json");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let session_path = temp_dir.join("session.jsonl");
    workspace_session(&temp_dir)
        .save_to_path(&session_path)
        .expect("persist ok");

    let output = run_claw(
        &temp_dir,
        &[
            "--output-format",
            "json",
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/allowed-tools",
        ],
    );

    // Stub commands exit with code 2
    assert!(!output.status.success());
    // #819/#820/#823: JSON abort envelopes route to stdout
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("should be json");
    assert_eq!(
        parsed["status"], "error",
        "stub command should emit status:error"
    );
    assert_eq!(
        parsed["kind"], "unsupported_command",
        "stub command should emit kind:unsupported_command"
    );
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("not yet implemented"),
        "error should say not yet implemented: {:?}",
        parsed["error"]
    );
}

fn run_claw(current_dir: &Path, args: &[&str]) -> Output {
    run_claw_with_env(current_dir, args, &[])
}

fn workspace_session(root: &Path) -> Session {
    Session::new().with_workspace_root(root.to_path_buf())
}

fn run_claw_with_env(current_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_claw"));
    command.current_dir(current_dir).args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("claw should launch")
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_millis();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "claw-{label}-{}-{millis}-{counter}",
        std::process::id()
    ))
}
