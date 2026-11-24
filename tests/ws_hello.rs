//! CV-160: WebSocket Hello / Welcome / Ping contract tests against PROTOCOL.md.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn serve_ws() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let app = corvin_server::ws_router();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    tokio::task::yield_now().await;
    addr
}

async fn connect(
    addr: SocketAddr,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = connect_async(&url).await.expect("ws connect");
    ws
}

async fn recv_json(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("recv timeout")
        .expect("stream closed")
        .expect("ws error");
    match msg {
        Message::Text(text) => serde_json::from_str(&text).expect("json"),
        other => panic!("expected text frame, got {other:?}"),
    }
}

async fn send_json(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    value: Value,
) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .expect("send");
}

#[tokio::test]
async fn hello_receives_welcome_with_protocol_shape() {
    let addr = serve_ws().await;
    let mut ws = connect(addr).await;

    send_json(
        &mut ws,
        json!({
            "typ": "Hello",
            "v": 1,
            "client": "web",
            "locale": "en"
        }),
    )
    .await;

    let welcome = recv_json(&mut ws).await;
    assert_eq!(welcome["typ"], "Welcome");
    assert_eq!(welcome["v"], 1);
    let session_id = welcome["session_id"].as_str().expect("session_id string");
    assert!(!session_id.is_empty(), "session_id must be non-empty");
    assert_eq!(session_id.len(), 26, "session_id should be a ULID");
    let server_time = welcome["server_time"].as_u64().expect("server_time ms");
    assert!(
        server_time > 1_700_000_000_000,
        "server_time looks like unix ms"
    );
    let features = welcome["features"].as_array().expect("features array");
    assert!(features.is_empty(), "features starts empty per PROTOCOL");
}

#[tokio::test]
async fn welcome_followed_by_server_rtt_ping() {
    // PROTOCOL: either side may send Ping; server measures RTT via a post-Welcome Ping.
    let addr = serve_ws().await;
    let mut ws = connect(addr).await;

    send_json(
        &mut ws,
        json!({ "typ": "Hello", "v": 1, "client": "bot", "locale": "en" }),
    )
    .await;

    let welcome = recv_json(&mut ws).await;
    assert_eq!(welcome["typ"], "Welcome");

    let ping = recv_json(&mut ws).await;
    assert_eq!(ping["typ"], "Ping");
    assert_eq!(ping["v"], 1);
    let nonce = ping["nonce"].as_str().expect("nonce string");
    assert!(!nonce.is_empty());

    send_json(&mut ws, json!({ "typ": "Pong", "v": 1, "nonce": nonce })).await;

    // Connection remains usable for client-initiated Ping after server RTT sample.
    send_json(
        &mut ws,
        json!({ "typ": "Ping", "v": 1, "nonce": "client-after-rtt" }),
    )
    .await;
    let pong = recv_json(&mut ws).await;
    assert_eq!(pong["typ"], "Pong");
    assert_eq!(pong["nonce"], "client-after-rtt");
}

#[tokio::test]
async fn client_ping_echoes_pong_nonce() {
    let addr = serve_ws().await;
    let mut ws = connect(addr).await;

    send_json(
        &mut ws,
        json!({ "typ": "Hello", "v": 1, "client": "mobile", "locale": "it" }),
    )
    .await;
    let _welcome = recv_json(&mut ws).await;
    let _server_ping = recv_json(&mut ws).await;

    send_json(
        &mut ws,
        json!({ "typ": "Ping", "v": 1, "nonce": "rtt-probe-1" }),
    )
    .await;

    let pong = recv_json(&mut ws).await;
    assert_eq!(pong["typ"], "Pong");
    assert_eq!(pong["v"], 1);
    assert_eq!(pong["nonce"], "rtt-probe-1");
}

#[tokio::test]
async fn unsupported_protocol_version_returns_proto_version_error() {
    let addr = serve_ws().await;
    let mut ws = connect(addr).await;

    send_json(
        &mut ws,
        json!({ "typ": "Hello", "v": 99, "client": "web", "locale": "en" }),
    )
    .await;

    let err = recv_json(&mut ws).await;
    assert_eq!(err["typ"], "Error");
    assert_eq!(err["v"], 1);
    assert_eq!(err["code"], "PROTO_VERSION");
}

#[tokio::test]
async fn ping_before_hello_is_rejected() {
    let addr = serve_ws().await;
    let mut ws = connect(addr).await;

    send_json(
        &mut ws,
        json!({ "typ": "Ping", "v": 1, "nonce": "too-early" }),
    )
    .await;

    let err = recv_json(&mut ws).await;
    assert_eq!(err["typ"], "Error");
    assert_eq!(err["v"], 1);
    assert_eq!(err["code"], "INTERNAL");
}

#[tokio::test]
async fn invalid_client_on_hello_is_rejected() {
    let addr = serve_ws().await;
    let mut ws = connect(addr).await;

    send_json(
        &mut ws,
        json!({ "typ": "Hello", "v": 1, "client": "desktop", "locale": "en" }),
    )
    .await;

    let err = recv_json(&mut ws).await;
    assert_eq!(err["typ"], "Error");
    assert_eq!(err["v"], 1);
    assert_eq!(err["code"], "INTERNAL");
}

#[tokio::test]
async fn invalid_locale_on_hello_is_rejected() {
    let addr = serve_ws().await;
    let mut ws = connect(addr).await;

    send_json(
        &mut ws,
        json!({ "typ": "Hello", "v": 1, "client": "web", "locale": "" }),
    )
    .await;
    let err = recv_json(&mut ws).await;
    assert_eq!(err["typ"], "Error");
    assert_eq!(err["code"], "INTERNAL");

    let mut ws = connect(addr).await;
    let long = "abcdefghijklmnopq"; // 17 chars > auth 2..=16
    send_json(
        &mut ws,
        json!({ "typ": "Hello", "v": 1, "client": "web", "locale": long }),
    )
    .await;
    let err = recv_json(&mut ws).await;
    assert_eq!(err["typ"], "Error");
    assert_eq!(err["code"], "INTERNAL");
}

#[tokio::test]
async fn ping_with_bad_v_returns_proto_version_error() {
    let addr = serve_ws().await;
    let mut ws = connect(addr).await;

    send_json(
        &mut ws,
        json!({ "typ": "Hello", "v": 1, "client": "web", "locale": "en" }),
    )
    .await;
    let _welcome = recv_json(&mut ws).await;
    let _server_ping = recv_json(&mut ws).await;

    send_json(&mut ws, json!({ "typ": "Ping", "v": 2, "nonce": "x" })).await;

    let err = recv_json(&mut ws).await;
    assert_eq!(err["typ"], "Error");
    assert_eq!(err["code"], "PROTO_VERSION");
}

#[tokio::test]
async fn invalid_json_returns_internal_error() {
    let addr = serve_ws().await;
    let mut ws = connect(addr).await;

    ws.send(Message::Text("{not-json".into()))
        .await
        .expect("send");
    let err = recv_json(&mut ws).await;
    assert_eq!(err["typ"], "Error");
    assert_eq!(err["code"], "INTERNAL");
}

#[tokio::test]
async fn oversized_text_frame_is_rejected() {
    let addr = serve_ws().await;
    let mut ws = connect(addr).await;
    let huge = format!(
        "{{\"typ\":\"Hello\",\"v\":1,\"client\":\"web\",\"locale\":\"{}\"}}",
        "e".repeat(5000)
    );
    ws.send(Message::Text(huge.into())).await.expect("send");
    let err = recv_json(&mut ws).await;
    assert_eq!(err["typ"], "Error");
    assert_eq!(err["code"], "INTERNAL");
}

#[tokio::test]
async fn health_still_works_on_ws_router() {
    let app = corvin_server::ws_router();
    let response = tower::ServiceExt::oneshot(
        app,
        axum::http::Request::builder()
            .uri("/health")
            .body(axum::body::Body::empty())
            .expect("request"),
    )
    .await
    .expect("oneshot");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}
