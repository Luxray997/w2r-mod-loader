//! The in-game APM counter (docs/apm.md): the local player's actions per minute over the last
//! 20 seconds played, drawn on the HUD, with an optional alert (red text, a sound) when it falls
//! below a threshold. Actions are counted where the game queues them (`QueueCommand`, called by
//! every UI order in every game mode, networked or single-player) and closed turn by turn from
//! `ProcessNetworkTurn` and `ProcessLocalMatchTick`. The tracker and the sound are pure and
//! tested here; the game side is x86 only.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::hostcfg::HostConfig;

/// How much play the figure covers.
pub const WINDOW_MS: u64 = 20_000;
/// No figure before this much play: a handful of clicks in two seconds is not a rate.
pub const MIN_MS: u64 = 5_000;
/// No alert before this much play (people open slowly), however low the figure is.
pub const GRACE_MS: u64 = 60_000;
/// Between two alert sounds while the figure stays low.
pub const REPEAT_MS: u64 = 30_000;
/// A gap between two turns longer than this is a pause or a stall, not play.
const MAX_STEP_MS: u64 = 500;

/// Actions per turn against play time. Time only advances while turns arrive, so pausing the
/// game does not drag the figure down.
#[derive(Debug, Default)]
pub struct Tracker {
    /// `(play time at the end of the turn, actions in it)`, oldest first, within the window.
    turns: VecDeque<(u64, u32)>,
    played_ms: u64,
    last_wall: Option<u64>,
    total: u32,
    last_alert: Option<u64>,
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker::default()
    }

    /// One turn completed at wall-clock time `wall_ms` in which the player gave `actions` orders.
    pub fn turn(&mut self, wall_ms: u64, actions: u32) {
        if let Some(last) = self.last_wall {
            self.played_ms += wall_ms.saturating_sub(last).min(MAX_STEP_MS);
        }
        self.last_wall = Some(wall_ms);
        self.total = self.total.saturating_add(actions);
        self.turns.push_back((self.played_ms, actions));
        while self
            .turns
            .front()
            .is_some_and(|&(at, _)| at + WINDOW_MS <= self.played_ms)
        {
            self.turns.pop_front();
        }
    }

    pub fn played_ms(&self) -> u64 {
        self.played_ms
    }

    /// Actions per minute over the last 20 seconds played (or all of it, if shorter), once there
    /// is enough play to measure.
    pub fn apm(&self) -> Option<u32> {
        if self.played_ms < MIN_MS {
            return None;
        }
        let span = self.played_ms.min(WINDOW_MS);
        let n: u64 = self.turns.iter().map(|&(_, a)| u64::from(a)).sum();
        Some((n * 60_000 / span) as u32)
    }

    /// The figure is below `threshold` (0 = no threshold) for a player who has played at all.
    /// An observer or an absent player never gives an order and is not alerted.
    pub fn low(&self, threshold: u32) -> bool {
        threshold > 0 && self.total > 0 && self.apm().is_some_and(|v| v < threshold)
    }

    /// True once when the figure is low, past the grace period and not alerted within
    /// `REPEAT_MS`.
    pub fn alert_due(&mut self, threshold: u32) -> bool {
        if !self.low(threshold) || self.played_ms < GRACE_MS {
            return false;
        }
        if self
            .last_alert
            .is_some_and(|at| self.played_ms < at + REPEAT_MS)
        {
            return false;
        }
        self.last_alert = Some(self.played_ms);
        true
    }
}

/// A 16-bit mono WAV of a falling two-note chirp, for `PlaySoundW` from memory.
pub fn alert_wav() -> Vec<u8> {
    const RATE: u32 = 22_050;
    let note = |freq: f32, ms: u32, out: &mut Vec<i16>| {
        let n = RATE * ms / 1000;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            // A short fade at both ends so it does not click.
            let edge = (i.min(n - i) as f32 / (RATE as f32 * 0.01)).min(1.0);
            let v = (t * freq * core::f32::consts::TAU).sin() * 0.35 * edge;
            out.push((v * f32::from(i16::MAX)) as i16);
        }
    };
    let mut samples = Vec::new();
    note(880.0, 110, &mut samples);
    note(587.0, 160, &mut samples);
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

static DISPLAY: AtomicBool = AtomicBool::new(false);
static BELOW: AtomicU32 = AtomicU32::new(60);
static COLOUR: AtomicBool = AtomicBool::new(true);
static SOUND: AtomicBool = AtomicBool::new(false);

/// Takes the settings (at startup and whenever the user changes one; they apply at once).
pub fn configure(cfg: &HostConfig) {
    DISPLAY.store(cfg.apm_display, Ordering::Release);
    BELOW.store(cfg.apm_alert_below, Ordering::Release);
    COLOUR.store(cfg.apm_color_text, Ordering::Release);
    SOUND.store(cfg.apm_play_sound, Ordering::Release);
}

#[cfg(target_arch = "x86")]
pub use live::{before_hud, install, on_turn, paused, reset};

#[cfg(target_arch = "x86")]
mod live {
    use core::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};

    use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

    use super::{BELOW, COLOUR, DISPLAY, SOUND, Tracker, alert_wav};
    use crate::chat::now_ms;
    use crate::guard::guarded;
    use crate::hooks::{self, Owner};
    use crate::host::Log;
    use crate::hudpanel::BeginFn;
    use crate::pe::Image;
    use crate::screenkit::game::{Kit, rgba};
    use crate::screenkit::{ALIGN_CENTRE, Rect};
    use crate::symbols::Symbols;

    type ExtentFn = unsafe extern "C" fn() -> f32;
    /// `QueueCommand(record, length)`: the game's "queue one command for this turn" (0x49ec10),
    /// called by every UI command (orders, selection, building, research...) in every game mode.
    type QueueFn = unsafe extern "C" fn(*const u8, i32);
    /// `ProcessLocalMatchTick()`: one game turn in a local (single-player) match; non-zero
    /// when a turn ran.
    type TurnFn = unsafe extern "C" fn() -> u32;

    /// The command records a player gives on purpose: not the idle record (05), the per-turn
    /// sync record (18) nor the lobby-only records from 19 up.
    const FIRST_COMMAND: u8 = 0x06;
    const LAST_COMMAND: u8 = 0x17;

    const WINDOW_FLAGS: u32 = 0x120;
    const WIDTH: f32 = 92.0;
    const HEIGHT: f32 = 18.0;
    const MARGIN_RIGHT: f32 = 4.0;
    /// Distance from the bottom edge of the screen to the panel's bottom edge.
    const MARGIN_BOTTOM: f32 = 4.0;
    const LOW: u32 = rgba(232, 72, 56, 255);

    struct Game {
        kit: Kit,
        ui_width: ExtentFn,
        ui_height: ExtentFn,
        /// `MatchPauseFlags`: non-zero while the game is paused.
        paused: usize,
    }

    // SAFETY: plain game function addresses, fixed for the session.
    unsafe impl Send for Game {}
    unsafe impl Sync for Game {}

    static GAME: OnceLock<Game> = OnceLock::new();
    static TRACKER: Mutex<Option<Tracker>> = Mutex::new(None);
    static SOUND_DATA: OnceLock<Vec<u8>> = OnceLock::new();
    static QUEUE_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
    static LOCAL_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
    /// Commands queued since the last turn.
    static PENDING: AtomicU32 = AtomicU32::new(0);
    /// The first command of the match was logged.
    static LOGGED: AtomicBool = AtomicBool::new(false);

    pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
        let get = |n: &str| symbols.address(n);
        let (Some(width), Some(height), Some(queue), Some(local_tick), Some(paused)) = (
            get("HudUiWidth"),
            get("HudUiHeight"),
            get("QueueCommand"),
            get("ProcessLocalMatchTick"),
            get("MatchPauseFlags"),
        ) else {
            log.line(W2R_LOG_ERROR, "host", "apm counter: symbols missing");
            return false;
        };
        let Some(kit) = Kit::new_in(symbols, "HudCtx") else {
            log.line(W2R_LOG_ERROR, "host", "apm counter: screen-kit symbols missing");
            return false;
        };
        // SAFETY: `HudUiWidth`/`HudUiHeight` take nothing and return a float (Ghidra, build 2818).
        let (ui_width, ui_height) = unsafe {
            (
                core::mem::transmute::<usize, ExtentFn>(width),
                core::mem::transmute::<usize, ExtentFn>(height),
            )
        };
        let _ = GAME.set(Game {
            kit,
            ui_width,
            ui_height,
            paused,
        });
        // SAFETY: each detour matches its target's cdecl signature (Ghidra, build 2818) and
        // calls the original with the same arguments.
        let hooked = unsafe {
            let a = hooks::global().install(
                Owner::Host,
                image,
                queue,
                queue_detour as *mut c_void,
                "host:QueueCommand",
                |original| QUEUE_ORIGINAL.store(original as usize, Ordering::Release),
            );
            let b = hooks::global().install(
                Owner::Host,
                image,
                local_tick,
                local_detour as *mut c_void,
                "host:ProcessLocalMatchTick",
                |original| LOCAL_ORIGINAL.store(original as usize, Ordering::Release),
            );
            (a.is_ok(), b.is_ok())
        };
        if hooked == (true, true) {
            log.line(W2R_LOG_INFO, "host", "apm counter ready");
            true
        } else {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("apm counter: could not hook (queue {}, local tick {})", hooked.0, hooked.1),
            );
            false
        }
    }

    /// A new match: start counting from zero.
    pub fn reset() {
        if let Ok(mut t) = TRACKER.lock() {
            *t = Some(Tracker::new());
        }
        PENDING.store(0, Ordering::Release);
        LOGGED.store(false, Ordering::Release);
    }

    unsafe extern "C" fn queue_detour(record: *const u8, length: i32) {
        let original: QueueFn =
            unsafe { core::mem::transmute(QUEUE_ORIGINAL.load(Ordering::Acquire)) };
        if !record.is_null() && length > 0 && !crate::replayplay::is_active() {
            // SAFETY: the game passes a record of `length` bytes; one byte is read, under `guarded`.
            if let Ok(op) = guarded(|| unsafe { *record }) {
                if (FIRST_COMMAND..=LAST_COMMAND).contains(&op) {
                    PENDING.fetch_add(1, Ordering::AcqRel);
                    if !LOGGED.swap(true, Ordering::AcqRel) {
                        crate::session::StartupLog.line(
                            W2R_LOG_INFO,
                            "apm",
                            &format!("first command of the match: record {op:#04x}"),
                        );
                    }
                }
            }
        }
        unsafe { original(record, length) };
    }

    unsafe extern "C" fn local_detour() -> u32 {
        let original: TurnFn =
            unsafe { core::mem::transmute(LOCAL_ORIGINAL.load(Ordering::Acquire)) };
        let ran = unsafe { original() };
        if ran != 0 {
            if let Err(fault) = guarded(on_turn) {
                crate::session::host_fault("apm counter", &fault);
            }
        }
        ran
    }

    /// The game is paused (turns still run, the simulation does not). Call inside `guarded`.
    pub fn paused() -> bool {
        let Some(g) = GAME.get() else { return false };
        // SAFETY: a byte in the game's data section, read under the caller's `guarded`.
        unsafe { core::ptr::read_unaligned(g.paused as *const u8) != 0 }
    }

    /// The game ran a turn: close the count for it. Call inside `guarded`.
    pub fn on_turn() {
        let Some(g) = GAME.get() else { return };
        if crate::replayplay::is_active() || crate::observer::local_is_observer() {
            return;
        }
        // The turns go on while the game is paused (only the simulation stops): a pause is not
        // play, so nothing is counted and no alert fires until it ends.
        // SAFETY: a byte in the game's data section, read under the caller's `guarded`.
        if unsafe { core::ptr::read_unaligned(g.paused as *const u8) } != 0 {
            return;
        }
        let actions = PENDING.swap(0, Ordering::AcqRel);
        let alert = {
            let Ok(mut t) = TRACKER.lock() else { return };
            let t = t.get_or_insert_with(Tracker::new);
            t.turn(now_ms(), actions);
            SOUND.load(Ordering::Acquire) && t.alert_due(BELOW.load(Ordering::Acquire))
        };
        if alert {
            play_alert();
        }
    }

    fn play_alert() {
        use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
        let wav = SOUND_DATA.get_or_init(alert_wav);
        // SAFETY: the WAV lives in a static for the rest of the process, as SND_ASYNC with
        // SND_MEMORY requires.
        unsafe {
            PlaySoundW(
                wav.as_ptr().cast(),
                core::ptr::null_mut(),
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            );
        }
    }

    /// Called by the window hook just before the game starts its `game_hud` window.
    ///
    /// # Safety
    /// `ctx` is the context the game is about to begin `game_hud` in; `begin` is the game's own
    /// `BeginNamedWindow`.
    pub unsafe fn before_hud(ctx: *mut c_void, begin: BeginFn) {
        if !DISPLAY.load(Ordering::Acquire)
            || crate::replayplay::is_active()
            || crate::observer::local_is_observer()
        {
            return;
        }
        let Some(g) = GAME.get() else { return };
        let (apm, low) = match TRACKER.lock() {
            Ok(t) => match t.as_ref() {
                Some(t) => (t.apm(), t.low(BELOW.load(Ordering::Acquire))),
                None => (None, false),
            },
            Err(_) => return,
        };
        let result = guarded(|| unsafe {
            let x = ((g.ui_width)() - MARGIN_RIGHT - WIDTH).max(0.0);
            let top = ((g.ui_height)() - MARGIN_BOTTOM - HEIGHT).max(0.0);
            if begin(ctx, c"apm_panel".as_ptr(), x, top, WIDTH, HEIGHT, WINDOW_FLAGS) != 0 {
                g.kit.space_begin();
                let text = match apm {
                    Some(v) => format!("APM {v}"),
                    None => "APM -".to_string(),
                };
                let r = Rect::new(0.0, 2.0, WIDTH, 14.0);
                if low && COLOUR.load(Ordering::Acquire) {
                    g.kit.label_coloured_at(r, &text, ALIGN_CENTRE, LOW);
                } else {
                    g.kit.label_at(r, &text, ALIGN_CENTRE);
                }
                g.kit.space_end();
                g.kit.end_window();
            }
        });
        if let Err(fault) = result {
            crate::session::host_fault("apm panel", &fault);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A turn every 100 ms with `per_turn` actions in each, for `ms` of play.
    fn play(t: &mut Tracker, from: u64, ms: u64, per_turn: u32) -> u64 {
        let mut now = from;
        while now < from + ms {
            now += 100;
            t.turn(now, per_turn);
        }
        now
    }

    #[test]
    fn no_figure_until_there_is_play_to_measure() {
        let mut t = Tracker::new();
        assert_eq!(t.apm(), None);
        let now = play(&mut t, 1_000, 5_000, 1);
        assert_eq!(t.apm(), None, "under five seconds is too little");
        play(&mut t, now, 6_000, 1);
        assert!(t.apm().is_some());
    }

    #[test]
    fn counts_actions_over_the_window() {
        let mut t = Tracker::new();
        // One action every 500 ms = 120 a minute.
        let mut now = 0;
        for i in 0..400 {
            now += 100;
            t.turn(now, u32::from(i % 5 == 0));
        }
        assert_eq!(t.apm(), Some(120));
        // Then 20 seconds of nothing: the old actions leave the window.
        play(&mut t, now, 21_000, 0);
        assert_eq!(t.apm(), Some(0));
    }

    #[test]
    fn a_pause_is_not_play() {
        let mut t = Tracker::new();
        let now = play(&mut t, 0, 20_000, 1);
        let before = t.played_ms();
        t.turn(now + 600_000, 0);
        assert!(t.played_ms() - before <= 500, "ten minutes away counts for one step");
    }

    #[test]
    fn alerts_after_the_grace_period_and_not_too_often() {
        let mut t = Tracker::new();
        // One action a second: 60 a minute, below 100.
        let mut now = 0;
        let mut alerts = Vec::new();
        for i in 0..1800 {
            now += 100;
            t.turn(now, u32::from(i % 10 == 0));
            if t.alert_due(100) {
                alerts.push(t.played_ms());
            }
        }
        assert!(!alerts.is_empty());
        assert!(alerts[0] >= GRACE_MS, "nothing in the first minute");
        assert!(alerts.windows(2).all(|w| w[1] - w[0] >= REPEAT_MS));
        assert!(t.low(100));
        assert!(!t.low(0), "a threshold of 0 is no alert");
        assert!(!t.low(30));
    }

    #[test]
    fn someone_who_never_acts_is_not_alerted() {
        let mut t = Tracker::new();
        play(&mut t, 0, 120_000, 0);
        assert_eq!(t.apm(), Some(0));
        assert!(!t.low(60));
        assert!(!t.alert_due(60));
    }

    #[test]
    fn the_alert_sound_is_a_wav() {
        let wav = alert_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert_eq!(wav.len(), 44 + data_len);
        assert_eq!(
            u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize,
            wav.len() - 8
        );
    }
}
