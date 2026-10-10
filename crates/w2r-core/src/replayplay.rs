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
//! Armed by a one-line file `<root>/replay-play.txt` naming a file in `<root>/replays/`, consumed
//! when the next game starts (renamed to `.used`), so a replay never plays twice by accident. The
//! live game must be on the same map; a refusal is logged and the game starts normally. While a
//! replay plays the recorder and the stats reporter stay off. x86 only (`cdecl`).

use core::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
type GatherFn = unsafe extern "C" fn(i32, i32, *mut u32, *mut u32, *mut u32) -> u32;

static ORIG_START: AtomicUsize = AtomicUsize::new(0);
static ORIG_SEED: AtomicUsize = AtomicUsize::new(0);
static ORIG_GATHER: AtomicUsize = AtomicUsize::new(0);
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
    let _ = ADDRS.set(Addrs {
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
            gather,
            gather_detour as *mut c_void,
            "TurnGather",
            &ORIG_GATHER,
            log,
        );
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

/// Stops driving the game; the live game carries on with its own input.
fn finish(reason: &str) {
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
