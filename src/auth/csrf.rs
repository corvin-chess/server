//! CSRF synchronizer helpers re-exported from session (CV-134).
//!
//! Cookie-authenticated state-changing HTTP requires the `X-CSRF-Token` header
//! to match the session row's `csrf_secret`. Auth start/verify routes stay
//! exempt because they mint a session rather than mutate under one.

pub use super::session::{csrf_matches, csrf_token_hex, CSRF_HEADER};
