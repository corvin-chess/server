//! WebSocket play protocol (Hello / Welcome / Ping + play dispatch CV-172).
//!
//! Handshake-only `router()` keeps CV-160 contract tests free of Postgres/Redis.
//! Full `app` wires `/ws` with `AppState` (cookie/AuthTicket, Redis, hub).

mod hub;
mod play;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use ulid::Ulid;

use crate::game::LagTracker;
use crate::redis_store;
use crate::AppState;

pub use hub::PlayHub;
pub use play::game_snapshot_json;

const PROTOCOL_V: u32 = 1;
/// Cap inbound JSON text frames before parse (DoS bound; PROTOCOL RATE_LIMIT comes later).
const MAX_TEXT_BYTES: usize = 4096;

/// Router fragment with `/ws` (and `/health`) for handshake-only tests.
pub fn router() -> Router {
    Router::new()
        .route("/ws", get(upgrade_handshake_only))
        .route("/health", get(crate::health))
}

async fn upgrade_handshake_only(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_handshake_only)
}

/// Stateful upgrade used by the full HTTP app.
pub async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    ws.on_upgrade(move |socket| handle_socket(socket, state, cookie))
}

async fn handle_handshake_only(mut socket: WebSocket) {
    let mut welcomed = false;
    let mut pending_rtt_nonce: Option<String> = None;

    while let Some(frame) = socket.recv().await {
        let Ok(msg) = frame else {
            break;
        };
        match msg {
            Message::Text(text) => {
                if text.len() > MAX_TEXT_BYTES {
                    let _ = send_error(&mut socket, "INTERNAL", "message too large").await;
                    break;
                }
                if handle_handshake_text(
                    &mut socket,
                    text.as_str(),
                    &mut welcomed,
                    &mut pending_rtt_nonce,
                    &mut LagTracker::new(),
                )
                .await
                .is_err()
                {
                    break;
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
        }
    }
}

async fn handle_socket(socket: WebSocket, state: AppState, cookie: Option<String>) {
    let mut user_id = play::user_id_from_cookie_header(&state, cookie.as_deref()).await;
    let hub = state.hub.clone();
    let mut redis = state.redis.clone();
    let mut lag = LagTracker::new();
    let mut welcomed = false;
    let mut pending_rtt_nonce: Option<String> = None;
    let mut pending_rtt_sent_ms: Option<u64> = None;
    let mut conn_id: Option<hub::ConnId> = None;

    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

    // Writer task: local replies + hub pushes.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let text = msg.to_string();
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(frame) = stream.next().await {
        let Ok(msg) = frame else {
            break;
        };
        match msg {
            Message::Text(text) => {
                if text.len() > MAX_TEXT_BYTES {
                    let _ = tx.send(error_value("INTERNAL", "message too large"));
                    break;
                }
                let value: Value = match serde_json::from_str(text.as_str()) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = tx.send(error_value("INTERNAL", "invalid JSON"));
                        continue;
                    }
                };
                let typ = value.get("typ").and_then(Value::as_str).unwrap_or("");

                if !welcomed {
                    match typ {
                        "Hello" => {
                            match handle_hello_value(
                                &value,
                                &mut welcomed,
                                &mut pending_rtt_nonce,
                                &mut pending_rtt_sent_ms,
                            ) {
                                Ok(frames) => {
                                    for f in frames {
                                        let _ = tx.send(f);
                                    }
                                    // Register hub after Welcome so GameStart can land.
                                    if let Some(uid) = user_id.as_deref() {
                                        let id = hub.register(uid, tx.clone()).await;
                                        conn_id = Some(id);
                                        if let Some(ref mut conn) = redis {
                                            let _ = redis_store::touch_presence(
                                                conn,
                                                uid,
                                                redis_store::DEFAULT_PRESENCE_TTL_SECS,
                                            )
                                            .await;
                                        }
                                    }
                                }
                                Err(err) => {
                                    let _ = tx.send(err);
                                }
                            }
                        }
                        _ => {
                            let _ = tx.send(error_value("INTERNAL", "Hello required"));
                        }
                    }
                    continue;
                }

                match typ {
                    "Ping" => {
                        if let Some(frame) = handle_client_ping(&value) {
                            let _ = tx.send(frame);
                        }
                    }
                    "Pong" => {
                        handle_client_pong(
                            &value,
                            &mut pending_rtt_nonce,
                            &mut pending_rtt_sent_ms,
                            &mut lag,
                        );
                    }
                    _ => {
                        // Re-bind hub when AuthTicket (or similar) changes the bound user.
                        let before = user_id.clone();
                        let frames = play::dispatch(
                            &state,
                            &hub,
                            &mut redis,
                            &mut user_id,
                            &mut lag,
                            &value,
                        )
                        .await;
                        if before.as_deref() != user_id.as_deref() {
                            if let (Some(old), Some(id)) = (before.as_deref(), conn_id) {
                                hub.unregister(old, id).await;
                                conn_id = None;
                            }
                            if let Some(uid) = user_id.as_deref() {
                                let id = hub.register(uid, tx.clone()).await;
                                conn_id = Some(id);
                            }
                        }
                        for f in frames {
                            let _ = tx.send(f);
                        }
                    }
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
        }
    }

    if let (Some(uid), Some(id)) = (user_id.as_deref(), conn_id) {
        hub.unregister(uid, id).await;
    }
    drop(tx);
    let _ = writer.await;
}

async fn handle_handshake_text(
    socket: &mut WebSocket,
    text: &str,
    welcomed: &mut bool,
    pending_rtt_nonce: &mut Option<String>,
    _lag: &mut LagTracker,
) -> Result<(), ()> {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            send_error(socket, "INTERNAL", "invalid JSON").await?;
            return Ok(());
        }
    };

    let typ = value.get("typ").and_then(Value::as_str).unwrap_or("");
    match typ {
        "Hello" => {
            let mut sent_ms = None;
            match handle_hello_value(&value, welcomed, pending_rtt_nonce, &mut sent_ms) {
                Ok(frames) => {
                    for f in frames {
                        send_json(socket, &f).await?;
                    }
                }
                Err(err) => send_json(socket, &err).await?,
            }
            Ok(())
        }
        "Ping" => {
            if !*welcomed {
                send_error(socket, "INTERNAL", "Hello required").await?;
                return Ok(());
            }
            if let Some(pong) = handle_client_ping(&value) {
                send_json(socket, &pong).await?;
            }
            Ok(())
        }
        "Pong" => {
            if !*welcomed {
                send_error(socket, "INTERNAL", "Hello required").await?;
                return Ok(());
            }
            // Handshake-only path does not feed LagTracker (no moves).
            let mut sent_ms = None;
            let mut lag = LagTracker::new();
            handle_client_pong(&value, pending_rtt_nonce, &mut sent_ms, &mut lag);
            Ok(())
        }
        _ => {
            if !*welcomed {
                send_error(socket, "INTERNAL", "Hello required").await?;
            } else {
                send_error(socket, "INTERNAL", "unknown message type").await?;
            }
            Ok(())
        }
    }
}

#[derive(Debug, Deserialize)]
struct HelloMsg {
    v: u32,
    client: String,
    locale: String,
}

fn handle_hello_value(
    value: &Value,
    welcomed: &mut bool,
    pending_rtt_nonce: &mut Option<String>,
    pending_rtt_sent_ms: &mut Option<u64>,
) -> Result<Vec<Value>, Value> {
    if *welcomed {
        return Err(error_value("INTERNAL", "already welcomed"));
    }

    let hello: HelloMsg = serde_json::from_value(value.clone())
        .map_err(|_| error_value("INTERNAL", "invalid Hello"))?;

    if hello.v != PROTOCOL_V {
        return Err(error_value("PROTO_VERSION", "unsupported protocol version"));
    }

    if !matches!(hello.client.as_str(), "web" | "mobile" | "bot") {
        return Err(error_value("INTERNAL", "client must be web|mobile|bot"));
    }

    let locale_len = hello.locale.trim().len();
    if !(2..=16).contains(&locale_len) {
        return Err(error_value("INTERNAL", "invalid locale"));
    }

    let session_id = Ulid::new().to_string();
    let server_time = unix_ms();
    let welcome = json!({
        "typ": "Welcome",
        "v": PROTOCOL_V,
        "session_id": session_id,
        "server_time": server_time,
        "features": []
    });
    *welcomed = true;

    let nonce = Ulid::new().to_string();
    *pending_rtt_nonce = Some(nonce.clone());
    *pending_rtt_sent_ms = Some(unix_ms());
    let ping = json!({
        "typ": "Ping",
        "v": PROTOCOL_V,
        "nonce": nonce
    });
    Ok(vec![welcome, ping])
}

fn handle_client_ping(value: &Value) -> Option<Value> {
    let v = value.get("v").and_then(Value::as_u64).unwrap_or(0);
    if v != u64::from(PROTOCOL_V) {
        return Some(error_value("PROTO_VERSION", "unsupported protocol version"));
    }
    let nonce = value.get("nonce").and_then(Value::as_str)?;
    if nonce.is_empty() || nonce.len() > 128 {
        return Some(error_value("INTERNAL", "Ping requires nonce"));
    }
    Some(json!({
        "typ": "Pong",
        "v": PROTOCOL_V,
        "nonce": nonce
    }))
}

fn handle_client_pong(
    value: &Value,
    pending_rtt_nonce: &mut Option<String>,
    pending_rtt_sent_ms: &mut Option<u64>,
    lag: &mut LagTracker,
) {
    let Some(expected) = pending_rtt_nonce.as_deref() else {
        return;
    };
    let Some(nonce) = value.get("nonce").and_then(Value::as_str) else {
        return;
    };
    if nonce != expected {
        return;
    }
    if let Some(sent) = pending_rtt_sent_ms.take() {
        let rtt = unix_ms().saturating_sub(sent);
        lag.observe_rtt(rtt);
    }
    *pending_rtt_nonce = None;
}

fn error_value(code: &str, message: &str) -> Value {
    json!({
        "typ": "Error",
        "v": PROTOCOL_V,
        "code": code,
        "message": message
    })
}

async fn send_error(socket: &mut WebSocket, code: &str, message: &str) -> Result<(), ()> {
    send_json(socket, &error_value(code, message)).await
}

async fn send_json(socket: &mut WebSocket, value: &Value) -> Result<(), ()> {
    let text = value.to_string();
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

fn unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
