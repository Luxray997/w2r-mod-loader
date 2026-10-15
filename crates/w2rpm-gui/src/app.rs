//! The egui window. It owns no logic: buttons turn into `Job`s that run on a worker thread
//! against `w2rpm-lib`, and the screen shows the `View` loaded afterwards. Only one job runs at a
//! time. The consent window is the only way to install: it lists every capability the plugin asks
//! for, marks the ones not approved yet, and makes `hooks.raw` need a separate tick.
//!
//! Results of jobs and errors are put in `status`; once per frame `flush_status` turns that into a
//! toast and an entry in the activity list, so code anywhere can just set `status`.

use crate::jobs::{self, Installed, Job, View};
use crate::settings::{self, Settings};
use crate::theme;
use eframe::egui::{self, Key, Modifiers, RichText};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};
use w2rpm_lib::catalog;
use w2rpm_lib::coremod::Installed as CoreInstalled;
use w2rpm_lib::index::{PluginEntry, VersionEntry};
use w2rpm_lib::session::{Preflight, Session, capability_note};
use w2rpm_lib::store::Root;

/// Seconds a success or info toast stays; errors stay until dismissed.
const TOAST_SECS: f64 = 6.0;
/// Entries kept in the activity list.
const HISTORY_MAX: usize = 100;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Installed,
    Browse,
    Settings,
}

#[derive(PartialEq, Clone, Copy)]
enum Filter {
    All,
    NotInstalled,
    HasUpdate,
}

#[derive(PartialEq, Clone, Copy)]
enum Sort {
    Default,
    Name,
    Author,
}

#[derive(PartialEq, Clone, Copy)]
enum Kind {
    Ok,
    Info,
    Err,
}

struct Toast {
    kind: Kind,
    text: String,
    born: f64,
}

struct Done {
    label: String,
    result: Result<String, String>,
    view: View,
    /// The job was the check made at startup, whose failure is not worth an error.
    startup: bool,
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
    filter: Filter,
    sort: Sort,
    focus_search: bool,
    consent: Option<Consent>,
    /// Updates still to be offered after the current consent window (Update all).
    update_queue: Vec<VersionEntry>,
    confirm_remove: Option<(String, bool)>,
    busy: Option<String>,
    /// Set anywhere; `flush_status` moves it to a toast and the activity list.
    status: Option<Result<String, String>>,
    toasts: Vec<Toast>,
    history: Vec<(Kind, String)>,
    show_history: bool,
    /// Fetch the index once, on the first frame.
    startup_pending: bool,
    startup_job: bool,
    game_running: bool,
    last_probe: Option<Instant>,
    /// Last known `[x, y, width, height]`, saved on exit.
    window: Option<[i32; 4]>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
}

impl App {
    pub fn new() -> App {
        let settings = Settings::load();
        let mut app = App::blank(settings);
        app.startup_pending = true;
        app.apply_settings();
        if app.session.is_none() {
            app.tab = Tab::Settings;
            app.startup_pending = false;
        }
        app
    }

    /// An app on an existing session, for tests: no settings file, no startup fetch.
    #[doc(hidden)]
    pub fn with_session(session: Session) -> App {
        let mut app = App::blank(Settings::default());
        let root = session.root.clone();
        app.view = View::load(&root, Some(&session));
        app.root = Some(root);
        app.session = Some(Arc::new(session));
        app
    }

    fn blank(settings: Settings) -> App {
        let (tx, rx) = channel();
        App {
            draft: settings.clone(),
            window: settings.window,
            settings,
            session: None,
            root: None,
            share: None,
            view: View::default(),
            tab: Tab::Installed,
            search: String::new(),
            filter: Filter::All,
            sort: Sort::Default,
            focus_search: false,
            consent: None,
            update_queue: Vec::new(),
            confirm_remove: None,
            busy: None,
            status: None,
            toasts: Vec::new(),
            history: Vec::new(),
            show_history: false,
            startup_pending: false,
            startup_job: false,
            game_running: false,
            last_probe: None,
            tx,
            rx,
        }
    }

    /// Test hook: show tab 0 Installed, 1 Browse, 2 Settings.
    #[doc(hidden)]
    pub fn select_tab(&mut self, i: usize) {
        self.tab = [Tab::Installed, Tab::Browse, Tab::Settings][i];
    }

    /// Test hook: open the consent window for the newest version of `id`.
    #[doc(hidden)]
    pub fn preview_consent(&mut self, id: &str) -> bool {
        let entry = self
            .view
            .index
            .as_ref()
            .and_then(|i| i.plugin(id))
            .and_then(|p| p.versions.first().cloned());
        if let Some(e) = entry {
            self.ask(&e);
        }
        self.consent.is_some()
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
        let startup = std::mem::take(&mut self.startup_job);
        std::thread::spawn(move || {
            let result = jobs::run(&session, &job).map_err(|e| e.to_string());
            let view = View::load(&session.root, Some(&session));
            let _ = tx.send(Done {
                label,
                result,
                view,
                startup,
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

    fn push(&mut self, ctx: &egui::Context, kind: Kind, text: String) {
        let born = ctx.input(|i| i.time);
        self.history.insert(0, (kind, text.clone()));
        self.history.truncate(HISTORY_MAX);
        self.toasts.push(Toast { kind, text, born });
    }

    /// Turns `status` into a toast and an activity entry.
    fn flush_status(&mut self, ctx: &egui::Context) {
        match self.status.take() {
            Some(Ok(m)) => self.push(ctx, Kind::Ok, m),
            Some(Err(m)) => self.push(ctx, Kind::Err, m),
            None => {}
        }
    }

    fn installed_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if ui.button("Verify files").clicked() {
                self.start(ctx, "Verifying", Job::Verify);
            }
            let mut pending: Vec<VersionEntry> = self
                .view
                .installed
                .iter()
                .filter_map(|p| self.entry_of(&p.id, p.update.as_ref()?))
                .collect();
            if !pending.is_empty()
                && ui
                    .button(
                        RichText::new(format!("Update all ({})", pending.len()))
                            .color(theme::GOOD),
                    )
                    .clicked()
                && self.busy.is_none()
            {
                let first = pending.remove(0);
                self.update_queue = pending;
                self.ask(&first);
            }
            ui.label(theme::muted(
                "Plugins are native code and run inside the game with your permissions.",
            ));
        });
        if self.view.installed.is_empty() {
            ui.add_space(8.0);
            ui.label(theme::muted("No plugins installed. Use the Browse tab."));
        }
        let rows: Vec<Installed> = self.view.installed.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for p in &rows {
                theme::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        theme::avatar(ui, &p.id);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&p.id).strong().size(16.0));
                                ui.label(theme::muted(p.version.to_string()));
                                if p.update.is_some() {
                                    theme::badge(ui, "Update available", theme::GOOD);
                                }
                                if !p.enabled {
                                    theme::badge(ui, "Disabled", theme::MUTED);
                                }
                            });
                            ui.label(theme::muted(format!(
                                "by {}",
                                p.author.as_deref().unwrap_or("unknown")
                            )));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Remove").clicked() {
                                self.confirm_remove = Some((p.id.clone(), false));
                            }
                            if ui.button("Roll back").clicked() {
                                self.start(ctx, "Rolling back", Job::Rollback { id: p.id.clone() });
                            }
                            if let Some(v) = &p.update {
                                let entry = self.entry_of(&p.id, v);
                                if ui
                                    .button(
                                        RichText::new(format!("Update to {v}")).color(theme::GOOD),
                                    )
                                    .clicked()
                                    && let Some(e) = entry
                                {
                                    self.ask(&e);
                                }
                            }
                            let mut on = p.enabled;
                            if ui.checkbox(&mut on, "Enabled").changed() {
                                let job = Job::SetEnabled {
                                    id: p.id.clone(),
                                    on,
                                };
                                self.start(ctx, "Saving", job);
                            }
                        });
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(theme::muted("Capabilities:"));
                        if p.capabilities.is_empty() {
                            ui.label(theme::muted("none"));
                        }
                        for c in &p.capabilities {
                            let color = if c == "hooks.raw" { theme::BAD } else { theme::MUTED };
                            theme::pill(ui, c, color);
                        }
                    });
                    if let Some(problem) = &p.problem {
                        ui.colored_label(theme::BAD, format!("Problem: {problem}"));
                    }
                    let entry = self
                        .view
                        .index
                        .as_ref()
                        .and_then(|i| i.plugin(&p.id))
                        .and_then(|pe| catalog::exact(pe, &p.version).map(|v| (pe, v)));
                    if let Some((pe, v)) = entry {
                        egui::CollapsingHeader::new("Details")
                            .id_salt(("installed", &p.id))
                            .show(ui, |ui| details_ui(ui, pe, v));
                    }
                });
                ui.add_space(4.0);
            }
        });
    }

    fn entry_of(&self, id: &str, v: &w2r_formats::semver::Version) -> Option<VersionEntry> {
        let idx = self.view.index.as_ref()?;
        catalog::exact(idx.plugin(id)?, v).cloned()
    }

    fn browse_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search plugins (Ctrl+F)")
                    .desired_width(240.0),
            );
            if std::mem::take(&mut self.focus_search) {
                edit.request_focus();
            }
            if !self.search.is_empty() && ui.button("Clear").clicked() {
                self.search.clear();
            }
            egui::ComboBox::from_id_salt("filter")
                .selected_text(match self.filter {
                    Filter::All => "All",
                    Filter::NotInstalled => "Not installed",
                    Filter::HasUpdate => "Has update",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter, Filter::All, "All");
                    ui.selectable_value(&mut self.filter, Filter::NotInstalled, "Not installed");
                    ui.selectable_value(&mut self.filter, Filter::HasUpdate, "Has update");
                });
            egui::ComboBox::from_id_salt("sort")
                .selected_text(match self.sort {
                    Sort::Default => "Sort: default",
                    Sort::Name => "Sort: name",
                    Sort::Author => "Sort: author",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sort, Sort::Default, "Default");
                    ui.selectable_value(&mut self.sort, Sort::Name, "Name");
                    ui.selectable_value(&mut self.sort, Sort::Author, "Author");
                });
            if ui.button("Refresh index").clicked() {
                self.start(ctx, "Fetching the index", Job::Refresh);
            }
        });
        let Some(idx) = self.view.index.clone() else {
            ui.label(theme::muted(
                "No index loaded. Press Refresh index (needs an index URL).",
            ));
            return;
        };
        let mut hits = catalog::search(&idx, &self.search);
        hits.retain(|(p, _)| {
            let have = self.view.installed.iter().find(|i| i.id == p.id);
            match self.filter {
                Filter::All => true,
                Filter::NotInstalled => have.is_none(),
                Filter::HasUpdate => have.is_some_and(|h| h.update.is_some()),
            }
        });
        match self.sort {
            Sort::Default => {}
            Sort::Name => hits.sort_by_key(|(_, n)| n.manifest.name.to_lowercase()),
            Sort::Author => hits.sort_by_key(|(_, n)| {
                (n.manifest.author.to_lowercase(), n.manifest.name.to_lowercase())
            }),
        }
        if hits.is_empty() {
            ui.add_space(8.0);
            ui.label(theme::muted("No plugins match."));
        }
        let mut install: Option<VersionEntry> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (p, newest) in hits {
                let have = self.view.installed.iter().find(|i| i.id == p.id);
                let can = catalog::latest(p, &self.manager_version());
                let m = &newest.manifest;
                theme::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        theme::avatar(ui, &m.name);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&m.name).strong().size(16.0));
                                ui.label(theme::muted(newest.version.to_string()));
                                if m.affects_simulation {
                                    theme::badge(ui, "Changes the simulation", theme::WARN);
                                }
                            });
                            ui.label(theme::muted(format!("by {}", m.author)));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(h) = have {
                                theme::badge(ui, &format!("Installed {}", h.version), theme::GOOD);
                            } else if let Some(entry) = can {
                                if ui.button("Install").clicked() {
                                    install = Some(entry.clone());
                                }
                            } else {
                                theme::badge(ui, "Needs a newer w2rpm", theme::MUTED);
                            }
                        });
                    });
                    ui.label(&m.description);
                    if m.affects_simulation {
                        ui.label(theme::muted("Every player must have it in multiplayer."));
                    }
                    egui::CollapsingHeader::new("Details")
                        .id_salt(("browse", &p.id))
                        .show(ui, |ui| details_ui(ui, p, newest));
                });
                ui.add_space(4.0);
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
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            theme::section(ui, "General", |ui| {
                let shown = match jobs::root_of(self.draft.root.as_ref()) {
                    Ok(r) => r.0.display().to_string(),
                    Err(e) => e.to_string(),
                };
                ui.label(theme::muted("w2r folder"));
                ui.label(shown);
                ui.horizontal(|ui| {
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
                ui.add_space(4.0);
                ui.label(theme::muted("Index URL"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft.index_url)
                        .hint_text("built-in index")
                        .desired_width(f32::INFINITY),
                );
                ui.label(
                    theme::muted(
                        "Leave empty to use the built-in index (once the plugin repository is \
                         published).",
                    )
                    .small(),
                );
                if ui.button("Save").clicked() {
                    self.draft.index_url = self.draft.index_url.trim().to_owned();
                    self.draft.window = self.window;
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
            });
            self.core_section(ui, ctx);
            self.sharing_section(ui);
        });
    }

    /// The core mod (`version.dll` in the game folder): installed version, newest published,
    /// update and rollback. Updating needs the game closed.
    fn core_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        theme::section(ui, "Core mod", |ui| self.core_body(ui, ctx));
    }

    fn core_body(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(core) = self.view.core.clone() else {
            ui.label(theme::muted("Connect to an index first."));
            return;
        };
        match &core.game_dir {
            Ok(d) => ui.label(theme::muted(format!("Game folder: {}", d.display()))),
            Err(e) => ui.colored_label(theme::BAD, e),
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
        ui.horizontal(|ui| {
            ui.label(theme::muted("Installed"));
            ui.label(RichText::new(installed).strong());
            ui.add_space(8.0);
            ui.label(theme::muted("Published"));
            ui.label(RichText::new(published).strong());
            if core.update {
                theme::badge(ui, "Update available", theme::GOOD);
            }
            if self.game_running {
                theme::badge(ui, "Game is running", theme::WARN);
            }
        });
        let running = self.game_running;
        let why = "Close the game first: it keeps version.dll in use while it runs.";
        ui.horizontal(|ui| {
            if ui.button("Check").clicked() {
                self.start(ctx, "Checking the core", Job::CoreCheck);
            }
            let label = if matches!(core.installed, Some(CoreInstalled::Missing)) {
                "Install core"
            } else {
                "Update core"
            };
            if ui
                .add_enabled(!running, egui::Button::new(label))
                .on_disabled_hover_text(why)
                .clicked()
            {
                self.start(ctx, "Updating the core", Job::CoreUpdate);
            }
            if ui
                .add_enabled(!running, egui::Button::new("Roll back core"))
                .on_disabled_hover_text(why)
                .clicked()
            {
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
        ui.label(
            theme::muted("Close the game before updating: it keeps the file in use while it runs.")
                .small(),
        );
    }

    /// The match-data sharing switch (the same setting as the checkbox on the in-game Plugins
    /// screen, `config\sharing.json`). It is saved as soon as it is changed.
    fn sharing_section(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Stats sharing", |ui| self.sharing_body(ui));
    }

    fn sharing_body(&mut self, ui: &mut egui::Ui) {
        let (Some(root), Some(share)) = (&self.root, &mut self.share) else {
            ui.label(theme::muted("Choose a w2r folder first."));
            return;
        };
        match share {
            Err(e) => {
                ui.colored_label(
                    theme::BAD,
                    format!("config/sharing.json is unusable: {e}"),
                );
            }
            Ok(settings) => {
                let mut on = settings.share_match_data;
                let label = "Share my match results and replays with the stats service";
                if ui.checkbox(&mut on, label).changed() {
                    settings.share_match_data = on;
                    if let Err(e) = settings.save(&root.0) {
                        self.status =
                            Some(Err(format!("could not save the sharing setting: {e}")));
                    }
                }
                ui.label(
                    theme::muted(
                        "Only games against other players are sent: the map and its settings, \
                         each player's BattleTag, teams, race, the result, and the replay file. \
                         The game sends them by itself after a match when this is on.",
                    )
                    .small(),
                );
                match settings.endpoint() {
                    Ok(url) => {
                        ui.label(theme::muted(format!(
                            "Service: {}",
                            url.trim_end_matches("/v1/reports")
                        )));
                    }
                    Err(_) => {
                        ui.label(theme::muted("Service: not set"));
                    }
                }
            }
        }
    }

    fn consent_window(&mut self, ctx: &egui::Context) {
        let Some(c) = &mut self.consent else { return };
        let mut decision = None;
        let m = c.entry.manifest.clone();
        let raw = c.pre.pending.iter().any(|x| x == "hooks.raw");
        let can_install = !raw || c.ack_raw;
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            decision = Some(false);
        } else if can_install && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            decision = Some(true);
        }
        egui::Window::new(format!("Install {} {}?", m.name, c.entry.version))
            .collapsible(false)
            .resizable(false)
            .min_width(440.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(theme::muted(format!("by {}", m.author)));
                ui.label(&m.description);
                ui.label(
                    theme::muted(
                        "Plugins are native code and run inside the game with your permissions.",
                    )
                    .small(),
                );
                if m.affects_simulation {
                    theme::badge(ui, "Changes the simulation", theme::WARN);
                    ui.label(theme::muted(
                        "Players need it to match in multiplayer.",
                    ));
                }
                let builds: Vec<String> = m.game_builds.iter().map(|b| b.to_string()).collect();
                ui.label(theme::muted(format!(
                    "Built for game builds: {}",
                    builds.join(", ")
                )));
                for w in &c.pre.warnings {
                    ui.colored_label(theme::BAD, format!("Warning: {w}"));
                }
                ui.add_space(4.0);
                ui.label(RichText::new("Capabilities").strong());
                if m.capabilities.is_empty() {
                    ui.label(theme::muted("Asks for no capabilities."));
                }
                for cap in &m.capabilities {
                    let new = c.pre.pending.contains(cap);
                    let danger = cap == "hooks.raw";
                    ui.horizontal(|ui| {
                        if danger {
                            theme::badge(ui, "dangerous", theme::BAD);
                        } else if new {
                            theme::badge(ui, "new", theme::WARN);
                        } else {
                            theme::badge(ui, "approved", theme::GOOD);
                        }
                        theme::pill(ui, cap, if danger { theme::BAD } else { theme::MUTED });
                        let note = theme::muted(capability_note(cap));
                        ui.label(if danger { note.color(theme::BAD) } else { note });
                    });
                }
                if raw {
                    ui.checkbox(
                        &mut c.ack_raw,
                        "I understand hooks.raw can crash or compromise the game",
                    );
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let verb = if c.pre.pending.is_empty() {
                        "Install"
                    } else {
                        "Approve and install"
                    };
                    if ui.add_enabled(can_install, egui::Button::new(verb)).clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                    ui.label(theme::muted("Enter confirms, Esc cancels").small());
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
            Some(false) => {
                self.consent = None;
                self.update_queue.clear();
            }
            None => {}
        }
    }

    fn remove_window(&mut self, ctx: &egui::Context) {
        let Some((id, purge)) = &mut self.confirm_remove else {
            return;
        };
        let mut decision = None;
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            decision = Some(false);
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            decision = Some(true);
        }
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
                    ui.label(theme::muted("Enter confirms, Esc cancels").small());
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

    /// Toasts at the bottom right; successes fade out, errors wait for the x.
    fn toast_area(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.toasts
            .retain(|t| t.kind == Kind::Err || now - t.born < TOAST_SECS);
        if self.toasts.is_empty() {
            return;
        }
        if self.toasts.iter().any(|t| t.kind != Kind::Err) {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        let mut dismiss = None;
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -42.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                for (i, t) in self.toasts.iter().enumerate() {
                    let color = match t.kind {
                        Kind::Ok => theme::GOOD,
                        Kind::Info => theme::WARN,
                        Kind::Err => theme::BAD,
                    };
                    theme::card().stroke(egui::Stroke::new(1.5_f32, color)).show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.add(egui::Label::new(&t.text).wrap());
                            if ui.small_button("×").clicked() {
                                dismiss = Some(i);
                            }
                        });
                    });
                }
            });
        if let Some(i) = dismiss {
            self.toasts.remove(i);
        }
    }

    fn history_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_history;
        let mut clear = false;
        egui::Window::new("Activity")
            .open(&mut open)
            .default_width(460.0)
            .show(ctx, |ui| {
                if self.history.is_empty() {
                    ui.label(theme::muted("Nothing yet."));
                }
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    for (kind, text) in &self.history {
                        let color = match kind {
                            Kind::Ok => theme::GOOD,
                            Kind::Info => theme::WARN,
                            Kind::Err => theme::BAD,
                        };
                        ui.colored_label(color, text);
                    }
                });
                if !self.history.is_empty() && ui.button("Clear").clicked() {
                    clear = true;
                }
            });
        self.show_history = open;
        if clear {
            self.history.clear();
        }
    }

    /// Records the window rectangle so the next launch can restore it.
    fn track_window(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            let v = i.viewport();
            if v.minimized == Some(true) || v.maximized == Some(true) {
                return;
            }
            if let (Some(outer), Some(inner)) = (v.outer_rect, v.inner_rect) {
                self.window = Some([
                    outer.min.x as i32,
                    outer.min.y as i32,
                    inner.width() as i32,
                    inner.height() as i32,
                ]);
            }
        });
    }

    /// One frame of the whole window.
    #[doc(hidden)]
    pub fn ui(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.rx.try_recv() {
            self.busy = None;
            self.view = done.view;
            let failed = done.result.is_err();
            if done.startup {
                match done.result {
                    Ok(_) => {
                        let n = self.view.installed.iter().filter(|p| p.update.is_some()).count();
                        if n > 0 {
                            let s = if n == 1 { "" } else { "s" };
                            self.push(ctx, Kind::Info, format!("{n} plugin update{s} available"));
                        }
                    }
                    Err(e) => self.push(ctx, Kind::Info, format!("could not check for updates: {e}")),
                }
            } else {
                self.status = Some(done.result.map_err(|e| format!("{}: {e}", done.label)));
            }
            if failed {
                self.update_queue.clear();
            } else if self.consent.is_none() && !self.update_queue.is_empty() {
                let next = self.update_queue.remove(0);
                self.ask(&next);
            }
        }
        if std::mem::take(&mut self.startup_pending) && self.session.is_some() {
            self.startup_job = true;
            self.start(ctx, "Checking for updates", Job::Refresh);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            self.tab = Tab::Browse;
            self.focus_search = true;
        }
        if self.tab == Tab::Settings {
            let due = self
                .last_probe
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(2));
            if due {
                self.game_running = match self.view.core.as_ref().map(|c| &c.game_dir) {
                    Some(Ok(dir)) => jobs::game_running(dir),
                    _ => false,
                };
                self.last_probe = Some(Instant::now());
            }
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        self.track_window(ctx);

        egui::TopBottomPanel::top("tabs")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ctx, |ui| {
                let updates = self.view.installed.iter().filter(|p| p.update.is_some()).count();
                let installed = format!("Installed ({})", self.view.installed.len());
                let browse = if updates > 0 {
                    format!("Browse ({updates} update{})", if updates == 1 { "" } else { "s" })
                } else {
                    "Browse".to_owned()
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new("w2rpm").strong().size(20.0).color(theme::ACCENT));
                    ui.add_space(12.0);
                    ui.selectable_value(&mut self.tab, Tab::Installed, installed);
                    ui.selectable_value(&mut self.tab, Tab::Browse, browse);
                    ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                });
            });
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 6)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(b) = &self.busy {
                        ui.spinner();
                        ui.label(format!("{b}…"));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = format!("Activity ({})", self.history.len());
                        if ui.selectable_label(self.show_history, label).clicked() {
                            self.show_history = !self.show_history;
                        }
                    });
                });
                for n in &self.view.notes {
                    ui.label(RichText::new(n).weak());
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin::same(12)))
            .show(ctx, |ui| {
                // Greyed out while a job runs or a dialog is open.
                let idle = self.consent.is_none()
                    && self.confirm_remove.is_none()
                    && self.busy.is_none();
                ui.add_enabled_ui(idle, |ui| match self.tab {
                    Tab::Installed => self.installed_tab(ui, ctx),
                    Tab::Browse => self.browse_tab(ui, ctx),
                    Tab::Settings => self.settings_tab(ui, ctx),
                });
            });
        self.consent_window(ctx);
        self.remove_window(ctx);
        self.history_window(ctx);
        self.flush_status(ctx);
        self.toast_area(ctx);
    }
}

/// What a card's "Details" section shows: the full description, builds, what each capability
/// allows, and every published version.
fn details_ui(ui: &mut egui::Ui, p: &PluginEntry, v: &VersionEntry) {
    let m = &v.manifest;
    ui.label(&m.description);
    let builds: Vec<String> = m.game_builds.iter().map(|b| b.to_string()).collect();
    ui.label(theme::muted(format!("Game builds: {}", builds.join(", "))));
    ui.label(theme::muted(format!("Size: {:.1} KiB", v.size as f64 / 1024.0)));
    if m.capabilities.is_empty() {
        ui.label(theme::muted("Asks for no capabilities."));
    }
    for cap in &m.capabilities {
        ui.horizontal(|ui| {
            let danger = cap == "hooks.raw";
            theme::pill(ui, cap, if danger { theme::BAD } else { theme::MUTED });
            ui.label(theme::muted(capability_note(cap)));
        });
    }
    let versions: Vec<String> = p.versions.iter().map(|x| x.version.to_string()).collect();
    ui.label(theme::muted(format!("Versions: {}", versions.join(", "))));
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let mut s = self.settings.clone();
        s.window = self.window;
        let _ = s.save();
    }
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}
