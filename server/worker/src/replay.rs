//! What the service accepts as a replay. Pure, like `ingest`: bytes, a key and a signature in, a
//! row or a reason out. The file is stored untouched; only the header is read, so a large replay
//! costs one hash and one signature check, not a parse of every turn.
//!
//! Whether the key has *reported this game* is the other half of the rule and needs the database,
//! so `app` checks it against `reports` (same key, same roster hash `setup`, start time within
//! ten minutes) before storing anything.

use w2r_replay::Replay;
use w2r_replay::format::LOBBY_LEN;
use w2r_replay::upload::setup_of;
use w2r_stats::slot::{self, Kind};
use w2r_stats::{blob, hex};

/// Largest file stored: a D1 row holds 2 MB, and real replays are 4 KB to 800 KB.
pub const MAX_BYTES: usize = 1_500_000;
pub const MAX_AGE_SECS: u64 = 14 * 24 * 3600;
pub const MAX_AHEAD_SECS: u64 = 3600;
/// How far the replay's recording start may differ from the report's first-turn time.
pub const LINK_WINDOW_SECS: u64 = 600;

#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    TooLarge,
    BadSignature,
    NotAReplay,
    /// An older recording without the settings block, or a roster with too few players.
    Unlinkable,
    /// Not a game between people: one human against computers, or humans only on one side.
    NoHumanOpponents,
    /// A human in the roster has no BattleTag, so the game was never reported.
    MissingTag,
    TooOld,
    FromTheFuture,
}

impl Reject {
    pub fn reason(&self) -> &'static str {
        match self {
            Reject::TooLarge => "too large",
            Reject::BadSignature => "bad signature",
            Reject::NotAReplay => "not a replay",
            Reject::Unlinkable => "no settings block in the replay",
            Reject::NoHumanOpponents => "no human opponents",
            Reject::MissingTag => "a human has no BattleTag",
            Reject::TooOld => "too old",
            Reject::FromTheFuture => "start time is in the future",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub sha: String,
    pub key: String,
    pub size: usize,
    pub started: u64,
    pub map: String,
    /// The roster hash, as the reports name it.
    pub setup: String,
    pub build: String,
    pub client: String,
}

pub fn check(raw: &[u8], key: &[u8; 32], sig: &[u8; 64], now: u64) -> Result<Row, Reject> {
    if raw.len() > MAX_BYTES {
        return Err(Reject::TooLarge);
    }
    if !blob::verify(key, sig, raw) {
        return Err(Reject::BadSignature);
    }
    let header = Replay::parse_header(raw).map_err(|_| Reject::NotAReplay)?;
    let players = slot::parse_table(&header.slots);
    if header.lobby.len() != LOBBY_LEN || players.len() < 2 || players.len() > 8 {
        return Err(Reject::Unlinkable);
    }
    if !slot::has_human_opponents(&players) {
        return Err(Reject::NoHumanOpponents);
    }
    if players
        .iter()
        .any(|p| p.kind == Kind::Human && p.tag.is_none())
    {
        return Err(Reject::MissingTag);
    }
    if header.started + MAX_AGE_SECS < now {
        return Err(Reject::TooOld);
    }
    if header.started > now + MAX_AHEAD_SECS {
        return Err(Reject::FromTheFuture);
    }
    let (setup, started) = setup_of(raw).ok_or(Reject::NotAReplay)?;
    Ok(Row {
        sha: hex::encode(&blob::digest(raw)),
        key: hex::encode(key),
        size: raw.len(),
        started,
        map: header.map,
        setup: hex::encode(&setup),
        build: header.build,
        client: header.client,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use w2r_replay::format::Header;
    use w2r_replay::{Footer, Writer};
    use w2r_stats::identity::Identity;
    use w2r_stats::slot::SLOT_LEN;

    const NOW: u64 = 1_791_700_000;

    fn slot_bytes(index: u8, team: u8, name: &str) -> [u8; SLOT_LEN] {
        let mut s = [0u8; SLOT_LEN];
        s[0] = index;
        s[10] = team;
        s[0x0d..0x0d + name.len()].copy_from_slice(name.as_bytes());
        s
    }

    fn replay(started: u64, second: &str, lobby: usize) -> Vec<u8> {
        replay_with(started, lobby, slot_bytes(1, 2, second))
    }

    fn replay_with(started: u64, lobby: usize, second: [u8; SLOT_LEN]) -> Vec<u8> {
        let mut slots = [[0u8; SLOT_LEN]; 8];
        for (i, s) in slots.iter_mut().enumerate() {
            s[0] = i as u8;
            s[8] = 3;
        }
        slots[0] = slot_bytes(0, 1, "Ann#1234");
        slots[1] = second;
        let header = Header {
            build: "2818".into(),
            client: "w2rpm test".into(),
            fingerprint: [0; 4],
            map: "Battle_2.pud".into(),
            started,
            local: 0,
            slots,
            owner: [0; 0x20],
            names: [0; 0x100],
            seed: vec![0; 8],
            lobby: vec![0; lobby],
            map_sha: None,
            slot_backup: Vec::new(),
        };
        let mut w = Writer::new(Vec::new(), &header).unwrap();
        w.turn(0, &[(0, &[5])]).unwrap();
        w.finish(&Footer {
            results: [3, 2, 0, 0, 0, 0, 0, 0],
            turns: 1,
            sync_mismatch: 0,
        })
        .unwrap()
    }

    fn signed(raw: &[u8]) -> ([u8; 32], [u8; 64]) {
        let id = Identity::from_secret([8; 32]);
        (id.public(), blob::sign(&id, raw))
    }

    fn good() -> Vec<u8> {
        replay(NOW - 600, "Bob#5678", LOBBY_LEN)
    }

    #[test]
    fn a_signed_replay_becomes_a_row_with_the_reports_roster_hash() {
        let raw = good();
        let (key, sig) = signed(&raw);
        let row = check(&raw, &key, &sig, NOW).unwrap();
        assert_eq!(row.map, "Battle_2.pud");
        assert_eq!(row.started, NOW - 600);
        assert_eq!(row.size, raw.len());
        assert_eq!(row.sha.len(), 64);
        assert_eq!(row.key, hex::encode(&key));
        let header = Replay::parse_header(&raw).unwrap();
        let players = slot::parse_table(&header.slots);
        assert_eq!(
            row.setup,
            hex::encode(&w2r_stats::report::setup_id("Battle_2.pud", &players))
        );
    }

    #[test]
    fn a_changed_byte_or_a_wrong_key_fails_the_signature() {
        let mut raw = good();
        let (key, sig) = signed(&raw);
        let n = raw.len();
        raw[n - 1] ^= 1;
        assert_eq!(check(&raw, &key, &sig, NOW), Err(Reject::BadSignature));
        let raw = good();
        let (_, sig) = signed(&raw);
        let other = Identity::from_secret([9; 32]).public();
        assert_eq!(check(&raw, &other, &sig, NOW), Err(Reject::BadSignature));
    }

    #[test]
    fn replays_that_could_never_match_a_report_are_refused() {
        let cases = [
            (replay(NOW - 600, "Bob", LOBBY_LEN), Reject::MissingTag),
            (replay(NOW - 600, "Bob#5678", 0), Reject::Unlinkable),
            (replay(NOW - MAX_AGE_SECS - 1, "Bob#5678", LOBBY_LEN), Reject::TooOld),
            (replay(NOW + MAX_AHEAD_SECS + 1, "Bob#5678", LOBBY_LEN), Reject::FromTheFuture),
        ];
        for (raw, want) in cases {
            let (key, sig) = signed(&raw);
            assert_eq!(check(&raw, &key, &sig, NOW), Err(want));
        }
    }

    #[test]
    fn a_game_without_human_opponents_is_refused() {
        let mut computer = slot_bytes(1, 2, "Nation of Azeroth");
        computer[8] = 1;
        let ally = slot_bytes(1, 1, "Bob#5678");
        for second in [computer, ally] {
            let raw = replay_with(NOW - 600, LOBBY_LEN, second);
            let (key, sig) = signed(&raw);
            assert_eq!(check(&raw, &key, &sig, NOW), Err(Reject::NoHumanOpponents));
        }
    }

    #[test]
    fn junk_and_oversize_files_are_refused() {
        let junk = b"definitely not a replay".to_vec();
        let (key, sig) = signed(&junk);
        assert_eq!(check(&junk, &key, &sig, NOW), Err(Reject::NotAReplay));
        let big = vec![0u8; MAX_BYTES + 1];
        let (key, sig) = signed(&big);
        assert_eq!(check(&big, &key, &sig, NOW), Err(Reject::TooLarge));
    }
}
