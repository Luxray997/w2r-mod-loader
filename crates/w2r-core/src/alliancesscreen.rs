//! The replacement Alliances dialog, ported from wc2r-plus `features/ui/alliances_screen.cpp`.
//! The game's `alliances_menu` window is begun as usual, then drawn here and reported closed so
//! the game skips its own content (the same hand-off as the lobby screen). Players are grouped
//! under their lobby team, the local player and their teammates are green, and players who lost,
//! won or left are red. The allied and shared-vision checkboxes are the game's own, so they keep
//! working. In addition, the host of a live match gets a Kick button at the end of each other
//! person's row (the same removal as the panel, `ingamekick.rs`). The pure part is in
//! `alliancelayout.rs`. x86 only (`cdecl`).
//!
//! Any fault under the draw switches back to the game's dialog for the rest of the session (which
//! still carries the stacked Kick rows of the footer hook).

use core::ffi::{c_char, c_void};
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::alliancelayout::{self as lay, Entry, Tint};
use crate::guard::guarded;
use crate::host::Log;
use crate::ingamekick;
use crate::screenkit::game::{Kit, cstr, rgba, str_data};
use crate::screenkit::{ALIGN_CENTRE, ALIGN_LEFT, Rect};
use crate::symbols::Symbols;

/// The window the game begins for the dialog.
pub const WINDOW: &[u8] = b"alliances_menu";

const PLAYERS: usize = 8;
const GROUP_FLAGS: u32 = 0x20;
const GROUP_PLAIN: u32 = 0;
/// The caption's second line is drawn centred, one step lower.
const ALIGN_SECOND_LINE: i32 = 0x0a;
const NAME_STRIDE: usize = 0x38;
const LOBBY_SLOT_STRIDE: usize = 0x26;
const LOBBY_SLOT_TEAM: usize = 0x0a;
const LOBBY_SLOT_NAME: usize = 0x0d;
const LOBBY_SLOT_NAME_MAX: usize = LOBBY_SLOT_STRIDE - LOBBY_SLOT_NAME;
const DIPLOMACY_ROW_STRIDE: usize = 0x10;
const PEER_STRIDE: usize = 0x22;
const PEER_DEPARTED: u8 = 2;
const STR_STRIDE: usize = 0x18;

const TEAMMATE: u32 = rgba(0x25, 0xe6, 0x5f, 255);
const OUT: u32 = rgba(0xe6, 0x40, 0x40, 255);
const OBSERVER: u32 = rgba(0x6f, 0xb4, 0xff, 255);

type CheckboxFn = unsafe extern "C" fn(*const c_char, *mut u8) -> u8;
type SendFn = unsafe extern "C" fn();
type AllowedFn = unsafe extern "C" fn() -> i32;
type IconFn = unsafe extern "C" fn(i32);
type FooterFn = unsafe extern "C" fn(*mut c_void, f32, f32, f32, f32, i32, i32);

struct Screen {
    kit: Kit,
    checkbox: CheckboxFn,
    send_diplomacy: SendFn,
    allowed: AllowedFn,
    icon: IconFn,
    footer: FooterFn,
    owner: usize,
    names: usize,
    lobby_slots: usize,
    stance: usize,
    result: usize,
    lobby_by_net: usize,
    peers: usize,
    local_player: usize,
    name_table: usize,
    allied_checks: usize,
    vision_checks: usize,
    victory_check: usize,
    row_font: usize,
    skin: usize,
    skin_alt: usize,
    skin_alt_flag: usize,
}

// SAFETY: addresses of the game's functions and globals, fixed for the session; the screen is
// only used from the game's main thread.
unsafe impl Send for Screen {}
unsafe impl Sync for Screen {}

/// Who was in the match when it started, and who the local player's teammates were.
#[derive(Clone, Default)]
struct Roster {
    team_game: bool,
    present: u8,
    human: u8,
    team: [u8; PLAYERS],
    teammates_of_me: u8,
    names: [String; PLAYERS],
}

static SCREEN: OnceLock<Option<Screen>> = OnceLock::new();
static ROSTER: Mutex<Option<Roster>> = Mutex::new(None);
/// A fault under the replacement draw: the game's dialog from here on.
static FAILED: AtomicBool = AtomicBool::new(false);
static ANNOUNCED: AtomicBool = AtomicBool::new(false);
static SHAPE: Mutex<i64> = Mutex::new(-1);

pub fn install(symbols: &Symbols, log: &dyn Log) -> bool {
    match resolve(symbols) {
        Ok(screen) => {
            let _ = SCREEN.set(Some(screen));
            log.line(W2R_LOG_INFO, "host", "alliances screen: replacement ready");
            true
        }
        Err(name) => {
            let _ = SCREEN.set(None);
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("alliances screen: symbol {name} missing; the game's dialog stays"),
            );
            false
        }
    }
}

fn resolve(symbols: &Symbols) -> Result<Screen, &'static str> {
    let at = |n: &'static str| symbols.address(n).ok_or(n);
    // SAFETY: signatures from the symbol table (wc2r-plus `addresses.h`, build 2818).
    unsafe {
        Ok(Screen {
            kit: Kit::new(symbols).ok_or("screen kit")?,
            checkbox: core::mem::transmute::<usize, CheckboxFn>(at("DipCheckbox")?),
            send_diplomacy: core::mem::transmute::<usize, SendFn>(at("SendDiplomacyFromUI")?),
            allowed: core::mem::transmute::<usize, AllowedFn>(at("AlliancesAllowed")?),
            icon: core::mem::transmute::<usize, IconFn>(at("DrawTeamColourIcon")?),
            footer: core::mem::transmute::<usize, FooterFn>(at("AlliancesFooter")?),
            owner: at("PlayerOwner")?,
            names: at("PlayerNames")?,
            lobby_slots: at("MpLobbySlots")?,
            stance: at("DiplomacyStance")?,
            result: at("PlayerMatchResult")?,
            lobby_by_net: at("LobbySlotByNetSlot")?,
            peers: at("PeerTable")?,
            local_player: at("LocalPlayer")?,
            name_table: at("DipNameTable")?,
            allied_checks: at("DipAlliedChecks")?,
            vision_checks: at("DipVisionChecks")?,
            victory_check: at("DipAlliedVictoryCheck")?,
            row_font: at("FontAlliancesRow")?,
            skin: at("SkinAlliances")?,
            skin_alt: at("SkinAlliancesAlt")?,
            skin_alt_flag: at("SkinAltFlag")?,
        })
    }
}

fn byte(addr: usize) -> u8 {
    // SAFETY: a byte in the game's data section; callers run under `guarded`.
    unsafe { core::ptr::read_unaligned(addr as *const u8) }
}

fn word(addr: usize) -> u32 {
    // SAFETY: as `byte`.
    unsafe { core::ptr::read_unaligned(addr as *const u32) }
}

/// A fixed-size, NUL-padded name field; empty when it has no terminator (not a name).
fn fixed_name(addr: usize, max: usize) -> String {
    // SAFETY: a name field inside one of the game's tables; callers run under `guarded`.
    let bytes = unsafe { core::slice::from_raw_parts(addr as *const u8, max) };
    match bytes.iter().position(|&b| b == 0) {
        Some(n) => crate::gamepath::decode(&bytes[..n]),
        None => String::new(),
    }
}

/// Owner 0 is a person and 1 the computer; anything else is an empty seat.
fn in_match(s: &Screen, player: usize) -> bool {
    byte(s.owner + player) <= 1
}

fn lobby_team(s: &Screen, player: usize) -> u8 {
    byte(s.lobby_slots + player * LOBBY_SLOT_STRIDE + LOBBY_SLOT_TEAM)
}

fn teammates_at_start(s: &Screen, player: usize) -> u8 {
    let row = s.stance + player * DIPLOMACY_ROW_STRIDE;
    let team = lobby_team(s, player);
    let mut mask = 0u8;
    for i in 0..PLAYERS {
        if i != player && in_match(s, i) && byte(row + i) != 0 && lobby_team(s, i) == team {
            mask |= 1 << i;
        }
    }
    mask
}

/// The match is starting: remember who is in it, who is on whose team, and what they are called.
/// Called from the match-start hook, before the game's own function, as wc2r-plus does.
pub fn on_match_start() {
    let Some(Some(s)) = SCREEN.get() else { return };
    let mut r = Roster::default();
    let me = byte(s.local_player) as usize;
    for i in 0..PLAYERS {
        if !in_match(s, i) {
            continue;
        }
        r.present |= 1 << i;
        if byte(s.owner + i) == 0 {
            r.human |= 1 << i;
        }
        r.team[i] = lobby_team(s, i);
        if teammates_at_start(s, i) != 0 {
            r.team_game = true;
        }
        let mut name = fixed_name(s.names + i * NAME_STRIDE, NAME_STRIDE);
        if name.is_empty() {
            name = fixed_name(
                s.lobby_slots + i * LOBBY_SLOT_STRIDE + LOBBY_SLOT_NAME,
                LOBBY_SLOT_NAME_MAX,
            );
        }
        r.names[i] = name;
    }
    if me < PLAYERS {
        r.teammates_of_me = teammates_at_start(s, me);
    }
    // Other features (the APM counter) stay quiet for someone who only watches.
    let observing =
        me < PLAYERS && r.human & (1 << me) != 0 && r.team[me] == crate::observer::OBSERVER_TEAM;
    crate::observer::set_local_observer(observing);
    // An observer gets the viewing system (switch player, per-player fog, scoreboard) for the
    // whole match.
    if observing {
        crate::replayplay::live_start(me as u8);
    }
    crate::session::log_info(
        "alliances",
        &format!(
            "match start: player {me}, present {:#04x}, humans {:#04x}, {}, my teammates {:#04x}",
            r.present,
            r.human,
            if r.team_game { "team game" } else { "no teams" },
            r.teammates_of_me
        ),
    );
    if let Ok(mut slot) = ROSTER.lock() {
        *slot = Some(r);
    }
}

/// Draws the replacement dialog inside the window the game just began. True when it drew and
/// ended the window (the caller must report the window closed); false leaves the frame to the game.
pub fn try_draw() -> bool {
    if FAILED.load(Ordering::Relaxed) {
        return false;
    }
    let Some(Some(s)) = SCREEN.get() else {
        return false;
    };
    let Some(roster) = ROSTER.lock().ok().and_then(|r| r.clone()) else {
        return false;
    };
    let Some(window) = s.kit.window_rect() else {
        return false;
    };
    ingamekick::set_own_screen(true);
    let drawn = guarded(|| s.draw(window, &roster));
    ingamekick::set_own_screen(false);
    match drawn {
        Ok(clicked) => {
            if !ANNOUNCED.swap(true, Ordering::Relaxed) {
                crate::session::log_info(
                    "alliances",
                    &format!(
                        "drawing the replacement dialog (window {:.0}x{:.0})",
                        window.w, window.h
                    ),
                );
            }
            // After the window is closed: a kick is the game's own network drop.
            if let Some((slot, name)) = clicked {
                ingamekick::kick_pressed(slot, name);
            }
            true
        }
        Err(fault) => {
            FAILED.store(true, Ordering::Relaxed);
            crate::session::host_fault("alliances screen", &fault);
            crate::session::log_error(
                "alliances",
                "the replacement dialog faulted; the game's dialog is used from now on",
            );
            false
        }
    }
}

impl Screen {
    fn loc(&self, key: &str) -> String {
        match CString::new(key) {
            Ok(k) => self.kit.localized(&k),
            Err(_) => key.to_owned(),
        }
    }

    /// The name the stock dialog has for this player; empty for the local player or when the
    /// dialog has no row for them.
    fn stock_name(&self, player: usize) -> String {
        let table = word(self.name_table) as usize;
        if table == 0 || player == byte(self.local_player) as usize {
            return String::new();
        }
        // SAFETY: a table of eight `std::string`s the game keeps for the open dialog.
        unsafe { str_data((table + player * STR_STRIDE) as *const u8) }
    }

    fn has_left(&self, roster: &Roster, player: usize) -> bool {
        if roster.human & (1 << player) == 0 {
            return false;
        }
        if byte(self.result + player) == 1 {
            return true;
        }
        (0..PLAYERS).any(|ns| {
            usize::from(word(self.lobby_by_net + ns * 4) as u8) == player
                && byte(self.peers + ns * PEER_STRIDE) == PEER_DEPARTED
        })
    }

    fn is_out(&self, roster: &Roster, player: usize) -> bool {
        if roster.present & (1 << player) == 0 {
            return false;
        }
        let result = byte(self.result + player);
        result == 1 || result == 2 || self.has_left(roster, player)
    }

    fn entries(&self, roster: &Roster, me: usize) -> Vec<Entry> {
        let mut out = Vec::new();
        for i in 0..PLAYERS {
            let stock = self.stock_name(i);
            let checkboxes = !stock.is_empty();
            let listed = roster.present & (1 << i) != 0;
            if !listed && !checkboxes {
                continue;
            }
            let raw = if checkboxes {
                stock
            } else {
                roster.names[i].clone()
            };
            let mut name = match lay::clan_key(&raw) {
                Some(key) => self.loc(key),
                None => raw,
            };
            if name.is_empty() {
                name = "?".to_owned();
            }
            let observer = roster.human & (1 << i) != 0
                && roster.team[i] == crate::observer::OBSERVER_TEAM;
            out.push(Entry {
                player: i as u8,
                team: if observer {
                    crate::observer::OBSERVER_TEAM
                } else if roster.team_game {
                    roster.team[i]
                } else {
                    0
                },
                name,
                tint: lay::tint(
                    // An observer owns nothing, so the game lists them as defeated; only
                    // leaving the game is red for them.
                    if observer {
                        self.has_left(roster, i)
                    } else {
                        self.is_out(roster, i)
                    },
                    observer,
                    roster.team_game,
                    i == me,
                    roster.teammates_of_me & (1 << i) != 0,
                ),
                checkboxes,
            });
        }
        out
    }

    fn checkbox(&self, label: &str, value: usize) {
        let l = cstr(label);
        // SAFETY: the game's own checkbox over its own flag byte.
        if unsafe { (self.checkbox)(l.as_ptr().cast(), value as *mut u8) } != 0 {
            // SAFETY: the game's own "send my diplomacy" after a change.
            unsafe { (self.send_diplomacy)() };
        }
    }

    fn draw_header_row(&self, kick: bool) {
        let k = &self.kit;
        k.row_ratios(lay::HEADER_ROW_H, lay::header_ratios(kick));
        k.spacing();
        // SAFETY: the game's own query.
        if unsafe { (self.allowed)() } == 0 {
            k.disable_begin();
        }
        self.checkbox(&self.loc("mp_dip_victory"), self.victory_check);
        k.disable_end();
        k.label(&self.loc("mp_dip_allies"), ALIGN_CENTRE);

        if k.begin_group_cell(c"shared_vision_title", GROUP_FLAGS) {
            let caption = self.loc("mp_dip_vision");
            match caption.split_once(' ') {
                None => {
                    k.row_dynamic(lay::HEADER_ROW_H, 1);
                    k.label(&caption, ALIGN_CENTRE);
                }
                Some((first, rest)) => {
                    k.row_dynamic(lay::VISION_LINE_H, 1);
                    k.label(first, ALIGN_CENTRE);
                    k.label(rest, ALIGN_SECOND_LINE);
                }
            }
            k.end_list();
        }
    }

    /// One player's row; returns the Kick button's net slot and name when it was clicked.
    fn draw_player_row(&self, e: &Entry, kick: Option<Option<(u8, bool)>>) -> Option<(u8, String)> {
        let k = &self.kit;
        k.row_ratios(lay::PLAYER_ROW_H, lay::player_ratios(kick.is_some()));
        let previous_font = k.push_font(word(self.row_font));
        // SAFETY: the game's own icon for a player index.
        unsafe { (self.icon)(i32::from(e.player)) };

        let before = k.current_text_colour();
        let colour = match e.tint {
            Tint::Plain => None,
            Tint::Teammate => Some(TEAMMATE),
            Tint::Out => Some(OUT),
            Tint::Observer => Some(OBSERVER),
        };
        if let Some(c) = colour {
            k.set_current_text_colour(c);
        }
        k.label(&e.name, ALIGN_LEFT);
        if colour.is_some() {
            k.set_current_text_colour(before);
        }

        if e.checkboxes {
            if byte(self.owner + usize::from(e.player)) == 1 {
                k.disable_begin();
            }
            self.checkbox("", self.allied_checks + usize::from(e.player));
            self.checkbox("", self.vision_checks + usize::from(e.player));
            k.disable_end();
        } else {
            k.spacing();
            k.spacing();
        }

        let mut clicked = None;
        if let Some(button) = kick {
            match button {
                Some((slot, armed)) => {
                    if k.button_cell(if armed { "Sure?" } else { "Kick" }) {
                        clicked = Some((slot, e.name.clone()));
                    }
                }
                None => k.spacing(),
            }
        }
        k.pop_font(previous_font);
        clicked
    }

    /// The whole dialog. Returns the Kick button clicked, if any, as (net slot, name).
    fn draw(&self, window: Rect, roster: &Roster) -> Option<(u8, String)> {
        let k = &self.kit;
        let me = byte(self.local_player);
        let alt = byte(self.skin_alt_flag) == 1;
        let previous_skin = k.push_skin(word(if alt { self.skin_alt } else { self.skin }));

        let my_team = if usize::from(me) < PLAYERS && roster.team_game {
            roster.team[usize::from(me)]
        } else {
            0
        };
        let groups = lay::build_groups(self.entries(roster, usize::from(me)), me, my_team);
        let players: usize = groups.iter().map(|g| g.rows.len()).sum();
        let observers_listed = groups
            .iter()
            .any(|g| g.team == crate::observer::OBSERVER_TEAM);
        let headers = if roster.team_game || observers_listed {
            groups.len()
        } else {
            0
        };

        let kickable = ingamekick::kickable();
        let armed = ingamekick::kick_armed();
        let kick_column = !kickable.is_empty();

        let content_h = lay::content_height(window.h);
        let left_mask = (0..PLAYERS)
            .filter(|&i| self.has_left(roster, i))
            .fold(0i64, |m, i| m | 1 << i);
        let shape = ((players * 16 + headers) as i64) << 8 | left_mask | (kick_column as i64) << 40;
        if let Ok(mut last) = SHAPE.lock()
            && *last != shape
        {
            *last = shape;
            crate::session::log_info(
                "alliances",
                &format!(
                    "drawing our dialog: {players} players in {headers} team headings, left \
                     {left_mask:#04x}, kick column {kick_column} (content {content_h:.0})"
                ),
            );
        }

        let mut clicked = None;
        k.space_begin();
        if k.begin_group_flags(
            Rect::new(0.0, 0.0, window.w, window.h - lay::FOOTER_BOTTOM_GAP),
            c"main",
            GROUP_FLAGS,
        ) {
            k.space_begin();
            k.label_at(
                Rect::new(0.0, lay::TITLE_Y, lay::TITLE_W, lay::TITLE_H),
                &self.loc("mp_dip_title"),
                ALIGN_CENTRE,
            );

            // The content group scrolls but shows no scrollbar of its own.
            let scrollbar = k.scrollbar_y();
            let saved = if scrollbar.is_null() {
                0.0
            } else {
                // SAFETY: a float in the live context.
                unsafe { scrollbar.read_unaligned() }
            };
            if !scrollbar.is_null() {
                // SAFETY: as above.
                unsafe { scrollbar.write_unaligned(0.0) };
            }
            let open = k.begin_group_flags(
                Rect::new(lay::CONTENT_X, lay::CONTENT_TOP, lay::CONTENT_W, content_h),
                c"main_content",
                GROUP_PLAIN,
            );
            if !scrollbar.is_null() {
                // SAFETY: as above.
                unsafe { scrollbar.write_unaligned(saved) };
            }
            if open {
                self.draw_header_row(kick_column);
                let team_word = self.loc("mp_lobby_team");
                for g in &groups {
                    if headers > 0 {
                        k.row_dynamic(lay::TEAM_HEADER_H, 1);
                        let heading = if g.team == crate::observer::OBSERVER_TEAM {
                            "Observers".to_owned()
                        } else {
                            format!("{team_word} {}", g.team)
                        };
                        k.label(&heading, ALIGN_LEFT);
                    }
                    for e in &g.rows {
                        let kick = kick_column.then(|| {
                            kickable
                                .iter()
                                .find(|(p, _)| *p == e.player)
                                .map(|&(_, slot)| (slot, armed == Some(slot)))
                        });
                        if let Some(c) = self.draw_player_row(e, kick) {
                            clicked = Some(c);
                        }
                    }
                }
                let end_scrollbar = if scrollbar.is_null() {
                    0.0
                } else {
                    // SAFETY: as above.
                    unsafe { scrollbar.read_unaligned() }
                };
                if !scrollbar.is_null() {
                    // SAFETY: as above.
                    unsafe { scrollbar.write_unaligned(0.0) };
                }
                k.end_list();
                if !scrollbar.is_null() {
                    // SAFETY: as above.
                    unsafe { scrollbar.write_unaligned(end_scrollbar) };
                }
            }
            k.space_end();
            k.end_list();
        }

        // SAFETY: the game's own Cancel/OK strip; the footer hook leaves it alone while we draw.
        unsafe {
            (self.footer)(
                k.ctx(),
                lay::FOOTER_X,
                window.h - lay::FOOTER_BOTTOM_GAP,
                lay::FOOTER_W,
                lay::FOOTER_H,
                0,
                0,
            )
        };
        k.space_end();
        k.end_window();
        k.pop_skin(previous_skin);
        clicked
    }
}
