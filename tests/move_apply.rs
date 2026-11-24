//! CV-162: Move apply + illegal reject; PROTOCOL MoveApplied / MoveRejected contracts.

use std::env;

use corvin_server::game::{
    apply_move, create_game, delete_game, move_applied_json, move_rejected_json, reconstruct_fen,
    ApplyError, GameMeta, MoveRejectCode, START_FEN,
};
use corvin_server::redis_store::connect;
use serde_json::json;

const AFTER_E4: &str = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1";
const WHITE: &str = "player-white-ulid-00000001";
const BLACK: &str = "player-black-ulid-00000001";

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
fn move_applied_json_matches_protocol_shape() {
    let body = move_applied_json(
        Some("client-move-1"),
        "01CV162GAME0000000000000001",
        "e2e4",
        "e4",
        AFTER_E4,
        300_000,
        300_000,
        "b",
        1,
        1_730_000_000_100,
        None,
    );
    assert_eq!(body["typ"], "MoveApplied");
    assert_eq!(body["v"], 1);
    assert_eq!(body["id"], "client-move-1");
    assert_eq!(body["game_id"], "01CV162GAME0000000000000001");
    assert_eq!(body["uci"], "e2e4");
    assert_eq!(body["san"], "e4");
    assert_eq!(body["fen"], AFTER_E4);
    assert_eq!(body["seq"], 1);
    assert_eq!(body["active"], "b");
    assert_eq!(body["clocks"]["w"], 300_000);
    assert_eq!(body["clocks"]["b"], 300_000);
    assert_eq!(body["ts"].as_u64(), Some(1_730_000_000_100));
    assert!(body.get("lag_comp_ms").is_none());
}

#[test]
fn move_rejected_json_matches_protocol_codes() {
    for (code, expected) in [
        (MoveRejectCode::IllegalMove, "ILLEGAL_MOVE"),
        (MoveRejectCode::NotYourTurn, "NOT_YOUR_TURN"),
        (MoveRejectCode::GameGone, "GAME_GONE"),
        (MoveRejectCode::Auth, "AUTH"),
    ] {
        let body = move_rejected_json("client-bad", code, "test");
        assert_eq!(body["typ"], "MoveRejected");
        assert_eq!(body["v"], 1);
        assert_eq!(body["id"], "client-bad");
        assert_eq!(body["code"], expected);
        assert_eq!(body["message"], "test");
    }
}

#[test]
fn apply_error_maps_to_protocol_reject_codes() {
    assert_eq!(
        ApplyError::IllegalMove("e2e5".into()).reject_code(),
        Some(MoveRejectCode::IllegalMove)
    );
    assert_eq!(
        ApplyError::NotYourTurn.reject_code(),
        Some(MoveRejectCode::NotYourTurn)
    );
    assert_eq!(
        ApplyError::GameGone.reject_code(),
        Some(MoveRejectCode::GameGone)
    );
    assert_eq!(ApplyError::Auth.reject_code(), Some(MoveRejectCode::Auth));
    assert_eq!(
        ApplyError::Corrupt("x".into()).reject_code(),
        None,
        "storage faults are Error INTERNAL, not MoveRejected"
    );
    assert_eq!(
        move_rejected_json(
            "x",
            ApplyError::IllegalMove("e2e5".into())
                .reject_code()
                .expect("reject"),
            "n"
        )["code"],
        json!("ILLEGAL_MOVE")
    );
}

#[tokio::test]
async fn redis_apply_legal_move_returns_applied_fields() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_apply_legal_move_returns_applied_fields: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV162APPLYLEGALMOVE00001";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(&mut conn, game_id, &seated_meta())
        .await
        .expect("create_game");

    let applied = apply_move(&mut conn, game_id, WHITE, "e2e4", 10)
        .await
        .expect("legal e2e4");
    assert_eq!(applied.uci, "e2e4");
    assert_eq!(applied.san, "e4");
    assert_eq!(applied.fen, AFTER_E4);
    assert_eq!(applied.seq, 1);
    assert_eq!(applied.active, "b");
    assert_eq!(applied.clock_w_ms, 300_000);
    assert_eq!(applied.clock_b_ms, 300_000);

    assert_eq!(
        reconstruct_fen(&mut conn, game_id)
            .await
            .expect("reconstruct"),
        AFTER_E4
    );

    let wire = move_applied_json(
        Some("c1"),
        game_id,
        &applied.uci,
        &applied.san,
        &applied.fen,
        applied.clock_w_ms,
        applied.clock_b_ms,
        applied.active,
        applied.seq,
        11,
        applied.lag_comp_ms,
    );
    assert_eq!(wire["typ"], "MoveApplied");
    assert_eq!(wire["san"], "e4");
    assert_eq!(wire["seq"], 1);

    delete_game(&mut conn, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_apply_rejects_illegal_and_wrong_turn() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_apply_rejects_illegal_and_wrong_turn: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV162APPLYREJECT00000001";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(&mut conn, game_id, &seated_meta())
        .await
        .expect("create_game");

    let illegal = apply_move(&mut conn, game_id, WHITE, "e2e5", 1)
        .await
        .expect_err("illegal");
    assert!(matches!(illegal, ApplyError::IllegalMove(_)));
    assert_eq!(illegal.reject_code(), Some(MoveRejectCode::IllegalMove));

    let wrong = apply_move(&mut conn, game_id, BLACK, "e2e4", 2)
        .await
        .expect_err("black cannot move first");
    assert!(matches!(wrong, ApplyError::NotYourTurn));
    assert_eq!(wrong.reject_code(), Some(MoveRejectCode::NotYourTurn));

    let stranger = apply_move(&mut conn, game_id, "not-a-player", "e2e4", 3)
        .await
        .expect_err("unseated");
    assert!(matches!(stranger, ApplyError::Auth));

    let gone = apply_move(&mut conn, "01CV162NOSUCHGAME00000001", WHITE, "e2e4", 4)
        .await
        .expect_err("missing game");
    assert!(matches!(gone, ApplyError::GameGone));

    // Illegal path must not append.
    assert_eq!(
        reconstruct_fen(&mut conn, game_id)
            .await
            .expect("still start"),
        START_FEN
    );

    delete_game(&mut conn, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_apply_rejects_unseated_game() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_apply_rejects_unseated_game: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV162APPLYUNSEATED000001";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1,
            ..GameMeta::default()
        },
    )
    .await
    .expect("create unseated");

    let err = apply_move(&mut conn, game_id, WHITE, "e2e4", 1)
        .await
        .expect_err("empty seats");
    assert!(matches!(err, ApplyError::Auth));
    assert_eq!(err.reject_code(), Some(MoveRejectCode::Auth));

    delete_game(&mut conn, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_apply_alternating_turns_two_plies() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_apply_alternating_turns_two_plies: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV162APPLYTWOPLIES000001";
    let _ = delete_game(&mut conn, game_id).await;
    create_game(&mut conn, game_id, &seated_meta())
        .await
        .expect("create");

    let first = apply_move(&mut conn, game_id, WHITE, "e2e4", 1)
        .await
        .expect("e4");
    assert_eq!(first.seq, 1);
    assert_eq!(first.active, "b");

    let again = apply_move(&mut conn, game_id, WHITE, "d2d4", 2)
        .await
        .expect_err("white again");
    assert!(matches!(again, ApplyError::NotYourTurn));

    let second = apply_move(&mut conn, game_id, BLACK, "e7e5", 3)
        .await
        .expect("e5");
    assert_eq!(second.seq, 2);
    assert_eq!(second.san, "e5");
    assert_eq!(second.active, "w");

    delete_game(&mut conn, game_id).await.expect("cleanup");
}
