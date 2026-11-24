//! Age-appropriate messaging defaults (CV-140).
//!
//! Birth year alone is a coarse age signal: a user born in year Y is treated as
//! at least `current_year - Y` years old. Missing birth year is treated as minor
//! so DMs stay off until an adult age can be established.

use chrono::{Datelike, NaiveDate};

/// Calendar age at or above which messaging defaults may enable DMs / public chat.
/// Chosen as under-18 platform safety (DSA-style), not GDPR Art. 8 digital-consent age.
pub const MESSAGING_ADULT_AGE: u16 = 18;

/// Persisted messaging preference flags on `users`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessagingPrefs {
    pub dm_enabled: bool,
    pub public_chat_enabled: bool,
}

/// True when birth year is missing or estimated age is under [`MESSAGING_ADULT_AGE`].
pub fn is_minor_for_messaging(birth_year: Option<i16>, today: NaiveDate) -> bool {
    let Some(year) = birth_year else {
        return true;
    };
    let age = today.year() - i32::from(year);
    age < i32::from(MESSAGING_ADULT_AGE)
}

/// Defaults applied at registration (and backfill).
pub fn default_messaging_prefs(birth_year: Option<i16>, today: NaiveDate) -> MessagingPrefs {
    if is_minor_for_messaging(birth_year, today) {
        MessagingPrefs {
            dm_enabled: false,
            public_chat_enabled: false,
        }
    } else {
        MessagingPrefs {
            dm_enabled: true,
            public_chat_enabled: true,
        }
    }
}

/// Gate for future DM send paths (CV-281). Err when DMs are disabled.
pub fn assert_can_send_dm(prefs: &MessagingPrefs) -> Result<(), &'static str> {
    if prefs.dm_enabled {
        Ok(())
    } else {
        Err("dm disabled")
    }
}

/// Whether the account may turn messaging flags on (adults only).
pub fn can_enable_messaging(birth_year: Option<i16>, today: NaiveDate) -> bool {
    !is_minor_for_messaging(birth_year, today)
}
