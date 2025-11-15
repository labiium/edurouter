use crate::types::{ApiKind, CacheStatus, PrivacyMode, RoutePlan};
use ahash::AHasher;
use chrono::{DateTime, Utc};
use moka::future::Cache;
use std::hash::Hasher;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey(pub u64);

impl CacheKey {
    #[allow(clippy::too_many_arguments)]
    pub fn derive(
        policy_rev: &str,
        alias_idx: u64,
        caps_mask: u64,
        json_mode: bool,
        in_bucket: u16,
        out_bucket: u16,
        region_mask: u32,
        boost: bool,
        plan_token_model: u32,
        overlay_hash: u64,
        privacy: PrivacyMode,
        api: ApiKind,
        freeze_hash: u64,
        canonical_hash: u64,
    ) -> Self {
        let mut hasher = AHasher::default();
        hasher.write(policy_rev.as_bytes());
        hasher.write_u64(alias_idx);
        hasher.write_u64(caps_mask);
        hasher.write_u8(json_mode as u8);
        hasher.write_u16(in_bucket);
        hasher.write_u16(out_bucket);
        hasher.write_u32(region_mask);
        hasher.write_u8(boost as u8);
        hasher.write_u32(plan_token_model);
        hasher.write_u64(overlay_hash);
        hasher.write_u8(privacy as u8);
        hasher.write_u8(api as u8);
        hasher.write_u64(freeze_hash);
        hasher.write_u64(canonical_hash);
        CacheKey(hasher.finish())
    }
}

#[derive(Clone)]
pub struct CacheHit {
    pub plan: Arc<RoutePlan>,
    pub status: CacheStatus,
    pub route_reason: Option<String>,
}

#[derive(Clone)]
struct CachedPlan {
    plan: Arc<RoutePlan>,
    inserted_at: Instant,
    valid_until: Option<DateTime<Utc>>,
    route_reason: Option<String>,
}

#[derive(Clone)]
pub struct PlanCache {
    inner: Cache<CacheKey, CachedPlan>,
    fresh_ttl: Duration,
}

impl PlanCache {
    pub fn new(capacity: u64, fresh_ttl_ms: u64, stale_extension_ms: u64) -> Self {
        let fresh = Duration::from_millis(fresh_ttl_ms);
        let ttl = fresh + Duration::from_millis(stale_extension_ms);
        let inner = Cache::builder()
            .max_capacity(capacity)
            .time_to_live(ttl)
            .support_invalidation_closures()
            .build();
        Self {
            inner,
            fresh_ttl: fresh,
        }
    }

    pub async fn get(&self, key: &CacheKey) -> Option<CacheHit> {
        self.inner.get(key).await.map(|entry| {
            let now = Instant::now();
            let mut status = CacheStatus::Hit;
            if now.duration_since(entry.inserted_at) > self.fresh_ttl {
                status = CacheStatus::Stale;
            }
            if let Some(valid_until) = entry.valid_until {
                if valid_until <= Utc::now() {
                    status = CacheStatus::Stale;
                }
            }
            CacheHit {
                plan: entry.plan.clone(),
                status,
                route_reason: entry.route_reason.clone(),
            }
        })
    }

    pub async fn insert(
        &self,
        key: CacheKey,
        plan: Arc<RoutePlan>,
        valid_until: Option<DateTime<Utc>>,
        route_reason: Option<String>,
    ) {
        let entry = CachedPlan {
            plan,
            inserted_at: Instant::now(),
            valid_until,
            route_reason,
        };
        self.inner.insert(key, entry).await;
    }

    pub async fn clear(&self) {
        self.inner.invalidate_all();
    }
}
