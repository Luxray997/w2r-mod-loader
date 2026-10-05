//! Choosing versions from a verified index: the latest installable one, an explicit one, and
//! which installed plugins have an update.

use crate::index::{Index, PluginEntry, VersionEntry};
use w2r_formats::semver::Version;
use w2r_formats::state::{PluginState, State};

/// The highest version that is not a pre-release and that this manager can install.
pub fn latest<'a>(p: &'a PluginEntry, manager: &Version) -> Option<&'a VersionEntry> {
    p.versions
        .iter()
        .filter(|v| v.version.pre.is_empty() && &v.min_manager <= manager)
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// An exact version, pre-releases included.
pub fn exact<'a>(p: &'a PluginEntry, version: &Version) -> Option<&'a VersionEntry> {
    p.versions.iter().find(|v| &v.version == version)
}

/// Installed plugins whose latest installable version is newer than the installed one.
pub fn updates<'a>(
    state: &'a State,
    index: &'a Index,
    manager: &Version,
) -> Vec<(&'a PluginState, &'a VersionEntry)> {
    state
        .plugins
        .iter()
        .filter_map(|s| {
            let v = latest(index.plugin(&s.id)?, manager)?;
            (v.version > s.version).then_some((s, v))
        })
        .collect()
}

/// Case-insensitive match on id, name or description.
pub fn search<'a>(index: &'a Index, term: &str) -> Vec<(&'a PluginEntry, &'a VersionEntry)> {
    let term = term.to_ascii_lowercase();
    let mut out: Vec<_> = index
        .plugins
        .iter()
        .filter_map(|p| {
            // A plugin shows its newest entry even when the manager cannot install it yet.
            let v = p.versions.iter().max_by(|a, b| a.version.cmp(&b.version))?;
            let m = &v.manifest;
            let hit = term.is_empty()
                || p.id.contains(&term)
                || m.name.to_ascii_lowercase().contains(&term)
                || m.description.to_ascii_lowercase().contains(&term);
            hit.then_some((p, v))
        })
        .collect();
    out.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    out
}
