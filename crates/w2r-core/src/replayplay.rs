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
use std::sync::atomic::{
    AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
};
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
type SlotRequestFn = unsafe extern "C" fn(*const u8, i32);
type DisplayChatFn = unsafe extern "C" fn(*const u8, u8, u32, i8);
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
static KEYS_DOWN: AtomicU16 = AtomicU16::new(0);
/// The scoreboard under the panel: 0 now, 1 totals, 2 rates, 3 hidden.
static BOARD: AtomicU8 = AtomicU8::new(0);
static PAUSED: AtomicBool = AtomicBool::new(false);
/// Let one turn through while paused.
static STEP: AtomicBool = AtomicBool::new(false);
/// The recording has run out; the game is held there until the player leaves.
static ENDED: AtomicBool = AtomicBool::new(false);
static SPEED: AtomicU8 = AtomicU8::new(NORMAL_SPEED);
/// The tick deadline as playback left it, to tell how far the game moved it on.
static LAST_DEADLINE: AtomicU32 = AtomicU32::new(0);
/// Game time played so far (ms at the game's own pace, whatever the playback speed).
static GAME_MS: AtomicU32 = AtomicU32::new(0);

/// Playback speeds, as multiples of the game's own pace.
const SPEEDS: [f32; 7] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];
const NORMAL_SPEED: u8 = 2;
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
    slot_type: usize,
    team_change: usize,
    deadline: usize,
    display_chat: usize,
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
    /// The next recorded chat line to show.
    next_chat: usize,
    /// Running totals of what each slot did on purpose, by turn.
    actions: Vec<[u32; 8]>,
}

fn log(level: u32, text: &str) {
    crate::session::StartupLog.line(level, "replay", text);
}

/// A queued replay is waiting for the lobby to open so its opponents can be seated.
static PREFILL: AtomicBool = AtomicBool::new(false);

/// The lobby opened: if a replay is queued, its opponents are seated on the first frames.
pub fn on_lobby_enter() {
    PREFILL_SINCE.store(0, Ordering::Release);
    PREFILL_LOGGED.store(false, Ordering::Release);
    PREFILL.store(queued().is_some(), Ordering::Release);
}

/// Once per lobby frame. When a replay is queued and the host is seated, fills the other slots
/// with computer opponents, one for each other player in the recording (its race and team), by
/// feeding the game's own "set slot" handler the packet the slot dropdown would have sent.
pub fn lobby_frame() {
    if !PREFILL.load(Ordering::Acquire) {
        return;
    }
    let _ = guarded(prefill_lobby);
}

/// The lobby slot table: stride, and where the state, race and team sit.
const SLOT_STRIDE: usize = 0x26;
const SLOT_STATE: usize = 8;
const SLOT_TEAM: usize = 10;
const STATE_HUMAN: u8 = 0;
const STATE_COMPUTER: u8 = 4;
/// What the host's slot dropdown sends (the net layer hands it to the game's own handlers, with
/// the sender's net slot, 0 for the host): a slot type request `[0x21, slot, mode]` (mode 0 is
/// Computer) and a team request `[0x20, slot, team]`.
const SLOT_TYPE_PACKET: u8 = 0x21;
const TEAM_PACKET: u8 = 0x20;
const MODE_COMPUTER: u8 = 0;

/// How long after the lobby opens the opponents are kept seated (ms): the game may still be
/// setting its own slots up on the first frames.
const PREFILL_WINDOW_MS: u64 = 4_000;
static PREFILL_SINCE: AtomicU64 = AtomicU64::new(0);
static PREFILL_LOGGED: AtomicBool = AtomicBool::new(false);

fn slot_states(a: &Addrs) -> [u8; 8] {
    let mut out = [0u8; 8];
    for (i, o) in out.iter_mut().enumerate() {
        // SAFETY: the lobby slot table is the start of the lobby block in the game's data section.
        *o = unsafe { *((a.lobby + i * SLOT_STRIDE + SLOT_STATE) as *const u8) };
    }
    out
}

fn prefill_lobby() {
    let (Some(a), Some(cfg)) = (ADDRS.get(), CONFIG.get()) else {
        return;
    };
    let now = crate::chat::now_ms();
    // The host takes its seat on the first frames; wait for it.
    let before = slot_states(a);
    if before[0] != STATE_HUMAN {
        return;
    }
    let since = match PREFILL_SINCE.load(Ordering::Acquire) {
        0 => {
            PREFILL_SINCE.store(now, Ordering::Release);
            now
        }
        t => t,
    };
    if now.saturating_sub(since) > PREFILL_WINDOW_MS {
        PREFILL.store(false, Ordering::Release);
        PREFILL_SINCE.store(0, Ordering::Release);
        PREFILL_LOGGED.store(false, Ordering::Release);
        return;
    }
    let Some(name) = queued() else {
        PREFILL.store(false, Ordering::Release);
        return;
    };
    let Ok(raw) = fs::read(cfg.root.join("replays").join(&name)) else {
        return;
    };
    let Ok(replay) = Replay::parse(&raw) else {
        return;
    };
    let block = &replay.header.lobby;
    if block.len() != LOBBY_LEN {
        return;
    }
    let local = usize::from(replay.header.local).min(7);
    // The recording's other players, in slot order: each becomes a computer in the next slot.
    let others: Vec<u8> = (0..8usize)
        .filter(|&i| i != local)
        .filter(|&i| matches!(block[i * SLOT_STRIDE + SLOT_STATE], 0 | STATE_COMPUTER))
        .map(|i| block[i * SLOT_STRIDE + SLOT_TEAM])
        .collect();
    let mut changed = false;
    for (n, team) in others.iter().enumerate().take(7) {
        let slot = n as u8 + 1;
        if before[usize::from(slot)] == STATE_COMPUTER {
            continue;
        }
        // SAFETY: each handler reads its three packet bytes and the sender (0: the host).
        unsafe {
            let kind = [SLOT_TYPE_PACKET, slot, MODE_COMPUTER];
            let set_type: SlotRequestFn = core::mem::transmute(a.slot_type);
            set_type(kind.as_ptr(), 0);
            let side = [TEAM_PACKET, slot, *team];
            let set_team: SlotRequestFn = core::mem::transmute(a.team_change);
            set_team(side.as_ptr(), 0);
        }
        changed = true;
    }
    if changed && !PREFILL_LOGGED.swap(true, Ordering::AcqRel) {
        log(
            W2R_LOG_INFO,
            &format!(
                "lobby: seating {} computer opponent(s) for {name}; slot states {:?} -> {:?}",
                others.len(),
                before,
                slot_states(a)
            ),
        );
    }
}

/// The folder the recordings live in.
pub fn replays_dir() -> Option<PathBuf> {
    CONFIG.get().map(|c| c.root.join("replays"))
}

/// The simulation fingerprint of the plugins loaded now; a recording plays only on the same one.
pub fn fingerprint() -> Option<[u8; 4]> {
    CONFIG.get().map(|c| c.fingerprint)
}

/// Queues a recording (a file in the replays folder) for the next game, as `w2rpm replays --play`
/// does.
pub fn queue(file: &str) -> std::io::Result<()> {
    let Some(cfg) = CONFIG.get() else {
        return Err(std::io::Error::other("replays are not set up"));
    };
    fs::write(cfg.root.join(MARKER), format!("{file}\n"))
}

/// The recording queued for the next game, if any.
pub fn queued() -> Option<String> {
    let cfg = CONFIG.get()?;
    let text = fs::read_to_string(cfg.root.join(MARKER)).ok()?;
    let name = text.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Takes the queued recording back out.
pub fn unqueue() {
    if let Some(cfg) = CONFIG.get() {
        let _ = fs::remove_file(cfg.root.join(MARKER));
    }
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
    let (Some(display_chat), Some(deadline), Some(slot_type), Some(team_change)) = (
        get("DisplayChatOrGameMessage"),
        get("TickDeadline"),
        get("HandleLobbySlotTypeRequest"),
        get("HandleLobbyTeamChangeRequest"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "replay chat: symbols missing");
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
        slot_type,
        team_change,
        deadline,
        display_chat,
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
                    actions: replay.action_totals(),
                    replay,
                    name,
                    bufs: Default::default(),
                    checked_rng: false,
                    next_chat: 0,
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
                "viewing keys: [ and ] switch player, Home returns to your own, Insert toggles full vision;                  Page Up/Down speed, End pause, Delete step",
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
    // Chat is not in the packets (the game sends it between machines): the lines recorded for
    // this turn or earlier are shown now, by the game's own display function.
    let mut due: Vec<w2r_replay::Chat> = Vec::new();
    while let Some(c) = play.replay.chats.get(play.next_chat) {
        if c.turn > turn {
            break;
        }
        due.push(c.clone());
        play.next_chat += 1;
    }
    if turn % 2000 == 0 {
        log(W2R_LOG_INFO, &format!("playing turn {turn}"));
    }
    drop(guard);
    for c in &due {
        show_chat(a, c);
    }
}

/// Shows a recorded chat line as the game itself would have (the chat plugin's hook sees it too).
fn show_chat(a: &Addrs, chat: &w2r_replay::Chat) {
    let mut text = chat.text.clone();
    text.push(0);
    // SAFETY: `DisplayChatOrGameMessage` takes a NUL-terminated line, a player, a duration and a
    // mode; called on the main thread outside the simulation.
    unsafe {
        let display: DisplayChatFn = core::mem::transmute(a.display_chat);
        display(text.as_ptr(), chat.who, chat.duration_ms, chat.mode as i8);
    }
}

// ---- Viewing -------------------------------------------------------------------------------

const KEY_PREV: u16 = 1;
const KEY_NEXT: u16 = 2;
const KEY_HOME: u16 = 4;
const KEY_FULL: u16 = 8;
const KEY_FASTER: u16 = 16;
const KEY_SLOWER: u16 = 32;
const KEY_PAUSE: u16 = 64;
const KEY_STEP: u16 = 128;
const KEY_BOARD: u16 = 256;
/// `[`, `]`, Home and Insert.
const WATCHED: [(u16, i32); 9] = [
    (KEY_BOARD, 0xC0),
    (KEY_FASTER, 0x21),
    (KEY_SLOWER, 0x22),
    (KEY_PAUSE, 0x23),
    (KEY_STEP, 0x2E),
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

/// What the on-screen panel shows (`hudpanel.rs`).
pub struct Pace {
    pub paused: bool,
    pub ended: bool,
    pub speed: f32,
    /// Game time played, in ms.
    pub game_ms: u32,
    pub turn: usize,
    pub total: usize,
}

pub fn pace() -> Option<Pace> {
    if !is_active() {
        return None;
    }
    let a = ADDRS.get()?;
    let total = PLAY.lock().ok()?.as_ref()?.replay.turns.len();
    // SAFETY: a u32 in the game's data section.
    let turn = unsafe { core::ptr::read_unaligned(a.turns as *const u32) } as usize;
    Some(Pace {
        paused: PAUSED.load(Ordering::Acquire),
        ended: ENDED.load(Ordering::Acquire),
        speed: SPEEDS[SPEED.load(Ordering::Acquire) as usize],
        game_ms: GAME_MS.load(Ordering::Acquire),
        turn: turn.min(total),
        total,
    })
}

/// A player in the match, for the scoreboard.
pub struct Member {
    pub slot: u8,
    /// A person (rather than the computer): only people send commands.
    pub human: bool,
    pub name: String,
}

/// The players in the match (a human or a computer), in slot order. Empty off a replay.
pub fn roster() -> Vec<Member> {
    let Some(a) = ADDRS.get().filter(|_| is_active()) else {
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

/// What `slot` did on purpose per game minute, over the last minute played (or all of it, if
/// shorter). `None` before there is anything to measure.
pub fn actions_per_minute(slot: u8) -> Option<u32> {
    let a = ADDRS.get()?;
    // SAFETY: a u32 in the game's data section.
    let turn = unsafe { core::ptr::read_unaligned(a.turns as *const u32) } as usize;
    let game_ms = GAME_MS.load(Ordering::Acquire);
    if turn < 2 || game_ms == 0 {
        return None;
    }
    let per_turn_ms = (game_ms as usize / turn).max(1);
    let window = (60_000 / per_turn_ms).clamp(1, turn);
    let guard = PLAY.lock().ok()?;
    let play = guard.as_ref()?;
    let slot = usize::from(slot);
    let now = play.actions.get(turn - 1)?.get(slot).copied()?;
    let before = if turn > window {
        play.actions.get(turn - 1 - window)?.get(slot).copied()?
    } else {
        0
    };
    Some((u64::from(now - before) * 60_000 / (window as u64 * per_turn_ms as u64)) as u32)
}

/// The player whose view is on screen, once the replay has started.
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

fn change_speed(by: i8) {
    let now = SPEED.load(Ordering::Acquire) as i8;
    let to = (now + by).clamp(0, SPEEDS.len() as i8 - 1);
    SPEED.store(to as u8, Ordering::Release);
}

/// Called before each network turn: true means the turn is not run yet (paused). Otherwise the
/// game's tick deadline is moved to run turns faster or slower than its own pace: the game adds
/// one frame time after every turn, so each turn's share of that is stretched or squeezed here.
pub fn hold_turn() -> bool {
    if !is_active() {
        return false;
    }
    if let Some(a) = ADDRS.get() {
        let total = PLAY
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|p| p.replay.turns.len()));
        // SAFETY: a u32 in the game's data section.
        let turn = unsafe { core::ptr::read_unaligned(a.turns as *const u32) } as usize;
        if total.is_some_and(|n| turn >= n) {
            // Nothing left to play: hold here (no live commands run) until the player leaves
            // through the game menu.
            ENDED.store(true, Ordering::Release);
            return true;
        }
    }
    if PAUSED.load(Ordering::Acquire) && !STEP.swap(false, Ordering::AcqRel) {
        return true;
    }
    let _ = guarded(|| {
        let Some(a) = ADDRS.get() else { return };
        let speed = SPEEDS[SPEED.load(Ordering::Acquire) as usize];
        // SAFETY: a u32 in the game's data section, touched on the main thread between ticks.
        unsafe {
            let at = a.deadline as *mut u32;
            let now = core::ptr::read_unaligned(at);
            let last = LAST_DEADLINE.load(Ordering::Acquire);
            let step = now.wrapping_sub(last) as i32;
            // No earlier value, or the game reset its clock: leave this one alone.
            if last != 0 && (1..=500).contains(&step) {
                GAME_MS.fetch_add(step as u32, Ordering::AcqRel);
            }
            let moved = if last != 0 && (1..=500).contains(&step) {
                let shift = step as f32 * (1.0 / speed - 1.0);
                now.wrapping_add(shift as i32 as u32)
            } else {
                now
            };
            core::ptr::write_unaligned(at, moved);
            LAST_DEADLINE.store(moved, Ordering::Release);
        }
    });
    false
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

pub(crate) fn game_has_focus() -> bool {
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
    if pressed & (KEY_FASTER | KEY_SLOWER | KEY_PAUSE | KEY_STEP) != 0 {
        if pressed & KEY_FASTER != 0 {
            change_speed(1);
        }
        if pressed & KEY_SLOWER != 0 {
            change_speed(-1);
        }
        if pressed & KEY_PAUSE != 0 {
            PAUSED.fetch_xor(true, Ordering::AcqRel);
        }
        if pressed & KEY_STEP != 0 {
            PAUSED.store(true, Ordering::Release);
            STEP.store(true, Ordering::Release);
        }
    }
    if pressed & KEY_BOARD != 0 {
        let next = (BOARD.load(Ordering::Acquire) + 1) % 4;
        BOARD.store(next, Ordering::Release);
    }
    if pressed & !(KEY_FASTER | KEY_SLOWER | KEY_PAUSE | KEY_STEP | KEY_BOARD) == 0 {
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
    PAUSED.store(false, Ordering::Release);
    BOARD.store(0, Ordering::Release);
    STEP.store(false, Ordering::Release);
    SPEED.store(NORMAL_SPEED, Ordering::Release);
    ENDED.store(false, Ordering::Release);
    LAST_DEADLINE.store(0, Ordering::Release);
    GAME_MS.store(0, Ordering::Release);
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
