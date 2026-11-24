//! Flush finished live games from Redis into Postgres (CV-169 / ADR-004).

use std::fmt;

use chrono::{TimeZone, Utc};
use redis::aio::ConnectionManager;
use serde_json::Value;
use sqlx::PgPool;

use super::events::{GameEvent, GameMeta};
use super::pgn::{render_pgn, PgnExportError};
use super::replay::replay_fen;
use super::store::{
    load_events, load_meta, GameLogError, MAX_EVENTS, MAX_EVENT_BYTES, MAX_GAME_ID_LEN,
};

/// Outcome of a once-only persist attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistOutcome {
    Persisted,
    AlreadyPersisted,
}

/// Inputs for writing a finished game row + event log.
#[derive(Clone, Debug)]
pub struct PersistInput<'a> {
    pub game_id: &'a str,
    pub meta: &'a GameMeta,
    pub events: &'a [GameEvent],
    pub result: &'a str,
    pub reason: &'a str,
    pub ended_ms: u64,
}

/// Persist / export errors.
#[derive(Debug)]
pub enum PersistError {
    InvalidGameId,
    InvalidResult,
    InvalidReason,
    InvalidTimeClass,
    MissingSeats,
    GameGone,
    Pgn(PgnExportError),
    Log(GameLogError),
    Sqlx(sqlx::Error),
}

impl fmt::Display for PersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGameId => write!(f, "invalid game id"),
            Self::InvalidResult => write!(f, "invalid result"),
            Self::InvalidReason => write!(f, "invalid reason"),
            Self::InvalidTimeClass => write!(f, "invalid time class"),
            Self::MissingSeats => write!(f, "missing seats"),
            Self::GameGone => write!(f, "game not found"),
            Self::Pgn(err) => write!(f, "{err}"),
            Self::Log(err) => write!(f, "{err}"),
            Self::Sqlx(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Pgn(err) => Some(err),
            Self::Log(err) => Some(err),
            Self::Sqlx(err) => Some(err),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for PersistError {
    fn from(value: sqlx::Error) -> Self {
        Self::Sqlx(value)
    }
}

impl From<PgnExportError> for PersistError {
    fn from(value: PgnExportError) -> Self {
        Self::Pgn(value)
    }
}

impl From<GameLogError> for PersistError {
    fn from(value: GameLogError) -> Self {
        match value {
            GameLogError::GameGone => Self::GameGone,
            GameLogError::InvalidGameId => Self::InvalidGameId,
            other => Self::Log(other),
        }
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

fn normalize_result(result: &str) -> Result<&str, PersistError> {
    match result.trim() {
        "1-0" | "0-1" | "1/2-1/2" | "*" => Ok(result.trim()),
        _ => Err(PersistError::InvalidResult),
    }
}

fn normalize_reason(reason: &str) -> Result<&str, PersistError> {
    let reason = reason.trim();
    if reason.is_empty() || reason.len() > 64 {
        return Err(PersistError::InvalidReason);
    }
    Ok(reason)
}

fn normalize_time_class(raw: &str) -> Result<String, PersistError> {
    let class = raw.trim().to_ascii_lowercase();
    match class.as_str() {
        "bullet" | "blitz" | "rapid" | "classical" | "correspondence" => Ok(class),
        "" => Ok("blitz".to_owned()),
        _ => Err(PersistError::InvalidTimeClass),
    }
}

fn is_ulid(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'Z'))
}

async fn resolve_username(
    pool: &PgPool,
    seat_id: &str,
    fallback: &str,
) -> Result<String, PersistError> {
    let trimmed = seat_id.trim();
    if trimmed.is_empty() {
        return Ok(fallback.to_owned());
    }
    if is_ulid(trimmed) {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT username FROM users WHERE id = $1 AND deleted_at IS NULL")
                .bind(trimmed)
                .fetch_optional(pool)
                .await?;
        if let Some((username,)) = row {
            if !username.is_empty() {
                return Ok(username);
            }
        }
    }
    // Guest / unknown seat: keep a stable public label without inventing accounts.
    // Char-based truncate (not bytes) so non-ASCII seat tokens cannot panic.
    let label = if trimmed.chars().count() > 16 {
        let prefix: String = trimmed.chars().take(12).collect();
        format!("{prefix}...")
    } else {
        trimmed.to_owned()
    };
    Ok(label)
}

fn ended_at_from_ms(ended_ms: u64) -> chrono::DateTime<Utc> {
    let secs = (ended_ms / 1000) as i64;
    Utc.timestamp_opt(secs, ((ended_ms % 1000) * 1_000_000) as u32)
        .single()
        .unwrap_or_else(Utc::now)
}

/// Persist a finished game once. Events are stored as JSONB for exact audit replay.
pub async fn persist_finished_game(
    pool: &PgPool,
    input: PersistInput<'_>,
) -> Result<PersistOutcome, PersistError> {
    if !is_safe_game_id(input.game_id) {
        return Err(PersistError::InvalidGameId);
    }
    let result = normalize_result(input.result)?;
    let reason = normalize_reason(input.reason)?;
    let time_class = normalize_time_class(&input.meta.time_class)?;

    let white_id = input.meta.white_id.trim();
    let black_id = input.meta.black_id.trim();
    if white_id.is_empty() || black_id.is_empty() {
        return Err(PersistError::MissingSeats);
    }

    if input.events.len() > MAX_EVENTS {
        return Err(PersistError::Log(GameLogError::TooManyEvents));
    }
    for event in input.events {
        let encoded = serde_json::to_vec(event).map_err(|err| {
            PersistError::Log(GameLogError::CorruptEvent(format!("serialize: {err}")))
        })?;
        if encoded.len() > MAX_EVENT_BYTES {
            return Err(PersistError::Log(GameLogError::CorruptEvent(
                "event payload exceeds max bytes".into(),
            )));
        }
    }

    let white_username = resolve_username(pool, white_id, "White").await?;
    let black_username = resolve_username(pool, black_id, "Black").await?;
    let final_fen = replay_fen(&input.meta.initial_fen, input.events)?;
    // Prefer explicit initial_ms. Do not invent TimeControl from remaining clocks.
    let initial_ms = input.meta.initial_ms;

    let mut tx = pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO games (
            id, initial_fen, white_id, black_id, white_username, black_username,
            time_class, rated, initial_ms, increment_ms, result, reason,
            final_fen, created_ms, ended_at
         ) VALUES (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9, $10, $11, $12,
            $13, $14, $15
         )
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(input.game_id)
    .bind(&input.meta.initial_fen)
    .bind(white_id)
    .bind(black_id)
    .bind(&white_username)
    .bind(&black_username)
    .bind(&time_class)
    .bind(input.meta.rated)
    .bind(initial_ms as i64)
    .bind(input.meta.increment_ms as i64)
    .bind(result)
    .bind(reason)
    .bind(&final_fen)
    .bind(input.meta.created_ms as i64)
    .bind(ended_at_from_ms(input.ended_ms))
    .execute(&mut *tx)
    .await?;

    if inserted.rows_affected() == 0 {
        // Once-only claim won by another writer; do not append events again.
        tx.commit().await?;
        return Ok(PersistOutcome::AlreadyPersisted);
    }

    for event in input.events {
        let payload = serde_json::to_value(event).map_err(|err| {
            PersistError::Log(GameLogError::CorruptEvent(format!("serialize: {err}")))
        })?;
        sqlx::query(
            "INSERT INTO game_events (game_id, seq, payload)
             VALUES ($1, $2, $3)",
        )
        .bind(input.game_id)
        .bind(event.seq() as i64)
        .bind(payload)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(PersistOutcome::Persisted)
}

/// Load Redis live log and persist it (helper for finish paths / tests).
pub async fn persist_finished_from_redis(
    pool: &PgPool,
    conn: &mut ConnectionManager,
    game_id: &str,
    result: &str,
    reason: &str,
    ended_ms: u64,
) -> Result<PersistOutcome, PersistError> {
    let meta = load_meta(conn, game_id).await?;
    let events = load_events(conn, game_id).await?;
    persist_finished_game(
        pool,
        PersistInput {
            game_id,
            meta: &meta,
            events: &events,
            result,
            reason,
            ended_ms,
        },
    )
    .await
}

#[derive(sqlx::FromRow)]
struct GameRow {
    initial_fen: String,
    white_username: String,
    black_username: String,
    time_class: String,
    rated: bool,
    initial_ms: i64,
    increment_ms: i64,
    result: String,
    reason: String,
    created_ms: i64,
    ended_at: chrono::DateTime<Utc>,
}

/// Export PGN for a durable game id, or `Ok(None)` when missing.
pub async fn export_pgn(pool: &PgPool, game_id: &str) -> Result<Option<String>, PersistError> {
    if !is_safe_game_id(game_id) {
        return Err(PersistError::InvalidGameId);
    }
    let row = sqlx::query_as::<_, GameRow>(
        "SELECT initial_fen, white_username, black_username, time_class, rated,
                initial_ms, increment_ms, result, reason, created_ms, ended_at
         FROM games
         WHERE id = $1",
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };

    let payloads: Vec<(Value,)> =
        sqlx::query_as("SELECT payload FROM game_events WHERE game_id = $1 ORDER BY seq ASC")
            .bind(game_id)
            .fetch_all(pool)
            .await?;
    if payloads.len() > MAX_EVENTS {
        return Err(PersistError::Log(GameLogError::TooManyEvents));
    }
    let mut events = Vec::with_capacity(payloads.len());
    for (payload,) in payloads {
        let event: GameEvent = serde_json::from_value(payload).map_err(|err| {
            PersistError::Log(GameLogError::CorruptEvent(format!("deserialize: {err}")))
        })?;
        events.push(event);
    }

    let meta = GameMeta {
        initial_fen: row.initial_fen,
        time_class: row.time_class,
        rated: row.rated,
        initial_ms: row.initial_ms.max(0) as u64,
        increment_ms: row.increment_ms.max(0) as u64,
        created_ms: row.created_ms.max(0) as u64,
        ..GameMeta::default()
    };
    let ended_ms = row.ended_at.timestamp_millis().max(0) as u64;
    let pgn = render_pgn(
        &meta,
        &events,
        &row.white_username,
        &row.black_username,
        &row.result,
        &row.reason,
        ended_ms,
    )?;
    Ok(Some(pgn))
}

/// Lightweight JSON summary for `GET /api/v1/games/{id}`.
pub async fn game_summary_json(
    pool: &PgPool,
    game_id: &str,
) -> Result<Option<Value>, PersistError> {
    if !is_safe_game_id(game_id) {
        return Err(PersistError::InvalidGameId);
    }
    let row = sqlx::query_as::<_, GameRow>(
        "SELECT initial_fen, white_username, black_username, time_class, rated,
                initial_ms, increment_ms, result, reason, created_ms, ended_at
         FROM games
         WHERE id = $1",
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(serde_json::json!({
        "id": game_id,
        "white": row.white_username,
        "black": row.black_username,
        "result": row.result,
        "reason": row.reason,
        "time_class": row.time_class,
        "rated": row.rated,
        "initial_ms": row.initial_ms,
        "increment_ms": row.increment_ms,
        "initial_fen": row.initial_fen,
        "created_ms": row.created_ms,
        "ended_at": row.ended_at.to_rfc3339(),
    })))
}
