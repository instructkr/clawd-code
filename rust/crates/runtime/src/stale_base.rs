#![allow(clippy::must_use_candidate)]
use std::path::Path;
use std::process::Command;

/// Outcome of comparing the worktree HEAD against the expected base commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseCommitState {
    /// HEAD matches the expected base commit.
    Matches,
    /// HEAD has diverged from the expected base.
    Diverged { expected: String, actual: String },
    /// No expected base was supplied (neither flag nor file).
    NoExpectedBase,
    /// The working directory is not inside a git repository.
    NotAGitRepo,
}

/// Where the expected base commit originated from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseCommitSource {
    Flag(String),
    File(String),
}

/// Read the `.claw-base` file from the given directory and return the trimmed
/// commit hash, or `None` when the file is absent or empty.
pub fn read_claw_base_file(cwd: &Path) -> Option<String> {
    let path = cwd.join(".claw-base");
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Resolve the expected base commit: prefer the `--base-commit` flag value,
/// fall back to reading `.claw-base` from `cwd`.
pub fn resolve_expected_base(flag_value: Option<&str>, cwd: &Path) -> Option<BaseCommitSource> {
    if let Some(value) = flag_value {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(BaseCommitSource::Flag(trimmed.to_string()));
        }
    }
    read_claw_base_file(cwd).map(BaseCommitSource::File)
}

/// Verify that the worktree HEAD matches `expected_base`.
///
/// Returns [`BaseCommitState::NoExpectedBase`] when no expected commit is
/// provided (the check is effectively a no-op in that case).
pub fn check_base_commit(cwd: &Path, expected_base: Option<&BaseCommitSource>) -> BaseCommitState {
    let Some(source) = expected_base else {
        return BaseCommitState::NoExpectedBase;
    };
    let expected_raw = match source {
        BaseCommitSource::Flag(value) | BaseCommitSource::File(value) => value.as_str(),
    };

    let Some(head_sha) = resolve_head_sha(cwd) else {
        return BaseCommitState::NotAGitRepo;
    };

    let Some(expected_sha) = resolve_rev(cwd, expected_raw) else {
        // If the expected ref cannot be resolved, compare raw strings as a
        // best-effort fallback (e.g. partial SHA provided by the caller).
        return if head_sha.starts_with(expected_raw) || expected_raw.starts_with(&head_sha) {
            BaseCommitState::Matches
        } else {
            BaseCommitState::Diverged {
                expected: expected_raw.to_string(),
                actual: head_sha,
            }
        };
    };

    if head_sha == expected_sha {
        BaseCommitState::Matches
    } else {
        BaseCommitState::Diverged {
            expected: expected_sha,
            actual: head_sha,
        }
    }
}

/// Format a human-readable warning when the base commit has diverged.
///
/// Returns `None` for non-warning states (`Matches`, `NoExpectedBase`).
pub fn format_stale_base_warning(state: &BaseCommitState) -> Option<String> {
    match state {
        BaseCommitState::Diverged { expected, actual } => Some(format!(
            "warning: worktree HEAD ({actual}) does not match expected base commit ({expected}). \
             Session may run against a stale codebase."
        )),
        BaseCommitState::NotAGitRepo => {
            Some("warning: stale-base check skipped — not inside a git repository.".to_string())
        }
        BaseCommitState::Matches | BaseCommitState::NoExpectedBase => None,
    }
}

fn resolve_head_sha(cwd: &Path) -> Option<String> {
    resolve_rev(cwd, "HEAD")
}

fn resolve_rev(cwd: &Path, rev: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?;
    let trimmed = sha.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("runtime-stale-base-{nanos}"))
    }

    fn init_repo(path: &std::path::Path) {
        fs::create_dir_all(path).expect("create repo dir");
        run(path, &["init", "--quiet", "-b", "main"]);
        run(path, &["config", "user.email", "tests@example.com"]);
        run(path, &["config", "user.name", "Stale Base Tests"]);
        fs::write(path.join("init.txt"), "initial\n").expect("write init file");
        run(path, &["add", "."]);
        run(path, &["commit", "-m", "initial commit", "--quiet"]);
    }

    fn run(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap_or_else(|e| panic!("git {} failed to execute: {e}", args.join(" ")));
        assert!(
            status.success(),
            "git {} exited with {status}",
            args.join(" ")
        );
    }

    fn commit_file(repo: &std::path::Path, name: &str, msg: &str) {
        fs::write(repo.join(name), format!("{msg}\n")).expect("write file");
        run(repo, &["add", name]);
        run(repo, &["commit", "-m", msg, "--quiet"]);
    }

    fn head_sha(repo: &std::path::Path) -> String {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .expect("git rev-parse HEAD");
        String::from_utf8(output.stdout)
            .expect("valid utf8")
            .trim()
            .to_string()
    }

    #[test]
    fn matches_when_head_equals_expected_base() {
        // given
        let root = temp_dir();
        init_repo(&root);
        let sha = head_sha(&root);
        let source = BaseCommitSource::Flag(sha);

        // when
        let state = check_base_commit(&root, Some(&source));

        // then
        assert_eq!(state, BaseCommitState::Matches);
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn diverged_when_head_moved_past_expected_base() {
        // given
        let root = temp_dir();
        init_repo(&root);
        let old_sha = head_sha(&root);
        commit_file(&root, "extra.txt", "move head forward");
        let new_sha = head_sha(&root);
        let source = BaseCommitSource::Flag(old_sha.clone());

        // when
        let state = check_base_commit(&root, Some(&source));

        // then
        assert_eq!(
            state,
            BaseCommitState::Diverged {
                expected: old_sha,
                actual: new_sha,
            }
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn no_expected_base_when_source_is_none() {
        // given
        let root = temp_dir();
        init_repo(&root);

        // when
        let state = check_base_commit(&root, None);

        // then
        assert_eq!(state, BaseCommitState::NoExpectedBase);
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn not_a_git_repo_when_outside_repo() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("create dir");
        let source = BaseCommitSource::Flag("abc1234".to_string());

        // when
        let state = check_base_commit(&root, Some(&source));

        // then
        assert_eq!(state, BaseCommitState::NotAGitRepo);
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn reads_claw_base_file() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("create dir");
        fs::write(root.join(".claw-base"), "abc1234def5678\n").expect("write .claw-base");

        // when
        let value = read_claw_base_file(&root);

        // then
        assert_eq!(value, Some("abc1234def5678".to_string()));
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn returns_none_for_missing_claw_base_file() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("create dir");

        // when
        let value = read_claw_base_file(&root);

        // then
        assert!(value.is_none());
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn returns_none_for_empty_claw_base_file() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("create dir");
        fs::write(root.join(".claw-base"), "  \n").expect("write empty .claw-base");

        // when
        let value = read_claw_base_file(&root);

        // then
        assert!(value.is_none());
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn resolve_expected_base_prefers_flag_over_file() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("create dir");
        fs::write(root.join(".claw-base"), "from_file\n").expect("write .claw-base");

        // when
        let source = resolve_expected_base(Some("from_flag"), &root);

        // then
        assert_eq!(
            source,
            Some(BaseCommitSource::Flag("from_flag".to_string()))
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn resolve_expected_base_falls_back_to_file() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("create dir");
        fs::write(root.join(".claw-base"), "from_file\n").expect("write .claw-base");

        // when
        let source = resolve_expected_base(None, &root);

        // then
        assert_eq!(
            source,
            Some(BaseCommitSource::File("from_file".to_string()))
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn resolve_expected_base_returns_none_when_nothing_available() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("create dir");

        // when
        let source = resolve_expected_base(None, &root);

        // then
        assert!(source.is_none());
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn format_warning_returns_message_for_diverged() {
        // given
        let state = BaseCommitState::Diverged {
            expected: "abc1234".to_string(),
            actual: "def5678".to_string(),
        };

        // when
        let warning = format_stale_base_warning(&state);

        // then
        let message = warning.expect("should produce warning");
        assert!(message.contains("abc1234"));
        assert!(message.contains("def5678"));
        assert!(message.contains("stale codebase"));
    }

    #[test]
    fn format_warning_returns_none_for_matches() {
        // given
        let state = BaseCommitState::Matches;

        // when
        let warning = format_stale_base_warning(&state);

        // then
        assert!(warning.is_none());
    }

    #[test]
    fn format_warning_returns_none_for_no_expected_base() {
        // given
        let state = BaseCommitState::NoExpectedBase;

        // when
        let warning = format_stale_base_warning(&state);

        // then
        assert!(warning.is_none());
    }

    #[test]
    fn matches_with_claw_base_file_in_real_repo() {
        // given
        let root = temp_dir();
        init_repo(&root);
        let sha = head_sha(&root);
        fs::write(root.join(".claw-base"), format!("{sha}\n")).expect("write .claw-base");
        let source = resolve_expected_base(None, &root);

        // when
        let state = check_base_commit(&root, source.as_ref());

        // then
        assert_eq!(state, BaseCommitState::Matches);
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn diverged_with_claw_base_file_after_new_commit() {
        // given
        let root = temp_dir();
        init_repo(&root);
        let old_sha = head_sha(&root);
        fs::write(root.join(".claw-base"), format!("{old_sha}\n")).expect("write .claw-base");
        commit_file(&root, "new.txt", "advance head");
        let new_sha = head_sha(&root);
        let source = resolve_expected_base(None, &root);

        // when
        let state = check_base_commit(&root, source.as_ref());

        // then
        assert_eq!(
            state,
            BaseCommitState::Diverged {
                expected: old_sha,
                actual: new_sha,
            }
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }
}
