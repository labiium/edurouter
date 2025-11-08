use crate::types::RoutePlan;
use ahash::AHasher;
use moka::future::Cache;
use std::{hash::Hasher, sync::Arc, time::Duration};

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
        CacheKey(hasher.finish())
    }
}

#[derive(Clone)]
pub struct PlanCache {
    inner: Cache<CacheKey, Arc<RoutePlan>>,
}

impl PlanCache {
    pub fn new(capacity: u64, ttl_ms: u64) -> Self {
        let ttl = Duration::from_millis(ttl_ms);
        let inner = Cache::builder()
            .max_capacity(capacity)
            .time_to_live(ttl)
            .support_invalidation_closures()
            .build();
        Self { inner }
    }

    pub async fn get(&self, key: &CacheKey) -> Option<Arc<RoutePlan>> {
        self.inner.get(key).await
    }

    pub async fn insert(&self, key: CacheKey, plan: Arc<RoutePlan>) {
        self.inner.insert(key, plan).await;
    }

    pub async fn clear(&self) {
        self.inner.invalidate_all();
    }
}
