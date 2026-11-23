//! Corvin game server library surface (HTTP router for CV-130+).

#![forbid(unsafe_code)]

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

/// Build the HTTP router. Only `/health` is wired in CV-130.
pub fn app() -> Router {
    Router::new().route("/health", get(health))
}

/// Liveness probe: cheap, no dependency checks (Postgres/Redis land later).
/// Body matches the infra Compose stub so probes can `grep ok`.
async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        "ok\n",
    )
}
