//! Live game state as an append-only Redis event log (CV-161 / ADR-004).
//!
//! Position is reconstructed by replaying `Move` events through chess-core.
//! Legal apply + PROTOCOL MoveRejected codes: CV-162.
//! Clocks + Fischer increment + flag: CV-163 / ADR-005.
//! Lag compensation quota from server RTT: CV-164.
//! Draw / takeback / resign / abort: CV-165.
//! Threefold / fifty claims + fivefold / seventy-five auto: CV-166.
//! Finished-game Postgres persist + PGN export: CV-169.
//! Atomic apply_move Lua/CAS: CV-171.

mod apply;
mod claims;
mod clock;
mod events;
mod lag;
mod offers;
mod persist;
mod pgn;
mod replay;
mod store;

pub use apply::{
    apply_move, apply_move_with_rtt, move_applied_json, move_rejected_json, AppliedMove,
    ApplyError, MoveRejectCode,
};
pub use claims::{
    auto_draw_reason, claim_denied_json, claim_draw, claimable_reason, game_end_claim_json,
    position_with_prior_keys, record_auto_draw, ClaimError, ClaimRejectCode,
};
pub use clock::{
    check_flag, claim_flag, clock_sync_json, flag_outcome_for_position, game_end_flag_json,
    is_flagged_at, is_flagged_at_compensated, remaining_at, tick_on_move, tick_on_move_compensated,
    ClockPair, ClockTick, FlagClaimError, FlagOutcome,
};
pub use events::{GameEvent, GameMeta, MoveEvent, PendingOffer, START_FEN};
pub use lag::{
    active_seat_credit_ms, credit_lag, effective_elapsed_ms, estimated_game_ms, initial_quota_ms,
    peek_credit_ms, seed_lag_quotas, LagCredit, LagTracker, ESTIMATE_INC_MOVES, QUOTA_CAP_MS,
    QUOTA_FLOOR_MS,
};
pub use offers::{
    abort, abort_allowed, abort_window_ms, game_end_offer_json, offer_draw, offer_error_json,
    offer_state_json, offer_takeback, resign, respond_offer, GameEndOut, OfferError, OfferOutcome,
    OfferRejectCode, OfferStateOut, OFFER_DRAW, OFFER_TAKEBACK,
};
pub use persist::{
    export_pgn, game_summary_json, persist_finished_from_redis, persist_finished_game,
    PersistError, PersistInput, PersistOutcome,
};
pub use pgn::{movetext_from_events, render_pgn, PgnExportError};
pub use replay::{net_move_events, replay_fen};
pub use store::{
    append_game_event, append_move_event, commit_apply_cas, commit_meta_cas, create_game,
    delete_game, game_events_key, game_meta_key, load_events, load_meta, parse_cas_commit_reply,
    reconstruct_fen, save_meta, CasCommitResult, GameLogError, GAME_KEY_PREFIX, MAX_EVENTS,
    MAX_EVENT_BYTES, MAX_FEN_LEN, MAX_GAME_ID_LEN, MAX_META_BYTES, MAX_PLAYER_ID_LEN, MAX_UCI_LEN,
};
