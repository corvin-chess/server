//! Account HTTP surface (messaging prefs for CV-140; export/delete stay later).

mod messaging;

use axum::routing::{get, patch};
use axum::Router;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/messaging", get(messaging::get_messaging))
        .route("/messaging", patch(messaging::patch_messaging))
}
