use crate::engine::{PlanOutcome, RouterEngine};
use crate::errors::{with_context, ApiError, RouterError};
use crate::types::{
    CacheStatus, CatalogDocument, ContentLevel, PolicyDocument, RouteFeedback, RouteRequest,
    TraceCtx,
};
use actix_web::http::header;
use actix_web::{get, post, web, HttpRequest, HttpResponse, Responder};
use chrono::Utc;
use serde::Serialize;
use std::time::Instant;

const SUPPORTED_SCHEMAS: &[&str] = &["1.0", "1.1"];

#[derive(Default)]
struct TraceHeaderCtx {
    traceparent: Option<String>,
    tracestate: Option<String>,
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(post_route_plan)
        .service(post_route_feedback)
        .service(get_capabilities)
        .service(get_catalog)
        .service(get_policy)
        .service(get_stats)
        .service(get_health)
        .service(reload_policy)
        .service(reload_catalog)
        .service(reload_overlays);
}

#[post("/route/plan")]
async fn post_route_plan(
    http_req: HttpRequest,
    engine: web::Data<RouterEngine>,
    payload: web::Json<RouteRequest>,
) -> Result<HttpResponse, ApiError> {
    let started = Instant::now();
    let mut request = payload.into_inner();
    if request.schema_version.is_empty() {
        request.schema_version = "1.1".into();
    }
    let policy_rev_hint = engine.policy_revision();
    if !SUPPORTED_SCHEMAS
        .iter()
        .any(|schema| schema == &request.schema_version.as_str())
    {
        return Err(with_context(
            RouterError::UnsupportedSchema {
                provided: request.schema_version.clone(),
                supported: SUPPORTED_SCHEMAS.iter().map(|s| s.to_string()).collect(),
            },
            Some(request.request_id.clone()),
            Some(policy_rev_hint.clone()),
        ));
    }
    if request.request_id.is_empty() {
        return Err(with_context(
            RouterError::InvalidRequest("request_id is required".into()),
            Some("unknown".into()),
            Some(policy_rev_hint.clone()),
        ));
    }
    let request_id = request.request_id.clone();
    if let Some(ip) = client_ip(&http_req) {
        if let Err(err) = engine.check_rate_limit(&ip) {
            return Err(with_context(
                err,
                Some(request_id.clone()),
                Some(policy_rev_hint.clone()),
            ));
        }
    }
    let header_traceparent = header_value(&http_req, "traceparent");
    let header_tracestate = header_value(&http_req, "tracestate");
    if request.trace.is_none() && (header_traceparent.is_some() || header_tracestate.is_some()) {
        request.trace = Some(TraceCtx::default());
    }
    if let Some(trace) = request.trace.as_mut() {
        if trace.traceparent.is_none() {
            trace.traceparent = header_traceparent.clone();
        }
        if trace.tracestate.is_none() {
            trace.tracestate = header_tracestate.clone();
        }
    }
    let trace_snapshot = request.trace.clone();
    let outcome = engine.plan(request).await.map_err(|err| {
        with_context(err, Some(request_id.clone()), Some(policy_rev_hint.clone()))
    })?;
    let elapsed = started.elapsed();
    let trace_headers = TraceHeaderCtx {
        traceparent: trace_snapshot
            .as_ref()
            .and_then(|ctx| ctx.traceparent.clone())
            .or(header_traceparent),
        tracestate: trace_snapshot
            .as_ref()
            .and_then(|ctx| ctx.tracestate.clone())
            .or(header_tracestate),
    };
    Ok(respond_with_plan(
        outcome,
        elapsed,
        &request_id,
        trace_headers,
    ))
}

fn respond_with_plan(
    outcome: PlanOutcome,
    elapsed: std::time::Duration,
    request_id: &str,
    trace: TraceHeaderCtx,
) -> HttpResponse {
    let mut response = HttpResponse::Ok();
    response.append_header(("Router-Schema", outcome.plan.schema_version.clone()));
    response.append_header(("Router-Latency", format!("{}ms", elapsed.as_millis())));
    response.append_header(("Config-Revision", outcome.policy_revision.clone()));
    response.append_header(("Catalog-Revision", outcome.catalog_revision.clone()));
    response.append_header(("X-Route-Cache", cache_status_value(outcome.cache_status)));
    response.append_header(("X-Resolved-Model", outcome.plan.upstream.model_id.clone()));
    response.append_header(("X-Route-Id", outcome.plan.route_id.clone()));
    response.append_header(("X-Policy-Rev", outcome.plan.policy_rev.clone()));
    response.append_header(("X-Request-Id", request_id.to_string()));
    if let Some(tier) = &outcome.plan.hints.tier {
        response.append_header(("X-Route-Tier", tier.clone()));
    }
    if let Some(provider) = &outcome.plan.hints.provider {
        response.append_header(("X-Route-Provider", provider.clone()));
    }
    if let Some(reason) = &outcome.route_reason {
        response.append_header(("X-Route-Why", reason.clone()));
    }
    response.append_header((
        "X-Content-Used",
        content_level_str(&outcome.plan.content_used),
    ));
    if let Some(parent) = trace.traceparent {
        response.append_header(("traceparent", parent));
    }
    if let Some(state) = trace.tracestate {
        response.append_header(("tracestate", state));
    }
    response.json(outcome.plan)
}

#[post("/route/feedback")]
async fn post_route_feedback(
    engine: web::Data<RouterEngine>,
    payload: web::Json<RouteFeedback>,
) -> Result<impl Responder, ApiError> {
    let feedback = payload.into_inner();
    engine.health().update(&feedback);
    Ok(HttpResponse::NoContent())
}

#[get("/capabilities")]
async fn get_capabilities(engine: web::Data<RouterEngine>) -> Result<impl Responder, ApiError> {
    #[derive(Serialize)]
    struct StickinessCaps {
        supported: bool,
        max_turns: u8,
        window_ms: u64,
    }

    #[derive(Serialize)]
    struct FeatureToggle {
        supported: bool,
    }

    #[derive(Serialize)]
    struct CapabilityResponse {
        schema_version: &'static str,
        privacy_modes: Vec<&'static str>,
        stickiness: StickinessCaps,
        batch: FeatureToggle,
        prefetch: FeatureToggle,
        provider_headers: bool,
    }

    let policy = engine.policy_document();
    let stickiness = policy.defaults.stickiness;
    let stickiness_caps = StickinessCaps {
        supported: stickiness.max_turns > 0 && stickiness.window_ms > 0,
        max_turns: stickiness.max_turns,
        window_ms: stickiness.window_ms,
    };
    let body = CapabilityResponse {
        schema_version: "1.1",
        privacy_modes: vec!["features_only", "summary", "full"],
        stickiness: stickiness_caps,
        batch: FeatureToggle { supported: false },
        prefetch: FeatureToggle { supported: false },
        provider_headers: true,
    };
    Ok(HttpResponse::Ok().json(body))
}

#[get("/catalog/models")]
async fn get_catalog(
    http_req: HttpRequest,
    engine: web::Data<RouterEngine>,
) -> Result<HttpResponse, ApiError> {
    let doc = engine.catalog_document();
    let revision = doc.revision.clone();
    let strong = format!("\"{}\"", revision);
    let weak = format!("W/\"{}\"", revision);
    if let Some(if_none_match) = http_req.headers().get(header::IF_NONE_MATCH) {
        if let Ok(value) = if_none_match.to_str() {
            if value == strong || value == weak {
                return Ok(HttpResponse::NotModified()
                    .append_header(("ETag", strong))
                    .append_header(("X-Catalog-Weak", weak.clone()))
                    .append_header(("X-Catalog-Revision", revision))
                    .finish());
            }
        }
    }

    Ok(HttpResponse::Ok()
        .append_header(("ETag", strong.clone()))
        .append_header(("X-Catalog-Weak", weak))
        .append_header(("X-Catalog-Revision", revision))
        .json(doc))
}

#[get("/policy")]
async fn get_policy(engine: web::Data<RouterEngine>) -> Result<impl Responder, ApiError> {
    Ok(HttpResponse::Ok().json(engine.policy_document()))
}

#[get("/stats")]
async fn get_stats(engine: web::Data<RouterEngine>) -> Result<impl Responder, ApiError> {
    Ok(HttpResponse::Ok().json(engine.stats()))
}

#[get("/healthz")]
async fn get_health(engine: web::Data<RouterEngine>) -> Result<impl Responder, ApiError> {
    #[derive(Serialize)]
    struct HealthResponse {
        status: &'static str,
        policy_revision: String,
        catalog_revision: String,
        timestamp: String,
    }

    let policy_rev = engine.policy_revision();
    let catalog_rev = engine.catalog_revision();
    Ok(HttpResponse::Ok().json(HealthResponse {
        status: "ok",
        policy_revision: policy_rev,
        catalog_revision: catalog_rev,
        timestamp: Utc::now().to_rfc3339(),
    }))
}

#[post("/admin/policy")]
async fn reload_policy(
    engine: web::Data<RouterEngine>,
    payload: web::Json<PolicyDocument>,
) -> Result<impl Responder, ApiError> {
    engine
        .reload_policy(payload.into_inner())
        .await
        .map_err(ApiError::from)?;
    Ok(HttpResponse::NoContent())
}

#[post("/admin/catalog")]
async fn reload_catalog(
    engine: web::Data<RouterEngine>,
    payload: web::Json<CatalogDocument>,
) -> Result<impl Responder, ApiError> {
    engine
        .reload_catalog(payload.into_inner())
        .await
        .map_err(ApiError::from)?;
    Ok(HttpResponse::NoContent())
}

#[post("/admin/overlays/reload")]
async fn reload_overlays(engine: web::Data<RouterEngine>) -> Result<impl Responder, ApiError> {
    engine.reload_overlays().await.map_err(ApiError::from)?;
    Ok(HttpResponse::NoContent())
}

fn cache_status_value(status: CacheStatus) -> &'static str {
    match status {
        CacheStatus::Hit => "hit",
        CacheStatus::Miss => "miss",
        CacheStatus::Stale => "stale",
    }
}

fn content_level_str(level: &ContentLevel) -> &'static str {
    match level {
        ContentLevel::None => "none",
        ContentLevel::Summary => "summary",
        ContentLevel::Full => "full",
    }
}

fn header_value(req: &HttpRequest, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|s| s.to_string())
}

fn client_ip(req: &HttpRequest) -> Option<String> {
    req.connection_info()
        .realip_remote_addr()
        .map(|s| s.to_string())
        .or_else(|| req.peer_addr().map(|addr| addr.ip().to_string()))
}
