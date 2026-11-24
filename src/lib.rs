//! Corvin game server library surface (HTTP router + Postgres + Redis presence/game log + auth + WS).

#![forbid(unsafe_code)]

pub mod account;
pub mod auth;
pub mod db;
pub mod game;
pub mod redis_store;
pub mod safety;
pub mod ws;

use std::sync::Arc;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use sqlx::PgPool;
use url::Url;
use webauthn_rs::prelude::Webauthn;
use webauthn_rs::WebauthnBuilder;

use crate::auth::mailer::LogMailer;
use crate::auth::passkey::PasskeyChallenges;
use crate::auth::rate_limit::OtpStartLimiter;
use crate::auth::SharedMailer;

/// Process-wide application state for HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub mailer: SharedMailer,
    pub webauthn: Arc<Webauthn>,
    pub passkey_challenges: Arc<PasskeyChallenges>,
    pub rp_origin: String,
    pub otp_pepper: Arc<Vec<u8>>,
    pub otp_rate_limit: Arc<OtpStartLimiter>,
}

impl AppState {
    /// Build state from a pool and WebAuthn relying-party settings.
    pub fn new(
        db: PgPool,
        mailer: SharedMailer,
        rp_id: &str,
        rp_origin: &str,
        otp_pepper: Vec<u8>,
    ) -> Result<Self, String> {
        if otp_pepper.len() < 16 {
            return Err("otp pepper must be at least 16 bytes".to_owned());
        }
        let origin = Url::parse(rp_origin).map_err(|err| format!("invalid RP origin: {err}"))?;
        let webauthn = WebauthnBuilder::new(rp_id, &origin)
            .map_err(|err| format!("webauthn builder: {err}"))?
            .rp_name("Corvin")
            .build()
            .map_err(|err| format!("webauthn build: {err}"))?;
        Ok(Self {
            db,
            mailer,
            webauthn: Arc::new(webauthn),
            passkey_challenges: Arc::new(PasskeyChallenges::new()),
            rp_origin: rp_origin.to_owned(),
            otp_pepper: Arc::new(otp_pepper),
            otp_rate_limit: Arc::new(OtpStartLimiter::new()),
        })
    }

    /// Test helper with an injected mailer.
    pub fn new_for_tests(
        db: PgPool,
        mailer: SharedMailer,
        rp_id: &str,
        rp_origin: &str,
        otp_pepper: &[u8],
    ) -> Result<Self, String> {
        Self::new(db, mailer, rp_id, rp_origin, otp_pepper.to_vec())
    }

    /// Production/dev constructor reading `CORVIN_RP_ID`, `CORVIN_RP_ORIGIN`, `CORVIN_OTP_PEPPER`.
    pub fn from_env(db: PgPool) -> Result<Self, String> {
        let rp_id = std::env::var("CORVIN_RP_ID").unwrap_or_else(|_| "localhost".to_owned());
        let rp_origin = std::env::var("CORVIN_RP_ORIGIN")
            .unwrap_or_else(|_| "http://localhost:3000".to_owned());
        let pepper = match std::env::var("CORVIN_OTP_PEPPER") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                let env = std::env::var("CORVIN_ENV").unwrap_or_else(|_| "development".to_owned());
                if env.eq_ignore_ascii_case("development") || env.eq_ignore_ascii_case("test") {
                    "dev-only-otp-pepper-change-me".to_owned()
                } else {
                    return Err(
                        "CORVIN_OTP_PEPPER is required when CORVIN_ENV is not development/test"
                            .to_owned(),
                    );
                }
            }
        };
        let mailer: SharedMailer = Arc::new(LogMailer);
        Self::new(db, mailer, &rp_id, &rp_origin, pepper.into_bytes())
    }
}

/// Full HTTP router including `/health`, `/ws`, `/api/v1/auth/*`, and `/api/v1/account/*`.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws::upgrade))
        .nest("/api/v1/auth", auth::router())
        .nest("/api/v1/account", account::router())
        .with_state(state)
}

/// Health-only router for unit tests that do not need Postgres.
pub fn health_router() -> Router {
    Router::new().route("/health", get(health))
}

/// WebSocket handshake router (`/ws` + `/health`) for PROTOCOL contract tests without Postgres.
pub fn ws_router() -> Router {
    ws::router()
}

/// Liveness probe: cheap, no dependency checks (Postgres readiness is separate).
/// Body matches the infra Compose stub so probes can `grep ok`.
pub(crate) async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        "ok\n",
    )
}
