//! Draws every screen headless against a signed test repository, so a panic, a layout assert or
//! a broken id clash shows up in `cargo test` (not a pixel check).

use eframe::egui;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use w2rpm_gui::app::App;
use w2rpm_gui::jobs::{self, Job};
use w2rpm_lib::devrepo::{self, Plugin};
use w2rpm_lib::session::Session;
use w2rpm_lib::store::Root;

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
    let base = std::env::temp_dir().join(format!("w2rpm-gui-smoke-{}", std::process::id()));
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
    devrepo::build(&repo, now, 2, &[plugin("1.0.0"), plugin("1.1.0")]).unwrap();
    jobs::run(&session, &Job::Refresh).unwrap();

    let mut app = App::with_session(session);
    let ctx = egui::Context::default();
    w2rpm_gui::theme::apply(&ctx);
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
