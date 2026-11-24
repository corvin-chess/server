//! CV-140: age-appropriate messaging defaults (minors cannot DM by default).

mod common;

use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::{Datelike, NaiveDate};
use corvin_server::safety::{
    assert_can_send_dm, default_messaging_prefs, is_minor_for_messaging, MessagingPrefs,
    MESSAGING_ADULT_AGE,
};
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
        builder = builder.header("x-csrf-token", csrf);
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

fn session_cookie_value(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .expect("cookie pair")
        .trim()
        .to_owned()
}

#[test]
fn age_defaults_migration_sql_files_are_present_and_paired() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let up = root.join("20261124150000_age_defaults.up.sql");
    let down = root.join("20261124150000_age_defaults.down.sql");
    assert!(up.is_file(), "missing {}", up.display());
    assert!(down.is_file(), "missing {}", down.display());

    let up_sql = std::fs::read_to_string(&up).expect("read up.sql");
    let down_sql = std::fs::read_to_string(&down).expect("read down.sql");
    let up_l = up_sql.to_lowercase();
    let down_l = down_sql.to_lowercase();
    assert!(
        up_l.contains("dm_enabled") && up_l.contains("public_chat_enabled"),
        "up migration must add messaging flags"
    );
    assert!(
        down_l.contains("dm_enabled") && down_l.contains("public_chat_enabled"),
        "down migration must drop messaging flags"
    );
    assert!(
        !up_l.contains("threshold") && !up_l.contains("fair_play"),
        "migration must not embed fair-play secrets"
    );
}

#[test]
fn safety_module_exists() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/safety/mod.rs");
    assert!(path.is_file(), "missing {}", path.display());
}

#[test]
fn missing_birth_year_is_treated_as_minor() {
    let today = NaiveDate::from_ymd_opt(2026, 11, 24).expect("date");
    assert!(is_minor_for_messaging(None, today));
    let prefs = default_messaging_prefs(None, today);
    assert!(!prefs.dm_enabled);
    assert!(!prefs.public_chat_enabled);
    assert!(assert_can_send_dm(&prefs).is_err());
}

#[test]
fn under_adult_age_cannot_dm_by_default() {
    let today = NaiveDate::from_ymd_opt(2026, 11, 24).expect("date");
    let birth_year = (2026 - (MESSAGING_ADULT_AGE as i32) + 1) as i16;
    assert!(is_minor_for_messaging(Some(birth_year), today));
    let prefs = default_messaging_prefs(Some(birth_year), today);
    assert_eq!(
        prefs,
        MessagingPrefs {
            dm_enabled: false,
            public_chat_enabled: false,
        }
    );
    assert!(assert_can_send_dm(&prefs).is_err());
}

#[test]
fn adult_defaults_allow_dm() {
    let today = NaiveDate::from_ymd_opt(2026, 11, 24).expect("date");
    let birth_year = (2026 - (MESSAGING_ADULT_AGE as i32)) as i16;
    assert!(!is_minor_for_messaging(Some(birth_year), today));
    let prefs = default_messaging_prefs(Some(birth_year), today);
    assert!(prefs.dm_enabled);
    assert!(prefs.public_chat_enabled);
    assert!(assert_can_send_dm(&prefs).is_ok());
}

#[tokio::test]
async fn register_minor_and_unknown_birth_year_disable_dm() {
    let Some(url) = database_url() else {
        eprintln!("skip register_minor_and_unknown_birth_year_disable_dm: DATABASE_URL unset");
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

    let today_year = chrono::Utc::now().date_naive().year();
    let minor_year = (today_year - (MESSAGING_ADULT_AGE as i32) + 1) as i16;

    // Minor with birth_year.
    let (status, body, cookie) = register_via_otp(
        &state,
        mailer.as_ref(),
        "minor@example.com",
        "MinorRaven",
        Some(minor_year),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "minor register body={body}");
    assert_eq!(body["dm_enabled"], false);
    assert_eq!(body["public_chat_enabled"], false);
    let cookie = cookie.expect("cookie");
    let csrf = body["csrf_token"].as_str().expect("csrf").to_owned();

    let (dm, chat): (bool, bool) =
        sqlx::query_as("SELECT dm_enabled, public_chat_enabled FROM users WHERE email_lower = $1")
            .bind("minor@example.com")
            .fetch_one(&pool)
            .await
            .expect("flags");
    assert!(!dm);
    assert!(!chat);

    // Minor cannot enable DMs.
    let (status, body, _) = json_request(
        corvin_server::app(state.clone()),
        "PATCH",
        "/api/v1/account/messaging",
        Some(json!({ "dm_enabled": true })),
        Some(&session_cookie_value(&cookie)),
        Some(&csrf),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "minor enable body={body}");
    assert_eq!(body["error"], "forbidden");

    let (status, body, _) = json_request(
        corvin_server::app(state.clone()),
        "GET",
        "/api/v1/account/messaging",
        None,
        Some(&session_cookie_value(&cookie)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["dm_enabled"], false);
    assert_eq!(body["public_chat_enabled"], false);
    assert_eq!(body["can_enable_messaging"], false);

    // Missing birth_year also disables.
    let (status, body, _) = register_via_otp(
        &state,
        mailer.as_ref(),
        "unknown@example.com",
        "UnknownAge",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unknown register body={body}");
    assert_eq!(body["dm_enabled"], false);
    assert_eq!(body["public_chat_enabled"], false);

    corvin_server::db::migrate_down(&pool)
        .await
        .expect("cleanup migrate_down");
}

#[tokio::test]
async fn register_adult_can_toggle_messaging() {
    let Some(url) = database_url() else {
        eprintln!("skip register_adult_can_toggle_messaging: DATABASE_URL unset");
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

    let today_year = chrono::Utc::now().date_naive().year();
    let adult_year = (today_year - (MESSAGING_ADULT_AGE as i32)) as i16;

    let (status, body, cookie) = register_via_otp(
        &state,
        mailer.as_ref(),
        "adult@example.com",
        "AdultRaven",
        Some(adult_year),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "adult register body={body}");
    assert_eq!(body["dm_enabled"], true);
    assert_eq!(body["public_chat_enabled"], true);
    assert!(assert_can_send_dm(&MessagingPrefs {
        dm_enabled: true,
        public_chat_enabled: true,
    })
    .is_ok());

    let cookie = cookie.expect("cookie");
    let csrf = body["csrf_token"].as_str().expect("csrf").to_owned();

    let (status, body, _) = json_request(
        corvin_server::app(state.clone()),
        "PATCH",
        "/api/v1/account/messaging",
        Some(json!({ "dm_enabled": false, "public_chat_enabled": false })),
        Some(&session_cookie_value(&cookie)),
        Some(&csrf),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "adult disable body={body}");
    assert_eq!(body["dm_enabled"], false);
    assert_eq!(body["public_chat_enabled"], false);

    let (status, body, _) = json_request(
        corvin_server::app(state.clone()),
        "PATCH",
        "/api/v1/account/messaging",
        Some(json!({ "dm_enabled": true })),
        Some(&session_cookie_value(&cookie)),
        Some(&csrf),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "adult re-enable body={body}");
    assert_eq!(body["dm_enabled"], true);

    // CSRF required on PATCH.
    let (status, _, _) = json_request(
        corvin_server::app(state),
        "PATCH",
        "/api/v1/account/messaging",
        Some(json!({ "dm_enabled": false })),
        Some(&session_cookie_value(&cookie)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    corvin_server::db::migrate_down(&pool)
        .await
        .expect("cleanup migrate_down");
}

async fn register_via_otp(
    state: &corvin_server::AppState,
    mailer: &corvin_server::auth::mailer::CaptureMailer,
    email: &str,
    username: &str,
    birth_year: Option<i16>,
) -> (StatusCode, Value, Option<String>) {
    let mut start = json!({
        "purpose": "register",
        "email": email,
        "username": username,
        "locale": "en"
    });
    if let Some(year) = birth_year {
        start["birth_year"] = json!(year);
    }
    let (status, body, _) = json_request(
        corvin_server::app(state.clone()),
        "POST",
        "/api/v1/auth/email/otp/start",
        Some(start),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "otp start body={body}");

    let code = mailer.last_code_for(email).expect("OTP captured");
    json_request(
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
    .await
}
