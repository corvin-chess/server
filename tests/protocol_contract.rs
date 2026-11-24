//! CV-170: PROTOCOL.md contract fixtures (always-on; no Redis/Postgres).
//!
//! Golden JSON under `tests/fixtures/protocol/` is the CI source of truth for
//! message shapes. Root `PROTOCOL.md` lives outside this repository checkout.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use corvin_server::game::{
    claim_denied_json, clock_sync_json, game_end_claim_json, game_end_flag_json,
    game_end_offer_json, move_applied_json, move_rejected_json, offer_error_json, offer_state_json,
    ClaimRejectCode, FlagOutcome, GameEndOut, MoveRejectCode, OfferRejectCode, OFFER_DRAW,
};
use corvin_server::matchmaking::{challenge_json, game_start_json, ColorPref, PairConfig};
use corvin_server::rating::{game_end_with_rating_diff, Rating, RatingDiffs};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct Manifest {
    protocol_version: u32,
    error_codes: Vec<String>,
    messages: Vec<ManifestMessage>,
}

#[derive(Debug, Deserialize)]
struct ManifestMessage {
    id: String,
    typ: String,
    file: String,
    required: Vec<String>,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/protocol")
}

fn load_manifest() -> Manifest {
    let path = fixtures_dir().join("manifest.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "missing protocol fixture manifest at {}: {err}",
            path.display()
        )
    });
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("invalid manifest.json: {err}"))
}

fn load_fixture(file: &str) -> Value {
    // Reject traversal and absolute paths so a tampered manifest cannot escape the fixture dir.
    if file.is_empty()
        || file.contains('/')
        || file.contains('\\')
        || file.contains("..")
        || PathBuf::from(file).is_absolute()
    {
        panic!("fixture file must be a bare name under tests/fixtures/protocol: {file}");
    }
    let path = fixtures_dir().join(file);
    let meta = fs::metadata(&path)
        .unwrap_or_else(|err| panic!("missing fixture {}: {err}", path.display()));
    // Bound CI memory if a fixture is accidentally huge.
    const MAX_FIXTURE_BYTES: u64 = 64 * 1024;
    assert!(
        meta.len() <= MAX_FIXTURE_BYTES,
        "fixture {} exceeds {MAX_FIXTURE_BYTES} bytes",
        path.display()
    );
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("missing fixture {}: {err}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("invalid JSON in {file}: {err}"))
}

fn assert_required_keys(label: &str, body: &Value, required: &[String]) {
    let obj = body
        .as_object()
        .unwrap_or_else(|| panic!("{label}: fixture must be a JSON object"));
    for key in required {
        assert!(
            obj.contains_key(key),
            "{label}: missing required key `{key}`"
        );
    }
}

fn assert_shape_compatible(label: &str, built: &Value, fixture: &Value) {
    let built_obj = built
        .as_object()
        .unwrap_or_else(|| panic!("{label}: builder output must be object"));
    let fix_obj = fixture
        .as_object()
        .unwrap_or_else(|| panic!("{label}: fixture must be object"));
    for (key, fix_val) in fix_obj {
        let Some(built_val) = built_obj.get(key) else {
            panic!("{label}: builder missing fixture key `{key}`");
        };
        assert_eq!(
            type_name(built_val),
            type_name(fix_val),
            "{label}: key `{key}` type mismatch"
        );
        if fix_val.is_object() {
            assert_shape_compatible(&format!("{label}.{key}"), built_val, fix_val);
        }
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[test]
fn manifest_lists_protocol_v1_and_error_codes() {
    let manifest = load_manifest();
    assert_eq!(manifest.protocol_version, 1);
    let codes: BTreeSet<_> = manifest.error_codes.iter().cloned().collect();
    let expected = BTreeSet::from([
        "PROTO_VERSION".to_owned(),
        "AUTH".to_owned(),
        "RATE_LIMIT".to_owned(),
        "NOT_YOUR_TURN".to_owned(),
        "ILLEGAL_MOVE".to_owned(),
        "GAME_GONE".to_owned(),
        "OFFER_INVALID".to_owned(),
        "ABORT_DENIED".to_owned(),
        "CLAIM_DENIED".to_owned(),
        "INTERNAL".to_owned(),
    ]);
    assert_eq!(
        codes, expected,
        "manifest error_codes must match PROTOCOL.md"
    );
}

#[test]
fn every_manifest_fixture_loads_with_required_keys() {
    let manifest = load_manifest();
    assert!(
        !manifest.messages.is_empty(),
        "manifest must list at least one message fixture"
    );
    let mut seen_ids = BTreeSet::new();
    for msg in &manifest.messages {
        assert!(
            seen_ids.insert(msg.id.clone()),
            "duplicate fixture id {}",
            msg.id
        );
        let body = load_fixture(&msg.file);
        assert_eq!(body["typ"], msg.typ, "fixture {} typ mismatch", msg.id);
        assert_eq!(body["v"], 1, "fixture {} must be protocol v1", msg.id);
        assert_required_keys(&msg.id, &body, &msg.required);
    }
}

#[test]
fn builders_match_fixture_shapes() {
    let fixtures = fixture_map();

    let move_applied = move_applied_json(
        Some("client-move-1"),
        "01CV170GAME0000000000000001",
        "e2e4",
        "e4",
        "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1",
        300_000,
        300_000,
        "b",
        1,
        1_730_000_000_100,
        None,
    );
    assert_shape_compatible("move_applied", &move_applied, &fixtures["move_applied"]);

    let move_applied_lag = move_applied_json(
        Some("client-move-2"),
        "01CV170GAME0000000000000001",
        "e7e5",
        "e5",
        "rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq e6 0 2",
        299_850,
        300_000,
        "w",
        2,
        1_730_000_000_250,
        Some(42),
    );
    assert_shape_compatible(
        "move_applied_lag",
        &move_applied_lag,
        &fixtures["move_applied_lag"],
    );

    let rejected = move_rejected_json("client-bad", MoveRejectCode::IllegalMove, "illegal move");
    assert_shape_compatible(
        "move_rejected",
        &rejected,
        &fixtures["move_rejected_illegal"],
    );

    let clock = clock_sync_json(
        "01CV170GAME0000000000000001",
        290_000,
        295_000,
        "w",
        1_730_000_001_000,
        12,
        18,
        5,
    );
    assert_shape_compatible("clock_sync", &clock, &fixtures["clock_sync"]);

    let offer = offer_state_json(
        "01CV170GAME0000000000000001",
        OFFER_DRAW,
        "w",
        "offered",
        3,
        1_730_000_002_000,
    );
    assert_shape_compatible("offer_state", &offer, &fixtures["offer_state"]);

    let offer_err = offer_error_json(
        Some("client-offer"),
        OfferRejectCode::OfferInvalid,
        "bad offer",
    );
    assert_shape_compatible("offer_error", &offer_err, &fixtures["error_offer_invalid"]);

    let abort_err = offer_error_json(
        Some("client-abort"),
        OfferRejectCode::AbortDenied,
        "abort denied",
    );
    assert_shape_compatible("abort_denied", &abort_err, &fixtures["error_abort_denied"]);

    let claim_err = claim_denied_json(
        Some("client-claim"),
        ClaimRejectCode::ClaimDenied,
        "not claimable",
    );
    assert_shape_compatible("claim_denied", &claim_err, &fixtures["error_claim_denied"]);

    let end_flag = game_end_flag_json(
        "01CV170GAME0000000000000001",
        &FlagOutcome {
            result: "1-0",
            winner: Some("w"),
            reason: "flag",
            flagged: "b",
        },
        1_730_000_003_000,
    );
    assert_shape_compatible("game_end_flag", &end_flag, &fixtures["game_end_flag"]);

    let end_resign = game_end_offer_json(
        "01CV170GAME0000000000000001",
        &GameEndOut {
            result: "0-1",
            winner: Some("b"),
            reason: "resign",
            seq: 10,
        },
        1_730_000_004_000,
    );
    assert_shape_compatible("game_end_resign", &end_resign, &fixtures["game_end_resign"]);

    let end_three = game_end_claim_json(
        "01CV170GAME0000000000000001",
        &GameEndOut {
            result: "1/2-1/2",
            winner: None,
            reason: "threefold",
            seq: 40,
        },
        1_730_000_005_000,
    );
    assert_shape_compatible(
        "game_end_threefold",
        &end_three,
        &fixtures["game_end_threefold"],
    );

    let end_rated = game_end_with_rating_diff(
        end_resign.clone(),
        Some(&RatingDiffs {
            white: -8,
            black: 8,
            white_before: Rating::unrated(),
            white_after: Rating::unrated(),
            black_before: Rating::unrated(),
            black_after: Rating::unrated(),
        }),
    );
    assert_shape_compatible("game_end_rated", &end_rated, &fixtures["game_end_rated"]);

    let config = PairConfig {
        time_class: "blitz".to_owned(),
        initial_ms: 300_000,
        increment_ms: 2_000,
        rated: true,
        color: ColorPref::Random,
    };
    let start = game_start_json(
        "01CV170GAME0000000000000001",
        "w",
        &config,
        "01CV170WHITE000000000000001",
        "01CV170BLACK000000000000001",
    );
    assert_shape_compatible("game_start", &start, &fixtures["game_start"]);

    let challenge = challenge_json(
        "01CV170CHALLENGE00000000001",
        "01CV170WHITE000000000000001",
        &config,
        1_730_000_100_000,
    );
    assert_shape_compatible("challenge", &challenge, &fixtures["challenge"]);
}

#[test]
fn handshake_and_error_fixtures_cover_connection_lifecycle() {
    let fixtures = fixture_map();
    for id in [
        "hello",
        "welcome",
        "ping",
        "pong",
        "error_proto_version",
        "error_internal",
    ] {
        let body = &fixtures[id];
        assert_eq!(body["v"], 1);
        assert!(body.get("typ").is_some(), "{id} needs typ");
    }
    assert_eq!(fixtures["hello"]["typ"], "Hello");
    assert_eq!(fixtures["welcome"]["typ"], "Welcome");
    assert_eq!(fixtures["ping"]["typ"], "Ping");
    assert_eq!(fixtures["pong"]["typ"], "Pong");
    assert_eq!(fixtures["error_proto_version"]["code"], "PROTO_VERSION");
    assert_eq!(fixtures["error_internal"]["code"], "INTERNAL");
}

#[test]
fn move_reject_codes_are_protocol_strings() {
    assert_eq!(MoveRejectCode::IllegalMove.as_str(), "ILLEGAL_MOVE");
    assert_eq!(MoveRejectCode::NotYourTurn.as_str(), "NOT_YOUR_TURN");
    assert_eq!(MoveRejectCode::GameGone.as_str(), "GAME_GONE");
    assert_eq!(MoveRejectCode::Auth.as_str(), "AUTH");
    assert_eq!(OfferRejectCode::OfferInvalid.as_str(), "OFFER_INVALID");
    assert_eq!(OfferRejectCode::AbortDenied.as_str(), "ABORT_DENIED");
    assert_eq!(OfferRejectCode::Auth.as_str(), "AUTH");
    assert_eq!(OfferRejectCode::GameGone.as_str(), "GAME_GONE");
    assert_eq!(ClaimRejectCode::ClaimDenied.as_str(), "CLAIM_DENIED");
    assert_eq!(ClaimRejectCode::Auth.as_str(), "AUTH");
    assert_eq!(ClaimRejectCode::GameGone.as_str(), "GAME_GONE");
}

fn fixture_map() -> BTreeMap<String, Value> {
    let manifest = load_manifest();
    let mut map = BTreeMap::new();
    for msg in manifest.messages {
        map.insert(msg.id, load_fixture(&msg.file));
    }
    map
}
