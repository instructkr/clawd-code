use std::path::Path;
use std::process::Command;

/// A single git commit entry from the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommitEntry {
    pub hash: String,
    pub subject: String,
}

/// Git-aware context gathered at startup for injection into the system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitContext {
    pub branch: Option<String>,
    pub recent_commits: Vec<GitCommitEntry>,
    pub staged_files: Vec<String>,
}

const MAX_RECENT_COMMITS: usize = 5;

impl GitContext {
    /// Detect the git context from the given working directory.
    ///
    /// Returns `None` when the directory is not inside a git repository.
    #[must_use]
    pub fn detect(cwd: &Path) -> Option<Self> {
        // Quick gate: is this a git repo at all?
        let rev_parse = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(cwd)
            .output()
            .ok()?;
        if !rev_parse.status.success() {
            return None;
        }

        Some(Self {
            branch: read_branch(cwd),
            recent_commits: read_recent_commits(cwd),
            staged_files: read_staged_files(cwd),
        })
    }

    /// Render a human-readable summary suitable for system-prompt injection.
    #[must_use]
    pub fn render(&self) -> String {
        let mut lines = Vec::new();

        if let Some(branch) = &self.branch {
            lines.push(format!("Git branch: {branch}"));
        }

        if !self.recent_commits.is_empty() {
            lines.push(String::new());
            lines.push("Recent commits:".to_string());
            for entry in &self.recent_commits {
                lines.push(format!("  {} {}", entry.hash, entry.subject));
            }
        }

        if !self.staged_files.is_empty() {
            lines.push(String::new());
            lines.push("Staged files:".to_string());
            for file in &self.staged_files {
                lines.push(format!("  {file}"));
            }
        }

        lines.join("\n")
    }
}

fn read_branch(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8(output.stdout).ok()?;
    let trimmed = branch.trim();
    if trimmed.is_empty() || trimmed == "HEAD" {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn read_recent_commits(cwd: &Path) -> Vec<GitCommitEntry> {
    let output = Command::new("git")
        .args([
            "--no-optional-locks",
            "log",
            "--oneline",
            "-n",
            &MAX_RECENT_COMMITS.to_string(),
            "--no-decorate",
        ])
        .current_dir(cwd)
        .output()
        .ok();
    let Some(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8(output.stdout).unwrap_or_default();
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (hash, subject) = line.split_once(' ')?;
            Some(GitCommitEntry {
                hash: hash.to_string(),
                subject: subject.to_string(),
            })
        })
        .collect()
}

fn read_staged_files(cwd: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["--no-optional-locks", "diff", "--cached", "--name-only"])
        .current_dir(cwd)
        .output()
        .ok();
    let Some(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8(output.stdout).unwrap_or_default();
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{GitCommitEntry, GitContext};
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("runtime-git-context-{label}-{nanos}"))
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::test_env_lock()
    }

    fn ensure_valid_cwd() {
        if std::env::current_dir().is_err() {
            std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))
                .expect("test cwd should be recoverable");
        }
    }

    #[test]
    fn returns_none_for_non_git_directory() {
        // given
        let _guard = env_lock();
        ensure_valid_cwd();
        let root = temp_dir("non-git");
        fs::create_dir_all(&root).expect("create dir");

        // when
        let context = GitContext::detect(&root);

        // then
        assert!(context.is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn detects_branch_name_and_commits() {
        // given
        let _guard = env_lock();
        ensure_valid_cwd();
        let root = temp_dir("branch-commits");
        fs::create_dir_all(&root).expect("create dir");
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["config", "user.email", "tests@example.com"]);
        git(&root, &["config", "user.name", "Git Context Tests"]);
        fs::write(root.join("a.txt"), "a\n").expect("write a");
        git(&root, &["add", "a.txt"]);
        git(&root, &["commit", "-m", "first commit", "--quiet"]);
        fs::write(root.join("b.txt"), "b\n").expect("write b");
        git(&root, &["add", "b.txt"]);
        git(&root, &["commit", "-m", "second commit", "--quiet"]);

        // when
        let context = GitContext::detect(&root).expect("should detect git repo");

        // then
        assert_eq!(context.branch.as_deref(), Some("main"));
        assert_eq!(context.recent_commits.len(), 2);
        assert_eq!(context.recent_commits[0].subject, "second commit");
        assert_eq!(context.recent_commits[1].subject, "first commit");
        assert!(context.staged_files.is_empty());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn detects_staged_files() {
        // given
        let _guard = env_lock();
        ensure_valid_cwd();
        let root = temp_dir("staged");
        fs::create_dir_all(&root).expect("create dir");
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["config", "user.email", "tests@example.com"]);
        git(&root, &["config", "user.name", "Git Context Tests"]);
        fs::write(root.join("init.txt"), "init\n").expect("write init");
        git(&root, &["add", "init.txt"]);
        git(&root, &["commit", "-m", "initial", "--quiet"]);
        fs::write(root.join("staged.txt"), "staged\n").expect("write staged");
        git(&root, &["add", "staged.txt"]);

        // when
        let context = GitContext::detect(&root).expect("should detect git repo");

        // then
        assert_eq!(context.staged_files, vec!["staged.txt"]);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn render_formats_all_sections() {
        // given
        let context = GitContext {
            branch: Some("feat/test".to_string()),
            recent_commits: vec![
                GitCommitEntry {
                    hash: "abc1234".to_string(),
                    subject: "add feature".to_string(),
                },
                GitCommitEntry {
                    hash: "def5678".to_string(),
                    subject: "fix bug".to_string(),
                },
            ],
            staged_files: vec!["src/main.rs".to_string()],
        };

        // when
        let rendered = context.render();

        // then
        assert!(rendered.contains("Git branch: feat/test"));
        assert!(rendered.contains("abc1234 add feature"));
        assert!(rendered.contains("def5678 fix bug"));
        assert!(rendered.contains("src/main.rs"));
    }

    #[test]
    fn render_omits_empty_sections() {
        // given
        let context = GitContext {
            branch: Some("main".to_string()),
            recent_commits: Vec::new(),
            staged_files: Vec::new(),
        };

        // when
        let rendered = context.render();

        // then
        assert!(rendered.contains("Git branch: main"));
        assert!(!rendered.contains("Recent commits:"));
        assert!(!rendered.contains("Staged files:"));
    }

    #[test]
    fn limits_to_five_recent_commits() {
        // given
        let _guard = env_lock();
        ensure_valid_cwd();
        let root = temp_dir("five-commits");
        fs::create_dir_all(&root).expect("create dir");
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["config", "user.email", "tests@example.com"]);
        git(&root, &["config", "user.name", "Git Context Tests"]);
        for i in 1..=8 {
            let name = format!("file{i}.txt");
            fs::write(root.join(&name), format!("{i}\n")).expect("write file");
            git(&root, &["add", &name]);
            git(&root, &["commit", "-m", &format!("commit {i}"), "--quiet"]);
        }

        // when
        let context = GitContext::detect(&root).expect("should detect git repo");

        // then
        assert_eq!(context.recent_commits.len(), 5);
        assert_eq!(context.recent_commits[0].subject, "commit 8");
        assert_eq!(context.recent_commits[4].subject, "commit 4");
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn git(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|_| panic!("git {args:?} should run"))
            .status;
        assert!(status.success(), "git {args:?} failed");
    }
}
