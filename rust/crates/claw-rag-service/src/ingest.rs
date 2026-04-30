//! Walk workspace and fill `SQLite` + embeddings.

use std::path::Path;
use std::path::PathBuf;

use reqwest::Client;
use walkdir::WalkDir;

use crate::chunk::chunk_text;
use crate::db::{
    delete_file_and_chunks, file_is_unchanged, insert_chunk, insert_embedding, list_all_files,
    open_db, upsert_file_meta,
};
use crate::embed::{embed_batch, EmbedConfig};
#[cfg(feature = "qdrant-index")]
use crate::qdrant_index::{upsert_points, ChunkPoint};

const DEFAULT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const CHUNK_CHARS: usize = 900;
const CHUNK_OVERLAP: usize = 120;
const EMBED_BATCH: usize = 16;

static SKIP_DIR_NAMES: &[&str] = &[".git", "target", "node_modules", "__pycache__", ".claw-rag"];

static TEXT_EXTENSIONS: &[&str] = &[
    "rs", "md", "toml", "txt", "json", "yaml", "yml", "js", "ts", "tsx", "jsx", "py", "go", "c",
    "h", "cpp", "hpp", "cs", "java", "kt", "swift", "rb", "php", "sh", "ps1", "html", "css", "sql",
];

#[derive(Debug, Default)]
pub struct IngestStats {
    pub files_indexed: usize,
    pub chunks_total: usize,
    pub embeddings_written: usize,
}

fn should_skip_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|n| SKIP_DIR_NAMES.contains(&n))
}

fn is_text_extension(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|e| TEXT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

async fn flush_path_batch(
    conn: &rusqlite::Connection,
    path: &str,
    batch: &mut Vec<(i32, String)>,
    client: &Client,
    cfg: &EmbedConfig,
    stats: &mut IngestStats,
) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
    let vecs = embed_batch(client, cfg, &texts).await?;
    if vecs.len() != batch.len() {
        return Err("embed batch size mismatch".into());
    }

    #[cfg(feature = "qdrant-index")]
    let mut qdrant_points: Vec<ChunkPoint> = Vec::with_capacity(batch.len());

    for ((ord, t), vec) in batch.drain(..).zip(vecs.into_iter()) {
        let dim = vec.len();
        let cid = insert_chunk(conn, path, ord, &t)?;
        insert_embedding(conn, cid, dim, &vec)?;
        stats.embeddings_written += 1;

        #[cfg(feature = "qdrant-index")]
        {
            qdrant_points.push(ChunkPoint {
                id: cid,
                vec,
                path: path.to_string(),
                text: t,
            });
        }
    }

    #[cfg(feature = "qdrant-index")]
    upsert_points(qdrant_points).await?;

    Ok(())
}

pub async fn run_ingest(
    workspaces: &[PathBuf],
    db_path: &Path,
    cfg: &EmbedConfig,
    client: &Client,
) -> Result<IngestStats, String> {
    let conn = open_db(db_path)?;

    let mut all_files: Vec<(String, PathBuf)> = Vec::new();
    let mut seen_paths: Vec<String> = Vec::new();

    for ws in workspaces {
        let workspace = ws
            .canonicalize()
            .map_err(|e| format!("workspace: {}: {e}", ws.display()))?;
        let ws_prefix = workspace.clone();
        let repo_id = repo_id_for_workspace(&workspace);

        for entry in WalkDir::new(&workspace)
            .into_iter()
            .filter_entry(|e| !should_skip_dir(e.path()))
        {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !is_text_extension(path) {
                continue;
            }
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            if meta.len() > DEFAULT_MAX_FILE_BYTES {
                continue;
            }
            let rel = path
                .strip_prefix(&ws_prefix)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let key = format!("{repo_id}:{rel}");
            seen_paths.push(key.clone());
            all_files.push((key, path.to_path_buf()));
        }
    }

    all_files.sort_by(|a, b| a.0.cmp(&b.0));
    seen_paths.sort();

    let mut stats = IngestStats {
        files_indexed: all_files.len(),
        ..Default::default()
    };

    for (rel, file) in all_files {
        let Ok(meta) = std::fs::metadata(&file) else {
            continue;
        };
        let size_bytes =
            i64::try_from(meta.len()).map_err(|_| "file size too large".to_string())?;
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0);

        let Ok(raw) = std::fs::read_to_string(&file) else {
            continue;
        };

        let content_hash = blake3::hash(raw.as_bytes()).to_hex().to_string();
        if file_is_unchanged(&conn, &rel, &content_hash, size_bytes, mtime_ms)? {
            continue;
        }

        // Re-index this file: delete previous chunks (and embeddings) for path.
        delete_file_and_chunks(&conn, &rel)?;

        let pieces = chunk_text(&raw, CHUNK_CHARS, CHUNK_OVERLAP);
        if pieces.is_empty() {
            continue;
        }

        let mut batch: Vec<(i32, String)> = Vec::new();
        for (ord, piece) in pieces.into_iter().enumerate() {
            stats.chunks_total += 1;
            let ord_i32 =
                i32::try_from(ord).map_err(|_| "file produced too many chunks".to_string())?;
            batch.push((ord_i32, piece));
            if batch.len() >= EMBED_BATCH {
                flush_path_batch(&conn, &rel, &mut batch, client, cfg, &mut stats).await?;
            }
        }
        flush_path_batch(&conn, &rel, &mut batch, client, cfg, &mut stats).await?;

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(0))
            .unwrap_or(0);
        upsert_file_meta(&conn, &rel, &content_hash, size_bytes, mtime_ms, now_ms)?;
    }

    // Delete entries for files that no longer exist.
    // (We compare against file list from DB to avoid needing a SQL "NOT IN" temp table.)
    let mut seen_set = std::collections::BTreeSet::new();
    for p in &seen_paths {
        seen_set.insert(p.as_str());
    }
    for p in list_all_files(&conn)? {
        if !seen_set.contains(p.as_str()) {
            delete_file_and_chunks(&conn, &p)?;
        }
    }

    Ok(stats)
}

fn repo_id_for_workspace(workspace: &Path) -> String {
    let name = workspace
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("workspace");
    let hash = blake3::hash(workspace.to_string_lossy().as_bytes())
        .to_hex()
        .to_string();
    format!("{name}-{h}", name = name, h = &hash[..8])
}
