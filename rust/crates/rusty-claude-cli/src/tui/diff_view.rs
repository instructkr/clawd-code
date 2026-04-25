use std::fmt::Write as _;

use crate::tui::theme::Theme;

/// A single line in a parsed unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Context(String),
    Addition(String),
    Deletion(String),
    HunkHeader(String),
    FileHeader(String),
    Binary(String),
}

/// Parse raw git diff output into structured lines.
pub fn parse_unified_diff(diff: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    for raw_line in diff.lines() {
        if raw_line.is_empty() {
            continue;
        }
        if raw_line.starts_with("diff --git") || raw_line.starts_with("index ") {
            lines.push(DiffLine::FileHeader(raw_line.to_string()));
        } else if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
            lines.push(DiffLine::FileHeader(raw_line.to_string()));
        } else if raw_line.starts_with("@@") && raw_line.contains("@@") {
            lines.push(DiffLine::HunkHeader(raw_line.to_string()));
        } else if raw_line.starts_with("Binary files") {
            lines.push(DiffLine::Binary(raw_line.to_string()));
        } else if raw_line.starts_with('+') && !raw_line.starts_with("+++") {
            lines.push(DiffLine::Addition(raw_line[1..].to_string()));
        } else if raw_line.starts_with('-') && !raw_line.starts_with("---") {
            lines.push(DiffLine::Deletion(raw_line[1..].to_string()));
        } else if raw_line.starts_with(' ') {
            lines.push(DiffLine::Context(raw_line[1..].to_string()));
        } else {
            // Any other lines (no space prefix) — pass as context
            lines.push(DiffLine::Context(raw_line.to_string()));
        }
    }
    lines
}

/// Count additions and deletions in parsed diff lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffCounts {
    pub additions: usize,
    pub deletions: usize,
}

/// Count additions and deletions in a set of parsed diff lines.
pub fn count_diff_lines(lines: &[DiffLine]) -> DiffCounts {
    let mut counts = DiffCounts::default();
    for line in lines {
        match line {
            DiffLine::Addition(_) => counts.additions += 1,
            DiffLine::Deletion(_) => counts.deletions += 1,
            _ => {}
        }
    }
    counts
}

/// Count files modified in parsed diff lines.
pub fn count_diff_files(lines: &[DiffLine]) -> usize {
    lines
        .iter()
        .filter(|l| matches!(l, DiffLine::FileHeader(h) if h.starts_with("diff --git")))
        .count()
}

/// Render parsed diff lines with ANSI colors.
///
/// - Additions are green
/// - Deletions are red
/// - Hunk headers are cyan
/// - File headers are dim/bold
pub fn render_colored_diff(lines: &[DiffLine]) -> String {
    let mut out = String::new();
    for line in lines {
        match line {
            DiffLine::Context(text) => {
                writeln!(out, " {text}").expect("write to string");
            }
            DiffLine::Addition(text) => {
                writeln!(out, "{}+{}{}", Theme::SUCCESS, text, Theme::RESET)
                    .expect("write to string");
            }
            DiffLine::Deletion(text) => {
                writeln!(out, "{}-{}{}", Theme::ERROR, text, Theme::RESET)
                    .expect("write to string");
            }
            DiffLine::HunkHeader(text) => {
                writeln!(out, "{}{}{}", Theme::HIGHLIGHT, text, Theme::RESET)
                    .expect("write to string");
            }
            DiffLine::FileHeader(text) => {
                // bold + muted — no dedicated Theme:: constant yet
                writeln!(out, "\x1b[1;38;5;245m{text}\x1b[0m").expect("write to string");
            }
            DiffLine::Binary(text) => {
                writeln!(out, "{}{}{}", Theme::MUTED, text, Theme::RESET).expect("write to string");
            }
        }
    }
    out
}

/// Render a compact file-level summary of a parsed diff.
pub fn render_diff_summary(lines: &[DiffLine]) -> String {
    let mut files: Vec<String> = Vec::new();
    let mut current_file = String::new();
    let mut file_counts = DiffCounts::default();

    for line in lines {
        match line {
            DiffLine::FileHeader(h) if h.starts_with("diff --git") => {
                // Emit previous file summary if any
                if !current_file.is_empty()
                    && (file_counts.additions > 0 || file_counts.deletions > 0)
                {
                    files.push(format!(
                        "  {}\t{}+{}{}/{}-{}{}",
                        current_file,
                        Theme::SUCCESS,
                        file_counts.additions,
                        Theme::RESET,
                        Theme::ERROR,
                        file_counts.deletions,
                        Theme::RESET,
                    ));
                }
                // Extract the b/ path from "diff --git a/path b/path"
                let path = h
                    .strip_prefix("diff --git ")
                    .and_then(|rest| rest.split_whitespace().nth(1))
                    .and_then(|p| p.strip_prefix("b/"))
                    .unwrap_or(h);
                current_file = path.to_string();
                file_counts = DiffCounts::default();
            }
            DiffLine::Addition(_) => file_counts.additions += 1,
            DiffLine::Deletion(_) => file_counts.deletions += 1,
            _ => {}
        }
    }
    // Emit last file
    if !current_file.is_empty() && (file_counts.additions > 0 || file_counts.deletions > 0) {
        files.push(format!(
            "  {}\t{}+{}{}/{}-{}{}",
            current_file,
            Theme::SUCCESS,
            file_counts.additions,
            Theme::RESET,
            Theme::ERROR,
            file_counts.deletions,
            Theme::RESET,
        ));
    }

    let total_files = count_diff_files(lines);
    let total_counts = count_diff_lines(lines);
    let mut out = format!(
        "{} file(s) changed\t{}+{}{} {}-{}{}\n",
        total_files,
        Theme::SUCCESS,
        total_counts.additions,
        Theme::RESET,
        Theme::ERROR,
        total_counts.deletions,
        Theme::RESET,
    );
    for file in &files {
        writeln!(out, "{file}").expect("write to string");
    }
    out
}

/// Render full colored diff with summary header.
pub fn format_colored_diff(diff: &str) -> String {
    if diff.trim().is_empty() {
        return format!("{}(empty diff){}", Theme::DIM, Theme::RESET);
    }
    let lines = parse_unified_diff(diff);
    let summary = render_diff_summary(&lines);
    let colored = render_colored_diff(&lines);
    format!("{summary}\n{colored}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_diff() -> &'static str {
        "diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,7 @@
 line one
 line two
+added line
 line three
-removed line
 line four
+another addition
"
    }

    #[test]
    fn parses_additions_deletions_and_context() {
        let lines = parse_unified_diff(sample_diff());
        let adds: Vec<_> = lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Addition(_)))
            .collect();
        let dels: Vec<_> = lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Deletion(_)))
            .collect();
        let contexts: Vec<_> = lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Context(_)))
            .collect();
        assert_eq!(adds.len(), 2);
        assert_eq!(dels.len(), 1);
        assert_eq!(contexts.len(), 4); // "line one" through "line four"
    }

    #[test]
    fn parses_hunk_header() {
        let lines = parse_unified_diff(sample_diff());
        let headers: Vec<_> = lines
            .iter()
            .filter(|l| matches!(l, DiffLine::HunkHeader(_)))
            .collect();
        assert_eq!(headers.len(), 1);
        if let DiffLine::HunkHeader(h) = &headers[0] {
            assert!(h.contains("@@"));
        }
    }

    #[test]
    fn counts_additions_and_deletions() {
        let lines = parse_unified_diff(sample_diff());
        let counts = count_diff_lines(&lines);
        assert_eq!(counts.additions, 2);
        assert_eq!(counts.deletions, 1);
    }

    #[test]
    fn counts_files() {
        let lines = parse_unified_diff(sample_diff());
        assert_eq!(count_diff_files(&lines), 1);
    }

    #[test]
    fn colored_output_contains_ansi_codes() {
        let lines = parse_unified_diff(sample_diff());
        let colored = render_colored_diff(&lines);
        assert!(colored.contains(Theme::SUCCESS)); // green for additions
        assert!(colored.contains(Theme::ERROR)); // red for deletions
        assert!(colored.contains(Theme::HIGHLIGHT)); // cyan for hunk headers
    }

    #[test]
    fn summary_shows_file_and_counts() {
        let lines = parse_unified_diff(sample_diff());
        let summary = render_diff_summary(&lines);
        assert!(summary.contains("1 file(s) changed"));
        assert!(summary.contains("+2"));
        assert!(summary.contains("-1"));
        assert!(summary.contains("src/main.rs"));
    }

    #[test]
    fn empty_diff_returns_placeholder() {
        let result = format_colored_diff("");
        assert!(result.contains("empty diff"));
    }

    #[test]
    fn multi_file_diff_shows_multiple_files() {
        let diff = "diff --git a/a.rs b/a.rs
index 1..2 100644
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-old_a
+new_a
diff --git a/b.rs b/b.rs
index 3..4 100644
--- a/b.rs
+++ b/b.rs
@@ -1 +1 @@
-old_b
+new_b
";
        let lines = parse_unified_diff(diff);
        assert_eq!(count_diff_files(&lines), 2);
        let summary = render_diff_summary(&lines);
        assert!(summary.contains("2 file(s) changed"));
        assert!(summary.contains("a.rs"));
        assert!(summary.contains("b.rs"));
    }

    #[test]
    fn binary_diff_is_parsed() {
        let diff = "diff --git a/image.png b/image.png
index 1..2 100644
Binary files a/image.png and b/image.png differ
";
        let lines = parse_unified_diff(diff);
        let binaries: Vec<_> = lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Binary(_)))
            .collect();
        assert_eq!(binaries.len(), 1);
    }
}
