#![allow(clippy::while_let_on_iterator)]

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mock_anthropic_service::{MockAnthropicService, SCENARIO_PREFIX};
use serde_json::Value;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn compact_flag_prints_only_final_assistant_text_without_tool_call_details() {
    // given a workspace pointed at the mock Anthropic service and a fixture file
    // that the read_file_roundtrip scenario will fetch through a tool call
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let base_url = server.base_url();

    let workspace = unique_temp_dir("compact-read-file");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");
    fs::write(workspace.join("fixture.txt"), "alpha parity line\n").expect("fixture should write");

    // when we run claw in compact text mode against a tool-using scenario
    let prompt = format!("{SCENARIO_PREFIX}read_file_roundtrip");
    let output = run_claw(
        &workspace,
        &config_home,
        &home,
        &base_url,
        &[
            "--model",
            "sonnet",
            "--permission-mode",
            "read-only",
            "--allowedTools",
            "read_file",
            "--compact",
            &prompt,
        ],
    );

    // then the command exits successfully and stdout contains exactly the final
    // assistant text with no tool call IDs, JSON envelopes, or spinner output
    assert!(
        output.status.success(),
        "compact run should succeed\nstdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let trimmed = stdout.trim_end_matches('\n');
    assert_eq!(
        trimmed, "read_file roundtrip complete: alpha parity line",
        "compact stdout should contain only the final assistant text"
    );
    assert!(
        !stdout.contains("toolu_"),
        "compact stdout must not leak tool_use_id ({stdout:?})"
    );
    assert!(
        !stdout.contains("\"tool_uses\""),
        "compact stdout must not leak json envelopes ({stdout:?})"
    );
    assert!(
        !stdout.contains("Thinking"),
        "compact stdout must not include the spinner banner ({stdout:?})"
    );

    fs::remove_dir_all(&workspace).expect("workspace cleanup should succeed");
}

#[test]
fn compact_flag_streaming_text_only_emits_final_message_text() {
    // given a workspace pointed at the mock Anthropic service running the
    // streaming_text scenario which only emits a single assistant text block
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let base_url = server.base_url();

    let workspace = unique_temp_dir("compact-streaming-text");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    // when we invoke claw with --compact for the streaming text scenario
    let prompt = format!("{SCENARIO_PREFIX}streaming_text");
    let output = run_claw(
        &workspace,
        &config_home,
        &home,
        &base_url,
        &[
            "--model",
            "sonnet",
            "--permission-mode",
            "read-only",
            "--compact",
            &prompt,
        ],
    );

    // then stdout should be exactly the assistant text followed by a newline
    assert!(
        output.status.success(),
        "compact streaming run should succeed\nstdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    assert_eq!(
        stdout, "Mock streaming says hello from the parity harness.\n",
        "compact streaming stdout should contain only the final assistant text"
    );

    fs::remove_dir_all(&workspace).expect("workspace cleanup should succeed");
}

#[test]
fn text_prompt_mode_prints_final_assistant_text_after_spinner() {
    // given a workspace pointed at the mock Anthropic service running the
    // streaming_text scenario which only emits a single assistant text block
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let base_url = server.base_url();

    let workspace = unique_temp_dir("text-prompt-mode");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    // when we invoke claw in normal text prompt mode for the streaming text scenario
    let prompt = format!("{SCENARIO_PREFIX}streaming_text");
    let output = run_claw(
        &workspace,
        &config_home,
        &home,
        &base_url,
        &[
            "--model",
            "sonnet",
            "--permission-mode",
            "read-only",
            &prompt,
        ],
    );

    // then stdout should contain the final assistant text, not just spinner output
    assert!(
        output.status.success(),
        "text prompt run should succeed\nstdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let plain_stdout = strip_ansi_codes(&stdout);
    assert!(
        plain_stdout.contains("Mock streaming says hello from the parity harness."),
        "text prompt stdout should include the assistant text ({stdout:?})"
    );
    assert!(
        plain_stdout.contains("✔ ✨ Done"),
        "text prompt stdout should still include spinner completion ({stdout:?})"
    );
    assert!(
        plain_stdout
            .lines()
            .any(|line| line == "Mock streaming says hello from the parity harness."),
        "text prompt stdout should print the assistant text as its own line ({stdout:?})"
    );

    fs::remove_dir_all(&workspace).expect("workspace cleanup should succeed");
}

#[test]
fn compact_flag_with_json_output_emits_structured_json() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let base_url = server.base_url();

    let workspace = unique_temp_dir("compact-json");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let prompt = format!("{SCENARIO_PREFIX}streaming_text");
    let output = run_claw(
        &workspace,
        &config_home,
        &home,
        &base_url,
        &[
            "--model",
            "sonnet",
            "--permission-mode",
            "read-only",
            "--output-format",
            "json",
            "--compact",
            &prompt,
        ],
    );

    assert!(
        output.status.success(),
        "compact json run should succeed
stdout:
{}

stderr:
{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let parsed: Value = serde_json::from_str(&stdout).expect("compact json stdout should parse");
    assert_eq!(
        parsed["message"],
        "Mock streaming says hello from the parity harness."
    );
    assert_eq!(parsed["compact"], true);
    assert_eq!(parsed["model"], "anthropic/claude-sonnet-4-6");
    assert!(parsed["usage"].is_object());

    fs::remove_dir_all(&workspace).expect("workspace cleanup should succeed");
}

#[test]
fn prompt_subcommand_reads_prompt_from_stdin_when_no_positional_arg_423() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let base_url = server.base_url();

    let workspace = unique_temp_dir("prompt-stdin-423");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let prompt = format!("{SCENARIO_PREFIX}streaming_text\n");
    let output = run_claw_with_stdin(
        &workspace,
        &config_home,
        &home,
        &base_url,
        &[
            "prompt",
            "--output-format",
            "json",
            "--compact",
            "--permission-mode",
            "read-only",
            "--model",
            "sonnet",
        ],
        &prompt,
    );

    assert!(
        output.status.success(),
        "prompt stdin run should succeed\nstdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("stdout should parse");
    assert_eq!(
        parsed["message"],
        "Mock streaming says hello from the parity harness."
    );
    let captured = runtime.block_on(server.captured_requests());
    assert!(
        captured
            .iter()
            .any(|request| request.raw_body.contains("PARITY_SCENARIO:streaming_text")),
        "stdin prompt should reach the provider request: {captured:?}"
    );

    fs::remove_dir_all(&workspace).expect("workspace cleanup should succeed");
}

#[test]
fn prompt_subcommand_stdin_flag_appends_pipe_context_423() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime should build");
    let server = runtime
        .block_on(MockAnthropicService::spawn())
        .expect("mock service should start");
    let base_url = server.base_url();

    let workspace = unique_temp_dir("prompt-stdin-flag-423");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let prompt_context = format!("{SCENARIO_PREFIX}streaming_text\n");
    let output = run_claw_with_stdin(
        &workspace,
        &config_home,
        &home,
        &base_url,
        &[
            "prompt",
            "Use stdin context",
            "--stdin",
            "--output-format",
            "json",
            "--compact",
            "--permission-mode",
            "read-only",
            "--model",
            "sonnet",
        ],
        &prompt_context,
    );

    assert!(
        output.status.success(),
        "prompt --stdin run should succeed\nstdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let captured = runtime.block_on(server.captured_requests());
    let provider_body = captured
        .iter()
        .find(|request| request.raw_body.contains("Use stdin context"))
        .expect("merged prompt should reach provider");
    assert!(
        provider_body
            .raw_body
            .contains("PARITY_SCENARIO:streaming_text"),
        "merged prompt should include stdin context: {provider_body:?}"
    );

    fs::remove_dir_all(&workspace).expect("workspace cleanup should succeed");
}

#[test]
fn compact_subcommand_json_fails_fast_when_stdin_closed() {
    let workspace = unique_temp_dir("compact-nontty-json");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let output = run_claw_closed_stdin_with_timeout(
        &workspace,
        &config_home,
        &home,
        &["compact", "--output-format", "json"],
        Duration::from_secs(2),
    );

    assert!(
        !output.status.success(),
        "compact json should fail non-zero"
    );
    // #819/#820/#823: JSON abort envelopes route to stdout
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    assert!(
        stderr.trim().is_empty() || !stderr.trim_start().starts_with('{'),
        "compact json should not emit JSON envelope to stderr (#819/#820/#823): {stderr}"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let parsed: Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be JSON error envelope");
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error_kind"], "interactive_only");
    assert_eq!(parsed["action"], "abort");
    assert!(
        parsed["message"]
            .as_str()
            .unwrap_or_default()
            .contains("claw compact"),
        "message should name compact: {parsed}"
    );
    // #749: hint must be non-empty (was null before fix — same class as #738/#745/#746)
    let hint = parsed["hint"].as_str().unwrap_or("");
    assert!(
        !hint.is_empty(),
        "compact interactive-only JSON must have non-empty hint (#749); got: {parsed}"
    );
    assert!(
        hint.contains("/compact") || hint.contains("--resume"),
        "hint should mention /compact or --resume: {hint}"
    );

    fs::remove_dir_all(&workspace).expect("workspace cleanup should succeed");
}

#[test]
fn compact_subcommand_text_fails_fast_when_stdin_closed() {
    let workspace = unique_temp_dir("compact-nontty-text");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let output = run_claw_closed_stdin_with_timeout(
        &workspace,
        &config_home,
        &home,
        &["compact"],
        Duration::from_secs(2),
    );

    assert!(
        !output.status.success(),
        "compact text should fail non-zero"
    );
    assert!(
        output.stdout.is_empty(),
        "compact text should not start a prompt/spinner on stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    assert!(
        stderr.contains("[error-kind: interactive_only]"),
        "{stderr}"
    );
    assert!(stderr.contains("claw compact"), "{stderr}");

    fs::remove_dir_all(&workspace).expect("workspace cleanup should succeed");
}

fn run_claw(
    cwd: &std::path::Path,
    config_home: &std::path::Path,
    home: &std::path::Path,
    base_url: &str,
    args: &[&str],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_claw"));
    command
        .current_dir(cwd)
        .env_clear()
        .env("ANTHROPIC_API_KEY", "test-compact-key")
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("CLAW_CONFIG_HOME", config_home)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .args(args);
    command.output().expect("claw should launch")
}

fn run_claw_with_stdin(
    cwd: &std::path::Path,
    config_home: &std::path::Path,
    home: &std::path::Path,
    base_url: &str,
    args: &[&str],
    stdin: &str,
) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_claw"))
        .current_dir(cwd)
        .env_clear()
        .env("ANTHROPIC_API_KEY", "test-compact-key")
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("CLAW_CONFIG_HOME", config_home)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args)
        .spawn()
        .expect("claw should launch");
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(stdin.as_bytes())
        .expect("stdin should write");
    child.stdin.take();
    child.wait_with_output().expect("output should collect")
}

fn run_claw_closed_stdin_with_timeout(
    cwd: &std::path::Path,
    config_home: &std::path::Path,
    home: &std::path::Path,
    args: &[&str],
    timeout: Duration,
) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_claw"))
        .current_dir(cwd)
        .env_clear()
        .env("CLAW_CONFIG_HOME", config_home)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args)
        .spawn()
        .expect("claw should launch");

    let start = Instant::now();
    loop {
        if child.try_wait().expect("try_wait should succeed").is_some() {
            return child.wait_with_output().expect("output should collect");
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .expect("killed output should collect");
            panic!(
                "claw did not exit within {:?}\nstdout:\n{}\nstderr:\n{}",
                timeout,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_millis();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "claw-compact-{label}-{}-{millis}-{counter}",
        std::process::id()
    ))
}

fn strip_ansi_codes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            chars.next();
            while let Some(next) = chars.next() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}
