//! The egui window. It owns no logic: buttons turn into `Job`s that run on a worker thread
//! against `w2rpm-lib`, and the screen shows the `View` loaded afterwards. Only one job runs at a
//! time. The consent window is the only way to install: it lists every capability the plugin asks
//! for, marks the ones not approved yet, and makes `hooks.raw` need a separate tick.

use crate::jobs::{self, Installed, Job, View};
use crate::settings::{self, Settings};
use eframe::egui::{self, Color32, RichText};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use w2rpm_lib::catalog;
use w2rpm_lib::coremod::Installed as CoreInstalled;
use w2rpm_lib::index::VersionEntry;
use w2rpm_lib::session::{Preflight, Session, capability_note};
use w2rpm_lib::store::Root;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Installed,
    Browse,
    Settings,
}

struct Done {
    label: String,
    result: Result<String, String>,
    view: View,
}

/// An install waiting for the user's yes.
struct Consent {
    entry: VersionEntry,
    pre: Preflight,
    ack_raw: bool,
}

pub struct App {
    settings: Settings,
    /// Settings being edited; applied with the Save button.
    draft: Settings,
    session: Option<Arc<Session>>,
    root: Option<Root>,
    /// The sharing setting of the chosen folder, saved the moment it is changed.
    share: Option<Result<w2rpm_lib::share::Settings, String>>,
    view: View,
    tab: Tab,
    search: String,
    consent: Option<Consent>,
    confirm_remove: Option<(String, bool)>,
    busy: Option<String>,
    status: Option<Result<String, String>>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
}

impl App {
    pub fn new() -> App {
        let settings = Settings::load();
        let (tx, rx) = channel();
        let mut app = App {
            draft: settings.clone(),
            settings,
            session: None,
            root: None,
            share: None,
            view: View::default(),
            tab: Tab::Installed,
            search: String::new(),
            consent: None,
            confirm_remove: None,
            busy: None,
            status: None,
            tx,
            rx,
        };
        app.apply_settings();
        if app.session.is_none() {
            app.tab = Tab::Settings;
        }
        app
    }

    /// Rebuilds the session from the saved settings and reloads the view.
    fn apply_settings(&mut self) {
        self.draft = self.settings.clone();
        match jobs::root_of(self.settings.root.as_ref()) {
            Ok(root) => {
                let url = (!self.settings.index_url.trim().is_empty())
                    .then(|| self.settings.index_url.trim().to_owned());
                self.session = Session::new(root.clone(), url, None)
                    .map(Arc::new)
                    .map_err(|e| self.status = Some(Err(format!("not connected to an index: {e}"))))
                    .ok();
                self.view = View::load(&root, self.session.as_deref());
                self.share = Some(
                    w2rpm_lib::share::Settings::load(&root.0).map_err(|e| e.to_string()),
                );
                self.root = Some(root);
            }
            Err(e) => {
                self.status = Some(Err(e.to_string()));
                self.session = None;
                self.root = None;
                self.share = None;
                self.view = View::default();
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, label: &str, job: Job) {
        let (Some(session), Some(_)) = (self.session.clone(), &self.root) else {
            self.status = Some(Err("set an index URL and a key in Settings first".into()));
            return;
        };
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(label.to_owned());
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let label = label.to_owned();
        std::thread::spawn(move || {
            let result = jobs::run(&session, &job).map_err(|e| e.to_string());
            let view = View::load(&session.root, Some(&session));
            let _ = tx.send(Done {
                label,
                result,
                view,
            });
            ctx.request_repaint();
        });
    }

    /// Starts the consent window for `entry`; installing needs the user's yes there.
    fn ask(&mut self, entry: &VersionEntry) {
        let Some(session) = &self.session else { return };
        let Some(idx) = &self.view.index else { return };
        match session.preflight(idx, entry) {
            Ok(pre) => {
                self.consent = Some(Consent {
                    entry: entry.clone(),
                    pre,
                    ack_raw: false,
                })
            }
            Err(e) => self.status = Some(Err(e.to_string())),
        }
    }

    fn installed_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if ui.button("Verify files").clicked() {
                self.start(ctx, "Verifying", Job::Verify);
            }
            ui.label("Plugins are native code and run inside the game with your permissions.");
        });
        if self.view.installed.is_empty() {
            ui.add_space(8.0);
            ui.label("No plugins installed. Use the Browse tab.");
        }
        let rows: Vec<Installed> = self.view.installed.clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for p in &rows {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong(&p.id);
                    ui.label(p.version.to_string());
                    ui.label(format!("by {}", p.author.as_deref().unwrap_or("unknown")));
                    let mut on = p.enabled;
                    if ui.checkbox(&mut on, "enabled").changed() {
                        let job = Job::SetEnabled {
                            id: p.id.clone(),
                            on,
                        };
                        self.start(ctx, "Saving", job);
                    }
                    if let Some(v) = &p.update {
                        let entry = self.entry_of(&p.id, v);
                        if ui
                            .button(RichText::new(format!("Update to {v}")).color(Color32::GREEN))
                            .clicked()
                            && let Some(e) = entry
                        {
                            self.ask(&e);
                        }
                    }
                    if ui.button("Roll back").clicked() {
                        self.start(ctx, "Rolling back", Job::Rollback { id: p.id.clone() });
                    }
                    if ui.button("Remove").clicked() {
                        self.confirm_remove = Some((p.id.clone(), false));
                    }
                });
                let caps = if p.capabilities.is_empty() {
                    "none".to_owned()
                } else {
                    p.capabilities.join(", ")
                };
                ui.label(format!("approved capabilities: {caps}"));
                if let Some(problem) = &p.problem {
                    ui.colored_label(Color32::LIGHT_RED, format!("problem: {problem}"));
                }
            }
        });
    }

    fn entry_of(&self, id: &str, v: &w2r_formats::semver::Version) -> Option<VersionEntry> {
        let idx = self.view.index.as_ref()?;
        catalog::exact(idx.plugin(id)?, v).cloned()
    }

    fn browse_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.text_edit_singleline(&mut self.search);
            if ui.button("Refresh index").clicked() {
                self.start(ctx, "Fetching the index", Job::Refresh);
            }
        });
        let Some(idx) = self.view.index.clone() else {
            ui.label("No index loaded. Press Refresh index (needs an index URL).");
            return;
        };
        let hits = catalog::search(&idx, &self.search);
        let mut install: Option<VersionEntry> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (p, newest) in hits {
                let have = self.view.installed.iter().find(|i| i.id == p.id);
                let can = catalog::latest(p, &self.manager_version());
                ui.separator();
                let m = &newest.manifest;
                ui.horizontal(|ui| {
                    ui.strong(&m.name);
                    ui.label(format!("{} by {}", newest.version, m.author));
                    if let Some(h) = have {
                        ui.label(format!("(installed {})", h.version));
                    } else if let Some(entry) = can {
                        if ui.button("Install").clicked() {
                            install = Some(entry.clone());
                        }
                    } else {
                        ui.label("needs a newer w2rpm");
                    }
                });
                ui.label(&m.description);
                if m.affects_simulation {
                    ui.colored_label(
                        Color32::YELLOW,
                        "Changes the simulation: every player must have it in multiplayer.",
                    );
                }
            }
        });
        if let Some(e) = install {
            self.ask(&e);
        }
    }

    fn manager_version(&self) -> w2r_formats::semver::Version {
        self.session.as_ref().map_or_else(
            || w2r_formats::semver::Version::parse("0.0.0").expect("valid"),
            |s| s.manager.clone(),
        )
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("Settings");
        ui.horizontal(|ui| {
            let shown = match jobs::root_of(self.draft.root.as_ref()) {
                Ok(r) => r.0.display().to_string(),
                Err(e) => e.to_string(),
            };
            ui.label(format!("w2r folder: {shown}"));
            if ui.button("Choose game folder…").clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .set_title("Choose the Warcraft2Remastered (or w2r) folder")
                    .pick_folder()
            {
                self.draft.root = Some(settings::root_from_pick(&p));
            }
            if ui.button("Use default").clicked() {
                self.draft.root = None;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Index URL:");
            ui.text_edit_singleline(&mut self.draft.index_url);
        });
        if ui.button("Save").clicked() {
            self.draft.index_url = self.draft.index_url.trim().to_owned();
            self.status = None;
            match self.draft.save() {
                Ok(()) => {}
                Err(e) => self.status = Some(Err(format!("could not save settings: {e}"))),
            }
            self.settings = self.draft.clone();
            let keep = self.status.take();
            self.apply_settings();
            if self.status.is_none() {
                self.status = keep;
            }
        }
        ui.add_space(6.0);
        ui.label(
            "Leave the index URL empty to use the built-in one (once the plugin repository is \
             published).",
        );
        self.core_section(ui, ctx);
        self.sharing_section(ui);
    }

    /// The core mod (`version.dll` in the game folder): installed version, newest published,
    /// update and rollback. Updating needs the game closed.
    fn core_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(12.0);
        ui.separator();
        ui.heading("Core mod");
        let Some(core) = self.view.core.clone() else {
            ui.label("Connect to an index first.");
            return;
        };
        match &core.game_dir {
            Ok(d) => ui.label(format!("Game folder: {}", d.display())),
            Err(e) => ui.colored_label(egui::Color32::RED, e),
        };
        let installed = match &core.installed {
            Some(CoreInstalled::Missing) => "not installed".to_owned(),
            Some(CoreInstalled::Foreign) => "another mod's version.dll (left alone)".to_owned(),
            Some(CoreInstalled::Ours(Some(v))) => v.to_string(),
            Some(CoreInstalled::Ours(None)) => "an older build (version unknown)".to_owned(),
            None => "unknown".to_owned(),
        };
        let published = core
            .published
            .as_ref()
            .map_or_else(|| "unknown (press Check)".to_owned(), |v| v.to_string());
        ui.label(format!("Installed: {installed}    Published: {published}"));
        if core.update {
            ui.colored_label(egui::Color32::from_rgb(0, 140, 60), "A core update is available.");
        }
        ui.horizontal(|ui| {
            if ui.button("Check").clicked() {
                self.start(ctx, "Checking the core", Job::CoreCheck);
            }
            let label = if matches!(core.installed, Some(CoreInstalled::Missing)) {
                "Install core"
            } else {
                "Update core"
            };
            if ui.button(label).clicked() {
                self.start(ctx, "Updating the core", Job::CoreUpdate);
            }
            if ui.button("Roll back core").clicked() {
                self.start(ctx, "Rolling back the core", Job::CoreRollback);
            }
            if ui.button("Choose game folder…").clicked()
                && let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose the Warcraft II Remastered folder (the one with x86)")
                    .pick_folder()
            {
                self.start(ctx, "Setting the game folder", Job::CoreGameDir { dir });
            }
        });
        ui.label("Close the game before updating: it keeps the file in use while it runs.");
    }

    /// The match-data sharing switch (the same setting as the checkbox on the in-game Plugins
    /// screen, `config\sharing.json`). It is saved as soon as it is changed.
    fn sharing_section(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        ui.separator();
        ui.heading("Stats sharing");
        let (Some(root), Some(share)) = (&self.root, &mut self.share) else {
            ui.label("Choose a w2r folder first.");
            return;
        };
        match share {
            Err(e) => {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("config/sharing.json is unusable: {e}"),
                );
            }
            Ok(settings) => {
                let mut on = settings.share_match_data;
                let label = "Share my match results and replays (including in-game chat) \
                             with the stats service";
                if ui.checkbox(&mut on, label).changed() {
                    settings.share_match_data = on;
                    if let Err(e) = settings.save(&root.0) {
                        self.status =
                            Some(Err(format!("could not save the sharing setting: {e}")));
                    }
                }
                ui.label(
                    "Only games against other players are sent: the map and its settings, each \
                     player's BattleTag, teams, race, the result, and the replay file. The game \
                     sends them by itself after a match when this is on.",
                );
                match settings.endpoint() {
                    Ok(url) => {
                        ui.label(format!("Service: {}", url.trim_end_matches("/v1/reports")));
                    }
                    Err(_) => {
                        ui.label("Service: not set");
                    }
                }
            }
        }
    }

    fn consent_window(&mut self, ctx: &egui::Context) {
        let Some(c) = &mut self.consent else { return };
        let mut decision = None;
        let m = c.entry.manifest.clone();
        egui::Window::new(format!("Install {} {}?", m.name, c.entry.version))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("by {}", m.author));
                ui.label(&m.description);
                ui.label("Plugins are native code and run inside the game with your permissions.");
                if m.affects_simulation {
                    ui.colored_label(
                        Color32::YELLOW,
                        "NOTE: changes the simulation; players need it to match in multiplayer.",
                    );
                }
                let builds: Vec<String> = m.game_builds.iter().map(|b| b.to_string()).collect();
                ui.label(format!("Built for game builds: {}", builds.join(", ")));
                for w in &c.pre.warnings {
                    ui.colored_label(Color32::LIGHT_RED, format!("WARNING: {w}"));
                }
                ui.separator();
                if m.capabilities.is_empty() {
                    ui.label("Asks for no capabilities.");
                }
                for cap in &m.capabilities {
                    let new = c.pre.pending.contains(cap);
                    let tag = if new { "new" } else { "approved" };
                    let text = format!("[{tag}] {cap}: {}", capability_note(cap));
                    if cap == "hooks.raw" {
                        ui.colored_label(Color32::LIGHT_RED, text);
                    } else {
                        ui.label(text);
                    }
                }
                let raw = c.pre.pending.iter().any(|x| x == "hooks.raw");
                if raw {
                    ui.checkbox(
                        &mut c.ack_raw,
                        "I understand hooks.raw can crash or compromise the game",
                    );
                }
                ui.horizontal(|ui| {
                    let ok = !raw || c.ack_raw;
                    let verb = if c.pre.pending.is_empty() {
                        "Install"
                    } else {
                        "Approve and install"
                    };
                    if ui.add_enabled(ok, egui::Button::new(verb)).clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                let c = self.consent.take().expect("consent is open");
                let job = Job::Install {
                    id: c.entry.manifest.id.clone(),
                    version: Some(c.entry.version.clone()),
                    approved: c.pre.pending,
                };
                self.start(ctx, "Installing", job);
            }
            Some(false) => self.consent = None,
            None => {}
        }
    }

    fn remove_window(&mut self, ctx: &egui::Context) {
        let Some((id, purge)) = &mut self.confirm_remove else {
            return;
        };
        let mut decision = None;
        egui::Window::new(format!("Remove {id}?"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.checkbox(purge, "Also delete the plugin's data folder");
                ui.horizontal(|ui| {
                    if ui.button("Remove").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                let (id, purge) = self.confirm_remove.take().expect("dialog is open");
                self.start(ctx, "Removing", Job::Remove { id, purge });
            }
            Some(false) => self.confirm_remove = None,
            None => {}
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(done) = self.rx.try_recv() {
            self.busy = None;
            self.view = done.view;
            self.status = Some(done.result.map_err(|e| format!("{}: {e}", done.label)));
        }
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Installed, "Installed");
                ui.selectable_value(&mut self.tab, Tab::Browse, "Browse");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
            });
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            if let Some(b) = &self.busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("{b}…"));
                });
            }
            match &self.status {
                Some(Ok(m)) => {
                    ui.label(m);
                }
                Some(Err(m)) => {
                    ui.colored_label(Color32::LIGHT_RED, m);
                }
                None => {}
            }
            for n in &self.view.notes {
                ui.label(RichText::new(n).weak());
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_enabled_ui(
                self.consent.is_none() && self.confirm_remove.is_none(),
                |ui| match self.tab {
                    Tab::Installed => self.installed_tab(ui, ctx),
                    Tab::Browse => self.browse_tab(ui, ctx),
                    Tab::Settings => self.settings_tab(ui, ctx),
                },
            );
        });
        self.consent_window(ctx);
        self.remove_window(ctx);
    }
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}
