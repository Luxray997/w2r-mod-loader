//! From several players' reports to one verified game and its rating changes. Pure: rosters and
//! votes in, a verdict out, so every rule here is tested natively; `app` only moves rows.
//!
//! The policy while few players run the mod is **lenient**: one reporter's account of a game is
//! enough to rate it, because most opponents will not be reporting at all, and nobody is assumed to
//! be cheating yet. Two things make that safe enough:
//!
//! - when both sides do report and agree, the game is `corroborated`, and every key that took part
//!   in such games earns weight (`trust`);
//! - when reports contradict each other, the side whose reporters have the better track record
//!   wins (`TRUST_RATIO`), otherwise the game is `disputed` and stays out of the ladder.
//!
//! Policy that can be changed in one place:
//! - `MIN_TURNS`: a result from a game shorter than this decides nothing (an aborted start).
//! - a player who leaves counts as having lost only when they were the only human on their side,
//!   because in a team game one person leaving does not end it, and only when the game's score
//!   tables show something was fought;
//! - a game with no kills and no razed buildings is not rated at all.

use std::collections::{BTreeMap, BTreeSet};

use w2r_formats::json::{self, Number, Value};
use w2r_stats::rating::{self, Rating};
use w2r_stats::slot::{Kind, OBSERVER_TEAM, Player};

/// A win or loss reported from fewer turns than this decides nothing.
pub const MIN_TURNS: u32 = 600;
/// In a dispute, one side's backers must weigh at least this many times the other's to win it.
pub const TRUST_RATIO: u32 = 3;
/// The weight of an allowlisted key: more than any track record.
const TRUSTED_WEIGHT: u32 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not enough agreement to count (yet).
    Unverified,
    /// Both sides agree, or a trusted key vouches for it.
    Verified,
    /// Reports contradict each other.
    Disputed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Unverified => "unverified",
            Status::Verified => "verified",
            Status::Disputed => "disputed",
        }
    }
}

/// One reporter's account of the game, taken from a stored `result` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vote {
    pub key: String,
    pub tag: String,
    pub team: u8,
    /// `win`, `loss`, `left` or `other`.
    pub outcome: String,
    pub turns: u32,
    pub sync_mismatch: u32,
    /// Whether anything was fought (a kill or a razed building), from the game's score tables;
    /// `None` for reports from clients that do not send them.
    pub combat: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub status: Status,
    pub winner_team: Option<u8>,
    /// Median turn count of the reports that back the winner.
    pub turns: Option<u32>,
    /// Why the game has this status, a fixed phrase.
    pub reason: &'static str,
    /// Both sides reported and agree. Only then do the reporters earn trust.
    pub corroborated: bool,
    /// Whether the game goes into the ratings: accepted, humans only, two sides.
    pub rated: bool,
}

fn unverified(reason: &'static str) -> Verdict {
    Verdict {
        status: Status::Unverified,
        winner_team: None,
        turns: None,
        reason,
        corroborated: false,
        rated: false,
    }
}

/// The non-observer teams of a roster.
fn teams(roster: &[Player]) -> BTreeSet<u8> {
    roster
        .iter()
        .filter(|p| p.team != OBSERVER_TEAM)
        .map(|p| p.team)
        .collect()
}

fn humans_on(roster: &[Player], team: u8) -> usize {
    roster
        .iter()
        .filter(|p| p.kind == Kind::Human && p.team == team)
        .count()
}

/// Who won according to one vote, or `None` when it says nothing about the winner.
fn claim(roster: &[Player], vote: &Vote, other_team: u8) -> Option<u8> {
    if vote.sync_mismatch != 0 || vote.turns < MIN_TURNS {
        return None;
    }
    // A game where nobody killed anything is not a contest, whoever left or won.
    if vote.combat == Some(false) {
        return None;
    }
    match vote.outcome.as_str() {
        "win" => Some(vote.team),
        "loss" => Some(other_team),
        // Leaving ends the game for the leaver's side only when nobody else is playing on it.
        // Only with proof of fighting; a report that cannot say (older clients) is not enough.
        "left" if humans_on(roster, vote.team) == 1 && vote.combat == Some(true) => Some(other_team),
        _ => None,
    }
}

/// What a key's word is worth in a dispute: 1, plus one for each corroborated game it took part in
/// (`trust`), or far more for an allowlisted key.
fn weight(key: &str, trusted: &BTreeSet<String>, trust: &BTreeMap<String, u32>) -> u32 {
    if trusted.contains(key) {
        TRUSTED_WEIGHT
    } else {
        1 + trust.get(key).copied().unwrap_or(0)
    }
}

/// Decides a game from its roster and the votes of its reporters. `trusted` is the set of
/// allowlisted keys; `trust` counts each key's corroborated games.
pub fn judge(
    roster: &[Player],
    votes: &[Vote],
    trusted: &BTreeSet<String>,
    trust: &BTreeMap<String, u32>,
) -> Verdict {
    let sides = teams(roster);
    if sides.len() != 2 {
        return unverified("not two teams");
    }
    let (a, b) = (*sides.iter().next().unwrap(), *sides.iter().nth(1).unwrap());
    let other = |t: u8| if t == a { b } else { a };

    // Only reporters that are in the roster, under the tag and team the roster gives them.
    let valid: Vec<&Vote> = votes
        .iter()
        .filter(|v| {
            roster
                .iter()
                .any(|p| p.kind == Kind::Human && p.tag.as_deref() == Some(&v.tag) && p.team == v.team)
        })
        .collect();
    if valid.is_empty() {
        return unverified("no reporter in the roster");
    }
    let claims: Vec<(&Vote, u8)> = valid
        .iter()
        .filter_map(|v| claim(roster, v, other(v.team)).map(|w| (*v, w)))
        .collect();
    if claims.is_empty() {
        return unverified("no result claimed (left, too short or out of sync)");
    }
    let winners: BTreeSet<u8> = claims.iter().map(|(_, w)| *w).collect();
    // The weight behind each candidate winner: each key counts once.
    let backing_of = |w: u8| -> BTreeMap<&str, &Vote> {
        let mut m = BTreeMap::new();
        for (v, claimed) in &claims {
            if *claimed == w {
                m.entry(v.key.as_str()).or_insert(*v);
            }
        }
        m
    };
    let mut settled = false;
    let winner = if winners.len() > 1 {
        let mut weights: Vec<(u32, u8)> = winners
            .iter()
            .map(|w| {
                let total = backing_of(*w)
                    .keys()
                    .map(|k| weight(k, trusted, trust))
                    .fold(0u32, u32::saturating_add);
                (total, *w)
            })
            .collect();
        weights.sort_unstable_by(|x, y| y.cmp(x));
        let (top, second) = (weights[0], weights[1]);
        if top.0 >= second.0.saturating_mul(TRUST_RATIO) {
            settled = true;
            top.1
        } else {
            return Verdict {
                status: Status::Disputed,
                winner_team: None,
                turns: None,
                reason: "reports disagree on the winner",
                corroborated: false,
                rated: false,
            };
        }
    } else {
        *winners.iter().next().unwrap()
    };
    let backing = backing_of(winner);
    let from_winners = backing.values().any(|v| v.team == winner);
    let from_losers = backing.values().any(|v| v.team != winner);
    let corroborated = from_winners && from_losers && !settled;
    let vouched = backing.keys().any(|k| trusted.contains(*k));
    let mut turns: Vec<u32> = backing.values().map(|v| v.turns).collect();
    turns.sort_unstable();
    let all_human = roster.iter().all(|p| p.kind == Kind::Human || p.team == OBSERVER_TEAM);
    Verdict {
        status: Status::Verified,
        winner_team: Some(winner),
        turns: Some(turns[turns.len() / 2]),
        reason: if settled {
            "settled by the reporters' track record"
        } else if corroborated {
            "both sides agree"
        } else if vouched {
            "vouched for by a trusted key"
        } else {
            "single report, accepted on trust"
        },
        corroborated,
        rated: all_human,
    }
}

// ---- ratings -------------------------------------------------------------------------------

/// What one player's rating did in one game.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub tag: String,
    pub won: bool,
    pub before: Rating,
    pub after: Rating,
}

/// A player's standing, as stored.
#[derive(Clone, Debug, PartialEq)]
pub struct Standing {
    pub rating: Rating,
    pub games: u32,
    pub wins: u32,
    pub losses: u32,
}

impl Default for Standing {
    fn default() -> Standing {
        Standing {
            rating: Rating::default(),
            games: 0,
            wins: 0,
            losses: 0,
        }
    }
}

/// Applies a rated game to `standings` and returns each player's change. Observers and
/// computers are not rated; a roster that is not two sides changes nothing.
pub fn apply(
    standings: &mut BTreeMap<String, Standing>,
    roster: &[Player],
    winner_team: u8,
    military: &BTreeMap<u8, u32>,
) -> Vec<Change> {
    let mut winners: Vec<String> = Vec::new();
    let mut losers: Vec<String> = Vec::new();
    let mut w_mil: Vec<Option<u32>> = Vec::new();
    let mut l_mil: Vec<Option<u32>> = Vec::new();
    for p in roster {
        let (Kind::Human, Some(tag)) = (p.kind, p.tag.as_ref()) else {
            continue;
        };
        if p.team == OBSERVER_TEAM {
            continue;
        }
        if p.team == winner_team {
            winners.push(tag.clone());
            w_mil.push(military.get(&p.slot).copied());
        } else {
            losers.push(tag.clone());
            l_mil.push(military.get(&p.slot).copied());
        }
    }
    if winners.is_empty() || losers.is_empty() {
        return Vec::new();
    }
    let get = |s: &BTreeMap<String, Standing>, tags: &[String]| -> Vec<Rating> {
        tags.iter()
            .map(|t| s.get(t).map_or(Rating::default(), |e| e.rating))
            .collect()
    };
    let (w_before, l_before) = (get(standings, &winners), get(standings, &losers));
    // A player who did little of the fighting moves less than the one who carried the game.
    let (w_after, l_after) = rating::update_weighted(
        &w_before,
        &l_before,
        false,
        &rating::contribution_weights(&w_mil),
        &rating::contribution_weights(&l_mil),
    );
    let mut out = Vec::new();
    for (tags, before, after, won) in [
        (&winners, &w_before, &w_after, true),
        (&losers, &l_before, &l_after, false),
    ] {
        for ((tag, b), a) in tags.iter().zip(before).zip(after) {
            let e = standings.entry(tag.clone()).or_default();
            e.rating = *a;
            e.games += 1;
            if won {
                e.wins += 1;
            } else {
                e.losses += 1;
            }
            out.push(Change {
                tag: tag.clone(),
                won,
                before: *b,
                after: *a,
            });
        }
    }
    out
}

/// Each slot's military score (units killed plus buildings razed) from a game's stored score
/// table, a JSON array of `{"slot":..,"kills":..,"razed":..,..}`. Empty when there is none.
pub fn military_by_slot(score_json: &str) -> BTreeMap<u8, u32> {
    let Some(doc) = json::parse(score_json.as_bytes()).ok() else {
        return BTreeMap::new();
    };
    let field = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_number)
            .and_then(Number::as_u64)
            .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
    };
    doc.as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(|v| {
            let slot = u8::try_from(v.get("slot")?.as_number()?.as_u64()?).ok()?;
            Some((slot, field(v, "kills").saturating_add(field(v, "razed"))))
        })
        .collect()
}

// ---- grouping reports into games ---------------------------------------------------------------

/// Reports of one game start within this many seconds of each other (their clocks differ a little,
/// and each one is taken at that player's first turn).
pub const SAME_GAME_SECS: u64 = 300;

/// A report that is not yet part of a game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loose {
    pub id: i64,
    pub setup: String,
    pub started: u64,
}

/// A game that already exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub id: i64,
    pub setup: String,
    pub started: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Existing(i64),
    /// Index into [`Plan::new_games`].
    New(usize),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// `(setup, started)` of games to create; `started` is the earliest report in it.
    pub new_games: Vec<(String, u64)>,
    /// Where each loose report goes.
    pub placed: Vec<(i64, Target)>,
}

/// Puts loose reports into games: the existing game with the same roster hash and a start within
/// [`SAME_GAME_SECS`], else a new one shared by the other loose reports close to it.
pub fn plan(known: &[Known], loose: &[Loose]) -> Plan {
    let mut out = Plan::default();
    let mut sorted: Vec<&Loose> = loose.iter().collect();
    sorted.sort_by_key(|l| (l.started, l.id));
    // (setup, earliest start so far) of each game in `new_games`
    for l in sorted {
        let existing = known
            .iter()
            .filter(|k| k.setup == l.setup && k.started.abs_diff(l.started) <= SAME_GAME_SECS)
            .min_by_key(|k| k.started.abs_diff(l.started));
        if let Some(k) = existing {
            out.placed.push((l.id, Target::Existing(k.id)));
            continue;
        }
        let fresh = out
            .new_games
            .iter()
            .position(|(setup, at)| *setup == l.setup && l.started.abs_diff(*at) <= SAME_GAME_SECS);
        let index = match fresh {
            Some(i) => i,
            None => {
                out.new_games.push((l.setup.clone(), l.started));
                out.new_games.len() - 1
            }
        };
        out.placed.push((l.id, Target::New(index)));
    }
    out
}

// ---- roster as stored in `games.roster` ------------------------------------------------------

fn num(n: u32) -> Value {
    Value::Number(Number {
        text: n.to_string(),
    })
}

/// `[{"tag":..|null,"name":..,"kind":"human","team":n,"race":n}, ...]`, compact.
pub fn roster_to_json(players: &[Player]) -> String {
    Value::Array(
        players
            .iter()
            .map(|p| {
                Value::Object(vec![
                    (
                        "tag".into(),
                        p.tag.clone().map_or(Value::Null, Value::String),
                    ),
                    ("name".into(), Value::String(p.name.clone())),
                    ("kind".into(), Value::String(p.kind.as_str().into())),
                    ("team".into(), num(u32::from(p.team))),
                    ("race".into(), num(u32::from(p.race))),
                ])
            })
            .collect(),
    )
    .to_json()
}

pub fn roster_from_json(text: &str) -> Option<Vec<Player>> {
    let doc = json::parse(text.as_bytes()).ok()?;
    let mut out = Vec::new();
    for (i, p) in doc.as_array()?.iter().enumerate() {
        let uint = |k: &str| {
            p.get(k)
                .and_then(Value::as_number)
                .and_then(Number::as_u64)
                .and_then(|n| u8::try_from(n).ok())
        };
        out.push(Player {
            slot: u8::try_from(i).ok()?,
            kind: Kind::parse(p.get("kind")?.as_str()?)?,
            name: p.get("name")?.as_str()?.to_string(),
            tag: p.get("tag").and_then(Value::as_str).map(str::to_string),
            team: uint("team")?,
            race: uint("race")?,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn vote(key: &str, name: &str, team: u8, outcome: &str, turns: u32) -> Vote {
        Vote {
            key: key.into(),
            tag: format!("{name}#1"),
            team,
            outcome: outcome.into(),
            turns,
            sync_mismatch: 0,
            combat: None,
        }
    }

    fn duel() -> Vec<Player> {
        vec![human(0, "Ann", 1), human(1, "Bob", 2)]
    }

    fn none() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn a_win_and_the_matching_loss_verify_and_rate_the_game() {
        let v = judge(
            &duel(),
            &[vote("k1", "Ann", 1, "win", 5000), vote("k2", "Bob", 2, "loss", 5002)],
            &none(),
            &BTreeMap::new(),
        );
        assert_eq!((v.status, v.winner_team, v.rated), (Status::Verified, Some(1), true));
        assert_eq!(v.turns, Some(5002));
    }

    #[test]
    fn one_side_alone_is_accepted_on_trust_but_not_called_corroborated() {
        for only in [vote("k1", "Ann", 1, "win", 5000), vote("k2", "Bob", 2, "loss", 5000)] {
            let v = judge(&duel(), &[only], &none(), &BTreeMap::new());
            assert_eq!((v.status, v.winner_team, v.rated), (Status::Verified, Some(1), true));
            assert!(!v.corroborated);
            assert_eq!(v.reason, "single report, accepted on trust");
        }
        // and both sides agreeing is what earns the label
        let both = judge(
            &duel(),
            &[vote("k1", "Ann", 1, "win", 5000), vote("k2", "Bob", 2, "loss", 5000)],
            &none(),
            &BTreeMap::new(),
        );
        assert!(both.corroborated);
    }

    #[test]
    fn two_wins_are_disputed_and_never_rated() {
        let v = judge(
            &duel(),
            &[vote("k1", "Ann", 1, "win", 5000), vote("k2", "Bob", 2, "win", 5000)],
            &none(),
            &BTreeMap::new(),
        );
        assert_eq!((v.status, v.winner_team, v.rated), (Status::Disputed, None, false));
    }

    #[test]
    fn a_dispute_goes_to_the_side_with_the_better_track_record() {
        let votes = [vote("k1", "Ann", 1, "win", 5000), vote("k2", "Bob", 2, "win", 5000)];
        // Ann's key has taken part in five corroborated games, Bob's in none: 6 against 1.
        let trust: BTreeMap<String, u32> = [("k1".to_string(), 5)].into();
        let v = judge(&duel(), &votes, &none(), &trust);
        assert_eq!((v.status, v.winner_team, v.rated), (Status::Verified, Some(1), true));
        assert_eq!(v.reason, "settled by the reporters' track record");
        assert!(!v.corroborated, "a settled dispute earns nobody trust");
        // A small edge is not enough: 2 against 1 is under the ratio.
        let small: BTreeMap<String, u32> = [("k1".to_string(), 1)].into();
        assert_eq!(judge(&duel(), &votes, &none(), &small).status, Status::Disputed);
        // and an allowlisted key outweighs any record
        let allow: BTreeSet<String> = ["k2".to_string()].into();
        let v = judge(&duel(), &votes, &allow, &trust);
        assert_eq!(v.winner_team, Some(2));
    }

    #[test]
    fn a_trusted_key_verifies_alone_but_only_for_a_game_it_played() {
        let trusted: BTreeSet<String> = ["k1".to_string()].into();
        let v = judge(&duel(), &[vote("k1", "Ann", 1, "win", 5000)], &trusted, &BTreeMap::new());
        assert_eq!((v.status, v.rated), (Status::Verified, true));
        // the trusted key reporting under a tag that is not in the roster counts for nothing
        let stranger = Vote {
            tag: "Eve#1".into(),
            ..vote("k1", "Eve", 1, "win", 5000)
        };
        assert_eq!(judge(&duel(), &[stranger], &trusted, &BTreeMap::new()).status, Status::Unverified);
    }

    #[test]
    fn leaving_concedes_only_for_the_last_human_on_a_side() {
        let v = judge(
            &duel(),
            &[vote("k1", "Ann", 1, "win", 5000), vote("k2", "Bob", 2, "left", 4000)],
            &none(),
            &BTreeMap::new(),
        );
        assert_eq!((v.status, v.winner_team), (Status::Verified, Some(1)));
        // two on Bob's side: one leaving says nothing about who won
        let team = vec![
            human(0, "Ann", 1),
            human(1, "Bob", 2),
            human(2, "Cy", 2),
        ];
        let v = judge(
            &team,
            &[vote("k1", "Ann", 1, "win", 5000), vote("k2", "Bob", 2, "left", 4000)],
            &none(),
            &BTreeMap::new(),
        );
        // Ann alone is accepted on trust; Bob's leaving added nothing.
        assert_eq!((v.status, v.corroborated), (Status::Verified, false));
    }

    #[test]
    fn leaving_counts_only_when_the_score_shows_a_fight() {
        let fought = |c| Vote {
            combat: c,
            ..vote("k2", "Bob", 2, "left", 4000)
        };
        let ann = vote("k1", "Ann", 1, "win", 5000);
        let judge_with = |c| judge(&duel(), &[fought(c)], &none(), &BTreeMap::new());
        let v = judge_with(Some(true));
        assert_eq!((v.status, v.winner_team, v.rated), (Status::Verified, Some(1), true));
        // no kills, or a client that cannot say: the leave is not a result
        assert_eq!(judge_with(Some(false)).status, Status::Unverified);
        assert_eq!(judge_with(None).status, Status::Unverified);
        // Ann's own win is unaffected by Bob's missing score (older client on one side)
        let v = judge(&duel(), &[ann, fought(None)], &none(), &BTreeMap::new());
        assert_eq!((v.status, v.winner_team), (Status::Verified, Some(1)));
    }

    #[test]
    fn a_game_without_kills_is_not_rated_even_with_a_win_and_a_loss() {
        let quiet = |key: &str, name: &str, team, outcome: &str| Vote {
            combat: Some(false),
            ..vote(key, name, team, outcome, 5000)
        };
        let v = judge(
            &duel(),
            &[quiet("k1", "Ann", 1, "win"), quiet("k2", "Bob", 2, "loss")],
            &none(),
            &BTreeMap::new(),
        );
        assert_eq!((v.status, v.rated), (Status::Unverified, false));
        // while a report with no score at all (older client) still counts
        let old = judge(&duel(), &[vote("k1", "Ann", 1, "win", 5000)], &none(), &BTreeMap::new());
        assert!(old.rated);
    }

    #[test]
    fn short_or_out_of_sync_reports_decide_nothing() {
        let short = judge(
            &duel(),
            &[vote("k1", "Ann", 1, "win", 100), vote("k2", "Bob", 2, "loss", 100)],
            &none(),
            &BTreeMap::new(),
        );
        assert_eq!(short.status, Status::Unverified);
        let desync = Vote {
            sync_mismatch: 1,
            ..vote("k2", "Bob", 2, "loss", 5000)
        };
        // Bob's out-of-sync report is ignored; Ann's win stands alone.
        let v = judge(&duel(), &[vote("k1", "Ann", 1, "win", 5000), desync], &none(), &BTreeMap::new());
        assert_eq!((v.status, v.corroborated, v.reason), (Status::Verified, false, "single report, accepted on trust"));
    }

    #[test]
    fn a_reporter_must_be_in_the_roster_under_that_tag_and_team() {
        let wrong_team = vote("k2", "Bob", 1, "loss", 5000);
        let v = judge(&duel(), &[vote("k1", "Ann", 1, "win", 5000), wrong_team], &none(), &BTreeMap::new());
        assert!(!v.corroborated, "the vote under the wrong team does not count");
        let v = judge(&duel(), &[], &none(), &BTreeMap::new());
        assert_eq!(v.reason, "no reporter in the roster");
    }

    #[test]
    fn a_verified_game_with_computers_is_not_rated() {
        let mut roster = duel();
        roster.push(Player {
            kind: Kind::Computer,
            tag: None,
            ..human(2, "Azeroth", 2)
        });
        let v = judge(
            &roster,
            &[vote("k1", "Ann", 1, "win", 5000), vote("k2", "Bob", 2, "loss", 5000)],
            &none(),
            &BTreeMap::new(),
        );
        assert_eq!((v.status, v.rated), (Status::Verified, false));
    }

    #[test]
    fn observers_neither_make_a_third_team_nor_get_rated() {
        let mut roster = duel();
        roster.push(human(2, "Watcher", OBSERVER_TEAM));
        let v = judge(
            &roster,
            &[vote("k1", "Ann", 1, "win", 5000), vote("k2", "Bob", 2, "loss", 5000)],
            &none(),
            &BTreeMap::new(),
        );
        assert_eq!((v.status, v.rated), (Status::Verified, true));
        let mut s = BTreeMap::new();
        apply(&mut s, &roster, 1, &BTreeMap::new());
        assert!(!s.contains_key("Watcher#1"));
    }

    #[test]
    fn applying_a_game_moves_the_winner_up_and_the_loser_down() {
        let mut s = BTreeMap::new();
        let changes = apply(&mut s, &duel(), 1, &BTreeMap::new());
        assert_eq!(changes.len(), 2);
        let (ann, bob) = (&s["Ann#1"], &s["Bob#1"]);
        assert!(ann.rating.mu > 25.0 && bob.rating.mu < 25.0);
        assert_eq!((ann.games, ann.wins, bob.losses), (1, 1, 1));
        // the first two-player result of new players is the published walk-through
        assert!((ann.rating.mu - 29.4).abs() < 0.1, "{}", ann.rating.mu);
        let again = apply(&mut s, &duel(), 1, &BTreeMap::new());
        assert!(again[0].after.mu > again[0].before.mu);
    }

    #[test]
    fn a_teammate_who_did_no_fighting_earns_less_of_the_win() {
        let team = |name: &str, slot, team| human(slot, name, team);
        let roster = vec![
            team("Ann", 0, 1),
            team("Cid", 1, 1),
            team("Bob", 2, 2),
            team("Dee", 3, 2),
        ];
        let even = |military: &BTreeMap<u8, u32>| {
            let mut s = BTreeMap::new();
            apply(&mut s, &roster, 1, military);
            s
        };
        let flat = even(&BTreeMap::new());
        // Ann killed and razed 40; Cid only mined. Bob and Dee fought evenly.
        let score = military_by_slot(
            r#"[{"slot":0,"kills":30,"razed":10},{"slot":1,"kills":0,"razed":0},
                {"slot":2,"kills":8,"razed":2},{"slot":3,"kills":9,"razed":1}]"#,
        );
        assert_eq!(score[&0], 40);
        let weighted = even(&score);
        assert!(weighted["Ann#1"].rating.mu - 25.0 > weighted["Cid#1"].rating.mu - 25.0);
        // Cid gets half the gain Ann does; without the score they were identical.
        let gain = |s: &BTreeMap<String, Standing>, t: &str| s[t].rating.mu - 25.0;
        assert!((gain(&flat, "Ann#1") - gain(&flat, "Cid#1")).abs() < 1e-9);
        assert!((gain(&weighted, "Cid#1") / gain(&weighted, "Ann#1") - 0.5).abs() < 1e-9);
        // The losing side fought evenly: both lose the same.
        assert!((gain(&weighted, "Bob#1") - gain(&weighted, "Dee#1")).abs() < 1e-9);
    }

    #[test]
    fn reports_of_one_game_share_it_and_a_rematch_is_a_new_game() {
        let l = |id, setup: &str, started| Loose {
            id,
            setup: setup.into(),
            started,
        };
        let known = [Known {
            id: 7,
            setup: "s1".into(),
            started: 1000,
        }];
        let p = plan(
            &known,
            &[
                l(1, "s1", 1100),  // joins game 7
                l(2, "s2", 5000),  // a new game ...
                l(3, "s2", 5120),  // ... shared by the other side's report
                l(4, "s2", 9000),  // the same people again, an hour later: another game
                l(5, "s1", 9000),  // far from game 7: new
            ],
        );
        assert_eq!(p.new_games, vec![("s2".into(), 5000), ("s2".into(), 9000), ("s1".into(), 9000)]);
        assert_eq!(
            p.placed,
            vec![
                (1, Target::Existing(7)),
                (2, Target::New(0)),
                (3, Target::New(0)),
                (4, Target::New(1)),
                (5, Target::New(2)),
            ]
        );
        assert_eq!(plan(&[], &[]), Plan::default());
    }

    #[test]
    fn rosters_round_trip_through_their_stored_text() {
        let mut roster = duel();
        roster.push(Player {
            kind: Kind::Computer,
            tag: None,
            ..human(2, "Azeroth", 3)
        });
        let back = roster_from_json(&roster_to_json(&roster)).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].tag.as_deref(), Some("Ann#1"));
        assert_eq!((back[2].kind, back[2].tag.clone(), back[2].team), (Kind::Computer, None, 3));
        assert!(roster_from_json("nope").is_none());
    }
}
