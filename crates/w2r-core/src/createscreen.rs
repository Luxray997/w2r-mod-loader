//! The replacement Create Multiplayer Game screen (docs/create-screen.md, slice 1), ported from
//! wc2r-plus `features/ui/lobby_creation_screen.cpp`. When the `enhanced_create_screen` setting
//! is on and everything it needs resolved, [`try_draw`] draws it in place of the game's own
//! content (inside the same window); otherwise the stock screen is drawn. Any fault under it
//! hands the screen back to the game for the rest of the session. x86 only.
//!
//! This slice has no map browser: the map column shows the map the game selected, and "Change
//! map" is greyed out until the browser lands.

use core::ffi::{CStr, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use w2r_abi::{W2R_LOG_INFO, W2R_LOG_WARN};

use crate::guard::guarded;
use crate::host::Log;
use crate::screenkit::game::{Kit, str_data};
use crate::screenkit::*;
use crate::symbols::Symbols;

/// Where things sit in the game's map-list entry (wc2r-plus `struct_offsets.h`).
const ENTRY_PLAYERS: usize = 0x04;
const ENTRY_DIM: usize = 0x06;
const ENTRY_NAME: usize = 0x08;
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
    skin_text_edit: *const u32,
    font_heading: *const u32,
    selected: *const u8,
    ready_flag: *mut u8,
    game_speed: *mut u32,
    name_buffer: *const *mut u8,
    name_capacity: *const u32,
    password: *mut u8,
    options: [Option4; 4],
}

// SAFETY: addresses of the game's functions and globals, fixed for the session; the screen is
// only used from the game's main thread.
unsafe impl Send for Screen {}
unsafe impl Sync for Screen {}

static SCREEN: OnceLock<Option<Screen>> = OnceLock::new();
/// A fault under the replacement draw: stock screen from here on.
static FAILED: AtomicBool = AtomicBool::new(false);
static ANNOUNCED: AtomicBool = AtomicBool::new(false);

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
    match guarded(|| unsafe { screen.draw(window) }) {
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

impl Screen {
    unsafe fn draw(&self, window: Rect) {
        let k = &self.kit;
        let previous = k.push_skin(unsafe { *self.skin_screen });
        k.space_begin();

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

        self.draw_map_column(Rect::new(content_x, content_y, map_w, content_h));
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
    }

    unsafe fn selected_name(&self) -> String {
        unsafe { str_data(self.selected.add(ENTRY_NAME)) }
    }

    fn draw_map_column(&self, column: Rect) {
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
        let b = preview_box(column);
        // The map browser is slice 2; until then the map the game selected is the map.
        k.button_at(
            Rect::new(b.x, b.y + b.h + GAP, b.w, ROW_H),
            "Change map",
            true,
        );
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
        if !buffer.is_null() {
            let max = (unsafe { *self.name_capacity } & 0x7fff_ffff) as i32;
            unsafe {
                k.text_edit_at(
                    Rect::new(column.x, column.y + CAPTION_H, col_w, ROW_H),
                    buffer,
                    max,
                    text_skin,
                )
            };
        }

        k.label_at(
            Rect::new(right_x, column.y, col_w, CAPTION_H),
            "Password (Optional)",
            ALIGN_CAPTION,
        );
        unsafe {
            k.text_edit_at(
                Rect::new(right_x, column.y + CAPTION_H, col_w, ROW_H),
                self.password,
                PASSWORD_CAPACITY,
                text_skin,
            )
        };

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
