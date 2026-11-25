//! CV-173: CAS append for offers/claims — concurrent non-move writers cannot race seq.

use std::env;
use std::sync::Arc;

use corvin_server::game::{
    append_game_event, create_game, delete_game, load_events, load_meta, offer_draw, resign,
    CasCommitResult, GameEvent, GameMeta, OfferError, OfferOutcome, START_FEN,
};
use corvin_server::redis_store::connect;
use tokio::sync::Barrier;

const WHITE: &str = "player-white-ulid-cv173-0001";
const BLACK: &str = "player-black-ulid-cv173-0001";

fn redis_url() -> Option<String> {
    env::var("REDIS_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn seated_meta() -> GameMeta {
    GameMeta {
        initial_fen: START_FEN.to_owned(),
        created_ms: 1_730_000_000_000,
        white_id: WHITE.to_owned(),
        black_id: BLACK.to_owned(),
        clock_w_ms: 300_000,
        clock_b_ms: 300_000,
        ..GameMeta::default()
    }
}

#[test]
fn cas_append_reuses_apply_reply_shape() {
    // commit_append_cas shares parse_cas_commit_reply with apply CAS (CV-171).
    assert_eq!(
        corvin_server::game::parse_cas_commit_reply(&[1, 7, 8]),
        CasCommitResult::Ok {
            move_seq: 7,
            last_seq: 8
        }
    );
    assert_eq!(
        corvin_server::game::parse_cas_commit_reply(&[0, 3]),
        CasCommitResult::Conflict { actual_len: 3 }
    );
}

#[tokio::test]
async fn redis_concurrent_double_resign_one_end() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_concurrent_double_resign_one_end: REDIS_URL unset");
        return;
    };

    let mut setup = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV173CONCURRENTRESIGN00001";
    let _ = delete_game(&mut setup, game_id).await;
    create_game(&mut setup, game_id, &seated_meta())
        .await
        .expect("create");

    let barrier = Arc::new(Barrier::new(2));
    let url = Arc::new(url);
    let game = Arc::new(game_id.to_owned());

    let white = {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            resign(&mut conn, game.as_str(), WHITE, 100).await
        }
    };
    let black = {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            resign(&mut conn, game.as_str(), BLACK, 101).await
        }
    };

    let (a, b) = tokio::join!(white, black);
    let oks = [&a, &b].iter().filter(|r| r.is_ok()).count();
    let errs = [&a, &b].iter().filter(|r| r.is_err()).count();
    assert_eq!(oks, 1, "exactly one resign must win: {a:?} {b:?}");
    assert_eq!(errs, 1, "loser must fail: {a:?} {b:?}");

    let loser = if a.is_err() { &a } else { &b };
    assert!(
        matches!(loser, Err(OfferError::GameGone)),
        "loser after CAS retry should be GAME_GONE: {loser:?}"
    );

    let winner = if a.is_ok() { &a } else { &b };
    assert!(
        matches!(
            winner,
            Ok(OfferOutcome::End(end)) if end.reason == "resign"
        ),
        "winner must be resign GameEnd: {winner:?}"
    );

    let events = load_events(&mut setup, game_id).await.expect("events");
    assert_eq!(events.len(), 1, "one Resign event only");
    let meta = load_meta(&mut setup, game_id).await.expect("meta");
    assert!(meta.ended);
    assert_eq!(meta.ended_reason.as_deref(), Some("resign"));

    delete_game(&mut setup, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_concurrent_offer_vs_resign() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_concurrent_offer_vs_resign: REDIS_URL unset");
        return;
    };

    let mut setup = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV173CONCURRENTOFFERRESIGN1";
    let _ = delete_game(&mut setup, game_id).await;
    create_game(&mut setup, game_id, &seated_meta())
        .await
        .expect("create");

    let barrier = Arc::new(Barrier::new(2));
    let url = Arc::new(url);
    let game = Arc::new(game_id.to_owned());

    let offer = {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            offer_draw(&mut conn, game.as_str(), WHITE, 200).await
        }
    };
    let end = {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            resign(&mut conn, game.as_str(), BLACK, 201).await
        }
    };

    let (a, b) = tokio::join!(offer, end);
    let results = [a, b];
    let ends = results
        .iter()
        .filter(|r| matches!(r, Ok(OfferOutcome::End(_))))
        .count();
    let states = results
        .iter()
        .filter(|r| matches!(r, Ok(OfferOutcome::State(_))))
        .count();
    let gone = results
        .iter()
        .filter(|r| matches!(r, Err(OfferError::GameGone)))
        .count();

    // Exactly one terminal resign wins; the offer either commits first (State) then
    // resign ends, or loses the race (GameGone). Never two Ends, never duplicate seq.
    assert_eq!(ends, 1, "exactly one GameEnd: {results:?}");
    assert_eq!(
        states + gone,
        1,
        "offer must State or GameGone: {results:?}"
    );

    let events = load_events(&mut setup, game_id).await.expect("events");
    let seqs: Vec<u64> = events
        .iter()
        .map(|e| match e {
            GameEvent::Offer { seq, .. }
            | GameEvent::Resign { seq, .. }
            | GameEvent::OfferResponse { seq, .. }
            | GameEvent::Abort { seq, .. }
            | GameEvent::Takeback { seq, .. }
            | GameEvent::ClaimDraw { seq, .. }
            | GameEvent::AutoDraw { seq, .. }
            | GameEvent::Move { seq, .. }
            | GameEvent::ServerNote { seq, .. } => *seq,
        })
        .collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(seqs, sorted, "seqs must be monotonic unique");
    for (i, seq) in sorted.iter().enumerate() {
        assert_eq!(*seq, (i + 1) as u64, "seqs must be 1..=n without gaps");
    }

    let meta = load_meta(&mut setup, game_id).await.expect("meta");
    assert!(meta.ended);

    delete_game(&mut setup, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_concurrent_identical_draw_offers() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_concurrent_identical_draw_offers: REDIS_URL unset");
        return;
    };

    let mut setup = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV173CONCURRENTDRAWOFFER0001";
    let _ = delete_game(&mut setup, game_id).await;
    create_game(&mut setup, game_id, &seated_meta())
        .await
        .expect("create");

    let barrier = Arc::new(Barrier::new(2));
    let url = Arc::new(url);
    let game = Arc::new(game_id.to_owned());

    let white = {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            offer_draw(&mut conn, game.as_str(), WHITE, 300).await
        }
    };
    let black = {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            offer_draw(&mut conn, game.as_str(), BLACK, 301).await
        }
    };

    let (a, b) = tokio::join!(white, black);
    assert!(a.is_ok() && b.is_ok(), "both should resolve: {a:?} {b:?}");

    // One side offers; the other either also offers (if they raced before pending)
    // then counter-accepts on retry, or accepts immediately. Net: at most one
    // pending, unique seqs, and either both State or one End (agreement).
    let ends = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Ok(OfferOutcome::End(_))))
        .count();
    let states = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Ok(OfferOutcome::State(_))))
        .count();
    assert!(
        (ends == 1 && states == 1) || (ends == 0 && states == 2),
        "expected offer+accept or dual-offer race resolved: {a:?} {b:?}"
    );

    let events = load_events(&mut setup, game_id).await.expect("events");
    let mut seqs: Vec<u64> = events
        .iter()
        .map(|e| match e {
            GameEvent::Offer { seq, .. }
            | GameEvent::Resign { seq, .. }
            | GameEvent::OfferResponse { seq, .. }
            | GameEvent::Abort { seq, .. }
            | GameEvent::Takeback { seq, .. }
            | GameEvent::ClaimDraw { seq, .. }
            | GameEvent::AutoDraw { seq, .. }
            | GameEvent::Move { seq, .. }
            | GameEvent::ServerNote { seq, .. } => *seq,
        })
        .collect();
    let unique: std::collections::BTreeSet<_> = seqs.iter().copied().collect();
    assert_eq!(
        unique.len(),
        seqs.len(),
        "duplicate seq under concurrent offers"
    );
    seqs.sort_unstable();
    for (i, seq) in seqs.iter().enumerate() {
        assert_eq!(*seq, (i + 1) as u64);
    }

    delete_game(&mut setup, game_id).await.expect("cleanup");
}

#[tokio::test]
async fn redis_concurrent_bare_append_unique_seq() {
    let Some(url) = redis_url() else {
        eprintln!("skip redis_concurrent_bare_append_unique_seq: REDIS_URL unset");
        return;
    };

    let mut setup = connect(&url)
        .await
        .unwrap_or_else(|err| panic!("connect REDIS_URL failed: {err}"));
    let game_id = "01CV173CONCURRENTBAREAPPEND001";
    let _ = delete_game(&mut setup, game_id).await;
    create_game(&mut setup, game_id, &seated_meta())
        .await
        .expect("create");

    let barrier = Arc::new(Barrier::new(2));
    let url = Arc::new(url);
    let game = Arc::new(game_id.to_owned());

    let left = {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            append_game_event(
                &mut conn,
                game.as_str(),
                GameEvent::ServerNote {
                    seq: 0,
                    text: "left".into(),
                    ts: 1,
                },
            )
            .await
        }
    };
    let right = {
        let barrier = Arc::clone(&barrier);
        let url = Arc::clone(&url);
        let game = Arc::clone(&game);
        async move {
            let mut conn = connect(url.as_str()).await.expect("conn");
            barrier.wait().await;
            append_game_event(
                &mut conn,
                game.as_str(),
                GameEvent::ServerNote {
                    seq: 0,
                    text: "right".into(),
                    ts: 2,
                },
            )
            .await
        }
    };

    let (a, b) = tokio::join!(left, right);
    let sa = a.expect("left append");
    let sb = b.expect("right append");
    assert_ne!(sa, sb, "CAS append must assign distinct seqs");
    assert_eq!(
        std::collections::BTreeSet::from([sa, sb]),
        std::collections::BTreeSet::from([1, 2])
    );

    let events = load_events(&mut setup, game_id).await.expect("events");
    assert_eq!(events.len(), 2);

    delete_game(&mut setup, game_id).await.expect("cleanup");
}
