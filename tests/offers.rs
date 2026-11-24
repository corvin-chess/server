//! CV-165: Draw / resign / abort / takeback offer state machine (PROTOCOL).

use std::env;

use corvin_server::game::{
    abort, abort_allowed, abort_window_ms, apply_move, create_game, delete_game,
    game_end_offer_json, load_meta, offer_draw, offer_error_json, offer_state_json, offer_takeback,
    reconstruct_fen, resign, respond_offer, GameEndOut, GameMeta, OfferError, OfferOutcome,
    OfferRejectCode, OFFER_DRAW, OFFER_TAKEBACK, START_FEN,
};
use corvin_server::redis_store::connect;

const WHITE: &str = "player-white-ulid-cv165-00001";
const BLACK: &str = "player-black-ulid-cv165-00001";

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn seated_meta(time_class: &str, clock_ms: u64) -> GameMeta {
    GameMeta {
        initial_fen: START_FEN.to_owned(),
        created_ms: 1_000,
        white_id: WHITE.to_owned(),
        black_id: BLACK.to_owned(),
        clock_w_ms: clock_ms,
        clock_b_ms: clock_ms,
        increment_ms: 0,
        active: "w".to_owned(),
        active_started_ms: 1_000,
        time_class: time_class.to_owned(),
        ..GameMeta::default()
    }
}

#[test]
fn abort_windows_match_adr_005() {
    assert_eq!(abort_window_ms("bullet"), 15_000);
    assert_eq!(abort_window_ms("blitz"), 20_000);
    assert_eq!(abort_window_ms("rapid"), 60_000);
    assert_eq!(abort_window_ms("classical"), 60_000);
    assert_eq!(abort_window_ms("correspondence"), 60_000);
    assert_eq!(abort_window_ms("unknown"), 20_000);
}

#[test]
fn abort_allowed_zero_or_one_ply_within_window() {
    assert!(abort_allowed(0, None, 50_000, "blitz"));
    assert!(abort_allowed(1, Some(10_000), 25_000, "blitz")); // 15s < 20s
    assert!(!abort_allowed(1, Some(10_000), 35_000, "blitz")); // 25s > 20s
    assert!(!abort_allowed(1, None, 11_000, "blitz")); // missing ts with a ply → deny
    assert!(!abort_allowed(2, Some(10_000), 11_000, "blitz"));
    assert!(abort_allowed(1, Some(10_000), 69_000, "rapid")); // 59s < 60s
}

#[test]
fn offer_state_and_game_end_json_match_protocol() {
    let state = offer_state_json("g1", OFFER_DRAW, "w", "offered", 3, 9);
    assert_eq!(state["typ"], "OfferState");
    assert_eq!(state["v"], 1);
    assert_eq!(state["offer"], "draw");
    assert_eq!(state["by"], "w");
    assert_eq!(state["status"], "offered");
    assert_eq!(state["seq"], 3);

    let end = game_end_offer_json(
        "g1",
        &GameEndOut {
            result: "1/2-1/2",
            winner: None,
            reason: "agreement",
            seq: 4,
        },
        10,
    );
    assert_eq!(end["typ"], "GameEnd");
    assert_eq!(end["result"], "1/2-1/2");
    assert_eq!(end["reason"], "agreement");
    assert!(end["winner"].is_null());
    assert!(end.get("flagged").is_none());

    let resign_end = game_end_offer_json(
        "g1",
        &GameEndOut {
            result: "0-1",
            winner: Some("b"),
            reason: "resign",
            seq: 2,
        },
        11,
    );
    assert_eq!(resign_end["winner"], "b");
    assert_eq!(resign_end["reason"], "resign");

    let err = offer_error_json(Some("req-1"), OfferRejectCode::AbortDenied, "too late");
    assert_eq!(err["typ"], "Error");
    assert_eq!(err["code"], "ABORT_DENIED");
    assert_eq!(err["id"], "req-1");
}

#[test]
fn offer_error_maps_reject_codes() {
    assert_eq!(
        OfferError::OfferInvalid("x".into()).reject_code(),
        OfferRejectCode::OfferInvalid
    );
    assert_eq!(
        OfferError::AbortDenied("x".into()).reject_code(),
        OfferRejectCode::AbortDenied
    );
    assert_eq!(OfferError::Auth.reject_code(), OfferRejectCode::Auth);
    assert_eq!(
        OfferError::GameGone.reject_code(),
        OfferRejectCode::GameGone
    );
}

#[tokio::test]
async fn redis_draw_offer_accept_ends_agreement() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_draw_offer_accept_ends_agreement: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV165DRAW0000000000000001";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(&mut conn, game_id, &seated_meta("blitz", 60_000))
        .await
        .expect("create");

    let offered = offer_draw(&mut conn, game_id, WHITE, 2_000)
        .await
        .expect("offer draw");
    match offered {
        OfferOutcome::State(s) => {
            assert_eq!(s.offer, OFFER_DRAW);
            assert_eq!(s.by, "w");
            assert_eq!(s.status, "offered");
        }
        other => panic!("expected OfferState, got {other:?}"),
    }

    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert_eq!(meta.pending_offer.as_ref().unwrap().kind, OFFER_DRAW);

    let ended = respond_offer(&mut conn, game_id, BLACK, OFFER_DRAW, true, 3_000)
        .await
        .expect("accept draw");
    match ended {
        OfferOutcome::End(e) => {
            assert_eq!(e.result, "1/2-1/2");
            assert_eq!(e.reason, "agreement");
            assert!(e.winner.is_none());
        }
        other => panic!("expected GameEnd, got {other:?}"),
    }

    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert!(meta.ended);
    assert_eq!(meta.ended_reason.as_deref(), Some("agreement"));
    assert!(meta.pending_offer.is_none());

    let _ = delete_game(&mut conn, game_id).await;
}

#[tokio::test]
async fn redis_draw_counter_offer_accepts() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_draw_counter_offer_accepts: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV165DRAW0000000000000002";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(&mut conn, game_id, &seated_meta("blitz", 60_000))
        .await
        .expect("create");

    offer_draw(&mut conn, game_id, WHITE, 2_000)
        .await
        .expect("offer");
    let ended = offer_draw(&mut conn, game_id, BLACK, 3_000)
        .await
        .expect("counter");
    match ended {
        OfferOutcome::End(e) => assert_eq!(e.reason, "agreement"),
        other => panic!("expected GameEnd, got {other:?}"),
    }
    let _ = delete_game(&mut conn, game_id).await;
}

#[tokio::test]
async fn redis_draw_decline_and_withdraw() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_draw_decline_and_withdraw: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV165DRAW0000000000000003";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(&mut conn, game_id, &seated_meta("blitz", 60_000))
        .await
        .expect("create");

    offer_draw(&mut conn, game_id, WHITE, 2_000)
        .await
        .expect("offer");
    let declined = respond_offer(&mut conn, game_id, BLACK, OFFER_DRAW, false, 3_000)
        .await
        .expect("decline");
    match declined {
        OfferOutcome::State(s) => assert_eq!(s.status, "declined"),
        other => panic!("expected declined, got {other:?}"),
    }
    assert!(load_meta(&mut conn, game_id)
        .await
        .unwrap()
        .pending_offer
        .is_none());

    offer_draw(&mut conn, game_id, BLACK, 4_000)
        .await
        .expect("re-offer");
    let withdrawn = respond_offer(&mut conn, game_id, BLACK, OFFER_DRAW, false, 5_000)
        .await
        .expect("withdraw");
    match withdrawn {
        OfferOutcome::State(s) => assert_eq!(s.status, "withdrawn"),
        other => panic!("expected withdrawn, got {other:?}"),
    }

    let _ = delete_game(&mut conn, game_id).await;
}

#[tokio::test]
async fn redis_move_clears_pending_draw() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_move_clears_pending_draw: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV165DRAW0000000000000004";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(&mut conn, game_id, &seated_meta("blitz", 60_000))
        .await
        .expect("create");

    offer_draw(&mut conn, game_id, BLACK, 2_000)
        .await
        .expect("offer");
    apply_move(&mut conn, game_id, WHITE, "e2e4", 3_000)
        .await
        .expect("move");
    assert!(load_meta(&mut conn, game_id)
        .await
        .unwrap()
        .pending_offer
        .is_none());

    let _ = delete_game(&mut conn, game_id).await;
}

#[tokio::test]
async fn redis_resign_awards_opponent() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_resign_awards_opponent: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV165RESIGN00000000000001";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(&mut conn, game_id, &seated_meta("blitz", 60_000))
        .await
        .expect("create");

    let ended = resign(&mut conn, game_id, WHITE, 2_000)
        .await
        .expect("resign");
    match ended {
        OfferOutcome::End(e) => {
            assert_eq!(e.result, "0-1");
            assert_eq!(e.winner, Some("b"));
            assert_eq!(e.reason, "resign");
        }
        other => panic!("expected GameEnd, got {other:?}"),
    }
    assert!(load_meta(&mut conn, game_id).await.unwrap().ended);

    let again = resign(&mut conn, game_id, BLACK, 3_000).await;
    assert!(matches!(again, Err(OfferError::GameGone)));

    let _ = delete_game(&mut conn, game_id).await;
}

#[tokio::test]
async fn redis_abort_zero_ply_ok_two_plies_denied() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_abort_zero_ply_ok_two_plies_denied: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV165ABORT000000000000001";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(&mut conn, game_id, &seated_meta("blitz", 60_000))
        .await
        .expect("create");

    let ended = abort(&mut conn, game_id, BLACK, 2_000)
        .await
        .expect("abort at 0 plies");
    match ended {
        OfferOutcome::End(e) => {
            assert_eq!(e.result, "*");
            assert_eq!(e.reason, "abort");
            assert!(e.winner.is_none());
        }
        other => panic!("expected abort GameEnd, got {other:?}"),
    }
    let _ = delete_game(&mut conn, game_id).await;

    let game_id2 = "01CV165ABORT000000000000002";
    let _ = delete_game(&mut conn, game_id2).await;
    create_game(&mut conn, game_id2, &seated_meta("blitz", 60_000))
        .await
        .expect("create");
    apply_move(&mut conn, game_id2, WHITE, "e2e4", 2_000)
        .await
        .expect("e4");
    apply_move(&mut conn, game_id2, BLACK, "e7e5", 3_000)
        .await
        .expect("e5");
    let denied = abort(&mut conn, game_id2, WHITE, 4_000).await;
    assert!(matches!(denied, Err(OfferError::AbortDenied(_))));
    assert_eq!(
        denied.unwrap_err().reject_code(),
        OfferRejectCode::AbortDenied
    );

    let _ = delete_game(&mut conn, game_id2).await;
}

#[tokio::test]
async fn redis_abort_one_ply_outside_window_denied() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_abort_one_ply_outside_window_denied: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV165ABORT000000000000003";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(&mut conn, game_id, &seated_meta("bullet", 60_000))
        .await
        .expect("create");
    apply_move(&mut conn, game_id, WHITE, "e2e4", 10_000)
        .await
        .expect("e4");
    // Bullet window 15s; claim at +16s.
    let denied = abort(&mut conn, game_id, BLACK, 26_000).await;
    assert!(matches!(denied, Err(OfferError::AbortDenied(_))));

    let ok = abort(&mut conn, game_id, BLACK, 20_000).await;
    // Fresh game needed — previous denied left game live; retry within window.
    // 20_000 - 10_000 = 10s < 15s → allowed.
    match ok {
        Ok(OfferOutcome::End(e)) => assert_eq!(e.reason, "abort"),
        other => panic!("expected abort within window, got {other:?}"),
    }

    let _ = delete_game(&mut conn, game_id).await;
}

#[tokio::test]
async fn redis_takeback_accept_restores_fen_and_clocks() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_takeback_accept_restores_fen_and_clocks: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV165TAKEBACK000000000001";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(&mut conn, game_id, &seated_meta("blitz", 60_000))
        .await
        .expect("create");

    apply_move(&mut conn, game_id, WHITE, "e2e4", 5_000)
        .await
        .expect("e4");
    let after = load_meta(&mut conn, game_id).await.expect("meta");
    assert!(after.clock_w_ms < 60_000);
    assert_eq!(after.prev_clock_w_ms, 60_000);

    let no_tb = offer_takeback(&mut conn, game_id, WHITE, 6_000)
        .await
        .expect("offer takeback");
    match no_tb {
        OfferOutcome::State(s) => assert_eq!(s.status, "offered"),
        other => panic!("expected offered, got {other:?}"),
    }

    let accepted = respond_offer(&mut conn, game_id, BLACK, OFFER_TAKEBACK, true, 7_000)
        .await
        .expect("accept takeback");
    match accepted {
        OfferOutcome::State(s) => {
            assert_eq!(s.offer, OFFER_TAKEBACK);
            assert_eq!(s.status, "accepted");
        }
        other => panic!("expected accepted OfferState, got {other:?}"),
    }

    let fen = reconstruct_fen(&mut conn, game_id).await.expect("fen");
    assert_eq!(fen, START_FEN);
    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert_eq!(meta.clock_w_ms, 60_000);
    assert_eq!(meta.clock_b_ms, 60_000);
    assert_eq!(meta.active, "w");
    assert!(!meta.ended);
    assert!(meta.pending_offer.is_none());

    // Takeback with zero plies is invalid.
    let empty = offer_takeback(&mut conn, game_id, WHITE, 8_000).await;
    assert!(matches!(empty, Err(OfferError::OfferInvalid(_))));

    let _ = delete_game(&mut conn, game_id).await;
}

#[tokio::test]
async fn redis_offer_rejects_auth_and_invalid() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_offer_rejects_auth_and_invalid: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV165AUTH0000000000000001";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(&mut conn, game_id, &seated_meta("blitz", 60_000))
        .await
        .expect("create");

    let auth = offer_draw(&mut conn, game_id, "not-a-player", 1_000).await;
    assert!(matches!(auth, Err(OfferError::Auth)));

    let none = respond_offer(&mut conn, game_id, WHITE, OFFER_DRAW, true, 2_000).await;
    assert!(matches!(none, Err(OfferError::OfferInvalid(_))));

    offer_draw(&mut conn, game_id, WHITE, 3_000)
        .await
        .expect("offer");
    let dup = offer_draw(&mut conn, game_id, WHITE, 4_000).await;
    assert!(matches!(dup, Err(OfferError::OfferInvalid(_))));

    let own = respond_offer(&mut conn, game_id, WHITE, OFFER_DRAW, true, 5_000).await;
    assert!(matches!(own, Err(OfferError::OfferInvalid(_))));

    let _ = delete_game(&mut conn, game_id).await;
}
