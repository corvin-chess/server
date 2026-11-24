//! Pure FEN reconstruction from an initial FEN + ordered events (chess-core).

use corvin_chess_core::{Move, Position};

use super::events::GameEvent;
use super::store::GameLogError;

/// Net Move events after applying Takeback pops (audit/non-move events ignored).
pub fn net_move_events(events: &[GameEvent]) -> Vec<&GameEvent> {
    let mut stack: Vec<&GameEvent> = Vec::new();
    for event in events {
        match event {
            GameEvent::Move { .. } => stack.push(event),
            GameEvent::Takeback { .. } => {
                let _ = stack.pop();
            }
            _ => {}
        }
    }
    stack
}

/// Replay `events` from `initial_fen` and return the resulting FEN.
///
/// Only net `GameEvent::Move` plies update the board. `Takeback` pops the last
/// net Move. Other event types are skipped so offers/resigns can share the log.
pub fn replay_fen(initial_fen: &str, events: &[GameEvent]) -> Result<String, GameLogError> {
    let mut pos = Position::from_fen(initial_fen).map_err(GameLogError::InvalidFen)?;
    for event in net_move_events(events) {
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
