//! What the CLI and the GUI both need before they can do anything: the w2r folder, an index
//! source, a fetcher, the clock and this manager's version. No decisions live here beyond "refuse
//! rather than guess" (docs/manager.md).

use crate::Error;
use crate::fetch::{self, DirFetcher, Fetcher, Https, Source};
use crate::index::{Index, VersionEntry};
use crate::store::Root;
use crate::{catalog, check, coremod, install};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use w2r_formats::semver::Version;

/// `%USERPROFILE%\Saved Games\Warcraft2Remastered\w2r` (docs/layout.md).
pub fn default_root() -> Result<PathBuf, Error> {
    let home = std::env::var_os("USERPROFILE")
        .ok_or_else(|| Error::new("no_root", "USERPROFILE is not set; pass --root"))?;
    Ok(PathBuf::from(home)
        .join("Saved Games")
        .join("Warcraft2Remastered")
        .join("w2r"))
}

/// An explicit root must be absolute; none means the default.
pub fn resolve_root(explicit: Option<&Path>) -> Result<Root, Error> {
    match explicit {
        Some(p) if p.is_absolute() => Ok(Root(p.to_path_buf())),
        Some(p) => Err(Error::new(
            "no_root",
            format!("--root must be absolute: {}", p.display()),
        )),
        None => Ok(Root(default_root()?)),
    }
}

/// The index this manager reads unless told otherwise (`w2r-plugins` on GitHub Pages).
pub const DEFAULT_INDEX_URL: &str = "https://hastinbe.github.io/w2r-plugins/index.json";

/// Seconds since 1970. No clock means no expiry check: refuse rather than treat it as 1970.
pub fn now() -> Result<i64, Error> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::new("no_clock", "the system clock is before 1970"))?
        .as_secs() as i64)
}

/// A manager's connection to one root and one index source.
pub struct Session {
    pub root: Root,
    pub fetcher: Arc<dyn Fetcher + Send + Sync>,
    pub source: Source,
    /// Where `core.json` is published: next to the index.
    pub core_url: String,
    pub manager: Version,
}

/// `core.json` lives in the same folder as `index.json`.
pub fn core_url_for(index_url: &str) -> String {
    match index_url.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/core.json"),
        None => "https://dev.invalid/core.json".to_owned(),
    }
}

impl Session {
    pub fn new(
        root: Root,
        index_url: Option<String>,
        index_dir: Option<PathBuf>,
    ) -> Result<Session, Error> {
        let (fetcher, index_url): (Arc<dyn Fetcher + Send + Sync>, String) =
            match (index_dir, index_url) {
                (Some(dir), url) => (
                    Arc::new(DirFetcher(dir)),
                    url.unwrap_or_else(|| "https://dev.invalid/index.json".to_owned()),
                ),
                (None, Some(url)) => (Arc::new(Https::default()), url),
                (None, None) => (Arc::new(Https::default()), DEFAULT_INDEX_URL.to_owned()),
            };
        let core_url = core_url_for(&index_url);
        Ok(Session {
            root,
            fetcher,
            source: Source { index_url },
            core_url,
            manager: Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is SemVer"),
        })
    }

    /// The cached index when it still parses, else a fresh one.
    pub fn index(&self) -> Result<Index, Error> {
        match fetch::load_cached(&self.root) {
            Ok(i) => Ok(i),
            Err(_) => self.refresh(),
        }
    }

    /// The cached index only; never touches the network.
    pub fn cached_index(&self) -> Result<Index, Error> {
        fetch::load_cached(&self.root)
    }

    pub fn refresh(&self) -> Result<Index, Error> {
        fetch::refresh(&self.root, self.fetcher.as_ref(), &self.source)
    }

    /// The newest core, freshly fetched.
    pub fn core_refresh(&self) -> Result<coremod::CoreInfo, Error> {
        coremod::refresh(&self.root, self.fetcher.as_ref(), &self.core_url)
    }

    /// Downloads, verifies and installs the core into `game_dir` (the game must be closed).
    pub fn core_update(
        &self,
        game_dir: &Path,
        info: &coremod::CoreInfo,
    ) -> Result<coremod::Outcome, Error> {
        self.core_update_with_progress(game_dir, info, &mut |_, _| {})
    }

    /// `core_update`, reporting `(bytes so far, total)` while it downloads.
    pub fn core_update_with_progress(
        &self,
        game_dir: &Path,
        info: &coremod::CoreInfo,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<coremod::Outcome, Error> {
        let bytes = self
            .fetcher
            .get_with_progress(&info.url, info.size, progress)?;
        coremod::update(&self.root, game_dir, info, &bytes, &self.manager)
    }

    /// What an install of `entry` would ask and warn about, for the caller to show.
    pub fn preflight(&self, idx: &Index, entry: &VersionEntry) -> Result<Preflight, Error> {
        let state = self.root.load_state()?;
        Ok(Preflight {
            pending: install::pending_approvals(&state, entry),
            warnings: check::warnings(&state, Some(idx), entry, &self.manager),
        })
    }

    /// Downloads, verifies and installs `entry`, with the capabilities the user approved.
    pub fn install(
        &self,
        entry: &VersionEntry,
        approved: &[String],
    ) -> Result<install::Outcome, Error> {
        self.install_with_progress(entry, approved, &mut |_, _| {})
    }

    /// `install`, reporting `(bytes so far, total)` while it downloads.
    pub fn install_with_progress(
        &self,
        entry: &VersionEntry,
        approved: &[String],
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<install::Outcome, Error> {
        let bytes = fetch::download_with_progress(self.fetcher.as_ref(), entry, progress)?;
        install::install(&self.root, entry, &bytes, approved, &self.manager).map_err(hint_io)
    }

    /// The version `update` would install for each installed plugin that has a newer one.
    pub fn updates<'a>(&self, idx: &'a Index) -> Result<Vec<(String, &'a VersionEntry)>, Error> {
        let state = self.root.load_state()?;
        Ok(state
            .plugins
            .iter()
            .filter_map(|s| {
                let v = catalog::latest(idx.plugin(&s.id)?, &self.manager)?;
                (v.version > s.version).then(|| (s.id.clone(), v))
            })
            .collect())
    }
}

pub struct Preflight {
    /// Capabilities the user has not approved for this plugin yet.
    pub pending: Vec<String>,
    /// Compatibility warnings (never blocking).
    pub warnings: Vec<String>,
}

/// Folder moves fail while the game holds a plugin DLL open.
pub fn hint_io(e: Error) -> Error {
    if e.code == "io" {
        Error::new(
            "io",
            format!("{} (is the game running? close it and try again)", e.detail),
        )
    } else {
        e
    }
}

/// One line for the consent prompt.
pub fn capability_note(c: &str) -> &'static str {
    match c {
        "game.read" => "read game state",
        "chat.read" => "see chat text and player names from others",
        "chat.write" => "send chat messages as you",
        "net.send" => "send plugin messages to other players",
        "hooks.raw" => "DANGEROUS: patch the game's code directly",
        _ => "",
    }
}
