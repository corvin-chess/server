//! WebAuthn passkey register/login (CV-133).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use uuid::Uuid;
use webauthn_rs::prelude::{
    Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential,
};

use super::email_otp::AuthSuccess;
use super::session::{create_session, set_session_cookie};
use super::AuthError;
use crate::AppState;

const CHALLENGE_TTL: Duration = Duration::from_secs(300);

pub struct PasskeyChallenges {
    inner: Mutex<HashMap<String, PendingChallenge>>,
}

enum PendingChallenge {
    Register {
        username: String,
        email: Option<String>,
        locale: String,
        birth_year: Option<i16>,
        state: PasskeyRegistration,
        expires: Instant,
    },
    Login {
        user_id: String,
        username: String,
        state: PasskeyAuthentication,
        passkeys: Vec<Passkey>,
        expires: Instant,
    },
}

impl PasskeyChallenges {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn insert(&self, id: String, pending: PendingChallenge) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.retain(|_, value| !value.expired());
        guard.insert(id, pending);
    }

    fn take(&self, id: &str) -> Option<PendingChallenge> {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.retain(|_, value| !value.expired());
        guard.remove(id)
    }
}

impl PendingChallenge {
    fn expired(&self) -> bool {
        let expires = match self {
            Self::Register { expires, .. } | Self::Login { expires, .. } => *expires,
        };
        Instant::now() > expires
    }
}

impl Default for PasskeyChallenges {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
pub struct RegisterStartRequest {
    pub username: String,
    pub email: Option<String>,
    pub birth_year: Option<i16>,
    pub locale: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChallengeStartResponse {
    pub challenge_id: String,
    #[serde(rename = "publicKey")]
    pub public_key: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct RegisterFinishRequest {
    pub challenge_id: String,
    pub credential: RegisterPublicKeyCredential,
}

#[derive(Debug, Deserialize)]
pub struct LoginStartRequest {
    pub username: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginFinishRequest {
    pub challenge_id: String,
    pub credential: PublicKeyCredential,
}

pub async fn register_start(
    State(state): State<AppState>,
    Json(body): Json<RegisterStartRequest>,
) -> Result<Json<ChallengeStartResponse>, AuthError> {
    let username = normalize_username(&body.username)?;
    let locale = body.locale.as_deref().unwrap_or("en").trim().to_owned();
    if !(2..=16).contains(&locale.len()) {
        return Err(AuthError::BadRequest("invalid locale"));
    }
    if let Some(year) = body.birth_year {
        if !(1900..=2100).contains(&year) {
            return Err(AuthError::BadRequest("invalid birth_year"));
        }
    }
    let email = match body.email.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(normalize_email(raw)?),
        _ => None,
    };

    let taken: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM users
         WHERE deleted_at IS NULL AND username_lower = $1
         LIMIT 1",
    )
    .bind(username.to_ascii_lowercase())
    .fetch_optional(&state.db)
    .await?;
    if taken.is_some() {
        // Generic conflict: do not say which field collided.
        return Err(AuthError::Conflict("registration unavailable"));
    }
    if let Some(ref email) = email {
        let email_taken: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM users
             WHERE deleted_at IS NULL AND email_lower = $1
             LIMIT 1",
        )
        .bind(email)
        .fetch_optional(&state.db)
        .await?;
        if email_taken.is_some() {
            return Err(AuthError::Conflict("registration unavailable"));
        }
    }

    // Provisional user handle for the ceremony; durable ULID is written on finish.
    let user_uuid = Uuid::from_bytes(Ulid::new().to_bytes());
    let (ccr, reg_state) = state
        .webauthn
        .start_passkey_registration(user_uuid, &username, &username, None)
        .map_err(|err| AuthError::Webauthn(err.to_string()))?;

    let challenge_id = new_challenge_id();
    state.passkey_challenges.insert(
        challenge_id.clone(),
        PendingChallenge::Register {
            username,
            email,
            locale,
            birth_year: body.birth_year,
            state: reg_state,
            expires: Instant::now() + CHALLENGE_TTL,
        },
    );

    Ok(Json(ChallengeStartResponse {
        challenge_id,
        public_key: serde_json::to_value(ccr)
            .map_err(|err| AuthError::Internal(err.to_string()))?,
    }))
}

pub async fn register_finish(
    State(state): State<AppState>,
    Json(body): Json<RegisterFinishRequest>,
) -> Result<Response, AuthError> {
    let pending = state
        .passkey_challenges
        .take(&body.challenge_id)
        .ok_or(AuthError::Unauthorized)?;
    let PendingChallenge::Register {
        username,
        email,
        locale,
        birth_year,
        state: reg_state,
        ..
    } = pending
    else {
        return Err(AuthError::Unauthorized);
    };

    let passkey = state
        .webauthn
        .finish_passkey_registration(&body.credential, &reg_state)
        .map_err(|err| AuthError::Webauthn(err.to_string()))?;

    let user_id = Ulid::new().to_string();
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO users (id, username, username_lower, email, email_lower, birth_year, locale)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(&user_id)
    .bind(&username)
    .bind(username.to_ascii_lowercase())
    .bind(&email)
    .bind(&email)
    .bind(birth_year)
    .bind(&locale)
    .execute(&mut *tx)
    .await
    .map_err(|err| {
        if is_unique_violation(&err) {
            AuthError::Conflict("registration unavailable")
        } else {
            AuthError::from(err)
        }
    })?;

    let cred_id = passkey.cred_id().to_vec();
    let passkey_json =
        serde_json::to_value(&passkey).map_err(|err| AuthError::Internal(err.to_string()))?;
    sqlx::query(
        "INSERT INTO passkeys (credential_id, user_id, passkey_json)
         VALUES ($1, $2, $3)",
    )
    .bind(&cred_id)
    .bind(&user_id)
    .bind(passkey_json)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let session_id = create_session(&state.db, &user_id).await?;
    auth_success_response(&state, user_id, username, session_id)
}

pub async fn login_start(
    State(state): State<AppState>,
    Json(body): Json<LoginStartRequest>,
) -> Result<Json<ChallengeStartResponse>, AuthError> {
    let username = normalize_username(&body.username)?;
    let user: Option<(String, String)> = sqlx::query_as(
        "SELECT id, username FROM users
         WHERE username_lower = $1 AND deleted_at IS NULL
         LIMIT 1",
    )
    .bind(username.to_ascii_lowercase())
    .fetch_optional(&state.db)
    .await?;
    let Some((user_id, username)) = user else {
        return Err(AuthError::Unauthorized);
    };

    let rows: Vec<(serde_json::Value,)> =
        sqlx::query_as("SELECT passkey_json FROM passkeys WHERE user_id = $1")
            .bind(&user_id)
            .fetch_all(&state.db)
            .await?;
    if rows.is_empty() {
        return Err(AuthError::Unauthorized);
    }

    let mut passkeys = Vec::with_capacity(rows.len());
    for (value,) in rows {
        let passkey: Passkey =
            serde_json::from_value(value).map_err(|err| AuthError::Internal(err.to_string()))?;
        passkeys.push(passkey);
    }

    let (rcr, auth_state) = state
        .webauthn
        .start_passkey_authentication(&passkeys)
        .map_err(|err| AuthError::Webauthn(err.to_string()))?;

    let challenge_id = new_challenge_id();
    state.passkey_challenges.insert(
        challenge_id.clone(),
        PendingChallenge::Login {
            user_id,
            username,
            state: auth_state,
            passkeys,
            expires: Instant::now() + CHALLENGE_TTL,
        },
    );

    Ok(Json(ChallengeStartResponse {
        challenge_id,
        public_key: serde_json::to_value(rcr)
            .map_err(|err| AuthError::Internal(err.to_string()))?,
    }))
}

pub async fn login_finish(
    State(state): State<AppState>,
    Json(body): Json<LoginFinishRequest>,
) -> Result<Response, AuthError> {
    let pending = state
        .passkey_challenges
        .take(&body.challenge_id)
        .ok_or(AuthError::Unauthorized)?;
    let PendingChallenge::Login {
        user_id,
        username,
        state: auth_state,
        passkeys,
        ..
    } = pending
    else {
        return Err(AuthError::Unauthorized);
    };

    let auth_result = state
        .webauthn
        .finish_passkey_authentication(&body.credential, &auth_state)
        .map_err(|err| AuthError::Webauthn(err.to_string()))?;

    let cred_id = auth_result.cred_id().to_vec();
    let updated = passkeys
        .into_iter()
        .find(|pk| pk.cred_id() == auth_result.cred_id())
        .map(|mut pk| {
            pk.update_credential(&auth_result);
            pk
        });

    if let Some(passkey) = updated {
        let passkey_json =
            serde_json::to_value(&passkey).map_err(|err| AuthError::Internal(err.to_string()))?;
        sqlx::query(
            "UPDATE passkeys
             SET passkey_json = $1, last_used_at = now()
             WHERE credential_id = $2 AND user_id = $3",
        )
        .bind(passkey_json)
        .bind(&cred_id)
        .bind(&user_id)
        .execute(&state.db)
        .await?;
    }

    let session_id = create_session(&state.db, &user_id).await?;
    auth_success_response(&state, user_id, username, session_id)
}

fn auth_success_response(
    state: &AppState,
    user_id: String,
    username: String,
    session_id: [u8; 32],
) -> Result<Response, AuthError> {
    let body = AuthSuccess { user_id, username };
    let builder = Response::builder().status(StatusCode::OK);
    let builder = set_session_cookie(builder, &session_id, &state.rp_origin);
    Ok(builder
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(
            serde_json::to_vec(&body).expect("AuthSuccess serializes"),
        ))
        .expect("response"))
}

fn new_challenge_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
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

fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db) => db.code().as_deref() == Some("23505"),
        _ => false,
    }
}
