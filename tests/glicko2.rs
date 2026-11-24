//! CV-168: Glicko-2 math matches Glickman paper + ADR-007 defaults.

use corvin_server::rating::{
    glicko2_update, is_provisional, Rating, INITIAL_R, INITIAL_RD, INITIAL_VOLATILITY, TAU,
};

#[test]
fn adr_007_initial_and_tau() {
    assert!((INITIAL_R - 1500.0).abs() < f64::EPSILON);
    assert!((INITIAL_RD - 350.0).abs() < f64::EPSILON);
    assert!((INITIAL_VOLATILITY - 0.06).abs() < f64::EPSILON);
    assert!((TAU - 0.75).abs() < f64::EPSILON);
    let unrated = Rating::unrated();
    assert!((unrated.r - 1500.0).abs() < f64::EPSILON);
    assert!((unrated.rd - 350.0).abs() < f64::EPSILON);
    assert!((unrated.sigma - 0.06).abs() < f64::EPSILON);
}

#[test]
fn provisional_when_rd_at_least_110() {
    assert!(is_provisional(110.0));
    assert!(is_provisional(350.0));
    assert!(!is_provisional(109.9));
    assert!(!is_provisional(50.0));
}

/// Official worked example from https://www.glicko.net/glicko/glicko2.pdf (τ = 0.5).
#[test]
fn glickman_paper_example_tau_half() {
    let player = Rating {
        r: 1500.0,
        rd: 200.0,
        sigma: 0.06,
    };
    let opponents = [
        (
            Rating {
                r: 1400.0,
                rd: 30.0,
                sigma: 0.06,
            },
            1.0,
        ),
        (
            Rating {
                r: 1550.0,
                rd: 100.0,
                sigma: 0.06,
            },
            0.0,
        ),
        (
            Rating {
                r: 1700.0,
                rd: 300.0,
                sigma: 0.06,
            },
            0.0,
        ),
    ];
    let updated = glicko2_update(&player, &opponents, 0.5);
    assert!((updated.r - 1464.06).abs() < 0.05, "r={}", updated.r);
    assert!((updated.rd - 151.52).abs() < 0.05, "rd={}", updated.rd);
    assert!(
        (updated.sigma - 0.05999).abs() < 0.0001,
        "sigma={}",
        updated.sigma
    );
}

#[test]
fn single_game_win_raises_rating_with_adr_tau() {
    let a = Rating::unrated();
    let b = Rating::unrated();
    let after = glicko2_update(&a, &[(b, 1.0)], TAU);
    assert!(after.r > a.r, "winner rating should rise");
    assert!(after.rd < a.rd, "RD should shrink after a game");
}

#[test]
fn draw_keeps_equal_players_near_start() {
    let a = Rating {
        r: 1500.0,
        rd: 50.0,
        sigma: 0.06,
    };
    let b = a;
    let after = glicko2_update(&a, &[(b, 0.5)], TAU);
    assert!((after.r - 1500.0).abs() < 1.0, "r={}", after.r);
}
