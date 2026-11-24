//! Draw / takeback offers, resign, and abort (CV-165 / PROTOCOL / ADR-004 / ADR-005).

use std::fmt;

use redis::aio::ConnectionManager;
use serde_json::{json, Value};

use super::events::{GameEvent, GameMeta, PendingOffer};
use super::replay::{net_move_events, replay_fen};
use super::store::{
    append_game_event, load_events, load_meta, save_meta, GameLogError, MAX_PLAYER_ID_LEN,
};

const PROTOCOL_V: u32 = 1;

/// Offer kinds accepted by PROTOCOL `OfferResponse.offer`.
pub const OFFER_DRAW: &str = "draw";
pub const OFFER_TAKEBACK: &str = "takeback";

/// ADR-005 first-move abort windows by time class (ms).
pub fn abort_window_ms(time_class: &str) -> u64 {
    match time_class.trim().to_ascii_lowercase().as_str() {
        "bullet" => 15_000,
        "blitz" => 20_000,
        "rapid" | "classical" | "correspondence" => 60_000,
        // Unknown class: treat as blitz so abort stays bounded.
        _ => 20_000,
    }
}

/// Whether abort is still allowed given net ply count and first-move timestamp.
pub fn abort_allowed(
    net_plies: usize,
    first_move_ts: Option<u64>,
    now_ms: u64,
    time_class: &str,
) -> bool {
    match net_plies {
        0 => true,
        1 => match first_move_ts {
            // Missing ts with a ply on the board is corrupt; deny rather than open an unbounded window.
            None => false,
            Some(start) => now_ms.saturating_sub(start) <= abort_window_ms(time_class),
        },
        _ => false,
    }
}

/// PROTOCOL error codes for offer/abort rejects (and AUTH / GAME_GONE).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OfferRejectCode {
    OfferInvalid,
    AbortDenied,
    Auth,
    GameGone,
}

impl OfferRejectCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OfferInvalid => "OFFER_INVALID",
            Self::AbortDenied => "ABORT_DENIED",
            Self::Auth => "AUTH",
            Self::GameGone => "GAME_GONE",
        }
    }
}

/// Errors from offer / resign / abort mutations.
#[derive(Debug)]
pub enum OfferError {
    OfferInvalid(String),
    AbortDenied(String),
    Auth,
    GameGone,
    Corrupt(String),
    TooManyEvents,
    Redis(redis::RedisError),
}

impl OfferError {
    pub fn reject_code(&self) -> OfferRejectCode {
        match self {
            Self::OfferInvalid(_) => OfferRejectCode::OfferInvalid,
            Self::AbortDenied(_) => OfferRejectCode::AbortDenied,
            Self::Auth => OfferRejectCode::Auth,
            Self::GameGone => OfferRejectCode::GameGone,
            Self::Corrupt(_) | Self::TooManyEvents | Self::Redis(_) => {
                OfferRejectCode::OfferInvalid
            }
        }
    }
}

impl fmt::Display for OfferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OfferInvalid(msg) => write!(f, "{msg}"),
            Self::AbortDenied(msg) => write!(f, "{msg}"),
            Self::Auth => write!(f, "not a player in this game"),
            Self::GameGone => write!(f, "game not found"),
            Self::Corrupt(detail) => write!(f, "corrupt game state: {detail}"),
            Self::TooManyEvents => write!(f, "game event log exceeds max length"),
            Self::Redis(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for OfferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Redis(err) => Some(err),
            _ => None,
        }
    }
}

impl From<GameLogError> for OfferError {
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

/// Successful non-terminal offer mutation (PROTOCOL `OfferState`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferStateOut {
    pub offer: &'static str,
    pub by: &'static str,
    pub status: &'static str,
    pub seq: u64,
}

/// Terminal game result from resign / abort / draw agreement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameEndOut {
    pub result: &'static str,
    pub winner: Option<&'static str>,
    pub reason: &'static str,
    pub seq: u64,
}

/// Result of an offer / resign / abort call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferOutcome {
    State(OfferStateOut),
    End(GameEndOut),
}

/// PROTOCOL `OfferState` JSON.
pub fn offer_state_json(
    game_id: &str,
    offer: &str,
    by: &str,
    status: &str,
    seq: u64,
    ts: u64,
) -> Value {
    json!({
        "typ": "OfferState",
        "v": PROTOCOL_V,
        "game_id": game_id,
        "offer": offer,
        "by": by,
        "status": status,
        "seq": seq,
        "ts": ts
    })
}

/// PROTOCOL `GameEnd` JSON for resign / abort / agreement (no `flagged`).
pub fn game_end_offer_json(game_id: &str, end: &GameEndOut, ts: u64) -> Value {
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

/// PROTOCOL `Error` for offer/abort rejects.
pub fn offer_error_json(id: Option<&str>, code: OfferRejectCode, message: &str) -> Value {
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

fn seat_color(meta: &GameMeta, player_id: &str) -> Result<&'static str, OfferError> {
    if player_id.is_empty() || player_id.len() > MAX_PLAYER_ID_LEN {
        return Err(OfferError::Auth);
    }
    if meta.white_id.is_empty() || meta.black_id.is_empty() {
        return Err(OfferError::Auth);
    }
    if player_id == meta.white_id {
        Ok("w")
    } else if player_id == meta.black_id {
        Ok("b")
    } else {
        Err(OfferError::Auth)
    }
}

fn require_live(meta: &GameMeta) -> Result<(), OfferError> {
    if meta.ended {
        return Err(OfferError::GameGone);
    }
    Ok(())
}

fn opponent(color: &str) -> &'static str {
    if color == "w" {
        "b"
    } else {
        "w"
    }
}

fn parse_kind(kind: &str) -> Result<&'static str, OfferError> {
    match kind {
        OFFER_DRAW => Ok(OFFER_DRAW),
        OFFER_TAKEBACK => Ok(OFFER_TAKEBACK),
        _ => Err(OfferError::OfferInvalid("unknown offer kind".into())),
    }
}

/// Offer a draw. Counter-offering an opponent draw accepts it.
pub async fn offer_draw(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    ts: u64,
) -> Result<OfferOutcome, OfferError> {
    offer_kind(conn, game_id, player_id, OFFER_DRAW, ts).await
}

/// Offer a takeback of the last net ply. Counter-offer accepts.
pub async fn offer_takeback(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    ts: u64,
) -> Result<OfferOutcome, OfferError> {
    offer_kind(conn, game_id, player_id, OFFER_TAKEBACK, ts).await
}

async fn offer_kind(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    kind: &'static str,
    ts: u64,
) -> Result<OfferOutcome, OfferError> {
    let mut meta = load_meta(conn, game_id).await?;
    require_live(&meta)?;
    let by = seat_color(&meta, player_id)?;
    let events = load_events(conn, game_id).await?;

    if kind == OFFER_TAKEBACK && net_move_events(&events).is_empty() {
        return Err(OfferError::OfferInvalid("no move to take back".into()));
    }

    if let Some(pending) = meta.pending_offer.clone() {
        if pending.kind == kind && pending.by != by {
            // Opponent already offered the same kind → treat as accept.
            return respond_offer_inner(conn, game_id, &mut meta, &events, by, kind, true, ts)
                .await;
        }
        if pending.kind == kind && pending.by == by {
            return Err(OfferError::OfferInvalid(
                "offer already pending from this seat".into(),
            ));
        }
        if pending.kind != kind {
            return Err(OfferError::OfferInvalid(
                "different offer already pending".into(),
            ));
        }
    }

    // Append audit event before meta so a failed RPUSH cannot leave pending without a log entry.
    let seq = append_game_event(
        conn,
        game_id,
        GameEvent::Offer {
            seq: 0,
            kind: kind.to_owned(),
            by: by.to_owned(),
            ts,
        },
    )
    .await?;
    meta.pending_offer = Some(PendingOffer {
        kind: kind.to_owned(),
        by: by.to_owned(),
    });
    save_meta(conn, game_id, &meta).await?;

    Ok(OfferOutcome::State(OfferStateOut {
        offer: kind,
        by,
        status: "offered",
        seq,
    }))
}

/// Accept, decline, or withdraw a pending offer (`accept:false` = decline or withdraw).
pub async fn respond_offer(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    offer: &str,
    accept: bool,
    ts: u64,
) -> Result<OfferOutcome, OfferError> {
    let kind = parse_kind(offer)?;
    let mut meta = load_meta(conn, game_id).await?;
    require_live(&meta)?;
    let by = seat_color(&meta, player_id)?;
    let events = load_events(conn, game_id).await?;
    respond_offer_inner(conn, game_id, &mut meta, &events, by, kind, accept, ts).await
}

#[allow(clippy::too_many_arguments)] // PROTOCOL fields + Redis handles; keep flat for call sites.
async fn respond_offer_inner(
    conn: &mut ConnectionManager,
    game_id: &str,
    meta: &mut GameMeta,
    events: &[GameEvent],
    by: &'static str,
    kind: &'static str,
    accept: bool,
    ts: u64,
) -> Result<OfferOutcome, OfferError> {
    let pending = meta
        .pending_offer
        .clone()
        .ok_or_else(|| OfferError::OfferInvalid("no pending offer".into()))?;
    if pending.kind != kind {
        return Err(OfferError::OfferInvalid(
            "pending offer kind mismatch".into(),
        ));
    }

    if accept {
        // Only the opponent of the offerer may accept.
        if pending.by == by {
            return Err(OfferError::OfferInvalid("cannot accept own offer".into()));
        }
        match kind {
            OFFER_DRAW => {
                let seq = append_game_event(
                    conn,
                    game_id,
                    GameEvent::OfferResponse {
                        seq: 0,
                        kind: kind.to_owned(),
                        by: by.to_owned(),
                        accept: true,
                        ts,
                    },
                )
                .await?;
                meta.pending_offer = None;
                meta.ended = true;
                meta.ended_reason = Some("agreement".into());
                meta.active_started_ms = 0;
                save_meta(conn, game_id, meta).await?;
                Ok(OfferOutcome::End(GameEndOut {
                    result: "1/2-1/2",
                    winner: None,
                    reason: "agreement",
                    seq,
                }))
            }
            OFFER_TAKEBACK => {
                let offerer = if pending.by == "w" { "w" } else { "b" };
                apply_takeback(conn, game_id, meta, events, offerer, ts).await
            }
            _ => Err(OfferError::OfferInvalid("unknown offer kind".into())),
        }
    } else {
        // Decline (opponent) or withdraw (offerer).
        let status = if pending.by == by {
            "withdrawn"
        } else {
            "declined"
        };
        let offerer = if pending.by == "w" { "w" } else { "b" };
        let seq = append_game_event(
            conn,
            game_id,
            GameEvent::OfferResponse {
                seq: 0,
                kind: kind.to_owned(),
                by: by.to_owned(),
                accept: false,
                ts,
            },
        )
        .await?;
        meta.pending_offer = None;
        save_meta(conn, game_id, meta).await?;
        Ok(OfferOutcome::State(OfferStateOut {
            offer: kind,
            by: offerer,
            status,
            seq,
        }))
    }
}

async fn apply_takeback(
    conn: &mut ConnectionManager,
    game_id: &str,
    meta: &mut GameMeta,
    events: &[GameEvent],
    offerer: &'static str,
    ts: u64,
) -> Result<OfferOutcome, OfferError> {
    let net = net_move_events(events);
    let last = net
        .last()
        .ok_or_else(|| OfferError::OfferInvalid("no move to take back".into()))?;
    let GameEvent::Move {
        seq: undone_seq, ..
    } = last
    else {
        return Err(OfferError::Corrupt("net move stack corrupt".into()));
    };

    // Append first so a failed RPUSH cannot leave meta clocks undone while the ply remains.
    let _ = append_game_event(
        conn,
        game_id,
        GameEvent::OfferResponse {
            seq: 0,
            kind: OFFER_TAKEBACK.to_owned(),
            by: offerer.to_owned(),
            accept: true,
            ts,
        },
    )
    .await?;
    let seq = append_game_event(
        conn,
        game_id,
        GameEvent::Takeback {
            seq: 0,
            undone_seq: *undone_seq,
            ts,
        },
    )
    .await?;

    // Restore clocks + lag quotas from the snapshot taken before the undone ply.
    meta.clock_w_ms = meta.prev_clock_w_ms;
    meta.clock_b_ms = meta.prev_clock_b_ms;
    meta.lag_quota_w_ms = meta.prev_lag_quota_w_ms;
    meta.lag_quota_b_ms = meta.prev_lag_quota_b_ms;
    meta.pending_offer = None;

    let fen = replay_fen(&meta.initial_fen, &load_events(conn, game_id).await?)?;
    let active = fen.split_whitespace().nth(1).unwrap_or("w");
    meta.active = active.to_owned();
    // Fresh think clock for the side that must move again after the undo.
    meta.active_started_ms = ts;

    save_meta(conn, game_id, meta).await?;

    Ok(OfferOutcome::State(OfferStateOut {
        offer: OFFER_TAKEBACK,
        by: offerer,
        status: "accepted",
        seq,
    }))
}

/// Resign: opponent wins. Allowed any time while the game is live.
pub async fn resign(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    ts: u64,
) -> Result<OfferOutcome, OfferError> {
    let mut meta = load_meta(conn, game_id).await?;
    require_live(&meta)?;
    let by = seat_color(&meta, player_id)?;
    let winner = opponent(by);
    let result = if winner == "w" { "1-0" } else { "0-1" };

    let seq = append_game_event(
        conn,
        game_id,
        GameEvent::Resign {
            seq: 0,
            by: by.to_owned(),
            ts,
        },
    )
    .await?;
    meta.pending_offer = None;
    meta.ended = true;
    meta.ended_reason = Some("resign".into());
    meta.active_started_ms = 0;
    save_meta(conn, game_id, &meta).await?;

    Ok(OfferOutcome::End(GameEndOut {
        result,
        winner: Some(winner),
        reason: "resign",
        seq,
    }))
}

/// Abort under ADR-005 first-move rules (0 plies, or 1 ply within class window).
pub async fn abort(
    conn: &mut ConnectionManager,
    game_id: &str,
    player_id: &str,
    ts: u64,
) -> Result<OfferOutcome, OfferError> {
    let mut meta = load_meta(conn, game_id).await?;
    require_live(&meta)?;
    let by = seat_color(&meta, player_id)?;
    let events = load_events(conn, game_id).await?;
    let net = net_move_events(&events);
    let first_ts = net.first().and_then(|e| match e {
        GameEvent::Move { ts, .. } => Some(*ts),
        _ => None,
    });

    if !abort_allowed(net.len(), first_ts, ts, &meta.time_class) {
        return Err(OfferError::AbortDenied(
            "abort not allowed after first-move window".into(),
        ));
    }

    let seq = append_game_event(
        conn,
        game_id,
        GameEvent::Abort {
            seq: 0,
            by: by.to_owned(),
            ts,
        },
    )
    .await?;
    meta.pending_offer = None;
    meta.ended = true;
    meta.ended_reason = Some("abort".into());
    meta.active_started_ms = 0;
    save_meta(conn, game_id, &meta).await?;

    Ok(OfferOutcome::End(GameEndOut {
        result: "*",
        winner: None,
        reason: "abort",
        seq,
    }))
}
