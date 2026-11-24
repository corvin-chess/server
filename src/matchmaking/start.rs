//! Create a seated live game and PROTOCOL `GameStart` payloads.

use redis::aio::ConnectionManager;
use serde_json::{json, Value};
use ulid::Ulid;

use crate::game::{create_game, GameMeta, START_FEN};

use super::config::{validate_pair_config, MatchError, PairConfig, PROTOCOL_V};

/// Align with game seat bound (`store::MAX_PLAYER_ID_LEN`).
pub const MAX_PLAYER_ID_LEN: usize = 64;

/// One seat's view of a newly started game.
#[derive(Clone, Debug)]
pub struct MatchBundle {
    pub game_id: String,
    pub white_id: String,
    pub black_id: String,
    /// `"w"` or `"b"` for this recipient.
    pub your_color: &'static str,
    pub start: Value,
}

/// Result of enqueueing a quick-pair seek.
#[derive(Clone, Debug)]
pub enum QuickPairOutcome {
    Waiting,
    Matched(MatchBundle),
}

/// PROTOCOL `GameStart` for one player.
pub fn game_start_json(
    game_id: &str,
    your_color: &str,
    config: &PairConfig,
    white_id: &str,
    black_id: &str,
) -> Value {
    json!({
        "typ": "GameStart",
        "v": PROTOCOL_V,
        "game_id": game_id,
        "color": your_color,
        "config": {
            "time_class": config.time_class,
            "initial_ms": config.initial_ms,
            "increment_ms": config.increment_ms,
            "rated": config.rated,
        },
        "opponents": {
            "w": { "id": white_id },
            "b": { "id": black_id },
        },
        "start_clocks": {
            "w": config.initial_ms,
            "b": config.initial_ms,
        },
    })
}

fn seat_color(player_id: &str, white_id: &str) -> &'static str {
    if player_id == white_id {
        "w"
    } else {
        "b"
    }
}

/// Build MatchBundle views for both seats after colors are known.
pub fn bundles_for_pair(
    game_id: &str,
    config: &PairConfig,
    white_id: &str,
    black_id: &str,
) -> (MatchBundle, MatchBundle) {
    let white = MatchBundle {
        game_id: game_id.to_owned(),
        white_id: white_id.to_owned(),
        black_id: black_id.to_owned(),
        your_color: "w",
        start: game_start_json(game_id, "w", config, white_id, black_id),
    };
    let black = MatchBundle {
        game_id: game_id.to_owned(),
        white_id: white_id.to_owned(),
        black_id: black_id.to_owned(),
        your_color: "b",
        start: game_start_json(game_id, "b", config, white_id, black_id),
    };
    (white, black)
}

pub fn bundle_for_player(
    game_id: &str,
    config: &PairConfig,
    white_id: &str,
    black_id: &str,
    player_id: &str,
) -> MatchBundle {
    let your_color = seat_color(player_id, white_id);
    MatchBundle {
        game_id: game_id.to_owned(),
        white_id: white_id.to_owned(),
        black_id: black_id.to_owned(),
        your_color,
        start: game_start_json(game_id, your_color, config, white_id, black_id),
    }
}

/// Create Redis live game with seats, clocks, and clock start timestamp.
pub async fn create_matched_game(
    conn: &mut ConnectionManager,
    config: &PairConfig,
    white_id: &str,
    black_id: &str,
    now_ms: u64,
) -> Result<String, MatchError> {
    validate_pair_config(config)?;
    if !super::config::is_safe_player_id(white_id, MAX_PLAYER_ID_LEN)
        || !super::config::is_safe_player_id(black_id, MAX_PLAYER_ID_LEN)
    {
        return Err(MatchError::InvalidPlayerId);
    }
    if white_id == black_id {
        return Err(MatchError::SelfMatch);
    }

    let game_id = Ulid::new().to_string();
    let meta = GameMeta {
        initial_fen: START_FEN.to_owned(),
        created_ms: now_ms,
        white_id: white_id.to_owned(),
        black_id: black_id.to_owned(),
        clock_w_ms: config.initial_ms,
        clock_b_ms: config.initial_ms,
        increment_ms: config.increment_ms,
        initial_ms: config.initial_ms,
        active: "w".to_owned(),
        // Clocks start at GameStart so flag claims have a baseline (CV-163).
        active_started_ms: now_ms,
        time_class: config.time_class.trim().to_ascii_lowercase(),
        rated: config.rated,
        ..GameMeta::default()
    };
    create_game(conn, &game_id, &meta).await?;
    Ok(game_id)
}
