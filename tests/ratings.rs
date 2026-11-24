//! CV-168: Rated Glicko-2 finish updates rating rows exactly once.

mod common;

use std::env;
use std::time::Duration;

use corvin_server::game::GameMeta;
use corvin_server::rating::{
    apply_rated_finish, game_end_with_rating_diff, get_rating, rating_diff_json, FinishOutcome,
    RatedFinish, INITIAL_R,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use ulid::Ulid;

fn database_url() -> Option<String> {
    env::var("DATABASE_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

async fn connect(url: &str) -> PgPool {
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

fn rated_meta(white: &str, black: &str) -> GameMeta {
    GameMeta {
        white_id: white.to_owned(),
        black_id: black.to_owned(),
        time_class: "blitz".to_owned(),
        rated: true,
        ..GameMeta::default()
    }
}

#[test]
fn rating_diff_json_shape() {
    use corvin_server::rating::{Rating, RatingDiffs};
    let diffs = RatingDiffs {
        white: 12,
        black: -11,
        white_before: Rating::unrated(),
        white_after: Rating::unrated(),
        black_before: Rating::unrated(),
        black_after: Rating::unrated(),
    };
    let body = rating_diff_json(&diffs);
    assert_eq!(body["w"], 12);
    assert_eq!(body["b"], -11);

    let end = game_end_with_rating_diff(
        json!({
            "typ": "GameEnd",
            "v": 1,
            "result": "1-0",
            "reason": "resign"
        }),
        Some(&diffs),
    );
    assert_eq!(end["rating_diff"]["w"], 12);
    assert_eq!(end["rating_diff"]["b"], -11);
}

#[tokio::test]
async fn rated_finish_changes_rows_once_and_skips_abort_casual() {
    let Some(url) = database_url() else {
        eprintln!("skip rated_finish_changes_rows_once_and_skips_abort_casual: DATABASE_URL unset");
        return;
    };
    let _lock = common::acquire_db_lock(&url).await;
    let pool = connect(&url).await;
    corvin_server::db::migrate_up(&pool)
        .await
        .expect("migrate_up");

    let white = Ulid::new().to_string();
    let black = Ulid::new().to_string();
    insert_user(&pool, &white, &format!("W{}", &white[0..8])).await;
    insert_user(&pool, &black, &format!("B{}", &black[0..8])).await;

    let game_id = Ulid::new().to_string();
    let meta = rated_meta(&white, &black);

    let first = apply_rated_finish(
        &pool,
        RatedFinish {
            game_id: &game_id,
            meta: &meta,
            result: "1-0",
            reason: "resign",
        },
    )
    .await
    .expect("first finish");
    let FinishOutcome::Applied(diffs) = first else {
        panic!("expected Applied, got {first:?}");
    };
    assert!(diffs.white > 0, "white win should raise display rating");
    assert!(diffs.black < 0, "black loss should lower display rating");

    let white_row = get_rating(&pool, &white, "blitz")
        .await
        .expect("get white")
        .expect("white rating row");
    let black_row = get_rating(&pool, &black, "blitz")
        .await
        .expect("get black")
        .expect("black rating row");
    assert!(white_row.r > INITIAL_R);
    assert!(black_row.r < INITIAL_R);

    let games_w: (i32,) =
        sqlx::query_as("SELECT games FROM ratings WHERE user_id = $1 AND time_class = 'blitz'")
            .bind(&white)
            .fetch_one(&pool)
            .await
            .expect("games_w");
    assert_eq!(games_w.0, 1);

    let second = apply_rated_finish(
        &pool,
        RatedFinish {
            game_id: &game_id,
            meta: &meta,
            result: "1-0",
            reason: "resign",
        },
    )
    .await
    .expect("second finish");
    match second {
        FinishOutcome::AlreadyApplied(again) => {
            assert_eq!(again.white, diffs.white);
            assert_eq!(again.black, diffs.black);
        }
        other => panic!("expected AlreadyApplied, got {other:?}"),
    }

    let games_w2: (i32,) =
        sqlx::query_as("SELECT games FROM ratings WHERE user_id = $1 AND time_class = 'blitz'")
            .bind(&white)
            .fetch_one(&pool)
            .await
            .expect("games_w2");
    assert_eq!(
        games_w2.0, 1,
        "idempotent finish must not increment games twice"
    );

    let white_after_dup = get_rating(&pool, &white, "blitz")
        .await
        .expect("get white dup")
        .expect("row");
    assert!((white_after_dup.r - white_row.r).abs() < 1e-9);

    let abort_game = Ulid::new().to_string();
    let abort = apply_rated_finish(
        &pool,
        RatedFinish {
            game_id: &abort_game,
            meta: &meta,
            result: "1-0",
            reason: "abort",
        },
    )
    .await
    .expect("abort");
    assert_eq!(abort, FinishOutcome::Skipped);

    let mut casual = meta.clone();
    casual.rated = false;
    let casual_game = Ulid::new().to_string();
    let casual_out = apply_rated_finish(
        &pool,
        RatedFinish {
            game_id: &casual_game,
            meta: &casual,
            result: "1-0",
            reason: "resign",
        },
    )
    .await
    .expect("casual");
    assert_eq!(casual_out, FinishOutcome::Skipped);

    let audit_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM rating_updates WHERE game_id = $1")
            .bind(&game_id)
            .fetch_one(&pool)
            .await
            .expect("audit count");
    assert_eq!(audit_count.0, 1);
}
