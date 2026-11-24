//! Game metadata and append-only event payloads stored in Redis.

use serde::{Deserialize, Serialize};

/// Standard chess starting position FEN (six fields).
pub const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

/// Durable-enough live meta for a Redis game (initial FEN + seats + stub clocks).
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
    /// Remaining White clock ms (stub until CV-163 ticks).
    #[serde(default)]
    pub clock_w_ms: u64,
    /// Remaining Black clock ms (stub until CV-163 ticks).
    #[serde(default)]
    pub clock_b_ms: u64,
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
