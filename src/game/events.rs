//! Game metadata and append-only event payloads stored in Redis.

use serde::{Deserialize, Serialize};

/// Standard chess starting position FEN (six fields).
pub const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

/// Durable-enough live meta for a Redis game (initial FEN + create time).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameMeta {
    pub initial_fen: String,
    pub created_ms: u64,
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
