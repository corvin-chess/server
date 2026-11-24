//! Postgres rating rows (CV-168).

use sqlx::{PgPool, Postgres, Transaction};

use super::glicko2::Rating;

#[derive(Debug)]
pub enum RatingError {
    Db(sqlx::Error),
    InvalidTimeClass,
    InvalidUserId,
    MissingSeats,
    InvalidResult,
    SelfMatch,
    RatingRowMissing,
}

impl std::fmt::Display for RatingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(err) => write!(f, "rating db: {err}"),
            Self::InvalidTimeClass => write!(f, "invalid time_class"),
            Self::InvalidUserId => write!(f, "invalid user id"),
            Self::MissingSeats => write!(f, "both seats required for rated finish"),
            Self::InvalidResult => write!(f, "unknown game result"),
            Self::SelfMatch => write!(f, "cannot rate a self-match"),
            Self::RatingRowMissing => write!(f, "rating row missing after lock"),
        }
    }
}

impl std::error::Error for RatingError {}

impl From<sqlx::Error> for RatingError {
    fn from(value: sqlx::Error) -> Self {
        Self::Db(value)
    }
}

const TIME_CLASSES: &[&str] = &["bullet", "blitz", "rapid", "classical", "correspondence"];

pub fn normalize_time_class(raw: &str) -> Result<String, RatingError> {
    let tc = raw.trim().to_ascii_lowercase();
    if TIME_CLASSES.contains(&tc.as_str()) {
        Ok(tc)
    } else {
        Err(RatingError::InvalidTimeClass)
    }
}

pub(crate) fn is_ulid(id: &str) -> bool {
    id.len() == 26
        && id.bytes().all(|b| {
            matches!(
                b,
                b'0'..=b'9' | b'A'..=b'H' | b'J'..=b'K' | b'M'..=b'N' | b'P'..=b'T' | b'V'..=b'Z'
            )
        })
}

/// Load rating row or return unrated defaults without inserting.
pub async fn get_rating(
    pool: &PgPool,
    user_id: &str,
    time_class: &str,
) -> Result<Option<Rating>, RatingError> {
    if !is_ulid(user_id) {
        return Err(RatingError::InvalidUserId);
    }
    let tc = normalize_time_class(time_class)?;
    let row: Option<(f64, f64, f64)> = sqlx::query_as(
        "SELECT rating, rd, volatility
         FROM ratings
         WHERE user_id = $1 AND time_class = $2",
    )
    .bind(user_id)
    .bind(&tc)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(r, rd, sigma)| Rating { r, rd, sigma }))
}

/// Ensure a ratings row exists (insert unrated defaults if missing).
pub async fn ensure_rating(
    pool: &PgPool,
    user_id: &str,
    time_class: &str,
) -> Result<Rating, RatingError> {
    if !is_ulid(user_id) {
        return Err(RatingError::InvalidUserId);
    }
    let tc = normalize_time_class(time_class)?;
    let unrated = Rating::unrated();
    sqlx::query(
        "INSERT INTO ratings (user_id, time_class, rating, rd, volatility, games)
         VALUES ($1, $2, $3, $4, $5, 0)
         ON CONFLICT (user_id, time_class) DO NOTHING",
    )
    .bind(user_id)
    .bind(&tc)
    .bind(unrated.r)
    .bind(unrated.rd)
    .bind(unrated.sigma)
    .execute(pool)
    .await?;

    let row: (f64, f64, f64) = sqlx::query_as(
        "SELECT rating, rd, volatility
         FROM ratings
         WHERE user_id = $1 AND time_class = $2",
    )
    .bind(user_id)
    .bind(&tc)
    .fetch_one(pool)
    .await?;
    Ok(Rating {
        r: row.0,
        rd: row.1,
        sigma: row.2,
    })
}

pub(crate) async fn ensure_rating_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    time_class: &str,
) -> Result<(), RatingError> {
    if !is_ulid(user_id) {
        return Err(RatingError::InvalidUserId);
    }
    let unrated = Rating::unrated();
    sqlx::query(
        "INSERT INTO ratings (user_id, time_class, rating, rd, volatility, games)
         VALUES ($1, $2, $3, $4, $5, 0)
         ON CONFLICT (user_id, time_class) DO NOTHING",
    )
    .bind(user_id)
    .bind(time_class)
    .bind(unrated.r)
    .bind(unrated.rd)
    .bind(unrated.sigma)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Lock one rating row for update (caller orders locks to avoid deadlock).
pub(crate) async fn lock_rating(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    time_class: &str,
) -> Result<Rating, RatingError> {
    let row: Option<(f64, f64, f64)> = sqlx::query_as(
        "SELECT rating, rd, volatility
         FROM ratings
         WHERE user_id = $1 AND time_class = $2
         FOR UPDATE",
    )
    .bind(user_id)
    .bind(time_class)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some((r, rd, sigma)) => Ok(Rating { r, rd, sigma }),
        None => Err(RatingError::RatingRowMissing),
    }
}

pub(crate) async fn write_rating(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    time_class: &str,
    rating: &Rating,
) -> Result<(), RatingError> {
    let result = sqlx::query(
        "UPDATE ratings
         SET rating = $3, rd = $4, volatility = $5, games = games + 1, updated_at = now()
         WHERE user_id = $1 AND time_class = $2",
    )
    .bind(user_id)
    .bind(time_class)
    .bind(rating.r)
    .bind(rating.rd)
    .bind(rating.sigma)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() != 1 {
        return Err(RatingError::RatingRowMissing);
    }
    Ok(())
}
