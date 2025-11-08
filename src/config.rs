use anyhow::{Context, Result};
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
        })
    }
}
