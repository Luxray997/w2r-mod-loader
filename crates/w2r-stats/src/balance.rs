//! Team balance for a lobby: read the service's ratings answer, and find the split of the players
//! into two teams that is closest to an even game. Pure logic; the lobby feeds it ratings and
//! shows the answer.

use std::collections::BTreeMap;

use crate::rating::{Rating, win_probability};

/// Most players the exhaustive search handles (the game has eight slots).
pub const MAX_PLAYERS: usize = 8;

/// `{"ratings":[{"tag":..,"mu":..,"sigma":..}, ..]}` into a map by tag. Anything malformed is
/// skipped: a tag without a rating is a new player.
pub fn parse_ratings(body: &[u8]) -> BTreeMap<String, (Rating, u32)> {
    let mut out = BTreeMap::new();
    let Ok(doc) = w2r_formats::json::parse(body) else {
        return out;
    };
    let Some(list) = doc.get("ratings").and_then(|v| v.as_array()) else {
        return out;
    };
    for item in list {
        let number = |k: &str| {
            item.get(k)
                .and_then(|v| v.as_number())
                .and_then(|n| n.text.parse::<f64>().ok())
        };
        let (Some(tag), Some(mu), Some(sigma)) =
            (item.get("tag").and_then(|v| v.as_str()), number("mu"), number("sigma"))
        else {
            continue;
        };
        if mu.is_finite() && sigma.is_finite() && sigma > 0.0 {
            let games = number("games").map_or(0, |g| g as u32);
            out.insert(tag.to_string(), (Rating { mu, sigma }, games));
        }
    }
    out
}

/// A split of players (by their position in the input) into two teams.
#[derive(Clone, Debug, PartialEq)]
pub struct Split {
    pub first: Vec<usize>,
    pub second: Vec<usize>,
    /// Chance that `first` wins.
    pub first_wins: f64,
}

impl Split {
    /// How far from an even game, 0 for a coin flip.
    pub fn unevenness(&self) -> f64 {
        (self.first_wins - 0.5).abs()
    }
}

/// The chance that `first` beats `second`, given indices into `ratings`.
pub fn chance(ratings: &[Rating], first: &[usize], second: &[usize]) -> f64 {
    let pick = |t: &[usize]| t.iter().map(|&i| ratings[i]).collect::<Vec<_>>();
    win_probability(&pick(first), &pick(second))
}

/// The most even split of `ratings` into two teams whose sizes differ by at most one. Player 0 is
/// always on the first team (mirror images are the same game), and among equally even splits the
/// first found wins, so the answer does not flicker. `None` for fewer than two players or more
/// than [`MAX_PLAYERS`].
pub fn best_split(ratings: &[Rating]) -> Option<Split> {
    let n = ratings.len();
    if !(2..=MAX_PLAYERS).contains(&n) {
        return None;
    }
    let mut best: Option<Split> = None;
    // Bit i of `mask` puts player i on the first team; bit 0 is always set.
    for mask in (1u32..1 << n).step_by(2) {
        let size = mask.count_ones() as usize;
        if size.abs_diff(n - size) > 1 {
            continue;
        }
        let (mut first, mut second) = (Vec::new(), Vec::new());
        for i in 0..n {
            if mask >> i & 1 == 1 {
                first.push(i);
            } else {
                second.push(i);
            }
        }
        let p = chance(ratings, &first, &second);
        let candidate = Split {
            first,
            second,
            first_wins: p,
        };
        if best
            .as_ref()
            .is_none_or(|b| candidate.unevenness() < b.unevenness() - 1e-9)
        {
            best = Some(candidate);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(mu: f64) -> Rating {
        Rating { mu, sigma: 2.0 }
    }

    #[test]
    fn parses_the_service_answer_and_skips_junk() {
        let body = br#"{"ratings":[{"tag":"A#1","mu":30.5,"sigma":3.2,"games":4},{"tag":"B#2"},{"tag":"C#3","mu":20,"sigma":0}]}"#;
        let got = parse_ratings(body);
        assert_eq!(got.len(), 1);
        assert_eq!(got["A#1"], (Rating { mu: 30.5, sigma: 3.2 }, 4));
        assert!(parse_ratings(b"nonsense").is_empty());
    }

    #[test]
    fn two_players_are_one_each() {
        let s = best_split(&[r(30.0), r(20.0)]).unwrap();
        assert_eq!((s.first, s.second), (vec![0], vec![1]));
    }

    #[test]
    fn the_strongest_pair_up_with_the_weakest() {
        // 40 and 10 against 25 and 25 is the even game, not 40 and 25 against 25 and 10.
        let ratings = [r(40.0), r(25.0), r(25.0), r(10.0)];
        let s = best_split(&ratings).unwrap();
        assert_eq!(s.first, vec![0, 3]);
        assert_eq!(s.second, vec![1, 2]);
        assert!((s.first_wins - 0.5).abs() < 1e-6);
    }

    #[test]
    fn odd_counts_split_by_one() {
        let s = best_split(&[r(25.0), r(25.0), r(25.0)]).unwrap();
        assert_eq!(s.first.len() + s.second.len(), 3);
        assert!(s.first.len().abs_diff(s.second.len()) == 1);
    }

    #[test]
    fn unrated_players_count_as_new() {
        let s = best_split(&[Rating::default(), Rating::default(), r(35.0), Rating::default()])
            .unwrap();
        // The strong player is paired with one new player against the other two.
        assert!(s.first.contains(&2) != s.second.contains(&2));
        assert_eq!(s.first.len(), 2);
    }

    #[test]
    fn too_few_or_too_many_gives_nothing() {
        assert!(best_split(&[r(25.0)]).is_none());
        assert!(best_split(&vec![r(25.0); 9]).is_none());
    }

    #[test]
    fn full_room_of_eight_finishes() {
        let ratings: Vec<Rating> = (0..8).map(|i| r(15.0 + 3.0 * i as f64)).collect();
        let s = best_split(&ratings).unwrap();
        assert_eq!(s.first.len(), 4);
        assert!(s.unevenness() < 0.05);
    }
}
