//! Reads a folder of sealed reports back into matches and totals. This is the local version of
//! what a server does on ingest: check each signature, pair a match's `start` and `result`
//! reports (same key, setup and start time), and count outcomes. Unverifiable files are set
//! aside and listed, never counted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::envelope::Envelope;
use crate::report::{Outcome, Phase, Report, ResultInfo};
use crate::slot::{Kind, Player};

/// One match as one reporter saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub key: [u8; 32],
    pub started: u64,
    pub setup: [u8; 32],
    pub map: String,
    pub me: Player,
    pub players: Vec<Player>,
    /// `None` while only the start report exists (still running, or the game died).
    pub result: Option<ResultInfo>,
}

impl Match {
    /// Players on another team than the reporter.
    pub fn opponents(&self) -> impl Iterator<Item = &Player> {
        self.players.iter().filter(|p| p.team != self.me.team)
    }

    /// Opponent label: humans by BattleTag, computers marked as such.
    pub fn label(p: &Player) -> String {
        match p.kind {
            Kind::Human => p.identity().to_string(),
            Kind::Computer => format!("{} (computer)", p.name),
        }
    }
}

#[derive(Debug, Default)]
pub struct Loaded {
    pub matches: Vec<Match>,
    /// Files that were not accepted, with the reason.
    pub rejected: Vec<(PathBuf, String)>,
}

/// Loads every `.json` report in `dir`.
pub fn load(dir: &Path) -> Loaded {
    let mut out = Loaded::default();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    let mut reports: Vec<([u8; 32], Report)> = Vec::new();
    for path in paths {
        let parsed = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|raw| Envelope::parse(&raw).map_err(str::to_string))
            .and_then(|env| env.report().map(|r| (env.key, r)).map_err(str::to_string));
        match parsed {
            Ok(r) => reports.push(r),
            Err(why) => out.rejected.push((path, why)),
        }
    }
    out.matches = pair(reports);
    out
}

/// Joins reports into matches, oldest first. A report whose own slot is not in its roster is
/// dropped (it cannot say what happened to the reporter).
pub fn pair(reports: Vec<([u8; 32], Report)>) -> Vec<Match> {
    type Id = ([u8; 32], [u8; 32], u64);
    let mut by_id: BTreeMap<Id, Match> = BTreeMap::new();
    for (key, r) in reports {
        let Some(me) = r.players.iter().find(|p| p.slot == r.me).cloned() else {
            continue;
        };
        let entry = by_id
            .entry((key, r.setup, r.started))
            .or_insert_with(|| Match {
                key,
                started: r.started,
                setup: r.setup,
                map: r.map.clone(),
                me,
                players: r.players.clone(),
                result: None,
            });
        if r.phase == Phase::Result {
            entry.result = r.result;
        }
    }
    let mut matches: Vec<Match> = by_id.into_values().collect();
    matches.sort_by_key(|m| m.started);
    matches
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub games: u32,
    pub wins: u32,
    pub losses: u32,
    pub left: u32,
    pub other: u32,
    /// Started, never finished.
    pub unfinished: u32,
}

impl Tally {
    fn add(&mut self, m: &Match) {
        self.games += 1;
        match m.result.as_ref().map(|r| &r.outcome) {
            Some(Outcome::Win) => self.wins += 1,
            Some(Outcome::Loss) => self.losses += 1,
            Some(Outcome::Left) => self.left += 1,
            Some(Outcome::Other(_)) => self.other += 1,
            None => self.unfinished += 1,
        }
    }
}

#[derive(Debug, Default)]
pub struct Summary {
    pub total: Tally,
    pub by_opponent: BTreeMap<String, Tally>,
    pub by_map: BTreeMap<String, Tally>,
}

pub fn summarize(matches: &[Match]) -> Summary {
    let mut s = Summary::default();
    for m in matches {
        s.total.add(m);
        s.by_map.entry(m.map.clone()).or_default().add(m);
        for p in m.opponents() {
            s.by_opponent.entry(Match::label(p)).or_default().add(m);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::seal;
    use crate::identity::Identity;
    use crate::queue::Queue;
    use crate::report::setup_id;

    fn players() -> Vec<Player> {
        vec![
            Player {
                slot: 0,
                kind: Kind::Human,
                name: "Me".into(),
                tag: Some("Me#1".into()),
                team: 1,
                race: 0,
            },
            Player {
                slot: 1,
                kind: Kind::Computer,
                name: "Azeroth".into(),
                tag: None,
                team: 2,
                race: 1,
            },
        ]
    }

    fn report(phase: Phase, started: u64, code: Option<(u8, u32)>) -> Report {
        let p = players();
        Report {
            phase,
            rid: [started as u8; 8],
            setup: setup_id("Battle_2.pud", &p),
            started,
            client: "t".into(),
            fingerprint: "00000000".into(),
            map: "Battle_2.pud".into(),
            me: 0,
            players: p,
            game: None,
            result: code.map(|(c, t)| ResultInfo {
                outcome: Outcome::from_code(c, t).unwrap(),
                code: c,
                turns: t,
                sync_mismatch: 0,
                score: None,
            }),
        }
    }

    fn keyed(reports: Vec<Report>) -> Vec<([u8; 32], Report)> {
        reports.into_iter().map(|r| ([1; 32], r)).collect()
    }

    #[test]
    fn pairs_start_with_result_and_keeps_unfinished_matches() {
        let m = pair(keyed(vec![
            report(Phase::Start, 100, None),
            report(Phase::Result, 100, Some((3, 500))),
            report(Phase::Start, 200, None),
            report(Phase::Start, 300, None),
            report(Phase::Result, 300, Some((0, 90))),
        ]));
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].result.as_ref().unwrap().outcome, Outcome::Win);
        assert!(m[1].result.is_none());
        assert_eq!(m[2].result.as_ref().unwrap().outcome, Outcome::Left);
    }

    #[test]
    fn a_result_without_its_start_still_counts() {
        let m = pair(keyed(vec![report(Phase::Result, 100, Some((2, 800)))]));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].result.as_ref().unwrap().outcome, Outcome::Loss);
    }

    #[test]
    fn totals_and_breakdowns() {
        let m = pair(keyed(vec![
            report(Phase::Result, 100, Some((3, 500))),
            report(Phase::Result, 200, Some((3, 600))),
            report(Phase::Result, 300, Some((2, 700))),
            report(Phase::Start, 400, None),
        ]));
        let s = summarize(&m);
        assert_eq!(s.total.games, 4);
        assert_eq!(
            (s.total.wins, s.total.losses, s.total.unfinished),
            (2, 1, 1)
        );
        assert_eq!(s.by_opponent["Azeroth (computer)"].games, 4);
        assert_eq!(s.by_map["Battle_2.pud"].wins, 2);
    }

    #[test]
    fn loads_a_folder_and_sets_aside_forgeries() {
        let dir = std::env::temp_dir().join(format!("w2r-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let q = Queue::open(&dir).unwrap();
        let id = Identity::from_secret([5; 32]);
        for r in [
            report(Phase::Start, 100, None),
            report(Phase::Result, 100, Some((3, 500))),
        ] {
            let name = Queue::file_name(r.started, r.phase.as_str(), &crate::hex::encode(&r.rid));
            q.push(&name, seal(&id, &r).to_json().as_bytes()).unwrap();
        }
        // A report edited after signing, and a file that is not a report at all.
        let forged = seal(&id, &report(Phase::Result, 900, Some((2, 10)))).to_json();
        q.push(
            "000000000900-result-ff.json",
            forged.replace("\"loss\"", "\"win\"").as_bytes(),
        )
        .unwrap();
        q.push("000000000901-result-ee.json", b"not json").unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.matches.len(), 1);
        assert_eq!(
            loaded.matches[0].result.as_ref().unwrap().outcome,
            Outcome::Win
        );
        assert_eq!(loaded.rejected.len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
