use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::Session;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn status_command_applies_model_and_permission_mode_flags() {
    // given
    let temp_dir = unique_temp_dir("status-flags");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");

    // when
    let output = Command::new(env!("CARGO_BIN_EXE_claw"))
        .current_dir(&temp_dir)
        .args([
            "--model",
            "sonnet",
            "--permission-mode",
            "read-only",
            "status",
        ])
        .output()
        .expect("claw should launch");

    // then
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    assert!(stdout.contains("Status"));
    assert!(stdout.contains("Model            anthropic/claude-sonnet-4-6"));
    assert!(stdout.contains("Permission mode  read-only"));

    fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
}

#[test]
fn resume_flag_loads_a_saved_session_and_dispatches_status() {
    // given
    let temp_dir = unique_temp_dir("resume-status");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let session_path = write_session(&temp_dir, "resume-status");

    // when
    let output = Command::new(env!("CARGO_BIN_EXE_claw"))
        .current_dir(&temp_dir)
        .args([
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/status",
        ])
        .output()
        .expect("claw should launch");

    // then
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    assert!(stdout.contains("Status"));
    assert!(stdout.contains("Messages         1"));
    assert!(stdout.contains("Session          "));
    assert!(stdout.contains(session_path.to_str().expect("utf8 path")));

    fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
}

#[test]
fn slash_command_names_match_known_commands_and_suggest_nearby_unknown_ones() {
    // given
    let temp_dir = unique_temp_dir("slash-dispatch");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");

    // when
    let help_output = Command::new(env!("CARGO_BIN_EXE_claw"))
        .current_dir(&temp_dir)
        .arg("/help")
        .output()
        .expect("claw should launch");
    let unknown_output = Command::new(env!("CARGO_BIN_EXE_claw"))
        .current_dir(&temp_dir)
        .arg("/zstats")
        .output()
        .expect("claw should launch");

    // then
    assert_success(&help_output);
    let help_stdout = String::from_utf8(help_output.stdout).expect("stdout should be utf8");
    assert!(help_stdout.contains("Interactive slash commands:"));
    assert!(help_stdout.contains("/status"));

    assert!(
        !unknown_output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&unknown_output.stdout),
        String::from_utf8_lossy(&unknown_output.stderr)
    );
    let stderr = String::from_utf8(unknown_output.stderr).expect("stderr should be utf8");
    assert!(stderr.contains("unknown slash command outside the REPL: /zstats"));
    assert!(stderr.contains("Did you mean"));
    assert!(stderr.contains("/status"));

    fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
}

#[test]
fn omc_namespaced_slash_commands_surface_a_targeted_compatibility_hint() {
    let temp_dir = unique_temp_dir("slash-dispatch-omc");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");

    let output = Command::new(env!("CARGO_BIN_EXE_claw"))
        .current_dir(&temp_dir)
        .arg("/oh-my-claudecode:hud")
        .output()
        .expect("claw should launch");

    assert!(
        !output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    assert!(stderr.contains("unknown slash command outside the REPL: /oh-my-claudecode:hud"));
    assert!(stderr.contains("Claude Code/OMC plugin command"));
    assert!(stderr.contains("does not yet load plugin slash commands"));

    fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
}

#[test]
fn config_command_loads_defaults_from_standard_config_locations() {
    // given
    let temp_dir = unique_temp_dir("config-defaults");
    let config_home = temp_dir.join("home").join(".claw");
    fs::create_dir_all(temp_dir.join(".claw")).expect("project config dir should exist");
    fs::create_dir_all(&config_home).expect("home config dir should exist");

    fs::write(config_home.join("settings.json"), r#"{"model":"haiku"}"#)
        .expect("write user settings");
    fs::write(temp_dir.join(".claw.json"), r#"{"model":"sonnet"}"#)
        .expect("write project settings");
    fs::write(
        temp_dir.join(".claw").join("settings.local.json"),
        r#"{"model":"opus"}"#,
    )
    .expect("write local settings");
    let session_path = write_session(&temp_dir, "config-defaults");

    // when
    let output = command_in(&temp_dir)
        .env("CLAW_CONFIG_HOME", &config_home)
        .args([
            "--resume",
            session_path.to_str().expect("utf8 path"),
            "/config",
            "model",
        ])
        .output()
        .expect("claw should launch");

    // then
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    assert!(stdout.contains("Config"));
    assert!(stdout.contains("Loaded files      3"));
    assert!(stdout.contains("Merged section: model"));
    assert!(stdout.contains("opus"));
    assert!(stdout.contains(
        config_home
            .join("settings.json")
            .to_str()
            .expect("utf8 path")
    ));
    assert!(stdout.contains(temp_dir.join(".claw.json").to_str().expect("utf8 path")));
    assert!(stdout.contains(
        temp_dir
            .join(".claw")
            .join("settings.local.json")
            .to_str()
            .expect("utf8 path")
    ));

    fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
}

#[test]
fn doctor_command_runs_as_a_local_shell_entrypoint() {
    // given
    let temp_dir = unique_temp_dir("doctor-entrypoint");
    let config_home = temp_dir.join("home").join(".claw");
    fs::create_dir_all(&config_home).expect("config home should exist");

    // when
    let output = command_in(&temp_dir)
        .env("CLAW_CONFIG_HOME", &config_home)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")
        .arg("doctor")
        .output()
        .expect("claw doctor should launch");

    // then
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    assert!(stdout.contains("Doctor"));
    assert!(stdout.contains("Auth"));
    assert!(stdout.contains("Config"));
    assert!(stdout.contains("Workspace"));
    assert!(stdout.contains("Sandbox"));
    assert!(!stdout.contains("Thinking"));

    fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
}

#[test]
fn local_smoke_commands_do_not_require_live_credentials() {
    let temp_dir = unique_temp_dir("offline-local-smoke path with spaces");
    let config_home = temp_dir.join("home with spaces").join(".claw");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&temp_dir).expect("temp dir should exist");

    for args in [
        &["help"][..],
        &["status"][..],
        &["config", "env"][..],
        &["doctor"][..],
    ] {
        let output = offline_command_in(&temp_dir, &config_home)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("claw {args:?} should launch: {error}"));

        assert_success(&output);
        let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
        let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
        assert!(
            stdout.contains("claw")
                || stdout.contains("Status")
                || stdout.contains("Config")
                || stdout.contains("Doctor"),
            "unexpected stdout for {args:?}: {stdout}"
        );
        assert!(
            !stderr.contains("missing Anthropic credentials")
                && !stderr.contains("auth_unavailable"),
            "local smoke command {args:?} should not require live credentials: {stderr}"
        );
        assert!(
            !stdout.contains("Thinking"),
            "local smoke command {args:?} should not enter prompt runtime"
        );
    }

    fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
}

#[test]
fn local_subcommand_help_does_not_fall_through_to_runtime_or_provider_calls() {
    let temp_dir = unique_temp_dir("subcommand-help");
    let config_home = temp_dir.join("home").join(".claw");
    fs::create_dir_all(&config_home).expect("config home should exist");

    let doctor_help = command_in(&temp_dir)
        .env("CLAW_CONFIG_HOME", &config_home)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")
        .args(["doctor", "--help"])
        .output()
        .expect("doctor help should launch");
    let status_help = command_in(&temp_dir)
        .env("CLAW_CONFIG_HOME", &config_home)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")
        .args(["status", "--help"])
        .output()
        .expect("status help should launch");

    assert_success(&doctor_help);
    let doctor_stdout = String::from_utf8(doctor_help.stdout).expect("stdout should be utf8");
    assert!(doctor_stdout.contains("Usage            claw doctor"));
    assert!(doctor_stdout.contains("local-only health report"));
    assert!(!doctor_stdout.contains("Thinking"));

    assert_success(&status_help);
    let status_stdout = String::from_utf8(status_help.stdout).expect("stdout should be utf8");
    assert!(status_stdout.contains("Usage            claw status"));
    assert!(status_stdout.contains("local workspace snapshot"));
    assert!(!status_stdout.contains("Thinking"));

    let doctor_stderr = String::from_utf8(doctor_help.stderr).expect("stderr should be utf8");
    let status_stderr = String::from_utf8(status_help.stderr).expect("stderr should be utf8");
    assert!(!doctor_stderr.contains("auth_unavailable"));
    assert!(!status_stderr.contains("auth_unavailable"));

    fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
}

fn offline_command_in(cwd: &Path, config_home: &Path) -> Command {
    let mut command = command_in(cwd);
    command
        .env("CLAW_CONFIG_HOME", config_home)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("OPENAI_API_KEY")
        .env_remove("XAI_API_KEY")
        .env_remove("DASHSCOPE_API_KEY")
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9");
    command
}

fn command_in(cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_claw"));
    command.current_dir(cwd);
    command
}

fn write_session(root: &Path, label: &str) -> PathBuf {
    let session_path = root.join(format!("{label}.jsonl"));
    let mut session = Session::new().with_workspace_root(root.to_path_buf());
    session
        .push_user_text(format!("session fixture for {label}"))
        .expect("session write should succeed");
    session
        .save_to_path(&session_path)
        .expect("session should persist");
    session_path
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
        "claw-{label}-{}-{millis}-{counter}",
        std::process::id()
    ))
}
