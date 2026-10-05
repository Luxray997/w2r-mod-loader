//! Where downloaded maps go (docs/map-download.md, slice 3), ported from wc2r-plus
//! `map_download/map_store.cpp`. A map is written to `<root>\V<n>\<name>`, where the root is
//! `Maps\Download` inside the game's own Maps folder (the game only looks there). Versions never
//! overwrite: the first `V<n>` without that name gets it, so two maps that share a name coexist.
//! The name has already passed [`mapname::validate`] (one path component, no device names).
//!
//! Safety: the root and the version folder must be real directories (reparse points and symlinks
//! are refused), the folder holds at most 64 MiB and 512 files, the file is written as
//! `<name>.part`, flushed and renamed into place, and a half-written file is removed.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::maplimits::{MAX_MAP_BYTES, sha256};
use crate::mapname;

pub const QUOTA_BYTES: u64 = 64 * 1024 * 1024;
pub const QUOTA_FILES: u32 = 512;
pub const MAX_VERSIONS: u32 = 99;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    BadName,
    /// Empty or over the protocol's size limit.
    BadSize,
    QuotaExceeded,
    /// The root's parent is missing, or the root or version folder is not a real directory.
    WriteFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stored {
    Written {
        dir: PathBuf,
        full: PathBuf,
    },
    /// A file with this name and these exact bytes was already there.
    AlreadyPresent {
        dir: PathBuf,
        full: PathBuf,
    },
}

impl Stored {
    pub fn dir(&self) -> &Path {
        match self {
            Stored::Written { dir, .. } | Stored::AlreadyPresent { dir, .. } => dir,
        }
    }

    pub fn full(&self) -> &Path {
        match self {
            Stored::Written { full, .. } | Stored::AlreadyPresent { full, .. } => full,
        }
    }
}

/// A reparse point on Windows (junction, symlink, mount point); a symlink elsewhere.
pub fn is_reparse(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// A real directory: exists, is a directory, and is not a link.
fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !is_reparse(&m))
}

fn ensure_real_dir(path: &Path) -> bool {
    if !path.exists() && fs::create_dir(path).is_err() {
        return false;
    }
    is_real_dir(path)
}

/// The root's parent must already exist (we never create the game's folders), then the root and
/// the version folder are made and checked.
fn ensure_ours(root: &Path, dir: &Path) -> bool {
    let Some(parent) = root.parent() else {
        return false;
    };
    parent.is_dir() && ensure_real_dir(root) && ensure_real_dir(dir)
}

/// Whether `path` is inside `root` (not equal to it), compared case-insensitively as Windows
/// does and by whole components, so `C:\Maps2` is not inside `C:\Maps`. Both should already be
/// canonical (links resolved); this is the string test on top of that.
pub fn path_under(path: &Path, root: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase()
            .trim_end_matches('\\')
            .to_owned()
    };
    let (p, r) = (norm(path), norm(root));
    !r.is_empty() && p.len() > r.len() + 1 && p.starts_with(&r) && p.as_bytes()[r.len()] == b'\\'
}

fn read_limited(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut out)
        .ok()?;
    (out.len() as u64 <= limit).then_some(out)
}

/// Bytes and files already under `root`, links not followed.
fn usage(root: &Path) -> (u64, u32) {
    let (mut bytes, mut files) = (0u64, 0u32);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(meta) = fs::symlink_metadata(e.path()) else {
                continue;
            };
            if meta.is_dir() {
                if !is_reparse(&meta) {
                    stack.push(e.path());
                }
            } else {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// Writes `bytes` (already hashed to `sha` and validated by the caller) under `root`.
pub fn store_map(
    root: &Path,
    name: &[u8],
    bytes: &[u8],
    sha: &[u8; 32],
) -> Result<Stored, StoreError> {
    mapname::validate(name).map_err(|_| StoreError::BadName)?;
    if bytes.is_empty() || bytes.len() > MAX_MAP_BYTES as usize {
        return Err(StoreError::BadSize);
    }
    // Printable ASCII, so this is lossless.
    let name = String::from_utf8_lossy(name).into_owned();

    let (mut dir, mut full) = (PathBuf::new(), PathBuf::new());
    for v in 1..=MAX_VERSIONS + 1 {
        if v > MAX_VERSIONS {
            return Err(StoreError::QuotaExceeded);
        }
        dir = root.join(format!("V{v}"));
        full = dir.join(&name);
        if fs::symlink_metadata(&full).is_err() {
            break;
        }
        if let Some(existing) = read_limited(&full, MAX_MAP_BYTES as u64)
            && existing.len() == bytes.len()
            && sha256(&existing) == *sha
        {
            return Ok(Stored::AlreadyPresent { dir, full });
        }
    }

    let (used, files) = usage(root);
    if used + bytes.len() as u64 > QUOTA_BYTES || files + 1 > QUOTA_FILES {
        return Err(StoreError::QuotaExceeded);
    }
    if !ensure_ours(root, &dir) {
        return Err(StoreError::WriteFailed);
    }
    let part = full.with_file_name(format!("{name}.part"));
    let wrote = fs::File::create(&part).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    });
    if wrote.is_err() || fs::rename(&part, &full).is_err() {
        let _ = fs::remove_file(&part);
        return Err(StoreError::WriteFailed);
    }
    Ok(Stored::Written { dir, full })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh `<temp>\w2r-store-N\Maps\Download` whose parent (`Maps`) exists, like the game's.
    fn root() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir().join(format!(
            "w2r-store-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("Maps")).unwrap();
        base.join("Maps").join("Download")
    }

    fn cleanup(root: &Path) {
        let _ = fs::remove_dir_all(root.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_map_lands_in_the_first_version_folder_and_reads_back() {
        let root = root();
        let data = vec![7u8; 500];
        let sha = sha256(&data);
        let stored = store_map(&root, b"ridge.pud", &data, &sha).unwrap();
        assert!(matches!(stored, Stored::Written { .. }));
        assert_eq!(stored.dir(), root.join("V1"));
        assert_eq!(stored.full(), root.join("V1").join("ridge.pud"));
        assert_eq!(fs::read(stored.full()).unwrap(), data);
        assert!(
            !root.join("V1").join("ridge.pud.part").exists(),
            "no half-written file is left"
        );
        cleanup(&root);
    }

    #[test]
    fn the_same_bytes_again_are_already_present_and_different_bytes_get_the_next_version() {
        let root = root();
        let a = vec![1u8; 100];
        let b = vec![2u8; 100];
        let first = store_map(&root, b"x.pud", &a, &sha256(&a)).unwrap();
        let again = store_map(&root, b"x.pud", &a, &sha256(&a)).unwrap();
        assert!(matches!(again, Stored::AlreadyPresent { .. }));
        assert_eq!(again.full(), first.full());
        let other = store_map(&root, b"x.pud", &b, &sha256(&b)).unwrap();
        assert!(matches!(other, Stored::Written { .. }));
        assert_eq!(other.dir(), root.join("V2"), "never overwritten");
        assert_eq!(fs::read(first.full()).unwrap(), a);
        let back = store_map(&root, b"x.pud", &b, &sha256(&b)).unwrap();
        assert!(
            matches!(back, Stored::AlreadyPresent { .. }),
            "found in a later version too"
        );
        assert_eq!(back.dir(), root.join("V2"));
        cleanup(&root);
    }

    #[test]
    fn bad_names_and_sizes_write_nothing() {
        let root = root();
        let d = vec![1u8; 10];
        let sha = sha256(&d);
        for bad in [&b"..\\x.pud"[..], b"con.pud", b"a/b.pud", b"x.txt", b""] {
            assert_eq!(store_map(&root, bad, &d, &sha), Err(StoreError::BadName));
        }
        assert_eq!(
            store_map(&root, b"x.pud", &[], &sha),
            Err(StoreError::BadSize)
        );
        let big = vec![0u8; MAX_MAP_BYTES as usize + 1];
        assert_eq!(
            store_map(&root, b"x.pud", &big, &sha),
            Err(StoreError::BadSize)
        );
        assert!(!root.exists(), "nothing was created for a refused map");
        cleanup(&root);
    }

    #[test]
    fn the_roots_parent_must_exist_and_a_file_in_the_way_is_refused() {
        let d = vec![1u8; 10];
        let sha = sha256(&d);
        let nowhere = std::env::temp_dir()
            .join(format!("w2r-nowhere-{}", std::process::id()))
            .join("Maps")
            .join("Download");
        assert_eq!(
            store_map(&nowhere, b"x.pud", &d, &sha),
            Err(StoreError::WriteFailed)
        );
        let root = root();
        fs::write(&root, b"a file named Download").unwrap();
        assert_eq!(
            store_map(&root, b"x.pud", &d, &sha),
            Err(StoreError::WriteFailed)
        );
        cleanup(&root);
    }

    #[test]
    fn containment_is_by_component_and_ignores_case() {
        let root = Path::new(r"C:\Games\W2\Maps");
        let under = |p: &str, r: &Path| path_under(Path::new(p), r);
        assert!(under(r"C:\Games\W2\Maps\a.pud", root));
        assert!(under(r"c:\games\w2\MAPS\Sub\a.pud", root));
        assert!(under("C:/Games/W2/Maps/a.pud", root), "either slash");
        assert!(under(
            r"C:\Games\W2\Maps\a.pud",
            Path::new(r"C:\Games\W2\Maps\")
        ));
        assert!(
            !under(r"C:\Games\W2\Maps2\a.pud", root),
            "a sibling that starts the same"
        );
        assert!(!under(r"C:\Games\W2\Maps", root), "the root itself");
        assert!(!under(r"C:\Games\W2\Mapsa.pud", root));
        assert!(!under(r"C:\Other\Maps\a.pud", root));
        assert!(!under(r"C:\x", Path::new("")));
    }

    #[test]
    fn the_folder_is_capped_in_files() {
        let root = root();
        fs::create_dir_all(root.join("V1")).unwrap();
        for i in 0..QUOTA_FILES {
            fs::write(root.join("V1").join(format!("m{i}.pud")), b"x").unwrap();
        }
        let d = vec![1u8; 10];
        assert_eq!(
            store_map(&root, b"new.pud", &d, &sha256(&d)),
            Err(StoreError::QuotaExceeded)
        );
        cleanup(&root);
    }

    #[test]
    fn the_folder_is_capped_in_bytes() {
        let root = root();
        fs::create_dir_all(root.join("V1")).unwrap();
        fs::write(
            root.join("V1").join("big.bin"),
            vec![0u8; (QUOTA_BYTES - 100) as usize],
        )
        .unwrap();
        let d = vec![1u8; 200];
        assert_eq!(
            store_map(&root, b"new.pud", &d, &sha256(&d)),
            Err(StoreError::QuotaExceeded)
        );
        let small = vec![1u8; 50];
        assert!(store_map(&root, b"fits.pud", &small, &sha256(&small)).is_ok());
        cleanup(&root);
    }

    #[test]
    fn version_folders_are_limited() {
        let root = root();
        for v in 1..=MAX_VERSIONS {
            fs::create_dir_all(root.join(format!("V{v}"))).unwrap();
            fs::write(root.join(format!("V{v}")).join("x.pud"), [v as u8]).unwrap();
        }
        let d = vec![9u8; 20];
        assert_eq!(
            store_map(&root, b"x.pud", &d, &sha256(&d)),
            Err(StoreError::QuotaExceeded)
        );
        cleanup(&root);
    }
}
