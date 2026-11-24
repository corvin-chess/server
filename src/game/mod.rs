//! Live game state as an append-only Redis event log (CV-161 / ADR-004).
//!
//! Position is reconstructed by replaying `Move` events through chess-core.
//! Legal apply + PROTOCOL MoveRejected codes: CV-162.
//! Clocks + Fischer increment + flag: CV-163 / ADR-005.
//! Lag compensation quota from server RTT: CV-164.

mod apply;
mod clock;
mod events;
mod lag;
mod replay;
mod store;

pub use apply::{
    apply_move, apply_move_with_rtt, move_applied_json, move_rejected_json, AppliedMove,
    ApplyError, MoveRejectCode,
};
pub use clock::{
    check_flag, claim_flag, clock_sync_json, flag_outcome_for_position, game_end_flag_json,
    is_flagged_at, is_flagged_at_compensated, remaining_at, tick_on_move, tick_on_move_compensated,
    ClockPair, ClockTick, FlagClaimError, FlagOutcome,
};
pub use events::{GameEvent, GameMeta, MoveEvent, START_FEN};
pub use lag::{
    active_seat_credit_ms, credit_lag, effective_elapsed_ms, estimated_game_ms, initial_quota_ms,
    peek_credit_ms, seed_lag_quotas, LagCredit, LagTracker, ESTIMATE_INC_MOVES, QUOTA_CAP_MS,
    QUOTA_FLOOR_MS,
};
pub use replay::replay_fen;
pub use store::{
    append_move_event, create_game, delete_game, game_events_key, game_meta_key, load_events,
    load_meta, reconstruct_fen, save_meta, GameLogError, GAME_KEY_PREFIX, MAX_EVENTS, MAX_FEN_LEN,
    MAX_GAME_ID_LEN, MAX_PLAYER_ID_LEN, MAX_UCI_LEN,
};
