//! Opaque session minting and cookie headers (CV-133).
//! Full CSRF binding and cookie hardening matrix remain CV-134.

use axum::http::{header, HeaderValue};
use chrono::{Duration, Utc};
use rand::RngCore;
use sqlx::PgPool;

pub const SESSION_COOKIE: &str = "corvin_session";
const SESSION_TTL_DAYS: i64 = 30;

/// Create a durable opaque session row and return the raw 32-byte id.
pub async fn create_session(pool: &PgPool, user_id: &str) -> Result<[u8; 32], sqlx::Error> {
    let mut id = [0u8; 32];
    let mut csrf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut id);
    rand::thread_rng().fill_bytes(&mut csrf);
    let expires_at = Utc::now() + Duration::days(SESSION_TTL_DAYS);

    sqlx::query(
        "INSERT INTO sessions (id, user_id, expires_at, csrf_secret)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(&id[..])
    .bind(user_id)
    .bind(expires_at)
    .bind(&csrf[..])
    .execute(pool)
    .await?;

    Ok(id)
}

/// Build a `Set-Cookie` header for the opaque session id.
/// `Secure` is omitted on plain http:// localhost so SoftPasskey/browser tests work;
/// production origins (https) get `Secure`.
pub fn session_cookie_header(session_id: &[u8; 32], rp_origin: &str) -> HeaderValue {
    let token = hex::encode(session_id);
    let secure = if rp_origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    let value = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        SESSION_TTL_DAYS * 24 * 60 * 60
    );
    HeaderValue::from_str(&value).expect("session cookie is ASCII")
}

pub fn set_session_cookie(
    builder: axum::http::response::Builder,
    session_id: &[u8; 32],
    rp_origin: &str,
) -> axum::http::response::Builder {
    builder.header(
        header::SET_COOKIE,
        session_cookie_header(session_id, rp_origin),
    )
}
