//! Watching a match as somebody else: the view system shared by replay playback and observers.
//!
//! The screen follows `LocalPlayer` (0x918ccd), the view player (0x922f5a) and the view bit mask
//! (0x922f23). While viewing is on, `[` and `]` cycle the player you watch as, Home goes back to
//! your own (the recorded player in a replay), Insert toggles full vision, and the backtick key
//! steps through the scoreboard pages. The change is applied only while the game's
//! simulation-mode flag (`SetSimMode`, 0x4a01b0) is 0, i.e. for drawing and input, and the real
//! values are put back whenever the flag goes to 1, so the simulation never sees which player is
//! being watched.
//!
//! Per-player fog (`fogtrack.rs`, `fogmaps.rs`) and the minimap memory (`viewmem.rs`) belong to
//! the same system. Whoever drives it calls [`enable`] before the first turn, [`begin`] once the
//! local player is known and [`disable`] when the match is over. x86 only (`cdecl`).

use core::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::host::Log;
use crate::pe::Image;
use crate::symbols::Symbols;

type SimModeFn = unsafe extern "C" fn(u32) -> u32;
type VoidFn = unsafe extern "C" fn();
type PushFn = unsafe extern "C" fn(*const u8, i32, u32);

static ORIG_SIM_MODE: AtomicUsize = AtomicUsize::new(0);
static ORIG_DRAW_LIST: AtomicUsize = AtomicUsize::new(0);
/// The units' real "hidden from" bits while they are recomputed for a computer player's view.
static HIDDEN_SAVED: Mutex<Option<Vec<u8>>> = Mutex::new(None);
/// The game's real fog shade maps while another player's are swapped in for drawing.
static FOG_SAVED: Mutex<Option<(Vec<u8>, Vec<u8>)>> = Mutex::new(None);
/// The fog shade maps are at most 128 x 128 bytes.
const FOG_LEN: usize = 0x4000;
const NONE: u8 = 0xff;
/// The player this machine really is (whose commands it sends); `NONE` until `begin`.
static REAL: AtomicU8 = AtomicU8::new(NONE);
/// The player being watched; `NONE` means the home one.
static VIEWED: AtomicU8 = AtomicU8::new(NONE);
/// The player the view starts as and Home returns to.
static HOME: AtomicU8 = AtomicU8::new(NONE);
static FULL_VISION: AtomicBool = AtomicBool::new(false);
/// Which view keys were down on the previous frame.
static KEYS_DOWN: AtomicU16 = AtomicU16::new(0);
/// The scoreboard under the panel: 0 now, 1 totals, 2 rates, 3 hidden.
static BOARD: AtomicU8 = AtomicU8::new(0);
static VIEWING: AtomicBool = AtomicBool::new(false);
static ADDRS: OnceLock<Addrs> = OnceLock::new();

struct Addrs {
    recompute_hidden: usize,
    fog_vis: usize,
    fog_explored: usize,
    relations: usize,
    fog: usize,
    refresh: usize,
    local: usize,
    view_player: usize,
    view_mask: usize,
    owner: usize,
    names: usize,
    push: usize,
    /// `SkinAltFlag`: the local player's race (0 human, 1 orc), which the UI skin and sounds follow.
    race_flag: usize,
    /// `PlayerRaces`: each player's race.
    races: usize,
    /// `MpLobbySlots`: who is on the observer team.
    lobby: usize,
}

const KEY_PREV: u16 = 1;
const KEY_NEXT: u16 = 2;
const KEY_HOME: u16 = 4;
const KEY_FULL: u16 = 8;
const KEY_BOARD: u16 = 16;
/// `[`, `]`, Home, Insert and the backtick.
const WATCHED: [(u16, i32); 5] = [
    (KEY_BOARD, 0xC0),
    (KEY_PREV, 0xDB),
    (KEY_NEXT, 0xDD),
    (KEY_HOME, 0x24),
    (KEY_FULL, 0x2D),
];
/// The lobby slot table: stride, and where the team byte sits.
const LOBBY_SLOT_STRIDE: usize = 0x26;
const LOBBY_SLOT_TEAM: usize = 0x0a;
/// A player's record in `PlayerNames` (0x91ada8): stride, and the longest name.
const NAME_STRIDE: usize = 0x38;
const NAME_MAX: usize = 0x19;

fn log(level: u32, text: &str) {
    crate::session::StartupLog.line(level, "viewer", text);
}

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let get = |name: &str| symbols.address(name);
    let (Some(sim_mode), Some(local), Some(view_player), Some(view_mask)) = (
        get("SetSimMode"),
        get("LocalPlayer"),
        get("ViewPlayer"),
        get("ViewMask"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "viewing: symbols missing");
        return false;
    };
    let (Some(recompute_hidden), Some(fog_vis), Some(fog_explored)) = (
        get("RecomputeHidden"),
        get("FogVisibilityPtr"),
        get("FogExploredPtr"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "viewing: symbols missing");
        return false;
    };
    let (Some(fog), Some(relations), Some(refresh), Some(draw_list)) = (
        get("FogShadePtr"),
        get("RelationColours"),
        get("RefreshView"),
        get("BuildDrawList"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "viewing: symbols missing");
        return false;
    };
    let (Some(owner), Some(names), Some(push), Some(race_flag), Some(races), Some(lobby)) = (
        get("PlayerOwner"),
        get("PlayerNames"),
        get("PushMapMessage"),
        get("SkinAltFlag"),
        get("PlayerRaces"),
        get("MpLobbySlots"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "viewing: symbols missing");
        return false;
    };
    let _ = ADDRS.set(Addrs {
        recompute_hidden,
        fog_vis,
        fog_explored,
        relations,
        fog,
        refresh,
        local,
        view_player,
        view_mask,
        owner,
        names,
        push,
        race_flag,
        races,
        lobby,
    });
    let mut ok = true;
    // SAFETY: each detour matches its target's cdecl signature (Ghidra, build 2818; the call
    // sites clean the stack themselves) and calls the original with the same arguments.
    unsafe {
        ok &= crate::replayplay::hook(
            image,
            sim_mode,
            sim_mode_detour as *mut c_void,
            "SetSimMode",
            &ORIG_SIM_MODE,
            log,
        );
        ok &= crate::replayplay::hook(
            image,
            draw_list,
            draw_list_detour as *mut c_void,
            "BuildDrawList",
            &ORIG_DRAW_LIST,
            log,
        );
    }
    ok &= crate::fogtrack::install(image, symbols, log);
    if !crate::viewmem::init(symbols) {
        log.line(W2R_LOG_ERROR, "host", "viewing: symbols missing");
        ok = false;
    }
    ok
}

/// Viewing is on: the fog of every player is tracked from now, and the hooks act. Call before the
/// match starts, so the fog is followed from the first light.
pub fn enable() {
    crate::fogmaps::arm();
    VIEWING.store(true, Ordering::Release);
}

/// Viewing is off and everything is back to the real player.
pub fn disable() {
    reset_view();
    VIEWING.store(false, Ordering::Release);
}

pub fn is_viewing() -> bool {
    VIEWING.load(Ordering::Acquire)
}

/// The local player is known (`real`, read before anything is applied); the view starts as `home`.
pub fn begin(real: u8, home: u8) {
    REAL.store(real, Ordering::Release);
    HOME.store(if home < 8 { home } else { real }, Ordering::Release);
    if home < 8 && home != real {
        VIEWED.store(home, Ordering::Release);
    }
}

/// The player this machine really is (`0xff` before `begin`).
pub fn real() -> u8 {
    REAL.load(Ordering::Acquire)
}

pub fn home() -> u8 {
    HOME.load(Ordering::Acquire)
}

/// The player whose view is on screen, once `begin` was called.
pub fn watching() -> Option<u8> {
    let (viewed, real) = (VIEWED.load(Ordering::Acquire), REAL.load(Ordering::Acquire));
    match (viewed < 8, real < 8) {
        (true, _) => Some(viewed),
        (false, true) => Some(real),
        _ => None,
    }
}

/// Which scoreboard page is up (the backtick key steps through them): 0 now, 1 totals, 2 rates,
/// 3 none.
pub fn board_mode() -> u8 {
    BOARD.load(Ordering::Acquire)
}

/// A player in the match, for the scoreboard.
pub struct Member {
    pub slot: u8,
    /// A person (rather than the computer): only people send commands.
    pub human: bool,
    pub name: String,
}

/// The players in the match (a human or a computer), in slot order. Empty unless viewing.
pub fn roster() -> Vec<Member> {
    let Some(a) = ADDRS.get().filter(|_| is_viewing()) else {
        return Vec::new();
    };
    players(a)
        .into_iter()
        .map(|slot| Member {
            slot,
            // SAFETY: the owner table is 0x20 bytes of the game's data section.
            human: unsafe { *((a.owner + usize::from(slot)) as *const u8) } == 0,
            name: player_name(a, slot),
        })
        .collect()
}

/// A person on the observer team (lobby team 9) only watches: not somebody to watch as.
fn is_observer(a: &Addrs, player: u8) -> bool {
    // SAFETY: the owner table and the lobby slot table are in the game's data section.
    unsafe {
        *((a.owner + usize::from(player)) as *const u8) == 0
            && *((a.lobby + usize::from(player) * LOBBY_SLOT_STRIDE + LOBBY_SLOT_TEAM) as *const u8)
                == crate::observer::OBSERVER_TEAM
    }
}

/// Players in the match (a human or a computer) that are playing it, by index.
fn players(a: &Addrs) -> Vec<u8> {
    (0..8u8)
        // SAFETY: the owner table is 0x20 bytes of the game's data section.
        .filter(|&i| matches!(unsafe { *((a.owner + usize::from(i)) as *const u8) }, 0 | 1))
        .filter(|&i| !is_observer(a, i))
        .collect()
}

fn player_name(a: &Addrs, player: u8) -> String {
    // SAFETY: the player records are a fixed table in the game's data section.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (a.names + usize::from(player) * NAME_STRIDE) as *const u8,
            NAME_MAX,
        )
    };
    let n = bytes.iter().position(|&b| b == 0).unwrap_or(NAME_MAX);
    String::from_utf8_lossy(&bytes[..n]).into_owned()
}

/// Shows a line in the game's map message area.
pub(crate) fn say(text: &str) {
    let Some(a) = ADDRS.get() else { return };
    let mut c = text.as_bytes().to_vec();
    c.push(0);
    // SAFETY: `PushMapMessage` copies the NUL-terminated text it is given; called on the main thread.
    unsafe {
        let push: PushFn = core::mem::transmute(a.push);
        push(c.as_ptr(), 1, 3000);
    }
}

/// # Safety
/// `a` holds the game's addresses; call inside `guarded`.
unsafe fn write_view(a: &Addrs, player: u8, mask: u8) {
    unsafe {
        core::ptr::write_volatile(a.local as *mut u8, player);
        core::ptr::write_volatile(a.view_player as *mut u8, player);
        core::ptr::write_volatile(a.view_mask as *mut u8, mask);
        // The sounds (and UI skin) follow the local player's race, so the watched player's.
        let race = core::ptr::read_volatile((a.races + usize::from(player & 7)) as *const u8);
        core::ptr::write_volatile(a.race_flag as *mut u8, race);
    }
}

/// The real player's own view: what the game itself has when nobody watches anyone else.
fn restore_view() {
    let real = REAL.load(Ordering::Acquire);
    let Some(a) = ADDRS.get() else { return };
    restore_fog(a);
    restore_hidden();
    crate::viewmem::leave();
    if real < 8 {
        // SAFETY: symbol-table addresses of single bytes in the game's data section.
        unsafe { write_view(a, real, 1 << real) };
    }
}

/// Full vision shows the whole map: both fog maps the game draws from are swapped for all-lit
/// ones (0 = lit) while drawing, and the real ones are put back before the simulation runs.
fn swap_in_clear_fog(a: &Addrs) {
    swap_in_fog(a, |current, remembered| {
        current.fill(0);
        remembered.fill(0);
    });
}

/// Swaps in fog maps made by `fill(current, remembered)` for drawing; the real ones are kept to
/// put back. `current` (0x91ad5c) is what is in sight now, `remembered` (0x91ad60) what has been
/// explored; both run from 0 (lit) to 0x10 (black) with soft values at the edges.
fn swap_in_fog(a: &Addrs, fill: impl FnOnce(&mut [u8], &mut [u8])) {
    let Ok(mut saved) = FOG_SAVED.lock() else {
        return;
    };
    if saved.is_some() {
        return;
    }
    // SAFETY: pointer variables of the game, each pointing at a FOG_LEN-byte map it allocated.
    let (current, remembered) = unsafe {
        (
            core::ptr::read_unaligned(a.fog as *const usize),
            core::ptr::read_unaligned(a.fog_explored as *const usize),
        )
    };
    if current == 0 || remembered == 0 {
        return;
    }
    // SAFETY: as above; the two maps are separate allocations.
    unsafe {
        let cur = core::slice::from_raw_parts_mut(current as *mut u8, FOG_LEN);
        let rem = core::slice::from_raw_parts_mut(remembered as *mut u8, FOG_LEN);
        *saved = Some((cur.to_vec(), rem.to_vec()));
        fill(cur, rem);
    }
}

fn restore_fog(a: &Addrs) {
    let Ok(mut saved) = FOG_SAVED.lock() else {
        return;
    };
    let Some((current, remembered)) = saved.take() else {
        return;
    };
    // SAFETY: as in `swap_in_fog`.
    unsafe {
        for (var, real) in [(a.fog, current), (a.fog_explored, remembered)] {
            let map = core::ptr::read_unaligned(var as *const usize);
            if map != 0 {
                core::ptr::copy_nonoverlapping(real.as_ptr(), map as *mut u8, real.len());
            }
        }
    }
}

/// Draws the map as `player` sees it, from the private maps the game's own code has kept for
/// them (`fogtrack.rs`).
fn swap_in_player_fog(a: &Addrs, player: u8) {
    swap_in_fog(a, |current, remembered| {
        crate::fogmaps::copy_into(player, current, remembered);
    });
}

/// The game never hides anything from a computer player. To watch one as a person would see,
/// the units' hidden-from bits are made again with the human rule (a unit is seen if a tile under
/// it is lit for that player) from that player's own dark map, by the game's own routine with
/// the slot briefly marked human. The real bits come back in `restore_hidden`.
fn hide_like_a_human(a: &Addrs, player: u8) {
    // SAFETY: the owner table is in the game's data section.
    let owner = (a.owner + usize::from(player)) as *mut u8;
    if unsafe { *owner } != 1 {
        return;
    }
    let Some(dark) = crate::fogmaps::dark_ptr(player) else {
        return;
    };
    if let Ok(mut saved) = HIDDEN_SAVED.lock() {
        if saved.is_none() {
            *saved = Some(crate::viewmem::save_hidden());
        }
    }
    // SAFETY: the game's globals; the private map outlives the call; `RecomputeHidden` takes no
    // arguments. Both are put back straight after.
    unsafe {
        let old_dark = core::ptr::read_unaligned(a.fog_vis as *const usize);
        *owner = 0;
        core::ptr::write_unaligned(a.fog_vis as *mut usize, dark);
        let recompute: VoidFn = core::mem::transmute(a.recompute_hidden);
        recompute();
        core::ptr::write_unaligned(a.fog_vis as *mut usize, old_dark);
        *owner = 1;
    }
}

fn restore_hidden() {
    if let Some(saved) = HIDDEN_SAVED.lock().ok().and_then(|mut s| s.take()) {
        crate::viewmem::restore_hidden(&saved);
    }
}

fn apply_view() {
    let real = REAL.load(Ordering::Acquire);
    let Some(a) = ADDRS.get() else { return };
    if real >= 8 {
        return;
    }
    // Always start from the real map, so a view that changed since the last time is rebuilt.
    restore_fog(a);
    restore_hidden();
    let viewed = VIEWED.load(Ordering::Acquire);
    if FULL_VISION.load(Ordering::Acquire) {
        swap_in_clear_fog(a);
        // What the draw pass remembers while it draws goes to this view, not the real player.
        crate::viewmem::enter(crate::viewmemory::FULL_VISION_SLOT);
    } else if viewed < 8 && viewed != real {
        swap_in_player_fog(a, viewed);
        crate::viewmem::enter(usize::from(viewed));
        hide_like_a_human(a, viewed);
    } else {
        crate::viewmem::leave();
    }
    let player = if viewed < 8 { viewed } else { real };
    // The mask hides objects whose own "hidden from" bits include it: 0 hides nothing.
    let mask = if FULL_VISION.load(Ordering::Acquire) {
        0
    } else {
        1 << player
    };
    // SAFETY: as in `restore_view`.
    unsafe { write_view(a, player, mask) };
    // The colours of hover and selection borders (own / allied / enemy) are a table the game
    // fills from the local player once; refill it for the player being watched. It is only
    // read for drawing.
    // SAFETY: `RelationColours` takes no arguments and writes only that table.
    unsafe {
        let relation_colours: VoidFn = core::mem::transmute(a.relations);
        relation_colours();
    }
}

/// The game's simulation-mode flag changes around every simulation step: 1 = entering it, so
/// the real local player goes back for the duration; 0 = leaving it, so the screen shows the
/// watched player again.
unsafe extern "C" fn sim_mode_detour(mode: u32) -> u32 {
    let original: SimModeFn =
        unsafe { core::mem::transmute(ORIG_SIM_MODE.load(Ordering::Acquire)) };
    let before = unsafe { original(mode) };
    if is_viewing() {
        let _ = guarded(|| {
            if mode & 0xff != 0 {
                restore_view();
            } else {
                apply_view();
            }
        });
    }
    before
}

/// Building the list of units to draw runs inside the simulation bracket but is display-only: it
/// follows the player being watched.
unsafe extern "C" fn draw_list_detour() {
    let original: VoidFn = unsafe { core::mem::transmute(ORIG_DRAW_LIST.load(Ordering::Acquire)) };
    let watching = is_viewing()
        && REAL.load(Ordering::Acquire) < 8
        && (VIEWED.load(Ordering::Acquire) < 8 || FULL_VISION.load(Ordering::Acquire));
    if watching {
        let _ = guarded(apply_view);
    }
    unsafe { original() };
    if watching {
        let _ = guarded(restore_view);
    }
}

/// Makes the game rebuild what it shows for the local player (fog, minimap): the routine it runs
/// itself when a player is eliminated.
fn refresh_screen(a: &Addrs) {
    // SAFETY: `RefreshView` takes no arguments; called on the main thread outside the simulation.
    unsafe {
        let refresh: VoidFn = core::mem::transmute(a.refresh);
        refresh();
    }
}

fn announce(a: &Addrs) {
    let real = REAL.load(Ordering::Acquire);
    let viewed = VIEWED.load(Ordering::Acquire);
    let player = if viewed < 8 { viewed } else { real };
    let mut text = if is_observer(a, player) {
        "Viewing the whole match".to_owned()
    } else {
        format!("Viewing {} (player {})", player_name(a, player), player + 1)
    };
    if FULL_VISION.load(Ordering::Acquire) {
        text.push_str(", full vision");
    }
    say(&text);
}

/// Once per game frame: the view keys. Only while viewing and the game window is the one with
/// focus (typing in another program does nothing).
pub fn poll_keys() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    if !is_viewing() {
        return;
    }
    let Some(a) = ADDRS.get() else { return };
    let real = REAL.load(Ordering::Acquire);
    if real >= 8 {
        return;
    }
    if !crate::replayplay::game_has_focus() {
        KEYS_DOWN.store(0, Ordering::Release);
        return;
    }
    let mut down = 0u16;
    for (bit, vk) in WATCHED {
        // SAFETY: a plain key-state query.
        if unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0 {
            down |= bit;
        }
    }
    let pressed = down & !KEYS_DOWN.swap(down, Ordering::AcqRel);
    if pressed == 0 {
        return;
    }
    if pressed & KEY_BOARD != 0 {
        let next = (BOARD.load(Ordering::Acquire) + 1) % 4;
        BOARD.store(next, Ordering::Release);
    }
    if pressed & !KEY_BOARD == 0 {
        return;
    }
    let list = players(a);
    let current = match VIEWED.load(Ordering::Acquire) {
        v if v < 8 => v,
        _ => real,
    };
    let step = |forward: bool| {
        let n = list.len().max(1);
        let to = match list.iter().position(|&p| p == current) {
            Some(at) if forward => (at + 1) % n,
            Some(at) => (at + n - 1) % n,
            // Not one of the players (an observer): the first or the last.
            None if forward => 0,
            None => n - 1,
        };
        list.get(to).copied()
    };
    if pressed & KEY_NEXT != 0 {
        if let Some(p) = step(true) {
            VIEWED.store(p, Ordering::Release);
        }
    }
    if pressed & KEY_PREV != 0 {
        if let Some(p) = step(false) {
            VIEWED.store(p, Ordering::Release);
        }
    }
    if pressed & KEY_HOME != 0 {
        let home = HOME.load(Ordering::Acquire);
        VIEWED.store(if home < 8 && home != real { home } else { NONE }, Ordering::Release);
    }
    if pressed & KEY_FULL != 0 {
        FULL_VISION.fetch_xor(true, Ordering::AcqRel);
    }
    apply_view();
    refresh_screen(a);
    announce(a);
}

/// Back to the real player, nothing watched.
fn reset_view() {
    crate::fogmaps::disarm();
    crate::viewmem::reset();
    VIEWED.store(NONE, Ordering::Release);
    HOME.store(NONE, Ordering::Release);
    FULL_VISION.store(false, Ordering::Release);
    KEYS_DOWN.store(0, Ordering::Release);
    BOARD.store(0, Ordering::Release);
    restore_view();
    REAL.store(NONE, Ordering::Release);
}

/// A line for the log when viewing is switched on, naming the keys.
pub fn log_keys(extra: &str) {
    log(
        W2R_LOG_INFO,
        &format!(
            "viewing keys: [ and ] switch player, Home returns to your own, Insert toggles full vision, backtick steps the scoreboard{extra}"
        ),
    );
}
