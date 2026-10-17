//! Network access and the index cache (docs/manager.md, slice 4). This is the only code that
//! touches the network; everything it fetches is checked before use: the index by the spec rules,
//! a package by the SHA-256 and size in its index entry.

use crate::Error;
use crate::index::{self, Index, MAX_INDEX_BYTES, VersionEntry};
use crate::store::{Root, read_bounded};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// Fetches the bytes behind an `https` URL, refusing anything over `max_bytes`.
pub trait Fetcher {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, Error>;

    /// Like `get`, reporting `(bytes so far, total)` as the body arrives; the total is 0 when the
    /// server did not say. The default reports once, at the end.
    fn get_with_progress(
        &self,
        url: &str,
        max_bytes: u64,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Vec<u8>, Error> {
        let bytes = self.get(url, max_bytes)?;
        let n = bytes.len() as u64;
        progress(n, n);
        Ok(bytes)
    }
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
            .user_agent(concat!("W2R-Mod-Loader/", env!("CARGO_PKG_VERSION")))
            .build();
        Https {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Https {
    pub(crate) fn agent(&self) -> &ureq::Agent {
        &self.agent
    }
}

impl Fetcher for Https {
    fn get_with_progress(
        &self,
        url: &str,
        max_bytes: u64,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Vec<u8>, Error> {
        if !index::url_ok(url) {
            return Err(Error::new("bad_url", url.to_owned()));
        }
        let net = |e: &dyn std::fmt::Display| Error::new("network", format!("{url}: {e}"));
        let mut resp = self.agent.get(url).call().map_err(|e| net(&e))?;
        read_body(resp.body_mut(), max_bytes, progress).map_err(|e| net(&e))
    }

    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, Error> {
        self.get_with_progress(url, max_bytes, &mut |_, _| {})
    }
}

/// Reads a response body of at most `max_bytes`, reporting `(bytes so far, total)`.
///
/// The reader is allowed one byte more than `max_bytes`: ureq's limit reader fails as soon as it
/// is asked for a byte past the limit, which includes the read that would have returned the end of
/// a body that is exactly `max_bytes` long. A body over `max_bytes` is refused here.
fn read_body(
    body: &mut ureq::Body,
    max_bytes: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    let total = body.content_length().unwrap_or(0);
    let mut reader = body
        .with_config()
        .limit(max_bytes.saturating_add(1))
        .reader();
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    progress(0, total);
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() as u64 > max_bytes {
            return Err(format!("the response is larger than the limit of {max_bytes} bytes"));
        }
        progress(out.len() as u64, total);
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
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Error> {
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

/// `download`, reporting `(bytes so far, total)`.
pub fn download_with_progress(
    fetcher: &dyn Fetcher,
    entry: &VersionEntry,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<u8>, Error> {
    fetcher.get_with_progress(&entry.url, entry.size, progress)
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

#[cfg(test)]
mod progress_tests {
    use super::*;

    struct One;

    impl Fetcher for One {
        fn get(&self, _url: &str, _max: u64) -> Result<Vec<u8>, Error> {
            Ok(vec![1, 2, 3])
        }
    }

    #[test]
    fn default_progress_reports_the_size_once_at_the_end() {
        let mut seen = Vec::new();
        let bytes = One
            .get_with_progress("https://x.invalid/a", 10, &mut |d, t| seen.push((d, t)))
            .unwrap();
        assert_eq!(bytes, [1, 2, 3]);
        assert_eq!(seen, [(3, 3)]);
    }
}

#[cfg(test)]
mod body_limit_tests {
    use super::*;
    use std::net::TcpListener;

    /// Serves one plain-HTTP response of `len` bytes and returns the URL.
    fn serve(len: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = listener.accept() {
                let mut req = [0u8; 1024];
                let _ = c.read(&mut req);
                let head = format!("HTTP/1.1 200 OK
Content-Length: {len}
Connection: close

");
                let _ = c.write_all(head.as_bytes());
                let _ = c.write_all(&vec![7u8; len]);
            }
        });
        format!("http://127.0.0.1:{port}/f")
    }

    fn fetch(len: usize, max: u64) -> Result<Vec<u8>, String> {
        let url = serve(len);
        let mut resp = ureq::get(&url).call().map_err(|e| e.to_string())?;
        read_body(resp.body_mut(), max, &mut |_, _| {})
    }

    #[test]
    fn a_body_exactly_at_the_limit_is_accepted() {
        assert_eq!(fetch(62415, 62415).unwrap().len(), 62415);
    }

    #[test]
    fn a_smaller_body_is_accepted() {
        assert_eq!(fetch(100, 62415).unwrap().len(), 100);
    }

    #[test]
    fn a_body_one_byte_over_the_limit_is_refused() {
        assert!(fetch(62416, 62415).is_err());
    }
}
