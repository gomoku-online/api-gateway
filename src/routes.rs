use crate::config::{AppConfig, Predicate};
use crate::handlers::{http_proxy::http_proxy_handler, ws_proxy::ws_proxy_handler};
use anyhow::{Context, Result, anyhow};
use axum::{
    Router,
    extract::{Path, WebSocketUpgrade},
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::{any, get},
};
use std::collections::HashSet;
use tracing::{debug, error};

pub fn build_router(config: AppConfig) -> Result<Router> {
    let mut registered_paths = HashSet::new();
    let mut router = Router::new();

    for route in config.gateway.routes.into_iter() {
        let backend_uri = parse_uri(&route.uri, &route.id)?;

        for predicate in route.predicates.into_iter() {
            let axum_path = convert_path_pattern_to_axum(&predicate)?;

            if !registered_paths.insert(axum_path.clone()) {
                return Err(anyhow!(
                    "중복된 라우트 경로가 감지되었습니다: '{}' (라우트 ID: '{}')",
                    axum_path,
                    route.id
                ));
            }

            router = add_route(
                router,
                backend_uri.clone(),
                &axum_path,
                &route.id
            );
        }
    }

    Ok(router)
}

fn parse_uri(uri_str: &str, route_id: &str) -> Result<Uri> {
    uri_str.parse::<Uri>().with_context(|| {
        format!("라우트 설정에 잘못된 URI가 있습니다 (ID: '{}'): '{}'", route_id, uri_str)
    })
}

fn convert_path_pattern_to_axum(predicate: &Predicate) -> Result<String> {
    match predicate {
        Predicate::Path(path_pattern) => Ok(path_pattern.replace("/**", "/{*path}")),
    }
}

fn add_route(
    router: Router,
    backend_uri: Uri,
    axum_path: &str,
    route_id: &str,
) -> Router {
    debug!(
        route_id = %route_id,
        "라우트 매핑: {} -> {}",
        axum_path,
        backend_uri,
    );

    if is_websocket_uri(&backend_uri) {
        router.route(
            axum_path,
            get(move |ws: WebSocketUpgrade, headers: HeaderMap, Path(tail): Path<String>| {
                ws_proxy_handler(ws, headers, backend_uri, tail)
            }),
        )
    } else {
        router.route(
            axum_path,
            any(async move |headers: HeaderMap, Path(tail): Path<String>, req| {
                match http_proxy_handler(backend_uri, tail, req).await {
                    Ok(response) => response.into_response(),
                    Err(e) => {
                        error!("HTTP 프록시 핸들러 실패: {:?}", e);
                        (StatusCode::INTERNAL_SERVER_ERROR, "내부 서버 오류").into_response()
                    }
                }
            }),
        )
    }
}

fn is_websocket_uri(uri: &Uri) -> bool {
    matches!(uri.scheme_str(), Some("ws") | Some("wss"))
}