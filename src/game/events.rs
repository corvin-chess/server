//! Game metadata and append-only event payloads stored in Redis.

use serde::{Deserialize, Serialize};

/// Standard chess starting position FEN (six fields).
pub const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

/// Durable-enough live meta for a Redis game (initial FEN + seats + clocks).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameMeta {
    pub initial_fen: String,
    pub created_ms: u64,
    /// Opaque seat token for White (session/user id). Empty until seated.
    #[serde(default)]
    pub white_id: String,
    /// Opaque seat token for Black (session/user id). Empty until seated.
    #[serde(default)]
    pub black_id: String,
    /// Remaining White clock ms (PROTOCOL `clocks.w`).
    #[serde(default)]
    pub clock_w_ms: u64,
    /// Remaining Black clock ms (PROTOCOL `clocks.b`).
    #[serde(default)]
    pub clock_b_ms: u64,
    /// Fischer increment added after each accepted move (ms).
    #[serde(default)]
    pub increment_ms: u64,
    /// Side whose clock is running (`"w"` / `"b"`); empty → derive from FEN.
    #[serde(default)]
    pub active: String,
    /// Server unix-ms when `active` began thinking; `0` = not started (no elapsed).
    /// GameStart / matchmaking must set this when clocks begin; until then flag claims stay NotFlagged.
    #[serde(default)]
    pub active_started_ms: u64,
    /// True after a terminal result (flag, resign, …) has been recorded.
    #[serde(default)]
    pub ended: bool,
    /// Short reason when `ended` (e.g. `flag`); omitted while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_reason: Option<String>,
}

impl Default for GameMeta {
    fn default() -> Self {
        Self {
            initial_fen: START_FEN.to_owned(),
            created_ms: 0,
            white_id: String::new(),
            black_id: String::new(),
            clock_w_ms: 0,
            clock_b_ms: 0,
            increment_ms: 0,
            active: String::new(),
            active_started_ms: 0,
            ended: false,
            ended_reason: None,
        }
    }
}

/// A single ply applied to the position (UCI long algebraic).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveEvent {
    pub seq: u64,
    pub uci: String,
    pub ts: u64,
}

/// Append-only game events. Only `Move` changes the board for FEN replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "typ")]
pub enum GameEvent {
    #[serde(rename = "Move")]
    Move { seq: u64, uci: String, ts: u64 },
    /// Non-position note (pairing, admin). Ignored by FEN replay.
    ServerNote { seq: u64, text: String, ts: u64 },
}

impl GameEvent {
    pub fn seq(&self) -> u64 {
        match self {
            Self::Move { seq, .. } | Self::ServerNote { seq, .. } => *seq,
        }
    }

    pub fn move_uci(seq: u64, uci: impl Into<String>, ts: u64) -> Self {
        Self::Move {
            seq,
            uci: uci.into(),
            ts,
        }
    }
}

impl From<MoveEvent> for GameEvent {
    fn from(value: MoveEvent) -> Self {
        Self::Move {
            seq: value.seq,
            uci: value.uci,
            ts: value.ts,
        }
    }
}
