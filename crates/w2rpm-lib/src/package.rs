//! Package verification and safe extraction (docs/index.md, "Package verification").
//!
//! `extract` checks size and SHA-256 against the signed index entry before it opens the archive,
//! applies the archive rules entry by entry, writes only into a fresh staging directory and
//! removes that directory on the first violation. Only four names are ever written, each created
//! by us (`create_new`), so a name inside the archive never reaches the filesystem as a path.

use crate::Error;
use crate::index::VersionEntry;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::Path;
use w2r_formats::manifest;

pub const MAX_ENTRIES: usize = 64;
pub const MAX_UNCOMPRESSED: u64 = 128 * 1024 * 1024;
pub const MAX_RATIO: u64 = 100;

/// What a successful extraction produced.
#[derive(Clone, Debug)]
pub struct Extracted {
    /// SHA-256 of the entry DLL: the value pinned in `state.json`.
    pub entry_sha256: [u8; 32],
    /// Names written to the staging directory.
    pub files: Vec<String>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Verifies `bytes` as the package for `entry` and extracts it into `staging`, which must not
/// exist yet. On any error `staging` is removed.
pub fn extract(bytes: &[u8], entry: &VersionEntry, staging: &Path) -> Result<Extracted, Error> {
    if bytes.len() as u64 != entry.size {
        return Err(Error::new(
            "size_mismatch",
            format!(
                "package is {} bytes, the index says {}",
                bytes.len(),
                entry.size
            ),
        ));
    }
    if Sha256::digest(bytes).as_slice() != entry.sha256 {
        return Err(Error::new(
            "hash_mismatch",
            "package SHA-256 differs from the index",
        ));
    }
    fs::create_dir(staging)?;
    let result = extract_into(bytes, entry, staging);
    if result.is_err() {
        let _ = fs::remove_dir_all(staging);
    }
    result
}

/// Entry count from the end-of-central-directory record (the last 22 + comment bytes).
fn declared_entries(b: &[u8]) -> Option<usize> {
    let last = b.len().checked_sub(22)?;
    (last.saturating_sub(65_535)..=last).rev().find_map(|p| {
        let comment = u16::from_le_bytes([b[p + 20], b[p + 21]]) as usize;
        (b[p..p + 4] == [0x50, 0x4b, 0x05, 0x06] && p + 22 + comment == b.len())
            .then(|| u16::from_le_bytes([b[p + 10], b[p + 11]]) as usize)
    })
}

fn extract_into(bytes: &[u8], entry: &VersionEntry, dir: &Path) -> Result<Extracted, Error> {
    let bad = |detail: String| Error::new("bad_archive", detail);
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| bad(e.to_string()))?;
    // The reader merges entries that share a name; the directory header still counts them all.
    let declared =
        declared_entries(bytes).ok_or_else(|| bad("no end-of-central-directory record".into()))?;
    if declared != zip.len() {
        return Err(Error::new("collision", "two entries share a name"));
    }
    if zip.len() > MAX_ENTRIES {
        return Err(Error::new(
            "too_many",
            format!("{} entries, at most {MAX_ENTRIES}", zip.len()),
        ));
    }
    let wanted_entry = entry.manifest.entry.as_str();
    let allowed = ["plugin.json", wanted_entry, "README.md", "LICENSE"];

    let mut seen: HashSet<String> = HashSet::new();
    let mut total: u64 = 0;
    let mut files = Vec::new();
    let mut manifest_raw: Option<Vec<u8>> = None;
    let mut entry_sha = None;

    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| bad(e.to_string()))?;
        let name = f.name().to_owned();
        if f.is_dir() || f.is_symlink() || !f.is_file() {
            return Err(Error::new(
                "bad_entry_name",
                format!("{name:?} is not a regular file"),
            ));
        }
        if !allowed.contains(&name.as_str()) {
            return Err(Error::new(
                "bad_entry_name",
                format!("{name:?} is not allowed in a package"),
            ));
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(Error::new("collision", format!("{name:?} appears twice")));
        }
        let declared = f.size();
        if declared > MAX_UNCOMPRESSED || total + declared > MAX_UNCOMPRESSED {
            return Err(Error::new("too_large", "archive expands past 128 MiB"));
        }
        if declared > 0 && declared > f.compressed_size().saturating_mul(MAX_RATIO) {
            return Err(Error::new(
                "ratio",
                format!("{name:?} compresses more than {MAX_RATIO}:1"),
            ));
        }
        // Never trust the declared size: read at most one byte past it.
        let mut data = Vec::new();
        f.by_ref()
            .take(declared + 1)
            .read_to_end(&mut data)
            .map_err(|e| bad(format!("{name:?}: {e}")))?;
        if data.len() as u64 != declared {
            return Err(bad(format!("{name:?} is not the size it declares")));
        }
        total += declared;
        if name == "plugin.json" {
            if declared > manifest::MAX_BYTES as u64 {
                return Err(Error::new("bad_manifest", "too_large"));
            }
            manifest_raw = Some(data.clone());
        }
        if name == wanted_entry {
            entry_sha = Some(Sha256::digest(&data).into());
        }
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(&name))?;
        out.write_all(&data)?;
        out.sync_all()?;
        files.push(name);
    }

    let raw = manifest_raw.ok_or_else(|| Error::new("missing_file", "plugin.json"))?;
    let packaged = manifest::validate(&raw).map_err(|r| Error::new("bad_manifest", r))?;
    if packaged != entry.manifest {
        return Err(Error::new(
            "manifest_mismatch",
            "packaged plugin.json differs from the index",
        ));
    }
    let entry_sha256 =
        entry_sha.ok_or_else(|| Error::new("missing_file", wanted_entry.to_owned()))?;
    Ok(Extracted {
        entry_sha256,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{self, Index};
    use w2r_formats::json;
    use zip::write::SimpleFileOptions;

    const VECTORS: &str = include_str!("../../../docs/vectors/index.json");

    fn manifest_json() -> String {
        // The manifest of the "valid" vector, written out so the tests do not depend on it.
        r#"{"schema":1,"id":"example-plugin","name":"Example Plugin","version":"1.2.0","author":"someone","description":"One sentence.","entry":"example_plugin.dll","abi":1,"game_builds":["1.0.2.2818"],"capabilities":["game.read","chat.read"],"affects_simulation":false,"dependencies":[]}"#.to_owned()
    }

    fn patch(z: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        let mut out = z.to_vec();
        let mut i = 0;
        while i + from.len() <= out.len() {
            if &out[i..i + from.len()] == from {
                out[i..i + from.len()].copy_from_slice(to);
                i += from.len();
            } else {
                i += 1;
            }
        }
        out
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn entry_for(zip: &[u8]) -> VersionEntry {
        let doc = json::parse(VECTORS.as_bytes()).unwrap();
        let case = &doc.get("cases").unwrap().as_array().unwrap()[0];
        let raw = case.get("index").unwrap().as_str().unwrap();
        let idx: Index = index::parse_index(raw.as_bytes(), 5).unwrap();
        let mut e = idx.plugins[0].versions[0].clone();
        e.size = zip.len() as u64;
        e.sha256 = Sha256::digest(zip).into();
        e
    }

    fn stage(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("w2rpm-pkg-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn run(name: &str, files: &[(&str, &[u8])]) -> (Result<Extracted, Error>, std::path::PathBuf) {
        let z = zip_of(files);
        let e = entry_for(&z);
        let d = stage(name);
        (extract(&z, &e, &d), d)
    }

    fn good() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("plugin.json", manifest_json().into_bytes()),
            ("example_plugin.dll", b"MZ-dll".to_vec()),
        ]
    }

    fn refs<'a>(v: &'a [(&'static str, Vec<u8>)]) -> Vec<(&'static str, &'a [u8])> {
        v.iter().map(|(n, d)| (*n, d.as_slice())).collect()
    }

    #[test]
    fn accepts_a_good_package() {
        let g = good();
        let (r, d) = run("ok", &refs(&g));
        let ex = r.unwrap();
        assert_eq!(
            ex.entry_sha256.as_slice(),
            Sha256::digest(b"MZ-dll").as_slice()
        );
        assert_eq!(fs::read(d.join("example_plugin.dll")).unwrap(), b"MZ-dll");
        let _ = fs::remove_dir_all(d);
    }

    fn refused(name: &str, files: &[(&str, &[u8])], code: &str) {
        let (r, d) = run(name, files);
        assert_eq!(r.unwrap_err().code, code, "{name}");
        assert!(!d.exists(), "{name}: staging must be removed");
    }

    #[test]
    fn refuses_names_outside_the_allowlist() {
        let m = manifest_json().into_bytes();
        for (n, bad) in [
            ("traversal", "../evil.dll"),
            ("nested", "sub/x.dll"),
            ("backslash", "..\\evil.dll"),
            ("absolute", "/etc/passwd"),
            ("drive", "C:evil.dll"),
            ("unc", "//host/share/x"),
            ("extra", "extra.dll"),
            ("dir", "dir/"),
            ("case", "Plugin.json"),
        ] {
            refused(
                n,
                &[
                    ("plugin.json", &m),
                    ("example_plugin.dll", b"x"),
                    (bad, b"x"),
                ],
                "bad_entry_name",
            );
        }
    }

    #[test]
    fn refuses_duplicates_missing_files_and_mismatches() {
        let m = manifest_json().into_bytes();
        // ZipWriter refuses duplicate names, so write a decoy name and patch it in the bytes.
        let z = zip_of(&[
            ("plugin.json", &m),
            ("plugin.jsoX", &m),
            ("example_plugin.dll", b"x"),
        ]);
        let patched = patch(&z, b"plugin.jsoX", b"plugin.json");
        let e = entry_for(&patched);
        let d = stage("dup");
        assert_eq!(extract(&patched, &e, &d).unwrap_err().code, "collision");
        assert!(!d.exists());
        refused("nodll", &[("plugin.json", &m)], "missing_file");
        refused("nojson", &[("example_plugin.dll", b"x")], "missing_file");
        let other = manifest_json()
            .replace("One sentence.", "Another.")
            .into_bytes();
        refused(
            "differs",
            &[("plugin.json", &other), ("example_plugin.dll", b"x")],
            "manifest_mismatch",
        );
        refused(
            "junk",
            &[("plugin.json", b"{not json"), ("example_plugin.dll", b"x")],
            "bad_manifest",
        );
    }

    #[test]
    fn refuses_hash_size_and_garbage() {
        let g = good();
        let z = zip_of(&refs(&g));
        let e = entry_for(&z);
        let d = stage("hash");
        let mut tampered = z.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_eq!(
            extract(&tampered, &e, &d).unwrap_err().code,
            "hash_mismatch"
        );
        let mut short = e.clone();
        short.size += 1;
        assert_eq!(extract(&z, &short, &d).unwrap_err().code, "size_mismatch");
        assert!(!d.exists());
        let junk = vec![7u8; 100];
        let mut e2 = entry_for(&junk);
        e2.size = 100;
        assert_eq!(extract(&junk, &e2, &d).unwrap_err().code, "bad_archive");
        assert!(!d.exists());
    }

    #[test]
    fn refuses_a_compression_bomb_and_too_many_entries() {
        let m = manifest_json().into_bytes();
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("plugin.json", SimpleFileOptions::default())
            .unwrap();
        w.write_all(&m).unwrap();
        w.start_file(
            "example_plugin.dll",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        w.write_all(&vec![0u8; 8 * 1024 * 1024]).unwrap();
        let z = w.finish().unwrap().into_inner();
        let e = entry_for(&z);
        let d = stage("bomb");
        assert_eq!(extract(&z, &e, &d).unwrap_err().code, "ratio");
        assert!(!d.exists());

        let names: Vec<String> = (0..65).map(|i| format!("f{i}")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        refused("many", &files, "too_many");
    }

    #[test]
    fn refuses_a_symlink_entry() {
        let m = manifest_json().into_bytes();
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("plugin.json", SimpleFileOptions::default())
            .unwrap();
        w.write_all(&m).unwrap();
        w.add_symlink(
            "example_plugin.dll",
            "C:/Windows/System32/x.dll",
            SimpleFileOptions::default(),
        )
        .unwrap();
        let z = w.finish().unwrap().into_inner();
        let e = entry_for(&z);
        let d = stage("sym");
        assert_eq!(extract(&z, &e, &d).unwrap_err().code, "bad_entry_name");
        assert!(!d.exists());
    }
}
