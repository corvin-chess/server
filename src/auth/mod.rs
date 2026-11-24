//! Passkey + email OTP authentication (CV-133) with session cookies + CSRF (CV-134).

pub mod csrf;
pub mod email_otp;
pub mod mailer;
pub mod passkey;
pub mod rate_limit;
pub mod session;

use std::sync::Arc;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::AppState;

/// Auth HTTP routes under `/api/v1/auth`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/email/otp/start", post(email_otp::otp_start))
        .route("/email/otp/verify", post(email_otp::otp_verify))
        .route("/passkey/register/start", post(passkey::register_start))
        .route("/passkey/register/finish", post(passkey::register_finish))
        .route("/passkey/login/start", post(passkey::login_start))
        .route("/passkey/login/finish", post(passkey::login_finish))
        .route("/session", get(session::get_session))
        .route("/logout", post(session::logout))
}

/// Shared mailer handle.
pub type SharedMailer = Arc<dyn mailer::Mailer>;

#[derive(Debug)]
pub enum AuthError {
    BadRequest(&'static str),
    Unauthorized,
    Forbidden(&'static str),
    Conflict(&'static str),
    Mail(String),
    Webauthn(String),
    Db(sqlx::Error),
    Internal(String),
}

impl From<sqlx::Error> for AuthError {
    fn from(value: sqlx::Error) -> Self {
        Self::Db(value)
    }
}

impl AuthError {
    pub fn to_response(&self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, (*msg).to_owned()),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_owned()),
            // CSRF failures use a fixed message so clients cannot probe token validity.
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden".to_owned()),
            Self::Conflict(msg) => (StatusCode::CONFLICT, (*msg).to_owned()),
            Self::Mail(_) => (StatusCode::BAD_GATEWAY, "mail delivery failed".to_owned()),
            Self::Webauthn(_) => (StatusCode::UNAUTHORIZED, "webauthn failed".to_owned()),
            Self::Db(err) => {
                eprintln!("corvin-server auth db error: {err}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_owned(),
                )
            }
            Self::Internal(err) => {
                eprintln!("corvin-server auth internal error: {err}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_owned(),
                )
            }
        };
        let body = serde_json::json!({ "error": message });
        (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            body.to_string(),
        )
            .into_response()
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        self.to_response()
    }
}
