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

/// Meta JSON DoS bound (offers + prev clocks grow the payload; CV-165).
pub const MAX_META_BYTES: usize = 2048;

/// Single event JSON DoS bound.
pub const MAX_EVENT_BYTES: usize = 512;

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
///
/// When both lag quotas are zero and clocks are set, seeds interim quotas (CV-164).
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
    let mut stored = meta.clone();
    if stored.lag_quota_w_ms == 0
        && stored.lag_quota_b_ms == 0
        && (stored.clock_w_ms > 0 || stored.clock_b_ms > 0)
    {
        super::lag::seed_lag_quotas(&mut stored);
    }
    let payload = serde_json::to_string(&stored)
        .map_err(|err| GameLogError::CorruptEvent(err.to_string()))?;
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
    // Clock/offer fields grew the payload; keep a hard DoS bound.
    if raw.len() > MAX_META_BYTES {
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
    if payload.len() > MAX_META_BYTES {
        return Err(GameLogError::CorruptEvent("meta payload too large".into()));
    }
    let _: () = conn.set(meta_key, payload).await?;
    Ok(())
}

/// Outcome of the apply CAS Lua script (CV-171).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CasCommitResult {
    Ok { move_seq: u64, last_seq: u64 },
    Conflict { actual_len: u64 },
    Gone,
    TooManyEvents,
    Corrupt(String),
}

/// Parse the integer vector returned by `commit_apply_cas` Lua.
pub fn parse_cas_commit_reply(reply: &[i64]) -> CasCommitResult {
    match reply {
        // move_seq may be 0 for meta-only CAS when the event list is still empty.
        [1, move_seq, last_seq] if *move_seq >= 0 && *last_seq >= *move_seq => {
            CasCommitResult::Ok {
                move_seq: *move_seq as u64,
                last_seq: *last_seq as u64,
            }
        }
        [0, actual] if *actual >= 0 => CasCommitResult::Conflict {
            actual_len: *actual as u64,
        },
        [-1] => CasCommitResult::Gone,
        [-2] => CasCommitResult::TooManyEvents,
        _ => CasCommitResult::Corrupt("unexpected CAS reply".into()),
    }
}

/// Lua: CAS on event list length, then SET meta + RPUSH move (+ optional auto-draw).
///
/// KEYS[1]=meta KEYS[2]=events
/// ARGV[1]=expected_len ARGV[2]=max_events ARGV[3]=meta_json ARGV[4]=move_json
/// ARGV[5]=auto_json or empty
const COMMIT_APPLY_CAS_LUA: &str = r#"
local expected = tonumber(ARGV[1])
local max_events = tonumber(ARGV[2])
if redis.call('EXISTS', KEYS[1]) == 0 then
  return {-1}
end
local len = redis.call('LLEN', KEYS[2])
if len ~= expected then
  return {0, len}
end
local add = 1
if ARGV[5] ~= '' then
  add = 2
end
if len + add > max_events then
  return {-2}
end
redis.call('SET', KEYS[1], ARGV[3])
redis.call('RPUSH', KEYS[2], ARGV[4])
local move_seq = len + 1
local last_seq = move_seq
if ARGV[5] ~= '' then
  redis.call('RPUSH', KEYS[2], ARGV[5])
  last_seq = move_seq + 1
end
return {1, move_seq, last_seq}
"#;

/// Atomically commit meta + one or two events when `LLEN` still equals `expected_len`.
///
/// Shared by move apply (CV-171) and offer/claim/resign appenders (CV-173).
/// Fails with `CasCommitResult::Conflict` when another writer already appended
/// since `expected_len` was observed (callers retry after re-validation).
pub async fn commit_append_cas(
    conn: &mut ConnectionManager,
    game_id: &str,
    expected_len: u64,
    meta: &GameMeta,
    first: &GameEvent,
    second: Option<&GameEvent>,
) -> Result<CasCommitResult, GameLogError> {
    let (meta_key, events_key) = require_keys(game_id)?;
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
    let meta_payload =
        serde_json::to_string(meta).map_err(|err| GameLogError::CorruptEvent(err.to_string()))?;
    if meta_payload.len() > MAX_META_BYTES {
        return Err(GameLogError::CorruptEvent("meta payload too large".into()));
    }
    let first_payload =
        serde_json::to_string(first).map_err(|err| GameLogError::CorruptEvent(err.to_string()))?;
    if first_payload.len() > MAX_EVENT_BYTES {
        return Err(GameLogError::CorruptEvent("event payload too large".into()));
    }
    let second_payload = match second {
        Some(ev) => {
            let raw = serde_json::to_string(ev)
                .map_err(|err| GameLogError::CorruptEvent(err.to_string()))?;
            if raw.len() > MAX_EVENT_BYTES {
                return Err(GameLogError::CorruptEvent("event payload too large".into()));
            }
            raw
        }
        None => String::new(),
    };

    let reply: Vec<i64> = redis::Script::new(COMMIT_APPLY_CAS_LUA)
        .key(meta_key)
        .key(events_key)
        .arg(expected_len)
        .arg(MAX_EVENTS as u64)
        .arg(meta_payload)
        .arg(first_payload)
        .arg(second_payload)
        .invoke_async(conn)
        .await?;

    Ok(parse_cas_commit_reply(&reply))
}

/// Atomically commit an applied ply: meta + Move event (+ optional AutoDraw).
///
/// Thin wrapper over [`commit_append_cas`] kept for apply call sites (CV-171).
pub async fn commit_apply_cas(
    conn: &mut ConnectionManager,
    game_id: &str,
    expected_len: u64,
    meta: &GameMeta,
    move_event: &GameEvent,
    auto_event: Option<&GameEvent>,
) -> Result<CasCommitResult, GameLogError> {
    commit_append_cas(conn, game_id, expected_len, meta, move_event, auto_event).await
}

/// Lua: CAS on event list length, then SET meta only (flag / terminal without a ply).
///
/// KEYS[1]=meta KEYS[2]=events
/// ARGV[1]=expected_len ARGV[2]=meta_json
const COMMIT_META_CAS_LUA: &str = r#"
local expected = tonumber(ARGV[1])
if redis.call('EXISTS', KEYS[1]) == 0 then
  return {-1}
end
local len = redis.call('LLEN', KEYS[2])
if len ~= expected then
  return {0, len}
end
redis.call('SET', KEYS[1], ARGV[2])
return {1, len, len}
"#;

/// Atomically write meta only when the event list length still matches `expected_len`.
///
/// Used for flag termination so a concurrent move CAS cannot be overwritten by a
/// stale `save_meta` after the ply already committed.
pub async fn commit_meta_cas(
    conn: &mut ConnectionManager,
    game_id: &str,
    expected_len: u64,
    meta: &GameMeta,
) -> Result<CasCommitResult, GameLogError> {
    let (meta_key, events_key) = require_keys(game_id)?;
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
    let meta_payload =
        serde_json::to_string(meta).map_err(|err| GameLogError::CorruptEvent(err.to_string()))?;
    if meta_payload.len() > MAX_META_BYTES {
        return Err(GameLogError::CorruptEvent("meta payload too large".into()));
    }

    let reply: Vec<i64> = redis::Script::new(COMMIT_META_CAS_LUA)
        .key(meta_key)
        .key(events_key)
        .arg(expected_len)
        .arg(meta_payload)
        .invoke_async(conn)
        .await?;

    Ok(parse_cas_commit_reply(&reply))
}

/// Bounded CAS retries when another writer wins the event-length race (CV-173).
const MAX_APPEND_CAS_RETRIES: u32 = 8;

/// Lua: CAS on event list length, then RPUSH only (never SET meta).
///
/// KEYS[1]=meta KEYS[2]=events
/// ARGV[1]=expected_len ARGV[2]=max_events ARGV[3]=event_json
///
/// Bare appenders must not rewrite meta: a concurrent offer/move may have already
/// committed a newer meta at the same prior length observation window.
const COMMIT_EVENT_ONLY_CAS_LUA: &str = r#"
local expected = tonumber(ARGV[1])
local max_events = tonumber(ARGV[2])
if redis.call('EXISTS', KEYS[1]) == 0 then
  return {-1}
end
local len = redis.call('LLEN', KEYS[2])
if len ~= expected then
  return {0, len}
end
if len + 1 > max_events then
  return {-2}
end
redis.call('RPUSH', KEYS[2], ARGV[3])
local seq = len + 1
return {1, seq, seq}
"#;

/// Append any game event; assigns monotonic `seq` (1-based list length) and returns it.
///
/// Pass `seq: 0` on the event; the stored value is overwritten with the list length.
/// Uses Lua CAS on `LLEN` + RPUSH only so concurrent appenders cannot share a seq
/// and cannot stomp a newer meta written by offer/move CAS (CV-173).
pub async fn append_game_event(
    conn: &mut ConnectionManager,
    game_id: &str,
    event: GameEvent,
) -> Result<u64, GameLogError> {
    let (meta_key, events_key) = require_keys(game_id)?;
    for attempt in 0..MAX_APPEND_CAS_RETRIES {
        let exists: bool = conn.exists(&meta_key).await?;
        if !exists {
            return Err(GameLogError::GameGone);
        }
        let expected: u64 = conn.llen(&events_key).await?;
        if expected as usize >= MAX_EVENTS {
            return Err(GameLogError::TooManyEvents);
        }
        let mut ev = event.clone();
        set_event_seq(&mut ev, expected + 1);
        let payload = serde_json::to_string(&ev)
            .map_err(|err| GameLogError::CorruptEvent(err.to_string()))?;
        if payload.len() > MAX_EVENT_BYTES {
            return Err(GameLogError::CorruptEvent("event payload too large".into()));
        }
        let reply: Vec<i64> = redis::Script::new(COMMIT_EVENT_ONLY_CAS_LUA)
            .key(&meta_key)
            .key(&events_key)
            .arg(expected)
            .arg(MAX_EVENTS as u64)
            .arg(payload)
            .invoke_async(conn)
            .await?;
        match parse_cas_commit_reply(&reply) {
            CasCommitResult::Ok { last_seq, .. } => return Ok(last_seq),
            CasCommitResult::Conflict { .. } if attempt + 1 < MAX_APPEND_CAS_RETRIES => continue,
            CasCommitResult::Conflict { .. } => {
                return Err(GameLogError::CorruptEvent(
                    "append CAS exhausted after concurrent writers".into(),
                ));
            }
            CasCommitResult::Gone => return Err(GameLogError::GameGone),
            CasCommitResult::TooManyEvents => return Err(GameLogError::TooManyEvents),
            CasCommitResult::Corrupt(detail) => return Err(GameLogError::CorruptEvent(detail)),
        }
    }
    Err(GameLogError::CorruptEvent(
        "append CAS exhausted after concurrent writers".into(),
    ))
}

fn set_event_seq(event: &mut GameEvent, seq: u64) {
    match event {
        GameEvent::Move { seq: s, .. }
        | GameEvent::ServerNote { seq: s, .. }
        | GameEvent::Offer { seq: s, .. }
        | GameEvent::OfferResponse { seq: s, .. }
        | GameEvent::Resign { seq: s, .. }
        | GameEvent::Abort { seq: s, .. }
        | GameEvent::Takeback { seq: s, .. }
        | GameEvent::ClaimDraw { seq: s, .. }
        | GameEvent::AutoDraw { seq: s, .. } => *s = seq,
    }
}

/// Append a Move event; returns the new monotonic `seq` (1-based list length).
pub async fn append_move_event(
    conn: &mut ConnectionManager,
    game_id: &str,
    uci: &str,
    ts: u64,
) -> Result<u64, GameLogError> {
    if uci.is_empty() || uci.len() > MAX_UCI_LEN {
        return Err(GameLogError::InvalidUci(UciError::Invalid));
    }
    // Shape-check UCI before storage; callers that need legality use `apply_move`.
    let _ = corvin_chess_core::Move::from_uci(uci).map_err(GameLogError::InvalidUci)?;
    append_game_event(conn, game_id, GameEvent::move_uci(0, uci, ts)).await
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
        if line.len() > MAX_EVENT_BYTES {
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
