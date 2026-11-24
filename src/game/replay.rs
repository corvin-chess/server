//! Pure FEN reconstruction from an initial FEN + ordered events (chess-core).

use corvin_chess_core::{Move, Position};

use super::events::GameEvent;
use super::store::GameLogError;

/// Replay `events` from `initial_fen` and return the resulting FEN.
///
/// Only `GameEvent::Move` updates the board. Non-move events are skipped so
/// clock/offer notes can sit in the same log without changing position.
pub fn replay_fen(initial_fen: &str, events: &[GameEvent]) -> Result<String, GameLogError> {
    let mut pos = Position::from_fen(initial_fen).map_err(GameLogError::InvalidFen)?;
    for event in events {
        let GameEvent::Move { uci, .. } = event else {
            continue;
        };
        let mv = Move::from_uci(uci).map_err(GameLogError::InvalidUci)?;
        pos = pos
            .try_make_move(mv)
            .ok_or_else(|| GameLogError::IllegalMove(uci.clone()))?;
    }
    Ok(pos.to_fen())
}
