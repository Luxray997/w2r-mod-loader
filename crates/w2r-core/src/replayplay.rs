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
//! Viewing (which player is on screen, full vision, per-player fog) is `viewer.rs`, shared with the
//! observer; this module switches it on for the replay and adds the playback keys (speed, pause,
//! step) and the camera follow.
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

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO, W2R_LOG_WARN};
use w2r_replay::Replay;
use w2r_replay::format::{LOBBY_LEN, MAX_PACKET, SLOT_BACKUP_LEN};

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
type SyncCheckFn = unsafe extern "C" fn(*const u8) -> u8;
type GatherFn = unsafe extern "C" fn(i32, i32, *mut u32, *mut u32, *mut u32) -> u32;

static ORIG_START: AtomicUsize = AtomicUsize::new(0);
static ORIG_SEED: AtomicUsize = AtomicUsize::new(0);
static ORIG_GATHER: AtomicUsize = AtomicUsize::new(0);
static ORIG_SYNC_CHECK: AtomicUsize = AtomicUsize::new(0);
/// Sync records that disagreed with the game's own hashes during this replay.
static SYNC_BAD: AtomicU32 = AtomicU32::new(0);
/// The map view follows the recorded player's camera (while that player is the one watched).
static FOLLOW: AtomicBool = AtomicBool::new(true);
/// Live observing: the auto-director picks the player to watch (End toggles).
static AUTO: AtomicBool = AtomicBool::new(false);
static DIRECTOR: Mutex<Option<crate::director::Director>> = Mutex::new(None);
/// Which playback keys were down on the previous frame.
static KEYS_DOWN: AtomicU16 = AtomicU16::new(0);
static PAUSED: AtomicBool = AtomicBool::new(false);
/// Let one turn through while paused.
static STEP: AtomicBool = AtomicBool::new(false);
/// The recording has run out; the game is held there until the player leaves.
static ENDED: AtomicBool = AtomicBool::new(false);
static SPEED: AtomicU8 = AtomicU8::new(NORMAL_SPEED);
/// The turn a battle jump is fast-forwarding to; `NO_SEEK` when none.
static SEEK_TO: AtomicU32 = AtomicU32::new(NO_SEEK);
const NO_SEEK: u32 = u32::MAX;
/// The battle being jumped to: `index << 16 | tile x << 8 | tile y`.
static SEEK_INFO: AtomicU32 = AtomicU32::new(0);
/// Frames left of easing the view to the battle after arriving, and where (map pixels).
static GOTO_FRAMES: AtomicU32 = AtomicU32::new(0);
static GOTO_X: AtomicU32 = AtomicU32::new(0);
static GOTO_Y: AtomicU32 = AtomicU32::new(0);
/// A jump runs the turns this many times faster than normal; the frame rate and the game's own
/// work per turn decide how fast that really is.
const SEEK_SPEED: f32 = 200.0;
/// A jump lands this many turns before the battle starts (about six seconds) so it is seen coming.
const LEAD_IN: u32 = 100;
/// Bars in the timeline graph.
const GRAPH_BUCKETS: usize = 70;
const GOTO_FRAME_COUNT: u32 = 90;
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
    local: usize,
    seed: usize,
    lobby: usize,
    turns: usize,
    rng: usize,
    backup: usize,
    sync_init: usize,
    cam_x: usize,
    cam_y: usize,
    view_w: usize,
    view_h: usize,
    clamp_camera: usize,
    /// The map's side in tiles, a u16.
    map_side: usize,
    /// `SelectedMapPath`: the file the game loaded the map from, a C string.
    map_path: usize,
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
    /// The fights found in the recorded combat events, and the timeline bar heights (empty when
    /// the recording has no events).
    battles: Vec<w2r_replay::heat::Battle>,
    graph: Vec<u8>,
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
    // `header.local` is the position after the game's random start positions; the block is the
    // entry order, so find the recording player's entry there.
    let local = replay
        .local_entry()
        .unwrap_or_else(|| usize::from(replay.header.local))
        .min(7);
    // The recording's other players, in slot order: each becomes a computer in the next slot.
    let others: Vec<u8> = (0..8usize)
        .filter(|&i| i != local)
        .filter(|&i| matches!(block[i * SLOT_STRIDE + SLOT_STATE], 0 | STATE_COMPUTER))
        .map(|i| block[i * SLOT_STRIDE + SLOT_TEAM])
        .collect();
    let mut changed = false;
    // The host's own seat takes the recorded player's team, so the lobby shows the same sides.
    let own_team = block[local * SLOT_STRIDE + SLOT_TEAM];
    // SAFETY: the live lobby block is a fixed region of the game's data section; the handler reads
    // its three packet bytes and the sender (0: the host).
    unsafe {
        let live_team = *((a.lobby + SLOT_TEAM) as *const u8);
        if live_team != own_team {
            let side = [TEAM_PACKET, 0, own_team];
            let set_team: SlotRequestFn = core::mem::transmute(a.team_change);
            set_team(side.as_ptr(), 0);
            changed = true;
        }
    }
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

/// Queues a recording (a file in the replays folder) for the next game, as `w2r-loader replays --play`
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
    let (Some(seed_at), Some(lobby), Some(turns), Some(rng), Some(backup), Some(sync_init), Some(sync_check)) = (
        get("GameSeed"),
        get("MpLobbySlots"),
        get("NetTurnCounter"),
        get("SimRngState"),
        get("SlotBackup"),
        get("SyncRingInit"),
        get("SyncCheck"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "replay playback: symbols missing");
        return false;
    };
    let Some(local) = get("LocalPlayer") else {
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
    let (Some(cam_x), Some(cam_y), Some(view_w), Some(view_h), Some(clamp_camera), Some(map_side), Some(map_path)) = (
        get("CameraScrollX"),
        get("CameraScrollY"),
        get("ViewportWidth"),
        get("ViewportHeight"),
        get("ClampCamera"),
        get("MapSide"),
        get("SelectedMapPath"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "replay follow view: symbols missing");
        return false;
    };
    let _ = ADDRS.set(Addrs {
        slot_type,
        team_change,
        deadline,
        display_chat,
        local,
        seed: seed_at,
        lobby,
        turns,
        rng,
        backup,
        sync_init,
        cam_x,
        cam_y,
        view_w,
        view_h,
        clamp_camera,
        map_side,
        map_path,
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
        ok &= hook(
            image,
            sync_check,
            sync_check_detour as *mut c_void,
            "SyncCheck",
            &ORIG_SYNC_CHECK,
            log,
        );
    }
    // The view system (hooks, per-player fog, minimap memory) is shared with the observer.
    ok &= crate::viewer::install(image, symbols, log);
    ok
}

pub(crate) unsafe fn hook(
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
    // SAFETY: the game's slot backup, a fixed region of its data section.
    let backup = unsafe { core::slice::from_raw_parts(a.backup as *const u8, SLOT_BACKUP_LEN) }.to_vec();
    crate::replayrec::note_lobby(live.clone(), backup);
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
            // Before the game picks random start positions it copies a backup of the slots (outside
            // the lobby block) over the lobby's; without the recorded one the local lobby's players
            // would replace the recorded ones.
            match replay.slot_backup() {
                Some((recorded, backup)) => {
                    log(
                        W2R_LOG_INFO,
                        &format!(
                            "slot backup: {}",
                            if recorded { "recorded" } else { "taken from the start state" }
                        ),
                    );
                    // SAFETY: the game's slot backup, SLOT_BACKUP_LEN bytes in its data section.
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            backup.as_ptr(),
                            a.backup as *mut u8,
                            SLOT_BACKUP_LEN,
                        )
                    };
                }
                None => log(
                    W2R_LOG_WARN,
                    "slot backup: none recorded; players may start in other places",
                ),
            }
            if let Ok(mut r) = RESTORE.lock() {
                *r = Some(live);
            }
            if let Ok(mut p) = PLAY.lock() {
                let total = replay.turns.len() as u32;
                let graph = if replay.events.is_empty() {
                    Vec::new()
                } else {
                    w2r_replay::heat::graph(&w2r_replay::heat::intensity(
                        &replay.events,
                        total,
                        GRAPH_BUCKETS,
                    ))
                };
                *p = Some(Play {
                    battles: w2r_replay::heat::detect(&replay.events),
                    graph,
                    actions: replay.action_totals(),
                    replay,
                    name,
                    bufs: Default::default(),
                    checked_rng: false,
                    next_chat: 0,
                });
            }
            crate::viewer::enable();
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
        // Not fatal: the recording may still play, but a simulation plugin that differs from the
        // recorded set can make it drift, so the player is told rather than stopped.
        log(
            W2R_LOG_WARN,
            &format!(
                "{name} was recorded with different simulation plugins (recorded {:02x?}, loaded {:02x?}); playing anyway, it may desync",
                h.fingerprint, cfg.fingerprint
            ),
        );
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

// ---- SyncCheck -----------------------------------------------------------------------------

/// The game compares a sync record (`[0x18, ring << 4 | kind, hash lo, hash hi]`) in a turn's
/// packet with the hash it made itself, and on a difference treats the sender as out of sync.
/// While a replay plays that is only advice: one kind of hash includes values the simulation does
/// not depend on, so the units can agree turn after turn while that hash differs, and acting on
/// it ends the match. The difference is logged and the record is accepted.
unsafe extern "C" fn sync_check_detour(record: *const u8) -> u8 {
    let original: SyncCheckFn =
        unsafe { core::mem::transmute(ORIG_SYNC_CHECK.load(Ordering::Acquire)) };
    let ok = unsafe { original(record) };
    if ok != 0 || !is_active() {
        return ok;
    }
    let n = SYNC_BAD.fetch_add(1, Ordering::AcqRel) + 1;
    if n <= 6 {
        // SAFETY: the game passes a pointer to the four bytes of the record.
        let bytes = unsafe { core::slice::from_raw_parts(record, 4) };
        let turn = ADDRS.get().map_or(0, |a| unsafe {
            core::ptr::read_unaligned(a.turns as *const u32)
        });
        log(
            W2R_LOG_WARN,
            &format!(
                "sync record differs at turn {turn} (ring {} kind {} hash {:#06x}); accepted,                  the replay goes on",
                bytes[1] >> 4,
                bytes[1] & 0xf,
                u16::from_le_bytes([bytes[2], bytes[3]])
            ),
        );
    }
    1
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

/// How many draws of the game's generator take `from` to `to` (at most 64), if it is that few.
fn draws_apart(from: u32, to: u32) -> Option<u32> {
    let mut s = from;
    for n in 0..=64 {
        if s == to {
            return Some(n);
        }
        s = s.wrapping_mul(0x015a_4e35).wrapping_add(1);
    }
    None
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
    if crate::viewer::real() >= 8 {
        // SAFETY: a byte in the game's data section; nothing is applied yet, so this is the
        // player the recorded game was played as.
        let local = unsafe { *(a.local as *const u8) };
        if local < 8 {
            // Start as the player who made the recording, not the seat this game gave the viewer.
            crate::viewer::begin(local, play.replay.header.local);
            crate::viewer::log_keys(
                ", semicolon follows the recorded player's camera; plus/minus speed, End pause, Delete step",
            );
        }
    }
    if !play.checked_rng {
        play.checked_rng = true;
        match play.replay.header.rng_at_start() {
            Some(want) if want == rng => log(
                W2R_LOG_INFO,
                &format!("simulation RNG before turn {turn} is {rng:#010x}, as recorded"),
            ),
            Some(want) => {
                // The start-up differs a little between the recorded machine (often a client) and
                // this one (the host of the replay game): usually by a draw or two that nothing in
                // the simulation depends on. Put the recorded state back so the turns replay from
                // the same random numbers.
                let apart = match (draws_apart(rng, want), draws_apart(want, rng)) {
                    (Some(n), _) => format!("{n} draw(s) behind the recording"),
                    (None, Some(n)) => format!("{n} draw(s) ahead of the recording"),
                    _ => "unrelated to the recording".to_string(),
                };
                log(
                    W2R_LOG_WARN,
                    &format!(
                        "simulation RNG before turn {turn} is {rng:#010x}, recorded {want:#010x} \
                         ({apart}); setting the recorded state"
                    ),
                );
                // SAFETY: the simulation RNG is a u32 in the game's data section.
                unsafe { core::ptr::write_unaligned(a.rng as *mut u32, want) };
                // The game's sync check keeps a ring of state hashes, the first made while the
                // match was set up (one kind of hash includes the RNG state). The recorded turns
                // carry the original machine's hashes, so make the first one again from the state
                // just restored.
                // SAFETY: the sync ring initialiser takes nothing and returns nothing (Ghidra,
                // build 2818); the state it hashes is the game's own.
                unsafe {
                    core::mem::transmute::<usize, unsafe extern "C" fn()>(a.sync_init)()
                };
                log(W2R_LOG_INFO, "sync check: first hash made again from the recorded state");
            }
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

// ---- Observing a live match ----------------------------------------------------------------

/// The local player watches the match that is running (a person on the observer team).
static LIVE: AtomicBool = AtomicBool::new(false);

pub fn is_live() -> bool {
    LIVE.load(Ordering::Acquire)
}

/// A match is starting and the local player (`me`) only watches it: the view system goes on for
/// the whole match, with the per-player fog followed from the start, and the player sees the
/// match as the observer they are (Home) until they pick somebody with `[` or `]`.
pub fn live_start(me: u8) {
    if is_active() || me >= 8 {
        return;
    }
    reset_replay();
    crate::viewer::enable();
    crate::viewer::begin(me, me);
    crate::viewer::log_keys("");
    LIVE.store(true, Ordering::Release);
    AUTO.store(false, Ordering::Release);
}

/// The match is over: the view system goes off.
pub fn live_end() {
    if LIVE.swap(false, Ordering::AcqRel) {
        reset_replay();
        crate::viewer::disable();
    }
}

/// What the panel shows for a match watched live.
fn live_pace() -> Option<Pace> {
    if !is_live() {
        return None;
    }
    let a = ADDRS.get()?;
    // SAFETY: a u32 in the game's data section.
    let turn = unsafe { core::ptr::read_unaligned(a.turns as *const u32) } as usize;
    Some(Pace {
        paused: false,
        ended: false,
        speed: 1.0,
        game_ms: GAME_MS.load(Ordering::Acquire),
        turn,
        total: 0,
        live: true,
        auto: AUTO.load(Ordering::Acquire),
        graph: Vec::new(),
        seeking: false,
        help: false,
    })
}

/// Once per network turn while watching live: adds the time the game moved its clock on to the
/// game time shown. The deadline is only read, never moved.
fn track_live_time() {
    let Some(a) = ADDRS.get() else { return };
    // SAFETY: a u32 in the game's data section, read on the main thread between ticks.
    let now = unsafe { core::ptr::read_unaligned(a.deadline as *const u32) };
    let last = LAST_DEADLINE.swap(now, Ordering::AcqRel);
    let step = now.wrapping_sub(last) as i32;
    if last != 0 && (1..=500).contains(&step) {
        GAME_MS.fetch_add(step as u32, Ordering::AcqRel);
    }
}

// ---- Keys ----------------------------------------------------------------------------------

const KEY_FASTER: u16 = 1;
const KEY_SLOWER: u16 = 2;
const KEY_PAUSE: u16 = 4;
const KEY_STEP: u16 = 8;
const KEY_FOLLOW: u16 = 16;
const KEY_NEXT_BATTLE: u16 = 32;
const KEY_PREV_BATTLE: u16 = 64;
const KEY_HEAT: u16 = 128;
/// Shift and the slash key, a question mark on most keyboards: shows or hides the full key list.
const KEY_HELP: u16 = 256;
const VK_SHIFT: i32 = 0x10;
/// Whether the panel lists every key (kept for the rest of the session).
static HELP: AtomicBool = AtomicBool::new(false);
/// Plus and minus (main row and numpad), End, Delete, period, comma, slash and semicolon (the view
/// keys are in `viewer.rs`).
const WATCHED: [(u16, i32); 10] = [
    (KEY_HEAT, 0xBF),
    (KEY_NEXT_BATTLE, 0xBE),
    (KEY_PREV_BATTLE, 0xBC),
    (KEY_FOLLOW, 0xBA),
    (KEY_FASTER, 0xBB),
    (KEY_FASTER, 0x6B),
    (KEY_SLOWER, 0xBD),
    (KEY_SLOWER, 0x6D),
    (KEY_PAUSE, 0x23),
    (KEY_STEP, 0x2E),
];

/// Whether the game's victory/defeat test should be skipped: while a replay is still playing.
pub fn match_end_held() -> bool {
    is_active() && !ENDED.load(Ordering::Acquire)
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
    /// A match being observed live: no progress, no playback controls.
    pub live: bool,
    /// The auto-director is choosing who to watch.
    pub auto: bool,
    /// Timeline bar heights (0..=255) across the whole recording; empty without combat events.
    pub graph: Vec<u8>,
    /// A battle jump is fast-forwarding.
    pub seeking: bool,
    /// The panel lists every key instead of the few used most.
    pub help: bool,
}

pub fn pace() -> Option<Pace> {
    if !is_active() {
        return live_pace();
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
        live: false,
        auto: false,
        graph: PLAY.lock().ok()?.as_ref()?.graph.clone(),
        seeking: SEEK_TO.load(Ordering::Acquire) != NO_SEEK,
        help: HELP.load(Ordering::Acquire),
    })
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

/// The heat map thumbnail: off, the game so far, or the whole recording (`/` steps through them).
static HEAT_MODE: AtomicU8 = AtomicU8::new(0);
const HEAT_SO_FAR: u8 = 1;
const HEAT_WHOLE: u8 = 2;
/// How often the so-far map is made again, in turns (about four seconds).
const HEAT_STEP: u32 = 64;

struct HeatCache {
    mode: u8,
    bucket: u32,
    image: std::sync::Arc<Vec<u8>>,
}

static HEAT_CACHE: Mutex<Option<HeatCache>> = Mutex::new(None);

/// What the panel draws as the heat thumbnail.
pub struct HeatView {
    /// The map's side in tiles.
    pub side: usize,
    /// The map file, to draw the heat over a picture of the map; empty when it cannot be read.
    pub map_path: String,
    /// `heat::SIDE` x `SIDE` heat values.
    pub image: std::sync::Arc<Vec<u8>>,
    pub whole: bool,
}

/// The heat map to show now, or `None` when it is off or the recording has no combat events.
pub fn heat_view() -> Option<HeatView> {
    let mode = HEAT_MODE.load(Ordering::Acquire);
    if mode == 0 || !is_active() {
        return None;
    }
    let a = ADDRS.get()?;
    // SAFETY: a u32 and a u16 in the game's data section.
    let (turn, side) = unsafe {
        (
            core::ptr::read_unaligned(a.turns as *const u32),
            core::ptr::read_unaligned(a.map_side as *const u16),
        )
    };
    let side = usize::from(side).clamp(1, w2r_replay::heat::SIDE);
    let bucket = if mode == HEAT_WHOLE { 0 } else { turn / HEAT_STEP };
    let mut cache = HEAT_CACHE.lock().ok()?;
    if let Some(c) = cache.as_ref().filter(|c| c.mode == mode && c.bucket == bucket) {
        return Some(HeatView { side, map_path: map_path(a), image: c.image.clone(), whole: mode == HEAT_WHOLE });
    }
    let guard = PLAY.lock().ok()?;
    let play = guard.as_ref()?;
    if play.replay.events.is_empty() {
        return None;
    }
    let total = play.replay.turns.len() as u32;
    let range = if mode == HEAT_WHOLE {
        w2r_replay::heat::Range::All
    } else {
        w2r_replay::heat::Range::Until(turn)
    };
    let image = std::sync::Arc::new(w2r_replay::heat::image(&play.replay.events, total, range));
    *cache = Some(HeatCache { mode, bucket, image: image.clone() });
    Some(HeatView { side, map_path: map_path(a), image, whole: mode == HEAT_WHOLE })
}

/// The map file the game is playing on.
fn map_path(a: &Addrs) -> String {
    // SAFETY: a NUL-terminated path buffer in the game's data section.
    unsafe { core::ffi::CStr::from_ptr(a.map_path as *const core::ffi::c_char) }
        .to_string_lossy()
        .into_owned()
}

/// The heat key: off, game so far, whole game, off again.
fn step_heat() {
    let has_events = PLAY
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|p| !p.replay.events.is_empty()))
        .unwrap_or(false);
    if !has_events {
        crate::viewer::say("This recording has no combat events (made before they were kept)");
        return;
    }
    let next = (HEAT_MODE.load(Ordering::Acquire) + 1) % 3;
    HEAT_MODE.store(next, Ordering::Release);
    crate::viewer::say(match next {
        HEAT_SO_FAR => "Heat map: the game so far",
        HEAT_WHOLE => "Heat map: the whole game",
        _ => "Heat map off",
    });
}

/// Whether a battle jump is still fast-forwarding. When the turn it was heading for is reached it
/// ends: play carries on at the normal speed, the follow of the recorded camera is turned off and
/// the view eases to the battle.
fn seek_active(a: &Addrs) -> bool {
    let target = SEEK_TO.load(Ordering::Acquire);
    if target == NO_SEEK {
        return false;
    }
    // SAFETY: a u32 in the game's data section.
    let turn = unsafe { core::ptr::read_unaligned(a.turns as *const u32) };
    if turn < target {
        return true;
    }
    SEEK_TO.store(NO_SEEK, Ordering::Release);
    let info = SEEK_INFO.load(Ordering::Acquire);
    let (index, x, y) = (info >> 16, (info >> 8) & 0xff, info & 0xff);
    PAUSED.store(false, Ordering::Release);
    FOLLOW.store(false, Ordering::Release);
    LAST_DEADLINE.store(0, Ordering::Release);
    GOTO_X.store(x * 32 + 16, Ordering::Release);
    GOTO_Y.store(y * 32 + 16, Ordering::Release);
    GOTO_FRAMES.store(GOTO_FRAME_COUNT, Ordering::Release);
    let battle = PLAY
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|p| (p.battles.len(), p.battles.get(index as usize).copied())));
    let (count, battle) = battle.unwrap_or((0, None));
    // In a free-for-all the fight may be between other players, on ground the watched player has
    // not seen. Watch it from one of the players in it, unless the whole map is already shown.
    if let Some(b) = battle
        && !crate::viewer::full_vision()
        && crate::viewer::watching().is_none_or(|w| b.players & (1 << w) == 0)
    {
        crate::viewer::watch(b.lead);
    }
    crate::viewer::say(&format!("Battle {} of {count}", index + 1));
    false
}

/// A battle key: `forward` jumps to the next fight (fast-forwarding to a little before it starts),
/// otherwise says why going back is not possible. The game cannot run turns backwards, so an
/// earlier fight means playing the recording again from the start.
fn battle_jump(a: &Addrs, forward: bool) {
    let Ok(guard) = PLAY.lock() else { return };
    let Some(play) = guard.as_ref() else { return };
    if play.battles.is_empty() {
        let why = if play.graph.is_empty() {
            "This recording has no combat events (made before they were kept)"
        } else {
            "No battles in this recording"
        };
        drop(guard);
        crate::viewer::say(why);
        return;
    }
    if !forward {
        drop(guard);
        crate::viewer::say("A replay cannot rewind: restart it to see an earlier fight");
        return;
    }
    // From where the last jump is heading, so pressing again skips on to the following fight.
    let seeking = SEEK_TO.load(Ordering::Acquire);
    // SAFETY: a u32 in the game's data section.
    let turn = unsafe { core::ptr::read_unaligned(a.turns as *const u32) };
    let base = if seeking == NO_SEEK { turn } else { seeking };
    let Some(b) = w2r_replay::heat::jump(&play.battles, base, true, LEAD_IN).copied() else {
        drop(guard);
        crate::viewer::say("No more battles");
        return;
    };
    let index = play.battles.iter().position(|x| x.start == b.start).unwrap_or(0) as u32;
    drop(guard);
    SEEK_INFO.store(index << 16 | u32::from(b.x) << 8 | u32::from(b.y), Ordering::Release);
    SEEK_TO.store(b.start.saturating_sub(LEAD_IN), Ordering::Release);
    PAUSED.store(false, Ordering::Release);
}

/// Once per frame after a battle jump arrived: eases the view onto the fight for a moment.
fn goto_camera() {
    let frames = GOTO_FRAMES.load(Ordering::Acquire);
    if frames == 0 {
        return;
    }
    GOTO_FRAMES.store(frames - 1, Ordering::Release);
    let Some(a) = ADDRS.get() else { return };
    ease_view_to(a, (GOTO_X.load(Ordering::Acquire), GOTO_Y.load(Ordering::Acquire)));
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
        if is_live() {
            let _ = guarded(track_live_time);
        }
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
    // A battle jump runs on through a pause until it arrives.
    let seeking = ADDRS.get().is_some_and(seek_active);
    if !seeking && PAUSED.load(Ordering::Acquire) && !STEP.swap(false, Ordering::AcqRel) {
        return true;
    }
    let _ = guarded(|| {
        let Some(a) = ADDRS.get() else { return };
        let speed = if seeking {
            SEEK_SPEED
        } else {
            SPEEDS[SPEED.load(Ordering::Acquire) as usize]
        };
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

/// Once per game frame: the playback keys (speed, pause, step, camera) and the camera follow. Only
/// while a replay is playing and the game window is the one with focus (typing in another program
/// does nothing). The view keys are polled by `viewer::poll_keys`.
pub fn poll() {
    if !is_active() {
        if is_live() {
            let _ = guarded(crate::viewer::poll_keys);
            let _ = guarded(poll_live_follow_key);
            let _ = guarded(poll_live_auto_key);
            let _ = guarded(direct_live);
            let _ = guarded(follow_live_camera);
        }
        return;
    }
    let _ = guarded(crate::viewer::poll_keys);
    let _ = guarded(poll_keys);
    let _ = guarded(follow_camera);
    let _ = guarded(goto_camera);
}

/// Once per frame: moves the map view towards where the recorded player was looking at this
/// turn, easing between the turn-sized steps. Only while that player is the one being watched
/// (nobody else's camera was recorded) and the view has not been switched off with `\`.
fn follow_camera() {
    let Some(a) = ADDRS.get() else { return };
    let real = crate::viewer::real();
    if real >= 8 || !FOLLOW.load(Ordering::Acquire) {
        return;
    }
    if crate::viewer::watching() != Some(crate::viewer::home()) {
        return;
    }
    // SAFETY: the turn counter is a u32 in the game's data section.
    let turn = unsafe { core::ptr::read_unaligned(a.turns as *const u32) };
    let Some(target) = PLAY
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|p| w2r_replay::camera::at(&p.replay.cameras, turn)))
    else {
        return;
    };
    ease_view_to(a, target);
}

/// One frame of easing the map view towards a view centred on `target` (map pixels).
fn ease_view_to(a: &Addrs, target: (u32, u32)) {
    // SAFETY: the camera scroll is two i32 and the view size two u16 in the game's data section;
    // `ClampCamera` takes nothing and keeps the scroll inside the map. Main thread, between ticks.
    unsafe {
        let x = a.cam_x as *mut i32;
        let y = a.cam_y as *mut i32;
        let w = u32::from(core::ptr::read_unaligned(a.view_w as *const u16));
        let h = u32::from(core::ptr::read_unaligned(a.view_h as *const u16));
        let goal = w2r_replay::camera::origin(target, w, h);
        let from = (core::ptr::read_unaligned(x), core::ptr::read_unaligned(y));
        // A jump of more than a view's width is taken at once (the first frame, a map alert).
        let (nx, ny) = w2r_replay::camera::ease(from, goal, w.max(h) as i32);
        core::ptr::write_unaligned(x, nx);
        core::ptr::write_unaligned(y, ny);
        core::mem::transmute::<usize, VoidFn>(a.clamp_camera)();
    }
}

/// Once per frame while watching live: eases the map view to where the watched player was last
/// seen looking (`camerashare.rs`). Not for the observer's own all-seeing view, and only while the
/// follow is on (semicolon).
fn follow_live_camera() {
    let Some(a) = ADDRS.get() else { return };
    let real = crate::viewer::real();
    if real >= 8 || !FOLLOW.load(Ordering::Acquire) {
        return;
    }
    let Some(watching) = crate::viewer::watching().filter(|&p| p != real) else {
        return;
    };
    let Some((x, y)) = crate::camerashare::latest(watching, crate::chat::now_ms()) else {
        return;
    };
    // Messages arrive in steps; a smoothed point chasing them keeps the scroll gliding between steps.
    static SMOOTH: Mutex<Option<(i32, i32, u8)>> = Mutex::new(None);
    let (x, y) = (i32::from(x), i32::from(y));
    let target = match SMOOTH.lock() {
        Ok(mut s) => {
            let (sx, sy) = match *s {
                Some((px, py, w)) if w == watching => {
                    let step = |from: i32, to: i32| if (to - from).abs() > 600 { to } else { from + (to - from) / 4 + (to - from).signum() * i32::from(to != from) };
                    (step(px, x), step(py, y))
                }
                _ => (x, y),
            };
            *s = Some((sx, sy, watching));
            (sx, sy)
        }
        Err(_) => (x, y),
    };
    ease_view_to(a, (target.0 as u32, target.1 as u32));
}

/// The End key while watching live: turns the auto-director on and off.
fn poll_live_auto_key() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    static DOWN: AtomicBool = AtomicBool::new(false);
    if crate::viewer::real() >= 8 || !game_has_focus() {
        DOWN.store(false, Ordering::Release);
        return;
    }
    // SAFETY: a plain key-state query.
    let now = unsafe { GetAsyncKeyState(0x23) } as u16 & 0x8000 != 0;
    if now && !DOWN.swap(true, Ordering::AcqRel) {
        if AUTO.fetch_xor(true, Ordering::AcqRel) {
            crate::viewer::say("Auto-director off");
        } else {
            if let Ok(mut d) = DIRECTOR.lock() {
                *d = Some(crate::director::Director::new());
            }
            crate::viewer::say("Auto-director on: the view moves to the busiest player");
        }
    } else if !now {
        DOWN.store(false, Ordering::Release);
    }
}

/// Once per frame while the auto-director is on: looks at how busy the players are and moves the
/// view to the busiest when it is clearly worth it.
fn direct_live() {
    if !AUTO.load(Ordering::Acquire) {
        return;
    }
    let combat = crate::hudpanel::combat_totals().unwrap_or([0; 8]);
    let samples: Vec<crate::director::Sample> = crate::viewer::watchable()
        .into_iter()
        .map(|p| crate::director::Sample {
            player: p,
            apm: crate::liveapm::apm(p),
            combat: combat[usize::from(p) & 7],
        })
        .collect();
    let current = crate::viewer::watching();
    let pick = match DIRECTOR.lock() {
        Ok(mut d) => d
            .get_or_insert_with(crate::director::Director::new)
            .tick(crate::chat::now_ms(), current, &samples),
        Err(_) => None,
    };
    if let Some(player) = pick {
        crate::viewer::watch(player);
    }
}

/// The semicolon key while watching live: turns the camera follow on and off.
fn poll_live_follow_key() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    static DOWN: AtomicBool = AtomicBool::new(false);
    if crate::viewer::real() >= 8 || !game_has_focus() {
        DOWN.store(false, Ordering::Release);
        return;
    }
    // SAFETY: a plain key-state query.
    let now = unsafe { GetAsyncKeyState(0xBA) } as u16 & 0x8000 != 0;
    if now && !DOWN.swap(true, Ordering::AcqRel) {
        if FOLLOW.fetch_xor(true, Ordering::AcqRel) {
            crate::viewer::say("Camera follow off");
        } else {
            crate::viewer::say("Camera follows the player you watch (; to stop)");
        }
    } else if !now {
        DOWN.store(false, Ordering::Release);
    }
}

fn poll_keys() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    if crate::viewer::real() >= 8 {
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
    // Shift with the slash key is the question mark: help, not the heat map.
    // SAFETY: a plain key-state query.
    if down & KEY_HEAT != 0 && unsafe { GetAsyncKeyState(VK_SHIFT) } as u16 & 0x8000 != 0 {
        down = down & !KEY_HEAT | KEY_HELP;
    }
    let pressed = down & !KEYS_DOWN.swap(down, Ordering::AcqRel);
    if pressed == 0 {
        return;
    }
    if pressed & KEY_HELP != 0 {
        HELP.fetch_xor(true, Ordering::AcqRel);
    }
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
    if pressed & KEY_HEAT != 0 {
        step_heat();
    }
    if pressed & (KEY_NEXT_BATTLE | KEY_PREV_BATTLE) != 0 {
        if let Some(a) = ADDRS.get() {
            battle_jump(a, pressed & KEY_NEXT_BATTLE != 0);
        }
    }
    if pressed & KEY_FOLLOW != 0 {
        let has_camera = PLAY
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|p| !p.replay.cameras.is_empty()))
            .unwrap_or(false);
        if !has_camera {
            crate::viewer::say("This recording has no camera data (made before it was kept)");
        } else if FOLLOW.fetch_xor(true, Ordering::AcqRel) {
            crate::viewer::say("Camera follow off");
        } else {
            crate::viewer::say("Camera follows the recorded player (; to stop)");
        }
    }
}

/// Playback state back to its start.
fn reset_replay() {
    SYNC_BAD.store(0, Ordering::Release);
    FOLLOW.store(true, Ordering::Release);
    KEYS_DOWN.store(0, Ordering::Release);
    PAUSED.store(false, Ordering::Release);
    STEP.store(false, Ordering::Release);
    SPEED.store(NORMAL_SPEED, Ordering::Release);
    ENDED.store(false, Ordering::Release);
    LAST_DEADLINE.store(0, Ordering::Release);
    GAME_MS.store(0, Ordering::Release);
    SEEK_TO.store(NO_SEEK, Ordering::Release);
    GOTO_FRAMES.store(0, Ordering::Release);
    HEAT_MODE.store(0, Ordering::Release);
    if let Ok(mut c) = HEAT_CACHE.lock() {
        *c = None;
    }
}

/// Stops driving the game; the live game carries on with its own input.
fn finish(reason: &str) {
    let bad = SYNC_BAD.swap(0, Ordering::AcqRel);
    reset_replay();
    crate::viewer::disable();
    ACTIVE.store(false, Ordering::Release);
    if let Ok(mut p) = PLAY.lock() {
        *p = None;
    }
    if bad > 0 {
        log(
            W2R_LOG_INFO,
            &format!("{bad} sync record(s) differed from the game's hashes and were accepted"),
        );
    }
    log(W2R_LOG_INFO, &format!("replay stopped: {reason}"));
}

/// `RunGame` returned: put the live lobby block back.
pub fn on_run_game_return() {
    live_end();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_distance_counts_draws_of_the_games_generator() {
        let seed = 0x6acb_9dc7;
        let next = |s: u32| s.wrapping_mul(0x015a_4e35).wrapping_add(1);
        assert_eq!(draws_apart(seed, seed), Some(0));
        assert_eq!(draws_apart(seed, next(seed)), Some(1));
        assert_eq!(draws_apart(next(seed), seed), None);
        // the recorded and played states of a real recording are 69 and 70 draws from the seed:
        // one apart, though each is beyond the search limit from the seed itself
        let (mut a, mut b) = (seed, seed);
        for _ in 0..69 {
            a = next(a);
        }
        for _ in 0..70 {
            b = next(b);
        }
        assert_eq!(a, 0x9075_1470);
        assert_eq!(b, 0x33d7_5b31);
        assert_eq!(draws_apart(a, b), Some(1));
        assert_eq!(draws_apart(seed, a), None);
    }
}
