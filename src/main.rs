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
        .context("설정을 불러오는데 실패했습니다")?;

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(&config.logging.level))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let app = build_router(config.clone())
        .context("설정으로 라우터를 빌드하는데 실패했습니다")?
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        );

    let addr_str = format!("{}:{}", config.server.host, config.server.port);
    let addr: SocketAddr = addr_str
        .parse()
        .with_context(|| format!("잘못된 서버 주소 형식입니다: '{}'", addr_str))?;

    info!("API 게이트웨이가 {}에서 수신 대기 중입니다", addr);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("{} 주소에 바인딩하는데 실패했습니다", addr))?;

    axum::serve(listener, app.into_make_service())
        .await
        .context("API 게이트웨이 서버에 오류가 발생했습니다")?;

    Ok(())
}