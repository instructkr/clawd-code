use super::*;
use super::parse::*;

/// Requires rust-analyzer to be installed on the system.
/// Run with: cargo test -p runtime -- --ignored
#[tokio::test]
#[ignore = "requires rust-analyzer installed on PATH"]
async fn spawn_and_initialize_rust_analyzer() {
    let root = std::env::current_dir().expect("should have cwd");
    let process = LspProcess::start("rust-analyzer", &[], &root).await;
    assert!(process.is_ok(), "should spawn and initialize rust-analyzer");

    let mut process = process.unwrap();
    assert_eq!(process.status(), LspServerStatus::Connected);
    assert_eq!(process.language(), "rust-analyzer");

    let shutdown_result = process.shutdown().await;
    assert!(shutdown_result.is_ok(), "shutdown should succeed: {shutdown_result:?}");
}

/// Requires rust-analyzer to be installed and a Rust project on disk.
/// Run with: cargo test -p runtime -- --ignored
#[tokio::test]
#[ignore = "requires rust-analyzer installed on PATH"]
async fn hover_on_real_file() {
    let root = std::env::current_dir().expect("should have cwd");
    let mut process = LspProcess::start("rust-analyzer", &[], &root)
        .await
        .expect("should start rust-analyzer");

    // Try hover on src/main.rs — the result might be None if the file
    // doesn't exist at that path, but the call itself should not error.
    let file_path = root.join("src").join("main.rs");
    let path_str = file_path.to_string_lossy();
    let result = process.hover(&path_str, 0, 0).await;
    assert!(result.is_ok(), "hover should not return an error: {:?}", result.err());

    let _ = process.shutdown().await;
}

#[test]
fn parse_hover_markup_content() {
    let value = serde_json::json!({
        "contents": {
            "kind": "plaintext",
            "value": "fn main()"
        }
    });
    let result = parse_hover(&value);
    assert!(result.is_some());
    let hover = result.unwrap();
    assert_eq!(hover.content, "fn main()");
}

#[test]
fn parse_hover_marked_string_object() {
    let value = serde_json::json!({
        "contents": {
            "language": "rust",
            "value": "pub fn foo()"
        }
    });
    let result = parse_hover(&value);
    assert!(result.is_some());
    let hover = result.unwrap();
    assert_eq!(hover.content, "pub fn foo()");
    assert_eq!(hover.language.as_deref(), Some("rust"));
}

#[test]
fn parse_hover_plain_string() {
    let value = serde_json::json!({
        "contents": "some text"
    });
    let result = parse_hover(&value);
    assert!(result.is_some());
    let hover = result.unwrap();
    assert_eq!(hover.content, "some text");
    assert!(hover.language.is_none());
}

#[test]
fn parse_hover_array_of_marked_strings() {
    let value = serde_json::json!({
        "contents": [
            "first line",
            { "language": "rust", "value": "fn bar()" }
        ]
    });
    let result = parse_hover(&value);
    assert!(result.is_some());
    let hover = result.unwrap();
    assert!(hover.content.contains("first line"));
    assert!(hover.content.contains("fn bar()"));
}

#[test]
fn parse_locations_empty_array() {
    let value = serde_json::json!([]);
    let locations = parse_locations(&value);
    assert!(locations.is_empty());
}

#[test]
fn parse_locations_valid() {
    let value = serde_json::json!([
        {
            "uri": "file:///tmp/test.rs",
            "range": {
                "start": { "line": 5, "character": 10 },
                "end": { "line": 5, "character": 15 }
            }
        }
    ]);
    let locations = parse_locations(&value);
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].line, 5);
    assert_eq!(locations[0].character, 10);
    assert_eq!(locations[0].end_line, Some(5));
    assert_eq!(locations[0].end_character, Some(15));
}

#[test]
fn parse_symbols_basic() {
    let value = serde_json::json!([
        {
            "name": "main",
            "kind": 12,
            "range": {
                "start": { "line": 1, "character": 0 },
                "end": { "line": 5, "character": 1 }
            }
        }
    ]);
    let symbols = parse_symbols(&value, "/tmp/test.rs");
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "main");
    assert_eq!(symbols[0].kind, "Function");
    assert_eq!(symbols[0].line, 1);
}

#[test]
fn parse_completions_basic() {
    let value = serde_json::json!([
        { "label": "foo", "kind": 3, "detail": "fn foo()" },
        { "label": "bar", "kind": 6 }
    ]);
    let completions = parse_completions(&value);
    assert_eq!(completions.len(), 2);
    assert_eq!(completions[0].label, "foo");
    assert_eq!(completions[0].kind.as_deref(), Some("Function"));
    assert_eq!(completions[0].detail.as_deref(), Some("fn foo()"));
    assert_eq!(completions[1].label, "bar");
    assert_eq!(completions[1].kind.as_deref(), Some("Variable"));
}

#[test]
fn symbol_kind_name_all_variants() {
    assert_eq!(symbol_kind_name(1), "File");
    assert_eq!(symbol_kind_name(6), "Method");
    assert_eq!(symbol_kind_name(12), "Function");
    assert_eq!(symbol_kind_name(13), "Variable");
    assert_eq!(symbol_kind_name(23), "Struct");
    assert_eq!(symbol_kind_name(99), "Unknown(99)");
}

#[test]
fn completion_kind_name_all_variants() {
    assert_eq!(completion_kind_name(1), "Text");
    assert_eq!(completion_kind_name(3), "Function");
    assert_eq!(completion_kind_name(6), "Variable");
    assert_eq!(completion_kind_name(14), "Keyword");
    assert_eq!(completion_kind_name(99), "Unknown(99)");
}

#[test]
fn text_document_position_params_structure() {
    let params = text_document_position_params("file:///test.rs", 5, 10);
    assert_eq!(params["textDocument"]["uri"], "file:///test.rs");
    assert_eq!(params["position"]["line"], 5);
    assert_eq!(params["position"]["character"], 10);
}

#[test]
fn path_to_uri_absolute() {
    let uri = path_to_uri("/tmp/test.rs");
    assert_eq!(uri, "file:///tmp/test.rs");
}

#[test]
fn uri_to_path_extracts_path() {
    assert_eq!(uri_to_path("file:///tmp/test.rs"), "/tmp/test.rs");
    assert_eq!(uri_to_path("/no/prefix"), "/no/prefix");
}
