//! A listing of the recordings in a folder, for the replay picker and anything else that wants
//! to show them: who played, on what map, how it ended and whether this install can play it.
//! Pure (files in, entries out), so it is tested without the game.

use std::path::Path;

use crate::format;
use crate::reader::Replay;

/// How a recording ended, from the recorder's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Won,
    Lost,
    /// The recorder left, or the match ended without a result for them.
    Left,
    /// No footer: the game or the host stopped mid-match.
    CutOff,
}

impl Outcome {
    pub fn text(self) -> &'static str {
        match self {
            Outcome::Won => "won",
            Outcome::Lost => "lost",
            Outcome::Left => "left",
            Outcome::CutOff => "cut off",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The file name, inside the replays folder.
    pub file: String,
    /// The map file the game was played on.
    pub map: String,
    pub started: u64,
    pub players: Vec<PlayerLine>,
    pub turns: u32,
    pub outcome: Outcome,
    pub fingerprint: [u8; 4],
    /// Recorded with a start state (older recordings have none and cannot be played).
    pub has_start_state: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerLine {
    pub name: String,
    pub team: u8,
    pub race: u8,
    pub human: bool,
}

impl Entry {
    /// "Beau v Stormreaver Clan".
    pub fn versus(&self) -> String {
        let names: Vec<&str> = self.players.iter().map(|p| p.name.as_str()).collect();
        names.join(" v ")
    }

    /// The start time as `YYYY-MM-DD HH:MM` shifted by `utc_offset_minutes` (the viewer's zone).
    pub fn when(&self, utc_offset_minutes: i32) -> String {
        let shifted = (self.started as i64 + i64::from(utc_offset_minutes) * 60).max(0) as u64;
        let (y, m, d, hh, mm, _) = format::civil(shifted);
        format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
    }
}

/// One recording's entry, or `None` when the bytes are not a recording.
pub fn entry_from_bytes(file: &str, raw: &[u8]) -> Option<Entry> {
    let r = Replay::parse(raw).ok()?;
    let local = usize::from(r.header.local).min(7);
    let outcome = match r.footer.map(|f| f.results[local]) {
        Some(3) => Outcome::Won,
        Some(2) => Outcome::Lost,
        Some(_) => Outcome::Left,
        None => Outcome::CutOff,
    };
    let players = r
        .players()
        .iter()
        .map(|p| PlayerLine {
            name: p.name.clone(),
            team: p.team,
            race: p.race,
            human: p.kind == w2r_stats::slot::Kind::Human,
        })
        .collect();
    Some(Entry {
        file: file.to_owned(),
        map: r.header.map.clone(),
        started: r.header.started,
        players,
        turns: if r.turns.is_empty() {
            0
        } else {
            r.last_turn() + 1
        },
        outcome,
        fingerprint: r.header.fingerprint,
        has_start_state: !r.header.lobby.is_empty(),
    })
}

/// The recordings in `dir`, newest first (the names start with the date), at most `limit`.
pub fn scan(dir: &Path, limit: usize) -> Vec<Entry> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".w2rr"))
        .collect();
    names.sort();
    names
        .into_iter()
        .rev()
        .filter_map(|n| {
            let raw = std::fs::read(dir.join(&n)).ok()?;
            entry_from_bytes(&n, &raw)
        })
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{Footer, Header};
    use crate::writer::Writer;

    fn header(map: &str, started: u64, lobby: bool) -> Header {
        use w2r_stats::slot::{SLOT_LEN, SLOTS};
        Header {
            build: "2818".into(),
            client: "w2rpm test".into(),
            fingerprint: [1, 2, 3, 4],
            map: map.into(),
            started,
            local: 0,
            slots: [[0; SLOT_LEN]; SLOTS],
            owner: [0; crate::format::OWNER_LEN],
            names: [0; crate::format::NAMES_LEN],
            seed: Header::seed_bytes(1, 2),
            lobby: if lobby {
                vec![0; crate::format::LOBBY_LEN]
            } else {
                Vec::new()
            },
        }
    }

    fn footer(result: u8) -> Footer {
        Footer {
            results: [result, 0, 0, 0, 0, 0, 0, 0],
            turns: 2,
            sync_mismatch: 0,
        }
    }

    /// A two-turn recording; `result` of `None` leaves it without a footer.
    fn recording(map: &str, started: u64, lobby: bool, result: Option<u8>) -> Vec<u8> {
        let mut w = Writer::new(Vec::new(), &header(map, started, lobby)).unwrap();
        w.turn(0, &[(0, &[5])]).unwrap();
        w.turn(1, &[(0, &[5])]).unwrap();
        match result {
            Some(r) => w.finish(&footer(r)).unwrap(),
            // The writer flushes each record, so what was written so far is a cut-off file.
            None => w.into_inner(),
        }
    }

    #[test]
    fn an_entry_says_how_the_match_ended() {
        let at = 1_700_000_000;
        let won = entry_from_bytes("a.w2rr", &recording("Map.pud", at, true, Some(3)));
        let lost = entry_from_bytes("a.w2rr", &recording("Map.pud", at, true, Some(2)));
        let left = entry_from_bytes("a.w2rr", &recording("Map.pud", at, true, Some(0)));
        let cut = entry_from_bytes("a.w2rr", &recording("Map.pud", at, true, None));
        assert_eq!(won.unwrap().outcome, Outcome::Won);
        assert_eq!(lost.unwrap().outcome, Outcome::Lost);
        assert_eq!(left.unwrap().outcome, Outcome::Left);
        let cut = cut.unwrap();
        assert_eq!(cut.outcome, Outcome::CutOff);
        assert_eq!(cut.turns, 2);
        assert_eq!(cut.map, "Map.pud");
        assert!(cut.has_start_state);
    }

    #[test]
    fn old_recordings_have_no_start_state() {
        let e = entry_from_bytes("a.w2rr", &recording("Map.pud", 1_700_000_000, false, Some(3)));
        assert!(!e.unwrap().has_start_state);
    }

    #[test]
    fn garbage_is_not_an_entry() {
        assert!(entry_from_bytes("a.w2rr", b"not a recording").is_none());
    }

    #[test]
    fn the_start_time_is_shifted_into_the_viewers_zone() {
        let e = entry_from_bytes("a.w2rr", &recording("Map.pud", 1_700_000_000, true, Some(3)))
            .unwrap();
        // 2023-11-14 22:13:20 UTC.
        assert_eq!(e.when(0), "2023-11-14 22:13");
        assert_eq!(e.when(-300), "2023-11-14 17:13");
        assert_eq!(e.when(120), "2023-11-15 00:13");
    }

    #[test]
    fn scan_lists_newest_first_and_skips_other_files() {
        let dir = std::env::temp_dir().join(format!("w2r-catalog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("20230101-000000-A.w2rr"),
            recording("A.pud", 1_672_531_200, true, Some(3)),
        )
        .unwrap();
        std::fs::write(
            dir.join("20240101-000000-B.w2rr"),
            recording("B.pud", 1_704_067_200, true, Some(2)),
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("20250101-000000-C.w2rr"), b"broken").unwrap();
        let entries = scan(&dir, 10);
        let maps: Vec<&str> = entries.iter().map(|e| e.map.as_str()).collect();
        assert_eq!(maps, ["B.pud", "A.pud"]);
        assert_eq!(scan(&dir, 1).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
