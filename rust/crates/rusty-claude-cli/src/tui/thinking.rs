use std::time::Duration;

use crate::tui::theme::Theme;

/// Generate animated thinking indicator frames (dot-wave).
pub struct ThinkingFrames;

impl ThinkingFrames {
    /// Returns an iterator that cycles through animation frames forever.
    pub fn frames() -> impl Iterator<Item = &'static str> {
        [
            "\x1b[38;5;13m  ●\x1b[0m",
            "\x1b[38;5;13m  ●●\x1b[0m",
            "\x1b[38;5;13m  ●●●\x1b[0m",
            "\x1b[38;5;13m  ●●●●\x1b[0m",
            "\x1b[38;5;13m  ●●●●●\x1b[0m",
            "\x1b[38;5;13m  ●●●●\x1b[0m",
            "\x1b[38;5;13m  ●●●\x1b[0m",
            "\x1b[38;5;13m  ●●\x1b[0m",
        ]
        .iter()
        .copied()
        .cycle()
    }

    /// Frame delay for smooth animation.
    pub fn frame_delay() -> Duration {
        Duration::from_millis(120)
    }
}

/// Format the static "Reasoned for X.Xs" line after thinking completes.
pub fn format_thinking_completed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    format!(
        "{}\u{25b6} Reasoned for {secs:.1}s{}",
        Theme::THINKING,
        Theme::RESET
    )
}

/// Render a short inline thinking indicator for non-animated use.
pub fn render_thinking_inline(char_count: Option<usize>, redacted: bool) -> String {
    let summary = if redacted {
        format!(
            "{}\u{25b6} Thinking block hidden by provider{}",
            Theme::THINKING,
            Theme::RESET
        )
    } else if let Some(char_count) = char_count {
        format!(
            "{}\u{25b6} Reasoning ({char_count} chars){}",
            Theme::THINKING,
            Theme::RESET
        )
    } else {
        format!("{}\u{25b6} Reasoning{}", Theme::THINKING, Theme::RESET)
    };
    format!("\n{summary}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_cycles_indefinitely() {
        let frames: Vec<&str> = ThinkingFrames::frames().take(16).collect();
        // 8 unique frames, then repeats
        assert_eq!(frames.len(), 16);
        let first = frames[0];
        assert_eq!(frames[8], first); // 9th frame = 1st (cycle)
    }

    #[test]
    fn thinking_completed_formats_seconds() {
        let result = format_thinking_completed(Duration::from_secs_f64(3.5));
        assert!(result.contains("Reasoned for"));
        assert!(result.contains("3.5s"));
        assert!(result.contains(Theme::THINKING)); // magenta
    }

    #[test]
    fn thinking_inline_with_char_count() {
        let result = render_thinking_inline(Some(42), false);
        assert!(result.contains("Reasoning"));
        assert!(result.contains("42 chars"));
        assert!(result.contains(Theme::THINKING));
    }

    #[test]
    fn thinking_inline_redacted() {
        let result = render_thinking_inline(None, true);
        assert!(result.contains("hidden by provider"));
    }

    #[test]
    fn thinking_inline_without_count() {
        let result = render_thinking_inline(None, false);
        assert!(result.contains("Reasoning"));
        assert!(!result.contains("chars"));
    }
}
