//! Opaque session minting, cookie headers, and CSRF-bound logout (CV-133/CV-134).

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{Duration, Utc};
use rand::RngCore;
use serde::Serialize;
use sqlx::PgPool;

use super::AuthError;
use crate::AppState;

pub const SESSION_COOKIE: &str = "corvin_session";
/// Clients must send this header (any casing) on cookie-authenticated mutations.
pub const CSRF_HEADER: &str = "x-csrf-token";
const SESSION_TTL_DAYS: i64 = 30;

/// Fresh opaque session id + synchronizer CSRF secret (both 32 random bytes).
#[derive(Debug, Clone, Copy)]
pub struct SessionTokens {
    pub id: [u8; 32],
    pub csrf: [u8; 32],
}

/// Shared auth success body (login/register + session probe).
#[derive(Debug, Serialize)]
pub struct AuthSuccess {
    pub user_id: String,
    pub username: String,
    pub csrf_token: String,
}

/// Create a durable opaque session row and return id + CSRF secret.
/// Prior active sessions for the same user are revoked so a stolen cookie
/// does not survive a fresh login.
pub async fn create_session(pool: &PgPool, user_id: &str) -> Result<SessionTokens, sqlx::Error> {
    let mut id = [0u8; 32];
    let mut csrf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut id);
    rand::thread_rng().fill_bytes(&mut csrf);
    let expires_at = Utc::now() + Duration::days(SESSION_TTL_DAYS);

    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE sessions SET revoked_at = now()
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO sessions (id, user_id, expires_at, csrf_secret)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(&id[..])
    .bind(user_id)
    .bind(expires_at)
    .bind(&csrf[..])
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(SessionTokens { id, csrf })
}

/// Active session row resolved from the opaque cookie.
#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub id: [u8; 32],
    pub user_id: String,
    pub username: String,
    pub csrf: [u8; 32],
}

/// Look up a non-expired, non-revoked session by raw id bytes.
pub async fn lookup_session(
    pool: &PgPool,
    session_id: &[u8; 32],
) -> Result<Option<ActiveSession>, sqlx::Error> {
    let row: Option<(Vec<u8>, String, String, Vec<u8>)> = sqlx::query_as(
        "SELECT s.id, u.id, u.username, s.csrf_secret
         FROM sessions s
         INNER JOIN users u ON u.id = s.user_id
         WHERE s.id = $1
           AND s.revoked_at IS NULL
           AND s.expires_at > now()
           AND u.deleted_at IS NULL",
    )
    .bind(&session_id[..])
    .fetch_optional(pool)
    .await?;

    Ok(row.and_then(|(id_bytes, user_id, username, csrf_bytes)| {
        let id: [u8; 32] = id_bytes.try_into().ok()?;
        let csrf: [u8; 32] = csrf_bytes.try_into().ok()?;
        Some(ActiveSession {
            id,
            user_id,
            username,
            csrf,
        })
    }))
}

/// Mark the session revoked so the cookie can no longer authenticate.
pub async fn revoke_session(pool: &PgPool, session_id: &[u8; 32]) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE sessions SET revoked_at = now()
         WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(&session_id[..])
    .execute(pool)
    .await?;
    Ok(())
}

/// Build a `Set-Cookie` header for the opaque session id.
/// `Secure` is omitted on plain http:// localhost so SoftPasskey/browser tests work;
/// production origins (https) get `Secure`.
pub fn session_cookie_header(session_id: &[u8; 32], rp_origin: &str) -> HeaderValue {
    let token = hex::encode(session_id);
    let secure = secure_attr(rp_origin);
    let value = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        SESSION_TTL_DAYS * 24 * 60 * 60
    );
    HeaderValue::from_str(&value).expect("session cookie is ASCII")
}

/// Clear the session cookie (logout).
pub fn clear_session_cookie_header(rp_origin: &str) -> HeaderValue {
    let secure = secure_attr(rp_origin);
    let value = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}");
    HeaderValue::from_str(&value).expect("clear cookie is ASCII")
}

fn secure_attr(rp_origin: &str) -> &'static str {
    if rp_origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    }
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

pub fn clear_session_cookie(
    builder: axum::http::response::Builder,
    rp_origin: &str,
) -> axum::http::response::Builder {
    builder.header(header::SET_COOKIE, clear_session_cookie_header(rp_origin))
}

/// Hex-encode the CSRF secret for clients (synchronizer token).
pub fn csrf_token_hex(csrf: &[u8; 32]) -> String {
    hex::encode(csrf)
}

/// Constant-time compare of a hex CSRF header to the session secret.
pub fn csrf_matches(header_hex: &str, csrf: &[u8; 32]) -> bool {
    let Ok(provided) = hex::decode(header_hex.trim()) else {
        return false;
    };
    if provided.len() != csrf.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in provided.iter().zip(csrf.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Parse `corvin_session=<hex>` from a Cookie header value.
pub fn parse_session_id_from_cookie(cookie_header: &str) -> Option<[u8; 32]> {
    for part in cookie_header.split(';') {
        let part = part.trim();
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        if name != SESSION_COOKIE {
            continue;
        }
        let Ok(bytes) = hex::decode(value.trim()) else {
            return None;
        };
        return <[u8; 32]>::try_from(bytes.as_slice()).ok();
    }
    None
}

fn session_id_from_headers(headers: &HeaderMap) -> Option<[u8; 32]> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    parse_session_id_from_cookie(cookie)
}

fn csrf_from_headers(headers: &HeaderMap) -> Option<&str> {
    let name = HeaderName::from_static(CSRF_HEADER);
    headers.get(&name).and_then(|value| value.to_str().ok())
}

/// `GET /api/v1/auth/session` — resolve cookie to user + CSRF (no CSRF required).
pub async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AuthSuccess>, AuthError> {
    let session_id = session_id_from_headers(&headers).ok_or(AuthError::Unauthorized)?;
    let session = lookup_session(&state.db, &session_id)
        .await?
        .ok_or(AuthError::Unauthorized)?;
    Ok(Json(AuthSuccess {
        user_id: session.user_id,
        username: session.username,
        csrf_token: csrf_token_hex(&session.csrf),
    }))
}

#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    pub ok: bool,
}

/// `POST /api/v1/auth/logout` — requires matching `X-CSRF-Token`.
/// All failure modes return Forbidden so cookie validity is not an oracle.
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AuthError> {
    let deny = || AuthError::Forbidden("logout denied");
    let session_id = session_id_from_headers(&headers).ok_or_else(deny)?;
    let session = lookup_session(&state.db, &session_id)
        .await?
        .ok_or_else(deny)?;

    let csrf_header = csrf_from_headers(&headers).ok_or_else(deny)?;
    if !csrf_matches(csrf_header, &session.csrf) {
        return Err(deny());
    }

    revoke_session(&state.db, &session.id).await?;

    let body = LogoutResponse { ok: true };
    let builder = Response::builder().status(StatusCode::OK);
    let builder = clear_session_cookie(builder, &state.rp_origin);
    Ok(builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(
            serde_json::to_vec(&body).expect("LogoutResponse serializes"),
        ))
        .expect("response"))
}

/// Build the standard login/register success response with session cookie + CSRF.
pub fn auth_success_response(
    rp_origin: &str,
    user_id: String,
    username: String,
    tokens: SessionTokens,
) -> Response {
    let body = AuthSuccess {
        user_id,
        username,
        csrf_token: csrf_token_hex(&tokens.csrf),
    };
    let builder = Response::builder().status(StatusCode::OK);
    let builder = set_session_cookie(builder, &tokens.id, rp_origin);
    builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(
            serde_json::to_vec(&body).expect("AuthSuccess serializes"),
        ))
        .expect("response")
}
