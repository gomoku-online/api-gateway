use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, Method, Version, uri::Uri},
    response::Response,
};
use http::uri::PathAndQuery;
use hyper::{
    Response as HyperResponse,
    body::Incoming as HyperBody,
    client::conn::http1,
    header::{CONNECTION, HOST, HeaderName, HeaderValue, UPGRADE},
};
use hyper_util::rt::TokioIo;
use once_cell::sync::Lazy;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use tokio::net::TcpStream;
use tracing::{debug, error};

static HOP_BY_HOP_HEADERS: Lazy<HashSet<HeaderName>> = Lazy::new(|| {
    [
        CONNECTION,
        HeaderName::from_static("keep-alive"),
        HeaderName::from_static("proxy-authenticate"),
        HeaderName::from_static("proxy-authorization"),
        HeaderName::from_static("te"),
        HeaderName::from_static("trailers"),
        HeaderName::from_static("transfer-encoding"),
        UPGRADE,
    ]
    .into_iter()
    .collect()
});

pub async fn http_proxy_handler(backend_uri: Uri, tail: String, req: Request) -> Result<Response> {
    debug!("Proxying request to backend: {}", backend_uri);

    let backend_request = build_backend_request(req, &backend_uri, &tail)?;
    let backend_response = send_request_to_backend(backend_request, &backend_uri).await?;
    let filtered_response = filter_response_headers(backend_response);

    Ok(filtered_response)
}

fn build_backend_request(mut req: Request, backend_uri: &Uri, tail: &str) -> Result<Request> {
    let new_path_and_query = build_path_and_query(req.uri(), tail)?;
    let authority = extract_authority(backend_uri)?;
    let filtered_headers = filter_request_headers(req.headers(), &authority)?;

    let relative_uri = Uri::from(new_path_and_query);

    *req.uri_mut() = relative_uri;
    *req.headers_mut() = filtered_headers;

    Ok(req)
}

fn build_path_and_query(original_uri: &Uri, tail: &str) -> Result<PathAndQuery> {
    let normalized_tail = if tail.starts_with('/') {
        tail.to_string()
    } else {
        format!("/{}", tail)
    };

    let full_path = match original_uri.query() {
        Some(query) => format!("{}?{}", normalized_tail, query),
        None => normalized_tail,
    };

    PathAndQuery::from_str(&full_path)
        .context("Failed to create PathAndQuery from constructed path")
}

fn extract_authority(backend_uri: &Uri) -> Result<hyper::http::uri::Authority> {
    backend_uri
        .authority()
        .cloned()
        .context("Backend URI must contain authority (host:port) information")
}

fn filter_request_headers(
    original_headers: &HeaderMap,
    authority: &hyper::http::uri::Authority,
) -> Result<HeaderMap> {
    let mut filtered_headers = HeaderMap::with_capacity(original_headers.len());
    let connection_specific_headers = extract_connection_headers(original_headers);

    for (name, value) in original_headers.iter() {
        if should_forward_header(name, &connection_specific_headers) {
            filtered_headers.insert(name.clone(), value.clone());
        }
    }

    let host_value =
        HeaderValue::from_str(authority.as_str()).context("Failed to create Host header value")?;
    filtered_headers.insert(HOST, host_value);

    Ok(filtered_headers)
}

fn extract_connection_headers(headers: &HeaderMap) -> HashSet<String> {
    headers
        .get(CONNECTION)
        .and_then(|header_value| header_value.to_str().ok())
        .map(|connection_str| {
            connection_str
                .split(',')
                .map(|header_name| header_name.trim().to_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

fn should_forward_header(name: &HeaderName, connection_headers: &HashSet<String>) -> bool {
    !HOP_BY_HOP_HEADERS.contains(name)
        && !connection_headers.contains(&name.as_str().to_lowercase())
}

async fn send_request_to_backend(
    req: Request,
    backend_uri: &Uri,
) -> Result<HyperResponse<HyperBody>> {
    let socket_addr = resolve_backend_address(backend_uri)?;
    let tcp_stream = establish_connection(socket_addr).await?;
    let (mut sender, connection) = perform_http_handshake(tcp_stream).await?;

    tokio::spawn(async move {
        if let Err(err) = connection.await {
            error!("Backend connection error: {:?}", err);
        }
    });

    sender
        .send_request(req)
        .await
        .context("Failed to send request to backend")
}

fn resolve_backend_address(backend_uri: &Uri) -> Result<SocketAddr> {
    let host = backend_uri.host().context("Backend URI missing host")?;

    let port = backend_uri.port_u16().unwrap_or(80);
    let ip_addr: IpAddr = host
        .parse()
        .with_context(|| format!("Invalid IP address in backend URI: {}", host))?;

    Ok(SocketAddr::new(ip_addr, port))
}

async fn establish_connection(addr: SocketAddr) -> Result<TcpStream> {
    TcpStream::connect(addr)
        .await
        .with_context(|| format!("Failed to connect to backend at {}", addr))
}

async fn perform_http_handshake(
    stream: TcpStream,
) -> Result<(
    hyper::client::conn::http1::SendRequest<Body>,
    hyper::client::conn::http1::Connection<TokioIo<TcpStream>, Body>,
)> {
    let io = TokioIo::new(stream);
    http1::handshake(io)
        .await
        .context("HTTP/1.1 handshake with backend failed")
}

fn filter_response_headers(backend_response: HyperResponse<HyperBody>) -> Response {
    let (parts, body) = backend_response.into_parts();
    let connection_headers = extract_connection_headers(&parts.headers);

    let mut filtered_headers = HeaderMap::with_capacity(parts.headers.len());

    for (name, value) in parts.headers.iter() {
        if should_forward_header(name, &connection_headers) {
            filtered_headers.insert(name.clone(), value.clone());
        }
    }

    Response::builder()
        .status(parts.status)
        .version(parts.version)
        .body(Body::new(body))
        .expect("Failed to build response - this should never happen")
}
