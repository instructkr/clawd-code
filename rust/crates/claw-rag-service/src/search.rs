//! Vector search over indexed chunks (linear scan MVP).

use std::path::Path;

use reqwest::Client;

use crate::db::{load_all_indexed, open_db};
use crate::embed::{cosine_similarity, embed_batch, EmbedConfig};
use crate::{QueryRequest, QueryResponse, RagHit};

pub async fn query_index(
    db_path: &Path,
    client: &Client,
    cfg: &EmbedConfig,
    req: &QueryRequest,
) -> Result<QueryResponse, String> {
    if !db_path.is_file() {
        return Ok(QueryResponse {
            hits: Vec::new(),
            phase: "1-sqlite-no-db",
        });
    }

    let conn = open_db(db_path)?;
    let qvecs = embed_batch(client, cfg, std::slice::from_ref(&req.query)).await?;
    let q = qvecs
        .into_iter()
        .next()
        .ok_or_else(|| "no query embedding".to_string())?;

    #[cfg(feature = "qdrant-index")]
    if let Ok(Some(r)) = crate::qdrant_index::query_qdrant(&q, req.top_k).await {
        return Ok(r);
    }

    let rows = load_all_indexed(&conn)?;
    drop(conn);

    if rows.is_empty() {
        return Ok(QueryResponse {
            hits: Vec::new(),
            phase: "1-sqlite-empty",
        });
    }

    let expected = rows[0].vec.len();
    if q.len() != expected {
        return Err(format!(
            "embedding dimension mismatch: index uses dim {} but query embedding has {} (same model/env as ingest required)",
            expected, q.len()
        ));
    }

    let mut scored: Vec<(f32, usize)> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (cosine_similarity(&q, &r.vec), i))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let top = req.top_k.min(64) as usize;
    let hits: Vec<RagHit> = scored
        .into_iter()
        .take(top)
        .map(|(score, i)| {
            let r = &rows[i];
            RagHit {
                path: r.path.clone(),
                snippet: truncate_snippet(&r.text, 480),
                score: Some(score),
            }
        })
        .collect();

    Ok(QueryResponse {
        hits,
        phase: "1-sqlite",
    })
}

fn truncate_snippet(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect::<String>() + "…"
}
