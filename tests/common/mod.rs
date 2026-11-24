//! Shared helpers for integration tests that mutate the shared CI Postgres.

use sqlx::postgres::PgConnection;
use sqlx::Connection;

/// Cross-process lock so migrate/auth e2e tests do not clobber each other.
/// Held on a dedicated connection; dropping the connection releases the lock.
const ADVISORY_KEY: i64 = 133_001;

pub async fn acquire_db_lock(database_url: &str) -> PgConnection {
    let mut conn = PgConnection::connect(database_url)
        .await
        .expect("advisory lock connection");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(ADVISORY_KEY)
        .execute(&mut conn)
        .await
        .expect("pg_advisory_lock");
    conn
}
