use std::cmp::Reverse;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use glob::Pattern;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

/// Maximum file size that can be read (10 MB).
const MAX_READ_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum file size that can be written (10 MB).
const MAX_WRITE_SIZE: usize = 10 * 1024 * 1024;

fn staging_write_enabled() -> bool {
    matches!(
        std::env::var("CLAW_STAGING_WRITE").ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

fn staging_dir(workspace_root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("CLAW_STAGING_DIR") {
        return PathBuf::from(p);
    }
    workspace_root.join(".claw").join("staging")
}

fn now_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn structured_patch_to_unified_diff(path: &str, hunks: &[StructuredPatchHunk]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "--- a/{path}");
    let _ = writeln!(out, "+++ b/{path}");
    for h in hunks {
        let _ = writeln!(
            out,
            "@@ -{},{} +{},{} @@",
            h.old_start, h.old_lines, h.new_start, h.new_lines
        );
        for line in &h.lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Check whether a file appears to contain binary content by examining
/// the first chunk for NUL bytes.
fn is_binary_file(path: &Path) -> io::Result<bool> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut buffer = [0u8; 8192];
    let bytes_read = file.read(&mut buffer)?;
    Ok(buffer[..bytes_read].contains(&0))
}

/// Validate that a resolved path stays within the given workspace root.
/// Returns the canonical path on success, or an error if the path escapes
/// the workspace boundary (e.g. via `../` traversal or symlink).
#[allow(dead_code)]
fn validate_workspace_boundary(resolved: &Path, workspace_root: &Path) -> io::Result<()> {
    if !resolved.starts_with(workspace_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "path {} escapes workspace boundary {}",
                resolved.display(),
                workspace_root.display()
            ),
        ));
    }
    Ok(())
}

/// Text payload returned by file-reading operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextFilePayload {
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "numLines")]
    pub num_lines: usize,
    #[serde(rename = "startLine")]
    pub start_line: usize,
    #[serde(rename = "totalLines")]
    pub total_lines: usize,
}

/// Output envelope for the `read_file` tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadFileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    pub file: TextFilePayload,
}

/// Structured patch hunk emitted by write and edit operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StructuredPatchHunk {
    #[serde(rename = "oldStart")]
    pub old_start: usize,
    #[serde(rename = "oldLines")]
    pub old_lines: usize,
    #[serde(rename = "newStart")]
    pub new_start: usize,
    #[serde(rename = "newLines")]
    pub new_lines: usize,
    pub lines: Vec<String>,
}

/// Output envelope for full-file write operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteFileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<StructuredPatchHunk>,
    #[serde(rename = "originalFile")]
    pub original_file: Option<String>,
    #[serde(rename = "gitDiff")]
    pub git_diff: Option<serde_json::Value>,
}

/// Output envelope for targeted string-replacement edits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditFileOutput {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(rename = "oldString")]
    pub old_string: String,
    #[serde(rename = "newString")]
    pub new_string: String,
    #[serde(rename = "originalFile")]
    pub original_file: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<StructuredPatchHunk>,
    #[serde(rename = "userModified")]
    pub user_modified: bool,
    #[serde(rename = "replaceAll")]
    pub replace_all: bool,
    #[serde(rename = "gitDiff")]
    pub git_diff: Option<serde_json::Value>,
}

/// Result of a glob-based filename search.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobSearchOutput {
    #[serde(rename = "durationMs")]
    pub duration_ms: u128,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub truncated: bool,
}

/// Parameters accepted by the grep-style search tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepSearchInput {
    pub pattern: String,
    pub path: Option<String>,
    pub glob: Option<String>,
    #[serde(rename = "output_mode")]
    pub output_mode: Option<String>,
    #[serde(rename = "-B")]
    pub before: Option<usize>,
    #[serde(rename = "-A")]
    pub after: Option<usize>,
    #[serde(rename = "-C")]
    pub context_short: Option<usize>,
    pub context: Option<usize>,
    #[serde(rename = "-n")]
    pub line_numbers: Option<bool>,
    #[serde(rename = "-i")]
    pub case_insensitive: Option<bool>,
    #[serde(rename = "type")]
    pub file_type: Option<String>,
    pub head_limit: Option<usize>,
    pub offset: Option<usize>,
    pub multiline: Option<bool>,
}

/// Result payload returned by the grep-style search tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepSearchOutput {
    pub mode: Option<String>,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub content: Option<String>,
    #[serde(rename = "numLines")]
    pub num_lines: Option<usize>,
    #[serde(rename = "numMatches")]
    pub num_matches: Option<usize>,
    #[serde(rename = "appliedLimit")]
    pub applied_limit: Option<usize>,
    #[serde(rename = "appliedOffset")]
    pub applied_offset: Option<usize>,
}

/// Reads a text file and returns a line-windowed payload.
pub fn read_file(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> io::Result<ReadFileOutput> {
    let absolute_path = normalize_path(path)?;

    // Check file size before reading
    let metadata = fs::metadata(&absolute_path)?;
    if metadata.len() > MAX_READ_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "file is too large ({} bytes, max {} bytes)",
                metadata.len(),
                MAX_READ_SIZE
            ),
        ));
    }

    // Detect binary files
    if is_binary_file(&absolute_path)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file appears to be binary",
        ));
    }

    let content = fs::read_to_string(&absolute_path)?;
    let lines: Vec<&str> = content.lines().collect();
    let start_index = offset.unwrap_or(0).min(lines.len());
    let end_index = limit.map_or(lines.len(), |limit| {
        start_index.saturating_add(limit).min(lines.len())
    });
    let selected = lines[start_index..end_index].join("\n");

    Ok(ReadFileOutput {
        kind: String::from("text"),
        file: TextFilePayload {
            file_path: absolute_path.to_string_lossy().into_owned(),
            content: selected,
            num_lines: end_index.saturating_sub(start_index),
            start_line: start_index.saturating_add(1),
            total_lines: lines.len(),
        },
    })
}

/// Replaces a file's contents and returns patch metadata.
pub fn write_file(path: &str, content: &str) -> io::Result<WriteFileOutput> {
    if content.len() > MAX_WRITE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "content is too large ({} bytes, max {} bytes)",
                content.len(),
                MAX_WRITE_SIZE
            ),
        ));
    }

    let absolute_path = normalize_path_allow_missing(path)?;
    let original_file = fs::read_to_string(&absolute_path).ok();
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&absolute_path, content)?;

    Ok(WriteFileOutput {
        kind: if original_file.is_some() {
            String::from("update")
        } else {
            String::from("create")
        },
        file_path: absolute_path.to_string_lossy().into_owned(),
        content: content.to_owned(),
        structured_patch: make_patch(original_file.as_deref().unwrap_or(""), content),
        original_file,
        git_diff: None,
    })
}

/// Performs an in-file string replacement and returns patch metadata.
pub fn edit_file(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> io::Result<EditFileOutput> {
    let absolute_path = normalize_path(path)?;
    let original_file = fs::read_to_string(&absolute_path)?;
    if old_string == new_string {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "old_string and new_string must differ",
        ));
    }
    if !original_file.contains(old_string) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "old_string not found in file",
        ));
    }

    let updated = if replace_all {
        original_file.replace(old_string, new_string)
    } else {
        original_file.replacen(old_string, new_string, 1)
    };
    fs::write(&absolute_path, &updated)?;

    Ok(EditFileOutput {
        file_path: absolute_path.to_string_lossy().into_owned(),
        old_string: old_string.to_owned(),
        new_string: new_string.to_owned(),
        original_file: original_file.clone(),
        structured_patch: make_patch(&original_file, &updated),
        user_modified: false,
        replace_all,
        git_diff: None,
    })
}

/// Expands a glob pattern and returns matching filenames.
pub fn glob_search(pattern: &str, path: Option<&str>) -> io::Result<GlobSearchOutput> {
    let started = Instant::now();
    let base_dir = path
        .map(normalize_path)
        .transpose()?
        .unwrap_or(std::env::current_dir()?);
    if Path::new(pattern).is_absolute() {
        // Keep legacy behavior for absolute patterns.
        let mut matches = Vec::new();
        let entries = glob::glob(pattern)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
        for entry in entries.flatten() {
            if entry.is_file() {
                matches.push(entry);
            }
        }

        matches.sort_by_key(|path| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .map(Reverse)
        });

        let truncated = matches.len() > 100;
        let filenames = matches
            .into_iter()
            .take(100)
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        return Ok(GlobSearchOutput {
            duration_ms: started.elapsed().as_millis(),
            num_files: filenames.len(),
            filenames,
            truncated,
        });
    }

    // Relative patterns: walk the tree and respect .gitignore/.ignore + custom .clawignore.
    let mut b = globset::GlobSetBuilder::new();
    let g = globset::Glob::new(pattern)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    b.add(g);
    let set = b
        .build()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

    // Brace expansion: build multiple globsets so patterns like
    // `Assets/**/*.{cs,uxml,uss}` match all alternatives.
    //
    // Note: we intentionally do *not* use the `glob` crate for relative patterns,
    // because we want to respect `.gitignore` / `.ignore` and a custom `.clawignore`.
    let expanded = expand_braces(pattern);
    let sets = expanded
        .into_iter()
        .map(|pat| {
            let mut b = globset::GlobSetBuilder::new();
            let g = globset::Glob::new(&pat)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            b.add(g);
            b.build()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))
        })
        .collect::<io::Result<Vec<_>>>()?;

    let mut matches = Vec::new();
    let mut walker = WalkBuilder::new(&base_dir);
    walker
        .follow_links(false)
        .git_ignore(true)
        .git_exclude(true)
        .ignore(true)
        .hidden(false)
        .add_custom_ignore_filename(".clawignore");
    for result in walker.build() {
        let Ok(entry) = result else {
            continue;
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(&base_dir) else {
            continue;
        };
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        if sets.iter().any(|set| set.is_match(rel_s.as_str())) {
            matches.push(path.to_path_buf());
        }
    }

    matches.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(Reverse)
    });

    let truncated = matches.len() > 100;
    let filenames = matches
        .into_iter()
        .take(100)
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    Ok(GlobSearchOutput {
        duration_ms: started.elapsed().as_millis(),
        num_files: filenames.len(),
        filenames,
        truncated,
    })
}

/// Runs a regex search over workspace files with optional context lines.
pub fn grep_search(input: &GrepSearchInput) -> io::Result<GrepSearchOutput> {
    let base_path = input
        .path
        .as_deref()
        .map(normalize_path)
        .transpose()?
        .unwrap_or(std::env::current_dir()?);

    let regex = RegexBuilder::new(&input.pattern)
        .case_insensitive(input.case_insensitive.unwrap_or(false))
        .dot_matches_new_line(input.multiline.unwrap_or(false))
        .build()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;

    let glob_filter = input
        .glob
        .as_deref()
        .map(Pattern::new)
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let file_type = input.file_type.as_deref();
    let output_mode = input
        .output_mode
        .clone()
        .unwrap_or_else(|| String::from("files_with_matches"));
    let context = input.context.or(input.context_short).unwrap_or(0);

    let mut filenames = Vec::new();
    let mut content_lines = Vec::new();
    let mut total_matches = 0usize;

    for file_path in collect_search_files(&base_path) {
        if !matches_optional_filters(&file_path, glob_filter.as_ref(), file_type) {
            continue;
        }

        let Ok(file_contents) = fs::read_to_string(&file_path) else {
            continue;
        };

        if output_mode == "count" {
            let count = regex.find_iter(&file_contents).count();
            if count > 0 {
                filenames.push(file_path.to_string_lossy().into_owned());
                total_matches += count;
            }
            continue;
        }

        let lines: Vec<&str> = file_contents.lines().collect();
        let mut matched_lines = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if regex.is_match(line) {
                total_matches += 1;
                matched_lines.push(index);
            }
        }

        if matched_lines.is_empty() {
            continue;
        }

        filenames.push(file_path.to_string_lossy().into_owned());
        if output_mode == "content" {
            for index in matched_lines {
                let start = index.saturating_sub(input.before.unwrap_or(context));
                let end = (index + input.after.unwrap_or(context) + 1).min(lines.len());
                for (current, line) in lines.iter().enumerate().take(end).skip(start) {
                    let prefix = if input.line_numbers.unwrap_or(true) {
                        format!("{}:{}:", file_path.to_string_lossy(), current + 1)
                    } else {
                        format!("{}:", file_path.to_string_lossy())
                    };
                    content_lines.push(format!("{prefix}{line}"));
                }
            }
        }
    }

    let (filenames, applied_limit, applied_offset) =
        apply_limit(filenames, input.head_limit, input.offset);
    let content_output = if output_mode == "content" {
        let (lines, limit, offset) = apply_limit(content_lines, input.head_limit, input.offset);
        return Ok(GrepSearchOutput {
            mode: Some(output_mode),
            num_files: filenames.len(),
            filenames,
            num_lines: Some(lines.len()),
            content: Some(lines.join("\n")),
            num_matches: None,
            applied_limit: limit,
            applied_offset: offset,
        });
    } else {
        None
    };

    Ok(GrepSearchOutput {
        mode: Some(output_mode.clone()),
        num_files: filenames.len(),
        filenames,
        content: content_output,
        num_lines: None,
        num_matches: (output_mode == "count").then_some(total_matches),
        applied_limit,
        applied_offset,
    })
}

fn collect_search_files(base_path: &Path) -> Vec<PathBuf> {
    if base_path.is_file() {
        return vec![base_path.to_path_buf()];
    }

    let mut files = Vec::new();
    let mut walker = WalkBuilder::new(base_path);
    walker
        .follow_links(false)
        .git_ignore(true)
        .git_exclude(true)
        .ignore(true)
        .hidden(false)
        .add_custom_ignore_filename(".clawignore");
    for result in walker.build() {
        let Ok(entry) = result else {
            continue;
        };
        if entry.file_type().is_some_and(|t| t.is_file()) {
            files.push(entry.path().to_path_buf());
        }
    }
    files
}

fn matches_optional_filters(
    path: &Path,
    glob_filter: Option<&Pattern>,
    file_type: Option<&str>,
) -> bool {
    if let Some(glob_filter) = glob_filter {
        let path_string = path.to_string_lossy();
        if !glob_filter.matches(&path_string) && !glob_filter.matches_path(path) {
            return false;
        }
    }

    if let Some(file_type) = file_type {
        let extension = path.extension().and_then(|extension| extension.to_str());
        if extension != Some(file_type) {
            return false;
        }
    }

    true
}

fn apply_limit<T>(
    items: Vec<T>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> (Vec<T>, Option<usize>, Option<usize>) {
    let offset_value = offset.unwrap_or(0);
    let mut items = items.into_iter().skip(offset_value).collect::<Vec<_>>();
    let explicit_limit = limit.unwrap_or(250);
    if explicit_limit == 0 {
        return (items, None, (offset_value > 0).then_some(offset_value));
    }

    let truncated = items.len() > explicit_limit;
    items.truncate(explicit_limit);
    (
        items,
        truncated.then_some(explicit_limit),
        (offset_value > 0).then_some(offset_value),
    )
}

fn make_patch(original: &str, updated: &str) -> Vec<StructuredPatchHunk> {
    let mut lines = Vec::new();
    for line in original.lines() {
        lines.push(format!("-{line}"));
    }
    for line in updated.lines() {
        lines.push(format!("+{line}"));
    }

    vec![StructuredPatchHunk {
        old_start: 1,
        old_lines: original.lines().count(),
        new_start: 1,
        new_lines: updated.lines().count(),
        lines,
    }]
}

fn normalize_path(path: &str) -> io::Result<PathBuf> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?.join(path)
    };
    candidate.canonicalize()
}

fn normalize_path_allow_missing(path: &str) -> io::Result<PathBuf> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?.join(path)
    };

    if let Ok(canonical) = candidate.canonicalize() {
        return Ok(canonical);
    }

    if let Some(parent) = candidate.parent() {
        let canonical_parent = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        if let Some(name) = candidate.file_name() {
            return Ok(canonical_parent.join(name));
        }
    }

    Ok(candidate)
}

/// Read a file with workspace boundary enforcement.
#[allow(dead_code)]
pub fn read_file_in_workspace(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    workspace_root: &Path,
) -> io::Result<ReadFileOutput> {
    let absolute_path = normalize_path(path)?;
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    validate_workspace_boundary(&absolute_path, &canonical_root)?;
    read_file(path, offset, limit)
}

/// Write a file with workspace boundary enforcement.
#[allow(dead_code)]
pub fn write_file_in_workspace(
    path: &str,
    content: &str,
    workspace_root: &Path,
) -> io::Result<WriteFileOutput> {
    let absolute_path = normalize_path_allow_missing(path)?;
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    validate_workspace_boundary(&absolute_path, &canonical_root)?;

    if staging_write_enabled() {
        if content.len() > MAX_WRITE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "content is too large ({} bytes, max {} bytes)",
                    content.len(),
                    MAX_WRITE_SIZE
                ),
            ));
        }

        let original_file = fs::read_to_string(&absolute_path).ok();
        let structured_patch = make_patch(original_file.as_deref().unwrap_or(""), content);

        let rel_path = absolute_path
            .strip_prefix(&canonical_root)
            .unwrap_or(&absolute_path)
            .to_string_lossy()
            .replace('\\', "/");

        let staging_root = staging_dir(&canonical_root);
        let batch_dir = staging_root.join(format!("write-{}", now_ms()));
        let proposed_path = batch_dir.join(rel_path.as_str());
        let diff_path = batch_dir.join(format!("{rel_path}.diff"));
        if let Some(parent) = proposed_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&proposed_path, content)?;
        let diff = structured_patch_to_unified_diff(&rel_path, &structured_patch);
        if let Some(parent) = diff_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&diff_path, diff)?;

        return Ok(WriteFileOutput {
            kind: String::from("stage"),
            file_path: absolute_path.to_string_lossy().into_owned(),
            content: content.to_owned(),
            structured_patch,
            original_file,
            git_diff: None,
        });
    }

    write_file(path, content)
}

/// Edit a file with workspace boundary enforcement.
#[allow(dead_code)]
pub fn edit_file_in_workspace(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    workspace_root: &Path,
) -> io::Result<EditFileOutput> {
    let absolute_path = normalize_path(path)?;
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    validate_workspace_boundary(&absolute_path, &canonical_root)?;

    if staging_write_enabled() {
        let original_file = fs::read_to_string(&absolute_path)?;
        if old_string == new_string {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "old_string and new_string must differ",
            ));
        }
        if !original_file.contains(old_string) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "old_string not found in file",
            ));
        }
        let updated = if replace_all {
            original_file.replace(old_string, new_string)
        } else {
            original_file.replacen(old_string, new_string, 1)
        };

        let structured_patch = make_patch(&original_file, &updated);
        let rel_path = absolute_path
            .strip_prefix(&canonical_root)
            .unwrap_or(&absolute_path)
            .to_string_lossy()
            .replace('\\', "/");
        let staging_root = staging_dir(&canonical_root);
        let batch_dir = staging_root.join(format!("edit-{}", now_ms()));
        let proposed_path = batch_dir.join(rel_path.as_str());
        let diff_path = batch_dir.join(format!("{rel_path}.diff"));
        if let Some(parent) = proposed_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&proposed_path, &updated)?;
        let diff = structured_patch_to_unified_diff(&rel_path, &structured_patch);
        if let Some(parent) = diff_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&diff_path, diff)?;

        return Ok(EditFileOutput {
            file_path: absolute_path.to_string_lossy().into_owned(),
            old_string: old_string.to_owned(),
            new_string: new_string.to_owned(),
            original_file,
            structured_patch,
            user_modified: false,
            replace_all,
            git_diff: None,
        });
    }

    edit_file(path, old_string, new_string, replace_all)
}

/// Check whether a path is a symlink that resolves outside the workspace.
#[allow(dead_code)]
pub fn is_symlink_escape(path: &Path, workspace_root: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_symlink() {
        return Ok(false);
    }
    let resolved = path.canonicalize()?;
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    Ok(!resolved.starts_with(&canonical_root))
}

/// Expand shell-style brace groups in a glob pattern.
///
/// Handles one level of braces: `foo.{a,b,c}` → `["foo.a", "foo.b", "foo.c"]`.
/// Nested braces are not expanded (uncommon in practice).
/// Patterns without braces pass through unchanged.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(close) = pattern[open..].find('}').map(|i| open + i) else {
        // Unmatched brace — treat as literal.
        return vec![pattern.to_owned()];
    };
    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    let alternatives = &pattern[open + 1..close];
    alternatives
        .split(',')
        .flat_map(|alt| expand_braces(&format!("{prefix}{alt}{suffix}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        edit_file, expand_braces, glob_search, grep_search, is_symlink_escape, read_file,
        read_file_in_workspace, write_file, GrepSearchInput, MAX_WRITE_SIZE,
    };

    fn temp_path(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        std::env::temp_dir().join(format!("clawd-native-{name}-{unique}"))
    }

    #[test]
    fn reads_and_writes_files() {
        let path = temp_path("read-write.txt");
        let write_output = write_file(path.to_string_lossy().as_ref(), "one\ntwo\nthree")
            .expect("write should succeed");
        assert_eq!(write_output.kind, "create");

        let read_output = read_file(path.to_string_lossy().as_ref(), Some(1), Some(1))
            .expect("read should succeed");
        assert_eq!(read_output.file.content, "two");
    }

    #[test]
    fn edits_file_contents() {
        let path = temp_path("edit.txt");
        write_file(path.to_string_lossy().as_ref(), "alpha beta alpha")
            .expect("initial write should succeed");
        let output = edit_file(path.to_string_lossy().as_ref(), "alpha", "omega", true)
            .expect("edit should succeed");
        assert!(output.replace_all);
    }

    #[test]
    fn rejects_binary_files() {
        let path = temp_path("binary-test.bin");
        std::fs::write(&path, b"\x00\x01\x02\x03binary content").expect("write should succeed");
        let result = read_file(path.to_string_lossy().as_ref(), None, None);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("binary"));
    }

    #[test]
    fn rejects_oversized_writes() {
        let path = temp_path("oversize-write.txt");
        let huge = "x".repeat(MAX_WRITE_SIZE + 1);
        let result = write_file(path.to_string_lossy().as_ref(), &huge);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("too large"));
    }

    #[test]
    fn enforces_workspace_boundary() {
        let workspace = temp_path("workspace-boundary");
        std::fs::create_dir_all(&workspace).expect("workspace dir should be created");
        let inside = workspace.join("inside.txt");
        write_file(inside.to_string_lossy().as_ref(), "safe content")
            .expect("write inside workspace should succeed");

        // Reading inside workspace should succeed
        let result =
            read_file_in_workspace(inside.to_string_lossy().as_ref(), None, None, &workspace);
        assert!(result.is_ok());

        // Reading outside workspace should fail
        let outside = temp_path("outside-boundary.txt");
        write_file(outside.to_string_lossy().as_ref(), "unsafe content")
            .expect("write outside should succeed");
        let result =
            read_file_in_workspace(outside.to_string_lossy().as_ref(), None, None, &workspace);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("escapes workspace"));
    }

    #[test]
    fn detects_symlink_escape() {
        let workspace = temp_path("symlink-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir should be created");
        let outside = temp_path("symlink-target.txt");
        std::fs::write(&outside, "target content").expect("target should write");

        let _link_path = workspace.join("escape-link.txt");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &_link_path).expect("symlink should create");
            assert!(is_symlink_escape(&_link_path, &workspace).expect("check should succeed"));
        }

        // Non-symlink file should not be an escape
        let normal = workspace.join("normal.txt");
        std::fs::write(&normal, "normal content").expect("normal file should write");
        assert!(!is_symlink_escape(&normal, &workspace).expect("check should succeed"));
    }

    #[test]
    fn staging_write_creates_diff_without_modifying_file() {
        let workspace = temp_path("staging-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir should be created");
        let file = workspace.join("demo.txt");
        write_file(file.to_string_lossy().as_ref(), "old").expect("seed write");

        std::env::set_var("CLAW_STAGING_WRITE", "1");
        let out =
            super::write_file_in_workspace(file.to_string_lossy().as_ref(), "new", &workspace)
                .expect("staged write should succeed");
        assert_eq!(out.kind, "stage");
        assert_eq!(
            std::fs::read_to_string(&file).expect("original should remain"),
            "old"
        );

        let staging = workspace.join(".claw").join("staging");
        let mut found_diff = false;
        for entry in walkdir::WalkDir::new(&staging) {
            let entry = entry.expect("walk");
            if entry.path().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("diff"))
            {
                found_diff = true;
                break;
            }
        }
        assert!(found_diff, "expected a staged .diff file");
        std::env::remove_var("CLAW_STAGING_WRITE");
    }

    #[test]
    fn globs_and_greps_directory() {
        let dir = temp_path("search-dir");
        std::fs::create_dir_all(&dir).expect("directory should be created");
        let file = dir.join("demo.rs");
        write_file(
            file.to_string_lossy().as_ref(),
            "fn main() {\n println!(\"hello\");\n}\n",
        )
        .expect("file write should succeed");

        let globbed = glob_search("**/*.rs", Some(dir.to_string_lossy().as_ref()))
            .expect("glob should succeed");
        assert_eq!(globbed.num_files, 1);

        let grep_output = grep_search(&GrepSearchInput {
            pattern: String::from("hello"),
            path: Some(dir.to_string_lossy().into_owned()),
            glob: Some(String::from("**/*.rs")),
            output_mode: Some(String::from("content")),
            before: None,
            after: None,
            context_short: None,
            context: None,
            line_numbers: Some(true),
            case_insensitive: Some(false),
            file_type: None,
            head_limit: Some(10),
            offset: Some(0),
            multiline: Some(false),
        })
        .expect("grep should succeed");
        assert!(grep_output.content.unwrap_or_default().contains("hello"));
    }

    #[test]
    fn glob_and_grep_respect_gitignore_and_clawignore() {
        let dir = temp_path("search-ignore-dir");
        std::fs::create_dir_all(&dir).expect("directory should be created");
        std::fs::create_dir_all(dir.join(".git")).expect("repo marker");
        std::fs::write(dir.join(".gitignore"), "node_modules/\n").expect("write gitignore");
        std::fs::write(dir.join(".clawignore"), "ignored_dir/\n").expect("write clawignore");

        let kept = dir.join("src").join("kept.rs");
        std::fs::create_dir_all(kept.parent().expect("parent")).expect("mk src");
        write_file(
            kept.to_string_lossy().as_ref(),
            "fn kept() {\n println!(\"hello\");\n}\n",
        )
        .expect("kept write");

        let ignored_nm = dir.join("node_modules").join("ignored.rs");
        std::fs::create_dir_all(ignored_nm.parent().expect("parent")).expect("mk node_modules");
        write_file(
            ignored_nm.to_string_lossy().as_ref(),
            "fn ignored() {\n println!(\"hello\");\n}\n",
        )
        .expect("ignored write");

        let ignored_custom = dir.join("ignored_dir").join("also_ignored.rs");
        std::fs::create_dir_all(ignored_custom.parent().expect("parent")).expect("mk ignored_dir");
        write_file(
            ignored_custom.to_string_lossy().as_ref(),
            "fn ignored2() {\n println!(\"hello\");\n}\n",
        )
        .expect("ignored2 write");

        let globbed = glob_search("**/*.rs", Some(dir.to_string_lossy().as_ref()))
            .expect("glob should succeed");
        assert_eq!(globbed.num_files, 1);
        assert!(globbed.filenames[0]
            .replace('\\', "/")
            .ends_with("/src/kept.rs"));

        let grep_output = grep_search(&GrepSearchInput {
            pattern: String::from("hello"),
            path: Some(dir.to_string_lossy().into_owned()),
            glob: Some(String::from("**/*.rs")),
            output_mode: Some(String::from("files_with_matches")),
            before: None,
            after: None,
            context_short: None,
            context: None,
            line_numbers: Some(true),
            case_insensitive: Some(false),
            file_type: None,
            head_limit: Some(50),
            offset: Some(0),
            multiline: Some(false),
        })
        .expect("grep should succeed");
        assert_eq!(grep_output.num_files, 1);
        assert!(grep_output.filenames[0]
            .replace('\\', "/")
            .ends_with("/src/kept.rs"));
    }

    #[test]
    fn expand_braces_no_braces() {
        assert_eq!(expand_braces("*.rs"), vec!["*.rs"]);
    }

    #[test]
    fn expand_braces_single_group() {
        let mut result = expand_braces("Assets/**/*.{cs,uxml,uss}");
        result.sort();
        assert_eq!(result, vec!["Assets/**/*.cs", "Assets/**/*.uss", "Assets/**/*.uxml",]);
    }

    #[test]
    fn expand_braces_nested() {
        let mut result = expand_braces("src/{a,b}.{rs,toml}");
        result.sort();
        assert_eq!(result, vec!["src/a.rs", "src/a.toml", "src/b.rs", "src/b.toml"]);
    }

    #[test]
    fn expand_braces_unmatched() {
        assert_eq!(expand_braces("foo.{bar"), vec!["foo.{bar"]);
    }

    #[test]
    fn glob_search_with_braces_finds_files() {
        let dir = temp_path("glob-braces");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("b.toml"), "[package]").unwrap();
        std::fs::write(dir.join("c.txt"), "hello").unwrap();

        let result = glob_search("*.{rs,toml}", Some(dir.to_str().unwrap())).expect("glob ok");
        assert_eq!(result.num_files, 2, "should match .rs and .toml but not .txt");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
