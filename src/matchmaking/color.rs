//! Seat color assignment from PROTOCOL color preferences.

use serde::{Deserialize, Serialize};

/// Creator / seeker color preference (`random` | `white` | `black`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorPref {
    Random,
    White,
    Black,
}

impl ColorPref {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::White => "white",
            Self::Black => "black",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "random" => Some(Self::Random),
            "white" => Some(Self::White),
            "black" => Some(Self::Black),
            _ => None,
        }
    }
}

/// Assign White/Black seats. `None` when both demand the same exclusive color, or same player.
///
/// `coin` is the low bit of a server nonce used only when both prefs are `random`.
pub fn assign_colors(
    a_id: &str,
    a_pref: ColorPref,
    b_id: &str,
    b_pref: ColorPref,
    coin: bool,
) -> Option<(String, String)> {
    if a_id.is_empty() || b_id.is_empty() || a_id == b_id {
        return None;
    }
    if a_pref == ColorPref::White && b_pref == ColorPref::White {
        return None;
    }
    if a_pref == ColorPref::Black && b_pref == ColorPref::Black {
        return None;
    }

    // Honor an exclusive white request first.
    if a_pref == ColorPref::White {
        return Some((a_id.to_owned(), b_id.to_owned()));
    }
    if b_pref == ColorPref::White {
        return Some((b_id.to_owned(), a_id.to_owned()));
    }

    // Honor an exclusive black request (other seat is white).
    if a_pref == ColorPref::Black {
        return Some((b_id.to_owned(), a_id.to_owned()));
    }
    if b_pref == ColorPref::Black {
        return Some((a_id.to_owned(), b_id.to_owned()));
    }

    // Both random.
    if coin {
        Some((a_id.to_owned(), b_id.to_owned()))
    } else {
        Some((b_id.to_owned(), a_id.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_white_incompatible() {
        assert!(assign_colors("a", ColorPref::White, "b", ColorPref::White, true).is_none());
    }

    #[test]
    fn white_vs_black() {
        let (w, b) = assign_colors("a", ColorPref::White, "b", ColorPref::Black, true).unwrap();
        assert_eq!((w.as_str(), b.as_str()), ("a", "b"));
        let (w, b) = assign_colors("a", ColorPref::Black, "b", ColorPref::White, false).unwrap();
        assert_eq!((w.as_str(), b.as_str()), ("b", "a"));
    }

    #[test]
    fn both_random_uses_coin() {
        let (w, _) = assign_colors("a", ColorPref::Random, "b", ColorPref::Random, true).unwrap();
        assert_eq!(w, "a");
        let (w, _) = assign_colors("a", ColorPref::Random, "b", ColorPref::Random, false).unwrap();
        assert_eq!(w, "b");
    }
}
