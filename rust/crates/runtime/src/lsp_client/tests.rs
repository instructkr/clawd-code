//! Tests for the LSP client registry: registration, diagnostics, and type unit tests.

use super::*;
use super::types::*;

#[test]
fn registers_and_retrieves_server() {
    let registry = LspRegistry::new();
    registry.register(
        "rust",
        LspServerStatus::Connected,
        Some("/workspace"),
        vec!["hover".into(), "completion".into()],
    );

    let server = registry.get("rust").expect("should exist");
    assert_eq!(server.language, "rust");
    assert_eq!(server.status, LspServerStatus::Connected);
    assert_eq!(server.capabilities.len(), 2);
}

#[test]
fn finds_server_by_file_extension() {
    let registry = LspRegistry::new();
    registry.register("rust", LspServerStatus::Connected, None, vec![]);
    registry.register("typescript", LspServerStatus::Connected, None, vec![]);

    let rs_server = registry.find_server_for_path("src/main.rs").unwrap();
    assert_eq!(rs_server.language, "rust");

    let ts_server = registry.find_server_for_path("src/index.ts").unwrap();
    assert_eq!(ts_server.language, "typescript");

    assert!(registry.find_server_for_path("data.csv").is_none());
}

#[test]
fn manages_diagnostics() {
    let registry = LspRegistry::new();
    registry.register("rust", LspServerStatus::Connected, None, vec![]);

    registry
        .add_diagnostics(
            "rust",
            vec![LspDiagnostic {
                path: "src/main.rs".into(),
                line: 10,
                character: 5,
                severity: "error".into(),
                message: "mismatched types".into(),
                source: Some("rust-analyzer".into()),
            }],
        )
        .unwrap();

    let diags = registry.get_diagnostics("src/main.rs");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].message, "mismatched types");

    registry.clear_diagnostics("rust").unwrap();
    assert!(registry.get_diagnostics("src/main.rs").is_empty());
}

#[test]
fn dispatches_diagnostics_action() {
    let registry = LspRegistry::new();
    registry.register("rust", LspServerStatus::Connected, None, vec![]);
    registry
        .add_diagnostics(
            "rust",
            vec![LspDiagnostic {
                path: "src/lib.rs".into(),
                line: 1,
                character: 0,
                severity: "warning".into(),
                message: "unused import".into(),
                source: None,
            }],
        )
        .unwrap();

    let result = registry
        .dispatch("diagnostics", Some("src/lib.rs"), None, None, None)
        .unwrap();
    assert_eq!(result["count"], 1);
}

#[test]
fn dispatches_hover_action() {
    let registry = LspRegistry::new();
    registry.register("rust", LspServerStatus::Connected, None, vec![]);

    let result = registry
        .dispatch("hover", Some("src/main.rs"), Some(10), Some(5), None)
        .unwrap();
    assert_eq!(result["action"], "hover");
    assert_eq!(result["language"], "rust");
}

#[test]
fn rejects_action_on_disconnected_server() {
    let registry = LspRegistry::new();
    registry.register("rust", LspServerStatus::Disconnected, None, vec![]);

    assert!(registry
        .dispatch("hover", Some("src/main.rs"), Some(1), Some(0), None)
        .is_err());
}

#[test]
fn rejects_unknown_action() {
    let registry = LspRegistry::new();
    assert!(registry
        .dispatch("unknown_action", Some("file.rs"), None, None, None)
        .is_err());
}

#[test]
fn disconnects_server() {
    let registry = LspRegistry::new();
    registry.register("rust", LspServerStatus::Connected, None, vec![]);
    assert_eq!(registry.len(), 1);

    let removed = registry.disconnect("rust");
    assert!(removed.is_some());
    assert!(registry.is_empty());
}

#[test]
fn lsp_action_from_str_all_aliases() {
    // given
    let cases = [
        ("diagnostics", Some(LspAction::Diagnostics)),
        ("hover", Some(LspAction::Hover)),
        ("definition", Some(LspAction::Definition)),
        ("goto_definition", Some(LspAction::Definition)),
        ("references", Some(LspAction::References)),
        ("find_references", Some(LspAction::References)),
        ("completion", Some(LspAction::Completion)),
        ("completions", Some(LspAction::Completion)),
        ("symbols", Some(LspAction::Symbols)),
        ("document_symbols", Some(LspAction::Symbols)),
        ("format", Some(LspAction::Format)),
        ("formatting", Some(LspAction::Format)),
        ("unknown", None),
    ];

    // when
    let resolved: Vec<_> = cases
        .into_iter()
        .map(|(input, expected)| (input, LspAction::from_str(input), expected))
        .collect();

    // then
    for (input, actual, expected) in resolved {
        assert_eq!(actual, expected, "unexpected action resolution for {input}");
    }
}

#[test]
fn lsp_server_status_display_all_variants() {
    // given
    let cases = [
        (LspServerStatus::Connected, "connected"),
        (LspServerStatus::Disconnected, "disconnected"),
        (LspServerStatus::Starting, "starting"),
        (LspServerStatus::Error, "error"),
    ];

    // when
    let rendered: Vec<_> = cases
        .into_iter()
        .map(|(status, expected)| (status.to_string(), expected))
        .collect();

    // then
    assert_eq!(
        rendered,
        vec![
            ("connected".to_string(), "connected"),
            ("disconnected".to_string(), "disconnected"),
            ("starting".to_string(), "starting"),
            ("error".to_string(), "error"),
        ]
    );
}

#[test]
fn dispatch_diagnostics_without_path_aggregates() {
    // given
    let registry = LspRegistry::new();
    registry.register("rust", LspServerStatus::Connected, None, vec![]);
    registry.register("python", LspServerStatus::Connected, None, vec![]);
    registry
        .add_diagnostics(
            "rust",
            vec![LspDiagnostic {
                path: "src/lib.rs".into(),
                line: 1,
                character: 0,
                severity: "warning".into(),
                message: "unused import".into(),
                source: Some("rust-analyzer".into()),
            }],
        )
        .expect("rust diagnostics should add");
    registry
        .add_diagnostics(
            "python",
            vec![LspDiagnostic {
                path: "script.py".into(),
                line: 2,
                character: 4,
                severity: "error".into(),
                message: "undefined name".into(),
                source: Some("pyright".into()),
            }],
        )
        .expect("python diagnostics should add");

    // when
    let result = registry
        .dispatch("diagnostics", None, None, None, None)
        .expect("aggregate diagnostics should work");

    // then
    assert_eq!(result["action"], "diagnostics");
    assert_eq!(result["count"], 2);
    assert_eq!(result["diagnostics"].as_array().map(Vec::len), Some(2));
}

#[test]
fn dispatch_non_diagnostics_requires_path() {
    // given
    let registry = LspRegistry::new();

    // when
    let result = registry.dispatch("hover", None, Some(1), Some(0), None);

    // then
    assert_eq!(
        result.expect_err("path should be required"),
        "path is required for this LSP action"
    );
}

#[test]
fn dispatch_no_server_for_path_errors() {
    // given
    let registry = LspRegistry::new();

    // when
    let result = registry.dispatch("hover", Some("notes.md"), Some(1), Some(0), None);

    // then
    let error = result.expect_err("missing server should fail");
    assert!(error.contains("no LSP server available for path: notes.md"));
}

#[test]
fn dispatch_disconnected_server_error_payload() {
    // given
    let registry = LspRegistry::new();
    registry.register("typescript", LspServerStatus::Disconnected, None, vec![]);

    // when
    let result = registry.dispatch("hover", Some("src/index.ts"), Some(3), Some(2), None);

    // then
    let error = result.expect_err("disconnected server should fail");
    assert!(error.contains("typescript"));
    assert!(error.contains("disconnected"));
}
