//! The two per-plugin stores behind the implicit host functions (docs/abi.md): `settings_*`
//! (`config\<plugin-id>.json`) and `data_*` (`plugins\<plugin-id>\data\`). Pure `std::fs` over
//! paths the caller chose, so every rule is testable without a game. The plugin never sees a path.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::json::{self, Number, Value};

// ---- settings -----------------------------------------------------------------------------

pub const MAX_KEYS: usize = 128;
pub const MAX_KEY_BYTES: usize = 64;
pub const MAX_STR_BYTES: usize = 1024;
pub const MAX_SETTINGS_FILE: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Int(i64),
    Str(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SetError {
    BadKey,
    TooManyKeys,
    TooLarge,
    Io,
}

/// A plugin's settings, loaded once and written through on every change.
pub struct Settings {
    path: PathBuf,
    map: BTreeMap<String, Val>,
}

pub fn valid_key(key: &str) -> bool {
    (1..=MAX_KEY_BYTES).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

impl Settings {
    /// Reads `path`. A missing file is empty; a bad one is empty with a warning (it is replaced
    /// only when the plugin changes a setting, like `host.json`).
    pub fn open(path: PathBuf) -> (Settings, Option<String>) {
        let (map, warning) = match read_bounded(&path, MAX_SETTINGS_FILE) {
            Ok(raw) => match parse_settings(&raw) {
                Ok(map) => (map, None),
                Err(code) => (
                    BTreeMap::new(),
                    Some(format!("settings file refused ({code}); starting empty")),
                ),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => (BTreeMap::new(), None),
            Err(e) => (
                BTreeMap::new(),
                Some(format!("settings file unreadable ({e}); starting empty")),
            ),
        };
        (Settings { path, map }, warning)
    }

    pub fn get(&self, key: &str) -> Option<&Val> {
        self.map.get(key)
    }

    pub fn set(&mut self, key: &str, value: Val) -> Result<(), SetError> {
        if !valid_key(key) {
            return Err(SetError::BadKey);
        }
        if let Val::Str(s) = &value {
            if s.len() > MAX_STR_BYTES {
                return Err(SetError::TooLarge);
            }
        }
        if !self.map.contains_key(key) && self.map.len() >= MAX_KEYS {
            return Err(SetError::TooManyKeys);
        }
        let before = self.map.insert(key.to_owned(), value);
        match self.persist() {
            Ok(()) => Ok(()),
            Err(e) => {
                // Keep memory and disk in step: undo the change that could not be saved.
                match before {
                    Some(v) => self.map.insert(key.to_owned(), v),
                    None => self.map.remove(key),
                };
                Err(e)
            }
        }
    }

    fn persist(&self) -> Result<(), SetError> {
        let values = self
            .map
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    Val::Int(i) => Value::Number(Number {
                        text: i.to_string(),
                    }),
                    Val::Str(s) => Value::String(s.clone()),
                };
                (k.clone(), v)
            })
            .collect();
        let text = Value::Object(vec![
            (
                "schema".to_owned(),
                Value::Number(Number { text: "1".into() }),
            ),
            ("values".to_owned(), Value::Object(values)),
        ])
        .to_json();
        if text.len() > MAX_SETTINGS_FILE {
            return Err(SetError::TooLarge);
        }
        write_atomic(&self.path, text.as_bytes()).map_err(|_| SetError::Io)
    }
}

fn parse_settings(raw: &[u8]) -> Result<BTreeMap<String, Val>, &'static str> {
    let doc = json::parse(raw).map_err(|_| "not_json")?;
    let fields = doc.as_object().ok_or("not_object")?;
    if fields.len() != 2 {
        return Err("bad_fields");
    }
    if doc
        .get("schema")
        .and_then(Value::as_number)
        .and_then(Number::as_u64)
        != Some(1)
    {
        return Err("bad_schema");
    }
    let values = doc
        .get("values")
        .and_then(Value::as_object)
        .ok_or("bad_values")?;
    if values.len() > MAX_KEYS {
        return Err("too_many_keys");
    }
    let mut map = BTreeMap::new();
    for (key, value) in values {
        if !valid_key(key) {
            return Err("bad_key");
        }
        let v = match value {
            Value::Number(n) if n.is_integer() => {
                Val::Int(n.text.parse().map_err(|_| "bad_number")?)
            }
            Value::String(s) if s.len() <= MAX_STR_BYTES => Val::Str(s.clone()),
            _ => return Err("bad_value"),
        };
        map.insert(key.clone(), v);
    }
    Ok(map)
}

fn read_bounded(path: &Path, max: usize) -> io::Result<Vec<u8>> {
    let mut raw = Vec::new();
    File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too large"));
    }
    Ok(raw)
}

/// A temp file next to the target, then a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

// ---- data files ---------------------------------------------------------------------------

pub const MAX_NAME_BYTES: usize = 64;
pub const MAX_FILES: usize = 64;
pub const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_OPEN: usize = 8;

#[derive(Debug, PartialEq, Eq)]
pub enum DataError {
    BadName,
    BadFlags,
    TooManyOpen,
    TooManyFiles,
    TooLarge,
    NotFound,
    BadHandle,
    WrongMode,
    Io,
}

/// A plain file name: 1-64 bytes of `[A-Za-z0-9_.-]`, not starting or ending with `.` (Windows
/// drops trailing dots), and not a device name with or without an extension.
pub fn valid_file_name(name: &str) -> bool {
    if !(1..=MAX_NAME_BYTES).contains(&name.len()) {
        return false;
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return false;
    }
    if name.starts_with('.') || name.ends_with('.') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|p| {
        stem.strip_prefix(p)
            .is_some_and(|d| d.len() == 1 && d != "0" && d.bytes().all(|b| b.is_ascii_digit()))
    });
    !device
}

enum Mode {
    Read,
    Write,
}

struct Open {
    handle: u32,
    name: String,
    file: File,
    mode: Mode,
}

/// One plugin's data folder and its open handles.
pub struct DataDir {
    dir: PathBuf,
    open: Vec<Open>,
    next_handle: u32,
}

impl DataDir {
    pub fn new(dir: PathBuf) -> DataDir {
        DataDir {
            dir,
            open: Vec::new(),
            next_handle: 1,
        }
    }

    /// `write` is the plugin's `W2R_FILE_WRITE` (create or truncate); otherwise the file must
    /// exist and is read from the start. Exactly one of the two flags.
    pub fn open(&mut self, name: &str, flags: u32) -> Result<u32, DataError> {
        let write = match flags {
            1 => false,
            2 => true,
            _ => return Err(DataError::BadFlags),
        };
        if !valid_file_name(name) {
            return Err(DataError::BadName);
        }
        if self.open.len() >= MAX_OPEN {
            return Err(DataError::TooManyOpen);
        }
        let path = self.dir.join(name);
        // Never follow a link someone put in the folder.
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if !meta.file_type().is_file() {
                return Err(DataError::BadName);
            }
        }
        let (mode, file) = if write {
            fs::create_dir_all(&self.dir).map_err(|_| DataError::Io)?;
            let (files, others) = self.usage(Some(name))?;
            let exists = path.exists();
            if !exists && files >= MAX_FILES {
                return Err(DataError::TooManyFiles);
            }
            if others >= MAX_TOTAL_BYTES {
                return Err(DataError::TooLarge);
            }
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
                .map_err(|_| DataError::Io)?;
            (Mode::Write, file)
        } else {
            match File::open(&path) {
                Ok(f) => (Mode::Read, f),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(DataError::NotFound),
                Err(_) => return Err(DataError::Io),
            }
        };
        let handle = self.next_handle;
        self.next_handle = self.next_handle.wrapping_add(1).max(1);
        self.open.push(Open {
            handle,
            name: name.to_owned(),
            file,
            mode,
        });
        Ok(handle)
    }

    /// Up to `cap` bytes from the current position; empty at end of file.
    pub fn read(&mut self, handle: u32, cap: usize) -> Result<Vec<u8>, DataError> {
        let f = self.find(handle)?;
        if !matches!(f.mode, Mode::Read) {
            return Err(DataError::WrongMode);
        }
        let mut buf = vec![0u8; cap];
        let n = f.file.read(&mut buf).map_err(|_| DataError::Io)?;
        buf.truncate(n);
        Ok(buf)
    }

    pub fn write(&mut self, handle: u32, bytes: &[u8]) -> Result<(), DataError> {
        let idx = self
            .open
            .iter()
            .position(|o| o.handle == handle)
            .ok_or(DataError::BadHandle)?;
        if !matches!(self.open[idx].mode, Mode::Write) {
            return Err(DataError::WrongMode);
        }
        let name = self.open[idx].name.clone();
        let (_, others) = self.usage(Some(&name))?;
        let own = self.open[idx]
            .file
            .metadata()
            .map_err(|_| DataError::Io)?
            .len();
        if others + own + bytes.len() as u64 > MAX_TOTAL_BYTES {
            return Err(DataError::TooLarge);
        }
        self.open[idx]
            .file
            .write_all(bytes)
            .map_err(|_| DataError::Io)
    }

    pub fn close(&mut self, handle: u32) -> Result<(), DataError> {
        let idx = self
            .open
            .iter()
            .position(|o| o.handle == handle)
            .ok_or(DataError::BadHandle)?;
        self.open.remove(idx);
        Ok(())
    }

    fn find(&mut self, handle: u32) -> Result<&mut Open, DataError> {
        self.open
            .iter_mut()
            .find(|o| o.handle == handle)
            .ok_or(DataError::BadHandle)
    }

    /// File count and total bytes of the folder, not counting `except`.
    fn usage(&self, except: Option<&str>) -> Result<(usize, u64), DataError> {
        let mut files = 0;
        let mut bytes = 0;
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
            Err(_) => return Err(DataError::Io),
        };
        for entry in entries.flatten() {
            files += 1;
            if except.is_some_and(|n| entry.file_name().to_string_lossy() == n) {
                continue;
            }
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
        Ok((files, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("w2r-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn keys_and_file_names_follow_the_abi_rules() {
        assert!(valid_key("a.b_c9"));
        assert!(!valid_key(""));
        assert!(!valid_key("A"));
        assert!(!valid_key("a-b"));
        assert!(valid_key(&"k".repeat(64)));
        assert!(!valid_key(&"k".repeat(65)));
        assert!(valid_file_name("chat-2026_10.log"));
        for bad in [
            "",
            ".hidden",
            "trailing.",
            "a b",
            "a/b",
            "a\\b",
            "con",
            "CON.txt",
            "nul",
            "Com1",
            "lpt9.log",
            "aux.x.y",
        ] {
            assert!(!valid_file_name(bad), "{bad}");
        }
        assert!(valid_file_name("com0"), "COM0 is not a device");
        assert!(valid_file_name("console.log"));
        assert!(!valid_file_name(&"n".repeat(65)));
    }

    #[test]
    fn settings_round_trip_and_enforce_their_limits() {
        let dir = temp("settings");
        let path = dir.join("p.json");
        let (mut s, warn) = Settings::open(path.clone());
        assert!(warn.is_none());
        assert_eq!(s.get("x"), None);
        s.set("lines", Val::Int(8)).unwrap();
        s.set("name", Val::Str("héllo \"q\"".into())).unwrap();
        s.set("neg", Val::Int(i64::MIN)).unwrap();
        let (again, warn) = Settings::open(path.clone());
        assert!(warn.is_none());
        assert_eq!(again.get("lines"), Some(&Val::Int(8)));
        assert_eq!(again.get("name"), Some(&Val::Str("héllo \"q\"".into())));
        assert_eq!(again.get("neg"), Some(&Val::Int(i64::MIN)));

        assert_eq!(s.set("Bad", Val::Int(1)), Err(SetError::BadKey));
        assert_eq!(
            s.set("long", Val::Str("x".repeat(MAX_STR_BYTES + 1))),
            Err(SetError::TooLarge)
        );
        s.set("long", Val::Str("x".repeat(MAX_STR_BYTES))).unwrap();
        for i in s.map.len()..MAX_KEYS {
            s.set(&format!("k{i}"), Val::Int(i as i64)).unwrap();
        }
        assert_eq!(s.set("one.more", Val::Int(0)), Err(SetError::TooManyKeys));
        s.set("lines", Val::Int(9)).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_settings_file_starts_empty_and_is_replaced_on_write() {
        let dir = temp("badsettings");
        let path = dir.join("p.json");
        for bad in [
            &b"not json"[..],
            br#"{"schema":2,"values":{}}"#,
            br#"{"schema":1,"values":{"Bad Key":1}}"#,
            br#"{"schema":1,"values":{"a":1.5}}"#,
            br#"{"schema":1,"values":{"a":true}}"#,
            br#"{"schema":1,"values":{},"extra":1}"#,
        ] {
            fs::write(&path, bad).unwrap();
            let (s, warn) = Settings::open(path.clone());
            assert!(warn.is_some(), "{}", String::from_utf8_lossy(bad));
            assert_eq!(s.get("a"), None);
        }
        fs::write(&path, b"junk").unwrap();
        let (mut s, _) = Settings::open(path.clone());
        s.set("a", Val::Int(1)).unwrap();
        let (s, warn) = Settings::open(path);
        assert!(warn.is_none());
        assert_eq!(s.get("a"), Some(&Val::Int(1)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_save_leaves_the_old_value() {
        let dir = temp("savefail");
        // The target's parent is a file, so every write fails.
        let blocker = dir.join("blocker");
        fs::write(&blocker, b"x").unwrap();
        let (mut s, _) = Settings::open(blocker.join("p.json"));
        assert_eq!(s.set("a", Val::Int(1)), Err(SetError::Io));
        assert_eq!(s.get("a"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_files_read_write_and_respect_their_limits() {
        let dir = temp("data");
        let mut d = DataDir::new(dir.join("data"));
        assert_eq!(d.open("nope.txt", 1), Err(DataError::NotFound));
        assert_eq!(d.open("x", 3), Err(DataError::BadFlags));
        assert_eq!(d.open("x", 0), Err(DataError::BadFlags));
        assert_eq!(d.open("../x", 2), Err(DataError::BadName));
        let w = d.open("a.log", 2).unwrap();
        d.write(w, b"hello ").unwrap();
        d.write(w, b"world").unwrap();
        assert_eq!(d.read(w, 4), Err(DataError::WrongMode));
        d.close(w).unwrap();
        assert_eq!(d.close(w), Err(DataError::BadHandle));
        let r = d.open("a.log", 1).unwrap();
        assert_eq!(d.write(r, b"x"), Err(DataError::WrongMode));
        assert_eq!(d.read(r, 5).unwrap(), b"hello");
        assert_eq!(d.read(r, 100).unwrap(), b" world");
        assert!(d.read(r, 100).unwrap().is_empty());
        d.close(r).unwrap();
        // Opening for write truncates.
        let w = d.open("a.log", 2).unwrap();
        d.close(w).unwrap();
        assert_eq!(
            fs::metadata(dir.join("data").join("a.log")).unwrap().len(),
            0
        );

        // Open handles are capped.
        let mut handles = Vec::new();
        for i in 0..MAX_OPEN {
            handles.push(d.open(&format!("f{i}"), 2).unwrap());
        }
        assert_eq!(d.open("one-more", 2), Err(DataError::TooManyOpen));
        for h in handles {
            d.close(h).unwrap();
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_count_and_total_size_are_capped() {
        let dir = temp("caps");
        let mut d = DataDir::new(dir.join("data"));
        for i in 0..MAX_FILES {
            let h = d.open(&format!("f{i}"), 2).unwrap();
            d.close(h).unwrap();
        }
        assert_eq!(d.open("extra", 2), Err(DataError::TooManyFiles));
        // An existing file can still be reopened.
        let h = d.open("f0", 2).unwrap();
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..16 {
            d.write(h, &chunk).unwrap();
        }
        assert_eq!(d.write(h, b"x"), Err(DataError::TooLarge));
        d.close(h).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_link_in_the_folder_is_not_followed() {
        let dir = temp("link");
        let data = dir.join("data");
        fs::create_dir_all(&data).unwrap();
        let secret = dir.join("secret.txt");
        fs::write(&secret, b"s").unwrap();
        // Needs privilege or developer mode on Windows; skip quietly when unavailable.
        if std::os::windows::fs::symlink_file(&secret, data.join("l.txt")).is_ok() {
            let mut d = DataDir::new(data);
            assert_eq!(d.open("l.txt", 1), Err(DataError::BadName));
            assert_eq!(d.open("l.txt", 2), Err(DataError::BadName));
            assert_eq!(fs::read(&secret).unwrap(), b"s");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
