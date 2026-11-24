//! Legal move apply with PROTOCOL MoveApplied / MoveRejected mapping (CV-162).

use std::fmt;

use corvin_chess_core::{Color, Move, Position, UciError};
use redis::aio::ConnectionManager;
use serde_json::{json, Value};

use super::replay::replay_fen;
use super::store::{
    append_move_event, load_events, load_meta, GameLogError, MAX_PLAYER_ID_LEN, MAX_UCI_LEN,
};

const PROTOCOL_V: u32 = 1;

/// Successful apply payload fields (PROTOCOL `MoveApplied` body).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedMove {
    pub uci: String,
    pub san: String,
    pub fen: String,
    pub clock_w_ms: u64,
    pub clock_b_ms: u64,
    /// Side to move after the ply (`"w"` / `"b"`).
    pub active: &'static str,
    pub seq: u64,
}

/// PROTOCOL `MoveRejected.code` values used by move apply.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MoveRejectCode {
    IllegalMove,
    NotYourTurn,
    GameGone,
    Auth,
}

impl MoveRejectCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IllegalMove => "ILLEGAL_MOVE",
            Self::NotYourTurn => "NOT_YOUR_TURN",
            Self::GameGone => "GAME_GONE",
            Self::Auth => "AUTH",
        }
    }
}

/// Errors from seating / legality checks before or during apply.
#[derive(Debug)]
pub enum ApplyError {
    IllegalMove(String),
    NotYourTurn,
    GameGone,
    Auth,
    InvalidUci(UciError),
    Corrupt(String),
    TooManyEvents,
    Redis(redis::RedisError),
}

impl ApplyError {
    /// PROTOCOL `MoveRejected.code` when this error is a client reject; `None` → `Error` `INTERNAL`.
    pub fn reject_code(&self) -> Option<MoveRejectCode> {
        match self {
            Self::IllegalMove(_) | Self::InvalidUci(_) => Some(MoveRejectCode::IllegalMove),
            Self::NotYourTurn => Some(MoveRejectCode::NotYourTurn),
            Self::GameGone => Some(MoveRejectCode::GameGone),
            Self::Auth => Some(MoveRejectCode::Auth),
            // Storage faults are not MoveRejected; WS callers must send Error INTERNAL.
            Self::Corrupt(_) | Self::TooManyEvents | Self::Redis(_) => None,
        }
    }
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IllegalMove(uci) => write!(f, "illegal move: {uci}"),
            Self::NotYourTurn => write!(f, "not your turn"),
            Self::GameGone => write!(f, "game not found"),
            Self::Auth => write!(f, "not a player in this game"),
            Self::InvalidUci(err) => write!(f, "invalid UCI: {err}"),
            Self::Corrupt(detail) => write!(f, "corrupt game state: {detail}"),
            Self::TooManyEvents => write!(f, "game event log exceeds max length"),
            Self::Redis(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ApplyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidUci(err) => Some(err),
            Self::Redis(err) => Some(err),
            _ => None,
        }
    }
}

impl From<GameLogError> for ApplyError {
    fn from(value: GameLogError) -> Self {
        match value {
            GameLogError::GameGone | GameLogError::InvalidGameId => Self::GameGone,
            GameLogError::InvalidUci(err) => Self::InvalidUci(err),
            GameLogError::IllegalMove(uci) => Self::IllegalMove(uci),
            GameLogError::TooManyEvents => Self::TooManyEvents,
            GameLogError::Redis(err) => Self::Redis(err),
            GameLogError::CorruptEvent(detail) => Self::Corrupt(detail),
            GameLogError::InvalidFen(err) => Self::Corrupt(err.to_string()),
            GameLogError::GameExists => Self::Corrupt("unexpected game exists".into()),
        }
    }
}

fn color_active(color: Color) -> &'static str {
    match color {
        Color::White => "w",
        Color::Black => "b",
    }
}

/// Build a PROTOCOL `MoveApplied` JSON object (envelope + body).
#[allow(clippy::too_many_arguments)]
pub fn move_applied_json(
    id: Option<&str>,
    game_id: &str,
    uci: &str,
    san: &str,
    fen: &str,
    clock_w_ms: u64,
    clock_b_ms: u64,
    active: &str,
    seq: u64,
    ts: u64,
) -> Value {
    let mut body = json!({
        "typ": "MoveApplied",
        "v": PROTOCOL_V,
        "game_id": game_id,
        "uci": uci,
        "san": san,
        "fen": fen,
        "clocks": { "w": clock_w_ms, "b": clock_b_ms },
        "active": active,
        "seq": seq,
        "ts": ts
    });
    if let Some(id) = id {
        body["id"] = json!(id);
    }
    body
}

/// Build a PROTOCOL `MoveRejected` JSON object.
pub fn move_rejected_json(id: &str, code: MoveRejectCode, message: &str) -> Value {
    json!({
        "typ": "MoveRejected",
        "v": PROTOCOL_V,
        "id": id,
        "code": code.as_str(),
        "message": message
    })
}

/// Apply a legal UCI move for `player_id` and append it to the Redis event log.
///
/// Clocks are echoed from meta unchanged (CV-163 owns tick/increment/flag).
/// Single-writer per game is assumed; concurrent apply needs Lua/CAS before WS (CV-171).
pub async fn apply_move(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    uci: &str,
    ts: u64,
) -> Result<AppliedMove, ApplyError> {
    if player_id.is_empty() || player_id.len() > MAX_PLAYER_ID_LEN {
        return Err(ApplyError::Auth);
    }
    if uci.is_empty() || uci.len() > MAX_UCI_LEN {
        return Err(ApplyError::InvalidUci(UciError::Invalid));
    }

    let meta = load_meta(conn, game_id).await?;
    if meta.white_id.is_empty() || meta.black_id.is_empty() {
        return Err(ApplyError::Auth);
    }

    let seat = if player_id == meta.white_id {
        Color::White
    } else if player_id == meta.black_id {
        Color::Black
    } else {
        return Err(ApplyError::Auth);
    };

    let events = load_events(conn, game_id).await?;
    let fen = replay_fen(&meta.initial_fen, &events)?;
    let pos = Position::from_fen(&fen).map_err(|err| ApplyError::Corrupt(err.to_string()))?;

    if pos.side_to_move() != seat {
        return Err(ApplyError::NotYourTurn);
    }

    let mv = Move::from_uci(uci).map_err(ApplyError::InvalidUci)?;
    let san = pos
        .to_san(mv)
        .map_err(|_| ApplyError::IllegalMove(uci.to_owned()))?;
    let next = pos
        .try_make_move(mv)
        .ok_or_else(|| ApplyError::IllegalMove(uci.to_owned()))?;
    let next_fen = next.to_fen();

    let seq = append_move_event(conn, game_id, uci, ts).await?;

    Ok(AppliedMove {
        uci: uci.to_owned(),
        san,
        fen: next_fen,
        clock_w_ms: meta.clock_w_ms,
        clock_b_ms: meta.clock_b_ms,
        active: color_active(next.side_to_move()),
        seq,
    })
}
