//! Pure Glicko-2 (Mark Glickman, https://www.glicko.net/glicko/glicko2.pdf).
//!
//! ADR-007: initial r=1500, RD=350, σ=0.06; τ=0.75 (CV-220 may retune).
//! Color score weighting stays out of scope (CV-221).

use std::f64::consts::PI;

/// Glicko scale conversion constant.
pub const SCALE: f64 = 173.7178;

/// Unrated starting rating (Glickman Step 1a / ADR-007).
pub const INITIAL_R: f64 = 1500.0;

/// Unrated starting RD.
pub const INITIAL_RD: f64 = 350.0;

/// Unrated starting volatility.
pub const INITIAL_VOLATILITY: f64 = 0.06;

/// System constant τ (ADR-007 starting guess; predictive calibration is CV-220).
pub const TAU: f64 = 0.75;

/// Provisional badge when RD ≥ this (ADR-007 / RESEARCH).
pub const PROVISIONAL_RD: f64 = 110.0;

const VOL_EPS: f64 = 0.000_001;

/// Glicko-scale rating triple.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rating {
    pub r: f64,
    pub rd: f64,
    pub sigma: f64,
}

impl Rating {
    pub fn unrated() -> Self {
        Self {
            r: INITIAL_R,
            rd: INITIAL_RD,
            sigma: INITIAL_VOLATILITY,
        }
    }
}

pub fn is_provisional(rd: f64) -> bool {
    rd >= PROVISIONAL_RD
}

fn g(phi: f64) -> f64 {
    1.0 / (1.0 + 3.0 * phi * phi / (PI * PI)).sqrt()
}

fn e(mu: f64, mu_j: f64, phi_j: f64) -> f64 {
    1.0 / (1.0 + (-g(phi_j) * (mu - mu_j)).exp())
}

fn to_mu(r: f64) -> f64 {
    (r - INITIAL_R) / SCALE
}

fn to_phi(rd: f64) -> f64 {
    rd / SCALE
}

fn from_mu(mu: f64) -> f64 {
    mu * SCALE + INITIAL_R
}

fn from_phi(phi: f64) -> f64 {
    phi * SCALE
}

/// Update one player against a list of `(opponent, score)` where score is 1 / 0.5 / 0.
///
/// `tau` is the system constant (use [`TAU`] in production; paper example uses 0.5).
pub fn glicko2_update(player: &Rating, results: &[(Rating, f64)], tau: f64) -> Rating {
    if results.is_empty() {
        // Inactivity: only Step 6 with existing σ (φ* = sqrt(φ² + σ²)).
        let phi = to_phi(player.rd);
        let phi_star = (phi * phi + player.sigma * player.sigma).sqrt();
        return Rating {
            r: player.r,
            rd: from_phi(phi_star),
            sigma: player.sigma,
        };
    }

    let mu = to_mu(player.r);
    let phi = to_phi(player.rd);
    let sigma = player.sigma;

    let mut v_inv = 0.0;
    let mut delta_sum = 0.0;
    for (opp, score) in results {
        let mu_j = to_mu(opp.r);
        let phi_j = to_phi(opp.rd);
        let g_j = g(phi_j);
        let e_j = e(mu, mu_j, phi_j);
        v_inv += g_j * g_j * e_j * (1.0 - e_j);
        delta_sum += g_j * (score - e_j);
    }
    let v = 1.0 / v_inv;
    let delta = v * delta_sum;

    let a = (sigma * sigma).ln();
    let tau2 = tau * tau;

    let f = |x: f64| -> f64 {
        let ex = x.exp();
        let num = ex * (delta * delta - phi * phi - v - ex);
        let den = 2.0 * (phi * phi + v + ex).powi(2);
        num / den - (x - a) / tau2
    };

    let mut a_iter = a;
    let mut b_iter = if delta * delta > phi * phi + v {
        (delta * delta - phi * phi - v).ln()
    } else {
        let mut k = 1.0;
        while f(a - k * tau) < 0.0 {
            k += 1.0;
            // Bound search; paper notes k is almost always 1–2.
            if k > 100.0 {
                break;
            }
        }
        a - k * tau
    };

    let mut f_a = f(a_iter);
    let mut f_b = f(b_iter);

    while (b_iter - a_iter).abs() > VOL_EPS {
        let c = a_iter + (a_iter - b_iter) * f_a / (f_b - f_a);
        let f_c = f(c);
        if f_c * f_b <= 0.0 {
            a_iter = b_iter;
            f_a = f_b;
        } else {
            f_a /= 2.0;
        }
        b_iter = c;
        f_b = f_c;
    }

    let sigma0 = (a_iter / 2.0).exp();
    let phi_star = (phi * phi + sigma0 * sigma0).sqrt();
    let phi0 = 1.0 / (1.0 / (phi_star * phi_star) + 1.0 / v).sqrt();
    let mu0 = mu + phi0 * phi0 * delta_sum;

    Rating {
        r: from_mu(mu0),
        rd: from_phi(phi0),
        sigma: sigma0,
    }
}
