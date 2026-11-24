//! Durable challenge links in Postgres (CV-167 / ADR-006).

use redis::aio::ConnectionManager;
use serde_json::{json, Value};
use sqlx::PgPool;
use ulid::Ulid;

use super::color::{assign_colors, ColorPref};
use super::config::{is_safe_player_id, validate_pair_config, MatchError, PairConfig, PROTOCOL_V};
use super::pool::{match_bundle_for, store_pair_notify};
use super::start::{create_matched_game, MatchBundle, MAX_PLAYER_ID_LEN};

/// Default open-link TTL (15 minutes).
pub const DEFAULT_CHALLENGE_TTL_MS: u64 = 15 * 60 * 1000;

/// Errors specific to challenge create/accept/decline.
#[derive(Debug)]
pub enum ChallengeError {
    Match(MatchError),
    NotFound,
    Expired,
    NotOpen,
    Forbidden,
    BadToken,
    Db(sqlx::Error),
}

impl std::fmt::Display for ChallengeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Match(err) => write!(f, "{err}"),
            Self::NotFound => write!(f, "challenge not found"),
            Self::Expired => write!(f, "challenge expired"),
            Self::NotOpen => write!(f, "challenge not open"),
            Self::Forbidden => write!(f, "not allowed for this challenge"),
            Self::BadToken => write!(f, "invalid challenge token"),
            Self::Db(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ChallengeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Match(err) => Some(err),
            Self::Db(err) => Some(err),
            _ => None,
        }
    }
}

impl From<MatchError> for ChallengeError {
    fn from(value: MatchError) -> Self {
        Self::Match(value)
    }
}

impl From<sqlx::Error> for ChallengeError {
    fn from(value: sqlx::Error) -> Self {
        Self::Db(value)
    }
}

/// Result of creating a shareable challenge link.
#[derive(Clone, Debug)]
pub struct ChallengeCreated {
    pub challenge_id: String,
    pub link_token: String,
    pub expires_ts: u64,
    pub challenge: Value,
}

/// Loaded challenge row (subset used by accept).
#[derive(Clone, Debug)]
pub struct ChallengeRow {
    pub id: String,
    pub from_player_id: String,
    pub opponent_player_id: Option<String>,
    pub link_token: String,
    pub time_class: String,
    pub initial_ms: i64,
    pub increment_ms: i64,
    pub rated: bool,
    pub color_pref: String,
    pub expires_at_ms: u64,
    pub status: String,
    pub game_id: Option<String>,
}

/// PROTOCOL `Challenge` server message.
pub fn challenge_json(
    challenge_id: &str,
    from_player_id: &str,
    config: &PairConfig,
    expires_ts: u64,
) -> Value {
    json!({
        "typ": "Challenge",
        "v": PROTOCOL_V,
        "challenge_id": challenge_id,
        "from": { "id": from_player_id },
        "config": {
            "time_class": config.time_class,
            "initial_ms": config.initial_ms,
            "increment_ms": config.increment_ms,
            "rated": config.rated,
            "color": config.color.as_str(),
            "link": true,
        },
        "expires_ts": expires_ts,
    })
}

fn require_player(player_id: &str) -> Result<(), ChallengeError> {
    if !is_safe_player_id(player_id, MAX_PLAYER_ID_LEN) {
        return Err(ChallengeError::Match(MatchError::InvalidPlayerId));
    }
    Ok(())
}

fn new_link_token() -> String {
    // 26-char ULID is within 16..=64; opaque enough for shareable URLs.
    Ulid::new().to_string()
}

/// Create an open challenge link (`ChallengeCreate` with `link:true`).
pub async fn challenge_create_link(
    db: &PgPool,
    from_player_id: &str,
    config: &PairConfig,
    now_ms: u64,
    ttl_ms: u64,
) -> Result<ChallengeCreated, ChallengeError> {
    require_player(from_player_id)?;
    validate_pair_config(config)?;
    let ttl = if ttl_ms == 0 {
        DEFAULT_CHALLENGE_TTL_MS
    } else {
        ttl_ms.min(24 * 60 * 60 * 1000)
    };
    let challenge_id = Ulid::new().to_string();
    let link_token = new_link_token();
    let expires_ts = now_ms.saturating_add(ttl);
    let created = chrono::DateTime::from_timestamp_millis(now_ms as i64)
        .ok_or_else(|| ChallengeError::Match(MatchError::Internal("bad now_ms".into())))?;
    let expires = chrono::DateTime::from_timestamp_millis(expires_ts as i64)
        .ok_or_else(|| ChallengeError::Match(MatchError::Internal("bad expires_ts".into())))?;

    sqlx::query(
        "INSERT INTO challenges (
            id, from_player_id, opponent_player_id, link_token,
            time_class, initial_ms, increment_ms, rated, color_pref,
            expires_at, status, created_at
         ) VALUES (
            $1, $2, NULL, $3,
            $4, $5, $6, $7, $8,
            $9, 'open', $10
         )",
    )
    .bind(&challenge_id)
    .bind(from_player_id)
    .bind(&link_token)
    .bind(config.time_class.trim().to_ascii_lowercase())
    .bind(config.initial_ms as i64)
    .bind(config.increment_ms as i64)
    .bind(config.rated)
    .bind(config.color.as_str())
    .bind(expires)
    .bind(created)
    .execute(db)
    .await?;

    let challenge = challenge_json(&challenge_id, from_player_id, config, expires_ts);
    Ok(ChallengeCreated {
        challenge_id,
        link_token,
        expires_ts,
        challenge,
    })
}

async fn load_challenge(db: &PgPool, challenge_id: &str) -> Result<ChallengeRow, ChallengeError> {
    if challenge_id.len() != 26 {
        return Err(ChallengeError::NotFound);
    }
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<String>,
            String,
            String,
            i64,
            i64,
            bool,
            String,
            chrono::DateTime<chrono::Utc>,
            String,
            Option<String>,
        ),
    >(
        "SELECT id, from_player_id, opponent_player_id, link_token,
                time_class, initial_ms, increment_ms, rated, color_pref,
                expires_at, status, game_id
         FROM challenges WHERE id = $1",
    )
    .bind(challenge_id)
    .fetch_optional(db)
    .await?
    .ok_or(ChallengeError::NotFound)?;

    Ok(ChallengeRow {
        id: row.0,
        from_player_id: row.1,
        opponent_player_id: row.2,
        link_token: row.3,
        time_class: row.4,
        initial_ms: row.5,
        increment_ms: row.6,
        rated: row.7,
        color_pref: row.8,
        expires_at_ms: row.9.timestamp_millis() as u64,
        status: row.10,
        game_id: row.11,
    })
}

/// Accept a challenge link; creates the live game and notifies the creator.
pub async fn challenge_accept(
    db: &PgPool,
    redis: &mut ConnectionManager,
    challenge_id: &str,
    link_token: &str,
    accepter_id: &str,
    now_ms: u64,
) -> Result<(MatchBundle, MatchBundle), ChallengeError> {
    require_player(accepter_id)?;
    if link_token.len() < 16 || link_token.len() > 64 {
        return Err(ChallengeError::BadToken);
    }

    let row = load_challenge(db, challenge_id).await?;
    if row.link_token != link_token {
        return Err(ChallengeError::BadToken);
    }
    if row.status != "open" {
        return Err(ChallengeError::NotOpen);
    }
    if now_ms >= row.expires_at_ms {
        let _ = sqlx::query(
            "UPDATE challenges SET status = 'expired' WHERE id = $1 AND status = 'open'",
        )
        .bind(challenge_id)
        .execute(db)
        .await;
        return Err(ChallengeError::Expired);
    }
    if row.from_player_id == accepter_id {
        return Err(ChallengeError::Match(MatchError::SelfMatch));
    }
    if let Some(ref only) = row.opponent_player_id {
        if only != accepter_id {
            return Err(ChallengeError::Forbidden);
        }
    }

    let creator_pref = ColorPref::parse(&row.color_pref).unwrap_or(ColorPref::Random);
    // Accepter takes the opposite preference when creator fixed a color; else random.
    let accepter_pref = match creator_pref {
        ColorPref::White => ColorPref::Black,
        ColorPref::Black => ColorPref::White,
        ColorPref::Random => ColorPref::Random,
    };
    let coin = (Ulid::new().to_string().as_bytes()[0] & 1) == 1;
    let (white_id, black_id) = assign_colors(
        &row.from_player_id,
        creator_pref,
        accepter_id,
        accepter_pref,
        coin,
    )
    .ok_or_else(|| ChallengeError::Match(MatchError::Internal("color assign failed".into())))?;

    let config = PairConfig {
        time_class: row.time_class,
        initial_ms: row.initial_ms as u64,
        increment_ms: row.increment_ms as u64,
        rated: row.rated,
        color: creator_pref,
    };

    // Claim before Redis create so concurrent accepts cannot both succeed.
    let claimed =
        sqlx::query("UPDATE challenges SET status = 'accepted' WHERE id = $1 AND status = 'open'")
            .bind(challenge_id)
            .execute(db)
            .await?
            .rows_affected();
    if claimed != 1 {
        return Err(ChallengeError::NotOpen);
    }

    let game_id = match create_matched_game(redis, &config, &white_id, &black_id, now_ms).await {
        Ok(gid) => gid,
        Err(err) => {
            let _ = sqlx::query(
                "UPDATE challenges SET status = 'open' WHERE id = $1 AND status = 'accepted' AND game_id IS NULL",
            )
            .bind(challenge_id)
            .execute(db)
            .await;
            return Err(ChallengeError::Match(err));
        }
    };
    sqlx::query("UPDATE challenges SET game_id = $2 WHERE id = $1")
        .bind(challenge_id)
        .bind(&game_id)
        .execute(db)
        .await?;

    let for_creator =
        match_bundle_for(&game_id, &config, &white_id, &black_id, &row.from_player_id);
    let for_accepter = match_bundle_for(&game_id, &config, &white_id, &black_id, accepter_id);
    store_pair_notify(redis, &row.from_player_id, &for_creator).await?;
    Ok((for_accepter, for_creator))
}

/// Decline an open challenge (creator cancel uses `challenge_cancel`).
///
/// Open links require `link_token` so a leaked `challenge_id` alone cannot grief-decline.
pub async fn challenge_decline(
    db: &PgPool,
    challenge_id: &str,
    link_token: &str,
    actor_id: &str,
) -> Result<(), ChallengeError> {
    require_player(actor_id)?;
    if link_token.len() < 16 || link_token.len() > 64 {
        return Err(ChallengeError::BadToken);
    }
    let row = load_challenge(db, challenge_id).await?;
    if row.status != "open" {
        return Err(ChallengeError::NotOpen);
    }
    if row.link_token != link_token {
        return Err(ChallengeError::BadToken);
    }
    // Directed: only the named opponent may decline. Open link: any non-creator with
    // the token may decline; creator uses cancel.
    if row.from_player_id == actor_id {
        return Err(ChallengeError::Forbidden);
    }
    if let Some(ref only) = row.opponent_player_id {
        if only != actor_id {
            return Err(ChallengeError::Forbidden);
        }
    }
    let updated =
        sqlx::query("UPDATE challenges SET status = 'declined' WHERE id = $1 AND status = 'open'")
            .bind(challenge_id)
            .execute(db)
            .await?
            .rows_affected();
    if updated != 1 {
        return Err(ChallengeError::NotOpen);
    }
    Ok(())
}

/// Creator cancels an open challenge.
pub async fn challenge_cancel(
    db: &PgPool,
    challenge_id: &str,
    creator_id: &str,
) -> Result<(), ChallengeError> {
    require_player(creator_id)?;
    let row = load_challenge(db, challenge_id).await?;
    if row.from_player_id != creator_id {
        return Err(ChallengeError::Forbidden);
    }
    if row.status != "open" {
        return Err(ChallengeError::NotOpen);
    }
    let updated =
        sqlx::query("UPDATE challenges SET status = 'cancelled' WHERE id = $1 AND status = 'open'")
            .bind(challenge_id)
            .execute(db)
            .await?
            .rows_affected();
    if updated != 1 {
        return Err(ChallengeError::NotOpen);
    }
    Ok(())
}
