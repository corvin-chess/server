//! CV-133: email OTP register + login e2e (gated on DATABASE_URL).

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
async fn email_otp_register_then_login_sets_session_cookie() {
    let Some(url) = database_url() else {
        eprintln!("skip email_otp_register_then_login_sets_session_cookie: DATABASE_URL unset");
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
    let state = corvin_server::AppState::new_for_tests(
        pool.clone(),
        mailer.clone(),
        "localhost",
        "http://localhost:3000",
        b"test-otp-pepper-not-for-prod!!!!",
    )
    .expect("AppState");

    let email = "raven@example.com";
    let username = "RavenOTP";

    let (status, body, _) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/email/otp/start",
        json!({
            "purpose": "register",
            "email": email,
            "username": username,
            "locale": "en"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);

    let code = mailer
        .last_code_for(email)
        .expect("OTP must be captured for register");

    let (status, body, cookie) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/email/otp/verify",
        json!({
            "purpose": "register",
            "email": email,
            "code": code
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register verify body={body}");
    assert_eq!(body["username"], username);
    assert!(body["user_id"].as_str().is_some());
    assert_eq!(
        body["csrf_token"].as_str().map(|value| value.len()),
        Some(64),
        "register must return 32-byte hex csrf_token"
    );
    let cookie = cookie.expect("register must Set-Cookie");
    assert!(
        cookie.contains("corvin_session="),
        "cookie missing session: {cookie}"
    );
    assert!(
        cookie.to_ascii_lowercase().contains("httponly"),
        "cookie must be HttpOnly: {cookie}"
    );
    assert!(
        cookie.to_ascii_lowercase().contains("samesite=lax"),
        "cookie must be SameSite=Lax: {cookie}"
    );

    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE email_lower = $1")
        .bind(email)
        .fetch_one(&pool)
        .await
        .expect("count users");
    assert_eq!(count, 1);

    // Second login OTP against the same account.
    let (status, body, _) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/email/otp/start",
        json!({
            "purpose": "login",
            "email": email
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);

    let login_code = mailer
        .last_code_for(email)
        .expect("OTP must be captured for login");
    assert_ne!(login_code, code, "login OTP should be a fresh code");

    let (status, body, cookie) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/email/otp/verify",
        json!({
            "purpose": "login",
            "email": email,
            "code": login_code
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login verify body={body}");
    assert_eq!(body["username"], username);
    let cookie = cookie.expect("login must Set-Cookie");
    assert!(cookie.contains("corvin_session="));

    // Wrong code is rejected without setting a session.
    let (status, _, cookie) = json_request(
        corvin_server::app(state),
        "POST",
        "/api/v1/auth/email/otp/verify",
        json!({
            "purpose": "login",
            "email": email,
            "code": "000000"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(cookie.is_none());

    corvin_server::db::migrate_down(&pool)
        .await
        .expect("cleanup migrate_down");
}

#[tokio::test]
async fn email_otp_start_does_not_reveal_unknown_login_email() {
    let Some(url) = database_url() else {
        eprintln!("skip email_otp_start_does_not_reveal_unknown_login_email: DATABASE_URL unset");
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
    let state = corvin_server::AppState::new_for_tests(
        pool.clone(),
        mailer.clone(),
        "localhost",
        "http://localhost:3000",
        b"test-otp-pepper-not-for-prod!!!!",
    )
    .expect("AppState");

    let (status, body, _) = json_request(
        corvin_server::app(state),
        "POST",
        "/api/v1/auth/email/otp/start",
        json!({
            "purpose": "login",
            "email": "nobody@example.com"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert!(
        mailer.last_code_for("nobody@example.com").is_none(),
        "must not send OTP for unknown login email"
    );

    corvin_server::db::migrate_down(&pool)
        .await
        .expect("cleanup migrate_down");
}
