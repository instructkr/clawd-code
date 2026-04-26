use serde_json;

use crate::format::truncate_for_summary;
use crate::tui::theme::Theme;

/// Decision from parsing a user's permission prompt response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny { reason: String },
    AllowAll,
    ViewInput,
}

/// Plain-English description of what a tool will do.
pub fn describe_tool_action(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "bash" | "Bash" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Execute shell command: {}", truncate_for_summary(cmd, 80))
        }
        "edit_file" | "Edit" => {
            let path = extract_path(input);
            format!("Edit file: {path}")
        }
        "write_file" | "Write" => {
            let path = extract_path(input);
            let content = input
                .get("content")
                .and_then(|v| v.as_str())
                .map(|c| c.lines().count())
                .unwrap_or(0);
            format!("Write file: {path} ({content} lines)")
        }
        "read_file" | "Read" => {
            let path = extract_path(input);
            let start_line = input
                .get("start")
                .or_else(|| input.get("startLine"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            format!("Read file: {path} (from line {start_line})")
        }
        "web_search" | "WebSearch" => {
            let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Search the web: {query}")
        }
        "glob_search" | "Glob" | "grep_search" | "Grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!("Search code: {pattern} in {path}")
        }
        _ => format!("Execute tool: {tool_name}"),
    }
}

fn extract_path(input: &serde_json::Value) -> String {
    input
        .get("file_path")
        .or_else(|| input.get("filePath"))
        .or_else(|| input.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string()
}

/// Render an enhanced permission prompt with box-drawing and action descriptions.
pub fn format_enhanced_permission_prompt(
    tool_name: &str,
    input: &serde_json::Value,
    current_mode_str: &str,
    required_mode_str: &str,
    reason: Option<&str>,
) -> String {
    let action = describe_tool_action(tool_name, input);
    let header = format!("{}⚠ Permission Required{}", Theme::WARNING, Theme::RESET);
    let border = Theme::permission_border();
    let mut lines = vec![
        String::new(),
        header,
        border.clone(),
        format!("  Tool:\t{}", tool_name),
        format!("  Action:\t{}", action),
        format!(
            "  Mode:\t{} → \x1b[1m{}\x1b[0m",
            current_mode_str, required_mode_str
        ),
    ];
    if let Some(reason) = reason {
        lines.push(format!("  Reason:\t{}", reason));
    }
    lines.push(border);
    lines.push(format!(
        "  {}[y]es | [n]o | [a]llow all | [v]iew input{}",
        Theme::DIM,
        Theme::RESET
    ));
    lines.push(String::new());
    lines.push("  [\x1b[1my\x1b[0m/\x1b[1mN\x1b[0m/\x1b[1ma\x1b[0m/\x1b[1mv\x1b[0m]: ".to_string());
    lines.join("\n")
}

/// Parse user input from the permission prompt.
pub fn parse_permission_response(input: &str) -> PermissionDecision {
    let normalized = input.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "y" | "yes" => PermissionDecision::Allow,
        "a" | "all" => PermissionDecision::AllowAll,
        "v" | "view" => PermissionDecision::ViewInput,
        _ => PermissionDecision::Deny {
            reason: "denied by user".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn describe_bash_action() {
        let input = json!({"command": "rm -rf /tmp/test"});
        let result = describe_tool_action("bash", &input);
        assert!(result.contains("Execute shell command"));
        assert!(result.contains("rm -rf /tmp/test"));
    }

    #[test]
    fn describe_edit_file_action() {
        let input = json!({"file_path": "/src/main.rs"});
        let result = describe_tool_action("edit_file", &input);
        assert!(result.contains("Edit file"));
        assert!(result.contains("/src/main.rs"));
    }

    #[test]
    fn describe_read_file_action() {
        let input = json!({"filePath": "/src/lib.rs", "startLine": 42});
        let result = describe_tool_action("read_file", &input);
        assert!(result.contains("Read file"));
        assert!(result.contains("/src/lib.rs"));
        assert!(result.contains("42"));
    }

    #[test]
    fn describe_web_search_action() {
        let input = json!({"query": "rust async best practices"});
        let result = describe_tool_action("web_search", &input);
        assert!(result.contains("Search the web"));
        assert!(result.contains("rust async best practices"));
    }

    #[test]
    fn describe_generic_tool_action() {
        let input = json!({});
        let result = describe_tool_action("custom_plugin", &input);
        assert_eq!(result, "Execute tool: custom_plugin");
    }

    #[test]
    fn enhanced_prompt_contains_box_borders_and_options() {
        let input = json!({"command": "git status"});
        let result = format_enhanced_permission_prompt(
            "bash",
            &input,
            "read-only",
            "danger-full-access",
            Some("bash requires full access"),
        );
        assert!(result.contains("Permission Required"));
        assert!(result.contains("bash"));
        assert!(result.contains("read-only"));
        assert!(result.contains("danger-full-access"));
        assert!(result.contains("[y]es"));
        assert!(result.contains("[a]llow all"));
        assert!(result.contains("[v]iew input"));
    }

    #[test]
    fn enhanced_prompt_without_reason_still_shows_options() {
        let input = json!({"command": "ls"});
        let result = format_enhanced_permission_prompt(
            "bash",
            &input,
            "read-only",
            "danger-full-access",
            None,
        );
        assert!(result.contains("Permission Required"));
        assert!(!result.contains("Reason:"));
    }

    #[test]
    fn parse_allow_response() {
        assert_eq!(parse_permission_response("y"), PermissionDecision::Allow);
        assert_eq!(parse_permission_response("yes"), PermissionDecision::Allow);
    }

    #[test]
    fn parse_deny_response() {
        assert!(matches!(
            parse_permission_response("n"),
            PermissionDecision::Deny { .. }
        ));
        assert!(matches!(
            parse_permission_response(""),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn parse_allow_all_response() {
        assert_eq!(parse_permission_response("a"), PermissionDecision::AllowAll);
        assert_eq!(
            parse_permission_response("all"),
            PermissionDecision::AllowAll
        );
    }

    #[test]
    fn parse_view_response() {
        assert_eq!(
            parse_permission_response("v"),
            PermissionDecision::ViewInput
        );
    }
}
