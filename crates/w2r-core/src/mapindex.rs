//! The hash index of the maps on this machine (docs/map-download.md, slice 3), ported from
//! wc2r-plus `map_download/map_index.cpp`. It answers "do I already have a map with this SHA-256
//! and where": every `.pud` under the maps root is hashed once in the background and remembered
//! in a cache keyed by path, size and modification time, so a later launch only rehashes what
//! changed. A map the player just downloaded is added without a rescan.
//!
//! Hardening over the C++: the walk is bounded in depth and in entries, never follows links, and
//! skips files outside 1 byte to 4 MiB; the cache file is size-capped and every line is parsed
//! strictly (a bad line is skipped, a bad header drops the cache).

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use crate::maplimits::sha256;
use crate::mapstore::is_reparse;

pub const CACHE_HEADER: &str = "w2r-maphash v1";
pub const MAX_LOCAL_MAP_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_ENTRIES: usize = 20_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    /// Modification time in 100 ns ticks since the Unix epoch (any unit works, it is only
    /// compared for equality).
    pub mtime: u64,
    pub sha: [u8; 32],
}

fn mtime_of(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64 / 100)
}

fn is_pud(name: &std::ffi::OsStr) -> bool {
    let s = name.to_string_lossy();
    s.len() > 4 && s[s.len() - 4..].eq_ignore_ascii_case(".pud")
}

/// Every `.pud` of 1 byte to 4 MiB under `root`, with size and time, links not followed.
pub fn walk(root: &Path) -> Vec<(PathBuf, u64, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            if out.len() >= MAX_ENTRIES {
                return out;
            }
            let path = e.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if !is_reparse(&meta) && depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if meta.is_file()
                && !is_reparse(&meta)
                && is_pud(&e.file_name())
                && (1..=MAX_LOCAL_MAP_BYTES).contains(&meta.len())
            {
                out.push((path, meta.len(), mtime_of(&meta)));
            }
        }
    }
    out
}

/// The SHA-256 of a local map file (1 byte to 4 MiB), or `None`.
pub fn hash_file(path: &Path) -> Option<[u8; 32]> {
    let mut data = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_LOCAL_MAP_BYTES + 1)
        .read_to_end(&mut data)
        .ok()?;
    ((1..=MAX_LOCAL_MAP_BYTES as usize).contains(&data.len())).then(|| sha256(&data))
}

fn hex(sha: &[u8; 32]) -> String {
    sha.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// The cache as text: the header, then `sha<TAB>size<TAB>mtime<TAB>path` per map.
pub fn cache_text(entries: &[Entry]) -> String {
    let mut text = format!("{CACHE_HEADER}\n");
    for e in entries {
        let Some(path) = e.path.to_str() else {
            continue;
        };
        if path.contains(['\t', '\n', '\r']) {
            continue;
        }
        text.push_str(&format!(
            "{}\t{}\t{}\t{path}\n",
            hex(&e.sha),
            e.size,
            e.mtime
        ));
    }
    text
}

/// Parses a cache; a wrong header gives an empty one, a bad line is skipped.
pub fn parse_cache(text: &str) -> HashMap<PathBuf, Entry> {
    let mut lines = text.lines();
    let mut map = HashMap::new();
    if lines.next() != Some(CACHE_HEADER) {
        return map;
    }
    for line in lines {
        let mut f = line.splitn(4, '\t');
        let (Some(h), Some(size), Some(mtime), Some(path)) =
            (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        let (Some(sha), Ok(size), Ok(mtime)) = (parse_hex(h), size.parse(), mtime.parse()) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let path = PathBuf::from(path);
        map.insert(
            path.clone(),
            Entry {
                path,
                size,
                mtime,
                sha,
            },
        );
    }
    map
}

fn load_cache(file: &Path) -> HashMap<PathBuf, Entry> {
    let mut text = String::new();
    let ok = fs::File::open(file)
        .and_then(|f| f.take(MAX_CACHE_BYTES + 1).read_to_string(&mut text))
        .is_ok();
    if !ok || text.len() as u64 > MAX_CACHE_BYTES {
        return HashMap::new();
    }
    parse_cache(&text)
}

fn save_cache(file: &Path, entries: &[Entry]) {
    let tmp = file.with_extension("txt.tmp");
    if fs::write(&tmp, cache_text(entries)).is_err() || fs::rename(&tmp, file).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// Hashes everything under `root` (reusing `cache_file` for what did not change), rewrites the
/// cache, and returns the entries and how many files had to be hashed.
pub fn build_entries(root: &Path, cache_file: Option<&Path>) -> (Vec<Entry>, usize) {
    let cache = cache_file.map(load_cache).unwrap_or_default();
    let mut hashed = 0;
    let mut kept = Vec::new();
    for (path, size, mtime) in walk(root) {
        let sha = match cache.get(&path) {
            Some(c) if c.size == size && c.mtime == mtime => c.sha,
            _ => match hash_file(&path) {
                Some(s) => {
                    hashed += 1;
                    s
                }
                None => continue,
            },
        };
        kept.push(Entry {
            path,
            size,
            mtime,
            sha,
        });
    }
    if let Some(file) = cache_file {
        save_cache(file, &kept);
    }
    (kept, hashed)
}

#[derive(Default)]
struct Shared {
    entries: Mutex<Vec<Entry>>,
    /// Maps added while the first build ran; merged when it finishes.
    added: Mutex<Vec<Entry>>,
    ready: AtomicBool,
    started: AtomicBool,
    hashed: AtomicUsize,
}

/// The index, shared with the thread that builds it.
#[derive(Clone, Default)]
pub struct MapIndex(Arc<Shared>);

impl MapIndex {
    pub fn new() -> MapIndex {
        MapIndex::default()
    }

    /// Starts the one background build; later calls do nothing.
    pub fn start_build(&self, root: PathBuf, cache_file: Option<PathBuf>) {
        if self.0.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = self.0.clone();
        let spawned = std::thread::Builder::new()
            .name("w2r-map-index".into())
            .spawn(move || {
                let (mut kept, hashed) = build_entries(&root, cache_file.as_deref());
                let added = shared
                    .added
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                for a in added {
                    if !kept.iter().any(|k| same_path(&k.path, &a.path)) {
                        kept.push(a);
                    }
                }
                *shared.entries.lock().unwrap_or_else(|e| e.into_inner()) = kept;
                shared.hashed.store(hashed, Ordering::Release);
                shared.ready.store(true, Ordering::Release);
            });
        if spawned.is_err() {
            self.0.started.store(false, Ordering::Release);
        }
    }

    pub fn ready(&self) -> bool {
        self.0.ready.load(Ordering::Acquire)
    }

    pub fn count(&self) -> usize {
        self.0
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    pub fn hashed_last_build(&self) -> usize {
        self.0.hashed.load(Ordering::Acquire)
    }

    /// Where a map with this hash is.
    pub fn find_by_sha(&self, sha: &[u8; 32]) -> Option<PathBuf> {
        self.0
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|e| e.sha == *sha)
            .map(|e| e.path.clone())
    }

    /// Records a map just written (a download), without waiting for a rescan.
    pub fn add(&self, path: PathBuf, sha: [u8; 32]) {
        let entry = Entry {
            size: fs::metadata(&path).map_or(0, |m| m.len()),
            mtime: fs::metadata(&path).map_or(0, |m| mtime_of(&m)),
            path,
            sha,
        };
        self.0
            .added
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(entry.clone());
        let mut entries = self.0.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.iter_mut().find(|k| same_path(&k.path, &entry.path)) {
            Some(k) => k.sha = sha,
            None => entries.push(entry),
        }
    }
}

/// Windows paths compare without regard to case.
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::{Duration, Instant};

    fn tree() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir().join(format!(
            "w2r-index-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("Maps").join("Duels")).unwrap();
        fs::write(base.join("Maps").join("a.pud"), b"AAAA").unwrap();
        fs::write(base.join("Maps").join("Duels").join("b.PUD"), b"BBBBBB").unwrap();
        fs::write(base.join("Maps").join("notes.txt"), b"not a map").unwrap();
        fs::write(base.join("Maps").join("empty.pud"), b"").unwrap();
        base
    }

    #[test]
    fn the_walk_finds_pud_files_of_a_sane_size_in_every_folder() {
        let base = tree();
        let mut found: Vec<String> = walk(&base.join("Maps"))
            .into_iter()
            .map(|(p, _, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, ["a.pud", "b.PUD"], "no text files, no empty files");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn hashing_matches_sha256_and_refuses_empty_and_huge_files() {
        let base = tree();
        assert_eq!(
            hash_file(&base.join("Maps").join("a.pud")),
            Some(sha256(b"AAAA"))
        );
        assert_eq!(hash_file(&base.join("Maps").join("empty.pud")), None);
        assert_eq!(hash_file(&base.join("Maps").join("missing.pud")), None);
        let big = base.join("Maps").join("big.pud");
        fs::write(&big, vec![0u8; MAX_LOCAL_MAP_BYTES as usize + 1]).unwrap();
        assert_eq!(hash_file(&big), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn the_cache_round_trips_and_a_bad_file_gives_nothing_or_skips_the_line() {
        let e = Entry {
            path: PathBuf::from("Maps\\Duels\\b.PUD"),
            size: 6,
            mtime: 12345,
            sha: sha256(b"BBBBBB"),
        };
        let text = cache_text(std::slice::from_ref(&e));
        let back = parse_cache(&text);
        assert_eq!(back.get(&e.path), Some(&e));
        assert!(
            parse_cache("something else\nabc").is_empty(),
            "wrong header"
        );
        assert!(parse_cache("").is_empty());
        let messy = format!(
            "{CACHE_HEADER}\nnothex\t1\t1\tx\n{}\t1\t1\n{}\tNaN\t1\tp\n{}\t7\t8\tgood.pud\n",
            hex(&[1; 32]),
            hex(&[2; 32]),
            hex(&[3; 32])
        );
        let m = parse_cache(&messy);
        assert_eq!(m.len(), 1);
        assert_eq!(m[&PathBuf::from("good.pud")].size, 7);
        // A path with a tab or newline would break the format; it is left out.
        let weird = Entry {
            path: PathBuf::from("a\tb.pud"),
            ..e
        };
        assert_eq!(cache_text(&[weird]), format!("{CACHE_HEADER}\n"));
    }

    #[test]
    fn a_second_build_rehashes_only_what_changed() {
        let base = tree();
        let cache = base.join("cache.txt");
        let (first, hashed) = build_entries(&base.join("Maps"), Some(&cache));
        assert_eq!((first.len(), hashed), (2, 2));
        let (_, hashed) = build_entries(&base.join("Maps"), Some(&cache));
        assert_eq!(hashed, 0, "everything came from the cache");
        fs::write(base.join("Maps").join("a.pud"), b"AAAAAAA").unwrap();
        let (again, hashed) = build_entries(&base.join("Maps"), Some(&cache));
        assert_eq!(hashed, 1, "the changed file only");
        let a = again.iter().find(|e| e.path.ends_with("a.pud")).unwrap();
        assert_eq!(a.sha, sha256(b"AAAAAAA"));
        let _ = fs::remove_dir_all(&base);
    }

    fn wait_ready(i: &MapIndex) {
        let start = Instant::now();
        while !i.ready() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(i.ready());
    }

    #[test]
    fn the_background_index_finds_maps_by_hash_and_takes_new_ones() {
        let base = tree();
        let index = MapIndex::new();
        assert!(!index.ready());
        index.start_build(base.join("Maps"), Some(base.join("cache.txt")));
        index.start_build(base.join("nowhere"), None); // a second start does nothing
        wait_ready(&index);
        assert_eq!(index.count(), 2);
        assert!(index.hashed_last_build() <= 2);
        let found = index.find_by_sha(&sha256(b"BBBBBB")).unwrap();
        assert!(found.ends_with("b.PUD"));
        assert_eq!(index.find_by_sha(&sha256(b"nothing like it")), None);
        let fresh = base.join("Maps").join("new.pud");
        fs::write(&fresh, b"NEW").unwrap();
        index.add(fresh.clone(), sha256(b"NEW"));
        assert_eq!(index.find_by_sha(&sha256(b"NEW")), Some(fresh.clone()));
        index.add(fresh.clone(), sha256(b"NEW"));
        assert_eq!(
            index.count(),
            3,
            "adding the same path again does not duplicate it"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_map_added_during_the_first_build_is_kept_when_it_finishes() {
        let base = tree();
        let index = MapIndex::new();
        let extra = base.join("elsewhere.pud");
        fs::write(&extra, b"XTRA").unwrap();
        // Added before the build runs: it must survive the build replacing the list.
        index.add(extra.clone(), sha256(b"XTRA"));
        index.start_build(base.join("Maps"), None);
        wait_ready(&index);
        assert_eq!(index.find_by_sha(&sha256(b"XTRA")), Some(extra));
        assert_eq!(index.count(), 3);
        let _ = fs::remove_dir_all(&base);
    }
}
