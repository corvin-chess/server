//! CV-171: Atomic apply_move Lua/CAS — concurrent apply cannot double-append.

use std::env;
use std::sync::Arc;

use corvin_server::game::{
    apply_move, create_game, delete_game, load_events, parse_cas_commit_reply, reconstruct_fen,
    ApplyError, CasCommitResult, GameMeta, START_FEN,
};
use corvin_server::redis_store::connect;
use redis::aio::ConnectionManager;
use tokio::sync::Barrier;

const AFTER_E4: &str = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1";
const AFTER_D4: &str = "rnbqkbnr/pppppppp/8/8/3P4/8/PPP1PPPP/RNBQKBNR b KQkq d3 0 1";
const WHITE: &str = "player-white-ulid-cv171-0001";
const BLACK: &str = "player-black-ulid-cv171-0001";

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn seated_meta() -> GameMeta {
    GameMeta {
        initial_fen: START_FEN.to_owned(),
        created_ms: 1_730_000_000_000,
        white_id: WHITE.to_owned(),
        black_id: BLACK.to_owned(),
        clock_w_ms: 300_000,
        clock_b_ms: 300_000,
        ..GameMeta::default()
    }
}

#[test]
fn cas_commit_reply_parsing() {
    assert_eq!(
        parse_cas_commit_reply(&[1, 0, 0]),
        CasCommitResult::Ok {
            move_seq: 0,
            last_seq: 0
        }
    );
    assert_eq!(
        parse_cas_commit_reply(&[1, 5, 5]),
        CasCommitResult::Ok {
            move_seq: 5,
            last_seq: 5
        }
    );
    assert_eq!(
        parse_cas_commit_reply(&[1, 3, 4]),
        CasCommitResult::Ok {
            move_seq: 3,
            last_seq: 4
        }
    );
    assert_eq!(
        parse_cas_commit_reply(&[0, 2]),
        CasCommitResult::Conflict { actual_len: 2 }
    );
    assert_eq!(parse_cas_commit_reply(&[-1]), CasCommitResult::Gone);
    assert_eq!(
        parse_cas_commit_reply(&[-2]),
        CasCommitResult::TooManyEvents
    );
    assert_eq!(
        parse_cas_commit_reply(&[9]),
        CasCommitResult::Corrupt("unexpected CAS reply".into())
    );
}

#[test]
fn apply_error_conflict_is_not_move_rejected() {
    assert_eq!(ApplyError::Conflict.reject_code(), None);
}

#[tokio::test]
async fn redis_concurrent_identical_uci_one_ply() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_concurrent_identical_uci_one_ply: REDIS_URL unset");
        return;
    };

    let mut setup = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV171CONCURRENTE2E4000001";
    let _ = delete_game(&mut setup, game_id).await;
    create_game(&mut setup, game_id, &seated_meta())
        .await
        .expect("create");

    let barrier = Arc::new(Barrier::new(2));
    let url = Arc::new(url);
    let game = Arc::new(game_id.to_owned());

    let run = |uci: &'static str, ts: u64| {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            apply_move(&mut conn, game.as_str(), WHITE, uci, ts).await
        }
    };

    let (a, b) = tokio::join!(run("e2e4", 10), run("e2e4", 11));
    let oks = [&a, &b].iter().filter(|r| r.is_ok()).count();
    let errs = [&a, &b].iter().filter(|r| r.is_err()).count();
    assert_eq!(oks, 1, "exactly one apply must win: {a:?} {b:?}");
    assert_eq!(errs, 1, "loser must fail: {a:?} {b:?}");

    let loser = if a.is_err() { &a } else { &b };
    assert!(
        matches!(loser, Err(ApplyError::NotYourTurn)),
        "loser after CAS retry should be NOT_YOUR_TURN: {loser:?}"
    );

    let events = load_events(&mut setup, game_id).await.expect("events");
    assert_eq!(events.len(), 1, "one Move event only");
    assert_eq!(
        reconstruct_fen(&mut setup, game_id).await.expect("fen"),
        AFTER_E4
    );

    delete_game(&mut setup, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_concurrent_racing_white_ucis_one_ply() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_concurrent_racing_white_ucis_one_ply: REDIS_URL unset");
        return;
    };

    let mut setup = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV171CONCURRENTD2D4000001";
    let _ = delete_game(&mut setup, game_id).await;
    create_game(&mut setup, game_id, &seated_meta())
        .await
        .expect("create");

    let barrier = Arc::new(Barrier::new(2));
    let url = Arc::new(url);
    let game = Arc::new(game_id.to_owned());

    let left = {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn: ConnectionManager = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            apply_move(&mut conn, game.as_str(), WHITE, "e2e4", 20).await
        }
    };
    let right = {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn: ConnectionManager = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            apply_move(&mut conn, game.as_str(), WHITE, "d2d4", 21).await
        }
    };

    let (a, b) = tokio::join!(left, right);
    let oks: Vec<_> = [a, b].into_iter().filter_map(Result::ok).collect();
    assert_eq!(oks.len(), 1, "exactly one white ply may commit");
    let fen = reconstruct_fen(&mut setup, game_id).await.expect("fen");
    assert!(
        fen == AFTER_E4 || fen == AFTER_D4,
        "winner FEN must be e4 or d4, got {fen}"
    );
    assert_eq!(
        load_events(&mut setup, game_id)
            .await
            .expect("events")
            .len(),
        1
    );

    delete_game(&mut setup, game_id).await.expect("cleanup");
}
