//! Email OTP register/login (CV-133).

use axum::extract::State;
use axum::response::Response;
use axum::Json;
use chrono::{Duration, Utc};
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use super::session::{auth_success_response, create_session};
use super::AuthError;
use crate::safety::default_messaging_prefs;
use crate::AppState;

const OTP_TTL_MINUTES: i64 = 10;
const OTP_MAX_ATTEMPTS: i32 = 5;

#[derive(Debug, Deserialize)]
pub struct OtpStartRequest {
    pub purpose: String,
    pub email: String,
    pub username: Option<String>,
    pub birth_year: Option<i16>,
    pub locale: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OtpStartResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize)]
pub struct OtpVerifyRequest {
    pub purpose: String,
    pub email: String,
    pub code: String,
}

pub async fn otp_start(
    State(state): State<AppState>,
    Json(body): Json<OtpStartRequest>,
) -> Result<Json<OtpStartResponse>, AuthError> {
    let purpose = normalize_purpose(&body.purpose)?;
    let email = normalize_email(&body.email)?;

    // Uniform success body avoids email enumeration on login.
    let ok = Json(OtpStartResponse { ok: true });

    if purpose == "login" {
        let exists: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM users
             WHERE email_lower = $1 AND deleted_at IS NULL
             LIMIT 1",
        )
        .bind(&email)
        .fetch_optional(&state.db)
        .await?;
        if exists.is_none() {
            return Ok(ok);
        }
        issue_otp(&state, &email, purpose, None, None, "en").await?;
        return Ok(ok);
    }

    // register
    let username = body
        .username
        .as_deref()
        .map(normalize_username)
        .transpose()?
        .ok_or(AuthError::BadRequest("username required for register"))?;
    let locale = body.locale.as_deref().unwrap_or("en").trim().to_owned();
    if !(2..=16).contains(&locale.len()) {
        return Err(AuthError::BadRequest("invalid locale"));
    }
    if let Some(year) = body.birth_year {
        if !(1900..=2100).contains(&year) {
            return Err(AuthError::BadRequest("invalid birth_year"));
        }
    }

    let taken: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM users
         WHERE deleted_at IS NULL
           AND (username_lower = $1 OR email_lower = $2)
         LIMIT 1",
    )
    .bind(username.to_ascii_lowercase())
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;
    if taken.is_some() {
        // Still return ok; verification will fail later if they guess a code.
        // Do not send OTP for colliding identities.
        return Ok(ok);
    }

    issue_otp(
        &state,
        &email,
        purpose,
        Some(&username),
        body.birth_year,
        &locale,
    )
    .await?;
    Ok(ok)
}

pub async fn otp_verify(
    State(state): State<AppState>,
    Json(body): Json<OtpVerifyRequest>,
) -> Result<Response, AuthError> {
    let purpose = normalize_purpose(&body.purpose)?;
    let email = normalize_email(&body.email)?;
    let code = body.code.trim();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return Err(AuthError::Unauthorized);
    }

    type OtpRow = (Vec<u8>, Vec<u8>, Option<String>, Option<i16>, String);
    let row: Option<OtpRow> = sqlx::query_as(
        "SELECT id, code_hash, username, birth_year, locale
         FROM email_otps
         WHERE email_lower = $1
           AND purpose = $2
           AND consumed_at IS NULL
           AND expires_at > now()
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(&email)
    .bind(purpose)
    .fetch_optional(&state.db)
    .await?;

    let Some((otp_id, code_hash, username, birth_year, locale)) = row else {
        return Err(AuthError::Unauthorized);
    };

    let attempts: (i32,) = sqlx::query_as("SELECT attempts FROM email_otps WHERE id = $1")
        .bind(&otp_id)
        .fetch_one(&state.db)
        .await?;
    if attempts.0 >= OTP_MAX_ATTEMPTS {
        return Err(AuthError::Unauthorized);
    }

    let expected = hash_otp_code(&state.otp_pepper, &email, code);
    if !constant_time_eq(&expected, &code_hash) {
        sqlx::query("UPDATE email_otps SET attempts = attempts + 1 WHERE id = $1")
            .bind(&otp_id)
            .execute(&state.db)
            .await?;
        return Err(AuthError::Unauthorized);
    }

    sqlx::query("UPDATE email_otps SET consumed_at = now() WHERE id = $1")
        .bind(&otp_id)
        .execute(&state.db)
        .await?;

    let (user_id, username, dm_enabled, public_chat_enabled) = if purpose == "register" {
        let username = username.ok_or(AuthError::BadRequest("missing username on OTP"))?;
        let user_id = Ulid::new().to_string();
        let prefs = default_messaging_prefs(birth_year, Utc::now().date_naive());
        sqlx::query(
            "INSERT INTO users (
                id, username, username_lower, email, email_lower, birth_year, locale,
                dm_enabled, public_chat_enabled
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&user_id)
        .bind(&username)
        .bind(username.to_ascii_lowercase())
        .bind(&email)
        .bind(&email)
        .bind(birth_year)
        .bind(&locale)
        .bind(prefs.dm_enabled)
        .bind(prefs.public_chat_enabled)
        .execute(&state.db)
        .await
        .map_err(|err| {
            if is_unique_violation(&err) {
                AuthError::Conflict("registration unavailable")
            } else {
                AuthError::from(err)
            }
        })?;
        (
            user_id,
            username,
            prefs.dm_enabled,
            prefs.public_chat_enabled,
        )
    } else {
        let row: Option<(String, String, bool, bool)> = sqlx::query_as(
            "SELECT id, username, dm_enabled, public_chat_enabled FROM users
             WHERE email_lower = $1 AND deleted_at IS NULL
             LIMIT 1",
        )
        .bind(&email)
        .fetch_optional(&state.db)
        .await?;
        let Some((user_id, username, dm_enabled, public_chat_enabled)) = row else {
            return Err(AuthError::Unauthorized);
        };
        (user_id, username, dm_enabled, public_chat_enabled)
    };

    let tokens = create_session(&state.db, &user_id).await?;
    Ok(auth_success_response(
        &state.rp_origin,
        user_id,
        username,
        tokens,
        dm_enabled,
        public_chat_enabled,
    ))
}

async fn issue_otp(
    state: &AppState,
    email: &str,
    purpose: &str,
    username: Option<&str>,
    birth_year: Option<i16>,
    locale: &str,
) -> Result<(), AuthError> {
    if !state.otp_rate_limit.allow(email) {
        // Still look like success to callers; do not send mail.
        return Ok(());
    }

    // Collapse prior unconsumed challenges so attempt budgets cannot reset forever.
    sqlx::query(
        "UPDATE email_otps
         SET consumed_at = now()
         WHERE email_lower = $1
           AND purpose = $2
           AND consumed_at IS NULL",
    )
    .bind(email)
    .bind(purpose)
    .execute(&state.db)
    .await?;

    let mut id = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut id);
    let code = format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32));
    let code_hash = hash_otp_code(&state.otp_pepper, email, &code);
    let expires_at = Utc::now() + Duration::minutes(OTP_TTL_MINUTES);

    sqlx::query(
        "INSERT INTO email_otps
            (id, email_lower, purpose, code_hash, username, birth_year, locale, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&id[..])
    .bind(email)
    .bind(purpose)
    .bind(&code_hash[..])
    .bind(username)
    .bind(birth_year)
    .bind(locale)
    .bind(expires_at)
    .execute(&state.db)
    .await?;

    // Mail failure must not change the uniform success response (enumeration + UX).
    if let Err(err) = state.mailer.send_otp(email, &code) {
        eprintln!("corvin-server: OTP mail failed for {email}: {err}");
    }
    Ok(())
}

fn hash_otp_code(pepper: &[u8], email: &str, code: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(pepper);
    hasher.update(b"|");
    hasher.update(email.as_bytes());
    hasher.update(b"|");
    hasher.update(code.as_bytes());
    hasher.finalize().into()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn normalize_purpose(raw: &str) -> Result<&'static str, AuthError> {
    match raw.trim() {
        "register" => Ok("register"),
        "login" => Ok("login"),
        _ => Err(AuthError::BadRequest("purpose must be register or login")),
    }
}

fn normalize_email(raw: &str) -> Result<String, AuthError> {
    let email = raw.trim().to_ascii_lowercase();
    if email.len() < 5 || email.len() > 254 || !email.contains('@') || email.contains(' ') {
        return Err(AuthError::BadRequest("invalid email"));
    }
    let (local, domain) = email
        .split_once('@')
        .ok_or(AuthError::BadRequest("invalid email"))?;
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(AuthError::BadRequest("invalid email"));
    }
    Ok(email)
}

fn normalize_username(raw: &str) -> Result<String, AuthError> {
    let username = raw.trim();
    if !(1..=32).contains(&username.len()) {
        return Err(AuthError::BadRequest("username length"));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AuthError::BadRequest("username charset"));
    }
    Ok(username.to_owned())
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db) => db.code().as_deref() == Some("23505"),
        _ => false,
    }
}
