use std::collections::BTreeMap;
use std::path::Path;

use crate::config::ConfigError;
use crate::json::JsonValue;

/// Diagnostic emitted when a config file contains a suspect field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    pub path: String,
    pub field: String,
    pub line: Option<usize>,
    pub kind: DiagnosticKind,
}

/// Classification of the diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticKind {
    UnknownKey {
        suggestion: Option<String>,
    },
    WrongType {
        expected: &'static str,
        got: &'static str,
    },
    Deprecated {
        replacement: &'static str,
    },
}

impl std::fmt::Display for ConfigDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let location = self
            .line
            .map_or_else(String::new, |line| format!(" (line {line})"));
        match &self.kind {
            DiagnosticKind::UnknownKey { suggestion: None } => {
                write!(f, "{}: unknown key \"{}\"{location}", self.path, self.field)
            }
            DiagnosticKind::UnknownKey {
                suggestion: Some(hint),
            } => {
                write!(
                    f,
                    "{}: unknown key \"{}\"{location}. Did you mean \"{}\"?",
                    self.path, self.field, hint
                )
            }
            DiagnosticKind::WrongType { expected, got } => {
                write!(
                    f,
                    "{}: field \"{}\" must be {expected}, got {got}{location}",
                    self.path, self.field
                )
            }
            DiagnosticKind::Deprecated { replacement } => {
                write!(
                    f,
                    "{}: field \"{}\" is deprecated{location}. Use \"{replacement}\" instead",
                    self.path, self.field
                )
            }
        }
    }
}

/// Result of validating a single config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    pub errors: Vec<ConfigDiagnostic>,
    pub warnings: Vec<ConfigDiagnostic>,
}

impl ValidationResult {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    fn merge(&mut self, other: Self) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
    }
}

// ---- known-key schema ----

/// Expected type for a config field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldType {
    String,
    Bool,
    Object,
    StringArray,
    Number,
}

impl FieldType {
    fn label(self) -> &'static str {
        match self {
            Self::String => "a string",
            Self::Bool => "a boolean",
            Self::Object => "an object",
            Self::StringArray => "an array of strings",
            Self::Number => "a number",
        }
    }

    fn matches(self, value: &JsonValue) -> bool {
        match self {
            Self::String => value.as_str().is_some(),
            Self::Bool => value.as_bool().is_some(),
            Self::Object => value.as_object().is_some(),
            Self::StringArray => value
                .as_array()
                .is_some_and(|arr| arr.iter().all(|v| v.as_str().is_some())),
            Self::Number => value.as_i64().is_some(),
        }
    }
}

fn json_type_label(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "a boolean",
        JsonValue::Number(_) => "a number",
        JsonValue::String(_) => "a string",
        JsonValue::Array(_) => "an array",
        JsonValue::Object(_) => "an object",
    }
}

struct FieldSpec {
    name: &'static str,
    expected: FieldType,
}

struct DeprecatedField {
    name: &'static str,
    replacement: &'static str,
}

const TOP_LEVEL_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "$schema",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "model",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "hooks",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "permissions",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "permissionMode",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "mcpServers",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "oauth",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "enabledPlugins",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "plugins",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "sandbox",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "env",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "aliases",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "providerFallbacks",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "trustedRoots",
        expected: FieldType::StringArray,
    },
    FieldSpec {
        name: "provider",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "lsp",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "lspAutoStart",
        expected: FieldType::Bool,
    },
];

const HOOKS_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "PreToolUse",
        expected: FieldType::StringArray,
    },
    FieldSpec {
        name: "PostToolUse",
        expected: FieldType::StringArray,
    },
    FieldSpec {
        name: "PostToolUseFailure",
        expected: FieldType::StringArray,
    },
];

const PERMISSIONS_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "defaultMode",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "allow",
        expected: FieldType::StringArray,
    },
    FieldSpec {
        name: "deny",
        expected: FieldType::StringArray,
    },
    FieldSpec {
        name: "ask",
        expected: FieldType::StringArray,
    },
];

const PLUGINS_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "enabled",
        expected: FieldType::Object,
    },
    FieldSpec {
        name: "externalDirectories",
        expected: FieldType::StringArray,
    },
    FieldSpec {
        name: "installRoot",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "registryPath",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "bundledRoot",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "maxOutputTokens",
        expected: FieldType::Number,
    },
];

const SANDBOX_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "enabled",
        expected: FieldType::Bool,
    },
    FieldSpec {
        name: "namespaceRestrictions",
        expected: FieldType::Bool,
    },
    FieldSpec {
        name: "networkIsolation",
        expected: FieldType::Bool,
    },
    FieldSpec {
        name: "filesystemMode",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "allowedMounts",
        expected: FieldType::StringArray,
    },
];

const OAUTH_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "clientId",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "authorizeUrl",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "tokenUrl",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "callbackPort",
        expected: FieldType::Number,
    },
    FieldSpec {
        name: "manualRedirectUrl",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "scopes",
        expected: FieldType::StringArray,
    },
];


const LSP_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "command",
        expected: FieldType::String,
    },
    FieldSpec {
        name: "args",
        expected: FieldType::StringArray,
    },
    FieldSpec {
        name: "enabled",
        expected: FieldType::Bool,
    },
];

const DEPRECATED_FIELDS: &[DeprecatedField] = &[
    DeprecatedField {
        name: "permissionMode",
        replacement: "permissions.defaultMode",
    },
    DeprecatedField {
        name: "enabledPlugins",
        replacement: "plugins.enabled",
    },
];

// ---- line-number resolution ----

/// Find the 1-based line number where a JSON key first appears in the raw source.
fn find_key_line(source: &str, key: &str) -> Option<usize> {
    // Search for `"key"` followed by optional whitespace and a colon.
    let needle = format!("\"{key}\"");
    let mut search_start = 0;
    while let Some(offset) = source[search_start..].find(&needle) {
        let absolute = search_start + offset;
        let after = absolute + needle.len();
        // Verify the next non-whitespace char is `:` to confirm this is a key, not a value.
        if source[after..].chars().find(|ch| !ch.is_ascii_whitespace()) == Some(':') {
            return Some(source[..absolute].chars().filter(|&ch| ch == '\n').count() + 1);
        }
        search_start = after;
    }
    None
}

// ---- core validation ----

fn validate_object_keys(
    object: &BTreeMap<String, JsonValue>,
    known_fields: &[FieldSpec],
    prefix: &str,
    source: &str,
    path_display: &str,
) -> ValidationResult {
    let mut result = ValidationResult {
        errors: Vec::new(),
        warnings: Vec::new(),
    };

    let known_names: Vec<&str> = known_fields.iter().map(|f| f.name).collect();

    for (key, value) in object {
        let field_path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };

        if let Some(spec) = known_fields.iter().find(|f| f.name == key) {
            // Type check.
            if !spec.expected.matches(value) {
                result.errors.push(ConfigDiagnostic {
                    path: path_display.to_string(),
                    field: field_path,
                    line: find_key_line(source, key),
                    kind: DiagnosticKind::WrongType {
                        expected: spec.expected.label(),
                        got: json_type_label(value),
                    },
                });
            }
        } else if DEPRECATED_FIELDS.iter().any(|d| d.name == key) {
            // Deprecated key — handled separately, not an unknown-key error.
        } else {
            // Unknown key.
            let suggestion = suggest_field(key, &known_names);
            result.errors.push(ConfigDiagnostic {
                path: path_display.to_string(),
                field: field_path,
                line: find_key_line(source, key),
                kind: DiagnosticKind::UnknownKey { suggestion },
            });
        }
    }

    result
}

fn suggest_field(input: &str, candidates: &[&str]) -> Option<String> {
    let input_lower = input.to_ascii_lowercase();
    candidates
        .iter()
        .filter_map(|candidate| {
            let distance = simple_edit_distance(&input_lower, &candidate.to_ascii_lowercase());
            (distance <= 3).then_some((distance, *candidate))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, name)| name.to_string())
}

fn simple_edit_distance(left: &str, right: &str) -> usize {
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }
    let right_chars: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right_chars.len()).collect();
    let mut current = vec![0; right_chars.len() + 1];

    for (left_index, left_char) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let cost = usize::from(left_char != *right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + cost);
        }
        previous.clone_from(&current);
    }

    previous[right_chars.len()]
}

/// Validate a parsed config file's keys and types against the known schema.
///
/// Returns diagnostics (errors and deprecation warnings) without blocking the load.
pub fn validate_config_file(
    object: &BTreeMap<String, JsonValue>,
    source: &str,
    file_path: &Path,
) -> ValidationResult {
    let path_display = file_path.display().to_string();
    let mut result = validate_object_keys(object, TOP_LEVEL_FIELDS, "", source, &path_display);

    // Check deprecated fields.
    for deprecated in DEPRECATED_FIELDS {
        if object.contains_key(deprecated.name) {
            result.warnings.push(ConfigDiagnostic {
                path: path_display.clone(),
                field: deprecated.name.to_string(),
                line: find_key_line(source, deprecated.name),
                kind: DiagnosticKind::Deprecated {
                    replacement: deprecated.replacement,
                },
            });
        }
    }

    // Validate known nested objects.
    if let Some(hooks) = object.get("hooks").and_then(JsonValue::as_object) {
        result.merge(validate_object_keys(
            hooks,
            HOOKS_FIELDS,
            "hooks",
            source,
            &path_display,
        ));
    }
    if let Some(permissions) = object.get("permissions").and_then(JsonValue::as_object) {
        result.merge(validate_object_keys(
            permissions,
            PERMISSIONS_FIELDS,
            "permissions",
            source,
            &path_display,
        ));
    }
    if let Some(plugins) = object.get("plugins").and_then(JsonValue::as_object) {
        result.merge(validate_object_keys(
            plugins,
            PLUGINS_FIELDS,
            "plugins",
            source,
            &path_display,
        ));
    }
    if let Some(sandbox) = object.get("sandbox").and_then(JsonValue::as_object) {
        result.merge(validate_object_keys(
            sandbox,
            SANDBOX_FIELDS,
            "sandbox",
            source,
            &path_display,
        ));
    }
    if let Some(oauth) = object.get("oauth").and_then(JsonValue::as_object) {
        result.merge(validate_object_keys(
            oauth,
            OAUTH_FIELDS,
            "oauth",
            source,
            &path_display,
        ));
    }

    // Validate lsp map: each value must be an object with LSP_FIELDS.
    if let Some(lsp) = object.get("lsp").and_then(JsonValue::as_object) {
        for (server_name, server_value) in lsp {
            if let Some(server_obj) = server_value.as_object() {
                result.merge(validate_object_keys(
                    server_obj,
                    LSP_FIELDS,
                    &format!("lsp.{server_name}"),
                    source,
                    &path_display,
                ));
            } else {
                result.errors.push(ConfigDiagnostic {
                    path: path_display.clone(),
                    field: format!("lsp.{server_name}"),
                    line: find_key_line(source, server_name),
                    kind: DiagnosticKind::WrongType {
                        expected: "an object",
                        got: json_type_label(server_value),
                    },
                });
            }
        }
    }

    result
}

/// Check whether a file path uses an unsupported config format (e.g. TOML).
pub fn check_unsupported_format(file_path: &Path) -> Result<(), ConfigError> {
    if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
        if ext.eq_ignore_ascii_case("toml") {
            return Err(ConfigError::Parse(format!(
                "{}: TOML config files are not supported. Use JSON (settings.json) instead",
                file_path.display()
            )));
        }
    }
    Ok(())
}

/// Format all diagnostics into a human-readable report.
#[must_use]
pub fn format_diagnostics(result: &ValidationResult) -> String {
    let mut lines = Vec::new();
    for warning in &result.warnings {
        lines.push(format!("warning: {warning}"));
    }
    for error in &result.errors {
        lines.push(format!("error: {error}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_path() -> PathBuf {
        PathBuf::from("/test/settings.json")
    }

    #[test]
    fn detects_unknown_top_level_key() {
        // given
        let source = r#"{"model": "opus", "unknownField": true}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "unknownField");
        assert!(matches!(
            result.errors[0].kind,
            DiagnosticKind::UnknownKey { .. }
        ));
    }

    #[test]
    fn detects_wrong_type_for_model() {
        // given
        let source = r#"{"model": 123}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "model");
        assert!(matches!(
            result.errors[0].kind,
            DiagnosticKind::WrongType {
                expected: "a string",
                got: "a number"
            }
        ));
    }

    #[test]
    fn detects_deprecated_permission_mode() {
        // given
        let source = r#"{"permissionMode": "plan"}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].field, "permissionMode");
        assert!(matches!(
            result.warnings[0].kind,
            DiagnosticKind::Deprecated {
                replacement: "permissions.defaultMode"
            }
        ));
    }

    #[test]
    fn detects_deprecated_enabled_plugins() {
        // given
        let source = r#"{"enabledPlugins": {"tool-guard@builtin": true}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].field, "enabledPlugins");
        assert!(matches!(
            result.warnings[0].kind,
            DiagnosticKind::Deprecated {
                replacement: "plugins.enabled"
            }
        ));
    }

    #[test]
    fn reports_line_number_for_unknown_key() {
        // given
        let source = "{\n  \"model\": \"opus\",\n  \"badKey\": true\n}";
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].line, Some(3));
        assert_eq!(result.errors[0].field, "badKey");
    }

    #[test]
    fn reports_line_number_for_wrong_type() {
        // given
        let source = "{\n  \"model\": 42\n}";
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].line, Some(2));
    }

    #[test]
    fn validates_nested_hooks_keys() {
        // given
        let source = r#"{"hooks": {"PreToolUse": ["cmd"], "BadHook": ["x"]}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "hooks.BadHook");
    }

    #[test]
    fn validates_nested_permissions_keys() {
        // given
        let source = r#"{"permissions": {"allow": ["Read"], "denyAll": true}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "permissions.denyAll");
    }

    #[test]
    fn validates_nested_sandbox_keys() {
        // given
        let source = r#"{"sandbox": {"enabled": true, "containerMode": "strict"}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "sandbox.containerMode");
    }

    #[test]
    fn validates_nested_plugins_keys() {
        // given
        let source = r#"{"plugins": {"installRoot": "/tmp", "autoUpdate": true}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "plugins.autoUpdate");
    }

    #[test]
    fn validates_nested_oauth_keys() {
        // given
        let source = r#"{"oauth": {"clientId": "abc", "secret": "hidden"}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "oauth.secret");
    }

    #[test]
    fn valid_config_produces_no_diagnostics() {
        // given
        let source = r#"{
  "model": "opus",
  "hooks": {"PreToolUse": ["guard"]},
  "permissions": {"defaultMode": "plan", "allow": ["Read"]},
  "mcpServers": {},
  "sandbox": {"enabled": false}
}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert!(result.is_ok());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn suggests_close_field_name() {
        // given
        let source = r#"{"modle": "opus"}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        match &result.errors[0].kind {
            DiagnosticKind::UnknownKey {
                suggestion: Some(s),
            } => assert_eq!(s, "model"),
            other => panic!("expected suggestion, got {other:?}"),
        }
    }

    #[test]
    fn format_diagnostics_includes_all_entries() {
        // given
        let source = r#"{"permissionMode": "plan", "badKey": 1}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");
        let result = validate_config_file(object, source, &test_path());

        // when
        let output = format_diagnostics(&result);

        // then
        assert!(output.contains("warning:"));
        assert!(output.contains("error:"));
        assert!(output.contains("badKey"));
        assert!(output.contains("permissionMode"));
    }

    #[test]
    fn check_unsupported_format_rejects_toml() {
        // given
        let path = PathBuf::from("/home/.claw/settings.toml");

        // when
        let result = check_unsupported_format(&path);

        // then
        assert!(result.is_err());
        let message = result.unwrap_err().to_string();
        assert!(message.contains("TOML"));
        assert!(message.contains("settings.toml"));
    }

    #[test]
    fn check_unsupported_format_allows_json() {
        // given
        let path = PathBuf::from("/home/.claw/settings.json");

        // when / then
        assert!(check_unsupported_format(&path).is_ok());
    }

    #[test]
    fn wrong_type_in_nested_sandbox_field() {
        // given
        let source = r#"{"sandbox": {"enabled": "yes"}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "sandbox.enabled");
        assert!(matches!(
            result.errors[0].kind,
            DiagnosticKind::WrongType {
                expected: "a boolean",
                got: "a string"
            }
        ));
    }

    #[test]
    fn display_format_unknown_key_with_line() {
        // given
        let diag = ConfigDiagnostic {
            path: "/test/settings.json".to_string(),
            field: "badKey".to_string(),
            line: Some(5),
            kind: DiagnosticKind::UnknownKey { suggestion: None },
        };

        // when
        let output = diag.to_string();

        // then
        assert_eq!(
            output,
            r#"/test/settings.json: unknown key "badKey" (line 5)"#
        );
    }

    #[test]
    fn display_format_wrong_type_with_line() {
        // given
        let diag = ConfigDiagnostic {
            path: "/test/settings.json".to_string(),
            field: "model".to_string(),
            line: Some(2),
            kind: DiagnosticKind::WrongType {
                expected: "a string",
                got: "a number",
            },
        };

        // when
        let output = diag.to_string();

        // then
        assert_eq!(
            output,
            r#"/test/settings.json: field "model" must be a string, got a number (line 2)"#
        );
    }

    #[test]
    fn display_format_deprecated_with_line() {
        // given
        let diag = ConfigDiagnostic {
            path: "/test/settings.json".to_string(),
            field: "permissionMode".to_string(),
            line: Some(3),
            kind: DiagnosticKind::Deprecated {
                replacement: "permissions.defaultMode",
            },
        };

        // when
        let output = diag.to_string();

        // then
        assert_eq!(
            output,
            r#"/test/settings.json: field "permissionMode" is deprecated (line 3). Use "permissions.defaultMode" instead"#
        );
    }

    #[test]
    fn validates_lsp_config_valid() {
        // given
        let source = r#"{"lsp": {"rust": {"command": "rust-analyzer", "args": [], "enabled": true}, "python": {"command": "pyright-langserver", "args": ["--stdio"], "enabled": false}}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert!(result.is_ok());
    }

    #[test]
    fn validates_lsp_config_unknown_field() {
        // given
        let source = r#"{"lsp": {"rust": {"command": "rust-analyzer", "port": 8080}}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "lsp.rust.port");
        assert!(matches!(
            result.errors[0].kind,
            DiagnosticKind::UnknownKey { .. }
        ));
    }

    #[test]
    fn validates_lsp_config_wrong_type_for_command() {
        // given
        let source = r#"{"lsp": {"rust": {"command": 123}}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "lsp.rust.command");
        assert!(matches!(
            result.errors[0].kind,
            DiagnosticKind::WrongType {
                expected: "a string",
                got: "a number"
            }
        ));
    }

    #[test]
    fn validates_lsp_config_wrong_type_for_args() {
        // given
        let source = r#"{"lsp": {"rust": {"command": "rust-analyzer", "args": "wrong"}}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "lsp.rust.args");
        assert!(matches!(
            result.errors[0].kind,
            DiagnosticKind::WrongType { .. }
        ));
    }

    #[test]
    fn validates_lsp_config_wrong_type_for_enabled() {
        // given
        let source = r#"{"lsp": {"rust": {"command": "rust-analyzer", "enabled": "yes"}}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "lsp.rust.enabled");
        assert!(matches!(
            result.errors[0].kind,
            DiagnosticKind::WrongType {
                expected: "a boolean",
                got: "a string"
            }
        ));
    }

    #[test]
    fn validates_lsp_server_must_be_object() {
        // given
        let source = r#"{"lsp": {"rust": "not-an-object"}}"#;
        let parsed = JsonValue::parse(source).expect("valid json");
        let object = parsed.as_object().expect("object");

        // when
        let result = validate_config_file(object, source, &test_path());

        // then
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].field, "lsp.rust");
        assert!(matches!(
            result.errors[0].kind,
            DiagnosticKind::WrongType {
                expected: "an object",
                got: "a string"
            }
        ));
    }
}
