//! CV-164: Lag compensation quota with injectable fake RTT (ADR-005).

use std::env;

use corvin_server::game::{
    apply_move_with_rtt, claim_flag, clock_sync_json, create_game, credit_lag, delete_game,
    estimated_game_ms, initial_quota_ms, load_meta, move_applied_json, FlagClaimError, GameMeta,
    LagTracker, QUOTA_CAP_MS, QUOTA_FLOOR_MS, START_FEN,
};
use corvin_server::redis_store::connect;

const WHITE: &str = "player-white-ulid-cv164-00001";
const BLACK: &str = "player-black-ulid-cv164-00001";

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[test]
fn estimated_game_ms_is_initial_plus_forty_increments() {
    // 5+3 → 300_000 + 40*3_000 = 420_000 (public TC estimate shape).
    assert_eq!(estimated_game_ms(300_000, 3_000), 420_000);
    assert_eq!(estimated_game_ms(60_000, 0), 60_000);
}

#[test]
fn initial_quota_ms_clamps_to_published_interim_bounds() {
    // Short TC: floor.
    assert_eq!(initial_quota_ms(5_000, 0), QUOTA_FLOOR_MS);
    // Mid: estimated/60.
    assert_eq!(initial_quota_ms(30_000, 0), 500);
    // Long: cap (CV-305 will remeasure).
    assert_eq!(initial_quota_ms(300_000, 0), QUOTA_CAP_MS);
    assert_eq!(initial_quota_ms(0, 0), 0);
}

#[test]
fn credit_lag_takes_min_of_one_way_and_remaining_quota() {
    let full = credit_lag(80, 200);
    assert_eq!(full.credit_ms, 80);
    assert_eq!(full.remaining_quota_ms, 120);

    let capped = credit_lag(500, 100);
    assert_eq!(capped.credit_ms, 100);
    assert_eq!(capped.remaining_quota_ms, 0);

    let none = credit_lag(40, 0);
    assert_eq!(none.credit_ms, 0);
    assert_eq!(none.remaining_quota_ms, 0);
}

#[test]
fn lag_tracker_ewma_one_way_is_half_rtt() {
    let mut tracker = LagTracker::new();
    assert_eq!(tracker.one_way_ms(), None);

    tracker.observe_rtt(200);
    assert_eq!(tracker.ewma_rtt_ms(), Some(200));
    assert_eq!(tracker.one_way_ms(), Some(100));

    // α=1/4: (100 + 3*200) / 4 = 175
    tracker.observe_rtt(100);
    assert_eq!(tracker.ewma_rtt_ms(), Some(175));
    assert_eq!(tracker.one_way_ms(), Some(87));
}

#[test]
fn move_applied_json_includes_optional_lag_comp_ms() {
    let with = move_applied_json(
        None,
        "01CV164GAME0000000000000001",
        "e2e4",
        "e4",
        START_FEN,
        59_900,
        60_000,
        "b",
        1,
        50,
        Some(100),
    );
    assert_eq!(with["lag_comp_ms"], 100);

    let without = move_applied_json(
        None,
        "01CV164GAME0000000000000001",
        "e2e4",
        "e4",
        START_FEN,
        60_000,
        60_000,
        "b",
        1,
        50,
        None,
    );
    assert!(without.get("lag_comp_ms").is_none());
}

#[test]
fn clock_sync_exposes_lag_fields_for_display() {
    let sync = clock_sync_json("g1", 1, 2, "w", 9, 40, 50, 3);
    assert_eq!(sync["lag"]["you_ms"], 40);
    assert_eq!(sync["lag"]["opp_ms"], 50);
    assert_eq!(sync["lag"]["server_ms"], 3);
}

#[tokio::test]
async fn redis_apply_credits_fake_rtt_and_depletes_quota() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_apply_credits_fake_rtt_and_depletes_quota: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV164LAGCREDIT00000000001";
    let _ = delete_game(&mut conn, game_id).await;

    let quota = initial_quota_ms(60_000, 0);
    assert_eq!(quota, QUOTA_CAP_MS);

    create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1,
            white_id: WHITE.to_owned(),
            black_id: BLACK.to_owned(),
            clock_w_ms: 60_000,
            clock_b_ms: 60_000,
            increment_ms: 0,
            active_started_ms: 1_000,
            lag_quota_w_ms: quota,
            lag_quota_b_ms: quota,
            ..GameMeta::default()
        },
    )
    .await
    .expect("create");

    // Think 1000ms wall, RTT 200 → one-way 100 credited; remaining 59000+100 wait:
    // effective elapsed 900 → clock 59100; quota 900 left.
    let applied = apply_move_with_rtt(&mut conn, game_id, WHITE, "e2e4", 2_000, Some(200))
        .await
        .expect("e2e4");
    assert_eq!(applied.lag_comp_ms, Some(100));
    assert_eq!(applied.clock_w_ms, 59_100);
    assert_eq!(applied.clock_b_ms, 60_000);

    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert_eq!(meta.lag_quota_w_ms, quota - 100);
    assert_eq!(meta.lag_quota_b_ms, quota);

    delete_game(&mut conn, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_apply_without_rtt_credits_zero() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_apply_without_rtt_credits_zero: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV164LAGNORT000000000001";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1,
            white_id: WHITE.to_owned(),
            black_id: BLACK.to_owned(),
            clock_w_ms: 10_000,
            clock_b_ms: 10_000,
            increment_ms: 0,
            active_started_ms: 5_000,
            lag_quota_w_ms: 500,
            lag_quota_b_ms: 500,
            ..GameMeta::default()
        },
    )
    .await
    .expect("create");

    let applied = apply_move_with_rtt(&mut conn, game_id, WHITE, "e2e4", 7_000, None)
        .await
        .expect("e2e4");
    assert_eq!(applied.lag_comp_ms, None);
    assert_eq!(applied.clock_w_ms, 8_000);
    let meta = load_meta(&mut conn, game_id).await.expect("meta");
    assert_eq!(meta.lag_quota_w_ms, 500);

    delete_game(&mut conn, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_apply_caps_credit_at_remaining_quota() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_apply_caps_credit_at_remaining_quota: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV164LAGCAPQUOTA000000001";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1,
            white_id: WHITE.to_owned(),
            black_id: BLACK.to_owned(),
            clock_w_ms: 10_000,
            clock_b_ms: 10_000,
            increment_ms: 0,
            active_started_ms: 1_000,
            lag_quota_w_ms: 50,
            lag_quota_b_ms: 50,
            ..GameMeta::default()
        },
    )
    .await
    .expect("create");

    // RTT 400 → one-way 200, but quota only 50.
    let applied = apply_move_with_rtt(&mut conn, game_id, WHITE, "e2e4", 2_000, Some(400))
        .await
        .expect("e2e4");
    assert_eq!(applied.lag_comp_ms, Some(50));
    assert_eq!(applied.clock_w_ms, 9_050);

    delete_game(&mut conn, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_claim_flag_respects_stored_one_way_credit() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_claim_flag_respects_stored_one_way_credit: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV164LAGCLAIMONEWAY000001";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1,
            white_id: WHITE.to_owned(),
            black_id: BLACK.to_owned(),
            clock_w_ms: 400,
            clock_b_ms: 60_000,
            increment_ms: 0,
            active_started_ms: 1_000,
            lag_quota_w_ms: 500,
            lag_quota_b_ms: 500,
            lag_one_way_w_ms: 150,
            ..GameMeta::default()
        },
    )
    .await
    .expect("create");

    // Wall 500ms vs 400 stock; with 150ms one-way credit → not flagged.
    let early = claim_flag(&mut conn, game_id, BLACK, "w", 1_500)
        .await
        .expect_err("lag credit should block claim");
    assert!(matches!(early, FlagClaimError::NotFlagged));

    delete_game(&mut conn, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_apply_lag_credit_can_prevent_false_flag() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_apply_lag_credit_can_prevent_false_flag: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV164LAGSAVEFLAG00000001";
    let _ = delete_game(&mut conn, game_id).await;

    // 400ms stock, 500ms wall elapsed, RTT 300 → one-way 150 → effective 350 → survives.
    create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1,
            white_id: WHITE.to_owned(),
            black_id: BLACK.to_owned(),
            clock_w_ms: 400,
            clock_b_ms: 60_000,
            increment_ms: 0,
            active_started_ms: 1_000,
            lag_quota_w_ms: 500,
            lag_quota_b_ms: 500,
            ..GameMeta::default()
        },
    )
    .await
    .expect("create");

    let applied = apply_move_with_rtt(&mut conn, game_id, WHITE, "e2e4", 1_500, Some(300))
        .await
        .expect("should not flag");
    assert_eq!(applied.lag_comp_ms, Some(150));
    assert_eq!(applied.clock_w_ms, 50);

    delete_game(&mut conn, game_id).await.expect("cleanup");
}
