//! Pair / challenge clock config validation (PROTOCOL QuickPair / ChallengeCreate).

use std::fmt;

use super::color::ColorPref;

/// PROTOCOL play message version.
pub const PROTOCOL_V: u32 = 1;

/// Shared clock + rated + color preference for a seek or challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairConfig {
    pub time_class: String,
    pub initial_ms: u64,
    pub increment_ms: u64,
    pub rated: bool,
    pub color: ColorPref,
}

/// Matchmaking validation / Redis / Postgres errors.
#[derive(Debug)]
pub enum MatchError {
    InvalidPlayerId,
    InvalidConfig(&'static str),
    SelfMatch,
    Redis(redis::RedisError),
    Game(crate::game::GameLogError),
    Internal(String),
}

impl fmt::Display for MatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPlayerId => write!(f, "invalid player id"),
            Self::InvalidConfig(msg) => write!(f, "invalid config: {msg}"),
            Self::SelfMatch => write!(f, "cannot match yourself"),
            Self::Redis(err) => write!(f, "{err}"),
            Self::Game(err) => write!(f, "{err}"),
            Self::Internal(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for MatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Redis(err) => Some(err),
            Self::Game(err) => Some(err),
            _ => None,
        }
    }
}

impl From<redis::RedisError> for MatchError {
    fn from(value: redis::RedisError) -> Self {
        Self::Redis(value)
    }
}

impl From<crate::game::GameLogError> for MatchError {
    fn from(value: crate::game::GameLogError) -> Self {
        Self::Game(value)
    }
}

/// Validate PROTOCOL time-control bounds and time_class / color.
pub fn validate_pair_config(config: &PairConfig) -> Result<(), MatchError> {
    let tc = config.time_class.trim();
    if tc.is_empty() || tc.len() > 32 {
        return Err(MatchError::InvalidConfig("time_class length"));
    }
    if !tc
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(MatchError::InvalidConfig("time_class charset"));
    }
    // Cap at 7 days initial (correspondence upper bound for v1 helpers).
    if config.initial_ms == 0 || config.initial_ms > 604_800_000 {
        return Err(MatchError::InvalidConfig("initial_ms out of range"));
    }
    if config.increment_ms > 3_600_000 {
        return Err(MatchError::InvalidConfig("increment_ms out of range"));
    }
    Ok(())
}

pub fn is_safe_player_id(player_id: &str, max_len: usize) -> bool {
    if player_id.is_empty() || player_id.len() > max_len {
        return false;
    }
    player_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
