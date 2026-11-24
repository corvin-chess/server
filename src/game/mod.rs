//! Live game state as an append-only Redis event log (CV-161 / ADR-004).
//!
//! Position is reconstructed by replaying `Move` events through chess-core.
//! Clock/offer apply and PROTOCOL move reject stay in later tasks (CV-162+).

mod events;
mod replay;
mod store;

pub use events::{GameEvent, GameMeta, MoveEvent, START_FEN};
pub use replay::replay_fen;
pub use store::{
    append_move_event, create_game, delete_game, game_events_key, game_meta_key, load_events,
    load_meta, reconstruct_fen, GameLogError, GAME_KEY_PREFIX, MAX_EVENTS, MAX_FEN_LEN,
    MAX_GAME_ID_LEN, MAX_UCI_LEN,
};
