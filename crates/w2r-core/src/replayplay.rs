//! Replay playback (docs/replay-research.md, "Playback design"). A replay is played as a normal
//! custom game with three overrides, so the game's own start, pacing and simulation do the work:
//!
//! 1. `StartGame` (0x4c4280): the live lobby block is replaced by the recorded one, so every
//!    in-game table (players, races, resources, speed, flags, map text) comes out as it did.
//! 2. `SetGameSeed` (0x4a01f0): the seed the host would take from the clock is the recorded one.
//! 3. `TurnGather` (0x49d130): the game collects this turn's packets from its net layer; when a
//!    turn arrived we overwrite the packet pointers, lengths and "has packet" bits with the
//!    recorded turn. The net layer still paces the turns.
//!
//! Viewing: the screen follows `LocalPlayer` (0x918ccd), the view player (0x922f5a) and the view
//! bit mask (0x922f23). While a replay plays, `[` and `]` cycle the player you watch as, Home goes
//! back to the recorded player and Insert toggles full vision (every player's bit in the mask).
//! The change is applied only while the game's simulation-mode flag (`SetSimMode`, 0x4a01b0) is
//! 0, i.e. for drawing and input, and the real values are put back whenever the flag goes to 1,
//! so the simulation never sees which player is being watched.
//!
//! Armed by a one-line file `<root>/replay-play.txt` naming a file in `<root>/replays/`, consumed
//! when the next game starts (renamed to `.used`), so a replay never plays twice by accident. The
//! live game must be on the same map; a refusal is logged and the game starts normally. While a
//! replay plays the recorder and the stats reporter stay off. x86 only (`cdecl`).

use core::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};
use w2r_replay::Replay;
use w2r_replay::format::{LOBBY_LEN, MAX_PACKET};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::symbols::Symbols;

const BUILD: &str = "2818";
/// `TurnPlayerStatus` bit set while a slot has a packet for the turn.
const HAS_PACKET: u32 = 0x20000;
/// Offset of the map text inside the lobby block (`MpLobbyMapNameText` 0x91657d).
const MAP_AT: usize = 0x91_657d - 0x91_6260;
const MAP_MAX: usize = 64;
pub const MARKER: &str = "replay-play.txt";

type StartFn = unsafe extern "C" fn(i32, *mut c_void, i32) -> u32;
type SeedFn = unsafe extern "C" fn(u32);
type VoidFn = unsafe extern "C" fn();
type SimModeFn = unsafe extern "C" fn(u32) -> u32;
type PushFn = unsafe extern "C" fn(*const u8, i32, u32);
type GatherFn = unsafe extern "C" fn(i32, i32, *mut u32, *mut u32, *mut u32) -> u32;

static ORIG_START: AtomicUsize = AtomicUsize::new(0);
static ORIG_SEED: AtomicUsize = AtomicUsize::new(0);
static ORIG_GATHER: AtomicUsize = AtomicUsize::new(0);
static ORIG_SIM_MODE: AtomicUsize = AtomicUsize::new(0);
static ORIG_DRAW_LIST: AtomicUsize = AtomicUsize::new(0);
/// A sync mismatch was already logged for this replay.
static SYNC_LOGGED: AtomicBool = AtomicBool::new(false);
/// The game's real fog shade map while an all-visible one is swapped in for drawing.
/// The units' real "hidden from" bits while they are recomputed for a computer player's view.
static HIDDEN_SAVED: Mutex<Option<Vec<u8>>> = Mutex::new(None);
static FOG_SAVED: Mutex<Option<(Vec<u8>, Vec<u8>)>> = Mutex::new(None);
/// The fog shade maps are at most 128 x 128 bytes.
const FOG_LEN: usize = 0x4000;
const NONE: u8 = 0xff;
/// The player the recorded game was played as; `NONE` until the first turn.
static REAL: AtomicU8 = AtomicU8::new(NONE);
/// The player being watched; `NONE` means the recorded one.
static VIEWED: AtomicU8 = AtomicU8::new(NONE);
static FULL_VISION: AtomicBool = AtomicBool::new(false);
/// Which watched keys were down on the previous frame.
static KEYS_DOWN: AtomicU8 = AtomicU8::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static CONFIG: OnceLock<Config> = OnceLock::new();
static ADDRS: OnceLock<Addrs> = OnceLock::new();
static PLAY: Mutex<Option<Play>> = Mutex::new(None);
/// The live lobby block, put back when the game that played a replay ends.
static RESTORE: Mutex<Option<Vec<u8>>> = Mutex::new(None);

struct Config {
    root: PathBuf,
    fingerprint: [u8; 4],
}

struct Addrs {
    recompute_hidden: usize,
    fog_vis: usize,
    fog_explored: usize,
    relations: usize,
    fog: usize,
    refresh: usize,
    sync: usize,
    local: usize,
    view_player: usize,
    view_mask: usize,
    owner: usize,
    names: usize,
    push: usize,
    seed: usize,
    lobby: usize,
    turns: usize,
    rng: usize,
}

struct Play {
    replay: Replay,
    name: String,
    /// Owns the packet bytes the game reads for the current turn.
    bufs: [Vec<u8>; 8],
    checked_rng: bool,
}

fn log(level: u32, text: &str) {
    crate::session::StartupLog.line(level, "replay", text);
}

/// The player the replay was recorded as (`0xff` until its first turn).
pub fn real_player() -> u8 {
    REAL.load(Ordering::Acquire)
}

pub fn init(root: &Path, fingerprint: [u8; 4]) {
    let _ = CONFIG.set(Config {
        root: root.to_path_buf(),
        fingerprint,
    });
}

/// True while a replay is driving the game; the recorder and the stats reporter stay off.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let get = |name: &str| symbols.address(name);
    let (Some(start), Some(seed), Some(gather)) =
        (get("StartGame"), get("SetGameSeed"), get("TurnGather"))
    else {
        log.line(W2R_LOG_ERROR, "host", "replay playback: symbols missing");
        return false;
    };
    let (Some(seed_at), Some(lobby), Some(turns), Some(rng)) = (
        get("GameSeed"),
        get("MpLobbySlots"),
        get("NetTurnCounter"),
        get("SimRngState"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "replay playback: symbols missing");
        return false;
    };
    let (Some(sim_mode), Some(local), Some(view_player), Some(view_mask)) = (
        get("SetSimMode"),
        get("LocalPlayer"),
        get("ViewPlayer"),
        get("ViewMask"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "replay viewing: symbols missing");
        return false;
    };
    let (Some(recompute_hidden), Some(fog_vis)) = (get("RecomputeHidden"), get("FogVisibilityPtr"))
    else {
        log.line(W2R_LOG_ERROR, "host", "replay viewing: symbols missing");
        return false;
    };
    let Some(fog_explored) = get("FogExploredPtr") else {
        log.line(W2R_LOG_ERROR, "host", "replay viewing: symbols missing");
        return false;
    };
    let (Some(fog), Some(relations)) = (get("FogShadePtr"), get("RelationColours")) else {
        log.line(W2R_LOG_ERROR, "host", "replay viewing: symbols missing");
        return false;
    };
    let (Some(refresh), Some(draw_list), Some(sync)) = (
        get("RefreshView"),
        get("BuildDrawList"),
        get("TurnSyncMismatch"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "replay viewing: symbols missing");
        return false;
    };
    let (Some(owner), Some(names), Some(push)) = (
        get("PlayerOwner"),
        get("PlayerNames"),
        get("PushMapMessage"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "replay viewing: symbols missing");
        return false;
    };
    let _ = ADDRS.set(Addrs {
        recompute_hidden,
        fog_vis,
        fog_explored,
        relations,
        fog,
        refresh,
        sync,
        local,
        view_player,
        view_mask,
        owner,
        names,
        push,
        seed: seed_at,
        lobby,
        turns,
        rng,
    });
    let mut ok = true;
    // SAFETY: each detour matches its target's cdecl signature (Ghidra, build 2818; the call
    // sites clean the stack themselves) and calls the original with the same arguments.
    unsafe {
        ok &= hook(
            image,
            start,
            start_detour as *mut c_void,
            "StartGame",
            &ORIG_START,
            log,
        );
        ok &= hook(
            image,
            seed,
            seed_detour as *mut c_void,
            "SetGameSeed",
            &ORIG_SEED,
            log,
        );
        ok &= hook(
            image,
            sim_mode,
            sim_mode_detour as *mut c_void,
            "SetSimMode",
            &ORIG_SIM_MODE,
            log,
        );
        ok &= hook(
            image,
            draw_list,
            draw_list_detour as *mut c_void,
            "BuildDrawList",
            &ORIG_DRAW_LIST,
            log,
        );
        ok &= hook(
            image,
            gather,
            gather_detour as *mut c_void,
            "TurnGather",
            &ORIG_GATHER,
            log,
        );
    }
    ok &= crate::fogtrack::install(image, symbols, log);
    if !crate::viewmem::init(symbols) {
        log.line(W2R_LOG_ERROR, "host", "replay viewing: symbols missing");
        ok = false;
    }
    ok
}

unsafe fn hook(
    image: &Image,
    target: usize,
    detour: *mut c_void,
    name: &str,
    original: &'static AtomicUsize,
    log: &dyn Log,
) -> bool {
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            detour,
            &format!("host:{name}"),
            |o| original.store(o as usize, Ordering::Release),
        )
    };
    match result {
        Ok(_) => {
            log.line(
                W2R_LOG_INFO,
                "host",
                &format!("hooked {name} @ {target:#010x} (replay)"),
            );
            true
        }
        Err(e) => {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("could not hook {name}: {e:?}"),
            );
            false
        }
    }
}

// ---- StartGame -----------------------------------------------------------------------------

unsafe extern "C" fn start_detour(loaded: i32, map: *mut c_void, len: i32) -> u32 {
    let original: StartFn = unsafe { core::mem::transmute(ORIG_START.load(Ordering::Acquire)) };
    if let Err(fault) = guarded(|| prepare(loaded)) {
        crate::session::host_fault("replay start", &fault);
    }
    unsafe { original(loaded, map, len) }
}

/// Reads the live lobby block (for the recorder) and arms playback if a replay is queued.
fn prepare(loaded: i32) {
    let Some(a) = ADDRS.get() else { return };
    // SAFETY: the lobby block is a fixed region of the game's data section.
    let live = unsafe { core::slice::from_raw_parts(a.lobby as *const u8, LOBBY_LEN) }.to_vec();
    crate::replayrec::note_lobby(live.clone());
    if loaded == 0 {
        arm(a, live);
    }
}

fn arm(a: &Addrs, live: Vec<u8>) {
    let Some(cfg) = CONFIG.get() else { return };
    let marker = cfg.root.join(MARKER);
    let Ok(text) = fs::read_to_string(&marker) else {
        return;
    };
    // Consumed whatever happens next: a replay that cannot start must not retry forever.
    let _ = fs::rename(&marker, cfg.root.join(format!("{MARKER}.used")));
    let name = text.trim().to_string();
    match load(cfg, &name, &live) {
        Ok(replay) => {
            log(
                W2R_LOG_INFO,
                &format!(
                    "playing {name}: map {:?}, {} turns, game seed {:#010x}",
                    replay.header.map,
                    replay.turns.len(),
                    replay.header.game_seed().unwrap_or(0)
                ),
            );
            // SAFETY: as in `prepare`; the recorded block is exactly LOBBY_LEN bytes (checked).
            unsafe {
                core::ptr::copy_nonoverlapping(
                    replay.header.lobby.as_ptr(),
                    a.lobby as *mut u8,
                    LOBBY_LEN,
                );
            }
            // In a lobby game the seed was chosen (and sent) before `StartGame`, so `SetGameSeed`
            // is not called again: set it here. The hook still covers a host that takes the clock.
            if let Some(seed) = replay.header.game_seed() {
                // SAFETY: `GameSeed` is a u32 in the game's data section.
                unsafe { core::ptr::write_unaligned(a.seed as *mut u32, seed) };
            }
            if let Ok(mut r) = RESTORE.lock() {
                *r = Some(live);
            }
            if let Ok(mut p) = PLAY.lock() {
                *p = Some(Play {
                    replay,
                    name,
                    bufs: Default::default(),
                    checked_rng: false,
                });
            }
            crate::fogmaps::arm();
            ACTIVE.store(true, Ordering::Release);
        }
        Err(why) => log(
            W2R_LOG_ERROR,
            &format!("not playing {name:?}: {why}; starting a normal game"),
        ),
    }
}

fn map_text(block: &[u8]) -> &[u8] {
    let text = block.get(MAP_AT..MAP_AT + MAP_MAX).unwrap_or(&[]);
    &text[..text.iter().position(|&b| b == 0).unwrap_or(text.len())]
}

fn load(cfg: &Config, name: &str, live: &[u8]) -> Result<Replay, String> {
    if name.is_empty() || name.contains(['/', '\\', ':']) || !name.ends_with(".w2rr") {
        return Err("the marker must name a .w2rr file in the replays folder".into());
    }
    let raw = fs::read(cfg.root.join("replays").join(name))
        .map_err(|e| format!("cannot read it ({e})"))?;
    let replay = Replay::parse(&raw).map_err(|e| e.to_string())?;
    let h = &replay.header;
    if h.lobby.len() != LOBBY_LEN {
        return Err("it was recorded before playback was supported (no start state)".into());
    }
    if h.build != BUILD {
        return Err(format!("it is for game build {}, this is {BUILD}", h.build));
    }
    if h.fingerprint != cfg.fingerprint {
        return Err("it was recorded with a different set of simulation plugins".into());
    }
    if h.game_seed().is_none() {
        return Err("it has no game seed".into());
    }
    if replay.turns.is_empty() {
        return Err("it has no turns".into());
    }
    if map_text(live) != map_text(&h.lobby) {
        return Err(format!(
            "it was played on {:?}; start the game on that map",
            String::from_utf8_lossy(map_text(&h.lobby))
        ));
    }
    Ok(replay)
}

// ---- SetGameSeed ---------------------------------------------------------------------------

unsafe extern "C" fn seed_detour(seed: u32) {
    let original: SeedFn = unsafe { core::mem::transmute(ORIG_SEED.load(Ordering::Acquire)) };
    let mut use_seed = seed;
    if is_active() {
        if let Ok(p) = PLAY.lock() {
            if let Some(s) = p.as_ref().and_then(|p| p.replay.header.game_seed()) {
                use_seed = s;
                log(
                    W2R_LOG_INFO,
                    &format!("game seed {seed:#010x} replaced by the recorded {s:#010x}"),
                );
            }
        }
    }
    unsafe { original(use_seed) }
}

// ---- TurnGather ----------------------------------------------------------------------------

unsafe extern "C" fn gather_detour(
    a: i32,
    slots: i32,
    ptrs: *mut u32,
    lens: *mut u32,
    status: *mut u32,
) -> u32 {
    let original: GatherFn = unsafe { core::mem::transmute(ORIG_GATHER.load(Ordering::Acquire)) };
    let done = unsafe { original(a, slots, ptrs, lens, status) };
    if done == 1 && is_active() {
        // SAFETY: the three arrays are the game's own 8-entry tables it just filled.
        if let Err(fault) = guarded(|| unsafe { inject(ptrs, lens, status) }) {
            finish("a fault while injecting");
            crate::session::host_fault("replay playback", &fault);
        }
    }
    done
}

/// # Safety
/// `ptrs`, `lens` and `status` must point to the game's tables of 8 `u32`.
unsafe fn inject(ptrs: *mut u32, lens: *mut u32, status: *mut u32) {
    let Some(a) = ADDRS.get() else { return };
    let Ok(mut guard) = PLAY.lock() else { return };
    let Some(play) = guard.as_mut() else { return };
    // SAFETY: symbol-table addresses in the game's data section.
    let (turn, rng) = unsafe {
        (
            core::ptr::read_unaligned(a.turns as *const u32),
            core::ptr::read_unaligned(a.rng as *const u32),
        )
    };
    if REAL.load(Ordering::Acquire) >= 8 {
        // SAFETY: a byte in the game's data section; nothing is applied yet, so this is the
        // player the recorded game was played as.
        let local = unsafe { *(a.local as *const u8) };
        if local < 8 {
            REAL.store(local, Ordering::Release);
            log(
                W2R_LOG_INFO,
                "viewing keys: [ and ] switch player, Home returns to your own, Insert toggles full vision",
            );
        }
    }
    // SAFETY: a u32 in the game's data section.
    if unsafe { core::ptr::read_unaligned(a.sync as *const u32) } != 0
        && !SYNC_LOGGED.swap(true, Ordering::AcqRel)
    {
        log(
            W2R_LOG_ERROR,
            &format!("the game reports a sync mismatch around turn {turn}"),
        );
    }
    if !play.checked_rng {
        play.checked_rng = true;
        match play.replay.header.rng_at_start() {
            Some(want) if want == rng => log(
                W2R_LOG_INFO,
                &format!("simulation RNG before turn {turn} is {rng:#010x}, as recorded"),
            ),
            Some(want) => log(
                W2R_LOG_ERROR,
                &format!(
                    "simulation RNG before turn {turn} is {rng:#010x}, recorded {want:#010x}: \
                     the start state differs, expect a desync"
                ),
            ),
            None => {}
        }
    }
    let Some(rec) = play.replay.turns.get(turn as usize) else {
        let (n, name) = (play.replay.turns.len(), play.name.clone());
        drop(guard);
        finish(&format!("{name} ended after {n} turns"));
        return;
    };
    if rec.turn != turn {
        let want = rec.turn;
        drop(guard);
        finish(&format!(
            "turn numbering broke: wanted {turn}, file has {want}"
        ));
        return;
    }
    for slot in 0..8usize {
        // SAFETY: slot < 8.
        unsafe {
            match rec.packets.iter().find(|(s, _)| usize::from(*s) == slot) {
                Some((_, data)) if data.len() <= MAX_PACKET => {
                    play.bufs[slot].clear();
                    play.bufs[slot].extend_from_slice(data);
                    *ptrs.add(slot) = play.bufs[slot].as_ptr() as u32;
                    *lens.add(slot) = data.len() as u32;
                    *status.add(slot) |= HAS_PACKET;
                }
                _ => {
                    *lens.add(slot) = 0;
                    *status.add(slot) &= !HAS_PACKET;
                }
            }
        }
    }
    if turn % 2000 == 0 {
        log(W2R_LOG_INFO, &format!("playing turn {turn}"));
    }
}

// ---- Viewing -------------------------------------------------------------------------------

const KEY_PREV: u8 = 1;
const KEY_NEXT: u8 = 2;
const KEY_HOME: u8 = 4;
const KEY_FULL: u8 = 8;
/// `[`, `]`, Home and Insert.
const WATCHED: [(u8, i32); 4] = [
    (KEY_PREV, 0xDB),
    (KEY_NEXT, 0xDD),
    (KEY_HOME, 0x24),
    (KEY_FULL, 0x2D),
];
/// A player's record in `PlayerNames` (0x91ada8): stride, and the longest name.
const NAME_STRIDE: usize = 0x38;
const NAME_MAX: usize = 0x19;

/// # Safety
/// `a` holds the game's addresses; call inside `guarded`.
unsafe fn write_view(a: &Addrs, player: u8, mask: u8) {
    unsafe {
        core::ptr::write_volatile(a.local as *mut u8, player);
        core::ptr::write_volatile(a.view_player as *mut u8, player);
        core::ptr::write_volatile(a.view_mask as *mut u8, mask);
    }
}

/// The recorded player's own view: what the game itself has when nobody watches anyone else.
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
    if is_active() {
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
    let watching = is_active()
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

/// Players that are in the match (a human or a computer), by index.
fn players(a: &Addrs) -> Vec<u8> {
    (0..8u8)
        // SAFETY: the owner table is 0x20 bytes of the game's data section.
        .filter(|&i| matches!(unsafe { *((a.owner + usize::from(i)) as *const u8) }, 0 | 1))
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

fn say(a: &Addrs, text: &str) {
    let mut c = text.as_bytes().to_vec();
    c.push(0);
    // SAFETY: `PushMapMessage` copies the NUL-terminated text it is given; called on the main thread.
    unsafe {
        let push: PushFn = core::mem::transmute(a.push);
        push(c.as_ptr(), 1, 3000);
    }
}

fn announce(a: &Addrs) {
    let real = REAL.load(Ordering::Acquire);
    let viewed = VIEWED.load(Ordering::Acquire);
    let player = if viewed < 8 { viewed } else { real };
    let mut text = format!("Viewing {} (player {})", player_name(a, player), player + 1);
    if FULL_VISION.load(Ordering::Acquire) {
        text.push_str(", full vision");
    }
    say(a, &text);
}

fn game_has_focus() -> bool {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    // SAFETY: plain queries about the foreground window.
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        pid == GetCurrentProcessId()
    }
}

/// Once per game frame: the viewing keys. Only while a replay is playing and the game window is
/// the one with focus (typing in another program does nothing).
pub fn poll() {
    if !is_active() {
        return;
    }
    let _ = guarded(poll_keys);
}

fn poll_keys() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    let Some(a) = ADDRS.get() else { return };
    let real = REAL.load(Ordering::Acquire);
    if real >= 8 {
        return;
    }
    if !game_has_focus() {
        KEYS_DOWN.store(0, Ordering::Release);
        return;
    }
    let mut down = 0u8;
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
    let list = players(a);
    let current = match VIEWED.load(Ordering::Acquire) {
        v if v < 8 => v,
        _ => real,
    };
    let step = |forward: bool| {
        let at = list.iter().position(|&p| p == current).unwrap_or(0);
        let n = list.len().max(1);
        let to = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
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
        VIEWED.store(NONE, Ordering::Release);
    }
    if pressed & KEY_FULL != 0 {
        FULL_VISION.fetch_xor(true, Ordering::AcqRel);
    }
    apply_view();
    refresh_screen(a);
    announce(a);
}

/// Back to the recorded player, nothing watched.
fn reset_view() {
    SYNC_LOGGED.store(false, Ordering::Release);
    crate::fogmaps::disarm();
    crate::viewmem::reset();
    VIEWED.store(NONE, Ordering::Release);
    FULL_VISION.store(false, Ordering::Release);
    KEYS_DOWN.store(0, Ordering::Release);
    restore_view();
    REAL.store(NONE, Ordering::Release);
}

/// Stops driving the game; the live game carries on with its own input.
fn finish(reason: &str) {
    reset_view();
    ACTIVE.store(false, Ordering::Release);
    if let Ok(mut p) = PLAY.lock() {
        *p = None;
    }
    log(W2R_LOG_INFO, &format!("replay stopped: {reason}"));
}

/// `RunGame` returned: put the live lobby block back.
pub fn on_run_game_return() {
    if is_active() {
        finish("the match ended");
    }
    let Some(live) = RESTORE.lock().ok().and_then(|mut r| r.take()) else {
        return;
    };
    if let Some(a) = ADDRS.get() {
        // SAFETY: as in `prepare`; `live` was read from the same region.
        unsafe { core::ptr::copy_nonoverlapping(live.as_ptr(), a.lobby as *mut u8, live.len()) };
    }
}
