//! Redis connection helpers and online presence keys (CV-132).
//!
//! Presence is ephemeral: a key per user with a short TTL refreshed by heartbeats.
//! Live game event logs live in `crate::game` (CV-161); lobby keys come later.

use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Client, RedisError};
use std::fmt;

/// Redis key prefix for per-user online presence (`corvin:presence:{user_id}`).
pub const PRESENCE_KEY_PREFIX: &str = "corvin:presence:";

/// Default presence TTL in seconds. Short enough that a dead socket expires soon;
/// long enough that a normal heartbeat interval (WS Ping) can refresh it.
pub const DEFAULT_PRESENCE_TTL_SECS: u64 = 60;

/// Max user id length for presence keys (ULID is 26; headroom for future opaque ids).
pub const MAX_PRESENCE_USER_ID_LEN: usize = 32;

/// Errors from Redis connect / presence operations.
#[derive(Debug)]
pub enum PresenceError {
    /// `REDIS_URL` was missing or empty.
    MissingRedisUrl,
    /// User id failed the presence key allowlist (ASCII alnum + limited punctuation).
    InvalidUserId,
    /// TTL must be at least 1 second (Redis EX 0 expires immediately).
    InvalidTtl,
    /// Underlying redis-rs error.
    Redis(RedisError),
}

impl fmt::Display for PresenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRedisUrl => write!(f, "REDIS_URL is required for Redis"),
            Self::InvalidUserId => write!(f, "invalid presence user id"),
            Self::InvalidTtl => write!(f, "presence TTL must be at least 1 second"),
            Self::Redis(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PresenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Redis(err) => Some(err),
            _ => None,
        }
    }
}

impl From<RedisError> for PresenceError {
    fn from(value: RedisError) -> Self {
        Self::Redis(value)
    }
}

/// Build the Redis key for a user's presence, or empty if `user_id` is invalid.
///
/// Allowed characters: ASCII letters, digits, `_`, `-` (ULID-safe). Empty input,
/// oversized ids, and path/space/colon characters are rejected so callers cannot
/// invent rogue keys.
pub fn presence_key(user_id: &str) -> String {
    if user_id.is_empty() || !is_safe_user_id(user_id) {
        return String::new();
    }
    format!("{PRESENCE_KEY_PREFIX}{user_id}")
}

fn is_safe_user_id(user_id: &str) -> bool {
    if user_id.len() > MAX_PRESENCE_USER_ID_LEN {
        return false;
    }
    user_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Read and validate `REDIS_URL` from the environment (no network I/O).
pub fn redis_url_from_env() -> Result<String, PresenceError> {
    require_redis_url(std::env::var("REDIS_URL").ok())
}

/// Validate a Redis URL string from config or env (empty / missing rejected).
pub fn require_redis_url(raw: Option<String>) -> Result<String, PresenceError> {
    let url = raw
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or(PresenceError::MissingRedisUrl)?;
    Ok(url)
}

/// Connect using `REDIS_URL` (required).
pub async fn connect_from_env() -> Result<ConnectionManager, PresenceError> {
    let url = redis_url_from_env()?;
    connect(&url).await
}

/// Connect with an explicit Redis URL (tests and callers with config).
pub async fn connect(redis_url: &str) -> Result<ConnectionManager, PresenceError> {
    let url = redis_url.trim();
    if url.is_empty() {
        return Err(PresenceError::MissingRedisUrl);
    }
    let client = Client::open(url)?;
    let manager = client.get_connection_manager().await?;
    Ok(manager)
}

/// `PING` Redis; returns the reply string (normally `PONG`).
pub async fn ping(conn: &mut ConnectionManager) -> Result<String, PresenceError> {
    let reply: String = redis::cmd("PING").query_async(conn).await?;
    Ok(reply)
}

/// Remaining TTL in milliseconds for a presence key, or `None` if absent / no expire.
pub async fn presence_pttl_ms(
    conn: &mut ConnectionManager,
    user_id: &str,
) -> Result<Option<i64>, PresenceError> {
    let key = presence_key(user_id);
    if key.is_empty() {
        return Err(PresenceError::InvalidUserId);
    }
    let pttl: i64 = redis::cmd("PTTL").arg(key).query_async(conn).await?;
    // Redis: -2 missing, -1 no expire, >=0 remaining ms.
    if pttl < 0 {
        return Ok(None);
    }
    Ok(Some(pttl))
}

/// Mark `user_id` online with `ttl_secs` (SET key value EX ttl).
pub async fn touch_presence(
    conn: &mut ConnectionManager,
    user_id: &str,
    ttl_secs: u64,
) -> Result<(), PresenceError> {
    if ttl_secs == 0 {
        return Err(PresenceError::InvalidTtl);
    }
    let key = presence_key(user_id);
    if key.is_empty() {
        return Err(PresenceError::InvalidUserId);
    }
    // Value is a fixed marker; WS connection ids can layer on later without key renames.
    let _: () = conn.set_ex(key, "1", ttl_secs).await?;
    Ok(())
}

/// True when the presence key exists (within TTL).
pub async fn is_online(conn: &mut ConnectionManager, user_id: &str) -> Result<bool, PresenceError> {
    let key = presence_key(user_id);
    if key.is_empty() {
        return Err(PresenceError::InvalidUserId);
    }
    let exists: bool = conn.exists(key).await?;
    Ok(exists)
}

/// Remove presence immediately (disconnect / logout).
pub async fn clear_presence(
    conn: &mut ConnectionManager,
    user_id: &str,
) -> Result<(), PresenceError> {
    let key = presence_key(user_id);
    if key.is_empty() {
        return Err(PresenceError::InvalidUserId);
    }
    let _: () = conn.del(key).await?;
    Ok(())
}
