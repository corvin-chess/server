//! Glicko-2 ratings per time class (CV-168 / ADR-007).

mod finish;
mod glicko2;
mod store;

pub use finish::{
    apply_rated_finish, game_end_with_rating_diff, rating_diff_json, FinishOutcome, RatedFinish,
    RatingDiffs,
};
pub use glicko2::{
    glicko2_update, is_provisional, Rating, INITIAL_R, INITIAL_RD, INITIAL_VOLATILITY,
    PROVISIONAL_RD, SCALE, TAU,
};
pub use store::{ensure_rating, get_rating, RatingError};
