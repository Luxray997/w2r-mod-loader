//! Network access and the index cache (docs/manager.md, slice 4). This is the only code that
//! touches the network; everything it fetches is checked before use: the index by the spec rules,
//! a package by the SHA-256 and size in its index entry.

use crate::Error;
use crate::index::{self, Index, MAX_INDEX_BYTES, VersionEntry};
use crate::store::{Root, read_bounded};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// Fetches the bytes behind an `https` URL, refusing anything over `max_bytes`.
pub trait Fetcher {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, Error>;
}

/// The real fetcher: HTTPS only (also across redirects), at most 3 redirects, 60 s overall.
pub struct Https {
    agent: ureq::Agent,
}

impl Default for Https {
    fn default() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(3)
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(concat!("w2rpm/", env!("CARGO_PKG_VERSION")))
            .build();
        Https {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Fetcher for Https {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, Error> {
        if !index::url_ok(url) {
            return Err(Error::new("bad_url", url.to_owned()));
        }
        let mut resp = self
            .agent
            .get(url)
            .call()
            .map_err(|e| Error::new("network", format!("{url}: {e}")))?;
        resp.body_mut()
            .with_config()
            .limit(max_bytes)
            .read_to_vec()
            .map_err(|e| Error::new("network", format!("{url}: {e}")))
    }
}

/// A development source: serves `https://.../name` from `dir/name`, so a test index and
/// its packages work offline. The URLs still go through the index rules; only the transport differs.
pub struct DirFetcher(pub std::path::PathBuf);

impl Fetcher for DirFetcher {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, Error> {
        if !index::url_ok(url) {
            return Err(Error::new("bad_url", url.to_owned()));
        }
        let name = url.rsplit('/').next().unwrap_or("");
        let safe = !name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
            && !name.starts_with('.');
        if !safe {
            return Err(Error::new(
                "bad_url",
                format!("{name:?} is not a plain file name"),
            ));
        }
        read_bounded(&self.0.join(name), max_bytes as usize)
            .map_err(|e| Error::new("network", e.to_string()))
    }
}

/// Where the index lives.
#[derive(Clone, Debug)]
pub struct Source {
    pub index_url: String,
}

fn serial_path(root: &Root) -> std::path::PathBuf {
    root.cache_dir().join("index.serial")
}

/// The highest serial accepted so far (0 when none).
pub fn cached_serial(root: &Root) -> u64 {
    read_bounded(&serial_path(root), 32)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Temp file, then rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Fetches the index, checks it against the cached serial, and only then replaces the cache. A
/// refused index leaves the previous cache untouched.
pub fn refresh(root: &Root, fetcher: &dyn Fetcher, src: &Source) -> Result<Index, Error> {
    let raw = fetcher.get(&src.index_url, MAX_INDEX_BYTES as u64)?;
    let idx = index::parse_index(&raw, cached_serial(root))
        .map_err(|r| Error::new(r, "the fetched index was refused"))?;
    fs::create_dir_all(root.cache_dir())?;
    // The serial goes first: a crash after it can only make the next check stricter, never let
    // an older index through.
    write_atomic(&serial_path(root), idx.serial.to_string().as_bytes())?;
    write_atomic(&root.cache_dir().join("index.json"), &raw)?;
    Ok(idx)
}

/// Loads and re-checks the cached index (the cache is never trusted unparsed).
pub fn load_cached(root: &Root) -> Result<Index, Error> {
    let raw = read_bounded(&root.cache_dir().join("index.json"), MAX_INDEX_BYTES).map_err(|e| {
        if e.code == "not_found" {
            Error::new("no_index", "run refresh first")
        } else {
            e
        }
    })?;
    index::parse_index(&raw, cached_serial(root))
        .map_err(|r| Error::new(r, "the cached index was refused"))
}

/// Downloads the package for a verified index entry (at most its declared size).
pub fn download(fetcher: &dyn Fetcher, entry: &VersionEntry) -> Result<Vec<u8>, Error> {
    fetcher.get(&entry.url, entry.size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempRoot;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use w2r_formats::json::{self, Value};

    const VECTORS: &str = include_str!("../../../docs/vectors/index.json");
    const URL: &str = "https://example.com/index.json";

    #[derive(Default)]
    struct Fake {
        files: RefCell<HashMap<String, Vec<u8>>>,
        asked: RefCell<Vec<(String, u64)>>,
    }

    impl Fake {
        fn serve(&self, index: &str) {
            self.files
                .borrow_mut()
                .insert(URL.to_owned(), index.as_bytes().to_vec());
        }
    }

    impl Fetcher for Fake {
        fn get(&self, url: &str, max: u64) -> Result<Vec<u8>, Error> {
            self.asked.borrow_mut().push((url.to_owned(), max));
            self.files
                .borrow()
                .get(url)
                .cloned()
                .ok_or_else(|| Error::new("network", "404"))
        }
    }

    fn vector(name: &str) -> String {
        let doc = json::parse(VECTORS.as_bytes()).unwrap();
        for c in doc.get("cases").unwrap().as_array().unwrap() {
            if c.get("name").and_then(Value::as_str) == Some(name) {
                return c.get("index").unwrap().as_str().unwrap().to_owned();
            }
        }
        panic!("no vector {name}");
    }

    fn source() -> Source {
        Source {
            index_url: URL.to_owned(),
        }
    }

    #[test]
    fn refresh_caches_a_checked_index_and_keeps_it_when_a_later_one_is_refused() {
        let t = TempRoot::new("refresh");
        let src = source();
        let fake = Fake::default();
        assert_eq!(load_cached(&t.0).unwrap_err().code, "no_index");

        fake.serve(&vector("valid"));
        let got = refresh(&t.0, &fake, &src).unwrap();
        assert_eq!(got.serial, 7);
        assert_eq!(cached_serial(&t.0), 7);
        assert_eq!(load_cached(&t.0).unwrap().serial, 7);
        let asked = fake.asked.borrow().clone();
        assert_eq!(asked, vec![(URL.to_owned(), MAX_INDEX_BYTES as u64)]);

        // An older index is a replay or a stale mirror: refused, cache untouched.
        fake.serve(&vector("rollback"));
        assert_eq!(refresh(&t.0, &fake, &src).unwrap_err().code, "rollback");
        // A malformed one is refused too.
        fake.serve(&vector("not_json"));
        assert_eq!(refresh(&t.0, &fake, &src).unwrap_err().code, "not_json");
        assert_eq!(cached_serial(&t.0), 7);
        assert_eq!(load_cached(&t.0).unwrap().serial, 7);
    }

    #[test]
    fn the_cache_is_rechecked_on_load() {
        let t = TempRoot::new("recheck");
        let fake = Fake::default();
        fake.serve(&vector("valid"));
        refresh(&t.0, &fake, &source()).unwrap();
        let path = t.0.cache_dir().join("index.json");
        fs::write(&path, b"{").unwrap();
        assert_eq!(load_cached(&t.0).unwrap_err().code, "not_json");
    }

    #[test]
    fn download_asks_for_at_most_the_declared_size() {
        let (e, z) = crate::testutil::package("1.0.0", &[], b"dll");
        let fake = Fake::default();
        fake.files.borrow_mut().insert(e.url.clone(), z.clone());
        assert_eq!(download(&fake, &e).unwrap(), z);
        assert_eq!(fake.asked.borrow()[0].1, e.size);
    }

    #[test]
    fn the_real_fetcher_refuses_plain_http_before_any_connection() {
        let h = Https::default();
        assert_eq!(
            h.get("http://example.com/x", 10).unwrap_err().code,
            "bad_url"
        );
    }
}
