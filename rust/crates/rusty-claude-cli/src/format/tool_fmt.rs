use serde_json;

use std::fmt::Write as _;

use crate::tui::theme::Theme;
use crate::tui::tool_panel::{collapse_tool_output, ToolDisplayConfig};

const DISPLAY_TRUNCATION_NOTICE: &str =
    "\x1b[2m… output truncated for display; full result preserved in session.\x1b[0m";
const READ_DISPLAY_MAX_LINES: usize = 80;
const READ_DISPLAY_MAX_CHARS: usize = 6_000;

pub(crate) fn format_tool_call_start(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));

    let detail = match name {
        "bash" | "Bash" => format_bash_call(&parsed),
        "read_file" | "Read" => {
            let path = extract_tool_path(&parsed);
            format!("{}📄 Reading {path}…{}", Theme::DIM, Theme::RESET)
        }
        "write_file" | "Write" => {
            let path = extract_tool_path(&parsed);
            let lines = parsed
                .get("content")
                .and_then(|value| value.as_str())
                .map_or(0, |content| content.lines().count());
            format!(
                "{}✏️ Writing {path}{} {}({lines} lines){}",
                Theme::SUCCESS_BOLD,
                Theme::RESET,
                Theme::DIM,
                Theme::RESET
            )
        }
        "edit_file" | "Edit" => {
            let path = extract_tool_path(&parsed);
            let old_value = parsed
                .get("old_string")
                .or_else(|| parsed.get("oldString"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let new_value = parsed
                .get("new_string")
                .or_else(|| parsed.get("newString"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            format!(
                "{}📝 Editing {path}{}{}",
                Theme::WARNING,
                Theme::RESET,
                format_patch_preview(old_value, new_value)
                    .map(|preview| format!("\n{preview}"))
                    .unwrap_or_default()
            )
        }
        "glob_search" | "Glob" => format_search_start("🔎 Glob", &parsed),
        "grep_search" | "Grep" => format_search_start("🔎 Grep", &parsed),
        "web_search" | "WebSearch" => parsed
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or("?")
            .to_string(),
        _ => summarize_tool_payload(input),
    };

    let border = "─".repeat(name.len() + 8);
    format!(
        "{}╭─ {}{}{}{} ─╮{}\n{}{} {detail}\n{}╰{border}╯{}",
        Theme::MUTED,
        Theme::HIGHLIGHT,
        name,
        Theme::RESET,
        Theme::MUTED,
        Theme::RESET,
        Theme::MUTED,
        Theme::RESET,
        Theme::MUTED,
        Theme::RESET,
    )
}

pub(crate) fn format_tool_result(name: &str, output: &str, is_error: bool) -> String {
    let icon = if is_error {
        format!("{}{}{}", Theme::ERROR_BRIGHT, "✗", Theme::RESET)
    } else {
        format!("{}{}{}", Theme::SUCCESS_BOLD, "✓", Theme::RESET)
    };
    if is_error {
        let summary = truncate_for_summary(output.trim(), 160);
        return if summary.is_empty() {
            format!("{icon} {}{}{}", Theme::MUTED, name, Theme::RESET)
        } else {
            format!(
                "{icon} {}{}{}\n{}{}{}",
                Theme::MUTED,
                name,
                Theme::RESET,
                Theme::ERROR,
                summary,
                Theme::RESET
            )
        };
    }

    let parsed: serde_json::Value =
        serde_json::from_str(output).unwrap_or(serde_json::Value::String(output.to_string()));
    match name {
        "bash" | "Bash" => format_bash_result(&icon, &parsed),
        "read_file" | "Read" => format_read_result(&icon, &parsed),
        "write_file" | "Write" => format_write_result(&icon, &parsed),
        "edit_file" | "Edit" => format_edit_result(&icon, &parsed),
        "glob_search" | "Glob" => format_glob_result(&icon, &parsed),
        "grep_search" | "Grep" => format_grep_result(&icon, &parsed),
        _ => format_generic_tool_result(&icon, name, &parsed),
    }
}

pub(crate) fn extract_tool_path(parsed: &serde_json::Value) -> String {
    parsed
        .get("file_path")
        .or_else(|| parsed.get("filePath"))
        .or_else(|| parsed.get("path"))
        .and_then(|value| value.as_str())
        .unwrap_or("?")
        .to_string()
}

pub(crate) fn format_search_start(label: &str, parsed: &serde_json::Value) -> String {
    let pattern = parsed
        .get("pattern")
        .and_then(|value| value.as_str())
        .unwrap_or("?");
    let scope = parsed
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or(".");
    format!("{label} {pattern}\n{}{scope}{}", Theme::DIM, Theme::RESET)
}

pub(crate) fn format_patch_preview(old_value: &str, new_value: &str) -> Option<String> {
    if old_value.is_empty() && new_value.is_empty() {
        return None;
    }
    Some(format!(
        "{}- {}{}\n{}+ {}{}",
        Theme::ERROR,
        truncate_for_summary(first_visible_line(old_value), 72),
        Theme::RESET,
        Theme::SUCCESS,
        truncate_for_summary(first_visible_line(new_value), 72),
        Theme::RESET,
    ))
}

pub(crate) fn format_bash_call(parsed: &serde_json::Value) -> String {
    let command = parsed
        .get("command")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if command.is_empty() {
        String::new()
    } else {
        format!(
            "{} $ {} {}",
            Theme::COMMAND_BG,
            truncate_for_summary(command, 160),
            Theme::RESET,
        )
    }
}

pub(crate) fn first_visible_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
}

pub(crate) fn format_bash_result(icon: &str, parsed: &serde_json::Value) -> String {
    let mut lines = vec![format!("{icon} {}{}{}", Theme::MUTED, "bash", Theme::RESET)];
    if let Some(task_id) = parsed
        .get("backgroundTaskId")
        .and_then(|value| value.as_str())
    {
        write!(&mut lines[0], " backgrounded ({task_id})").expect("write to string");
    } else if let Some(status) = parsed
        .get("returnCodeInterpretation")
        .and_then(|value| value.as_str())
        .filter(|status| !status.is_empty())
    {
        write!(&mut lines[0], " {status}").expect("write to string");
    }

    let config = ToolDisplayConfig::default();
    if let Some(stdout) = parsed.get("stdout").and_then(|value| value.as_str()) {
        if !stdout.trim().is_empty() {
            let collapsed = collapse_tool_output(stdout, "bash", false, &config);
            lines.push(collapsed.visible);
        }
    }
    if let Some(stderr) = parsed.get("stderr").and_then(|value| value.as_str()) {
        if !stderr.trim().is_empty() {
            let collapsed = collapse_tool_output(stderr, "bash", true, &config);
            lines.push(format!(
                "{}{}{}",
                Theme::ERROR,
                collapsed.visible,
                Theme::RESET
            ));
        }
    }

    lines.join("\n\n")
}

pub(crate) fn format_read_result(icon: &str, parsed: &serde_json::Value) -> String {
    let file = parsed.get("file").unwrap_or(parsed);
    let path = extract_tool_path(file);
    let start_line = file
        .get("startLine")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let num_lines = file
        .get("numLines")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let total_lines = file
        .get("totalLines")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(num_lines);
    let content = file
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let end_line = start_line.saturating_add(num_lines.saturating_sub(1));

    format!(
        "{icon} {}📄 Read {path} (lines {}-{} of {}){}\n{}",
        Theme::DIM,
        start_line,
        end_line.max(start_line),
        total_lines,
        Theme::RESET,
        truncate_output_for_display(content, READ_DISPLAY_MAX_LINES, READ_DISPLAY_MAX_CHARS)
    )
}

pub(crate) fn format_write_result(icon: &str, parsed: &serde_json::Value) -> String {
    let path = extract_tool_path(parsed);
    let kind = parsed
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("write");
    let line_count = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .map_or(0, |content| content.lines().count());
    format!(
        "{icon} {}✏️ {} {path}{} {}({line_count} lines){}",
        Theme::SUCCESS_BOLD,
        if kind == "create" { "Wrote" } else { "Updated" },
        Theme::RESET,
        Theme::DIM,
        Theme::RESET,
    )
}

pub(crate) fn format_structured_patch_preview(parsed: &serde_json::Value) -> Option<String> {
    let hunks = parsed.get("structuredPatch")?.as_array()?;
    let mut preview = Vec::new();
    for hunk in hunks.iter().take(2) {
        let lines = hunk.get("lines")?.as_array()?;
        for line in lines.iter().filter_map(|value| value.as_str()).take(6) {
            match line.chars().next() {
                Some('+') => preview.push(format!("{}{}{}", Theme::SUCCESS, line, Theme::RESET)),
                Some('-') => preview.push(format!("{}{}{}", Theme::ERROR, line, Theme::RESET)),
                _ => preview.push(line.to_string()),
            }
        }
    }
    if preview.is_empty() {
        None
    } else {
        Some(preview.join("\n"))
    }
}

pub(crate) fn format_edit_result(icon: &str, parsed: &serde_json::Value) -> String {
    let path = extract_tool_path(parsed);
    let suffix = if parsed
        .get("replaceAll")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        " (replace all)"
    } else {
        ""
    };
    let preview = format_structured_patch_preview(parsed).or_else(|| {
        let old_value = parsed
            .get("oldString")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let new_value = parsed
            .get("newString")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        format_patch_preview(old_value, new_value)
    });

    match preview {
        Some(preview) => format!(
            "{icon} {}📝 Edited {path}{suffix}{}\n{preview}",
            Theme::WARNING,
            Theme::RESET
        ),
        None => format!(
            "{icon} {}📝 Edited {path}{suffix}{}",
            Theme::WARNING,
            Theme::RESET
        ),
    }
}

pub(crate) fn format_glob_result(icon: &str, parsed: &serde_json::Value) -> String {
    let num_files = parsed
        .get("numFiles")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let filenames = parsed
        .get("filenames")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|value| value.as_str())
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if filenames.is_empty() {
        format!(
            "{icon} {}glob_search{} matched {num_files} files",
            Theme::MUTED,
            Theme::RESET
        )
    } else {
        format!(
            "{icon} {}glob_search{} matched {num_files} files\n{filenames}",
            Theme::MUTED,
            Theme::RESET
        )
    }
}

pub(crate) fn format_grep_result(icon: &str, parsed: &serde_json::Value) -> String {
    let num_matches = parsed
        .get("numMatches")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let num_files = parsed
        .get("numFiles")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let content = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let filenames = parsed
        .get("filenames")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|value| value.as_str())
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let summary = format!(
        "{icon} {}grep_search{} {num_matches} matches across {num_files} files",
        Theme::MUTED,
        Theme::RESET,
    );
    if !content.trim().is_empty() {
        let collapsed =
            collapse_tool_output(content, "grep_search", false, &ToolDisplayConfig::default());
        format!("{summary}\n{}", collapsed.visible)
    } else if !filenames.is_empty() {
        format!("{summary}\n{filenames}")
    } else {
        summary
    }
}

pub(crate) fn format_generic_tool_result(
    icon: &str,
    name: &str,
    parsed: &serde_json::Value,
) -> String {
    let rendered_output = match parsed {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            serde_json::to_string_pretty(parsed).unwrap_or_else(|_| parsed.to_string())
        }
        _ => parsed.to_string(),
    };
    let collapsed =
        collapse_tool_output(&rendered_output, name, false, &ToolDisplayConfig::default());
    let preview = collapsed.visible;

    if preview.is_empty() {
        format!("{icon} {}{}{}", Theme::MUTED, name, Theme::RESET)
    } else if preview.contains('\n') {
        format!("{icon} {}{}{}\n{preview}", Theme::MUTED, name, Theme::RESET)
    } else {
        format!("{icon} {}{}:{} {preview}", Theme::MUTED, name, Theme::RESET)
    }
}

pub(crate) fn summarize_tool_payload(payload: &str) -> String {
    let compact = match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => value.to_string(),
        Err(_) => payload.trim().to_string(),
    };
    truncate_for_summary(&compact, 96)
}

pub(crate) fn truncate_for_summary(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

pub(crate) fn truncate_output_for_display(
    content: &str,
    max_lines: usize,
    max_chars: usize,
) -> String {
    let original = content.trim_end_matches('\n');
    if original.is_empty() {
        return String::new();
    }

    let mut preview_lines = Vec::new();
    let mut used_chars = 0usize;
    let mut truncated = false;

    for (index, line) in original.lines().enumerate() {
        if index >= max_lines {
            truncated = true;
            break;
        }

        let newline_cost = usize::from(!preview_lines.is_empty());
        let available = max_chars.saturating_sub(used_chars + newline_cost);
        if available == 0 {
            truncated = true;
            break;
        }

        let line_chars = line.chars().count();
        if line_chars > available {
            preview_lines.push(line.chars().take(available).collect::<String>());
            truncated = true;
            break;
        }

        preview_lines.push(line.to_string());
        used_chars += newline_cost + line_chars;
    }

    let mut preview = preview_lines.join("\n");
    if truncated {
        if !preview.is_empty() {
            preview.push('\n');
        }
        preview.push_str(DISPLAY_TRUNCATION_NOTICE);
    }
    preview
}
