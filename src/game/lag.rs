//! Lag compensation quota from server-measured RTT (CV-164 / ADR-005).
//!
//! Never trust client-declared send times. Interim constants are published here
//! and will be remeasured under CV-305 (LAN/WAN clock accuracy).

/// Moves assumed when estimating game length (`initial + 40 × increment`).
pub const ESTIMATE_INC_MOVES: u64 = 40;

/// Interim quota floor (ms). Short time controls still get a small spike buffer.
pub const QUOTA_FLOOR_MS: u64 = 200;

/// Interim quota cap (ms). Exact fairness bounds are **to be measured** (CV-305).
pub const QUOTA_CAP_MS: u64 = 1_000;

/// `estimated_game_ms / QUOTA_DIVISOR` before clamp.
const QUOTA_DIVISOR: u64 = 60;

/// EWMA weight numerator for new RTT samples (`α = 1/4`).
const EWMA_NUM: u64 = 1;
/// EWMA weight denominator (`α = 1/4`).
const EWMA_DEN: u64 = 4;

/// Result of taking lag credit from a seat's remaining quota.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LagCredit {
    pub credit_ms: u64,
    pub remaining_quota_ms: u64,
}

/// Smoothed RTT tracker for one connection (WS Ping/Pong samples).
#[derive(Clone, Debug, Default)]
pub struct LagTracker {
    ewma_rtt_ms: Option<u64>,
}

impl LagTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe a server-measured round-trip (ms). Ignores absurd samples (>60s).
    pub fn observe_rtt(&mut self, rtt_ms: u64) {
        if rtt_ms == 0 || rtt_ms > 60_000 {
            return;
        }
        self.ewma_rtt_ms = Some(match self.ewma_rtt_ms {
            None => rtt_ms,
            Some(prev) => {
                // Integer EWMA: (α*sample + (1-α)*prev) with α=1/4.
                (EWMA_NUM * rtt_ms + (EWMA_DEN - EWMA_NUM) * prev) / EWMA_DEN
            }
        });
    }

    pub fn ewma_rtt_ms(&self) -> Option<u64> {
        self.ewma_rtt_ms
    }

    /// One-way estimate = EWMA RTT / 2 (display + compensation input).
    pub fn one_way_ms(&self) -> Option<u64> {
        self.ewma_rtt_ms.map(|rtt| rtt / 2)
    }
}

/// Estimated game length in ms: `initial + 40 × increment` (ADR-005).
pub fn estimated_game_ms(initial_ms: u64, increment_ms: u64) -> u64 {
    initial_ms.saturating_add(increment_ms.saturating_mul(ESTIMATE_INC_MOVES))
}

/// Interim per-seat lag quota for a time control (ms).
///
/// `clamp(estimated / 60, 200, 1000)`; zero clocks → zero quota.
pub fn initial_quota_ms(initial_ms: u64, increment_ms: u64) -> u64 {
    let est = estimated_game_ms(initial_ms, increment_ms);
    if est == 0 {
        return 0;
    }
    (est / QUOTA_DIVISOR).clamp(QUOTA_FLOOR_MS, QUOTA_CAP_MS)
}

/// Credit `min(one_way, remaining_quota)` and return the depleted remainder.
pub fn credit_lag(one_way_ms: u64, remaining_quota_ms: u64) -> LagCredit {
    let credit_ms = one_way_ms.min(remaining_quota_ms);
    LagCredit {
        credit_ms,
        remaining_quota_ms: remaining_quota_ms.saturating_sub(credit_ms),
    }
}

/// Peek credit for flag checks without depleting quota (uses last one-way sample).
pub fn peek_credit_ms(one_way_ms: u64, remaining_quota_ms: u64) -> u64 {
    credit_lag(one_way_ms, remaining_quota_ms).credit_ms
}

/// Lag credit for the side whose clock is running, from meta samples (no deplete).
pub fn active_seat_credit_ms(meta: &super::events::GameMeta, active: &str) -> u64 {
    match active {
        "w" => peek_credit_ms(meta.lag_one_way_w_ms, meta.lag_quota_w_ms),
        "b" => peek_credit_ms(meta.lag_one_way_b_ms, meta.lag_quota_b_ms),
        _ => 0,
    }
}

/// Wall elapsed minus lag credit (never negative).
pub fn effective_elapsed_ms(elapsed_ms: u64, credit_ms: u64) -> u64 {
    elapsed_ms.saturating_sub(credit_ms)
}

/// Seed both seats' lag quotas from each side's starting clock + shared increment.
pub fn seed_lag_quotas(meta: &mut super::events::GameMeta) {
    meta.lag_quota_w_ms = initial_quota_ms(meta.clock_w_ms, meta.increment_ms);
    meta.lag_quota_b_ms = initial_quota_ms(meta.clock_b_ms, meta.increment_ms);
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn observe_rtt_rejects_zero_and_huge() {
        let mut t = LagTracker::new();
        t.observe_rtt(0);
        assert_eq!(t.ewma_rtt_ms(), None);
        t.observe_rtt(60_001);
        assert_eq!(t.ewma_rtt_ms(), None);
        t.observe_rtt(50);
        assert_eq!(t.ewma_rtt_ms(), Some(50));
    }
}
