//! Redis quick-pair pools (ADR-006 / ARCHITECTURE).

use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

use super::color::{assign_colors, ColorPref};
use super::config::{is_safe_player_id, validate_pair_config, MatchError, PairConfig};
use super::start::{
    bundle_for_player, bundles_for_pair, create_matched_game, MatchBundle, QuickPairOutcome,
    MAX_PLAYER_ID_LEN,
};

/// Pool index prefix: `corvin:pair:pool:{time_class}:{rated}:{initial}:{inc}`.
pub const PAIR_KEY_PREFIX: &str = "corvin:pair:pool:";

/// Waiting seeker payload key prefix: `corvin:pair:seek:{player_id}`.
pub const QUEUE_KEY_PREFIX: &str = "corvin:pair:seek:";

/// Ready `GameStart` notify key: `corvin:pair:notify:{player_id}`.
pub const PAIR_NOTIFY_PREFIX: &str = "corvin:pair:notify:";

/// Seek TTL so abandoned queues expire (seconds).
pub const PAIR_SEEK_TTL_SECS: u64 = 120;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SeekRecord {
    player_id: String,
    color: String,
    time_class: String,
    initial_ms: u64,
    increment_ms: u64,
    rated: bool,
    enqueued_ms: u64,
}

/// Exact-config pool key so mismatched clocks never pair.
pub fn pool_key(config: &PairConfig) -> String {
    let rated = if config.rated { "rated" } else { "casual" };
    let tc = config.time_class.trim().to_ascii_lowercase();
    format!(
        "{PAIR_KEY_PREFIX}{tc}:{rated}:{}:{}",
        config.initial_ms, config.increment_ms
    )
}

fn seek_key(player_id: &str) -> String {
    format!("{QUEUE_KEY_PREFIX}{player_id}")
}

fn notify_key(player_id: &str) -> String {
    format!("{PAIR_NOTIFY_PREFIX}{player_id}")
}

fn require_player(player_id: &str) -> Result<(), MatchError> {
    if !is_safe_player_id(player_id, MAX_PLAYER_ID_LEN) {
        return Err(MatchError::InvalidPlayerId);
    }
    Ok(())
}

/// Enqueue for quick pair or match an existing compatible seeker in the same pool.
pub async fn quick_pair(
    conn: &mut ConnectionManager,
    player_id: &str,
    config: &PairConfig,
    now_ms: u64,
) -> Result<QuickPairOutcome, MatchError> {
    require_player(player_id)?;
    validate_pair_config(config)?;

    // Deliver a pending notify from a prior match before re-enqueueing.
    if let Some(bundle) = take_pair_result(conn, player_id).await? {
        return Ok(QuickPairOutcome::Matched(bundle));
    }

    // Replace any prior seek for this player.
    let _ = quick_pair_cancel(conn, player_id).await?;

    let key = pool_key(config);
    // Try up to a few queue heads in case of stale / incompatible / self entries.
    for _ in 0..8 {
        let other: Option<String> = conn.lpop(&key, None).await?;
        let Some(other_id) = other else {
            break;
        };
        if other_id == player_id || !is_safe_player_id(&other_id, MAX_PLAYER_ID_LEN) {
            continue;
        }
        // Atomic claim: only one matcher can GETDEL a seek (Redis 6.2+ / CI Redis 7).
        let other_seek: Option<String> = redis::cmd("GETDEL")
            .arg(seek_key(&other_id))
            .query_async(conn)
            .await?;
        let Some(raw) = other_seek else {
            // Stale pool entry or already claimed by another matcher.
            continue;
        };
        let record: SeekRecord = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(_) => continue,
        };
        // Config must still match (defensive; pool key already encodes it).
        if record.time_class != config.time_class.trim().to_ascii_lowercase()
            || record.initial_ms != config.initial_ms
            || record.increment_ms != config.increment_ms
            || record.rated != config.rated
        {
            // Restore seek and re-queue; wrong pool contamination.
            restore_seek(conn, &other_id, &raw).await?;
            let _: () = conn.rpush(&key, &other_id).await?;
            continue;
        }
        let other_pref = ColorPref::parse(&record.color).unwrap_or(ColorPref::Random);
        let coin = (Ulid::new().to_string().as_bytes()[0] & 1) == 1;
        let Some((white_id, black_id)) =
            assign_colors(player_id, config.color, &other_id, other_pref, coin)
        else {
            // Color conflict: restore seek and re-queue.
            restore_seek(conn, &other_id, &raw).await?;
            let _: () = conn.rpush(&key, &other_id).await?;
            continue;
        };

        let game_id = match create_matched_game(conn, config, &white_id, &black_id, now_ms).await {
            Ok(gid) => gid,
            Err(err) => {
                // Return the waiter to the pool so they can match again.
                restore_seek(conn, &other_id, &raw).await?;
                let _: () = conn.rpush(&key, &other_id).await?;
                return Err(err);
            }
        };
        let (white_bundle, black_bundle) = bundles_for_pair(&game_id, config, &white_id, &black_id);
        let for_other = if other_id == white_id {
            white_bundle.clone()
        } else {
            black_bundle.clone()
        };
        let for_me = if player_id == white_id {
            white_bundle
        } else {
            black_bundle
        };

        store_notify(conn, &other_id, &for_other).await?;
        return Ok(QuickPairOutcome::Matched(for_me));
    }

    // Nobody waiting: enqueue. Cancel above removed prior seek/list entries for this player.
    let record = SeekRecord {
        player_id: player_id.to_owned(),
        color: config.color.as_str().to_owned(),
        time_class: config.time_class.trim().to_ascii_lowercase(),
        initial_ms: config.initial_ms,
        increment_ms: config.increment_ms,
        rated: config.rated,
        enqueued_ms: now_ms,
    };
    let payload =
        serde_json::to_string(&record).map_err(|err| MatchError::Internal(err.to_string()))?;
    let sk = seek_key(player_id);
    let _: () = conn.set_ex(&sk, payload, PAIR_SEEK_TTL_SECS).await?;
    let _: () = conn.rpush(&key, player_id).await?;
    // Bound pool list TTL loosely via seek TTL; also EXPIRE the list.
    let _: bool = conn.expire(&key, PAIR_SEEK_TTL_SECS as i64).await?;
    Ok(QuickPairOutcome::Waiting)
}

async fn restore_seek(
    conn: &mut ConnectionManager,
    player_id: &str,
    raw: &str,
) -> Result<(), MatchError> {
    let sk = seek_key(player_id);
    let _: () = conn.set_ex(sk, raw, PAIR_SEEK_TTL_SECS).await?;
    Ok(())
}

/// Remove a player's seek from the pool (PROTOCOL `QuickPairCancel`).
pub async fn quick_pair_cancel(
    conn: &mut ConnectionManager,
    player_id: &str,
) -> Result<bool, MatchError> {
    require_player(player_id)?;
    let sk = seek_key(player_id);
    let raw: Option<String> = conn.get(&sk).await?;
    let Some(raw) = raw else {
        return Ok(false);
    };
    let _: u64 = conn.del(&sk).await?;
    if let Ok(record) = serde_json::from_str::<SeekRecord>(&raw) {
        let cfg = PairConfig {
            time_class: record.time_class,
            initial_ms: record.initial_ms,
            increment_ms: record.increment_ms,
            rated: record.rated,
            color: ColorPref::Random,
        };
        let key = pool_key(&cfg);
        let _: u64 = conn.lrem(&key, 0, player_id).await?;
    }
    Ok(true)
}

/// Take a pending match notify for a waiting player (simulates WS `GameStart` push).
pub async fn take_pair_result(
    conn: &mut ConnectionManager,
    player_id: &str,
) -> Result<Option<MatchBundle>, MatchError> {
    require_player(player_id)?;
    let key = notify_key(player_id);
    let raw: Option<String> = conn.get(&key).await?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let _: u64 = conn.del(&key).await?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|err| MatchError::Internal(format!("corrupt pair notify: {err}")))?;
    let game_id = value
        .get("game_id")
        .and_then(Value::as_str)
        .ok_or_else(|| MatchError::Internal("notify missing game_id".into()))?
        .to_owned();
    let your_color = value
        .get("color")
        .and_then(Value::as_str)
        .ok_or_else(|| MatchError::Internal("notify missing color".into()))?;
    let your_color = if your_color == "w" { "w" } else { "b" };
    let white_id = value["opponents"]["w"]["id"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let black_id = value["opponents"]["b"]["id"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    Ok(Some(MatchBundle {
        game_id,
        white_id,
        black_id,
        your_color,
        start: value,
    }))
}

async fn store_notify(
    conn: &mut ConnectionManager,
    player_id: &str,
    bundle: &MatchBundle,
) -> Result<(), MatchError> {
    let key = notify_key(player_id);
    let payload = bundle.start.to_string();
    let _: () = conn.set_ex(key, payload, PAIR_SEEK_TTL_SECS).await?;
    Ok(())
}

/// Store a GameStart notify for a player (challenge accept path).
pub async fn store_pair_notify(
    conn: &mut ConnectionManager,
    player_id: &str,
    bundle: &MatchBundle,
) -> Result<(), MatchError> {
    require_player(player_id)?;
    store_notify(conn, player_id, bundle).await
}

/// Rebuild a bundle from known seats (challenge path helper).
pub fn match_bundle_for(
    game_id: &str,
    config: &PairConfig,
    white_id: &str,
    black_id: &str,
    player_id: &str,
) -> MatchBundle {
    bundle_for_player(game_id, config, white_id, black_id, player_id)
}
