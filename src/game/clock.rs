//! Server-authoritative clocks: Fischer increment + flag (CV-163 / ADR-005).
//!
//! Times are milliseconds to match PROTOCOL.md (`clocks.w` / `clocks.b`).
//! ADR-005's "centiseconds" wording is mapped 1:1 onto these ms fields.
//! Lag compensation quota is CV-164; this module never trusts client RTT.

use std::fmt;

use corvin_chess_core::{Color, Position};
use redis::aio::ConnectionManager;
use serde_json::{json, Value};

use super::replay::replay_fen;
use super::store::{load_events, load_meta, save_meta, GameLogError};

const PROTOCOL_V: u32 = 1;

/// Mutable clock pair for one live game (ms remaining + Fischer increment).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockPair {
    pub w_ms: u64,
    pub b_ms: u64,
    pub increment_ms: u64,
    /// Side whose clock is running (`"w"` / `"b"`).
    pub active: String,
    /// Server unix-ms when `active` began thinking; `0` means not started (no elapsed).
    pub active_started_ms: u64,
}

/// Result of stopping the active clock on a legal move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClockTick {
    Applied(ClockPair),
    Flagged {
        color: &'static str,
        clocks: ClockPair,
    },
}

/// Outcome of a verified flag (PROTOCOL `GameEnd` fields).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlagOutcome {
    pub result: &'static str,
    pub winner: Option<&'static str>,
    pub reason: &'static str,
    pub flagged: &'static str,
}

/// Errors from `claim_flag` / `check_flag` Redis path.
#[derive(Debug)]
pub enum FlagClaimError {
    GameGone,
    NotFlagged,
    Auth,
    Corrupt(String),
    Redis(redis::RedisError),
}

impl fmt::Display for FlagClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GameGone => write!(f, "game not found or already ended"),
            Self::NotFlagged => write!(f, "claimed side still has time"),
            Self::Auth => write!(f, "claimant is not the opposing seat"),
            Self::Corrupt(detail) => write!(f, "corrupt game state: {detail}"),
            Self::Redis(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for FlagClaimError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Redis(err) => Some(err),
            _ => None,
        }
    }
}

impl From<GameLogError> for FlagClaimError {
    fn from(value: GameLogError) -> Self {
        match value {
            GameLogError::GameGone | GameLogError::InvalidGameId => Self::GameGone,
            GameLogError::Redis(err) => Self::Redis(err),
            other => Self::Corrupt(other.to_string()),
        }
    }
}

fn parse_active(active: &str) -> Option<&'static str> {
    match active {
        "w" => Some("w"),
        "b" => Some("b"),
        _ => None,
    }
}

fn elapsed_ms(started: u64, now_ms: u64) -> u64 {
    if started == 0 {
        return 0;
    }
    now_ms.saturating_sub(started)
}

/// Remaining ms at `now_ms` (active side counts down; opponent frozen).
pub fn remaining_at(clocks: &ClockPair, now_ms: u64) -> (u64, u64) {
    let elapsed = elapsed_ms(clocks.active_started_ms, now_ms);
    match clocks.active.as_str() {
        "w" => (clocks.w_ms.saturating_sub(elapsed), clocks.b_ms),
        "b" => (clocks.w_ms, clocks.b_ms.saturating_sub(elapsed)),
        _ => (clocks.w_ms, clocks.b_ms),
    }
}

/// Stop the active clock at `now_ms`: flag if depleted, else deduct + Fischer increment + switch.
pub fn tick_on_move(clocks: &ClockPair, now_ms: u64) -> ClockTick {
    // Corrupt active → treat as white so callers stay deterministic.
    let active = parse_active(&clocks.active).unwrap_or("w");
    let elapsed = elapsed_ms(clocks.active_started_ms, now_ms);
    let (mut w, mut b) = (clocks.w_ms, clocks.b_ms);
    let active_remaining = if active == "w" { w } else { b };

    if active_remaining <= elapsed {
        if active == "w" {
            w = 0;
        } else {
            b = 0;
        }
        return ClockTick::Flagged {
            color: active,
            clocks: ClockPair {
                w_ms: w,
                b_ms: b,
                increment_ms: clocks.increment_ms,
                active: active.to_owned(),
                active_started_ms: clocks.active_started_ms,
            },
        };
    }

    if active == "w" {
        w = w - elapsed + clocks.increment_ms;
    } else {
        b = b - elapsed + clocks.increment_ms;
    }
    let next_active = if active == "w" { "b" } else { "w" };
    ClockTick::Applied(ClockPair {
        w_ms: w,
        b_ms: b,
        increment_ms: clocks.increment_ms,
        active: next_active.to_owned(),
        active_started_ms: now_ms,
    })
}

/// Whether the active side is already out of time at `now_ms`.
pub fn is_flagged_at(clocks: &ClockPair, now_ms: u64) -> Option<&'static str> {
    let active = parse_active(&clocks.active)?;
    if clocks.active_started_ms == 0 {
        return None;
    }
    let elapsed = elapsed_ms(clocks.active_started_ms, now_ms);
    let stock = if active == "w" {
        clocks.w_ms
    } else {
        clocks.b_ms
    };
    if stock <= elapsed {
        Some(active)
    } else {
        None
    }
}

/// Build PROTOCOL `ClockSync` JSON.
#[allow(clippy::too_many_arguments)]
pub fn clock_sync_json(
    game_id: &str,
    clock_w_ms: u64,
    clock_b_ms: u64,
    active: &str,
    server_time: u64,
    you_ms: u64,
    opp_ms: u64,
    server_ms: u64,
) -> Value {
    json!({
        "typ": "ClockSync",
        "v": PROTOCOL_V,
        "game_id": game_id,
        "clocks": { "w": clock_w_ms, "b": clock_b_ms },
        "active": active,
        "server_time": server_time,
        "lag": {
            "you_ms": you_ms,
            "opp_ms": opp_ms,
            "server_ms": server_ms
        }
    })
}

/// Build PROTOCOL `GameEnd` JSON for a flag outcome.
pub fn game_end_flag_json(game_id: &str, outcome: &FlagOutcome, ts: u64) -> Value {
    let mut body = json!({
        "typ": "GameEnd",
        "v": PROTOCOL_V,
        "game_id": game_id,
        "result": outcome.result,
        "reason": outcome.reason,
        "flagged": outcome.flagged,
        "ts": ts
    });
    if let Some(winner) = outcome.winner {
        body["winner"] = json!(winner);
    } else {
        body["winner"] = Value::Null;
    }
    body
}

/// Resolve flag result using chess-core mating potential (timeout draw when opp cannot mate).
pub fn flag_outcome_for_position(fen: &str, flagged: &str) -> Result<FlagOutcome, String> {
    let flagged = parse_active(flagged).ok_or_else(|| "invalid flagged color".to_owned())?;
    let pos = Position::from_fen(fen).map_err(|err| err.to_string())?;
    let winner_color = if flagged == "w" {
        Color::Black
    } else {
        Color::White
    };
    // If the side that would win cannot mate, FIDE-style timeout is a draw.
    if !pos.has_mating_potential(winner_color) {
        return Ok(FlagOutcome {
            result: "1/2-1/2",
            winner: None,
            reason: "flag",
            flagged,
        });
    }
    let (result, winner) = if flagged == "w" {
        ("0-1", "b")
    } else {
        ("1-0", "w")
    };
    Ok(FlagOutcome {
        result,
        winner: Some(winner),
        reason: "flag",
        flagged,
    })
}

fn pair_from_meta(meta: &super::events::GameMeta, active_from_fen: &str) -> ClockPair {
    let active = if meta.active.is_empty() {
        active_from_fen.to_owned()
    } else {
        meta.active.clone()
    };
    ClockPair {
        w_ms: meta.clock_w_ms,
        b_ms: meta.clock_b_ms,
        increment_ms: meta.increment_ms,
        active,
        active_started_ms: meta.active_started_ms,
    }
}

fn apply_pair_to_meta(meta: &mut super::events::GameMeta, pair: &ClockPair) {
    meta.clock_w_ms = pair.w_ms;
    meta.clock_b_ms = pair.b_ms;
    meta.increment_ms = pair.increment_ms;
    meta.active = pair.active.clone();
    meta.active_started_ms = pair.active_started_ms;
}

/// Peek whether the active side is flagged at `now_ms` without mutating (loads Redis).
pub async fn check_flag(
    conn: &mut ConnectionManager,
    game_id: &str,
    now_ms: u64,
) -> Result<Option<&'static str>, FlagClaimError> {
    let meta = load_meta(conn, game_id).await?;
    if meta.ended {
        return Ok(None);
    }
    let events = load_events(conn, game_id).await?;
    let fen = replay_fen(&meta.initial_fen, &events)
        .map_err(|err| FlagClaimError::Corrupt(err.to_string()))?;
    let pos = Position::from_fen(&fen).map_err(|err| FlagClaimError::Corrupt(err.to_string()))?;
    let active = match pos.side_to_move() {
        Color::White => "w",
        Color::Black => "b",
    };
    let clocks = pair_from_meta(&meta, active);
    Ok(is_flagged_at(&clocks, now_ms))
}

/// Verify a PROTOCOL `Flag` claim, persist ended meta, return `GameEnd` fields.
pub async fn claim_flag(
    conn: &mut ConnectionManager,
    game_id: &str,
    claimant_id: &str,
    claimed_color: &str,
    now_ms: u64,
) -> Result<FlagOutcome, FlagClaimError> {
    if claimant_id.is_empty() || claimant_id.len() > super::store::MAX_PLAYER_ID_LEN {
        return Err(FlagClaimError::Auth);
    }
    let claimed = parse_active(claimed_color).ok_or(FlagClaimError::NotFlagged)?;
    let mut meta = load_meta(conn, game_id).await?;
    if meta.ended {
        return Err(FlagClaimError::GameGone);
    }
    if meta.white_id.is_empty() || meta.black_id.is_empty() {
        return Err(FlagClaimError::Auth);
    }
    // Claimant must be the opposing seat (cannot flag yourself).
    let claimant_ok = match claimed {
        "w" => claimant_id == meta.black_id,
        "b" => claimant_id == meta.white_id,
        _ => false,
    };
    if !claimant_ok {
        return Err(FlagClaimError::Auth);
    }

    let events = load_events(conn, game_id).await?;
    let fen = replay_fen(&meta.initial_fen, &events)
        .map_err(|err| FlagClaimError::Corrupt(err.to_string()))?;
    let pos = Position::from_fen(&fen).map_err(|err| FlagClaimError::Corrupt(err.to_string()))?;
    let board_active = match pos.side_to_move() {
        Color::White => "w",
        Color::Black => "b",
    };
    // Only the side to move's clock runs; claiming the idle side is invalid.
    if claimed != board_active {
        return Err(FlagClaimError::NotFlagged);
    }

    let clocks = pair_from_meta(&meta, board_active);
    let Some(flagged) = is_flagged_at(&clocks, now_ms) else {
        return Err(FlagClaimError::NotFlagged);
    };
    if flagged != claimed {
        return Err(FlagClaimError::NotFlagged);
    }

    let outcome = flag_outcome_for_position(&fen, flagged).map_err(FlagClaimError::Corrupt)?;

    // Persist zeroed active clock + ended marker.
    let (w, b) = remaining_at(&clocks, now_ms);
    meta.clock_w_ms = if flagged == "w" { 0 } else { w };
    meta.clock_b_ms = if flagged == "b" { 0 } else { b };
    meta.ended = true;
    meta.ended_reason = Some("flag".to_owned());
    save_meta(conn, game_id, &meta).await?;
    Ok(outcome)
}

/// Tick clocks for a successful move; returns updated pair or flagged color.
/// Used by `apply_move`; persists via caller after append.
pub fn tick_meta_on_move(
    meta: &super::events::GameMeta,
    board_active: Color,
    now_ms: u64,
) -> ClockTick {
    let active = match board_active {
        Color::White => "w",
        Color::Black => "b",
    };
    let clocks = pair_from_meta(meta, active);
    tick_on_move(&clocks, now_ms)
}

/// Write clock fields from a successful tick into meta (caller saves).
pub fn write_tick_to_meta(meta: &mut super::events::GameMeta, pair: &ClockPair) {
    apply_pair_to_meta(meta, pair);
    meta.ended = false;
}

/// Write a flag into meta (caller saves).
pub fn write_flag_to_meta(meta: &mut super::events::GameMeta, color: &str, pair: &ClockPair) {
    apply_pair_to_meta(meta, pair);
    if color == "w" {
        meta.clock_w_ms = 0;
    } else {
        meta.clock_b_ms = 0;
    }
    meta.ended = true;
    meta.ended_reason = Some("flag".to_owned());
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn flag_outcome_startpos_is_win_for_opponent() {
        let out = flag_outcome_for_position(super::super::events::START_FEN, "w").expect("ok");
        assert_eq!(out.result, "0-1");
        assert_eq!(out.winner, Some("b"));
    }

    #[test]
    fn parse_active_helpers() {
        assert_eq!(parse_active("w"), Some("w"));
        assert_eq!(parse_active("x"), None);
    }
}
