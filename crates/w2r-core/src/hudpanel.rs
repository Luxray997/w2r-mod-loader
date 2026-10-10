//! The replay panel: speed, pause state, the hotkeys and a progress bar, drawn on the in-match
//! HUD while a replay plays. The HUD builder itself belongs to the chat plugin (it replaces it
//! whole and also hooks the map-message functions), so the panel is a small window of its own
//! that the host's `BeginNamedWindow` hook opens just before the game begins `game_hud`.
//! x86 only (`cdecl`).

use core::ffi::c_void;
use std::sync::OnceLock;

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::host::Log;
use crate::replayplay::{self, Pace};
use crate::screenkit::game::{Kit, rgba};
use crate::screenkit::{ALIGN_CENTRE, ALIGN_LEFT, ALIGN_RIGHT, Rect};
use crate::symbols::Symbols;

/// The game's `BeginNamedWindow`.
pub type BeginFn =
    unsafe extern "C" fn(*mut c_void, *const core::ffi::c_char, f32, f32, f32, f32, u32) -> i32;
type ExtentFn = unsafe extern "C" fn() -> f32;

/// The flags the HUD's own full-screen window is begun with.
const WINDOW_FLAGS: u32 = 0x120;
static PANEL: OnceLock<Panel> = OnceLock::new();

struct Panel {
    kit: Kit,
    ui_width: ExtentFn,
}

// SAFETY: plain game function addresses, fixed for the session.
unsafe impl Send for Panel {}
unsafe impl Sync for Panel {}

const WIDTH: f32 = 250.0;
const HEIGHT: f32 = 46.0;
const MARGIN_RIGHT: f32 = 24.0;
const TOP: f32 = 22.0;
const BAR_HEIGHT: f32 = 6.0;

const BACKGROUND: u32 = rgba(12, 9, 6, 200);
const TRACK: u32 = rgba(70, 60, 44, 255);
const FILL: u32 = rgba(226, 184, 68, 255);
const FILL_PAUSED: u32 = rgba(190, 96, 72, 255);

pub fn install(symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(width) = symbols.address("HudUiWidth") else {
        log.line(W2R_LOG_ERROR, "host", "replay panel: symbols missing");
        return false;
    };
    let Some(kit) = Kit::new_in(symbols, "HudCtx") else {
        log.line(W2R_LOG_ERROR, "host", "replay panel: screen-kit symbols missing");
        return false;
    };
    // SAFETY: `HudUiWidth` takes nothing and returns a float (Ghidra, build 2818).
    let ui_width = unsafe { core::mem::transmute::<usize, ExtentFn>(width) };
    let _ = PANEL.set(Panel { kit, ui_width });
    log.line(W2R_LOG_INFO, "host", "replay panel ready");
    true
}

/// Called by the window hook just before the game starts its `game_hud` window: while a replay
/// plays, the panel is a small window of its own, begun and ended here (the chat plugin owns
/// every HUD function, so nothing is hooked inside the HUD). `begin` is the game's own
/// `BeginNamedWindow`, so this does not come back through the hook.
///
/// # Safety
/// `ctx` is the context the game is about to begin `game_hud` in.
pub unsafe fn before_hud(ctx: *mut c_void, begin: BeginFn) {
    let Some(pace) = replayplay::pace() else { return };
    let Some(panel) = PANEL.get() else { return };
    let result = guarded(|| unsafe {
        let x = ((panel.ui_width)() - MARGIN_RIGHT - WIDTH).max(0.0);
        if begin(ctx, c"replay_panel".as_ptr(), x, TOP, WIDTH, HEIGHT, WINDOW_FLAGS) != 0 {
            panel.kit.space_begin();
            draw(&pace, x);
            panel.kit.space_end();
            panel.kit.end_window();
        }
    });
    if let Err(fault) = result {
        crate::session::host_fault("replay panel", &fault);
    }
}

/// `x` is the window's left edge: fills use screen coordinates, labels the window's own.
fn draw(pace: &Pace, x: f32) {
    let Some(panel) = PANEL.get() else { return };
    let kit = &panel.kit;
    let frame = Rect::new(x, TOP, WIDTH, HEIGHT);
    kit.fill_round(frame, 3.0, BACKGROUND);

    let fraction = if pace.total == 0 {
        0.0
    } else {
        pace.turn as f32 / pace.total as f32
    };
    let bar = Rect::new(x + 6.0, TOP + 19.0, WIDTH - 12.0, BAR_HEIGHT);
    kit.fill_rect(bar, TRACK);
    let colour = if pace.paused || pace.ended {
        FILL_PAUSED
    } else {
        FILL
    };
    kit.fill_rect(Rect::new(bar.x, bar.y, bar.w * fraction, bar.h), colour);

    let state = if pace.ended {
        "FINISHED".to_string()
    } else if pace.paused {
        format!("PAUSED  {}x", pace.speed)
    } else {
        format!("PLAYING  {}x", pace.speed)
    };
    let time = format!(
        "turn {} / {}   {}%",
        pace.turn,
        pace.total,
        (fraction * 100.0) as u32
    );
    kit.label_at(Rect::new(6.0, 2.0, WIDTH * 0.5, 14.0), &state, ALIGN_LEFT);
    kit.label_at(
        Rect::new(WIDTH * 0.5, 2.0, WIDTH * 0.5 - 6.0, 14.0),
        &time,
        ALIGN_RIGHT,
    );
    let hint = if pace.ended {
        "Open the menu to leave"
    } else {
        "PgUp/PgDn speed   End pause   Del step"
    };
    kit.label_at(Rect::new(0.0, 28.0, WIDTH, 14.0), hint, ALIGN_CENTRE);
}
