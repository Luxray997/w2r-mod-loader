//! The replacement multiplayer lobby screen (docs/lobby-screen.md, slice 1), ported from wc2r-plus
//! `features/ui/multiplayer_lobby_screen.cpp` and `lobby_chat.cpp`. The game draws a window named
//! `multiplayer_lobby_screen` every frame; when the `enhanced_lobby_screen` setting is on and
//! everything resolved, [`try_draw`] draws our content inside that window, ends it, and the frame
//! hook tells the game the window is closed so its own content is skipped. Any fault hands the
//! lobby back to the game for the rest of the session. x86 only.
//!
//! Never begins a Nuklear window of its own: that froze the lobby once (PLAN.md, the banner).

use core::ffi::{CStr, c_char, c_void};
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO, W2R_LOG_WARN};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::lobbylayout::{self as geo, SLOT_COUNT, ScrollTracker};
use crate::mappreview::tileset_for_key;
use crate::pe::Image;
use crate::previewdraw::{self, PreviewGame, Request};
use crate::screenkit::game::{Kit, str_data};
use crate::screenkit::*;
use crate::symbols::Symbols;

/// `NetSession`: the "I am the host" byte.
const SESSION_IS_HOST: usize = 0x18;
/// MSVC `std::string` as the game lays it out in a vector: stride, size at +0x10, capacity at
/// +0x14, inline up to 15 bytes.
const STR_STRIDE: usize = 0x18;
const STR_SIZE: usize = 0x10;
const STR_CAPACITY: usize = 0x14;
const STR_SSO_MAX: u32 = 15;
/// The chat history's inset inside its frame, and the most lines drawn (the newest).
const CHAT_INSET_X: f32 = 60.0;
const CHAT_INSET_TOP: f32 = 20.0;
const CHAT_MAX_LINES: usize = 500;
const KEY_ENTER: i32 = 4;
/// Window flags: a group with no scrollbar.
const NO_SCROLLBAR: u32 = 0x20;

type SlotRowFn = unsafe extern "C" fn(i32, *const f32, f32, f32);
type VoidFn = unsafe extern "C" fn();
type MaxPlayersFn = unsafe extern "C" fn() -> u8;
type PopupFn = unsafe extern "thiscall" fn(*mut c_void);
type LocalizedFn = unsafe extern "C" fn(*const c_char) -> *const c_char;

struct Screen {
    kit: Kit,
    slot_row: SlotRowFn,
    leave_button: VoidFn,
    start_button: VoidFn,
    send_chat: VoidFn,
    max_players: MaxPlayersFn,
    draw_popup: PopupFn,
    popup: *mut c_void,
    skin_popup: *const u32,
    skin_left: *const u32,
    skin_chat: *const u32,
    skin_chat_text: *const u32,
    skin_text_edit: *const u32,
    font_heading: *const u32,
    game_name: *const u8,
    map_name: *const u8,
    player_count: *const u8,
    countdown: *const u8,
    enter_ms: *const i64,
    chat_begin: *const *const u8,
    chat_end: *const *const u8,
    chat_input: *mut u8,
    chat_scroll_request: *mut u8,
    net_session: *const *const u8,
    team_options: *const *const *const c_char,
    resource_keys: *const *const *const c_char,
    tileset_keys: *const *const *const c_char,
    match_flags: *const u32,
    fixed_start: *const u8,
    game_speed: *const u8,
    selected_map_path: *const u8,
    preview: PreviewGame,
}

// SAFETY: addresses of the game's functions and globals, fixed for the session; the screen is
// only used from the game's main thread.
unsafe impl Send for Screen {}
unsafe impl Sync for Screen {}

#[derive(Default)]
struct ChatState {
    scroll: ScrollTracker,
    wrap_w: f32,
    wipe_logged: bool,
}

static SCREEN: OnceLock<Option<Screen>> = OnceLock::new();
static CHAT: Mutex<ChatState> = Mutex::new(ChatState {
    scroll: ScrollTracker::new(),
    wrap_w: 0.0,
    wipe_logged: false,
});
/// A fault under the replacement draw: the game's lobby from here on.
static FAILED: AtomicBool = AtomicBool::new(false);
static ANNOUNCED: AtomicBool = AtomicBool::new(false);

// The team dropdown shows the game's words; ours are digits, but only while our slots panel
// draws, so nothing else that looks a key up is touched.
static REAL_LOCALIZED: AtomicUsize = AtomicUsize::new(0);
static REWRITE_TEAMS: AtomicBool = AtomicBool::new(false);
static TEAM_OPTIONS: AtomicUsize = AtomicUsize::new(0);
const MAX_TEAM_KEYS: usize = 64;

/// Resolves everything the screen needs and hooks the localized-string lookup. False (logged by
/// name) when anything is missing; the game's lobby is then all that is ever drawn.
pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    match resolve(symbols) {
        Ok(screen) => {
            TEAM_OPTIONS.store(screen.team_options as usize, Ordering::Release);
            let _ = SCREEN.set(Some(screen));
            let hooked = hook_localized(image, symbols, log);
            log.line(
                W2R_LOG_INFO,
                "host",
                &format!(
                    "lobby screen: replacement ready (off until enhanced_lobby_screen is on){}",
                    if hooked {
                        ""
                    } else {
                        "; team numbers unavailable"
                    }
                ),
            );
            true
        }
        Err(name) => {
            let _ = SCREEN.set(None);
            log.line(
                W2R_LOG_WARN,
                "host",
                &format!("lobby screen: symbol {name} missing; the game's lobby stays"),
            );
            false
        }
    }
}

fn resolve(symbols: &Symbols) -> Result<Screen, &'static str> {
    let at = |name: &'static str| symbols.address(name).ok_or(name);
    let kit = Kit::new(symbols).ok_or("a screen-kit symbol")?;
    // SAFETY: each pointer type is the symbol table's `c_type` for that name.
    unsafe {
        Ok(Screen {
            kit,
            slot_row: core::mem::transmute::<usize, SlotRowFn>(at("MpLobbySlotRow")?),
            leave_button: core::mem::transmute::<usize, VoidFn>(at("MpLobbyLeaveButton")?),
            start_button: core::mem::transmute::<usize, VoidFn>(at("MpLobbyStartGameButton")?),
            send_chat: core::mem::transmute::<usize, VoidFn>(at("MpLobbySendChat")?),
            max_players: core::mem::transmute::<usize, MaxPlayersFn>(at("MpLobbyMaxPlayers")?),
            draw_popup: core::mem::transmute::<usize, PopupFn>(at("DrawNkPopup")?),
            popup: at("MpLobbyPopup")? as *mut c_void,
            skin_popup: at("SkinMpLobbyPopup")? as *const u32,
            skin_left: at("SkinLobbyCreationLeft")? as *const u32,
            skin_chat: at("SkinMultiplayerLobbyChat")? as *const u32,
            skin_chat_text: at("SkinMultiplayerLobbyText")? as *const u32,
            skin_text_edit: at("SkinTextEdit")? as *const u32,
            font_heading: at("FontMultiplayerLobbyHeading")? as *const u32,
            game_name: at("MpLobbyGameName")? as *const u8,
            map_name: at("MpLobbyMapNameText")? as *const u8,
            player_count: at("MpLobbyPlayerCount")? as *const u8,
            countdown: at("MpLobbyCountdownActive")? as *const u8,
            enter_ms: at("MpLobbyEnterMs")? as *const i64,
            chat_begin: at("MpLobbyChatBegin")? as *const *const u8,
            chat_end: at("MpLobbyChatEnd")? as *const *const u8,
            chat_input: at("MpLobbyChatInput")? as *mut u8,
            chat_scroll_request: symbols
                .address("MpLobbyChatScrollRequest")
                .map_or(core::ptr::null_mut(), |a| a as *mut u8),
            net_session: at("NetSession")? as *const *const u8,
            team_options: at("MpLobbyTeamOptions")? as *const *const *const c_char,
            resource_keys: at("MpLobbyResourceKeys")? as *const *const *const c_char,
            tileset_keys: at("MpLobbyTilesetKeys")? as *const *const *const c_char,
            match_flags: at("MpLobbyMatchFlags")? as *const u32,
            fixed_start: at("MpLobbyFixedStartLocation")? as *const u8,
            game_speed: at("MpLobbyGameSpeed")? as *const u8,
            selected_map_path: at("SelectedMapPath")? as *const u8,
            preview: PreviewGame::new(symbols)?,
        })
    }
}

fn hook_localized(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(target) = symbols.address("LookupLocalized") else {
        return false;
    };
    // SAFETY: `localized_detour` matches LookupLocalized's cdecl signature.
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            localized_detour as *mut c_void,
            "host:LookupLocalized (lobby team numbers)",
            |original| REAL_LOCALIZED.store(original as usize, Ordering::Release),
        )
    };
    if let Err(ref e) = result {
        log.line(
            W2R_LOG_ERROR,
            "host",
            &format!("could not hook LookupLocalized: {e:?}"),
        );
    }
    result.is_ok()
}

unsafe extern "C" fn localized_detour(key: *const c_char) -> *const c_char {
    let original: LocalizedFn =
        unsafe { core::mem::transmute(REAL_LOCALIZED.load(Ordering::Acquire)) };
    if REWRITE_TEAMS.load(Ordering::Relaxed) && !key.is_null() {
        let options = TEAM_OPTIONS.load(Ordering::Acquire) as *const *const *const c_char;
        if !options.is_null() {
            // The option list is a vector of C strings: begin and end pointers.
            let (begin, end) = unsafe { (*options, *options.add(1)) };
            if !begin.is_null() && end >= begin {
                let count = (unsafe { end.offset_from(begin) } as usize).min(MAX_TEAM_KEYS);
                let wanted = unsafe { CStr::from_ptr(key) };
                for index in 0..count {
                    let candidate = unsafe { *begin.add(index) };
                    if candidate.is_null() || unsafe { CStr::from_ptr(candidate) } != wanted {
                        continue;
                    }
                    if let Some(digit) = geo::team_digit(index) {
                        return digit.as_ptr();
                    }
                    break;
                }
            }
        }
    }
    unsafe { original(key) }
}

/// The base of a `std::string`'s character buffer (heap, or inline in the object).
unsafe fn str_buffer(s: *mut u8) -> *mut u8 {
    unsafe {
        let cap = s.add(STR_CAPACITY).cast::<u32>().read_unaligned();
        if cap > STR_SSO_MAX {
            s.cast::<*mut u8>().read_unaligned()
        } else {
            s
        }
    }
}

/// Draws the replacement lobby if it is on and usable, inside the window the game just began.
/// True when it drew and ended the window (the caller must report the window closed so the game
/// skips its own content); false leaves the frame to the game.
pub fn try_draw() -> bool {
    if FAILED.load(Ordering::Relaxed) || !crate::session::enhanced_lobby_screen() {
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
                    "lobbyscreen",
                    &format!(
                        "drawing the replacement lobby (window {:.0}x{:.0})",
                        window.w, window.h
                    ),
                );
            }
            true
        }
        Err(fault) => {
            FAILED.store(true, Ordering::Relaxed);
            REWRITE_TEAMS.store(false, Ordering::Relaxed);
            crate::session::host_fault("lobby screen", &fault);
            crate::session::log_error(
                "lobbyscreen",
                "the replacement lobby faulted; the game's lobby is used from now on",
            );
            false
        }
    }
}

impl Screen {
    fn is_host(&self) -> bool {
        unsafe {
            let session = *self.net_session;
            !session.is_null() && *session.add(SESSION_IS_HOST) != 0
        }
    }

    unsafe fn draw(&self, window: Rect) {
        let k = &self.kit;
        let l = geo::layout(window);
        k.space_begin();

        let countdown = unsafe { *self.countdown } != 0;
        if countdown {
            k.disable_begin();
        }

        k.banner();
        k.label(
            &geo::clean_text(&unsafe { str_data_c(self.game_name) }),
            ALIGN_CENTRE,
        );
        k.panel_frame(l.frame, c"lobby_panel", Some(unsafe { *self.skin_left }));

        unsafe { self.draw_slots(l.slots) };
        unsafe { self.draw_chat(l.chat_history, l.chat_input) };

        let request = unsafe { self.draw_right(l.right) };

        unsafe { self.draw_footer(window, l.footer_y) };

        if countdown {
            k.disable_end();
        }
        k.space_end();

        // The picture goes straight onto the window's canvas, after the layout space has ended.
        if let Some(r) = request {
            previewdraw::draw(&self.preview, k, &r);
        }

        // The game's own popups (kicked, host left, ...) belong to this window.
        let previous = k.push_skin(unsafe { *self.skin_popup });
        unsafe { (self.draw_popup)(self.popup) };
        k.pop_skin(previous);

        k.end_window();
    }

    /// The entry `index` of one of the game's option lists (a vector of C strings), or "".
    unsafe fn key_by_index(&self, list: *const *const *const c_char, index: usize) -> String {
        unsafe {
            let (begin, end) = (*list, *list.add(1));
            if begin.is_null() || end <= begin || index >= end.offset_from(begin) as usize {
                return String::new();
            }
            let key = *begin.add(index);
            if key.is_null() {
                String::new()
            } else {
                CStr::from_ptr(key).to_string_lossy().into_owned()
            }
        }
    }

    /// The map name, its preview's square and the info rows. Returns what to paint.
    unsafe fn draw_right(&self, column: Rect) -> Option<Request> {
        let k = &self.kit;
        let c = geo::right_column(column);
        let map = geo::clean_text(&unsafe { str_data_c(self.map_name) });
        k.heading_at(
            c.name,
            if map.is_empty() { "No map" } else { &map },
            ALIGN_CENTRE,
            Some(unsafe { *self.font_heading }),
        );
        unsafe { self.draw_info(&c) };

        let path = unsafe { str_data_c(self.selected_map_path) };
        let flags = unsafe { *self.match_flags };
        let tileset_key =
            unsafe { self.key_by_index(self.tileset_keys, geo::tileset_index(flags)) };
        (!path.is_empty() && c.preview.w > 0.0).then(|| Request {
            area: c.preview,
            path,
            tileset: tileset_for_key(&tileset_key),
        })
    }

    /// Password, speed, resources, peasants, starting location, and the two lock rows (a flip
    /// button each for the host, plain text for everyone else).
    unsafe fn draw_info(&self, c: &geo::RightColumn) {
        let k = &self.kit;
        let flags = unsafe { *self.match_flags };
        let host = self.is_host();
        let row = |i: usize| geo::info_row(c.info, c.row_h, i);
        let line = |i: usize, caption: &str, value: &str| {
            k.label_at(row(i), caption, ALIGN_LEFT);
            k.label_at(row(i), value, ALIGN_RIGHT);
        };
        let caption = |key: &CStr| geo::with_colon(&k.localized(key));

        let password = if host {
            Some(crate::lobbypolicy::host_password())
        } else {
            crate::lobbypolicy::with(|p| p.password().map(<[u8]>::to_vec))
        };
        line(0, "Password:", &geo::password_text(password.as_deref()));
        line(
            1,
            &caption(c"option_speed_game_speed"),
            &unsafe { *self.game_speed }.to_string(),
        );
        let resources =
            unsafe { self.key_by_index(self.resource_keys, geo::resources_index(flags)) };
        let resources = CString::new(resources).unwrap_or_default();
        line(
            2,
            &caption(c"customscenario_resources"),
            &k.localized(&resources),
        );
        line(
            3,
            &caption(c"customscenario_units_peasant"),
            &k.localized(if flags & geo::ONE_PEON_BIT != 0 {
                c"option_common_on"
            } else {
                c"option_common_off"
            }),
        );
        let fixed = unsafe { *self.fixed_start } == 1;
        line(
            4,
            &caption(c"customscenario_starting_location"),
            &k.localized(if fixed {
                c"customscenario_fixed_location"
            } else {
                c"customscenario_random_location"
            }),
        );

        let locks = crate::lobbypolicy::with(|p| p.locks());
        let mut flip = (false, false);
        for (i, (name, locked, flipped)) in [
            ("Teams:", locks.teams, &mut flip.0),
            ("Slots:", locks.slots, &mut flip.1),
        ]
        .into_iter()
        .enumerate()
        {
            let r = row(5 + i);
            let value = if locked { "Locked" } else { "Unlocked" };
            k.label_at(r, name, ALIGN_LEFT);
            if host {
                *flipped = k.button_at(geo::lock_button(r), value, false);
            } else {
                k.label_at(r, value, ALIGN_RIGHT);
            }
        }
        if flip.0 || flip.1 {
            crate::session::set_lock_choice(locks.teams ^ flip.0, locks.slots ^ flip.1);
        }
    }

    /// The Players panel: a header, then the game's own slot rows in our column ratios.
    unsafe fn draw_slots(&self, panel: Rect) {
        let k = &self.kit;
        let g = geo::slot_geometry(panel);
        if !k.begin_group_flags(g.rows, c"mod_lobby_slots", NO_SCROLLBAR) {
            return;
        }
        k.row_ratios(g.header_h, &g.ratios);
        k.label(&self.players_heading(), ALIGN_LEFT);
        k.spacing();
        k.label(&k.localized(c"mp_lobby_race"), ALIGN_LEFT);
        k.spacing();
        k.label(&k.localized(c"mp_lobby_team"), ALIGN_LEFT);

        REWRITE_TEAMS.store(true, Ordering::Relaxed);
        for i in 0..SLOT_COUNT {
            unsafe { (self.slot_row)(i as i32, g.ratios.as_ptr(), g.row_h, g.spacer) };
        }
        REWRITE_TEAMS.store(false, Ordering::Relaxed);
        k.end_list();
    }

    fn players_heading(&self) -> String {
        let mut heading = self.kit.localized(c"mp_lobby_players");
        let count = unsafe { *self.player_count };
        let max = unsafe { (self.max_players)() };
        heading.push_str(&format!(" ({count}/{max})"));
        heading
    }

    /// The game's chat lines in a framed, scrolling list, and the input row under it.
    unsafe fn draw_chat(&self, history: Rect, input: Rect) {
        let k = &self.kit;
        k.panel_frame(history, c"chat_frame", Some(unsafe { *self.skin_chat }));
        let inner = Rect::new(
            fl(history.x + CHAT_INSET_X),
            fl(history.y + CHAT_INSET_TOP),
            fl(history.w - CHAT_INSET_X * 2.0),
            fl(history.h),
        );
        let mut state = CHAT.lock().unwrap_or_else(|e| e.into_inner());
        let wiped = state.scroll.wiped(k.ctx_seq());
        if k.begin_list(inner, c"chat_history") {
            if wiped {
                k.set_scroll_offset_y(state.scroll.scroll);
            } else {
                state.scroll.scroll = k.scroll_offset_y();
            }
            let previous = k.push_skin(unsafe { *self.skin_chat_text });
            let line_h = k.font_height();
            let content_top = k.next_row_top();
            let wrap_w = if state.wrap_w > 0.0 && state.wrap_w <= inner.w {
                state.wrap_w
            } else {
                inner.w
            };
            let colour = k.current_text_colour();
            let mut measured = false;
            for text in unsafe { self.chat_lines() } {
                let lines = k.count_lines(&text, wrap_w).max(1);
                k.row_dynamic(lines as f32 * line_h, 1);
                k.wrapped_label(&text, colour);
                if !measured {
                    measured = true;
                    if let Some(cell) = k.last_row_first_cell() {
                        state.wrap_w = cell.w;
                    }
                }
            }
            let content_h = k.next_row_top() - content_top;
            if state.scroll.follow(content_h, inner.h, line_h) {
                k.set_scroll_offset_y(state.scroll.scroll);
            }
            if !self.chat_scroll_request.is_null() {
                unsafe { *self.chat_scroll_request = 0 };
            }
            if wiped && !state.wipe_logged {
                state.wipe_logged = true;
                crate::session::log_info(
                    "lobbyscreen",
                    &format!(
                        "mid-frame clear seen; chat scroll restored to {:.0}",
                        state.scroll.scroll
                    ),
                );
            }
            k.pop_skin(previous);
            k.end_list();
        }
        drop(state);
        unsafe { self.draw_chat_input(input) };
    }

    /// The newest chat lines the game holds, oldest first, as text.
    unsafe fn chat_lines(&self) -> Vec<String> {
        let (begin, end) = unsafe { (*self.chat_begin, *self.chat_end) };
        if begin.is_null() || end <= begin {
            return Vec::new();
        }
        let total = (end as usize - begin as usize) / STR_STRIDE;
        let first = total.saturating_sub(CHAT_MAX_LINES);
        let mut out = Vec::with_capacity(total - first);
        for i in first..total {
            let s = unsafe { begin.add(i * STR_STRIDE) };
            let len = unsafe { s.add(STR_SIZE).cast::<i32>().read_unaligned() };
            let text = unsafe { str_data(s) };
            if !text.is_empty() && len > 0 {
                out.push(text);
            }
        }
        out
    }

    unsafe fn draw_chat_input(&self, r: Rect) {
        let k = &self.kit;
        let (edit, send) = geo::chat_input_cells(r);
        let size = unsafe { self.chat_input.add(STR_SIZE).cast::<i32>().read_unaligned() };
        let buffer = unsafe { str_buffer(self.chat_input) };
        if buffer.is_null() || size <= 1 {
            return;
        }
        unsafe { k.text_edit_at(edit, buffer, size - 1, Some(*self.skin_text_edit)) };
        let mut pressed = k.button_at(send, &k.localized(c"mp_chat_send"), false);
        if !pressed {
            pressed = k.key_pressed(KEY_ENTER);
        }
        if pressed {
            unsafe { (self.send_chat)() };
        }
    }

    /// Leave, and Start for the host: the game's own buttons, dead for a moment after entering.
    unsafe fn draw_footer(&self, window: Rect, footer_y: f32) {
        let k = &self.kit;
        let (leave, start) = geo::footer_buttons(window, footer_y, self.is_host());
        let dead = geo::entry_dead(k.clock_ms(), unsafe { *self.enter_ms });
        if dead {
            k.disable_begin();
        }
        k.set_rect(leave);
        unsafe { (self.leave_button)() };
        if let Some(start) = start {
            k.set_rect(start);
            unsafe { (self.start_button)() };
        }
        if dead {
            k.disable_end();
        }
    }
}

/// The text of a plain C string buffer the game owns.
unsafe fn str_data_c(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p.cast()).to_string_lossy().into_owned() }
}
