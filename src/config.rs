use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use std::{env, path::PathBuf};

use crate::types::{CatalogDocument, PolicyDocument};

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub workers: usize,
}

#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub server: ServerConfig,
    pub overlay_dir: PathBuf,
    pub cache_ttl_ms: u64,
    pub cache_stale_ms: u64,
    pub sticky_secret: Vec<u8>,
    pub policy: PolicyDocument,
    pub catalog: CatalogDocument,
    pub rate_limit_burst: f64,
    pub rate_limit_refill_per_sec: f64,
    pub embedding: Option<EmbeddingConfig>,
}

impl RouterConfig {
    pub fn from_env() -> Result<Self> {
        let bind_addr = env::var("ROUTER_BIND").unwrap_or_else(|_| "0.0.0.0:9099".to_string());
        let workers = env::var("ROUTER_WORKERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(num_cpus::get_physical);

        let policy_path = PathBuf::from(
            env::var("ROUTER_POLICY_PATH").unwrap_or_else(|_| "./configs/policy.json".into()),
        );
        let catalog_path = PathBuf::from(
            env::var("ROUTER_CATALOG_PATH").unwrap_or_else(|_| "./configs/catalog.json".into()),
        );
        let overlay_dir = PathBuf::from(
            env::var("ROUTER_OVERLAY_DIR").unwrap_or_else(|_| "./configs/overlays".into()),
        );
        let cache_ttl_ms = env::var("ROUTER_CACHE_TTL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15_000);
        let cache_stale_ms = env::var("ROUTER_CACHE_STALE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(cache_ttl_ms);
        let rate_limit_burst = env::var("ROUTER_PLAN_RATE_BURST")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30.0);
        let rate_limit_refill_per_sec = env::var("ROUTER_PLAN_RATE_REFILL_PER_SEC")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10.0);

        let sticky_secret = match env::var("ROUTER_STICKY_SECRET") {
            Ok(value) if !value.is_empty() => {
                let engine = base64::engine::general_purpose::STANDARD;
                engine
                    .decode(value)
                    .context("decode ROUTER_STICKY_SECRET base64")?
            }
            _ => {
                tracing::warn!("ROUTER_STICKY_SECRET not set; using insecure default");
                b"labiium-router-dev-secret".to_vec()
            }
        };

        let policy_json = std::fs::read_to_string(&policy_path)
            .with_context(|| format!("read policy file at {:?}", policy_path))?;
        let catalog_json = std::fs::read_to_string(&catalog_path)
            .with_context(|| format!("read catalog file at {:?}", catalog_path))?;

        let policy: PolicyDocument = serde_json::from_str(&policy_json)
            .or_else(|_| serde_yaml::from_str(&policy_json))
            .with_context(|| "parse policy document")?;
        let catalog: CatalogDocument = serde_json::from_str(&catalog_json)
            .or_else(|_| serde_yaml::from_str(&catalog_json))
            .with_context(|| "parse catalog document")?;
        let embedding = embedding_from_env()?;

        Ok(Self {
            server: ServerConfig { bind_addr, workers },
            overlay_dir,
            cache_ttl_ms,
            cache_stale_ms,
            sticky_secret,
            policy,
            catalog,
            rate_limit_burst,
            rate_limit_refill_per_sec,
            embedding,
        })
    }
}

fn embedding_from_env() -> Result<Option<EmbeddingConfig>> {
    if !env_truthy("ROUTER_EMBEDDINGS_ENABLED") {
        return Ok(None);
    }

    let canonical_path = PathBuf::from(
        env::var("ROUTER_CANONICAL_TASKS")
            .unwrap_or_else(|_| "./configs/canonical_tasks.json".into()),
    );
    let top_k = env::var("ROUTER_EMBEDDINGS_TOP_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
        .max(1);
    let cache_ttl_ms = env::var("ROUTER_EMBEDDINGS_CACHE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300_000);
    let provider_name = env::var("ROUTER_EMBEDDINGS_PROVIDER")
        .unwrap_or_else(|_| "fastembed".into())
        .to_ascii_lowercase();
    let provider = match provider_name.as_str() {
        "fastembed" => EmbeddingProviderKind::FastEmbed {
            model: env::var("ROUTER_EMBEDDINGS_FASTEMBED_MODEL")
                .unwrap_or_else(|_| "bge-small-en-v1.5".into()),
        },
        "hashed" => {
            if !env_truthy("ROUTER_EMBEDDINGS_ALLOW_HASHED") {
                return Err(anyhow!(
                    "ROUTER_EMBEDDINGS_PROVIDER=hashed requires ROUTER_EMBEDDINGS_ALLOW_HASHED=1"
                ));
            }
            EmbeddingProviderKind::Hashed
        }
        other => {
            return Err(anyhow!(
                "unknown ROUTER_EMBEDDINGS_PROVIDER '{other}'. expected fastembed or hashed"
            ))
        }
    };

    Ok(Some(EmbeddingConfig {
        canonical_path,
        top_k,
        cache_ttl_ms,
        provider,
    }))
}

fn env_truthy(key: &str) -> bool {
    env::var(key)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    pub canonical_path: PathBuf,
    pub top_k: usize,
    pub cache_ttl_ms: u64,
    pub provider: EmbeddingProviderKind,
}

#[derive(Debug, Clone)]
pub enum EmbeddingProviderKind {
    FastEmbed { model: String },
    Hashed,
}
