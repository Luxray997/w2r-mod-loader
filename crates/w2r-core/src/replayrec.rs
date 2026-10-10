//! Replay recording (docs/replay-research.md). `ProcessNetworkTurn` collects every slot's command
//! packet for the turn into `TurnPacketPtrs`/`TurnPacketLens` (flag 0x20000 in `TurnPlayerStatus`
//! marks a slot that sent one), runs them and bumps `NetTurnCounter`. We run the game's function
//! first; when it reports a turn done (1) we copy the packets of that turn into a `.w2rr` file
//! (crate `w2r-replay`), so the file holds exactly what the simulation consumed. x86 only (`cdecl`).
//!
//! The file is `replays/<date>-<map>.w2rr` from the first turn on and is a valid replay at every
//! moment (flushed every few seconds). The result footer is written the instant `CheckMatchEnd`
//! reports it, so quitting from the results screen, a crash or a power cut loses nothing; recording
//! then continues (a defeated player's team may still be playing) and re-entering a lobby (or the
//! next match) closes it with a final footer. A file with no footer is a match that was left
//! (surrender, quit) or cut off. Nothing here depends on the lobby.

use core::ffi::c_void;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};
use w2r_replay::format::{self, MAX_PACKET, NAMES_LEN, OWNER_LEN};
use w2r_replay::{Footer, Header, Writer};
use w2r_stats::slot::{SLOT_LEN, SLOTS};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::symbols::Symbols;

pub const SYMBOL: &str = "ProcessNetworkTurn";
/// The game build the packet layout was read from.
const BUILD: &str = "2818";
/// `TurnPlayerStatus` bit set while a slot has a packet for the turn.
const HAS_PACKET: u32 = 0x20000;
const FLUSH_EVERY_SECS: u64 = 5;

type TurnFn = unsafe extern "C" fn() -> u32;

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// A match began and no recording has been started for it yet.
static ARMED: AtomicBool = AtomicBool::new(false);
static ADDRS: OnceLock<Addrs> = OnceLock::new();
static CONFIG: OnceLock<Config> = OnceLock::new();
/// `SimRngState` read just before the latest call of the turn function while no recording has
/// started: the value before the first turn is the last one stored.
static RNG_BEFORE: AtomicU32 = AtomicU32::new(0);
/// The lobby block as it was when `StartGame` was entered (before the game normalised it).
static PRE_LOBBY: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static REC: Mutex<Option<Rec>> = Mutex::new(None);
/// Final `(results, sync_mismatch)` from `CheckMatchEnd`, repeated in the closing footer.
static RESULT: Mutex<Option<([u8; 8], u32)>> = Mutex::new(None);

struct Addrs {
    ptrs: usize,
    lens: usize,
    status: usize,
    turns: usize,
    local: usize,
    slots: usize,
    owner: usize,
    names: usize,
    seed: usize,
    rng: usize,
}

struct Config {
    dir: PathBuf,
    fingerprint: [u8; 4],
    client: String,
}

struct Rec {
    writer: Writer<BufWriter<File>>,
    path: PathBuf,
    last_turn: u32,
    flushed_at: u64,
}

fn log(level: u32, text: &str) {
    crate::session::StartupLog.line(level, "replay", text);
}

/// Folder and identity for the files; also keeps recordings a crash left behind.
pub fn init(root: &Path, fingerprint: [u8; 4], client: &str, log: &dyn Log) {
    let dir = root.join("replays");
    if let Err(e) = fs::create_dir_all(&dir) {
        log.line(
            W2R_LOG_ERROR,
            "replay",
            &format!("recording off: {} unavailable ({e})", dir.display()),
        );
        return;
    }
    let mut kept = 0;
    for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "part") {
            // "x.w2rr.part" -> "x.w2rr": a cut-off recording is still a replay.
            if fs::rename(&p, p.with_extension("")).is_ok() {
                kept += 1;
            }
        }
    }
    if kept > 0 {
        log.line(
            W2R_LOG_INFO,
            "replay",
            &format!("kept {kept} recording(s) cut off by an earlier run"),
        );
    }
    let _ = CONFIG.set(Config {
        dir,
        fingerprint,
        client: client.to_string(),
    });
}

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let get = |name: &str| symbols.address(name);
    let (Some(target), Some(ptrs), Some(lens), Some(status), Some(turns)) = (
        get(SYMBOL),
        get("TurnPacketPtrs"),
        get("TurnPacketLens"),
        get("TurnPlayerStatus"),
        get("NetTurnCounter"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "replay recording: symbols missing");
        return false;
    };
    let (Some(local), Some(slots), Some(owner), Some(names)) = (
        get("LocalPlayer"),
        get("MpLobbySlots"),
        get("PlayerOwner"),
        get("PlayerNames"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "replay recording: symbols missing");
        return false;
    };
    let (Some(seed), Some(rng)) = (get("GameSeed"), get("SimRngState")) else {
        log.line(W2R_LOG_ERROR, "host", "replay recording: symbols missing");
        return false;
    };
    let _ = ADDRS.set(Addrs {
        ptrs,
        lens,
        status,
        turns,
        local,
        slots,
        owner,
        names,
        seed,
        rng,
    });
    // SAFETY: `detour` matches ProcessNetworkTurn's cdecl `u32()` signature (Ghidra, build 2818).
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            detour as *mut c_void,
            &format!("host:{SYMBOL}"),
            |original| ORIGINAL.store(original as usize, Ordering::Release),
        )
    };
    match result {
        Ok(_) => {
            log.line(
                W2R_LOG_INFO,
                "host",
                &format!("hooked {SYMBOL} @ {target:#010x} (replay recording)"),
            );
            true
        }
        Err(e) => {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("could not hook {SYMBOL}: {e:?}"),
            );
            false
        }
    }
}

/// `StartGame` was entered: remember the lobby block for the header of this match's recording.
pub fn note_lobby(block: Vec<u8>) {
    if let Ok(mut l) = PRE_LOBBY.lock() {
        *l = block;
    }
}

/// A new match: record it. A recording still open from the one before is closed as cut off.
pub fn rearm() {
    end(None);
    if let Ok(mut r) = RESULT.lock() {
        *r = None;
    }
    // A replay being played is not recorded again.
    ARMED.store(
        CONFIG.get().is_some() && ADDRS.get().is_some() && !crate::replayplay::is_active(),
        Ordering::Release,
    );
}

unsafe extern "C" fn detour() -> u32 {
    let original: TurnFn = unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    if ARMED.load(Ordering::Acquire) {
        // The turn function may run the first turn: note the RNG before it does.
        if let Some(a) = ADDRS.get() {
            // SAFETY: a symbol-table address in the game's data section, read under `guarded`.
            if let Ok(rng) = guarded(|| unsafe { read_u32(a.rng) }) {
                RNG_BEFORE.store(rng, Ordering::Release);
            }
        }
    }
    let done = unsafe { original() };
    if done == 1 && (ARMED.load(Ordering::Acquire) || is_recording()) {
        if let Err(fault) = guarded(capture) {
            // Stop recording a match whose tables fault: one report, then silence.
            ARMED.store(false, Ordering::Release);
            if let Ok(mut r) = REC.lock() {
                *r = None;
            }
            crate::session::host_fault("replay recording", &fault);
        }
    }
    done
}

fn is_recording() -> bool {
    REC.lock().is_ok_and(|r| r.is_some())
}

/// Copies this turn's packets. Runs right after the game processed the turn, so
/// `NetTurnCounter - 1` is the turn that was just consumed.
fn capture() {
    let (Some(a), Some(cfg)) = (ADDRS.get(), CONFIG.get()) else {
        return;
    };
    let Ok(mut guard) = REC.lock() else { return };
    // SAFETY: addresses come from the symbol table for this build; the caller wraps us in
    // `guarded`, and the packet pointers are the net layer's buffers for the turn just run.
    let (turn, packets) = unsafe {
        let turn = core::ptr::read_unaligned(a.turns as *const u32).wrapping_sub(1);
        let mut packets: Vec<(u8, &[u8])> = Vec::new();
        for slot in 0..SLOTS {
            let status = core::ptr::read_unaligned((a.status + slot * 4) as *const u32);
            let ptr = core::ptr::read_unaligned((a.ptrs + slot * 4) as *const usize);
            let len = core::ptr::read_unaligned((a.lens + slot * 4) as *const u32) as usize;
            if status & HAS_PACKET != 0 && ptr != 0 && len > 0 && len <= MAX_PACKET {
                packets.push((
                    slot as u8,
                    core::slice::from_raw_parts(ptr as *const u8, len),
                ));
            }
        }
        (turn, packets)
    };
    if guard.is_none() {
        if !ARMED.swap(false, Ordering::AcqRel) {
            return;
        }
        match unsafe { start(a, cfg, turn) } {
            Some(rec) => *guard = Some(rec),
            None => return,
        }
    }
    let Some(rec) = guard.as_mut() else { return };
    let now = crate::matchend::now_secs();
    let mut ok = rec.writer.turn(turn, &packets).is_ok();
    if ok && now >= rec.flushed_at + FLUSH_EVERY_SECS {
        ok = rec.writer.flush().is_ok();
        rec.flushed_at = now;
    }
    rec.last_turn = turn;
    if !ok {
        log(
            W2R_LOG_ERROR,
            "recording stopped: could not write the replay file",
        );
        *guard = None;
    }
}

unsafe fn read_u32(addr: usize) -> u32 {
    unsafe { core::ptr::read_unaligned(addr as *const u32) }
}

/// # Safety
/// Reads the game's data section at the symbol addresses; call inside `guarded`.
unsafe fn start(a: &Addrs, cfg: &Config, first_turn: u32) -> Option<Rec> {
    let mut slots = [[0u8; SLOT_LEN]; SLOTS];
    for (i, s) in slots.iter_mut().enumerate() {
        *s = unsafe { core::ptr::read((a.slots + i * SLOT_LEN) as *const [u8; SLOT_LEN]) };
    }
    let started = crate::matchend::now_secs();
    let header = Header {
        build: BUILD.to_string(),
        client: cfg.client.clone(),
        fingerprint: cfg.fingerprint,
        map: crate::matchend::read_map(),
        started,
        local: unsafe { *(a.local as *const u8) },
        slots,
        owner: unsafe { core::ptr::read(a.owner as *const [u8; OWNER_LEN]) },
        names: unsafe { core::ptr::read(a.names as *const [u8; NAMES_LEN]) },
        seed: Header::seed_bytes(
            unsafe { read_u32(a.seed) },
            RNG_BEFORE.load(Ordering::Acquire),
        ),
        lobby: PRE_LOBBY.lock().map(|l| l.clone()).unwrap_or_default(),
    };
    let name = format::file_name(started, &header.map);
    let (path, file) = match create_new(&cfg.dir, &name) {
        Ok(x) => x,
        Err(e) => {
            log(
                W2R_LOG_ERROR,
                &format!("recording off for this match: {name} ({e})"),
            );
            return None;
        }
    };
    let writer = Writer::new(BufWriter::new(file), &header)
        .map_err(|e| log(W2R_LOG_ERROR, &format!("recording off for this match: {e}")))
        .ok()?;
    log(
        W2R_LOG_INFO,
        &format!(
            "recording {} (first turn {first_turn}, local slot {}, game seed {:#010x}, rng before first turn {:#010x})",
            path.display(),
            header.local,
            header.game_seed().unwrap_or(0),
            header.rng_at_start().unwrap_or(0)
        ),
    );
    Some(Rec {
        writer,
        path,
        last_turn: first_turn,
        flushed_at: started,
    })
}

/// A file that does not exist yet: `name`, or `name` with `-2`, `-3`... before the extension.
fn create_new(dir: &Path, name: &str) -> std::io::Result<(PathBuf, File)> {
    let stem = name.strip_suffix(".w2rr").unwrap_or(name);
    for n in 1..100 {
        let file_name = if n == 1 {
            name.to_string()
        } else {
            format!("{stem}-{n}.w2rr")
        };
        let path = dir.join(file_name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "too many replays with this name",
    ))
}

/// The local player's result is final: write the footer now and flush, so the replay on disk
/// already has its result whatever happens next (quit, crash, no lobby). Recording goes on.
pub fn note_result(results: [u8; 8], sync_mismatch: u32) {
    if let Ok(mut r) = RESULT.lock() {
        *r = Some((results, sync_mismatch));
    }
    let Ok(mut r) = REC.lock() else { return };
    let Some(rec) = r.as_mut() else { return };
    let footer = Footer {
        results,
        turns: rec.last_turn.saturating_add(1),
        sync_mismatch,
    };
    if let Err(e) = rec.writer.footer(&footer) {
        log(W2R_LOG_ERROR, &format!("could not write the result: {e}"));
    } else {
        log(
            W2R_LOG_INFO,
            &format!(
                "result written to {} ({} turns so far)",
                rec.path.display(),
                footer.turns
            ),
        );
    }
}

/// Closes the recording with a final footer: the remembered result if the match ended normally,
/// else `left` (the last state seen when the match was abandoned), else all zeros.
pub fn end(left: Option<([u8; 8], u32)>) {
    ARMED.store(false, Ordering::Release);
    let Some(rec) = REC.lock().ok().and_then(|mut r| r.take()) else {
        return;
    };
    let noted = RESULT.lock().ok().and_then(|mut r| r.take());
    let (results, sync_mismatch) = noted.or(left).unwrap_or(([0; 8], 0));
    let turns = rec.last_turn.saturating_add(1);
    let footer = Footer {
        results,
        turns,
        sync_mismatch,
    };
    match rec.writer.finish(&footer) {
        Ok(_) => log(
            W2R_LOG_INFO,
            &format!("saved {} ({turns} turns)", rec.path.display()),
        ),
        Err(e) => log(W2R_LOG_ERROR, &format!("could not finish the replay: {e}")),
    }
}
