//! CV-133: passkey register + login e2e via SoftPasskey (gated on DATABASE_URL).

mod common;

use std::env;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower::ServiceExt;
use url::Url;
use webauthn_authenticator_rs::prelude::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse};

fn database_url() -> Option<String> {
    env::var("DATABASE_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

async fn connect(url: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
        .unwrap_or_else(|err| panic!("connect DATABASE_URL failed: {err}"))
}

async fn json_request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value, Option<String>) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).expect("body")))
                .expect("request"),
        )
        .await
        .expect("oneshot");

    let status = response.status();
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            panic!(
                "expected JSON body, got {:?}: {err}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, json, set_cookie)
}

#[tokio::test]
async fn passkey_register_then_login_sets_session_cookie() {
    let Some(url) = database_url() else {
        eprintln!("skip passkey_register_then_login_sets_session_cookie: DATABASE_URL unset");
        return;
    };
    let _lock = common::acquire_db_lock(&url).await;
    let pool = connect(&url).await;
    corvin_server::db::migrate_down(&pool)
        .await
        .expect("migrate_down");
    corvin_server::db::migrate_up(&pool)
        .await
        .expect("migrate_up");

    let mailer = Arc::new(corvin_server::auth::mailer::CaptureMailer::default());
    let origin = "http://localhost:3000";
    let state = corvin_server::AppState::new_for_tests(
        pool.clone(),
        mailer,
        "localhost",
        origin,
        b"test-otp-pepper-not-for-prod!!!!",
    )
    .expect("AppState");

    let username = "RavenKey";
    let origin_url = Url::parse(origin).expect("origin url");
    let mut soft = SoftPasskey::new(true);

    let (status, start_body, _) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/passkey/register/start",
        json!({
            "username": username,
            "email": "ravenkey@example.com",
            "locale": "en"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register start body={start_body}");
    let challenge_id = start_body["challenge_id"]
        .as_str()
        .expect("challenge_id")
        .to_owned();
    let ccr: CreationChallengeResponse =
        serde_json::from_value(start_body["publicKey"].clone()).expect("ccr");

    let reg_cred = soft
        .do_registration(origin_url.clone(), ccr)
        .expect("softpasskey registration");

    let (status, finish_body, cookie) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/passkey/register/finish",
        json!({
            "challenge_id": challenge_id,
            "credential": reg_cred
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register finish body={finish_body}");
    assert_eq!(finish_body["username"], username);
    let cookie = cookie.expect("register finish must Set-Cookie");
    assert!(cookie.contains("corvin_session="));

    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM passkeys")
        .fetch_one(&pool)
        .await
        .expect("count passkeys");
    assert_eq!(count, 1);

    let (status, login_start, _) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/passkey/login/start",
        json!({ "username": username }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login start body={login_start}");
    let login_challenge_id = login_start["challenge_id"]
        .as_str()
        .expect("challenge_id")
        .to_owned();
    let rcr: RequestChallengeResponse =
        serde_json::from_value(login_start["publicKey"].clone()).expect("rcr");

    let auth_cred = soft
        .do_authentication(origin_url, rcr)
        .expect("softpasskey authentication");

    let (status, login_finish, cookie) = json_request(
        corvin_server::app(state),
        "POST",
        "/api/v1/auth/passkey/login/finish",
        json!({
            "challenge_id": login_challenge_id,
            "credential": auth_cred
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login finish body={login_finish}");
    assert_eq!(login_finish["username"], username);
    let cookie = cookie.expect("login finish must Set-Cookie");
    assert!(cookie.contains("corvin_session="));
    assert!(cookie.to_ascii_lowercase().contains("httponly"));

    corvin_server::db::migrate_down(&pool)
        .await
        .expect("cleanup migrate_down");
}
