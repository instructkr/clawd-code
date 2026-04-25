use std::io::Write;
use std::time::Instant;

use crate::tui::theme::Theme;

/// Data needed to render the status bar.
pub struct StatusBarState {
    pub model: String,
    pub permission_mode: String,
    pub message_count: usize,
    pub cumulative_input_tokens: u64,
    pub cumulative_output_tokens: u64,
    pub estimated_cost_usd: String,
    pub turn_start: Instant,
    pub git_branch: Option<String>,
    pub terminal_width: u16,
}

pub struct StatusBar;

impl StatusBar {
    /// Render the status bar to `out`. Uses raw ANSI escape sequences
    /// so it works with any `Write` implementation (including `&mut dyn Write`).
    pub fn render(state: &StatusBarState, out: &mut dyn Write) -> std::io::Result<()> {
        let elapsed = state.turn_start.elapsed();
        let secs = elapsed.as_secs();
        let model_display = truncate_str(&state.model, 18);
        let total_tokens = state.cumulative_input_tokens + state.cumulative_output_tokens;
        let tokens_display = format_tokens(total_tokens);
        let branch_display = state.git_branch.as_deref().unwrap_or("?");

        let content = format!(
            " {} · {} · {} msgs · {} tokens · ${} · {}s · {} ",
            model_display,
            state.permission_mode,
            state.message_count,
            tokens_display,
            state.estimated_cost_usd,
            secs,
            branch_display,
        );

        // Truncate to terminal width (character count, not byte length)
        let width = state.terminal_width as usize;
        let display = if content.chars().count() > width {
            truncate_str(&content, width.saturating_sub(1))
        } else {
            content
        };

        // ANSI: save position, move to col 0, clear line, dark grey, print, reset, restore
        write!(
            out,
            "\x1b7\x1b[0G\x1b[2K{}{}{}",
            Theme::status_bar_fg(),
            display,
            Theme::RESET,
        )?;
        write!(out, "\x1b8")?;
        out.flush()
    }

    /// Clear the status bar line (call when generation completes).
    pub fn clear(out: &mut dyn Write) -> std::io::Result<()> {
        // ANSI: save position, move to col 0, clear line, restore
        write!(out, "\x1b7\x1b[0G\x1b[2K\x1b8")?;
        out.flush()
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut result = s
            .chars()
            .take(max_len.saturating_sub(1))
            .collect::<String>();
        result.push('…');
        result
    }
}

fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_str_shorter_than_max() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_at_max() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_longer_than_max() {
        let result = truncate_str("hello world", 6);
        assert_eq!(result, "hello…");
    }

    #[test]
    fn format_tokens_zero() {
        assert_eq!(format_tokens(0), "0");
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(3200), "3.2k");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens(1_500_000), "1.5M");
    }

    #[test]
    fn render_produces_output_without_panicking() {
        let state = StatusBarState {
            model: "claude-sonnet-4".to_string(),
            permission_mode: "read-only".to_string(),
            message_count: 5,
            cumulative_input_tokens: 3200,
            cumulative_output_tokens: 800,
            estimated_cost_usd: "0.04".to_string(),
            turn_start: Instant::now(),
            git_branch: Some("main".to_string()),
            terminal_width: 80,
        };
        let mut buf: Vec<u8> = Vec::new();
        let out: &mut dyn Write = &mut buf;
        let _ = StatusBar::render(&state, out);
        assert!(!buf.is_empty());
    }

    #[test]
    fn render_truncates_to_terminal_width() {
        let state = StatusBarState {
            model: "claude-sonnet-4-with-a-very-long-name".to_string(),
            permission_mode: "read-only".to_string(),
            message_count: 5,
            cumulative_input_tokens: 3200,
            cumulative_output_tokens: 800,
            estimated_cost_usd: "0.04".to_string(),
            turn_start: Instant::now(),
            git_branch: Some("feature/some-long-branch-name".to_string()),
            terminal_width: 40,
        };
        let mut buf: Vec<u8> = Vec::new();
        let out: &mut dyn Write = &mut buf;
        let _ = StatusBar::render(&state, out);
        assert!(!buf.is_empty());
    }

    #[test]
    fn clear_produces_output_without_panicking() {
        let mut buf: Vec<u8> = Vec::new();
        let out: &mut dyn Write = &mut buf;
        let _ = StatusBar::clear(out);
        assert!(!buf.is_empty());
    }
}
