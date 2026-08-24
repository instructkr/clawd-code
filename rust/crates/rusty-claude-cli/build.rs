use std::env;
use std::process::Command;

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn main() {
    let git_sha =
        command_output("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let git_sha_short = command_output("git", &["rev-parse", "--short=12", "HEAD"])
        .or_else(|| git_sha.get(..git_sha.len().min(12)).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    let git_dirty = command_output("git", &["status", "--porcelain"])
        .map(|status| (!status.trim().is_empty()).to_string())
        .unwrap_or_else(|| "false".to_string());
    let git_branch = command_output("git", &["branch", "--show-current"])
        .unwrap_or_else(|| "unknown".to_string());
    let git_commit_date = command_output("git", &["show", "-s", "--format=%cI", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    let git_commit_timestamp = command_output("git", &["show", "-s", "--format=%ct", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    let rustc_version =
        command_output("rustc", &["--version"]).unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_SHA={git_sha}");
    println!("cargo:rustc-env=GIT_SHA_SHORT={git_sha_short}");
    println!("cargo:rustc-env=GIT_DIRTY={git_dirty}");
    println!("cargo:rustc-env=GIT_BRANCH={git_branch}");
    println!("cargo:rustc-env=GIT_COMMIT_DATE={git_commit_date}");
    println!("cargo:rustc-env=GIT_COMMIT_TIMESTAMP={git_commit_timestamp}");
    println!("cargo:rustc-env=RUSTC_VERSION={rustc_version}");

    // TARGET is always set by Cargo during build.
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=TARGET={target}");

    // Build date from SOURCE_DATE_EPOCH (reproducible builds) or current UTC date.
    // Intentionally ignoring time component to keep output deterministic within a day.
    let build_date = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|epoch| epoch.parse::<i64>().ok())
        .map(|_ts| {
            // Use SOURCE_DATE_EPOCH to derive date via chrono if available;
            // for simplicity we just use the env var as a signal and fall back
            // to build-time env. In practice CI sets this via workflow.
            std::env::var("BUILD_DATE").unwrap_or_else(|_| "unknown".to_string())
        })
        .or_else(|| std::env::var("BUILD_DATE").ok())
        .unwrap_or_else(|| {
            command_output("date", &["+%Y-%m-%d"]).unwrap_or_else(|| "unknown".to_string())
        });
    println!("cargo:rustc-env=BUILD_DATE={build_date}");

    // Rerun if git state changes. Paths are relative to this package root.
    println!("cargo:rerun-if-changed=../../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../../.git/refs");
    println!("cargo:rerun-if-changed=../../../.git/index");
}
