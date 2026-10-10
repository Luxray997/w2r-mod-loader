//! The replay panel: speed, pause state, the hotkeys and a progress bar, with a scoreboard under
//! it (each player's name, resources, supply and unit counts), drawn on the in-match HUD while a
//! replay plays. The HUD builder itself belongs to the chat plugin (it replaces it whole and also
//! hooks the map-message functions), so the panel is a small window of its own that the host's
//! `BeginNamedWindow` hook opens just before the game begins `game_hud`. x86 only (`cdecl`).

use core::ffi::c_void;
use std::sync::OnceLock;

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::host::Log;
use crate::replayplay::{self, Member, Pace};
use crate::scoreboard::{self, Counts};
use crate::screenkit::game::{Kit, rgba};
use crate::screenkit::{ALIGN_CENTRE, ALIGN_LEFT, ALIGN_RIGHT, Rect};
use crate::symbols::Symbols;

type ExtentFn = unsafe extern "C" fn() -> f32;
/// The game's `BeginNamedWindow`.
pub type BeginFn =
    unsafe extern "C" fn(*mut c_void, *const core::ffi::c_char, f32, f32, f32, f32, u32) -> i32;

static PANEL: OnceLock<Panel> = OnceLock::new();

/// Where the game keeps what the scoreboard shows (all indexed by player).
struct Game {
    gold: usize,
    lumber: usize,
    oil: usize,
    food_used: usize,
    food_free: usize,
    food_cap: usize,
    units: usize,
    unit_count: usize,
    type_flags: usize,
}

struct Panel {
    kit: Kit,
    ui_width: ExtentFn,
    game: Game,
}

// SAFETY: plain game function and data addresses, fixed for the session.
unsafe impl Send for Panel {}
unsafe impl Sync for Panel {}

/// The flags the HUD's own full-screen window is begun with.
const WINDOW_FLAGS: u32 = 0x120;
const WIDTH: f32 = 362.0;
const HEADER_HEIGHT: f32 = 46.0;
const ROW_HEIGHT: f32 = 12.0;
const MARGIN_RIGHT: f32 = 24.0;
const TOP: f32 = 22.0;
const BAR_HEIGHT: f32 = 6.0;
const MAX_UNITS: usize = 4096;

const BACKGROUND: u32 = rgba(12, 9, 6, 200);
const TRACK: u32 = rgba(70, 60, 44, 255);
const FILL: u32 = rgba(226, 184, 68, 255);
const FILL_PAUSED: u32 = rgba(190, 96, 72, 255);
const WATCHED_ROW: u32 = rgba(226, 184, 68, 70);
const DIM: u32 = rgba(140, 130, 112, 255);
const GONE: u32 = rgba(110, 100, 88, 255);

/// Player colours by slot: red, blue, green, violet, orange, black, white, yellow.
const SLOT_COLOURS: [u32; 8] = [
    rgba(214, 40, 40, 255),
    rgba(40, 92, 224, 255),
    rgba(40, 176, 64, 255),
    rgba(160, 60, 196, 255),
    rgba(244, 140, 30, 255),
    rgba(100, 100, 100, 255),
    rgba(236, 236, 236, 255),
    rgba(244, 224, 40, 255),
];

/// A column of the scoreboard: left edge, width, heading.
const COLUMNS: [(f32, f32, &str); 7] = [
    (112.0, 40.0, "Gold"),
    (154.0, 40.0, "Wood"),
    (196.0, 36.0, "Oil"),
    (234.0, 46.0, "Food"),
    (286.0, 22.0, "Arm"),
    (314.0, 22.0, "Wrk"),
    (342.0, 18.0, "Bld"),
];

pub fn install(symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(width) = symbols.address("HudUiWidth") else {
        log.line(W2R_LOG_ERROR, "host", "replay panel: symbols missing");
        return false;
    };
    let Some(kit) = Kit::new_in(symbols, "HudCtx") else {
        log.line(W2R_LOG_ERROR, "host", "replay panel: screen-kit symbols missing");
        return false;
    };
    let get = |n: &str| symbols.address(n);
    let (
        Some(gold),
        Some(lumber),
        Some(oil),
        Some(food_used),
        Some(food_free),
        Some(food_cap),
        Some(units),
        Some(unit_count),
        Some(type_flags),
    ) = (
        get("PlayerGold"),
        get("PlayerLumber"),
        get("PlayerOil"),
        get("FoodA"),
        get("FoodB"),
        get("FoodCap"),
        get("UnitArrayPtr"),
        get("UnitCount"),
        get("UnitTypeFlags"),
    )
    else {
        log.line(W2R_LOG_ERROR, "host", "replay scoreboard: symbols missing");
        return false;
    };
    // SAFETY: `HudUiWidth` takes nothing and returns a float (Ghidra, build 2818).
    let ui_width = unsafe { core::mem::transmute::<usize, ExtentFn>(width) };
    let game = Game {
        gold,
        lumber,
        oil,
        food_used,
        food_free,
        food_cap,
        units,
        unit_count,
        type_flags,
    };
    let _ = PANEL.set(Panel {
        kit,
        ui_width,
        game,
    });
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
        let board = if replayplay::board_shown() {
            Some(read_board(&panel.game))
        } else {
            None
        };
        let height = match &board {
            Some(rows) => HEADER_HEIGHT + ROW_HEIGHT * (rows.len() as f32 + 1.0) + 4.0,
            None => HEADER_HEIGHT,
        };
        let x = ((panel.ui_width)() - MARGIN_RIGHT - WIDTH).max(0.0);
        if begin(ctx, c"replay_panel".as_ptr(), x, TOP, WIDTH, height, WINDOW_FLAGS) != 0 {
            panel.kit.space_begin();
            draw(&pace, board.as_deref(), x, height);
            panel.kit.space_end();
            panel.kit.end_window();
        }
    });
    if let Err(fault) = result {
        crate::session::host_fault("replay panel", &fault);
    }
}

/// One scoreboard row, read from the game's tables.
struct Row {
    member: Member,
    gold: u32,
    lumber: u32,
    oil: u32,
    food_used: u16,
    food_cap: u16,
    counts: Counts,
}

/// # Safety
/// Reads the game's player tables and unit array; the caller wraps it in `guarded`.
unsafe fn read_board(g: &Game) -> Vec<Row> {
    let mut counts = [Counts::default(); 8];
    unsafe {
        let base = core::ptr::read_unaligned(g.units as *const usize);
        let n = core::ptr::read_unaligned(g.unit_count as *const u32) as usize;
        if base != 0 {
            let flags_of = |ty: u8| {
                core::ptr::read_unaligned((g.type_flags + usize::from(ty) * 4) as *const u32)
            };
            for i in 0..n.min(MAX_UNITS) {
                let slot = core::slice::from_raw_parts(
                    (base + i * scoreboard::STRIDE) as *const u8,
                    scoreboard::STRIDE,
                );
                scoreboard::count(slot, flags_of, &mut counts);
            }
        }
    }
    replayplay::roster()
        .into_iter()
        .map(|member| {
            let p = usize::from(member.slot);
            // SAFETY: per-player tables of at least 8 entries in the game's data section.
            unsafe {
                let u32_at = |base: usize| core::ptr::read_unaligned((base + p * 4) as *const u32);
                let u16_at = |base: usize| core::ptr::read_unaligned((base + p * 2) as *const u16);
                Row {
                    gold: u32_at(g.gold),
                    lumber: u32_at(g.lumber),
                    oil: u32_at(g.oil),
                    food_used: u16_at(g.food_used).wrapping_sub(u16_at(g.food_free)),
                    food_cap: u16_at(g.food_cap).min(200),
                    counts: counts[p],
                    member,
                }
            }
        })
        .collect()
}

/// `x` is the window's left edge: fills use screen coordinates, labels the window's own.
fn draw(pace: &Pace, board: Option<&[Row]>, x: f32, height: f32) {
    let Some(panel) = PANEL.get() else { return };
    let kit = &panel.kit;
    kit.fill_round(Rect::new(x, TOP, WIDTH, height), 3.0, BACKGROUND);

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
    let progress = format!(
        "turn {} / {}   {}%",
        pace.turn,
        pace.total,
        (fraction * 100.0) as u32
    );
    kit.label_at(Rect::new(6.0, 2.0, WIDTH * 0.5, 14.0), &state, ALIGN_LEFT);
    kit.label_at(
        Rect::new(WIDTH * 0.5, 2.0, WIDTH * 0.5 - 6.0, 14.0),
        &progress,
        ALIGN_RIGHT,
    );
    let hint = if pace.ended {
        "Open the menu to leave"
    } else {
        "PgUp/PgDn speed   End pause   Del step   ` scoreboard"
    };
    kit.label_at(Rect::new(0.0, 28.0, WIDTH, 14.0), hint, ALIGN_CENTRE);

    let Some(rows) = board else { return };
    let watching = replayplay::watching();
    let top = HEADER_HEIGHT;
    for (rect_x, rect_w, title) in COLUMNS {
        kit.label_coloured_at(
            Rect::new(rect_x, top, rect_w, ROW_HEIGHT),
            title,
            ALIGN_RIGHT,
            DIM,
        );
    }
    kit.label_coloured_at(
        Rect::new(18.0, top, 90.0, ROW_HEIGHT),
        "Player",
        ALIGN_LEFT,
        DIM,
    );
    for (n, row) in rows.iter().enumerate() {
        let y = top + ROW_HEIGHT * (n as f32 + 1.0);
        let gone = row.counts.is_empty();
        if watching == Some(row.member.slot) {
            kit.fill_rect(
                Rect::new(x + 2.0, TOP + y, WIDTH - 4.0, ROW_HEIGHT),
                WATCHED_ROW,
            );
        }
        let swatch = SLOT_COLOURS[usize::from(row.member.slot) % 8];
        kit.fill_rect(
            Rect::new(x + 6.0, TOP + y + 2.0, 8.0, ROW_HEIGHT - 4.0),
            swatch,
        );
        let text = |value: String, column: usize| {
            let (cx, cw, _) = COLUMNS[column];
            let r = Rect::new(cx, y, cw, ROW_HEIGHT);
            if gone {
                kit.label_coloured_at(r, &value, ALIGN_RIGHT, GONE);
            } else {
                kit.label_at(r, &value, ALIGN_RIGHT);
            }
        };
        let name = Rect::new(18.0, y, 92.0, ROW_HEIGHT);
        if gone {
            kit.label_coloured_at(name, &row.member.name, ALIGN_LEFT, GONE);
        } else {
            kit.label_at(name, &row.member.name, ALIGN_LEFT);
        }
        text(row.gold.to_string(), 0);
        text(row.lumber.to_string(), 1);
        text(row.oil.to_string(), 2);
        text(format!("{}/{}", row.food_used, row.food_cap), 3);
        text(row.counts.army.to_string(), 4);
        text(row.counts.workers.to_string(), 5);
        text(row.counts.buildings.to_string(), 6);
    }
}
