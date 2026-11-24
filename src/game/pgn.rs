//! Standard tag-pair PGN export from durable game data (CV-169).

use std::fmt;

use chrono::{TimeZone, Utc};
use corvin_chess_core::{encode_movetext, Move, Position};

use super::events::{GameEvent, GameMeta};
use super::replay::net_move_events;
use super::store::GameLogError;

/// Errors building a PGN document.
#[derive(Debug)]
pub enum PgnExportError {
    InvalidFen(String),
    InvalidUci(String),
    IllegalMove(String),
    Encode(String),
    InvalidResult,
}

impl fmt::Display for PgnExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFen(detail) => write!(f, "invalid FEN: {detail}"),
            Self::InvalidUci(detail) => write!(f, "invalid UCI: {detail}"),
            Self::IllegalMove(uci) => write!(f, "illegal move: {uci}"),
            Self::Encode(detail) => write!(f, "PGN encode: {detail}"),
            Self::InvalidResult => write!(f, "invalid PGN result"),
        }
    }
}

impl std::error::Error for PgnExportError {}

impl From<GameLogError> for PgnExportError {
    fn from(value: GameLogError) -> Self {
        match value {
            GameLogError::InvalidFen(err) => Self::InvalidFen(err.to_string()),
            GameLogError::InvalidUci(err) => Self::InvalidUci(err.to_string()),
            GameLogError::IllegalMove(uci) => Self::IllegalMove(uci),
            other => Self::Encode(other.to_string()),
        }
    }
}

fn escape_tag(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn tag(name: &str, value: &str) -> String {
    format!("[{name} \"{}\"]", escape_tag(value))
}

fn normalize_result(result: &str) -> Result<&str, PgnExportError> {
    match result.trim() {
        "1-0" | "0-1" | "1/2-1/2" | "*" => Ok(result.trim()),
        _ => Err(PgnExportError::InvalidResult),
    }
}

fn event_name(meta: &GameMeta) -> String {
    let rated = if meta.rated { "Rated" } else { "Casual" };
    let class = meta.time_class.trim();
    let class = if class.is_empty() { "blitz" } else { class };
    let mut label = class.chars();
    let titled = match label.next() {
        Some(c) => format!("{}{}", c.to_ascii_uppercase(), label.as_str()),
        None => "Blitz".to_owned(),
    };
    format!("Corvin {rated} {titled}")
}

fn time_control(meta: &GameMeta) -> Option<String> {
    if meta.initial_ms == 0 && meta.increment_ms == 0 {
        return None;
    }
    let base = meta.initial_ms / 1000;
    let inc = meta.increment_ms / 1000;
    Some(format!("{base}+{inc}"))
}

fn termination(result: &str, reason: &str) -> String {
    let reason = reason.trim().to_ascii_lowercase();
    match reason.as_str() {
        "resign" => match result {
            "1-0" => "White wins by resignation".into(),
            "0-1" => "Black wins by resignation".into(),
            _ => "Resignation".into(),
        },
        "flag" => match result {
            "1-0" => "White wins on time".into(),
            "0-1" => "Black wins on time".into(),
            "1/2-1/2" => "Draw by timeout vs insufficient mating material".into(),
            _ => "Time forfeit".into(),
        },
        "agreement" => "Game drawn by agreement".into(),
        "abort" => "Game aborted".into(),
        "threefold" => "Draw by threefold repetition".into(),
        "fifty" => "Draw by fifty-move rule".into(),
        "fivefold" => "Draw by fivefold repetition".into(),
        "seventy_five" => "Draw by seventy-five-move rule".into(),
        "checkmate" => match result {
            "1-0" => "White wins by checkmate".into(),
            "0-1" => "Black wins by checkmate".into(),
            _ => "Checkmate".into(),
        },
        "stalemate" => "Draw by stalemate".into(),
        other if !other.is_empty() => format!("Game ended ({other})"),
        _ => "Game ended".into(),
    }
}

fn utc_parts(ms: u64) -> (String, String, String) {
    let secs = (ms / 1000) as i64;
    let dt = Utc
        .timestamp_opt(secs, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().expect("epoch"));
    (
        dt.format("%Y.%m.%d").to_string(),
        dt.format("%Y.%m.%d").to_string(),
        dt.format("%H:%M:%S").to_string(),
    )
}

/// Movetext from net Move events via chess-core (DoD: export matches encode).
pub fn movetext_from_events(
    initial_fen: &str,
    events: &[GameEvent],
) -> Result<String, PgnExportError> {
    let start = Position::from_fen(initial_fen)
        .map_err(|err| PgnExportError::InvalidFen(err.to_string()))?;
    let mut moves: Vec<Move> = Vec::new();
    for event in net_move_events(events) {
        let GameEvent::Move { uci, .. } = event else {
            continue;
        };
        let mv = Move::from_uci(uci).map_err(|err| PgnExportError::InvalidUci(err.to_string()))?;
        moves.push(mv);
    }
    encode_movetext(&start, &moves).map_err(|err| PgnExportError::Encode(err.to_string()))
}

/// Build a complete PGN document (tags + movetext + result).
pub fn render_pgn(
    meta: &GameMeta,
    events: &[GameEvent],
    white_username: &str,
    black_username: &str,
    result: &str,
    reason: &str,
    ended_ms: u64,
) -> Result<String, PgnExportError> {
    let result = normalize_result(result)?;
    let movetext = movetext_from_events(&meta.initial_fen, events)?;
    let (date, utc_date, utc_time) = utc_parts(if ended_ms > 0 {
        ended_ms
    } else {
        meta.created_ms
    });

    let mut lines: Vec<String> = Vec::with_capacity(16);
    lines.push(tag("Event", &event_name(meta)));
    lines.push(tag("Site", "Corvin"));
    lines.push(tag("Date", &date));
    lines.push(tag("White", white_username));
    lines.push(tag("Black", black_username));
    lines.push(tag("Result", result));
    lines.push(tag("UTCDate", &utc_date));
    lines.push(tag("UTCTime", &utc_time));
    if let Some(tc) = time_control(meta) {
        lines.push(tag("TimeControl", &tc));
    }
    lines.push(tag("Termination", &termination(result, reason)));
    if meta.rated {
        lines.push(tag("Rated", "true"));
    } else {
        lines.push(tag("Rated", "false"));
    }
    lines.push(tag("Variant", "Standard"));
    if meta.initial_fen != super::events::START_FEN {
        lines.push(tag("FEN", &meta.initial_fen));
        lines.push(tag("SetUp", "1"));
    }
    lines.push(String::new());
    if movetext.is_empty() {
        lines.push(result.to_owned());
    } else {
        lines.push(format!("{movetext} {result}"));
    }
    lines.push(String::new());
    Ok(lines.join("\n"))
}
