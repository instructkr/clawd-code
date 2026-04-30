use crate::{QueryResponse, RagHit};
use serde_json::json;

async fn ensure_collection(
    client: &qdrant_client::Qdrant,
    collection: &str,
    dim: usize,
) -> Result<(), String> {
    let dim_u64 = u64::try_from(dim).map_err(|_| "embedding dim too large".to_string())?;

    // Try to create the collection; if it already exists, Qdrant will error.
    // We treat "already exists" as success to keep ingest idempotent.
    let res = client
        .create_collection(
            qdrant_client::qdrant::CreateCollectionBuilder::new(collection).vectors_config(
                qdrant_client::qdrant::VectorParamsBuilder::new(
                    dim_u64,
                    qdrant_client::qdrant::Distance::Cosine,
                ),
            ),
        )
        .await;

    match res {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") || msg.contains("Already exists") {
                Ok(())
            } else {
                Err(format!("qdrant create_collection: {e}"))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct QdrantConfig {
    pub url: String,
    pub api_key: Option<String>,
    pub collection: String,
}

impl QdrantConfig {
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("CLAW_RAG_QDRANT_URL").ok()?;
        let collection = std::env::var("CLAW_RAG_QDRANT_COLLECTION")
            .ok()
            .unwrap_or_else(|| "claw_rag_chunks".to_string());
        let api_key = std::env::var("CLAW_RAG_QDRANT_API_KEY").ok();
        Some(Self {
            url,
            api_key,
            collection,
        })
    }
}

pub async fn query_qdrant(q: &[f32], top_k: u32) -> Result<Option<QueryResponse>, String> {
    let Some(cfg) = QdrantConfig::from_env() else {
        return Ok(None);
    };

    let limit = top_k.min(64);
    let mut client = qdrant_client::Qdrant::from_url(&cfg.url);
    if let Some(key) = &cfg.api_key {
        client = client.api_key(key.clone());
    }
    let client = client.build().map_err(|e| format!("qdrant client: {e}"))?;

    // If collection doesn't exist yet, treat it as "no results" and fall back.
    // (We avoid creating it on query because ingest controls dimension/model.)
    if let Err(e) = client.collection_info(&cfg.collection).await {
        let msg = e.to_string();
        if msg.contains("doesn't exist") || msg.contains("Not found") {
            return Ok(None);
        }
        return Err(format!("qdrant collection_info: {e}"));
    }

    let res = client
        .query(
            qdrant_client::qdrant::QueryPointsBuilder::new(&cfg.collection)
                .query(q.to_vec())
                .limit(u64::from(limit))
                .with_payload(true),
        )
        .await
        .map_err(|e| format!("qdrant query: {e}"))?;

    let mut hits = Vec::new();
    for p in res.result {
        let payload = p.payload;
        let path = payload
            .get("path")
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .unwrap_or_default();
        let text = payload
            .get("text")
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .unwrap_or_default();
        let score = p.score;
        if !path.is_empty() {
            hits.push(RagHit {
                path,
                snippet: truncate_snippet(&text, 480),
                score: Some(score),
            });
        }
    }

    Ok(Some(QueryResponse {
        hits,
        phase: "2-qdrant",
    }))
}

#[derive(Debug, Clone)]
pub struct ChunkPoint {
    pub id: i64,
    pub vec: Vec<f32>,
    pub path: String,
    pub text: String,
}

pub async fn upsert_points(points: Vec<ChunkPoint>) -> Result<(), String> {
    let Some(cfg) = QdrantConfig::from_env() else {
        return Ok(());
    };
    if points.is_empty() {
        return Ok(());
    }

    let mut client = qdrant_client::Qdrant::from_url(&cfg.url);
    if let Some(key) = &cfg.api_key {
        client = client.api_key(key.clone());
    }
    let client = client.build().map_err(|e| format!("qdrant client: {e}"))?;

    let dim = points[0].vec.len();
    ensure_collection(&client, &cfg.collection, dim).await?;

    let mut qpoints = Vec::with_capacity(points.len());
    for p in points {
        if p.vec.len() != dim {
            return Err("qdrant upsert: embedding dimension mismatch within batch".to_string());
        }
        let id = u64::try_from(p.id).map_err(|_| "chunk id must be non-negative".to_string())?;
        let payload_map = serde_json::Map::from_iter([
            ("path".to_string(), json!(p.path)),
            ("text".to_string(), json!(p.text)),
        ]);
        let payload: qdrant_client::Payload = payload_map.into();

        qpoints.push(qdrant_client::qdrant::PointStruct::new(id, p.vec, payload));
    }

    client
        .upsert_points(qdrant_client::qdrant::UpsertPointsBuilder::new(
            &cfg.collection,
            qpoints,
        ))
        .await
        .map_err(|e| format!("qdrant upsert: {e}"))?;

    Ok(())
}

fn truncate_snippet(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect::<String>() + "…"
}
