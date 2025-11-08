use crate::engine::{PlanOutcome, RouterEngine};
use crate::errors::RouterError;
use crate::types::{CatalogDocument, PolicyDocument, RouteFeedback, RouteRequest};
use actix_web::{get, post, web, HttpResponse, Responder};
use chrono::Utc;
use serde::Serialize;
use std::sync::Arc;
use std::time::Instant;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(post_route_plan)
        .service(post_route_feedback)
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
    engine: web::Data<Arc<RouterEngine>>,
    payload: web::Json<RouteRequest>,
) -> Result<HttpResponse, RouterError> {
    let started = Instant::now();
    let mut request = payload.into_inner();
    if request.schema_version.is_empty() {
        request.schema_version = "1.1".into();
    }
    let outcome = engine.plan(request).await?;
    let elapsed = started.elapsed();

    respond_with_plan(outcome, elapsed)
}

fn respond_with_plan(
    outcome: PlanOutcome,
    elapsed: std::time::Duration,
) -> Result<HttpResponse, RouterError> {
    let mut response = HttpResponse::Ok();
    response.append_header(("Router-Schema", outcome.plan.schema_version.clone()));
    response.append_header(("Router-Latency", format!("{}ms", elapsed.as_millis())));
    response.append_header(("Config-Revision", outcome.policy_revision.clone()));
    response.append_header(("Catalog-Revision", outcome.catalog_revision.clone()));
    response.append_header((
        "X-Route-Cache",
        format!(
            "{}",
            match outcome.cache_status {
                crate::types::CacheStatus::Hit => "hit",
                crate::types::CacheStatus::Miss => "miss",
                crate::types::CacheStatus::Stale => "stale",
            }
        ),
    ));
    response.append_header(("X-Resolved-Model", outcome.plan.upstream.model_id.clone()));
    response.append_header(("X-Route-Id", outcome.plan.route_id.clone()));
    if let Some(hints) = &outcome.plan.hints {
        if let Some(tier) = &hints.tier {
            response.append_header(("X-Route-Tier", tier.clone()));
        }
    }
    if let Some(content) = &outcome.plan.content_used {
        let value = match content {
            crate::types::ContentLevel::None => "none",
            crate::types::ContentLevel::Summary => "summary",
            crate::types::ContentLevel::Full => "full",
        };
        response.append_header(("X-Content-Used", value));
    }

    Ok(response.json(outcome.plan))
}

#[post("/route/feedback")]
async fn post_route_feedback(
    engine: web::Data<Arc<RouterEngine>>,
    payload: web::Json<RouteFeedback>,
) -> Result<impl Responder, RouterError> {
    let feedback = payload.into_inner();
    engine.health().update(&feedback);
    Ok(HttpResponse::NoContent())
}

#[get("/catalog/models")]
async fn get_catalog(engine: web::Data<Arc<RouterEngine>>) -> Result<impl Responder, RouterError> {
    Ok(HttpResponse::Ok().json(&engine.catalog_document()))
}

#[get("/policy")]
async fn get_policy(engine: web::Data<Arc<RouterEngine>>) -> Result<impl Responder, RouterError> {
    Ok(HttpResponse::Ok().json(&engine.policy_document()))
}

#[get("/stats")]
async fn get_stats(engine: web::Data<Arc<RouterEngine>>) -> Result<impl Responder, RouterError> {
    Ok(HttpResponse::Ok().json(engine.stats()))
}

#[get("/healthz")]
async fn get_health(engine: web::Data<Arc<RouterEngine>>) -> Result<impl Responder, RouterError> {
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
    engine: web::Data<Arc<RouterEngine>>,
    payload: web::Json<PolicyDocument>,
) -> Result<impl Responder, RouterError> {
    engine.reload_policy(payload.into_inner()).await?;
    Ok(HttpResponse::NoContent())
}

#[post("/admin/catalog")]
async fn reload_catalog(
    engine: web::Data<Arc<RouterEngine>>,
    payload: web::Json<CatalogDocument>,
) -> Result<impl Responder, RouterError> {
    engine.reload_catalog(payload.into_inner()).await?;
    Ok(HttpResponse::NoContent())
}

#[post("/admin/overlays/reload")]
async fn reload_overlays(
    engine: web::Data<Arc<RouterEngine>>,
) -> Result<impl Responder, RouterError> {
    engine.reload_overlays().await?;
    Ok(HttpResponse::NoContent())
}
