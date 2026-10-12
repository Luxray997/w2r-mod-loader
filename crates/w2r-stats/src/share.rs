//! Sharing match data (docs/stats-service.md): sends the signed reports in `outbox\` to the stats
//! service, once the user has turned `share_match_data` on. Only the manager does this; the
//! in-game host has no network stack. Reports stay in the outbox (they are also the local
//! history); a ledger in `config\upload-state.json` remembers which ones the server has settled.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use w2r_formats::json::{self, Number, Value};

use crate::envelope::Envelope;
use crate::queue::Queue;
use crate::slot::Kind;

/// A refusal with a stable `code`, like the manager's own errors (it converts into one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub detail: String,
}

impl Error {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Error {
        Error {
            code,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::new("io", e.to_string())
    }
}

/// The service address used while `sharing.json` names none.
pub const DEFAULT_URL: &str = "https://w2r-stats.saweet.net";

/// `https://host/...`: ASCII, no spaces, no credentials in the address.
fn url_ok(u: &str) -> bool {
    if u.len() > 2048 || !u.is_ascii() || u.bytes().any(|c| c <= b' ') {
        return false;
    }
    let Some((scheme, rest)) = u.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    scheme.eq_ignore_ascii_case("https") && !authority.is_empty() && !authority.contains('@')
}

pub const FILE: &str = "sharing.json";
/// The ledger of report files; replays keep their own (`w2r_replay::upload`).
pub const REPORT_LEDGER: &str = "upload-state.json";
const MAX_BYTES: usize = 64 * 1024;
const BATCH: usize = 20;

/// The sharing settings, `config\sharing.json`. With no file, sharing is on exactly when the build
/// names a service (`DEFAULT_URL`), so it starts working once the service exists and stays off
/// before that. A file that says `false` always wins; the plugin screen shows the setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub share_match_data: bool,
    /// Base URL of the service, `https://...`, no trailing slash needed.
    pub url: String,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            share_match_data: !DEFAULT_URL.is_empty(),
            url: String::new(),
        }
    }
}

impl Settings {
    pub fn parse(raw: &[u8]) -> Result<Settings, &'static str> {
        if raw.len() > MAX_BYTES {
            return Err("too_large");
        }
        let doc = json::parse(raw).map_err(|_| "not_json")?;
        let fields = doc.as_object().ok_or("not_object")?;
        let mut s = Settings::default();
        let mut schema = false;
        for (key, value) in fields {
            match key.as_str() {
                "schema" => {
                    if value.as_number().and_then(Number::as_u64) != Some(1) {
                        return Err("bad_schema");
                    }
                    schema = true;
                }
                "share_match_data" => s.share_match_data = value.as_bool().ok_or("bad_field")?,
                "url" => s.url = value.as_str().ok_or("bad_field")?.to_owned(),
                _ => return Err("unknown_field"),
            }
        }
        if !schema {
            return Err("bad_schema");
        }
        Ok(s)
    }

    pub fn to_json(&self) -> String {
        Value::Object(vec![
            ("schema".into(), Value::Number(Number { text: "1".into() })),
            (
                "share_match_data".into(),
                Value::Bool(self.share_match_data),
            ),
            ("url".into(), Value::String(self.url.clone())),
        ])
        .to_json()
    }

    pub fn load(root: &Path) -> Result<Settings, Error> {
        match fs::read(root.join("config").join(FILE)) {
            Ok(raw) => Settings::parse(&raw).map_err(|r| Error::new("bad_sharing", r)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, root: &Path) -> Result<(), Error> {
        fs::create_dir_all(root.join("config"))?;
        write_atomic(root.join("config").join(FILE), self.to_json().as_bytes())?;
        Ok(())
    }

    /// The reports endpoint, or why sharing cannot run.
    pub fn endpoint(&self) -> Result<String, Error> {
        self.endpoint_for("v1/reports")
    }

    /// `path` (no leading slash) on the service, or why sharing cannot run.
    pub fn endpoint_for(&self, path: &str) -> Result<String, Error> {
        let base = if self.url.is_empty() { DEFAULT_URL } else { &self.url };
        let base = base.trim_end_matches('/');
        if !url_ok(base) {
            return Err(Error::new(
                "bad_url",
                "the stats service URL must be https://...",
            ));
        }
        Ok(format!("{base}/{path}"))
    }
}

fn write_atomic(path: PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, &path)
}

/// What the server decided about one report file; kept so it is never sent again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    Sent,
    /// The server refused it for a reason that will not change (it is not retried).
    Rejected,
    /// Never sent: not a game against human opponents, or a human has no BattleTag.
    Skipped,
}

impl Fate {
    pub fn as_str(self) -> &'static str {
        match self {
            Fate::Sent => "sent",
            Fate::Rejected => "rejected",
            Fate::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Option<Fate> {
        match s {
            "sent" => Some(Fate::Sent),
            "rejected" => Some(Fate::Rejected),
            "skipped" => Some(Fate::Skipped),
            _ => None,
        }
    }
}

/// File name to what the server decided, kept in `config\<name>` so nothing is sent twice.
#[derive(Debug, Default)]
pub struct Ledger(pub BTreeMap<String, Fate>);

impl Ledger {
    pub fn load(root: &Path, name: &str) -> Ledger {
        let Ok(raw) = fs::read(root.join("config").join(name)) else {
            return Ledger::default();
        };
        let Ok(doc) = json::parse(&raw) else {
            return Ledger::default();
        };
        let mut map = BTreeMap::new();
        for (name, v) in doc.get("files").and_then(Value::as_object).unwrap_or(&[]) {
            if let Some(f) = v.as_str().and_then(Fate::parse) {
                map.insert(name.clone(), f);
            }
        }
        Ledger(map)
    }

    pub fn save(&self, root: &Path, name: &str) -> Result<(), Error> {
        let files = self
            .0
            .iter()
            .map(|(n, f)| (n.clone(), Value::String(f.as_str().into())))
            .collect();
        let doc = Value::Object(vec![
            ("schema".into(), Value::Number(Number { text: "1".into() })),
            ("files".into(), Value::Object(files)),
        ]);
        fs::create_dir_all(root.join("config"))?;
        write_atomic(root.join("config").join(name), doc.to_json().as_bytes())?;
        Ok(())
    }
}

/// How the reports reach the service. A trait so tests need no network.
pub trait Poster {
    fn post_json(&self, url: &str, body: &[u8]) -> Result<Vec<u8>, Error>;

    /// PUTs `body` with extra headers. Any HTTP status comes back as `Ok((status, answer))`, so
    /// the caller can tell "try again later" from "never": only transport failures are errors.
    fn put_bytes(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), Error>;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub sent: usize,
    pub rejected: usize,
    pub skipped: usize,
    /// Reports still to send (the service was unreachable).
    pub pending: usize,
}

/// True when the report is of a game between people and every human in it has a BattleTag. The
/// server refuses the rest, so they are not sent (they stay in the outbox as local history).
fn ratable(raw: &[u8]) -> bool {
    let Ok(env) = Envelope::parse(raw) else {
        return false;
    };
    let Ok(report) = env.report() else {
        return false;
    };
    crate::slot::has_human_opponents(&report.players)
        && report
            .players
            .iter()
            .all(|p| p.kind != Kind::Human || p.tag.is_some())
}

/// Sends every report the server has not settled yet. Does nothing unless sharing is on. A
/// network failure leaves the ledger as it was, so the next run retries the same files.
pub fn upload(root: &Path, settings: &Settings, poster: &dyn Poster) -> Result<Summary, Error> {
    let mut summary = Summary::default();
    if !settings.share_match_data {
        return Ok(summary);
    }
    let endpoint = settings.endpoint()?;
    let queue = Queue::open(&root.join("outbox"))?;
    let mut ledger = Ledger::load(root, REPORT_LEDGER);
    let files = queue.list();
    let names: Vec<String> = files
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    // Forget files that have left the outbox, so the ledger stays as small as the queue.
    ledger.0.retain(|n, _| names.contains(n));

    let mut todo: Vec<(String, Vec<u8>)> = Vec::new();
    for (path, name) in files.iter().zip(&names) {
        if ledger.0.contains_key(name) {
            continue;
        }
        let Ok(raw) = fs::read(path) else { continue };
        if ratable(&raw) {
            todo.push((name.clone(), raw));
        } else {
            ledger.0.insert(name.clone(), Fate::Skipped);
            summary.skipped += 1;
        }
    }

    let mut failure = None;
    for batch in todo.chunks(BATCH) {
        let body = format!(
            r#"{{"reports":[{}]}}"#,
            batch
                .iter()
                .map(|(_, raw)| String::from_utf8_lossy(raw).trim().to_owned())
                .collect::<Vec<_>>()
                .join(",")
        );
        let answer = match poster.post_json(&endpoint, body.as_bytes()) {
            Ok(a) => a,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        let doc = json::parse(&answer).map_err(|_| Error::new("bad_answer", "not json"))?;
        let results = doc
            .get("results")
            .and_then(Value::as_array)
            .filter(|r| r.len() == batch.len())
            .ok_or_else(|| Error::new("bad_answer", "wrong number of results"))?;
        for ((name, _), verdict) in batch.iter().zip(results) {
            if verdict.get("ok").and_then(Value::as_bool) == Some(true) {
                ledger.0.insert(name.clone(), Fate::Sent);
                summary.sent += 1;
            } else {
                ledger.0.insert(name.clone(), Fate::Rejected);
                summary.rejected += 1;
            }
        }
    }
    summary.pending = todo.len() - summary.sent - summary.rejected;
    ledger.save(root, REPORT_LEDGER)?;
    match failure {
        Some(e) => Err(e),
        None => Ok(summary),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use crate::envelope::seal;
    use crate::identity::Identity;
    use crate::report::{Outcome, Phase, Report, ResultInfo, setup_id};
    use crate::slot::Player;

    fn temp_root(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "w2r-share-{}-{tag}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn player(slot: u8, name: &str, tagged: bool) -> Player {
        Player {
            slot,
            kind: Kind::Human,
            name: name.into(),
            tag: tagged.then(|| format!("{name}#1")),
            team: slot + 1,
            race: 0,
        }
    }

    fn put(root: &Path, started: u64, tagged: bool) -> String {
        put_with(root, started, vec![player(0, "Ann", true), player(1, "Bob", tagged)])
    }

    fn put_with(root: &Path, started: u64, players: Vec<Player>) -> String {
        let report = Report {
            phase: Phase::Result,
            rid: started.to_le_bytes(),
            setup: setup_id("m.pud", &players),
            started,
            client: "t".into(),
            fingerprint: "00000000".into(),
            map: "m.pud".into(),
            me: 0,
            players,
            game: None,
            result: Some(ResultInfo {
                outcome: Outcome::Win,
                code: 3,
                turns: 100,
                sync_mismatch: 0,
                score: None,
                activity: None,
            }),
        };
        let json = seal(&Identity::from_secret([3; 32]), &report).to_json();
        let name = Queue::file_name(started, "result", &format!("{started:016x}"));
        Queue::open(&root.join("outbox"))
            .unwrap()
            .push(&name, json.as_bytes())
            .unwrap();
        name
    }

    struct Fake {
        answer: Result<String, Error>,
        bodies: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(answer: Result<&str, Error>) -> Fake {
            Fake {
                answer: answer.map(str::to_owned),
                bodies: RefCell::default(),
            }
        }
    }

    impl Poster for Fake {
        fn post_json(&self, _url: &str, body: &[u8]) -> Result<Vec<u8>, Error> {
            self.bodies
                .borrow_mut()
                .push(String::from_utf8_lossy(body).into_owned());
            self.answer.clone().map(String::into_bytes)
        }

        fn put_bytes(
            &self,
            _url: &str,
            _headers: &[(&str, String)],
            _body: &[u8],
        ) -> Result<(u16, Vec<u8>), Error> {
            Err(Error::new("network", "not used by these tests"))
        }
    }

    const OK: &str = r#"{"results":[{"ok":true}]}"#;

    fn on() -> Settings {
        Settings {
            share_match_data: true,
            url: "https://stats.example.workers.dev/".into(),
        }
    }

    #[test]
    fn settings_round_trip_and_default_off() {
        let root = temp_root("settings");
        assert_eq!(Settings::load(&root).unwrap(), Settings::default());
        // Off only because no service address is built in yet; see `Settings`.
        assert_eq!(Settings::default().share_match_data, !DEFAULT_URL.is_empty());
        on().save(&root).unwrap();
        assert_eq!(Settings::load(&root).unwrap(), on());
        assert!(Settings::parse(br#"{"schema":1,"nope":true}"#).is_err());
    }

    #[test]
    fn endpoint_needs_https() {
        assert_eq!(
            on().endpoint().unwrap(),
            "https://stats.example.workers.dev/v1/reports"
        );
        let plain = Settings {
            url: "http://x.example".into(),
            ..on()
        };
        assert!(plain.endpoint().is_err());
    }

    #[test]
    fn nothing_is_sent_while_sharing_is_off() {
        let root = temp_root("off");
        put(&root, 10, true);
        let fake = Fake::new(Ok(OK));
        let off = Settings {
            share_match_data: false,
            ..on()
        };
        let s = upload(&root, &off, &fake).unwrap();
        assert_eq!(s, Summary::default());
        assert!(fake.bodies.borrow().is_empty());
    }

    #[test]
    fn sent_reports_are_not_sent_twice_and_stay_in_the_outbox() {
        let root = temp_root("once");
        let name = put(&root, 10, true);
        let fake = Fake::new(Ok(OK));
        assert_eq!(upload(&root, &on(), &fake).unwrap().sent, 1);
        assert_eq!(upload(&root, &on(), &fake).unwrap(), Summary::default());
        assert_eq!(fake.bodies.borrow().len(), 1);
        assert!(root.join("outbox").join(name).exists());
    }

    #[test]
    fn games_with_an_untagged_human_are_never_sent() {
        let root = temp_root("untagged");
        put(&root, 10, false);
        let fake = Fake::new(Ok(OK));
        let s = upload(&root, &on(), &fake).unwrap();
        assert_eq!((s.skipped, s.sent), (1, 0));
        assert!(fake.bodies.borrow().is_empty());
    }

    #[test]
    fn games_without_human_opponents_are_never_sent() {
        let root = temp_root("vs-ai");
        let mut bot = player(1, "Azeroth", false);
        bot.kind = Kind::Computer;
        put_with(&root, 10, vec![player(0, "Ann", true), bot]);
        let mut ally = player(1, "Bob", true);
        ally.team = 1;
        put_with(&root, 11, vec![player(0, "Ann", true), ally]);
        let fake = Fake::new(Ok(OK));
        let s = upload(&root, &on(), &fake).unwrap();
        assert_eq!((s.skipped, s.sent), (2, 0));
        assert!(fake.bodies.borrow().is_empty());
    }

    #[test]
    fn a_network_failure_keeps_everything_for_the_next_run() {
        let root = temp_root("down");
        put(&root, 10, true);
        let down = Fake::new(Err(Error::new("network", "offline")));
        assert!(upload(&root, &on(), &down).is_err());
        assert_eq!(upload(&root, &on(), &Fake::new(Ok(OK))).unwrap().sent, 1);
    }

    #[test]
    fn a_refusal_is_remembered_and_not_retried() {
        let root = temp_root("refused");
        put(&root, 10, true);
        let fake = Fake::new(Ok(r#"{"results":[{"ok":false,"why":"too old"}]}"#));
        assert_eq!(upload(&root, &on(), &fake).unwrap().rejected, 1);
        assert_eq!(upload(&root, &on(), &fake).unwrap(), Summary::default());
        assert_eq!(fake.bodies.borrow().len(), 1);
    }

    #[test]
    fn a_wrong_sized_answer_is_an_error_and_settles_nothing() {
        let root = temp_root("short");
        put(&root, 10, true);
        assert!(upload(&root, &on(), &Fake::new(Ok(r#"{"results":[]}"#))).is_err());
        assert_eq!(upload(&root, &on(), &Fake::new(Ok(OK))).unwrap().sent, 1);
    }
}
