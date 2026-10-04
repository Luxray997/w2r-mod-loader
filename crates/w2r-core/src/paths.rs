//! Where the host keeps its files: `<Saved Games>\Warcraft2Remastered\w2r\` (docs/layout.md).
//! Per-user, no administrator rights. The game folder only ever holds the proxy DLL.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{FOLDERID_SavedGames, KF_FLAG_CREATE, SHGetKnownFolderPath};

pub const GAME_FOLDER: &str = "Warcraft2Remastered";
pub const HOST_FOLDER: &str = "w2r";
pub const SUBDIRS: [&str; 4] = ["plugins", "config", "cache", "logs"];

/// The user's Saved Games folder, created if missing.
pub fn saved_games() -> Option<PathBuf> {
    let mut wide: *mut u16 = core::ptr::null_mut();
    let hr = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_SavedGames,
            KF_FLAG_CREATE as u32,
            core::ptr::null_mut(),
            &mut wide,
        )
    };
    if wide.is_null() {
        return None;
    }
    let result = if hr < 0 {
        None
    } else {
        let len = (0..).take_while(|&i| unsafe { *wide.add(i) } != 0).count();
        let s = OsString::from_wide(unsafe { core::slice::from_raw_parts(wide, len) });
        Some(PathBuf::from(s))
    };
    unsafe { CoTaskMemFree(wide.cast()) };
    result
}

/// Creates the subfolders under `root`.
pub fn prepare(root: &Path) -> io::Result<()> {
    for sub in SUBDIRS {
        std::fs::create_dir_all(root.join(sub))?;
    }
    Ok(())
}

/// Environment variable that replaces the Saved Games location (development and tests).
pub const ROOT_OVERRIDE: &str = "W2R_ROOT";

/// Where the root came from, for the log: a user looking in Saved Games should learn that an
/// override sent the files elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    SavedGames,
    /// `W2R_ROOT` was set to an absolute path.
    Override,
    /// `W2R_ROOT` was set but not absolute, so it was ignored.
    RelativeOverrideIgnored,
}

pub struct Root {
    pub path: PathBuf,
    pub origin: Origin,
}

/// The host root with its subfolders in place, or `None` when the user profile is unusable (the
/// host then does nothing: no log, no plugins). An override must be absolute: a relative one
/// would resolve against the game's working directory, which is not the user's choice.
pub fn root() -> Option<Root> {
    let (path, origin) = match std::env::var_os(ROOT_OVERRIDE).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => (dir, Origin::Override),
        Some(_) => (default_root()?, Origin::RelativeOverrideIgnored),
        None => (default_root()?, Origin::SavedGames),
    };
    prepare(&path).ok()?;
    Some(Root { path, origin })
}

fn default_root() -> Option<PathBuf> {
    Some(saved_games()?.join(GAME_FOLDER).join(HOST_FOLDER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_games_resolves_to_an_existing_absolute_folder() {
        let p = saved_games().expect("known folder");
        assert!(p.is_absolute() && p.is_dir(), "{}", p.display());
    }

    #[test]
    fn prepare_creates_every_subfolder_under_a_temp_root() {
        let root = std::env::temp_dir().join(format!("w2r-paths-test-{}", std::process::id()));
        prepare(&root).unwrap();
        for sub in SUBDIRS {
            assert!(root.join(sub).is_dir(), "{sub}");
        }
        prepare(&root).unwrap(); // idempotent
        let _ = std::fs::remove_dir_all(&root);
    }
}
