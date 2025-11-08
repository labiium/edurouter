mod api;
mod cache;
mod config;
mod engine;
mod errors;
mod health;
mod stickiness;
mod types;

use actix_cors::Cors;
use actix_web::{middleware::Logger, web, App, HttpServer};
use anyhow::Context;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::RouterConfig;
use crate::engine::RouterEngine;

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "router=info,actix_web=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = RouterConfig::from_env().context("load router config")?;

    let engine = RouterEngine::bootstrap(&cfg).await?;
    let shared_engine = Arc::new(engine);

    let bind_addr: SocketAddr = cfg.server.bind_addr.parse().with_context(|| {
        format!(
            "invalid ROUTER_BIND '{}': expected host:port",
            cfg.server.bind_addr
        )
    })?;

    HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allowed_methods(vec!["GET", "POST", "PUT"])
            .allowed_headers(vec![
                actix_web::http::header::CONTENT_TYPE,
                actix_web::http::header::ACCEPT,
                actix_web::http::header::AUTHORIZATION,
            ])
            .max_age(3600);

        App::new()
            .wrap(Logger::default())
            .wrap(cors)
            .app_data(web::Data::from(shared_engine.clone()))
            .configure(api::configure)
    })
    .bind(bind_addr)?
    .workers(cfg.server.workers)
    .run()
    .await?;

    Ok(())
}
