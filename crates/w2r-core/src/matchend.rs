//! Match end and the result reporter.
//!
//! `W2R_EVT_MATCH_END`: `CheckMatchEnd` runs the victory/defeat test each in-game tick and writes
//! `PlayerMatchResult` (0 = still playing). Once the local player's own entry is non-zero the
//! match is over for this machine: we run the game's function first, then snapshot the tables once
//! per match, log them and fire the event. x86 only (`cdecl`).
//!
//! A surrender or an early leave writes nothing there (verified 2026-10-09 against the AI), and the
//! lobby wipes the tables before it is entered again, so every window/frame call while a match is
//! open keeps the last state seen; entering a lobby with the match still open logs that state.
//!
//! The reporter (docs/stats-reporting.md) hangs off the same three moments: a signed `start` report
//! when the first turn has run, a `result` report at the end or when the lobby is re-entered. Reports
//! only go to the local outbox folder for now; nothing is uploaded.

use core::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};
use w2r_stats::game::{self, GameInfo};
use w2r_stats::identity::{self, Identity};
use w2r_stats::inflight::{self, Inflight};
use w2r_stats::queue::Queue;
use w2r_stats::report::{self, Outcome, Phase, PlayerScore, Report, ResultInfo};
use w2r_stats::slot::{self, Player};
use w2r_stats::{envelope, hex};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::symbols::Symbols;

pub const SYMBOL: &str = "CheckMatchEnd";
const MAP_NAME_MAX: usize = 64;

type CheckFn = unsafe extern "C" fn();

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static REPORTED: AtomicBool = AtomicBool::new(false);
/// A match began and no result was snapshotted yet.
static OPEN: AtomicBool = AtomicBool::new(false);
static ADDRS: OnceLock<Addrs> = OnceLock::new();
static LAST: Mutex<Option<Raw>> = Mutex::new(None);
static REPORTER: Mutex<Option<Reporter>> = Mutex::new(None);

/// Absolute addresses of the globals the snapshot reads.
/// The game's score tables (`ScoreUnitsKilled` and the rest): one entry per player slot, u16 for
/// the counts and u32 for the mined resources. The replay panel reads the same ones.
#[derive(Clone, Copy)]
struct ScoreAddrs {
    kills: usize,
    razed: usize,
    lost: usize,
    buildings_lost: usize,
    gold: usize,
    lumber: usize,
    oil: usize,
}

struct Addrs {
    score: Option<ScoreAddrs>,
    results: usize,
    local: usize,
    owner: usize,
    slots: usize,
    names: usize,
    turns: usize,
    mismatch: usize,
    map: usize,
}

/// A new match: allow one more snapshot.
pub fn rearm() {
    // A replay being played is not a game: nothing is reported for it.
    let playing = crate::replayplay::is_active();
    REPORTED.store(playing, Ordering::Release);
    OPEN.store(!playing, Ordering::Release);
    if let Ok(mut l) = LAST.lock() {
        *l = None;
    }
    if let Ok(mut r) = REPORTER.lock() {
        if let Some(rep) = r.as_mut() {
            rep.ctx = None;
        }
    }
}

/// Back in a lobby. `RunGame` returning is the real end of a match; this stays as a fallback for a
/// match whose end was not seen.
pub fn on_lobby_enter() {
    conclude();
}

/// The match is over for this machine, however it ended. One that began but never produced a
/// result (surrender, quit) is logged once, from the last state seen, and reported as `left`; the
/// replay recording is closed with the final result.
fn conclude() {
    let mut left = None;
    if OPEN.swap(false, Ordering::AcqRel) && !REPORTED.swap(true, Ordering::AcqRel) {
        let last = LAST.lock().ok().and_then(|mut l| l.take());
        let text = match &last {
            Some(raw) => raw.format("match left without a result (last state seen)"),
            None => "match left without a result (nothing seen)".to_string(),
        };
        crate::session::StartupLog.line(W2R_LOG_INFO, "host", &text);
        if let Some(raw) = last {
            left = Some((raw.results, raw.mismatch));
            let _ = guarded(|| report_result(&raw));
        }
    }
    crate::replayrec::end(left);
}

/// Called on every game window/frame: while a match is open, remember its tables so the last
/// state before the lobby wipes them can be used, and send the start report on the first turn.
pub fn note_frame() {
    if !OPEN.load(Ordering::Acquire) {
        return;
    }
    let _ = guarded(|| {
        // SAFETY: as in `check`.
        if let Some(raw) = unsafe { read_raw() } {
            if raw.turns > 0 {
                report_start(&raw);
                touch_marker(&raw);
                if let Ok(mut l) = LAST.lock() {
                    *l = Some(raw);
                }
            }
        }
    });
}

pub const RUN_GAME: &str = "RunGame";

type RunGameFn = unsafe extern "C" fn() -> u32;

static RUN_ORIGINAL: AtomicUsize = AtomicUsize::new(0);

/// `RunGame` (0x4c57f0) sets up the match, waits for the first turn, runs the match loop until a
/// win, a loss, a surrender or a quit clears `MatchLoopActive` (0x91c6c5), tears down and returns
/// the next game state. Its return is the one signal that the match is over whatever the way,
/// with the tables still intact (a lobby only wipes them later). x86 only (`cdecl`).
pub fn install_run_game(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(target) = symbols.address(RUN_GAME) else {
        log.line(W2R_LOG_ERROR, "host", "RunGame is not in the symbol table");
        return false;
    };
    // SAFETY: `run_game_detour` matches RunGame's cdecl no-argument signature (Ghidra, build 2818);
    // it returns the full EAX unchanged, so the game's 16-bit return value is untouched.
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            run_game_detour as *mut c_void,
            &format!("host:{RUN_GAME}"),
            |original| RUN_ORIGINAL.store(original as usize, Ordering::Release),
        )
    };
    match result {
        Ok(_) => {
            log.line(
                W2R_LOG_INFO,
                "host",
                &format!("hooked {RUN_GAME} @ {target:#010x} (match concluded)"),
            );
            true
        }
        Err(e) => {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("could not hook {RUN_GAME}: {e:?}"),
            );
            false
        }
    }
}

unsafe extern "C" fn run_game_detour() -> u32 {
    let original: RunGameFn = unsafe { core::mem::transmute(RUN_ORIGINAL.load(Ordering::Acquire)) };
    let next_state = unsafe { original() };
    crate::replayplay::on_run_game_return();
    if let Err(fault) = guarded(|| {
        // The tables are still the match's own: keep this state for `conclude`.
        // SAFETY: as in `check`.
        if let Some(raw) = unsafe { read_raw() }.filter(|r| r.turns > 0) {
            if let Ok(mut l) = LAST.lock() {
                *l = Some(raw);
            }
        }
        conclude();
    }) {
        crate::session::host_fault("match concluded", &fault);
    }
    next_state
}

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let get = |name: &str| symbols.address(name);
    let (Some(target), Some(results), Some(local), Some(owner), Some(slots), Some(names)) = (
        get(SYMBOL),
        get("PlayerMatchResult"),
        get("LocalPlayer"),
        get("PlayerOwner"),
        get("MpLobbySlots"),
        get("PlayerNames"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "match end: symbols missing");
        return false;
    };
    let (Some(turns), Some(mismatch), Some(map)) = (
        get("NetTurnCounter"),
        get("TurnSyncMismatch"),
        get("MpLobbyMapNameText"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "match end: symbols missing");
        return false;
    };
    let score = (|| {
        Some(ScoreAddrs {
            kills: get("ScoreUnitsKilled")?,
            razed: get("ScoreBuildingsRazed")?,
            lost: get("ScoreUnitsLost")?,
            buildings_lost: get("ScoreBuildingsLost")?,
            gold: get("ScoreGoldMined")?,
            lumber: get("ScoreLumberMined")?,
            oil: get("ScoreOilMined")?,
        })
    })();
    if score.is_none() {
        log.line(W2R_LOG_ERROR, "host", "match end: score symbols missing; results go without kills");
    }
    let _ = ADDRS.set(Addrs {
        score,
        results,
        local,
        owner,
        slots,
        names,
        turns,
        mismatch,
        map,
    });
    // SAFETY: `detour` matches CheckMatchEnd's cdecl `void()` signature from the symbol table.
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
                &format!("hooked {SYMBOL} @ {target:#010x} (match end)"),
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

unsafe extern "C" fn detour() {
    // A replay is not a live match: the game's test asks the network layer who is still connected,
    // and in the local replay game that is only the viewer, so it would call the match over at
    // once. It runs again when the recording has been played to its end.
    if crate::replayplay::match_end_held() {
        return;
    }
    let original: CheckFn = unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    unsafe { original() };
    if REPORTED.load(Ordering::Acquire) {
        return;
    }
    if let Err(fault) = guarded(check) {
        // Stop probing a table that faults: one report, then silence.
        REPORTED.store(true, Ordering::Release);
        crate::session::host_fault("match end", &fault);
    }
}

fn check() {
    let Some(a) = ADDRS.get() else { return };
    // SAFETY: addresses come from the symbol table for this build; all reads are of plain bytes
    // inside the game's data section, and the caller wraps us in `guarded`.
    let raw = unsafe {
        let local = *(a.local as *const u8);
        if local >= 8 || *((a.results + local as usize) as *const u8) == 0 {
            return;
        }
        if REPORTED.swap(true, Ordering::AcqRel) {
            return;
        }
        read_raw()
    };
    if let Some(raw) = raw {
        crate::session::StartupLog.line(W2R_LOG_INFO, "host", &raw.format("match end"));
        report_result(&raw);
        crate::replayrec::note_result(raw.results, raw.mismatch);
    }
    OPEN.store(false, Ordering::Release);
    crate::session::on_match_end();
}

/// One read of the tables the stats reporter cares about.
#[derive(Clone)]
struct Raw {
    local: u8,
    results: [u8; 8],
    owner: [u8; 0x20],
    turns: u32,
    mismatch: u32,
    slots: [[u8; slot::SLOT_LEN]; slot::SLOTS],
    names: [u8; 0x100],
    /// The game's lobby block (settings), `game::BLOCK_LEN` bytes.
    lobby: Vec<u8>,
    /// The score tables for all eight slots, or `None` when their symbols are missing.
    score: Option<[PlayerScore; 8]>,
}

/// # Safety
/// Reads the game's data section at the symbol addresses; call inside `guarded`.
unsafe fn read_raw() -> Option<Raw> {
    let a = ADDRS.get()?;
    unsafe {
        let mut slots = [[0u8; slot::SLOT_LEN]; slot::SLOTS];
        for (i, s) in slots.iter_mut().enumerate() {
            *s = core::ptr::read((a.slots + i * slot::SLOT_LEN) as *const [u8; slot::SLOT_LEN]);
        }
        let score = a.score.map(|t| {
            let mut all = [PlayerScore::default(); 8];
            for (p, entry) in all.iter_mut().enumerate() {
                let u16_at = |base: usize| core::ptr::read_unaligned((base + p * 2) as *const u16);
                let u32_at = |base: usize| core::ptr::read_unaligned((base + p * 4) as *const u32);
                *entry = PlayerScore {
                    slot: p as u8,
                    kills: u32::from(u16_at(t.kills)),
                    razed: u32::from(u16_at(t.razed)),
                    lost: u32::from(u16_at(t.lost)),
                    buildings_lost: u32::from(u16_at(t.buildings_lost)),
                    gold: u32_at(t.gold),
                    lumber: u32_at(t.lumber),
                    oil: u32_at(t.oil),
                };
            }
            all
        });
        Some(Raw {
            score,
            local: *(a.local as *const u8),
            results: core::ptr::read(a.results as *const [u8; 8]),
            owner: core::ptr::read(a.owner as *const [u8; 0x20]),
            turns: core::ptr::read_unaligned(a.turns as *const u32),
            mismatch: core::ptr::read_unaligned(a.mismatch as *const u32),
            slots,
            names: core::ptr::read(a.names as *const [u8; 0x100]),
            lobby: core::slice::from_raw_parts(
                game::LOBBY_AT as *const u8,
                game::BLOCK_LEN,
            )
            .to_vec(),
        })
    }
}

/// The lobby's map name text. Safe to call inside `guarded` only.
pub(crate) fn read_map() -> String {
    let Some(a) = ADDRS.get() else {
        return String::new();
    };
    // SAFETY: a fixed-size text buffer in the game's data section.
    let bytes = unsafe { core::slice::from_raw_parts(a.map as *const u8, MAP_NAME_MAX) };
    let n = bytes.iter().position(|&b| b == 0).unwrap_or(MAP_NAME_MAX);
    crate::gamepath::decode(&bytes[..n])
}

impl Raw {
    /// The score of every slot in `players`, in roster order.
    fn scores(&self, players: &[Player]) -> Option<Vec<PlayerScore>> {
        let all = self.score.as_ref()?;
        Some(
            players
                .iter()
                .filter_map(|p| all.get(usize::from(p.slot)).copied())
                .collect(),
        )
    }

    fn format(&self, tag: &str) -> String {
        let mut line = format!(
            "{tag}: local={} results={:?} turns={} sync_mismatch={}\n  owner[0x20]={:02x?}",
            self.local, self.results, self.turns, self.mismatch, self.owner
        );
        for (i, s) in self.slots.iter().enumerate() {
            line.push_str(&format!("\n  slot{i}={s:02x?}"));
        }
        let text: String = self
            .names
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '|'
                }
            })
            .collect();
        line.push_str(&format!("\n  names={text}"));
        line
    }
}

// ---- the reporter -------------------------------------------------------------------------

struct Reporter {
    identity: Identity,
    queue: Queue,
    client: String,
    fingerprint: String,
    ctx: Option<MatchCtx>,
    /// The in-flight marker file (see `w2r_stats::inflight`) and when it was last written.
    inflight: PathBuf,
    root: PathBuf,
    marker_at: u64,
}

/// What was true when the match began, kept for its result report.
struct MatchCtx {
    started: u64,
    map: String,
    players: Vec<Player>,
    me: u8,
    setup: [u8; 32],
    game: Option<GameInfo>,
    /// The start report that was queued, for the in-flight marker.
    start: Option<Report>,
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn random32() -> Option<[u8; 32]> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    let mut out = [0u8; 32];
    // SAFETY: a valid buffer of the stated length.
    let status = unsafe {
        BCryptGenRandom(
            core::ptr::null_mut(),
            out.as_mut_ptr(),
            32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    (status == 0).then_some(out)
}

/// Sets up the identity key and the outbox under the host root. On failure reporting stays off
/// (and says why once); the game and the rest of the host are unaffected.
pub fn init_reporter(root: &Path, fingerprint: [u8; 4], client: &str, log: &dyn Log) {
    let identity =
        match identity::load_or_create(&root.join("config").join("identity.json"), random32) {
            Ok(i) => i,
            Err(e) => {
                log.line(
                    W2R_LOG_ERROR,
                    "stats",
                    &format!("reporting off: identity key unavailable ({e:?})"),
                );
                return;
            }
        };
    let queue = match Queue::open(&root.join("outbox")) {
        Ok(q) => q,
        Err(e) => {
            log.line(
                W2R_LOG_ERROR,
                "stats",
                &format!("reporting off: outbox unavailable ({e})"),
            );
            return;
        }
    };
    log.line(
        W2R_LOG_INFO,
        "stats",
        &format!(
            "reporter key {} ({} reports waiting)",
            hex::encode(&identity.public()),
            queue.list().len()
        ),
    );
    let rep = Reporter {
        identity,
        queue,
        client: client.to_string(),
        fingerprint: hex::encode(&fingerprint),
        ctx: None,
        inflight: root.join("inflight.json"),
        root: root.to_path_buf(),
        marker_at: 0,
    };
    recover_unfinished(&rep, log);
    crate::uploader::kick(&rep.root, true);
    if let Ok(mut r) = REPORTER.lock() {
        *r = Some(rep);
    }
}

/// A marker left by an earlier run means that match was cut off (quit from the results screen,
/// crash): report it as left, at the last turn seen.
fn recover_unfinished(rep: &Reporter, log: &dyn Log) {
    let Some(marker) = inflight::read(&rep.inflight) else {
        return;
    };
    let rid = random32().map(|b| {
        let mut r = [0u8; 8];
        r.copy_from_slice(&b[..8]);
        r
    });
    match rid.and_then(|rid| inflight::recover(&marker, rid)) {
        Some(report) => {
            seal_and_queue(rep, &report);
            log.line(
                W2R_LOG_INFO,
                "stats",
                &format!("recovered a match cut off at turn {}", marker.turns),
            );
        }
        None => log.line(
            W2R_LOG_INFO,
            "stats",
            "dropped an in-flight marker with no turns",
        ),
    }
    inflight::clear(&rep.inflight);
}

fn write_marker(rep: &mut Reporter, raw: &Raw) {
    let Some(start) = rep.ctx.as_ref().and_then(|c| c.start.clone()) else {
        return;
    };
    let marker = Inflight {
        start,
        turns: raw.turns,
        sync_mismatch: raw.mismatch,
    };
    let _ = inflight::write(&rep.inflight, &marker);
    rep.marker_at = now_secs();
}

/// Called with every frame's tables: keeps the marker's turn count within 30 s of the truth.
fn touch_marker(raw: &Raw) {
    let Ok(mut guard) = REPORTER.lock() else {
        return;
    };
    let Some(rep) = guard.as_mut() else { return };
    if rep.ctx.is_some() && now_secs().saturating_sub(rep.marker_at) >= 30 {
        write_marker(rep, raw);
    }
}

fn seal_and_queue(rep: &Reporter, report: &Report) {
    let sealed = envelope::seal(&rep.identity, report);
    let name = Queue::file_name(
        report.started,
        report.phase.as_str(),
        &hex::encode(&report.rid),
    );
    let line = match rep.queue.push(&name, sealed.to_json().as_bytes()) {
        Ok(_) => format!("queued {name}"),
        Err(e) => format!("could not queue {name}: {e}"),
    };
    crate::session::StartupLog.line(W2R_LOG_INFO, "stats", &line);
    // A result report is the moment to share: the start report is already waiting beside it.
    if report.phase == Phase::Result {
        crate::uploader::kick(&rep.root, false);
    }
}

fn build(
    rep: &Reporter,
    ctx: &MatchCtx,
    phase: Phase,
    result: Option<ResultInfo>,
) -> Option<Report> {
    let mut rid = [0u8; 8];
    rid.copy_from_slice(&random32()?[..8]);
    Some(Report {
        phase,
        rid,
        setup: ctx.setup,
        started: ctx.started,
        client: rep.client.clone(),
        fingerprint: rep.fingerprint.clone(),
        map: ctx.map.clone(),
        me: ctx.me,
        players: ctx.players.clone(),
        game: ctx.game.clone(),
        result,
    })
}

fn context(raw: &Raw) -> Option<MatchCtx> {
    if raw.local >= 8 {
        return None;
    }
    let players = slot::parse_table(&raw.slots);
    // Only a match with the local player in the roster is ours to report.
    players.iter().find(|p| p.slot == raw.local)?;
    let map = read_map();
    let game = GameInfo::from_lobby_block(&raw.lobby, crate::mapdownload::played_map_sha(&map));
    Some(MatchCtx {
        started: now_secs(),
        setup: report::setup_id(&map, &players),
        game,
        map,
        players,
        me: raw.local,
        start: None,
    })
}

/// The first turn has run: remember the match and queue its start report (once per match).
fn report_start(raw: &Raw) {
    let Ok(mut guard) = REPORTER.lock() else {
        return;
    };
    let Some(rep) = guard.as_mut() else { return };
    if rep.ctx.is_some() {
        return;
    }
    let Some(ctx) = context(raw) else { return };
    let mut ctx = ctx;
    if let Some(report) = build(rep, &ctx, Phase::Start, None) {
        seal_and_queue(rep, &report);
        ctx.start = Some(report);
    }
    rep.ctx = Some(ctx);
    write_marker(rep, raw);
}

/// The match ended for this player (or was left): queue the result report.
fn report_result(raw: &Raw) {
    let Ok(mut guard) = REPORTER.lock() else {
        return;
    };
    let Some(rep) = guard.as_mut() else { return };
    if raw.local >= 8 {
        return;
    }
    let code = raw.results[raw.local as usize];
    let Some(outcome) = Outcome::from_code(code, raw.turns) else {
        inflight::clear(&rep.inflight);
        return;
    };
    let Some(ctx) = rep.ctx.take().or_else(|| context(raw)) else {
        return;
    };
    let info = ResultInfo {
        outcome,
        code,
        turns: raw.turns,
        sync_mismatch: raw.mismatch,
        score: raw.scores(&ctx.players),
        activity: Some(crate::replayrec::activity_report(&ctx.players)).filter(|a| !a.is_empty()),
    };
    if let Some(report) = build(rep, &ctx, Phase::Result, Some(info)) {
        seal_and_queue(rep, &report);
    }
    inflight::clear(&rep.inflight);
}
