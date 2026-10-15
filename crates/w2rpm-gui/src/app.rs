//! The egui window. It owns no logic: buttons turn into `Job`s that run on a worker thread
//! against `w2rpm-lib`, and the screen shows the `View` loaded afterwards. Only one job runs at a
//! time; while one runs the action buttons are off and the plugin it works on shows progress. The
//! consent window is the only way to install: it lists every plugin about to be installed (with
//! the dependencies it pulls in), every capability each asks for, marks the ones not approved
//! yet, and makes `hooks.raw` need a separate tick. Its yes queues one install job per plugin.
//!
//! Results of jobs and errors are put in `status`; once per frame `flush_status` turns that into a
//! toast and an entry in the activity list, so code anywhere can just set `status`.

use crate::jobs::{self, DepStatus, Installed, Job, View};
use crate::settings::{self, Settings};
use crate::theme;
use eframe::egui::{self, Key, Modifiers, RichText};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Arc;
use std::time::{Duration, Instant};
use w2r_formats::manifest::Dependency;
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

/// One plugin in an install waiting for the user's yes.
struct ConsentItem {
    entry: VersionEntry,
    pre: Preflight,
    /// Asked for by the user, as opposed to pulled in as a dependency.
    requested: bool,
}

/// An install (one plugin, an Update all, or a plugin with its dependencies) waiting for the
/// user's yes.
struct Consent {
    items: Vec<ConsentItem>,
    ack_raw: bool,
}

/// Bytes downloaded and expected for the running job, written by the worker thread.
#[derive(Default)]
struct Progress {
    done: AtomicU64,
    total: AtomicU64,
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
    /// Jobs to run one after another once the current one succeeds (the rest of an install).
    queue: Vec<(String, Job)>,
    confirm_remove: Option<(String, bool)>,
    busy: Option<String>,
    /// The plugin the running job works on, for the spinner on its card.
    busy_target: Option<String>,
    progress: Arc<Progress>,
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
            queue: Vec::new(),
            confirm_remove: None,
            busy: None,
            busy_target: None,
            progress: Arc::new(Progress::default()),
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

    /// Test hook: how many plugins the open consent window lists.
    #[doc(hidden)]
    pub fn consent_len(&self) -> usize {
        self.consent.as_ref().map_or(0, |c| c.items.len())
    }

    /// Rebuilds the session from the saved settings and reloads the view.
    fn apply_settings(&mut self) {
        self.draft = self.settings.clone();
        match jobs::root_of(self.settings.root.as_ref()) {
            Ok(root) => {
                let url = (!self.settings.index_url.trim().is_empty())
                    .then(|| self.settings.index_url.trim().to_owned());
                // Development: serve the index and packages from a folder (same checks as ever).
                let dir = std::env::var_os("W2RPM_INDEX_DIR").map(PathBuf::from);
                self.session = Session::new(root.clone(), url, dir)
                    .map(Arc::new)
                    .map_err(|e| self.status = Some(Err(format!("not connected to an index: {e}"))))
                    .ok();
                self.view = View::load(&root, self.session.as_deref());
                self.share =
                    Some(w2rpm_lib::share::Settings::load(&root.0).map_err(|e| e.to_string()));
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
        self.busy_target = job.target().map(str::to_owned);
        self.progress.done.store(0, Ordering::Relaxed);
        self.progress.total.store(0, Ordering::Relaxed);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let progress = self.progress.clone();
        let label = label.to_owned();
        let startup = std::mem::take(&mut self.startup_job);
        std::thread::spawn(move || {
            let result = jobs::run_with_progress(&session, &job, &mut |done, total| {
                progress.done.store(done, Ordering::Relaxed);
                progress.total.store(total, Ordering::Relaxed);
                ctx.request_repaint();
            })
            .map_err(|e| e.to_string());
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
        self.ask_many(vec![entry.clone()]);
    }

    /// Starts the consent window for `entries` and any dependencies the index can supply.
    fn ask_many(&mut self, entries: Vec<VersionEntry>) {
        let (Some(session), Some(idx)) = (self.session.clone(), self.view.index.clone()) else {
            return;
        };
        let all =
            jobs::with_dependencies(&idx, &self.view.installed, &session.manager, entries.clone());
        let mut items = Vec::new();
        for entry in all {
            match session.preflight(&idx, &entry) {
                Ok(mut pre) => {
                    // The dependency rows say this better than the library's one-liners.
                    pre.warnings.retain(|w| !w.starts_with("depends on "));
                    let requested = entries.iter().any(|e| e.manifest.id == entry.manifest.id);
                    items.push(ConsentItem {
                        entry,
                        pre,
                        requested,
                    });
                }
                Err(e) => {
                    self.status = Some(Err(e.to_string()));
                    return;
                }
            }
        }
        if !items.is_empty() {
            self.consent = Some(Consent {
                items,
                ack_raw: false,
            });
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

    fn open(&mut self, path: Option<PathBuf>) {
        match path {
            Some(p) => {
                if let Err(e) = jobs::open_path(&p) {
                    self.status = Some(Err(e));
                }
            }
            None => self.status = Some(Err("choose a w2r folder first".into())),
        }
    }

    fn installed_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let idle = self.busy.is_none();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(idle, egui::Button::new("Verify files"))
                .clicked()
            {
                self.start(ctx, "Verifying", Job::Verify);
            }
            let pending: Vec<VersionEntry> = self
                .view
                .installed
                .iter()
                .filter_map(|p| self.entry_of(&p.id, p.update.as_ref()?))
                .collect();
            if !pending.is_empty()
                && ui
                    .add_enabled(
                        idle,
                        egui::Button::new(
                            RichText::new(format!("Update all ({})", pending.len()))
                                .color(theme::GOOD),
                        ),
                    )
                    .clicked()
            {
                self.ask_many(pending);
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
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
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
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if self.busy_target.as_deref() == Some(p.id.as_str()) {
                                        let label = self.busy.clone().unwrap_or_default();
                                        busy_ui(ui, &self.progress, &label);
                                    }
                                    if !idle {
                                        ui.disable();
                                    }
                                    if ui.button("Remove").clicked() {
                                        self.confirm_remove = Some((p.id.clone(), false));
                                    }
                                    if ui.button("Roll back").clicked() {
                                        self.start(
                                            ctx,
                                            "Rolling back",
                                            Job::Rollback { id: p.id.clone() },
                                        );
                                    }
                                    if let Some(v) = &p.update {
                                        let entry = self.entry_of(&p.id, v);
                                        if ui
                                            .button(
                                                RichText::new(format!("Update to {v}"))
                                                    .color(theme::GOOD),
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
                                },
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label(theme::muted("Capabilities:"));
                            if p.capabilities.is_empty() {
                                ui.label(theme::muted("none"));
                            }
                            for c in &p.capabilities {
                                let color = if c == "hooks.raw" {
                                    theme::BAD
                                } else {
                                    theme::MUTED
                                };
                                theme::pill(ui, c, color);
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .small_button("Open folder")
                                    .on_hover_text("Open this plugin's folder in Explorer")
                                    .clicked()
                                {
                                    let dir = self.root.as_ref().map(|r| r.plugins_dir().join(&p.id));
                                    self.open(dir);
                                }
                            });
                        });
                        if let Some(problem) = &p.problem {
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(theme::BAD, format!("Problem: {problem}"));
                                let entry = self.entry_of(&p.id, &p.version);
                                let can = idle && entry.is_some();
                                if ui
                                    .add_enabled(can, egui::Button::new("Repair (reinstall)"))
                                    .on_disabled_hover_text(if idle {
                                        "Refresh the index first: this version is not in it"
                                    } else {
                                        "Another job is running"
                                    })
                                    .clicked()
                                    && let Some(e) = entry
                                {
                                    self.ask(&e);
                                }
                            });
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
                                .show(ui, |ui| details_ui(ui, pe, v, &rows, true));
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
        let idle = self.busy.is_none();
        ui.horizontal_wrapped(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search plugins (Ctrl+F)")
                    .desired_width(220.0),
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
            if ui
                .add_enabled(idle, egui::Button::new("Refresh index"))
                .clicked()
            {
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
                (
                    n.manifest.author.to_lowercase(),
                    n.manifest.name.to_lowercase(),
                )
            }),
        }
        if hits.is_empty() {
            ui.add_space(8.0);
            ui.label(theme::muted("No plugins match."));
        }
        let mut install: Option<VersionEntry> = None;
        let installed = self.view.installed.clone();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (p, newest) in hits {
                    let have = installed.iter().find(|i| i.id == p.id);
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
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if self.busy_target.as_deref() == Some(p.id.as_str()) {
                                        let label = self.busy.clone().unwrap_or_default();
                                        busy_ui(ui, &self.progress, &label);
                                    } else if let Some(h) = have {
                                        theme::badge(
                                            ui,
                                            &format!("Installed {}", h.version),
                                            theme::GOOD,
                                        );
                                    } else if let Some(entry) = can {
                                        if ui
                                            .add_enabled(idle, egui::Button::new("Install"))
                                            .clicked()
                                        {
                                            install = Some(entry.clone());
                                        }
                                    } else {
                                        theme::badge(ui, "Needs a newer w2rpm", theme::MUTED);
                                    }
                                },
                            );
                        });
                        ui.label(&m.description);
                        if m.affects_simulation {
                            ui.label(theme::muted("Every player must have it in multiplayer."));
                        }
                        egui::CollapsingHeader::new("Details")
                            .id_salt(("browse", &p.id))
                            .show(ui, |ui| details_ui(ui, p, newest, &installed, false));
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
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                theme::section(ui, "General", |ui| {
                    let shown = match jobs::root_of(self.draft.root.as_ref()) {
                        Ok(r) => r.0.display().to_string(),
                        Err(e) => e.to_string(),
                    };
                    ui.label(theme::muted("w2r folder"));
                    ui.label(shown);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Choose w2r folder…").clicked()
                            && let Some(p) = rfd::FileDialog::new()
                                .set_title("Choose the Warcraft2Remastered (or w2r) folder")
                                .pick_folder()
                        {
                            self.draft.root = Some(settings::root_from_pick(&p));
                        }
                        if ui.button("Use default").clicked() {
                            self.draft.root = None;
                        }
                        if ui.button("Open w2r folder").clicked() {
                            let dir = self.root.as_ref().map(|r| r.0.clone());
                            self.open(dir);
                        }
                        if ui.button("Open log").clicked() {
                            let log = self.root.as_ref().map(|r| r.0.join("logs").join("w2r.log"));
                            self.open(log);
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
                            "Leave empty to use the built-in index (once the plugin repository \
                             is published).",
                        )
                        .small(),
                    );
                    if ui.button("Save").clicked() {
                        self.draft.index_url = self.draft.index_url.trim().to_owned();
                        self.draft.window = self.window;
                        self.status = None;
                        match self.draft.save() {
                            Ok(()) => {}
                            Err(e) => {
                                self.status = Some(Err(format!("could not save settings: {e}")))
                            }
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
        let idle = self.busy.is_none();
        let running = self.game_running;
        let why = "Close the game first: it keeps version.dll in use while it runs.";
        let core_job = self.busy_target.is_none() && self.busy.is_some();
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(idle, egui::Button::new("Check")).clicked() {
                self.start(ctx, "Checking the core", Job::CoreCheck);
            }
            let label = if matches!(core.installed, Some(CoreInstalled::Missing)) {
                "Install core"
            } else {
                "Update core"
            };
            if ui
                .add_enabled(idle && !running, egui::Button::new(label))
                .on_disabled_hover_text(why)
                .clicked()
            {
                self.start(ctx, "Updating the core", Job::CoreUpdate);
            }
            if ui
                .add_enabled(idle && !running, egui::Button::new("Roll back core"))
                .on_disabled_hover_text(why)
                .clicked()
            {
                self.start(ctx, "Rolling back the core", Job::CoreRollback);
            }
            if ui
                .add_enabled(idle, egui::Button::new("Choose game folder…"))
                .clicked()
                && let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose the Warcraft II Remastered folder (the one with x86)")
                    .pick_folder()
            {
                self.start(ctx, "Setting the game folder", Job::CoreGameDir { dir });
            }
            if ui.button("Open game folder").clicked() {
                let dir = core.game_dir.clone().ok();
                self.open(dir);
            }
            if core_job {
                let label = self.busy.clone().unwrap_or_default();
                busy_ui(ui, &self.progress, &label);
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
                ui.colored_label(theme::BAD, format!("config/sharing.json is unusable: {e}"));
            }
            Ok(settings) => {
                let mut on = settings.share_match_data;
                let label = "Share my match results and replays with the stats service";
                if ui.checkbox(&mut on, label).changed() {
                    settings.share_match_data = on;
                    if let Err(e) = settings.save(&root.0) {
                        self.status = Some(Err(format!("could not save the sharing setting: {e}")));
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
        let raw = c
            .items
            .iter()
            .any(|i| i.pre.pending.iter().any(|x| x == "hooks.raw"));
        let can_install = !raw || c.ack_raw;
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            decision = Some(false);
        } else if can_install && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            decision = Some(true);
        }
        let title = match c.items.as_slice() {
            [one] => format!(
                "Install {} {}?",
                one.entry.manifest.name, one.entry.version
            ),
            many => format!("Install {} plugins?", many.len()),
        };
        let planned: Vec<VersionEntry> = c.items.iter().map(|i| i.entry.clone()).collect();
        let many = c.items.len() > 1;
        let all_approved = c.items.iter().all(|i| i.pre.pending.is_empty());
        let installed = &self.view.installed;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .min_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    theme::muted(
                        "Plugins are native code and run inside the game with your permissions.",
                    )
                    .small(),
                );
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .auto_shrink([true, true])
                    .show(ui, |ui| {
                        for item in &c.items {
                            consent_item_ui(ui, item, installed, &planned, many);
                        }
                    });
                if raw {
                    ui.checkbox(
                        &mut c.ack_raw,
                        "I understand hooks.raw can crash or compromise the game",
                    );
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let verb = if all_approved {
                        "Install"
                    } else {
                        "Approve and install"
                    };
                    if ui
                        .add_enabled(can_install, egui::Button::new(verb))
                        .clicked()
                    {
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
                let mut jobs: Vec<(String, Job)> = c
                    .items
                    .into_iter()
                    .map(|i| {
                        (
                            format!("Installing {}", i.entry.manifest.name),
                            Job::Install {
                                id: i.entry.manifest.id.clone(),
                                version: Some(i.entry.version.clone()),
                                approved: i.pre.pending,
                            },
                        )
                    })
                    .collect();
                let (label, first) = jobs.remove(0);
                self.queue = jobs;
                self.start(ctx, &label, first);
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
                ui.set_width(320.0);
                for (i, t) in self.toasts.iter().enumerate() {
                    let color = match t.kind {
                        Kind::Ok => theme::GOOD,
                        Kind::Info => theme::WARN,
                        Kind::Err => theme::BAD,
                    };
                    theme::card()
                        .stroke(egui::Stroke::new(1.5_f32, color))
                        .show(ui, |ui| {
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
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
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
            self.busy_target = None;
            self.view = done.view;
            let failed = done.result.is_err();
            if done.startup {
                match done.result {
                    Ok(_) => {
                        let n = self
                            .view
                            .installed
                            .iter()
                            .filter(|p| p.update.is_some())
                            .count();
                        if n > 0 {
                            let s = if n == 1 { "" } else { "s" };
                            self.push(ctx, Kind::Info, format!("{n} plugin update{s} available"));
                        }
                    }
                    Err(e) => self.push(
                        ctx,
                        Kind::Info,
                        format!("could not check for updates: {e}"),
                    ),
                }
            } else {
                self.status = Some(done.result.map_err(|e| format!("{}: {e}", done.label)));
            }
            if failed {
                self.queue.clear();
            } else if !self.queue.is_empty() {
                let (label, job) = self.queue.remove(0);
                self.start(ctx, &label, job);
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
                let updates = self
                    .view
                    .installed
                    .iter()
                    .filter(|p| p.update.is_some())
                    .count();
                let installed = format!("Installed ({})", self.view.installed.len());
                let browse = if updates > 0 {
                    format!(
                        "Browse ({updates} update{})",
                        if updates == 1 { "" } else { "s" }
                    )
                } else {
                    "Browse".to_owned()
                };
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("w2rpm")
                            .strong()
                            .size(20.0)
                            .color(theme::ACCENT),
                    );
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
                        let queued = self.queue.len();
                        let label = if queued > 0 {
                            format!("{b} ({queued} more after this)")
                        } else {
                            b.clone()
                        };
                        busy_ui(ui, &self.progress, &label);
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
                // Everything is greyed while a dialog is open; while a job runs only the action
                // buttons are (each screen checks `busy`), so you can still scroll and search.
                let modal = self.consent.is_some() || self.confirm_remove.is_some();
                ui.add_enabled_ui(!modal, |ui| match self.tab {
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

fn human(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    } else {
        format!("{:.0} KB", n as f64 / 1024.0)
    }
}

/// A spinner and the job's name, or a bar while a download reports its size.
fn busy_ui(ui: &mut egui::Ui, progress: &Progress, label: &str) {
    let done = progress.done.load(Ordering::Relaxed);
    let total = progress.total.load(Ordering::Relaxed);
    if total > 0 {
        ui.add(
            egui::ProgressBar::new((done as f32 / total as f32).min(1.0))
                .desired_width(140.0)
                .text(format!("{} / {}", human(done), human(total))),
        );
    } else {
        ui.spinner();
    }
    ui.label(format!("{label}…"));
}

/// The dependency rows shared by Details and the consent window.
fn dep_rows(
    ui: &mut egui::Ui,
    deps: &[Dependency],
    installed: &[Installed],
    planned: &[VersionEntry],
) {
    for d in deps {
        ui.horizontal(|ui| {
            match jobs::dep_status(installed, planned, d) {
                DepStatus::Satisfied(v) => {
                    theme::badge(ui, &format!("installed {v}"), theme::GOOD)
                }
                DepStatus::Planned(v) => {
                    theme::badge(ui, &format!("will be installed {v}"), theme::GOOD)
                }
                DepStatus::Disabled(v) => {
                    theme::badge(ui, &format!("installed {v}, disabled"), theme::WARN)
                }
                DepStatus::WrongVersion(v) => {
                    theme::badge(ui, &format!("installed {v}, wrong version"), theme::WARN)
                }
                DepStatus::Missing => theme::badge(ui, "missing", theme::BAD),
            }
            theme::pill(ui, &d.id, theme::MUTED);
            ui.label(theme::muted(d.version_req.to_string()));
        });
    }
}

/// One plugin's block in the consent window.
fn consent_item_ui(
    ui: &mut egui::Ui,
    item: &ConsentItem,
    installed: &[Installed],
    planned: &[VersionEntry],
    many: bool,
) {
    let m = &item.entry.manifest;
    let body = |ui: &mut egui::Ui| {
        ui.horizontal(|ui| {
            if many {
                ui.label(RichText::new(&m.name).strong());
                ui.label(theme::muted(item.entry.version.to_string()));
                if !item.requested {
                    theme::badge(ui, "dependency", theme::MUTED);
                }
            }
            ui.label(theme::muted(format!("by {}", m.author)));
        });
        ui.label(&m.description);
        if m.affects_simulation {
            theme::badge(ui, "Changes the simulation", theme::WARN);
            ui.label(theme::muted("Players need it to match in multiplayer."));
        }
        let builds: Vec<String> = m.game_builds.iter().map(|b| b.to_string()).collect();
        ui.label(theme::muted(format!(
            "Built for game builds: {}",
            builds.join(", ")
        )));
        for w in &item.pre.warnings {
            ui.colored_label(theme::BAD, format!("Warning: {w}"));
        }
        if !m.dependencies.is_empty() {
            ui.add_space(2.0);
            ui.label(RichText::new("Needs").strong());
            dep_rows(ui, &m.dependencies, installed, planned);
        }
        ui.add_space(2.0);
        ui.label(RichText::new("Capabilities").strong());
        if m.capabilities.is_empty() {
            ui.label(theme::muted("Asks for no capabilities."));
        }
        for cap in &m.capabilities {
            let new = item.pre.pending.contains(cap);
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
    };
    if many {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui)
        });
        ui.add_space(4.0);
    } else {
        body(ui);
    }
}

/// What a card's "Details" section shows: the full description, builds, dependencies, what each
/// capability allows, and every published version.
fn details_ui(
    ui: &mut egui::Ui,
    p: &PluginEntry,
    v: &VersionEntry,
    installed: &[Installed],
    with_description: bool,
) {
    let m = &v.manifest;
    if with_description {
        ui.label(&m.description);
    }
    let builds: Vec<String> = m.game_builds.iter().map(|b| b.to_string()).collect();
    ui.label(theme::muted(format!("Game builds: {}", builds.join(", "))));
    ui.label(theme::muted(format!(
        "Size: {:.1} KiB",
        v.size as f64 / 1024.0
    )));
    if !m.dependencies.is_empty() {
        ui.label(theme::muted("Needs:"));
        dep_rows(ui, &m.dependencies, installed, &[]);
    }
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
