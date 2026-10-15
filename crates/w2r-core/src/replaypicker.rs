//! The replay picker: a panel on the Create Multiplayer Game screen (opened by its "Watch a
//! replay" button) that lists the recordings, newest first, and queues one. Choosing "Watch"
//! makes the recording's map the screen's selected map and queues the replay, so pressing Create
//! plays it (docs/replay-research.md, "Playback"). The same panel shape as the map browser
//! (`browserview`). x86 only.

use std::sync::Arc;

use w2r_replay::catalog::{self, Entry};

use crate::browserview::{INDEX_SLICE, Theme, View};
use crate::mapbrowser::{Browser, IndexedMap, Source};
use crate::screenkit::game::Kit;
use crate::screenkit::*;

const PANEL_W_FRACTION: f32 = 0.80;
const PANEL_H_FRACTION: f32 = 0.88;
const LIST_FRACTION: f32 = 0.56;
/// Room the list's scrollbar takes at the right, which the selection wash leaves free.
const LIST_SCROLLBAR_W: f32 = 90.0;
/// The newest this many recordings are listed.
const LIST_LIMIT: usize = 200;

/// Whether the installed map is the one a recording was played on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MapCheck {
    /// Same bytes as when it was recorded.
    Same,
    /// A file of that name, but different bytes: the replay would desync.
    Differs,
    /// Nothing to compare (the recording has no hash, or the file cannot be read).
    Unknown,
}

#[derive(Default)]
pub struct Picker {
    open: bool,
    entries: Vec<Entry>,
    chosen: Option<usize>,
    /// Minutes east of UTC for the dates shown.
    utc_offset: i32,
    /// The queued recording whose map has been dealt with on this visit to the screen.
    auto_for: Option<String>,
    /// The map file of the queued recording, once read.
    auto_map: Option<String>,
    /// The queued recording whose game name and password have been filled on this visit.
    filled_for: Option<String>,
    /// The last map comparison, for the recording and map file it was made for (hashing reads
    /// the whole file, so it is done once, not every frame).
    checked: Option<(String, String, MapCheck)>,
    /// The recording whose Delete button has been pressed once.
    confirm_delete: Option<String>,
    /// The combat events of the highlighted recording and the heat map shown over its map.
    heat: HeatState,
}

/// The heat map choices, in the order the button steps through them.
const HEAT_LABELS: [&str; 5] = [
    "Heat map: off",
    "Heat map: whole game",
    "Heat map: early game",
    "Heat map: mid game",
    "Heat map: late game",
];
/// Recordings larger than this are not read for their heat map.
const HEAT_MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Default)]
struct HeatState {
    /// The recording the events are for.
    file: String,
    /// Its combat events and number of turns; `None` when it has none (or cannot be read).
    events: Option<(Vec<w2r_replay::Event>, u32)>,
    mode: usize,
    image: Option<Arc<Vec<u8>>>,
}

impl HeatState {
    /// Reads the events of `file` when it is not the one already held.
    fn load(&mut self, file: &str, bytes: u64) {
        if self.file == file {
            return;
        }
        self.file = file.to_string();
        self.mode = 0;
        self.image = None;
        self.events = None;
        if bytes > HEAT_MAX_BYTES {
            return;
        }
        let Some(raw) = crate::replayplay::replays_dir().and_then(|d| std::fs::read(d.join(file)).ok())
        else {
            return;
        };
        if let Ok(replay) = w2r_replay::Replay::parse(&raw)
            && !replay.events.is_empty()
        {
            let total = replay.footer.map_or(replay.turns.len() as u32, |f| f.turns);
            self.events = Some((replay.events, total));
        }
    }

    /// Steps to the next choice and makes its image.
    fn step(&mut self) {
        self.mode = (self.mode + 1) % HEAT_LABELS.len();
        self.image = self.events.as_ref().and_then(|(events, total)| {
            let range = match self.mode {
                1 => w2r_replay::heat::Range::All,
                n @ 2..=4 => w2r_replay::heat::Range::Third(n as u8 - 2),
                _ => return None,
            };
            Some(Arc::new(w2r_replay::heat::image(events, *total, range)))
        });
    }
}

impl Picker {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Opens the picker on a fresh read of the replays folder.
    pub fn open(&mut self) {
        self.entries = crate::replayplay::replays_dir()
            .map(|dir| catalog::scan(&dir, LIST_LIMIT))
            .unwrap_or_default();
        self.chosen = (!self.entries.is_empty()).then_some(0);
        self.utc_offset = local_utc_offset_minutes();
        self.open = true;
    }

    /// A new visit to the screen: a queued replay's map is selected again.
    pub fn forget_auto(&mut self) {
        self.auto_for = None;
        self.auto_map = None;
        self.filled_for = None;
    }

    /// While a replay is queued, fills the game name and password once per queued recording per
    /// visit, so what is typed afterwards stays. Both are NUL-terminated C buffers.
    ///
    /// # Safety
    /// `name` must be valid for `name_cap` bytes (or null) and `password` for `password_cap`.
    pub unsafe fn fill_lobby_text(
        &mut self,
        name: *mut u8,
        name_cap: usize,
        password: *mut u8,
        password_cap: usize,
    ) {
        let Some(file) = crate::replayplay::queued() else {
            self.filled_for = None;
            return;
        };
        if self.filled_for.as_deref() == Some(file.as_str()) {
            return;
        }
        let stem = file.rsplit_once('.').map_or(file.as_str(), |(s, _)| s);
        let put = |buf: *mut u8, cap: usize, text: &str| {
            if buf.is_null() || cap == 0 {
                return;
            }
            let n = text.len().min(cap - 1);
            // SAFETY: the caller guarantees `cap` bytes; the text is cut to leave room for the NUL.
            unsafe {
                core::ptr::copy_nonoverlapping(text.as_ptr(), buf, n);
                *buf.add(n) = 0;
            }
        };
        put(name, name_cap, &format!("Replay {stem}"));
        put(password, password_cap, &random_password());
        self.filled_for = Some(file);
    }

    /// While a replay is queued, makes its map the screen's selected map (once per queued
    /// recording per visit, so a map picked by hand afterwards stays). The map is found in the
    /// map index, which is walked a slice a frame, so this may take a few frames.
    pub fn auto_select(&mut self, browser: &mut Browser, view: &View) {
        let Some(file) = crate::replayplay::queued() else {
            self.auto_for = None;
            self.auto_map = None;
            return;
        };
        if self.auto_for.as_deref() == Some(file.as_str()) {
            return;
        }
        if self.auto_map.is_none() {
            let entry = crate::replayplay::replays_dir()
                .and_then(|dir| std::fs::read(dir.join(&file)).ok())
                .and_then(|raw| catalog::entry_from_bytes(&file, &raw));
            match entry {
                Some(e) => self.auto_map = Some(e.map),
                None => {
                    // Not a recording we can read: nothing to select.
                    self.auto_for = Some(file);
                    return;
                }
            }
        }
        let Some(want) = self.auto_map.clone() else { return };
        if file_of(&view.selected_choice().path).eq_ignore_ascii_case(&want) {
            self.auto_for = Some(file);
            return;
        }
        let mut src = view.source;
        if let Some(root) = crate::mapdownload::maps_root_string() {
            browser.set_root(&root);
        }
        browser.observe_root(&src.rel_path());
        if !browser.index_ready() {
            browser.step_index(&mut src, Some(INDEX_SLICE));
            if !browser.index_ready() {
                return;
            }
        }
        let found = browser
            .indexed()
            .iter()
            .find(|m| file_of(&m.path).eq_ignore_ascii_case(&want))
            .cloned();
        if let Some(m) = found
            && view.select_indexed(browser, &m)
        {
            crate::session::log_info(
                "replaypicker",
                &format!("selected {want} for the queued replay {file}"),
            );
        }
        self.auto_for = Some(file);
    }

    /// Compares the installed map file with the hash the recording was made with.
    fn check_map(&mut self, entry: &Entry, path: &str) -> MapCheck {
        if let Some((file, p, check)) = &self.checked
            && *file == entry.file
            && p == path
        {
            return *check;
        }
        let check = match (
            entry.map_sha,
            crate::mapindex::hash_file(std::path::Path::new(path)),
        ) {
            (Some(want), Some(have)) if want == have => MapCheck::Same,
            (Some(_), Some(_)) => MapCheck::Differs,
            _ => MapCheck::Unknown,
        };
        self.checked = Some((entry.file.clone(), path.to_owned(), check));
        check
    }

    /// Deletes a recording's file and drops it from the list, taking it out of the queue first
    /// if it was the queued one.
    fn delete_chosen(&mut self, file: &str) {
        self.confirm_delete = None;
        let Some(dir) = crate::replayplay::replays_dir() else { return };
        match catalog::delete(&dir, file) {
            Ok(()) => {
                crate::session::log_info("replaypicker", &format!("deleted {file}"));
                if crate::replayplay::queued().as_deref() == Some(file) {
                    crate::replayplay::unqueue();
                }
                self.entries.retain(|e| e.file != file);
                self.chosen = (!self.entries.is_empty())
                    .then(|| self.chosen.unwrap_or(0).min(self.entries.len() - 1));
                self.checked = None;
            }
            Err(err) => {
                crate::session::log_error("replaypicker", &format!("could not delete {file}: {err}"))
            }
        }
    }

    fn entry(&self) -> Option<&Entry> {
        self.chosen.and_then(|i| self.entries.get(i))
    }
}

/// Minutes east of UTC in the viewer's time zone right now (0 if Windows will not say).
/// Six random digits, from the standard library's randomly keyed hasher (no extra dependency).
fn random_password() -> String {
    use std::hash::{BuildHasher, Hasher};
    let n = std::collections::hash_map::RandomState::new().build_hasher().finish();
    format!("{:06}", n % 1_000_000)
}

fn local_utc_offset_minutes() -> i32 {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    // SAFETY: a plain query into a zeroed out-structure of the documented type.
    unsafe {
        let mut info: TIME_ZONE_INFORMATION = core::mem::zeroed();
        let id = GetTimeZoneInformation(&mut info);
        // 2 is TIME_ZONE_ID_DAYLIGHT; the bias is minutes WEST of UTC.
        let extra = if id == 2 { info.DaylightBias } else { info.StandardBias };
        -(info.Bias + extra)
    }
}

/// The last component of a game path (either slash).
fn file_of(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// The indexed map a recording was played on, if the map is installed.
fn find_map<'a>(browser: &'a Browser, entry: &Entry) -> Option<&'a IndexedMap> {
    browser
        .indexed()
        .iter()
        .find(|m| file_of(&m.path).eq_ignore_ascii_case(&entry.map))
}

/// Why a recording cannot be played here, if it cannot.
fn problem(entry: &Entry) -> Option<&'static str> {
    if !entry.has_start_state {
        return Some("Recorded before replays existed");
    }
    None
}

/// A reason to think the recording may not play back cleanly, which does not stop it.
fn warning(entry: &Entry) -> Option<&'static str> {
    match crate::replayplay::fingerprint() {
        Some(now) if now != entry.fingerprint => Some("Plugins differ from the recording"),
        _ => None,
    }
}

/// About how many characters of a list row fit in `width` (the game's row font, 2160-high space).
fn row_budget(width: f32) -> usize {
    (width / 36.0).max(10.0) as usize
}

/// Cuts `text` to `max` characters, ending in ".." when something was dropped.
fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(2)).collect();
    format!("{kept}..")
}

/// The map's name without its file extension.
fn map_title(map: &str) -> &str {
    map.trim_end_matches(".pud").trim_end_matches(".PUD")
}

/// The race name for a slot's race byte, when it is one of the two known.
fn race_name(race: u8) -> Option<&'static str> {
    match race {
        0 => Some("Human"),
        1 => Some("Orc"),
        _ => None,
    }
}

/// One list row, most important first: the result, the map, who it was against, then the short
/// date. The map and the opponents are cut to fit; the rest is in the details.
fn row_label(entry: &Entry, utc_offset: i32, budget: usize) -> String {
    let when = entry.when_short(utc_offset);
    let result = capitalised(entry.outcome.text());
    let map = shorten(map_title(&entry.map), 24);
    let head = format!("  {result}  {map}  ");
    let tail = format!("  {when}");
    let room = budget.saturating_sub(head.chars().count() + tail.chars().count() + 3);
    let opponents = entry.opponents();
    if opponents.is_empty() {
        return format!("{head}{when}");
    }
    format!("{head}vs {}{tail}", shorten(&opponents, room))
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// "1.2 MB" / "340 KB".
fn size_text(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{} KB", (bytes / 1000).max(1))
    }
}

/// A player's line in the details: "Beau - Orc, team 1" (computers marked).
fn player_text(p: &w2r_replay::catalog::PlayerLine, is_recorder: bool) -> String {
    let mut parts = Vec::new();
    if let Some(r) = race_name(p.race) {
        parts.push(r.to_owned());
    }
    parts.push(format!("team {}", p.team));
    if !p.human {
        parts.push("computer".to_owned());
    }
    let you = if is_recorder { " (recorder)" } else { "" };
    format!("{}{you} - {}", p.name, parts.join(", "))
}

impl Picker {
    /// Draws the panel. `window` is the screen's window. Returns the square for the chosen
    /// recording's map picture and its file, to be painted once the layout space has ended.
    ///
    /// # Safety
    /// Must run inside the screen's layout space, on the game's main thread.
    pub unsafe fn draw(
        &mut self,
        k: &Kit,
        browser: &mut Browser,
        view: &View,
        window: Rect,
        theme: Theme,
    ) -> Option<(Rect, String, Option<Arc<Vec<u8>>>)> {
        let mut src = view.source;
        // The map index is walked a slice a frame while the panel is up, to find the maps.
        if let Some(root) = crate::mapdownload::maps_root_string() {
            browser.set_root(&root);
        }
        browser.observe_root(&src.rel_path());
        if !browser.index_ready() {
            browser.step_index(&mut src, Some(INDEX_SLICE));
        }

        let w = fl(window.w * PANEL_W_FRACTION);
        let h = fl(window.h * PANEL_H_FRACTION);
        let frame = Rect::new(fl((window.w - w) * 0.5), fl((window.h - h) * 0.5), w, h);
        k.panel_frame(frame, c"watch_replay", Some(theme.right_frame));

        let x0 = frame.x + FRAME_PAD;
        let top = frame.y + FRAME_PAD;
        let inner_right = frame.x + w - FRAME_PAD;
        let inner_w = inner_right - x0;
        let bottom = frame.y + h - FRAME_PAD;
        let left_w = fl(inner_w * LIST_FRACTION);
        let right_x = x0 + left_w + GAP * 2.0;
        let right_w = inner_right - right_x;

        // The heading sits over the list; the details panel starts at the top of the frame.
        k.heading_at(
            Rect::new(x0, top, left_w, NAME_H),
            "Watch a Replay",
            ALIGN_CENTRE,
            Some(theme.heading_font),
        );
        let list_y = top + NAME_H + GAP;

        // The list, in the same group the map list uses, so it gets the same scrollbar.
        let list = Rect::new(x0, list_y, left_w, bottom - list_y);
        if k.begin_list(list, c"select_map_list") {
            if self.entries.is_empty() {
                k.row_dynamic(ROW_H, 1);
                k.label("No recordings yet. Play a game and it is saved here.", ALIGN_LEFT);
            }
            let budget = row_budget(left_w);
            let mut picked = None;
            for (i, e) in self.entries.iter().enumerate() {
                let label = row_label(e, self.utc_offset, budget);
                let row_top = k.next_row_top();
                if k.list_row_button(&label) {
                    picked = Some(i);
                }
                if self.chosen == Some(i) {
                    k.highlight_row(row_top, left_w - LIST_SCROLLBAR_W);
                }
            }
            if picked.is_some() {
                if picked != self.chosen {
                    self.confirm_delete = None;
                }
                self.chosen = picked;
            }
            k.end_list();
        }

        // Buttons under the details: Clear queued (when one is queued), then Cancel and Watch.
        let buttons_y = bottom - ROW_H;
        let clear_y = buttons_y - GAP - ROW_H;
        let queued = crate::replayplay::queued();
        if queued.is_some()
            && k.button_at(Rect::new(right_x, clear_y, right_w, ROW_H), "Clear queued replay", false)
        {
            crate::replayplay::unqueue();
        }
        let details_bottom = if queued.is_some() { clear_y - GAP } else { buttons_y - GAP };

        // The details of the highlighted recording.
        let details = Rect::new(right_x, top, right_w, details_bottom - top);
        k.panel_frame(details, c"watch_replay_details", Some(theme.left_frame));
        let dx = right_x + FRAME_PAD;
        let dw = right_w - FRAME_PAD * 2.0;
        let entry = self.entry().cloned();
        let found = entry.as_ref().and_then(|e| find_map(browser, e).cloned());
        let check = match (&entry, &found) {
            (Some(e), Some(m)) => self.check_map(e, &m.path),
            _ => MapCheck::Unknown,
        };
        let mut preview = None;
        if let Some(e) = &entry {
            self.heat.load(&e.file, e.bytes);
            k.heading_at(
                Rect::new(dx, top + FRAME_PAD, dw, NAME_H),
                &e.map,
                ALIGN_CENTRE,
                Some(theme.heading_font),
            );
            let mut y = top + FRAME_PAD + NAME_H + GAP;
            let mut line = |text: &str| {
                k.label_at(Rect::new(dx, y, dw, CAPTION_H), text, ALIGN_LEFT);
                y += CAPTION_H;
            };
            let fit = row_budget(dw);
            line(&format!("{} - {}", capitalised(e.outcome.text()), e.when(self.utc_offset)));
            line(&format!("{} turns, {}", e.turns, size_text(e.bytes)));
            let shown = 4;
            for p in e.players.iter().take(shown) {
                line(&shorten(&player_text(p, p.slot == e.local), fit));
            }
            if e.players.len() > shown {
                line(&format!("and {} more", e.players.len() - shown));
            }
            let map_state = if found.is_some() {
                match check {
                    MapCheck::Same => "Map found, verified",
                    MapCheck::Differs => "Map differs from recording",
                    MapCheck::Unknown => "Map found",
                }
            } else if browser.index_ready() {
                "This map is not installed"
            } else {
                "Looking for the map..."
            };
            line(map_state);
            if let Some(why) = problem(e) {
                line(why);
            }
            if let Some(note) = warning(e) {
                line(note);
                line("It may go out of sync");
            }
            // Where the fighting was, over the picture below.
            if self.heat.events.is_some() {
                if k.button_at(
                    Rect::new(dx, y + GAP, dw, ROW_H),
                    HEAT_LABELS[self.heat.mode],
                    false,
                ) {
                    self.heat.step();
                }
                y += GAP + ROW_H;
            } else if self.heat.mode == 0 {
                line("No combat events (recorded before they were kept)");
            }
            // The picture, a square inside the frame's padding.
            let picture_y = y + GAP;
            let side = dw.min(details.y + details.h - FRAME_PAD - picture_y).max(0.0);
            if let Some(m) = &found
                && side > 0.0
            {
                preview = Some((
                    Rect::new(dx + (dw - side) * 0.5, picture_y, side, side),
                    m.path.clone(),
                    self.heat.image.clone(),
                ));
            }
        }

        let third = fl((right_w - GAP * 2.0) / 3.0);
        // Delete takes two presses: the first arms it for the highlighted recording.
        let armed = entry
            .as_ref()
            .is_some_and(|e| self.confirm_delete.as_deref() == Some(e.file.as_str()));
        if k.button_at(
            Rect::new(right_x, buttons_y, third, ROW_H),
            if armed { "Sure?" } else { "Delete" },
            entry.is_none(),
        ) && let Some(e) = &entry
        {
            if armed {
                self.delete_chosen(&e.file);
            } else {
                self.confirm_delete = Some(e.file.clone());
            }
        }
        if k.button_at(Rect::new(right_x + third + GAP, buttons_y, third, ROW_H), "Cancel", false) {
            browser.cancel_walk();
            self.open = false;
        }
        let can_watch = entry.as_ref().is_some_and(|e| problem(e).is_none())
            && found.is_some()
            && check != MapCheck::Differs;
        if k.button_at(
            Rect::new(right_x + (third + GAP) * 2.0, buttons_y, right_w - (third + GAP) * 2.0, ROW_H),
            "Watch",
            !can_watch,
        ) && let (Some(e), Some(m)) = (&entry, &found)
            && view.select_indexed(browser, m)
        {
            match crate::replayplay::queue(&e.file) {
                Ok(()) => {
                    self.auto_for = Some(e.file.clone());
                    crate::session::log_info(
                        "replaypicker",
                        &format!("queued {} on {}", e.file, e.map),
                    );
                    browser.cancel_walk();
                    self.open = false;
                }
                Err(err) => crate::session::log_error(
                    "replaypicker",
                    &format!("could not queue {}: {err}", e.file),
                ),
            }
        }
        preview
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_cut_with_two_dots() {
        assert_eq!(shorten("Stormreaver Clan", 20), "Stormreaver Clan");
        assert_eq!(shorten("Stormreaver Clan", 10), "Stormrea..");
        assert_eq!(shorten("abc", 2), "..");
    }

    #[test]
    fn the_budget_grows_with_the_width() {
        assert!(row_budget(1700.0) > row_budget(900.0));
        assert!(row_budget(0.0) >= 10);
    }
}
