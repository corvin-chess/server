//! CV-167: Quick pair + challenge link (PROTOCOL GameStart / Challenge).

mod common;

use std::env;
use std::time::Duration;

use corvin_server::game::{delete_game, load_meta};
use corvin_server::matchmaking::{
    assign_colors, challenge_accept, challenge_cancel, challenge_create_link, challenge_decline,
    challenge_json, game_start_json, pool_key, quick_pair, quick_pair_cancel, take_pair_result,
    validate_pair_config, ChallengeError, ColorPref, MatchError, PairConfig, QuickPairOutcome,
};
use corvin_server::redis_store::connect;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

const A: &str = "player-a-ulid-cv167-00000001";
const B: &str = "player-b-ulid-cv167-00000001";
const C: &str = "player-c-ulid-cv167-00000001";

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn database_url() -> Option<String> {
    env::var("DATABASE_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

async fn connect_db(url: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
        .unwrap_or_else(|err| panic!("connect DATABASE_URL failed: {err}"))
}

fn blitz_config(color: ColorPref) -> PairConfig {
    PairConfig {
        time_class: "blitz".to_owned(),
        initial_ms: 300_000,
        increment_ms: 2_000,
        rated: true,
        color,
    }
}

#[test]
fn game_start_json_matches_protocol() {
    let cfg = blitz_config(ColorPref::Random);
    let start = game_start_json("01GAMEIDCV167TEST00000001", "w", &cfg, A, B);
    assert_eq!(start["typ"], "GameStart");
    assert_eq!(start["v"], 1);
    assert_eq!(start["game_id"], "01GAMEIDCV167TEST00000001");
    assert_eq!(start["color"], "w");
    assert_eq!(start["config"]["time_class"], "blitz");
    assert_eq!(start["config"]["initial_ms"], 300_000);
    assert_eq!(start["config"]["increment_ms"], 2_000);
    assert_eq!(start["config"]["rated"], true);
    assert_eq!(start["opponents"]["w"]["id"], A);
    assert_eq!(start["opponents"]["b"]["id"], B);
    assert_eq!(start["start_clocks"]["w"], 300_000);
    assert_eq!(start["start_clocks"]["b"], 300_000);
}

#[test]
fn challenge_json_matches_protocol() {
    let cfg = blitz_config(ColorPref::White);
    let msg = challenge_json("01CHALLENGECV167TEST0001", A, &cfg, 1_700_000_000_000);
    assert_eq!(msg["typ"], "Challenge");
    assert_eq!(msg["v"], 1);
    assert_eq!(msg["challenge_id"], "01CHALLENGECV167TEST0001");
    assert_eq!(msg["from"]["id"], A);
    assert_eq!(msg["config"]["link"], true);
    assert_eq!(msg["config"]["color"], "white");
    assert_eq!(msg["expires_ts"], 1_700_000_000_000_u64);
}

#[test]
fn pool_key_encodes_exact_config() {
    let a = blitz_config(ColorPref::Random);
    let mut b = a.clone();
    b.initial_ms = 180_000;
    assert_ne!(pool_key(&a), pool_key(&b));
    let mut casual = a.clone();
    casual.rated = false;
    assert!(pool_key(&a).contains(":rated:"));
    assert!(pool_key(&casual).contains(":casual:"));
}

#[test]
fn validate_rejects_bad_config_and_colors_conflict() {
    let mut bad = blitz_config(ColorPref::Random);
    bad.initial_ms = 0;
    assert!(matches!(
        validate_pair_config(&bad),
        Err(MatchError::InvalidConfig(_))
    ));
    assert!(assign_colors(A, ColorPref::White, B, ColorPref::White, true).is_none());
    assert!(assign_colors(A, ColorPref::Random, A, ColorPref::Random, true).is_none());
}

#[tokio::test]
async fn quick_pair_two_clients_match() {
    let Some(url) = redis_url() else {
        eprintln!("skip quick_pair_two_clients_match: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url).await.expect("redis connect");
    let cfg = blitz_config(ColorPref::Random);
    let now = 1_700_000_100_000_u64;

    let _ = quick_pair_cancel(&mut conn, A).await;
    let _ = quick_pair_cancel(&mut conn, B).await;
    let _ = take_pair_result(&mut conn, A).await;
    let _ = take_pair_result(&mut conn, B).await;

    let first = quick_pair(&mut conn, A, &cfg, now)
        .await
        .expect("A enqueue");
    assert!(matches!(first, QuickPairOutcome::Waiting));

    let second = quick_pair(&mut conn, B, &cfg, now + 1)
        .await
        .expect("B match");
    let QuickPairOutcome::Matched(for_b) = second else {
        panic!("B should match immediately");
    };

    let for_a = take_pair_result(&mut conn, A)
        .await
        .expect("take A")
        .expect("A should have GameStart notify");

    assert_eq!(for_a.game_id, for_b.game_id);
    assert_ne!(for_a.your_color, for_b.your_color);
    assert_eq!(for_a.start["typ"], "GameStart");
    assert_eq!(for_b.start["typ"], "GameStart");
    assert_eq!(
        for_a.start["opponents"]["w"]["id"],
        for_b.start["opponents"]["w"]["id"]
    );

    let meta = load_meta(&mut conn, &for_a.game_id).await.expect("meta");
    assert!(!meta.white_id.is_empty());
    assert!(!meta.black_id.is_empty());
    assert_eq!(meta.clock_w_ms, cfg.initial_ms);
    assert_eq!(meta.increment_ms, cfg.increment_ms);
    assert_eq!(meta.active_started_ms, now + 1);
    assert_eq!(meta.time_class, "blitz");

    let _ = delete_game(&mut conn, &for_a.game_id).await;
}

#[tokio::test]
async fn quick_pair_cancel_prevents_match() {
    let Some(url) = redis_url() else {
        eprintln!("skip quick_pair_cancel_prevents_match: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url).await.expect("redis connect");
    let cfg = blitz_config(ColorPref::Random);
    let now = 1_700_000_200_000_u64;

    let _ = quick_pair_cancel(&mut conn, A).await;
    let _ = quick_pair_cancel(&mut conn, C).await;

    assert!(matches!(
        quick_pair(&mut conn, A, &cfg, now).await.unwrap(),
        QuickPairOutcome::Waiting
    ));
    assert!(quick_pair_cancel(&mut conn, A).await.unwrap());

    let outcome = quick_pair(&mut conn, C, &cfg, now + 1).await.unwrap();
    assert!(
        matches!(outcome, QuickPairOutcome::Waiting),
        "cancelled seek must not leave a matchable peer"
    );
    let _ = quick_pair_cancel(&mut conn, C).await;
}

#[tokio::test]
async fn quick_pair_rejects_self_and_bad_id() {
    let Some(url) = redis_url() else {
        eprintln!("skip quick_pair_rejects_self_and_bad_id: REDIS_URL unset");
        return;
    };
    let mut conn = connect(&url).await.expect("redis connect");
    let cfg = blitz_config(ColorPref::Random);
    let err = quick_pair(&mut conn, "bad id!", &cfg, 1).await.unwrap_err();
    assert!(matches!(err, MatchError::InvalidPlayerId));
}

#[tokio::test]
async fn challenge_link_two_clients_match() {
    let Some(db_url) = database_url() else {
        eprintln!("skip challenge_link_two_clients_match: DATABASE_URL unset");
        return;
    };
    let Some(redis_url) = redis_url() else {
        eprintln!("skip challenge_link_two_clients_match: REDIS_URL unset");
        return;
    };
    let _lock = common::acquire_db_lock(&db_url).await;
    let pool = connect_db(&db_url).await;
    corvin_server::db::migrate_up(&pool)
        .await
        .expect("migrate_up");
    let mut redis = connect(&redis_url).await.expect("redis");

    let cfg = blitz_config(ColorPref::White);
    let now = 1_700_000_300_000_u64;
    let created = challenge_create_link(&pool, A, &cfg, now, 60_000)
        .await
        .expect("create link");
    assert_eq!(created.challenge["typ"], "Challenge");
    assert!(!created.link_token.is_empty());

    let (for_b, for_a_view) = challenge_accept(
        &pool,
        &mut redis,
        &created.challenge_id,
        &created.link_token,
        B,
        now + 1_000,
    )
    .await
    .expect("accept");

    assert_eq!(for_b.game_id, for_a_view.game_id);
    assert_eq!(for_a_view.your_color, "w"); // creator asked white
    assert_eq!(for_b.your_color, "b");

    let notified = take_pair_result(&mut redis, A)
        .await
        .expect("notify")
        .expect("creator GameStart");
    assert_eq!(notified.game_id, for_b.game_id);
    assert_eq!(notified.your_color, "w");

    let meta = load_meta(&mut redis, &for_b.game_id).await.expect("meta");
    assert_eq!(meta.white_id, A);
    assert_eq!(meta.black_id, B);

    // Second accept must fail.
    let again = challenge_accept(
        &pool,
        &mut redis,
        &created.challenge_id,
        &created.link_token,
        C,
        now + 2_000,
    )
    .await;
    assert!(matches!(again, Err(ChallengeError::NotOpen)));

    let _ = delete_game(&mut redis, &for_b.game_id).await;
}

#[tokio::test]
async fn challenge_decline_and_cancel() {
    let Some(db_url) = database_url() else {
        eprintln!("skip challenge_decline_and_cancel: DATABASE_URL unset");
        return;
    };
    let _lock = common::acquire_db_lock(&db_url).await;
    let pool = connect_db(&db_url).await;
    corvin_server::db::migrate_up(&pool)
        .await
        .expect("migrate_up");

    let cfg = blitz_config(ColorPref::Random);
    let now = 1_700_000_400_000_u64;
    let created = challenge_create_link(&pool, A, &cfg, now, 60_000)
        .await
        .expect("create");
    challenge_decline(&pool, &created.challenge_id, &created.link_token, B)
        .await
        .expect("decline");

    let created2 = challenge_create_link(&pool, A, &cfg, now + 1, 60_000)
        .await
        .expect("create2");
    challenge_cancel(&pool, &created2.challenge_id, A)
        .await
        .expect("cancel");
    let bad = challenge_cancel(&pool, &created2.challenge_id, B).await;
    assert!(matches!(bad, Err(ChallengeError::Forbidden)));
}

#[tokio::test]
async fn challenge_self_accept_rejected() {
    let Some(db_url) = database_url() else {
        eprintln!("skip challenge_self_accept_rejected: DATABASE_URL unset");
        return;
    };
    let Some(redis_url) = redis_url() else {
        eprintln!("skip challenge_self_accept_rejected: REDIS_URL unset");
        return;
    };
    let _lock = common::acquire_db_lock(&db_url).await;
    let pool = connect_db(&db_url).await;
    corvin_server::db::migrate_up(&pool)
        .await
        .expect("migrate_up");
    let mut redis = connect(&redis_url).await.expect("redis");
    let cfg = blitz_config(ColorPref::Random);
    let now = 1_700_000_500_000_u64;
    let created = challenge_create_link(&pool, A, &cfg, now, 60_000)
        .await
        .expect("create");
    let err = challenge_accept(
        &pool,
        &mut redis,
        &created.challenge_id,
        &created.link_token,
        A,
        now + 1,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ChallengeError::Match(MatchError::SelfMatch)));
}
