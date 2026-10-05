//! What the window can ask for, run off the UI thread, and the snapshot it shows afterwards.
//! No UI code here, so tests drive it against a signed test repository. All real work is in
//! `w2rpm-lib`; this file only sequences calls (docs/manager.md).

use std::path::PathBuf;
use w2r_formats::semver::Version;
use w2rpm_lib::index::Index;
use w2rpm_lib::session::{self, Session};
use w2rpm_lib::store::{self, Finding, Root};
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

/// An installed plugin as a row.
#[derive(Clone, Debug)]
pub struct Installed {
    pub id: String,
    pub version: Version,
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
    pub rotation_pending: bool,
    pub findings: Vec<Finding>,
}

impl View {
    pub fn load(root: &Root, session: Option<&Session>) -> View {
        let mut v = View::default();
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
        if let (Some(s), Some(idx)) = (session, &v.index) {
            v.rotation_pending = s.rotation_pending(idx);
        }
        let updates = match (session, &v.index) {
            (Some(s), Some(idx)) => s.updates(idx).unwrap_or_default(),
            _ => Vec::new(),
        };
        for p in state.iter().flat_map(|s| &s.plugins) {
            v.installed.push(Installed {
                id: p.id.clone(),
                version: p.version.clone(),
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
