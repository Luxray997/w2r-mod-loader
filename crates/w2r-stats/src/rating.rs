//! Skill ratings: the TrueSkill update for two teams (Herbrich, Minka, Graepel 2006), with the
//! win probability that team balancing and matchmaking will use. A rating is a normal belief
//! about skill, `mu` +/- `sigma`; a new player starts wide and narrows with every rated game.
//!
//! Scope: two teams of any size, a win or a draw. Free-for-all and three or more teams are not
//! rated yet. Nothing here knows about reports; [`ladder`] turns verified matches into updates.

use std::collections::BTreeMap;

use crate::history::Match;
use crate::report::Outcome;
use crate::slot::Kind;

pub const MU0: f64 = 25.0;
pub const SIGMA0: f64 = MU0 / 3.0;
pub const BETA: f64 = SIGMA0 / 2.0;
/// Skill drift per game, so a long-quiet rating can move again.
pub const TAU: f64 = SIGMA0 / 100.0;
pub const DRAW_PROBABILITY: f64 = 0.10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rating {
    pub mu: f64,
    pub sigma: f64,
}

impl Default for Rating {
    fn default() -> Self {
        Rating {
            mu: MU0,
            sigma: SIGMA0,
        }
    }
}

impl Rating {
    /// The number to sort and display by: what the player is very likely at least as good as.
    pub fn conservative(&self) -> f64 {
        self.mu - 3.0 * self.sigma
    }
}

// ---- normal distribution -------------------------------------------------------------------

fn erfc(x: f64) -> f64 {
    // Numerical Recipes erfc, Chebyshev fit, relative error below 1.2e-7.
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let r = t
        * (-z * z - 1.26551223
            + t * (1.00002368
                + t * (0.37409196
                    + t * (0.09678418
                        + t * (-0.18628806
                            + t * (0.27886807
                                + t * (-1.13520398
                                    + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))))
            .exp();
    if x >= 0.0 { r } else { 2.0 - r }
}

fn cdf(x: f64) -> f64 {
    0.5 * erfc(-x / core::f64::consts::SQRT_2)
}

fn pdf(x: f64) -> f64 {
    (-0.5 * x * x).exp() / (2.0 * core::f64::consts::PI).sqrt()
}

/// Inverse of [`cdf`] (Acklam's rational approximation).
#[allow(clippy::excessive_precision)]
fn inv_cdf(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.383577518672690e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    const LOW: f64 = 0.02425;
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    if p < LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - LOW {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

// ---- the update ----------------------------------------------------------------------------

/// Additive correction for a win by the first team: `v = pdf(t - e) / cdf(t - e)`.
fn v_win(t: f64, eps: f64) -> f64 {
    let x = t - eps;
    let d = cdf(x);
    if d < 1e-12 { -x } else { pdf(x) / d }
}

fn w_win(t: f64, eps: f64) -> f64 {
    let x = t - eps;
    let v = v_win(t, eps);
    (v * (v + x)).clamp(0.0, 1.0 - 1e-9)
}

fn v_draw(t: f64, eps: f64) -> f64 {
    let a = -eps - t;
    let b = eps - t;
    let d = cdf(b) - cdf(a);
    if d < 1e-12 {
        return if t < 0.0 { -t - eps } else { -t + eps };
    }
    (pdf(a) - pdf(b)) / d
}

fn w_draw(t: f64, eps: f64) -> f64 {
    let a = -eps - t;
    let b = eps - t;
    let d = cdf(b) - cdf(a);
    if d < 1e-12 {
        return 1.0 - 1e-9;
    }
    let v = v_draw(t, eps);
    (v * v + (b * pdf(b) - a * pdf(a)) / d).clamp(0.0, 1.0 - 1e-9)
}

/// What a finished game between two teams did to every player. `first` and `second` are the two
/// teams as they were rated before the game; with `draw` false the first team won.
pub fn update(first: &[Rating], second: &[Rating], draw: bool) -> (Vec<Rating>, Vec<Rating>) {
    if first.is_empty() || second.is_empty() {
        return (first.to_vec(), second.to_vec());
    }
    let drift = |r: &Rating| Rating {
        mu: r.mu,
        sigma: (r.sigma * r.sigma + TAU * TAU).sqrt(),
    };
    let a: Vec<Rating> = first.iter().map(drift).collect();
    let b: Vec<Rating> = second.iter().map(drift).collect();
    let n = (a.len() + b.len()) as f64;
    let c2: f64 = a
        .iter()
        .chain(b.iter())
        .map(|r| r.sigma * r.sigma + BETA * BETA)
        .sum();
    let c = c2.sqrt();
    let t = (a.iter().map(|r| r.mu).sum::<f64>() - b.iter().map(|r| r.mu).sum::<f64>()) / c;
    let eps = inv_cdf((DRAW_PROBABILITY + 1.0) / 2.0) * n.sqrt() * BETA / c;
    let (v, w) = if draw {
        (v_draw(t, eps), w_draw(t, eps))
    } else {
        (v_win(t, eps), w_win(t, eps))
    };
    let apply = |r: &Rating, sign: f64| {
        let s2 = r.sigma * r.sigma;
        Rating {
            mu: r.mu + sign * s2 / c * v,
            sigma: (s2 * (1.0 - s2 / c2 * w)).max(1e-6).sqrt(),
        }
    };
    (
        a.iter().map(|r| apply(r, 1.0)).collect(),
        b.iter().map(|r| apply(r, -1.0)).collect(),
    )
}

/// Chance that the first team beats the second, drawing aside.
pub fn win_probability(first: &[Rating], second: &[Rating]) -> f64 {
    let n = (first.len() + second.len()) as f64;
    let sum_mu = |t: &[Rating]| t.iter().map(|r| r.mu).sum::<f64>();
    let var: f64 = first
        .iter()
        .chain(second.iter())
        .map(|r| r.sigma * r.sigma)
        .sum();
    cdf((sum_mu(first) - sum_mu(second)) / (n * BETA * BETA + var).sqrt())
}

// ---- from matches to a ladder --------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub rating: Rating,
    pub games: u32,
}

#[derive(Debug, Default)]
pub struct Ladder {
    pub players: BTreeMap<String, Entry>,
    /// Matches that were not rated, with the reason, for the caller to show.
    pub skipped: Vec<(u64, &'static str)>,
}

/// Rates matches oldest first. A match counts when its reporter won or lost, it has exactly two
/// teams, and every player is a human (or `with_computers`, which rates computers by name; for
/// trying the maths on single-player history, not for a real ladder). A leave is not rated here:
/// whether it is a loss is a server policy (docs/stats-reporting.md).
pub fn ladder(matches: &[Match], with_computers: bool) -> Ladder {
    let mut out = Ladder::default();
    for m in matches {
        let Some(result) = &m.result else {
            out.skipped.push((m.started, "unfinished"));
            continue;
        };
        let me_won = match result.outcome {
            Outcome::Win => true,
            Outcome::Loss => false,
            Outcome::Left => {
                out.skipped.push((m.started, "left"));
                continue;
            }
            Outcome::Other(_) => {
                out.skipped.push((m.started, "unknown result"));
                continue;
            }
        };
        if !with_computers && m.players.iter().any(|p| p.kind == Kind::Computer) {
            out.skipped.push((m.started, "has computer players"));
            continue;
        }
        let mut teams: BTreeMap<u8, Vec<String>> = BTreeMap::new();
        for p in &m.players {
            let id = match p.kind {
                Kind::Human => p.identity().to_string(),
                Kind::Computer => Match::label(p),
            };
            teams.entry(p.team).or_default().push(id);
        }
        if teams.len() != 2 || teams.contains_key(&0) {
            out.skipped.push((m.started, "not two teams"));
            continue;
        }
        let mine = teams.remove(&m.me.team).unwrap_or_default();
        let theirs: Vec<String> = teams.into_values().flatten().collect();
        let (winners, losers) = if me_won {
            (mine, theirs)
        } else {
            (theirs, mine)
        };
        let get = |l: &Ladder, ids: &[String]| -> Vec<Rating> {
            ids.iter()
                .map(|id| l.players.get(id).map_or(Rating::default(), |e| e.rating))
                .collect()
        };
        let (w_new, l_new) = update(&get(&out, &winners), &get(&out, &losers), false);
        for (id, r) in winners.iter().zip(w_new).chain(losers.iter().zip(l_new)) {
            let e = out.players.entry(id.clone()).or_default();
            e.rating = r;
            e.games += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::pair;
    use crate::report::{Phase, Report, ResultInfo, setup_id};
    use crate::slot::Player;

    fn near(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "{a} vs {b}");
    }

    /// The same update without the drift term, to compare with published values.
    fn no_drift(first: &[Rating], second: &[Rating], draw: bool) -> (Vec<Rating>, Vec<Rating>) {
        // sigma is widened by tau first inside `update`; undo that for the check.
        let undo = |r: &Rating| Rating {
            mu: r.mu,
            sigma: (r.sigma * r.sigma - TAU * TAU).sqrt(),
        };
        let a: Vec<_> = first.iter().map(undo).collect();
        let b: Vec<_> = second.iter().map(undo).collect();
        update(&a, &b, draw)
    }

    #[test]
    fn normal_helpers() {
        near(cdf(0.0), 0.5, 1e-7);
        near(cdf(1.96), 0.975, 1e-4);
        near(inv_cdf(0.975), 1.96, 1e-3);
        near(inv_cdf(cdf(-1.3)), -1.3, 1e-4);
        near(pdf(0.0), 0.398942, 1e-5);
    }

    #[test]
    fn one_v_one_win_matches_the_published_numbers() {
        // Moserware's walkthrough: two new players, no drift; winner 29.396, loser 20.604, both
        // sigma 7.171.
        let p = Rating::default();
        let (w, l) = no_drift(&[p], &[p], false);
        near(w[0].mu, 29.396, 0.01);
        near(l[0].mu, 20.604, 0.01);
        near(w[0].sigma, 7.171, 0.01);
        near(l[0].sigma, 7.171, 0.01);
    }

    #[test]
    fn one_v_one_draw_keeps_mu_and_narrows_sigma() {
        let p = Rating::default();
        let (a, b) = no_drift(&[p], &[p], true);
        near(a[0].mu, 25.0, 1e-6);
        near(b[0].mu, 25.0, 1e-6);
        // A draw between equals is more informative than a win: sigma 6.457 by hand from the
        // formulas (w ~ 0.9989 at t = 0), against 7.171 for a win.
        near(a[0].sigma, 6.457, 0.01);
    }

    #[test]
    fn an_upset_moves_ratings_more_than_an_expected_win() {
        let strong = Rating {
            mu: 32.0,
            sigma: 3.0,
        };
        let weak = Rating {
            mu: 18.0,
            sigma: 3.0,
        };
        let (w1, _) = update(&[strong], &[weak], false);
        let (w2, _) = update(&[weak], &[strong], false);
        assert!(w2[0].mu - weak.mu > 5.0 * (w1[0].mu - strong.mu));
    }

    #[test]
    fn a_win_never_lowers_the_winner_or_raises_the_loser() {
        let a = Rating {
            mu: 20.0,
            sigma: 5.0,
        };
        let b = Rating {
            mu: 30.0,
            sigma: 2.0,
        };
        for (x, y) in [(a, b), (b, a)] {
            let (w, l) = update(&[x], &[y], false);
            assert!(w[0].mu > x.mu && l[0].mu < y.mu);
            assert!(w[0].sigma < x.sigma + TAU && l[0].sigma < y.sigma + TAU);
        }
    }

    #[test]
    fn team_games_update_every_member_and_uncertain_players_move_most() {
        let sure = Rating {
            mu: 25.0,
            sigma: 1.0,
        };
        let new = Rating::default();
        let (win, lose) = update(&[sure, new], &[sure, sure], false);
        let d_sure = win[0].mu - 25.0;
        let d_new = win[1].mu - 25.0;
        assert!(d_new > d_sure && d_sure > 0.0);
        assert!(lose.iter().all(|r| r.mu < 25.0));
    }

    #[test]
    fn win_probability_is_symmetric_and_ordered() {
        let a = Rating::default();
        near(win_probability(&[a], &[a]), 0.5, 1e-6);
        let hi = Rating {
            mu: 30.0,
            sigma: 2.0,
        };
        let lo = Rating {
            mu: 20.0,
            sigma: 2.0,
        };
        let p = win_probability(&[hi], &[lo]);
        near(p + win_probability(&[lo], &[hi]), 1.0, 1e-6);
        assert!(p > 0.9);
    }

    fn human(slot: u8, name: &str, team: u8) -> Player {
        Player {
            slot,
            kind: Kind::Human,
            name: name.into(),
            tag: Some(format!("{name}#1")),
            team,
            race: 0,
        }
    }

    fn matches_for(roster: Vec<Player>, me: u8, results: &[(u64, u8, u32)]) -> Vec<Match> {
        let mut reports = Vec::new();
        for &(started, code, turns) in results {
            reports.push((
                [1u8; 32],
                Report {
                    phase: Phase::Result,
                    rid: [0; 8],
                    setup: setup_id("m", &roster),
                    started,
                    client: "t".into(),
                    fingerprint: "00000000".into(),
                    map: "m".into(),
                    me,
                    players: roster.clone(),
                    result: Some(ResultInfo {
                        outcome: Outcome::from_code(code, turns).unwrap(),
                        code,
                        turns,
                        sync_mismatch: 0,
                    }),
                },
            ));
        }
        pair(reports)
    }

    #[test]
    fn the_ladder_rates_human_games_and_skips_the_rest() {
        let roster = vec![human(0, "Ann", 1), human(1, "Bob", 2)];
        let m = matches_for(roster, 0, &[(1, 3, 900), (2, 3, 900), (3, 0, 50)]);
        let l = ladder(&m, false);
        // Ann won twice; the leave is skipped, not rated.
        assert_eq!(l.players["Ann#1"].games, 2);
        assert_eq!(l.skipped, vec![(3, "left")]);
        assert!(l.players["Ann#1"].rating.mu > 30.0);
        assert!(l.players["Bob#1"].rating.mu < 20.0);
        assert!(l.players["Ann#1"].rating.sigma < SIGMA0);
    }

    #[test]
    fn an_upset_in_the_ladder_pulls_the_ratings_back_together() {
        let roster = vec![human(0, "Ann", 1), human(1, "Bob", 2)];
        let m = matches_for(roster, 0, &[(1, 3, 900), (2, 3, 900), (3, 2, 900)]);
        let l = ladder(&m, false);
        let (ann, bob) = (l.players["Ann#1"].rating.mu, l.players["Bob#1"].rating.mu);
        assert!((ann - bob).abs() < 3.0, "{ann} {bob}");
    }

    #[test]
    fn computers_are_not_rated_unless_asked() {
        let mut bot = human(1, "Azeroth", 2);
        bot.kind = Kind::Computer;
        bot.tag = None;
        let m = matches_for(vec![human(0, "Ann", 1), bot], 0, &[(1, 3, 900)]);
        assert!(ladder(&m, false).players.is_empty());
        assert_eq!(ladder(&m, true).players.len(), 2);
    }

    #[test]
    fn three_teams_are_not_rated_yet() {
        let m = matches_for(
            vec![human(0, "A", 1), human(1, "B", 2), human(2, "C", 3)],
            0,
            &[(1, 3, 900)],
        );
        let l = ladder(&m, false);
        assert!(l.players.is_empty());
        assert_eq!(l.skipped, vec![(1, "not two teams")]);
    }
}
