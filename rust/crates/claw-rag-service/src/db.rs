//! `SQLite` storage for chunks and embedding vectors.

use std::path::Path;

use rusqlite::{params, Connection};

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS chunks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    text TEXT NOT NULL,
    UNIQUE(path, ordinal)
);
CREATE TABLE IF NOT EXISTS embeddings (
    chunk_id INTEGER PRIMARY KEY,
    dim INTEGER NOT NULL,
    vec BLOB NOT NULL,
    FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS files (
    path TEXT PRIMARY KEY,
    content_hash TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    mtime_ms INTEGER NOT NULL,
    indexed_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chunks_path ON chunks(path);
";

pub fn open_db(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }

    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.execute_batch(
        r"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
",
    )
    .map_err(|e| e.to_string())?;
    conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;

    Ok(conn)
}

#[allow(dead_code)]
pub fn truncate_index(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("DELETE FROM embeddings; DELETE FROM chunks; DELETE FROM files;")
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn file_is_unchanged(
    conn: &Connection,
    path: &str,
    content_hash: &str,
    size_bytes: i64,
    mtime_ms: i64,
) -> Result<bool, String> {
    let mut stmt = conn
        .prepare("SELECT content_hash, size_bytes, mtime_ms FROM files WHERE path=?1 LIMIT 1")
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query(params![path]).map_err(|e| e.to_string())?;
    if let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let h: String = r.get(0).map_err(|e| e.to_string())?;
        let sz: i64 = r.get(1).map_err(|e| e.to_string())?;
        let mt: i64 = r.get(2).map_err(|e| e.to_string())?;
        return Ok(h == content_hash && sz == size_bytes && mt == mtime_ms);
    }
    Ok(false)
}

pub fn upsert_file_meta(
    conn: &Connection,
    path: &str,
    content_hash: &str,
    size_bytes: i64,
    mtime_ms: i64,
    indexed_at_ms: i64,
) -> Result<(), String> {
    conn.execute(
        r"
INSERT INTO files(path, content_hash, size_bytes, mtime_ms, indexed_at_ms)
VALUES (?1, ?2, ?3, ?4, ?5)
ON CONFLICT(path) DO UPDATE SET
  content_hash=excluded.content_hash,
  size_bytes=excluded.size_bytes,
  mtime_ms=excluded.mtime_ms,
  indexed_at_ms=excluded.indexed_at_ms
",
        params![path, content_hash, size_bytes, mtime_ms, indexed_at_ms],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_file_and_chunks(conn: &Connection, path: &str) -> Result<(), String> {
    // Delete chunks first (embeddings cascade); then remove file meta.
    conn.execute("DELETE FROM chunks WHERE path=?1", params![path])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM files WHERE path=?1", params![path])
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn list_all_files(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare("SELECT path FROM files")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

pub fn insert_chunk(
    conn: &Connection,
    path: &str,
    ordinal: i32,
    text: &str,
) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO chunks (path, ordinal, text) VALUES (?1, ?2, ?3)",
        params![path, ordinal, text],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

pub fn insert_embedding(
    conn: &Connection,
    chunk_id: i64,
    dim: usize,
    vec: &[f32],
) -> Result<(), String> {
    let bytes = f32_slice_to_blob(vec);
    let dim_i64 = i64::try_from(dim).map_err(|_| "embedding dim too large".to_string())?;
    conn.execute(
        "INSERT INTO embeddings (chunk_id, dim, vec) VALUES (?1, ?2, ?3)",
        params![chunk_id, dim_i64, bytes],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

pub(crate) fn f32_slice_to_blob(v: &[f32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(v.len() * 4);
    for x in v {
        b.extend_from_slice(&x.to_le_bytes());
    }
    b
}

pub fn blob_to_f32_vec(blob: &[u8], dim: usize) -> Option<Vec<f32>> {
    if blob.len() != dim * 4 {
        return None;
    }
    let mut v = Vec::with_capacity(dim);
    for chunk in blob.chunks_exact(4) {
        v.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some(v)
}

#[derive(Debug, Clone)]
pub struct ChunkRow {
    pub path: String,
    pub text: String,
    pub vec: Vec<f32>,
}

pub fn load_all_indexed(conn: &Connection) -> Result<Vec<ChunkRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT c.path, c.text, e.dim, e.vec FROM chunks c
         INNER JOIN embeddings e ON e.chunk_id = c.id",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let path: String = r.get(0).map_err(|e| e.to_string())?;
        let text: String = r.get(1).map_err(|e| e.to_string())?;
        let dim: i64 = r.get(2).map_err(|e| e.to_string())?;
        let blob: Vec<u8> = r.get(3).map_err(|e| e.to_string())?;
        let dim = usize::try_from(dim).map_err(|_| "invalid embedding dim in db".to_string())?;
        let Some(vec) = blob_to_f32_vec(&blob, dim) else {
            continue;
        };
        out.push(ChunkRow { path, text, vec });
    }
    Ok(out)
}

pub fn chunk_count(conn: &Connection) -> Result<i64, String> {
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(n)
}
