//! Updating the core mod itself: the `version.dll` proxy in the game's `x86` folder
//! (docs/core.md). The manager is the only thing that can do this, because the game holds the
//! DLL open while it runs. The source is `core.json`, published next to `index.json`; it is
//! kept out of the index so managers that predate it keep working. Trust is the same as for
//! plugins: HTTPS only, plus the SHA-256 and size in `core.json`, plus the serial rollback check.

use crate::Error;
use crate::fetch::{Fetcher, write_atomic};
use crate::index::{self, MAX_PACKAGE_BYTES};
use crate::store::{Root, read_bounded};
use crate::Reason;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use w2r_formats::json::{self, Value};
use w2r_formats::semver::Version;

pub const MAX_CORE_JSON: usize = 64 * 1024;
const MAX_SERIAL: u64 = (1 << 53) - 1;
const MAX_BUILDS: usize = 16;
/// Every build of our DLL carries this (the first log line); another mod's `version.dll` does not.
pub const MARKER_OURS: &[u8] = b"=== w2rpm host ";
/// `W2R-CORE-VERSION:<semver>;`, embedded by `w2r-core` (`entry.rs`).
pub const MARKER_VERSION: &[u8] = b"W2R-CORE-VERSION:";
const DLL_NAME: &str = "version.dll";
pub const GAME_EXE: &str = "Warcraft II.exe";

/// What `core.json` says about the newest core.
#[derive(Clone, Debug)]
pub struct CoreInfo {
    pub serial: u64,
    pub version: Version,
    pub url: String,
    pub sha256: [u8; 32],
    pub size: u64,
    pub min_manager: Version,
    /// Informational: the game builds this core was made for (the DLL's own guard enforces them).
    pub game_builds: Vec<String>,
}

/// The checks of docs/core.md, in order. `cached_serial` is the highest accepted so far.
pub fn parse_core(raw: &[u8], cached_serial: u64) -> Result<CoreInfo, Reason> {
    if raw.len() > MAX_CORE_JSON {
        return Err("too_large");
    }
    let doc = json::parse(raw).map_err(|r| r.code())?;
    index::keys(
        &doc,
        &[
            "schema",
            "serial",
            "version",
            "url",
            "sha256",
            "size",
            "min_manager",
            "game_builds",
        ],
        &[],
    )?;
    let schema = index::field(&doc, "schema");
    if !index::is_int(schema) || index::int(schema) != Some(1) {
        return Err("bad_schema");
    }
    let serial = index::int(index::field(&doc, "serial"))
        .filter(|&s| s <= MAX_SERIAL)
        .ok_or("bad_type")?;
    if serial < cached_serial {
        return Err("rollback");
    }
    let version = index::field(&doc, "version")
        .as_str()
        .and_then(Version::parse)
        .filter(|v| v.pre.is_empty())
        .ok_or("bad_version")?;
    let min_manager = index::field(&doc, "min_manager")
        .as_str()
        .and_then(Version::parse)
        .ok_or("bad_version")?;
    let url = index::field(&doc, "url")
        .as_str()
        .filter(|u| index::url_ok(u))
        .ok_or("bad_url")?
        .to_owned();
    let sha256 = index::field(&doc, "sha256")
        .as_str()
        .and_then(index::hex32)
        .ok_or("bad_hash")?;
    let size = index::field(&doc, "size");
    if !index::is_int(size) {
        return Err("bad_type");
    }
    let size = index::int(size)
        .filter(|s| (1..=MAX_PACKAGE_BYTES).contains(s))
        .ok_or("bad_size")?;
    let builds = index::field(&doc, "game_builds")
        .as_array()
        .ok_or("bad_type")?;
    if builds.len() > MAX_BUILDS {
        return Err("too_many");
    }
    let game_builds = builds
        .iter()
        .map(|b| match b {
            Value::String(s)
                if !s.is_empty()
                    && s.len() <= 32
                    && s.bytes().all(|c| c.is_ascii_digit() || c == b'.') =>
            {
                Ok(s.clone())
            }
            _ => Err("bad_type"),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CoreInfo {
        serial,
        version,
        url,
        sha256,
        size,
        min_manager,
        game_builds,
    })
}

// ---- cache ----

fn cache_path(root: &Root, name: &str) -> PathBuf {
    root.cache_dir().join(name)
}

/// The highest `core.json` serial accepted so far (0 when none).
pub fn cached_serial(root: &Root) -> u64 {
    read_bounded(&cache_path(root, "core.serial"), 32)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Fetches and checks `core.json`, then replaces the cache. A refused file leaves it untouched.
pub fn refresh(root: &Root, fetcher: &dyn Fetcher, url: &str) -> Result<CoreInfo, Error> {
    let raw = fetcher.get(url, MAX_CORE_JSON as u64)?;
    let info = parse_core(&raw, cached_serial(root))
        .map_err(|r| Error::new(r, "the fetched core.json was refused"))?;
    fs::create_dir_all(root.cache_dir())?;
    write_atomic(
        &cache_path(root, "core.serial"),
        info.serial.to_string().as_bytes(),
    )?;
    write_atomic(&cache_path(root, "core.json"), &raw)?;
    Ok(info)
}

/// The cached `core.json`, re-checked; `None` when there is none or it no longer parses.
pub fn load_cached(root: &Root) -> Option<CoreInfo> {
    let raw = read_bounded(&cache_path(root, "core.json"), MAX_CORE_JSON).ok()?;
    parse_core(&raw, cached_serial(root)).ok()
}

// ---- the game folder ----

/// `dir` itself or its `x86` child, whichever holds `Warcraft II.exe`.
pub fn normalize_game_dir(dir: &Path) -> Option<PathBuf> {
    [dir.to_path_buf(), dir.join("x86")]
        .into_iter()
        .find(|d| d.join(GAME_EXE).is_file())
}

fn saved_path(root: &Root) -> PathBuf {
    cache_path(root, "game_dir.txt")
}

pub fn saved_game_dir(root: &Root) -> Option<PathBuf> {
    let raw = read_bounded(&saved_path(root), 4096).ok()?;
    let text = String::from_utf8(raw).ok()?;
    normalize_game_dir(Path::new(text.trim()))
}

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for drive in 'C'..='G' {
        for tail in [
            r"Games\Warcraft II Remastered",
            r"Program Files (x86)\Warcraft II Remastered",
            r"Program Files\Warcraft II Remastered",
            r"Program Files (x86)\Steam\steamapps\common\Warcraft II Remastered",
            r"Steam\steamapps\common\Warcraft II Remastered",
        ] {
            out.push(PathBuf::from(format!("{drive}:\\{tail}")));
        }
    }
    out
}

/// The game's `x86` folder: an explicit one, else the remembered one, else a few usual places.
/// An explicit folder that is not the game is an error rather than a fall-through.
pub fn resolve_game_dir(root: &Root, explicit: Option<&Path>) -> Result<PathBuf, Error> {
    if let Some(p) = explicit {
        return normalize_game_dir(p).ok_or_else(|| {
            Error::new(
                "no_game_dir",
                format!("{} does not hold {GAME_EXE}", p.display()),
            )
        });
    }
    saved_game_dir(root)
        .or_else(|| candidates().iter().find_map(|c| normalize_game_dir(c)))
        .ok_or_else(|| {
            Error::new(
                "no_game_dir",
                "could not find the game; pass --game-dir with the folder that holds Warcraft II.exe",
            )
        })
}

/// Remembers the folder for next time (best effort is not enough here: report failure).
pub fn save_game_dir(root: &Root, dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(root.cache_dir())?;
    write_atomic(&saved_path(root), dir.display().to_string().as_bytes())
}

// ---- inspecting a DLL ----

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// For a DLL that is ours: its embedded version (`None` for a build before the marker existed).
/// `None` outright when the bytes are not our DLL.
pub fn inspect(bytes: &[u8]) -> Option<Option<Version>> {
    if !bytes.starts_with(b"MZ") || find(bytes, MARKER_OURS).is_none() {
        return None;
    }
    let version = find(bytes, MARKER_VERSION).and_then(|at| {
        let rest = &bytes[at + MARKER_VERSION.len()..];
        let end = rest.iter().take(40).position(|&c| c == b';')?;
        Version::parse(std::str::from_utf8(&rest[..end]).ok()?)
    });
    Some(version)
}

/// What sits at `version.dll` in the game folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    Missing,
    /// Another mod's `version.dll`; we never touch it.
    Foreign,
    /// Ours, with its version when it carries one.
    Ours(Option<Version>),
}

pub fn installed(game_dir: &Path) -> Result<Installed, Error> {
    match read_bounded(&game_dir.join(DLL_NAME), MAX_PACKAGE_BYTES as usize) {
        Ok(b) => Ok(match inspect(&b) {
            Some(v) => Installed::Ours(v),
            None => Installed::Foreign,
        }),
        Err(e) if e.code == "not_found" => Ok(Installed::Missing),
        Err(e) => Err(e),
    }
}

/// True when an update is offered: ours and older, or nothing installed yet. A build without a
/// version marker counts as older than anything.
pub fn update_available(installed: &Installed, info: &CoreInfo, manager: &Version) -> bool {
    if &info.min_manager > manager {
        return false;
    }
    match installed {
        Installed::Missing => true,
        Installed::Foreign => false,
        Installed::Ours(None) => true,
        Installed::Ours(Some(v)) => info.version > *v,
    }
}

// ---- installing ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The version replaced (`None` for a first install or an unmarked build).
    pub from: Option<Version>,
    pub to: Option<Version>,
}

fn parked_path(root: &Root) -> PathBuf {
    root.cache_dir().join("core-rollback").join(DLL_NAME)
}

/// Whether the game (or anything) holds the DLL open. A mapped DLL cannot be opened for writing.
fn ensure_unlocked(path: &Path) -> Result<(), Error> {
    if !path.exists() {
        return Ok(());
    }
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map(drop)
        .map_err(|e| {
            Error::new(
                "game_running",
                format!("{} is in use ({e}); close the game and try again", path.display()),
            )
        })
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// Installs the downloaded `bytes` as the game's `version.dll`. Checks, in order: this manager is
/// new enough, size and SHA-256 match `core.json`, the DLL is ours and carries the announced
/// version, the folder is the game, the current `version.dll` is ours (or absent) and unlocked.
/// The old DLL is parked for `rollback` first; a failure before the final rename leaves it alone.
pub fn update(
    root: &Root,
    game_dir: &Path,
    info: &CoreInfo,
    bytes: &[u8],
    manager: &Version,
) -> Result<Outcome, Error> {
    if &info.min_manager > manager {
        return Err(Error::new(
            "needs_newer_manager",
            format!("this core needs w2rpm {} or newer", info.min_manager),
        ));
    }
    if bytes.len() as u64 != info.size {
        return Err(Error::new("bad_size", "the download is not the announced size"));
    }
    if Sha256::digest(bytes).as_slice() != info.sha256 {
        return Err(Error::new("bad_hash", "the download does not match core.json"));
    }
    let new_version = match inspect(bytes) {
        Some(v) => v,
        None => return Err(Error::new("not_core", "the download is not the w2rpm core")),
    };
    if new_version.as_ref() != Some(&info.version) {
        return Err(Error::new(
            "mismatch",
            "the DLL's own version is not the one core.json announces",
        ));
    }
    if normalize_game_dir(game_dir).as_deref() != Some(game_dir) {
        return Err(Error::new(
            "no_game_dir",
            format!("{} does not hold {GAME_EXE}", game_dir.display()),
        ));
    }
    let target = game_dir.join(DLL_NAME);
    let from = match installed(game_dir)? {
        Installed::Foreign => {
            return Err(Error::new(
                "not_ours",
                format!(
                    "{} belongs to another mod; move it away first",
                    target.display()
                ),
            ));
        }
        Installed::Missing => None,
        Installed::Ours(v) => v,
    };
    ensure_unlocked(&target)?;
    swap_in(root, &target, bytes, target.exists())?;
    Ok(Outcome {
        from,
        to: new_version,
    })
}

/// Writes `bytes` next to `target`, parks the current file (when `had_old`) and renames the new
/// one into place.
fn swap_in(root: &Root, target: &Path, bytes: &[u8], had_old: bool) -> Result<(), Error> {
    let staged = target.with_extension("dll.new");
    write_synced(&staged, bytes).inspect_err(|_| {
        let _ = fs::remove_file(&staged);
    })?;
    if had_old {
        let parked = parked_path(root);
        let step = (|| -> Result<(), Error> {
            if let Some(dir) = parked.parent() {
                fs::create_dir_all(dir)?;
            }
            let tmp = parked.with_extension("dll.tmp");
            fs::copy(target, &tmp)?;
            fs::rename(&tmp, &parked)?;
            Ok(())
        })();
        if let Err(e) = step {
            let _ = fs::remove_file(&staged);
            return Err(e);
        }
    }
    fs::rename(&staged, target).inspect_err(|_| {
        let _ = fs::remove_file(&staged);
    })?;
    Ok(())
}

/// Swaps the parked DLL back in (run again to swap forward), exactly like plugin rollback.
pub fn rollback(root: &Root, game_dir: &Path) -> Result<Outcome, Error> {
    let parked = parked_path(root);
    let old = read_bounded(&parked, MAX_PACKAGE_BYTES as usize)
        .map_err(|e| {
            if e.code == "not_found" {
                Error::new("no_rollback", "no earlier core is kept")
            } else {
                e
            }
        })?;
    let Some(to) = inspect(&old) else {
        return Err(Error::new("not_core", "the kept file is not the w2rpm core"));
    };
    let target = game_dir.join(DLL_NAME);
    let from = match installed(game_dir)? {
        Installed::Foreign => {
            return Err(Error::new(
                "not_ours",
                format!("{} belongs to another mod", target.display()),
            ));
        }
        Installed::Missing => None,
        Installed::Ours(v) => v,
    };
    ensure_unlocked(&target)?;
    swap_in(root, &target, &old, target.exists())?;
    Ok(Outcome { from, to })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempRoot;

    fn dll(version: &str) -> Vec<u8> {
        let mut b = b"MZ....".to_vec();
        b.extend_from_slice(MARKER_OURS);
        b.extend_from_slice(b" attached ");
        b.extend_from_slice(MARKER_VERSION);
        b.extend_from_slice(version.as_bytes());
        b.extend_from_slice(b";");
        b
    }

    fn info_for(bytes: &[u8], version: &str) -> CoreInfo {
        CoreInfo {
            serial: 1,
            version: Version::parse(version).unwrap(),
            url: "https://example.com/version.dll".into(),
            sha256: Sha256::digest(bytes).into(),
            size: bytes.len() as u64,
            min_manager: Version::parse("0.1.0").unwrap(),
            game_builds: vec!["1.0.2.2818".into()],
        }
    }

    fn game(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("w2rpm-game-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join(GAME_EXE), b"exe").unwrap();
        p
    }

    fn manager() -> Version {
        Version::parse("0.1.0").unwrap()
    }

    const GOOD: &str = r#"{"schema":1,"serial":3,"version":"0.2.0","url":"https://example.com/version.dll","sha256":"0000000000000000000000000000000000000000000000000000000000000000","size":10,"min_manager":"0.1.0","game_builds":["1.0.2.2818"]}"#;

    #[test]
    fn core_json_is_checked_strictly() {
        let ok = parse_core(GOOD.as_bytes(), 0).unwrap();
        assert_eq!(ok.serial, 3);
        assert_eq!(ok.game_builds, vec!["1.0.2.2818"]);
        let code = |s: &str, serial| parse_core(s.as_bytes(), serial).unwrap_err();
        assert_eq!(code(GOOD, 4), "rollback");
        assert_eq!(code(&GOOD.replace("\"schema\":1", "\"schema\":2"), 0), "bad_schema");
        assert_eq!(code(&GOOD.replace("0.2.0", "0.2.0-beta"), 0), "bad_version");
        assert_eq!(code(&GOOD.replace("https", "http"), 0), "bad_url");
        assert_eq!(code(&GOOD.replace("\"size\":10", "\"size\":0"), 0), "bad_size");
        assert_eq!(code(&GOOD.replace("0000000000", "00000000zz"), 0), "bad_hash");
        assert_eq!(
            code(&GOOD.replace("\"game_builds\"", "\"extra\":1,\"game_builds\""), 0),
            "unknown_field"
        );
        assert_eq!(code(&GOOD.replace(",\"game_builds\":[\"1.0.2.2818\"]", ""), 0), "missing_field");
        assert_eq!(code("{", 0), "not_json");
    }

    #[test]
    fn inspect_tells_our_dll_from_another() {
        assert_eq!(inspect(&dll("0.2.0")), Some(Version::parse("0.2.0")));
        assert_eq!(inspect(b"MZ nothing here"), None);
        assert_eq!(inspect(b"=== w2rpm host but no MZ"), None);
        let mut old = b"MZ".to_vec();
        old.extend_from_slice(MARKER_OURS);
        assert_eq!(inspect(&old), Some(None));
    }

    #[test]
    fn first_install_then_update_parks_the_old_one_and_rollback_swaps() {
        let t = TempRoot::new("core1");
        let g = game("core1");
        let v1 = dll("0.1.0");
        update(&t.0, &g, &info_for(&v1, "0.1.0"), &v1, &manager()).unwrap();
        assert_eq!(fs::read(g.join(DLL_NAME)).unwrap(), v1);
        assert!(!parked_path(&t.0).exists());

        let v2 = dll("0.2.0");
        let out = update(&t.0, &g, &info_for(&v2, "0.2.0"), &v2, &manager()).unwrap();
        assert_eq!(out.from, Version::parse("0.1.0"));
        assert_eq!(fs::read(g.join(DLL_NAME)).unwrap(), v2);
        assert_eq!(fs::read(parked_path(&t.0)).unwrap(), v1);
        assert!(!g.join("version.dll.new").exists());

        let back = rollback(&t.0, &g).unwrap();
        assert_eq!(back.to, Version::parse("0.1.0"));
        assert_eq!(fs::read(g.join(DLL_NAME)).unwrap(), v1);
        rollback(&t.0, &g).unwrap();
        assert_eq!(fs::read(g.join(DLL_NAME)).unwrap(), v2);
        let _ = fs::remove_dir_all(&g);
    }

    #[test]
    fn refuses_a_bad_download_another_mod_and_a_wrong_folder() {
        let t = TempRoot::new("core2");
        let g = game("core2");
        let v2 = dll("0.2.0");
        let info = info_for(&v2, "0.2.0");

        let mut tampered = v2.clone();
        tampered.push(b'!');
        assert_eq!(update(&t.0, &g, &info, &tampered, &manager()).unwrap_err().code, "bad_size");
        let mut flipped = v2.clone();
        flipped[0] = b'N';
        assert_eq!(update(&t.0, &g, &info, &flipped, &manager()).unwrap_err().code, "bad_hash");

        // The DLL says 0.3.0 while core.json announces 0.2.0.
        let other = dll("0.3.0");
        let mut lie = info_for(&other, "0.2.0");
        lie.sha256 = Sha256::digest(&other).into();
        assert_eq!(update(&t.0, &g, &lie, &other, &manager()).unwrap_err().code, "mismatch");

        // Not our DLL at all.
        let junk = b"MZ junk".to_vec();
        let junk_info = info_for(&junk, "0.2.0");
        assert_eq!(update(&t.0, &g, &junk_info, &junk, &manager()).unwrap_err().code, "not_core");

        // Too old a manager.
        let mut newer = info.clone();
        newer.min_manager = Version::parse("9.0.0").unwrap();
        assert_eq!(update(&t.0, &g, &newer, &v2, &manager()).unwrap_err().code, "needs_newer_manager");

        // Another mod's version.dll is never overwritten.
        fs::write(g.join(DLL_NAME), b"MZ wc2r-plus").unwrap();
        assert_eq!(update(&t.0, &g, &info, &v2, &manager()).unwrap_err().code, "not_ours");
        assert_eq!(fs::read(g.join(DLL_NAME)).unwrap(), b"MZ wc2r-plus");

        // A folder without the game.
        let empty = std::env::temp_dir().join(format!("w2rpm-nogame-{}", std::process::id()));
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(update(&t.0, &empty, &info, &v2, &manager()).unwrap_err().code, "no_game_dir");
        let _ = fs::remove_dir_all(&empty);
        let _ = fs::remove_dir_all(&g);
    }

    #[test]
    fn update_is_offered_only_for_a_newer_core_of_ours() {
        let info = info_for(&dll("0.2.0"), "0.2.0");
        let m = manager();
        assert!(update_available(&Installed::Missing, &info, &m));
        assert!(update_available(&Installed::Ours(None), &info, &m));
        assert!(update_available(&Installed::Ours(Version::parse("0.1.0")), &info, &m));
        assert!(!update_available(&Installed::Ours(Version::parse("0.2.0")), &info, &m));
        assert!(!update_available(&Installed::Foreign, &info, &m));
    }

    #[test]
    fn game_dir_accepts_the_x86_child_and_remembers_it() {
        let t = TempRoot::new("core3");
        let g = game("core3");
        let parent = g.parent().unwrap().join(format!("w2rpm-parent-{}", std::process::id()));
        let x86 = parent.join("x86");
        fs::create_dir_all(&x86).unwrap();
        fs::write(x86.join(GAME_EXE), b"exe").unwrap();
        assert_eq!(normalize_game_dir(&parent), Some(x86.clone()));
        assert_eq!(resolve_game_dir(&t.0, Some(&parent)).unwrap(), x86);
        assert_eq!(resolve_game_dir(&t.0, Some(Path::new("Z:\\nope"))).unwrap_err().code, "no_game_dir");
        save_game_dir(&t.0, &x86).unwrap();
        assert_eq!(saved_game_dir(&t.0), Some(x86));
        let _ = fs::remove_dir_all(&parent);
        let _ = fs::remove_dir_all(&g);
    }

    #[test]
    fn core_json_cache_keeps_the_serial() {
        struct One(Vec<u8>);
        impl Fetcher for One {
            fn get(&self, _: &str, _: u64) -> Result<Vec<u8>, Error> {
                Ok(self.0.clone())
            }
        }
        let t = TempRoot::new("core4");
        refresh(&t.0, &One(GOOD.as_bytes().to_vec()), "https://x.test/core.json").unwrap();
        assert_eq!(cached_serial(&t.0), 3);
        assert_eq!(load_cached(&t.0).unwrap().version, Version::parse("0.2.0").unwrap());
        let older = GOOD.replace("\"serial\":3", "\"serial\":2");
        let err = refresh(&t.0, &One(older.into_bytes()), "https://x.test/core.json").unwrap_err();
        assert_eq!(err.code, "rollback");
        assert_eq!(cached_serial(&t.0), 3);
    }
}
