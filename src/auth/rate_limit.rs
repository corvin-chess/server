//! Simple in-process OTP start throttle (CV-133).
//! Multi-node shared limits remain Planned with Redis.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(15 * 60);
const MAX_STARTS: usize = 5;

#[derive(Debug, Default)]
pub struct OtpStartLimiter {
    inner: Mutex<HashMap<String, Vec<Instant>>>,
}

impl OtpStartLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true when this email may receive another OTP start in the window.
    pub fn allow(&self, email: &str) -> bool {
        let key = email.trim().to_ascii_lowercase();
        let now = Instant::now();
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entries = guard.entry(key).or_default();
        entries.retain(|at| now.duration_since(*at) < WINDOW);
        if entries.len() >= MAX_STARTS {
            return false;
        }
        entries.push(now);
        true
    }
}
