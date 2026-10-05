//! The window's jobs against a signed test repository: browse, consent data, install, update,
//! toggle, roll back, verify, remove (the Phase 5 exit, minus pixels).

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use w2rpm_gui::jobs::{self, Job, View};
use w2rpm_lib::devrepo::{self, Plugin};
use w2rpm_lib::session::Session;
use w2rpm_lib::store::Root;

fn setup(name: &str) -> (PathBuf, Session) {
    let base = std::env::temp_dir().join(format!("w2rpm-gui-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let repo = base.join("repo");
    publish(&repo, 1, &["1.0.0"]);
    let session = Session::new(
        Root(base.join("root")),
        None,
        Some(repo.clone()),
        &[repo.join("test.pub")],
    )
    .unwrap();
    (repo, session)
}

fn publish(repo: &std::path::Path, serial: u64, versions: &[&str]) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let dlls: Vec<Vec<u8>> = versions
        .iter()
        .map(|v| format!("dll {v}").into_bytes())
        .collect();
    let plugins: Vec<Plugin> = versions
        .iter()
        .zip(&dlls)
        .map(|(v, dll)| Plugin {
            id: "fancy",
            version: v,
            capabilities: &["game.read", "chat.read"],
            dependencies: &[],
            dll,
        })
        .collect();
    devrepo::build(repo, now, serial, &plugins).unwrap();
}

#[test]
fn full_cycle() {
    let (repo, s) = setup("cycle");

    // Before any refresh: no cached index, nothing installed.
    let v = View::load(&s.root, Some(&s));
    assert!(v.index.is_none() && v.installed.is_empty());

    jobs::run(&s, &Job::Refresh).unwrap();
    let v = View::load(&s.root, Some(&s));
    let idx = v.index.as_ref().expect("cached after refresh");
    let entry = &idx.plugin("fancy").unwrap().versions[0];

    // The consent window's data: both capabilities are new.
    let pre = s.preflight(idx, entry).unwrap();
    assert_eq!(pre.pending, ["game.read", "chat.read"]);

    // Without approval the library refuses.
    let id = || "fancy".to_owned();
    let refused = jobs::run(
        &s,
        &Job::Install {
            id: id(),
            version: None,
            approved: vec![],
        },
    );
    assert_eq!(refused.unwrap_err().code, "approval_needed");

    jobs::run(
        &s,
        &Job::Install {
            id: id(),
            version: None,
            approved: pre.pending,
        },
    )
    .unwrap();
    let v = View::load(&s.root, Some(&s));
    assert_eq!(v.installed.len(), 1);
    assert!(v.installed[0].enabled && v.installed[0].problem.is_none());
    assert_eq!(v.installed[0].author.as_deref(), Some("test"));
    assert!(v.installed[0].update.is_none());

    // A newer version appears: the cached index is stale until refreshed, then shows the update.
    publish(&repo, 2, &["1.0.0", "1.1.0"]);
    jobs::run(&s, &Job::Refresh).unwrap();
    let v = View::load(&s.root, Some(&s));
    assert_eq!(v.installed[0].update.as_ref().unwrap().to_string(), "1.1.0");

    // Updating asks about nothing new, installs, and clears the indicator.
    let idx = v.index.as_ref().unwrap();
    let newest = idx
        .plugin("fancy")
        .unwrap()
        .versions
        .iter()
        .find(|e| e.version.to_string() == "1.1.0")
        .unwrap();
    assert!(s.preflight(idx, newest).unwrap().pending.is_empty());
    jobs::run(
        &s,
        &Job::Install {
            id: id(),
            version: None,
            approved: vec![],
        },
    )
    .unwrap();
    let v = View::load(&s.root, Some(&s));
    assert_eq!(v.installed[0].version.to_string(), "1.1.0");
    assert!(v.installed[0].update.is_none());

    jobs::run(
        &s,
        &Job::SetEnabled {
            id: id(),
            on: false,
        },
    )
    .unwrap();
    assert!(!View::load(&s.root, Some(&s)).installed[0].enabled);

    jobs::run(&s, &Job::Rollback { id: id() }).unwrap();
    assert_eq!(
        View::load(&s.root, Some(&s)).installed[0]
            .version
            .to_string(),
        "1.0.0"
    );

    // Tampering shows up as a problem on the row.
    let dll = s.root.plugins_dir().join("fancy").join("fancy.dll");
    fs::write(&dll, b"evil").unwrap();
    let v = View::load(&s.root, Some(&s));
    assert!(v.installed[0].problem.is_some() && !v.findings.is_empty());

    jobs::run(
        &s,
        &Job::Remove {
            id: id(),
            purge: true,
        },
    )
    .unwrap();
    assert!(View::load(&s.root, Some(&s)).installed.is_empty());
}

#[test]
fn view_without_a_session_still_lists_installed_plugins() {
    let (_repo, s) = setup("nosession");
    jobs::run(&s, &Job::Refresh).unwrap();
    jobs::run(
        &s,
        &Job::Install {
            id: "fancy".into(),
            version: None,
            approved: vec!["game.read".into(), "chat.read".into()],
        },
    )
    .unwrap();
    let v = View::load(&s.root, None);
    assert_eq!(v.installed.len(), 1);
    assert!(v.index.is_none());
}
