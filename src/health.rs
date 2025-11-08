use crate::types::RouteFeedback;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct HealthStats {
    pub p50_ms: f32,
    pub p95_ms: f32,
    pub err_rate: f32,
    pub tokens_per_sec: f32,
    pub last_update: DateTime<Utc>,
}

impl Default for HealthStats {
    fn default() -> Self {
        Self {
            p50_ms: 700.0,
            p95_ms: 2100.0,
            err_rate: 0.01,
            tokens_per_sec: 300.0,
            last_update: Utc::now(),
        }
    }
}

#[derive(Clone)]
pub struct HealthStore {
    inner: Arc<DashMap<String, HealthStats>>,
}

impl Default for HealthStore {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    pub fn snapshot(&self, model_id: &str) -> HealthStats {
        self.inner
            .get(model_id)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    pub fn update(&self, feedback: &RouteFeedback) {
        let mut entry = self.inner.entry(feedback.model_id.clone()).or_default();
        let alpha = 0.2_f32;
        let latency = feedback.duration_ms as f32;
        entry.p50_ms = blend(entry.p50_ms, latency, alpha);
        entry.p95_ms = blend(entry.p95_ms, latency * 1.3, alpha / 2.0);
        let err = if feedback.success { 0.0 } else { 1.0 };
        entry.err_rate = blend(entry.err_rate, err, 0.1);
        if let Some(usage) = &feedback.usage {
            let total_tokens = (usage.prompt_tokens + usage.completion_tokens) as f32;
            if feedback.duration_ms > 0 {
                let tps = total_tokens / (feedback.duration_ms as f32 / 1000.0);
                entry.tokens_per_sec = blend(entry.tokens_per_sec, tps, 0.2);
            }
        }
        entry.last_update = Utc::now();
    }
}

fn blend(prev: f32, new: f32, alpha: f32) -> f32 {
    prev + (new - prev) * alpha.clamp(0.0, 1.0)
}
