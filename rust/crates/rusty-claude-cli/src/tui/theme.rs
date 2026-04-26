/// Named semantic color tokens for the TUI.
///
/// All TUI modules should reference these constants instead of
/// hard-coding ANSI escape sequences. This makes theme switching
/// a single-point change.
pub struct Theme;

/// ── Base palette ──────────────────────────────────────────────────────────
impl Theme {
    /// Dark grey/dim for secondary info (status bar, truncation notices).
    pub const DIM: &'static str = "\x1b[2m";
    /// Reset all attributes.
    pub const RESET: &'static str = "\x1b[0m";

    /// ── 256-color semantic tokens ──────────────────────────────────────────

    /// Green: success indicators, additions.
    pub const SUCCESS: &'static str = "\x1b[38;5;70m";
    /// Red: error indicators, deletions.
    pub const ERROR: &'static str = "\x1b[38;5;203m";
    /// Bright red: critical errors.
    pub const ERROR_BRIGHT: &'static str = "\x1b[1;31m";
    /// Cyan: highlight (tool names, hunk headers).
    pub const HIGHLIGHT: &'static str = "\x1b[1;36m";
    /// Magenta: thinking/reasoning indicators.
    pub const THINKING: &'static str = "\x1b[38;5;13m";
    /// Yellow: warning, permission prompts.
    pub const WARNING: &'static str = "\x1b[1;33m";
    /// Grey: borders, secondary labels, file headers.
    pub const MUTED: &'static str = "\x1b[38;5;245m";
    /// White on dark grey background: command display (bash inline).
    pub const COMMAND_BG: &'static str = "\x1b[48;5;236;38;5;255m";
    /// Bold green: file write/create.
    pub const SUCCESS_BOLD: &'static str = "\x1b[1;32m";
    /// Bold yellow: file edit.
    pub const EDIT: &'static str = "\x1b[1;33m";

    /// ── Composite styles ───────────────────────────────────────────────────

    /// Truncation notice suffix.
    pub fn truncation_notice() -> String {
        format!(
            "{}… output truncated for display; full result preserved in session.{}",
            Self::DIM,
            Self::RESET
        )
    }

    /// Status bar foreground.
    pub fn status_bar_fg() -> &'static str {
        "\x1b[90m"
    }

    /// Permission prompt border.
    pub fn permission_border() -> String {
        format!(
            "{}────────────────────────────────────────────────{}",
            Self::MUTED,
            Self::RESET
        )
    }
}

/// ── Unit tests ────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_constants_are_non_empty() {
        assert!(!Theme::DIM.is_empty());
        assert!(!Theme::RESET.is_empty());
        assert!(!Theme::SUCCESS.is_empty());
        assert!(!Theme::ERROR.is_empty());
        assert!(!Theme::HIGHLIGHT.is_empty());
        assert!(!Theme::THINKING.is_empty());
        assert!(!Theme::WARNING.is_empty());
        assert!(!Theme::MUTED.is_empty());
        assert!(!Theme::EDIT.is_empty());
    }

    #[test]
    fn truncation_notice_contains_dim_and_reset() {
        let notice = Theme::truncation_notice();
        assert!(notice.contains(Theme::DIM));
        assert!(notice.contains(Theme::RESET));
        assert!(notice.contains("truncated for display"));
    }
}
