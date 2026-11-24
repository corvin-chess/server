//! Game metadata and append-only event payloads stored in Redis.

use serde::{Deserialize, Serialize};

/// Standard chess starting position FEN (six fields).
pub const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

/// Pending draw or takeback offer (cleared on move apply or response).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOffer {
    /// `"draw"` or `"takeback"`.
    pub kind: String,
    /// Seat that created the offer (`"w"` / `"b"`).
    pub by: String,
}

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
    /// Remaining lag compensation quota for White (ms); seed via `seed_lag_quotas` / CV-164.
    #[serde(default)]
    pub lag_quota_w_ms: u64,
    /// Remaining lag compensation quota for Black (ms).
    #[serde(default)]
    pub lag_quota_b_ms: u64,
    /// Last server-measured one-way lag estimate for White (ms); used by flag claims.
    #[serde(default)]
    pub lag_one_way_w_ms: u64,
    /// Last server-measured one-way lag estimate for Black (ms).
    #[serde(default)]
    pub lag_one_way_b_ms: u64,
    /// Time class for abort first-move windows (ADR-005): bullet/blitz/rapid/classical/correspondence.
    #[serde(default = "default_time_class")]
    pub time_class: String,
    /// When true, a terminal finish updates Glicko-2 once (CV-168). Abort never rates.
    #[serde(default)]
    pub rated: bool,
    /// Open draw/takeback offer, if any (CV-165).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_offer: Option<PendingOffer>,
    /// Clock snapshot before the last accepted ply (one-ply takeback restore).
    #[serde(default)]
    pub prev_clock_w_ms: u64,
    #[serde(default)]
    pub prev_clock_b_ms: u64,
    #[serde(default)]
    pub prev_active: String,
    #[serde(default)]
    pub prev_active_started_ms: u64,
    /// Lag quota snapshot before the last accepted ply (restored on takeback).
    #[serde(default)]
    pub prev_lag_quota_w_ms: u64,
    #[serde(default)]
    pub prev_lag_quota_b_ms: u64,
    /// True after a terminal result (flag, resign, …) has been recorded.
    #[serde(default)]
    pub ended: bool,
    /// Short reason when `ended` (e.g. `flag`); omitted while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_reason: Option<String>,
}

fn default_time_class() -> String {
    "blitz".to_owned()
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
            lag_quota_w_ms: 0,
            lag_quota_b_ms: 0,
            lag_one_way_w_ms: 0,
            lag_one_way_b_ms: 0,
            time_class: default_time_class(),
            rated: false,
            pending_offer: None,
            prev_clock_w_ms: 0,
            prev_clock_b_ms: 0,
            prev_active: String::new(),
            prev_active_started_ms: 0,
            prev_lag_quota_w_ms: 0,
            prev_lag_quota_b_ms: 0,
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

/// Append-only game events. Only `Move` changes the board for FEN replay
/// (and `Takeback` undoes the latest net Move; see `replay_fen`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "typ")]
pub enum GameEvent {
    #[serde(rename = "Move")]
    Move {
        seq: u64,
        uci: String,
        ts: u64,
    },
    /// Non-position note (pairing, admin). Ignored by FEN replay.
    ServerNote {
        seq: u64,
        text: String,
        ts: u64,
    },
    /// Draw or takeback offered (audit; pending also mirrored on meta).
    Offer {
        seq: u64,
        kind: String,
        by: String,
        ts: u64,
    },
    /// Accept/decline/withdraw of a pending offer.
    OfferResponse {
        seq: u64,
        kind: String,
        by: String,
        accept: bool,
        ts: u64,
    },
    Resign {
        seq: u64,
        by: String,
        ts: u64,
    },
    Abort {
        seq: u64,
        by: String,
        ts: u64,
    },
    /// Undo the net Move with `undone_seq` (replay pops that ply).
    Takeback {
        seq: u64,
        undone_seq: u64,
        ts: u64,
    },
    /// Player claimed threefold or fifty-move (CV-166).
    ClaimDraw {
        seq: u64,
        by: String,
        /// `"threefold"` or `"fifty"`.
        kind: String,
        ts: u64,
    },
    /// Automatic fivefold or seventy-five draw after a ply (CV-166).
    AutoDraw {
        seq: u64,
        /// `"fivefold"` or `"seventy_five"`.
        kind: String,
        ts: u64,
    },
}

impl GameEvent {
    pub fn seq(&self) -> u64 {
        match self {
            Self::Move { seq, .. }
            | Self::ServerNote { seq, .. }
            | Self::Offer { seq, .. }
            | Self::OfferResponse { seq, .. }
            | Self::Resign { seq, .. }
            | Self::Abort { seq, .. }
            | Self::Takeback { seq, .. }
            | Self::ClaimDraw { seq, .. }
            | Self::AutoDraw { seq, .. } => *seq,
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
