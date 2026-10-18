//! The "Send diagnostics" window: shows exactly which files go into the bundle, takes an optional
//! note, and either saves the zip or sends it to the stats service. Nothing leaves the machine
//! until Send is pressed. The bundle is built by `w2r_loader_lib::diag`; this file is only the
//! screen and the worker thread.

use crate::theme;
use eframe::egui::{self, Key, Modifiers};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use w2r_loader_lib::diag::{self, Bundle};
use w2r_loader_lib::fetch::Https;
use w2r_loader_lib::share::Settings;

pub struct DiagWin {
    root: PathBuf,
    summary: String,
    dumps: bool,
    has_dumps: bool,
    note: String,
    /// The preview: the files and the zip's size, or why none could be built.
    preview: Result<(Bundle, usize), String>,
    rx: Option<Receiver<Result<String, String>>>,
    /// True while a Send (not a Save) runs, so its answer is shown as a report code.
    sending: bool,
    /// What the last Send or Save did: (was a send, the report code or saved path or the error).
    outcome: Option<(bool, Result<String, String>)>,
}

impl DiagWin {
    pub fn new(root: PathBuf, summary: String) -> DiagWin {
        let has_dumps = std::fs::read_dir(root.join("logs"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("dmp")));
        let mut w = DiagWin {
            root,
            summary,
            dumps: false,
            has_dumps,
            note: String::new(),
            preview: Err(String::new()),
            rx: None,
            sending: false,
            outcome: None,
        };
        w.refresh();
        w
    }

    fn options(&self) -> diag::Options {
        diag::Options {
            include_dumps: self.dumps,
            note: self.note.clone(),
            summary: self.summary.clone(),
        }
    }

    fn refresh(&mut self) {
        self.preview = diag::build(&self.root, &self.options(), &diag::Private::from_env())
            .map(|(b, z)| (b, z.len()))
            .map_err(|e| e.detail);
    }

    /// Builds the bundle again (the note may have changed) on a worker thread, then saves it to
    /// `save_to` and/or sends it.
    fn spawn(&mut self, ctx: &egui::Context, save_to: Option<PathBuf>, send: bool) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.sending = send;
        self.outcome = None;
        let (root, opts) = (self.root.clone(), self.options());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let run = || -> Result<String, String> {
                let (_, zip) = diag::build(&root, &opts, &diag::Private::from_env())
                    .map_err(|e| e.detail)?;
                let mut said = String::new();
                if let Some(p) = save_to {
                    std::fs::write(&p, &zip).map_err(|e| format!("could not save: {e}"))?;
                    said = format!("Saved {}", p.display());
                }
                if send {
                    let settings = Settings::load(&root).unwrap_or_default();
                    said = diag::send(&root, &settings, &Https::default(), &zip)
                        .map_err(|e| e.detail)?;
                }
                Ok(said)
            };
            let _ = tx.send(run());
            ctx.request_repaint();
        });
    }

    /// Draws the window; true when it should close.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut close = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        if let Some(rx) = &self.rx
            && let Ok(r) = rx.try_recv()
        {
            if let (true, Ok(code)) = (self.sending, &r) {
                ctx.copy_text(code.clone());
            }
            self.outcome = Some((self.sending, r));
            self.rx = None;
        }
        let busy = self.rx.is_some();
        let mut refresh = false;
        let mut save = false;
        let mut send = false;
        egui::Window::new("Send diagnostics")
            .collapsible(false)
            .resizable(false)
            .min_width(480.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "This bundles your logs and settings so the developer can see what went \
                     wrong. Nothing is sent until you press Send.",
                );
                ui.label(
                    theme::muted(
                        "Your Windows user name and computer name, BattleTags (and player names \
                         seen with them) and IP addresses are replaced in every text file. \
                         Replays, chat logs and your installation key are never included.",
                    )
                    .small(),
                );
                ui.add_space(4.0);
                match &self.preview {
                    Err(e) => {
                        ui.colored_label(theme::BAD, e);
                    }
                    Ok((bundle, zip)) => {
                        let head = format!(
                            "{} files, {} KB when sent",
                            bundle.entries.len(),
                            zip.div_ceil(1024)
                        );
                        egui::CollapsingHeader::new(head).show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                                for e in &bundle.entries {
                                    let warn = if e.scrubbed { "" } else { "  (not scrubbed)" };
                                    ui.label(
                                        theme::muted(format!(
                                            "{}  {} KB{warn}",
                                            e.name,
                                            e.bytes.len().div_ceil(1024)
                                        ))
                                        .small(),
                                    );
                                }
                            });
                        });
                    }
                }
                if self.has_dumps
                    && ui
                        .checkbox(&mut self.dumps, "Include the latest crash/hang dumps")
                        .on_hover_text(
                            "Memory snapshots from a hang or crash. They help find the cause \
                             but cannot be scrubbed and may contain anything that was in memory.",
                        )
                        .changed()
                {
                    refresh = true;
                }
                ui.add_space(4.0);
                ui.label(theme::muted("What happened? (optional)"));
                if ui
                    .add(
                        egui::TextEdit::multiline(&mut self.note)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .char_limit(diag::MAX_NOTE),
                    )
                    .lost_focus()
                {
                    refresh = true;
                }
                ui.add_space(4.0);
                match &self.outcome {
                    Some((true, Ok(code))) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Sent (code copied). Give the developer this code:");
                            ui.label(egui::RichText::new(code).strong().monospace());
                            if ui.button("Copy").clicked() {
                                ctx.copy_text(code.clone());
                            }
                        });
                    }
                    Some((false, Ok(msg))) => {
                        ui.label(msg.as_str());
                    }
                    Some((_, Err(e))) => {
                        ui.colored_label(theme::BAD, format!("Could not finish: {e}"));
                    }
                    None => {}
                }
                ui.horizontal(|ui| {
                    let ready = !busy && self.preview.is_ok();
                    send = ui.add_enabled(ready, egui::Button::new("Send")).clicked();
                    save = ui
                        .add_enabled(ready, egui::Button::new("Save zip…"))
                        .clicked();
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    if busy {
                        ui.spinner();
                    }
                });
            });
        if refresh && !busy {
            self.refresh();
        }
        if send {
            self.spawn(ctx, None, true);
        } else if save
            && let Some(p) = rfd::FileDialog::new()
                .set_title("Save diagnostics")
                .set_file_name("w2r-diagnostics.zip")
                .save_file()
        {
            self.spawn(ctx, Some(p), false);
        }
        close
    }
}
