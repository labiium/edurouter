use actix_web::{
    http::{header, StatusCode},
    test, web, App,
};
use router::api;
use router::config::{RouterConfig, ServerConfig};
use router::engine::RouterEngine;
use router::types::{CatalogDocument, PolicyDocument};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::sleep;

fn test_router_config() -> RouterConfig {
    let policy_raw = std::fs::read_to_string("configs/policy.json").expect("read policy fixture");
    let catalog_raw =
        std::fs::read_to_string("configs/catalog.json").expect("read catalog fixture");
    let mut policy: PolicyDocument =
        serde_json::from_str(&policy_raw).expect("parse policy fixture");
    let catalog: CatalogDocument =
        serde_json::from_str(&catalog_raw).expect("parse catalog fixture");
    policy.defaults.stickiness.window_ms = 200;

    RouterConfig {
        server: ServerConfig {
            bind_addr: "127.0.0.1:0".into(),
            workers: 1,
        },
        overlay_dir: PathBuf::from("configs/overlays"),
        cache_ttl_ms: 60,
        cache_stale_ms: 60,
        sticky_secret: b"test-secret-key".to_vec(),
        policy,
        catalog,
        rate_limit_burst: 500.0,
        rate_limit_refill_per_sec: 500.0,
    }
}

async fn bootstrap_engine() -> web::Data<RouterEngine> {
    let cfg = test_router_config();
    web::Data::new(
        RouterEngine::bootstrap(&cfg)
            .await
            .expect("bootstrap router"),
    )
}

fn base_plan_request(request_id: &str) -> Value {
    json!({
        "schema_version": "1.1",
        "request_id": request_id,
        "alias": "edu-general",
        "api": "responses",
        "privacy_mode": "features_only",
        "stream": false
    })
}

#[actix_web::test]
async fn schema_and_headers_contract() {
    let engine = bootstrap_engine().await;
    let app = test::init_service(
        App::new()
            .app_data(engine.clone())
            .configure(api::configure),
    )
    .await;
    let req = test::TestRequest::post()
        .uri("/route/plan")
        .set_json(base_plan_request("schema-headers"))
        .to_request();
    let resp = test::call_service(&app, req).await;
    let status = resp.status();
    let headers = resp.headers().clone();
    let body_bytes = test::read_body(resp).await;
    if status != StatusCode::OK {
        panic!(
            "plan failed: {} {}",
            status,
            String::from_utf8_lossy(&body_bytes)
        );
    }
    assert_eq!(headers.get("Router-Schema").unwrap(), "1.1");
    assert!(headers.contains_key("Router-Latency"));
    assert!(headers.contains_key("X-Route-Id"));
    assert_eq!(headers.get("X-Route-Cache").unwrap(), "miss");
    assert_eq!(headers.get("X-Content-Used").unwrap(), "none");
    assert!(headers.contains_key("X-Policy-Rev"));
    let body: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["schema_version"], "1.1");
    assert!(body["upstream"]["base_url"].is_string());
    assert!(body["limits"]["max_output_tokens"].is_u64());
    assert!(body["prompt_overlays"].is_object());
    assert!(body["cache"]["ttl_ms"].is_u64());
    assert!(body["stickiness"]["plan_token"].is_string());
    assert!(body["fallbacks"].is_array());
    assert!(body["governance_echo"]["budgets"].is_object());
    assert!(body["policy_rev"].is_string());
}

#[actix_web::test]
async fn cache_and_stickiness_semantics() {
    let engine = bootstrap_engine().await;
    let app = test::init_service(
        App::new()
            .app_data(engine.clone())
            .configure(api::configure),
    )
    .await;

    let req1 = test::TestRequest::post()
        .uri("/route/plan")
        .set_json(base_plan_request("cache-miss"))
        .to_request();
    let resp1 = test::call_service(&app, req1).await;
    assert_eq!(resp1.headers().get("X-Route-Cache").unwrap(), "miss");
    let body1: Value = test::read_body_json(resp1).await;
    let token = body1["stickiness"]["plan_token"]
        .as_str()
        .unwrap()
        .to_string();

    let mut hit_body = base_plan_request("cache-hit");
    hit_body["overrides"] = json!({ "plan_token": token });
    let req2 = test::TestRequest::post()
        .uri("/route/plan")
        .set_json(&hit_body)
        .to_request();
    let resp2 = test::call_service(&app, req2).await;
    assert_eq!(resp2.headers().get("X-Route-Cache").unwrap(), "hit");
    let _body2: Value = test::read_body_json(resp2).await;

    sleep(Duration::from_millis(80)).await;
    let req3 = test::TestRequest::post()
        .uri("/route/plan")
        .set_json(base_plan_request("cache-stale"))
        .to_request();
    let resp3 = test::call_service(&app, req3).await;
    assert_eq!(resp3.headers().get("X-Route-Cache").unwrap(), "stale");
}

#[actix_web::test]
async fn stickiness_rotation_respects_turn_limit() {
    let engine = bootstrap_engine().await;
    let app = test::init_service(
        App::new()
            .app_data(engine.clone())
            .configure(api::configure),
    )
    .await;
    let req = test::TestRequest::post()
        .uri("/route/plan")
        .set_json(base_plan_request("stickiness-root"))
        .to_request();
    let resp = test::call_service(&app, req).await;
    let resp_json: Value = test::read_body_json(resp).await;
    let mut last_token = resp_json["stickiness"]["plan_token"]
        .as_str()
        .unwrap()
        .to_string();

    for turn in 0..3 {
        let mut body = base_plan_request(&format!("stickiness-{turn}"));
        body["overrides"] = json!({ "plan_token": last_token });
        let req = test::TestRequest::post()
            .uri("/route/plan")
            .set_json(&body)
            .to_request();
        let resp = test::call_service(&app, req).await;
        let payload: Value = test::read_body_json(resp).await;
        let next_token = payload["stickiness"]["plan_token"]
            .as_str()
            .unwrap()
            .to_string();
        if turn < 2 {
            // still within window, expect token to advance
            assert_ne!(last_token, next_token);
        }
        last_token = next_token;
    }
}

#[actix_web::test]
async fn escalation_and_boost_headers() {
    let engine = bootstrap_engine().await;
    let app = test::init_service(
        App::new()
            .app_data(engine.clone())
            .configure(api::configure),
    )
    .await;

    let mut long_body = base_plan_request("complexity");
    long_body["estimates"] = json!({ "prompt_tokens": 9000 });
    let resp_complex = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/route/plan")
            .set_json(&long_body)
            .to_request(),
    )
    .await;
    assert_eq!(
        resp_complex.headers().get("X-Route-Why").unwrap(),
        "complexity"
    );

    let mut uncertain_body = base_plan_request("uncertainty");
    uncertain_body["conversation"] = json!({ "summary": "I am unsure what happened" });
    let resp_uncertain = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/route/plan")
            .set_json(&uncertain_body)
            .to_request(),
    )
    .await;
    assert_eq!(
        resp_uncertain.headers().get("X-Route-Why").unwrap(),
        "uncertainty"
    );

    let mut boost_body = base_plan_request("teacher-boost");
    boost_body["overrides"] = json!({ "teacher_boost": true });
    let resp_boost = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/route/plan")
            .set_json(&boost_body)
            .to_request(),
    )
    .await;
    assert_eq!(
        resp_boost.headers().get("X-Route-Why").unwrap(),
        "teacher_boost"
    );
}

#[actix_web::test]
async fn privacy_headers_echo_content_usage() {
    let engine = bootstrap_engine().await;
    let app = test::init_service(
        App::new()
            .app_data(engine.clone())
            .configure(api::configure),
    )
    .await;
    let resp = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/route/plan")
            .set_json(base_plan_request("privacy"))
            .to_request(),
    )
    .await;
    assert_eq!(resp.headers().get("X-Content-Used").unwrap(), "none");
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["content_used"], "none");
}

#[actix_web::test]
async fn catalog_etag_and_not_modified() {
    let engine = bootstrap_engine().await;
    let app = test::init_service(
        App::new()
            .app_data(engine.clone())
            .configure(api::configure),
    )
    .await;
    let resp = test::call_service(
        &app,
        test::TestRequest::get().uri("/catalog/models").to_request(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers().get(header::ETAG).unwrap().to_str().unwrap();
    let weak = resp
        .headers()
        .get("X-Catalog-Weak")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let revision = resp
        .headers()
        .get("X-Catalog-Revision")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let resp304 = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/catalog/models")
            .insert_header((header::IF_NONE_MATCH, etag))
            .to_request(),
    )
    .await;
    assert_eq!(resp304.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        resp304.headers().get("X-Catalog-Weak").unwrap(),
        weak.as_str()
    );
    assert_eq!(
        resp304.headers().get("X-Catalog-Revision").unwrap(),
        revision.as_str()
    );
}

#[actix_web::test]
async fn typed_error_envelopes() {
    let engine = bootstrap_engine().await;
    let app = test::init_service(
        App::new()
            .app_data(engine.clone())
            .configure(api::configure),
    )
    .await;
    let mut bad_alias = base_plan_request("bad-alias");
    bad_alias["alias"] = json!("missing-alias");
    let resp = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/route/plan")
            .set_json(&bad_alias)
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["code"], "ALIAS_UNKNOWN");
    assert!(body["message"].as_str().unwrap().contains("alias"));
    assert!(body["policy_rev"].is_string());
    assert!(body["retry_hint_ms"].is_number());

    let mut bad_schema = base_plan_request("bad-schema");
    bad_schema["schema_version"] = json!("2.0");
    let resp_schema = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/route/plan")
            .set_json(&bad_schema)
            .to_request(),
    )
    .await;
    assert_eq!(resp_schema.status(), StatusCode::CONFLICT);
    let body_schema: Value = test::read_body_json(resp_schema).await;
    assert_eq!(body_schema["code"], "UNSUPPORTED_SCHEMA");
    assert!(body_schema["supported"]
        .as_array()
        .unwrap()
        .contains(&json!("1.1")));
}

#[actix_web::test]
async fn fallback_list_present() {
    let engine = bootstrap_engine().await;
    let app = test::init_service(
        App::new()
            .app_data(engine.clone())
            .configure(api::configure),
    )
    .await;
    let resp = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/route/plan")
            .set_json(base_plan_request("fallbacks"))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    let fallbacks = body["fallbacks"].as_array().unwrap();
    assert!(!fallbacks.is_empty());
    assert!(fallbacks[0]["model_id"].is_string());
}
