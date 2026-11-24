//! CV-166: Threefold / fifty-move claims + fivefold / seventy-five auto draws (PROTOCOL).

use std::env;

use corvin_server::game::{
    apply_move, claim_denied_json, claim_draw, create_game, delete_game, game_end_claim_json,
    load_meta, AppliedMove, ClaimError, ClaimRejectCode, GameEndOut, GameMeta, START_FEN,
};
use corvin_server::redis_store::connect;

const WHITE: &str = "player-white-ulid-cv166-00001";
const BLACK: &str = "player-black-ulid-cv166-00001";

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn seated_meta(initial_fen: &str) -> GameMeta {
    GameMeta {
        initial_fen: initial_fen.to_owned(),
        created_ms: 1_000,
        white_id: WHITE.to_owned(),
        black_id: BLACK.to_owned(),
        clock_w_ms: 600_000,
        clock_b_ms: 600_000,
        increment_ms: 0,
        active: "w".to_owned(),
        active_started_ms: 1_000,
        time_class: "blitz".to_owned(),
        ..GameMeta::default()
    }
}

#[test]
fn claim_and_game_end_json_match_protocol() {
    let end = game_end_claim_json(
        "g1",
        &GameEndOut {
            result: "1/2-1/2",
            winner: None,
            reason: "threefold",
            seq: 9,
        },
        42,
    );
    assert_eq!(end["typ"], "GameEnd");
    assert_eq!(end["v"], 1);
    assert_eq!(end["game_id"], "g1");
    assert_eq!(end["result"], "1/2-1/2");
    assert_eq!(end["reason"], "threefold");
    assert!(end["winner"].is_null());
    assert_eq!(end["seq"], 9);
    assert_eq!(end["ts"], 42);
    assert!(end.get("flagged").is_none());

    let fifty = game_end_claim_json(
        "g1",
        &GameEndOut {
            result: "1/2-1/2",
            winner: None,
            reason: "fifty",
            seq: 2,
        },
        3,
    );
    assert_eq!(fifty["reason"], "fifty");

    let fivefold = game_end_claim_json(
        "g1",
        &GameEndOut {
            result: "1/2-1/2",
            winner: None,
            reason: "fivefold",
            seq: 1,
        },
        4,
    );
    assert_eq!(fivefold["reason"], "fivefold");

    let seventy = game_end_claim_json(
        "g1",
        &GameEndOut {
            result: "1/2-1/2",
            winner: None,
            reason: "seventy_five",
            seq: 1,
        },
        5,
    );
    assert_eq!(seventy["reason"], "seventy_five");

    let denied = claim_denied_json(Some("req-1"), ClaimRejectCode::ClaimDenied, "not claimable");
    assert_eq!(denied["typ"], "Error");
    assert_eq!(denied["v"], 1);
    assert_eq!(denied["code"], "CLAIM_DENIED");
    assert_eq!(denied["id"], "req-1");
    assert_eq!(denied["message"], "not claimable");
}

#[test]
fn claim_error_maps_reject_codes() {
    assert_eq!(
        ClaimError::ClaimDenied("x".into()).reject_code(),
        ClaimRejectCode::ClaimDenied
    );
    assert_eq!(ClaimError::Auth.reject_code(), ClaimRejectCode::Auth);
    assert_eq!(
        ClaimError::GameGone.reject_code(),
        ClaimRejectCode::GameGone
    );
}

#[tokio::test]
async fn redis_fifty_move_claim_by_side_to_move() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_fifty_move_claim_by_side_to_move: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url).await.expect("redis");
    let game_id = "cv166fifty01";
    let _ = delete_game(&mut conn, game_id).await;

    // Halfmove already 100; White to move may claim fifty.
    let fen = "8/8/8/8/8/4k3/8/4K3 w - - 100 60";
    create_game(&mut conn, game_id, &seated_meta(fen))
        .await
        .expect("create");

    let end = claim_draw(&mut conn, game_id, WHITE, 2_000)
        .await
        .expect("claim fifty");
    assert_eq!(end.result, "1/2-1/2");
    assert!(end.winner.is_none());
    assert_eq!(end.reason, "fifty");

    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert!(meta.ended);
    assert_eq!(meta.ended_reason.as_deref(), Some("fifty"));

    let again = claim_draw(&mut conn, game_id, WHITE, 3_000).await;
    assert!(matches!(again, Err(ClaimError::GameGone)));

    let _ = delete_game(&mut conn, game_id).await;
}

#[tokio::test]
async fn redis_fifty_claim_denied_below_threshold_and_wrong_seat() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_fifty_claim_denied_below_threshold_and_wrong_seat: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url).await.expect("redis");
    let game_id = "cv166fifty02";
    let _ = delete_game(&mut conn, game_id).await;

    let fen = "8/8/8/8/8/4k3/8/4K3 w - - 99 60";
    create_game(&mut conn, game_id, &seated_meta(fen))
        .await
        .expect("create");

    let denied = claim_draw(&mut conn, game_id, WHITE, 2_000).await;
    assert!(matches!(denied, Err(ClaimError::ClaimDenied(_))));

    // Black is not side to move.
    let fen100 = "8/8/8/8/8/4k3/8/4K3 w - - 100 60";
    let game_id2 = "cv166fifty03";
    let _ = delete_game(&mut conn, game_id2).await;
    create_game(&mut conn, game_id2, &seated_meta(fen100))
        .await
        .expect("create");
    let wrong = claim_draw(&mut conn, game_id2, BLACK, 2_000).await;
    assert!(matches!(wrong, Err(ClaimError::ClaimDenied(_))));

    let auth = claim_draw(&mut conn, game_id2, "not-a-player", 2_000).await;
    assert!(matches!(auth, Err(ClaimError::Auth)));

    let _ = delete_game(&mut conn, game_id).await;
    let _ = delete_game(&mut conn, game_id2).await;
}

#[tokio::test]
async fn redis_threefold_claim_after_knight_cycles() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_threefold_claim_after_knight_cycles: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url).await.expect("redis");
    let game_id = "cv166three01";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(&mut conn, game_id, &seated_meta(START_FEN))
        .await
        .expect("create");

    // Two full Nf3/Nf6/Ng1/Ng8 cycles → startpos occurs three times; White to move claims.
    let cycle = ["g1f3", "g8f6", "f3g1", "f6g8"];
    let mut ts = 2_000u64;
    for _ in 0..2 {
        for (i, uci) in cycle.iter().enumerate() {
            let player = if i % 2 == 0 { WHITE } else { BLACK };
            apply_move(&mut conn, game_id, player, uci, ts)
                .await
                .expect("cycle ply");
            ts += 1_000;
        }
    }

    let premature = claim_draw(&mut conn, game_id, BLACK, ts).await;
    assert!(
        matches!(premature, Err(ClaimError::ClaimDenied(_))),
        "Black is not side to move"
    );

    let end = claim_draw(&mut conn, game_id, WHITE, ts)
        .await
        .expect("threefold claim");
    assert_eq!(end.reason, "threefold");
    assert_eq!(end.result, "1/2-1/2");

    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert!(meta.ended);
    assert_eq!(meta.ended_reason.as_deref(), Some("threefold"));

    let _ = delete_game(&mut conn, game_id).await;
}

#[tokio::test]
async fn redis_fivefold_auto_on_apply() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_fivefold_auto_on_apply: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url).await.expect("redis");
    let game_id = "cv166five01";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(&mut conn, game_id, &seated_meta(START_FEN))
        .await
        .expect("create");

    // Four full cycles → fivefold at startpos; the move that completes the 4th cycle
    // returns AutoDraw on AppliedMove (last ply of cycle is Black's Ng8).
    let cycle = ["g1f3", "g8f6", "f3g1", "f6g8"];
    let mut ts = 2_000u64;
    let mut last: Option<AppliedMove> = None;
    for round in 0..4 {
        for (i, uci) in cycle.iter().enumerate() {
            let player = if i % 2 == 0 { WHITE } else { BLACK };
            let applied = apply_move(&mut conn, game_id, player, uci, ts)
                .await
                .expect("cycle ply");
            ts += 1_000;
            if round == 3 && i == 3 {
                last = Some(applied);
            } else {
                assert!(
                    applied.auto_draw.is_none(),
                    "auto draw only on completing fivefold"
                );
            }
        }
    }

    let applied = last.expect("last ply");
    let end = applied.auto_draw.expect("fivefold auto");
    assert_eq!(end.reason, "fivefold");
    assert_eq!(end.result, "1/2-1/2");

    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert!(meta.ended);
    assert_eq!(meta.ended_reason.as_deref(), Some("fivefold"));

    let after = apply_move(&mut conn, game_id, WHITE, "g1f3", ts).await;
    assert!(after.is_err());

    let _ = delete_game(&mut conn, game_id).await;
}

#[tokio::test]
async fn redis_seventy_five_auto_on_apply() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_seventy_five_auto_on_apply: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url).await.expect("redis");
    let game_id = "cv166sev501";
    let _ = delete_game(&mut conn, game_id).await;

    // Halfmove 149; a quiet king move bumps to 150 → auto seventy_five.
    let fen = "8/8/8/8/8/4k3/8/4K3 w - - 149 80";
    create_game(&mut conn, game_id, &seated_meta(fen))
        .await
        .expect("create");

    // e1d1 stays clear of the black king on e3.
    let applied = apply_move(&mut conn, game_id, WHITE, "e1d1", 2_000)
        .await
        .expect("quiet move");
    let end = applied.auto_draw.expect("seventy_five auto");
    assert_eq!(end.reason, "seventy_five");
    assert_eq!(end.result, "1/2-1/2");

    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert!(meta.ended);
    assert_eq!(meta.ended_reason.as_deref(), Some("seventy_five"));

    let _ = delete_game(&mut conn, game_id).await;
}
