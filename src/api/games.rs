//! Finished-game HTTP: summary + PGN export (PROTOCOL sketch / CV-169).

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::game::{export_pgn, game_summary_json, PersistError};
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{id}", get(get_game))
        .route("/{id}/pgn", get(get_game_pgn))
}

async fn get_game(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match game_summary_json(&state.db, &id).await {
        Ok(Some(body)) => (StatusCode::OK, Json(body)).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(PersistError::InvalidGameId) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn get_game_pgn(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match export_pgn(&state.db, &id).await {
        Ok(Some(pgn)) => (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/x-chess-pgn; charset=utf-8"),
                ),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=60"),
                ),
            ],
            pgn,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(PersistError::InvalidGameId) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
