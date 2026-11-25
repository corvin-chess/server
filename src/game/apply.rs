//! Legal move apply with PROTOCOL MoveApplied / MoveRejected mapping (CV-162).
//! Clocks tick + Fischer increment + flag-before-increment (CV-163 / ADR-005).
//! Lag compensation from server-measured RTT (CV-164); never trusts client send time.
//! Fivefold / seventy-five auto draws after a successful ply (CV-166).
//! Redis Lua CAS commit so concurrent apply cannot double-append (CV-171).

use std::fmt;

use corvin_chess_core::{Color, Move, UciError};
use redis::aio::ConnectionManager;
use serde_json::{json, Value};

use super::claims::{auto_draw_reason, position_with_prior_keys};
use super::clock::{
    flag_outcome_for_position, is_flagged_at_compensated, tick_meta_on_move_compensated,
    write_flag_to_meta, write_tick_to_meta, ClockPair, ClockTick, FlagOutcome,
};
use super::events::GameEvent;
use super::lag::{credit_lag, LagCredit};
use super::offers::GameEndOut;
use super::store::{
    commit_apply_cas, commit_meta_cas, load_events, load_meta, CasCommitResult, GameLogError,
    MAX_PLAYER_ID_LEN, MAX_UCI_LEN,
};

const PROTOCOL_V: u32 = 1;
/// Bounded CAS retries when another writer wins the event-length race.
const MAX_CAS_RETRIES: u32 = 8;

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
    /// Lag credit applied this ply (ms); `None` when zero / no RTT sample.
    pub lag_comp_ms: Option<u64>,
    /// Set when the ply triggers an automatic draw (fivefold / seventy-five).
    pub auto_draw: Option<GameEndOut>,
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

/// Errors from seating / legality / clock checks before or during apply.
#[derive(Debug)]
pub enum ApplyError {
    IllegalMove(String),
    NotYourTurn,
    GameGone,
    Auth,
    /// Active side flagged at `ts` before the move could be accepted.
    Flagged {
        outcome: FlagOutcome,
    },
    /// CAS lost the event-length race; apply retries internally before surfacing.
    Conflict,
    InvalidUci(UciError),
    Corrupt(String),
    TooManyEvents,
    Redis(redis::RedisError),
}

impl ApplyError {
    /// PROTOCOL `MoveRejected.code` when this error is a client reject; `None` → `Error`/`GameEnd`.
    pub fn reject_code(&self) -> Option<MoveRejectCode> {
        match self {
            Self::IllegalMove(_) | Self::InvalidUci(_) => Some(MoveRejectCode::IllegalMove),
            Self::NotYourTurn => Some(MoveRejectCode::NotYourTurn),
            Self::GameGone => Some(MoveRejectCode::GameGone),
            Self::Auth => Some(MoveRejectCode::Auth),
            // Flag mid-apply is GameEnd, not MoveRejected. Storage faults → Error INTERNAL.
            Self::Flagged { .. }
            | Self::Conflict
            | Self::Corrupt(_)
            | Self::TooManyEvents
            | Self::Redis(_) => None,
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
            Self::Flagged { outcome } => write!(f, "flagged: {}", outcome.flagged),
            Self::Conflict => write!(f, "apply conflict (CAS)"),
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
    lag_comp_ms: Option<u64>,
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
    if let Some(comp) = lag_comp_ms.filter(|ms| *ms > 0) {
        body["lag_comp_ms"] = json!(comp);
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
/// Deducts elapsed think time from the mover, adds Fischer `increment_ms`, then
/// starts the opponent clock at `ts`. Flags before increment (ADR-005).
/// Commits via Redis Lua CAS on event length (CV-171).
pub async fn apply_move(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    uci: &str,
    ts: u64,
) -> Result<AppliedMove, ApplyError> {
    apply_move_with_rtt(conn, game_id, player_id, uci, ts, None).await
}

/// Like `apply_move`, with injectable server-measured RTT for lag compensation (CV-164).
///
/// `rtt_ms` is a full round-trip; one-way estimate is `rtt_ms / 2`. Missing RTT → no credit.
pub async fn apply_move_with_rtt(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    uci: &str,
    ts: u64,
    rtt_ms: Option<u64>,
) -> Result<AppliedMove, ApplyError> {
    if player_id.is_empty() || player_id.len() > MAX_PLAYER_ID_LEN {
        return Err(ApplyError::Auth);
    }
    if uci.is_empty() || uci.len() > MAX_UCI_LEN {
        return Err(ApplyError::InvalidUci(UciError::Invalid));
    }

    for attempt in 0..MAX_CAS_RETRIES {
        match try_apply_once(conn, game_id, player_id, uci, ts, rtt_ms).await {
            Ok(applied) => return Ok(applied),
            Err(ApplyError::Conflict) if attempt + 1 < MAX_CAS_RETRIES => continue,
            Err(ApplyError::Conflict) => {
                // Last resort: map exhausted CAS races to a client reject, not INTERNAL.
                return resolve_exhausted_conflict(conn, game_id, player_id).await;
            }
            Err(err) => return Err(err),
        }
    }
    resolve_exhausted_conflict(conn, game_id, player_id).await
}

async fn resolve_exhausted_conflict(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
) -> Result<AppliedMove, ApplyError> {
    let meta = load_meta(conn, game_id).await?;
    if meta.ended {
        return Err(ApplyError::GameGone);
    }
    let events = load_events(conn, game_id).await?;
    let (pos, _) = position_with_prior_keys(&meta.initial_fen, &events)
        .map_err(|err| ApplyError::Corrupt(err.to_string()))?;
    let seat = if player_id == meta.white_id {
        Color::White
    } else if player_id == meta.black_id {
        Color::Black
    } else {
        return Err(ApplyError::Auth);
    };
    if pos.side_to_move() != seat {
        return Err(ApplyError::NotYourTurn);
    }
    Err(ApplyError::Conflict)
}

async fn try_apply_once(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    uci: &str,
    ts: u64,
    rtt_ms: Option<u64>,
) -> Result<AppliedMove, ApplyError> {
    let mut meta = load_meta(conn, game_id).await?;
    if meta.ended {
        return Err(ApplyError::GameGone);
    }
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
    let expected_len = events.len() as u64;
    // Same walk as FEN replay, but keep prior repetition keys for post-ply auto draws.
    let (pos, prior) = position_with_prior_keys(&meta.initial_fen, &events)
        .map_err(|err| ApplyError::Corrupt(err.to_string()))?;

    if pos.side_to_move() != seat {
        return Err(ApplyError::NotYourTurn);
    }

    let board_active = color_active(seat);
    let seat_quota = match seat {
        Color::White => meta.lag_quota_w_ms,
        Color::Black => meta.lag_quota_b_ms,
    };
    // Server-measured RTT only; one-way = RTT/2; never trust client send time (ADR-005).
    let one_way = rtt_ms
        .filter(|rtt| *rtt > 0 && *rtt <= 60_000)
        .map(|rtt| rtt / 2)
        .unwrap_or(0);
    let LagCredit {
        credit_ms: lag_comp,
        remaining_quota_ms: new_quota,
    } = credit_lag(one_way, seat_quota);

    let peek = ClockPair {
        w_ms: meta.clock_w_ms,
        b_ms: meta.clock_b_ms,
        increment_ms: meta.increment_ms,
        active: if meta.active.is_empty() {
            board_active.to_owned()
        } else {
            meta.active.clone()
        },
        active_started_ms: meta.active_started_ms,
    };
    let fen = pos.to_fen();
    // Flag before legality: out-of-time ends the game even on an illegal UCI.
    if let Some(color) = is_flagged_at_compensated(&peek, ts, lag_comp) {
        let outcome = flag_outcome_for_position(&fen, color).map_err(ApplyError::Corrupt)?;
        let mut zeroed = peek;
        if color == "w" {
            zeroed.w_ms = 0;
        } else {
            zeroed.b_ms = 0;
        }
        write_flag_to_meta(&mut meta, color, &zeroed);
        meta.pending_offer = None;
        meta.ended_reason = Some(format!("flag:{}", outcome.result));
        // CAS meta-only so a concurrent ply cannot be overwritten by stale flag meta.
        match commit_meta_cas(conn, game_id, expected_len, &meta).await? {
            CasCommitResult::Ok { .. } => return Err(ApplyError::Flagged { outcome }),
            CasCommitResult::Conflict { .. } => return Err(ApplyError::Conflict),
            CasCommitResult::Gone => return Err(ApplyError::GameGone),
            CasCommitResult::TooManyEvents => return Err(ApplyError::TooManyEvents),
            CasCommitResult::Corrupt(detail) => return Err(ApplyError::Corrupt(detail)),
        }
    }

    // Legality before clock commit so illegal attempts do not mutate Redis clocks.
    let mv = Move::from_uci(uci).map_err(ApplyError::InvalidUci)?;
    let san = pos
        .to_san(mv)
        .map_err(|_| ApplyError::IllegalMove(uci.to_owned()))?;
    let next = pos
        .try_make_move(mv)
        .ok_or_else(|| ApplyError::IllegalMove(uci.to_owned()))?;
    let next_fen = next.to_fen();

    // Snapshot clocks + lag quotas for one-ply takeback; clear pending offers (CV-165).
    meta.prev_clock_w_ms = meta.clock_w_ms;
    meta.prev_clock_b_ms = meta.clock_b_ms;
    meta.prev_active = if meta.active.is_empty() {
        board_active.to_owned()
    } else {
        meta.active.clone()
    };
    meta.prev_active_started_ms = meta.active_started_ms;
    meta.prev_lag_quota_w_ms = meta.lag_quota_w_ms;
    meta.prev_lag_quota_b_ms = meta.lag_quota_b_ms;
    meta.pending_offer = None;

    // Deduct effective elapsed then add Fischer increment (ADR-005).
    match tick_meta_on_move_compensated(&meta, seat, ts, lag_comp) {
        ClockTick::Flagged { color, clocks } => {
            // Same `ts` as peek should not flag; treat as internal fault.
            let outcome = flag_outcome_for_position(&fen, color).map_err(ApplyError::Corrupt)?;
            write_flag_to_meta(&mut meta, color, &clocks);
            meta.pending_offer = None;
            meta.ended_reason = Some(format!("flag:{}", outcome.result));
            match commit_meta_cas(conn, game_id, expected_len, &meta).await? {
                CasCommitResult::Ok { .. } => return Err(ApplyError::Flagged { outcome }),
                CasCommitResult::Conflict { .. } => return Err(ApplyError::Conflict),
                CasCommitResult::Gone => return Err(ApplyError::GameGone),
                CasCommitResult::TooManyEvents => return Err(ApplyError::TooManyEvents),
                CasCommitResult::Corrupt(detail) => return Err(ApplyError::Corrupt(detail)),
            }
        }
        ClockTick::Applied(pair) => {
            write_tick_to_meta(&mut meta, &pair);
        }
    }

    match seat {
        Color::White => {
            meta.lag_quota_w_ms = new_quota;
            if one_way > 0 {
                meta.lag_one_way_w_ms = one_way;
            }
        }
        Color::Black => {
            meta.lag_quota_b_ms = new_quota;
            if one_way > 0 {
                meta.lag_one_way_b_ms = one_way;
            }
        }
    }

    // Align meta.active with post-move side to move (tick already switched).
    meta.active = color_active(next.side_to_move()).to_owned();

    let mut prior_for_next = prior;
    prior_for_next.push(pos.repetition_key());
    // Detect auto-draw before Redis writes so meta.ended is set with the Move CAS
    // (Move + AutoDraw share one Lua script; no live fivefold/seventy-five gap).
    let auto_reason = auto_draw_reason(&next, &prior_for_next);
    if let Some(reason) = auto_reason {
        meta.pending_offer = None;
        meta.ended = true;
        meta.ended_reason = Some(reason.to_owned());
        meta.active_started_ms = 0;
    }

    let move_seq = expected_len + 1;
    let move_event = GameEvent::move_uci(move_seq, uci, ts);
    let auto_event = auto_reason.map(|reason| GameEvent::AutoDraw {
        seq: move_seq + 1,
        kind: reason.to_owned(),
        ts,
    });

    // Atomic SET meta + RPUSH Move (+ AutoDraw): concurrent apply loses CAS, retries.
    let cas = commit_apply_cas(
        conn,
        game_id,
        expected_len,
        &meta,
        &move_event,
        auto_event.as_ref(),
    )
    .await?;

    match cas {
        CasCommitResult::Ok {
            move_seq: seq,
            last_seq,
        } => {
            let auto_draw = auto_reason.map(|reason| GameEndOut {
                result: "1/2-1/2",
                winner: None,
                reason,
                seq: last_seq,
            });
            Ok(AppliedMove {
                uci: uci.to_owned(),
                san,
                fen: next_fen,
                clock_w_ms: meta.clock_w_ms,
                clock_b_ms: meta.clock_b_ms,
                active: color_active(next.side_to_move()),
                seq,
                lag_comp_ms: if lag_comp > 0 { Some(lag_comp) } else { None },
                auto_draw,
            })
        }
        CasCommitResult::Conflict { .. } => Err(ApplyError::Conflict),
        CasCommitResult::Gone => Err(ApplyError::GameGone),
        CasCommitResult::TooManyEvents => Err(ApplyError::TooManyEvents),
        CasCommitResult::Corrupt(detail) => Err(ApplyError::Corrupt(detail)),
    }
}
