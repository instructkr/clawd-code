use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn compact_slash_command_in_repl_does_not_start_nested_tokio_runtime() {
    // given
    let workspace = unique_temp_dir("compact-repl-panic");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    // when
    let output = run_claw_repl(&workspace, &config_home, &home, "/compact\n/exit\n");

    // then
    assert!(
        output.status.success(),
        "compact repl run should succeed\nstdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    assert!(
        !stderr.contains("Cannot start a runtime"),
        "stderr must not contain nested runtime panic: {stderr:?}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "stderr must not contain panic output: {stderr:?}"
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let plain_stdout = strip_ansi_codes(&stdout);
    assert!(
        plain_stdout.contains("Compaction skipped")
            || plain_stdout.contains("Result           skipped")
            || plain_stdout.contains("Result           compacted"),
        "stdout should contain compact report output ({stdout:?})"
    );

    fs::remove_dir_all(&workspace).expect("workspace cleanup should succeed");
}

fn run_claw_repl(
    cwd: &std::path::Path,
    config_home: &std::path::Path,
    home: &std::path::Path,
    stdin: &str,
) -> Output {
    let mut command = python_pty_command(env!("CARGO_BIN_EXE_claw"));
    let mut child = command
        .current_dir(cwd)
        .env_clear()
        .env("ANTHROPIC_API_KEY", "test-compact-repl-key")
        .env("CLAW_CONFIG_HOME", config_home)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
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
}

fn python_pty_command(claw: &str) -> Command {
    let mut command = Command::new("python3");
    command.args([
        "-c",
        r#"
import os
import pty
import subprocess
import sys

claw = sys.argv[1]
payload = sys.stdin.buffer.read()
master, slave = pty.openpty()
child = subprocess.Popen([claw], stdin=slave, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
os.close(slave)
os.write(master, payload)
stdout, stderr = child.communicate(timeout=30)
os.close(master)
sys.stdout.buffer.write(stdout)
sys.stderr.buffer.write(stderr)
raise SystemExit(child.returncode)
"#,
        claw,
    ]);
    command
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

fn strip_ansi_codes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            chars.next();
            for next in chars.by_ref() {
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
