//! Threefold / fifty-move claims and fivefold / seventy-five auto draws (CV-166).
//!
//! Current-position claims only (FIDE "has appeared"); claim-with-move ("about to
//! appear") stays out of scope. Side to move must be the claimant. Auto draws run
//! after a successful apply when chess-core reports fivefold or seventy-five.
//! Redis Lua CAS commit for claim appends (CV-173).

use std::fmt;

use corvin_chess_core::{Color, Move, Position, RepetitionKey};
use redis::aio::ConnectionManager;
use serde_json::{json, Value};

use super::events::GameEvent;
use super::offers::GameEndOut;
use super::replay::net_move_events;
use super::store::{
    append_game_event, commit_append_cas, load_events, load_meta, CasCommitResult, GameLogError,
    MAX_PLAYER_ID_LEN,
};

const PROTOCOL_V: u32 = 1;
/// Bounded CAS retries when another writer wins the event-length race.
const MAX_CAS_RETRIES: u32 = 8;

/// PROTOCOL `Error.code` for failed draw claims.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ClaimRejectCode {
    ClaimDenied,
    Auth,
    GameGone,
}

impl ClaimRejectCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaimDenied => "CLAIM_DENIED",
            Self::Auth => "AUTH",
            Self::GameGone => "GAME_GONE",
        }
    }
}

/// Errors from `claim_draw`.
#[derive(Debug)]
pub enum ClaimError {
    ClaimDenied(String),
    Auth,
    GameGone,
    /// CAS lost the event-length race; `claim_draw` retries before surfacing GameGone.
    Conflict,
    Corrupt(String),
    TooManyEvents,
    Redis(redis::RedisError),
}

impl ClaimError {
    pub fn reject_code(&self) -> ClaimRejectCode {
        match self {
            Self::ClaimDenied(_) => ClaimRejectCode::ClaimDenied,
            Self::Auth => ClaimRejectCode::Auth,
            Self::GameGone | Self::Conflict => ClaimRejectCode::GameGone,
            // Storage faults surface as CLAIM_DENIED to the client (same as offers).
            Self::Corrupt(_) | Self::TooManyEvents | Self::Redis(_) => ClaimRejectCode::ClaimDenied,
        }
    }
}

impl fmt::Display for ClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClaimDenied(msg) => write!(f, "{msg}"),
            Self::Auth => write!(f, "not a player in this game"),
            Self::GameGone => write!(f, "game not found"),
            Self::Conflict => write!(f, "claim conflict (CAS)"),
            Self::Corrupt(detail) => write!(f, "corrupt game state: {detail}"),
            Self::TooManyEvents => write!(f, "game event log exceeds max length"),
            Self::Redis(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ClaimError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Redis(err) => Some(err),
            _ => None,
        }
    }
}

impl From<GameLogError> for ClaimError {
    fn from(value: GameLogError) -> Self {
        match value {
            GameLogError::GameGone | GameLogError::InvalidGameId => Self::GameGone,
            GameLogError::TooManyEvents => Self::TooManyEvents,
            GameLogError::Redis(err) => Self::Redis(err),
            GameLogError::CorruptEvent(detail) | GameLogError::IllegalMove(detail) => {
                Self::Corrupt(detail)
            }
            GameLogError::InvalidFen(err) => Self::Corrupt(err.to_string()),
            GameLogError::InvalidUci(err) => Self::Corrupt(err.to_string()),
            GameLogError::GameExists => Self::Corrupt("unexpected game exists".into()),
        }
    }
}

/// PROTOCOL `GameEnd` JSON for claim / auto draws (no `flagged`).
pub fn game_end_claim_json(game_id: &str, end: &GameEndOut, ts: u64) -> Value {
    let mut body = json!({
        "typ": "GameEnd",
        "v": PROTOCOL_V,
        "game_id": game_id,
        "result": end.result,
        "reason": end.reason,
        "seq": end.seq,
        "ts": ts
    });
    match end.winner {
        Some(w) => body["winner"] = json!(w),
        None => body["winner"] = Value::Null,
    }
    body
}

/// PROTOCOL `Error` for claim rejects.
pub fn claim_denied_json(id: Option<&str>, code: ClaimRejectCode, message: &str) -> Value {
    let mut body = json!({
        "typ": "Error",
        "v": PROTOCOL_V,
        "code": code.as_str(),
        "message": message
    });
    if let Some(id) = id {
        body["id"] = json!(id);
    }
    body
}

/// Replay net moves and collect repetition keys prior to the current position.
///
/// Keeps keys from the start of the game (FIDE-correct). Truncating after
/// irreversible moves is an optional optimisation only; full history never
/// under-counts and cannot falsely match after a capture or pawn move.
pub fn position_with_prior_keys(
    initial_fen: &str,
    events: &[GameEvent],
) -> Result<(Position, Vec<RepetitionKey>), ClaimError> {
    let mut pos =
        Position::from_fen(initial_fen).map_err(|err| ClaimError::Corrupt(err.to_string()))?;
    let mut prior = Vec::with_capacity(net_move_events(events).len());
    for event in net_move_events(events) {
        let GameEvent::Move { uci, .. } = event else {
            continue;
        };
        prior.push(pos.repetition_key());
        let mv = Move::from_uci(uci).map_err(|err| ClaimError::Corrupt(err.to_string()))?;
        pos = pos
            .try_make_move(mv)
            .ok_or_else(|| ClaimError::Corrupt(format!("illegal move in log: {uci}")))?;
    }
    Ok((pos, prior))
}

/// Reason string when the current position is claimable; prefers threefold over fifty.
pub fn claimable_reason(pos: &Position, prior: &[RepetitionKey]) -> Option<&'static str> {
    if pos.is_threefold(prior) {
        Some("threefold")
    } else if pos.can_claim_fifty_move() {
        Some("fifty")
    } else {
        None
    }
}

/// Auto-draw reason after a ply that produced `next`, with `prior_for_next` including the
/// pre-move position key. Prefers fivefold over seventy-five when both apply.
pub fn auto_draw_reason(next: &Position, prior_for_next: &[RepetitionKey]) -> Option<&'static str> {
    if next.is_fivefold(prior_for_next) {
        Some("fivefold")
    } else if next.is_seventy_five_move() {
        Some("seventy_five")
    } else {
        None
    }
}

fn seat_color(
    meta: &super::events::GameMeta,
    player_id: &str,
) -> Result<(Color, &'static str), ClaimError> {
    if player_id.is_empty() || player_id.len() > MAX_PLAYER_ID_LEN {
        return Err(ClaimError::Auth);
    }
    if meta.white_id.is_empty() || meta.black_id.is_empty() {
        return Err(ClaimError::Auth);
    }
    if player_id == meta.white_id {
        Ok((Color::White, "w"))
    } else if player_id == meta.black_id {
        Ok((Color::Black, "b"))
    } else {
        Err(ClaimError::Auth)
    }
}

fn map_cas(cas: CasCommitResult) -> Result<u64, ClaimError> {
    match cas {
        CasCommitResult::Ok { last_seq, .. } => Ok(last_seq),
        CasCommitResult::Conflict { .. } => Err(ClaimError::Conflict),
        CasCommitResult::Gone => Err(ClaimError::GameGone),
        CasCommitResult::TooManyEvents => Err(ClaimError::TooManyEvents),
        CasCommitResult::Corrupt(detail) => Err(ClaimError::Corrupt(detail)),
    }
}

/// Claim a draw by threefold or fifty-move rule. Only the side to move may claim.
pub async fn claim_draw(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    ts: u64,
) -> Result<GameEndOut, ClaimError> {
    for attempt in 0..MAX_CAS_RETRIES {
        match claim_draw_once(conn, game_id, player_id, ts).await {
            Ok(out) => return Ok(out),
            Err(ClaimError::Conflict) if attempt + 1 < MAX_CAS_RETRIES => continue,
            Err(ClaimError::Conflict) => return Err(ClaimError::GameGone),
            Err(err) => return Err(err),
        }
    }
    Err(ClaimError::GameGone)
}

async fn claim_draw_once(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    ts: u64,
) -> Result<GameEndOut, ClaimError> {
    let mut meta = load_meta(conn, game_id).await?;
    if meta.ended {
        return Err(ClaimError::GameGone);
    }
    let (seat, by) = seat_color(&meta, player_id)?;
    let events = load_events(conn, game_id).await?;
    let (pos, prior) = position_with_prior_keys(&meta.initial_fen, &events)?;

    if pos.side_to_move() != seat {
        return Err(ClaimError::ClaimDenied("not your turn to claim".into()));
    }

    let Some(reason) = claimable_reason(&pos, &prior) else {
        return Err(ClaimError::ClaimDenied(
            "position is not claimable by threefold or fifty-move".into(),
        ));
    };

    let expected_len = events.len() as u64;
    let seq = expected_len + 1;
    meta.pending_offer = None;
    meta.ended = true;
    meta.ended_reason = Some(reason.to_owned());
    meta.active_started_ms = 0;
    let event = GameEvent::ClaimDraw {
        seq,
        by: by.to_owned(),
        kind: reason.to_owned(),
        ts,
    };
    let last_seq =
        map_cas(commit_append_cas(conn, game_id, expected_len, &meta, &event, None).await?)?;

    Ok(GameEndOut {
        result: "1/2-1/2",
        winner: None,
        reason,
        seq: last_seq,
    })
}

/// Append an `AutoDraw` audit event after a ply that already marked `meta.ended`.
///
/// `apply_move` sets `ended` + `ended_reason` before the Move append so a failed
/// AutoDraw write cannot leave the game live on a terminal position.
pub async fn record_auto_draw(
    conn: &mut ConnectionManager,
    game_id: &str,
    reason: &'static str,
    ts: u64,
) -> Result<GameEndOut, ClaimError> {
    let meta = load_meta(conn, game_id).await?;
    if !meta.ended || meta.ended_reason.as_deref() != Some(reason) {
        return Err(ClaimError::Corrupt(
            "auto draw requires meta already ended with matching reason".into(),
        ));
    }
    let seq = append_game_event(
        conn,
        game_id,
        GameEvent::AutoDraw {
            seq: 0,
            kind: reason.to_owned(),
            ts,
        },
    )
    .await?;

    Ok(GameEndOut {
        result: "1/2-1/2",
        winner: None,
        reason,
        seq,
    })
}
