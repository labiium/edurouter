use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RateLimiter {
    buckets: Arc<DashMap<String, Bucket>>,
    capacity: f64,
    refill_per_sec: f64,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            capacity,
            refill_per_sec,
        }
    }

    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut entry = self
            .buckets
            .entry(key.to_string())
            .or_insert_with(|| Bucket {
                tokens: self.capacity,
                last_refill: now,
            });
        let elapsed = now.duration_since(entry.last_refill);
        if elapsed > Duration::ZERO {
            let refill = elapsed.as_secs_f64() * self.refill_per_sec;
            entry.tokens = (entry.tokens + refill).min(self.capacity);
            entry.last_refill = now;
        }
        if entry.tokens >= 1.0 {
            entry.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}
