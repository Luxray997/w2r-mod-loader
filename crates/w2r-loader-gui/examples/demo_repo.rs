//! Writes a throwaway repository and w2r folder for looking at the window with data in it:
//!
//!   cargo run -p w2r-loader-gui --example demo_repo -- <dir>
//!
//! then run the GUI with `W2R_LOADER_GUI_CONFIG=<dir>/gui.json` and `W2R_LOADER_INDEX_DIR=<dir>/repo`.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use w2r_loader_gui::jobs::{self, Job};
use w2r_loader_lib::devrepo::{self, Plugin};
use w2r_loader_lib::session::Session;
use w2r_loader_lib::store::Root;

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: demo_repo <dir>"));
    let repo = dir.join("repo");
    let root = dir.join("root");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let dll = b"demo dll".to_vec();
    let p = |id, version, capabilities, dependencies| Plugin {
        id,
        version,
        capabilities,
        dependencies,
        dll: &dll,
    };
    let alpha = |v| p("w2r-alpha", v, &["game.read"][..], &[][..]);
    let beta_deps = [("w2r-alpha", ">=1.0.0")];
    let gamma_deps = [("w2r-alpha", ">=1.1.0"), ("w2r-beta", ">=1.0.0")];
    let one = |id, caps| p(id, "1.0.0", caps, &[][..]);

    let first = [alpha("1.0.0")];
    devrepo::build(&repo, now, 1, &first).unwrap();
    let session = Session::new(Root(root.clone()), None, Some(repo.clone())).unwrap();
    jobs::run(&session, &Job::Refresh).unwrap();
    jobs::run(
        &session,
        &Job::Install {
            id: "w2r-alpha".into(),
            version: None,
            approved: vec!["game.read".into()],
        },
    )
    .unwrap();

    let all = [
        alpha("1.0.0"),
        alpha("1.1.0"),
        p("w2r-beta", "1.0.0", &["game.read", "hooks.raw"][..], &beta_deps[..]),
        p("w2r-gamma", "1.0.0", &["chat.read"][..], &gamma_deps[..]),
        one("w2r-delta", &["game.read", "chat.write"][..]),
        one("w2r-epsilon", &[][..]),
    ];
    devrepo::build(&repo, now, 2, &all).unwrap();
    std::fs::write(
        dir.join("gui.json"),
        format!(r#"{{"root":{:?},"index_url":""}}"#, root.to_string_lossy()),
    )
    .unwrap();
    println!("repo: {}\nroot: {}", repo.display(), root.display());
}
