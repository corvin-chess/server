//! CV-134: session cookie hardening + CSRF security tests.

mod common;

use std::env;
use std::path::PathBuf;
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

#[test]
fn session_cookie_header_is_httponly_samesite_lax_path_root() {
    let id = [0xab; 32];
    let http = corvin_server::auth::session::session_cookie_header(&id, "http://localhost:3000");
    let http_s = http.to_str().expect("cookie ascii");
    let lower = http_s.to_ascii_lowercase();
    assert!(http_s.contains("corvin_session="));
    assert!(lower.contains("httponly"));
    assert!(lower.contains("samesite=lax"));
    assert!(lower.contains("path=/"));
    assert!(
        !lower.contains("secure"),
        "http origin must omit Secure: {http_s}"
    );

    let https =
        corvin_server::auth::session::session_cookie_header(&id, "https://play.corvin.example");
    let https_s = https.to_str().expect("cookie ascii");
    let https_l = https_s.to_ascii_lowercase();
    assert!(
        https_l.contains("secure"),
        "https origin must set Secure: {https_s}"
    );
    assert!(https_l.contains("httponly"));
    assert!(https_l.contains("samesite=lax"));
}

#[test]
fn clear_session_cookie_expires_immediately() {
    let cleared =
        corvin_server::auth::session::clear_session_cookie_header("http://localhost:3000");
    let value = cleared.to_str().expect("cookie ascii");
    let lower = value.to_ascii_lowercase();
    assert!(value.starts_with("corvin_session="));
    assert!(lower.contains("max-age=0") || lower.contains("expires="));
    assert!(lower.contains("httponly"));
    assert!(lower.contains("path=/"));
}

#[test]
fn csrf_header_name_is_stable() {
    assert_eq!(corvin_server::auth::session::CSRF_HEADER, "x-csrf-token");
}

#[test]
fn session_csrf_source_modules_exist() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("auth");
    assert!(src.join("session.rs").is_file());
    assert!(
        src.join("csrf.rs").is_file(),
        "expected src/auth/csrf.rs for CSRF helpers"
    );
}

async fn json_request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    csrf: Option<&str>,
) -> (StatusCode, Value, Option<String>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    if let Some(csrf) = csrf {
        builder = builder.header(corvin_server::auth::session::CSRF_HEADER, csrf);
    }
    let payload = body
        .map(|value| Body::from(serde_json::to_vec(&value).expect("body")))
        .unwrap_or_else(Body::empty);
    let response = app
        .oneshot(builder.body(payload).expect("request"))
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

fn cookie_pair(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .expect("cookie name=value")
        .trim()
        .to_owned()
}

#[tokio::test]
async fn session_csrf_logout_requires_matching_token() {
    let Some(url) = database_url() else {
        eprintln!("skip session_csrf_logout_requires_matching_token: DATABASE_URL unset");
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

    let email = "csrf-raven@example.com";
    let username = "CsrfRaven";

    let (status, body, _) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/email/otp/start",
        Some(json!({
            "purpose": "register",
            "email": email,
            "username": username,
            "locale": "en"
        })),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);

    let code = mailer
        .last_code_for(email)
        .expect("OTP must be captured for register");

    let (status, body, set_cookie) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/email/otp/verify",
        Some(json!({
            "purpose": "register",
            "email": email,
            "code": code
        })),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register verify body={body}");
    let csrf = body["csrf_token"]
        .as_str()
        .expect("register must return csrf_token")
        .to_owned();
    assert_eq!(csrf.len(), 64, "csrf_token must be 32-byte hex");
    let set_cookie = set_cookie.expect("register must Set-Cookie");
    let cookie = cookie_pair(&set_cookie);
    assert!(cookie.starts_with("corvin_session="));
    assert!(set_cookie.to_ascii_lowercase().contains("httponly"));
    assert!(set_cookie.to_ascii_lowercase().contains("samesite=lax"));

    let (status, session_body, _) = json_request(
        corvin_server::app(state.clone()),
        "GET",
        "/api/v1/auth/session",
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "session body={session_body}");
    assert_eq!(session_body["username"], username);
    assert_eq!(session_body["csrf_token"], csrf);

    let (status, err_body, _) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/logout",
        Some(json!({})),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "logout without CSRF must fail"
    );
    assert_eq!(err_body["error"], "forbidden");

    let (status, err_body, _) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/logout",
        Some(json!({})),
        Some(&cookie),
        Some("0000000000000000000000000000000000000000000000000000000000000000"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "logout with wrong CSRF must fail"
    );
    assert_eq!(err_body["error"], "forbidden");

    let (status, err_body, _) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/logout",
        Some(json!({})),
        None,
        Some(&csrf),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "logout without cookie must not oracle session validity"
    );
    assert_eq!(err_body["error"], "forbidden");

    let (status, body, clear_cookie) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/logout",
        Some(json!({})),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "logout body={body}");
    assert_eq!(body["ok"], true);
    let clear_cookie = clear_cookie.expect("logout must clear Set-Cookie");
    let clear_l = clear_cookie.to_ascii_lowercase();
    assert!(clear_l.contains("corvin_session="));
    assert!(clear_l.contains("max-age=0") || clear_l.contains("expires="));

    let (status, _, _) = json_request(
        corvin_server::app(state),
        "GET",
        "/api/v1/auth/session",
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "revoked session must not resolve"
    );

    corvin_server::db::migrate_down(&pool)
        .await
        .expect("cleanup migrate_down");
}

#[tokio::test]
async fn expired_session_is_rejected() {
    let Some(url) = database_url() else {
        eprintln!("skip expired_session_is_rejected: DATABASE_URL unset");
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

    let email = "expired-raven@example.com";
    let (status, _, _) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/email/otp/start",
        Some(json!({
            "purpose": "register",
            "email": email,
            "username": "ExpiredRaven",
            "locale": "en"
        })),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let code = mailer.last_code_for(email).expect("OTP");
    let (status, body, set_cookie) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/email/otp/verify",
        Some(json!({
            "purpose": "register",
            "email": email,
            "code": code
        })),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let cookie = cookie_pair(&set_cookie.expect("Set-Cookie"));
    let csrf = body["csrf_token"].as_str().expect("csrf_token").to_owned();

    // Keep expires_at > created_at (table check) while both sit in the past.
    sqlx::query(
        "UPDATE sessions
         SET created_at = now() - interval '2 minutes',
             expires_at = now() - interval '1 minute'",
    )
    .execute(&pool)
    .await
    .expect("expire sessions");

    let (status, _, _) = json_request(
        corvin_server::app(state.clone()),
        "GET",
        "/api/v1/auth/session",
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, err_body, _) = json_request(
        corvin_server::app(state),
        "POST",
        "/api/v1/auth/logout",
        Some(json!({})),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(err_body["error"], "forbidden");

    corvin_server::db::migrate_down(&pool)
        .await
        .expect("cleanup migrate_down");
}

#[tokio::test]
async fn session_endpoint_rejects_missing_cookie() {
    let Some(url) = database_url() else {
        eprintln!("skip session_endpoint_rejects_missing_cookie: DATABASE_URL unset");
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
        mailer,
        "localhost",
        "http://localhost:3000",
        b"test-otp-pepper-not-for-prod!!!!",
    )
    .expect("AppState");

    let (status, body, _) = json_request(
        corvin_server::app(state),
        "GET",
        "/api/v1/auth/session",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthorized");

    corvin_server::db::migrate_down(&pool)
        .await
        .expect("cleanup migrate_down");
}
