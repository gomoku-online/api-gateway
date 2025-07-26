mod config;
mod handlers;
mod routes;

use crate::config::AppConfig;
use crate::routes::build_router;
use anyhow::{Context, Result};
use std::net::SocketAddr;
use tower_http::trace::{DefaultMakeSpan, TraceLayer};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let config = AppConfig::load()
        .context("Failed to load configuration")?;

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(&config.logging.level))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let app = build_router(config.clone())
        .context("Failed to build router from configuration")?
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        );

    let addr_str = format!("{}:{}", config.server.host, config.server.port);
    let addr: SocketAddr = addr_str
        .parse()
        .with_context(|| format!("Invalid server address format: '{}'", addr_str))?;

    info!("API Gateway listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to address {}", addr))?;

    axum::serve(listener, app.into_make_service())
        .await
        .context("API Gateway server failed")?;

    Ok(())
}