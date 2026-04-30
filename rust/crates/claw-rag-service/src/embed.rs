//! OpenAI-compatible embeddings HTTP client.

use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct EmbedConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

impl EmbedConfig {
    pub fn from_env() -> Result<Self, String> {
        let api_key = std::env::var("CLAW_RAG_OPENAI_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .map_err(|_| {
                "set CLAW_RAG_OPENAI_API_KEY or OPENAI_API_KEY for embeddings".to_string()
            })?;
        let base_url = std::env::var("CLAW_RAG_EMBEDDING_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".into());
        let model = std::env::var("CLAW_RAG_EMBEDDING_MODEL")
            .unwrap_or_else(|_| "text-embedding-3-small".into());
        Ok(Self {
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
        })
    }

    /// Deterministic fake vectors for tests / dry-run (1536 dims match common `OpenAI` models;
    /// truncated scan still works if dim mismatches — ingest uses same mock for all).
    #[must_use]
    pub fn mock_from_env() -> Option<Self> {
        if std::env::var("CLAW_RAG_MOCK_PROVIDERS").ok().as_deref() != Some("1") {
            return None;
        }
        Some(Self {
            api_key: "mock".into(),
            base_url: "mock://".into(),
            model: "mock-embedding".into(),
        })
    }
}

#[derive(Serialize)]
struct EmbeddingsRequest<'a> {
    model: &'a str,
    input: Vec<&'a str>,
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
}

pub async fn embed_batch(
    client: &Client,
    cfg: &EmbedConfig,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    if cfg.base_url.starts_with("mock://") {
        return Ok(texts
            .iter()
            .map(|s| mock_vector_for_text(s.as_str()))
            .collect());
    }

    let url = format!("{}/embeddings", cfg.base_url);
    let inputs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let body = EmbeddingsRequest {
        model: &cfg.model,
        input: inputs,
    };
    let res = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let t = res.text().await.unwrap_or_default();
        return Err(format!("embeddings HTTP error: {t}"));
    }
    let parsed: EmbeddingsResponse = res.json().await.map_err(|e| e.to_string())?;
    if parsed.data.len() != texts.len() {
        return Err(format!(
            "embeddings count mismatch: got {} for {} inputs",
            parsed.data.len(),
            texts.len()
        ));
    }
    Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
}

fn mock_vector_for_text(s: &str) -> Vec<f32> {
    const DIM: usize = 16;
    let mut v = vec![0f32; DIM];
    for (i, b) in s.bytes().enumerate().take(DIM * 4) {
        v[i % DIM] += f32::from(b) / 255.0;
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}
