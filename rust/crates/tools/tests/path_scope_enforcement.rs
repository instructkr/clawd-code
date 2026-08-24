use runtime::{permission_enforcer::PermissionEnforcer, PermissionMode, PermissionPolicy};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tools::{mvp_tool_specs, GlobalToolRegistry};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn temp_path(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    std::env::temp_dir().join(format!("claw-path-scope-{unique}-{name}"))
}

fn workspace_write_registry() -> GlobalToolRegistry {
    let policy = mvp_tool_specs().into_iter().fold(
        PermissionPolicy::new(PermissionMode::WorkspaceWrite),
        |policy, spec| policy.with_tool_requirement(spec.name, spec.required_permission),
    );
    GlobalToolRegistry::builtin().with_enforcer(PermissionEnforcer::new(policy))
}

fn run_bash(command: &str) -> Result<String, String> {
    workspace_write_registry().execute("bash", &json!({ "command": command }))
}

fn run_powershell(command: &str) -> Result<String, String> {
    workspace_write_registry().execute("PowerShell", &json!({ "command": command }))
}

fn run_read_file(path: &Path) -> Result<String, String> {
    workspace_write_registry().execute("read_file", &json!({ "path": path.display().to_string() }))
}

fn assert_permission_denied(result: Result<String, String>, case_name: &str) {
    let err = result
        .unwrap_err_or_else(|ok| panic!("{case_name} should be denied before execution, got {ok}"));
    assert!(
        (err.contains("requires danger-full-access permission")
            || err.contains("requires \'danger-full-access\' permission"))
            || err.contains("current mode is workspace-write")
            || err.contains("escapes workspace"),
        "{case_name} should fail in permission enforcement, got: {err}"
    );
}

trait UnwrapErrOrElse<T, E> {
    fn unwrap_err_or_else<F: FnOnce(T) -> E>(self, op: F) -> E;
}

impl<T, E> UnwrapErrOrElse<T, E> for Result<T, E> {
    fn unwrap_err_or_else<F: FnOnce(T) -> E>(self, op: F) -> E {
        match self {
            Ok(value) => op(value),
            Err(error) => error,
        }
    }
}

fn with_cwd<T>(cwd: &Path, f: impl FnOnce() -> T) -> T {
    let previous = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(cwd).expect("set cwd");
    let result = f();
    std::env::set_current_dir(previous).expect("restore cwd");
    result
}

#[test]
fn direct_paths_allow_workspace_file_and_deny_absolute_outside_file() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = temp_path("direct");
    fs::create_dir_all(root.join("src")).expect("create workspace");
    fs::write(root.join("src/lib.rs"), "workspace\n").expect("write workspace file");
    let outside = temp_path("direct-outside.txt");
    fs::write(&outside, "secret\n").expect("write outside file");

    with_cwd(&root, || {
        let allowed = run_bash("cat src/lib.rs").expect("workspace-relative read should execute");
        assert!(allowed.contains("workspace"));
        assert_permission_denied(
            run_bash(&format!("cat {}", outside.display())),
            "absolute outside file",
        );
    });

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_file(outside);
}

#[test]
fn file_tool_direct_outside_path_is_denied_before_reading() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = temp_path("file-tool-direct");
    fs::create_dir_all(&root).expect("create workspace");
    let outside = temp_path("file-tool-secret.txt");
    fs::write(&outside, "secret\n").expect("write outside file");

    with_cwd(&root, || {
        assert_permission_denied(run_read_file(&outside), "read_file outside workspace");
    });

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_file(outside);
}

#[cfg(unix)]
#[test]
fn symlink_resolving_outside_workspace_is_denied_before_execution() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = temp_path("symlink");
    fs::create_dir_all(&root).expect("create workspace");
    let outside = temp_path("symlink-secret.txt");
    fs::write(&outside, "secret\n").expect("write outside file");
    std::os::unix::fs::symlink(&outside, root.join("secret-link")).expect("create symlink");

    with_cwd(&root, || {
        assert_permission_denied(run_bash("cat secret-link"), "outside symlink");
    });

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_file(outside);
}

#[test]
fn shell_expansion_and_glob_parent_traversal_are_denied_before_execution() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = temp_path("expansion");
    fs::create_dir_all(&root).expect("create workspace");

    with_cwd(&root, || {
        for (name, command) in [
            ("parent glob", "ls ../*"),
            ("PWD parent expansion", "cat $PWD/../secret.txt"),
            ("braced PWD parent expansion", "cat ${PWD}/../secret.txt"),
            (
                "command substitution parent expansion",
                "cat $(pwd)/../secret.txt",
            ),
        ] {
            assert_permission_denied(run_bash(command), name);
        }
    });

    let _ = fs::remove_dir_all(root);
}

#[test]
fn nested_worktree_paths_are_allowed_but_parent_escape_is_denied() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = temp_path("worktree");
    let worktree = root.join("main").join("linked-worktree");
    fs::create_dir_all(worktree.join("src")).expect("create worktree");
    fs::write(worktree.join("src/lib.rs"), "worktree\n").expect("write worktree file");

    with_cwd(&worktree, || {
        let allowed =
            run_bash("cat src/lib.rs").expect("nested worktree-relative read should execute");
        assert!(allowed.contains("worktree"));
        assert_permission_denied(run_bash("cat ../../outside.txt"), "worktree parent escape");
    });

    let _ = fs::remove_dir_all(root);
}

#[test]
fn windows_style_absolute_paths_are_denied_before_execution() {
    for (name, command) in [
        (
            "windows drive backslash",
            r"cat C:\Users\attacker\secret.txt",
        ),
        ("windows drive slash", r"cat C:/Users/attacker/secret.txt"),
    ] {
        assert_permission_denied(run_bash(command), name);
    }

    for (name, command) in [
        (
            "powershell windows drive backslash",
            r"Get-Content -Path C:\Users\attacker\secret.txt",
        ),
        (
            "powershell windows drive slash",
            r"Get-Content -Path C:/Users/attacker/secret.txt",
        ),
    ] {
        assert_permission_denied(run_powershell(command), name);
    }
}
