//! Ratings for the players in the lobby: asks the stats service for the tags it sees, keeps the
//! answers for a few minutes, and gives the lobby screen something to draw and a balanced split to
//! suggest. Only runs while match sharing is on: asking tells the service who is in the room.
//!
//! The lobby calls [`want`] every frame; it is cheap, and starts one background request at a time
//! for tags it has no fresh answer for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use w2r_stats::balance;
use w2r_stats::rating::Rating;
use w2r_stats::share::Settings;

/// An answer is used for this long before it is asked for again.
const FRESH_MS: u64 = 5 * 60_000;
/// After a failed or empty request the same tags are left alone this long.
const RETRY_MS: u64 = 30_000;
/// The service takes this many tags at once.
const MAX_TAGS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Standing {
    /// Not asked about yet, or the answer has not come.
    Unknown,
    /// The service has no rated game for this player.
    New,
    Rated { rating: Rating, games: u32 },
}

#[derive(Default)]
struct State {
    root: Option<PathBuf>,
    known: BTreeMap<String, (Option<(Rating, u32)>, u64)>,
    asked: BTreeMap<String, u64>,
    busy: bool,
    /// Bumped whenever an answer arrives, so the lobby can tell when to rework its suggestion.
    version: u32,
}

static STATE: Mutex<State> = Mutex::new(State {
    root: None,
    known: BTreeMap::new(),
    asked: BTreeMap::new(),
    busy: false,
    version: 0,
});

fn lock() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn init(root: &Path) {
    lock().root = Some(root.to_path_buf());
}

/// The BattleTag in a lobby name: `Name#12345`. Computers and untagged names give `None`.
pub fn tag_of(name: &str) -> Option<String> {
    let name = name.trim();
    let (base, digits) = name.rsplit_once('#')?;
    let ok = !base.is_empty()
        && (1..=10).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| name.to_owned())
}

/// What is known about `tag` right now.
pub fn standing(tag: &str) -> Standing {
    match lock().known.get(tag) {
        None => Standing::Unknown,
        Some((None, _)) => Standing::New,
        Some((Some((rating, games)), _)) => Standing::Rated {
            rating: *rating,
            games: *games,
        },
    }
}

/// Changes whenever an answer arrives.
pub fn version() -> u32 {
    lock().version
}

/// Starts a request for the tags in `tags` that have no fresh answer. `now` is any steady clock in
/// milliseconds.
pub fn want(tags: &[String], now: u64) {
    let (root, ask) = {
        let mut s = lock();
        if s.busy {
            return;
        }
        let Some(root) = s.root.clone() else { return };
        let ask: Vec<String> = tags
            .iter()
            .filter(|t| {
                let fresh = s
                    .known
                    .get(*t)
                    .is_some_and(|(_, at)| now.saturating_sub(*at) < FRESH_MS);
                let tried = s
                    .asked
                    .get(*t)
                    .is_some_and(|at| now.saturating_sub(*at) < RETRY_MS);
                !fresh && !tried
            })
            .take(MAX_TAGS)
            .cloned()
            .collect();
        if ask.is_empty() {
            return;
        }
        for t in &ask {
            s.asked.insert(t.clone(), now);
        }
        s.busy = true;
        (root, ask)
    };
    let spawned = std::thread::Builder::new()
        .name("w2r-ratings".into())
        .spawn(move || {
            let answer = fetch(&root, &ask);
            let mut s = lock();
            s.busy = false;
            if let Some(found) = answer {
                // Stamped with the time the question was asked, which `asked` still holds.
                for t in ask {
                    let at = s.asked.get(&t).copied().unwrap_or(0);
                    let entry = found.get(&t).copied();
                    s.known.insert(t, (entry, at));
                }
                s.version = s.version.wrapping_add(1);
            }
        });
    if spawned.is_err() {
        lock().busy = false;
    }
}

fn fetch(root: &Path, tags: &[String]) -> Option<BTreeMap<String, (Rating, u32)>> {
    let settings = Settings::load(root).ok()?;
    if !settings.share_match_data {
        return None;
    }
    let base = settings.endpoint_for("/v1/ratings").ok()?;
    let url = format!("{base}?tags={}", encode_list(tags));
    let body = crate::uploader::get(&url).ok()?;
    Some(balance::parse_ratings(&body))
}

/// Tags joined by commas, each percent-encoded (`#` and everything else outside the unreserved set).
fn encode_list(tags: &[String]) -> String {
    let mut out = String::new();
    for (i, tag) in tags.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        for b in tag.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// The text beside a player's name.
pub fn label(s: Standing) -> String {
    match s {
        Standing::Unknown => String::new(),
        Standing::New => "new".to_owned(),
        Standing::Rated { rating, .. } => format!("{:.0}", rating.mu),
    }
}

/// The hover text for a rating.
pub fn tip(s: Standing) -> Option<String> {
    match s {
        Standing::Unknown => None,
        Standing::New => Some("No rated games yet (counts as an average player)".to_owned()),
        Standing::Rated { rating, games } => Some(format!(
            "Rating {:.1} (give or take {:.1}) from {games} rated game{}",
            rating.mu,
            rating.sigma,
            if games == 1 { "" } else { "s" }
        )),
    }
}

/// A line suggesting how to split the humans in `players` (name, tag) into two even teams, or
/// `None` when there is nothing to choose between (fewer than four, or no answers yet).
pub fn suggestion(players: &[(String, String)]) -> Option<String> {
    if players.len() < 4 || players.len() > balance::MAX_PLAYERS {
        return None;
    }
    let standings: Vec<Standing> = players.iter().map(|(_, t)| standing(t)).collect();
    if standings.iter().all(|s| matches!(s, Standing::Unknown | Standing::New)) {
        return None;
    }
    let ratings: Vec<Rating> = standings
        .iter()
        .map(|s| match s {
            Standing::Rated { rating, .. } => *rating,
            _ => Rating::default(),
        })
        .collect();
    let split = balance::best_split(&ratings)?;
    let names = |team: &[usize]| {
        team.iter()
            .map(|&i| players[i].0.as_str())
            .collect::<Vec<_>>()
            .join(" + ")
    };
    Some(format!(
        "Even teams: {} against {} ({:.0}% / {:.0}%)",
        names(&split.first),
        names(&split.second),
        split.first_wins * 100.0,
        (1.0 - split.first_wins) * 100.0
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_need_a_name_and_digits() {
        assert_eq!(tag_of("Beau#1318").as_deref(), Some("Beau#1318"));
        assert_eq!(tag_of(" Beau#1318 ").as_deref(), Some("Beau#1318"));
        assert_eq!(tag_of("Beau"), None);
        assert_eq!(tag_of("#1318"), None);
        assert_eq!(tag_of("Beau#12x"), None);
        assert_eq!(tag_of("Beau#"), None);
    }

    #[test]
    fn tags_are_percent_encoded_in_the_list() {
        let tags = vec!["a b#1".to_owned(), "C#2".to_owned()];
        assert_eq!(encode_list(&tags), "a%20b%231,C%232");
    }

    #[test]
    fn suggestion_needs_four_and_some_news() {
        let four: Vec<(String, String)> = (0..4)
            .map(|i| (format!("P{i}"), format!("zz-test-{i}#1")))
            .collect();
        assert_eq!(suggestion(&four), None, "nobody is known");
        assert_eq!(suggestion(&four[..3]), None);
    }

    #[test]
    fn labels_and_tips() {
        let r = Standing::Rated {
            rating: Rating { mu: 29.4, sigma: 3.1 },
            games: 1,
        };
        assert_eq!(label(r), "29");
        assert_eq!(label(Standing::New), "new");
        assert!(tip(r).unwrap().contains("1 rated game"));
        assert_eq!(tip(Standing::Unknown), None);
    }
}
