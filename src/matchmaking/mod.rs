//! Quick pair pools and challenge links (CV-167 / PROTOCOL lobby).
//!
//! Redis holds ephemeral quick-pair queues; Postgres stores durable challenge rows.
//! WS lobby dispatch is wired in `crate::ws` (CV-172); helpers return PROTOCOL `GameStart` JSON.

mod challenge;
mod color;
mod config;
mod pool;
mod start;

pub use challenge::{
    challenge_accept, challenge_cancel, challenge_create_link, challenge_decline, challenge_json,
    ChallengeCreated, ChallengeError, ChallengeRow, DEFAULT_CHALLENGE_TTL_MS,
};
pub use color::{assign_colors, ColorPref};
pub use config::{validate_pair_config, MatchError, PairConfig, PROTOCOL_V};
pub use pool::{
    match_bundle_for, pool_key, quick_pair, quick_pair_cancel, store_pair_notify, take_pair_result,
    PAIR_KEY_PREFIX, PAIR_NOTIFY_PREFIX, PAIR_SEEK_TTL_SECS, QUEUE_KEY_PREFIX,
};
pub use start::{
    create_matched_game, game_start_json, MatchBundle, QuickPairOutcome, MAX_PLAYER_ID_LEN,
};
