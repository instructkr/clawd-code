use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::tui::theme::Theme;

/// A recorded tool call event in the session.
#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub step: usize,
    pub name: String,
    pub started_at: Instant,
    pub completed_at: Option<Instant>,
    pub is_error: bool,
    pub was_truncated: bool,
    pub output_lines: usize,
}

/// Accumulator for building a tool call timeline during a turn.
///
/// Wrapped in `Arc<Mutex<>>` so it can be shared between the streaming
/// client (which records `start_tool`) and the tool executor (which
/// records `complete_tool`).
#[derive(Debug, Default, Clone)]
pub struct SharedToolCallTimeline(pub Arc<Mutex<ToolCallTimeline>>);

/// Accumulator for building a tool call timeline during a turn.
#[derive(Debug, Default)]
pub struct ToolCallTimeline {
    events: Vec<ToolCallEvent>,
    start: Option<Instant>,
}

impl SharedToolCallTimeline {
    /// Lock the inner timeline and call a function on it.
    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut ToolCallTimeline) -> R,
    {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }
}

impl ToolCallTimeline {
    /// Create a new empty timeline.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the start of a tool call.
    pub fn start_tool(&mut self, name: &str) {
        if self.start.is_none() {
            self.start = Some(Instant::now());
        }
        self.events.push(ToolCallEvent {
            step: self.events.len() + 1,
            name: name.to_string(),
            started_at: Instant::now(),
            completed_at: None,
            is_error: false,
            was_truncated: false,
            output_lines: 0,
        });
    }

    /// Mark the most recent tool call as completed.
    pub fn complete_tool(&mut self, is_error: bool, was_truncated: bool, output_lines: usize) {
        if let Some(event) = self.events.last_mut() {
            event.completed_at = Some(Instant::now());
            event.is_error = is_error;
            event.was_truncated = was_truncated;
            event.output_lines = output_lines;
        }
    }

    /// Get the current events.
    pub fn events(&self) -> &[ToolCallEvent] {
        &self.events
    }

    /// Total elapsed time since the first tool call started.
    pub fn total_elapsed(&self) -> Option<std::time::Duration> {
        self.start.map(|s| s.elapsed())
    }

    /// Render the timeline as a string.
    pub fn render(&self) -> String {
        if self.events.is_empty() {
            return String::new();
        }

        let mut out = String::new();
        writeln!(out, "{}── Tool calls ──{}", Theme::MUTED, Theme::RESET).expect("write to string");

        for event in &self.events {
            let elapsed = event
                .completed_at
                .map(|c| c.duration_since(event.started_at))
                .unwrap_or_default();
            let status_icon = if event.is_error {
                format!("{}✗{}", Theme::ERROR_BRIGHT, Theme::RESET)
            } else {
                format!("{}✓{}", Theme::SUCCESS_BOLD, Theme::RESET)
            };
            let truncated_mark = if event.was_truncated {
                " (truncated)"
            } else {
                ""
            };
            let secs = elapsed.as_secs_f64();
            writeln!(
                out,
                "  {}. {status_icon} {h}{name}{r}  {d}{secs:.1}s  {lines} lines{truncated_mark}{r}",
                event.step,
                name = event.name,
                secs = secs,
                lines = event.output_lines,
                h = Theme::HIGHLIGHT,
                r = Theme::RESET,
                d = Theme::DIM,
            )
            .expect("write to string");
        }

        if let Some(elapsed) = self.total_elapsed() {
            writeln!(
                out,
                "\n  {d}Total: {secs:.1}s across {count} tool call(s){r}",
                d = Theme::DIM,
                r = Theme::RESET,
                secs = elapsed.as_secs_f64(),
                count = self.events.len(),
            )
            .expect("write to string");
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn empty_timeline_renders_nothing() {
        let timeline = ToolCallTimeline::new();
        assert!(timeline.render().is_empty());
    }

    #[test]
    fn single_tool_call_appears_in_render() {
        let mut timeline = ToolCallTimeline::new();
        timeline.start_tool("bash");
        sleep(Duration::from_millis(10));
        timeline.complete_tool(false, false, 5);
        let rendered = timeline.render();
        assert!(rendered.contains("bash"));
        assert!(rendered.contains("✓"));
        assert!(rendered.contains("Tool calls"));
    }

    #[test]
    fn error_tool_call_shows_error_icon() {
        let mut timeline = ToolCallTimeline::new();
        timeline.start_tool("read_file");
        timeline.complete_tool(true, false, 0);
        let rendered = timeline.render();
        assert!(rendered.contains("✗"));
    }

    #[test]
    fn truncated_tool_call_marks_truncated() {
        let mut timeline = ToolCallTimeline::new();
        timeline.start_tool("bash");
        timeline.complete_tool(false, true, 100);
        let rendered = timeline.render();
        assert!(rendered.contains("truncated"));
    }

    #[test]
    fn multiple_tool_calls_are_numbered() {
        let mut timeline = ToolCallTimeline::new();
        timeline.start_tool("read_file");
        timeline.complete_tool(false, false, 10);
        timeline.start_tool("edit_file");
        timeline.complete_tool(false, false, 3);
        let rendered = timeline.render();
        assert!(rendered.contains("1."));
        assert!(rendered.contains("2."));
        assert!(rendered.contains("2 tool call(s)"));
    }

    #[test]
    fn events_reflects_count() {
        let mut timeline = ToolCallTimeline::new();
        assert_eq!(timeline.events().len(), 0);
        timeline.start_tool("bash");
        assert_eq!(timeline.events().len(), 1);
    }
}
