use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub type Currency = String;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PrivacyMode {
    #[default]
    FeaturesOnly,
    Summary,
    Full,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ApiKind {
    #[default]
    Responses,
    Chat,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraceCtx {
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ContentLevel {
    #[default]
    None,
    Summary,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContentAttestation {
    pub included: ContentLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    pub amount_micro: u64,
    pub currency: Currency,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Estimates {
    pub prompt_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub tokenizer_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Targets {
    pub p95_latency_ms: Option<u32>,
    pub min_tokens_per_sec: Option<u32>,
    pub reliability_tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Conversation {
    pub turns: Option<u16>,
    pub system_fingerprint: Option<String>,
    pub history_fingerprint: Option<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OrgCtx {
    pub tenant: Option<String>,
    pub project: Option<String>,
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Geo {
    pub region: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolHint {
    pub name: String,
    pub json_schema_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteRequest {
    pub schema_version: String,
    pub request_id: String,
    pub trace: Option<TraceCtx>,
    pub alias: String,
    pub api: ApiKind,
    pub privacy_mode: PrivacyMode,
    pub content_attestation: Option<ContentAttestation>,
    #[serde(default)]
    pub caps: Vec<String>,
    pub stream: bool,
    pub params: Option<Value>,
    pub targets: Option<Targets>,
    pub budget: Option<Budget>,
    pub estimates: Option<Estimates>,
    pub conversation: Option<Conversation>,
    pub org: Option<OrgCtx>,
    pub geo: Option<Geo>,
    pub tools: Option<Vec<ToolHint>>,
    pub overrides: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum UpstreamMode {
    #[default]
    Responses,
    Chat,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Limits {
    pub max_input_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptOverlays {
    pub system_overlay: Option<String>,
    pub overlay_fingerprint: Option<String>,
    pub overlay_size_bytes: Option<u32>,
    pub max_overlay_bytes: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Hints {
    pub tier: Option<String>,
    pub est_cost_micro: Option<u64>,
    pub currency: Option<Currency>,
    pub est_latency_ms: Option<u32>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CanonicalContext {
    #[serde(default)]
    pub ids: Vec<String>,
    pub model: Option<String>,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fallback {
    pub base_url: String,
    pub mode: UpstreamMode,
    pub model_id: String,
    pub reason: Option<String>,
    pub penalty: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheHints {
    pub ttl_ms: Option<u32>,
    pub etag: Option<String>,
    pub valid_until: Option<String>,
    pub freeze_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stickiness {
    pub plan_token: Option<String>,
    pub max_turns: Option<u8>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicyInfo {
    pub revision: Option<String>,
    pub id: Option<String>,
    pub explain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GovernanceBudgets {
    pub total: Option<u32>,
    pub l3_max: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GovernanceApprovals {
    #[serde(default)]
    pub require_for_levels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GovernanceEcho {
    pub budgets: GovernanceBudgets,
    pub approvals: GovernanceApprovals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Upstream {
    pub base_url: String,
    pub mode: UpstreamMode,
    pub model_id: String,
    pub auth_env: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutePlan {
    pub schema_version: String,
    pub route_id: String,
    pub upstream: Upstream,
    pub limits: Limits,
    pub prompt_overlays: PromptOverlays,
    pub hints: Hints,
    pub fallbacks: Vec<Fallback>,
    pub cache: CacheHints,
    pub stickiness: Stickiness,
    pub policy: PolicyInfo,
    pub policy_rev: String,
    pub content_used: ContentLevel,
    pub governance_echo: GovernanceEcho,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub canonical: Option<CanonicalContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeedbackUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cached_tokens: u32,
    pub reasoning_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteFeedback {
    pub route_id: String,
    pub model_id: String,
    pub success: bool,
    pub duration_ms: u32,
    pub usage: Option<FeedbackUsage>,
    pub status_code: u16,
    pub actual_cost_micro: Option<u64>,
    pub currency: Option<Currency>,
    pub upstream_error_code: Option<String>,
    pub rl_applied: Option<bool>,
    pub cache_hit: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModelCost {
    pub currency: Currency,
    pub input_per_million_micro: u64,
    pub output_per_million_micro: u64,
    #[serde(default)]
    pub cached_per_million_micro: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModelLimits {
    #[serde(default)]
    pub tps: Option<u32>,
    #[serde(default)]
    pub rpm: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModelCapabilities {
    #[serde(default)]
    pub modalities: Vec<String>,
    #[serde(default)]
    pub context_tokens: u32,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub json_mode: bool,
    #[serde(default)]
    pub prompt_cache: bool,
    #[serde(default)]
    pub structured_output: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModelSloRecent {
    #[serde(default)]
    pub p50_ms: Option<u32>,
    #[serde(default)]
    pub p95_ms: Option<u32>,
    #[serde(default)]
    pub error_rate: Option<f32>,
    #[serde(default)]
    pub tokens_per_sec: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModelSlos {
    pub target_p95_ms: u32,
    #[serde(default)]
    pub recent: Option<CatalogModelSloRecent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    pub provider: String,
    #[serde(default)]
    pub region: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub capabilities: CatalogModelCapabilities,
    #[serde(default)]
    pub limits: Option<CatalogModelLimits>,
    pub cost: CatalogModelCost,
    pub slos: CatalogModelSlos,
    #[serde(default)]
    pub policy_tags: Vec<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogDocument {
    pub revision: String,
    #[serde(default = "Vec::new")]
    pub models: Vec<CatalogModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    #[default]
    Miss,
    Hit,
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterStats {
    pub policy_revision: String,
    pub catalog_revision: String,
    pub total_requests: u64,
    pub cache_hit_ratio: f32,
    pub model_share: HashMap<String, u64>,
    pub error_rate: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyAliasRule {
    pub candidates: Vec<String>,
    #[serde(default)]
    pub require_caps: Vec<String>,
    #[serde(default)]
    pub allowed_regions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyWeights {
    pub cost: f32,
    pub latency: f32,
    pub health: f32,
    pub context: f32,
    #[serde(default)]
    pub tier_bonus: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyStickiness {
    pub window_ms: u64,
    pub max_turns: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDefaults {
    pub cost_norm_micro: f32,
    pub latency_ms: f32,
    pub timeout_ms: u32,
    pub max_output_tokens: u32,
    pub max_overlay_bytes: u32,
    pub stickiness: PolicyStickiness,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicyEscalations {
    pub token_len_over: Option<u32>,
    pub uncertainty_regex: Option<String>,
    pub scpi_error_present: Option<bool>,
    pub teacher_boost_tier: Option<String>,
    pub default_tier: Option<String>,
    pub fallback_tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicyTier {
    pub id: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDocument {
    pub id: String,
    pub revision: String,
    pub schema_version: String,
    pub weights: PolicyWeights,
    pub defaults: PolicyDefaults,
    #[serde(default)]
    pub governance: GovernanceEcho,
    #[serde(default)]
    pub escalations: PolicyEscalations,
    #[serde(default)]
    pub tiers: HashMap<String, PolicyTier>,
    #[serde(default)]
    pub aliases: HashMap<String, PolicyAliasRule>,
    #[serde(default)]
    pub overlay_map: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub overlay_defaults: HashMap<String, String>,
}
