use actix_cors::Cors;
use actix_web::{middleware::Logger, web, App, HttpServer};
use anyhow::Context;
use std::net::SocketAddr;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use router::config::RouterConfig;
use router::engine::RouterEngine;
use router::{api, errors};

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
    let shared_engine = web::Data::new(engine);

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
            .app_data(
                web::JsonConfig::default()
                    .limit(64 * 1024)
                    .error_handler(|err, _| errors::json_error(err)),
            )
            .app_data(shared_engine.clone())
            .configure(api::configure)
    })
    .bind(bind_addr)?
    .workers(cfg.server.workers)
    .run()
    .await?;

    Ok(())
}
