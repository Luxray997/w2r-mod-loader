//! The replacement Create Multiplayer Game screen (docs/create-screen.md, slice 1), ported from
//! wc2r-plus `features/ui/lobby_creation_screen.cpp`. When the `enhanced_create_screen` setting
//! is on and everything it needs resolved, [`try_draw`] draws it in place of the game's own
//! content (inside the same window); otherwise the stock screen is drawn. Any fault under it
//! hands the screen back to the game for the rest of the session. x86 only.
//!
//! "Change map" opens the map browser (`browserview`) in place of the screen's content.

use core::cell::Cell;
use core::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use w2r_abi::{W2R_E_BUSY, W2R_E_NOT_FOUND, W2R_LOG_INFO, W2R_LOG_WARN, W2R_OK, W2rStatus};

use crate::browserview::{INDEX_SLICE, Theme, View};
use crate::createpick::MapPicker;
use crate::guard::guarded;
use crate::host::Log;
use crate::mapbrowser::{Browser, Source};
use crate::mappreview::tileset_for_key;
use crate::previewdraw::{self, PreviewGame, Request};
use crate::replaypicker::Picker;
use crate::screenkit::game::{Kit, str_data};
use crate::screenkit::*;
use crate::symbols::Symbols;
use crate::ui::Widgets;

/// Where things sit in the game's map-list entry (wc2r-plus `struct_offsets.h`).
const ENTRY_PLAYERS: usize = 0x04;
const ENTRY_DIM: usize = 0x06;
const ENTRY_NAME: usize = 0x08;
const ENTRY_PATH: usize = 0x20;
const ENTRY_DESC: usize = 0x50;
const PASSWORD_CAPACITY: i32 = 0x18;
const MAP_PANEL_FRACTION: f32 = 0.47;
const PANEL_GUTTER_FRACTION: f32 = 0.03;

type CreateButtonFn = unsafe extern "thiscall" fn(*mut c_void);
type PreviousStateFn = unsafe extern "C" fn() -> u16;
type TransitionFn = unsafe extern "C" fn(i32);

/// One of the four option dropdowns: its caption key, combo id and the game's own objects.
struct Option4 {
    caption: &'static CStr,
    id: &'static CStr,
    current: *mut c_void,
    options: *const c_void,
    open: *mut u8,
}

struct Screen {
    kit: Kit,
    create_button: CreateButtonFn,
    previous_state: PreviousStateFn,
    transition: TransitionFn,
    skin_screen: *const u32,
    skin_left: *const u32,
    skin_right: *const u32,
    skin_text_edit: *const u32,
    font_heading: *const u32,
    selected: *const u8,
    ready_flag: *mut u8,
    game_speed: *mut u32,
    name_buffer: *const *mut u8,
    name_capacity: *const u32,
    password: *mut u8,
    options: [Option4; 4],
    view: View,
    browser: Mutex<Browser>,
    picker: Mutex<Picker>,
    preview: PreviewGame,
    /// Presses of the name button; each one moves on to the next suggestion.
    name_presses: Cell<usize>,
    /// The map the name was last considered for, and the name this screen put in the box for it.
    auto_name: Mutex<AutoName>,
}

#[derive(Default)]
struct AutoName {
    map: String,
    filled: String,
}

// SAFETY: addresses of the game's functions and globals, fixed for the session; the screen is
// only used from the game's main thread.
unsafe impl Send for Screen {}
unsafe impl Sync for Screen {}

static SCREEN: OnceLock<Option<Screen>> = OnceLock::new();
/// A fault under the replacement draw: stock screen from here on.
static FAILED: AtomicBool = AtomicBool::new(false);
/// The `auto_game_name` setting: suggest a game name when the map changes.
static AUTO_GAME_NAME: AtomicBool = AtomicBool::new(false);
static ANNOUNCED: AtomicBool = AtomicBool::new(false);
/// When the replacement screen last drew; a longer gap means a fresh visit, and the browser from
/// the last one is closed (wc2r-plus `kReentryGapMs`).
static LAST_FRAME_MS: AtomicU64 = AtomicU64::new(0);
const REENTRY_GAP_MS: u64 = 250;

pub fn set_auto_game_name(on: bool) {
    AUTO_GAME_NAME.store(on, Ordering::Relaxed);
}

/// Resolves everything the screen needs. False (logged by name) when anything is missing; the
/// stock screen is then all that is ever drawn.
pub fn install(symbols: &Symbols, log: &dyn Log) -> bool {
    match resolve(symbols) {
        Ok(screen) => {
            let _ = SCREEN.set(Some(screen));
            log.line(
                W2R_LOG_INFO,
                "host",
                "create screen: replacement ready (off until enhanced_create_screen is on)",
            );
            true
        }
        Err(name) => {
            let _ = SCREEN.set(None);
            log.line(
                W2R_LOG_WARN,
                "host",
                &format!("create screen: symbol {name} missing; the stock screen stays"),
            );
            false
        }
    }
}

fn resolve(symbols: &Symbols) -> Result<Screen, &'static str> {
    let at = |name: &'static str| symbols.address(name).ok_or(name);
    let function = |name: &'static str| at(name);
    let option = |caption, id, cur, opts, open| -> Result<Option4, &'static str> {
        Ok(Option4 {
            caption,
            id,
            current: at(cur)? as *mut c_void,
            options: at(opts)? as *const c_void,
            open: at(open)? as *mut u8,
        })
    };
    let kit = Kit::new(symbols).ok_or("a screen-kit symbol")?;
    // SAFETY: each pointer type is the symbol table's `c_type` for that name.
    Ok(Screen {
        kit,
        create_button: unsafe {
            core::mem::transmute::<usize, CreateButtonFn>(function("LobbyCreateButton")?)
        },
        previous_state: unsafe {
            core::mem::transmute::<usize, PreviousStateFn>(function("PreviousGameState")?)
        },
        transition: unsafe {
            core::mem::transmute::<usize, TransitionFn>(function("TransitionGameState")?)
        },
        skin_screen: at("SkinLobbyCreation")? as *const u32,
        skin_left: at("SkinLobbyCreationLeft")? as *const u32,
        skin_right: at("SkinLobbyCreationRight")? as *const u32,
        skin_text_edit: at("SkinTextEdit")? as *const u32,
        font_heading: at("FontLobbyCreationHeading")? as *const u32,
        selected: at("LobbyCreationSelected")? as *const u8,
        ready_flag: at("LobbyCreationReady")? as *mut u8,
        game_speed: at("LobbyGameSpeed")? as *mut u32,
        name_buffer: at("LobbyNameBuffer")? as *const *mut u8,
        name_capacity: at("LobbyNameCapacity")? as *const u32,
        password: at("LobbyPassword")? as *mut u8,
        options: [
            option(
                c"customscenario_resources",
                c"resources",
                "LobbyResourcesCurrent",
                "LobbyResourcesOptions",
                "LobbyComboOpenResources",
            )?,
            option(
                c"customscenario_units",
                c"units",
                "LobbyUnitsCurrent",
                "LobbyUnitsOptions",
                "LobbyComboOpenUnits",
            )?,
            option(
                c"customscenario_tileset",
                c"tileset",
                "LobbyTilesetCurrent",
                "LobbyTilesetOptions",
                "LobbyComboOpenTileset",
            )?,
            option(
                c"customscenario_starting_location",
                c"startingLocation",
                "LobbyStartLocCurrent",
                "LobbyStartLocOptions",
                "LobbyComboOpenStartLoc",
            )?,
        ],
        view: View::new(symbols)?,
        browser: Mutex::new(Browser::default()),
        picker: Mutex::new(Picker::default()),
        preview: PreviewGame::new(symbols)?,
        name_presses: Cell::new(0),
        auto_name: Mutex::new(AutoName::default()),
    })
}

/// Draws the replacement screen if it is on and usable. True when it drew (the caller must then
/// not draw the stock one); false leaves the frame to the game.
pub fn try_draw() -> bool {
    if FAILED.load(Ordering::Relaxed) || !crate::session::enhanced_create_screen() {
        return false;
    }
    let Some(Some(screen)) = SCREEN.get() else {
        return false;
    };
    let Some(window) = screen.kit.window_rect() else {
        return false;
    };
    let now = crate::chat::now_ms();
    let last = LAST_FRAME_MS.swap(now, Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) > REENTRY_GAP_MS {
        screen.browser().close();
        let mut picker = screen.picker();
        picker.close();
        picker.forget_auto();
    }
    let drawn = guarded(|| {
        let request = unsafe { screen.draw(window) };
        // The picture goes straight onto the window's canvas, after the layout space has ended.
        if let Some(r) = request {
            previewdraw::draw(&screen.preview, &screen.kit, &r);
        }
    });
    match drawn {
        Ok(()) => {
            if !ANNOUNCED.swap(true, Ordering::Relaxed) {
                crate::session::log_info(
                    "createscreen",
                    &format!(
                        "drawing the replacement screen (window {:.0}x{:.0})",
                        window.w, window.h
                    ),
                );
            }
        }
        Err(fault) => {
            FAILED.store(true, Ordering::Relaxed);
            crate::session::host_fault("create screen", &fault);
            crate::session::log_error(
                "createscreen",
                "the replacement screen faulted; the stock screen is used from now on",
            );
        }
    }
    true
}

/// Height reserved under the Change map button for the plugins' rows: a slider with its caption
/// and a button. Rows that do not fit are not drawn.
const PLUGIN_AREA_H: f32 = CAPTION_H + ROW_H + GAP + ROW_H + GAP;

/// The plugins' rows: placed one under the other in the map column, in the screen's own style.
struct PluginRows<'a> {
    kit: &'a Kit,
    area: Rect,
    y: Cell<f32>,
}

impl<'a> PluginRows<'a> {
    fn new(kit: &'a Kit, area: Rect) -> PluginRows<'a> {
        PluginRows {
            kit,
            y: Cell::new(area.y),
            area,
        }
    }

    /// The next `h`-high row, or none when the area is used up.
    fn next(&self, h: f32) -> Option<Rect> {
        let y = self.y.get();
        if y + h > self.area.y + self.area.h {
            return None;
        }
        self.y.set(y + h);
        Some(Rect::new(self.area.x, y, self.area.w, h))
    }

    fn gap(&self) {
        self.y.set(self.y.get() + GAP);
    }
}

impl Widgets for PluginRows<'_> {
    fn row(&self, _height: f32, _cols: i32) {}

    fn label(&self, text: &str) {
        if let Some(r) = self.next(CAPTION_H) {
            self.kit.label_at(r, text, ALIGN_LEFT);
        }
    }

    fn button(&self, text: &str) -> bool {
        let pressed = self
            .next(ROW_H)
            .is_some_and(|r| self.kit.button_at(r, text, false));
        self.gap();
        pressed
    }

    /// Not offered on this screen: the text is shown and the value stays.
    fn checkbox(&self, text: &str, _value: &mut bool) -> bool {
        self.label(text);
        false
    }

    fn slider(&self, text: &str, value: &mut i32, min: i32, max: i32) -> bool {
        let (Some(caption), Some(bar)) = (self.next(CAPTION_H), self.next(ROW_H)) else {
            return false;
        };
        self.kit.label_at(caption, text, ALIGN_LEFT);
        self.kit.set_rect(bar);
        let mut offset = (*value).clamp(min, max).wrapping_sub(min) as u32;
        let changed = unsafe { self.kit.slider(&mut offset, 0, (max - min) as u32) };
        if changed {
            *value = min.saturating_add(offset as i32).min(max);
        }
        self.gap();
        changed
    }
}

/// The host's map index, as the plugins' `create_map_count` and `create_map_select` see it.
struct GamePicker<'a> {
    screen: &'a Screen,
}

impl MapPicker for GamePicker<'_> {
    fn count(&mut self, players: u32) -> Result<u32, W2rStatus> {
        let mut browser = self.screen.browser();
        let mut src = self.screen.view.source;
        if let Some(root) = crate::mapdownload::maps_root_string() {
            browser.set_root(&root);
        }
        browser.observe_root(&src.rel_path());
        if !browser.index_ready() {
            // A slice of the walk per call (one call a frame), as the browser's search does.
            browser.step_index(&mut src, Some(INDEX_SLICE));
        }
        if !browser.index_ready() {
            return Err(W2R_E_BUSY);
        }
        Ok(browser.with_players(players).count() as u32)
    }

    fn select(&mut self, players: u32, nth: u32) -> W2rStatus {
        let mut browser = self.screen.browser();
        let Some(map) = browser.with_players(players).nth(nth as usize).cloned() else {
            return W2R_E_NOT_FOUND;
        };
        if self.screen.view.select_indexed(&mut browser, &map) {
            W2R_OK
        } else {
            W2R_E_NOT_FOUND
        }
    }
}

impl Screen {
    fn browser(&self) -> std::sync::MutexGuard<'_, Browser> {
        self.browser.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn picker(&self) -> std::sync::MutexGuard<'_, Picker> {
        self.picker.lock().unwrap_or_else(|e| e.into_inner())
    }

    unsafe fn draw(&self, window: Rect) -> Option<Request> {
        let k = &self.kit;
        let previous = k.push_skin(unsafe { *self.skin_screen });
        k.space_begin();
        {
            let mut picker = self.picker();
            // A queued replay's map becomes the selected one.
            if !picker.is_open() && !self.browser().is_open() {
                let mut browser = self.browser();
                picker.auto_select(&mut browser, &self.view);
            }
            if picker.is_open() {
                let theme = Theme {
                    right_frame: unsafe { *self.skin_right },
                    left_frame: unsafe { *self.skin_left },
                    text_edit: unsafe { *self.skin_text_edit },
                    heading_font: unsafe { *self.font_heading },
                };
                let mut browser = self.browser();
                let shown =
                    unsafe { picker.draw(k, &mut browser, &self.view, window, theme) };
                k.space_end();
                k.pop_skin(previous);
                return shown.map(|(area, path, heat)| Request {
                    area,
                    path,
                    tileset: None,
                    heat,
                });
            }
        }
        {
            let mut browser = self.browser();
            if browser.is_open() {
                let theme = Theme {
                    right_frame: unsafe { *self.skin_right },
                    left_frame: unsafe { *self.skin_left },
                    text_edit: unsafe { *self.skin_text_edit },
                    heading_font: unsafe { *self.font_heading },
                };
                let shown = unsafe { self.view.draw(k, &mut browser, window, theme) };
                k.space_end();
                k.pop_skin(previous);
                return shown.map(|(area, path)| Request {
                    area,
                    path,
                    tileset: None,
                    heat: None,
                });
            }
        }

        k.banner();
        k.label(&k.localized(c"mp_create_game"), ALIGN_CENTRE);

        let frame = lobby_frame(window);
        k.panel_frame(frame, c"lobby_panel", Some(unsafe { *self.skin_left }));

        let content_x = frame.x + FRAME_PAD;
        let content_right = frame.x + frame.w - FRAME_PAD;
        let content_w = content_right - content_x;
        let content_y = frame.y + FRAME_PAD;
        let footer_y = window.h - FOOTER_BOTTOM_GAP;
        let content_h = (footer_y - GAP) - content_y;

        let map_w = fl(content_w * MAP_PANEL_FRACTION);
        let gutter = fl(content_w * PANEL_GUTTER_FRACTION);
        let right_x = content_x + map_w + gutter;
        let right_w = content_right - right_x;
        let options_h = fl(CAPTION_H + ROW_H + GAP) * 4.0;

        let preview = self.draw_map_column(Rect::new(content_x, content_y, map_w, content_h));
        unsafe { self.draw_options(Rect::new(right_x, content_y, right_w, options_h)) };
        let details_y = content_y + options_h + GAP;
        unsafe {
            self.draw_details(Rect::new(
                right_x,
                details_y,
                right_w,
                (content_y + content_h) - details_y,
            ))
        };
        unsafe { self.draw_footer(window, footer_y) };

        k.space_end();
        k.pop_skin(previous);
        preview
    }

    /// With the setting on, a new map (or the first one of a visit) gets a suggested game name if
    /// the box is empty or still holds the name this put there for the map before.
    ///
    /// # Safety
    /// `buffer` must be the game's NUL-terminated name buffer of `max` bytes.
    unsafe fn auto_fill_name(&self, buffer: *mut u8, max: i32) {
        let map = unsafe { self.selected_name() };
        let mut state = self.auto_name.lock().unwrap_or_else(|e| e.into_inner());
        if map.is_empty() || state.map == map {
            return;
        }
        // SAFETY: the buffer is NUL-terminated within its capacity.
        let current = unsafe { CStr::from_ptr(buffer.cast()) }.to_string_lossy().into_owned();
        let cap = max.max(0) as usize;
        if cap > 0 && (current.is_empty() || current == state.filled) {
            let suggestion = game_name_from_map(&map, 0);
            let len = suggestion.len().min(cap - 1);
            // SAFETY: `cap` bytes are writable; the NUL fits after `len`.
            unsafe {
                core::ptr::copy_nonoverlapping(suggestion.as_ptr(), buffer, len);
                *buffer.add(len) = 0;
            }
            state.filled = suggestion;
            self.name_presses.set(1);
        }
        state.map = map;
    }

    unsafe fn selected_name(&self) -> String {
        unsafe { str_data(self.selected.add(ENTRY_NAME)) }
    }

    /// The name, the picture's square and the Change map button. Returns what to paint.
    fn draw_map_column(&self, column: Rect) -> Option<Request> {
        let k = &self.kit;
        let name = unsafe { self.selected_name() };
        k.heading_at(
            Rect::new(column.x, column.y, column.w, NAME_H),
            if name.is_empty() {
                "No map selected"
            } else {
                &name
            },
            ALIGN_CENTRE,
            Some(unsafe { *self.font_heading }),
        );
        // Room under the Change map button for the plugins' rows, only when one wants them.
        let plugin_h = if crate::session::create_ui_present() {
            PLUGIN_AREA_H
        } else {
            0.0
        };
        let b = preview_box(Rect::new(
            column.x,
            column.y,
            column.w,
            (column.h - plugin_h).max(0.0),
        ));
        if plugin_h > 0.0 {
            let top = b.y + b.h + GAP + ROW_H + GAP;
            // The plugins' rows use the whole column, not just the picture's width.
            let area = Rect::new(
                column.x,
                top,
                column.w,
                (column.y + column.h - top).max(0.0),
            );
            let widgets = PluginRows::new(k, area);
            crate::session::draw_create_ui(&widgets, &mut GamePicker { screen: self });
        }
        // Change map and the replay picker share a row across the whole column, half each; the
        // picker's label says when a replay is queued, in words when there is room for them.
        let half = fl((column.w - GAP) * 0.5);
        let row_y = b.y + b.h + GAP;
        if k.button_at(Rect::new(column.x, row_y, half, ROW_H), "Change map", false) {
            let selected = self.view.selected_choice();
            self.browser().open(Some(selected));
        }
        let queued = crate::replayplay::queued().is_some();
        // About 36 units a character in this font (2160-high space).
        let roomy = half >= 15.0 * 36.0;
        let replay_label = match (queued, roomy) {
            (false, true) => "Watch a replay",
            (false, false) => "Replays",
            (true, true) => "Replay queued",
            (true, false) => "Replay set",
        };
        if k.button_at(
            Rect::new(column.x + half + GAP, row_y, column.w - half - GAP, ROW_H),
            replay_label,
            false,
        ) {
            self.picker().open();
        }
        let path = unsafe { str_data(self.selected.add(ENTRY_PATH)) };
        let tileset = tileset_for_key(&unsafe { str_data(self.options[2].current as *const u8) });
        (!path.is_empty()).then_some(Request {
            area: b,
            path,
            tileset,
            heat: None,
        })
    }

    unsafe fn draw_options(&self, column: Rect) {
        let k = &self.kit;
        let col_w = (column.w - GAP) * 0.5;
        let cell_h = column.h / 4.0;
        let right_x = column.x + col_w + GAP;
        let text_skin = Some(unsafe { *self.skin_text_edit });

        k.label_at(
            Rect::new(column.x, column.y, col_w, CAPTION_H),
            &k.localized(c"mp_lobby_name"),
            ALIGN_CAPTION,
        );
        let buffer = unsafe { *self.name_buffer };
        // Tab walks the keyboard through the text fields: each one's id and the text it holds.
        let mut fields: Vec<(u32, *const u8)> = Vec::with_capacity(2);
        if !buffer.is_null() {
            let max = (unsafe { *self.name_capacity } & 0x7fff_ffff) as i32;
            // A queued replay names the game and sets a password, once per visit.
            unsafe {
                self.picker().fill_lobby_text(
                    buffer,
                    max.max(0) as usize,
                    self.password,
                    PASSWORD_CAPACITY as usize,
                )
            };
            if AUTO_GAME_NAME.load(Ordering::Relaxed) {
                unsafe { self.auto_fill_name(buffer, max) };
            }
            let button_w = ROW_H * 1.6;
            fields.push((k.edit_seq(), buffer));
            unsafe {
                k.text_edit_at(
                    Rect::new(column.x, column.y + CAPTION_H, col_w - button_w - GAP * 0.5, ROW_H),
                    buffer,
                    max,
                    text_skin,
                )
            };
            let button = Rect::new(
                column.x + col_w - button_w,
                column.y + CAPTION_H,
                button_w,
                ROW_H,
            );
            if k.button_at(button, "Auto", false) {
                let n = self.name_presses.get();
                self.name_presses.set(n.wrapping_add(1));
                let map = unsafe { self.selected_name() };
                let suggestion = game_name_from_map(&map, n);
                let cap = max.max(0) as usize;
                if !suggestion.is_empty() && cap > 0 {
                    let len = suggestion.len().min(cap - 1);
                    // SAFETY: the game's name buffer holds `cap` bytes; the NUL fits after `len`.
                    unsafe {
                        core::ptr::copy_nonoverlapping(suggestion.as_ptr(), buffer, len);
                        *buffer.add(len) = 0;
                    }
                }
            }
        }

        k.label_at(
            Rect::new(right_x, column.y, col_w, CAPTION_H),
            "Password (Optional)",
            ALIGN_CAPTION,
        );
        fields.push((k.edit_seq(), self.password));
        unsafe {
            k.text_edit_at(
                Rect::new(right_x, column.y + CAPTION_H, col_w, ROW_H),
                self.password,
                PASSWORD_CAPACITY,
                text_skin,
            )
        };
        let lengths: Vec<(u32, usize)> = fields
            .iter()
            // SAFETY: both are the NUL-terminated buffers just edited.
            .map(|&(id, text)| (id, unsafe { core::ffi::CStr::from_ptr(text.cast()) }.to_bytes().len()))
            .collect();
        k.tab_between(&lengths);

        for (i, d) in self.options.iter().enumerate() {
            let x = column.x + (i % 2) as f32 * (col_w + GAP);
            let y = column.y + (1 + i / 2) as f32 * cell_h;
            k.label_at(
                Rect::new(x, y, col_w, CAPTION_H),
                &k.localized(d.caption),
                ALIGN_CAPTION,
            );
            k.set_rect(Rect::new(x, y + CAPTION_H, col_w, ROW_H));
            unsafe { k.dropdown(d.id, d.current, d.options, d.open) };
        }

        let row4 = column.y + 3.0 * cell_h;
        k.label_at(
            Rect::new(column.x, row4, col_w, CAPTION_H),
            &k.localized(c"option_speed_game_speed"),
            ALIGN_CAPTION,
        );
        k.set_rect(Rect::new(column.x, row4 + CAPTION_H, col_w, ROW_H));
        unsafe { k.slider(self.game_speed, 0, 8) };

        // The lock choice (docs/lobby-policy.md): host.json, applied when hosting.
        let (teams, slots) = crate::session::lock_choice().unwrap_or((false, false));
        let lock_w = (col_w - GAP) * 0.5;
        let slots_x = right_x + lock_w + GAP;
        k.label_at(
            Rect::new(right_x, row4, lock_w, CAPTION_H),
            "Teams",
            ALIGN_CAPTION,
        );
        k.label_at(
            Rect::new(slots_x, row4, lock_w, CAPTION_H),
            "Slots",
            ALIGN_CAPTION,
        );
        let state = |locked| if locked { "Locked" } else { "Unlocked" };
        let flip_teams = k.button_at(
            Rect::new(right_x, row4 + CAPTION_H, lock_w, ROW_H),
            state(teams),
            false,
        );
        let flip_slots = k.button_at(
            Rect::new(slots_x, row4 + CAPTION_H, lock_w, ROW_H),
            state(slots),
            false,
        );
        if flip_teams || flip_slots {
            crate::session::set_lock_choice(teams ^ flip_teams, slots ^ flip_slots);
        }
    }

    /// Size, player count and description of the selected map (wc2r-plus `DrawDetails`).
    unsafe fn draw_details(&self, r: Rect) {
        let k = &self.kit;
        let name = unsafe { self.selected_name() };
        if name.is_empty() {
            return;
        }
        let dim = unsafe { self.selected.add(ENTRY_DIM).cast::<u16>().read_unaligned() };
        let players = unsafe { *self.selected.add(ENTRY_PLAYERS) };
        let mut y = r.y;
        k.label_at(
            Rect::new(r.x, y, r.w, CAPTION_H),
            &format!("{dim} x {dim}"),
            ALIGN_LEFT,
        );
        y += CAPTION_H;
        k.label_at(
            Rect::new(r.x, y, r.w, CAPTION_H),
            &k.format_localized(c"customscenario_maxplayers", &players.to_string()),
            ALIGN_LEFT,
        );
        y += CAPTION_H + GAP;
        let desc = unsafe { str_data(self.selected.add(ENTRY_DESC)) };
        if !desc.is_empty() && desc != name && y + CAPTION_H < r.y + r.h {
            k.wrapped_label_at(Rect::new(r.x, y, r.w, (r.y + r.h) - y), &desc);
        }
    }

    unsafe fn draw_footer(&self, window: Rect, footer_y: f32) {
        let k = &self.kit;
        let button_w = footer_button_width(window);
        let x = fl((window.w - button_w * 2.0) * 0.5);
        if k.button_at(Rect::new(x, footer_y, button_w, FOOTER_H), "Cancel", false) {
            unsafe {
                *self.ready_flag = 0;
                (self.transition)((self.previous_state)() as i32);
            }
            return;
        }
        // The game's Create button takes a closure whose first field is the exit flag.
        let mut leave: u8 = 0;
        let mut closure: *mut u8 = &mut leave;
        k.set_rect(Rect::new(x + button_w, footer_y, button_w, FOOTER_H));
        unsafe {
            (self.create_button)(&mut closure as *mut *mut u8 as *mut c_void);
        }
    }
}

/// Fewest characters a suggested game name has; shorter ones tend to get padded with spaces.
const MIN_GAME_NAME: usize = 5;

/// The `n`th suggestion for a game name from the map's name: a word of it, longest first
/// ("Bridge to Bridge Combat" gives "combat" and "bridge"), then the initials ("gow" for "Garden
/// of War") with a number so the name reaches [`MIN_GAME_NAME`] characters. Pressing again moves
/// to the next one, which also sidesteps a name another lobby already uses.
fn game_name_from_map(map: &str, n: usize) -> String {
    let words: Vec<String> = map
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
    let mut bases: Vec<String> = Vec::new();
    let mut long: Vec<&String> = words.iter().filter(|w| w.len() >= MIN_GAME_NAME).collect();
    long.sort_by_key(|w| core::cmp::Reverse(w.len()));
    for w in long {
        if !bases.contains(w) {
            bases.push(w.clone());
        }
    }
    if initials.len() >= MIN_GAME_NAME && !bases.contains(&initials) {
        bases.push(initials.clone());
    }
    if let Some(base) = bases.get(n) {
        return base.clone();
    }
    let prefix = if initials.is_empty() { "game" } else { initials.as_str() };
    let width = MIN_GAME_NAME.saturating_sub(prefix.len()).max(2);
    format!("{prefix}{:0width$}", n - bases.len() + 1)
}

#[cfg(test)]
mod name_tests {
    use super::game_name_from_map;

    #[test]
    fn word_then_initials_with_number() {
        assert_eq!(game_name_from_map("Garden of War", 0), "garden");
        assert_eq!(game_name_from_map("Garden of War", 1), "gow01");
        assert_eq!(game_name_from_map("Garden of War", 2), "gow02");
    }

    #[test]
    fn longest_word_first_and_unique() {
        assert_eq!(game_name_from_map("Bridge to Bridge Combat", 0), "bridge");
        assert_eq!(game_name_from_map("Bridge to Bridge Combat", 1), "combat");
        assert_eq!(game_name_from_map("Bridge to Bridge Combat", 2), "btbc01");
    }

    #[test]
    fn never_short() {
        for map in ["", "A B", "Ice", "Plains of Snow", "!!!"] {
            for n in 0..30 {
                assert!(game_name_from_map(map, n).len() >= 5, "{map:?} {n}");
            }
        }
    }
}
