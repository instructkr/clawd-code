#![allow(
    clippy::match_wildcard_for_single_variants,
    clippy::must_use_candidate,
    clippy::uninlined_format_args
)]
//! Permission enforcement layer that gates tool execution based on the
//! active `PermissionPolicy`.

use crate::permissions::{PermissionMode, PermissionOutcome, PermissionPolicy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome")]
pub enum EnforcementResult {
    /// Tool execution is allowed.
    Allowed,
    /// Tool execution was denied due to insufficient permissions.
    Denied {
        tool: String,
        active_mode: String,
        required_mode: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionEnforcer {
    policy: PermissionPolicy,
}

impl PermissionEnforcer {
    #[must_use]
    pub fn new(policy: PermissionPolicy) -> Self {
        Self { policy }
    }

    /// Check whether a tool can be executed under the current permission policy.
    /// Auto-denies when prompting is required but no prompter is provided.
    pub fn check(&self, tool_name: &str, input: &str) -> EnforcementResult {
        // When the active mode is Prompt, defer to the caller's interactive
        // prompt flow rather than hard-denying (the enforcer has no prompter).
        if self.policy.active_mode() == PermissionMode::Prompt {
            return EnforcementResult::Allowed;
        }

        let outcome = self.policy.authorize(tool_name, input, None);

        match outcome {
            PermissionOutcome::Allow => EnforcementResult::Allowed,
            PermissionOutcome::Deny { reason } => {
                let active_mode = self.policy.active_mode();
                let required_mode = self.policy.required_mode_for(tool_name);
                EnforcementResult::Denied {
                    tool: tool_name.to_owned(),
                    active_mode: active_mode.as_str().to_owned(),
                    required_mode: required_mode.as_str().to_owned(),
                    reason,
                }
            }
        }
    }

    #[must_use]
    pub fn is_allowed(&self, tool_name: &str, input: &str) -> bool {
        matches!(self.check(tool_name, input), EnforcementResult::Allowed)
    }

    /// Check permission with an explicitly provided required mode.
    /// Used when the required mode is determined dynamically (e.g., bash command classification).
    pub fn check_with_required_mode(
        &self,
        tool_name: &str,
        input: &str,
        required_mode: PermissionMode,
    ) -> EnforcementResult {
        // When the active mode is Prompt, defer to the caller's interactive
        // prompt flow rather than hard-denying.
        if self.policy.active_mode() == PermissionMode::Prompt {
            return EnforcementResult::Allowed;
        }

        let active_mode = self.policy.active_mode();

        // Check if active mode meets the dynamically determined required mode
        if active_mode >= required_mode {
            return EnforcementResult::Allowed;
        }

        // Permission denied - active mode is insufficient
        EnforcementResult::Denied {
            tool: tool_name.to_owned(),
            active_mode: active_mode.as_str().to_owned(),
            required_mode: required_mode.as_str().to_owned(),
            reason: format!(
                "'{tool_name}' with input '{input}' requires '{}' permission, but current mode is '{}'",
                required_mode.as_str(),
                active_mode.as_str()
            ),
        }
    }

    #[must_use]
    pub fn active_mode(&self) -> PermissionMode {
        self.policy.active_mode()
    }

    /// Classify a file operation against workspace boundaries.
    pub fn check_file_write(&self, path: &str, workspace_root: &str) -> EnforcementResult {
        let mode = self.policy.active_mode();

        match mode {
            PermissionMode::ReadOnly => EnforcementResult::Denied {
                tool: "write_file".to_owned(),
                active_mode: mode.as_str().to_owned(),
                required_mode: PermissionMode::WorkspaceWrite.as_str().to_owned(),
                reason: format!("file writes are not allowed in '{}' mode", mode.as_str()),
            },
            PermissionMode::WorkspaceWrite => {
                if is_within_workspace(path, workspace_root) {
                    EnforcementResult::Allowed
                } else {
                    EnforcementResult::Denied {
                        tool: "write_file".to_owned(),
                        active_mode: mode.as_str().to_owned(),
                        required_mode: PermissionMode::DangerFullAccess.as_str().to_owned(),
                        reason: format!(
                            "path '{}' is outside workspace root '{}'",
                            path, workspace_root
                        ),
                    }
                }
            }
            // Allow and DangerFullAccess permit all writes
            PermissionMode::Allow | PermissionMode::DangerFullAccess => EnforcementResult::Allowed,
            PermissionMode::Prompt => EnforcementResult::Denied {
                tool: "write_file".to_owned(),
                active_mode: mode.as_str().to_owned(),
                required_mode: PermissionMode::WorkspaceWrite.as_str().to_owned(),
                reason: "file write requires confirmation in prompt mode".to_owned(),
            },
        }
    }

    /// Check if a bash command should be allowed based on current mode.
    pub fn check_bash(&self, command: &str) -> EnforcementResult {
        let mode = self.policy.active_mode();

        match mode {
            PermissionMode::ReadOnly => {
                if is_read_only_command(command) {
                    EnforcementResult::Allowed
                } else {
                    EnforcementResult::Denied {
                        tool: "bash".to_owned(),
                        active_mode: mode.as_str().to_owned(),
                        required_mode: PermissionMode::WorkspaceWrite.as_str().to_owned(),
                        reason: format!(
                            "command may modify state; not allowed in '{}' mode",
                            mode.as_str()
                        ),
                    }
                }
            }
            PermissionMode::Prompt => EnforcementResult::Denied {
                tool: "bash".to_owned(),
                active_mode: mode.as_str().to_owned(),
                required_mode: PermissionMode::DangerFullAccess.as_str().to_owned(),
                reason: "bash requires confirmation in prompt mode".to_owned(),
            },
            // WorkspaceWrite, Allow, DangerFullAccess: permit bash
            _ => EnforcementResult::Allowed,
        }
    }
}

/// Workspace boundary check.
///
/// Resolves `.` and `..` components lexically *before* comparing against the
/// workspace root, so that traversal sequences like `/workspace/../../etc`
/// cannot escape the sandbox via a naive string prefix match. Normalization is
/// lexical (it does not touch the filesystem) because the target path may not
/// exist yet on a write, and we must not depend on CWD.
fn is_within_workspace(path: &str, workspace_root: &str) -> bool {
    let combined = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{workspace_root}/{path}")
    };

    let normalized = lexically_normalize(&combined);
    let root = lexically_normalize(workspace_root);
    let root_with_slash = if root.ends_with('/') {
        root.clone()
    } else {
        format!("{root}/")
    };

    normalized == root || normalized.starts_with(&root_with_slash)
}

/// Collapse `.` and `..` segments without consulting the filesystem.
/// `..` that would climb above an absolute root is clamped at `/`, so the
/// result can never be a prefix-match for a deeper workspace root.
fn lexically_normalize(path: &str) -> String {
    let is_absolute = path.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    let joined = stack.join("/");
    if is_absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Conservative heuristic: is this bash command read-only?
///
/// Hardening notes:
/// - Any shell metacharacter that could chain, substitute, pipe, or redirect
///   into a state-changing command rejects the whole line. This blocks
///   `cat x; rm -rf y`, `cat x | sh`, `$(...)`, backticks, redirects, and
///   subshells regardless of the leading token.
/// - Language interpreters (`python`, `node`, `ruby`) and build drivers
///   (`cargo`, `rustc`) are NOT read-only: they execute arbitrary code, so they
///   are excluded from the allow-list.
/// - `git` is allowed only for a known set of non-mutating subcommands.
/// - `find` is rejected when it carries an action that can execute or delete.
///
/// Residual known gaps (documented, not yet closed): `sed`'s `w`/`e` script
/// commands and `awk`'s `system()` can still mutate — these require quoting or
/// metacharacters that the checks above usually catch, but a dedicated parser
/// would be more robust. Tracked as follow-up.
fn is_read_only_command(command: &str) -> bool {
    // Shell metacharacters that enable command chaining, substitution,
    // piping, redirection, or subshells. Presence of any of these means we
    // cannot reason about the command from its leading token alone.
    const SHELL_METACHARS: &[char] = &[';', '|', '&', '$', '`', '>', '<', '(', ')', '{', '}', '\n'];
    if command.contains(SHELL_METACHARS) {
        return false;
    }

    let mut tokens = command.split_whitespace();
    let first_token = tokens.next().unwrap_or("").rsplit('/').next().unwrap_or("");

    // `git` is only read-only for a curated set of subcommands.
    if first_token == "git" {
        let subcommand = tokens.next().unwrap_or("");
        return matches!(
            subcommand,
            "status"
                | "log"
                | "diff"
                | "show"
                | "branch"
                | "rev-parse"
                | "ls-files"
                | "blame"
                | "describe"
                | "tag"
                | "remote"
        );
    }

    // `find` can execute or delete via actions; reject those forms.
    if first_token == "find"
        && (command.contains("-exec")
            || command.contains("-execdir")
            || command.contains("-delete")
            || command.contains("-ok")
            || command.contains("-fprintf"))
    {
        return false;
    }

    matches!(
        first_token,
        "cat"
            | "head"
            | "tail"
            | "less"
            | "more"
            | "wc"
            | "ls"
            | "find"
            | "grep"
            | "rg"
            | "awk"
            | "sed"
            | "echo"
            | "printf"
            | "which"
            | "where"
            | "whoami"
            | "pwd"
            | "env"
            | "printenv"
            | "date"
            | "cal"
            | "df"
            | "du"
            | "free"
            | "uptime"
            | "uname"
            | "file"
            | "stat"
            | "diff"
            | "sort"
            | "uniq"
            | "tr"
            | "cut"
            | "paste"
            | "test"
            | "true"
            | "false"
            | "type"
            | "readlink"
            | "realpath"
            | "basename"
            | "dirname"
            | "sha256sum"
            | "md5sum"
            | "b3sum"
            | "xxd"
            | "hexdump"
            | "od"
            | "strings"
            | "tree"
            | "jq"
            | "yq"
    ) && !command.contains("-i ")
        && !command.contains("--in-place")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_enforcer(mode: PermissionMode) -> PermissionEnforcer {
        let policy = PermissionPolicy::new(mode);
        PermissionEnforcer::new(policy)
    }

    #[test]
    fn allow_mode_permits_everything() {
        let enforcer = make_enforcer(PermissionMode::Allow);
        assert!(enforcer.is_allowed("bash", ""));
        assert!(enforcer.is_allowed("write_file", ""));
        assert!(enforcer.is_allowed("edit_file", ""));
        assert_eq!(
            enforcer.check_file_write("/outside/path", "/workspace"),
            EnforcementResult::Allowed
        );
        assert_eq!(enforcer.check_bash("rm -rf /"), EnforcementResult::Allowed);
    }

    #[test]
    fn read_only_denies_writes() {
        let policy = PermissionPolicy::new(PermissionMode::ReadOnly)
            .with_tool_requirement("read_file", PermissionMode::ReadOnly)
            .with_tool_requirement("grep_search", PermissionMode::ReadOnly)
            .with_tool_requirement("write_file", PermissionMode::WorkspaceWrite);

        let enforcer = PermissionEnforcer::new(policy);
        assert!(enforcer.is_allowed("read_file", ""));
        assert!(enforcer.is_allowed("grep_search", ""));

        // write_file requires WorkspaceWrite but we're in ReadOnly
        let result = enforcer.check("write_file", "");
        assert!(matches!(result, EnforcementResult::Denied { .. }));

        let result = enforcer.check_file_write("/workspace/file.rs", "/workspace");
        assert!(matches!(result, EnforcementResult::Denied { .. }));
    }

    #[test]
    fn read_only_allows_read_commands() {
        let enforcer = make_enforcer(PermissionMode::ReadOnly);
        assert_eq!(
            enforcer.check_bash("cat src/main.rs"),
            EnforcementResult::Allowed
        );
        assert_eq!(
            enforcer.check_bash("grep -r 'pattern' ."),
            EnforcementResult::Allowed
        );
        assert_eq!(enforcer.check_bash("ls -la"), EnforcementResult::Allowed);
    }

    #[test]
    fn read_only_denies_write_commands() {
        let enforcer = make_enforcer(PermissionMode::ReadOnly);
        let result = enforcer.check_bash("rm file.txt");
        assert!(matches!(result, EnforcementResult::Denied { .. }));
    }

    #[test]
    fn workspace_write_allows_within_workspace() {
        let enforcer = make_enforcer(PermissionMode::WorkspaceWrite);
        let result = enforcer.check_file_write("/workspace/src/main.rs", "/workspace");
        assert_eq!(result, EnforcementResult::Allowed);
    }

    #[test]
    fn workspace_write_denies_outside_workspace() {
        let enforcer = make_enforcer(PermissionMode::WorkspaceWrite);
        let result = enforcer.check_file_write("/etc/passwd", "/workspace");
        assert!(matches!(result, EnforcementResult::Denied { .. }));
    }

    #[test]
    fn prompt_mode_denies_without_prompter() {
        let enforcer = make_enforcer(PermissionMode::Prompt);
        let result = enforcer.check_bash("echo test");
        assert!(matches!(result, EnforcementResult::Denied { .. }));

        let result = enforcer.check_file_write("/workspace/file.rs", "/workspace");
        assert!(matches!(result, EnforcementResult::Denied { .. }));
    }

    #[test]
    fn workspace_boundary_check() {
        assert!(is_within_workspace("/workspace/src/main.rs", "/workspace"));
        assert!(is_within_workspace("/workspace", "/workspace"));
        assert!(!is_within_workspace("/etc/passwd", "/workspace"));
        assert!(!is_within_workspace("/workspacex/hack", "/workspace"));
    }

    #[test]
    fn read_only_command_heuristic() {
        assert!(is_read_only_command("cat file.txt"));
        assert!(is_read_only_command("grep pattern file"));
        assert!(is_read_only_command("git log --oneline"));
        assert!(!is_read_only_command("rm file.txt"));
        assert!(!is_read_only_command("echo test > file.txt"));
        assert!(!is_read_only_command("sed -i 's/a/b/' file"));
    }

    // --- Hardening regression tests (#2: read-only bypasses) ---

    #[test]
    fn read_only_rejects_command_chaining() {
        // A leading read-only token must not launder a trailing destructive one.
        assert!(!is_read_only_command("cat foo; rm -rf bar"));
        assert!(!is_read_only_command("cat foo && rm -rf bar"));
        assert!(!is_read_only_command("ls || rm bar"));
        assert!(!is_read_only_command("cat foo | sh"));
        assert!(!is_read_only_command("echo `rm bar`"));
        assert!(!is_read_only_command("echo $(rm bar)"));
        assert!(!is_read_only_command("echo x>file")); // redirect without spaces
    }

    #[test]
    fn read_only_rejects_interpreters_and_build_drivers() {
        // These execute arbitrary code and are no longer read-only.
        assert!(!is_read_only_command(
            "python3 -c \"import os; os.system('rm -rf .')\""
        ));
        assert!(!is_read_only_command("python script.py"));
        assert!(!is_read_only_command("node app.js"));
        assert!(!is_read_only_command("ruby x.rb"));
        assert!(!is_read_only_command("cargo run"));
        assert!(!is_read_only_command("rustc evil.rs"));
    }

    #[test]
    fn read_only_gates_git_subcommands() {
        // Read-only git subcommands remain allowed...
        assert!(is_read_only_command("git status"));
        assert!(is_read_only_command("git diff HEAD~1"));
        assert!(is_read_only_command("git show abc123"));
        // ...but mutating/exfiltrating ones are rejected.
        assert!(!is_read_only_command("git commit -m x"));
        assert!(!is_read_only_command("git push origin main"));
        assert!(!is_read_only_command("git reset --hard"));
        assert!(!is_read_only_command("git clean -fd"));
        assert!(!is_read_only_command("git config user.email a@b.c"));
    }

    #[test]
    fn read_only_rejects_find_actions() {
        assert!(is_read_only_command("find . -name Cargo.toml"));
        assert!(!is_read_only_command("find . -delete"));
        // -exec uses braces/semicolon which also trip the metachar guard,
        // but the explicit action check is the primary defense.
        assert!(!is_read_only_command("find . -execdir rm rf"));
    }

    // --- Hardening regression tests (#1: workspace path traversal) ---

    #[test]
    fn workspace_rejects_parent_traversal() {
        assert!(!is_within_workspace(
            "/workspace/../etc/passwd",
            "/workspace"
        ));
        assert!(!is_within_workspace(
            "/workspace/../../etc/crontab",
            "/workspace"
        ));
        assert!(!is_within_workspace("../etc/passwd", "/workspace"));
        assert!(!is_within_workspace(
            "/workspace/sub/../../outside",
            "/workspace"
        ));
        // Legitimate paths still resolve inside.
        assert!(is_within_workspace(
            "/workspace/./src/main.rs",
            "/workspace"
        ));
        assert!(is_within_workspace(
            "/workspace/src/../src/main.rs",
            "/workspace"
        ));
    }

    #[test]
    fn workspace_write_denies_traversal_escape() {
        let enforcer = make_enforcer(PermissionMode::WorkspaceWrite);
        let result = enforcer.check_file_write("/workspace/../../etc/crontab", "/workspace");
        assert!(matches!(result, EnforcementResult::Denied { .. }));
    }

    #[test]
    fn active_mode_returns_policy_mode() {
        // given
        let modes = [
            PermissionMode::ReadOnly,
            PermissionMode::WorkspaceWrite,
            PermissionMode::DangerFullAccess,
            PermissionMode::Prompt,
            PermissionMode::Allow,
        ];

        // when
        let active_modes: Vec<_> = modes
            .into_iter()
            .map(|mode| make_enforcer(mode).active_mode())
            .collect();

        // then
        assert_eq!(active_modes, modes);
    }

    #[test]
    fn danger_full_access_permits_file_writes_and_bash() {
        // given
        let enforcer = make_enforcer(PermissionMode::DangerFullAccess);

        // when
        let file_result = enforcer.check_file_write("/outside/workspace/file.txt", "/workspace");
        let bash_result = enforcer.check_bash("rm -rf /tmp/scratch");

        // then
        assert_eq!(file_result, EnforcementResult::Allowed);
        assert_eq!(bash_result, EnforcementResult::Allowed);
    }

    #[test]
    fn check_denied_payload_contains_tool_and_modes() {
        // given
        let policy = PermissionPolicy::new(PermissionMode::ReadOnly)
            .with_tool_requirement("write_file", PermissionMode::WorkspaceWrite);
        let enforcer = PermissionEnforcer::new(policy);

        // when
        let result = enforcer.check("write_file", "{}");

        // then
        match result {
            EnforcementResult::Denied {
                tool,
                active_mode,
                required_mode,
                reason,
            } => {
                assert_eq!(tool, "write_file");
                assert_eq!(active_mode, "read-only");
                assert_eq!(required_mode, "workspace-write");
                assert!(reason.contains("requires workspace-write permission"));
            }
            other => panic!("expected denied result, got {other:?}"),
        }
    }

    #[test]
    fn workspace_write_relative_path_resolved() {
        // given
        let enforcer = make_enforcer(PermissionMode::WorkspaceWrite);

        // when
        let result = enforcer.check_file_write("src/main.rs", "/workspace");

        // then
        assert_eq!(result, EnforcementResult::Allowed);
    }

    #[test]
    fn workspace_root_with_trailing_slash() {
        // given
        let enforcer = make_enforcer(PermissionMode::WorkspaceWrite);

        // when
        let result = enforcer.check_file_write("/workspace/src/main.rs", "/workspace/");

        // then
        assert_eq!(result, EnforcementResult::Allowed);
    }

    #[test]
    fn workspace_root_equality() {
        // given
        let root = "/workspace/";

        // when
        let equal_to_root = is_within_workspace("/workspace", root);

        // then
        assert!(equal_to_root);
    }

    #[test]
    fn bash_heuristic_full_path_prefix() {
        // given
        let full_path_command = "/usr/bin/cat Cargo.toml";
        let git_path_command = "/usr/local/bin/git status";

        // when
        let cat_result = is_read_only_command(full_path_command);
        let git_result = is_read_only_command(git_path_command);

        // then
        assert!(cat_result);
        assert!(git_result);
    }

    #[test]
    fn bash_heuristic_redirects_block_read_only_commands() {
        // given
        let overwrite = "cat Cargo.toml > out.txt";
        let append = "echo test >> out.txt";

        // when
        let overwrite_result = is_read_only_command(overwrite);
        let append_result = is_read_only_command(append);

        // then
        assert!(!overwrite_result);
        assert!(!append_result);
    }

    #[test]
    fn bash_heuristic_in_place_flag_blocks() {
        // given
        let interactive_python = "python -i script.py";
        let in_place_sed = "sed --in-place 's/a/b/' file.txt";

        // when
        let interactive_result = is_read_only_command(interactive_python);
        let in_place_result = is_read_only_command(in_place_sed);

        // then
        assert!(!interactive_result);
        assert!(!in_place_result);
    }

    #[test]
    fn bash_heuristic_empty_command() {
        // given
        let empty = "";
        let whitespace = "   ";

        // when
        let empty_result = is_read_only_command(empty);
        let whitespace_result = is_read_only_command(whitespace);

        // then
        assert!(!empty_result);
        assert!(!whitespace_result);
    }

    #[test]
    fn prompt_mode_check_bash_denied_payload_fields() {
        // given
        let enforcer = make_enforcer(PermissionMode::Prompt);

        // when
        let result = enforcer.check_bash("git status");

        // then
        match result {
            EnforcementResult::Denied {
                tool,
                active_mode,
                required_mode,
                reason,
            } => {
                assert_eq!(tool, "bash");
                assert_eq!(active_mode, "prompt");
                assert_eq!(required_mode, "danger-full-access");
                assert_eq!(reason, "bash requires confirmation in prompt mode");
            }
            other => panic!("expected denied result, got {other:?}"),
        }
    }

    #[test]
    fn read_only_check_file_write_denied_payload() {
        // given
        let enforcer = make_enforcer(PermissionMode::ReadOnly);

        // when
        let result = enforcer.check_file_write("/workspace/file.txt", "/workspace");

        // then
        match result {
            EnforcementResult::Denied {
                tool,
                active_mode,
                required_mode,
                reason,
            } => {
                assert_eq!(tool, "write_file");
                assert_eq!(active_mode, "read-only");
                assert_eq!(required_mode, "workspace-write");
                assert!(reason.contains("file writes are not allowed"));
            }
            other => panic!("expected denied result, got {other:?}"),
        }
    }
}
