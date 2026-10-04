//! Startup loading: docs/load-policy.md checks 1 to 8 for every enabled plugin in
//! `state.json`, in the order the policy prescribes, then the registry's checks 9 and 10. A
//! failing plugin is logged as `<id>: <code>: <detail>` and skipped; it never stops the game or
//! the other plugins.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use w2r_abi::{W2R_ABI_VERSION, W2R_LOG_INFO, W2R_LOG_WARN};

use crate::dispatch::Registry;
use crate::host::{Game, Log, Services};
use crate::loader::sha256_of;
use crate::manifest::{self, Build, Manifest};
use crate::peer::peer_tag;
use crate::semver::Version;
use crate::state::{self, PluginState, State};

const HOST: &str = "host";

struct Candidate {
    st: PluginState,
    manifest: Manifest,
    dll: PathBuf,
}

type Refusal = (&'static str, String);

/// Reads at most `max + 1` bytes of `path`, so an oversized (or sparse multi-gigabyte) file is
/// refused by the parser's size rule instead of exhausting memory first. The parsers' limits are
/// on the byte count, so one extra byte is enough to trip them.
fn read_bounded(path: &Path, max: usize) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut raw = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut raw)?;
    Ok(raw)
}

/// Checks 1 to 6 for one enabled plugin.
fn candidate(root: &Path, st: &PluginState, running: Build) -> Result<Candidate, Refusal> {
    // `id` passed the manifest id rule in state.rs, so it is a plain folder name.
    let folder = root.join("plugins").join(&st.id);
    let raw = match read_bounded(&folder.join("plugin.json"), manifest::MAX_BYTES) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(("E_STATE", "plugin.json is missing".into()));
        }
        Err(e) => return Err(("E_STATE", format!("plugin.json is unreadable: {e}"))),
    };
    let manifest = manifest::validate(&raw).map_err(|code| ("E_MANIFEST", code.to_owned()))?;
    if manifest.id != st.id {
        return Err((
            "E_STATE",
            format!("manifest id {} differs from the folder name", manifest.id),
        ));
    }
    if !manifest.supports_build(running) {
        return Err((
            "E_BUILD",
            format!("game build {running} is not in game_builds"),
        ));
    }
    if manifest.abi > W2R_ABI_VERSION {
        return Err((
            "E_ABI",
            format!("needs ABI {}, host has {W2R_ABI_VERSION}", manifest.abi),
        ));
    }
    // The hash is checked again through a locked handle at load time; this early check only
    // keeps the refusal order the policy prescribes.
    let dll = folder.join(&manifest.entry);
    let hash =
        sha256_of(&dll).map_err(|e| ("E_HASH", format!("cannot hash {}: {e}", manifest.entry)))?;
    if hash != st.sha256 {
        return Err((
            "E_HASH",
            format!("sha256 of {} differs from the pin", manifest.entry),
        ));
    }
    if let Some(cap) = manifest
        .capabilities
        .iter()
        .find(|c| !st.approved_capabilities.contains(c))
    {
        return Err(("E_CAPS", format!("capability {cap} is not approved")));
    }
    Ok(Candidate {
        st: st.clone(),
        manifest,
        dll,
    })
}

/// Dependency order among `accepted`: dependencies first, ties by id. Members of a cycle come
/// back in the second list.
fn load_order(accepted: Vec<Candidate>) -> (Vec<Candidate>, Vec<Candidate>) {
    let ids: BTreeSet<String> = accepted.iter().map(|c| c.st.id.clone()).collect();
    let mut by_id: BTreeMap<String, Candidate> =
        accepted.into_iter().map(|c| (c.st.id.clone(), c)).collect();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, c) in &by_id {
        let n = c
            .manifest
            .dependencies
            .iter()
            .filter(|d| ids.contains(&d.id))
            .inspect(|d| dependents.entry(d.id.clone()).or_default().push(id.clone()))
            .count();
        indegree.insert(id.clone(), n);
    }
    let mut ready: BTreeSet<String> = indegree
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut order = Vec::new();
    while let Some(id) = ready.pop_first() {
        for dep in dependents.get(&id).into_iter().flatten() {
            let n = indegree.get_mut(dep).expect("known id");
            *n -= 1;
            if *n == 0 {
                ready.insert(dep.clone());
            }
        }
        order.push(by_id.remove(&id).expect("each id once"));
    }
    let cycle = by_id.into_values().collect();
    (order, cycle)
}

/// Loads everything `state.json` enables. Returns the registry, with a slot per plugin that
/// reached check 9.
pub fn load_all(
    root: &Path,
    running: Build,
    log: Rc<dyn Log>,
    game: Option<Game>,
    services: Services,
) -> (Registry, State) {
    let mut registry = Registry::with(log.clone(), game, services);
    let state_path = root.join("config").join("state.json");
    let state = match read_bounded(&state_path, state::MAX_BYTES) {
        Ok(raw) => match state::parse(&raw) {
            Ok(s) => s,
            Err(code) => {
                log.line(
                    W2R_LOG_WARN,
                    HOST,
                    &format!("state.json refused ({code}); loading nothing"),
                );
                return (registry, State::default());
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            log.line(
                W2R_LOG_INFO,
                HOST,
                "no config\\state.json: no plugins to load",
            );
            return (registry, State::default());
        }
        Err(e) => {
            log.line(
                W2R_LOG_WARN,
                HOST,
                &format!("state.json unreadable ({e}); loading nothing"),
            );
            return (registry, State::default());
        }
    };

    // Checks 1-6, then 7 (tags) by id order among the survivors.
    let mut accepted = Vec::new();
    for st in state.plugins.iter().filter(|p| p.enabled) {
        match candidate(root, st, running) {
            Ok(c) => accepted.push(c),
            Err((code, detail)) => log.line(W2R_LOG_WARN, &st.id, &format!("{code}: {detail}")),
        }
    }
    accepted.sort_by(|a, b| a.st.id.cmp(&b.st.id));
    let mut tags: BTreeMap<[u8; 4], String> = BTreeMap::new();
    accepted.retain(|c| match tags.get(&peer_tag(&c.st.id)) {
        Some(earlier) => {
            log.line(
                W2R_LOG_WARN,
                &c.st.id,
                &format!("E_TAG: same peer tag as {earlier}"),
            );
            false
        }
        None => {
            tags.insert(peer_tag(&c.st.id), c.st.id.clone());
            true
        }
    });

    // Checks 8-10 at each plugin's turn, in dependency order.
    let (order, cycle) = load_order(accepted);
    let mut loaded: BTreeMap<String, Version> = BTreeMap::new();
    for c in order {
        let unmet = c
            .manifest
            .dependencies
            .iter()
            .find_map(|d| match loaded.get(&d.id) {
                Some(v) if d.version_req.matches(v) => None,
                Some(v) => Some(format!(
                    "dependency {} {v:?} does not satisfy its version_req",
                    d.id
                )),
                None => Some(format!("dependency {} is not loaded", d.id)),
            });
        if let Some(detail) = unmet {
            log.line(W2R_LOG_WARN, &c.st.id, &format!("E_DEP: {detail}"));
            continue;
        }
        // Grant what the manifest declares (a subset of the approvals after check 6), not the
        // approval list: a stale approval from an older version must not widen the plugin.
        let caps: Vec<&str> = c.manifest.capabilities.iter().map(String::as_str).collect();
        if registry
            .load_dll(&c.st.id, &c.dll, &c.st.sha256, &caps)
            .is_ok()
        {
            loaded.insert(c.st.id.clone(), c.manifest.version.clone());
        }
    }
    for c in cycle {
        log.line(
            W2R_LOG_WARN,
            &c.st.id,
            "E_DEP: part of a dependency cycle, or depends on one",
        );
    }
    log.line(
        W2R_LOG_INFO,
        HOST,
        &format!(
            "{} of {} enabled plugins loaded",
            loaded.len(),
            state.plugins.iter().filter(|p| p.enabled).count()
        ),
    );
    (registry, state)
}
