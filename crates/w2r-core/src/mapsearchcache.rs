//! The create screen browser's search-index cache on disk (docs/create-screen.md, "browser"
//! leftovers), ported from wc2r-plus `features/ui/map_search_cache.cpp`. The index is every map
//! under the maps root, found by walking the game's own folder list (slow with a big collection);
//! this remembers it between launches. It is trusted only while the maps root is the same and every
//! folder and map file still has the size and modification time it had when the index was saved,
//! so any change anywhere rebuilds it. No game access: the file system is behind a `stat` closure,
//! so everything is tested on any target. Paths are the game's, with `\` separators.
//!
//! The text format is wc2r-plus's (header `wc2r-mapsearch v1`, then `R`oot, `D`irectory and `M`ap
//! lines of tab-separated fields, with `\\`, `\t`, `\n` and `\r` escaped), so a file written by
//! one is read by the other; any doubt about a file means "rebuild", never an error.

use std::io::Read;
use std::path::Path;

use crate::mapbrowser::{IndexedMap, MAX_INDEXED_MAPS, sanitize};

pub const HEADER: &str = "wc2r-mapsearch v1";
/// A cache file larger than this is ignored.
pub const MAX_CACHE_BYTES: u64 = 16 * 1024 * 1024;
/// More folders than this in a cache file is not a cache we wrote.
pub const MAX_DIRS: usize = 20_000;

/// Size and modification time of a file or folder (100 ns units), `None` if it is gone.
pub type Stamp = (u64, u64);

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// The text of a cache for `maps` (each with its stamp) under `root`, with the stamps of every
/// folder walked. Maps whose stamp is `None` (gone since the walk) are left out; the second value
/// is how many.
pub fn encode(
    root: &str,
    dirs: &[(String, Stamp)],
    maps: &[(IndexedMap, Option<Stamp>)],
) -> (String, usize) {
    let mut text = format!("{HEADER}\nR\t{}\n", escape(root));
    for (dir, (_, mtime)) in dirs {
        text.push_str(&format!("D\t{mtime}\t{}\n", escape(dir)));
    }
    let mut dropped = 0;
    for (m, stamp) in maps {
        let Some((size, mtime)) = stamp else {
            dropped += 1;
            continue;
        };
        text.push_str(&format!(
            "M\t{size}\t{mtime}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            m.dim,
            m.players,
            escape(&m.rel_dir),
            escape(&m.name),
            escape(&m.path),
            escape(&m.desc),
        ));
    }
    (text, dropped)
}

/// The first `want - 1` tab-separated fields and the rest as the last; `None` if a field is
/// missing. (Escaped text never contains a raw tab.)
fn fields(line: &str, want: usize) -> Option<Vec<&str>> {
    let parts: Vec<&str> = line.splitn(want, '\t').collect();
    (parts.len() == want).then_some(parts)
}

/// Reads a cache back: `Some(maps)` only if it is a cache of `root` and every folder and map in
/// it still has the stamp it was saved with (`stat` says what is on disk now). Everything is
/// bounded and sanitised: the file is ours, but nothing in it is trusted.
pub fn decode(
    text: &str,
    root: &str,
    stat: &dyn Fn(&str) -> Option<Stamp>,
) -> Option<Vec<IndexedMap>> {
    let mut lines = text.lines();
    if lines.next()? != HEADER {
        return None;
    }
    let mut have_root = false;
    let mut dirs = 0usize;
    let mut maps = Vec::new();
    for line in lines {
        let Some(kind) = line.chars().next() else {
            continue;
        };
        match kind {
            'R' => {
                let f = fields(line, 2)?;
                if unescape(f[1]) != root {
                    return None;
                }
                have_root = true;
            }
            'D' => {
                let f = fields(line, 3)?;
                dirs += 1;
                if dirs > MAX_DIRS {
                    return None;
                }
                let dir = unescape(f[2]);
                let (_, mtime) = stat(&dir)?;
                if mtime != f[1].parse::<u64>().ok()? {
                    return None;
                }
            }
            'M' => {
                let f = fields(line, 9)?;
                if maps.len() >= MAX_INDEXED_MAPS {
                    return None;
                }
                let path = unescape(f[7]);
                let (size, mtime) = stat(&path)?;
                if size != f[1].parse::<u64>().ok()? || mtime != f[2].parse::<u64>().ok()? {
                    return None;
                }
                maps.push(IndexedMap {
                    dim: f[3].parse().ok()?,
                    players: f[4].parse().ok()?,
                    rel_dir: sanitize(&unescape(f[5])),
                    name: sanitize(&unescape(f[6])),
                    path: sanitize(&path),
                    desc: sanitize(&unescape(f[8])),
                });
            }
            _ => {}
        }
    }
    have_root.then_some(maps)
}

/// Reads `file` (bounded) and decodes it. `None` for a missing, oversized or stale cache.
pub fn load(
    file: &Path,
    root: &str,
    stat: &dyn Fn(&str) -> Option<Stamp>,
) -> Option<Vec<IndexedMap>> {
    let mut text = String::new();
    std::fs::File::open(file)
        .ok()?
        .take(MAX_CACHE_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_CACHE_BYTES {
        return None;
    }
    decode(&text, root, stat)
}

/// Writes the cache next to its final name and renames it into place. Returns how many maps
/// were left out because they had vanished, or the error.
pub fn save(
    file: &Path,
    root: &str,
    dirs: &[String],
    maps: &[IndexedMap],
    stat: &dyn Fn(&str) -> Option<Stamp>,
) -> std::io::Result<usize> {
    let dir_stamps: Vec<(String, Stamp)> = dirs
        .iter()
        .filter_map(|d| stat(d).map(|s| (d.clone(), s)))
        .collect();
    let stamped: Vec<(IndexedMap, Option<Stamp>)> =
        maps.iter().map(|m| (m.clone(), stat(&m.path))).collect();
    let (text, dropped) = encode(root, &dir_stamps, &stamped);
    if text.len() as u64 > MAX_CACHE_BYTES {
        return Err(std::io::Error::other("the index is too large to cache"));
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension("txt.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map(name: &str, dir: &str) -> IndexedMap {
        IndexedMap {
            name: name.into(),
            path: format!("{dir}\\{name}.pud"),
            desc: "A map. With a description.".into(),
            rel_dir: dir.into(),
            players: 4,
            dim: 64,
        }
    }

    /// A pretend disk: path to stamp.
    fn disk(entries: &[(&str, Stamp)]) -> HashMap<String, Stamp> {
        entries.iter().map(|(p, s)| ((*p).to_owned(), *s)).collect()
    }

    fn sample() -> (Vec<String>, Vec<IndexedMap>, HashMap<String, Stamp>) {
        let dirs = vec!["Maps".to_owned(), "Maps\\Duels".to_owned()];
        let maps = vec![map("Gold", "Maps"), map("Ridge", "Maps\\Duels")];
        let d = disk(&[
            ("Maps", (0, 50)),
            ("Maps\\Duels", (0, 60)),
            ("Maps\\Gold.pud", (1000, 111)),
            ("Maps\\Duels\\Ridge.pud", (2000, 222)),
        ]);
        (dirs, maps, d)
    }

    fn stat_of(d: &HashMap<String, Stamp>) -> impl Fn(&str) -> Option<Stamp> + '_ {
        move |p| d.get(p).copied()
    }

    fn encoded(dirs: &[String], maps: &[IndexedMap], d: &HashMap<String, Stamp>) -> String {
        let ds: Vec<(String, Stamp)> = dirs.iter().map(|x| (x.clone(), d[x])).collect();
        let ms: Vec<(IndexedMap, Option<Stamp>)> = maps
            .iter()
            .map(|m| (m.clone(), d.get(&m.path).copied()))
            .collect();
        encode("Maps", &ds, &ms).0
    }

    #[test]
    fn a_saved_index_comes_back_unchanged_while_the_disk_is_unchanged() {
        let (dirs, maps, d) = sample();
        let text = encoded(&dirs, &maps, &d);
        assert!(text.starts_with("wc2r-mapsearch v1\nR\tMaps\n"));
        assert_eq!(decode(&text, "Maps", &stat_of(&d)), Some(maps));
    }

    #[test]
    fn any_change_on_disk_makes_it_stale() {
        let (dirs, maps, d) = sample();
        let text = encoded(&dirs, &maps, &d);
        for (path, new) in [
            ("Maps", (0, 51)),                       // a folder changed (file added)
            ("Maps\\Duels\\Ridge.pud", (2001, 222)), // size changed
            ("Maps\\Gold.pud", (1000, 112)),         // touched
        ] {
            let mut changed = d.clone();
            changed.insert(path.to_owned(), new);
            assert_eq!(decode(&text, "Maps", &stat_of(&changed)), None, "{path}");
        }
        let mut gone = d.clone();
        gone.remove("Maps\\Gold.pud");
        assert_eq!(
            decode(&text, "Maps", &stat_of(&gone)),
            None,
            "a map vanished"
        );
        let mut no_dir = d.clone();
        no_dir.remove("Maps\\Duels");
        assert_eq!(
            decode(&text, "Maps", &stat_of(&no_dir)),
            None,
            "a folder vanished"
        );
    }

    #[test]
    fn a_different_maps_root_is_not_this_cache() {
        let (dirs, maps, d) = sample();
        let text = encoded(&dirs, &maps, &d);
        assert_eq!(decode(&text, "Other\\Maps", &stat_of(&d)), None);
    }

    #[test]
    fn text_with_tabs_newlines_and_backslashes_survives() {
        let (dirs, mut maps, d) = sample();
        maps[0].desc = "a\\b\tc\nd\re".into();
        let text = encoded(&dirs, &maps, &d);
        assert_eq!(text.lines().count(), 1 + 1 + 2 + 2, "no raw newline leaked");
        let back = decode(&text, "Maps", &stat_of(&d)).unwrap();
        // The browser's sanitiser removes control characters on the way back in.
        assert_eq!(back[0].desc, "a\\bcde");
    }

    #[test]
    fn maps_that_vanished_before_saving_are_left_out_and_counted() {
        let (dirs, maps, mut d) = sample();
        d.remove("Maps\\Gold.pud");
        let ds: Vec<(String, Stamp)> = dirs.iter().map(|x| (x.clone(), d[x])).collect();
        let ms: Vec<(IndexedMap, Option<Stamp>)> = maps
            .iter()
            .map(|m| (m.clone(), d.get(&m.path).copied()))
            .collect();
        let (text, dropped) = encode("Maps", &ds, &ms);
        assert_eq!(dropped, 1);
        assert!(!text.contains("Gold"));
    }

    #[test]
    fn garbage_is_a_miss_never_a_panic() {
        let d = disk(&[("Maps", (0, 50))]);
        let stat = stat_of(&d);
        for text in [
            "",
            "not a cache\n",
            "wc2r-mapsearch v1\n",                      // no root
            "wc2r-mapsearch v1\nR\tMaps\nD\tx\tMaps\n", // bad number
            "wc2r-mapsearch v1\nR\tMaps\nD\t50\n",      // too few fields
            "wc2r-mapsearch v1\nR\tMaps\nM\t1\t2\t3\n", // too few fields
            "wc2r-mapsearch v1\nR\tMaps\nM\t1\t2\tx\t4\ta\tb\tc\td\n", // bad dimension
        ] {
            assert_eq!(decode(text, "Maps", &stat), None, "{text:?}");
        }
        assert_eq!(
            decode("wc2r-mapsearch v1\nR\tMaps\nD\t50\tMaps\n", "Maps", &stat),
            Some(vec![]),
            "an empty but valid cache"
        );
    }

    #[test]
    fn a_cache_with_too_many_maps_or_folders_is_refused() {
        let d = disk(&[("Maps", (0, 50)), ("Maps\\m.pud", (1, 2))]);
        let stat = stat_of(&d);
        let mut text = String::from("wc2r-mapsearch v1\nR\tMaps\n");
        for _ in 0..=MAX_INDEXED_MAPS {
            text.push_str("M\t1\t2\t64\t4\tMaps\tm\tMaps\\m.pud\t\n");
        }
        assert_eq!(decode(&text, "Maps", &stat), None);
        let mut text = String::from("wc2r-mapsearch v1\nR\tMaps\n");
        for _ in 0..=MAX_DIRS {
            text.push_str("D\t50\tMaps\n");
        }
        assert_eq!(decode(&text, "Maps", &stat), None);
    }

    #[test]
    fn control_characters_in_a_stored_name_are_stripped_on_load() {
        let d = disk(&[("Maps", (0, 50)), ("Maps\\m.pud", (1, 2))]);
        let text = "wc2r-mapsearch v1\nR\tMaps\nM\t1\t2\t64\t4\tMaps\tna\u{7}me\tMaps\\\\m.pud\t\n";
        let back = decode(text, "Maps", &stat_of(&d)).expect("valid");
        assert_eq!(back[0].name, "name");
    }

    #[test]
    fn save_and_load_round_trip_through_a_file() {
        let (dirs, maps, d) = sample();
        let tmp = std::env::temp_dir().join(format!("w2r-searchcache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let file = tmp.join("cache").join("map_search_index.txt");
        assert_eq!(
            load(&file, "Maps", &stat_of(&d)),
            None,
            "nothing stored yet"
        );
        assert_eq!(save(&file, "Maps", &dirs, &maps, &stat_of(&d)).unwrap(), 0);
        assert_eq!(load(&file, "Maps", &stat_of(&d)), Some(maps.clone()));
        assert!(
            !file.with_extension("txt.tmp").exists(),
            "no temp file left"
        );
        let mut changed = d.clone();
        changed.insert("Maps".into(), (0, 99));
        assert_eq!(load(&file, "Maps", &stat_of(&changed)), None);
        // An oversized file is ignored without being read whole.
        std::fs::write(&file, vec![b'x'; (MAX_CACHE_BYTES + 10) as usize]).unwrap();
        assert_eq!(load(&file, "Maps", &stat_of(&d)), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
