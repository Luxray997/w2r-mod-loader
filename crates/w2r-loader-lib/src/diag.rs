//! Diagnostics bundle: the logs and settings a bug report needs, scrubbed of personal details,
//! zipped, and (when the user asks) sent to the stats service as `PUT /v1/diagnostics`.
//!
//! Only a fixed list of files is ever read (the host log, the share log, `config\*.json` except
//! the installation key, each plugin's `plugin.json`, and crash dumps if the user ticks them).
//! Replays, chat logs and the outbox are never touched. Text files go through [`Scrubber`]:
//!
//! - the Windows user name, profile folder name and computer name become `<user>` and `<pc>`;
//! - BattleTags (`Name#1234`) become `playerN`, and so does the bare name wherever it appears,
//!   the same `N` for the same person throughout the bundle, so a lobby can still be followed;
//! - IPv4 addresses become `ipN` (loopback stays).
//!
//! Crash dumps are binary memory snapshots and cannot be scrubbed, which is why they are opt-in.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use w2r_stats::share::{Poster, Settings};
use w2r_stats::{blob, hex, identity};

use crate::Error;

/// Largest zip the service takes, and the client's own limit.
pub const MAX_ZIP: usize = 4 * 1024 * 1024;
/// How much of the end of each log is kept, tried in turn until the zip fits.
const TAIL_STEPS: [usize; 3] = [3 * 1024 * 1024, 1024 * 1024, 256 * 1024];
/// The first lines of the host log (versions, root, build check) are always kept.
const HEAD_LINES: usize = 300;
const MAX_CONFIG_FILE: u64 = 64 * 1024;
const MAX_DUMPS: usize = 2;
pub const MAX_NOTE: usize = 2000;

/// Who the machine belongs to, as far as logs may mention it.
#[derive(Clone, Debug, Default)]
pub struct Private {
    pub user: Vec<String>,
    pub computer: Vec<String>,
}

impl Private {
    pub fn from_env() -> Private {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let mut user = Vec::new();
        user.extend(get("USERNAME"));
        if let Some(p) = get("USERPROFILE")
            && let Some(name) = Path::new(&p).file_name()
        {
            user.push(name.to_string_lossy().into_owned());
        }
        Private {
            user,
            computer: get("COMPUTERNAME").into_iter().collect(),
        }
    }
}

/// Words that appear in the logs themselves; a player called "Host" must not turn `host:` into
/// `player3:`. Bare names on this list are left alone (their tagged form is still replaced).
const LOG_WORDS: [&str; 33] = [
    "host", "info", "warn", "warning", "error", "debug", "game", "lobby", "map", "chat", "player",
    "plugin", "core", "none", "true", "false", "null", "human", "computer", "team", "slot",
    "build", "guard", "root", "the", "and", "for", "not", "you", "new", "old", "off", "set",
];

pub struct Scrubber {
    literals: Vec<(String, &'static str)>,
    names: BTreeMap<String, usize>,
    ips: BTreeMap<String, usize>,
}

impl Scrubber {
    pub fn new(private: &Private) -> Scrubber {
        let mut literals: Vec<(String, &'static str)> = Vec::new();
        for (list, with) in [(&private.user, "<user>"), (&private.computer, "<pc>")] {
            for n in list {
                if n.chars().count() >= 3 && !literals.iter().any(|(l, _)| l.eq_ignore_ascii_case(n)) {
                    literals.push((n.clone(), with));
                }
            }
        }
        literals.sort_by_key(|(l, _)| std::cmp::Reverse(l.len()));
        Scrubber {
            literals,
            names: BTreeMap::new(),
            ips: BTreeMap::new(),
        }
    }

    /// Notes every BattleTag in `text` so its name gets a number. Call on all files before
    /// [`apply`](Self::apply), so a name seen bare in one file and tagged in another is caught.
    pub fn learn(&mut self, text: &str) {
        for (name, _) in tags_in(text) {
            let next = self.names.len() + 1;
            self.names.entry(name.to_lowercase()).or_insert(next);
        }
    }

    pub fn apply(&mut self, text: &str) -> String {
        let mut s = text.to_owned();
        for (lit, with) in &self.literals {
            s = replace_ci(&s, lit, with);
        }
        let s = self.names_pass(&s);
        self.ip_pass(&s)
    }

    fn names_pass(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.char_indices().peekable();
        while let Some((start, c)) = chars.next() {
            if !c.is_alphanumeric() {
                out.push(c);
                continue;
            }
            let mut end = start + c.len_utf8();
            while let Some(&(i, d)) = chars.peek() {
                if !d.is_alphanumeric() {
                    break;
                }
                end = i + d.len_utf8();
                chars.next();
            }
            let word = &s[start..end];
            let key = word.to_lowercase();
            let tag_len = tag_digits(&s[end..]);
            match self.names.get(&key) {
                Some(n) if tag_len > 0 => {
                    out.push_str(&format!("player{n}"));
                    for _ in 0..s[end..end + tag_len].chars().count() {
                        chars.next();
                    }
                }
                Some(n)
                    if word.chars().count() >= 3 && !LOG_WORDS.contains(&key.as_str()) =>
                {
                    out.push_str(&format!("player{n}"));
                }
                _ => out.push_str(word),
            }
        }
        out
    }

    fn ip_pass(&mut self, s: &str) -> String {
        let b = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let (mut i, mut last) = (0, 0);
        while i < b.len() {
            let starts = b[i].is_ascii_digit() && (i == 0 || !(b[i - 1].is_ascii_digit() || b[i - 1] == b'.'));
            if starts && let Some(end) = ipv4_at(b, i) {
                let after_ok = end == b.len()
                    || !(b[end].is_ascii_digit()
                        || (b[end] == b'.' && b.get(end + 1).is_some_and(u8::is_ascii_digit)));
                let ip = &s[i..end];
                if after_ok && !ip.starts_with("127.") && ip != "0.0.0.0" {
                    let next = self.ips.len() + 1;
                    let n = *self.ips.entry(ip.to_owned()).or_insert(next);
                    out.push_str(&s[last..i]);
                    out.push_str(&format!("ip{n}"));
                    last = end;
                    i = end;
                    continue;
                }
            }
            i += 1;
        }
        out.push_str(&s[last..]);
        out
    }
}

fn replace_ci(s: &str, needle: &str, with: &str) -> String {
    let hay = s.to_ascii_lowercase();
    let n = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for (i, _) in hay.match_indices(&n) {
        out.push_str(&s[last..i]);
        out.push_str(with);
        last = i + n.len();
    }
    out.push_str(&s[last..]);
    out
}

/// Length in bytes of `#` plus 4 to 8 digits at the start of `s` (a BattleTag's number), or 0.
fn tag_digits(s: &str) -> usize {
    let b = s.as_bytes();
    if b.first() != Some(&b'#') {
        return 0;
    }
    let digits = b[1..].iter().take_while(|c| c.is_ascii_digit()).count();
    if (4..=8).contains(&digits) { 1 + digits } else { 0 }
}

/// Every `Name#1234` in `text`: the name (3 to 12 letters or digits) and the whole tag's length.
fn tags_in(text: &str) -> Vec<(String, usize)> {
    let mut found = Vec::new();
    for (i, _) in text.match_indices('#') {
        let len = tag_digits(&text[i..]);
        if len == 0 {
            continue;
        }
        let name: Vec<char> = text[..i]
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric())
            .collect();
        if (3..=12).contains(&name.len()) {
            found.push((name.into_iter().rev().collect(), len));
        }
    }
    found
}

/// End of a dotted-quad starting at `i`, if one does.
fn ipv4_at(b: &[u8], i: usize) -> Option<usize> {
    let mut at = i;
    for part in 0..4 {
        let digits = b[at..].iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=3).contains(&digits) {
            return None;
        }
        let v: u32 = std::str::from_utf8(&b[at..at + digits]).ok()?.parse().ok()?;
        if v > 255 {
            return None;
        }
        at += digits;
        if part < 3 {
            if b.get(at) != Some(&b'.') {
                return None;
            }
            at += 1;
        }
    }
    Some(at)
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub bytes: Vec<u8>,
    /// False for crash dumps, which are included as they are.
    pub scrubbed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub include_dumps: bool,
    /// What the user wrote for the developer (cut to [`MAX_NOTE`]).
    pub note: String,
    /// Extra lines for `summary.txt` (the manager's own view: core, game folder, plugins).
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct Bundle {
    pub entries: Vec<Entry>,
}

impl Bundle {
    pub fn total(&self) -> usize {
        self.entries.iter().map(|e| e.bytes.len()).sum()
    }
}

/// Keeps the first `head` lines and the last `tail` bytes (from a line start) of a log.
fn trim_log(raw: &[u8], head: usize, tail: usize) -> Vec<u8> {
    if raw.len() <= tail + 64 * 1024 {
        return raw.to_vec();
    }
    let mut cut = raw.len() - tail;
    while cut < raw.len() && raw[cut - 1] != b'\n' {
        cut += 1;
    }
    let mut head_end = 0;
    for _ in 0..head {
        match raw[head_end..cut].iter().position(|&c| c == b'\n') {
            Some(p) => head_end += p + 1,
            None => break,
        }
    }
    let mut out = raw[..head_end].to_vec();
    out.extend_from_slice(format!("*** {} bytes omitted ***\n", cut - head_end).as_bytes());
    out.extend_from_slice(&raw[cut..]);
    out
}

fn list(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(ext))
        })
        .collect();
    v.sort();
    v
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Reads the fixed list of files under `root` and scrubs them. `tail` is how much of each log
/// to keep.
pub fn collect(root: &Path, opts: &Options, private: &Private, tail: usize) -> Bundle {
    let mut raw: Vec<(String, Vec<u8>)> = Vec::new();
    let logs = root.join("logs");
    for (file, head) in [("w2r.log", HEAD_LINES), ("w2r.1.log", 20), ("share.log", 0)] {
        if let Ok(b) = fs::read(logs.join(file)) {
            raw.push((format!("logs/{file}"), trim_log(&b, head, tail)));
        }
    }
    for p in list(&root.join("config"), "json") {
        let name = name_of(&p);
        // The installation key signs reports; its secret never leaves the machine.
        if name == "identity.json" || p.metadata().map_or(true, |m| m.len() > MAX_CONFIG_FILE) {
            continue;
        }
        if let Ok(b) = fs::read(&p) {
            raw.push((format!("config/{name}"), b));
        }
    }
    let mut plugins: Vec<PathBuf> = fs::read_dir(root.join("plugins"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    plugins.sort();
    for dir in plugins {
        let manifest = dir.join("plugin.json");
        if manifest.metadata().is_ok_and(|m| m.len() <= MAX_CONFIG_FILE)
            && let Ok(b) = fs::read(&manifest)
        {
            raw.push((format!("plugins/{}/plugin.json", name_of(&dir)), b));
        }
    }

    let mut summary = format!(
        "W2R Mod Loader {}\nroot: {}\ntime: {} (unix seconds)\nos: {}\n",
        env!("CARGO_PKG_VERSION"),
        root.display(),
        crate::session::now().unwrap_or(0),
        std::env::consts::OS,
    );
    if !opts.summary.is_empty() {
        summary.push('\n');
        summary.push_str(&opts.summary);
    }
    raw.insert(0, ("summary.txt".into(), summary.into_bytes()));
    let note: String = opts.note.chars().take(MAX_NOTE).collect();
    if !note.trim().is_empty() {
        raw.insert(1, ("notes.txt".into(), note.into_bytes()));
    }

    let texts: Vec<(String, String)> = raw
        .into_iter()
        .map(|(n, b)| (n, String::from_utf8_lossy(&b).into_owned()))
        .collect();
    let mut scrubber = Scrubber::new(private);
    for (_, t) in &texts {
        scrubber.learn(t);
    }
    let mut entries: Vec<Entry> = texts
        .iter()
        .map(|(n, t)| Entry {
            name: n.clone(),
            bytes: scrubber.apply(t).into_bytes(),
            scrubbed: true,
        })
        .collect();

    if opts.include_dumps {
        let mut dumps = list(&logs, "dmp");
        dumps.sort_by_key(|p| std::cmp::Reverse(p.metadata().and_then(|m| m.modified()).ok()));
        for p in dumps.into_iter().take(MAX_DUMPS) {
            if p.metadata().is_ok_and(|m| m.len() <= 2 * 1024 * 1024)
                && let Ok(b) = fs::read(&p)
            {
                entries.push(Entry {
                    name: format!("logs/{}", name_of(&p)),
                    bytes: b,
                    scrubbed: false,
                });
            }
        }
    }
    Bundle { entries }
}

pub fn to_zip(bundle: &Bundle) -> Result<Vec<u8>, Error> {
    use zip::write::SimpleFileOptions;
    let bad = |e: &dyn std::fmt::Display| Error::new("io", format!("could not build the zip: {e}"));
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for e in &bundle.entries {
        w.start_file(e.name.as_str(), options).map_err(|x| bad(&x))?;
        w.write_all(&e.bytes).map_err(|x| bad(&x))?;
    }
    Ok(w.finish().map_err(|x| bad(&x))?.into_inner())
}

/// The bundle and its zip, shrinking the log tails until the zip fits [`MAX_ZIP`].
pub fn build(root: &Path, opts: &Options, private: &Private) -> Result<(Bundle, Vec<u8>), Error> {
    for tail in TAIL_STEPS {
        let bundle = collect(root, opts, private, tail);
        let zip = to_zip(&bundle)?;
        if zip.len() <= MAX_ZIP {
            return Ok((bundle, zip));
        }
    }
    Err(Error::new(
        "too_big",
        "the diagnostics are too large to send even with the logs cut short; leave out the crash dumps",
    ))
}

/// Sends `zip`, signed with the installation key when there is one. Returns the report code the
/// user gives the developer.
pub fn send(root: &Path, settings: &Settings, poster: &dyn Poster, zip: &[u8]) -> Result<String, Error> {
    let url = settings
        .endpoint_for("v1/diagnostics")
        .map_err(|e| Error::new(e.code, e.detail))?;
    let mut headers: Vec<(&str, String)> = vec![("content-type", "application/zip".into())];
    if let Ok(id) = identity::load_or_create(&root.join("config").join("identity.json"), || None) {
        headers.push(("x-w2r-key", hex::encode(&id.public())));
        headers.push(("x-w2r-sig", hex::encode(&blob::sign_diag(&id, zip))));
    }
    let (status, answer) = poster
        .put_bytes(&url, &headers, zip)
        .map_err(|e| Error::new(e.code, e.detail))?;
    let text = String::from_utf8_lossy(&answer);
    if status == 200
        && let Ok(v) = w2r_formats::json::parse(&answer)
        && let Some(code) = v.get("code").and_then(|c| c.as_str())
    {
        return Ok(code.to_owned());
    }
    let why = w2r_formats::json::parse(&answer)
        .ok()
        .and_then(|v| v.get("why").and_then(|w| w.as_str().map(str::to_owned)))
        .unwrap_or_else(|| text.chars().take(200).collect());
    Err(Error::new("refused", format!("the service said {status}: {why}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn private() -> Private {
        Private {
            user: vec!["beau".into()],
            computer: vec!["DESKTOP-ABC".into()],
        }
    }

    fn scrub(texts: &[&str]) -> Vec<String> {
        let mut s = Scrubber::new(&private());
        for t in texts {
            s.learn(t);
        }
        texts.iter().map(|t| s.apply(t)).collect()
    }

    #[test]
    fn user_and_computer_names_go_in_any_case() {
        let out = scrub(&["root C:\\Users\\Beau\\Saved Games on desktop-abc"]);
        assert_eq!(out[0], "root C:\\Users\\<user>\\Saved Games on <pc>");
    }

    #[test]
    fn tags_and_bare_names_get_the_same_number_across_files() {
        let out = scrub(&[
            "slot 2: Ann#1234 joined",
            "chat: Bob#98765 said hi to Ann",
            "Ann left; ann#1234 again",
        ]);
        assert_eq!(out[0], "slot 2: player1 joined");
        assert_eq!(out[1], "chat: player2 said hi to player1");
        assert_eq!(out[2], "player1 left; player1 again");
    }

    #[test]
    fn a_name_that_is_a_log_word_is_only_replaced_with_its_tag() {
        let out = scrub(&["Host#4321 hosts", "INFO  host: ready"]);
        assert_eq!(out[0], "player1 hosts");
        assert_eq!(out[1], "INFO  host: ready");
    }

    #[test]
    fn short_numbers_after_a_hash_are_not_tags() {
        let out = scrub(&["Fix #12 and item#123 at Ann#1234"]);
        assert_eq!(out[0], "Fix #12 and item#123 at player1");
    }

    #[test]
    fn ip_addresses_are_numbered_but_versions_and_loopback_stay() {
        let out = scrub(&[
            "peer 192.168.1.20:6112 and 8.8.8.8, again 192.168.1.20; build 1.0.2.2818 lo 127.0.0.1",
        ]);
        assert_eq!(
            out[0],
            "peer ip1:6112 and ip2, again ip1; build 1.0.2.2818 lo 127.0.0.1"
        );
    }

    #[test]
    fn long_logs_keep_the_start_and_the_end() {
        let mut log = String::new();
        for i in 0..40_000 {
            log.push_str(&format!("line {i:06}\n"));
        }
        let out = String::from_utf8(trim_log(log.as_bytes(), 3, 100_000)).unwrap();
        assert!(out.starts_with("line 000000\nline 000001\nline 000002\n*** "));
        assert!(out.ends_with("line 039999\n"));
        assert!(out.len() < 110_000);
        let tail = out.split("omitted ***\n").nth(1).unwrap();
        assert!(tail.starts_with("line "));
    }

    fn tree() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "w2r-diag-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        for d in ["logs", "config", "plugins/p1", "replays", "plugins/w2r-chat/data"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(root.join("logs/w2r.log"), "root C:\\Users\\beau\\w2r\npeer 10.0.0.5 Ann#1234\n").unwrap();
        fs::write(root.join("logs/hang-1.dmp"), [0u8, 1, 2, 3]).unwrap();
        fs::write(root.join("config/identity.json"), "{\"v\":1,\"secret\":\"00\"}").unwrap();
        fs::write(root.join("config/host.json"), "{\"schema\":1}").unwrap();
        fs::write(root.join("plugins/p1/plugin.json"), "{\"id\":\"p1\"}").unwrap();
        fs::write(root.join("replays/a.w2rr"), "replay").unwrap();
        fs::write(root.join("plugins/w2r-chat/data/chat-1.log"), "private talk").unwrap();
        root
    }

    #[test]
    fn only_the_fixed_files_are_read_and_the_key_never_is() {
        let root = tree();
        let b = collect(&root, &Options::default(), &private(), 1 << 20);
        let names: Vec<&str> = b.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["summary.txt", "logs/w2r.log", "config/host.json", "plugins/p1/plugin.json"]
        );
        let log = String::from_utf8(b.entries[1].bytes.clone()).unwrap();
        assert_eq!(log, "root C:\\Users\\<user>\\w2r\npeer ip1 player1\n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dumps_and_the_note_come_only_when_asked() {
        let root = tree();
        let opts = Options {
            include_dumps: true,
            note: "it froze at the lobby".into(),
            summary: String::new(),
        };
        let b = collect(&root, &opts, &private(), 1 << 20);
        assert_eq!(b.entries[1].name, "notes.txt");
        let dump = b.entries.last().unwrap();
        assert_eq!(dump.name, "logs/hang-1.dmp");
        assert!(!dump.scrubbed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_zip_reads_back() {
        let root = tree();
        let (bundle, zip) = build(&root, &Options::default(), &private()).unwrap();
        let mut a = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
        assert_eq!(a.len(), bundle.entries.len());
        assert!(a.by_name("logs/w2r.log").is_ok());
        let _ = fs::remove_dir_all(root);
    }

    struct Fake {
        status: u16,
        answer: &'static str,
        seen: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl Poster for Fake {
        fn post_json(&self, _: &str, _: &[u8]) -> Result<Vec<u8>, w2r_stats::share::Error> {
            unreachable!()
        }
        fn put_bytes(
            &self,
            url: &str,
            headers: &[(&str, String)],
            _: &[u8],
        ) -> Result<(u16, Vec<u8>), w2r_stats::share::Error> {
            self.seen
                .borrow_mut()
                .push((url.into(), headers.iter().map(|h| h.0.to_string()).collect()));
            Ok((self.status, self.answer.as_bytes().to_vec()))
        }
    }

    #[test]
    fn send_returns_the_code_and_signs_when_a_key_exists() {
        let root = tree();
        let ok = Fake {
            status: 200,
            answer: "{\"ok\":true,\"code\":\"W2R-ABCD-EFGH\"}",
            seen: RefCell::default(),
        };
        let settings = Settings::default();
        // No installation key yet: unsigned.
        fs::remove_file(root.join("config/identity.json")).unwrap();
        assert_eq!(send(&root, &settings, &ok, b"PK").unwrap(), "W2R-ABCD-EFGH");
        assert_eq!(ok.seen.borrow()[0].1, ["content-type"]);
        // With a key: signed.
        identity::load_or_create(&root.join("config/identity.json"), || Some([3; 32])).unwrap();
        send(&root, &settings, &ok, b"PK").unwrap();
        assert!(ok.seen.borrow()[1].1.contains(&"x-w2r-sig".to_string()));
        assert!(ok.seen.borrow()[1].0.ends_with("/v1/diagnostics"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_refusal_carries_the_servers_reason() {
        let root = tree();
        let no = Fake {
            status: 429,
            answer: "{\"ok\":false,\"why\":\"too many today\"}",
            seen: RefCell::default(),
        };
        let e = send(&root, &Settings::default(), &no, b"PK").unwrap_err();
        assert!(e.detail.contains("429") && e.detail.contains("too many today"));
        let _ = fs::remove_dir_all(root);
    }
}
