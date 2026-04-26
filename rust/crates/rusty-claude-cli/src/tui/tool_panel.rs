use std::fmt::Write as _;

use crate::tui::theme::Theme;

/// Configuration for tool output truncation.
pub struct ToolDisplayConfig {
    pub visible_lines: usize,
    pub max_chars: usize,
}

impl Default for ToolDisplayConfig {
    fn default() -> Self {
        Self {
            visible_lines: 10,
            max_chars: 4_000,
        }
    }
}

/// Result of collapsing tool output for display.
pub struct CollapsedToolOutput {
    /// The visible portion (first N lines).
    pub visible: String,
    /// Total lines in the full output.
    pub total_lines: usize,
    /// Whether the output was truncated.
    pub was_truncated: bool,
    /// Summary line for the collapsed indicator.
    pub summary: String,
}

const DISPLAY_TRUNCATION_NOTICE: &str =
    "\x1b[2m… output truncated for display; full result preserved in session.\x1b[0m";

/// Collapse tool output to the configured visible line count.
/// Returns a struct with the visible portion and metadata.
pub fn collapse_tool_output(
    output: &str,
    tool_name: &str,
    is_error: bool,
    config: &ToolDisplayConfig,
) -> CollapsedToolOutput {
    let lines: Vec<&str> = output.lines().collect();
    let total_lines = lines.len();
    let was_truncated = total_lines > config.visible_lines;

    let mut visible = if was_truncated {
        lines
            .iter()
            .take(config.visible_lines)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        output.to_string()
    };

    // Also enforce max_chars (character count) on the visible portion
    if visible.chars().count() > config.max_chars {
        let prefix: String = visible
            .chars()
            .take(config.max_chars.saturating_sub(1))
            .collect();
        visible = format!("{prefix}…");
    }

    let icon = if is_error { "✗" } else { "✓" };
    let summary = if was_truncated {
        format!(
            "{} {} ({} lines) — full output in session · [scroll up or /debugToolCall to inspect]",
            icon, tool_name, total_lines
        )
    } else {
        format!("{} {}", icon, tool_name)
    };

    if was_truncated {
        write!(&mut visible, "\n{}", DISPLAY_TRUNCATION_NOTICE).expect("write to string");
    }

    CollapsedToolOutput {
        visible,
        total_lines,
        was_truncated,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_is_not_truncated() {
        let config = ToolDisplayConfig::default();
        let result = collapse_tool_output("line 1\nline 2\n", "bash", false, &config);
        assert!(!result.was_truncated);
        assert_eq!(result.total_lines, 2);
    }

    #[test]
    fn long_output_is_truncated() {
        let config = ToolDisplayConfig {
            visible_lines: 3,
            max_chars: 100_000,
        };
        let input = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = collapse_tool_output(&input, "bash", false, &config);
        assert!(result.was_truncated);
        assert_eq!(result.total_lines, 20);
        assert!(result.visible.contains("line 1"));
        assert!(result.visible.contains("line 3"));
        assert!(!result.visible.contains("line 4"));
    }

    #[test]
    fn error_tool_gets_error_icon() {
        let config = ToolDisplayConfig::default();
        let result = collapse_tool_output("error", "bash", true, &config);
        assert!(result.summary.starts_with('✗'));
    }

    #[test]
    fn success_tool_gets_check_icon() {
        let config = ToolDisplayConfig::default();
        let result = collapse_tool_output("ok", "bash", false, &config);
        assert!(result.summary.starts_with('✓'));
    }

    #[test]
    fn empty_output_is_not_truncated() {
        let config = ToolDisplayConfig::default();
        let result = collapse_tool_output("", "bash", false, &config);
        assert!(!result.was_truncated);
        assert_eq!(result.total_lines, 0);
    }

    #[test]
    fn max_chars_enforced_on_visible() {
        let config = ToolDisplayConfig {
            visible_lines: 100,
            max_chars: 20,
        };
        let input = "a".repeat(50);
        let result = collapse_tool_output(&input, "bash", false, &config);
        assert!(result.visible.chars().count() <= 20);
        assert!(result.visible.ends_with('…'));
    }

    #[test]
    fn summary_contains_line_count() {
        let config = ToolDisplayConfig {
            visible_lines: 2,
            max_chars: 100_000,
        };
        let input = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = collapse_tool_output(&input, "bash", false, &config);
        assert!(result.summary.contains("10 lines"));
    }
}
