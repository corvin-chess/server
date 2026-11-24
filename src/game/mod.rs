//! Live game state as an append-only Redis event log (CV-161 / ADR-004).
//!
//! Position is reconstructed by replaying `Move` events through chess-core.
//! Legal apply + PROTOCOL MoveRejected codes: CV-162.
//! Clocks + Fischer increment + flag: CV-163 / ADR-005. Lag quota: CV-164.

mod apply;
mod clock;
mod events;
mod replay;
mod store;

pub use apply::{
    apply_move, move_applied_json, move_rejected_json, AppliedMove, ApplyError, MoveRejectCode,
};
pub use clock::{
    check_flag, claim_flag, clock_sync_json, flag_outcome_for_position, game_end_flag_json,
    is_flagged_at, remaining_at, tick_on_move, ClockPair, ClockTick, FlagClaimError, FlagOutcome,
};
pub use events::{GameEvent, GameMeta, MoveEvent, START_FEN};
pub use replay::replay_fen;
pub use store::{
    append_move_event, create_game, delete_game, game_events_key, game_meta_key, load_events,
    load_meta, reconstruct_fen, save_meta, GameLogError, GAME_KEY_PREFIX, MAX_EVENTS, MAX_FEN_LEN,
    MAX_GAME_ID_LEN, MAX_PLAYER_ID_LEN, MAX_UCI_LEN,
};
