//! Workspace RAG: ingest files → `SQLite` + embeddings, query via cosine similarity (linear scan MVP).
#![forbid(unsafe_code)]

mod chunk;
mod db;
mod embed;
mod ingest;
#[cfg(feature = "qdrant-index")]
mod qdrant_index;
mod search;

pub use db::{chunk_count, open_db};
pub use embed::EmbedConfig;
pub use ingest::{run_ingest, IngestStats};
pub use search::query_index;

use serde::{Deserialize, Serialize};

/// One retrieved chunk for the model or UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagHit {
    pub path: String,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueryRequest {
    pub query: String,
    #[serde(default = "default_top_k")]
    pub top_k: u32,
}

fn default_top_k() -> u32 {
    8
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryResponse {
    pub hits: Vec<RagHit>,
    /// `0-stub` (legacy), `1-sqlite`, `1-sqlite-empty`, `1-sqlite-no-db`
    pub phase: &'static str,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use reqwest::Client;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn query_missing_db_reports_phase() {
        let client = Client::new();
        let cfg = EmbedConfig {
            api_key: "x".into(),
            base_url: "mock://".into(),
            model: "m".into(),
        };
        let r = query_index(
            Path::new("/no/such/claw_rag.sqlite"),
            &client,
            &cfg,
            &QueryRequest {
                query: "hello".into(),
                top_k: 3,
            },
        )
        .await
        .unwrap();
        assert_eq!(r.phase, "1-sqlite-no-db");
    }

    #[tokio::test]
    async fn ingest_and_query_roundtrip_mock() {
        std::env::set_var("CLAW_RAG_MOCK_PROVIDERS", "1");
        let dir = tempdir().unwrap();
        let ws1 = dir.path().join("ws1");
        let ws2 = dir.path().join("ws2");
        std::fs::create_dir_all(&ws1).unwrap();
        std::fs::create_dir_all(&ws2).unwrap();
        std::fs::write(ws1.join("note.md"), "hello RAG service test content").unwrap();
        std::fs::write(ws2.join("docs.md"), "secondary repo doc about embeddings").unwrap();
        let db = dir.path().join("idx.sqlite");
        let client = Client::new();
        let cfg = EmbedConfig::mock_from_env().expect("mock");
        let st = run_ingest(&[ws1.clone(), ws2.clone()], &db, &cfg, &client)
            .await
            .unwrap();
        assert!(st.embeddings_written >= 1);

        let r = query_index(
            &db,
            &client,
            &cfg,
            &QueryRequest {
                query: "RAG service".into(),
                top_k: 4,
            },
        )
        .await
        .unwrap();
        assert_eq!(r.phase, "1-sqlite");
        assert!(!r.hits.is_empty());
        assert!(r.hits.iter().all(|h| h.path.contains(':')));
        std::env::remove_var("CLAW_RAG_MOCK_PROVIDERS");
    }
}
