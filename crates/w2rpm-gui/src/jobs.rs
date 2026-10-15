//! What the window can ask for, run off the UI thread, and the snapshot it shows afterwards.
//! No UI code here, so tests drive it against a signed test repository. All real work is in
//! `w2rpm-lib`; this file only sequences calls (docs/manager.md).

use std::path::PathBuf;
use w2r_formats::semver::Version;
use w2rpm_lib::index::Index;
use w2rpm_lib::session::{self, Session};
use w2rpm_lib::store::{self, Finding, Root};
use w2rpm_lib::coremod::{self, Installed as CoreInstalled};
use w2rpm_lib::{Error, catalog, install};

#[derive(Clone, Debug)]
pub enum Job {
    /// Fetch and verify a fresh index.
    Refresh,
    /// Install `version`, or the latest installable one, with the capabilities the user ticked.
    Install {
        id: String,
        version: Option<Version>,
        approved: Vec<String>,
    },
    Remove {
        id: String,
        purge: bool,
    },
    SetEnabled {
        id: String,
        on: bool,
    },
    Rollback {
        id: String,
    },
    /// Re-check installed files against their pins (also done after every other job).
    Verify,
    /// Fetch `core.json` to see what is published.
    CoreCheck,
    /// Install or update the core mod (`version.dll`); the game must be closed.
    CoreUpdate,
    /// Swap the previous core back in (again to swap forward).
    CoreRollback,
    /// Remember the folder the user picked as the game folder.
    CoreGameDir { dir: PathBuf },
}

pub fn run(session: &Session, job: &Job) -> Result<String, Error> {
    match job {
        Job::Refresh => {
            let idx = session.refresh()?;
            Ok(format!(
                "index serial {}: {} plugins",
                idx.serial,
                idx.plugins.len()
            ))
        }
        Job::Install {
            id,
            version,
            approved,
        } => {
            let idx = session.index()?;
            let p = idx
                .plugin(id)
                .ok_or_else(|| Error::new("unknown_plugin", id.clone()))?;
            let entry = match version {
                Some(v) => catalog::exact(p, v),
                None => catalog::latest(p, &session.manager),
            }
            .ok_or_else(|| Error::new("no_version", format!("no installable version of {id}")))?;
            let v = &entry.version;
            Ok(match session.install(entry, approved)? {
                install::Outcome::Installed => format!("installed {id} {v}"),
                install::Outcome::Updated { from } => format!("updated {id} {from} -> {v}"),
                install::Outcome::Reinstalled => format!("reinstalled {id} {v}"),
            })
        }
        Job::Remove { id, purge } => {
            install::remove(&session.root, id, *purge).map_err(session::hint_io)?;
            Ok(format!("removed {id}"))
        }
        Job::SetEnabled { id, on } => {
            install::set_enabled(&session.root, id, *on)?;
            Ok(format!("{} {id}", if *on { "enabled" } else { "disabled" }))
        }
        Job::Rollback { id } => {
            let v = install::rollback(&session.root, id).map_err(session::hint_io)?;
            Ok(format!("{id} is now {v}"))
        }
        Job::CoreCheck => {
            let info = session.core_refresh()?;
            Ok(format!("newest core: {}", info.version))
        }
        Job::CoreUpdate => {
            let game = coremod::resolve_game_dir(&session.root, None)?;
            let now = coremod::installed(&game)?;
            let info = session.core_refresh()?;
            if !coremod::update_available(&now, &info, &session.manager) {
                return match now {
                    CoreInstalled::Foreign => Err(Error::new(
                        "not_ours",
                        "version.dll belongs to another mod; it is left alone",
                    )),
                    _ => Ok("the core is up to date".to_owned()),
                };
            }
            let out = session.core_update(&game, &info).map_err(session::hint_io)?;
            coremod::save_game_dir(&session.root, &game)?;
            Ok(format!("core is now {}", core_version(&out.to)))
        }
        Job::CoreRollback => {
            let game = coremod::resolve_game_dir(&session.root, None)?;
            let out = coremod::rollback(&session.root, &game)?;
            Ok(format!("core is now {}", core_version(&out.to)))
        }
        Job::CoreGameDir { dir } => {
            let game = coremod::resolve_game_dir(&session.root, Some(dir))?;
            coremod::save_game_dir(&session.root, &game)?;
            Ok(format!("game folder: {}", game.display()))
        }
        Job::Verify => {
            let findings = store::verify(&session.root)?;
            Ok(if findings.is_empty() {
                "ok: every installed plugin matches its pin".to_owned()
            } else {
                format!("{} problem(s) found", findings.len())
            })
        }
    }
}

fn core_version(v: &Option<Version>) -> String {
    v.as_ref().map_or_else(|| "installed".to_owned(), |v| v.to_string())
}

/// The core mod as the settings tab shows it, from disk and the cached `core.json`.
#[derive(Clone, Debug)]
pub struct CoreView {
    /// The game folder, or why none is known.
    pub game_dir: Result<PathBuf, String>,
    pub installed: Option<CoreInstalled>,
    pub published: Option<Version>,
    pub update: bool,
}

impl CoreView {
    pub fn load(root: &Root, manager: &Version) -> CoreView {
        let game_dir = coremod::resolve_game_dir(root, None).map_err(|e| e.detail);
        let installed = game_dir
            .as_ref()
            .ok()
            .and_then(|d| coremod::installed(d).ok());
        let info = coremod::load_cached(root);
        let update = match (&installed, &info) {
            (Some(i), Some(info)) => coremod::update_available(i, info, manager),
            _ => false,
        };
        CoreView {
            game_dir,
            installed,
            published: info.map(|i| i.version),
            update,
        }
    }
}

/// An installed plugin as a row.
#[derive(Clone, Debug)]
pub struct Installed {
    pub id: String,
    pub version: Version,
    /// From the installed `plugin.json`.
    pub author: Option<String>,
    pub enabled: bool,
    pub capabilities: Vec<String>,
    /// The newer version the cached index offers, if any.
    pub update: Option<Version>,
    pub problem: Option<String>,
}

/// Everything the window shows, loaded without the network.
#[derive(Clone, Debug, Default)]
pub struct View {
    pub installed: Vec<Installed>,
    pub index: Option<Index>,
    /// Why the installed list or the index is not available.
    pub notes: Vec<String>,
    pub findings: Vec<Finding>,
    pub core: Option<CoreView>,
}

impl View {
    pub fn load(root: &Root, session: Option<&Session>) -> View {
        let mut v = View::default();
        if let Some(s) = session {
            v.core = Some(CoreView::load(root, &s.manager));
        }
        let state = match root.load_state() {
            Ok(s) => Some(s),
            Err(e) => {
                v.notes.push(format!("installed plugins: {e}"));
                None
            }
        };
        if let Some(s) = session {
            match s.cached_index() {
                Ok(i) => v.index = Some(i),
                Err(e) => v.notes.push(format!("no usable cached index ({e})")),
            }
        }
        match store::verify(root) {
            Ok(f) => v.findings = f,
            Err(e) if state.is_some() => v.notes.push(format!("verify: {e}")),
            Err(_) => {}
        }
        let updates = match (session, &v.index) {
            (Some(s), Some(idx)) => s.updates(idx).unwrap_or_default(),
            _ => Vec::new(),
        };
        for p in state.iter().flat_map(|s| &s.plugins) {
            v.installed.push(Installed {
                id: p.id.clone(),
                version: p.version.clone(),
                author: root.installed_author(&p.id),
                enabled: p.enabled,
                capabilities: p.approved_capabilities.clone(),
                update: updates
                    .iter()
                    .find(|(id, _)| *id == p.id)
                    .map(|(_, e)| e.version.clone()),
                problem: v
                    .findings
                    .iter()
                    .filter(|f| f.id == p.id)
                    .map(|f| f.problem.clone())
                    .reduce(|a, b| format!("{a}; {b}")),
            });
        }
        v
    }
}

/// The root the settings name, or the default.
pub fn root_of(explicit: Option<&PathBuf>) -> Result<Root, Error> {
    session::resolve_root(explicit.map(|p| p.as_path()))
}

/// True while the game has `version.dll` loaded: Windows refuses a write-open of a file in use
/// (sharing violation, or mapped-image error), so updating the core would fail. Opens without
/// truncating and writes nothing.
pub fn game_running(game_dir: &std::path::Path) -> bool {
    let dll = game_dir.join("version.dll");
    match std::fs::OpenOptions::new().write(true).open(dll) {
        Ok(_) => false,
        Err(e) => matches!(e.raw_os_error(), Some(32 | 1224)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_or_missing_dll_is_not_running() {
        let dir = std::env::temp_dir().join(format!("w2rpm-gui-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!game_running(&dir));
        std::fs::write(dir.join("version.dll"), b"x").unwrap();
        assert!(!game_running(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
