use crate::cache::{CacheKey, PlanCache};
use crate::config::RouterConfig;
use crate::embedding::{canonical_hash, CanonicalSelection, EmbeddingRuntime};
use crate::errors::RouterError;
use crate::health::{HealthStats, HealthStore};
use crate::rate::RateLimiter;
use crate::stickiness::{StickinessClaims, StickinessManager};
use crate::types::*;
use arc_swap::ArcSwap;
use dashmap::DashMap;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

const MAX_CACHE_CAPACITY: u64 = 100_000;
const DEFAULT_PROMPT_TOKENS: u32 = 512;
const DEFAULT_OUTPUT_TOKENS: u32 = 256;

bitflags::bitflags! {
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
    struct CapabilityFlags: u32 {
        const TEXT = 1 << 0;
        const VISION = 1 << 1;
        const TOOLS = 1 << 2;
        const JSON = 1 << 3;
        const STRUCTURED = 1 << 4;
        const PROMPT_CACHE = 1 << 5;
    }
}

bitflags::bitflags! {
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
    struct RegionMask: u32 {
        const GLOBAL = 1 << 0;
        const EU = 1 << 1;
        const US = 1 << 2;
        const APAC = 1 << 3;
        const EDGE = 1 << 4;
    }
}

#[derive(Debug, Clone)]
struct CompiledModel {
    id: String,
    provider: String,
    base_url: String,
    mode: UpstreamMode,
    auth_env: Option<String>,
    headers: HashMap<String, String>,
    capabilities: CapabilityFlags,
    regions: RegionMask,
    context_tokens: u32,
    prices: ModelPrice,
    target_latency_ms: u32,
    base_latency_ms: u32,
    status: ModelStatus,
    policy_tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelStatus {
    Healthy,
    Degraded,
    Offline,
}

#[derive(Debug, Clone, Copy)]
struct ModelPrice {
    input_micro_per_million: u64,
    output_micro_per_million: u64,
    cached_micro_per_million: u64,
}

#[derive(Debug, Clone)]
struct CompiledCatalog {
    pub revision: String,
    models: Vec<CompiledModel>,
    index: HashMap<String, usize>,
    pub raw: CatalogDocument,
}

#[derive(Debug, Clone)]
struct CompiledAlias {
    candidates: Vec<usize>,
    require_caps: CapabilityFlags,
    allowed_regions: RegionMask,
}

#[derive(Debug, Clone)]
struct CompiledPolicy {
    pub doc: PolicyDocument,
    alias_map: HashMap<String, CompiledAlias>,
    escalation_regex: Option<Regex>,
}

#[derive(Debug, Clone, Default)]
struct OverlayStore {
    content: HashMap<String, String>,
}

#[derive(Debug, Default, Clone)]
struct RouterMetrics {
    total_requests: Arc<DashMap<String, u64>>, // alias -> count
    model_share: Arc<DashMap<String, u64>>,    // model -> count
    cache_hits: Arc<DashMap<String, u64>>,     // alias -> hits
}

pub struct RouterEngine {
    overlay_dir: String,
    cache: PlanCache,
    cache_ttl_ms: u64,
    policy: ArcSwap<CompiledPolicy>,
    catalog: ArcSwap<CompiledCatalog>,
    overlays: ArcSwap<OverlayStore>,
    stickiness: StickinessManager,
    health: HealthStore,
    metrics: RouterMetrics,
    rate_limiter: RateLimiter,
    embedding: Option<Arc<EmbeddingRuntime>>,
}

pub struct PlanOutcome {
    pub plan: RoutePlan,
    pub cache_status: CacheStatus,
    pub policy_revision: String,
    pub catalog_revision: String,
    pub route_reason: Option<String>,
}

impl RouterEngine {
    pub async fn bootstrap(cfg: &RouterConfig) -> Result<Self, RouterError> {
        let compiled_catalog = compile_catalog(&cfg.catalog)?;
        let compiled_policy = compile_policy(&cfg.policy, &compiled_catalog)?;
        let overlays = load_overlays(&cfg.overlay_dir)?;

        let cache = PlanCache::new(MAX_CACHE_CAPACITY, cfg.cache_ttl_ms, cfg.cache_stale_ms);
        let policy = ArcSwap::from_pointee(compiled_policy);
        let catalog = ArcSwap::from_pointee(compiled_catalog);
        let overlays = ArcSwap::from_pointee(overlays);

        let embedding = match cfg.embedding.as_ref() {
            Some(settings) => Some(Arc::new(EmbeddingRuntime::new(settings).await?)),
            None => None,
        };

        Ok(Self {
            overlay_dir: cfg.overlay_dir.to_string_lossy().into_owned(),
            cache,
            cache_ttl_ms: cfg.cache_ttl_ms,
            policy,
            catalog,
            overlays,
            stickiness: StickinessManager::new(cfg.sticky_secret.clone()),
            health: HealthStore::new(),
            metrics: RouterMetrics::default(),
            rate_limiter: RateLimiter::new(cfg.rate_limit_burst, cfg.rate_limit_refill_per_sec),
            embedding,
        })
    }

    pub fn health(&self) -> &HealthStore {
        &self.health
    }

    pub fn check_rate_limit(&self, key: &str) -> Result<(), RouterError> {
        if self.rate_limiter.check(key) {
            Ok(())
        } else {
            Err(RouterError::PolicyDeny(format!(
                "rate limit exceeded for {key}"
            )))
        }
    }

    pub async fn reload_policy(&self, doc: PolicyDocument) -> Result<(), RouterError> {
        let compiled = compile_policy(&doc, &self.catalog.load())?;
        self.policy.store(Arc::new(compiled));
        self.cache.clear().await;
        Ok(())
    }

    pub async fn reload_catalog(&self, doc: CatalogDocument) -> Result<(), RouterError> {
        let compiled = compile_catalog(&doc)?;
        self.catalog.store(Arc::new(compiled));
        let compiled_policy = compile_policy(&self.policy.load().doc, &self.catalog.load())?;
        self.policy.store(Arc::new(compiled_policy));
        self.cache.clear().await;
        Ok(())
    }

    pub async fn reload_overlays(&self) -> Result<(), RouterError> {
        let overlays = load_overlays(Path::new(&self.overlay_dir))?;
        self.overlays.store(Arc::new(overlays));
        Ok(())
    }

    pub async fn plan(&self, req: RouteRequest) -> Result<PlanOutcome, RouterError> {
        let policy = self.policy.load();
        let catalog = self.catalog.load();
        let overlays = self.overlays.load();

        let alias = policy
            .alias_map
            .get(&req.alias)
            .ok_or_else(|| RouterError::UnknownAlias(req.alias.clone()))?;

        let caps_mask = caps_from_request(&req);
        let json_mode = req
            .params
            .as_ref()
            .and_then(|val| val.get("json_mode"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let in_tokens = req
            .estimates
            .as_ref()
            .and_then(|est| est.prompt_tokens)
            .unwrap_or(DEFAULT_PROMPT_TOKENS);
        let mut out_tokens = req
            .estimates
            .as_ref()
            .and_then(|est| est.max_output_tokens)
            .unwrap_or_else(|| policy.doc.defaults.max_output_tokens);
        if out_tokens == 0 {
            out_tokens = DEFAULT_OUTPUT_TOKENS;
        }

        let region_mask = region_from_request(&req);
        let boost = has_teacher_boost(&req);
        let sticky_claims = req
            .overrides
            .as_ref()
            .and_then(|ov| ov.get("plan_token"))
            .and_then(Value::as_str)
            .and_then(|token| match self.stickiness.verify(token) {
                Ok(claims) => Some(claims),
                Err(err) => {
                    tracing::warn!("invalid stickiness token: {err}");
                    None
                }
            });
        let plan_token_model = sticky_claims
            .as_ref()
            .and_then(|claims| catalog.index.get(&claims.model_id))
            .copied()
            .unwrap_or_default() as u32;
        let content_used = determine_content_usage(&req);
        let freeze_key = freeze_key_from_request(&req, &policy.doc.revision);
        let prompt_overlays = resolve_overlay(
            &req,
            &policy.doc,
            &overlays,
            policy.doc.defaults.max_overlay_bytes,
        )?;
        let overlay_hash = hash_string(
            prompt_overlays
                .overlay_fingerprint
                .as_deref()
                .unwrap_or("overlay:none"),
        );
        let (forced_tier, mut base_reason) = determine_escalation(
            &req,
            &policy.doc,
            policy.escalation_regex.as_ref(),
            in_tokens,
            boost,
        );
        let forced_tag = forced_tier.as_ref().map(|tier| format!("tier:{}", tier));

        let canonical_match = if let Some(runtime) = self.embedding.as_ref() {
            match runtime.select(&req).await {
                Ok(result) => result,
                Err(err) => {
                    tracing::warn!("embedding routing failed: {err}");
                    None
                }
            }
        } else {
            None
        };
        if let Some(selection) = canonical_match.as_ref() {
            if base_reason.is_none() {
                base_reason = Some(format!("canonical:{}", selection.model_id));
            }
        }

        let cache_key = CacheKey::derive(
            &policy.doc.revision,
            hash_alias(&req.alias),
            caps_mask.bits() as u64,
            json_mode,
            bucket_tokens(in_tokens),
            bucket_tokens(out_tokens),
            region_mask.bits(),
            boost,
            plan_token_model,
            overlay_hash,
            req.privacy_mode,
            req.api,
            hash_string(&freeze_key),
            canonical_match.as_ref().map(canonical_hash).unwrap_or(0),
        );

        if let Some(hit) = self.cache.get(&cache_key).await {
            let mut response_plan = (*hit.plan).clone();
            self.attach_stickiness(
                &policy.doc,
                &req,
                &mut response_plan,
                sticky_claims.as_ref(),
            )?;
            let mut effective_reason = hit.route_reason.clone();
            if sticky_claims.is_some() {
                effective_reason = Some("policy_lock".into());
            }
            self.metrics
                .total_requests
                .entry(req.alias.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
            self.metrics
                .cache_hits
                .entry(req.alias.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
            return Ok(PlanOutcome {
                plan: response_plan,
                cache_status: hit.status,
                policy_revision: policy.doc.revision.clone(),
                catalog_revision: catalog.revision.clone(),
                route_reason: effective_reason,
            });
        }

        let forced_tag_value = forced_tag.clone();
        let mut candidates = score_candidates(ScoreContext {
            req: &req,
            alias,
            policy: &policy.doc,
            catalog: &catalog,
            health: &self.health,
            caps_mask,
            in_tokens,
            out_tokens,
            region_mask,
            boost,
            forced_tier: forced_tag_value.as_deref(),
            canonical_hint: canonical_match.as_ref(),
        })?;
        if candidates.is_empty() && forced_tag_value.is_some() {
            base_reason = None;
            candidates = score_candidates(ScoreContext {
                req: &req,
                alias,
                policy: &policy.doc,
                catalog: &catalog,
                health: &self.health,
                caps_mask,
                in_tokens,
                out_tokens,
                region_mask,
                boost,
                forced_tier: None,
                canonical_hint: canonical_match.as_ref(),
            })?;
        }

        let best = choose_primary(&candidates, sticky_claims.as_ref(), &req.alias)
            .ok_or_else(|| RouterError::Planning("no candidates after scoring".into()))?;

        if sticky_claims
            .as_ref()
            .map(|claims| claims.alias == req.alias && claims.model_id == best.model.id)
            .unwrap_or(false)
        {
            base_reason = Some("policy_lock".into());
        }

        let fallbacks = build_fallbacks(&candidates, best)
            .into_iter()
            .map(|cand| Fallback {
                base_url: cand.model.base_url.clone(),
                mode: cand.model.mode.clone(),
                model_id: cand.model.id.clone(),
                reason: Some("alternate".into()),
                penalty: Some(cand.penalty),
            })
            .collect::<Vec<_>>();

        let catalog_revision = catalog.revision.clone();
        let plan_blueprint = materialize_plan(PlanAssembly {
            req: &req,
            policy: &policy.doc,
            prompt_overlays,
            primary: best,
            fallbacks: &fallbacks,
            out_tokens,
            content_used,
            cache_ttl_ms: self.cache_ttl_ms as u32,
            freeze_key,
            catalog_revision: &catalog_revision,
            canonical: canonical_match.clone(),
        })?;
        let mut response_plan = plan_blueprint.clone();
        let issued_stickiness = self.attach_stickiness(
            &policy.doc,
            &req,
            &mut response_plan,
            sticky_claims.as_ref(),
        )?;
        let valid_until = issued_stickiness.map(|claims| claims.expires_at);

        let plan_arc = Arc::new(plan_blueprint);
        self.cache
            .insert(cache_key, plan_arc, valid_until, base_reason.clone())
            .await;

        self.metrics
            .model_share
            .entry(best.model.id.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        self.metrics
            .total_requests
            .entry(req.alias.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);

        Ok(PlanOutcome {
            plan: response_plan,
            cache_status: CacheStatus::Miss,
            policy_revision: policy.doc.revision.clone(),
            catalog_revision,
            route_reason: base_reason,
        })
    }

    pub fn stats(&self) -> RouterStats {
        RouterStats {
            policy_revision: self.policy.load().doc.revision.clone(),
            catalog_revision: self.catalog.load().revision.clone(),
            total_requests: self
                .metrics
                .total_requests
                .iter()
                .map(|entry| *entry.value())
                .sum(),
            cache_hit_ratio: calc_cache_ratio(&self.metrics),
            model_share: self
                .metrics
                .model_share
                .iter()
                .map(|entry| (entry.key().clone(), *entry.value()))
                .collect(),
            error_rate: 0.0,
        }
    }

    pub fn policy_document(&self) -> PolicyDocument {
        self.policy.load().doc.clone()
    }

    pub fn catalog_document(&self) -> CatalogDocument {
        self.catalog.load().raw.clone()
    }

    pub fn policy_revision(&self) -> String {
        self.policy.load().doc.revision.clone()
    }

    pub fn catalog_revision(&self) -> String {
        self.catalog.load().revision.clone()
    }
}

struct CandidateRef<'a> {
    model: &'a CompiledModel,
    score: f32,
    est_cost_micro: u64,
    est_latency_ms: u32,
    penalty: f32,
}

struct ScoreContext<'req, 'a, 'tier> {
    req: &'req RouteRequest,
    alias: &'a CompiledAlias,
    policy: &'req PolicyDocument,
    catalog: &'a CompiledCatalog,
    health: &'req HealthStore,
    caps_mask: CapabilityFlags,
    in_tokens: u32,
    out_tokens: u32,
    region_mask: RegionMask,
    boost: bool,
    forced_tier: Option<&'tier str>,
    canonical_hint: Option<&'tier CanonicalSelection>,
}

#[derive(Clone, Copy)]
struct ScoreFactors {
    est_cost_micro: u64,
    est_latency_ms: u32,
    in_tokens: u32,
    out_tokens: u32,
}

struct PlanAssembly<'req, 'a> {
    req: &'req RouteRequest,
    policy: &'req PolicyDocument,
    prompt_overlays: PromptOverlays,
    primary: &'a CandidateRef<'a>,
    fallbacks: &'a [Fallback],
    out_tokens: u32,
    content_used: ContentLevel,
    cache_ttl_ms: u32,
    freeze_key: String,
    catalog_revision: &'req str,
    canonical: Option<CanonicalSelection>,
}

fn compile_catalog(doc: &CatalogDocument) -> Result<CompiledCatalog, RouterError> {
    let mut models = Vec::with_capacity(doc.models.len());
    let mut index = HashMap::new();
    for (idx, model) in doc.models.iter().enumerate() {
        let metadata = &model.metadata;
        let base_url = metadata
            .get("base_url")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                RouterError::Planning(format!(
                    "catalog model {} missing metadata.base_url",
                    model.id
                ))
            })?
            .to_string();
        let mode = metadata
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("responses")
            .to_lowercase();
        let upstream_mode = match mode.as_str() {
            "chat" => UpstreamMode::Chat,
            _ => UpstreamMode::Responses,
        };
        let auth_env = metadata
            .get("auth_env")
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        let headers = metadata
            .get("headers")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let mut capability = CapabilityFlags::empty();
        for modality in &model.capabilities.modalities {
            match modality.to_lowercase().as_str() {
                "text" => capability.insert(CapabilityFlags::TEXT),
                "vision" => capability.insert(CapabilityFlags::VISION),
                _ => {}
            }
        }
        if model.capabilities.tools {
            capability.insert(CapabilityFlags::TOOLS);
        }
        if model.capabilities.json_mode {
            capability.insert(CapabilityFlags::JSON);
        }
        if model.capabilities.structured_output {
            capability.insert(CapabilityFlags::STRUCTURED);
        }
        if model.capabilities.prompt_cache {
            capability.insert(CapabilityFlags::PROMPT_CACHE);
        }
        let regions = if model.region.is_empty() {
            RegionMask::GLOBAL
        } else {
            model
                .region
                .iter()
                .fold(RegionMask::empty(), |mask, region| {
                    mask | region_from_str(region)
                })
        };
        let status = match model.status.as_deref().map(|s| s.to_lowercase()).as_deref() {
            Some("degraded") => ModelStatus::Degraded,
            Some("offline") | Some("drained") => ModelStatus::Offline,
            _ => ModelStatus::Healthy,
        };
        let recent = model.slos.recent.as_ref();
        let target_latency_ms = model.slos.target_p95_ms;
        let base_latency_ms = recent
            .and_then(|r| r.p50_ms)
            .unwrap_or((target_latency_ms as f32 * 0.4) as u32);

        let prices = ModelPrice {
            input_micro_per_million: model.cost.input_per_million_micro,
            output_micro_per_million: model.cost.output_per_million_micro,
            cached_micro_per_million: model
                .cost
                .cached_per_million_micro
                .unwrap_or(model.cost.input_per_million_micro / 2),
        };

        let compiled = CompiledModel {
            id: model.id.clone(),
            provider: model.provider.clone(),
            base_url,
            mode: upstream_mode,
            auth_env,
            headers,
            capabilities: capability,
            regions,
            context_tokens: model.capabilities.context_tokens.max(1024),
            prices,
            target_latency_ms,
            base_latency_ms,
            status,
            policy_tags: model.policy_tags.clone(),
        };
        index.insert(model.id.clone(), idx);
        models.push(compiled);
    }

    Ok(CompiledCatalog {
        revision: doc.revision.clone(),
        models,
        index,
        raw: doc.clone(),
    })
}

fn compile_policy(
    doc: &PolicyDocument,
    catalog: &CompiledCatalog,
) -> Result<CompiledPolicy, RouterError> {
    let mut alias_map = HashMap::new();
    for (alias, rule) in &doc.aliases {
        let mut candidates = Vec::new();
        for model_id in &rule.candidates {
            if let Some(idx) = catalog.index.get(model_id) {
                candidates.push(*idx);
            } else {
                tracing::warn!(alias, model_id, "alias references unknown model");
            }
        }
        let require_caps = caps_from_strings(&rule.require_caps);
        let allowed_regions = if rule.allowed_regions.is_empty() {
            RegionMask::all()
        } else {
            rule.allowed_regions
                .iter()
                .fold(RegionMask::empty(), |mask, region| {
                    mask | region_from_str(region)
                })
        };
        alias_map.insert(
            alias.clone(),
            CompiledAlias {
                candidates,
                require_caps,
                allowed_regions,
            },
        );
    }

    let escalation_regex = doc
        .escalations
        .uncertainty_regex
        .as_ref()
        .and_then(|pattern| Regex::new(pattern).ok());

    Ok(CompiledPolicy {
        doc: doc.clone(),
        alias_map,
        escalation_regex,
    })
}

fn load_overlays(dir: &Path) -> Result<OverlayStore, RouterError> {
    let mut content = HashMap::new();
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                        let data = std::fs::read_to_string(&path)?;
                        content.insert(name.to_string(), data);
                    }
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = ?dir, "overlay directory missing; continuing with empty overlays");
        }
        Err(err) => return Err(err.into()),
    }
    Ok(OverlayStore { content })
}

fn region_from_request(req: &RouteRequest) -> RegionMask {
    req.geo
        .as_ref()
        .and_then(|geo| geo.region.as_ref())
        .map(|region| region_from_str(region))
        .unwrap_or_else(RegionMask::all)
}

fn region_from_str(region: &str) -> RegionMask {
    match region.to_ascii_lowercase().as_str() {
        "eu" | "eu-west-1" => RegionMask::EU,
        "us" | "us-east-1" | "useast" => RegionMask::US,
        "apac" | "ap-southeast-1" => RegionMask::APAC,
        "edge" => RegionMask::EDGE,
        _ => RegionMask::GLOBAL,
    }
}

fn caps_from_strings(values: &[String]) -> CapabilityFlags {
    let mut flags = CapabilityFlags::empty();
    for value in values {
        match value.to_ascii_lowercase().as_str() {
            "text" => flags.insert(CapabilityFlags::TEXT),
            "vision" => flags.insert(CapabilityFlags::VISION),
            "tools" => flags.insert(CapabilityFlags::TOOLS),
            "json" | "json_mode" => flags.insert(CapabilityFlags::JSON),
            "structured" | "structured_output" => flags.insert(CapabilityFlags::STRUCTURED),
            "prompt_cache" => flags.insert(CapabilityFlags::PROMPT_CACHE),
            _ => {}
        }
    }
    flags
}

fn caps_from_request(req: &RouteRequest) -> CapabilityFlags {
    let mut flags = caps_from_strings(&req.caps);
    if req
        .tools
        .as_ref()
        .map(|tools| !tools.is_empty())
        .unwrap_or(false)
    {
        flags.insert(CapabilityFlags::TOOLS);
    }
    if req
        .params
        .as_ref()
        .and_then(|val| val.get("json_mode"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        flags.insert(CapabilityFlags::JSON);
    }
    flags.insert(CapabilityFlags::TEXT);
    flags
}

fn bucket_tokens(tokens: u32) -> u16 {
    match tokens {
        0..=256 => 0,
        257..=512 => 1,
        513..=1024 => 2,
        1025..=2048 => 3,
        2049..=4096 => 4,
        4097..=8192 => 5,
        8193..=16384 => 6,
        _ => 7,
    }
}

fn has_teacher_boost(req: &RouteRequest) -> bool {
    req.overrides
        .as_ref()
        .and_then(|val| val.get("teacher_boost"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn hash_alias(alias: &str) -> u64 {
    use ahash::AHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = AHasher::default();
    alias.hash(&mut hasher);
    hasher.finish()
}

fn determine_content_usage(req: &RouteRequest) -> ContentLevel {
    if let Some(attestation) = req.content_attestation.as_ref() {
        return attestation.included;
    }
    match req.privacy_mode {
        PrivacyMode::FeaturesOnly => ContentLevel::None,
        PrivacyMode::Summary => ContentLevel::Summary,
        PrivacyMode::Full => ContentLevel::Full,
    }
}

fn freeze_key_from_request(req: &RouteRequest, policy_rev: &str) -> String {
    req.overrides
        .as_ref()
        .and_then(|ov| ov.get("freeze_key"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("frz_{}", policy_rev))
}

fn determine_escalation(
    req: &RouteRequest,
    policy: &PolicyDocument,
    regex: Option<&Regex>,
    in_tokens: u32,
    teacher_boost: bool,
) -> (Option<String>, Option<String>) {
    let fallback = policy
        .escalations
        .fallback_tier
        .clone()
        .unwrap_or_else(|| "T3".into());
    let fallback = fallback.to_ascii_uppercase();

    if teacher_boost {
        if let Some(target) = policy
            .escalations
            .teacher_boost_tier
            .clone()
            .or_else(|| Some(fallback.clone()))
        {
            return (
                Some(target.to_ascii_uppercase()),
                Some("teacher_boost".into()),
            );
        }
    }

    if let Some(limit) = policy.escalations.token_len_over {
        if in_tokens > limit {
            return (Some(fallback.clone()), Some("complexity".into()));
        }
    }

    if let (Some(re), Some(summary)) = (
        regex,
        req.conversation
            .as_ref()
            .and_then(|conv| conv.summary.as_ref()),
    ) {
        if re.is_match(summary) {
            return (Some(fallback.clone()), Some("uncertainty".into()));
        }
    }

    if policy.escalations.scpi_error_present.unwrap_or(false)
        && req
            .overrides
            .as_ref()
            .and_then(|ov| ov.get("scpi_error_present"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return (Some(fallback), Some("policy_lock".into()));
    }

    (None, None)
}

fn hash_string(value: &str) -> u64 {
    use ahash::AHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = AHasher::default();
    value.hash(&mut hasher);
    hasher.finish()
}

fn score_candidates<'req, 'a, 'tier>(
    ctx: ScoreContext<'req, 'a, 'tier>,
) -> Result<Vec<CandidateRef<'a>>, RouterError> {
    let mut scored = Vec::new();
    for idx in &ctx.alias.candidates {
        let model = ctx
            .catalog
            .models
            .get(*idx)
            .ok_or_else(|| RouterError::UnknownModel(format!("unknown model idx {idx}")))?;
        if !model
            .capabilities
            .contains(ctx.caps_mask | ctx.alias.require_caps)
        {
            continue;
        }
        if !ctx.alias.allowed_regions.intersects(ctx.region_mask) {
            continue;
        }
        if !ctx.region_mask.intersects(model.regions) && !model.regions.contains(RegionMask::GLOBAL)
        {
            continue;
        }
        if model.context_tokens < ctx.in_tokens + ctx.out_tokens {
            continue;
        }
        if model.status == ModelStatus::Offline {
            continue;
        }
        if let Some(tag) = ctx.forced_tier {
            if !model
                .policy_tags
                .iter()
                .any(|entry| entry.eq_ignore_ascii_case(tag))
            {
                continue;
            }
        }

        let use_prompt_cache = model.capabilities.contains(CapabilityFlags::PROMPT_CACHE)
            && (ctx
                .req
                .conversation
                .as_ref()
                .and_then(|conv| conv.turns)
                .unwrap_or(0)
                > 0
                || ctx
                    .req
                    .overrides
                    .as_ref()
                    .and_then(|ov| ov.get("plan_token"))
                    .is_some());

        let est_cost_micro =
            estimate_cost_micro(model, ctx.in_tokens, ctx.out_tokens, use_prompt_cache);
        if let Some(budget) = &ctx.req.budget {
            if est_cost_micro > budget.amount_micro {
                continue;
            }
        }

        let health_snapshot = ctx.health.snapshot(&model.id);
        let est_latency_ms =
            estimate_latency(model, &health_snapshot, ctx.in_tokens, ctx.out_tokens);
        if let Some(targets) = ctx.req.targets.as_ref() {
            if let Some(max_latency) = targets.p95_latency_ms {
                if est_latency_ms > max_latency {
                    continue;
                }
            }
        }

        let mut score = compute_score(
            model,
            &health_snapshot,
            ScoreFactors {
                est_cost_micro,
                est_latency_ms,
                in_tokens: ctx.in_tokens,
                out_tokens: ctx.out_tokens,
            },
            ctx.policy,
            ctx.boost,
        );
        if let Some(hint) = ctx.canonical_hint {
            if hint.model_id == model.id {
                score += hint.score;
            }
        }

        scored.push(CandidateRef {
            model,
            score,
            est_cost_micro,
            est_latency_ms,
            penalty: if model.status == ModelStatus::Degraded {
                0.1
            } else {
                0.0
            },
        });
    }

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(scored)
}

fn estimate_cost_micro(
    model: &CompiledModel,
    in_tokens: u32,
    out_tokens: u32,
    use_prompt_cache: bool,
) -> u64 {
    let (cached_tokens, normal_tokens) = if use_prompt_cache {
        let cached = ((in_tokens as f32) * 0.4).round() as u32;
        (cached, in_tokens.saturating_sub(cached))
    } else {
        (0, in_tokens)
    };

    let cached_cost =
        (cached_tokens as u128 * model.prices.cached_micro_per_million as u128) / 1_000_000;
    let normal_cost =
        (normal_tokens as u128 * model.prices.input_micro_per_million as u128) / 1_000_000;
    let out_cost = (out_tokens as u128 * model.prices.output_micro_per_million as u128) / 1_000_000;

    (cached_cost + normal_cost + out_cost) as u64
}

fn estimate_latency(
    model: &CompiledModel,
    health: &HealthStats,
    in_tokens: u32,
    out_tokens: u32,
) -> u32 {
    let throughput = health.tokens_per_sec.max(60.0);
    let gen_ms = ((in_tokens + out_tokens) as f32 / throughput) * 1000.0;
    let base = health.p50_ms.max(model.base_latency_ms as f32);
    let mut latency = base + gen_ms;
    let uprange = (model.target_latency_ms.max(1) as f32) * 1.5;
    latency = latency.min(uprange);
    latency.round() as u32
}

fn compute_score(
    model: &CompiledModel,
    health: &HealthStats,
    factors: ScoreFactors,
    policy: &PolicyDocument,
    boost: bool,
) -> f32 {
    let defaults = &policy.defaults;
    let weights = &policy.weights;
    let cost_ratio = (factors.est_cost_micro as f32 / defaults.cost_norm_micro).min(1.5);
    let latency_ratio = (factors.est_latency_ms as f32 / defaults.latency_ms).min(1.5);
    let fit_cost = 1.0 - cost_ratio;
    let fit_latency = 1.0 - latency_ratio;
    let fit_health = (1.0 - health.err_rate * 5.0).clamp(0.0, 1.0);
    let fit_context = (model.context_tokens as f32
        / (factors.in_tokens + factors.out_tokens + 32) as f32)
        .min(1.0);
    let mut score = weights.cost * fit_cost
        + weights.latency * fit_latency
        + weights.health * fit_health
        + weights.context * fit_context;

    let has_bonus = boost
        || model
            .policy_tags
            .iter()
            .any(|tag| tag.eq_ignore_ascii_case("tier:T1"));

    if has_bonus {
        score += weights.tier_bonus;
    }

    if model.status == ModelStatus::Degraded {
        score -= 0.05;
    }

    score
}

fn build_fallbacks<'a>(
    candidates: &'a [CandidateRef<'a>],
    primary: &'a CandidateRef<'a>,
) -> Vec<&'a CandidateRef<'a>> {
    candidates
        .iter()
        .filter(|cand| cand.model.id != primary.model.id)
        .take(3)
        .collect()
}

fn choose_primary<'a>(
    candidates: &'a [CandidateRef<'a>],
    sticky: Option<&StickinessClaims>,
    alias: &str,
) -> Option<&'a CandidateRef<'a>> {
    if let Some(claims) = sticky {
        if claims.alias == alias {
            if let Some(candidate) = candidates
                .iter()
                .find(|cand| cand.model.id == claims.model_id)
            {
                return Some(candidate);
            }
        }
    }
    candidates.first()
}

fn materialize_plan(ctx: PlanAssembly<'_, '_>) -> Result<RoutePlan, RouterError> {
    let policy_revision = ctx.policy.revision.clone();
    let canonical_block = ctx.canonical.as_ref().map(|sel| CanonicalContext {
        ids: sel.canonical_ids.clone(),
        model: Some(sel.model_id.clone()),
        score: Some(sel.score),
    });
    let plan = RoutePlan {
        schema_version: ctx.req.schema_version.clone(),
        route_id: Uuid::new_v4().to_string(),
        upstream: Upstream {
            base_url: ctx.primary.model.base_url.clone(),
            mode: ctx.primary.model.mode.clone(),
            model_id: ctx.primary.model.id.clone(),
            auth_env: ctx.primary.model.auth_env.clone(),
            headers: ctx.primary.model.headers.clone(),
        },
        limits: Limits {
            max_input_tokens: Some(ctx.primary.model.context_tokens),
            max_output_tokens: Some(ctx.out_tokens.min(ctx.policy.defaults.max_output_tokens)),
            timeout_ms: Some(ctx.policy.defaults.timeout_ms),
        },
        prompt_overlays: ctx.prompt_overlays,
        hints: Hints {
            tier: ctx
                .primary
                .model
                .policy_tags
                .iter()
                .find_map(|tag| tag.strip_prefix("tier:").map(|tier| tier.to_string())),
            est_cost_micro: Some(ctx.primary.est_cost_micro),
            currency: Some(
                ctx.req
                    .budget
                    .as_ref()
                    .map(|b| b.currency.clone())
                    .unwrap_or_else(|| "USD".into()),
            ),
            est_latency_ms: Some(ctx.primary.est_latency_ms),
            provider: Some(ctx.primary.model.provider.clone()),
        },
        fallbacks: ctx.fallbacks.to_vec(),
        cache: CacheHints {
            ttl_ms: Some(ctx.cache_ttl_ms),
            etag: Some(format!(
                "W/\"cat_{}@pol_{}\"",
                ctx.catalog_revision, policy_revision
            )),
            valid_until: None,
            freeze_key: Some(ctx.freeze_key),
        },
        stickiness: Stickiness::default(),
        policy: PolicyInfo {
            revision: Some(policy_revision.clone()),
            id: Some(ctx.policy.id.clone()),
            explain: Some(format!(
                "score={:.3} cost={}µ latency={}ms",
                ctx.primary.score, ctx.primary.est_cost_micro, ctx.primary.est_latency_ms
            )),
        },
        policy_rev: policy_revision.clone(),
        content_used: ctx.content_used,
        governance_echo: ctx.policy.governance.clone(),
        canonical: canonical_block,
    };

    Ok(plan)
}

fn resolve_overlay(
    req: &RouteRequest,
    policy: &PolicyDocument,
    overlays: &OverlayStore,
    max_overlay_bytes: u32,
) -> Result<PromptOverlays, RouterError> {
    let role = req
        .org
        .as_ref()
        .and_then(|org| org.role.clone())
        .unwrap_or_else(|| "default".into());
    let overlay_id = policy
        .overlay_map
        .get(&req.alias)
        .and_then(|map| map.get(&role).cloned())
        .or_else(|| policy.overlay_defaults.get(&role).cloned());

    let mut block = PromptOverlays {
        system_overlay: None,
        overlay_fingerprint: None,
        overlay_size_bytes: Some(0),
        max_overlay_bytes: Some(max_overlay_bytes),
    };

    if let Some(id) = overlay_id {
        if let Some(content) = overlays.content.get(&id) {
            let size = content.len() as u32;
            if size > max_overlay_bytes {
                return Err(RouterError::PolicyDeny(format!(
                    "overlay {id} exceeds max_overlay_bytes ({size} > {max_overlay_bytes})"
                )));
            }
            let mut hasher = Sha256::new();
            hasher.update(content.as_bytes());
            let fingerprint = format!("sha256:{}", hex::encode(hasher.finalize()));
            block.system_overlay = Some(content.clone());
            block.overlay_fingerprint = Some(fingerprint);
            block.overlay_size_bytes = Some(size);
        } else {
            block.overlay_fingerprint = Some(format!("missing:{id}"));
        }
    }

    Ok(block)
}

fn calc_cache_ratio(metrics: &RouterMetrics) -> f32 {
    let hits: u64 = metrics.cache_hits.iter().map(|entry| *entry.value()).sum();
    let total: u64 = metrics
        .total_requests
        .iter()
        .map(|entry| *entry.value())
        .sum();
    if total == 0 {
        0.0
    } else {
        hits as f32 / total as f32
    }
}

impl RouterEngine {
    fn attach_stickiness(
        &self,
        policy: &PolicyDocument,
        req: &RouteRequest,
        plan: &mut RoutePlan,
        existing: Option<&StickinessClaims>,
    ) -> Result<Option<StickinessClaims>, RouterError> {
        let cfg = &policy.defaults.stickiness;
        if cfg.max_turns == 0 || cfg.window_ms == 0 {
            plan.stickiness = Stickiness::default();
            return Ok(None);
        }

        let tenant = req.org.as_ref().and_then(|org| org.tenant.as_deref());
        let project = req.org.as_ref().and_then(|org| org.project.as_deref());
        let model_id = plan.upstream.model_id.clone();

        let (token, claims) = match existing
            .filter(|claims| claims.alias == req.alias && claims.model_id == model_id)
        {
            Some(claims) if claims.turn + 1 < claims.max_turns => {
                self.stickiness.progress_turn(claims, cfg.window_ms)?
            }
            _ => self.stickiness.issue(
                tenant,
                project,
                &req.alias,
                &model_id,
                cfg.max_turns,
                cfg.window_ms,
            )?,
        };

        plan.stickiness = Stickiness {
            plan_token: Some(token),
            max_turns: Some(cfg.max_turns),
            expires_at: Some(claims.expires_at.to_rfc3339()),
        };
        plan.cache.valid_until = Some(claims.expires_at.to_rfc3339());
        Ok(Some(claims))
    }
}
