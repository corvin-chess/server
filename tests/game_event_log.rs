//! CV-161: Redis game event log; replay reconstructs FEN.

use std::env;

use corvin_server::game::{
    append_move_event, create_game, delete_game, game_events_key, game_meta_key, load_events,
    reconstruct_fen, replay_fen, GameEvent, GameLogError, GameMeta, MAX_EVENTS, MAX_FEN_LEN,
    MAX_GAME_ID_LEN, MAX_UCI_LEN, START_FEN,
};
use corvin_server::redis_store::connect;

const AFTER_E4: &str = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1";
const AFTER_E4_E5: &str = "rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq e6 0 2";
const AFTER_E4_E5_NF3: &str = "rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2";

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[test]
fn game_key_shapes_are_stable_and_scoped() {
    assert_eq!(
        game_meta_key("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        "corvin:game:01ARZ3NDEKTSV4RRFFQ69G5FAV:meta"
    );
    assert_eq!(
        game_events_key("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        "corvin:game:01ARZ3NDEKTSV4RRFFQ69G5FAV:events"
    );
    assert!(game_meta_key("").is_empty());
    assert!(game_events_key("bad id").is_empty());
    assert!(game_meta_key(&"x".repeat(MAX_GAME_ID_LEN + 1)).is_empty());
    for bad in ["a/b", "a b", "a:b", "../x"] {
        assert!(
            game_meta_key(bad).is_empty(),
            "invalid game id {bad:?} must yield empty key"
        );
    }
}

#[test]
fn replay_fen_reconstructs_known_line_without_redis() {
    let events = [
        GameEvent::Move {
            seq: 1,
            uci: "e2e4".into(),
            ts: 1,
        },
        GameEvent::Move {
            seq: 2,
            uci: "e7e5".into(),
            ts: 2,
        },
        GameEvent::Move {
            seq: 3,
            uci: "g1f3".into(),
            ts: 3,
        },
    ];
    assert_eq!(replay_fen(START_FEN, &events[..1]).expect("e4"), AFTER_E4);
    assert_eq!(
        replay_fen(START_FEN, &events[..2]).expect("e4 e5"),
        AFTER_E4_E5
    );
    assert_eq!(
        replay_fen(START_FEN, &events).expect("e4 e5 Nf3"),
        AFTER_E4_E5_NF3
    );
    assert_eq!(replay_fen(START_FEN, &[]).expect("empty"), START_FEN);
}

#[test]
fn replay_fen_rejects_illegal_uci_and_bad_initial() {
    const {
        assert!(MAX_UCI_LEN >= 5);
        assert!(MAX_FEN_LEN >= 80);
        assert!(MAX_EVENTS >= 100);
    }
    let bad = [GameEvent::Move {
        seq: 1,
        uci: "e2e5".into(),
        ts: 1,
    }];
    assert!(matches!(
        replay_fen(START_FEN, &bad),
        Err(GameLogError::IllegalMove(_))
    ));
    assert!(matches!(
        replay_fen("not-a-fen", &[]),
        Err(GameLogError::InvalidFen(_))
    ));
    let junk = [GameEvent::Move {
        seq: 1,
        uci: "zzzz".into(),
        ts: 1,
    }];
    assert!(matches!(
        replay_fen(START_FEN, &junk),
        Err(GameLogError::InvalidUci(_))
    ));
}

#[test]
fn replay_skips_non_move_events_for_position() {
    let events = [
        GameEvent::ServerNote {
            seq: 1,
            text: "paired".into(),
            ts: 1,
        },
        GameEvent::Move {
            seq: 2,
            uci: "e2e4".into(),
            ts: 2,
        },
    ];
    assert_eq!(replay_fen(START_FEN, &events).expect("skip note"), AFTER_E4);
}

#[tokio::test]
async fn redis_create_append_reconstruct_fen() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_create_append_reconstruct_fen: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let game_id = "01CV161GAMEEVENTLOGTEST0001";
    let _ = delete_game(&mut conn, game_id).await;

    create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1_730_000_000_000,
        },
    )
    .await
    .expect("create_game");

    let dup = create_game(
        &mut conn,
        game_id,
        &GameMeta {
            initial_fen: START_FEN.to_owned(),
            created_ms: 1,
        },
    )
    .await
    .expect_err("duplicate create");
    assert!(matches!(dup, GameLogError::GameExists));

    assert_eq!(
        reconstruct_fen(&mut conn, game_id)
            .await
            .expect("empty reconstruct"),
        START_FEN
    );

    let seq1 = append_move_event(&mut conn, game_id, "e2e4", 10)
        .await
        .expect("e2e4");
    assert_eq!(seq1, 1);
    let seq2 = append_move_event(&mut conn, game_id, "e7e5", 20)
        .await
        .expect("e7e5");
    assert_eq!(seq2, 2);
    let seq3 = append_move_event(&mut conn, game_id, "g1f3", 30)
        .await
        .expect("g1f3");
    assert_eq!(seq3, 3);

    let events = load_events(&mut conn, game_id).await.expect("load");
    assert_eq!(events.len(), 3);
    assert_eq!(
        reconstruct_fen(&mut conn, game_id)
            .await
            .expect("reconstruct"),
        AFTER_E4_E5_NF3
    );

    delete_game(&mut conn, game_id).await.expect("cleanup");
    assert!(matches!(
        reconstruct_fen(&mut conn, game_id).await,
        Err(GameLogError::GameGone)
    ));
}

#[tokio::test]
async fn redis_game_ops_reject_invalid_game_id() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_game_ops_reject_invalid_game_id: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let meta = GameMeta {
        initial_fen: START_FEN.to_owned(),
        created_ms: 1,
    };
    for bad in ["bad id", "a/b"] {
        assert!(matches!(
            create_game(&mut conn, bad, &meta).await,
            Err(GameLogError::InvalidGameId)
        ));
        assert!(matches!(
            append_move_event(&mut conn, bad, "e2e4", 1).await,
            Err(GameLogError::InvalidGameId)
        ));
        assert!(matches!(
            reconstruct_fen(&mut conn, bad).await,
            Err(GameLogError::InvalidGameId)
        ));
    }
}
