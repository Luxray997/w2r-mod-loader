//! The host's in-match kick (docs/ingame-kick-research.md). In a live match the host presses
//! Ctrl+Shift+K for a small panel on the HUD listing the other people: PgUp/PgDn pick one, Delete
//! twice drops them. The drop is the game's own `FUN_0049ef20`, which asks the transport to cut
//! the player off and to tell every other client; each client then removes the player at its next
//! turn through the path an ordinary leave takes. x86 only (`cdecl`).
//!
//! The host's own removal is the one thing the research could not settle, so it is watched: if the
//! player's result is still unset a second after the drop, the host raises the game's "peer left"
//! flag itself. Every step is logged with the turn counter and the game's lockstep mismatch flag.

use core::ffi::c_void;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO, W2R_LOG_WARN};

use crate::chat::now_ms;
use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::hudpanel::BeginFn;
use crate::kickpanel::{Action, Candidate, Confirm, Keys, Panel};
use crate::pe::Image;
use crate::replayplay;
use crate::screenkit::game::{Kit, rgba};
use crate::screenkit::{ALIGN_LEFT, ALIGN_RIGHT, Rect};
use crate::symbols::Symbols;

type ExtentFn = unsafe extern "C" fn() -> f32;
/// `DropNetPeer(netSlot, message, reason)`; a null message shows no chat line.
type DropFn = unsafe extern "C" fn(u32, *const u8, u32);
/// `NetPeerLeft(netSlot)`: the game's "peer left" notification.
type PeerLeftFn = unsafe extern "C" fn(i32);
/// `AlliancesFooter(ctx, x, y, w, h, a, b)`: the Cancel/OK strip of the Alliances dialog.
type FooterFn = unsafe extern "C" fn(*mut c_void, f32, f32, f32, f32, i32, i32);

const FOOTER_SYMBOL: &str = "AlliancesFooter";

const SESSION_IS_HOST: usize = 0x18;
/// The game state while a match is being played.
const STATE_IN_MATCH: u16 = 3;
/// The reason `KickLobbyPeer` passes to the transport.
const REASON_KICKED: u32 = 0x4000_0001;
/// `TurnSlotFlags` bits: in the match, and already dropped.
const FLAG_IN_MATCH: u32 = 0x1_0000;
const FLAG_DROPPED: u32 = 0x4_0000;
/// A lobby seat value meaning "nobody".
const NO_SEAT: u32 = 8;
const NAME_STRIDE: usize = 0x38;
const NAME_MAX: usize = 0x19;
/// How long to wait for the transport to remove the player on the host before doing it.
const SETTLE_MS: u64 = 1000;
const GIVE_UP_MS: u64 = 4000;

const WINDOW_FLAGS: u32 = 0x120;
const WIDTH: f32 = 300.0;
const TOP: f32 = 22.0;
const MARGIN_RIGHT: f32 = 24.0;
const ROW_HEIGHT: f32 = 12.0;
const HEADER_HEIGHT: f32 = 32.0;
const BACKGROUND: u32 = rgba(12, 9, 6, 200);
const PICKED_ROW: u32 = rgba(226, 184, 68, 70);
const DIM: u32 = rgba(140, 130, 112, 255);
const WARN: u32 = rgba(190, 96, 72, 255);

const VK_CONTROL: i32 = 0x11;
const VK_SHIFT: i32 = 0x10;
const VK_K: i32 = 0x4B;
const VK_PRIOR: i32 = 0x21;
const VK_NEXT: i32 = 0x22;
const VK_DELETE: i32 = 0x2E;
const VK_ESCAPE: i32 = 0x1B;

const KEY_TOGGLE: u16 = 1;
const KEY_UP: u16 = 2;
const KEY_DOWN: u16 = 4;
const KEY_KICK: u16 = 8;
const KEY_CLOSE: u16 = 16;

struct Game {
    session: usize,
    lobby_by_net: usize,
    player_by_net: usize,
    names: usize,
    owner: usize,
    turn_flags: usize,
    left_flags: usize,
    result: usize,
    state: usize,
    turns: usize,
    mismatch: usize,
    drop: DropFn,
    peer_left: PeerLeftFn,
    ui_width: ExtentFn,
    kit: Kit,
    /// The menu context the Alliances dialog is drawn in.
    menu_kit: Kit,
}

// SAFETY: plain game function and data addresses, fixed for the session.
unsafe impl Send for Game {}
unsafe impl Sync for Game {}

/// A drop the host has asked for and is watching until the player is gone.
struct Pending {
    slot: u8,
    player: u8,
    name: String,
    asked_at: u64,
    raised: bool,
}

static GAME: OnceLock<Game> = OnceLock::new();
static PANEL: Mutex<Panel> = Mutex::new(Panel::new());
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
static KEYS_DOWN: AtomicU16 = AtomicU16::new(0);
static DIALOG_CONFIRM: Mutex<Confirm> = Mutex::new(Confirm::new());
static FOOTER_ORIGINAL: AtomicUsize = AtomicUsize::new(0);

fn log(level: u32, text: &str) {
    crate::session::StartupLog.line(level, "kick", text);
}

pub fn install(symbols: &Symbols, log: &dyn Log) -> bool {
    let get = |n: &str| symbols.address(n);
    let (
        Some(session),
        Some(lobby_by_net),
        Some(player_by_net),
        Some(names),
        Some(owner),
        Some(turn_flags),
        Some(left_flags),
        Some(result),
        Some(state),
        Some(turns),
        Some(mismatch),
        Some(drop),
        Some(peer_left),
        Some(ui_width),
    ) = (
        get("NetSession"),
        get("LobbySlotByNetSlot"),
        get("NetSlotPlayer"),
        get("PlayerNames"),
        get("PlayerOwner"),
        get("TurnSlotFlags"),
        get("PeerLeftFlags"),
        get("PlayerMatchResult"),
        get("GameStateCurrent"),
        get("NetTurnCounter"),
        get("TurnSyncMismatch"),
        get("DropNetPeer"),
        get("NetPeerLeft"),
        get("HudUiWidth"),
    )
    else {
        log.line(W2R_LOG_ERROR, "host", "host kick: symbols missing");
        return false;
    };
    let (Some(kit), Some(menu_kit)) = (Kit::new_in(symbols, "HudCtx"), Kit::new(symbols)) else {
        log.line(
            W2R_LOG_ERROR,
            "host",
            "host kick: screen-kit symbols missing",
        );
        return false;
    };
    // SAFETY: signatures from Ghidra, build 2818 (docs/ingame-kick-research.md).
    let game = unsafe {
        Game {
            session,
            lobby_by_net,
            player_by_net,
            names,
            owner,
            turn_flags,
            left_flags,
            result,
            state,
            turns,
            mismatch,
            drop: core::mem::transmute::<usize, DropFn>(drop),
            peer_left: core::mem::transmute::<usize, PeerLeftFn>(peer_left),
            ui_width: core::mem::transmute::<usize, ExtentFn>(ui_width),
            kit,
            menu_kit,
        }
    };
    let _ = GAME.set(game);
    log.line(
        W2R_LOG_INFO,
        "host",
        "host kick ready (Ctrl+Shift+K in a match)",
    );
    true
}

/// Hooks the Alliances dialog's footer so the host gets a Kick row per player in the dialog.
pub fn install_alliances(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(target) = symbols.address(FOOTER_SYMBOL) else {
        log.line(W2R_LOG_ERROR, "host", "host kick: AlliancesFooter missing");
        return false;
    };
    // SAFETY: `footer_detour` matches AlliancesFooter's cdecl signature from the symbol table.
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            footer_detour as *mut c_void,
            &format!("host:{FOOTER_SYMBOL}"),
            |original| FOOTER_ORIGINAL.store(original as usize, Ordering::Release),
        )
    };
    match result {
        Ok(_) => {
            log.line(
                W2R_LOG_INFO,
                "host",
                &format!(
                    "hooked {FOOTER_SYMBOL} @ {target:#010x} (host kick in the Alliances dialog)"
                ),
            );
            true
        }
        Err(e) => {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("could not hook {FOOTER_SYMBOL}: {e:?}"),
            );
            false
        }
    }
}

unsafe extern "C" fn footer_detour(
    ctx: *mut c_void,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    a: i32,
    b: i32,
) {
    let original: FooterFn =
        unsafe { core::mem::transmute(FOOTER_ORIGINAL.load(Ordering::Acquire)) };
    if let Err(fault) = guarded(|| draw_dialog(x, y, w, h)) {
        crate::session::host_fault("alliances kick", &fault);
    }
    unsafe { original(ctx, x, y, w, h, a, b) };
}

/// One "name  [Kick]" row per other person, stacked in the empty area above the footer strip
/// (`x, y, w, h` is the strip, in the window's own units). Hosts only, in a live match.
fn draw_dialog(x: f32, y: f32, w: f32, h: f32) {
    let Some(g) = GAME.get() else { return };
    if !g.live_host() {
        return;
    }
    let list = g.candidates();
    if list.is_empty() {
        return;
    }
    let now = now_ms();
    let k = &g.menu_kit;
    let row = h * 0.6;
    let bottom = y - h * 0.3;
    let mut clicked = None;
    let armed = DIALOG_CONFIRM.lock().ok().and_then(|c| c.armed(now));
    k.space_begin();
    let top = bottom - row * (list.len() as f32 + 1.0);
    k.label_coloured_at(
        Rect::new(x + w * 0.04, top, w * 0.92, row),
        "Host: remove a player from the match",
        ALIGN_LEFT,
        DIM,
    );
    for (i, c) in list.iter().enumerate() {
        let ry = top + row * (i as f32 + 1.0);
        k.label_at(
            Rect::new(x + w * 0.04, ry, w * 0.55, row),
            &c.name,
            ALIGN_LEFT,
        );
        let text = if armed == Some(c.net_slot) {
            "Sure?"
        } else {
            "Kick"
        };
        if k.button_at(
            Rect::new(x + w * 0.62, ry, w * 0.34, row * 0.9),
            text,
            false,
        ) {
            clicked = Some(c.clone());
        }
    }
    k.space_end();
    let Some(c) = clicked else { return };
    let confirmed = DIALOG_CONFIRM
        .lock()
        .map(|mut d| d.press(c.net_slot, now))
        .unwrap_or(false);
    if confirmed {
        kick(g, c.net_slot, c.name, now);
    }
}

impl Game {
    /// A live match of ours, hosted by us, not a replay.
    fn live_host(&self) -> bool {
        if replayplay::is_active() {
            return false;
        }
        // SAFETY: the state is a u16 and the session pointer a global in the game's data section;
        // the callers wrap this in `guarded`.
        unsafe {
            if core::ptr::read_unaligned(self.state as *const u16) != STATE_IN_MATCH {
                return false;
            }
            let session = core::ptr::read_unaligned(self.session as *const usize);
            session != 0 && *((session + SESSION_IS_HOST) as *const u8) != 0
        }
    }

    fn u32_at(&self, base: usize, index: usize) -> u32 {
        // SAFETY: per-slot tables of at least 8 entries in the game's data section.
        unsafe { core::ptr::read_unaligned((base + index * 4) as *const u32) }
    }

    fn name(&self, player: u8) -> String {
        // SAFETY: the player records are a fixed table in the game's data section.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                (self.names + usize::from(player) * NAME_STRIDE) as *const u8,
                NAME_MAX,
            )
        };
        let n = bytes.iter().position(|&b| b == 0).unwrap_or(NAME_MAX);
        String::from_utf8_lossy(&bytes[..n]).into_owned()
    }

    fn result(&self, player: u8) -> u8 {
        // SAFETY: one byte per player, 8 of them.
        unsafe { *((self.result + usize::from(player)) as *const u8) }
    }

    /// The people the host could remove: net slots 1..8 (0 is the host) that are seated, in the
    /// match and not already dropped, and are not the computer.
    fn candidates(&self) -> Vec<Candidate> {
        (1..8usize)
            .filter_map(|n| {
                if self.u32_at(self.lobby_by_net, n) & 0xff >= NO_SEAT {
                    return None;
                }
                let flags = self.u32_at(self.turn_flags, n);
                if flags & FLAG_IN_MATCH == 0 || flags & FLAG_DROPPED != 0 {
                    return None;
                }
                let player = (self.u32_at(self.player_by_net, n) & 0xff) as u8;
                // SAFETY: the owner table is 0x20 bytes of the game's data section.
                if player >= 8 || unsafe { *((self.owner + usize::from(player)) as *const u8) } != 0
                {
                    return None;
                }
                Some(Candidate {
                    net_slot: n as u8,
                    name: self.name(player),
                })
            })
            .collect()
    }

    /// The numbers worth having beside every step of a kick.
    fn lockstep(&self) -> String {
        format!(
            "turn {}, mismatch flag {}",
            self.u32_at(self.turns, 0),
            // SAFETY: a byte in the game's data section.
            unsafe { *(self.mismatch as *const u8) }
        )
    }
}

/// Once per game frame: the keys, then any drop still being watched.
pub fn poll() {
    let Some(g) = GAME.get() else { return };
    let _ = guarded(|| poll_inner(g));
}

fn poll_inner(g: &Game) {
    let now = now_ms();
    watch_pending(g, now);
    if !g.live_host() {
        if let Ok(mut p) = PANEL.lock() {
            p.close();
        }
        KEYS_DOWN.store(0, Ordering::Release);
        return;
    }
    let keys = read_keys();
    if !keys.any() {
        return;
    }
    let list = g.candidates();
    let action = match PANEL.lock() {
        Ok(mut p) => p.press(keys, &list, now),
        Err(_) => return,
    };
    if let Action::Kick(slot) = action {
        let name = list
            .iter()
            .find(|c| c.net_slot == slot)
            .map_or_else(String::new, |c| c.name.clone());
        kick(g, slot, name, now);
    }
}

/// The key presses since the last frame; empty when the game is not the focused window.
fn read_keys() -> Keys {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    if !replayplay::game_has_focus() {
        KEYS_DOWN.store(0, Ordering::Release);
        return Keys::default();
    }
    let held = |vk: i32| {
        // SAFETY: a plain key-state query.
        (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
    };
    let mut down = 0u16;
    for (bit, vk) in [
        (KEY_UP, VK_PRIOR),
        (KEY_DOWN, VK_NEXT),
        (KEY_KICK, VK_DELETE),
        (KEY_CLOSE, VK_ESCAPE),
    ] {
        if held(vk) {
            down |= bit;
        }
    }
    if held(VK_K) && held(VK_CONTROL) && held(VK_SHIFT) {
        down |= KEY_TOGGLE;
    }
    let pressed = down & !KEYS_DOWN.swap(down, Ordering::AcqRel);
    Keys {
        toggle: pressed & KEY_TOGGLE != 0,
        up: pressed & KEY_UP != 0,
        down: pressed & KEY_DOWN != 0,
        kick: pressed & KEY_KICK != 0,
        close: pressed & KEY_CLOSE != 0,
    }
}

fn kick(g: &Game, slot: u8, name: String, now: u64) {
    let player = (g.u32_at(g.player_by_net, usize::from(slot)) & 0xff) as u8;
    log(
        W2R_LOG_INFO,
        &format!(
            "host kicks net slot {slot} (player {player}, {name:?}); {}",
            g.lockstep()
        ),
    );
    // SAFETY: the game's own drop; a null message shows no chat line. The host is the only caller.
    unsafe { (g.drop)(u32::from(slot), core::ptr::null(), REASON_KICKED) };
    if let Ok(mut p) = PENDING.lock() {
        *p = Some(Pending {
            slot,
            player,
            name,
            asked_at: now,
            raised: false,
        });
    }
}

/// Follows a drop until the player's result is set (the game removed them), raising the host's
/// own "peer left" if the transport did not.
fn watch_pending(g: &Game, now: u64) {
    let Ok(mut pending) = PENDING.lock() else {
        return;
    };
    let Some(p) = pending.as_mut() else { return };
    let waited = now.saturating_sub(p.asked_at);
    if !g.live_host() {
        log(
            W2R_LOG_INFO,
            &format!("kick of {:?}: the match ended first", p.name),
        );
        *pending = None;
        return;
    }
    if g.result(p.player) != 0 {
        log(
            W2R_LOG_INFO,
            &format!(
                "{:?} removed after {waited} ms (result {}, peer-left raised by host: {}); {}",
                p.name,
                g.result(p.player),
                p.raised,
                g.lockstep()
            ),
        );
        *pending = None;
        return;
    }
    // A flag already set means the transport raised it and the next turn will act on it.
    let flagged = g.u32_at(g.left_flags, usize::from(p.slot)) != 0;
    if !p.raised && !flagged && waited >= SETTLE_MS {
        // SAFETY: the game's own notification, called on the main thread.
        unsafe { (g.peer_left)(i32::from(p.slot)) };
        p.raised = true;
        log(
            W2R_LOG_WARN,
            &format!(
                "{:?} not removed after {waited} ms: raised the host's peer-left; {}",
                p.name,
                g.lockstep()
            ),
        );
    } else if waited >= GIVE_UP_MS {
        log(
            W2R_LOG_WARN,
            &format!(
                "{:?} still in the match after {waited} ms; {}",
                p.name,
                g.lockstep()
            ),
        );
        *pending = None;
    }
}

/// Called by the window hook just before the game starts its `game_hud` window.
///
/// # Safety
/// `ctx` is the context the game is about to begin `game_hud` in; `begin` is the game's own
/// `BeginNamedWindow`.
pub unsafe fn before_hud(ctx: *mut c_void, begin: BeginFn) {
    let Some(g) = GAME.get() else { return };
    let open = PANEL.lock().map(|p| p.is_open()).unwrap_or(false);
    if !open {
        return;
    }
    let result = guarded(|| unsafe {
        if !g.live_host() {
            return;
        }
        let list = g.candidates();
        let (picked, armed) = match PANEL.lock() {
            Ok(p) => (p.selected(), p.armed(now_ms())),
            Err(_) => return,
        };
        let rows = list.len().max(1) as f32;
        let height = HEADER_HEIGHT + ROW_HEIGHT * rows + 6.0;
        let x = ((g.ui_width)() - MARGIN_RIGHT - WIDTH).max(0.0);
        if begin(
            ctx,
            c"kick_panel".as_ptr(),
            x,
            TOP,
            WIDTH,
            height,
            WINDOW_FLAGS,
        ) != 0
        {
            g.kit.space_begin();
            draw(g, &list, picked, armed, x, height);
            g.kit.space_end();
            g.kit.end_window();
        }
    });
    if let Err(fault) = result {
        crate::session::host_fault("kick panel", &fault);
    }
}

fn draw(g: &Game, list: &[Candidate], picked: Option<u8>, armed: Option<u8>, x: f32, height: f32) {
    let kit = &g.kit;
    kit.fill_round(Rect::new(x, TOP, WIDTH, height), 3.0, BACKGROUND);
    kit.label_at(
        Rect::new(6.0, 2.0, WIDTH * 0.5, 14.0),
        "Remove a player",
        ALIGN_LEFT,
    );
    kit.label_coloured_at(
        Rect::new(WIDTH * 0.4, 2.0, WIDTH * 0.6 - 6.0, 14.0),
        "Ctrl+Shift+K closes",
        ALIGN_RIGHT,
        DIM,
    );
    kit.label_coloured_at(
        Rect::new(6.0, 16.0, WIDTH - 12.0, 14.0),
        "PgUp/PgDn pick   Del twice to remove",
        ALIGN_LEFT,
        DIM,
    );
    if list.is_empty() {
        kit.label_coloured_at(
            Rect::new(6.0, HEADER_HEIGHT, WIDTH - 12.0, ROW_HEIGHT),
            "Nobody else to remove",
            ALIGN_LEFT,
            DIM,
        );
        return;
    }
    for (n, c) in list.iter().enumerate() {
        let y = HEADER_HEIGHT + ROW_HEIGHT * n as f32;
        if picked == Some(c.net_slot) {
            kit.fill_rect(
                Rect::new(x + 2.0, TOP + y, WIDTH - 4.0, ROW_HEIGHT),
                PICKED_ROW,
            );
        }
        kit.label_at(
            Rect::new(8.0, y, WIDTH * 0.55, ROW_HEIGHT),
            &c.name,
            ALIGN_LEFT,
        );
        if armed == Some(c.net_slot) {
            kit.label_coloured_at(
                Rect::new(WIDTH * 0.4, y, WIDTH * 0.6 - 6.0, ROW_HEIGHT),
                "Del again to remove",
                ALIGN_RIGHT,
                WARN,
            );
        }
    }
}
