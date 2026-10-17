//! "Is a newer manager published?" `manager.json` sits next to `index.json`:
//! `{"schema":1,"version":"0.3.0","url":"https://..."}` where `url` is the page to get it from.
//! Unknown fields are ignored so it can grow, and a missing or bad file just means no notice.
//!
//! Three more fields, all or none, let the manager replace itself: `package_url` (the GUI exe
//! itself, no zip), `sha256` and `size`. Without them the notice only links to `url`.
//! `install_self` checks size, hash and the version marker the exe carries, then swaps files; it
//! never runs the new exe (the caller restarts).

use crate::Error;
use crate::fetch::{Fetcher, write_atomic};
use crate::index::{MAX_PACKAGE_BYTES, hex32, url_ok};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use w2r_formats::json::{self, Value};
use w2r_formats::semver::Version;

const MAX_BYTES: u64 = 16 * 1024;
/// `W2R-LOADER-VERSION:<semver>;`, embedded by the GUI crate.
pub const MARKER_VERSION: &[u8] = b"W2R-LOADER-VERSION:";

/// The exe a self-update downloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub url: String,
    pub sha256: [u8; 32],
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Latest {
    pub version: Version,
    pub url: String,
    /// Present when `manager.json` offers the exe itself.
    pub package: Option<Package>,
}

/// `manager.json` lives in the same folder as `index.json`.
pub fn url_for(index_url: &str) -> Option<String> {
    index_url
        .rsplit_once('/')
        .map(|(dir, _)| format!("{dir}/manager.json"))
}

pub fn parse(raw: &[u8]) -> Option<Latest> {
    let doc = json::parse(raw).ok()?;
    if doc.get("schema")?.as_number()?.as_u64()? != 1 {
        return None;
    }
    let version = Version::parse(doc.get("version").and_then(Value::as_str)?)?;
    let url = doc.get("url").and_then(Value::as_str)?.to_owned();
    if !url_ok(&url) {
        return None;
    }
    // All three package fields or none; a half-filled set offers only the link.
    let package = (|| {
        let url = doc.get("package_url")?.as_str()?.to_owned();
        let sha256 = hex32(doc.get("sha256")?.as_str()?)?;
        let size = doc.get("size")?.as_number()?.as_u64()?;
        (url_ok(&url) && (1..=MAX_PACKAGE_BYTES).contains(&size)).then_some(Package {
            url,
            sha256,
            size,
        })
    })();
    Some(Latest {
        version,
        url,
        package,
    })
}

/// The published manager when it is newer than `current`.
pub fn newer(fetcher: &dyn Fetcher, index_url: &str, current: &Version) -> Option<Latest> {
    let raw = fetcher.get(&url_for(index_url)?, MAX_BYTES).ok()?;
    parse(&raw).filter(|l| l.version > *current)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The version a manager exe carries; `None` when the bytes are not a manager exe.
pub fn inspect(bytes: &[u8]) -> Option<Version> {
    if !bytes.starts_with(b"MZ") {
        return None;
    }
    let at = find(bytes, MARKER_VERSION)?;
    let rest = &bytes[at + MARKER_VERSION.len()..];
    let end = rest.iter().take(40).position(|&c| c == b';')?;
    Version::parse(std::str::from_utf8(&rest[..end]).ok()?)
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// `<exe>.old`: where the running exe is moved to during the swap.
pub fn old_path(exe: &Path) -> PathBuf {
    sibling(exe, ".old")
}

/// Removes the leftover of an earlier swap; call once at startup. Best effort: the old exe may
/// still be shutting down.
pub fn cleanup_old(exe: &Path) {
    let _ = fs::remove_file(old_path(exe));
}

/// Downloads `latest.package` and replaces `exe` with it. Windows will not let a running exe be
/// overwritten but does let it be renamed, so: write `<exe>.new`, move the running file to
/// `<exe>.old`, move the new one into place (and move the old back if that fails). The caller
/// starts the new exe and quits. Refuses a version that is not above `current`.
pub fn install_self(
    fetcher: &dyn Fetcher,
    exe: &Path,
    latest: &Latest,
    current: &Version,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Version, Error> {
    let Some(pkg) = &latest.package else {
        return Err(Error::new("no_package", "this release has no direct download"));
    };
    if latest.version <= *current {
        return Err(Error::new("not_newer", "the published manager is not newer"));
    }
    let bytes = fetcher.get_with_progress(&pkg.url, pkg.size, progress)?;
    swap_in(exe, &bytes, latest, pkg)?;
    Ok(latest.version.clone())
}

fn swap_in(exe: &Path, bytes: &[u8], latest: &Latest, pkg: &Package) -> Result<(), Error> {
    if bytes.len() as u64 != pkg.size {
        return Err(Error::new("bad_size", "the download is not the announced size"));
    }
    if Sha256::digest(bytes).as_slice() != pkg.sha256 {
        return Err(Error::new("bad_hash", "the download does not match manager.json"));
    }
    if inspect(bytes).as_ref() != Some(&latest.version) {
        return Err(Error::new(
            "mismatch",
            "the download is not the W2R Mod Loader version manager.json announces",
        ));
    }
    let staged = sibling(exe, ".new");
    let old = old_path(exe);
    write_atomic(&staged, bytes).inspect_err(|_| {
        let _ = fs::remove_file(&staged);
    })?;
    let _ = fs::remove_file(&old);
    if let Err(e) = fs::rename(exe, &old) {
        let _ = fs::remove_file(&staged);
        return Err(Error::new(
            "io",
            format!("cannot move {} aside ({e}); is the folder read-only?", exe.display()),
        ));
    }
    if let Err(e) = fs::rename(&staged, exe) {
        let _ = fs::rename(&old, exe);
        let _ = fs::remove_file(&staged);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_refuses() {
        let ok = br#"{"schema":1,"version":"0.3.0","url":"https://example.com/w2rpm"}"#;
        assert_eq!(parse(ok).unwrap().version, Version::parse("0.3.0").unwrap());
        assert!(parse(ok).unwrap().package.is_none());
        assert!(parse(br#"{"schema":2,"version":"0.3.0","url":"https://example.com/"}"#).is_none());
        assert!(parse(br#"{"schema":1,"version":"0.3.0","url":"http://example.com/"}"#).is_none());
        assert!(parse(b"nope").is_none());
        assert_eq!(
            url_for("https://x.io/p/index.json").unwrap(),
            "https://x.io/p/manager.json"
        );
    }

    fn exe(version: &str) -> Vec<u8> {
        let mut b = b"MZ....".to_vec();
        b.extend_from_slice(MARKER_VERSION);
        b.extend_from_slice(version.as_bytes());
        b.extend_from_slice(b";");
        b
    }

    fn latest_for(bytes: &[u8], version: &str) -> Latest {
        Latest {
            version: Version::parse(version).unwrap(),
            url: "https://example.com/".into(),
            package: Some(Package {
                url: "https://example.com/w2r.exe".into(),
                sha256: Sha256::digest(bytes).into(),
                size: bytes.len() as u64,
            }),
        }
    }

    #[test]
    fn package_fields_are_all_or_none() {
        let h = "0".repeat(64);
        let full = format!(
            r#"{{"schema":1,"version":"0.3.0","url":"https://e.com/","package_url":"https://e.com/a.exe","sha256":"{h}","size":10}}"#
        );
        assert_eq!(parse(full.as_bytes()).unwrap().package.unwrap().size, 10);
        let half = full.replace(r#","size":10"#, "");
        assert!(parse(half.as_bytes()).unwrap().package.is_none());
        let http = full.replace("https://e.com/a.exe", "http://e.com/a.exe");
        assert!(parse(http.as_bytes()).unwrap().package.is_none());
    }

    struct One(Vec<u8>);
    impl Fetcher for One {
        fn get(&self, _: &str, _: u64) -> Result<Vec<u8>, Error> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn self_install_swaps_the_files_and_refuses_bad_downloads() {
        let dir = std::env::temp_dir().join(format!("w2rpm-self-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let exe_path = dir.join("W2R Mod Loader.exe");
        let v1 = exe("1.0.0");
        fs::write(&exe_path, &v1).unwrap();
        let cur = Version::parse("1.0.0").unwrap();
        let v2 = exe("1.1.0");
        let latest = latest_for(&v2, "1.1.0");
        let noop = &mut |_: u64, _: u64| {};
        let code = |r: Result<Version, Error>| r.unwrap_err().code;

        // Tampered, wrong version, not newer, no package: the exe is untouched.
        let mut bad = v2.clone();
        bad[0] = b'N';
        assert_eq!(code(install_self(&One(bad), &exe_path, &latest, &cur, noop)), "bad_hash");
        let other = exe("1.2.0");
        let lie = latest_for(&other, "1.1.0");
        assert_eq!(code(install_self(&One(other), &exe_path, &lie, &cur, noop)), "mismatch");
        assert_eq!(
            code(install_self(&One(v2.clone()), &exe_path, &latest, &latest.version, noop)),
            "not_newer"
        );
        let mut bare = latest.clone();
        bare.package = None;
        assert_eq!(code(install_self(&One(v2.clone()), &exe_path, &bare, &cur, noop)), "no_package");
        assert_eq!(fs::read(&exe_path).unwrap(), v1);
        assert!(!sibling(&exe_path, ".new").exists());

        let got = install_self(&One(v2.clone()), &exe_path, &latest, &cur, noop).unwrap();
        assert_eq!(got, latest.version);
        assert_eq!(fs::read(&exe_path).unwrap(), v2);
        assert_eq!(fs::read(old_path(&exe_path)).unwrap(), v1);
        assert!(!sibling(&exe_path, ".new").exists());
        cleanup_old(&exe_path);
        assert!(!old_path(&exe_path).exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
