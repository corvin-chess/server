//! CV-132: Redis connection + presence keys (integration when REDIS_URL is set).

use std::env;
use std::time::Duration;

use corvin_server::redis_store::{
    clear_presence, connect, is_online, ping, presence_key, presence_pttl_ms, require_redis_url,
    touch_presence, PresenceError, DEFAULT_PRESENCE_TTL_SECS, MAX_PRESENCE_USER_ID_LEN,
    PRESENCE_KEY_PREFIX,
};

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[test]
fn presence_key_shape_is_stable_and_scoped() {
    assert_eq!(PRESENCE_KEY_PREFIX, "corvin:presence:");
    assert_eq!(
        presence_key("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        "corvin:presence:01ARZ3NDEKTSV4RRFFQ69G5FAV"
    );
    // Heartbeat window: long enough for WS Ping, short enough for stale sockets.
    const TTL: u64 = DEFAULT_PRESENCE_TTL_SECS;
    assert!(
        (30..=300).contains(&TTL),
        "presence TTL out of range: {TTL}"
    );

    let empty = presence_key("");
    assert!(
        empty.is_empty(),
        "empty user id must not invent a free-form key; got {empty:?}"
    );

    let too_long = "a".repeat(MAX_PRESENCE_USER_ID_LEN + 1);
    assert!(
        presence_key(&too_long).is_empty(),
        "oversized user id must be rejected"
    );
}

#[test]
fn presence_key_rejects_path_separators_and_spaces() {
    for bad in ["a/b", "a b", "a\nb", "../x", "a:b"] {
        let key = presence_key(bad);
        assert!(
            key.is_empty(),
            "invalid user id {bad:?} must yield empty key, got {key:?}"
        );
    }
}

#[test]
fn require_redis_url_rejects_missing_and_blank() {
    assert!(matches!(
        require_redis_url(None),
        Err(PresenceError::MissingRedisUrl)
    ));
    assert!(matches!(
        require_redis_url(Some(String::new())),
        Err(PresenceError::MissingRedisUrl)
    ));
    assert!(matches!(
        require_redis_url(Some("   ".to_owned())),
        Err(PresenceError::MissingRedisUrl)
    ));
    assert_eq!(
        require_redis_url(Some("  redis://127.0.0.1:6379/0  ".to_owned())).expect("ok"),
        "redis://127.0.0.1:6379/0"
    );
}

#[test]
fn presence_ops_reject_invalid_user_id_without_redis() {
    // Key validation is sync; ops share the same guard before any Redis call.
    assert!(presence_key("bad id").is_empty());
    assert!(presence_key("a/b").is_empty());
    assert!(presence_key(&"x".repeat(MAX_PRESENCE_USER_ID_LEN + 1)).is_empty());
}

#[tokio::test]
async fn redis_ping_and_presence_round_trip() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_ping_and_presence_round_trip: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    let pong = ping(&mut conn)
        .await
        .unwrap_or_else(|err| panic!("PING failed: {err}"));
    assert_eq!(pong, "PONG");

    let user_id = "01CV132PRESENCETESTUSER0001";
    let key = presence_key(user_id);
    assert!(!key.is_empty());

    // Ensure a clean start for this key (integration DB may be shared).
    clear_presence(&mut conn, user_id)
        .await
        .expect("clear before touch");
    assert!(!is_online(&mut conn, user_id)
        .await
        .expect("is_online after clear"));

    touch_presence(&mut conn, user_id, DEFAULT_PRESENCE_TTL_SECS)
        .await
        .expect("touch_presence");
    assert!(is_online(&mut conn, user_id)
        .await
        .expect("is_online after touch"));
    let first_pttl = presence_pttl_ms(&mut conn, user_id)
        .await
        .expect("pttl after touch")
        .expect("ttl must be set");
    assert!(
        first_pttl > 0 && first_pttl <= (DEFAULT_PRESENCE_TTL_SECS as i64) * 1000,
        "unexpected pttl {first_pttl}"
    );

    // Brief pause so a refresh can show a higher remaining TTL than a drained key.
    tokio::time::sleep(Duration::from_millis(200)).await;
    touch_presence(&mut conn, user_id, DEFAULT_PRESENCE_TTL_SECS)
        .await
        .expect("touch_presence refresh");
    let refreshed = presence_pttl_ms(&mut conn, user_id)
        .await
        .expect("pttl after refresh")
        .expect("ttl after refresh");
    assert!(
        refreshed > first_pttl.saturating_sub(200),
        "refresh should reset TTL; before≈{first_pttl} after={refreshed}"
    );
    assert!(is_online(&mut conn, user_id)
        .await
        .expect("is_online after refresh"));

    clear_presence(&mut conn, user_id)
        .await
        .expect("clear_presence");
    assert!(!is_online(&mut conn, user_id)
        .await
        .expect("is_online after final clear"));

    // Short TTL expires without an explicit clear; poll PTTL instead of fixed sleep.
    touch_presence(&mut conn, user_id, 1)
        .await
        .expect("touch with 1s TTL");
    assert!(is_online(&mut conn, user_id)
        .await
        .expect("online before expire"));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let online = is_online(&mut conn, user_id)
            .await
            .expect("is_online while waiting for TTL");
        if !online {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("presence key did not expire within 3s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let zero_ttl = touch_presence(&mut conn, user_id, 0)
        .await
        .expect_err("ttl 0 must fail");
    assert!(matches!(zero_ttl, PresenceError::InvalidTtl));
}

#[tokio::test]
async fn presence_ops_reject_invalid_user_id_on_redis_path() {
    let Some(url) = redis_url() else {
        eprintln!("skip presence_ops_reject_invalid_user_id_on_redis_path: REDIS_URL unset");
        return;
    };

    let mut conn = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));

    for bad in ["bad id", "a/b"] {
        assert!(matches!(
            touch_presence(&mut conn, bad, 60).await,
            Err(PresenceError::InvalidUserId)
        ));
        assert!(matches!(
            is_online(&mut conn, bad).await,
            Err(PresenceError::InvalidUserId)
        ));
        assert!(matches!(
            clear_presence(&mut conn, bad).await,
            Err(PresenceError::InvalidUserId)
        ));
        assert!(matches!(
            presence_pttl_ms(&mut conn, bad).await,
            Err(PresenceError::InvalidUserId)
        ));
    }
}
