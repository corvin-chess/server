//! Postgres pool helpers and reversible migrations (CV-131).

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Error as SqlxError;
use std::time::Duration;

/// Embedded reversible migrations from `migrations/`.
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Connect to Postgres using `DATABASE_URL` (required).
pub async fn connect_from_env() -> Result<PgPool, SqlxError> {
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| SqlxError::Configuration("DATABASE_URL is required for Postgres".into()))?;
    connect(&url).await
}

/// Connect with an explicit URL (tests and CLI).
pub async fn connect(database_url: &str) -> Result<PgPool, SqlxError> {
    PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(10))
        .connect(database_url)
        .await
}

/// Apply all pending migrations (up).
pub async fn migrate_up(pool: &PgPool) -> Result<(), MigrateError> {
    MIGRATOR.run(pool).await
}

/// Revert all applied reversible migrations (down to version 0).
pub async fn migrate_down(pool: &PgPool) -> Result<(), MigrateError> {
    MIGRATOR.undo(pool, 0).await
}
