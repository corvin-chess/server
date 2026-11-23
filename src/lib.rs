//! Corvin game server library surface (HTTP router + Postgres migrations).

#![forbid(unsafe_code)]

pub mod db;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

/// Build the HTTP router. Only `/health` is wired so far (CV-130).
pub fn app() -> Router {
    Router::new().route("/health", get(health))
}

/// Liveness probe: cheap, no dependency checks (Postgres readiness is separate).
/// Body matches the infra Compose stub so probes can `grep ok`.
async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        "ok\n",
    )
}
