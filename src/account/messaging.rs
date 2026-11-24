//! Messaging preference read/update (CV-140).

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::auth::session::{csrf_matches, lookup_session, parse_session_id_from_cookie};
use crate::auth::AuthError;
use crate::safety::{can_enable_messaging, MessagingPrefs};
use crate::AppState;

fn session_id_from_headers(headers: &HeaderMap) -> Option<[u8; 32]> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    parse_session_id_from_cookie(cookie)
}

fn csrf_from_headers(headers: &HeaderMap) -> Option<&str> {
    let name = HeaderName::from_static("x-csrf-token");
    headers.get(&name).and_then(|value| value.to_str().ok())
}

#[derive(Debug, Serialize)]
pub struct MessagingResponse {
    pub dm_enabled: bool,
    pub public_chat_enabled: bool,
    pub can_enable_messaging: bool,
}

#[derive(Debug, Deserialize)]
pub struct MessagingPatch {
    pub dm_enabled: Option<bool>,
    pub public_chat_enabled: Option<bool>,
}

async fn load_prefs(
    pool: &PgPool,
    user_id: &str,
) -> Result<(MessagingPrefs, Option<i16>), AuthError> {
    let row: Option<(bool, bool, Option<i16>)> = sqlx::query_as(
        "SELECT dm_enabled, public_chat_enabled, birth_year
         FROM users
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    let Some((dm_enabled, public_chat_enabled, birth_year)) = row else {
        return Err(AuthError::Unauthorized);
    };
    Ok((
        MessagingPrefs {
            dm_enabled,
            public_chat_enabled,
        },
        birth_year,
    ))
}

/// `GET /api/v1/account/messaging`
pub async fn get_messaging(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MessagingResponse>, AuthError> {
    let session_id = session_id_from_headers(&headers).ok_or(AuthError::Unauthorized)?;
    let session = lookup_session(&state.db, &session_id)
        .await?
        .ok_or(AuthError::Unauthorized)?;
    let (prefs, birth_year) = load_prefs(&state.db, &session.user_id).await?;
    let today = Utc::now().date_naive();
    Ok(Json(MessagingResponse {
        dm_enabled: prefs.dm_enabled,
        public_chat_enabled: prefs.public_chat_enabled,
        can_enable_messaging: can_enable_messaging(birth_year, today),
    }))
}

/// `PATCH /api/v1/account/messaging` — CSRF required; minors cannot enable flags.
pub async fn patch_messaging(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MessagingPatch>,
) -> Result<Json<MessagingResponse>, AuthError> {
    let deny = || AuthError::Forbidden("messaging denied");
    let session_id = session_id_from_headers(&headers).ok_or_else(deny)?;
    let session = lookup_session(&state.db, &session_id)
        .await?
        .ok_or_else(deny)?;
    let csrf_header = csrf_from_headers(&headers).ok_or_else(deny)?;
    if !csrf_matches(csrf_header, &session.csrf) {
        return Err(deny());
    }

    // After CSRF validation, keep failure modes Forbidden so clients cannot
    // distinguish empty body / deleted user from CSRF denial (same as logout).
    if body.dm_enabled.is_none() && body.public_chat_enabled.is_none() {
        return Err(deny());
    }

    let (current, birth_year) = match load_prefs(&state.db, &session.user_id).await {
        Ok(row) => row,
        Err(_) => return Err(deny()),
    };
    let today = Utc::now().date_naive();
    let adult = can_enable_messaging(birth_year, today);

    let mut next = current;
    if let Some(want_dm) = body.dm_enabled {
        if want_dm && !adult {
            return Err(deny());
        }
        next.dm_enabled = want_dm;
    }
    if let Some(want_chat) = body.public_chat_enabled {
        if want_chat && !adult {
            return Err(deny());
        }
        next.public_chat_enabled = want_chat;
    }

    sqlx::query(
        "UPDATE users
         SET dm_enabled = $2,
             public_chat_enabled = $3,
             updated_at = now()
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(&session.user_id)
    .bind(next.dm_enabled)
    .bind(next.public_chat_enabled)
    .execute(&state.db)
    .await?;

    Ok(Json(MessagingResponse {
        dm_enabled: next.dm_enabled,
        public_chat_enabled: next.public_chat_enabled,
        can_enable_messaging: adult,
    }))
}
