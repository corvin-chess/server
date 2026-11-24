//! PROTOCOL play dispatch after Welcome (CV-172): quick pair, moves, offers, resume.
//!
//! Server remains authoritative; clients reconcile to sequenced events.

use std::sync::Arc;

use corvin_chess_core::{Move, Position};
use redis::aio::ConnectionManager;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::auth::session::{lookup_session, parse_session_id_from_cookie};
use crate::game::{
    abort, apply_move_with_rtt, claim_denied_json, claim_draw, claim_flag, game_end_claim_json,
    game_end_flag_json, game_end_offer_json, load_events, load_meta, move_applied_json,
    move_rejected_json, offer_draw, offer_error_json, offer_state_json, offer_takeback,
    persist_finished_from_redis, resign, respond_offer, ApplyError, ClaimError, ClaimRejectCode,
    FlagClaimError, GameEvent, GameMeta, LagTracker, OfferError, OfferOutcome,
};
use crate::matchmaking::{
    match_bundle_for, quick_pair, quick_pair_cancel, take_pair_result, ColorPref, MatchError,
    PairConfig, QuickPairOutcome,
};
use crate::rating::{apply_rated_finish, game_end_with_rating_diff, FinishOutcome, RatedFinish};
use crate::AppState;

use super::hub::PlayHub;

const PROTOCOL_V: u32 = 1;

/// Resolve opaque session cookie to user id (None if missing/expired).
pub async fn user_id_from_cookie_header(
    state: &AppState,
    cookie_header: Option<&str>,
) -> Option<String> {
    let raw = cookie_header?;
    let session_id = parse_session_id_from_cookie(raw)?;
    let session = lookup_session(&state.db, &session_id).await.ok()??;
    Some(session.user_id)
}

/// Resolve AuthTicket token (hex session id) to user id.
pub async fn user_id_from_auth_ticket(state: &AppState, token: &str) -> Option<String> {
    let bytes = hex::decode(token.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut id = [0u8; 32];
    id.copy_from_slice(&bytes);
    let session = lookup_session(&state.db, &id).await.ok()??;
    Some(session.user_id)
}

fn unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn error_json(code: &str, message: &str, id: Option<&str>) -> Value {
    let mut body = json!({
        "typ": "Error",
        "v": PROTOCOL_V,
        "code": code,
        "message": message
    });
    if let Some(id) = id {
        body["id"] = json!(id);
    }
    body
}

fn ack_json(id: Option<&str>) -> Value {
    let mut body = json!({
        "typ": "Ack",
        "v": PROTOCOL_V
    });
    if let Some(id) = id {
        body["id"] = json!(id);
    }
    body
}

fn parse_color(raw: &str) -> Result<ColorPref, MatchError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "random" => Ok(ColorPref::Random),
        "white" => Ok(ColorPref::White),
        "black" => Ok(ColorPref::Black),
        _ => Err(MatchError::InvalidConfig("color")),
    }
}

fn require_user<'a>(user_id: &'a Option<String>, id: Option<&str>) -> Result<&'a str, Value> {
    user_id
        .as_deref()
        .filter(|u| !u.is_empty())
        .ok_or_else(|| error_json("AUTH", "authentication required", id))
}

fn take_redis(redis: &mut Option<ConnectionManager>) -> Result<ConnectionManager, Value> {
    redis
        .take()
        .ok_or_else(|| error_json("INTERNAL", "redis unavailable", None))
}

/// Build PROTOCOL `GameSnapshot` at `seq` for resume.
pub fn game_snapshot_json(
    game_id: &str,
    fen: &str,
    clock_w_ms: u64,
    clock_b_ms: u64,
    active: &str,
    seq: u64,
) -> Value {
    json!({
        "typ": "GameSnapshot",
        "v": PROTOCOL_V,
        "game_id": game_id,
        "fen": fen,
        "clocks": { "w": clock_w_ms, "b": clock_b_ms },
        "active": active,
        "seq": seq
    })
}

fn event_to_wire(
    game_id: &str,
    ev: &GameEvent,
    move_bodies: &std::collections::HashMap<u64, Value>,
) -> Option<Value> {
    match ev {
        GameEvent::Move { seq, .. } => move_bodies.get(seq).cloned(),
        GameEvent::Offer {
            kind, by, seq, ts, ..
        } => Some(offer_state_json(game_id, kind, by, "offered", *seq, *ts)),
        GameEvent::OfferResponse {
            kind,
            by,
            accept,
            seq,
            ts,
            ..
        } => {
            let status = if *accept { "accepted" } else { "declined" };
            Some(offer_state_json(game_id, kind, by, status, *seq, *ts))
        }
        GameEvent::Resign { seq, by, ts } => Some(json!({
            "typ": "GameEnd",
            "v": PROTOCOL_V,
            "game_id": game_id,
            "result": if by == "w" { "0-1" } else { "1-0" },
            "winner": if by == "w" { "b" } else { "w" },
            "reason": "resign",
            "seq": seq,
            "ts": ts
        })),
        GameEvent::Abort { seq, ts, .. } => Some(json!({
            "typ": "GameEnd",
            "v": PROTOCOL_V,
            "game_id": game_id,
            "result": "*",
            "winner": Value::Null,
            "reason": "abort",
            "seq": seq,
            "ts": ts
        })),
        GameEvent::ClaimDraw { seq, kind, ts, .. } | GameEvent::AutoDraw { seq, kind, ts } => {
            Some(json!({
                "typ": "GameEnd",
                "v": PROTOCOL_V,
                "game_id": game_id,
                "result": "1/2-1/2",
                "winner": Value::Null,
                "reason": kind,
                "seq": seq,
                "ts": ts
            }))
        }
        _ => None,
    }
}

/// Walk moves to build PROTOCOL MoveApplied bodies for resume replay.
fn build_move_bodies(
    initial_fen: &str,
    events: &[GameEvent],
    game_id: &str,
) -> Result<std::collections::HashMap<u64, Value>, String> {
    let mut pos = Position::from_fen(initial_fen).map_err(|e| e.to_string())?;
    let mut out = std::collections::HashMap::new();
    for ev in events {
        let GameEvent::Move { seq, uci, ts } = ev else {
            continue;
        };
        let mv = Move::from_uci(uci).map_err(|e| e.to_string())?;
        let san = pos.to_san(mv).unwrap_or_else(|_| uci.clone());
        pos = pos
            .try_make_move(mv)
            .ok_or_else(|| format!("illegal {uci}"))?;
        let fen = pos.to_fen();
        let active = if fen.split_whitespace().nth(1) == Some("b") {
            "b"
        } else {
            "w"
        };
        out.insert(
            *seq,
            move_applied_json(
                None, game_id, uci, &san, &fen, 0, 0, active, *seq, *ts, None,
            ),
        );
    }
    Ok(out)
}

async fn finish_and_broadcast(
    hub: &Arc<PlayHub>,
    pool: &PgPool,
    redis: &mut ConnectionManager,
    game_id: &str,
    meta: &GameMeta,
    mut end_json: Value,
) {
    let result = end_json
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or("*");
    let reason = end_json
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let ended_ms = end_json
        .get("ts")
        .and_then(Value::as_u64)
        .unwrap_or_else(unix_ms);
    let _ = persist_finished_from_redis(pool, redis, game_id, result, reason, ended_ms).await;

    match apply_rated_finish(
        pool,
        RatedFinish {
            game_id,
            meta,
            result,
            reason,
        },
    )
    .await
    {
        Ok(FinishOutcome::Applied(diffs) | FinishOutcome::AlreadyApplied(diffs)) => {
            end_json = game_end_with_rating_diff(end_json, Some(&diffs));
        }
        Ok(FinishOutcome::Skipped) | Err(_) => {}
    }

    hub.send_to_seats(&meta.white_id, &meta.black_id, end_json)
        .await;
}

/// Handle one client play message; returns frames for this socket (hub may push more).
pub async fn dispatch(
    state: &AppState,
    hub: &Arc<PlayHub>,
    redis: &mut Option<ConnectionManager>,
    user_id: &mut Option<String>,
    lag: &mut LagTracker,
    value: &Value,
) -> Vec<Value> {
    let typ = value.get("typ").and_then(Value::as_str).unwrap_or("");
    let id = value.get("id").and_then(Value::as_str);

    match typ {
        "AuthTicket" => {
            let token = value.get("token").and_then(Value::as_str).unwrap_or("");
            match user_id_from_auth_ticket(state, token).await {
                Some(uid) => {
                    *user_id = Some(uid);
                    vec![ack_json(id)]
                }
                None => vec![error_json("AUTH", "invalid auth ticket", id)],
            }
        }
        "QuickPair" => handle_quick_pair(hub, redis, user_id, value, id).await,
        "QuickPairCancel" => handle_quick_pair_cancel(redis, user_id, id).await,
        "Move" => handle_move(state, hub, redis, user_id, lag, value, id).await,
        "OfferDraw" => {
            handle_simple_offer(state, hub, redis, user_id, value, id, OfferKind::Draw).await
        }
        "OfferTakeback" => {
            handle_simple_offer(state, hub, redis, user_id, value, id, OfferKind::Takeback).await
        }
        "OfferResponse" => handle_offer_response(state, hub, redis, user_id, value, id).await,
        "Resign" => handle_resign(state, hub, redis, user_id, value, id).await,
        "Abort" => handle_abort(state, hub, redis, user_id, value, id).await,
        "ClaimDraw" => handle_claim_draw(state, hub, redis, user_id, value, id).await,
        "Flag" => handle_flag(state, hub, redis, user_id, value, id).await,
        "ResumeGame" => handle_resume(redis, user_id, value, id).await,
        _ => vec![error_json("INTERNAL", "unknown message type", id)],
    }
}

async fn handle_quick_pair(
    hub: &Arc<PlayHub>,
    redis: &mut Option<ConnectionManager>,
    user_id: &Option<String>,
    value: &Value,
    id: Option<&str>,
) -> Vec<Value> {
    let uid = match require_user(user_id, id) {
        Ok(u) => u.to_owned(),
        Err(e) => return vec![e],
    };
    let mut conn = match take_redis(redis) {
        Ok(c) => c,
        Err(e) => return vec![e],
    };

    let time_class = value
        .get("time_class")
        .and_then(Value::as_str)
        .unwrap_or("blitz");
    let initial_ms = value
        .get("initial_ms")
        .and_then(Value::as_u64)
        .unwrap_or(300_000);
    let increment_ms = value
        .get("increment_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let rated = value.get("rated").and_then(Value::as_bool).unwrap_or(true);
    let color_raw = value
        .get("color")
        .and_then(Value::as_str)
        .unwrap_or("random");
    let color = match parse_color(color_raw) {
        Ok(c) => c,
        Err(_) => {
            *redis = Some(conn);
            return vec![error_json("INTERNAL", "invalid color", id)];
        }
    };
    let config = PairConfig {
        time_class: time_class.to_owned(),
        initial_ms,
        increment_ms,
        rated,
        color,
    };
    let now = unix_ms();
    let out = match quick_pair(&mut conn, &uid, &config, now).await {
        Ok(QuickPairOutcome::Waiting) => vec![ack_json(id)],
        Ok(QuickPairOutcome::Matched(bundle)) => {
            let other = if bundle.your_color == "w" {
                bundle.black_id.as_str()
            } else {
                bundle.white_id.as_str()
            };
            let other_start = match_bundle_for(
                &bundle.game_id,
                &config,
                &bundle.white_id,
                &bundle.black_id,
                other,
            )
            .start;
            // Prefer live hub push; clear Redis notify so a later QuickPair does not
            // re-deliver the same GameStart to an already-connected peer.
            if hub.is_connected(other).await {
                hub.send_to_user(other, other_start).await;
                let _ = take_pair_result(&mut conn, other).await;
            }
            vec![bundle.start]
        }
        Err(MatchError::InvalidPlayerId) => vec![error_json("AUTH", "invalid player", id)],
        Err(MatchError::InvalidConfig(_)) => {
            vec![error_json("INTERNAL", "invalid pair config", id)]
        }
        Err(err) => vec![error_json("INTERNAL", &err.to_string(), id)],
    };
    *redis = Some(conn);
    out
}

async fn handle_quick_pair_cancel(
    redis: &mut Option<ConnectionManager>,
    user_id: &Option<String>,
    id: Option<&str>,
) -> Vec<Value> {
    let uid = match require_user(user_id, id) {
        Ok(u) => u.to_owned(),
        Err(e) => return vec![e],
    };
    let mut conn = match take_redis(redis) {
        Ok(c) => c,
        Err(e) => return vec![e],
    };
    let _ = quick_pair_cancel(&mut conn, &uid).await;
    *redis = Some(conn);
    vec![ack_json(id)]
}

async fn handle_move(
    state: &AppState,
    hub: &Arc<PlayHub>,
    redis: &mut Option<ConnectionManager>,
    user_id: &Option<String>,
    lag: &mut LagTracker,
    value: &Value,
    id: Option<&str>,
) -> Vec<Value> {
    let uid = match require_user(user_id, id) {
        Ok(u) => u.to_owned(),
        Err(e) => return vec![e],
    };
    let mut conn = match take_redis(redis) {
        Ok(c) => c,
        Err(e) => return vec![e],
    };
    let game_id = value.get("game_id").and_then(Value::as_str).unwrap_or("");
    let uci = value.get("uci").and_then(Value::as_str).unwrap_or("");
    let ts = unix_ms();
    let rtt = lag.ewma_rtt_ms();
    let out = match apply_move_with_rtt(&mut conn, game_id, &uid, uci, ts, rtt).await {
        Ok(applied) => {
            let msg = move_applied_json(
                id,
                game_id,
                &applied.uci,
                &applied.san,
                &applied.fen,
                applied.clock_w_ms,
                applied.clock_b_ms,
                applied.active,
                applied.seq,
                ts,
                applied.lag_comp_ms,
            );
            if let Ok(meta) = load_meta(&mut conn, game_id).await {
                hub.send_to_seats(&meta.white_id, &meta.black_id, msg).await;
                if let Some(end) = applied.auto_draw {
                    let end_json = game_end_claim_json(game_id, &end, ts);
                    finish_and_broadcast(hub, &state.db, &mut conn, game_id, &meta, end_json).await;
                }
            }
            vec![]
        }
        Err(ApplyError::Flagged { outcome }) => {
            let end_json = game_end_flag_json(game_id, &outcome, ts);
            if let Ok(meta) = load_meta(&mut conn, game_id).await {
                finish_and_broadcast(hub, &state.db, &mut conn, game_id, &meta, end_json).await;
            }
            vec![]
        }
        Err(err) => {
            if let Some(code) = err.reject_code() {
                let corr = id.unwrap_or("move");
                vec![move_rejected_json(corr, code, &err.to_string())]
            } else {
                vec![error_json("INTERNAL", &err.to_string(), id)]
            }
        }
    };
    *redis = Some(conn);
    out
}

enum OfferKind {
    Draw,
    Takeback,
}

async fn handle_simple_offer(
    state: &AppState,
    hub: &Arc<PlayHub>,
    redis: &mut Option<ConnectionManager>,
    user_id: &Option<String>,
    value: &Value,
    id: Option<&str>,
    kind: OfferKind,
) -> Vec<Value> {
    let uid = match require_user(user_id, id) {
        Ok(u) => u.to_owned(),
        Err(e) => return vec![e],
    };
    let mut conn = match take_redis(redis) {
        Ok(c) => c,
        Err(e) => return vec![e],
    };
    let game_id = value.get("game_id").and_then(Value::as_str).unwrap_or("");
    let ts = unix_ms();
    let result = match kind {
        OfferKind::Draw => offer_draw(&mut conn, game_id, &uid, ts).await,
        OfferKind::Takeback => offer_takeback(&mut conn, game_id, &uid, ts).await,
    };
    let out = emit_offer_outcome(state, hub, &mut conn, game_id, ts, id, result).await;
    *redis = Some(conn);
    out
}

async fn handle_offer_response(
    state: &AppState,
    hub: &Arc<PlayHub>,
    redis: &mut Option<ConnectionManager>,
    user_id: &Option<String>,
    value: &Value,
    id: Option<&str>,
) -> Vec<Value> {
    let uid = match require_user(user_id, id) {
        Ok(u) => u.to_owned(),
        Err(e) => return vec![e],
    };
    let mut conn = match take_redis(redis) {
        Ok(c) => c,
        Err(e) => return vec![e],
    };
    let game_id = value.get("game_id").and_then(Value::as_str).unwrap_or("");
    let offer = value.get("offer").and_then(Value::as_str).unwrap_or("");
    let accept = value
        .get("accept")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ts = unix_ms();
    let result = respond_offer(&mut conn, game_id, &uid, offer, accept, ts).await;
    let out = emit_offer_outcome(state, hub, &mut conn, game_id, ts, id, result).await;
    *redis = Some(conn);
    out
}

async fn handle_resign(
    state: &AppState,
    hub: &Arc<PlayHub>,
    redis: &mut Option<ConnectionManager>,
    user_id: &Option<String>,
    value: &Value,
    id: Option<&str>,
) -> Vec<Value> {
    let uid = match require_user(user_id, id) {
        Ok(u) => u.to_owned(),
        Err(e) => return vec![e],
    };
    let mut conn = match take_redis(redis) {
        Ok(c) => c,
        Err(e) => return vec![e],
    };
    let game_id = value.get("game_id").and_then(Value::as_str).unwrap_or("");
    let ts = unix_ms();
    let result = resign(&mut conn, game_id, &uid, ts).await;
    let out = emit_offer_outcome(state, hub, &mut conn, game_id, ts, id, result).await;
    *redis = Some(conn);
    out
}

async fn handle_abort(
    state: &AppState,
    hub: &Arc<PlayHub>,
    redis: &mut Option<ConnectionManager>,
    user_id: &Option<String>,
    value: &Value,
    id: Option<&str>,
) -> Vec<Value> {
    let uid = match require_user(user_id, id) {
        Ok(u) => u.to_owned(),
        Err(e) => return vec![e],
    };
    let mut conn = match take_redis(redis) {
        Ok(c) => c,
        Err(e) => return vec![e],
    };
    let game_id = value.get("game_id").and_then(Value::as_str).unwrap_or("");
    let ts = unix_ms();
    let result = abort(&mut conn, game_id, &uid, ts).await;
    let out = emit_offer_outcome(state, hub, &mut conn, game_id, ts, id, result).await;
    *redis = Some(conn);
    out
}

async fn emit_offer_outcome(
    state: &AppState,
    hub: &Arc<PlayHub>,
    conn: &mut ConnectionManager,
    game_id: &str,
    ts: u64,
    id: Option<&str>,
    result: Result<OfferOutcome, OfferError>,
) -> Vec<Value> {
    match result {
        Ok(OfferOutcome::State(st)) => {
            let msg = offer_state_json(game_id, st.offer, st.by, st.status, st.seq, ts);
            if let Ok(meta) = load_meta(conn, game_id).await {
                hub.send_to_seats(&meta.white_id, &meta.black_id, msg).await;
            }
            vec![]
        }
        Ok(OfferOutcome::End(end)) => {
            let end_json = game_end_offer_json(game_id, &end, ts);
            if let Ok(meta) = load_meta(conn, game_id).await {
                finish_and_broadcast(hub, &state.db, conn, game_id, &meta, end_json).await;
            }
            vec![]
        }
        Err(err) => vec![offer_error_json(id, err.reject_code(), &err.to_string())],
    }
}

async fn handle_claim_draw(
    state: &AppState,
    hub: &Arc<PlayHub>,
    redis: &mut Option<ConnectionManager>,
    user_id: &Option<String>,
    value: &Value,
    id: Option<&str>,
) -> Vec<Value> {
    let uid = match require_user(user_id, id) {
        Ok(u) => u.to_owned(),
        Err(e) => return vec![e],
    };
    let mut conn = match take_redis(redis) {
        Ok(c) => c,
        Err(e) => return vec![e],
    };
    let game_id = value.get("game_id").and_then(Value::as_str).unwrap_or("");
    let ts = unix_ms();
    let out = match claim_draw(&mut conn, game_id, &uid, ts).await {
        Ok(end) => {
            let end_json = game_end_claim_json(game_id, &end, ts);
            if let Ok(meta) = load_meta(&mut conn, game_id).await {
                finish_and_broadcast(hub, &state.db, &mut conn, game_id, &meta, end_json).await;
            }
            vec![]
        }
        Err(ClaimError::ClaimDenied(msg)) => {
            vec![claim_denied_json(id, ClaimRejectCode::ClaimDenied, &msg)]
        }
        Err(err) => vec![error_json(err.reject_code().as_str(), &err.to_string(), id)],
    };
    *redis = Some(conn);
    out
}

async fn handle_flag(
    state: &AppState,
    hub: &Arc<PlayHub>,
    redis: &mut Option<ConnectionManager>,
    user_id: &Option<String>,
    value: &Value,
    id: Option<&str>,
) -> Vec<Value> {
    let uid = match require_user(user_id, id) {
        Ok(u) => u.to_owned(),
        Err(e) => return vec![e],
    };
    let mut conn = match take_redis(redis) {
        Ok(c) => c,
        Err(e) => return vec![e],
    };
    let game_id = value.get("game_id").and_then(Value::as_str).unwrap_or("");
    let color = value.get("color").and_then(Value::as_str).unwrap_or("");
    let ts = unix_ms();
    let out = match claim_flag(&mut conn, game_id, &uid, color, ts).await {
        Ok(outcome) => {
            let end_json = game_end_flag_json(game_id, &outcome, ts);
            if let Ok(meta) = load_meta(&mut conn, game_id).await {
                finish_and_broadcast(hub, &state.db, &mut conn, game_id, &meta, end_json).await;
            }
            vec![]
        }
        Err(FlagClaimError::Auth) => vec![error_json("AUTH", "not a player in this game", id)],
        Err(FlagClaimError::GameGone) => vec![error_json("GAME_GONE", "game not found", id)],
        Err(FlagClaimError::NotFlagged) => {
            vec![error_json("INTERNAL", "claimed side still has time", id)]
        }
        Err(err) => vec![error_json("INTERNAL", &err.to_string(), id)],
    };
    *redis = Some(conn);
    out
}

async fn handle_resume(
    redis: &mut Option<ConnectionManager>,
    user_id: &Option<String>,
    value: &Value,
    id: Option<&str>,
) -> Vec<Value> {
    let uid = match require_user(user_id, id) {
        Ok(u) => u.to_owned(),
        Err(e) => return vec![e],
    };
    let mut conn = match take_redis(redis) {
        Ok(c) => c,
        Err(e) => return vec![e],
    };
    let game_id = value.get("game_id").and_then(Value::as_str).unwrap_or("");
    let last_seq = value.get("last_seq").and_then(Value::as_u64).unwrap_or(0);
    let token = value
        .get("player_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    if token != uid {
        *redis = Some(conn);
        return vec![error_json("AUTH", "player_token mismatch", id)];
    }
    let meta = match load_meta(&mut conn, game_id).await {
        Ok(m) => m,
        Err(_) => {
            *redis = Some(conn);
            return vec![error_json("GAME_GONE", "game not found", id)];
        }
    };
    if meta.ended {
        *redis = Some(conn);
        return vec![error_json("GAME_GONE", "game ended", id)];
    }
    if uid != meta.white_id && uid != meta.black_id {
        *redis = Some(conn);
        return vec![error_json("AUTH", "not a player in this game", id)];
    }
    let events = match load_events(&mut conn, game_id).await {
        Ok(e) => e,
        Err(_) => {
            *redis = Some(conn);
            return vec![error_json("GAME_GONE", "game not found", id)];
        }
    };
    let fen = match reconstruct_to_seq(&meta.initial_fen, &events, last_seq) {
        Ok(f) => f,
        Err(_) => {
            *redis = Some(conn);
            return vec![error_json("GAME_GONE", "corrupt game history", id)];
        }
    };
    let fen_active = if fen.split_whitespace().nth(1) == Some("b") {
        "b"
    } else {
        "w"
    };
    // shortcut: clocks are current meta (not reconstructed at last_seq); upgrade when
    // clock history is stored per ply (CV-180 / clock sync on resume).
    let max_seq = events.iter().map(GameEvent::seq).max().unwrap_or(0);
    let active = if last_seq >= max_seq && !meta.active.is_empty() {
        meta.active.as_str()
    } else {
        fen_active
    };
    let snap = game_snapshot_json(
        game_id,
        &fen,
        meta.clock_w_ms,
        meta.clock_b_ms,
        active,
        last_seq,
    );
    let move_bodies = match build_move_bodies(&meta.initial_fen, &events, game_id) {
        Ok(m) => m,
        Err(_) => {
            *redis = Some(conn);
            return vec![error_json("GAME_GONE", "corrupt game history", id)];
        }
    };
    let mut replay_events = Vec::new();
    for ev in &events {
        if ev.seq() > last_seq {
            if let Some(wire) = event_to_wire(game_id, ev, &move_bodies) {
                replay_events.push(wire);
            }
        }
    }
    let replay = json!({
        "typ": "Replay",
        "v": PROTOCOL_V,
        "game_id": game_id,
        "events": replay_events
    });
    *redis = Some(conn);
    vec![snap, replay]
}

fn reconstruct_to_seq(
    initial_fen: &str,
    events: &[GameEvent],
    last_seq: u64,
) -> Result<String, String> {
    let mut pos = Position::from_fen(initial_fen).map_err(|e| e.to_string())?;
    for ev in events {
        let GameEvent::Move { seq, uci, .. } = ev else {
            continue;
        };
        if *seq > last_seq {
            break;
        }
        let mv = Move::from_uci(uci).map_err(|e| e.to_string())?;
        pos = pos
            .try_make_move(mv)
            .ok_or_else(|| format!("illegal {uci}"))?;
    }
    Ok(pos.to_fen())
}
