//! Apply Glicko-2 once when a rated game finishes (CV-168).

use serde_json::{json, Value};
use sqlx::PgPool;

use crate::game::GameMeta;

use super::glicko2::{glicko2_update, Rating, TAU};
use super::store::{
    ensure_rating_tx, is_ulid, lock_rating, normalize_time_class, write_rating, RatingError,
};

/// Display deltas for PROTOCOL `GameEnd.rating_diff`.
#[derive(Clone, Debug, PartialEq)]
pub struct RatingDiffs {
    pub white: i32,
    pub black: i32,
    pub white_before: Rating,
    pub white_after: Rating,
    pub black_before: Rating,
    pub black_after: Rating,
}

/// Outcome of a finish attempt.
#[derive(Clone, Debug, PartialEq)]
pub enum FinishOutcome {
    /// Casual or abort — no rating change.
    Skipped,
    /// First successful apply for this `game_id`.
    Applied(RatingDiffs),
    /// Same `game_id` already rated; diffs are the stored audit row.
    AlreadyApplied(RatingDiffs),
}

/// Inputs for a rated finish (result is PROTOCOL `1-0` / `0-1` / `1/2-1/2`).
///
/// Seat ids must be authenticated `users.id` ULIDs (not guest tokens).
#[derive(Clone, Debug)]
pub struct RatedFinish<'a> {
    pub game_id: &'a str,
    pub meta: &'a GameMeta,
    pub result: &'a str,
    pub reason: &'a str,
}

/// PROTOCOL-shaped `rating_diff` object.
pub fn rating_diff_json(diffs: &RatingDiffs) -> Value {
    json!({
        "w": diffs.white,
        "b": diffs.black,
    })
}

/// Attach `rating_diff` to an existing PROTOCOL `GameEnd` body when present.
pub fn game_end_with_rating_diff(mut end: Value, diffs: Option<&RatingDiffs>) -> Value {
    if let Some(d) = diffs {
        end["rating_diff"] = rating_diff_json(d);
    }
    end
}

fn scores_for_result(result: &str) -> Result<(f64, f64), RatingError> {
    match result.trim() {
        "1-0" => Ok((1.0, 0.0)),
        "0-1" => Ok((0.0, 1.0)),
        "1/2-1/2" => Ok((0.5, 0.5)),
        _ => Err(RatingError::InvalidResult),
    }
}

fn display_delta(before: f64, after: f64) -> i32 {
    (after - before).round() as i32
}

fn load_audit_diffs(
    white_before: Rating,
    white_after: Rating,
    black_before: Rating,
    black_after: Rating,
) -> RatingDiffs {
    RatingDiffs {
        white: display_delta(white_before.r, white_after.r),
        black: display_delta(black_before.r, black_after.r),
        white_before,
        white_after,
        black_before,
        black_after,
    }
}

#[derive(sqlx::FromRow)]
struct AuditRow {
    white_rating_before: f64,
    white_rd_before: f64,
    white_vol_before: f64,
    white_rating_after: f64,
    white_rd_after: f64,
    white_vol_after: f64,
    black_rating_before: f64,
    black_rd_before: f64,
    black_vol_before: f64,
    black_rating_after: f64,
    black_rd_after: f64,
    black_vol_after: f64,
    white_diff: i32,
    black_diff: i32,
}

fn diffs_from_audit(row: AuditRow) -> RatingDiffs {
    RatingDiffs {
        white: row.white_diff,
        black: row.black_diff,
        white_before: Rating {
            r: row.white_rating_before,
            rd: row.white_rd_before,
            sigma: row.white_vol_before,
        },
        white_after: Rating {
            r: row.white_rating_after,
            rd: row.white_rd_after,
            sigma: row.white_vol_after,
        },
        black_before: Rating {
            r: row.black_rating_before,
            rd: row.black_rd_before,
            sigma: row.black_vol_before,
        },
        black_after: Rating {
            r: row.black_rating_after,
            rd: row.black_rd_after,
            sigma: row.black_vol_after,
        },
    }
}

/// Update both seats' Glicko-2 rows for `meta.time_class` exactly once per `game_id`.
///
/// Skips when `meta.rated` is false or `reason` is `abort` (PROJECT_SPEC).
/// Locks both rating rows (`FOR UPDATE`, ordered by user id) before computing so
/// concurrent finishes for the same player cannot lose an update.
pub async fn apply_rated_finish(
    pool: &PgPool,
    finish: RatedFinish<'_>,
) -> Result<FinishOutcome, RatingError> {
    if !finish.meta.rated {
        return Ok(FinishOutcome::Skipped);
    }
    if finish.reason.trim().eq_ignore_ascii_case("abort") {
        return Ok(FinishOutcome::Skipped);
    }

    let white_id = finish.meta.white_id.trim();
    let black_id = finish.meta.black_id.trim();
    if white_id.is_empty() || black_id.is_empty() {
        return Err(RatingError::MissingSeats);
    }
    if white_id == black_id {
        return Err(RatingError::SelfMatch);
    }
    if !is_ulid(white_id) || !is_ulid(black_id) {
        return Err(RatingError::InvalidUserId);
    }

    let time_class = normalize_time_class(&finish.meta.time_class)?;
    let (score_w, score_b) = scores_for_result(finish.result)?;

    let mut tx = pool.begin().await?;

    // Fast path: already rated this game_id.
    let existing = sqlx::query_as::<_, AuditRow>(
        "SELECT
            white_rating_before, white_rd_before, white_vol_before,
            white_rating_after, white_rd_after, white_vol_after,
            black_rating_before, black_rd_before, black_vol_before,
            black_rating_after, black_rd_after, black_vol_after,
            white_diff, black_diff
         FROM rating_updates
         WHERE game_id = $1
         FOR SHARE",
    )
    .bind(finish.game_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = existing {
        tx.commit().await?;
        return Ok(FinishOutcome::AlreadyApplied(diffs_from_audit(row)));
    }

    ensure_rating_tx(&mut tx, white_id, &time_class).await?;
    ensure_rating_tx(&mut tx, black_id, &time_class).await?;

    // Deterministic lock order avoids deadlocks when two games share a player.
    let (first, second) = if white_id < black_id {
        (white_id, black_id)
    } else {
        (black_id, white_id)
    };
    let first_locked = lock_rating(&mut tx, first, &time_class).await?;
    let second_locked = lock_rating(&mut tx, second, &time_class).await?;
    let (white_before, black_before) = if white_id < black_id {
        (first_locked, second_locked)
    } else {
        (second_locked, first_locked)
    };

    let white_after = glicko2_update(&white_before, &[(black_before, score_w)], TAU);
    let black_after = glicko2_update(&black_before, &[(white_before, score_b)], TAU);
    let diffs = load_audit_diffs(white_before, white_after, black_before, black_after);

    // Claim once: unique game_id is the DoD gate (race with another finisher → AlreadyApplied).
    let claimed = sqlx::query(
        "INSERT INTO rating_updates (
            game_id, time_class, white_id, black_id, result, reason,
            white_rating_before, white_rd_before, white_vol_before,
            white_rating_after, white_rd_after, white_vol_after,
            black_rating_before, black_rd_before, black_vol_before,
            black_rating_after, black_rd_after, black_vol_after,
            white_diff, black_diff
         ) VALUES (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9, $10, $11, $12,
            $13, $14, $15, $16, $17, $18,
            $19, $20
         )
         ON CONFLICT (game_id) DO NOTHING",
    )
    .bind(finish.game_id)
    .bind(&time_class)
    .bind(white_id)
    .bind(black_id)
    .bind(finish.result)
    .bind(finish.reason)
    .bind(white_before.r)
    .bind(white_before.rd)
    .bind(white_before.sigma)
    .bind(white_after.r)
    .bind(white_after.rd)
    .bind(white_after.sigma)
    .bind(black_before.r)
    .bind(black_before.rd)
    .bind(black_before.sigma)
    .bind(black_after.r)
    .bind(black_after.rd)
    .bind(black_after.sigma)
    .bind(diffs.white)
    .bind(diffs.black)
    .execute(&mut *tx)
    .await?;

    if claimed.rows_affected() == 0 {
        let row = sqlx::query_as::<_, AuditRow>(
            "SELECT
                white_rating_before, white_rd_before, white_vol_before,
                white_rating_after, white_rd_after, white_vol_after,
                black_rating_before, black_rd_before, black_vol_before,
                black_rating_after, black_rd_after, black_vol_after,
                white_diff, black_diff
             FROM rating_updates
             WHERE game_id = $1",
        )
        .bind(finish.game_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(FinishOutcome::AlreadyApplied(diffs_from_audit(row)));
    }

    write_rating(&mut tx, white_id, &time_class, &white_after).await?;
    write_rating(&mut tx, black_id, &time_class, &black_after).await?;
    tx.commit().await?;

    Ok(FinishOutcome::Applied(diffs))
}
