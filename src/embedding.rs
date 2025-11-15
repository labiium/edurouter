use crate::config::{EmbeddingConfig, EmbeddingProviderKind};
use crate::errors::RouterError;
use crate::types::RouteRequest;
use ahash::AHasher;
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use moka::future::Cache;
use parking_lot::Mutex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::hash::Hasher;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::task;

const HASH_EMBED_DIMS: usize = 48;
const MIN_CANONICAL_SCORE: f32 = 0.2;

#[derive(Debug, Clone)]
pub struct CanonicalSelection {
    pub model_id: String,
    pub canonical_ids: Vec<String>,
    pub score: f32,
}

pub struct EmbeddingRuntime {
    router: EmbeddingRouter,
    provider: Arc<dyn EmbeddingBackend>,
    cache: Cache<u64, Arc<Vec<f32>>>,
    top_k: usize,
}

impl EmbeddingRuntime {
    pub async fn new(cfg: &EmbeddingConfig) -> Result<Self, RouterError> {
        let provider: Arc<dyn EmbeddingBackend> = match &cfg.provider {
            EmbeddingProviderKind::FastEmbed { model } => {
                Arc::new(FastEmbedBackend::new(model).await?)
            }
            EmbeddingProviderKind::Hashed => Arc::new(HashingBackend),
        };

        let tasks = load_canonical_tasks(&cfg.canonical_path, provider.clone()).await?;
        let cache = Cache::builder()
            .max_capacity(2048)
            .time_to_live(Duration::from_millis(cfg.cache_ttl_ms))
            .build();

        Ok(Self {
            router: EmbeddingRouter::new(tasks),
            provider,
            cache,
            top_k: cfg.top_k,
        })
    }

    pub async fn select(
        &self,
        req: &RouteRequest,
    ) -> Result<Option<CanonicalSelection>, RouterError> {
        let text = match extract_summary(req) {
            Some(value) => value,
            None => return Ok(None),
        };
        let text_hash = hash_text(&text);
        let embedding = if let Some(hit) = self.cache.get(&text_hash).await {
            hit
        } else {
            let vectors = self.provider.embed(std::slice::from_ref(&text)).await?;
            if vectors.is_empty() {
                return Ok(None);
            }
            let vec = Arc::new(normalize(vectors.into_iter().next().unwrap()));
            self.cache.insert(text_hash, vec.clone()).await;
            vec
        };

        Ok(self.router.select(&embedding, self.top_k))
    }
}

#[derive(Debug)]
struct CanonicalTask {
    id: String,
    preferred_model: String,
    weight: f32,
    embedding: Vec<f32>,
}

#[derive(Debug)]
struct EmbeddingRouter {
    tasks: Vec<CanonicalTask>,
}

impl EmbeddingRouter {
    fn new(tasks: Vec<CanonicalTask>) -> Self {
        Self { tasks }
    }

    fn select(&self, query: &[f32], k: usize) -> Option<CanonicalSelection> {
        if self.tasks.is_empty() {
            return None;
        }
        let mut scored: Vec<(f32, &CanonicalTask)> = self
            .tasks
            .iter()
            .map(|task| {
                let sim = dot(&task.embedding, query);
                let weight = if task.weight <= 0.0 { 1.0 } else { task.weight };
                (sim * weight, task)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut aggregated: HashMap<&str, (f32, Vec<String>)> = HashMap::new();
        for (score, task) in scored.into_iter().take(k.max(1)) {
            if score <= 0.0 {
                continue;
            }
            aggregated
                .entry(&task.preferred_model)
                .and_modify(|entry| {
                    entry.0 += score;
                    entry.1.push(task.id.clone());
                })
                .or_insert_with(|| (score, vec![task.id.clone()]));
        }
        let (model_id, (score_sum, ids)) = aggregated.into_iter().max_by(|a, b| {
            a.1 .0
                .partial_cmp(&b.1 .0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let normalized = (score_sum / k.max(1) as f32).min(1.0);
        if normalized < MIN_CANONICAL_SCORE {
            return None;
        }
        Some(CanonicalSelection {
            model_id: model_id.to_string(),
            canonical_ids: ids,
            score: normalized,
        })
    }
}

#[derive(Debug, Deserialize)]
struct CanonicalTaskConfig {
    id: String,
    text: String,
    preferred_model: String,
    #[serde(default = "default_weight")]
    weight: f32,
    #[serde(default)]
    _tags: Vec<String>,
}

fn default_weight() -> f32 {
    1.0
}

async fn load_canonical_tasks(
    path: &Path,
    provider: Arc<dyn EmbeddingBackend>,
) -> Result<Vec<CanonicalTask>, RouterError> {
    let raw = std::fs::read_to_string(path)?;
    let configs: Vec<CanonicalTaskConfig> =
        serde_json::from_str(&raw).map_err(|err| RouterError::Planning(err.to_string()))?;
    if configs.is_empty() {
        return Err(RouterError::Planning(
            "canonical task list cannot be empty".into(),
        ));
    }
    let texts: Vec<String> = configs.iter().map(|cfg| cfg.text.clone()).collect();
    let vectors = provider.embed(&texts).await?;
    if vectors.len() != configs.len() {
        return Err(RouterError::Planning(
            "embedding provider returned unexpected output".into(),
        ));
    }
    let tasks = configs
        .into_iter()
        .zip(vectors)
        .map(|(cfg, vector)| CanonicalTask {
            id: cfg.id,
            preferred_model: cfg.preferred_model,
            weight: cfg.weight.max(0.1),
            embedding: normalize(vector),
        })
        .collect();
    Ok(tasks)
}

fn extract_summary(req: &RouteRequest) -> Option<String> {
    let convo = req
        .conversation
        .as_ref()
        .and_then(|conv| conv.summary.as_ref())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if convo.is_some() {
        return convo;
    }
    req.overrides
        .as_ref()
        .and_then(|ov| {
            ov.get("canonical_summary")
                .or_else(|| ov.get("summary"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = AHasher::default();
    hasher.write(text.as_bytes());
    hasher.finish()
}

fn normalize(mut vec: Vec<f32>) -> Vec<f32> {
    let norm = vec
        .iter()
        .map(|v| (*v as f64) * (*v as f64))
        .sum::<f64>()
        .sqrt();
    if norm == 0.0 {
        return vec;
    }
    let norm = norm as f32;
    for value in vec.iter_mut() {
        *value /= norm;
    }
    vec
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[async_trait]
trait EmbeddingBackend: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, RouterError>;
}

struct FastEmbedBackend {
    model: Arc<Mutex<TextEmbedding>>,
}

impl FastEmbedBackend {
    async fn new(model: &str) -> Result<Self, RouterError> {
        let parsed = match EmbeddingModel::from_str(model) {
            Ok(model) => model,
            Err(_) => map_friendly_model(model)?,
        };
        let builder = move || {
            TextEmbedding::try_new(
                InitOptions::new(parsed.clone()).with_show_download_progress(false),
            )
            .map_err(|err| RouterError::Planning(err.to_string()))
        };
        let model = task::spawn_blocking(builder)
            .await
            .map_err(|err| RouterError::Planning(err.to_string()))??;
        Ok(Self {
            model: Arc::new(Mutex::new(model)),
        })
    }
}

#[async_trait]
impl EmbeddingBackend for FastEmbedBackend {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, RouterError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let docs: Vec<String> = texts
            .iter()
            .map(|t| format!("query: {}", t.trim()))
            .collect();
        let model = self.model.clone();
        task::spawn_blocking(move || {
            let mut guard = model.lock();
            guard
                .embed(docs, None)
                .map(|vectors| vectors.into_iter().map(normalize).collect())
                .map_err(|err| RouterError::Planning(err.to_string()))
        })
        .await
        .map_err(|err| RouterError::Planning(err.to_string()))?
    }
}

#[derive(Default)]
struct HashingBackend;

#[async_trait]
impl EmbeddingBackend for HashingBackend {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, RouterError> {
        Ok(texts.iter().map(|t| hash_embedding(t)).collect())
    }
}

fn hash_embedding(text: &str) -> Vec<f32> {
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    let bytes = digest.finalize();
    let mut vec = vec![0f32; HASH_EMBED_DIMS];
    for (idx, value) in vec.iter_mut().enumerate() {
        let byte = bytes[idx % bytes.len()] as f32;
        *value = (byte / 255.0) * 2.0 - 1.0;
    }
    normalize(vec)
}

fn map_friendly_model(name: &str) -> Result<EmbeddingModel, RouterError> {
    let normalized = name.trim().to_ascii_lowercase();
    let model = match normalized.as_str() {
        "bge-small-en-v1.5" | "bge_small_en_v1.5" => EmbeddingModel::BGESmallENV15,
        "bge-base-en-v1.5" | "bge_base_en_v1.5" => EmbeddingModel::BGEBaseENV15,
        "bge-large-en-v1.5" | "bge_large_en_v1.5" => EmbeddingModel::BGELargeENV15,
        "all-minilm-l6-v2" | "all_minilm_l6_v2" => EmbeddingModel::AllMiniLML6V2,
        "all-minilm-l12-v2" | "all_minilm_l12_v2" => EmbeddingModel::AllMiniLML12V2,
        "nomic-embed-text-v1" => EmbeddingModel::NomicEmbedTextV1,
        "nomic-embed-text-v1.5" => EmbeddingModel::NomicEmbedTextV15,
        "gte-base-en-v1.5" => EmbeddingModel::GTEBaseENV15,
        "gte-large-en-v1.5" => EmbeddingModel::GTELargeENV15,
        "multilingual-e5-small" => EmbeddingModel::MultilingualE5Small,
        "multilingual-e5-base" => EmbeddingModel::MultilingualE5Base,
        "multilingual-e5-large" => EmbeddingModel::MultilingualE5Large,
        other => {
            return Err(RouterError::Planning(format!(
                "unsupported fastembed model '{other}'"
            )))
        }
    };
    Ok(model)
}

pub fn canonical_hash(selection: &CanonicalSelection) -> u64 {
    let mut hasher = AHasher::default();
    hasher.write(selection.model_id.as_bytes());
    for id in &selection.canonical_ids {
        hasher.write(id.as_bytes());
    }
    hasher.finish()
}
