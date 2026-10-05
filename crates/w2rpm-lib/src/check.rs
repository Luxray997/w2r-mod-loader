//! Install-time compatibility warnings. The host enforces these at load (`E_ABI`, `E_DEP`,
//! `E_BUILD`; docs/load-policy.md); the manager says so earlier, while the user can still act.
//! Warnings never block an install: the host has the last word, and `update` may install a
//! plugin before its dependency.

use crate::catalog;
use crate::index::{Index, VersionEntry};
use w2r_formats::semver::Version;
use w2r_formats::state::State;

/// What would stop `entry` from loading, given the plugins already installed.
pub fn warnings(
    state: &State,
    index: Option<&Index>,
    entry: &VersionEntry,
    manager: &Version,
) -> Vec<String> {
    let m = &entry.manifest;
    let mut out = Vec::new();
    if m.abi > w2r_abi::W2R_ABI_VERSION {
        out.push(format!(
            "needs plugin ABI {}, this manager's host speaks {}; update w2rpm and the host",
            m.abi,
            w2r_abi::W2R_ABI_VERSION
        ));
    }
    for d in &m.dependencies {
        match state.get(&d.id) {
            None => {
                let offered = index
                    .and_then(|i| i.plugin(&d.id))
                    .and_then(|p| {
                        p.versions
                            .iter()
                            .filter(|v| d.version_req.matches(&v.version))
                            .max_by(|a, b| a.version.cmp(&b.version))
                    })
                    .is_some();
                let hint = if offered {
                    format!("; install it with: w2rpm install {}", d.id)
                } else {
                    String::new()
                };
                out.push(format!("depends on {} which is not installed{hint}", d.id));
            }
            Some(p) if !d.version_req.matches(&p.version) => {
                let upd = index
                    .and_then(|i| i.plugin(&d.id))
                    .and_then(|p| catalog::latest(p, manager))
                    .is_some_and(|v| d.version_req.matches(&v.version));
                let hint = if upd { "; an update is available" } else { "" };
                out.push(format!(
                    "depends on {} {}, installed is {}{hint}",
                    d.id, d.version_req, p.version
                ));
            }
            Some(p) if !p.enabled => out.push(format!("depends on {}, which is disabled", d.id)),
            Some(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::package;
    use w2r_formats::manifest::Dependency;
    use w2r_formats::semver::VersionReq;
    use w2r_formats::state::PluginState;

    fn dep(id: &str, req: &str) -> Dependency {
        Dependency {
            id: id.into(),
            version_req: VersionReq::parse(req).unwrap(),
        }
    }

    fn installed(id: &str, version: &str, enabled: bool) -> PluginState {
        PluginState {
            id: id.into(),
            version: Version::parse(version).unwrap(),
            enabled,
            sha256: [0; 32],
            approved_capabilities: vec![],
        }
    }

    fn mgr() -> Version {
        Version::parse("0.1.0").unwrap()
    }

    #[test]
    fn quiet_when_nothing_is_wrong() {
        let (e, _) = package("1.0.0", &[], b"dll");
        assert!(warnings(&State::default(), None, &e, &mgr()).is_empty());
    }

    #[test]
    fn abi_newer_than_the_host() {
        let (mut e, _) = package("1.0.0", &[], b"dll");
        e.manifest.abi = w2r_abi::W2R_ABI_VERSION + 1;
        let w = warnings(&State::default(), None, &e, &mgr());
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("plugin ABI"), "{w:?}");
    }

    #[test]
    fn dependencies_missing_unsatisfied_or_disabled() {
        let (mut e, _) = package("1.0.0", &[], b"dll");
        e.manifest.dependencies = vec![
            dep("lib-a", ">=1.0.0"),
            dep("lib-b", ">=2.0.0"),
            dep("lib-c", ">=1.0.0"),
        ];
        let state = State {
            plugins: vec![
                installed("lib-b", "1.5.0", true),
                installed("lib-c", "1.0.0", false),
            ],
        };
        let w = warnings(&state, None, &e, &mgr());
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(
            w[0].contains("lib-a") && w[0].contains("not installed"),
            "{w:?}"
        );
        assert!(
            w[1].contains("lib-b") && w[1].contains(">=2.0.0") && w[1].contains("1.5.0"),
            "{w:?}"
        );
        assert!(w[2].contains("lib-c") && w[2].contains("disabled"), "{w:?}");
    }

    #[test]
    fn satisfied_dependency_is_silent() {
        let (mut e, _) = package("1.0.0", &[], b"dll");
        e.manifest.dependencies = vec![dep("lib-a", ">=1.0.0,<2.0.0")];
        let state = State {
            plugins: vec![installed("lib-a", "1.9.9", true)],
        };
        assert!(warnings(&state, None, &e, &mgr()).is_empty());
    }
}
