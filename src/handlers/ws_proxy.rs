use anyhow::{Context, Result};
use axum::{
    extract::{
        WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::{HeaderMap, Request, uri::Uri},
    response::IntoResponse,
};
use futures_util::{SinkExt, stream::StreamExt};
use hyper::header::{CONNECTION, HOST, HeaderName, HeaderValue, UPGRADE};
use once_cell::sync::Lazy;
use std::collections::HashSet;
use tokio_tungstenite::{
    connect_async,
    tungstenite::protocol::{CloseFrame as TungsteniteCloseFrame, Message as TungsteniteMessage},
};
use tracing::{debug, error, info};

static WS_HOP_BY_HOP_HEADERS: Lazy<HashSet<HeaderName>> = Lazy::new(|| {
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

pub async fn ws_proxy_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    backend_uri: Uri,
    tail: String,
) -> impl IntoResponse {
    ws.on_upgrade(move |client_socket| async move {
        if let Err(error) = handle_websocket_proxy(client_socket, headers, backend_uri, tail).await
        {
            error!("WebSocket proxy error: {:?}", error);
        }
    })
}

async fn handle_websocket_proxy(
    client_socket: WebSocket,
    original_headers: HeaderMap,
    backend_uri: Uri,
    tail: String,
) -> Result<()> {
    let backend_socket =
        establish_backend_connection(&original_headers, &backend_uri, &tail).await?;
    info!("WebSocket proxy connection established successfully");

    bridge_websocket_connections(client_socket, backend_socket).await;
    debug!("WebSocket proxy connection terminated");

    Ok(())
}

async fn establish_backend_connection(
    original_headers: &HeaderMap,
    backend_uri: &Uri,
    tail: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let backend_request = build_backend_upgrade_request(original_headers, backend_uri, tail)?;
    let backend_url = backend_request.uri().to_string();

    debug!("Connecting to backend WebSocket: {}", backend_url);
    debug!("Backend headers: {:?}", backend_request.headers());

    let (backend_socket, _response) = connect_async(backend_request)
        .await
        .with_context(|| format!("Failed to connect to backend WebSocket: {}", backend_url))?;

    Ok(backend_socket)
}

fn build_backend_upgrade_request(
    original_headers: &HeaderMap,
    backend_uri: &Uri,
    tail: &str,
) -> Result<Request<()>> {
    let backend_url = construct_backend_websocket_url(backend_uri, tail)?;
    let filtered_headers = create_websocket_headers(original_headers, backend_uri)?;

    Request::builder()
        .method("GET")
        .uri(&backend_url)
        .body(())
        .map(|mut req| {
            *req.headers_mut() = filtered_headers;
            req
        })
        .context("Failed to build backend WebSocket upgrade request")
}

fn construct_backend_websocket_url(backend_uri: &Uri, tail: &str) -> Result<String> {
    let authority = backend_uri
        .authority()
        .context("Backend WebSocket URI must contain authority information")?;

    let scheme = match backend_uri.scheme_str() {
        Some("https") => "wss",
        Some("http") => "ws",
        Some(s) if s == "ws" || s == "wss" => s,
        _ => "ws",
    };

    let url = if tail.is_empty() {
        format!("{}://{}", scheme, authority)
    } else {
        let normalized_tail = tail.strip_prefix('/').unwrap_or(tail);
        format!("{}://{}/{}", scheme, authority, normalized_tail)
    };

    Ok(url)
}

fn create_websocket_headers(original_headers: &HeaderMap, backend_uri: &Uri) -> Result<HeaderMap> {
    let mut headers = HeaderMap::with_capacity(original_headers.len() + 4);

    for (name, value) in original_headers.iter() {
        if !WS_HOP_BY_HOP_HEADERS.contains(name) {
            headers.insert(name.clone(), value.clone());
        }
    }

    headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));

    if !headers.contains_key("sec-websocket-version") {
        headers.insert(
            HeaderName::from_static("sec-websocket-version"),
            HeaderValue::from_static("13"),
        );
    }

    let host = backend_uri
        .host()
        .context("Backend WebSocket URI missing host")?;
    let host_value =
        HeaderValue::from_str(host).context("Failed to create Host header for WebSocket")?;
    headers.insert(HOST, host_value);

    Ok(headers)
}

async fn bridge_websocket_connections(
    client_socket: WebSocket,
    backend_socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let (mut client_sender, mut client_receiver) = client_socket.split();
    let (mut backend_sender, mut backend_receiver) = backend_socket.split();

    let client_to_backend_task = async {
        while let Some(message_result) = client_receiver.next().await {
            match message_result {
                Ok(client_msg) => {
                    if let Some(backend_msg) = convert_axum_to_tungstenite(client_msg) {
                        if backend_sender.send(backend_msg).await.is_err() {
                            debug!(
                                "Backend connection closed, stopping client-to-backend forwarding"
                            );
                            break;
                        }
                    }
                }
                Err(error) => {
                    debug!("Client connection error: {:?}", error);
                    break;
                }
            }
        }
    };

    let backend_to_client_task = async {
        while let Some(message_result) = backend_receiver.next().await {
            match message_result {
                Ok(backend_msg) => {
                    if let Some(client_msg) = convert_tungstenite_to_axum(backend_msg) {
                        if client_sender.send(client_msg).await.is_err() {
                            debug!(
                                "Client connection closed, stopping backend-to-client forwarding"
                            );
                            break;
                        }
                    }
                }
                Err(error) => {
                    debug!("Backend connection error: {:?}", error);
                    break;
                }
            }
        }
    };

    tokio::select! {
        _ = client_to_backend_task => debug!("Client-to-backend message forwarding completed"),
        _ = backend_to_client_task => debug!("Backend-to-client message forwarding completed"),
    }
}

fn convert_axum_to_tungstenite(msg: Message) -> Option<TungsteniteMessage> {
    Some(match msg {
        Message::Text(t) => TungsteniteMessage::Text(t.as_str().into()),
        Message::Binary(b) => TungsteniteMessage::Binary(b),
        Message::Ping(p) => TungsteniteMessage::Ping(p),
        Message::Pong(p) => TungsteniteMessage::Pong(p),
        Message::Close(Some(frame)) => {
            TungsteniteMessage::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: frame.code.into(),
                reason: frame.reason.to_string().into(),
            }))
        }
        Message::Close(None) => TungsteniteMessage::Close(None),
    })
}

fn convert_tungstenite_to_axum(msg: TungsteniteMessage) -> Option<Message> {
    Some(match msg {
        TungsteniteMessage::Text(t) => Message::Text(t.as_str().into()),
        TungsteniteMessage::Binary(b) => Message::Binary(b),
        TungsteniteMessage::Ping(p) => Message::Ping(p),
        TungsteniteMessage::Pong(p) => Message::Pong(p),
        TungsteniteMessage::Close(Some(frame)) => Message::Close(Some(CloseFrame {
            code: frame.code.into(),
            reason: frame.reason.to_string().into(),
        })),
        TungsteniteMessage::Close(None) => Message::Close(None),
        TungsteniteMessage::Frame(_) => return None,
    })
}
