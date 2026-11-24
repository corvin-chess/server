//! WebSocket play protocol handshake (Hello / Welcome / Ping).
//!
//! Scope for CV-160: connection lifecycle only. Auth tickets, lobby,
//! in-game messages, and per-connection `RATE_LIMIT` land in later tasks.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};
use ulid::Ulid;

const PROTOCOL_V: u32 = 1;
/// Cap inbound JSON text frames before parse (DoS bound; PROTOCOL RATE_LIMIT comes later).
const MAX_TEXT_BYTES: usize = 4096;

/// Router fragment with `/ws` (and `/health` for handshake-only tests).
pub fn router() -> Router {
    Router::new()
        .route("/ws", get(upgrade))
        .route("/health", get(crate::health))
}

pub async fn upgrade(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    let mut welcomed = false;
    // Nonce of the post-Welcome server Ping used to sample client RTT.
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
                if handle_text(
                    &mut socket,
                    text.as_str(),
                    &mut welcomed,
                    &mut pending_rtt_nonce,
                )
                .await
                .is_err()
                {
                    break;
                }
            }
            Message::Close(_) => break,
            // Axum auto-replies to WebSocket control Ping; ignore control Pong/Ping here.
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
        }
    }
}

async fn handle_text(
    socket: &mut WebSocket,
    text: &str,
    welcomed: &mut bool,
    pending_rtt_nonce: &mut Option<String>,
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
        "Hello" => handle_hello(socket, &value, welcomed, pending_rtt_nonce).await,
        "Ping" => handle_ping(socket, &value, *welcomed).await,
        "Pong" => {
            if !*welcomed {
                send_error(socket, "INTERNAL", "Hello required").await?;
                return Ok(());
            }
            handle_pong(&value, pending_rtt_nonce);
            Ok(())
        }
        _ => {
            if !*welcomed {
                send_error(socket, "INTERNAL", "Hello required").await?;
            } else {
                // Unknown types after Welcome stay out of scope for CV-160.
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

async fn handle_hello(
    socket: &mut WebSocket,
    value: &Value,
    welcomed: &mut bool,
    pending_rtt_nonce: &mut Option<String>,
) -> Result<(), ()> {
    if *welcomed {
        send_error(socket, "INTERNAL", "already welcomed").await?;
        return Ok(());
    }

    let hello: HelloMsg = match serde_json::from_value(value.clone()) {
        Ok(h) => h,
        Err(_) => {
            send_error(socket, "INTERNAL", "invalid Hello").await?;
            return Ok(());
        }
    };

    if hello.v != PROTOCOL_V {
        send_error(socket, "PROTO_VERSION", "unsupported protocol version").await?;
        return Ok(());
    }

    if !matches!(hello.client.as_str(), "web" | "mobile" | "bot") {
        send_error(socket, "INTERNAL", "client must be web|mobile|bot").await?;
        return Ok(());
    }

    // Match HTTP auth locale bounds (2..=16) so Hello and register agree.
    let locale_len = hello.locale.trim().len();
    if !(2..=16).contains(&locale_len) {
        send_error(socket, "INTERNAL", "invalid locale").await?;
        return Ok(());
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
    send_json(socket, &welcome).await?;
    *welcomed = true;

    // PROTOCOL: either side may Ping; server starts RTT sampling after Welcome.
    let nonce = Ulid::new().to_string();
    *pending_rtt_nonce = Some(nonce.clone());
    let ping = json!({
        "typ": "Ping",
        "v": PROTOCOL_V,
        "nonce": nonce
    });
    send_json(socket, &ping).await?;
    Ok(())
}

async fn handle_ping(socket: &mut WebSocket, value: &Value, welcomed: bool) -> Result<(), ()> {
    if !welcomed {
        send_error(socket, "INTERNAL", "Hello required").await?;
        return Ok(());
    }

    let v = value.get("v").and_then(Value::as_u64).unwrap_or(0);
    if v != u64::from(PROTOCOL_V) {
        send_error(socket, "PROTO_VERSION", "unsupported protocol version").await?;
        return Ok(());
    }

    let nonce = match value.get("nonce").and_then(Value::as_str) {
        Some(n) if !n.is_empty() && n.len() <= 128 => n,
        _ => {
            send_error(socket, "INTERNAL", "Ping requires nonce").await?;
            return Ok(());
        }
    };

    let pong = json!({
        "typ": "Pong",
        "v": PROTOCOL_V,
        "nonce": nonce
    });
    send_json(socket, &pong).await
}

fn handle_pong(value: &Value, pending_rtt_nonce: &mut Option<String>) {
    let Some(expected) = pending_rtt_nonce.as_deref() else {
        return;
    };
    let Some(nonce) = value.get("nonce").and_then(Value::as_str) else {
        return;
    };
    if nonce == expected {
        // RTT sample complete; WS move dispatch will feed LagTracker / apply_move_with_rtt.
        *pending_rtt_nonce = None;
    }
}

async fn send_error(socket: &mut WebSocket, code: &str, message: &str) -> Result<(), ()> {
    let err = json!({
        "typ": "Error",
        "v": PROTOCOL_V,
        "code": code,
        "message": message
    });
    send_json(socket, &err).await
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
