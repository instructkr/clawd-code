use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitTextOutput {
    #[serde(rename = "type")]
    pub kind: String,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub exit_code: Option<i32>,
}

fn git_gate_is_repo(workspace_root: &Path) -> io::Result<()> {
    let out = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !out.success() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a git work tree",
        ));
    }
    Ok(())
}

fn is_safe_git_rev_range(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.len() > 200 {
        return false;
    }
    t.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '_' | '-' | '^' | '~' | ':' | '@')
    })
}

fn read_pipe_capped(r: impl std::io::Read, cap: usize) -> io::Result<(Vec<u8>, bool)> {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut limited = r.take(u64::try_from(cap.saturating_add(1)).unwrap_or(u64::MAX));
    limited.read_to_end(&mut buf)?;
    let truncated = buf.len() > cap;
    if truncated {
        buf.truncate(cap);
    }
    Ok((buf, truncated))
}

fn run_git_capped(workspace_root: &Path, args: &[String], cap: usize) -> io::Result<GitTextOutput> {
    git_gate_is_repo(workspace_root)?;
    let mut child = Command::new("git")
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(workspace_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("git stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("git stderr unavailable"))?;

    let out_handle = std::thread::spawn(move || read_pipe_capped(stdout, cap));
    let err_handle = std::thread::spawn(move || read_pipe_capped(stderr, cap));

    let status = child.wait()?;
    let exit_code = status.code();
    let (out_bytes, out_trunc) = out_handle
        .join()
        .map_err(|_| io::Error::other("git stdout thread panicked"))??;
    let (err_bytes, err_trunc) = err_handle
        .join()
        .map_err(|_| io::Error::other("git stderr thread panicked"))??;

    Ok(GitTextOutput {
        kind: "git".to_string(),
        stdout: String::from_utf8_lossy(&out_bytes).into_owned(),
        stderr: String::from_utf8_lossy(&err_bytes).into_owned(),
        truncated: out_trunc || err_trunc,
        exit_code,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GitDiffOptions {
    pub cached: bool,
    pub rev_range: Option<String>,
    pub context_lines: Option<i64>,
    pub paths: Option<Vec<String>>,
}

pub fn git_diff_in_workspace(
    workspace_root: &Path,
    options: GitDiffOptions,
    max_bytes: usize,
) -> io::Result<GitTextOutput> {
    let mut args: Vec<String> = vec![
        "diff".to_string(),
        "--no-color".to_string(),
        "--no-ext-diff".to_string(),
    ];
    if let Some(n) = options.context_lines {
        let n = n.clamp(0, 100);
        args.push(format!("-U{n}"));
    }
    if options.cached {
        args.push("--cached".to_string());
    }
    if let Some(rr) = options.rev_range.as_deref() {
        if !rr.trim().is_empty() {
            if !is_safe_git_rev_range(rr) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid rev_range",
                ));
            }
            args.push(rr.trim().to_string());
        }
    }
    if let Some(paths) = options.paths {
        let mut cleaned = Vec::new();
        for p in paths {
            if p.trim().is_empty() {
                continue;
            }
            // paths are passed after `--`, so they are not parsed as flags; still enforce "relative-ish".
            if Path::new(&p).is_absolute() || p.contains("..") {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid path"));
            }
            cleaned.push(p.replace('\\', "/"));
        }
        if !cleaned.is_empty() {
            args.push("--".to_string());
            args.extend(cleaned);
        }
    }
    run_git_capped(workspace_root, &args, max_bytes)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GitLogOptions {
    pub max_count: Option<u64>,
    pub rev_range: Option<String>,
    pub paths: Option<Vec<String>>,
}

pub fn git_log_in_workspace(
    workspace_root: &Path,
    options: GitLogOptions,
    max_bytes: usize,
) -> io::Result<GitTextOutput> {
    let max_count = options.max_count.unwrap_or(20).min(50);
    let mut args: Vec<String> = vec![
        "log".to_string(),
        "--no-color".to_string(),
        "--no-decorate".to_string(),
        format!("--max-count={max_count}"),
        "--pretty=format:%h %s".to_string(),
    ];
    if let Some(rr) = options.rev_range.as_deref() {
        if !rr.trim().is_empty() {
            if !is_safe_git_rev_range(rr) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid rev_range",
                ));
            }
            args.push(rr.trim().to_string());
        }
    }
    if let Some(paths) = options.paths {
        let mut cleaned = Vec::new();
        for p in paths {
            if p.trim().is_empty() {
                continue;
            }
            if Path::new(&p).is_absolute() || p.contains("..") {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid path"));
            }
            cleaned.push(p.replace('\\', "/"));
        }
        if !cleaned.is_empty() {
            args.push("--".to_string());
            args.extend(cleaned);
        }
    }
    run_git_capped(workspace_root, &args, max_bytes)
}

#[cfg(test)]
mod tests {
    use super::{git_diff_in_workspace, git_log_in_workspace, GitDiffOptions, GitLogOptions};
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("runtime-git-tools-{label}-{nanos}"))
    }

    fn git(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git should run");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn diff_and_log_work() {
        let _guard = crate::test_env_lock();
        let root = temp_dir("basic");
        fs::create_dir_all(&root).expect("dir");

        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["config", "user.email", "tests@example.com"]);
        git(&root, &["config", "user.name", "Runtime Git Tools"]);
        fs::write(root.join("a.txt"), "a\n").expect("write");
        git(&root, &["add", "a.txt"]);
        git(&root, &["commit", "-m", "initial", "--quiet"]);
        fs::write(root.join("a.txt"), "a!\n").expect("modify");

        let log = git_log_in_workspace(
            &root,
            GitLogOptions {
                max_count: Some(5),
                ..Default::default()
            },
            64 * 1024,
        )
        .expect("log");
        assert!(log.stdout.contains("initial"));

        let diff =
            git_diff_in_workspace(&root, GitDiffOptions::default(), 64 * 1024).expect("diff");
        assert!(diff.stdout.contains("diff --git") || diff.stdout.contains("@@"));

        fs::remove_dir_all(&root).expect("cleanup");
    }
}
