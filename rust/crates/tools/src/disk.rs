use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize)]
pub struct DiskUsageEntry {
    pub path: String,
    pub kind: &'static str,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskUsageReport {
    pub root: String,
    pub elapsed_ms: u128,
    pub truncated: bool,
    pub total_bytes_scanned: u64,
    pub top_entries: Vec<DiskUsageEntry>,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiskUsageInput {
    pub path: Option<String>,
    pub max_entries: Option<usize>,
    pub max_files: Option<usize>,
    pub max_seconds: Option<u64>,
    pub min_file_mb: Option<u64>,
}

fn to_lossy(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn is_probably_junk_dir(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "node_modules"
            | "target"
            | ".git"
            | ".hg"
            | ".svn"
            | ".gradle"
            | ".m2"
            | ".cargo"
            | ".next"
            | "dist"
            | "build"
            | ".cache"
            | "cache"
            | "tmp"
            | "temp"
            | "logs"
    )
}

fn suggestions_for_root(root: &Path) -> Vec<String> {
    let mut out = vec![
        "Общие: начни с корзины, временных файлов, Downloads и больших видео/архивов.".to_string(),
        "Windows: Settings → System → Storage → Temporary files (аккуратно) и Storage Sense."
            .to_string(),
        "Перенос: большие папки (Videos/Downloads/VMs) лучше переносить на другой диск и делать ярлыки/точки монтирования.".to_string(),
        "Увеличение диска: если это системный диск, обычно нужно расширять раздел (Disk Management) или менять диск/SSD; для VHD/VM — расширять виртуальный диск.".to_string(),
    ];

    // Workspace-ish hints
    if root.join("node_modules").exists() {
        out.push("Проект: `node_modules` часто самый большой — можно удалить и восстановить `npm ci`/`pnpm install`.".to_string());
    }
    if root.join("target").exists() {
        out.push("Rust: `target/` можно удалить (пересоберётся).".to_string());
    }
    out
}

#[allow(clippy::too_many_lines)]
pub fn disk_usage_report(cwd: &Path, input: &DiskUsageInput) -> Result<DiskUsageReport, String> {
    let root = input
        .path
        .as_deref()
        .map_or_else(|| cwd.to_path_buf(), PathBuf::from);
    let root = root
        .canonicalize()
        .map_err(|e| format!("failed to resolve path {}: {e}", to_lossy(&root)))?;
    if !root.exists() {
        return Err(format!("path does not exist: {}", to_lossy(&root)));
    }

    let max_entries = input.max_entries.unwrap_or(40).clamp(1, 200);
    let max_files = input.max_files.unwrap_or(150_000).clamp(1, 2_000_000);
    let max_seconds = input.max_seconds.unwrap_or(10).clamp(1, 120);
    let min_file_mb = input.min_file_mb.unwrap_or(50);
    let min_file_bytes = min_file_mb.saturating_mul(1024).saturating_mul(1024);

    let deadline = Instant::now() + Duration::from_secs(max_seconds);
    let start = Instant::now();

    let mut truncated = false;
    let mut total_bytes_scanned: u64 = 0;
    let mut files_seen: usize = 0;

    let mut big_files: Vec<DiskUsageEntry> = Vec::new();
    let mut dir_bytes: std::collections::HashMap<PathBuf, u64> = std::collections::HashMap::new();

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if Instant::now() >= deadline {
            truncated = true;
            break;
        }
        if files_seen >= max_files {
            truncated = true;
            break;
        }

        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if is_probably_junk_dir(name) {
                    // still traverse, but mark as potential hot-spot via suggestions; skipping would hide sizes.
                }
            }
            continue;
        }

        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }

        files_seen += 1;
        let len = meta.len();
        total_bytes_scanned = total_bytes_scanned.saturating_add(len);

        // attribute file size to all parent dirs up to root
        let mut cur = path.parent();
        while let Some(p) = cur {
            *dir_bytes.entry(p.to_path_buf()).or_insert(0) += len;
            if p == root {
                break;
            }
            cur = p.parent();
        }

        if len >= min_file_bytes {
            big_files.push(DiskUsageEntry {
                path: to_lossy(path),
                kind: "file",
                bytes: len,
            });
        }
    }

    big_files.sort_by_key(|e| Reverse(e.bytes));
    if big_files.len() > max_entries {
        big_files.truncate(max_entries);
    }

    let mut big_dirs: Vec<DiskUsageEntry> = dir_bytes
        .into_iter()
        .filter(|(p, _)| *p != root)
        .map(|(p, bytes)| DiskUsageEntry {
            path: to_lossy(&p),
            kind: "dir",
            bytes,
        })
        .collect();
    big_dirs.sort_by_key(|e| Reverse(e.bytes));
    if big_dirs.len() > max_entries {
        big_dirs.truncate(max_entries);
    }

    let mut top_entries = Vec::new();
    top_entries.extend(big_dirs.into_iter().take(max_entries / 2));
    top_entries.extend(big_files);
    top_entries.sort_by_key(|e| Reverse(e.bytes));
    if top_entries.len() > max_entries {
        top_entries.truncate(max_entries);
    }

    Ok(DiskUsageReport {
        root: to_lossy(&root),
        elapsed_ms: start.elapsed().as_millis(),
        truncated,
        total_bytes_scanned,
        top_entries,
        suggestions: suggestions_for_root(&root),
    })
}
