//! CV-131: Postgres migrations for users/sessions must migrate up and down.

mod common;

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

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

async fn table_exists(pool: &PgPool, name: &str) -> bool {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1
            FROM information_schema.tables
            WHERE table_schema = 'public' AND table_name = $1
        )",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("table_exists query");
    row.0
}

async fn column_names(pool: &PgPool, table: &str) -> Vec<String> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT column_name
         FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = $1
         ORDER BY ordinal_position",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .expect("column_names query");
    rows.into_iter().map(|(name,)| name).collect()
}

#[test]
fn migration_sql_files_are_present_and_paired() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
    assert!(root.is_dir(), "migrations/ directory missing");

    let up = root.join("20261123120000_users_sessions.up.sql");
    let down = root.join("20261123120000_users_sessions.down.sql");
    assert!(up.is_file(), "missing {}", up.display());
    assert!(down.is_file(), "missing {}", down.display());

    let up_sql = std::fs::read_to_string(&up).expect("read up.sql");
    let down_sql = std::fs::read_to_string(&down).expect("read down.sql");

    assert!(
        up_sql.to_lowercase().contains("create table")
            && up_sql.to_lowercase().contains("users")
            && up_sql.to_lowercase().contains("sessions"),
        "up migration must create users and sessions"
    );
    assert!(
        down_sql.to_lowercase().contains("drop table")
            && down_sql.to_lowercase().contains("users")
            && down_sql.to_lowercase().contains("sessions"),
        "down migration must drop users and sessions"
    );
    assert!(
        !up_sql.to_lowercase().contains("password")
            && !up_sql.to_lowercase().contains("threshold")
            && !up_sql.to_lowercase().contains("secret_key"),
        "migration must not embed passwords or fair-play secrets"
    );
}

#[tokio::test]
async fn migrate_up_creates_users_and_sessions_then_down_removes_them() {
    let Some(url) = database_url() else {
        eprintln!(
            "skip migrate_up_creates_users_and_sessions_then_down_removes_them: DATABASE_URL unset"
        );
        return;
    };
    let _lock = common::acquire_db_lock(&url).await;
    let pool = connect(&url).await;

    // Start from a clean public schema for this integration DB.
    corvin_server::db::migrate_down(&pool)
        .await
        .expect("migrate_down before up");
    assert!(!table_exists(&pool, "users").await);
    assert!(!table_exists(&pool, "sessions").await);
    assert!(!table_exists(&pool, "passkeys").await);
    assert!(!table_exists(&pool, "email_otps").await);

    corvin_server::db::migrate_up(&pool)
        .await
        .expect("migrate_up");

    assert!(table_exists(&pool, "users").await);
    assert!(table_exists(&pool, "sessions").await);
    assert!(table_exists(&pool, "passkeys").await);
    assert!(table_exists(&pool, "email_otps").await);
    assert!(table_exists(&pool, "challenges").await);
    assert!(table_exists(&pool, "ratings").await);
    assert!(table_exists(&pool, "rating_updates").await);

    let rating_cols = column_names(&pool, "ratings").await;
    for required in [
        "user_id",
        "time_class",
        "rating",
        "rd",
        "volatility",
        "games",
        "updated_at",
    ] {
        assert!(
            rating_cols.iter().any(|col| col == required),
            "ratings missing column {required}; have {rating_cols:?}"
        );
    }

    let challenge_cols = column_names(&pool, "challenges").await;
    for required in [
        "id",
        "from_player_id",
        "opponent_player_id",
        "link_token",
        "time_class",
        "initial_ms",
        "increment_ms",
        "rated",
        "color_pref",
        "expires_at",
        "status",
        "game_id",
        "created_at",
    ] {
        assert!(
            challenge_cols.iter().any(|col| col == required),
            "challenges missing column {required}; have {challenge_cols:?}"
        );
    }

    let user_cols = column_names(&pool, "users").await;
    for required in [
        "id",
        "username",
        "username_lower",
        "email",
        "email_lower",
        "birth_year",
        "locale",
        "dm_enabled",
        "public_chat_enabled",
        "created_at",
        "updated_at",
        "deleted_at",
    ] {
        assert!(
            user_cols.iter().any(|col| col == required),
            "users missing column {required}; have {user_cols:?}"
        );
    }

    let session_cols = column_names(&pool, "sessions").await;
    for required in [
        "id",
        "user_id",
        "created_at",
        "expires_at",
        "csrf_secret",
        "user_agent",
        "ip",
        "revoked_at",
    ] {
        assert!(
            session_cols.iter().any(|col| col == required),
            "sessions missing column {required}; have {session_cols:?}"
        );
    }

    // Insert round-trip proves FK + uniqueness wiring without auth API.
    sqlx::query(
        "INSERT INTO users (id, username, username_lower, email, email_lower, birth_year, locale)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind("01ARZ3NDEKTSV4RRFFQ69G5FAV")
    .bind("Raven")
    .bind("raven")
    .bind("raven@example.com")
    .bind("raven@example.com")
    .bind(1990_i16)
    .bind("en")
    .execute(&pool)
    .await
    .expect("insert user");

    sqlx::query(
        "INSERT INTO sessions (id, user_id, expires_at, csrf_secret)
         VALUES ($1, $2, now() + interval '1 day', $3)",
    )
    .bind(&[0u8; 32][..])
    .bind("01ARZ3NDEKTSV4RRFFQ69G5FAV")
    .bind(&[1u8; 32][..])
    .execute(&pool)
    .await
    .expect("insert session");

    let duplicate = sqlx::query(
        "INSERT INTO users (id, username, username_lower)
         VALUES ($1, $2, $3)",
    )
    .bind("01ARZ3NDEKTSV4RRFFQ69G5FAW")
    .bind("raven")
    .bind("raven")
    .execute(&pool)
    .await;
    assert!(
        duplicate.is_err(),
        "username_lower must be unique (case-insensitive identity)"
    );

    corvin_server::db::migrate_down(&pool)
        .await
        .expect("migrate_down after up");
    assert!(!table_exists(&pool, "users").await);
    assert!(!table_exists(&pool, "sessions").await);

    // Re-up after down must succeed (DoD migrate up/down cycle).
    corvin_server::db::migrate_up(&pool)
        .await
        .expect("migrate_up after down");
    assert!(table_exists(&pool, "users").await);
    assert!(table_exists(&pool, "sessions").await);

    // Second up is a no-op (kept in this test to avoid parallel races on one DATABASE_URL).
    corvin_server::db::migrate_up(&pool)
        .await
        .expect("idempotent migrate_up");

    corvin_server::db::migrate_down(&pool)
        .await
        .expect("final migrate_down cleanup");
}
