//! HTTP API surfaces beyond auth/account (CV-169 games).

mod games;

use axum::Router;

use crate::AppState;

/// Routes nested under `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new().nest("/games", games::router())
}
