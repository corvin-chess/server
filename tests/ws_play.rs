//! CV-172: WS QuickPair + Move + Resign contract (gated on DATABASE_URL + REDIS_URL).

mod common;

use std::env;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tower::ServiceExt;

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn database_url() -> Option<String> {
    env::var("DATABASE_URL")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

async fn recv_json(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
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

async fn drain_until(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    typ: &str,
) -> Value {
    for _ in 0..12 {
        let msg = recv_json(ws).await;
        if msg.get("typ").and_then(Value::as_str) == Some(typ) {
            return msg;
        }
    }
    panic!("did not receive typ={typ}");
}

async fn register_user(
    state: corvin_server::AppState,
    mailer: &corvin_server::auth::mailer::CaptureMailer,
    email: &str,
    username: &str,
) -> (String, String) {
    let app = corvin_server::app(state.clone());

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/v1/auth/email/otp/start")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "purpose": "register",
                "email": email,
                "username": username,
                "locale": "en",
                "birth_year": 1990
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.expect("otp start");
    assert_eq!(res.status(), axum::http::StatusCode::OK);

    let code = mailer.last_code_for(email).expect("otp code");

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/v1/auth/email/otp/verify")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({
                "purpose": "register",
                "email": email,
                "code": code
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.expect("otp verify");
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let cookie = res
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("Set-Cookie")
        .to_owned();
    let body = res.into_body().collect().await.expect("body").to_bytes();
    let json: Value = serde_json::from_slice(&body).expect("json");
    let user_id = json["user_id"].as_str().expect("user_id").to_owned();
    let session = cookie
        .split(';')
        .next()
        .expect("cookie pair")
        .trim()
        .strip_prefix("corvin_session=")
        .expect("session hex")
        .to_owned();
    (user_id, session)
}

#[tokio::test]
async fn game_snapshot_json_shape() {
    let snap = corvin_server::ws::game_snapshot_json(
        "01GAMEIDCV172TEST00000001",
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
        300_000,
        300_000,
        "w",
        0,
    );
    assert_eq!(snap["typ"], "GameSnapshot");
    assert_eq!(snap["v"], 1);
    assert_eq!(snap["game_id"], "01GAMEIDCV172TEST00000001");
    assert_eq!(snap["active"], "w");
    assert_eq!(snap["seq"], 0);
    assert_eq!(snap["clocks"]["w"], 300_000);
}

#[tokio::test]
async fn two_clients_quick_pair_move_resign() {
    let Some(db_url) = database_url() else {
        eprintln!("skip ws_play: DATABASE_URL unset");
        return;
    };
    let Some(redis_url) = redis_url() else {
        eprintln!("skip ws_play: REDIS_URL unset");
        return;
    };
    let _lock = common::acquire_db_lock(&db_url).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&db_url)
        .await
        .expect("postgres");
    corvin_server::db::migrate_down(&pool)
        .await
        .expect("migrate_down");
    corvin_server::db::migrate_up(&pool)
        .await
        .expect("migrate_up");

    let mailer = Arc::new(corvin_server::auth::mailer::CaptureMailer::default());
    let redis = corvin_server::redis_store::connect(&redis_url)
        .await
        .expect("redis");
    let state = corvin_server::AppState::new_for_tests(
        pool.clone(),
        mailer.clone(),
        "localhost",
        "http://localhost:3000",
        b"test-otp-pepper-not-for-prod!!!!",
    )
    .expect("state")
    .with_redis(redis);

    let (user_a, session_a) = register_user(
        state.clone(),
        mailer.as_ref(),
        "wsplay-a@example.com",
        "WsPlayA",
    )
    .await;
    let (user_b, session_b) = register_user(
        state.clone(),
        mailer.as_ref(),
        "wsplay-b@example.com",
        "WsPlayB",
    )
    .await;
    assert_ne!(user_a, user_b);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = corvin_server::app(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    tokio::task::yield_now().await;

    async fn connect_authed(
        addr: std::net::SocketAddr,
        session_hex: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let url = format!("ws://{addr}/ws");
        // Auth via AuthTicket after Hello (cookie on WS upgrade is awkward in tungstenite).
        let (mut ws, _) = connect_async(&url).await.expect("ws connect");
        send_json(
            &mut ws,
            json!({"typ":"Hello","v":1,"client":"web","locale":"en"}),
        )
        .await;
        let welcome = drain_until(&mut ws, "Welcome").await;
        assert_eq!(welcome["v"], 1);
        let _ = drain_until(&mut ws, "Ping").await;
        send_json(
            &mut ws,
            json!({"typ":"AuthTicket","v":1,"token": session_hex}),
        )
        .await;
        let ack = drain_until(&mut ws, "Ack").await;
        assert_eq!(ack["v"], 1);
        ws
    }

    let mut ws_a = connect_authed(addr, &session_a).await;
    let mut ws_b = connect_authed(addr, &session_b).await;

    send_json(
        &mut ws_a,
        json!({
            "typ": "QuickPair",
            "v": 1,
            "time_class": "blitz",
            "initial_ms": 300_000,
            "increment_ms": 2000,
            "rated": true,
            "color": "random"
        }),
    )
    .await;
    let waiting = drain_until(&mut ws_a, "Ack").await;
    assert_eq!(waiting["typ"], "Ack");

    send_json(
        &mut ws_b,
        json!({
            "typ": "QuickPair",
            "v": 1,
            "time_class": "blitz",
            "initial_ms": 300_000,
            "increment_ms": 2000,
            "rated": true,
            "color": "random"
        }),
    )
    .await;

    let start_b = drain_until(&mut ws_b, "GameStart").await;
    let start_a = drain_until(&mut ws_a, "GameStart").await;
    assert_eq!(start_a["game_id"], start_b["game_id"]);
    let game_id = start_a["game_id"].as_str().expect("game_id").to_owned();
    let color_a = start_a["color"].as_str().expect("color a");
    let color_b = start_b["color"].as_str().expect("color b");
    assert_ne!(color_a, color_b);

    let (white_ws, black_ws, white_token) = if color_a == "w" {
        (&mut ws_a, &mut ws_b, user_a.as_str())
    } else {
        (&mut ws_b, &mut ws_a, user_b.as_str())
    };
    let _ = white_token;

    send_json(
        white_ws,
        json!({
            "typ": "Move",
            "v": 1,
            "id": "m1",
            "game_id": game_id,
            "uci": "e2e4"
        }),
    )
    .await;
    let applied_w = drain_until(white_ws, "MoveApplied").await;
    let applied_b = drain_until(black_ws, "MoveApplied").await;
    assert_eq!(applied_w["uci"], "e2e4");
    assert_eq!(applied_b["uci"], "e2e4");
    assert_eq!(applied_w["seq"], 1);

    send_json(
        white_ws,
        json!({
            "typ": "Resign",
            "v": 1,
            "game_id": game_id
        }),
    )
    .await;
    let end_w = drain_until(white_ws, "GameEnd").await;
    let end_b = drain_until(black_ws, "GameEnd").await;
    assert_eq!(end_w["reason"], "resign");
    assert_eq!(end_b["reason"], "resign");
    assert_eq!(end_w["result"], "0-1");
}
