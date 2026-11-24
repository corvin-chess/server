//! CV-169: Persist finished games to Postgres; PGN export matches chess-core movetext.

mod common;

use std::env;
use std::time::Duration;

use corvin_chess_core::{encode_movetext, Move, Position};
use corvin_server::game::{
    create_game, export_pgn, persist_finished_game, render_pgn, GameEvent, GameMeta, PersistInput,
    PersistOutcome, START_FEN,
};
use http_body_util::BodyExt;
use redis::aio::ConnectionManager;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower::ServiceExt;
use ulid::Ulid;

fn database_url() -> Option<String> {
    env::var("DATABASE_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

async fn connect_pg(url: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
        .unwrap_or_else(|err| panic!("connect DATABASE_URL failed: {err}"))
}

async fn insert_user(pool: &PgPool, id: &str, username: &str) {
    sqlx::query(
        "INSERT INTO users (id, username, username_lower, locale)
         VALUES ($1, $2, lower($2), 'en')
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(username)
    .execute(pool)
    .await
    .expect("insert user");
}

fn opening_events() -> Vec<GameEvent> {
    vec![
        GameEvent::move_uci(1, "e2e4", 1_000),
        GameEvent::move_uci(2, "e7e5", 2_000),
        GameEvent::move_uci(3, "g1f3", 3_000),
    ]
}

fn expected_movetext() -> String {
    let start = Position::from_fen(START_FEN).expect("start fen");
    let moves = [
        Move::from_uci("e2e4").expect("e2e4"),
        Move::from_uci("e7e5").expect("e7e5"),
        Move::from_uci("g1f3").expect("g1f3"),
    ];
    encode_movetext(&start, &moves).expect("encode")
}

#[test]
fn render_pgn_movetext_matches_chess_core_encode() {
    let meta = GameMeta {
        initial_fen: START_FEN.to_owned(),
        white_id: "w".into(),
        black_id: "b".into(),
        time_class: "blitz".into(),
        rated: true,
        initial_ms: 180_000,
        increment_ms: 2_000,
        created_ms: 1_700_000_000_000,
        ..GameMeta::default()
    };
    let pgn = render_pgn(
        &meta,
        &opening_events(),
        "Alice",
        "Bob",
        "1-0",
        "resign",
        1_700_000_100_000,
    )
    .expect("render");

    let movetext = expected_movetext();
    assert!(
        pgn.contains(&movetext),
        "PGN must contain chess-core movetext {movetext:?}; got:\n{pgn}"
    );
    assert!(pgn.contains("[White \"Alice\"]"));
    assert!(pgn.contains("[Black \"Bob\"]"));
    assert!(pgn.contains("[Result \"1-0\"]"));
    assert!(pgn.contains("[TimeControl \"180+2\"]"));
    assert!(pgn.trim_end().ends_with("1-0"));
    assert!(!pgn.contains('\u{1F300}'));
}

#[test]
fn render_pgn_movetext_honors_takeback_net_moves() {
    let meta = GameMeta {
        initial_fen: START_FEN.to_owned(),
        time_class: "blitz".into(),
        rated: false,
        initial_ms: 180_000,
        increment_ms: 0,
        created_ms: 1_700_000_000_000,
        ..GameMeta::default()
    };
    let events = vec![
        GameEvent::move_uci(1, "e2e4", 1_000),
        GameEvent::move_uci(2, "e7e5", 2_000),
        GameEvent::Takeback {
            seq: 3,
            undone_seq: 2,
            ts: 2_500,
        },
        GameEvent::move_uci(4, "c7c5", 3_000),
    ];
    let pgn = render_pgn(
        &meta,
        &events,
        "White",
        "Black",
        "1/2-1/2",
        "agreement",
        1_700_000_100_000,
    )
    .expect("render");
    let start = Position::from_fen(START_FEN).expect("fen");
    let net = [
        Move::from_uci("e2e4").expect("e2e4"),
        Move::from_uci("c7c5").expect("c7c5"),
    ];
    let expected = encode_movetext(&start, &net).expect("encode");
    assert!(
        pgn.contains(&expected),
        "takeback net movetext {expected:?} missing from:\n{pgn}"
    );
    assert!(!pgn.contains("e5"));
}

#[test]
fn render_pgn_escapes_tag_quotes_and_abort_star() {
    let meta = GameMeta {
        initial_fen: START_FEN.to_owned(),
        time_class: "bullet".into(),
        rated: false,
        initial_ms: 60_000,
        increment_ms: 0,
        created_ms: 1_700_000_000_000,
        ..GameMeta::default()
    };
    let pgn = render_pgn(
        &meta,
        &[],
        "A\"B\\C",
        "Opponent",
        "*",
        "abort",
        1_700_000_000_500,
    )
    .expect("render");
    assert!(pgn.contains("[White \"A\\\"B\\\\C\"]"));
    assert!(pgn.contains("[Result \"*\"]"));
    assert!(pgn.trim_end().ends_with('*'));
}

#[tokio::test]
async fn persist_finished_game_once_and_pgn_export_matches() {
    let Some(url) = database_url() else {
        eprintln!("skip persist_finished_game_once_and_pgn_export_matches: DATABASE_URL unset");
        return;
    };
    let _lock = common::acquire_db_lock(&url).await;
    let pool = connect_pg(&url).await;
    corvin_server::db::migrate_up(&pool)
        .await
        .expect("migrate_up");

    let white = Ulid::new().to_string();
    let black = Ulid::new().to_string();
    insert_user(&pool, &white, "WhiteBird").await;
    insert_user(&pool, &black, "BlackBird").await;

    let game_id = Ulid::new().to_string();
    let meta = GameMeta {
        initial_fen: START_FEN.to_owned(),
        white_id: white.clone(),
        black_id: black.clone(),
        time_class: "blitz".into(),
        rated: true,
        initial_ms: 180_000,
        increment_ms: 2_000,
        created_ms: 1_700_000_000_000,
        ended: true,
        ended_reason: Some("resign".into()),
        ..GameMeta::default()
    };
    let events = opening_events();

    let first = persist_finished_game(
        &pool,
        PersistInput {
            game_id: &game_id,
            meta: &meta,
            events: &events,
            result: "1-0",
            reason: "resign",
            ended_ms: 1_700_000_100_000,
        },
    )
    .await
    .expect("first persist");
    assert!(matches!(first, PersistOutcome::Persisted));

    let second = persist_finished_game(
        &pool,
        PersistInput {
            game_id: &game_id,
            meta: &meta,
            events: &events,
            result: "1-0",
            reason: "resign",
            ended_ms: 1_700_000_100_000,
        },
    )
    .await
    .expect("second persist");
    assert!(matches!(second, PersistOutcome::AlreadyPersisted));

    let exported = export_pgn(&pool, &game_id)
        .await
        .expect("export")
        .expect("row");
    let expected = expected_movetext();
    assert!(
        exported.contains(&expected),
        "export movetext mismatch; expected {expected:?} in:\n{exported}"
    );
    assert!(exported.contains("[White \"WhiteBird\"]"));
    assert!(exported.contains("[Black \"BlackBird\"]"));
    assert!(exported.contains("[Result \"1-0\"]"));
    assert_eq!(exported.matches("1. e4").count(), 1);

    let event_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM game_events WHERE game_id = $1")
        .bind(&game_id)
        .fetch_one(&pool)
        .await
        .expect("count events");
    assert_eq!(event_count.0, 3);

    // HTTP PGN must match library export (PROTOCOL GET /api/v1/games/{id}/pgn).
    let mailer = std::sync::Arc::new(corvin_server::auth::mailer::CaptureMailer::default());
    let state = corvin_server::AppState::new_for_tests(
        pool.clone(),
        mailer,
        "localhost",
        "http://localhost:3000",
        b"dev-only-otp-pepper-change-me!!",
    )
    .expect("app state");
    let response = corvin_server::app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/v1/games/{game_id}/pgn"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .expect("pgn response");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let ctype = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ctype.starts_with("application/x-chess-pgn") || ctype.starts_with("text/plain"),
        "unexpected content-type {ctype}"
    );
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let http_pgn = String::from_utf8(body.to_vec()).expect("utf8");
    assert_eq!(http_pgn, exported);

    let missing = corvin_server::app(state)
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/v1/games/01MISSINGGAMEID00000000000/pgn")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .expect("missing");
    assert_eq!(missing.status(), axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn persist_from_redis_log_then_export() {
    let Some(db_url) = database_url() else {
        eprintln!("skip persist_from_redis_log_then_export: DATABASE_URL unset");
        return;
    };
    let Some(r_url) = redis_url() else {
        eprintln!("skip persist_from_redis_log_then_export: REDIS_URL unset");
        return;
    };
    let _lock = common::acquire_db_lock(&db_url).await;
    let pool = connect_pg(&db_url).await;
    corvin_server::db::migrate_up(&pool)
        .await
        .expect("migrate_up");

    let client = redis::Client::open(r_url.as_str()).expect("redis client");
    let mut conn = ConnectionManager::new(client).await.expect("redis connect");

    let white = Ulid::new().to_string();
    let black = Ulid::new().to_string();
    insert_user(&pool, &white, "RedisWhite").await;
    insert_user(&pool, &black, "RedisBlack").await;

    let game_id = Ulid::new().to_string();
    let meta = GameMeta {
        initial_fen: START_FEN.to_owned(),
        white_id: white,
        black_id: black,
        time_class: "rapid".into(),
        rated: false,
        initial_ms: 600_000,
        increment_ms: 0,
        created_ms: 1_700_000_000_000,
        ended: true,
        ended_reason: Some("agreement".into()),
        ..GameMeta::default()
    };
    create_game(&mut conn, &game_id, &meta)
        .await
        .expect("create");
    for event in opening_events() {
        corvin_server::game::append_game_event(&mut conn, &game_id, event)
            .await
            .expect("append");
    }

    let outcome = corvin_server::game::persist_finished_from_redis(
        &pool,
        &mut conn,
        &game_id,
        "1/2-1/2",
        "agreement",
        1_700_000_200_000,
    )
    .await
    .expect("persist redis");
    assert!(matches!(outcome, PersistOutcome::Persisted));

    let exported = export_pgn(&pool, &game_id)
        .await
        .expect("export")
        .expect("row");
    assert!(exported.contains(&expected_movetext()));
    assert!(exported.contains("[Result \"1/2-1/2\"]"));
    assert!(exported.contains("[White \"RedisWhite\"]"));
}
