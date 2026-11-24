//! CV-163: Clocks + Fischer increment + flag; injectable now_ms; PROTOCOL shapes.

use std::env;

use corvin_server::game::{
    apply_move, check_flag, claim_flag, clock_sync_json, create_game, delete_game,
    game_end_flag_json, load_meta, remaining_at, tick_on_move, ApplyError, ClockPair, ClockTick,
    FlagClaimError, FlagOutcome, GameMeta, MoveRejectCode, START_FEN,
};
use corvin_server::redis_store::connect;

const WHITE: &str = "player-white-ulid-cv163-00001";
const BLACK: &str = "player-black-ulid-cv163-00001";

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn pair(w: u64, b: u64, inc: u64, active: &str, started: u64) -> ClockPair {
    ClockPair {
        w_ms: w,
        b_ms: b,
        increment_ms: inc,
        active: active.to_owned(),
        active_started_ms: started,
    }
}

#[test]
fn remaining_at_subtracts_elapsed_from_active_only() {
    let clocks = pair(60_000, 60_000, 0, "w", 1_000);
    let (w, b) = remaining_at(&clocks, 1_250);
    assert_eq!(w, 59_750);
    assert_eq!(b, 60_000);
}

#[test]
fn remaining_at_saturates_at_zero() {
    let clocks = pair(100, 50_000, 0, "b", 0);
    // active_started_ms 0 → no elapsed until started
    let (w, b) = remaining_at(&clocks, 9_999);
    assert_eq!(w, 100);
    assert_eq!(b, 50_000);

    let running = pair(100, 400, 0, "b", 1_000);
    let (w2, b2) = remaining_at(&running, 1_500);
    assert_eq!(w2, 100);
    assert_eq!(b2, 0, "400ms stock with 500ms elapsed saturates at zero");
}

#[test]
fn tick_on_move_deducts_then_adds_fischer_increment() {
    // White thinks 2s on 5+3; remaining becomes 3000+3000=6000, Black still 5000, active b.
    let clocks = pair(5_000, 5_000, 3_000, "w", 10_000);
    match tick_on_move(&clocks, 12_000) {
        ClockTick::Applied(next) => {
            assert_eq!(next.w_ms, 6_000);
            assert_eq!(next.b_ms, 5_000);
            assert_eq!(next.active, "b");
            assert_eq!(next.active_started_ms, 12_000);
            assert_eq!(next.increment_ms, 3_000);
        }
        other => panic!("expected Applied, got {other:?}"),
    }
}

#[test]
fn tick_on_move_flags_before_increment() {
    // White has 1500ms, thinks 2000ms → flag; increment must not rescue.
    let clocks = pair(1_500, 10_000, 5_000, "w", 100);
    match tick_on_move(&clocks, 2_100) {
        ClockTick::Flagged { color, clocks } => {
            assert_eq!(color, "w");
            assert_eq!(clocks.w_ms, 0);
            assert_eq!(clocks.b_ms, 10_000);
        }
        other => panic!("expected Flagged, got {other:?}"),
    }
}

#[test]
fn tick_on_move_zero_elapsed_when_not_started() {
    let clocks = pair(300_000, 300_000, 2_000, "w", 0);
    match tick_on_move(&clocks, 50) {
        ClockTick::Applied(next) => {
            assert_eq!(next.w_ms, 302_000);
            assert_eq!(next.b_ms, 300_000);
            assert_eq!(next.active, "b");
            assert_eq!(next.active_started_ms, 50);
        }
        other => panic!("expected Applied, got {other:?}"),
    }
}

#[test]
fn clock_sync_and_game_end_json_match_protocol() {
    let sync = clock_sync_json(
        "01CV163GAME0000000000000001",
        59_000,
        60_000,
        "w",
        1_730_000_000_500,
        12,
        34,
        5,
    );
    assert_eq!(sync["typ"], "ClockSync");
    assert_eq!(sync["v"], 1);
    assert_eq!(sync["game_id"], "01CV163GAME0000000000000001");
    assert_eq!(sync["clocks"]["w"], 59_000);
    assert_eq!(sync["clocks"]["b"], 60_000);
    assert_eq!(sync["active"], "w");
    assert_eq!(sync["server_time"], 1_730_000_000_500u64);
    assert_eq!(sync["lag"]["you_ms"], 12);
    assert_eq!(sync["lag"]["opp_ms"], 34);
    assert_eq!(sync["lag"]["server_ms"], 5);

    let end = game_end_flag_json(
        "01CV163GAME0000000000000001",
        &FlagOutcome {
            result: "1-0",
            winner: Some("w"),
            reason: "flag",
            flagged: "b",
        },
        99,
    );
    assert_eq!(end["typ"], "GameEnd");
    assert_eq!(end["v"], 1);
    assert_eq!(end["result"], "1-0");
    assert_eq!(end["winner"], "w");
    assert_eq!(end["reason"], "flag");
    assert_eq!(end["flagged"], "b");
    assert_eq!(end["ts"], 99);
}

#[test]
fn apply_error_flagged_maps_to_game_gone_reject() {
    // Flag mid-apply is not a MoveRejected legality code; callers send GameEnd.
    let flagged = ApplyError::Flagged {
        outcome: FlagOutcome {
            result: "0-1",
            winner: Some("b"),
            reason: "flag",
            flagged: "w",
        },
    };
    assert_eq!(flagged.reject_code(), None);
    assert_eq!(
        MoveRejectCode::GameGone.as_str(),
        "GAME_GONE",
        "ended games still surface GAME_GONE on stale moves"
    );
}

#[tokio::test]
async fn redis_apply_ticks_clock_and_persists_increment() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_apply_ticks_clock_and_persists_increment: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV163APPLYTICKINC00000001";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1_000,
            white_id: WHITE.to_owned(),
            black_id: BLACK.to_owned(),
            clock_w_ms: 10_000,
            clock_b_ms: 10_000,
            increment_ms: 1_000,
            active_started_ms: 5_000,
            ..GameMeta::default()
        },
    )
    .await
    .expect("create");

    // White thinks 2s: 10000-2000+1000 = 9000; Black unchanged; active b at ts 7000.
    let applied = apply_move(&mut conn, game_id, WHITE, "e2e4", 7_000)
        .await
        .expect("e2e4");
    assert_eq!(applied.clock_w_ms, 9_000);
    assert_eq!(applied.clock_b_ms, 10_000);
    assert_eq!(applied.active, "b");

    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert_eq!(meta.clock_w_ms, 9_000);
    assert_eq!(meta.clock_b_ms, 10_000);
    assert_eq!(meta.increment_ms, 1_000);
    assert_eq!(meta.active_started_ms, 7_000);

    delete_game(&mut conn, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_apply_rejects_when_mover_already_flagged() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_apply_rejects_when_mover_already_flagged: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV163APPLYFLAGGED0000001";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1,
            white_id: WHITE.to_owned(),
            black_id: BLACK.to_owned(),
            clock_w_ms: 500,
            clock_b_ms: 60_000,
            increment_ms: 10_000,
            active_started_ms: 1_000,
            ..GameMeta::default()
        },
    )
    .await
    .expect("create");

    // Elapsed 2000 > 500; increment must not apply; move must not append.
    let err = apply_move(&mut conn, game_id, WHITE, "e2e4", 3_000)
        .await
        .expect_err("flagged");
    match &err {
        ApplyError::Flagged { outcome } => {
            assert_eq!(outcome.flagged, "w");
            assert_eq!(outcome.winner, Some("b"));
            assert_eq!(outcome.result, "0-1");
            assert_eq!(outcome.reason, "flag");
        }
        other => panic!("expected Flagged, got {other:?}"),
    }
    assert_eq!(err.reject_code(), None);

    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert_eq!(meta.clock_w_ms, 0);
    assert_eq!(meta.clock_b_ms, 60_000);
    assert!(meta.ended);
    assert_eq!(meta.ended_reason.as_deref(), Some("flag:0-1"));

    delete_game(&mut conn, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_claim_flag_verifies_and_returns_outcome() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_claim_flag_verifies_and_returns_outcome: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV163CLAIMFLAG0000000001";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1,
            white_id: WHITE.to_owned(),
            black_id: BLACK.to_owned(),
            clock_w_ms: 100,
            clock_b_ms: 60_000,
            increment_ms: 0,
            active_started_ms: 1_000,
            ..GameMeta::default()
        },
    )
    .await
    .expect("create");

    assert!(
        check_flag(&mut conn, game_id, 1_050)
            .await
            .expect("check")
            .is_none(),
        "50ms elapsed of 100ms remaining is not yet flagged"
    );

    let outcome = claim_flag(&mut conn, game_id, BLACK, "w", 1_200)
        .await
        .expect("claim");
    assert_eq!(outcome.flagged, "w");
    assert_eq!(outcome.winner, Some("b"));
    assert_eq!(outcome.result, "0-1");
    assert_eq!(outcome.reason, "flag");

    let again = claim_flag(&mut conn, game_id, BLACK, "w", 1_300)
        .await
        .expect_err("already ended");
    assert!(matches!(again, FlagClaimError::GameGone));

    // Premature claim rejected.
    let game_id2 = "01CV163CLAIMFLAGEARLY000001";
    let _ = delete_game(&mut conn, game_id2).await;
    create_game(
        &mut conn,
        game_id2,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1,
            white_id: WHITE.to_owned(),
            black_id: BLACK.to_owned(),
            clock_w_ms: 5_000,
            clock_b_ms: 5_000,
            increment_ms: 0,
            active_started_ms: 1_000,
            ..GameMeta::default()
        },
    )
    .await
    .expect("create2");
    let early = claim_flag(&mut conn, game_id2, BLACK, "w", 1_100)
        .await
        .expect_err("still time");
    assert!(matches!(early, FlagClaimError::NotFlagged));

    delete_game(&mut conn, game_id).await.expect("cleanup");
    delete_game(&mut conn, game_id2).await.expect("cleanup");
}
