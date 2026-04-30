//! Tests for the LSP client registry: extension mapping, server lifecycle,
//! and diagnostics edge cases.

use super::*;
use super::types::*;

#[test]
fn find_server_for_all_extensions() {
    // given
    let registry = LspRegistry::new();
    for language in [
        "rust",
        "typescript",
        "javascript",
        "python",
        "go",
        "java",
        "c",
        "cpp",
        "ruby",
        "lua",
    ] {
        registry.register(language, LspServerStatus::Connected, None, vec![]);
    }
    let cases = [
        ("src/main.rs", "rust"),
        ("src/index.ts", "typescript"),
        ("src/view.tsx", "typescript"),
        ("src/app.js", "javascript"),
        ("src/app.jsx", "javascript"),
        ("script.py", "python"),
        ("main.go", "go"),
        ("Main.java", "java"),
        ("native.c", "c"),
        ("native.h", "c"),
        ("native.cpp", "cpp"),
        ("native.hpp", "cpp"),
        ("native.cc", "cpp"),
        ("script.rb", "ruby"),
        ("script.lua", "lua"),
    ];

    // when
    let resolved: Vec<_> = cases
        .into_iter()
        .map(|(path, expected)| {
            (
                path,
                registry
                    .find_server_for_path(path)
                    .map(|server| server.language),
                expected,
            )
        })
        .collect();

    // then
    for (path, actual, expected) in resolved {
        assert_eq!(
            actual.as_deref(),
            Some(expected),
            "unexpected mapping for {path}"
        );
    }
}

#[test]
fn find_server_for_path_no_extension() {
    // given
    let registry = LspRegistry::new();
    registry.register("rust", LspServerStatus::Connected, None, vec![]);

    // when
    let result = registry.find_server_for_path("Makefile");

    // then
    assert!(result.is_none());
}

#[test]
fn list_servers_with_multiple() {
    // given
    let registry = LspRegistry::new();
    registry.register("rust", LspServerStatus::Connected, None, vec![]);
    registry.register("typescript", LspServerStatus::Starting, None, vec![]);
    registry.register("python", LspServerStatus::Error, None, vec![]);

    // when
    let servers = registry.list_servers();

    // then
    assert_eq!(servers.len(), 3);
    assert!(servers.iter().any(|server| server.language == "rust"));
    assert!(servers.iter().any(|server| server.language == "typescript"));
    assert!(servers.iter().any(|server| server.language == "python"));
}

#[test]
fn get_missing_server_returns_none() {
    // given
    let registry = LspRegistry::new();

    // when
    let server = registry.get("missing");

    // then
    assert!(server.is_none());
}

#[test]
fn add_diagnostics_missing_language_errors() {
    // given
    let registry = LspRegistry::new();

    // when
    let result = registry.add_diagnostics("missing", vec![]);

    // then
    let error = result.expect_err("missing language should fail");
    assert!(error.contains("LSP server not found for language: missing"));
}

#[test]
fn get_diagnostics_across_servers() {
    // given
    let registry = LspRegistry::new();
    let shared_path = "shared/file.txt";
    registry.register("rust", LspServerStatus::Connected, None, vec![]);
    registry.register("python", LspServerStatus::Connected, None, vec![]);
    registry
        .add_diagnostics(
            "rust",
            vec![LspDiagnostic {
                path: shared_path.into(),
                line: 4,
                character: 1,
                severity: "warning".into(),
                message: "warn".into(),
                source: None,
            }],
        )
        .expect("rust diagnostics should add");
    registry
        .add_diagnostics(
            "python",
            vec![LspDiagnostic {
                path: shared_path.into(),
                line: 8,
                character: 3,
                severity: "error".into(),
                message: "err".into(),
                source: None,
            }],
        )
        .expect("python diagnostics should add");

    // when
    let diagnostics = registry.get_diagnostics(shared_path);

    // then
    assert_eq!(diagnostics.len(), 2);
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message == "warn"));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message == "err"));
}

#[test]
fn clear_diagnostics_missing_language_errors() {
    // given
    let registry = LspRegistry::new();

    // when
    let result = registry.clear_diagnostics("missing");

    // then
    let error = result.expect_err("missing language should fail");
    assert!(error.contains("LSP server not found for language: missing"));
}

#[test]
fn register_with_descriptor_stores_entry() {
    let registry = LspRegistry::new();
    let descriptor = LspServerDescriptor {
        language: "rust".into(),
        command: "rust-analyzer".into(),
        args: vec![],
        extensions: vec!["rs".into()],
        install_hint: vec![],
    };
    registry.register_with_descriptor(
        "rust",
        LspServerStatus::Connected,
        Some("/project"),
        vec!["hover".into()],
        descriptor,
    );

    let server = registry.get("rust").expect("should exist after register_with_descriptor");
    assert_eq!(server.language, "rust");
    assert_eq!(server.status, LspServerStatus::Connected);
    assert_eq!(server.root_path.as_deref(), Some("/project"));
    assert_eq!(server.capabilities, vec!["hover"]);
}

#[test]
fn stop_server_on_nonexistent_errors() {
    let registry = LspRegistry::new();
    let result = registry.stop_server("missing");
    assert!(result.is_err(), "stopping a nonexistent server should error");
    let error = result.unwrap_err();
    assert!(error.contains("missing"), "error message should reference 'missing', got: {error}");
}

/// This test requires rust-analyzer to be installed on the system.
/// Run with: cargo test -p runtime -- --ignored
#[test]
#[ignore = "requires rust-analyzer installed on PATH"]
fn start_server_without_descriptor_falls_back_to_discovery() {
    let registry = LspRegistry::new();
    registry.register("rust", LspServerStatus::Starting, None, vec![]);
    let result = registry.start_server("rust");
    assert!(result.is_ok(), "start_server should discover and start rust-analyzer: {result:?}");
    let server = registry.get("rust").expect("rust should be registered");
    assert_eq!(server.status, LspServerStatus::Connected);
    let _ = registry.stop_server("rust");
}

/// This test requires rust-analyzer to be installed on the system.
/// Run with: cargo test -p runtime -- --ignored
#[test]
#[ignore = "requires rust-analyzer installed on PATH"]
fn dispatch_hover_lazy_starts_server() {
    let registry = LspRegistry::new();
    let descriptor = crate::lsp_discovery::LspServerDescriptor {
        language: "rust".into(),
        command: "rust-analyzer".into(),
        args: vec![],
        extensions: vec!["rs".into()],
        install_hint: vec![],
    };
    registry.register_with_descriptor(
        "rust",
        LspServerStatus::Starting,
        None,
        vec![],
        descriptor,
    );
    // dispatch should trigger start_server because process is None
    let result = registry.dispatch("hover", Some("src/main.rs"), Some(0), Some(0), None);
    // Result may be Ok or Err depending on whether rust-analyzer can actually
    // respond for this path, but it should not fail with "not connected"
    // (which would indicate the lazy-start didn't kick in).
    if let Err(e) = &result {
        assert!(
            !e.contains("not connected"),
            "dispatch should have lazily started the server, got: {e}"
        );
    }
    let _ = registry.stop_server("rust");
}

/// This test requires rust-analyzer to be installed on the system.
/// Run with: cargo test -p runtime -- --ignored
#[test]
#[ignore = "requires rust-analyzer installed on PATH"]
fn start_and_stop_server() {
    let registry = LspRegistry::new();
    let descriptor = crate::lsp_discovery::LspServerDescriptor {
        language: "rust".into(),
        command: "rust-analyzer".into(),
        args: vec![],
        extensions: vec!["rs".into()],
        install_hint: vec![],
    };
    registry.register_with_descriptor(
        "rust",
        LspServerStatus::Starting,
        None,
        vec![],
        descriptor,
    );

    let start_result = registry.start_server("rust");
    assert!(start_result.is_ok(), "start_server should succeed: {start_result:?}");

    let server = registry.get("rust").expect("rust should exist");
    assert_eq!(server.status, LspServerStatus::Connected);

    let stop_result = registry.stop_server("rust");
    assert!(stop_result.is_ok(), "stop_server should succeed: {stop_result:?}");

    let server = registry.get("rust").expect("rust should still be in registry");
    assert_eq!(server.status, LspServerStatus::Disconnected);
}
