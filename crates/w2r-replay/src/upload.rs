//! Sending finished replays to the stats service (docs/stats-service.md), beside the signed
//! reports. A replay holds everything about a game (settings, the seed, every command and the
//! public chat), so the server can work out more from it later than a report says.
//!
//! The file goes up untouched, as `PUT /v1/replays` with the installation key and a signature over
//! the file's SHA-256 (`w2r_stats::blob`). The server keeps it only when that same key has already
//! reported the game, which is why the answer 409 ("no report yet") is retried later and every
//! other refusal is final.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use w2r_stats::identity::Identity;
use w2r_stats::report::setup_id;
use w2r_stats::share::{Error, Fate, Ledger, Poster, Settings};
use w2r_stats::slot::{self, Kind};
use w2r_stats::{blob, hex};

use crate::Replay;
use crate::format::LOBBY_LEN;

/// The replays' own ledger, next to the reports' one.
pub const LEDGER: &str = "replay-upload-state.json";
/// The largest file sent; bigger ones are skipped (the server refuses them too).
pub const MAX_BYTES: u64 = 1_500_000;
/// A file touched this recently may still be recording.
const SETTLE: Duration = Duration::from_secs(20);
/// A replay older than this is not sent (the server's report log does not reach back further).
const MAX_AGE_SECS: u64 = 14 * 24 * 3600;
/// At most this many files per run, so one run stays short.
const PER_RUN: usize = 10;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub sent: usize,
    pub rejected: usize,
    /// Not sent and never will be: no lobby block, an untagged human, too big, too old.
    pub skipped: usize,
    /// Left for the next run: the server has no report of the game yet, or was busy.
    pub waiting: usize,
}

enum Verdict {
    Sent,
    Rejected,
    Later,
}

/// What the server answered, as an outcome for the ledger.
fn verdict(status: u16) -> Verdict {
    match status {
        200 => Verdict::Sent,
        // No report of this game yet, too many requests, or the service had a problem.
        409 | 429 | 500..=599 => Verdict::Later,
        _ => Verdict::Rejected,
    }
}

/// True when a replay is worth sending: a readable header with the settings block, humans on
/// both sides (a game against the computer alone is not kept) and a BattleTag for every human,
/// started recently enough.
fn sendable(raw: &[u8], now: u64) -> bool {
    let Ok(header) = Replay::parse_header(raw) else {
        return false;
    };
    if header.lobby.len() != LOBBY_LEN || header.started + MAX_AGE_SECS < now {
        return false;
    }
    let players = slot::parse_table(&header.slots);
    slot::has_human_opponents(&players)
        && players
            .iter()
            .all(|p| p.kind != Kind::Human || p.tag.is_some())
}

fn replay_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "w2rr"))
        .collect();
    files.sort();
    files
}

/// Sends the replays the server has not settled yet. Does nothing unless sharing is on. A
/// transport failure stops the run and leaves the ledger as it was for that file.
pub fn upload(
    root: &Path,
    settings: &Settings,
    identity: &Identity,
    poster: &dyn Poster,
) -> Result<Summary, Error> {
    let mut summary = Summary::default();
    if !settings.share_match_data {
        return Ok(summary);
    }
    let endpoint = settings.endpoint_for("v1/replays")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let files = replay_files(&root.join("replays"));
    let names: Vec<String> = files
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let mut ledger = Ledger::load(root, LEDGER);
    ledger.0.retain(|n, _| names.contains(n));

    let key = hex::encode(&identity.public());
    let mut failure = None;
    let mut tried = 0;
    for (path, name) in files.iter().zip(&names) {
        if ledger.0.contains_key(name) {
            continue;
        }
        let Ok(meta) = fs::metadata(path) else {
            continue;
        };
        // Still being written, or being written right now: look again next time.
        let recent = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age < SETTLE);
        if recent {
            summary.waiting += 1;
            continue;
        }
        if meta.len() > MAX_BYTES {
            ledger.0.insert(name.clone(), Fate::Skipped);
            summary.skipped += 1;
            continue;
        }
        let Ok(raw) = fs::read(path) else { continue };
        if !sendable(&raw, now) {
            ledger.0.insert(name.clone(), Fate::Skipped);
            summary.skipped += 1;
            continue;
        }
        if tried == PER_RUN {
            summary.waiting += 1;
            continue;
        }
        tried += 1;
        let headers = [
            ("x-w2r-key", key.clone()),
            ("x-w2r-sig", hex::encode(&blob::sign(identity, &raw))),
        ];
        match poster.put_bytes(&endpoint, &headers, &raw) {
            Ok((status, _)) => match verdict(status) {
                Verdict::Sent => {
                    ledger.0.insert(name.clone(), Fate::Sent);
                    summary.sent += 1;
                }
                Verdict::Rejected => {
                    ledger.0.insert(name.clone(), Fate::Rejected);
                    summary.rejected += 1;
                }
                Verdict::Later => summary.waiting += 1,
            },
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }
    ledger.save(root, LEDGER)?;
    match failure {
        Some(e) => Err(e),
        None => Ok(summary),
    }
}

/// The match a replay's header describes, as the reports name it: the roster hash that links a
/// replay to the reports of the same game, and the recording start time.
pub fn setup_of(raw: &[u8]) -> Option<([u8; 32], u64)> {
    let header = Replay::parse_header(raw).ok()?;
    let players = slot::parse_table(&header.slots);
    Some((setup_id(&header.map, &players), header.started))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Header;
    use crate::Writer;
    use std::cell::RefCell;
    use w2r_stats::slot::SLOT_LEN;

    const NOW_OFFSET: u64 = 100;

    fn temp_root(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "w2r-replay-up-{}-{tag}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("replays")).unwrap();
        dir
    }

    fn slot_bytes(index: u8, team: u8, name: &str) -> [u8; SLOT_LEN] {
        let mut s = [0u8; SLOT_LEN];
        s[0] = index;
        s[8] = 0; // human
        s[10] = team;
        s[0x0d..0x0d + name.len()].copy_from_slice(name.as_bytes());
        s
    }

    fn empty_slot(index: u8) -> [u8; SLOT_LEN] {
        let mut s = [0u8; SLOT_LEN];
        s[0] = index;
        s[8] = 3;
        s
    }

    /// A finished replay of two humans on opposite teams; `tagged` false leaves the second
    /// without a BattleTag.
    fn replay(started: u64, tagged: bool, lobby: usize) -> Vec<u8> {
        let second = slot_bytes(1, 2, if tagged { "Bob#5678" } else { "Bob" });
        replay_with(started, lobby, second)
    }

    /// A finished replay of Ann (team 1) and whoever is in `second` (slot 1).
    fn replay_with(started: u64, lobby: usize, second: [u8; SLOT_LEN]) -> Vec<u8> {
        let mut slots = [[0u8; SLOT_LEN]; 8];
        for (i, s) in slots.iter_mut().enumerate() {
            *s = empty_slot(i as u8);
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
            map_sha: Some([9; 32]),
            slot_backup: Vec::new(),
        };
        let mut w = Writer::new(Vec::new(), &header).unwrap();
        w.turn(0, &[(0, &[5])]).unwrap();
        w.finish(&crate::Footer {
            results: [3, 2, 0, 0, 0, 0, 0, 0],
            turns: 1,
            sync_mismatch: 0,
        })
        .unwrap()
    }

    /// Writes `bytes` as a replay file that looks old enough to be finished.
    fn put(root: &Path, name: &str, bytes: &[u8]) {
        let path = root.join("replays").join(name);
        fs::write(&path, bytes).unwrap();
        let old = SystemTime::now() - Duration::from_secs(600);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    struct Fake {
        status: Result<u16, Error>,
        calls: RefCell<Vec<(String, Vec<(String, String)>, usize)>>,
    }

    impl Fake {
        fn new(status: Result<u16, Error>) -> Fake {
            Fake {
                status,
                calls: RefCell::default(),
            }
        }
    }

    impl Poster for Fake {
        fn post_json(&self, _: &str, _: &[u8]) -> Result<Vec<u8>, Error> {
            Err(Error::new("network", "unused"))
        }

        fn put_bytes(
            &self,
            url: &str,
            headers: &[(&str, String)],
            body: &[u8],
        ) -> Result<(u16, Vec<u8>), Error> {
            self.calls.borrow_mut().push((
                url.to_owned(),
                headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
                body.len(),
            ));
            self.status.clone().map(|s| (s, Vec::new()))
        }
    }

    fn on() -> Settings {
        Settings {
            share_match_data: true,
            url: "https://stats.example.workers.dev".into(),
        }
    }

    fn id() -> Identity {
        Identity::from_secret([4; 32])
    }

    fn fresh() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - NOW_OFFSET
    }

    #[test]
    fn a_finished_replay_is_sent_signed_and_not_sent_twice() {
        let root = temp_root("once");
        let bytes = replay(fresh(), true, LOBBY_LEN);
        put(&root, "a.w2rr", &bytes);
        let fake = Fake::new(Ok(200));
        assert_eq!(upload(&root, &on(), &id(), &fake).unwrap().sent, 1);
        assert_eq!(upload(&root, &on(), &id(), &fake).unwrap(), Summary::default());
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "https://stats.example.workers.dev/v1/replays");
        assert_eq!(calls[0].2, bytes.len());
        let hdr = |k: &str| calls[0].1.iter().find(|(n, _)| n == k).unwrap().1.clone();
        let key: [u8; 32] = hex::decode_array(&hdr("x-w2r-key")).unwrap();
        let sig: [u8; 64] = hex::decode_array(&hdr("x-w2r-sig")).unwrap();
        assert_eq!(key, id().public());
        assert!(blob::verify(&key, &sig, &bytes));
    }

    #[test]
    fn nothing_is_sent_while_sharing_is_off() {
        let root = temp_root("off");
        put(&root, "a.w2rr", &replay(fresh(), true, LOBBY_LEN));
        let fake = Fake::new(Ok(200));
        let off = Settings {
            share_match_data: false,
            ..on()
        };
        assert_eq!(upload(&root, &off, &id(), &fake).unwrap(), Summary::default());
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn replays_that_can_never_be_linked_are_skipped_not_sent() {
        let root = temp_root("skip");
        put(&root, "old-format.w2rr", &replay(fresh(), true, 0));
        put(&root, "untagged.w2rr", &replay(fresh(), false, LOBBY_LEN));
        put(&root, "ancient.w2rr", &replay(1_000, true, LOBBY_LEN));
        put(&root, "garbage.w2rr", b"not a replay");
        let fake = Fake::new(Ok(200));
        let s = upload(&root, &on(), &id(), &fake).unwrap();
        assert_eq!((s.sent, s.skipped), (0, 4));
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn games_without_human_opponents_are_skipped() {
        let root = temp_root("vs-ai");
        let mut computer = slot_bytes(1, 2, "Nation of Azeroth");
        computer[8] = 1; // computer
        put(&root, "solo.w2rr", &replay_with(fresh(), LOBBY_LEN, computer));
        let ally = slot_bytes(1, 1, "Bob#5678");
        put(&root, "allied.w2rr", &replay_with(fresh(), LOBBY_LEN, ally));
        let fake = Fake::new(Ok(200));
        let s = upload(&root, &on(), &id(), &fake).unwrap();
        assert_eq!((s.sent, s.skipped), (0, 2));
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn a_file_still_being_written_waits() {
        let root = temp_root("recent");
        fs::write(
            root.join("replays").join("live.w2rr"),
            replay(fresh(), true, LOBBY_LEN),
        )
        .unwrap();
        let fake = Fake::new(Ok(200));
        let s = upload(&root, &on(), &id(), &fake).unwrap();
        assert_eq!((s.sent, s.waiting), (0, 1));
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn no_report_yet_is_retried_but_other_refusals_are_final() {
        let root = temp_root("answers");
        put(&root, "a.w2rr", &replay(fresh(), true, LOBBY_LEN));
        let later = Fake::new(Ok(409));
        assert_eq!(upload(&root, &on(), &id(), &later).unwrap().waiting, 1);
        assert_eq!(upload(&root, &on(), &id(), &later).unwrap().waiting, 1);
        assert_eq!(later.calls.borrow().len(), 2, "tried again");
        let refused = Fake::new(Ok(400));
        assert_eq!(upload(&root, &on(), &id(), &refused).unwrap().rejected, 1);
        assert_eq!(upload(&root, &on(), &id(), &refused).unwrap(), Summary::default());
        assert_eq!(refused.calls.borrow().len(), 1, "not retried");
    }

    #[test]
    fn a_network_failure_keeps_the_file_for_next_time() {
        let root = temp_root("down");
        put(&root, "a.w2rr", &replay(fresh(), true, LOBBY_LEN));
        let down = Fake::new(Err(Error::new("network", "offline")));
        assert!(upload(&root, &on(), &id(), &down).is_err());
        let up = Fake::new(Ok(200));
        assert_eq!(upload(&root, &on(), &id(), &up).unwrap().sent, 1);
    }

    #[test]
    fn setup_of_names_the_roster_like_the_reports_do() {
        let bytes = replay(1234, true, LOBBY_LEN);
        let (setup, started) = setup_of(&bytes).unwrap();
        let header = Replay::parse_header(&bytes).unwrap();
        let players = slot::parse_table(&header.slots);
        assert_eq!(setup, setup_id("Battle_2.pud", &players));
        assert_eq!(started, 1234);
        assert!(setup_of(b"nope").is_none());
    }
}
