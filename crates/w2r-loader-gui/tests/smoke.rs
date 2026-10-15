//! Draws every screen headless against a signed test repository, so a panic, a layout assert or
//! a broken id clash shows up in `cargo test` (not a pixel check).

use eframe::egui;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use w2r_loader_gui::app::App;
use w2r_loader_gui::jobs::{self, Job};
use w2r_loader_lib::devrepo::{self, Plugin};
use w2r_loader_lib::session::Session;
use w2r_loader_lib::store::Root;

fn frame(ctx: &egui::Context, app: &mut App) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(760.0, 520.0),
        )),
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| app.ui(ctx));
}

#[test]
fn every_screen_draws() {
    let base = std::env::temp_dir().join(format!("w2r-loader-gui-smoke-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let repo = base.join("repo");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let dll = b"dll".to_vec();
    let plugin = |v: &'static str| Plugin {
        id: "fancy",
        version: v,
        capabilities: &["game.read", "hooks.raw"],
        dependencies: &[],
        dll: &dll,
    };
    devrepo::build(&repo, now, 1, &[plugin("1.0.0")]).unwrap();
    let session = Session::new(Root(base.join("root")), None, Some(repo.clone())).unwrap();
    jobs::run(&session, &Job::Refresh).unwrap();
    jobs::run(
        &session,
        &Job::Install {
            id: "fancy".into(),
            version: None,
            approved: vec!["game.read".into(), "hooks.raw".into()],
        },
    )
    .unwrap();
    let extra = Plugin {
        id: "extra",
        version: "1.0.0",
        capabilities: &[],
        dependencies: &[],
        dll: &dll,
    };
    let needs = Plugin {
        id: "needs-extra",
        version: "1.0.0",
        capabilities: &[],
        dependencies: &[("extra", ">=1.0.0"), ("fancy", ">=1.0.0")],
        dll: &dll,
    };
    devrepo::build(&repo, now, 2, &[plugin("1.0.0"), plugin("1.1.0"), extra, needs]).unwrap();
    jobs::run(&session, &Job::Refresh).unwrap();

    let mut app = App::with_session(session);
    let ctx = egui::Context::default();
    w2r_loader_gui::theme::apply(&ctx);
    for tab in 0..3 {
        app.select_tab(tab);
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }
    }
    assert!(app.preview_consent("fancy"));
    for _ in 0..3 {
        frame(&ctx, &mut app);
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_missing_dependency_joins_the_install() {
    let base = std::env::temp_dir().join(format!("w2r-loader-gui-deps-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let dll = b"dll".to_vec();
    let extra = Plugin {
        id: "extra",
        version: "1.0.0",
        capabilities: &[],
        dependencies: &[],
        dll: &dll,
    };
    let needs = Plugin {
        id: "needs-extra",
        version: "1.0.0",
        capabilities: &[],
        dependencies: &[("extra", ">=1.0.0")],
        dll: &dll,
    };
    devrepo::build(&repo, now, 1, &[extra, needs]).unwrap();
    let session = Session::new(Root(base.join("root")), None, Some(repo)).unwrap();
    jobs::run(&session, &Job::Refresh).unwrap();
    let mut app = App::with_session(session);
    assert!(app.preview_consent("needs-extra"));
    assert_eq!(app.consent_len(), 2, "the dependency is listed with the plugin");
    let _ = fs::remove_dir_all(&base);
}
