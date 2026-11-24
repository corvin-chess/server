//! Redis persistence for live game meta + append-only event lists.

use redis::aio::ConnectionManager;
use redis::{AsyncCommands, RedisError};
use std::fmt;

use corvin_chess_core::{FenError, UciError};

use super::events::{GameEvent, GameMeta};
use super::replay::replay_fen;

/// Redis key prefix for live games (`corvin:game:{id}:meta|events`).
pub const GAME_KEY_PREFIX: &str = "corvin:game:";

/// Max game id length (ULID is 26; headroom for opaque ids).
pub const MAX_GAME_ID_LEN: usize = 32;

/// Max seat token length (matches apply_move player_id bound).
pub const MAX_PLAYER_ID_LEN: usize = 64;

/// Reject absurd FEN blobs before parse / Redis write (six-field FEN is far smaller).
pub const MAX_FEN_LEN: usize = 128;

/// UCI moves are 4–5 chars; reject oversized payloads before RPUSH.
pub const MAX_UCI_LEN: usize = 8;

/// Hard cap on live event list length for a single reconstruct (DoS bound).
pub const MAX_EVENTS: usize = 1024;

/// Errors from game log create / append / replay.
#[derive(Debug)]
pub enum GameLogError {
    InvalidGameId,
    GameExists,
    GameGone,
    InvalidFen(FenError),
    InvalidUci(UciError),
    IllegalMove(String),
    CorruptEvent(String),
    TooManyEvents,
    Redis(RedisError),
}

impl fmt::Display for GameLogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGameId => write!(f, "invalid game id"),
            Self::GameExists => write!(f, "game already exists"),
            Self::GameGone => write!(f, "game not found"),
            Self::InvalidFen(err) => write!(f, "invalid FEN: {err}"),
            Self::InvalidUci(err) => write!(f, "invalid UCI: {err}"),
            Self::IllegalMove(uci) => write!(f, "illegal move: {uci}"),
            Self::CorruptEvent(detail) => write!(f, "corrupt game event: {detail}"),
            Self::TooManyEvents => write!(f, "game event log exceeds max length"),
            Self::Redis(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for GameLogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Redis(err) => Some(err),
            Self::InvalidFen(err) => Some(err),
            Self::InvalidUci(err) => Some(err),
            _ => None,
        }
    }
}

impl From<RedisError> for GameLogError {
    fn from(value: RedisError) -> Self {
        Self::Redis(value)
    }
}

fn is_safe_game_id(game_id: &str) -> bool {
    if game_id.is_empty() || game_id.len() > MAX_GAME_ID_LEN {
        return false;
    }
    game_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Meta key `corvin:game:{id}:meta`, or empty if `game_id` is invalid.
pub fn game_meta_key(game_id: &str) -> String {
    if !is_safe_game_id(game_id) {
        return String::new();
    }
    format!("{GAME_KEY_PREFIX}{game_id}:meta")
}

/// Events list key `corvin:game:{id}:events`, or empty if `game_id` is invalid.
pub fn game_events_key(game_id: &str) -> String {
    if !is_safe_game_id(game_id) {
        return String::new();
    }
    format!("{GAME_KEY_PREFIX}{game_id}:events")
}

fn require_keys(game_id: &str) -> Result<(String, String), GameLogError> {
    let meta = game_meta_key(game_id);
    let events = game_events_key(game_id);
    if meta.is_empty() || events.is_empty() {
        return Err(GameLogError::InvalidGameId);
    }
    Ok((meta, events))
}

/// Create a live game with `meta.initial_fen`. Fails if the meta key already exists.
pub async fn create_game(
    conn: &mut ConnectionManager,
    game_id: &str,
    meta: &GameMeta,
) -> Result<(), GameLogError> {
    let (meta_key, _) = require_keys(game_id)?;
    if meta.initial_fen.len() > MAX_FEN_LEN || meta.initial_fen.is_empty() {
        return Err(GameLogError::InvalidFen(FenError::FieldCount));
    }
    if meta.white_id.len() > MAX_PLAYER_ID_LEN || meta.black_id.len() > MAX_PLAYER_ID_LEN {
        return Err(GameLogError::CorruptEvent(
            "seat id exceeds max length".into(),
        ));
    }
    // Reject bad FEN before touching Redis so callers get a clear error.
    let _ = corvin_chess_core::Position::from_fen(&meta.initial_fen)
        .map_err(GameLogError::InvalidFen)?;
    let payload =
        serde_json::to_string(meta).map_err(|err| GameLogError::CorruptEvent(err.to_string()))?;
    // SET NX: first writer wins; avoids clobbering an in-progress game.
    let set: Option<String> = redis::cmd("SET")
        .arg(&meta_key)
        .arg(payload)
        .arg("NX")
        .query_async(conn)
        .await?;
    if set.is_none() {
        return Err(GameLogError::GameExists);
    }
    Ok(())
}

/// Load game meta, or `GameGone` if missing.
pub async fn load_meta(
    conn: &mut ConnectionManager,
    game_id: &str,
) -> Result<GameMeta, GameLogError> {
    let (meta_key, _) = require_keys(game_id)?;
    let raw: Option<String> = conn.get(meta_key).await?;
    let raw = raw.ok_or(GameLogError::GameGone)?;
    // Clock fields grew the payload; keep a hard DoS bound.
    if raw.len() > 1024 {
        return Err(GameLogError::CorruptEvent("meta payload too large".into()));
    }
    let meta: GameMeta =
        serde_json::from_str(&raw).map_err(|err| GameLogError::CorruptEvent(err.to_string()))?;
    if meta.initial_fen.len() > MAX_FEN_LEN || meta.initial_fen.is_empty() {
        return Err(GameLogError::CorruptEvent(
            "meta initial_fen out of bounds".into(),
        ));
    }
    Ok(meta)
}

/// Overwrite game meta (clocks / ended). Fails if the game key is missing.
pub async fn save_meta(
    conn: &mut ConnectionManager,
    game_id: &str,
    meta: &GameMeta,
) -> Result<(), GameLogError> {
    let (meta_key, _) = require_keys(game_id)?;
    if meta.initial_fen.len() > MAX_FEN_LEN || meta.initial_fen.is_empty() {
        return Err(GameLogError::CorruptEvent(
            "meta initial_fen out of bounds".into(),
        ));
    }
    if meta.white_id.len() > MAX_PLAYER_ID_LEN || meta.black_id.len() > MAX_PLAYER_ID_LEN {
        return Err(GameLogError::CorruptEvent(
            "seat id exceeds max length".into(),
        ));
    }
    let exists: bool = conn.exists(&meta_key).await?;
    if !exists {
        return Err(GameLogError::GameGone);
    }
    let payload =
        serde_json::to_string(meta).map_err(|err| GameLogError::CorruptEvent(err.to_string()))?;
    if payload.len() > 1024 {
        return Err(GameLogError::CorruptEvent("meta payload too large".into()));
    }
    let _: () = conn.set(meta_key, payload).await?;
    Ok(())
}

/// Append a Move event; returns the new monotonic `seq` (1-based list length).
///
/// Assumes single-writer ownership per game (live match node). Concurrent
/// appenders can race LLEN/RPUSH; CV-162+ can tighten with a Lua script.
pub async fn append_move_event(
    conn: &mut ConnectionManager,
    game_id: &str,
    uci: &str,
    ts: u64,
) -> Result<u64, GameLogError> {
    let (meta_key, events_key) = require_keys(game_id)?;
    if uci.is_empty() || uci.len() > MAX_UCI_LEN {
        return Err(GameLogError::InvalidUci(UciError::Invalid));
    }
    // Shape-check UCI before storage; callers that need legality use `apply_move`.
    let _ = corvin_chess_core::Move::from_uci(uci).map_err(GameLogError::InvalidUci)?;
    let exists: bool = conn.exists(meta_key).await?;
    if !exists {
        return Err(GameLogError::GameGone);
    }
    let prev: u64 = conn.llen(&events_key).await?;
    if prev as usize >= MAX_EVENTS {
        return Err(GameLogError::TooManyEvents);
    }
    let seq = prev + 1;
    let event = GameEvent::move_uci(seq, uci, ts);
    let payload =
        serde_json::to_string(&event).map_err(|err| GameLogError::CorruptEvent(err.to_string()))?;
    let new_len: u64 = conn.rpush(events_key, payload).await?;
    Ok(new_len)
}

/// Load all events in order (empty vec when none). Requires meta to exist.
pub async fn load_events(
    conn: &mut ConnectionManager,
    game_id: &str,
) -> Result<Vec<GameEvent>, GameLogError> {
    let (meta_key, events_key) = require_keys(game_id)?;
    let exists: bool = conn.exists(meta_key).await?;
    if !exists {
        return Err(GameLogError::GameGone);
    }
    let len: u64 = conn.llen(&events_key).await?;
    if len as usize > MAX_EVENTS {
        return Err(GameLogError::TooManyEvents);
    }
    let raw: Vec<String> = conn.lrange(events_key, 0, -1).await?;
    let mut out = Vec::with_capacity(raw.len());
    for (idx, line) in raw.iter().enumerate() {
        if line.len() > 512 {
            return Err(GameLogError::CorruptEvent(format!(
                "index {idx}: event payload too large"
            )));
        }
        let event: GameEvent = serde_json::from_str(line)
            .map_err(|err| GameLogError::CorruptEvent(format!("index {idx}: {err}")))?;
        out.push(event);
    }
    Ok(out)
}

/// Replay the Redis event log and return the current FEN.
pub async fn reconstruct_fen(
    conn: &mut ConnectionManager,
    game_id: &str,
) -> Result<String, GameLogError> {
    let meta = load_meta(conn, game_id).await?;
    let events = load_events(conn, game_id).await?;
    replay_fen(&meta.initial_fen, &events)
}

/// Delete meta + events keys (tests and abandoned games).
pub async fn delete_game(conn: &mut ConnectionManager, game_id: &str) -> Result<(), GameLogError> {
    let (meta_key, events_key) = require_keys(game_id)?;
    let _: () = conn.del(&[meta_key, events_key]).await?;
    Ok(())
}
